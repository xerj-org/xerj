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

/// repo -> recorded licence for a corpus. ALWAYS the recorded string, never
/// re-derived at query time: the licence decision was made once, at clone
/// time, against the checkout — re-deriving it per query is how prose drift
/// becomes load-bearing.
pub fn licence_map(root: &Path, corpus: &str) -> HashMap<String, String> {
    let path = root.join("corpora").join(corpus).join("corpus.json");
    read_corpus_manifest(&path)
        .map(|m| {
            m.repos
                .iter()
                .map(|r| (r.repo.clone(), r.licence.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// One repo entry as the writer emits it. Compact, key order pinned
/// (`repo,url,licence,sha,files,bytes[,review]`) — byte-identical to the
/// script's hand-built line so existing manifests diff clean against new ones.
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
    write_corpus_manifest_kind(path, corpus, None, cloned_at, repos, None)
}

/// [`write_corpus_manifest`] with an optional `kind` and query-hint block.
/// `None` for either emits byte-for-byte what the pinned format has always
/// been — a git corpus's manifest must not churn because a second kind of
/// corpus now exists. `Some("harvested")` inserts `"kind":"harvested"`
/// directly after `"corpus"`; only packs' corpora carry it, and `add` refuses
/// to mix the two kinds under one name. A `query` block (#1254) is appended
/// after `repos` verbatim — the corpus author's declaration must survive a
/// re-clone from a hub pin that carries it.
pub fn write_corpus_manifest_kind(
    path: &Path,
    corpus: &str,
    kind: Option<&str>,
    cloned_at: &str,
    repos: &[ManifestRepo],
    query: Option<&QueryHints>,
) {
    let kind_json = match kind {
        Some(k) => format!(",\"kind\":{}", Value::String(k.to_string())),
        None => String::new(),
    };
    let query_json = match query {
        Some(q) => match q.text_weight {
            Some(w) => format!(",\"query\":{{\"text_weight\":{w}}}"),
            None => String::new(),
        },
        None => String::new(),
    };
    let mut body = format!(
        "{{\"corpus\":{}{kind_json},\"cloned_at\":{},\"repos\":[\n",
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
    // A withdrawn pin is refused before any url is read. The field is how a
    // hub file says "do not clone this": the crypto-misuse source repository
    // returns HTTP 404, and a missing `url` alone only produced "missing
    // 'repo' or 'url'", which sent people looking for a typo.
    if let Some(reason) = v.get("withdrawn").and_then(Value::as_str) {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(format!("{}: 'withdrawn' is set but empty", path.display()));
        }
        return Err(format!(
            "{}: withdrawn, not cloneable: {reason}",
            path.display()
        ));
    }
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
        out.push(HubRow {
            repo: repo.to_string(),
            url: url.to_string(),
            sha: sha.to_string(),
            declared_licence: r
                .get("licence")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
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
        }];
        write_corpus_manifest_kind(&path, "rust-vulns", Some("harvested"), "t", &repos, None);
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
                },
                ManifestRepo {
                    repo: "sonic".into(),
                    url: "u2".into(),
                    licence: "MPL-2.0".into(),
                    sha: "s".into(),
                    files: None,
                    bytes: None,
                    review: None,
                },
            ],
        );
        let m = licence_map(&dir, "xerj-search");
        assert_eq!(m.get("sonic").map(String::as_str), Some("MPL-2.0"));
        assert!(!m.contains_key("nope"));
        assert!(licence_map(&dir, "never-cloned").is_empty());
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

    /// A hub pin whose source repository is gone must be refused before any
    /// `url` is handed to git. The crypto-misuse pin used to clone
    /// xerj-org/corpus-crypto-misuse-precedent, which returns HTTP 404.
    #[test]
    fn a_withdrawn_hub_manifest_is_refused_before_clone() {
        let dir = tmp();
        let bad = dir.join("crypto-misuse-precedent.json");
        std::fs::write(
            &bad,
            r#"{"corpus":"crypto-misuse-precedent","withdrawn":"the source repository returns HTTP 404","repos":[{"repo":"corpus-crypto-misuse-precedent","url":"https://github.com/xerj-org/corpus-crypto-misuse-precedent","sha":"a5a10175dc074483205b9609b97385ff557a3b56"}]}"#,
        )
        .unwrap();
        let err = read_hub_manifest(&bad).unwrap_err();
        assert!(err.contains("withdrawn"), "{err}");
        assert!(err.contains("404"), "{err}");
    }

    /// golang/vulndb rejects `6f4f07681f45fbbc11cf2f1543d4222b51e7808d`
    /// (`gh api` HTTP 422, "No commit found for SHA"). The review note already
    /// named `5cd8418c`, which GitHub serves as this full sha.
    #[test]
    fn go_vulndb_pin_is_the_commit_the_upstream_accepts() {
        let p = find_repo_root().join("tools/xerj-code/hub/go-vulndb.json");
        if !p.is_file() {
            return;
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let sha = v["repos"][0]["sha"].as_str().unwrap();
        assert_eq!(sha, "5cd8418cf9c867d344fda07a73ea844b0f34bca2");
    }

    /// The published crypto-misuse pin must not be a clone recipe. The
    /// repository returns HTTP 404; `corpus add --from` has to stop here.
    #[test]
    fn crypto_misuse_precedent_is_withdrawn_not_a_404_clone() {
        let p = find_repo_root().join("tools/xerj-code/hub/crypto-misuse-precedent.json");
        if !p.is_file() {
            return;
        }
        let err = read_hub_manifest(&p).unwrap_err();
        assert!(err.contains("404"), "{err}");
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            !text.contains("\"url\""),
            "a withdrawn pin must not carry a clone url"
        );
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
            let raw = std::fs::read_to_string(&p).unwrap_or_default();
            if raw.contains("\"withdrawn\"") {
                let err = read_hub_manifest(&p).expect_err("a withdrawn hub pin must be refused");
                assert!(err.contains("withdrawn"), "{}: {err}", p.display());
                checked += 1;
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
        write_corpus_manifest_kind(&path, "otel-proto", None, "t", &repos, Some(&hints));
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
