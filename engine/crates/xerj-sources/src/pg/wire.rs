//! A small synchronous Postgres client: startup, authentication (trust,
//! cleartext, MD5, SCRAM-SHA-256), TLS, the simple query protocol, `COPY ...
//! TO STDOUT`, and the CopyBoth sub-protocol that streaming replication uses.
//!
//! Message encoding and SCRAM come from `postgres-protocol`. The framing is
//! ours because replication needs two things the published sync client does
//! not offer: a connection started with `replication=database`, and CopyBoth
//! with a read timeout that never tears a message in half. Reads go through a
//! resumable buffer: a timeout before a whole message has arrived returns
//! "nothing yet" and keeps the partial bytes for the next call.

use crate::spec::{ConnParams, SslMode};
use anyhow::{anyhow, bail, Context, Result};
use bytes::BytesMut;
use postgres_protocol::authentication::sasl::{ChannelBinding, ScramSha256};
use postgres_protocol::message::frontend;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

trait Io: Read + Write + Send {}
impl<T: Read + Write + Send> Io for T {}

pub struct PgConn {
    io: Box<dyn Io>,
    tcp: TcpStream,
    pending: Vec<u8>,
    pub server_version: String,
}

/// A Postgres error, kept structured so callers can turn the SQLSTATE into a
/// fix hint.
#[derive(Debug, Clone)]
pub struct PgError {
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
}

impl std::fmt::Display for PgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "postgres {}: {}", self.code, self.message)?;
        if let Some(d) = &self.detail {
            write!(f, " ({d})")?;
        }
        Ok(())
    }
}
impl std::error::Error for PgError {}

fn parse_error(body: &[u8]) -> PgError {
    let mut e = PgError {
        code: String::new(),
        message: String::new(),
        detail: None,
        hint: None,
    };
    for part in body.split(|b| *b == 0) {
        if part.is_empty() {
            continue;
        }
        let v = String::from_utf8_lossy(&part[1..]).into_owned();
        match part[0] {
            b'C' => e.code = v,
            b'M' => e.message = v,
            b'D' => e.detail = Some(v),
            b'H' => e.hint = Some(v),
            _ => {}
        }
    }
    e
}

#[derive(Debug, Default, Clone)]
pub struct QueryResult {
    /// (name, type OID)
    pub columns: Vec<(String, u32)>,
    pub rows: Vec<Vec<Option<String>>>,
}

impl QueryResult {
    pub fn col(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|(n, _)| n == name)
    }
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

impl PgConn {
    /// Open a connection. `replication` starts a walsender session
    /// (`replication=database`) that accepts replication commands.
    pub fn connect(cp: &ConnParams, password: Option<&str>, replication: bool) -> Result<Self> {
        let addr = (cp.host.as_str(), cp.port);
        let tcp = std::net::ToSocketAddrs::to_socket_addrs(&addr)
            .with_context(|| format!("resolve {}:{}", cp.host, cp.port))?
            .find_map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(10)).ok())
            .ok_or_else(|| {
                anyhow!(
                    "cannot reach {}:{} (connection refused or timed out)",
                    cp.host,
                    cp.port
                )
            })?;
        tcp.set_nodelay(true).ok();
        tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut tcp_io = tcp.try_clone()?;
        let io: Box<dyn Io> = if cp.sslmode == SslMode::Disable {
            Box::new(tcp.try_clone()?)
        } else {
            let mut buf = BytesMut::new();
            frontend::ssl_request(&mut buf);
            tcp_io.write_all(&buf)?;
            let mut answer = [0u8; 1];
            tcp_io.read_exact(&mut answer)?;
            if answer[0] == b'S' {
                let name = rustls::pki_types::ServerName::try_from(cp.host.clone())
                    .map_err(|_| anyhow!("{} is not a valid TLS server name", cp.host))?;
                let conn = rustls::ClientConnection::new(tls_config(), name)?;
                Box::new(rustls::StreamOwned::new(conn, tcp.try_clone()?))
            } else if cp.sslmode == SslMode::Require {
                bail!(
                    "{} does not accept TLS and the URL says sslmode=require",
                    cp.host
                );
            } else {
                Box::new(tcp.try_clone()?)
            }
        };
        let mut c = PgConn {
            io,
            tcp,
            pending: Vec::new(),
            server_version: String::new(),
        };
        let mut params: Vec<(&str, &str)> = vec![
            ("user", &cp.user),
            ("database", &cp.dbname),
            ("application_name", "xerj-source"),
            ("client_encoding", "UTF8"),
            ("DateStyle", "ISO"),
        ];
        if replication {
            params.push(("replication", "database"));
        }
        let mut buf = BytesMut::new();
        frontend::startup_message(params.iter().copied(), &mut buf)?;
        c.io.write_all(&buf)?;
        c.authenticate(&cp.user, password)?;
        Ok(c)
    }

    fn authenticate(&mut self, user: &str, password: Option<&str>) -> Result<()> {
        let need_pw = || {
            anyhow!(
                "the database asked for a password and none is configured: set password_env or \
                 password_file on the source"
            )
        };
        loop {
            let (tag, body) = self.read_msg_blocking()?;
            match tag {
                b'R' => {
                    let code = be_u32(&body);
                    let mut buf = BytesMut::new();
                    match code {
                        0 => {}
                        3 => {
                            let pw = password.ok_or_else(need_pw)?;
                            frontend::password_message(pw.as_bytes(), &mut buf)?;
                            self.io.write_all(&buf)?;
                        }
                        5 => {
                            let pw = password.ok_or_else(need_pw)?;
                            let salt = [body[4], body[5], body[6], body[7]];
                            let hash = postgres_protocol::authentication::md5_hash(
                                user.as_bytes(),
                                pw.as_bytes(),
                                salt,
                            );
                            frontend::password_message(hash.as_bytes(), &mut buf)?;
                            self.io.write_all(&buf)?;
                        }
                        10 => {
                            let pw = password.ok_or_else(need_pw)?;
                            let mechs = String::from_utf8_lossy(&body[4..]);
                            if !mechs.split('\0').any(|m| m == "SCRAM-SHA-256") {
                                bail!(
                                    "the server offers only {mechs:?}; XERJ speaks SCRAM-SHA-256"
                                );
                            }
                            let mut scram =
                                ScramSha256::new(pw.as_bytes(), ChannelBinding::unsupported());
                            frontend::sasl_initial_response(
                                "SCRAM-SHA-256",
                                scram.message(),
                                &mut buf,
                            )?;
                            self.io.write_all(&buf)?;
                            let (t, b) = self.read_msg_blocking()?;
                            if t == b'E' {
                                return Err(parse_error(&b).into());
                            }
                            if t != b'R' || be_u32(&b) != 11 {
                                bail!("unexpected SASL message {:?}", t as char);
                            }
                            scram.update(&b[4..])?;
                            let mut buf = BytesMut::new();
                            frontend::sasl_response(scram.message(), &mut buf)?;
                            self.io.write_all(&buf)?;
                            let (t, b) = self.read_msg_blocking()?;
                            if t == b'E' {
                                return Err(parse_error(&b).into());
                            }
                            if t != b'R' || be_u32(&b) != 12 {
                                bail!("unexpected SASL final {:?}", t as char);
                            }
                            scram.finish(&b[4..])?;
                        }
                        other => bail!("unsupported authentication method {other}"),
                    }
                }
                b'S' => {
                    let mut it = body.split(|b| *b == 0);
                    let k = it.next().unwrap_or_default();
                    let v = it.next().unwrap_or_default();
                    if k == b"server_version" {
                        self.server_version = String::from_utf8_lossy(v).into_owned();
                    }
                }
                b'K' | b'N' => {}
                b'Z' => return Ok(()),
                b'E' => return Err(parse_error(&body).into()),
                other => bail!("unexpected message {:?} during startup", other as char),
            }
        }
    }

    /// Read one whole message, waiting as long as it takes (the socket's 30 s
    /// read timeout is retried; a dead peer surfaces as an EOF error).
    fn read_msg_blocking(&mut self) -> Result<(u8, Vec<u8>)> {
        let start = std::time::Instant::now();
        loop {
            if let Some(m) = self.try_read_msg()? {
                return Ok(m);
            }
            if start.elapsed() > Duration::from_secs(600) {
                bail!("no answer from the database for 10 minutes");
            }
        }
    }

    /// One whole message if one arrives before the read timeout, else `None`.
    pub fn try_read_msg(&mut self) -> Result<Option<(u8, Vec<u8>)>> {
        loop {
            if self.pending.len() >= 5 {
                let len = be_u32(&self.pending[1..5]) as usize;
                if len < 4 {
                    bail!("corrupt message length {len}");
                }
                if self.pending.len() > len {
                    let tag = self.pending[0];
                    let body = self.pending[5..1 + len].to_vec();
                    self.pending.drain(..1 + len);
                    return Ok(Some((tag, body)));
                }
            }
            let mut chunk = [0u8; 64 * 1024];
            match self.io.read(&mut chunk) {
                Ok(0) => bail!("the database closed the connection"),
                Ok(n) => self.pending.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Ok(None)
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    pub fn set_read_timeout(&self, d: Duration) -> Result<()> {
        self.tcp.set_read_timeout(Some(d))?;
        Ok(())
    }

    fn send_query(&mut self, sql: &str) -> Result<()> {
        let mut buf = BytesMut::new();
        frontend::query(sql, &mut buf)?;
        self.io.write_all(&buf)?;
        Ok(())
    }

    /// Run SQL with the simple query protocol. Returns the last result set
    /// that had rows (or an empty one).
    pub fn simple_query(&mut self, sql: &str) -> Result<QueryResult> {
        self.send_query(sql)?;
        let mut cur = QueryResult::default();
        let mut last = QueryResult::default();
        let mut err: Option<PgError> = None;
        loop {
            let (tag, body) = self.read_msg_blocking()?;
            match tag {
                b'T' => {
                    cur = QueryResult::default();
                    let n = u16::from_be_bytes([body[0], body[1]]) as usize;
                    let mut p = 2;
                    for _ in 0..n {
                        let end = p + body[p..].iter().position(|b| *b == 0).unwrap_or(0);
                        let name = String::from_utf8_lossy(&body[p..end]).into_owned();
                        p = end + 1;
                        let type_oid = be_u32(&body[p + 6..p + 10]);
                        p += 18;
                        cur.columns.push((name, type_oid));
                    }
                }
                b'D' => {
                    let n = u16::from_be_bytes([body[0], body[1]]) as usize;
                    let mut p = 2;
                    let mut row = Vec::with_capacity(n);
                    for _ in 0..n {
                        let len =
                            i32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]);
                        p += 4;
                        if len < 0 {
                            row.push(None);
                        } else {
                            let l = len as usize;
                            row.push(Some(String::from_utf8_lossy(&body[p..p + l]).into_owned()));
                            p += l;
                        }
                    }
                    cur.rows.push(row);
                }
                b'C' => {
                    if !cur.columns.is_empty() {
                        last = std::mem::take(&mut cur);
                    }
                }
                b'E' => err = Some(parse_error(&body)),
                b'Z' => {
                    return match err {
                        Some(e) => Err(e.into()),
                        None => Ok(last),
                    }
                }
                b'I' | b'N' | b'S' => {}
                other => bail!("unexpected message {:?} in a query", other as char),
            }
        }
    }

    /// `COPY ... TO STDOUT` in text format; `on_line` gets each row without
    /// its trailing newline. Returns the row count.
    pub fn copy_out(
        &mut self,
        sql: &str,
        mut on_line: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<u64> {
        self.send_query(sql)?;
        let mut rows = 0u64;
        let mut err: Option<anyhow::Error> = None;
        let mut carry: Vec<u8> = Vec::new();
        loop {
            let (tag, body) = self.read_msg_blocking()?;
            match tag {
                b'H' => {}
                b'd' => {
                    if err.is_some() {
                        continue;
                    }
                    carry.extend_from_slice(&body);
                    while let Some(nl) = carry.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = carry.drain(..=nl).collect();
                        if let Err(e) = on_line(&line[..line.len() - 1]) {
                            err = Some(e);
                            break;
                        }
                        rows += 1;
                    }
                }
                b'c' | b'C' | b'N' | b'S' => {}
                b'E' => err = Some(parse_error(&body).into()),
                b'Z' => {
                    return match err {
                        Some(e) => Err(e),
                        None => Ok(rows),
                    }
                }
                other => bail!("unexpected message {:?} in COPY", other as char),
            }
        }
    }

    /// Send a replication command that switches the connection to CopyBoth
    /// (`START_REPLICATION ...`).
    pub fn start_copy_both(&mut self, sql: &str) -> Result<()> {
        self.send_query(sql)?;
        loop {
            let (tag, body) = self.read_msg_blocking()?;
            match tag {
                b'W' => return Ok(()),
                b'E' => {
                    // Drain to ReadyForQuery so the error is the real one.
                    let e = parse_error(&body);
                    return Err(e.into());
                }
                b'N' | b'S' => {}
                other => bail!(
                    "unexpected message {:?} starting replication",
                    other as char
                ),
            }
        }
    }

    /// Next CopyData payload, or `None` when nothing arrived in time.
    pub fn read_copy_data(&mut self) -> Result<Option<Vec<u8>>> {
        match self.try_read_msg()? {
            None => Ok(None),
            Some((b'd', body)) => Ok(Some(body)),
            Some((b'E', body)) => Err(parse_error(&body).into()),
            Some((b'c', _)) => bail!("the server ended the replication stream"),
            Some((b'N', _)) => Ok(None),
            Some((t, _)) => bail!(
                "unexpected message {:?} in the replication stream",
                t as char
            ),
        }
    }

    pub fn send_copy_data(&mut self, data: &[u8]) -> Result<()> {
        let mut msg = Vec::with_capacity(data.len() + 5);
        msg.push(b'd');
        msg.extend_from_slice(&((data.len() + 4) as u32).to_be_bytes());
        msg.extend_from_slice(data);
        self.io.write_all(&msg)?;
        self.io.flush()?;
        Ok(())
    }

    pub fn close(mut self) {
        let mut buf = BytesMut::new();
        frontend::terminate(&mut buf);
        let _ = self.io.write_all(&buf);
    }
}

/// Postgres epoch (2000-01-01) microseconds -> Unix seconds.
pub fn pg_time_to_unix_secs(us: i64) -> f64 {
    946_684_800.0 + us as f64 / 1e6
}

pub fn unix_now_to_pg_time() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_micros() as i64) - 946_684_800_000_000
}

/// Standby status update: written/flushed/applied LSN.
pub fn standby_status(lsn: u64, reply: bool) -> Vec<u8> {
    let mut b = Vec::with_capacity(34);
    b.push(b'r');
    for _ in 0..3 {
        b.extend_from_slice(&lsn.to_be_bytes());
    }
    b.extend_from_slice(&unix_now_to_pg_time().to_be_bytes());
    b.push(reply as u8);
    b
}

/// Quote an identifier.
pub fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Quote a string literal.
pub fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
