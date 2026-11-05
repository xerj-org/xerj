//! What a source is: the connection, the tables, and the options an operator
//! sets once with `xerj connect` / `PUT /_xerj/sources/{name}`.
//!
//! A credential is never part of the spec. The URL may name a user but not a
//! password; the password is a *reference* (`password_env` names a variable in
//! the node's environment, `password_file` names a file the node can read),
//! resolved by the worker each time it connects. That keeps the spec file, the
//! API echo and every log line free of secrets, the same rule the WAL tap
//! applies to its target URL.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// How changes are followed after the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Logical replication when the server and role allow it, otherwise
    /// watermark polling when a watermark column is set, otherwise an error
    /// that says what to change.
    #[default]
    Auto,
    /// Logical replication only; refuse to start without it.
    Logical,
    /// Poll `watermark_column` every `poll_secs`. Sees inserts and updates,
    /// never deletes.
    Poll,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceSpec {
    pub name: String,
    /// `postgres://user@host:5432/db?sslmode=require`. No password.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_file: Option<String>,
    /// `schema.table` or `table` (schema `public`). Empty = every table with
    /// a primary key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_tables: Vec<String>,
    /// Index names are `{index_prefix}-{schema}-{table}`. Default `src-{name}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_prefix: Option<String>,
    #[serde(default)]
    pub mode: Mode,
    /// An existing publication to stream from (least-privilege setups). When
    /// absent the connector creates `xerj_{name}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_column: Option<String>,
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// Hook for rows-to-facts: when set, every applied row change is also
    /// offered to the facts observer for this namespace (see `crate::facts`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts_namespace: Option<String>,
    /// Rows per table in the snapshot, for a trial run. Streaming still
    /// applies every change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_limit: Option<u64>,
}

fn default_poll_secs() -> u64 {
    30
}

/// Connection parameters parsed from the URL.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnParams {
    pub user: String,
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub sslmode: SslMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && name.as_bytes()[0].is_ascii_alphanumeric()
}

impl SourceSpec {
    pub fn validate(&self) -> Result<()> {
        if !valid_name(&self.name) {
            bail!(
                "source name {:?} must be 1-48 characters of a-z, 0-9, '-' or '_', starting with a letter or digit",
                self.name
            );
        }
        let _ = self.conn_params()?;
        if self.password_env.is_some() && self.password_file.is_some() {
            bail!("set password_env or password_file, not both");
        }
        if let Some(v) = &self.password_env {
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                bail!("password_env must be an environment variable name, got {v:?}");
            }
        }
        if self.mode == Mode::Poll && self.watermark_column.is_none() {
            bail!(
                "mode poll needs watermark_column: a column that grows on every insert and update \
                 (for example updated_at)"
            );
        }
        if self.poll_secs == 0 {
            bail!("poll_secs must be at least 1");
        }
        if let Some(p) = &self.index_prefix {
            if !valid_name(p) {
                bail!("index_prefix {p:?} must be lowercase a-z, 0-9, '-' or '_'");
            }
        }
        Ok(())
    }

    pub fn conn_params(&self) -> Result<ConnParams> {
        parse_url(&self.url)
    }

    pub fn index_prefix(&self) -> String {
        self.index_prefix
            .clone()
            .unwrap_or_else(|| format!("src-{}", self.name))
    }

    /// Index for one table.
    pub fn index_for(&self, schema: &str, table: &str) -> String {
        format!(
            "{}-{}-{}",
            self.index_prefix(),
            sanitize(schema),
            sanitize(table)
        )
    }

    /// Replication slot owned by this source.
    pub fn slot_name(&self) -> String {
        format!("xerj_{}", self.name.replace('-', "_"))
    }

    pub fn publication_name(&self) -> String {
        self.publication
            .clone()
            .unwrap_or_else(|| format!("xerj_{}", self.name.replace('-', "_")))
    }

    /// Does the operator's table filter select `schema.table`?
    pub fn selects(&self, schema: &str, table: &str) -> bool {
        let m = |pat: &str| {
            let (s, t) = match pat.split_once('.') {
                Some((s, t)) => (s, t),
                None => ("public", pat),
            };
            (s == "*" || s == schema) && (t == "*" || t == table)
        };
        if self.exclude_tables.iter().any(|p| m(p)) {
            return false;
        }
        self.tables.is_empty() || self.tables.iter().any(|p| m(p))
    }

    /// The password, resolved now from the reference. Never logged.
    pub fn resolve_password(&self) -> Result<Option<String>> {
        if let Some(var) = &self.password_env {
            let v = std::env::var(var).with_context(|| {
                format!(
                    "password_env names {var}, which is not set in the XERJ node's environment. \
                     Set it where the node starts (systemd Environment=, docker -e) and restart, \
                     or use password_file"
                )
            })?;
            return Ok(Some(v));
        }
        if let Some(path) = &self.password_file {
            let v = std::fs::read_to_string(path).with_context(|| {
                format!("password_file {path} is not readable by the XERJ node")
            })?;
            return Ok(Some(v.trim_end_matches(['\n', '\r']).to_string()));
        }
        Ok(None)
    }

    /// The URL with nothing secret in it, for display. A password never gets
    /// in (validation refuses it), so this only normalises.
    pub fn display_url(&self) -> String {
        match self.conn_params() {
            Ok(c) => format!("postgres://{}@{}:{}/{}", c.user, c.host, c.port, c.dbname),
            Err(_) => "<invalid url>".into(),
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse `postgres://user@host:port/db?sslmode=…`. A password in the URL is
/// refused with the way to pass one instead.
pub fn parse_url(url: &str) -> Result<ConnParams> {
    let rest = if let Some(r) = url.strip_prefix("postgres://") {
        r
    } else if let Some(r) = url.strip_prefix("postgresql://") {
        r
    } else if url.starts_with("mysql://") {
        bail!(
            "mysql:// sources are not in this build yet: Postgres is first (snapshot plus logical \
             replication). Until then, dump the database and run `xerj autoindex <folder>` on the \
             dump"
        );
    } else {
        bail!("a source URL starts with postgres:// (got {url:?})");
    };
    let (main, query) = match rest.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (rest, None),
    };
    let (auth, hostpath) = match main.rsplit_once('@') {
        Some((a, h)) => (Some(a), h),
        None => (None, main),
    };
    let user = match auth {
        Some(a) => {
            if a.contains(':') {
                bail!(
                    "the URL carries a password; XERJ never stores one. Remove it from the URL and \
                     pass password_env (a variable in the node's environment) or password_file"
                );
            }
            pct_decode(a)
        }
        None => std::env::var("USER").unwrap_or_else(|_| "postgres".into()),
    };
    let (hostport, db) = match hostpath.split_once('/') {
        Some((h, d)) => (h, pct_decode(d)),
        None => (hostpath, String::new()),
    };
    if hostport.is_empty() {
        bail!("the URL has no host");
    }
    let (host, port) = if let Some(h) = hostport.strip_prefix('[') {
        let (h, p) = h.split_once(']').context("unterminated [ipv6] host")?;
        let port = p.strip_prefix(':').map(|p| p.parse::<u16>()).transpose()?;
        (h.to_string(), port.unwrap_or(5432))
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().context("bad port")?),
            None => (hostport.to_string(), 5432),
        }
    };
    let mut sslmode = SslMode::Prefer;
    for kv in query.unwrap_or("").split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        match k {
            "sslmode" => {
                sslmode = match v {
                    "disable" => SslMode::Disable,
                    "allow" | "prefer" => SslMode::Prefer,
                    "require" | "verify-ca" | "verify-full" => SslMode::Require,
                    other => bail!("unknown sslmode {other:?}"),
                }
            }
            "password" => {
                bail!("the URL carries a password; pass password_env or password_file instead")
            }
            "application_name" | "connect_timeout" => {}
            other => bail!("unsupported URL parameter {other:?} (supported: sslmode)"),
        }
    }
    let dbname = if db.is_empty() { user.clone() } else { db };
    Ok(ConnParams {
        user,
        host,
        port,
        dbname,
        sslmode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(url: &str) -> SourceSpec {
        SourceSpec {
            name: "shop".into(),
            url: url.into(),
            password_env: None,
            password_file: None,
            tables: vec![],
            exclude_tables: vec![],
            index_prefix: None,
            mode: Mode::Auto,
            publication: None,
            watermark_column: None,
            poll_secs: 30,
            facts_namespace: None,
            snapshot_limit: None,
        }
    }

    #[test]
    fn a_url_is_parsed_into_user_host_port_db_and_tls_mode() {
        let c = parse_url("postgres://app@db.local:6543/shop?sslmode=require").unwrap();
        assert_eq!(
            c,
            ConnParams {
                user: "app".into(),
                host: "db.local".into(),
                port: 6543,
                dbname: "shop".into(),
                sslmode: SslMode::Require
            }
        );
        let c = parse_url("postgresql://app@[::1]/shop").unwrap();
        assert_eq!((c.host.as_str(), c.port), ("::1", 5432));
    }

    #[test]
    fn a_password_in_the_url_is_refused_and_the_message_says_what_to_use() {
        let e = parse_url("postgres://app:secret@h/db")
            .unwrap_err()
            .to_string();
        assert!(e.contains("password_env"), "{e}");
        assert!(
            !e.contains("secret"),
            "the refusal must not echo the password: {e}"
        );
        assert!(parse_url("postgres://app@h/db?password=x").is_err());
    }

    #[test]
    fn mysql_is_refused_with_the_interim_path() {
        let e = parse_url("mysql://u@h/db").unwrap_err().to_string();
        assert!(e.contains("not in this build"), "{e}");
    }

    #[test]
    fn table_filters_default_to_public_and_exclusions_win() {
        let mut s = spec("postgres://u@h/db");
        assert!(s.selects("public", "orders"));
        s.tables = vec!["orders".into(), "sales.*".into()];
        s.exclude_tables = vec!["sales.audit".into()];
        assert!(s.selects("public", "orders"));
        assert!(!s.selects("public", "users"));
        assert!(s.selects("sales", "invoices"));
        assert!(!s.selects("sales", "audit"));
    }

    #[test]
    fn names_slots_and_indices_are_derived_and_sanitised() {
        let mut s = spec("postgres://u@h/db");
        s.name = "shop-prod".into();
        assert_eq!(s.slot_name(), "xerj_shop_prod");
        assert_eq!(s.publication_name(), "xerj_shop_prod");
        assert_eq!(
            s.index_for("public", "Order Items"),
            "src-shop-prod-public-order_items"
        );
        s.validate().unwrap();
        s.name = "Bad Name".into();
        assert!(s.validate().is_err());
    }

    #[test]
    fn poll_mode_requires_a_watermark() {
        let mut s = spec("postgres://u@h/db");
        s.mode = Mode::Poll;
        assert!(s.validate().is_err());
        s.watermark_column = Some("updated_at".into());
        s.validate().unwrap();
    }
}
