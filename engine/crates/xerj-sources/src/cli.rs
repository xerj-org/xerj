//! `xerj connect` and `xerj sources`: thin clients of the node's
//! `/_xerj/sources` API, so a source added from the shell and one added by an
//! agent over HTTP or MCP are the same thing.

use crate::spec::{Mode, SourceSpec};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const CONNECT_HELP: &str = "\
xerj connect <postgres://user@host:port/db> [OPTIONS]

Register a live database source with a running XERJ node. The node snapshots
the selected tables, then streams every change in real time (Postgres logical
replication). Run once; it survives restarts.

OPTIONS:
    --name <NAME>              source name (default: the database name)
    --tables <LIST>            schema.table or table, comma-separated; `sales.*` (default: all with a primary key)
    --exclude-tables <LIST>    tables to leave out
    --password-env <VAR>       env var in the NODE's environment that holds the password
    --password-file <PATH>     file readable by the node that holds the password
    --publication <NAME>       stream from an existing publication (least-privilege setup)
    --mode <auto|logical|poll> auto (default): replication if allowed, else polling with --watermark-column
    --watermark-column <COL>   column that grows on every write, for polling (e.g. updated_at)
    --poll-secs <N>            polling interval (default 30)
    --index-prefix <P>         index names are <P>-<schema>-<table> (default src-<name>)
    --facts-namespace <NS>     also offer every row change to XERJ Query-1 facts
    --snapshot-limit <N>       rows per table in the snapshot (trial runs)
    --url <URL>                the node (default http://localhost:9200)
    --api-key <KEY>            node API key (or XERJ_API_KEY)
    --no-wait                  return as soon as the node accepted the source

The password never goes in the URL and is never stored: the node reads it from
--password-env / --password-file each time it connects.

Verify: xerj sources <NAME>     expect: state streaming
";

const SOURCES_HELP: &str = "\
xerj sources [NAME] [OPTIONS]

List live database sources on a node, or show one in detail.
`xerj sources --types` lists every way to get data in instead.

OPTIONS:
    --delete          stop the source, drop its replication slot (and the
                      publication XERJ created). Indexed documents stay.
    --delete-indices  with --delete: also delete the source's indices
    --json            print the API response
    --url <URL>       the node (default http://localhost:9200)
    --api-key <KEY>   node API key (or XERJ_API_KEY)
";

struct Client {
    base: String,
    key: Option<String>,
    http: reqwest::blocking::Client,
}

impl Client {
    fn new(url: Option<String>, key: Option<String>) -> Result<Self> {
        Ok(Self {
            base: url
                .unwrap_or_else(|| "http://localhost:9200".into())
                .trim_end_matches('/')
                .to_string(),
            key: key.or_else(|| std::env::var("XERJ_API_KEY").ok()),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()?,
        })
    }
    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let r = self.http.request(m, format!("{}{}", self.base, path));
        match &self.key {
            Some(k) => r.header("Authorization", format!("ApiKey {k}")),
            None => r,
        }
    }
    fn send(&self, rb: reqwest::blocking::RequestBuilder) -> Result<Value> {
        let resp = rb
            .send()
            .with_context(|| format!("cannot reach the XERJ node at {}", self.base))?;
        let code = resp.status();
        let body: Value = resp.json().unwrap_or(Value::Null);
        if !code.is_success() {
            let reason = body
                .pointer("/error/reason")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| body.to_string());
            bail!("{code}: {reason}");
        }
        Ok(body)
    }
}

fn take_value(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .filter(|v| !v.starts_with("--"))
        .ok_or_else(|| anyhow!("{flag} needs a value"))
}

fn list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn run_connect(args: &[String]) -> i32 {
    match connect(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            eprintln!("run `xerj connect --help` for usage.");
            2
        }
    }
}

fn connect(args: &[String]) -> Result<i32> {
    let mut url_arg: Option<String> = None;
    let mut spec = json!({});
    let (mut node, mut key, mut wait) = (None, None, true);
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => {
                print!("{CONNECT_HELP}");
                return Ok(0);
            }
            "--name" => spec["name"] = json!(take_value(args, &mut i, a)?),
            "--tables" => spec["tables"] = json!(list(&take_value(args, &mut i, a)?)),
            "--exclude-tables" => {
                spec["exclude_tables"] = json!(list(&take_value(args, &mut i, a)?))
            }
            "--password-env" => spec["password_env"] = json!(take_value(args, &mut i, a)?),
            "--password-file" => spec["password_file"] = json!(take_value(args, &mut i, a)?),
            "--publication" => spec["publication"] = json!(take_value(args, &mut i, a)?),
            "--mode" => {
                let m = take_value(args, &mut i, a)?;
                if !["auto", "logical", "poll"].contains(&m.as_str()) {
                    bail!("--mode is auto, logical or poll");
                }
                spec["mode"] = json!(m)
            }
            "--watermark-column" => spec["watermark_column"] = json!(take_value(args, &mut i, a)?),
            "--poll-secs" => {
                spec["poll_secs"] = json!(take_value(args, &mut i, a)?.parse::<u64>()?)
            }
            "--index-prefix" => spec["index_prefix"] = json!(take_value(args, &mut i, a)?),
            "--facts-namespace" => spec["facts_namespace"] = json!(take_value(args, &mut i, a)?),
            "--snapshot-limit" => {
                spec["snapshot_limit"] = json!(take_value(args, &mut i, a)?.parse::<u64>()?)
            }
            "--url" => node = Some(take_value(args, &mut i, a)?),
            "--api-key" => key = Some(take_value(args, &mut i, a)?),
            "--no-wait" => wait = false,
            s if s.starts_with('-') => bail!("unknown option {s}"),
            s => {
                if url_arg.replace(s.to_string()).is_some() {
                    bail!("one database URL per `xerj connect`");
                }
            }
        }
        i += 1;
    }
    let Some(db_url) = url_arg else {
        print!("{CONNECT_HELP}");
        return Ok(2);
    };
    let cp = crate::spec::parse_url(&db_url)?;
    let name = spec
        .get("name")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| {
            cp.dbname
                .to_ascii_lowercase()
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect()
        });
    spec["name"] = json!(name);
    spec["url"] = json!(db_url);
    // Validate locally first, so a typo is reported without a round trip.
    let parsed: SourceSpec = serde_json::from_value(spec.clone())?;
    parsed.validate()?;
    if parsed.mode == Mode::Poll && parsed.watermark_column.is_none() {
        bail!("--mode poll needs --watermark-column");
    }
    let c = Client::new(node, key)?;
    let body = c.send(
        c.req(reqwest::Method::PUT, &format!("/_xerj/sources/{name}"))
            .json(&spec),
    )?;
    println!(
        "source {name} registered on {} ({})",
        c.base,
        body.get("result").and_then(Value::as_str).unwrap_or("ok")
    );
    if !wait {
        return Ok(0);
    }
    let start = Instant::now();
    let mut last = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let s = c.send(c.req(reqwest::Method::GET, &format!("/_xerj/sources/{name}")))?;
        let st = s
            .pointer("/status/state")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let line = match st {
            "snapshotting" => format!(
                "snapshotting {:.0}% ({} rows, {:.0} rows/s, ETA {} s)",
                s.pointer("/status/snapshot/percent")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                s.pointer("/status/snapshot/rows_done")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                s.pointer("/status/snapshot/rows_per_sec")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                s.pointer("/status/snapshot/eta_secs")
                    .and_then(Value::as_u64)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".into()),
            ),
            other => other.to_string(),
        };
        if line != last {
            eprintln!("{name}: {line}");
            last = line;
        }
        match st {
            "streaming" | "polling" => {
                println!("{name} is live ({st}). Indices:");
                for t in s
                    .pointer("/status/tables")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    println!(
                        "  {} -> {} ({} rows)",
                        t["table"].as_str().unwrap_or(""),
                        t["index"].as_str().unwrap_or(""),
                        t["rows_indexed"]
                    );
                }
                println!("Verify: xerj sources {name}");
                return Ok(0);
            }
            "error" => {
                eprintln!(
                    "{name}: {}",
                    s.pointer("/status/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                );
                if let Some(h) = s
                    .pointer("/status/error/hint")
                    .and_then(Value::as_str)
                    .filter(|h| !h.is_empty())
                {
                    eprintln!("fix: {h}");
                }
                eprintln!("The node keeps retrying; `xerj sources {name}` shows the state.");
                return Ok(1);
            }
            _ => {}
        }
        if start.elapsed() > Duration::from_secs(3600) {
            eprintln!("still {st} after an hour; it keeps running on the node.");
            return Ok(0);
        }
    }
}

pub fn run_sources(args: &[String]) -> i32 {
    match sources(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    }
}

fn sources(args: &[String]) -> Result<i32> {
    let (mut name, mut node, mut key) = (None::<String>, None, None);
    let (mut delete, mut delete_indices, mut as_json) = (false, false, false);
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => {
                print!("{SOURCES_HELP}");
                return Ok(0);
            }
            "--delete" => delete = true,
            "--delete-indices" => delete_indices = true,
            "--json" => as_json = true,
            "--url" => node = Some(take_value(args, &mut i, a)?),
            "--api-key" => key = Some(take_value(args, &mut i, a)?),
            s if s.starts_with('-') => bail!("unknown option {s}"),
            s => name = Some(s.to_string()),
        }
        i += 1;
    }
    let c = Client::new(node, key)?;
    if delete {
        let n = name.ok_or_else(|| anyhow!("--delete needs a source name"))?;
        let q = if delete_indices {
            "?delete_indices=true"
        } else {
            ""
        };
        let b = c.send(c.req(reqwest::Method::DELETE, &format!("/_xerj/sources/{n}{q}")))?;
        if as_json {
            println!("{}", serde_json::to_string_pretty(&b)?);
        } else {
            println!("source {n} deleted");
            for d in b
                .get("cleanup")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                println!("  {}", d.as_str().unwrap_or(""));
            }
        }
        return Ok(0);
    }
    let path = match &name {
        Some(n) => format!("/_xerj/sources/{n}"),
        None => "/_xerj/sources".into(),
    };
    let b = c.send(c.req(reqwest::Method::GET, &path))?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&b)?);
        return Ok(0);
    }
    let items: Vec<Value> = match &name {
        Some(_) => vec![b.clone()],
        None => b
            .get("sources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    };
    if items.is_empty() {
        println!(
            "no sources. Add one: xerj connect postgres://user@host/db --password-env PGPASSWORD"
        );
        return Ok(0);
    }
    println!(
        "{:<20} {:<12} {:<8} {:>10} {:>10} {:>12}  URL",
        "NAME", "STATE", "MODE", "LAG_S", "ROWS/S", "CONFIRMED"
    );
    for s in &items {
        let st = &s["status"];
        println!(
            "{:<20} {:<12} {:<8} {:>10.1} {:>10.0} {:>12}  {}",
            s["name"].as_str().unwrap_or(""),
            st["state"].as_str().unwrap_or(""),
            st["mode"].as_str().unwrap_or(""),
            st.pointer("/stream/lag_secs")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            st.pointer("/stream/rows_per_sec")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            st.pointer("/stream/confirmed_lsn")
                .and_then(Value::as_str)
                .unwrap_or(""),
            s["url"].as_str().unwrap_or(""),
        );
        if name.is_some() {
            if let Some(e) = st.get("error").filter(|e| !e.is_null()) {
                println!("  error: {}", e["message"].as_str().unwrap_or(""));
                if let Some(h) = e["hint"].as_str().filter(|h| !h.is_empty()) {
                    println!("  fix:   {h}");
                }
            }
            if st["state"] == "snapshotting" {
                println!(
                    "  snapshot: {:.0}% ({} of ~{} rows, ETA {} s)",
                    st.pointer("/snapshot/percent")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                    st.pointer("/snapshot/rows_done")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    st.pointer("/snapshot/rows_estimate")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    st.pointer("/snapshot/eta_secs")
                        .and_then(Value::as_u64)
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "?".into())
                );
            }
            for t in st["tables"].as_array().cloned().unwrap_or_default() {
                println!(
                    "  {} -> {} ({} rows)",
                    t["table"].as_str().unwrap_or(""),
                    t["index"].as_str().unwrap_or(""),
                    t["rows_indexed"]
                );
            }
            for sk in st["skipped"].as_array().cloned().unwrap_or_default() {
                println!("  skipped: {}", sk.as_str().unwrap_or(""));
            }
            for n in st["notes"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .rev()
                .take(5)
            {
                println!("  note: {}", n.as_str().unwrap_or(""));
            }
        }
    }
    Ok(0)
}
