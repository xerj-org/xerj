//! The corpus state ledger: `{root}/state/<corpus>.json`.
//!
//! The ledger records that a corpus was indexed, against which URL, and —
//! since the #930 build-verify-swap — WHICH build was verified. Two prefixes
//! on purpose: `prefix` (`xc-<corpus>`, the whole namespace, unchanged since
//! the first release so anything that globs on it keeps working) and
//! `index_prefix` (`xc-<corpus>-b<stamp>`, the ONE verified build). Querying
//! the exact build keeps a half-built replacement — or a retired build whose
//! delete failed — out of the answers.
//!
//! The schema is FROZEN: a renamed field would silently orphan every existing
//! corpus. Legacy files (written before builds existed, no
//! `build`/`index_prefix`/`state_dir`) parse via hand-rolled tolerant reads
//! and select legacy mode in `xerj corpus index`.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::STALE_DAYS;

/// The `source` value a Corpus-Hub-v2 pack writes into its state file
/// ([`write_hubpack_state`]). A ledger carrying this came from a pulled,
/// pre-indexed pack, not from a local clone-and-index — the distinction the
/// staleness rule keys on (a hub pack is re-pinned by CI, so its freshness is
/// the catalogue's job, not a 30-day local refusal).
pub const SOURCE_HUBPACK: &str = "hubpack";

/// One parsed state file. Every field optional; unknown fields ignored.
#[derive(Debug, Clone, Default)]
pub struct CorpusState {
    pub corpus: Option<String>,
    pub indexed_at: Option<String>,
    pub prefix: Option<String>,
    pub url: Option<String>,
    /// int normally; a bool is tolerated as "finished" (older writers wrote
    /// the shell's idea of a boolean here).
    pub autoindex_exit: Option<Value>,
    pub salvaged: Option<bool>,
    pub build: Option<String>,
    pub index_prefix: Option<String>,
    pub state_dir: Option<String>,
    /// `"hubpack"` when the index came from a Corpus-Hub-v2 pack
    /// ([`write_hubpack_state`]); absent for a locally indexed corpus. Additive
    /// field — older readers ignore it, older files leave it `None`.
    pub source: Option<String>,
    /// The pack's pinned commit (hub packs only). Carried so the `def`/`code`
    /// answer can name the exact source revision without re-reading the pack.
    pub pin_sha: Option<String>,
    /// `YYYY-MM-DD` the pin was taken (hub packs only).
    pub pin_date: Option<String>,
    /// SPDX id of the pack's licence (hub packs only).
    pub licence_spdx: Option<String>,
    /// The pack's licence class, kebab-case (hub packs only) —
    /// `adapt-with-attribution` / `approach-only` / `excluded`.
    pub licence_class: Option<String>,
}

impl CorpusState {
    fn from_value(v: &Value) -> CorpusState {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        CorpusState {
            corpus: s("corpus"),
            indexed_at: s("indexed_at"),
            prefix: s("prefix"),
            url: s("url"),
            autoindex_exit: v.get("autoindex_exit").cloned().filter(|x| !x.is_null()),
            salvaged: v.get("salvaged").and_then(Value::as_bool),
            build: s("build"),
            index_prefix: s("index_prefix"),
            state_dir: s("state_dir"),
            source: s("source"),
            pin_sha: s("pin_sha"),
            pin_date: s("pin_date"),
            licence_spdx: s("licence_spdx"),
            licence_class: s("licence_class"),
        }
    }

    /// The corpus's own name for itself, falling back to the caller's.
    pub fn name(&self, fallback: &str) -> String {
        self.corpus.clone().unwrap_or_else(|| fallback.to_string())
    }

    /// True when this ledger came from a hub pack ([`SOURCE_HUBPACK`]). A hub
    /// pack is never refused for age — its freshness is the catalogue's job.
    pub fn is_hubpack(&self) -> bool {
        self.source.as_deref() == Some(SOURCE_HUBPACK)
    }
}

/// Read `{root}/state/{corpus}.json`. Missing → the actionable refusal that
/// names the command that fixes it.
pub fn load_state(root: &Path, corpus: &str) -> Result<CorpusState, String> {
    let path = root.join("state").join(format!("{corpus}.json"));
    let raw = std::fs::read_to_string(&path).map_err(|_| {
        format!("corpus '{corpus}' is not indexed — run `xerj corpus index {corpus}` first")
    })?;
    let v: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("state file {} is not valid JSON: {e}", path.display()))?;
    Ok(CorpusState::from_value(&v))
}

/// The index prefix to query: the verified build when the ledger names one,
/// else the namespace, else the conventional `xc-<corpus>`.
pub fn query_prefix(st: &CorpusState) -> String {
    st.index_prefix
        .clone()
        .or_else(|| st.prefix.clone())
        .unwrap_or_else(|| format!("xc-{}", st.name("")))
}

/// Staleness: an index older than [`STALE_DAYS`] days is REFUSED, not served.
///
/// A missing or unparsable stamp disables the check (and the footer's age):
/// never refuse on an unreadable clock. `Ok(None)` = no age known.
///
/// `Err` carries the exact refusal text — it names the re-index command and
/// the one override the caller's surface accepts (`stale_hint`: "`--stale-ok`"
/// on the CLI, "`stale_ok:true`" on MCP). There is no env bypass: a wrapper
/// must not be able to silently de-fang the refusal.
pub fn check_fresh(
    st: &CorpusState,
    stale_ok: bool,
    stale_hint: &str,
) -> Result<Option<i64>, String> {
    let Some(stamp) = st.indexed_at.as_deref() else {
        return Ok(None);
    };
    let Some(dt) = parse_stamp(stamp) else {
        return Ok(None);
    };
    let age = Utc::now().signed_duration_since(dt).num_days();
    // A hub pack is re-pinned by CI, not re-indexed locally, so a 30-day-old
    // local copy is not a stale local index — its freshness is the catalogue's
    // job (plan §2, the staleness-rule change). Report the age (the footer and
    // a newer-pin warning still use it) but never refuse it.
    if st.is_hubpack() {
        return Ok(Some(age));
    }
    if age > STALE_DAYS && !stale_ok {
        let corpus = st.name("");
        return Err(format!(
            "index for '{corpus}' is {age} days old. Re-run `xerj corpus index {corpus} --fresh`, \
             or pass {stale_hint}."
        ));
    }
    Ok(Some(age))
}

/// The ledger's own word that this index came from a run that did not finish.
///
/// `autoindex_exit` 0 is clean, 3 is "completed with junk/refused datasets"
/// (both finished); a bool is tolerated as finished. Anything else, or
/// `salvaged: true`, means coverage is NOT verified — and a miss against a
/// half-built corpus must never read as "this code does not exist".
pub fn incomplete_coverage(st: &CorpusState) -> Option<String> {
    if exit_is_finished(st.autoindex_exit.as_ref()) && st.salvaged != Some(true) {
        return None;
    }
    let corpus = st.name("");
    let why = match (&st.autoindex_exit, st.salvaged == Some(true)) {
        (Some(v), true) => format!("autoindex exit {v}, kept unverified"),
        (None, true) => "no autoindex exit recorded, kept unverified".to_string(),
        (Some(v), false) => format!("autoindex exit {v}"),
        (None, false) => "no autoindex exit recorded".to_string(),
    };
    Some(format!(
        "WARNING: the index for '{corpus}' was NOT verified complete ({why}). A miss here \
         might mean 'not indexed yet', not 'does not exist'. \
         Re-run `xerj corpus index {corpus}` to resume or confirm it."
    ))
}

/// The DISTINCT not-loaded-here diagnosis (exit 3, not the no-match exit 1).
///
/// "0 live indices here" and "the corpus is loaded but nothing matched" are
/// different diagnoses with different fixes; collapsing them is the bug this
/// guard closes. The hint says where the corpus WAS indexed when the ledger
/// knows a different URL than the one being queried.
pub fn not_loaded_message(st: &CorpusState, prefix: &str, url: &str) -> String {
    let corpus = st.name("");
    let indexed_at = st.indexed_at.clone().unwrap_or_else(|| "?".to_string());
    let where_hint = match &st.url {
        Some(recorded) if recorded != url => {
            format!(" It was indexed against {recorded}.")
        }
        _ => " It was indexed against a different data dir or server.".to_string(),
    };
    format!(
        "corpus '{corpus}' is in state/ (indexed {indexed_at}) but has 0 live indices \
         ('{prefix}*') on {url}.{where_hint} \
         Re-run: xerj corpus index {corpus}, or set XERJ_URL to the node that has it. \
         (This is NOT a 'no match' — the corpus simply is not loaded on this server.)"
    )
}

/// Whether an `autoindex_exit` value counts as "the run finished".
fn exit_is_finished(rc: Option<&Value>) -> bool {
    match rc {
        None => true,
        Some(Value::Bool(_)) => true,
        Some(Value::Number(n)) => n.as_i64().map(|i| i == 0 || i == 3).unwrap_or(false),
        Some(_) => false,
    }
}

/// The state file's stamp format, written and read verbatim:
/// `%Y-%m-%dT%H:%M:%SZ` UTC (what `xc-index.sh` wrote; unchanged).
pub const STAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// Parse a stamp in the ledger's exact format (also accepts general RFC3339).
fn parse_stamp(stamp: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(stamp, STAMP_FORMAT) {
        return Some(dt.and_utc());
    }
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Write the ledger ATOMICALLY (tmp + rename): a reader sees the old build or
/// the new one, never half. Key order and names are byte-identical to the
/// script's writer — a renamed field would silently orphan every corpus.
#[allow(clippy::too_many_arguments)]
pub fn write_state(
    root: &Path,
    corpus: &str,
    url: &str,
    rc: i64,
    salvaged: bool,
    build: Option<&str>,
    index_prefix: Option<&str>,
    state_dir: Option<&str>,
) -> std::io::Result<()> {
    let mut state = json!({
        "corpus": corpus,
        "indexed_at": Utc::now().format(STAMP_FORMAT).to_string(),
        "prefix": format!("xc-{corpus}"),
        "url": url,
        "autoindex_exit": rc,
        "salvaged": salvaged
    });
    if let (Some(b), Some(ip), Some(sd)) = (build, index_prefix, state_dir) {
        let obj = state.as_object_mut().unwrap();
        obj.insert("build".into(), json!(b));
        obj.insert("index_prefix".into(), json!(ip));
        obj.insert("state_dir".into(), json!(sd));
    }
    persist_state(root, corpus, &state)
}

/// Write the ledger for a corpus restored from a Corpus-Hub-v2 pack
/// (plan §2, `xerj hub pull`). It reuses the same atomic writer and the same
/// base schema as [`write_state`] so `xerj code <corpus>` finds it unchanged,
/// and adds the hub fields: `source: "hubpack"` (never refused for age — see
/// [`check_fresh`]), the pack's `pin_sha`/`pin_date` and its licence, so a
/// `def`/`code` answer can name the exact pinned source and its licence
/// without re-reading the pack.
///
/// `index_prefix` is the pack's `xc-<slug>-b<build>`; the verified-build token
/// is derived from it so the query path pins that one build (no
/// "no verified build recorded" widening). `autoindex_exit: 0` /
/// `salvaged: false` mark the pack complete (a CI-built pack is verified), so
/// no incomplete-coverage warning fires.
#[allow(clippy::too_many_arguments)]
pub fn write_hubpack_state(
    root: &Path,
    corpus: &str,
    url: &str,
    index_prefix: &str,
    pin_sha: &str,
    pin_date: &str,
    licence_spdx: &str,
    licence_class: &str,
) -> std::io::Result<()> {
    let mut state = json!({
        "corpus": corpus,
        "indexed_at": Utc::now().format(STAMP_FORMAT).to_string(),
        "prefix": format!("xc-{corpus}"),
        "url": url,
        "autoindex_exit": 0,
        "salvaged": false,
        "index_prefix": index_prefix,
        "source": SOURCE_HUBPACK,
        "pin_sha": pin_sha,
        "pin_date": pin_date,
        "licence_spdx": licence_spdx,
        "licence_class": licence_class,
    });
    // The verified-build token is the part of the prefix after `xc-<corpus>-`
    // (e.g. `xc-lz4-b1` -> `b1`); recording it keeps parity with a locally
    // indexed corpus's ledger. Absent only if the prefix is not the usual
    // shape — the query still pins the exact `index_prefix` either way.
    if let Some(build) = index_prefix.strip_prefix(&format!("xc-{corpus}-")) {
        if !build.is_empty() {
            state
                .as_object_mut()
                .unwrap()
                .insert("build".into(), json!(build));
        }
    }
    persist_state(root, corpus, &state)
}

/// Atomic write of a ledger `Value` to `{root}/state/{corpus}.json` (tmp +
/// rename): a reader sees the old build or the new one, never half. Compact
/// separators, trailing newline — the script's exact bytes.
fn persist_state(root: &Path, corpus: &str, state: &Value) -> std::io::Result<()> {
    let dir = root.join("state");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{corpus}.json"));
    let tmp = dir.join(format!(".{corpus}.json.tmp"));
    std::fs::write(&tmp, format!("{state}\n"))?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_file(root: &Path, corpus: &str) -> std::path::PathBuf {
        root.join("state").join(format!("{corpus}.json"))
    }

    fn write(root: &Path, corpus: &str, body: &str) {
        let p = state_file(root, corpus);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn missing_state_names_the_command_that_fixes_it() {
        let root = tempfile::tempdir().unwrap();
        let err = load_state(root.path(), "nope").unwrap_err();
        assert!(
            err.contains("corpus 'nope' is not indexed") && err.contains("xerj corpus index nope"),
            "must name the corpus and the fix command: {err}"
        );
    }

    #[test]
    fn legacy_state_parses_and_prefix_falls_back_in_order() {
        // A pre-#930 file: no build fields. Resolution must fall back
        // index_prefix -> prefix -> xc-{corpus}.
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "old",
            r#"{"corpus":"old","indexed_at":"2026-01-02T03:04:05Z","prefix":"xc-old","url":"http://localhost:9200","autoindex_exit":0}"#,
        );
        let st = load_state(root.path(), "old").unwrap();
        assert_eq!(query_prefix(&st), "xc-old");

        let mut st2 = st.clone();
        st2.prefix = None;
        assert_eq!(query_prefix(&st2), "xc-old", "corpus name fallback");

        // A #930 file: the VERIFIED build wins over the namespace.
        write(
            root.path(),
            "new",
            r#"{"corpus":"new","prefix":"xc-new","index_prefix":"xc-new-b20260801","build":"b20260801","state_dir":"/tmp/s","autoindex_exit":0,"salvaged":false}"#,
        );
        let st3 = load_state(root.path(), "new").unwrap();
        assert_eq!(query_prefix(&st3), "xc-new-b20260801");
    }

    // ── staleness (fail-before: stub returns Ok(None), these fail) ─────────

    fn aged(root: &Path, corpus: &str, days_ago: i64) -> CorpusState {
        let stamp = (Utc::now() - chrono::Duration::days(days_ago))
            .format(STAMP_FORMAT)
            .to_string();
        write(
            root,
            corpus,
            &format!(r#"{{"corpus":"{corpus}","indexed_at":"{stamp}","prefix":"xc-{corpus}"}}"#),
        );
        load_state(root, corpus).unwrap()
    }

    #[test]
    fn an_index_older_than_30_days_is_refused_with_the_reindex_command() {
        let root = tempfile::tempdir().unwrap();
        let st = aged(root.path(), "kv", 40);
        let err = check_fresh(&st, false, "`--stale-ok`").unwrap_err();
        assert!(
            err.contains("index for 'kv' is 40 days old"),
            "exact refusal text: {err}"
        );
        assert!(
            err.contains("xerj corpus index kv --fresh"),
            "names the fix command: {err}"
        );
        assert!(
            err.ends_with("or pass `--stale-ok`."),
            "names the caller's override: {err}"
        );
        // MCP spelling of the same refusal.
        let err = check_fresh(&st, false, "stale_ok:true").unwrap_err();
        assert!(err.ends_with("or pass stale_ok:true."), "{err}");
    }

    #[test]
    fn a_29_day_old_index_is_served_and_reports_its_age() {
        let root = tempfile::tempdir().unwrap();
        let st = aged(root.path(), "kv", 29);
        assert_eq!(check_fresh(&st, false, "--stale-ok").unwrap(), Some(29));
    }

    #[test]
    fn stale_ok_is_the_only_override_and_missing_stamps_disable_the_check() {
        let root = tempfile::tempdir().unwrap();
        let st = aged(root.path(), "kv", 31);
        assert_eq!(check_fresh(&st, true, "--stale-ok").unwrap(), Some(31));

        let mut no_stamp = st.clone();
        no_stamp.indexed_at = None;
        assert_eq!(check_fresh(&no_stamp, false, "--stale-ok").unwrap(), None);

        let mut junk = st.clone();
        junk.indexed_at = Some("not-a-date".into());
        assert_eq!(
            check_fresh(&junk, false, "--stale-ok").unwrap(),
            None,
            "an unparsable clock never refuses"
        );
    }

    // ── incomplete coverage (fail-before: stub returns None) ───────────────

    #[test]
    fn an_unfinished_or_salvaged_run_warns_on_every_query() {
        let mut st = CorpusState {
            corpus: Some("kv".into()),
            autoindex_exit: Some(json!(1)),
            ..Default::default()
        };
        let w = incomplete_coverage(&st).unwrap();
        assert!(
            w.starts_with(
                "WARNING: the index for 'kv' was NOT verified complete (autoindex exit 1)"
            ),
            "{w}"
        );
        assert!(
            w.contains("Re-run `xerj corpus index kv` to resume or confirm it."),
            "{w}"
        );

        st.autoindex_exit = Some(json!(0));
        st.salvaged = Some(true);
        let w = incomplete_coverage(&st).unwrap();
        assert!(
            w.contains("(autoindex exit 0, kept unverified)"),
            "salvaged rides the same warning: {w}"
        );

        st.salvaged = Some(false);
        for clean in [None, Some(json!(0)), Some(json!(3)), Some(json!(true))] {
            st.autoindex_exit = clean.clone();
            assert!(
                incomplete_coverage(&st).is_none(),
                "exit {clean:?} is finished and must not warn"
            );
        }
    }

    // ── not-loaded-here (fail-before: stub returns empty text) ─────────────

    #[test]
    fn zero_live_indices_gets_the_distinct_not_loaded_diagnosis() {
        let st = CorpusState {
            corpus: Some("kv".into()),
            indexed_at: Some("2026-08-01T00:00:00Z".into()),
            prefix: Some("xc-kv".into()),
            url: Some("http://other-host:9200".into()),
            ..Default::default()
        };
        let msg = not_loaded_message(&st, "xc-kv-b1", "http://localhost:9200");
        assert!(
            msg.contains(
                "corpus 'kv' is in state/ (indexed 2026-08-01T00:00:00Z) but has 0 live \
                          indices ('xc-kv-b1*') on http://localhost:9200."
            ),
            "{msg}"
        );
        assert!(
            msg.contains("It was indexed against http://other-host:9200."),
            "names the recorded url when it differs: {msg}"
        );
        assert!(
            msg.contains("Re-run: xerj corpus index kv, or set XERJ_URL to the node that has it."),
            "{msg}"
        );
        assert!(
            msg.contains(
                "(This is NOT a 'no match' — the corpus simply is not loaded on this server.)"
            ),
            "{msg}"
        );

        let same = CorpusState {
            url: Some("http://localhost:9200".into()),
            ..st.clone()
        };
        assert!(
            not_loaded_message(&same, "xc-kv", "http://localhost:9200")
                .contains("It was indexed against a different data dir or server."),
            "same-url variant names the data dir"
        );
    }

    #[test]
    fn write_state_is_atomic_and_schema_identical() {
        let root = tempfile::tempdir().unwrap();
        write_state(
            root.path(),
            "kv",
            "http://localhost:9200",
            0,
            false,
            Some("b20260921"),
            Some("xc-kv-b20260921"),
            Some("/tmp/autoindex-state/kv/b20260921"),
        )
        .unwrap();
        let raw = std::fs::read_to_string(state_file(root.path(), "kv")).unwrap();
        assert!(
            raw.starts_with("{\"corpus\":\"kv\",\"indexed_at\":\""),
            "key order frozen: {raw}"
        );
        assert!(
            raw.contains("\"prefix\":\"xc-kv\"")
                && raw.contains("\"autoindex_exit\":0")
                && raw.contains("\"salvaged\":false")
                && raw.contains("\"build\":\"b20260921\"")
                && raw.contains("\"index_prefix\":\"xc-kv-b20260921\"")
                && raw.contains("\"state_dir\":\"/tmp/autoindex-state/kv/b20260921\""),
            "{raw}"
        );
        assert!(
            raw.ends_with("}\n"),
            "trailing newline like the script: {raw:?}"
        );
        // No tmp file left behind.
        assert!(!root.path().join("state/.kv.json.tmp").exists());
        // Round-trips through the tolerant reader.
        let st = load_state(root.path(), "kv").unwrap();
        assert_eq!(st.index_prefix.as_deref(), Some("xc-kv-b20260921"));
    }

    #[test]
    fn hubpack_state_carries_source_pin_and_licence_and_pins_the_build() {
        let root = tempfile::tempdir().unwrap();
        write_hubpack_state(
            root.path(),
            "lz4",
            "http://localhost:9400",
            "xc-lz4-b1",
            "0774d05537f9762f838f7ab541b7765f1a729cb5",
            "2026-10-02",
            "BSD-2-Clause",
            "adapt-with-attribution",
        )
        .unwrap();
        let st = load_state(root.path(), "lz4").unwrap();
        assert!(st.is_hubpack(), "source must be hubpack: {:?}", st.source);
        assert_eq!(st.source.as_deref(), Some("hubpack"));
        assert_eq!(st.index_prefix.as_deref(), Some("xc-lz4-b1"));
        // The verified build is pinned (no namespace widening at query time).
        assert_eq!(query_prefix(&st), "xc-lz4-b1");
        assert_eq!(st.build.as_deref(), Some("b1"));
        assert_eq!(
            st.pin_sha.as_deref(),
            Some("0774d05537f9762f838f7ab541b7765f1a729cb5")
        );
        assert_eq!(st.pin_date.as_deref(), Some("2026-10-02"));
        assert_eq!(st.licence_spdx.as_deref(), Some("BSD-2-Clause"));
        assert_eq!(st.licence_class.as_deref(), Some("adapt-with-attribution"));
        // A verified pack must not trip the incomplete-coverage warning.
        assert!(incomplete_coverage(&st).is_none());
        // Atomic write leaves no tmp file.
        assert!(!root.path().join("state/.lz4.json.tmp").exists());
    }

    #[test]
    fn a_hub_pack_is_never_refused_for_age() {
        // The staleness-rule change: a 400-day-old hub pack still answers
        // (freshness is the catalogue's job), while a locally indexed corpus of
        // the same age is refused.
        let root = tempfile::tempdir().unwrap();
        let mut st = aged(root.path(), "lz4", 400);
        assert!(
            check_fresh(&st, false, "`--stale-ok`").is_err(),
            "a local index this old is refused"
        );
        st.source = Some(SOURCE_HUBPACK.to_string());
        assert_eq!(
            check_fresh(&st, false, "`--stale-ok`").unwrap(),
            Some(400),
            "a hub pack reports its age but is never refused"
        );
    }
}
