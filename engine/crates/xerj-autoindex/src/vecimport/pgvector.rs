//! pgvector source: a Postgres table with a `vector(n)` column.
//!
//! * describe: the key column (`--id-column`, else the primary key), the
//!   vector column (`--vector-column`, else the only `vector` column), its
//!   declared dimension (`atttypmod`), the similarity from the operator class
//!   of an index on that column (`vector_cosine_ops` / `vector_ip_ops` /
//!   `vector_l2_ops`), and `count(*)`.
//! * page: keyset pagination, `WHERE key > $cursor ORDER BY key LIMIT n`,
//!   cursor = the last key as text cast back to the key's own type, so the
//!   order is the column's native order and the scan uses its index.
//! * top1: `ORDER BY vec <=>|<#>|<-> $q::vector LIMIT 1`.
//!
//! Every identifier is validated and double-quoted; values are bound
//! parameters. Plain TCP (no TLS) in this version: `sslmode=require` is
//! refused with a message rather than silently downgraded.

use anyhow::{anyhow, bail, Context, Result};
use postgres::{Client, NoTls};
use serde_json::{Map, Value};

use super::{Page, Point, Similarity, Source, SourceInfo};

pub struct PgVector {
    client: Client,
    table: String,
    schema: String,
    id_col: Option<String>,
    vec_col: Option<String>,
    id_type: String,
    sim: Similarity,
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn q(s: &str) -> String {
    format!("\"{s}\"")
}

/// `table` or `schema.table` → (schema, table), validated.
pub(crate) fn split_table(t: &str) -> Result<(String, String)> {
    let (s, n) = match t.split_once('.') {
        Some((s, n)) => (s.to_string(), n.to_string()),
        None => ("public".to_string(), t.to_string()),
    };
    if !ident_ok(&s) || !ident_ok(&n) {
        bail!(
            "--pgvector-table `{t}`: use letters, digits and underscores (schema.table optional)"
        );
    }
    Ok((s, n))
}

/// Parse pgvector's text form `[1,2,3]`.
pub(crate) fn parse_vector_text(s: &str) -> Result<Vec<f32>> {
    let t = s.trim().trim_start_matches('[').trim_end_matches(']');
    if t.is_empty() {
        return Ok(Vec::new());
    }
    t.split(',')
        .map(|x| {
            x.trim()
                .parse::<f32>()
                .map_err(|_| anyhow!("bad vector component `{x}`"))
        })
        .collect()
}

fn vector_text(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 10 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

pub(crate) fn opclass_similarity(op: &str) -> Option<Similarity> {
    match op {
        "vector_cosine_ops" | "halfvec_cosine_ops" => Some(Similarity::Cosine),
        "vector_ip_ops" | "halfvec_ip_ops" => Some(Similarity::MaxInnerProduct),
        "vector_l2_ops" | "halfvec_l2_ops" => Some(Similarity::L2),
        _ => None,
    }
}

impl PgVector {
    pub fn new(
        conn: &str,
        table: &str,
        id_col: Option<String>,
        vec_col: Option<String>,
    ) -> Result<Self> {
        if conn.contains("sslmode=require") || conn.contains("sslmode=verify") {
            bail!("pgvector source: TLS connections are not supported in this version; connect over a trusted network or an SSH tunnel with sslmode=disable");
        }
        for c in [&id_col, &vec_col].into_iter().flatten() {
            if !ident_ok(c) {
                bail!("column name `{c}`: use letters, digits and underscores");
            }
        }
        let (schema, table) = split_table(table)?;
        let client = Client::connect(conn, NoTls).context("connecting to Postgres")?;
        Ok(PgVector {
            client,
            table,
            schema,
            id_col,
            vec_col,
            id_type: String::new(),
            sim: Similarity::Cosine,
        })
    }

    fn fq(&self) -> String {
        format!("{}.{}", q(&self.schema), q(&self.table))
    }

    fn op(&self) -> &'static str {
        match self.sim {
            Similarity::Cosine => "<=>",
            Similarity::MaxInnerProduct => "<#>",
            Similarity::L2 => "<->",
        }
    }
}

impl Source for PgVector {
    fn describe(&mut self) -> Result<SourceInfo> {
        let rel = format!("{}.{}", self.schema, self.table);
        // Columns and their types.
        let cols = self
            .client
            .query(
                "SELECT a.attname, format_type(a.atttypid, a.atttypmod), t.typname, a.atttypmod
               FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid
              WHERE a.attrelid = $1::text::regclass AND a.attnum > 0 AND NOT a.attisdropped",
                &[&rel],
            )
            .with_context(|| format!("table `{rel}` not found"))?;
        let vec_cols: Vec<(String, i32)> = cols
            .iter()
            .filter(|r| r.get::<_, String>(2) == "vector")
            .map(|r| (r.get::<_, String>(0), r.get::<_, i32>(3)))
            .collect();
        let (vcol, typmod) = match &self.vec_col {
            Some(c) => vec_cols
                .iter()
                .find(|(n, _)| n == c)
                .cloned()
                .ok_or_else(|| anyhow!("`{c}` is not a vector column of {rel}"))?,
            None => match vec_cols.as_slice() {
                [one] => one.clone(),
                [] => bail!("{rel} has no pgvector `vector` column"),
                many => bail!(
                    "{rel} has several vector columns {:?}; pick one with --vector-column",
                    many.iter().map(|(n, _)| n).collect::<Vec<_>>()
                ),
            },
        };
        self.vec_col = Some(vcol.clone());
        let id = match &self.id_col {
            Some(c) => c.clone(),
            None => {
                let pk = self.client.query(
                    "SELECT a.attname FROM pg_index i
                       JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
                      WHERE i.indrelid = $1::text::regclass AND i.indisprimary",
                    &[&rel],
                )?;
                match pk.as_slice() {
                    [one] => one.get::<_, String>(0),
                    [] => bail!("{rel} has no primary key; pass --id-column <unique column>"),
                    _ => {
                        bail!("{rel} has a composite primary key; pass --id-column <unique column>")
                    }
                }
            }
        };
        self.id_type = cols
            .iter()
            .find(|r| r.get::<_, String>(0) == id)
            .map(|r| r.get::<_, String>(1))
            .ok_or_else(|| anyhow!("`{id}` is not a column of {rel}"))?;
        self.id_col = Some(id);
        // Similarity from an index on the vector column.
        let ops = self.client.query(
            "SELECT o.opcname FROM pg_index i
               JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[0]
               JOIN pg_opclass o ON o.oid = i.indclass[0]
              WHERE i.indrelid = $1::text::regclass AND a.attname = $2::text",
            &[&rel, &vcol],
        )?;
        let found = ops
            .iter()
            .filter_map(|r| opclass_similarity(&r.get::<_, String>(0)))
            .next();
        let (sim, basis) = match found {
            Some(s) => (s, format!("index operator class on `{vcol}`")),
            None => (Similarity::Cosine, format!("default cosine: no vector index on `{vcol}`; set --similarity if your queries use <#> or <->")),
        };
        self.sim = sim;
        let dims = if typmod > 0 {
            typmod as usize
        } else {
            let row = self.client.query_opt(
                &format!(
                    "SELECT vector_dims({}) FROM {} LIMIT 1",
                    q(&vcol),
                    self.fq()
                ),
                &[],
            )?;
            row.map(|r| r.get::<_, i32>(0) as usize).ok_or_else(|| {
                anyhow!("{rel}.{vcol} declares no dimension and the table is empty")
            })?
        };
        let count: i64 = self
            .client
            .query_one(&format!("SELECT count(*) FROM {}", self.fq()), &[])?
            .get(0);
        Ok(SourceInfo {
            kind: "pgvector",
            name: self.table.clone(),
            dims,
            similarity: sim,
            count: Some(count as u64),
            similarity_basis: basis,
        })
    }

    fn page(&mut self, cursor: Option<&str>, limit: usize) -> Result<Page> {
        // Output columns are aliased k/v/j and the ORDER BY names `t.<id>`:
        // an unqualified `ORDER BY "id"` resolves to an output column of the
        // same name first, which here is `id::text`, and a text order breaks
        // the keyset (`10` sorts before `9`) while the WHERE compares numbers.
        let id = q(self
            .id_col
            .as_deref()
            .ok_or_else(|| anyhow!("describe() not called"))?);
        let vc = q(self.vec_col.as_deref().unwrap());
        let lim = limit as i64;
        let base = format!(
            "SELECT t.{id}::text AS k, t.{vc}::text AS v, row_to_json(t)::text AS j FROM {} t",
            self.fq()
        );
        let rows = match cursor {
            None => self
                .client
                .query(&format!("{base} ORDER BY t.{id} LIMIT $1"), &[&lim])?,
            Some(c) => self.client.query(
                &format!(
                    "{base} WHERE t.{id} > $1::text::{} ORDER BY t.{id} LIMIT $2",
                    self.id_type
                ),
                &[&c, &lim],
            )?,
        };
        let idc = self.id_col.clone().unwrap();
        let vcn = self.vec_col.clone().unwrap();
        let mut points = Vec::with_capacity(rows.len());
        for r in &rows {
            let pid: Option<String> = r.get(0);
            let pid = pid.ok_or_else(|| anyhow!("a row has a NULL key"))?;
            let vt: Option<String> = r.get(1);
            let Some(vt) = vt else { continue };
            let vector = parse_vector_text(&vt)?;
            let row: String = r.get(2);
            let mut payload: Map<String, Value> = match serde_json::from_str(&row)? {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            payload.remove(&vcn);
            payload.remove(&idc);
            points.push(Point {
                id: pid,
                vector,
                payload,
                document: None,
            });
        }
        let next = if rows.len() < limit {
            None
        } else {
            rows.last().and_then(|r| r.get::<_, Option<String>>(0))
        };
        Ok(Page { points, next })
    }

    fn top1(&mut self, qv: &[f32]) -> Result<Option<String>> {
        let id = q(self.id_col.as_deref().unwrap());
        let vc = q(self.vec_col.as_deref().unwrap());
        let sql = format!(
            "SELECT {id}::text FROM {} ORDER BY {vc} {} $1::text::vector LIMIT 1",
            self.fq(),
            self.op()
        );
        // Exact answer for the comparison: an HNSW/IVFFlat index would make
        // the source's own top-1 approximate.
        let mut tx = self.client.transaction()?;
        tx.batch_execute("SET LOCAL enable_indexscan = off; SET LOCAL enable_bitmapscan = off")?;
        let row = tx.query_opt(&sql, &[&vector_text(qv)])?;
        tx.commit()?;
        Ok(row.and_then(|r| r.get::<_, Option<String>>(0)))
    }
}
