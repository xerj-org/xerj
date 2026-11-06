//! Argument parsing for `xerj autoindex <vector-source>`.
//!
//! Kept apart from the folder CLI (`crate::cli`) on purpose: none of the
//! folder flags (`--prefix`, `--no-graph`, `--watch` …) mean anything for a
//! collection copy, and a flag that is accepted and ignored is the defect
//! class this repo refuses (#204). Unknown flags are errors here.

use std::path::PathBuf;

use super::Similarity;

pub const VECTOR_SCHEMES: [&str; 5] = [
    "qdrant://",
    "qdrants://",
    "chroma://",
    "chromas://",
    "pinecone://",
];

#[derive(Debug, Clone, PartialEq)]
pub enum SourceSpec {
    /// `base` is `http(s)://host:port`.
    Qdrant { base: String, collection: String },
    Chroma {
        base: String,
        collection: String,
        tenant: String,
        database: String,
    },
    Pinecone {
        index: String,
        namespace: Option<String>,
    },
    /// `conn` is the libpq-style URL as given.
    PgVector { conn: String, table: String },
}

#[derive(Debug, Clone)]
pub struct VecImportCfg {
    pub source: SourceSpec,
    pub url: String,
    pub api_key: Option<String>,
    pub source_api_key: Option<String>,
    pub index: Option<String>,
    pub vector_field: String,
    pub vector_name: Option<String>,
    pub batch: usize,
    /// Concurrent `_bulk` writers.
    pub workers: usize,
    pub limit: Option<usize>,
    pub dry_run: bool,
    pub fresh: bool,
    pub verify: usize,
    pub similarity: Option<Similarity>,
    pub state_dir: Option<PathBuf>,
    pub progress_secs: u64,
    pub id_column: Option<String>,
    pub vector_column: Option<String>,
    pub pinecone_host: Option<String>,
    pub pinecone_controller: Option<String>,
}

impl VecImportCfg {
    /// The source as printed and journaled: credentials removed.
    pub fn source_display(&self) -> String {
        match &self.source {
            SourceSpec::Qdrant { base, collection } => format!("qdrant {base}/{collection}"),
            SourceSpec::Chroma {
                base,
                collection,
                tenant,
                database,
            } => {
                format!("chroma {base}/{tenant}/{database}/{collection}")
            }
            SourceSpec::Pinecone { index, namespace } => {
                format!("pinecone {index}/{}", namespace.as_deref().unwrap_or(""))
            }
            SourceSpec::PgVector { conn, table } => {
                format!("pgvector {}/{table}", redact_url(conn))
            }
        }
    }
}

/// Is this argv a vector-source import? True when the first non-flag argument
/// uses a vector scheme, or when `--pgvector-table` is present (pgvector is a
/// `postgres://` URL, which a live-SQL source may also claim; the flag is what
/// makes it a vector import).
pub fn looks_like_vector_source(args: &[String]) -> bool {
    if args
        .iter()
        .any(|a| a == "--pgvector-table" || a.starts_with("--pgvector-table="))
    {
        return true;
    }
    // Any argument, not just the first non-flag one: a flag value such as
    // `--url http://…` comes before the source on many command lines.
    args.iter()
        .any(|a| VECTOR_SCHEMES.iter().any(|s| a.starts_with(s)))
}

/// Remove `user:password@` from a URL for display.
pub fn redact_url(u: &str) -> String {
    match (u.find("://"), u.rfind('@')) {
        (Some(s), Some(at)) if at > s => format!("{}***@{}", &u[..s + 3], &u[at + 1..]),
        _ => u.to_string(),
    }
}

/// Default XERJ index name for a collection: `vec-<name>`, lowercased, with
/// anything outside `[a-z0-9_-]` turned into `-`.
pub fn default_index_name(collection: &str) -> String {
    let s: String = collection
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("vec-{}", s.trim_matches('-'))
}

fn split_query(rest: &str) -> (&str, Vec<(String, String)>) {
    match rest.split_once('?') {
        None => (rest, vec![]),
        Some((p, q)) => (
            p,
            q.split('&')
                .filter(|kv| !kv.is_empty())
                .map(|kv| match kv.split_once('=') {
                    Some((k, v)) => (k.to_string(), v.to_string()),
                    None => (kv.to_string(), String::new()),
                })
                .collect(),
        ),
    }
}

/// Parse `scheme://host:port/collection[?q]` into (`http(s)://host:port`, collection, query).
fn parse_hostpath(
    url: &str,
    scheme: &str,
    tls: bool,
    default_port: u16,
) -> Result<(String, String, Vec<(String, String)>), String> {
    let rest = &url[scheme.len()..];
    let (path, q) = split_query(rest);
    let (host, coll) = path
        .split_once('/')
        .ok_or_else(|| format!("{url}: expected {scheme}host:port/<collection>"))?;
    if host.is_empty() || coll.is_empty() || coll.contains('/') {
        return Err(format!("{url}: expected {scheme}host:port/<collection>"));
    }
    let host = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:{default_port}")
    };
    let proto = if tls { "https" } else { "http" };
    Ok((format!("{proto}://{host}"), coll.to_string(), q))
}

pub fn parse_source(url: &str, pg_table: Option<String>) -> Result<SourceSpec, String> {
    if let Some(table) = pg_table {
        if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
            return Err("--pgvector-table needs a postgres:// connection URL as the source".into());
        }
        return Ok(SourceSpec::PgVector {
            conn: url.to_string(),
            table,
        });
    }
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        return Err(
            "a postgres:// source is imported as vectors only with --pgvector-table <table>".into(),
        );
    }
    for (scheme, tls) in [("qdrants://", true), ("qdrant://", false)] {
        if url.starts_with(scheme) {
            let (base, collection, q) = parse_hostpath(url, scheme, tls, 6333)?;
            if let Some((k, _)) = q.first() {
                return Err(format!("{url}: unknown query parameter `{k}`"));
            }
            return Ok(SourceSpec::Qdrant { base, collection });
        }
    }
    for (scheme, tls) in [("chromas://", true), ("chroma://", false)] {
        if url.starts_with(scheme) {
            let (base, collection, q) = parse_hostpath(url, scheme, tls, 8000)?;
            let mut tenant = "default_tenant".to_string();
            let mut database = "default_database".to_string();
            for (k, v) in q {
                match k.as_str() {
                    "tenant" => tenant = v,
                    "database" => database = v,
                    other => {
                        return Err(format!(
                            "{url}: unknown query parameter `{other}` (tenant, database)"
                        ))
                    }
                }
            }
            return Ok(SourceSpec::Chroma {
                base,
                collection,
                tenant,
                database,
            });
        }
    }
    if let Some(rest) = url.strip_prefix("pinecone://") {
        let (index, q) = split_query(rest);
        if index.is_empty() || index.contains('/') {
            return Err(format!(
                "{url}: expected pinecone://<index-name>[?namespace=<ns>]"
            ));
        }
        let mut namespace = None;
        for (k, v) in q {
            match k.as_str() {
                "namespace" => namespace = Some(v),
                other => {
                    return Err(format!(
                        "{url}: unknown query parameter `{other}` (namespace)"
                    ))
                }
            }
        }
        return Ok(SourceSpec::Pinecone {
            index: index.to_string(),
            namespace,
        });
    }
    Err(format!("{url}: not a vector source (qdrant://, chroma://, pinecone://, or postgres:// with --pgvector-table)"))
}

/// `Ok(None)` = help was asked for.
pub fn parse(args: Vec<String>) -> Result<Option<VecImportCfg>, String> {
    let mut it = args.into_iter();
    let mut positional: Option<String> = None;
    let mut url = "http://localhost:9200".to_string();
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut source_api_key: Option<String> = None;
    let mut index = None;
    let mut vector_field = "vector".to_string();
    let mut vector_name = None;
    let mut batch = 500usize;
    let mut workers = 4usize;
    let mut limit = None;
    let mut dry_run = false;
    let mut fresh = false;
    let mut verify = 100usize;
    let mut similarity = None;
    let mut state_dir = None;
    let mut progress_secs = 5u64;
    let mut pg_table = None;
    let mut id_column = None;
    let mut vector_column = None;
    let mut pinecone_host = None;
    let pinecone_controller = std::env::var("PINECONE_CONTROLLER_URL")
        .ok()
        .filter(|s| !s.is_empty());

    fn val(
        it: &mut impl Iterator<Item = String>,
        flag: &str,
        inline: Option<String>,
    ) -> Result<String, String> {
        match inline {
            Some(v) => Ok(v),
            None => it
                .next()
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| format!("{flag} needs a value")),
        }
    }
    fn num<T: std::str::FromStr>(s: &str, flag: &str) -> Result<T, String> {
        s.parse()
            .map_err(|_| format!("{flag}: `{s}` is not a number"))
    }

    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if a.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        match flag.as_str() {
            "-h" | "--help" | "--help-vector" => return Ok(None),
            "--url" => url = val(&mut it, &flag, inline)?,
            "--api-key" => api_key = Some(val(&mut it, &flag, inline)?),
            "--source-api-key" => source_api_key = Some(val(&mut it, &flag, inline)?),
            "--index" => index = Some(val(&mut it, &flag, inline)?),
            "--vector-field" => vector_field = val(&mut it, &flag, inline)?,
            "--vector-name" => vector_name = Some(val(&mut it, &flag, inline)?),
            "--batch" => {
                batch = num(&val(&mut it, &flag, inline)?, &flag)?;
                if batch == 0 || batch > 10_000 {
                    return Err("--batch must be between 1 and 10000".into());
                }
            }
            "--workers" => {
                workers = num(&val(&mut it, &flag, inline)?, &flag)?;
                if workers == 0 || workers > 64 {
                    return Err("--workers must be between 1 and 64".into());
                }
            }
            "--limit" => limit = Some(num(&val(&mut it, &flag, inline)?, &flag)?),
            "--dry-run" => dry_run = true,
            "--fresh" => fresh = true,
            "--verify" => verify = num(&val(&mut it, &flag, inline)?, &flag)?,
            "--similarity" => {
                let v = val(&mut it, &flag, inline)?;
                similarity = Some(Similarity::parse_user(&v).ok_or_else(|| {
                    format!("--similarity `{v}`: use cosine, dot (max_inner_product) or l2")
                })?);
            }
            "--state-dir" => state_dir = Some(PathBuf::from(val(&mut it, &flag, inline)?)),
            "--progress-interval" => progress_secs = num(&val(&mut it, &flag, inline)?, &flag)?,
            "--pgvector-table" => pg_table = Some(val(&mut it, &flag, inline)?),
            "--id-column" => id_column = Some(val(&mut it, &flag, inline)?),
            "--vector-column" => vector_column = Some(val(&mut it, &flag, inline)?),
            "--pinecone-host" => pinecone_host = Some(val(&mut it, &flag, inline)?),
            f if f.starts_with('-') => {
                return Err(format!(
                    "{f} is not a vector-import flag (run `xerj autoindex --help-vector`)"
                ))
            }
            _ => {
                if positional.is_some() {
                    return Err(format!("unexpected extra argument `{a}`"));
                }
                positional = Some(a);
            }
        }
    }
    let positional = positional.ok_or("missing the source URL")?;
    let source = parse_source(&positional, pg_table)?;
    let is_pg = matches!(source, SourceSpec::PgVector { .. });
    if !is_pg && (id_column.is_some() || vector_column.is_some()) {
        return Err("--id-column/--vector-column apply only to a pgvector source".into());
    }
    let is_pc = matches!(source, SourceSpec::Pinecone { .. });
    if !is_pc && pinecone_host.is_some() {
        return Err("--pinecone-host applies only to a pinecone:// source".into());
    }
    if vector_name.is_some() && !matches!(source, SourceSpec::Qdrant { .. }) {
        return Err("--vector-name applies only to a Qdrant collection with named vectors".into());
    }
    if source_api_key.is_none() {
        let env = match &source {
            SourceSpec::Qdrant { .. } => "QDRANT_API_KEY",
            SourceSpec::Chroma { .. } => "CHROMA_TOKEN",
            SourceSpec::Pinecone { .. } => "PINECONE_API_KEY",
            SourceSpec::PgVector { .. } => "",
        };
        if !env.is_empty() {
            source_api_key = std::env::var(env).ok().filter(|s| !s.is_empty());
        }
    }
    if is_pc && source_api_key.is_none() {
        return Err(
            "a Pinecone source needs an API key: set PINECONE_API_KEY or pass --source-api-key"
                .into(),
        );
    }
    if !dry_run && limit == Some(0) {
        return Err("--limit 0 imports nothing; drop it or use --dry-run".into());
    }
    if dry_run && fresh {
        return Err(
            "--fresh resets the resume journal of an import; a --dry-run writes nothing. Drop one"
                .into(),
        );
    }
    if vector_field.is_empty() || vector_field == "document" || vector_field.starts_with('_') {
        return Err("--vector-field must be a plain field name other than `document`".into());
    }
    Ok(Some(VecImportCfg {
        source,
        url,
        api_key,
        source_api_key,
        index,
        vector_field,
        vector_name,
        batch,
        workers,
        limit,
        dry_run,
        fresh,
        verify,
        similarity,
        state_dir,
        progress_secs: progress_secs.max(1),
        id_column,
        vector_column,
        pinecone_host,
        pinecone_controller,
    }))
}

pub fn print_help() {
    print!(
        "\
xerj autoindex <vector-source> [OPTIONS]

Copy an existing vector collection into XERJ: vectors, ids and payload.
The index is created with a dense_vector field of the source's dimensions and
similarity; ids are kept, so a re-run overwrites instead of duplicating.

SOURCES
    qdrant://host:6333/<collection>          (qdrants:// for HTTPS)
    chroma://host:8000/<collection>[?tenant=t&database=d]   (chromas:// for HTTPS)
    pinecone://<index-name>[?namespace=ns]   (serverless indexes; list + fetch)
    postgres://user:pass@host:5432/db --pgvector-table <table>

OPTIONS
    --url <URL>              XERJ endpoint (default http://localhost:9200)
    --api-key <KEY>          XERJ API key (default $XERJ_API_KEY)
    --source-api-key <KEY>   source key; defaults: $QDRANT_API_KEY, $CHROMA_TOKEN,
                             $PINECONE_API_KEY
    --index <NAME>           target index (default vec-<collection>)
    --vector-field <NAME>    dense_vector field name (default vector)
    --vector-name <NAME>     Qdrant named vector to import
    --similarity <S>         override: cosine | dot | l2
    --batch <N>              points per page and per _bulk (default 500)
    --workers <N>            concurrent _bulk writers (default 4)
    --limit <N>              import at most N points
    --dry-run                read --limit points (default 5), print the plan, write nothing
    --fresh                  ignore the resume journal and start from the first point
    --verify <N>             sample N points and compare top-1 with the source (default 100, 0 = off)
    --state-dir <DIR>        journal directory (default ~/.xerj/vecimport)
    --progress-interval <S>  seconds between progress lines (default 5)
    --id-column <COL>        pgvector: key column (default: the table's primary key)
    --vector-column <COL>    pgvector: vector column (default: the only vector column)
    --pinecone-host <URL>    Pinecone index host (default: looked up from the control plane)

VERIFY
    After the copy, every sampled point is searched for in XERJ (it must be its
    own top-1), and a slightly perturbed copy of it is sent to both the source
    and XERJ; the share of identical top-1 ids is printed.

EXAMPLES
    xerj autoindex qdrant://localhost:6333/docs
    xerj autoindex chroma://localhost:8000/notes --dry-run
    PINECONE_API_KEY=… xerj autoindex pinecone://products?namespace=en
    xerj autoindex postgres://app@localhost/app --pgvector-table items
"
    );
}
