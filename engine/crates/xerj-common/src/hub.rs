//! Corpus Hub v2 — the types shared across crates.
//!
//! The hub serves PRE-INDEXED packs an agent pulls and queries, instead of a
//! JSON manifest it must clone and index locally (discussion #1374,
//! `docs/CORPUS_HUB_V2_IMPLEMENTATION.md`). Every type that crosses a crate
//! boundary or the network lives here so `xerj-hub`, `xerj-api`, `xerj-mcp`
//! and the CLI all speak one vocabulary.
//!
//! `xerj-common` depends on no other xerj crate, so this module holds only the
//! data shapes and the one renderer that turns an [`Answer`] into the text every
//! surface prints. The behaviour (download, verify, restore, route) is in
//! `xerj-hub`.
//!
//! Anything deserialized from the network carries `#[serde(deny_unknown_fields)]`
//! so a catalogue or a pack manifest from a future format version is refused
//! rather than half-read.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// The catalogue shape this build writes. 2 adds optional `upstream`
/// (`owner/name` of the pack's git remote) on each entry. Readers still load
/// format 1 (that field is absent and reads as empty) so a cached catalogue
/// from before this field keeps working. A newer shape is refused.
/// See [`catalogue_format_supported`].
pub const CATALOGUE_FORMAT_VERSION: u32 = 2;
/// Oldest catalogue shape this build still loads.
pub const CATALOGUE_FORMAT_VERSION_MIN: u32 = 1;

/// True for a catalogue `format_version` this build can read.
pub fn catalogue_format_supported(version: u32) -> bool {
    (CATALOGUE_FORMAT_VERSION_MIN..=CATALOGUE_FORMAT_VERSION).contains(&version)
}

/// Bump when the pack archive layout changes. `format_version: 1` means the
/// snapshot layout (engine `create_snapshot`/`restore_snapshot`).
pub const PACK_FORMAT_VERSION: u32 = 1;

/// Where the catalogue and the hosted query API live.
pub const DEFAULT_HUB_URL: &str = "https://hub.xerj.org";

/// The hub's local root: `XERJ_HUB_HOME` when set and non-empty, else
/// `<home>/.xerj/hub` ([`crate::home::home_dir_from`]: `HOME`, else
/// `USERPROFILE`). `None` when neither is set.
///
/// This is THE rule. The CLI (`xerj hub pull`, which extracts packs under
/// `<root>/packs`) and the server (which, by default, lets a snapshot restore
/// read from that same `<root>/packs` — `limits.hub_pack_restore`) both resolve
/// it here, so a server started by the same user agrees with the CLI on where a
/// pulled pack lives.
pub fn hub_root_from(
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    if let Some(h) = env("XERJ_HUB_HOME").filter(|h| !h.is_empty()) {
        return Some(std::path::PathBuf::from(h));
    }
    crate::home::home_dir_from(env).map(|home| home.join(".xerj").join("hub"))
}

/// The directory `xerj hub pull` extracts verified packs into:
/// [`hub_root_from`]`/packs`, over the process environment.
pub fn hub_packs_dir() -> Option<std::path::PathBuf> {
    hub_root_from(&|key| std::env::var_os(key)).map(|root| root.join(HUB_PACKS_DIR))
}

/// The pack-cache directory name under the hub root.
pub const HUB_PACKS_DIR: &str = "packs";

/// Packs at or below this size are pulled automatically; larger ones are
/// answered through the hosted route until the user runs `xerj hub pull`.
pub const AUTO_PULL_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// The local pack cache is pruned (least-recently-used) to stay under this.
pub const DISK_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Hex of the ed25519 public key every hub catalogue and pack must verify
/// against: the public half of the `HUB_SIGNING_SEED` CI secret, derived from
/// it by `.github/workflows/hub-pubkey.yml` (run 38869497662, #1494).
///
/// It changes ONLY together with that secret. Rotating the seed without this
/// constant (or the other way round) makes every client refuse every pack;
/// `hub-packs.yml` and `hub-pubkey.yml` both compare the two and fail loudly on
/// a mismatch. Tests point the client at a throwaway key with the
/// `XERJ_HUB_PUBKEY` environment variable — see [`hub_pubkey`].
pub const HUB_PUBKEY_HEX: &str = "b68899321ec01ad8d0130d6e4790d9c75ffbc03bb0271f0a1522114c43562936";

/// The `XERJ_HUB_PUBKEY` value that makes this process behave as a build with
/// no hub key (tests only): the no-key paths stay reachable now that a real key
/// is compiled in. It can only take capability away — with no key every hub
/// surface refuses to fetch or extract anything, it never skips a check.
pub const HUB_PUBKEY_NONE: &str = "none";

/// The key to verify hub catalogues and packs with: a non-empty
/// `XERJ_HUB_PUBKEY` (tests) wins over the compiled-in [`HUB_PUBKEY_HEX`].
/// `None` means "this build cannot verify anything": the override is
/// [`HUB_PUBKEY_NONE`], or no key is compiled in. Callers refuse rather than
/// proceed unverified.
pub fn hub_pubkey() -> Option<String> {
    match std::env::var("XERJ_HUB_PUBKEY") {
        Ok(k) if k.trim().eq_ignore_ascii_case(HUB_PUBKEY_NONE) => None,
        Ok(k) if !k.trim().is_empty() => Some(k.trim().to_string()),
        _ => Some(HUB_PUBKEY_HEX.to_string()).filter(|k| !k.is_empty()),
    }
}

/// True when this build can verify hub catalogues and packs, i.e. when
/// [`hub_pubkey`] has a key: the compiled-in [`HUB_PUBKEY_HEX`], or a test
/// `XERJ_HUB_PUBKEY`. With `XERJ_HUB_PUBKEY=none` ([`HUB_PUBKEY_NONE`]) it is
/// false, and the hub surfaces (`xerj ask`, `xerj hub sync|pull`, the MCP hub
/// tools, `xerj init`'s hub skill) say plainly that the hub is unavailable
/// instead of failing on whatever the network returns.
pub fn hub_key_configured() -> bool {
    hub_pubkey().is_some()
}

/// What every hub surface says in a build with no hub key.
pub const NO_HUB_KEY_MESSAGE: &str = "Corpus Hub packs are not available in this build: it has \
     no hub signing key, so no catalogue or pack can be verified and nothing was downloaded. \
     For reference code, index the projects yourself with `xerj corpus add` and search them \
     with `xerj code` (see `xerj code --help`). A folder you indexed with `xerj autoindex` \
     is queried with `xerj search` or `xerj def`, not `xerj ask`.";

/// What a per-source licence review allows, straight from the registry's
/// `review.use`. Governs whether a pack is pulled silently, with a warning, or
/// never.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum LicenceClass {
    /// Public-domain, OGL, permissive — pull silently.
    AdaptWithAttribution,
    /// GPL/AGPL/SSPL/BUSL and the like — pull with a "read, do not copy" warning.
    ApproachOnly,
    /// Closed or non-redistributable — never pulled.
    Excluded,
}

/// One corpus as the catalogue lists it. The client picks a corpus from this
/// without downloading anything.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct CatalogueEntry {
    pub slug: String,
    /// SEC, DB, NET, CRYPTO, WEB, API, OPS, LAW, HEALTH, AI, WRITE.
    pub domain: String,
    pub topics: Vec<String>,
    /// The registry's one-line "an agent working on ___ queries this for ___".
    pub ask_for: String,
    /// Upstream git repo as `owner/name` (the manifest's `repos[].url`, GitHub
    /// form). Several repos are joined with ", " (at most four). Empty on a
    /// catalogue written before this field, or when the manifest recorded no
    /// `owner/name` remote — never guessed from the slug.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub upstream: String,
    /// Full 40-hex commit the pack was built at.
    pub pin_sha: String,
    /// YYYY-MM-DD the pin was taken.
    pub pin_date: String,
    /// How often this corpus is re-pinned (7 for code, 1 for feeds).
    pub refresh_cadence_days: u32,
    pub licence_spdx: String,
    pub licence_class: LicenceClass,
    pub pack_bytes: u64,
    pub pack_url: String,
    pub pack_sha256: String,
    pub sig_url: String,
    /// crate / npm / go-module / pypi names this corpus answers for; used by the
    /// project-dependency router. Empty when none are recorded — never guessed.
    pub dependency_names: Vec<String>,
    /// e.g. "G7 4/5", or None. Never invented.
    pub quality_grade: Option<String>,
    pub term_sketch: Sketch,
}

/// A Bloom filter over a corpus's identifiers and vocabulary, used to route a
/// question to candidate corpora offline. See `xerj-hub`'s `sketch` module for
/// the hashing.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Sketch {
    pub m_bits: u32,
    pub k_hashes: u32,
    pub bits_base64: String,
}

/// The whole catalogue: one small signed file, refreshed in the background.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    pub format_version: u32,
    /// RFC 3339.
    pub generated_at: String,
    pub corpora: Vec<CatalogueEntry>,
}

/// `pack.json` inside a pack archive.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub format_version: u32,
    pub slug: String,
    pub pin_sha: String,
    pub pin_date: String,
    /// RFC 3339 the pack was built.
    pub built_at: String,
    /// `xerj --version` of the builder.
    pub builder_version: String,
    /// `xc-<slug>-b<build>`.
    pub index_prefix: String,
    /// Every index in the snapshot, the docs index and the defs index included.
    pub indices: Vec<PackIndex>,
    /// The name used with `create_snapshot`.
    pub snapshot_name: String,
    pub licence_spdx: String,
    pub licence_class: LicenceClass,
    /// Copied from the manifest's `repos[]`.
    pub sources: Vec<PackSource>,
    pub defs_count: u64,
    pub files_count: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PackIndex {
    pub name: String,
    pub docs: u64,
    pub bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PackSource {
    pub repo: String,
    pub url: String,
    pub sha: String,
    pub licence: String,
}

/// One line of `defs.jsonl` and one document of the `-defs` index: a single
/// definition, with the literal value for constants and macros.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DefRecord {
    pub symbol: String,
    /// As `extract/code.rs` names it: function, const, define, struct, …
    pub kind: String,
    pub lang: String,
    /// Path relative to the corpus root, forward slashes.
    pub file: String,
    /// 1-based line the NAME sits on.
    pub line: u32,
    /// The declaration, signature only, at most 400 chars.
    pub text: String,
    /// The literal value for constants/macros/defines; None otherwise.
    pub value: Option<String>,
}

/// Where an answer came from.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AnsweredFrom {
    /// A pack already on this machine.
    LocalPack,
    /// The hub's hosted query node.
    Hosted,
    /// A corpus indexed locally the old way (not a hub pack).
    LocalIndex,
}

/// Which shape of question this answer is.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AnswerMode {
    /// One definition, with its value.
    Def,
    /// Ranked passages for a how-does-it-work question.
    How,
    /// The file list with per-path licence.
    Files,
}

/// One retrieved definition or passage.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Hit {
    pub file: String,
    pub line: u32,
    pub kind: String,
    pub text: String,
    pub value: Option<String>,
    pub score: f32,
    /// Set for an approach-only source, naming the licence.
    pub licence_warning: Option<String>,
}

/// What every surface returns: the CLI (as `--json`), the MCP tools and the
/// hosted HTTP endpoints all build this and render it with [`render`].
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub corpus: String,
    pub pin_sha: String,
    pub pin_date: String,
    pub age_days: u32,
    pub licence_spdx: String,
    pub licence_class: LicenceClass,
    pub answered_from: AnsweredFrom,
    pub mode: AnswerMode,
    pub hits: Vec<Hit>,
    pub truncated: bool,
    /// One line each, e.g. "pin is 23 days old; the question asks for the current source".
    pub warnings: Vec<String>,
}

impl AnsweredFrom {
    /// The words printed on the `answered from:` line.
    pub fn label(self) -> &'static str {
        match self {
            AnsweredFrom::LocalPack => "local pack",
            AnsweredFrom::Hosted => "hosted hub",
            AnsweredFrom::LocalIndex => "local index",
        }
    }
}

/// Render an [`Answer`] to the text every surface prints. The format is fixed by
/// `docs/CORPUS_HUB_V2_IMPLEMENTATION.md` §1:
///
/// ```text
/// <corpus>  pin <pin_date> (<age_days> day[s] old)  <licence_spdx>  <file>:<line>
/// <text>
/// [value: <value>]            # only in def mode when value is Some
/// answered from: <local pack|hosted hub|local index>
/// [warning: …]                # one line per warning
/// ```
///
/// For several hits the first lines repeat per hit; the `answered from:` line is
/// printed once at the end. With no hits a single line states the corpus and pin.
pub fn render(answer: &Answer) -> String {
    let age = if answer.age_days == 1 {
        "1 day old".to_string()
    } else {
        format!("{} days old", answer.age_days)
    };
    let header_prefix = format!(
        "{}  pin {} ({})  {}",
        answer.corpus, answer.pin_date, age, answer.licence_spdx
    );

    let mut out = String::new();
    if answer.hits.is_empty() {
        let _ = write!(
            out,
            "{header_prefix}\nno match in {} (pin {})",
            answer.corpus, answer.pin_date
        );
    } else {
        for hit in &answer.hits {
            let _ = write!(
                out,
                "{header_prefix}  {}:{}\n{}",
                hit.file, hit.line, hit.text
            );
            if answer.mode == AnswerMode::Def {
                if let Some(v) = &hit.value {
                    let _ = write!(out, "\nvalue: {v}");
                }
            }
            if let Some(w) = &hit.licence_warning {
                let _ = write!(out, "\n  {w}");
            }
            out.push('\n');
        }
        out.pop(); // drop the trailing newline before the footer
    }

    let _ = write!(out, "\nanswered from: {}", answer.answered_from.label());
    for w in &answer.warnings {
        let _ = write!(out, "\nwarning: {w}");
    }
    out
}

#[cfg(test)]
mod tests {
    fn hub_env<'a>(
        vars: &'a [(&'a str, &'a str)],
    ) -> impl Fn(&str) -> Option<std::ffi::OsString> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| std::ffi::OsString::from(v))
        }
    }

    #[test]
    fn the_hub_root_rule_is_xerj_hub_home_then_home_then_nothing() {
        use std::path::PathBuf;
        assert_eq!(
            hub_root_from(&hub_env(&[
                ("XERJ_HUB_HOME", "/srv/hub"),
                ("HOME", "/home/u")
            ])),
            Some(PathBuf::from("/srv/hub"))
        );
        assert_eq!(
            hub_root_from(&hub_env(&[("XERJ_HUB_HOME", ""), ("HOME", "/home/u")])),
            Some(PathBuf::from("/home/u/.xerj/hub"))
        );
        assert_eq!(
            hub_root_from(&hub_env(&[("USERPROFILE", "/Users/u")])),
            Some(PathBuf::from("/Users/u/.xerj/hub"))
        );
        // No home at all: the server grants nothing (never `./.xerj/hub`).
        assert_eq!(hub_root_from(&hub_env(&[])), None);
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn a_build_without_a_hub_key_points_ask_at_local_search() {
        assert!(NO_HUB_KEY_MESSAGE.contains("`xerj search`"));
        assert!(NO_HUB_KEY_MESSAGE.contains("`xerj def`"));
        assert!(NO_HUB_KEY_MESSAGE.contains("`xerj autoindex`"));
        assert!(NO_HUB_KEY_MESSAGE.contains("not `xerj ask`"));
        assert!(!NO_HUB_KEY_MESSAGE.contains("xerj hub sync"));
    }

    #[test]
    fn the_compiled_in_hub_key_is_32_bytes_of_lower_hex() {
        // The workflows read it with a `[0-9a-fA-F]*` sed and compare lower-case;
        // ed25519.sh and verify_bytes both want exactly 64 hex characters.
        assert_eq!(HUB_PUBKEY_HEX.len(), 64, "{HUB_PUBKEY_HEX}");
        assert!(
            HUB_PUBKEY_HEX
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{HUB_PUBKEY_HEX}"
        );
    }

    #[test]
    fn the_hub_key_stays_on_the_one_line_the_workflows_read() {
        // hub-packs.yml and hub-pubkey.yml extract the key with a single-line
        // sed over this file. If a reformat ever wrapped the declaration, they
        // would read "" and go quietly dormant instead of publishing.
        let line = format!("\npub const HUB_PUBKEY_HEX: &str = \"{HUB_PUBKEY_HEX}\";\n");
        assert!(
            include_str!("hub.rs").replace("\r\n", "\n").contains(&line),
            "{line}"
        );
    }

    #[test]
    fn licence_class_is_kebab_case() {
        assert_eq!(
            serde_json::to_value(LicenceClass::AdaptWithAttribution).unwrap(),
            json!("adapt-with-attribution")
        );
        assert_eq!(
            serde_json::to_value(LicenceClass::ApproachOnly).unwrap(),
            json!("approach-only")
        );
        assert_eq!(
            serde_json::to_value(LicenceClass::Excluded).unwrap(),
            json!("excluded")
        );
        let c: LicenceClass = serde_json::from_value(json!("approach-only")).unwrap();
        assert_eq!(c, LicenceClass::ApproachOnly);
    }

    #[test]
    fn answered_from_and_mode_are_snake_case() {
        assert_eq!(
            serde_json::to_value(AnsweredFrom::LocalPack).unwrap(),
            json!("local_pack")
        );
        assert_eq!(
            serde_json::to_value(AnsweredFrom::Hosted).unwrap(),
            json!("hosted")
        );
        assert_eq!(
            serde_json::to_value(AnsweredFrom::LocalIndex).unwrap(),
            json!("local_index")
        );
        assert_eq!(serde_json::to_value(AnswerMode::Def).unwrap(), json!("def"));
        assert_eq!(serde_json::to_value(AnswerMode::How).unwrap(), json!("how"));
        assert_eq!(
            serde_json::to_value(AnswerMode::Files).unwrap(),
            json!("files")
        );
    }

    fn sample_entry() -> CatalogueEntry {
        CatalogueEntry {
            slug: "lz4".into(),
            domain: "NET".into(),
            topics: vec!["compression".into()],
            ask_for: "fast block compression".into(),
            upstream: "lz4/lz4".into(),
            pin_sha: "0774d05537f9762f838f7ab541b7765f1a729cb5".into(),
            pin_date: "2026-10-02".into(),
            refresh_cadence_days: 7,
            licence_spdx: "BSD-2-Clause".into(),
            licence_class: LicenceClass::AdaptWithAttribution,
            pack_bytes: 2_000_000,
            pack_url: "https://example/lz4.hubpack.tar.zst".into(),
            pack_sha256: "deadbeef".into(),
            sig_url: "https://example/lz4.hubpack.tar.zst.sig".into(),
            dependency_names: vec!["lz4".into()],
            quality_grade: None,
            term_sketch: Sketch {
                m_bits: 64,
                k_hashes: 7,
                bits_base64: "AAAAAAAAAAA=".into(),
            },
        }
    }

    #[test]
    fn catalogue_round_trips() {
        let cat = Catalogue {
            format_version: CATALOGUE_FORMAT_VERSION,
            generated_at: "2026-10-14T00:00:00Z".into(),
            corpora: vec![sample_entry()],
        };
        let s = serde_json::to_string(&cat).unwrap();
        let back: Catalogue = serde_json::from_str(&s).unwrap();
        assert_eq!(back.corpora.len(), 1);
        assert_eq!(back.corpora[0].slug, "lz4");
        assert_eq!(back.corpora[0].upstream, "lz4/lz4");
        assert_eq!(
            back.corpora[0].licence_class,
            LicenceClass::AdaptWithAttribution
        );
    }

    /// A catalogue written before `upstream` existed has no such key. It still
    /// loads, and the field is empty rather than invented from the slug.
    #[test]
    fn a_catalogue_entry_without_upstream_loads_as_empty() {
        let mut cat = Catalogue {
            format_version: CATALOGUE_FORMAT_VERSION_MIN,
            generated_at: "2026-10-14T00:00:00Z".into(),
            corpora: vec![sample_entry()],
        };
        cat.corpora[0].upstream.clear();
        let mut v = serde_json::to_value(&cat).unwrap();
        v["corpora"][0].as_object_mut().unwrap().remove("upstream");
        let back: Catalogue = serde_json::from_value(v).unwrap();
        assert_eq!(back.corpora[0].upstream, "");
        assert_eq!(back.corpora[0].slug, "lz4");
        assert!(catalogue_format_supported(CATALOGUE_FORMAT_VERSION_MIN));
        assert!(catalogue_format_supported(CATALOGUE_FORMAT_VERSION));
        assert!(!catalogue_format_supported(0));
        assert!(!catalogue_format_supported(CATALOGUE_FORMAT_VERSION + 1));
    }

    #[test]
    fn unknown_field_is_refused() {
        // A catalogue entry with a field this version does not know is rejected,
        // not half-read.
        let mut v = serde_json::to_value(sample_entry()).unwrap();
        v["surprise"] = json!(1);
        let r: Result<CatalogueEntry, _> = serde_json::from_value(v);
        assert!(
            r.is_err(),
            "deny_unknown_fields should reject an extra field"
        );
    }

    #[test]
    fn def_record_round_trips_with_and_without_value() {
        let with = DefRecord {
            symbol: "LZ4_VERSION_RELEASE".into(),
            kind: "define".into(),
            lang: "c".into(),
            file: "lib/lz4.h".into(),
            line: 133,
            text: "#define LZ4_VERSION_RELEASE  0".into(),
            value: Some("0".into()),
        };
        let back: DefRecord = serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
        assert_eq!(back, with);
        let without = DefRecord {
            value: None,
            ..with
        };
        let back: DefRecord =
            serde_json::from_str(&serde_json::to_string(&without).unwrap()).unwrap();
        assert_eq!(back.value, None);
    }

    fn answer_with(
        mode: AnswerMode,
        hits: Vec<Hit>,
        from: AnsweredFrom,
        warnings: Vec<String>,
    ) -> Answer {
        Answer {
            corpus: "lz4".into(),
            pin_sha: "0774d05537f9762f838f7ab541b7765f1a729cb5".into(),
            pin_date: "2026-10-02".into(),
            age_days: 1,
            licence_spdx: "BSD-2-Clause".into(),
            licence_class: LicenceClass::AdaptWithAttribution,
            answered_from: from,
            mode,
            hits,
            truncated: false,
            warnings,
        }
    }

    #[test]
    fn render_one_def_hit() {
        let a = answer_with(
            AnswerMode::Def,
            vec![Hit {
                file: "lib/lz4.h".into(),
                line: 133,
                kind: "define".into(),
                text: "#define LZ4_VERSION_RELEASE  0".into(),
                value: Some("0".into()),
                score: 14.5,
                licence_warning: None,
            }],
            AnsweredFrom::LocalPack,
            vec![],
        );
        let expected = "lz4  pin 2026-10-02 (1 day old)  BSD-2-Clause  lib/lz4.h:133\n\
                        #define LZ4_VERSION_RELEASE  0\n\
                        value: 0\n\
                        answered from: local pack";
        assert_eq!(render(&a), expected);
    }

    #[test]
    fn render_three_hits_how_mode_with_warning() {
        let hit = |f: &str, l: u32| Hit {
            file: f.into(),
            line: l,
            kind: "function".into(),
            text: format!("fn at {f}"),
            value: None,
            score: 1.0,
            licence_warning: None,
        };
        let a = answer_with(
            AnswerMode::How,
            vec![hit("a.rs", 1), hit("b.rs", 2), hit("c.rs", 3)],
            AnsweredFrom::Hosted,
            vec!["pin is 23 days old; the question asks for the current source".into()],
        );
        let expected = "lz4  pin 2026-10-02 (1 day old)  BSD-2-Clause  a.rs:1\n\
                        fn at a.rs\n\
                        lz4  pin 2026-10-02 (1 day old)  BSD-2-Clause  b.rs:2\n\
                        fn at b.rs\n\
                        lz4  pin 2026-10-02 (1 day old)  BSD-2-Clause  c.rs:3\n\
                        fn at c.rs\n\
                        answered from: hosted hub\n\
                        warning: pin is 23 days old; the question asks for the current source";
        assert_eq!(render(&a), expected);
    }

    #[test]
    fn render_no_hits() {
        let a = answer_with(AnswerMode::Def, vec![], AnsweredFrom::LocalPack, vec![]);
        let expected = "lz4  pin 2026-10-02 (1 day old)  BSD-2-Clause\n\
                        no match in lz4 (pin 2026-10-02)\n\
                        answered from: local pack";
        assert_eq!(render(&a), expected);
    }

    #[test]
    fn age_days_pluralization() {
        let a = answer_with(AnswerMode::Def, vec![], AnsweredFrom::LocalPack, vec![]);
        assert!(render(&a).contains("(1 day old)"));
        let mut a2 = a;
        a2.age_days = 5;
        assert!(render(&a2).contains("(5 days old)"));
    }
}
