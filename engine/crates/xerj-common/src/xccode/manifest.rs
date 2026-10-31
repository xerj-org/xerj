//! `corpora/<corpus>/corpus.json` — the per-corpus manifest — plus hub
//! manifests (`--from`), which are UNTRUSTED INPUT and validated as such.
//!
//! The manifest is regenerated FROM DISK at clone time, never copied through
//! from the input, so it always describes the checkout it sits beside. Its
//! byte format is pinned: humans diff these files by hand and share them in
//! chats, so the shape must not churn between releases.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::pathgate;

/// One repo entry. Tolerant parse: fields the writer has not always emitted
/// (`files`, `bytes`, `review`) are optional, unknown keys are ignored, so a
/// manifest written by any historical version still loads.
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestRepo {
    pub repo: String,
    pub url: String,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub licence: String,
    #[serde(default)]
    pub files: Option<u64>,
    #[serde(default)]
    pub bytes: Option<u64>,
    /// Vetted hub records carry a review block (`spdx`, `use`, `by`, `at`,
    /// `note`). It is PRESERVED verbatim across a rebuild when repo+sha
    /// match — a human's licence review must survive a re-index.
    #[serde(default)]
    pub review: Option<Value>,
    /// Directories or files this checkout harvested (`data/osv`). Empty means the
    /// whole pin. Author declaration, like [`CorpusManifest::query`]: it is
    /// written back so a shared `corpus.json` rebuilds the same limited
    /// tree. Absent on every historical manifest.
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CorpusManifest {
    #[serde(default)]
    pub corpus: String,
    /// `"harvested"` for corpora materialized from a pack (`corpus add --from
    /// <pack>`); absent for cloned-repo corpora. Readers must treat absent as
    /// the repo shape — every historical manifest has no `kind`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub cloned_at: Option<String>,
    #[serde(default)]
    pub repos: Vec<ManifestRepo>,
    /// Author-declared query hints (#1254), kept RAW: a malformed hint block
    /// must not make the whole manifest unparseable — that would silently
    /// blank the licence map, a far worse failure than an ignored hint.
    /// Validation happens in [`query_text_weight`] / [`read_hub_manifest`].
    #[serde(default)]
    pub query: Option<Value>,
    /// First-party marker (#2339). `true` only when the operator passed
    /// `xerj corpus add --own` or `xerj corpus own`. Absent on every
    /// historical manifest, and never taken from a `--from` document: that
    /// file is untrusted input and must not be able to silence the
    /// do-not-copy banner. A non-bool value is read as false so a corrupt
    /// key cannot blank the licence map.
    #[serde(default, deserialize_with = "deserialize_own")]
    pub own: bool,
}

/// Only JSON `true` counts. Anything else — including a string an untrusted
/// file might carry — is not the operator's marker.
fn deserialize_own<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value.as_bool() == Some(true))
}

/// `corpus.json`'s optional `"query"` block (#1254). The corpus AUTHOR knows
/// what the plain-text family carries in THEIR corpus — the primary content
/// (otel-proto: every `.proto` definition is a txt-lines record) or mirror
/// noise beside code-family `body` records (the #1238 exploit group, where
/// `text^1.0` was measured flooding the top-10 with sibling-CVE demos). That
/// discriminator is not visible at query time: mixed datasets and mixed index
/// mappings look the same from the client either way. So it is declared, once,
/// here, and the query path honours it or ignores it loudly.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
pub struct QueryHints {
    /// Weight for the `text` recall leg (`text^0.5` by default). Honest
    /// bounds are enforced by the reader, not trusted from the file.
    #[serde(default)]
    pub text_weight: Option<f64>,
}

/// The corpus's declared text-leg weight. `None` = nothing declared (every
/// historical manifest — the 0.5 default applies), `Some(Ok(w))` = declared,
/// `Some(Err(msg))` = declared but unusable (the caller warns and uses the
/// default rather than silently guessing what was meant).
pub fn query_text_weight(root: &Path, corpus: &str) -> Option<Result<f64, String>> {
    let path = root.join("corpora").join(corpus).join("corpus.json");
    let m = read_corpus_manifest(&path).ok()?;
    let raw = m.query?;
    let hints: QueryHints = match serde_json::from_value(raw) {
        Ok(h) => h,
        Err(e) => return Some(Err(format!("query block is malformed: {e}"))),
    };
    let w = hints.text_weight?;
    match w {
        w if (0.25..=4.0).contains(&w) => Some(Ok(w)),
        w => Some(Err(format!("query.text_weight {w} is outside 0.25..=4.0"))),
    }
}

/// Read a corpus manifest. Missing file is `Err` with the path named — the
/// caller decides whether that is fatal (`corpus add` regenerates it;
/// `xerj code` reports the corpus as licence-unmapped).
pub fn read_corpus_manifest(path: &Path) -> Result<CorpusManifest, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read manifest {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("{} is not valid JSON: {e}", path.display()))
}

/// Whether this corpus was marked first-party (#2339). Missing manifest,
/// a missing key, or any value other than JSON `true` is false — the
/// do-not-copy banner stays on.
pub fn corpus_is_own(root: &Path, corpus: &str) -> bool {
    let path = root.join("corpora").join(corpus).join("corpus.json");
    read_corpus_manifest(&path).is_ok_and(|m| m.own)
}

/// Set or clear the first-party marker on an existing `corpus.json`.
///
/// Rewrites through the pinned writer. `own: false` omits the key, which is
/// what every historical manifest looks like. The recorded licences are not
/// touched. A malformed `query` block is refused rather than dropped.
pub fn set_corpus_own(path: &Path, corpus: &str, own: bool) -> Result<(), String> {
    let m = read_corpus_manifest(path)?;
    let query = match m.query {
        None => None,
        Some(q) => Some(
            serde_json::from_value::<QueryHints>(q)
                .map_err(|e| format!("{}: query block is malformed: {e}", path.display()))?,
        ),
    };
    let cloned_at = m.cloned_at.unwrap_or_default();
    write_corpus_manifest_kind(
        path,
        corpus,
        m.kind.as_deref(),
        &cloned_at,
        &m.repos,
        query.as_ref(),
        own,
    );
    let back = read_corpus_manifest(path)?;
    if back.own != own {
        return Err(format!("could not record own={own} in {}", path.display()));
    }
    Ok(())
}

/// repo -> recorded licence for a corpus. ALWAYS the recorded data, never
/// re-derived at query time: the licence decision was made once, at clone
/// time, against the checkout — re-deriving it per query is how prose drift
/// becomes load-bearing. The recorded data includes the repo's human review:
/// when the detector could not identify a licence and a completed review
/// names one, the review's SPDX is the label (#1893, see
/// [`super::licence::effective_licence`]).
pub fn licence_map(root: &Path, corpus: &str) -> HashMap<String, String> {
    let path = root.join("corpora").join(corpus).join("corpus.json");
    read_corpus_manifest(&path)
        .map(|m| {
            m.repos
                .iter()
                .map(|r| {
                    (
                        r.repo.clone(),
                        super::licence::effective_licence(&r.licence, r.review.as_ref()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One repo entry as the writer emits it. Compact, key order pinned
/// (`repo,url,licence,sha,files,bytes[,paths][,review]`) — byte-identical to
/// the script's hand-built line so existing manifests diff clean against new
/// ones. `paths` is omitted when empty, which is every manifest written
/// before a subpath harvest existed.
fn entry_json(repo: &ManifestRepo) -> String {
    let mut m = Map::new();
    m.insert("repo".into(), Value::String(repo.repo.clone()));
    m.insert("url".into(), Value::String(repo.url.clone()));
    m.insert("licence".into(), Value::String(repo.licence.clone()));
    m.insert("sha".into(), Value::String(repo.sha.clone()));
    if let Some(f) = repo.files {
        m.insert("files".into(), Value::from(f));
    }
    if let Some(b) = repo.bytes {
        m.insert("bytes".into(), Value::from(b));
    }
    if !repo.paths.is_empty() {
        m.insert("paths".into(), Value::from(repo.paths.clone()));
    }
    if let Some(r) = &repo.review {
        m.insert("review".into(), r.clone());
    }
    Value::Object(m).to_string()
}

/// Write the manifest in the pinned format:
///
/// ```text
/// {"corpus":"kv","cloned_at":"2026-08-06T12:00:00Z","repos":[
///   {"repo":"valkey","url":"https://github.com/valkey/valkey","licence":"BSD-3-Clause","sha":"abc…","files":912,"bytes":1234567}
/// ]}
/// ```
///
/// Atomic (tmp + rename): a half-written manifest describes a checkout that
/// does not exist, and `xerj corpus list` reads these.
pub fn write_corpus_manifest(path: &Path, corpus: &str, cloned_at: &str, repos: &[ManifestRepo]) {
    write_corpus_manifest_kind(path, corpus, None, cloned_at, repos, None, false)
}

/// [`write_corpus_manifest`] with an optional `kind`, query-hint block, and
/// first-party marker. `None` / `false` for those emits byte-for-byte what
/// the pinned format has always been — a git corpus's manifest must not
/// churn because a second kind of corpus, or an own-code marker, now exists.
/// `Some("harvested")` inserts `"kind":"harvested"` directly after
/// `"corpus"`; only packs' corpora carry it, and `add` refuses to mix the
/// two kinds under one name. `own: true` inserts `"own":true` after
/// `cloned_at` (#2339). A `query` block (#1254) is appended after `repos`
/// verbatim — the corpus author's declaration must survive a re-clone from
/// a hub pin that carries it.
#[allow(clippy::too_many_arguments)]
pub fn write_corpus_manifest_kind(
    path: &Path,
    corpus: &str,
    kind: Option<&str>,
    cloned_at: &str,
    repos: &[ManifestRepo],
    query: Option<&QueryHints>,
    own: bool,
) {
    let kind_json = match kind {
        Some(k) => format!(",\"kind\":{}", Value::String(k.to_string())),
        None => String::new(),
    };
    let own_json = if own { ",\"own\":true" } else { "" };
    let query_json = match query {
        Some(q) => match q.text_weight {
            Some(w) => format!(",\"query\":{{\"text_weight\":{w}}}"),
            None => String::new(),
        },
        None => String::new(),
    };
    let mut body = format!(
        "{{\"corpus\":{}{kind_json},\"cloned_at\":{}{own_json},\"repos\":[\n",
        Value::String(corpus.to_string()),
        Value::String(cloned_at.to_string())
    );
    for (i, r) in repos.iter().enumerate() {
        if i > 0 {
            body.push_str(",\n");
        }
        body.push_str("  ");
        body.push_str(&entry_json(r));
    }
    body.push_str(&format!("\n]{query_json}}}\n"));
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// A validated row of a hub manifest, ready for the clone loop.
#[derive(Debug, Clone)]
pub struct HubRow {
    pub repo: String,
    pub url: String,
    pub sha: String,
    pub declared_licence: String,
    /// The row's `review` block, verbatim (a human's licence review of the
    /// repo at `sha`). `xerj corpus add` records it in the cloned corpus.json
    /// when the checkout lands at that same sha (#1893).
    pub review: Option<Value>,
    /// Repo-relative directories or files to harvest. Empty harvests the
    /// whole pin (#1842, #2486). Already validated by [`read_hub_manifest`].
    pub paths: Vec<String>,
}

/// A whole validated hub manifest: the corpus name plus its rows. The
/// `corpus` field is the name `xerj corpus add --from` uses when the
/// caller gave none — dropping it (an earlier port bug) made `--from`
/// unusable without a redundant positional.
#[derive(Debug, Clone)]
pub struct HubManifest {
    pub corpus: String,
    pub rows: Vec<HubRow>,
    /// The pin's `query` block (#1254), carried into the cloned corpus.json.
    pub query: Option<QueryHints>,
}

/// `paths` on one repo entry. Absent harvests the whole pin. Present must be
/// a non-empty array of relative directories or files; a string or an empty array is
/// a mistake, not a request for the whole repo, and is refused so a typo
/// cannot silently clone everything (#1842).
fn read_sparse_paths(repo: &str, row: &Value) -> Result<Vec<String>, String> {
    let Some(raw) = row.get("paths") else {
        return Ok(Vec::new());
    };
    let Some(arr) = raw.as_array() else {
        return Err(format!(
            "repo '{repo}': 'paths' must be a non-empty array of relative directories \
             or files such as [\"data/osv\"]; omit the field to harvest the whole pin"
        ));
    };
    if arr.is_empty() {
        return Err(format!(
            "repo '{repo}': 'paths' is empty; omit the field to harvest the whole pin, \
             or name at least one directory or file"
        ));
    }
    if arr.len() > pathgate::MAX_SPARSE_PATHS {
        return Err(format!(
            "repo '{repo}': 'paths' has {} entries; at most {} paths can be \
             sparse-checked out. Name a parent directory instead.",
            arr.len(),
            pathgate::MAX_SPARSE_PATHS
        ));
    }
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let Some(text) = entry.as_str() else {
            return Err(format!("repo '{repo}': 'paths' entries must be strings"));
        };
        let path = pathgate::valid_sparse_path(text).map_err(|e| format!("repo '{repo}': {e}"))?;
        if !out.iter().any(|seen: &String| seen == &path) {
            out.push(path);
        }
    }
    Ok(out)
}

/// Parse and VALIDATE a hub manifest (`--from`). Untrusted input rules from
/// the original `read_manifest`, enforced once, here, before any path is
/// built from a field: `repo` becomes a directory that is later
/// force-checked-out and cleaned inside, and a short sha is not fetchable,
/// so it silently rebuilds at the tip — the opposite of a pin.
pub fn read_hub_manifest(path: &Path) -> Result<HubManifest, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("no such manifest: {} ({e})", path.display()))?;
    let v: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    let repos = v
        .get("repos")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| format!("{} has no 'repos' array", path.display()))?;
    let corpus = v
        .get("corpus")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // The pin's optional query-hint block (#1254): carried verbatim into the
    // cloned corpus's corpus.json so the author's declaration survives a
    // re-clone. Malformed is refused here, in the validator, rather than
    // half-applied at query time.
    let query = match v.get("query") {
        None => None,
        Some(q) => Some(
            serde_json::from_value::<QueryHints>(q.clone())
                .map_err(|e| format!("{}: bad 'query' block: {e}", path.display()))?,
        ),
    };
    if !corpus.is_empty() {
        pathgate::valid_corpus_name(&corpus).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let mut out = Vec::new();
    for r in repos {
        let repo = r.get("repo").and_then(Value::as_str).unwrap_or("");
        let url = r.get("url").and_then(Value::as_str).unwrap_or("");
        if repo.is_empty() || url.is_empty() {
            return Err(format!(
                "{}: an entry is missing 'repo' or 'url'",
                path.display()
            ));
        }
        pathgate::valid_repo_name(repo).map_err(|e| format!("{}: {e}", path.display()))?;
        let sha = r.get("sha").and_then(Value::as_str).unwrap_or("");
        pathgate::valid_sha(repo, sha).map_err(|e| format!("{}: {e}", path.display()))?;
        let paths = read_sparse_paths(repo, r).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push(HubRow {
            repo: repo.to_string(),
            url: url.to_string(),
            sha: sha.to_string(),
            declared_licence: r
                .get("licence")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            review: r.get("review").filter(|v| v.is_object()).cloned(),
            paths,
        });
    }
    Ok(HubManifest {
        corpus,
        rows: out,
        query,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp() -> std::path::PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    #[test]
    fn round_trip_is_byte_stable_and_review_survives() {
        let dir = tmp();
        let path = dir.join("corpus.json");
        let review = json!({ "spdx": "Apache-2.0", "use": "search engine internals", "by": "adi", "at": "2026-08-06", "note": "safe" });
        let repos = vec![ManifestRepo {
            repo: "valkey".into(),
            url: "https://github.com/valkey/valkey".into(),
            licence: "BSD-3-Clause".into(),
            sha: "31081d9f05014003321333553bb3e657eb3da168".into(),
            files: Some(912),
            bytes: Some(1_234_567),
            review: Some(review.clone()),
            paths: Vec::new(),
        }];
        write_corpus_manifest(&path, "kv", "2026-08-06T12:00:00Z", &repos);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "{\"corpus\":\"kv\",\"cloned_at\":\"2026-08-06T12:00:00Z\",\"repos\":[\n  \
             {\"repo\":\"valkey\",\"url\":\"https://github.com/valkey/valkey\",\"licence\":\
             \"BSD-3-Clause\",\"sha\":\"31081d9f05014003321333553bb3e657eb3da168\",\"files\":912,\
             \"bytes\":1234567,\"review\":{\"spdx\":\"Apache-2.0\",\"use\":\"search engine \
             internals\",\"by\":\"adi\",\"at\":\"2026-08-06\",\"note\":\"safe\"}}\n]}\n"
        );
        let back = read_corpus_manifest(&path).unwrap();
        assert_eq!(back.repos[0].review, Some(review));
        assert_eq!(back.corpus, "kv");
    }

    #[test]
    fn harvested_kind_emits_after_corpus_and_reads_back() {
        let dir = tmp();
        let path = dir.join("corpus.json");
        let repos = vec![ManifestRepo {
            repo: "rustsec".into(),
            url: String::new(),
            licence: "CC0-1.0".into(),
            sha: String::new(),
            files: Some(2),
            bytes: Some(512),
            review: None,
            paths: Vec::new(),
        }];
        write_corpus_manifest_kind(
            &path,
            "rust-vulns",
            Some("harvested"),
            "t",
            &repos,
            None,
            false,
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("{\"corpus\":\"rust-vulns\",\"kind\":\"harvested\",\"cloned_at\":"),
            "{text}"
        );
        let back = read_corpus_manifest(&path).unwrap();
        assert_eq!(back.kind.as_deref(), Some("harvested"));
        assert_eq!(back.repos[0].repo, "rustsec");

        // and the pinned git shape is UNCHANGED by the extension
        write_corpus_manifest(&path, "kv", "t", &repos);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("{\"corpus\":\"kv\",\"cloned_at\":"),
            "{text}"
        );
        assert!(read_corpus_manifest(&path).unwrap().kind.is_none());
    }

    #[test]
    fn licence_map_comes_from_the_record_not_the_prose() {
        let dir = tmp();
        let corpora = dir.join("corpora").join("xerj-search");
        std::fs::create_dir_all(&corpora).unwrap();
        write_corpus_manifest(
            &corpora.join("corpus.json"),
            "xerj-search",
            "t",
            &[
                ManifestRepo {
                    repo: "tantivy".into(),
                    url: "u1".into(),
                    licence: "Apache-2.0/MIT".into(),
                    sha: "s".into(),
                    files: None,
                    bytes: None,
                    review: None,
                    paths: Vec::new(),
                },
                ManifestRepo {
                    repo: "sonic".into(),
                    url: "u2".into(),
                    licence: "MPL-2.0".into(),
                    sha: "s".into(),
                    files: None,
                    bytes: None,
                    review: None,
                    paths: Vec::new(),
                },
            ],
        );
        let m = licence_map(&dir, "xerj-search");
        assert_eq!(m.get("sonic").map(String::as_str), Some("MPL-2.0"));
        assert!(!m.contains_key("nope"));
        assert!(licence_map(&dir, "never-cloned").is_empty());
    }

    #[test]
    fn licence_map_uses_a_completed_review_for_an_unknown_licence() {
        // #1893: 18f-content-guide's checkout reads UNKNOWN, its review says
        // CC0-1.0; passages must be labelled CC0-1.0, not "do not copy".
        let dir = tmp();
        let pin = dir.join("18f.json");
        let sha = "1b1723d3d5b8f91d92c16487c88b56265dc0ec3a";
        let review = json!({ "spdx": "CC0-1.0", "use": "adapt-with-attribution", "by": "xerj-org", "at": "2026-10-13", "note": "LICENSE.md verified" });
        std::fs::write(&pin, json!({ "corpus": "18f-content-guide", "repos": [ { "repo": "content-guide", "url": "https://github.com/18F/content-guide", "licence": "CC0-1.0", "sha": sha, "review": review } ] }).to_string()).unwrap();
        let hub = read_hub_manifest(&pin).unwrap();
        assert_eq!(hub.rows[0].review.as_ref(), Some(&review));

        let cdir = dir.join("corpora").join("18f-content-guide");
        std::fs::create_dir_all(&cdir).unwrap();
        write_corpus_manifest_kind(
            &cdir.join("corpus.json"),
            "18f-content-guide",
            None,
            "2026-10-19T00:00:00Z",
            &[ManifestRepo {
                repo: "content-guide".into(),
                url: "https://github.com/18F/content-guide".into(),
                licence: "UNKNOWN".into(),
                sha: sha.into(),
                files: None,
                bytes: None,
                review: hub.rows[0].review.clone(),
                paths: Vec::new(),
            }],
            None,
            false,
        );
        let m = licence_map(&dir, "18f-content-guide");
        assert_eq!(m.get("content-guide").map(String::as_str), Some("CC0-1.0"));
    }

    /// #2339: `own` is absent on the historical byte format, round-trips when
    /// set, and a non-bool value does not fail the manifest (that would blank
    /// the licence map).
    #[test]
    fn own_is_omitted_unless_set_and_only_json_true_counts() {
        let dir = tmp();
        let path = dir.join("corpus.json");
        let repos = vec![ManifestRepo {
            repo: "mine".into(),
            url: "/src".into(),
            licence: "NONE-FOUND".into(),
            sha: "abc".into(),
            files: Some(1),
            bytes: Some(2),
            review: None,
            paths: Vec::new(),
        }];
        write_corpus_manifest(&path, "mine", "t", &repos);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("own"), "{text}");
        assert!(!read_corpus_manifest(&path).unwrap().own);

        write_corpus_manifest_kind(&path, "mine", None, "t", &repos, None, true);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"cloned_at\":\"t\",\"own\":true,\"repos\":"),
            "{text}"
        );
        assert!(read_corpus_manifest(&path).unwrap().own);
        assert_eq!(
            read_corpus_manifest(&path).unwrap().repos[0].licence,
            "NONE-FOUND"
        );

        std::fs::write(
            &path,
            "{\"corpus\":\"mine\",\"own\":\"yes\",\"repos\":[{\"repo\":\"mine\",\"url\":\"u\",\"licence\":\"UNKNOWN\"}]}",
        )
        .unwrap();
        let parsed = read_corpus_manifest(&path).unwrap();
        assert!(!parsed.own, "a string is not the operator's marker");
        assert_eq!(parsed.repos[0].licence, "UNKNOWN");

        let root = tmp();
        let corpora = root.join("corpora").join("mine");
        std::fs::create_dir_all(&corpora).unwrap();
        let manifest = corpora.join("corpus.json");
        std::fs::copy(&path, &manifest).unwrap();
        assert!(!corpus_is_own(&root, "mine"));
        set_corpus_own(&manifest, "mine", true).unwrap();
        assert!(corpus_is_own(&root, "mine"));
        assert_eq!(
            read_corpus_manifest(&manifest).unwrap().repos[0].licence,
            "UNKNOWN",
            "setting own does not rewrite the recorded licence"
        );
        set_corpus_own(&manifest, "mine", false).unwrap();
        let cleared = std::fs::read_to_string(&manifest).unwrap();
        assert!(!cleared.contains("own"), "{cleared}");
        assert!(!corpus_is_own(&root, "mine"));
        assert!(!corpus_is_own(&root, "missing"));
    }

    #[test]
    fn hub_validation_rejects_path_escapes_and_short_shas() {
        let dir = tmp();
        let bad = dir.join("evil.json");
        std::fs::write(&bad, json!({ "repos": [ { "repo": "../../work", "url": "u", "sha": "31081d9f05014003321333553bb3e657eb3da168" } ] }).to_string()).unwrap();
        let err = read_hub_manifest(&bad).unwrap_err();
        assert!(err.contains("force-checked-out"), "{err}");

        std::fs::write(
            &bad,
            json!({ "repos": [ { "repo": "valkey", "url": "u", "sha": "e449d17" } ] }).to_string(),
        )
        .unwrap();
        let err = read_hub_manifest(&bad).unwrap_err();
        assert!(err.contains("not a full 40-character sha"), "{err}");

        std::fs::write(
            &bad,
            json!({ "repos": [ { "repo": "valkey" } ] }).to_string(),
        )
        .unwrap();
        assert!(read_hub_manifest(&bad)
            .unwrap_err()
            .contains("missing 'repo' or 'url'"));
        std::fs::write(&bad, json!({ "repos": [] }).to_string()).unwrap();
        assert!(read_hub_manifest(&bad)
            .unwrap_err()
            .contains("no 'repos' array"));
    }

    /// The hub's own vetted manifests must stay valid under the SAME gate the
    /// `--from` path enforces — they are the reference for "rebuild a corpus
    /// someone else defined", so a hub file that the tool itself would reject
    /// is a bug in the hub. Reads them from the repo via CARGO_MANIFEST_DIR
    /// ancestors (the published_schema_drift.rs house pattern).
    #[test]
    fn hub_manifests_pass_the_untrusted_input_gate() {
        let hub = find_repo_root().join("tools/xerj-code/hub");
        let mut checked = 0;
        for entry in std::fs::read_dir(&hub).expect("hub dir") {
            let p = entry.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let hub = read_hub_manifest(&p)
                .unwrap_or_else(|e| panic!("{} failed the gate: {e}", p.display()));
            assert!(!hub.rows.is_empty());
            // Every hub manifest names its corpus — `--from` depends on it.
            assert!(
                !hub.corpus.is_empty(),
                "{} has no 'corpus' field",
                p.display()
            );
            checked += 1;
        }
        assert!(
            checked >= 1,
            "no hub manifests found under {}",
            hub.display()
        );
    }

    #[test]
    fn query_hints_round_trip_and_default_stays_pinned() {
        let dir = tmp();
        let path = dir.join("corpus.json");
        let repos = vec![ManifestRepo {
            repo: "opentelemetry-proto".into(),
            url: "https://github.com/open-telemetry/opentelemetry-proto".into(),
            licence: "Apache-2.0".into(),
            sha: "b3f7558".into(),
            files: None,
            bytes: None,
            review: None,
            paths: Vec::new(),
        }];
        // No hints: byte-for-byte the historical pinned format (the
        // round-trip test above pins the exact bytes; here the absence).
        write_corpus_manifest(&path, "otel-proto", "t", &repos);
        let plain = std::fs::read_to_string(&path).unwrap();
        assert!(plain.ends_with("]}\n") && !plain.contains("query"));
        assert_eq!(read_corpus_manifest(&path).unwrap().query, None);
        // With hints: appended after repos, parses back, survives a rewrite.
        let hints = QueryHints {
            text_weight: Some(1.0),
        };
        write_corpus_manifest_kind(&path, "otel-proto", None, "t", &repos, Some(&hints), false);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.ends_with("],\"query\":{\"text_weight\":1}}\n"),
            "{text}"
        );
        let m = read_corpus_manifest(&path).unwrap();
        assert_eq!(m.query, Some(json!({"text_weight": 1})));
        // A corpus with a block but no usable weight is still a valid
        // manifest — the hint is simply absent.
        std::fs::write(
            &path,
            "{\"corpus\":\"otel-proto\",\"repos\":[],\"query\":{}}",
        )
        .unwrap();
        assert_eq!(read_corpus_manifest(&path).unwrap().query, Some(json!({})));
    }

    #[test]
    fn query_text_weight_bounds_and_error_shapes() {
        let dir = tmp();
        let corpus_dir = dir.join("corpora").join("otel-proto");
        std::fs::create_dir_all(&corpus_dir).unwrap();
        let path = corpus_dir.join("corpus.json");
        let repo_line = "{\"repo\":\"r\",\"url\":\"u\"}";
        // absent block -> None (every historical manifest)
        std::fs::write(
            &path,
            format!("{{\"corpus\":\"c\",\"repos\":[{repo_line}]}}"),
        )
        .unwrap();
        assert_eq!(query_text_weight(&dir, "otel-proto"), None);
        // declared in bounds -> Ok
        std::fs::write(
            &path,
            format!(
                "{{\"corpus\":\"c\",\"repos\":[{repo_line}],\"query\":{{\"text_weight\":1.0}}}}"
            ),
        )
        .unwrap();
        assert_eq!(query_text_weight(&dir, "otel-proto"), Some(Ok(1.0)));
        // declared out of bounds -> Err naming the value (warned, ignored)
        for bad in [0.1f64, 9.0] {
            std::fs::write(
                &path,
                format!(
                    "{{\"corpus\":\"c\",\"repos\":[{repo_line}],\"query\":{{\"text_weight\":{bad}}}}}"
                ),
            )
            .unwrap();
            match query_text_weight(&dir, "otel-proto") {
                Some(Err(msg)) => assert!(msg.contains(&bad.to_string()), "{msg}"),
                other => panic!("expected Err for {bad}, got {other:?}"),
            }
        }
        // declared non-numeric -> Err (never silently coerced)
        std::fs::write(
            &path,
            format!(
                "{{\"corpus\":\"c\",\"repos\":[{repo_line}],\"query\":{{\"text_weight\":\"high\"}}}}"
            ),
        )
        .unwrap();
        assert!(query_text_weight(&dir, "otel-proto").is_some_and(|r| r.is_err()));
        // missing corpus -> None (licence-map tolerance: absent, not fatal)
        assert_eq!(query_text_weight(&dir, "no-such"), None);
    }

    #[test]
    fn hub_pin_query_block_is_carried_and_malformed_is_refused() {
        let dir = tmp();
        let pin = dir.join("otel-proto.json");
        std::fs::write(
            &pin,
            "{\"corpus\":\"otel-proto\",\"repos\":[{\"repo\":\"opentelemetry-proto\",\"url\":\
             \"https://github.com/open-telemetry/opentelemetry-proto\",\"sha\":\
             \"b3f7558a0123456789abcdef0123456789abcdef\"}],\"query\":{\"text_weight\":1.0}}",
        )
        .unwrap();
        let hub = read_hub_manifest(&pin).unwrap();
        assert_eq!(
            hub.query,
            Some(QueryHints {
                text_weight: Some(1.0)
            })
        );
        // no block -> None
        std::fs::write(
            &pin,
            "{\"corpus\":\"otel-proto\",\"repos\":[{\"repo\":\"r\",\"url\":\"u\",\"sha\":\"b3f7558a0123456789abcdef0123456789abcdef\"}]}",
        )
        .unwrap();
        assert_eq!(read_hub_manifest(&pin).unwrap().query, None);
        // malformed -> refused by the validator, not half-applied later
        std::fs::write(
            &pin,
            "{\"corpus\":\"otel-proto\",\"repos\":[{\"repo\":\"r\",\"url\":\"u\",\"sha\":\"b3f7558a0123456789abcdef0123456789abcdef\"}],\
             \"query\":{\"text_weight\":true}}",
        )
        .unwrap();
        let err = read_hub_manifest(&pin).unwrap_err();
        assert!(err.contains("query"), "{err}");
    }

    /// #1842: a repo-pin can name the directories to harvest. The field is
    /// per repo, normalized, and written back so a shared corpus.json rebuilds
    /// the same limited tree. A bad value is refused here, before git runs.
    #[test]
    fn hub_paths_are_normalized_written_back_and_refused_when_unsafe() {
        let dir = tmp();
        let pin = dir.join("go-vulndb.json");
        let sha = "5cd8418cf9c867d344fda07a73ea844b0f34bca2";
        std::fs::write(
            &pin,
            json!({
                "corpus": "go-vulndb",
                "repos": [{
                    "repo": "corpus-go-vulndb-osv",
                    "url": "https://github.com/golang/vulndb",
                    "sha": sha,
                    "paths": ["data/osv/", "data/osv", "cmd"]
                }]
            })
            .to_string(),
        )
        .unwrap();
        let hub = read_hub_manifest(&pin).unwrap();
        assert_eq!(
            hub.rows[0].paths,
            vec!["data/osv".to_string(), "cmd".to_string()]
        );

        let repos = vec![ManifestRepo {
            repo: "corpus-go-vulndb-osv".into(),
            url: "https://github.com/golang/vulndb".into(),
            licence: "BSD-3-Clause".into(),
            sha: sha.into(),
            files: Some(2),
            bytes: Some(40),
            review: None,
            paths: hub.rows[0].paths.clone(),
        }];
        let written = dir.join("corpus.json");
        write_corpus_manifest(&written, "go-vulndb", "t", &repos);
        let text = std::fs::read_to_string(&written).unwrap();
        assert!(text.contains("\"paths\":[\"data/osv\",\"cmd\"]"), "{text}");
        assert!(text.find("\"paths\"").unwrap() < text.find("}").unwrap());
        let back = read_hub_manifest(&written).unwrap();
        assert_eq!(back.rows[0].paths, hub.rows[0].paths);
        // A manifest with no paths field is the historical whole-pin harvest.
        std::fs::write(
            &pin,
            json!({
                "corpus": "go-vulndb",
                "repos": [{ "repo": "corpus-go-vulndb-osv", "url": "u", "sha": sha }]
            })
            .to_string(),
        )
        .unwrap();
        assert!(read_hub_manifest(&pin).unwrap().rows[0].paths.is_empty());

        for (body, needle) in [
            (
                json!({ "corpus": "go-vulndb", "repos": [{ "repo": "r", "url": "u", "sha": sha, "paths": "data/osv" }] }),
                "must be a non-empty array",
            ),
            (
                json!({ "corpus": "go-vulndb", "repos": [{ "repo": "r", "url": "u", "sha": sha, "paths": [] }] }),
                "paths' is empty",
            ),
            (
                json!({ "corpus": "go-vulndb", "repos": [{ "repo": "r", "url": "u", "sha": sha, "paths": ["../secret"] }] }),
                "escapes the repository",
            ),
            (
                json!({ "corpus": "go-vulndb", "repos": [{ "repo": "r", "url": "u", "sha": sha, "paths": ["/etc"] }] }),
                "is absolute",
            ),
            (
                json!({ "corpus": "go-vulndb", "repos": [{ "repo": "r", "url": "u", "sha": sha, "paths": ["data/osv/*"] }] }),
                "glob",
            ),
        ] {
            std::fs::write(&pin, body.to_string()).unwrap();
            let err = read_hub_manifest(&pin).unwrap_err();
            assert!(err.contains(needle), "expected {needle:?} in {err}");
        }
        let too_many: Vec<String> = (0..33).map(|i| format!("d{i}")).collect();
        std::fs::write(
            &pin,
            json!({
                "corpus": "go-vulndb",
                "repos": [{ "repo": "r", "url": "u", "sha": sha, "paths": too_many }]
            })
            .to_string(),
        )
        .unwrap();
        let err = read_hub_manifest(&pin).unwrap_err();
        assert!(err.contains("at most 32"), "{err}");
    }

    fn find_repo_root() -> std::path::PathBuf {
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        loop {
            if dir.join("tools/xerj-code/hub").is_dir() {
                return dir;
            }
            if !dir.pop() {
                panic!("repo root not found above {}", env!("CARGO_MANIFEST_DIR"));
            }
        }
    }
}
