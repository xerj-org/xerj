//! Corpus Hub v2 — the catalogue (`docs/CORPUS_HUB_V2_IMPLEMENTATION.md`, step 4).
//!
//! The catalogue is one small, signed JSON file listing every corpus the hub
//! serves. An agent reads it to pick a corpus — slug, domain, licence, pin,
//! pack URL and a Bloom [`term sketch`](crate::sketch) for offline routing —
//! without downloading a single pack.
//!
//! This module holds the two ends of that file:
//!
//! - [`build_catalogue`] — the CI-side builder. It turns one
//!   [`CorpusBuildInput`] per corpus into a [`CatalogueEntry`], assembles the
//!   [`Catalogue`], serialises it deterministically ([`canonical_json`]) and
//!   signs those exact bytes. It deliberately takes its per-corpus facts as a
//!   struct instead of opening pack archives, so it stays decoupled from the
//!   pack module built in parallel.
//! - [`load_catalogue`] — the client-side loader. It serves a cached copy while
//!   it is young, otherwise refreshes over HTTP with `If-None-Match` and
//!   **verifies the detached signature before letting the fetched bytes replace
//!   the cache**. On any network or verification failure it keeps the last good
//!   cached copy; `offline` never touches the network.
//!
//! The signing primitives are shared with the pack format
//! (`xerj_autoindex::harvest::sign`): a raw ed25519 signature over the exact
//! bytes, as lower-case hex in a sibling `.sig` file.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use xerj_autoindex::harvest::sign::{sign_bytes, verify_bytes};
use xerj_common::hub::{
    hub_pubkey, Catalogue, CatalogueEntry, LicenceClass, CATALOGUE_FORMAT_VERSION,
};

/// Path of the catalogue under the hub's API root.
const CATALOGUE_API_PATH: &str = "api/v1/catalogue.json";
/// The cached catalogue file name inside the cache dir.
const CACHE_FILE: &str = "catalogue.json";
/// Where the last response's ETag is stored for the conditional GET.
const ETAG_FILE: &str = "catalogue.etag";
/// A cached catalogue younger than this is used without touching the network.
const FRESH_FOR: Duration = Duration::from_secs(24 * 60 * 60);

// ── builder input ───────────────────────────────────────────────────────────

/// One corpus's inputs to [`build_catalogue`]: the path to its registry
/// manifest (`tools/xerj-code/hub/<slug>.json`) plus the pack facts and the
/// terms for its sketch.
///
/// Everything the catalogue entry needs that the manifest does not yet carry is
/// supplied by the caller here; the manifest's own field always wins when it
/// has one.
pub struct CorpusBuildInput {
    /// Registry manifest; read for slug, licence (spdx + class), the pin sha
    /// fallback and pin date, and — when future manifests carry them — topics,
    /// the `ask_for` sentence and `dependency_names`.
    pub manifest_path: PathBuf,
    /// SEC / DB / NET / … — the backlog row's domain, which the caller supplies
    /// (the manifest does not record it).
    pub domain: String,
    pub refresh_cadence_days: u32,
    pub pack_bytes: u64,
    pub pack_url: String,
    pub pack_sha256: String,
    pub sig_url: String,
    pub quality_grade: Option<String>,
    /// Identifiers / vocabulary for the Bloom sketch. The caller assembles these
    /// (def symbols, path components, `ask_for`/`topics` words) per the plan;
    /// the builder only hashes them.
    pub terms: Vec<String>,
    /// Pin sha override. When empty, the manifest's first repo sha is used — the
    /// manifests store the authoritative pin there, so that is the common path.
    pub pin_sha: String,
    /// The backlog row's `ask_for` sentence, used only when the manifest does
    /// not carry one of its own.
    pub ask_for: String,
}

// ── manifest shapes (only the fields we read) ───────────────────────────────
//
// No `deny_unknown_fields`: the real hub manifests carry many fields we do not
// need (`url`, `files`, `bytes`, the licence hint, review `by`/`note`, …), and
// serde skips them by default. New richer fields (`topics`, `ask_for`,
// `dependency_names`, `pin_date`) are read when present and default otherwise.

#[derive(Deserialize)]
struct Manifest {
    corpus: String,
    #[serde(default)]
    cloned_at: Option<String>,
    #[serde(default)]
    repos: Vec<ManifestRepo>,
    #[serde(default)]
    topics: Vec<String>,
    #[serde(default)]
    ask_for: Option<String>,
    /// Per the plan's note, the `ask_for` sentence rides in a `domain` field in
    /// the backlog-derived manifests; used as a secondary source for `ask_for`.
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    pin_date: Option<String>,
    #[serde(default)]
    dependency_names: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestRepo {
    #[serde(default)]
    sha: String,
    #[serde(default)]
    review: Option<ManifestReview>,
}

#[derive(Deserialize)]
struct ManifestReview {
    #[serde(default)]
    spdx: String,
    #[serde(rename = "use", default)]
    use_: String,
    /// `YYYY-MM-DD` the review (and thus the pin) was taken.
    #[serde(default)]
    at: Option<String>,
}

// ── builder ─────────────────────────────────────────────────────────────────

/// The exact bytes the catalogue is signed over, and the bytes CI must write to
/// `catalogue.json`. Compact UTF-8 JSON with the corpora sorted by slug, so the
/// same inputs produce a byte-identical file and the detached signature
/// reproduces across builds. The client verifies the signature over the raw
/// bytes it fetches, so the only contract is that the publisher writes *these*
/// bytes.
pub fn canonical_json(catalogue: &Catalogue) -> Result<Vec<u8>> {
    serde_json::to_vec(catalogue).context("serialize catalogue")
}

/// Build the catalogue and its detached signature (hex) over the exact JSON
/// bytes. One [`CatalogueEntry`] per input; the entries are sorted by slug so
/// the output does not depend on the order CI enumerates the registry in.
pub fn build_catalogue(
    inputs: &[CorpusBuildInput],
    generated_at: &str,
    sign_seed_hex: &str,
) -> Result<(Catalogue, String)> {
    let mut corpora = Vec::with_capacity(inputs.len());
    for input in inputs {
        corpora.push(build_entry(input)?);
    }
    corpora.sort_by(|a, b| a.slug.cmp(&b.slug));

    let catalogue = Catalogue {
        format_version: CATALOGUE_FORMAT_VERSION,
        generated_at: generated_at.to_string(),
        corpora,
    };

    let bytes = canonical_json(&catalogue)?;
    let signature = sign_bytes(&bytes, sign_seed_hex).context("sign catalogue bytes")?;
    Ok((catalogue, signature))
}

fn build_entry(input: &CorpusBuildInput) -> Result<CatalogueEntry> {
    let path = &input.manifest_path;
    let raw =
        fs::read_to_string(path).with_context(|| format!("read manifest {}", path.display()))?;
    let m: Manifest =
        serde_json::from_str(&raw).with_context(|| format!("parse manifest {}", path.display()))?;

    // Licence and the pin-sha fallback come from the repos; take them in a block
    // so the borrow ends before the owned manifest fields are moved out below.
    let (licence_spdx, licence_class, repo_sha, review_at) = {
        let first = m
            .repos
            .first()
            .ok_or_else(|| anyhow!("manifest {} lists no repos", path.display()))?;
        let review = first.review.as_ref().ok_or_else(|| {
            anyhow!(
                "manifest {}: first repo has no licence review",
                path.display()
            )
        })?;
        // Most restrictive class across all repos — a single GPL/mixed sub-repo
        // must not be masked by a permissive sibling. `licence_spdx` must name
        // the SAME repo whose class wins: otherwise a GPL sub-repo behind a
        // permissive FIRST sibling emits {licence_spdx:"MIT", licence_class:
        // ApproachOnly} — a correct verdict with a misleading SPDX, so step 5's
        // warning reads "approach-only licence (MIT)". Take both from that repo.
        let (class, spdx) = most_restrictive_class(&m.repos, path)?;
        if spdx.is_empty() {
            bail!("manifest {}: empty review.spdx", path.display());
        }
        (spdx, class, first.sha.trim().to_string(), review.at.clone())
    };

    // Caller's pin sha wins; otherwise the manifest's first repo sha.
    let pin_sha = {
        let caller = input.pin_sha.trim();
        if caller.is_empty() {
            repo_sha
        } else {
            caller.to_string()
        }
    };
    if pin_sha.is_empty() {
        bail!(
            "manifest {}: no pin sha (caller override empty and repo sha absent)",
            path.display()
        );
    }

    // Pin date: an explicit manifest field, else the review date, else the date
    // portion of the clone timestamp.
    let pin_date = m
        .pin_date
        .or(review_at)
        .or_else(|| {
            m.cloned_at
                .as_deref()
                .and_then(|c| c.get(0..10).map(str::to_string))
        })
        .unwrap_or_default();

    // ask_for: the manifest's own field, else its `domain` sentence, else the
    // caller's fallback.
    let ask_for = non_empty(m.ask_for.as_deref())
        .or_else(|| non_empty(m.domain.as_deref()))
        .unwrap_or_else(|| input.ask_for.clone());

    let term_sketch = crate::sketch::build(input.terms.iter().cloned());

    Ok(CatalogueEntry {
        slug: m.corpus,
        domain: input.domain.clone(),
        topics: m.topics,
        ask_for,
        pin_sha,
        pin_date,
        refresh_cadence_days: input.refresh_cadence_days,
        licence_spdx,
        licence_class,
        pack_bytes: input.pack_bytes,
        pack_url: input.pack_url.clone(),
        pack_sha256: input.pack_sha256.clone(),
        sig_url: input.sig_url.clone(),
        dependency_names: m.dependency_names,
        quality_grade: input.quality_grade.clone(),
        term_sketch,
    })
}

/// `Some(trimmed)` when the string is present and non-blank, else `None`.
fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Map a registry `review.use` to a [`LicenceClass`]. `mixed` is read as
/// `approach-only`: part of the repo is copyable and part is not, and the
/// licence-safe (restrictive-first) reading is "read, do not copy".
fn parse_class(use_: &str) -> Option<LicenceClass> {
    match use_.trim() {
        "adapt-with-attribution" => Some(LicenceClass::AdaptWithAttribution),
        "approach-only" => Some(LicenceClass::ApproachOnly),
        "excluded" => Some(LicenceClass::Excluded),
        "mixed" => Some(LicenceClass::ApproachOnly),
        _ => None,
    }
}

/// Ordering for "most restrictive wins": excluded > approach-only > adapt.
fn restrictiveness(c: LicenceClass) -> u8 {
    match c {
        LicenceClass::AdaptWithAttribution => 0,
        LicenceClass::ApproachOnly => 1,
        LicenceClass::Excluded => 2,
    }
}

/// The most restrictive licence class across all repos, paired with the SPDX id
/// of the FIRST repo that attains it. "most restrictive wins" so a single
/// GPL/mixed sub-repo is never masked by a permissive sibling, and returning
/// that repo's SPDX too keeps `licence_spdx` and `licence_class` describing one
/// and the same repo. Fail-closed: a repo with no review, or an unknown
/// `review.use`, is an error — never a silently-permissive default.
fn most_restrictive_class(repos: &[ManifestRepo], path: &Path) -> Result<(LicenceClass, String)> {
    let mut worst: Option<(LicenceClass, String)> = None;
    for repo in repos {
        let review = repo
            .review
            .as_ref()
            .ok_or_else(|| anyhow!("manifest {}: a repo has no licence review", path.display()))?;
        let class = parse_class(&review.use_).ok_or_else(|| {
            anyhow!(
                "manifest {}: unknown licence use '{}'",
                path.display(),
                review.use_
            )
        })?;
        // Strictly-more-restrictive replaces; a tie keeps the earlier repo, so
        // `worst` is set by the FIRST repo reaching the maximum restrictiveness.
        let more_restrictive = match &worst {
            Some((w, _)) => restrictiveness(class) > restrictiveness(*w),
            None => true,
        };
        if more_restrictive {
            worst = Some((class, review.spdx.trim().to_string()));
        }
    }
    worst.ok_or_else(|| anyhow!("manifest {}: no repos to classify", path.display()))
}

// ── client loader ───────────────────────────────────────────────────────────

/// Load the catalogue.
///
/// A cached `<cache_dir>/catalogue.json` younger than 24 h is returned as-is (it
/// was signature-verified when it was written). Otherwise the catalogue is
/// fetched from `<hub_url>/api/v1/catalogue.json` with `If-None-Match` (from
/// `<cache_dir>/catalogue.etag`), its detached signature
/// (`…/catalogue.json.sig`) is verified against the pubkey **before** the bytes
/// are allowed to replace the cache, and only a verified copy is cached and
/// returned. On any network, signature or parse failure the previous cached
/// copy is kept and returned; if there is none, an error is returned.
/// `offline == true` never fetches.
pub fn load_catalogue(hub_url: &str, cache_dir: &Path, offline: bool) -> Result<Catalogue> {
    let cache_file = cache_dir.join(CACHE_FILE);
    let etag_file = cache_dir.join(ETAG_FILE);

    if offline {
        return match read_cached(&cache_file) {
            Some(cat) => {
                warn_if_old(&cat);
                Ok(cat)
            }
            None => bail!(
                "offline and no usable cached catalogue at {}",
                cache_file.display()
            ),
        };
    }

    // A young cache is authoritative without a network round-trip.
    if cache_is_fresh(&cache_file) {
        if let Some(cat) = read_cached(&cache_file) {
            return Ok(cat);
        }
    }

    match fetch_and_store(hub_url, &cache_file, &etag_file) {
        Ok(cat) => Ok(cat),
        // Keep the last good copy on any failure, exactly as the plan requires.
        Err(e) => match read_cached(&cache_file) {
            Some(cat) => {
                eprintln!("xerj hub: catalogue refresh failed ({e:#}); keeping the cached copy");
                warn_if_old(&cat);
                Ok(cat)
            }
            None => Err(e.context("fetch catalogue, and no cached copy to fall back to")),
        },
    }
}

/// Ask the hub for the catalogue NOW, ignoring the 24 h freshness window that
/// [`load_catalogue`] honours. `xerj hub sync` calls this: a sync that trusted a
/// young cache would never see a pin that moved this morning.
///
/// The request is still conditional (`If-None-Match` from `catalogue.etag`, so an
/// unchanged catalogue costs a 304) and the detached signature is still verified
/// BEFORE the fetched bytes replace the cache. Unlike [`load_catalogue`] it does
/// not fall back to the cached copy on failure: the caller asked "is there
/// something newer?", and answering from a stale copy would be a wrong "no". On
/// any network, signature or parse failure the error is returned and the cache
/// is left exactly as it was.
pub fn refresh_catalogue(hub_url: &str, cache_dir: &Path) -> Result<Catalogue> {
    fetch_and_store(
        hub_url,
        &cache_dir.join(CACHE_FILE),
        &cache_dir.join(ETAG_FILE),
    )
}

/// Read and validate the cached catalogue, or `None` if it is missing,
/// unreadable, unparsable or a format version this client does not speak.
fn read_cached(cache_file: &Path) -> Option<Catalogue> {
    let raw = fs::read(cache_file).ok()?;
    parse_catalogue(&raw).ok()
}

/// Parse catalogue bytes and refuse a format version this client does not equal.
fn parse_catalogue(bytes: &[u8]) -> Result<Catalogue> {
    let cat: Catalogue = serde_json::from_slice(bytes).context("parse catalogue JSON")?;
    if cat.format_version != CATALOGUE_FORMAT_VERSION {
        bail!(
            "catalogue format_version {} is not the supported {}",
            cat.format_version,
            CATALOGUE_FORMAT_VERSION
        );
    }
    Ok(cat)
}

/// True when the cache file exists and its mtime is within [`FRESH_FOR`].
fn cache_is_fresh(cache_file: &Path) -> bool {
    let Ok(modified) = fs::metadata(cache_file).and_then(|m| m.modified()) else {
        return false;
    };
    match modified.elapsed() {
        Ok(elapsed) => elapsed < FRESH_FOR,
        // mtime in the future (clock skew): treat as fresh rather than hammer.
        Err(_) => true,
    }
}

/// Warn when the cached copy being served is a day or more old.
fn warn_if_old(cat: &Catalogue) {
    if let Ok(generated) = chrono::DateTime::parse_from_rfc3339(&cat.generated_at) {
        let days = chrono::Utc::now()
            .signed_duration_since(generated.with_timezone(&chrono::Utc))
            .num_days();
        if days >= 1 {
            eprintln!(
                "xerj hub: catalogue is {days} day{} old",
                if days == 1 { "" } else { "s" }
            );
        }
    }
}

/// Fetch, verify and cache the catalogue. The signature guards the replacement:
/// the fetched bytes are only written to the cache after they verify against
/// [`hub_pubkey`]. With no key (`XERJ_HUB_PUBKEY=none`, or a build without one)
/// nothing is fetched at all: there is no unverified path into the cache.
fn fetch_and_store(hub_url: &str, cache_file: &Path, etag_file: &Path) -> Result<Catalogue> {
    let Some(pubkey) = hub_pubkey() else {
        bail!(
            "no hub public key is configured, so the hub catalogue cannot be verified; \
             nothing was fetched"
        );
    };
    let base = hub_url.trim_end_matches('/');
    let cat_url = format!("{base}/{CATALOGUE_API_PATH}");
    let sig_url = format!("{cat_url}.sig");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .context("build http client")?;

    // Conditional GET: a stored ETag lets the hub answer 304 and save bandwidth.
    let prev_etag = fs::read_to_string(etag_file)
        .ok()
        .and_then(|s| non_empty(Some(&s)));
    let mut req = client.get(&cat_url);
    if let Some(etag) = &prev_etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag.as_str());
    }
    let resp = req.send().with_context(|| format!("GET {cat_url}"))?;

    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        // The cached bytes are still current; re-touch them so the next 24 h are
        // served from cache without re-asking.
        let raw =
            fs::read(cache_file).context("hub returned 304 Not Modified but the cache is gone")?;
        let cat = parse_catalogue(&raw)?;
        write_atomic(cache_file, &raw)?;
        return Ok(cat);
    }
    if !resp.status().is_success() {
        bail!("GET {cat_url} returned HTTP {}", resp.status());
    }

    // Capture the new ETag before the body consumes the response.
    let new_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let cat_type = content_type(&resp);
    let body = resp
        .bytes()
        .with_context(|| format!("read body of {cat_url}"))?
        .to_vec();
    // A static host answers an unknown path with its HTML page and a 200, so a
    // success status alone does not mean a catalogue is published. Say so plainly
    // instead of failing later on a signature that is really an HTML page.
    if !starts_like_json_object(&body) {
        bail!(
            "the hub catalogue is unavailable: {cat_url} returned {} ({cat_type}), not a signed \
             catalogue; no catalogue is served at this address",
            preview(&body)
        );
    }

    let sig_resp = client
        .get(&sig_url)
        .send()
        .with_context(|| format!("GET {sig_url}"))?;
    if !sig_resp.status().is_success() {
        bail!("GET {sig_url} returned HTTP {}", sig_resp.status());
    }
    let sig_hex = sig_resp
        .text()
        .with_context(|| format!("read body of {sig_url}"))?
        .trim()
        .to_string();

    // Verify BEFORE these bytes are allowed anywhere near the cache.
    verify_bytes(&body, &sig_hex, &pubkey)
        .context("catalogue signature did not verify; refusing to replace the cache")?;

    // Only trusted bytes get parsed and cached.
    let cat = parse_catalogue(&body)?;
    if let Some(parent) = cache_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create cache dir {}", parent.display()))?;
    }
    write_atomic(cache_file, &body)?;
    let _ = fs::write(sig_sibling(cache_file), &sig_hex);
    match &new_etag {
        Some(etag) => {
            let _ = fs::write(etag_file, etag);
        }
        // A fresh body with no ETag: drop a stale one so we do not send a
        // mismatched If-None-Match next time.
        None => {
            let _ = fs::remove_file(etag_file);
        }
    }
    Ok(cat)
}

/// The response's `Content-Type`, or `no content type`.
fn content_type(resp: &reqwest::blocking::Response) -> String {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("no content type")
        .to_string()
}

/// A catalogue is a JSON object: its first non-whitespace byte is `{`.
fn starts_like_json_object(body: &[u8]) -> bool {
    body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{')
}

/// A short, printable description of a body for an error message.
fn preview(body: &[u8]) -> String {
    if body.is_empty() {
        return "an empty body".to_string();
    }
    let text = String::from_utf8_lossy(&body[..body.len().min(40)]);
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    format!("{} bytes starting {:?}", body.len(), flat.trim())
}

/// `<cache_file>.sig` (e.g. `catalogue.json.sig`).
fn sig_sibling(cache_file: &Path) -> PathBuf {
    let mut name = cache_file.file_name().unwrap_or_default().to_os_string();
    name.push(".sig");
    cache_file.with_file_name(name)
}

/// Write via a sibling temp file and rename, so a crash mid-write never leaves a
/// half-written catalogue under the final name.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::HubPubkeyEnv;
    use std::io::{Read, Write};
    use std::time::SystemTime;

    use xerj_autoindex::harvest::sign::generate_keypair;

    /// A `.key`/`.pub` keypair for the tests (hex seed, hex public key).
    fn keypair() -> (String, String) {
        generate_keypair().unwrap()
    }

    fn write_manifest(dir: &Path, slug: &str, sha: &str, spdx: &str, use_: &str) -> PathBuf {
        let p = dir.join(format!("{slug}.json"));
        let json = serde_json::json!({
            "corpus": slug,
            "cloned_at": "2026-10-02T00:49:45Z",
            "repos": [{
                "repo": slug,
                "url": format!("https://example.test/{slug}"),
                "licence": "hint only",
                "sha": sha,
                "files": 1,
                "bytes": 1,
                "review": {
                    "spdx": spdx,
                    "use": use_,
                    "by": "xerj-org",
                    "at": "2026-10-02",
                    "note": "test fixture"
                }
            }]
        });
        fs::write(&p, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
        p
    }

    fn two_inputs(dir: &Path) -> Vec<CorpusBuildInput> {
        let alpha = write_manifest(
            dir,
            "alpha",
            "1111111111111111111111111111111111111111",
            "MIT",
            "adapt-with-attribution",
        );
        let beta = write_manifest(
            dir,
            "beta",
            "2222222222222222222222222222222222222222",
            "GPL-3.0-only",
            "approach-only",
        );
        vec![
            CorpusBuildInput {
                manifest_path: alpha,
                domain: "DB".into(),
                refresh_cadence_days: 7,
                pack_bytes: 111,
                pack_url: "https://example.test/alpha.hubpack.tar.zst".into(),
                pack_sha256: "a".repeat(64),
                sig_url: "https://example.test/alpha.hubpack.tar.zst.sig".into(),
                quality_grade: Some("G7 4/5".into()),
                terms: vec!["alpha_sym".into(), "db".into(), "shared".into()],
                pin_sha: String::new(),
                ask_for: "an agent working on storage queries this for alpha".into(),
            },
            CorpusBuildInput {
                manifest_path: beta,
                domain: "SEC".into(),
                refresh_cadence_days: 1,
                pack_bytes: 222,
                pack_url: "https://example.test/beta.hubpack.tar.zst".into(),
                pack_sha256: "b".repeat(64),
                sig_url: "https://example.test/beta.hubpack.tar.zst.sig".into(),
                quality_grade: None,
                terms: vec!["beta_sym".into(), "sec".into(), "shared".into()],
                pin_sha: String::new(),
                ask_for: "an agent working on security queries this for beta".into(),
            },
        ]
    }

    fn entry<'a>(cat: &'a Catalogue, slug: &str) -> &'a CatalogueEntry {
        cat.corpora.iter().find(|e| e.slug == slug).unwrap()
    }

    #[test]
    fn build_catalogue_two_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = two_inputs(tmp.path());
        let (seed, public) = keypair();

        let (cat, sig) =
            build_catalogue(&inputs, "2026-10-14T00:00:00Z", &seed).expect("build catalogue");

        assert_eq!(cat.format_version, CATALOGUE_FORMAT_VERSION);
        assert_eq!(cat.corpora.len(), 2);
        // sorted by slug, regardless of input order
        assert_eq!(cat.corpora[0].slug, "alpha");
        assert_eq!(cat.corpora[1].slug, "beta");

        let a = entry(&cat, "alpha");
        assert_eq!(a.domain, "DB");
        assert_eq!(a.licence_spdx, "MIT");
        assert_eq!(a.licence_class, LicenceClass::AdaptWithAttribution);
        assert_eq!(a.pack_bytes, 111);
        // caller pin_sha was empty → manifest's first repo sha
        assert_eq!(a.pin_sha, "1111111111111111111111111111111111111111");
        assert_eq!(a.pin_date, "2026-10-02");
        assert_eq!(a.quality_grade.as_deref(), Some("G7 4/5"));
        // caller fallback, since the fixture manifest has no ask_for field
        assert!(a.ask_for.contains("alpha"));
        assert!(a.term_sketch.m_bits >= 8 * 1024);

        let b = entry(&cat, "beta");
        assert_eq!(b.licence_spdx, "GPL-3.0-only");
        assert_eq!(b.licence_class, LicenceClass::ApproachOnly);
        assert_eq!(b.pack_bytes, 222);
        assert_eq!(b.quality_grade, None);

        assert!(!sig.is_empty(), "signature must be present");
        // the detached signature verifies over the exact canonical bytes
        let bytes = canonical_json(&cat).unwrap();
        verify_bytes(&bytes, &sig, &public).expect("signature verifies");
    }

    #[test]
    fn signature_verify_rejects_tampered_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = two_inputs(tmp.path());
        let (seed, public) = keypair();
        let (cat, sig) = build_catalogue(&inputs, "2026-10-14T00:00:00Z", &seed).unwrap();

        let mut bytes = canonical_json(&cat).unwrap();
        // flipping any one byte must break verification
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x01;
        assert!(
            verify_bytes(&bytes, &sig, &public).is_err(),
            "a tampered catalogue must not verify"
        );
    }

    #[test]
    fn mixed_licence_reads_as_approach_only() {
        // a `mixed` review.use is the licence-unsafe case; it must map to the
        // restrictive class, not the permissive one.
        let tmp = tempfile::tempdir().unwrap();
        let path = write_manifest(
            tmp.path(),
            "lz4",
            "0774d05537f9762f838f7ab541b7765f1a729cb5",
            "BSD-2-Clause",
            "mixed",
        );
        let input = CorpusBuildInput {
            manifest_path: path,
            domain: "NET".into(),
            refresh_cadence_days: 7,
            pack_bytes: 1,
            pack_url: "u".into(),
            pack_sha256: "c".repeat(64),
            sig_url: "s".into(),
            quality_grade: None,
            terms: vec![],
            pin_sha: String::new(),
            ask_for: "fast compression".into(),
        };
        let (seed, _public) = keypair();
        let (cat, _sig) = build_catalogue(&[input], "2026-10-14T00:00:00Z", &seed).unwrap();
        assert_eq!(cat.corpora[0].licence_class, LicenceClass::ApproachOnly);
    }

    #[test]
    fn licence_spdx_follows_most_restrictive_repo() {
        // A multi-repo manifest whose FIRST repo is permissive (MIT) and whose
        // second is restrictive (GPL). `licence_class` already followed the most
        // restrictive repo; `licence_spdx` must now name that SAME repo, so the
        // entry is {GPL-3.0-only, ApproachOnly}, not {MIT, ApproachOnly}.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("multi.json");
        let json = serde_json::json!({
            "corpus": "multi",
            "cloned_at": "2026-10-02T00:49:45Z",
            "repos": [
                {
                    "repo": "permissive",
                    "sha": "1111111111111111111111111111111111111111",
                    "review": { "spdx": "MIT", "use": "adapt-with-attribution", "at": "2026-10-02" }
                },
                {
                    "repo": "restrictive",
                    "sha": "2222222222222222222222222222222222222222",
                    "review": { "spdx": "GPL-3.0-only", "use": "approach-only", "at": "2026-10-02" }
                }
            ]
        });
        fs::write(&p, serde_json::to_vec_pretty(&json).unwrap()).unwrap();

        let input = CorpusBuildInput {
            manifest_path: p,
            domain: "DB".into(),
            refresh_cadence_days: 7,
            pack_bytes: 1,
            pack_url: "u".into(),
            pack_sha256: "d".repeat(64),
            sig_url: "s".into(),
            quality_grade: None,
            terms: vec![],
            pin_sha: String::new(),
            ask_for: "x".into(),
        };
        let (seed, _public) = keypair();
        let (cat, _sig) = build_catalogue(&[input], "2026-10-14T00:00:00Z", &seed).unwrap();
        let e = &cat.corpora[0];
        assert_eq!(e.licence_class, LicenceClass::ApproachOnly);
        assert_eq!(e.licence_spdx, "GPL-3.0-only");
        // the pin sha still comes from the FIRST repo — unchanged by this fix.
        assert_eq!(e.pin_sha, "1111111111111111111111111111111111111111");
    }

    /// Build a catalogue, return `(bytes, sig, pubkey)` ready to seed a cache.
    fn fixture_bytes(dir: &Path) -> (Vec<u8>, String, String) {
        let inputs = two_inputs(dir);
        let (seed, public) = keypair();
        let (cat, sig) = build_catalogue(&inputs, "2026-10-14T00:00:00Z", &seed).unwrap();
        (canonical_json(&cat).unwrap(), sig, public)
    }

    fn seed_cache(cache_dir: &Path, bytes: &[u8], sig: &str) {
        fs::write(cache_dir.join(CACHE_FILE), bytes).unwrap();
        fs::write(sig_sibling(&cache_dir.join(CACHE_FILE)), sig).unwrap();
    }

    fn age_file(path: &Path, secs: u64) {
        let when = SystemTime::now()
            .checked_sub(Duration::from_secs(secs))
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    #[test]
    fn load_offline_uses_fresh_cache_without_network() {
        let env = HubPubkeyEnv::lock();
        let fx = tempfile::tempdir().unwrap();
        let (bytes, sig, public) = fixture_bytes(fx.path());

        let cache = tempfile::tempdir().unwrap();
        seed_cache(cache.path(), &bytes, &sig);

        env.set(&public);
        // the hub URL is unreachable on purpose — offline must never dial it
        let cat = load_catalogue("http://127.0.0.1:1", cache.path(), true).expect("offline load");
        env.remove();

        assert_eq!(cat.corpora.len(), 2);
        assert_eq!(cat.corpora[0].slug, "alpha");
    }

    /// A one-shot localhost hub that answers exactly two GETs (catalogue, then
    /// signature) and then exits, so the test can join it. Each response says
    /// `Connection: close`, forcing a fresh TCP connection per GET.
    fn spawn_hub(cat_body: Vec<u8>, sig_body: String) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let is_sig = req.lines().next().unwrap_or("").contains(".sig");
                let body: Vec<u8> = if is_sig {
                    sig_body.clone().into_bytes()
                } else {
                    cat_body.clone()
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                     Content-Length: {}\r\nETag: \"test-etag\"\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        (url, handle)
    }

    #[test]
    fn load_rejects_tampered_network_replacement_and_keeps_cache() {
        let env = HubPubkeyEnv::lock();
        let fx = tempfile::tempdir().unwrap();
        let (good_bytes, good_sig, public) = fixture_bytes(fx.path());

        // seed a VALID cache, then age it so the loader must refresh.
        let cache = tempfile::tempdir().unwrap();
        seed_cache(cache.path(), &good_bytes, &good_sig);
        age_file(&cache.path().join(CACHE_FILE), 48 * 3600);

        // the hub serves a TAMPERED catalogue with the (now mismatched) good sig
        let mut tampered = good_bytes.clone();
        tampered[good_bytes.len() / 2] ^= 0x01;
        let (url, handle) = spawn_hub(tampered, good_sig.clone());

        env.set(&public);
        let cat = load_catalogue(&url, cache.path(), false).expect("falls back to cached copy");
        env.remove();
        handle.join().unwrap();

        // the signature guarded the replacement: the GOOD cached copy is served,
        // not the tampered network bytes…
        let served = canonical_json(&cat).unwrap();
        assert_eq!(served, good_bytes, "tampered replacement must be rejected");
        // …and the on-disk cache is untouched.
        let on_disk = fs::read(cache.path().join(CACHE_FILE)).unwrap();
        assert_eq!(on_disk, good_bytes, "cache file must not be overwritten");
    }

    #[test]
    fn load_accepts_verified_network_replacement_into_empty_cache() {
        let env = HubPubkeyEnv::lock();
        let fx = tempfile::tempdir().unwrap();
        let (good_bytes, good_sig, public) = fixture_bytes(fx.path());

        // empty cache → the loader must fetch
        let cache = tempfile::tempdir().unwrap();
        let (url, handle) = spawn_hub(good_bytes.clone(), good_sig.clone());

        env.set(&public);
        let cat = load_catalogue(&url, cache.path(), false).expect("verified fetch");
        env.remove();
        handle.join().unwrap();

        assert_eq!(cat.corpora.len(), 2);
        // the verified bytes are now cached, byte-for-byte
        let on_disk = fs::read(cache.path().join(CACHE_FILE)).unwrap();
        assert_eq!(on_disk, good_bytes);
    }

    #[test]
    fn refresh_ignores_a_young_cache_and_fetches_the_newer_catalogue() {
        let env = HubPubkeyEnv::lock();
        let fx = tempfile::tempdir().unwrap();
        let inputs = two_inputs(fx.path());

        // A YOUNG cache holding yesterday's catalogue: `load_catalogue` trusts it.
        let (old_seed, _old_public) = keypair();
        let (old_cat, old_sig) =
            build_catalogue(&inputs, "2026-10-13T00:00:00Z", &old_seed).unwrap();
        let old_bytes = canonical_json(&old_cat).unwrap();
        let cache = tempfile::tempdir().unwrap();
        seed_cache(cache.path(), &old_bytes, &old_sig);

        // The hub now serves a newer catalogue, signed by the key the client trusts.
        let (new_seed, new_public) = keypair();
        let (new_cat, new_sig) =
            build_catalogue(&inputs, "2026-10-14T00:00:00Z", &new_seed).unwrap();
        let new_bytes = canonical_json(&new_cat).unwrap();
        let (url, handle) = spawn_hub(new_bytes.clone(), new_sig);

        env.set(&new_public);
        let trusted = load_catalogue(&url, cache.path(), false).expect("young cache is served");
        assert_eq!(
            trusted.generated_at, "2026-10-13T00:00:00Z",
            "load_catalogue trusts a young cache without asking the hub"
        );
        let refreshed = refresh_catalogue(&url, cache.path()).expect("forced refresh");
        env.remove();
        handle.join().unwrap();

        assert_eq!(
            refreshed.generated_at, "2026-10-14T00:00:00Z",
            "refresh_catalogue asked the hub despite the young cache"
        );
        let on_disk = fs::read(cache.path().join(CACHE_FILE)).unwrap();
        assert_eq!(on_disk, new_bytes, "the verified bytes replaced the cache");
    }

    #[test]
    fn refresh_reports_a_tampered_catalogue_instead_of_serving_the_cache() {
        let env = HubPubkeyEnv::lock();
        let fx = tempfile::tempdir().unwrap();
        let (good_bytes, good_sig, public) = fixture_bytes(fx.path());
        let cache = tempfile::tempdir().unwrap();
        seed_cache(cache.path(), &good_bytes, &good_sig);

        let mut tampered = good_bytes.clone();
        tampered[good_bytes.len() / 2] ^= 0x01;
        let (url, handle) = spawn_hub(tampered, good_sig.clone());

        env.set(&public);
        let err = refresh_catalogue(&url, cache.path())
            .expect_err("a refresh that cannot be verified is an error, not a stale answer");
        env.remove();
        handle.join().unwrap();

        assert!(
            format!("{err:#}").contains("signature"),
            "the error names the signature check: {err:#}"
        );
        let on_disk = fs::read(cache.path().join(CACHE_FILE)).unwrap();
        assert_eq!(on_disk, good_bytes, "the cache is untouched");
    }

    /// A localhost "static site" that answers every GET with the same HTML page
    /// and a 200, for `gets` requests.
    fn spawn_html_site(gets: usize) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for _ in 0..gets {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let body = "<!doctype html><html><body>hub</body></html>";
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        (url, handle)
    }

    #[test]
    fn an_html_page_served_as_the_catalogue_reads_as_unavailable() {
        let env = HubPubkeyEnv::lock();
        let fx = tempfile::tempdir().unwrap();
        let (_, _, public) = fixture_bytes(fx.path());
        let cache = tempfile::tempdir().unwrap();
        // One GET: the catalogue is refused before the signature is even asked for.
        let (url, handle) = spawn_html_site(1);

        env.set(&public);
        let err = load_catalogue(&url, cache.path(), false).expect_err("HTML is not a catalogue");
        env.remove();
        handle.join().unwrap();

        let msg = format!("{err:#}");
        assert!(msg.contains("the hub catalogue is unavailable"), "{msg}");
        assert!(
            msg.contains("text/html"),
            "the message names what came back: {msg}"
        );
        assert!(!cache.path().join(CACHE_FILE).exists(), "nothing is cached");
    }

    #[test]
    fn load_with_no_key_fetches_nothing_and_caches_nothing() {
        let env = HubPubkeyEnv::lock();
        // The loader used to fail OPEN here (cache the fetched copy unverified,
        // with a warning). With no key it now refuses before dialing the hub.
        env.set(xerj_common::hub::HUB_PUBKEY_NONE);
        let hub = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        hub.set_nonblocking(true).unwrap();
        let url = format!("http://{}", hub.local_addr().unwrap());
        let cache = tempfile::tempdir().unwrap();

        let err = load_catalogue(&url, cache.path(), false).unwrap_err();
        env.remove();

        assert!(
            format!("{err:#}").contains("no hub public key"),
            "the error says why: {err:#}"
        );
        assert!(
            matches!(hub.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "nothing may be fetched without a key"
        );
        assert!(!cache.path().join(CACHE_FILE).exists(), "nothing cached");
    }

    #[test]
    fn the_compiled_in_key_verifies_and_refuses_a_test_signed_catalogue() {
        let env = HubPubkeyEnv::lock();
        // No override: the shipping key (HUB_PUBKEY_HEX) is what verifies. A
        // catalogue signed by any other key — here a throwaway test key — must be
        // refused and must not reach the cache.
        env.remove();
        let fx = tempfile::tempdir().unwrap();
        let (good_bytes, good_sig, _public) = fixture_bytes(fx.path());
        let cache = tempfile::tempdir().unwrap();
        let (url, handle) = spawn_hub(good_bytes, good_sig);

        let err = load_catalogue(&url, cache.path(), false).unwrap_err();
        handle.join().unwrap();

        let msg = format!("{err:#}");
        // "does not verify" (not a parse error) also proves the constant parses
        // as a 32-byte key in the very function the client verifies with.
        assert!(msg.contains("signature does not verify"), "{msg}");
        assert!(!cache.path().join(CACHE_FILE).exists(), "nothing cached");
    }
}
