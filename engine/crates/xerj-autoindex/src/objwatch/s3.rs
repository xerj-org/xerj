//! A minimal, synchronous S3-compatible client: `ListObjectsV2` and `GetObject`.
//!
//! WHY THIS EXISTS AND NOT `aws-sdk-s3`
//!
//! `xerj-autoindex` is a synchronous crate (std threads, `reqwest::blocking`);
//! the server calls it through `spawn_blocking`. `aws-sdk-s3` is async-only, so
//! using it here means standing up a runtime inside a blocking call and pulling
//! the whole SDK into a binary that today needs exactly two S3 verbs. The
//! watcher's needs are small enough to sign by hand: SigV4 over
//! `ListObjectsV2` + `GetObject`, path-style addressing, no session token
//! refresh.
//!
//! This is deliberately behind [`super::ObjectSource`]. When a richer object
//! source lands (streaming reads, multipart-aware fetch, a shared credential
//! resolver), swap the implementation and the watcher does not change.
//!
//! WHAT IS NOT IMPLEMENTED: request retries with backoff (a failed cycle is
//! reported and the next poll retries), virtual-host addressing, IMDS or
//! profile credential chains (environment only), server-side encryption
//! headers, and `ListObjectVersions` (a versioned bucket is watched by current
//! version only).

use anyhow::{anyhow, bail, Context, Result};
use hmac::{Hmac, Mac};
use quick_xml::events::Event;
use quick_xml::Reader;
use sha2::{Digest, Sha256};
use std::time::Duration;

use super::{FetchedObject, ListPage, ListRequest, ObjectMeta, ObjectSource};

type HmacSha256 = Hmac<Sha256>;

/// Credentials, read from the environment only — never from a flag, so they
/// cannot land in a shell history, a CI log or a `ps` listing.
#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl Credentials {
    /// `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`.
    pub fn from_env() -> Option<Credentials> {
        let id = std::env::var("AWS_ACCESS_KEY_ID")
            .ok()
            .filter(|v| !v.is_empty())?;
        let secret = std::env::var("AWS_SECRET_ACCESS_KEY")
            .ok()
            .filter(|v| !v.is_empty())?;
        Some(Credentials {
            access_key_id: id,
            secret_access_key: secret,
            session_token: std::env::var("AWS_SESSION_TOKEN")
                .ok()
                .filter(|v| !v.is_empty()),
        })
    }
}

/// An `s3://bucket/prefix` location plus the endpoint that serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Location {
    pub bucket: String,
    /// Key prefix. Empty means the whole bucket.
    pub prefix: String,
}

/// Parse `s3://bucket[/prefix...]`. Rejects anything else, including a bare
/// bucket name, so a typo cannot be read as a folder.
pub fn parse_s3_url(url: &str) -> Result<S3Location> {
    let rest = url
        .strip_prefix("s3://")
        .ok_or_else(|| anyhow!("not an s3 URL (expected s3://bucket/prefix): {url}"))?;
    let (bucket, prefix) = match rest.split_once('/') {
        Some((b, p)) => (b, p),
        None => (rest, ""),
    };
    if bucket.is_empty() {
        bail!("s3 URL has no bucket: {url}");
    }
    if bucket.contains('.') && bucket.ends_with(".com") {
        bail!(
            "s3 URL bucket looks like a hostname ({bucket}). Pass the endpoint with \
             --endpoint-url and the bucket as s3://<bucket>/<prefix>"
        );
    }
    Ok(S3Location {
        bucket: bucket.to_string(),
        prefix: prefix.to_string(),
    })
}

/// Anything the watcher can talk to over the S3 HTTP API: AWS S3, Cloudflare
/// R2, MinIO, Ceph RGW. Path-style addressing is used for all of them because
/// it is the one form every gateway serves.
pub struct S3Source {
    client: reqwest::blocking::Client,
    endpoint: String,
    region: String,
    creds: Credentials,
    loc: S3Location,
}

impl S3Source {
    pub fn new(
        endpoint: &str,
        region: &str,
        creds: Credentials,
        loc: S3Location,
        timeout: Duration,
    ) -> Result<S3Source> {
        let endpoint = endpoint.trim_end_matches('/').to_string();
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            bail!("endpoint must start with http:// or https:// (got {endpoint})");
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .context("build blocking HTTP client for object storage")?;
        Ok(S3Source {
            client,
            endpoint,
            region: region.to_string(),
            creds,
            loc,
        })
    }

    pub fn location(&self) -> &S3Location {
        &self.loc
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Sign and send one request. `query` is already in canonical (sorted) form.
    fn send(
        &self,
        method: &str,
        key_path: &str,
        query: &[(String, String)],
        body: &[u8],
    ) -> Result<reqwest::blocking::Response> {
        let host = host_of(&self.endpoint)?;
        // Path-style: /<bucket>/<key>
        let mut canonical_path = String::from("/");
        canonical_path.push_str(&uri_encode(&self.loc.bucket, false));
        if !key_path.is_empty() {
            canonical_path.push('/');
            canonical_path.push_str(&uri_encode(key_path, false));
        }
        let canonical_query = query
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
            .collect::<Vec<_>>()
            .join("&");

        let payload_hash = hex_lower(&Sha256::digest(body));
        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let mut signed_headers = vec![
            ("host".to_string(), host.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        if let Some(tok) = &self.creds.session_token {
            signed_headers.push(("x-amz-security-token".to_string(), tok.clone()));
        }
        signed_headers.sort_by(|a, b| a.0.cmp(&b.0));
        let signed_header_names = signed_headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical_headers = signed_headers
            .iter()
            .map(|(k, v)| format!("{k}:{}\n", v.trim()))
            .collect::<String>();

        let canonical_request = format!(
            "{method}\n{canonical_path}\n{canonical_query}\n{canonical_headers}\n\
             {signed_header_names}\n{payload_hash}"
        );
        let scope = format!("{date_stamp}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex_lower(&Sha256::digest(canonical_request.as_bytes()))
        );
        let signature = hex_lower(
            &signing_key(&self.creds.secret_access_key, &date_stamp, &self.region)?
                .chain_update(string_to_sign.as_bytes())
                .finalize()
                .into_bytes(),
        );
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_header_names}, \
             Signature={signature}",
            self.creds.access_key_id
        );

        let mut url = format!("{}{canonical_path}", self.endpoint);
        if !canonical_query.is_empty() {
            url.push('?');
            url.push_str(&canonical_query);
        }
        // The URL parser normalises dot segments (`a/../b` -> `b`, and `%2E`
        // counts as a dot under WHATWG rules), so a key such as `x/../y.txt`
        // would be SENT as a different path than the one signed and meant. Refuse
        // it before spending a request on a guaranteed failure — or worse, on
        // another object's bytes.
        let parsed =
            reqwest::Url::parse(&url).with_context(|| format!("parse {}", redact(&url)))?;
        let sent_path = parsed.path();
        let meant_path = match self.endpoint.find("://").map(|i| &self.endpoint[i + 3..]) {
            Some(rest) => match rest.find('/') {
                Some(i) => format!("{}{canonical_path}", &rest[i..]),
                None => canonical_path.clone(),
            },
            None => canonical_path.clone(),
        };
        if sent_path != meant_path {
            bail!(
                "key {key_path:?} cannot be addressed over HTTP: the URL parser rewrites its path \
                 ({meant_path} -> {sent_path}), usually because of a '.' or '..' path segment. \
                 Rename the object; it was not requested"
            );
        }
        let mut req = match method {
            "GET" => self.client.get(&url),
            "PUT" => self.client.put(&url),
            "POST" => self.client.post(&url),
            "DELETE" => self.client.delete(&url),
            other => bail!("unsupported method {other}"),
        };
        req = req
            .header("x-amz-content-sha256", &payload_hash)
            .header("x-amz-date", &amz_date)
            .header("authorization", &authorization);
        if let Some(tok) = &self.creds.session_token {
            req = req.header("x-amz-security-token", tok);
        }
        if !body.is_empty() {
            req = req.body(body.to_vec());
        }
        req.send()
            .with_context(|| format!("{method} {} failed", redact(&url)))
    }
}

impl ObjectSource for S3Source {
    fn describe(&self) -> String {
        // Never the credentials, and never a presigned URL: this string goes
        // into progress output, a JSON result and bug reports.
        format!(
            "s3://{}/{} @ {}",
            self.loc.bucket, self.loc.prefix, self.endpoint
        )
    }

    fn list(&self, req: &ListRequest) -> Result<ListPage> {
        let mut query: Vec<(String, String)> = vec![("list-type".into(), "2".into())];
        if !req.prefix.is_empty() {
            query.push(("prefix".into(), req.prefix.to_string()));
        }
        if let Some(t) = req.continuation_token {
            query.push(("continuation-token".into(), t.to_string()));
        }
        if let Some(sa) = req.start_after {
            query.push(("start-after".into(), sa.to_string()));
        }
        if let Some(d) = req.delimiter {
            query.push(("delimiter".into(), d.to_string()));
        }
        query.push(("max-keys".into(), req.max_keys.to_string()));
        query.sort_by(|a, b| a.0.cmp(&b.0));

        let resp = self.send("GET", "", &query, b"")?;
        let status = resp.status();
        let body = resp.text().context("read ListObjectsV2 body")?;
        if !status.is_success() {
            bail!(
                "ListObjectsV2 on {} returned {status}: {}",
                self.loc.bucket,
                first_chars(&body, 400)
            );
        }
        parse_list_v2(&body)
    }

    fn get(&self, key: &str, max_bytes: u64) -> Result<FetchedObject> {
        let resp = self.send("GET", key, &[], b"")?;
        let status = resp.status();
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(normalize_etag);
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            bail!(
                "GetObject {key} returned {status}: {}",
                first_chars(&body, 400)
            );
        }
        // Bounded read: a watcher must not be turned into an OOM by one
        // enormous object that appeared in the bucket.
        let mut body = Vec::new();
        let mut reader = std::io::Read::take(resp, max_bytes.saturating_add(1));
        std::io::Read::read_to_end(&mut reader, &mut body).context("read object body")?;
        let truncated = body.len() as u64 > max_bytes;
        if truncated {
            body.truncate(max_bytes as usize);
        }
        Ok(FetchedObject {
            bytes: body,
            etag,
            truncated,
        })
    }
}

/// Test/harness support: the watcher itself never writes. These verbs exist so
/// the integration tests can build a bucket state (including a real multipart
/// upload, whose ETag shape differs) through the same signer the read path
/// uses — a separate test client would prove the tests, not the code.
impl S3Source {
    pub fn create_bucket(&self) -> Result<()> {
        let resp = self.send("PUT", "", &[], b"")?;
        let status = resp.status();
        if status.is_success() || status.as_u16() == 409 {
            return Ok(());
        }
        let body = resp.text().unwrap_or_default();
        // MinIO and R2 both answer an existing bucket with a 409-family error.
        if body.contains("BucketAlreadyOwnedByYou") || body.contains("BucketAlreadyExists") {
            return Ok(());
        }
        bail!(
            "CreateBucket returned {status}: {}",
            first_chars(&body, 300)
        )
    }

    pub fn put_object(&self, key: &str, body: &[u8]) -> Result<Option<String>> {
        let resp = self.send("PUT", key, &[], body)?;
        let status = resp.status();
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(normalize_etag);
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            bail!(
                "PutObject {key} returned {status}: {}",
                first_chars(&text, 300)
            );
        }
        Ok(etag)
    }

    pub fn delete_object(&self, key: &str) -> Result<()> {
        let resp = self.send("DELETE", key, &[], b"")?;
        let status = resp.status();
        if !status.is_success() && status.as_u16() != 404 {
            let text = resp.text().unwrap_or_default();
            bail!(
                "DeleteObject {key} returned {status}: {}",
                first_chars(&text, 300)
            );
        }
        Ok(())
    }

    /// A real multipart upload, so a test can assert what the watcher does when
    /// an object is replaced by one: the ETag stops being an MD5 and becomes
    /// `<hex>-<part count>`.
    pub fn put_object_multipart(&self, key: &str, parts: &[Vec<u8>]) -> Result<Option<String>> {
        if parts.len() < 2 {
            bail!("a multipart upload needs at least 2 parts");
        }
        let resp = self.send("POST", key, &[("uploads".into(), String::new())], b"")?;
        let status = resp.status();
        let body = resp.text().context("read CreateMultipartUpload body")?;
        if !status.is_success() {
            bail!(
                "CreateMultipartUpload returned {status}: {}",
                first_chars(&body, 300)
            );
        }
        let upload_id = xml_text(&body, "UploadId")
            .ok_or_else(|| anyhow!("CreateMultipartUpload response had no UploadId"))?;

        let mut etags: Vec<(usize, String)> = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let n = i + 1;
            let mut q = vec![
                ("partNumber".to_string(), n.to_string()),
                ("uploadId".to_string(), upload_id.clone()),
            ];
            q.sort_by(|a, b| a.0.cmp(&b.0));
            let resp = self.send("PUT", key, &q, part)?;
            let status = resp.status();
            let etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string());
            if !status.is_success() {
                let text = resp.text().unwrap_or_default();
                bail!(
                    "UploadPart {n} returned {status}: {}",
                    first_chars(&text, 300)
                );
            }
            etags.push((
                n,
                etag.ok_or_else(|| anyhow!("UploadPart {n} returned no ETag"))?,
            ));
        }

        let mut xml = String::from("<CompleteMultipartUpload>");
        for (n, etag) in &etags {
            xml.push_str(&format!(
                "<Part><PartNumber>{n}</PartNumber><ETag>{etag}</ETag></Part>"
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let mut q = vec![("uploadId".to_string(), upload_id)];
        q.sort_by(|a, b| a.0.cmp(&b.0));
        let resp = self.send("POST", key, &q, xml.as_bytes())?;
        let status = resp.status();
        let body = resp.text().context("read CompleteMultipartUpload body")?;
        if !status.is_success() {
            bail!(
                "CompleteMultipartUpload returned {status}: {}",
                first_chars(&body, 300)
            );
        }
        // S3 answers 200 with an error document on some failures.
        if body.contains("<Error>") {
            bail!(
                "CompleteMultipartUpload reported an error: {}",
                first_chars(&body, 300)
            );
        }
        Ok(xml_text(&body, "ETag").map(|e| normalize_etag(&e)))
    }
}

fn signing_key(secret: &str, date_stamp: &str, region: &str) -> Result<HmacSha256> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes())?;
    let k_region = hmac(&k_date, region.as_bytes())?;
    let k_service = hmac(&k_region, b"s3")?;
    let k_signing = hmac(&k_service, b"aws4_request")?;
    HmacSha256::new_from_slice(&k_signing).map_err(|e| anyhow!("signing key: {e}"))
}

fn hmac(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| anyhow!("hmac key: {e}"))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((b & 0xf) as u32, 16).unwrap_or('0'));
    }
    out
}

/// RFC 3986 encoding as SigV4 defines it: unreserved characters pass through,
/// everything else becomes `%XX` uppercase. `/` is left alone in a path
/// (`encode_slash = false`) and encoded in a query value.
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '~' | '.') {
            out.push(c);
        } else if c == '/' && !encode_slash {
            out.push('/');
        } else {
            out.push('%');
            out.push(
                char::from_digit((b >> 4) as u32, 16)
                    .unwrap_or('0')
                    .to_ascii_uppercase(),
            );
            out.push(
                char::from_digit((b & 0xf) as u32, 16)
                    .unwrap_or('0')
                    .to_ascii_uppercase(),
            );
        }
    }
    out
}

fn host_of(endpoint: &str) -> Result<String> {
    let rest = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .ok_or_else(|| anyhow!("endpoint has no scheme: {endpoint}"))?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        bail!("endpoint has no host: {endpoint}");
    }
    Ok(host.to_string())
}

/// Strip an ETag's quotes. Weak-validator ETags (`W/"..."`) are normalised the
/// same way; the watcher compares them as opaque strings.
pub fn normalize_etag(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("W/")
        .trim_matches('"')
        .to_string()
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A URL is safe to print here (SigV4 puts the signature in a header, not the
/// query), but presigned URLs would not be — so strip any query that carries
/// one rather than relying on the caller never passing one.
fn redact(url: &str) -> String {
    match url.split_once("X-Amz-Signature") {
        Some((head, _)) => format!("{head}X-Amz-Signature=<redacted>"),
        None => url.to_string(),
    }
}

/// The one XML shape the watcher parses. Written against the ListObjectsV2
/// response as AWS documents it and as MinIO and R2 emit it; `Key` values are
/// XML-unescaped, which matters for keys containing `&`.
pub fn parse_list_v2(body: &str) -> Result<ListPage> {
    // NOT `trim_text(true)`: that trims every text event, and quick-xml emits
    // an entity (`&amp;`) as its own event, so `a &amp; b` arrived as "a", "&",
    // "b" and became `a&b`, and a key ending in a space lost it. Either way the
    // watcher then asked for a key that does not exist and got a 404 on every
    // cycle, forever (F5 of the #968 review). Whitespace between elements is
    // discarded anyway: `text` is cleared at every start and end tag. Fields
    // that are not keys are trimmed where they are read.
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(false);

    let mut objects: Vec<ObjectMeta> = Vec::new();
    let mut common_prefixes: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    let mut truncated = false;

    let mut in_contents = false;
    let mut in_common = false;
    let mut field = String::new();
    let mut cur = ObjectMeta::default();
    let mut text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "Contents" => {
                        in_contents = true;
                        cur = ObjectMeta::default();
                    }
                    "CommonPrefixes" => in_common = true,
                    _ => {}
                }
                field = name;
                text.clear();
            }
            Ok(Event::Text(t)) => {
                text.push_str(&t.xml10_content().unwrap_or_default());
            }
            Ok(Event::GeneralRef(r)) => {
                if let Some(resolved) = resolve_general_ref(&r) {
                    text.push_str(&resolved);
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "Contents" => {
                        in_contents = false;
                        if !cur.key.is_empty() {
                            objects.push(std::mem::take(&mut cur));
                        }
                    }
                    "CommonPrefixes" => in_common = false,
                    "Key" if in_contents => cur.key = text.clone(),
                    "ETag" if in_contents => cur.etag = normalize_etag(&text),
                    "Size" if in_contents => cur.size = text.trim().parse().unwrap_or(0),
                    "LastModified" if in_contents => cur.last_modified = text.trim().to_string(),
                    "Prefix" if in_common => common_prefixes.push(text.clone()),
                    "NextContinuationToken" => next_token = Some(text.trim().to_string()),
                    "IsTruncated" => truncated = text.trim().eq_ignore_ascii_case("true"),
                    _ => {}
                }
                field.clear();
                text.clear();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("malformed ListObjectsV2 XML: {e}"),
        }
    }
    let _ = field;

    // A truncated page with no token is a gateway bug we must not paper over:
    // silently stopping would report every unlisted object as deleted.
    if truncated && next_token.is_none() {
        bail!("ListObjectsV2 said IsTruncated but returned no NextContinuationToken");
    }
    Ok(ListPage {
        objects,
        next_token: if truncated { next_token } else { None },
        common_prefixes,
    })
}

/// quick-xml >= 0.38 emits `&amp;` and friends as their own event instead of
/// folding them into the neighbouring text, so a key containing an entity would
/// lose that character unless the reader puts it back. `extract::xml_x` has the
/// same helper but scoped `pub(super)`; this is the two-line version rather
/// than a visibility change in an unrelated module.
fn resolve_general_ref(r: &quick_xml::events::BytesRef) -> Option<String> {
    let name = String::from_utf8_lossy(r.as_ref()).to_string();
    if let Some(hexpart) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        return u32::from_str_radix(hexpart, 16)
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string());
    }
    if let Some(dec) = name.strip_prefix('#') {
        return dec
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string());
    }
    quick_xml::escape::resolve_predefined_entity(&name).map(str::to_string)
}

/// First text value of `<tag>` in a small response document.
fn xml_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s3_urls_parse_into_bucket_and_prefix() {
        assert_eq!(
            parse_s3_url("s3://logs/2026/09/").unwrap(),
            S3Location {
                bucket: "logs".into(),
                prefix: "2026/09/".into()
            }
        );
        assert_eq!(
            parse_s3_url("s3://logs").unwrap(),
            S3Location {
                bucket: "logs".into(),
                prefix: String::new()
            }
        );
        assert!(parse_s3_url("/tmp/folder").is_err());
        assert!(parse_s3_url("s3://").is_err());
        assert!(parse_s3_url("s3://bucket.example.com/x").is_err());
    }

    #[test]
    fn sigv4_uri_encoding_matches_the_spec() {
        // The four unreserved classes pass through; everything else is %XX
        // uppercase. `/` depends on position.
        assert_eq!(uri_encode("a-z_0.9~", false), "a-z_0.9~");
        assert_eq!(uri_encode("a/b", false), "a/b");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(uri_encode("a b", false), "a%20b");
        assert_eq!(uri_encode("ä", false), "%C3%A4");
        assert_eq!(uri_encode("+", false), "%2B");
    }

    /// AWS's published SigV4 test vector for the signing key derivation
    /// (`AWS4-HMAC-SHA256` documentation, service `iam`, region `us-east-1`,
    /// date `20150830`). Used here with service `s3` replaced only in our
    /// helper, so the vector is adapted: what it pins is the HMAC chain shape,
    /// which is the part that breaks silently.
    #[test]
    fn signing_key_chain_is_deterministic() {
        let a = hex_lower(
            &signing_key(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                "20150830",
                "us-east-1",
            )
            .unwrap()
            .chain_update(b"x")
            .finalize()
            .into_bytes(),
        );
        let b = hex_lower(
            &signing_key(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                "20150830",
                "us-east-1",
            )
            .unwrap()
            .chain_update(b"x")
            .finalize()
            .into_bytes(),
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        let other = hex_lower(
            &signing_key(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                "20150831",
                "us-east-1",
            )
            .unwrap()
            .chain_update(b"x")
            .finalize()
            .into_bytes(),
        );
        assert_ne!(a, other, "a different date must derive a different key");
    }

    #[test]
    fn hex_and_sha256_agree_with_the_known_empty_digest() {
        assert_eq!(
            hex_lower(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn list_v2_response_parses_keys_etags_sizes_and_token() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>b</Name><Prefix>docs/</Prefix><KeyCount>2</KeyCount>
  <IsTruncated>true</IsTruncated>
  <NextContinuationToken>tok-1</NextContinuationToken>
  <Contents>
    <Key>docs/a&amp;b.md</Key>
    <LastModified>2026-09-19T10:00:00.000Z</LastModified>
    <ETag>&quot;d41d8cd98f00b204e9800998ecf8427e&quot;</ETag>
    <Size>12</Size>
  </Contents>
  <Contents>
    <Key>docs/big.bin</Key>
    <LastModified>2026-09-19T11:00:00.000Z</LastModified>
    <ETag>&quot;abc123-4&quot;</ETag>
    <Size>10485760</Size>
  </Contents>
  <CommonPrefixes><Prefix>docs/sub/</Prefix></CommonPrefixes>
</ListBucketResult>"#;
        let page = parse_list_v2(xml).unwrap();
        assert_eq!(page.objects.len(), 2);
        assert_eq!(page.objects[0].key, "docs/a&b.md");
        assert_eq!(page.objects[0].etag, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(page.objects[0].size, 12);
        assert_eq!(
            page.objects[1].etag, "abc123-4",
            "multipart ETag keeps its -N"
        );
        assert_eq!(page.objects[1].size, 10_485_760);
        assert_eq!(page.next_token.as_deref(), Some("tok-1"));
        assert_eq!(page.common_prefixes, vec!["docs/sub/".to_string()]);
    }

    /// F5 of the #968 review, as a table: every key the parser used to change.
    /// A key is opaque bytes; the listing must hand back exactly what was PUT.
    #[test]
    fn keys_come_back_from_the_listing_byte_for_byte() {
        fn page(key_xml: &str) -> String {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult>\
                 <IsTruncated>false</IsTruncated><Contents><Key>{key_xml}</Key>\
                 <LastModified>2026-09-19T10:00:00.000Z</LastModified>\
                 <ETag>&quot;abc&quot;</ETag><Size> 7 </Size></Contents></ListBucketResult>"
            )
        }
        for (xml, want) in [
            ("docs/a &amp; b.md", "docs/a & b.md"),
            ("&#32;leading.txt", " leading.txt"),
            ("trailing&#32;.txt", "trailing .txt"),
            ("a&amp;b.md", "a&b.md"),
            ("tab&#9;sep.txt", "tab\tsep.txt"),
            ("x &lt;y&gt; z", "x <y> z"),
            ("new&#10;line.txt", "new\nline.txt"),
            (" rawlead.txt", " rawlead.txt"),
            ("rawtrail.txt ", "rawtrail.txt "),
            ("a &amp;&amp; b", "a && b"),
            ("mid  double.txt", "mid  double.txt"),
            ("ends ", "ends "),
            ("   ", "   "),
        ] {
            let got = parse_list_v2(&page(xml)).unwrap();
            assert_eq!(got.objects.len(), 1, "{xml:?}");
            assert_eq!(got.objects[0].key, want, "listing XML {xml:?}");
            assert_eq!(got.objects[0].size, 7, "numeric fields are still trimmed");
            assert_eq!(got.objects[0].etag, "abc");
        }
    }

    /// Whitespace-only text between elements must not leak into a field once
    /// trimming is off: pretty-printed XML is what MinIO and R2 both emit.
    #[test]
    fn indentation_between_elements_does_not_leak_into_fields() {
        let xml = "<ListBucketResult>\n  <IsTruncated>true</IsTruncated>\n  \
                   <NextContinuationToken>\n tok \n</NextContinuationToken>\n  <Contents>\n    \
                   <Key>k</Key>\n    <ETag>\"e\"</ETag>\n    <Size>\n3\n</Size>\n  </Contents>\n\
                   </ListBucketResult>";
        let p = parse_list_v2(xml).unwrap();
        assert_eq!(p.objects[0].key, "k");
        assert_eq!(p.objects[0].size, 3);
        assert_eq!(p.next_token.as_deref(), Some("tok"));
    }

    /// A dot-segment key would be sent as a different path than the one signed.
    /// It must be refused before any request goes out.
    #[test]
    fn a_key_the_url_parser_would_rewrite_is_refused_before_sending() {
        let src = S3Source::new(
            "http://127.0.0.1:9",
            "auto",
            Credentials {
                access_key_id: "k".into(),
                secret_access_key: "s".into(),
                session_token: None,
            },
            S3Location {
                bucket: "b".into(),
                prefix: String::new(),
            },
            Duration::from_secs(1),
        )
        .unwrap();
        for key in ["dots/../up.txt", "a/./b.txt", "..", "x/.."] {
            let err = src.get(key, 10).err().expect(key).to_string();
            assert!(err.contains("cannot be addressed"), "{key}: {err}");
        }
    }

    #[test]
    fn an_untruncated_page_reports_no_token() {
        let xml = "<ListBucketResult><IsTruncated>false</IsTruncated>\
                   <NextContinuationToken>stale</NextContinuationToken></ListBucketResult>";
        assert!(parse_list_v2(xml).unwrap().next_token.is_none());
    }

    /// The failure this guards is the dangerous one: a page that says there is
    /// more but hands back no cursor. Treating it as the end of the bucket
    /// would report every unlisted object as deleted.
    #[test]
    fn truncated_without_a_token_is_an_error_not_an_empty_tail() {
        let xml = "<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>";
        let err = parse_list_v2(xml).unwrap_err().to_string();
        assert!(err.contains("NextContinuationToken"), "{err}");
    }

    #[test]
    fn etags_are_normalised_without_losing_the_multipart_suffix() {
        assert_eq!(normalize_etag("\"abc\""), "abc");
        assert_eq!(normalize_etag("W/\"abc\""), "abc");
        assert_eq!(normalize_etag("\"abc-12\""), "abc-12");
    }
}
