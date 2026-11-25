//! Hub packs as the corpora of a pack recipe (#2762).
//!
//! [`HubPackDir`] is a [`RecipeCorpusSource`] over a directory of signed
//! `<slug>-<pin8>.hubpack.tar.zst` archives, each with its detached
//! `<archive>.sig` (what `xerj hub build-pack --out` writes and the catalogue
//! lists). A recipe's `[corpora]` names a corpus by its slug; the source finds
//! that corpus's archive, verifies it, and hands the recipe runner the records
//! the pack carries:
//!
//! * each `defs.jsonl` line (a definition, or a record of a record pack) is one
//!   hit: `record_id` `<file>:<line>:<symbol>`, `title` the symbol, `excerpt`
//!   the declaration text, and `fields` `kind`, `lang`, `file`, `line`, `value`
//!   (when the pack has one) and `source_member = "defs.jsonl"`, so a recipe's
//!   `[[filters]]` can select on them;
//! * each `files.jsonl` line (a source file with its per-path licence) is one
//!   hit: `record_id` and `title` the path, `fields` `kind = "file"`, `lang`,
//!   `bytes`, `licence`, `file` and `source_member = "files.jsonl"`.
//!
//! Every hit carries the pack's `pin_sha`, `pin_date` (as `published_date`),
//! `built_at` (as `fetched_at`) and licence class, so the runner's licence gate
//! and citation check work on it unchanged.
//!
//! **Verified, not trusted.** The archive is read through
//! [`read_verified_pack_records`]: the ed25519 signature over the archive bytes,
//! the decompression bound, `format_version` and every `SHA256SUMS` line must
//! hold, or the corpus is unreadable (and a required one refuses the call). An
//! archive without its `.sig`, a build with no hub key, and an archive whose
//! name and contents disagree about the slug or the pin are refused the same
//! way. Nothing is extracted.
//!
//! **Retrieval, not a dump.** A pack can hold hundreds of thousands of
//! definitions, so the source ranks them by the call's query
//! ([`xerj_common::pack_recipe::PackRecipe::corpus_query`]): the score of a hit
//! is the share of the query's words (letters and digits, lower-cased, two or
//! more characters) that appear in its fields, hits that share none are left
//! out, and the best `max_hits` come back, best first (score, then `record_id`).
//! A query with no words matches every record, scored `0.0`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use xerj_common::hub::{hub_pubkey, DefRecord, NO_HUB_KEY_MESSAGE};
use xerj_common::pack_recipe::{RecipeCorpusHit, RecipeCorpusSource, RecipeError};

use crate::pack::{read_verified_pack_records, sig_path_for, PackFileEntry, PackRecords};

/// File-name suffix of a pack archive (`<slug>-<pin8>.hubpack.tar.zst`).
const ARCHIVE_SUFFIX: &str = ".hubpack.tar.zst";

/// A directory of signed hub pack archives, as a recipe corpus source.
#[derive(Debug, Clone)]
pub struct HubPackDir {
    dir: PathBuf,
    pubkey: Option<String>,
}

impl HubPackDir {
    /// Packs in `dir`, verified against the hub key of this build
    /// ([`xerj_common::hub::hub_pubkey`], read at each call so the test override
    /// `XERJ_HUB_PUBKEY` applies).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            pubkey: None,
        }
    }

    /// Verify against `pubkey_hex` (an ed25519 public key, 64 hex digits)
    /// instead of the hub key of this build.
    pub fn with_pubkey(mut self, pubkey_hex: impl Into<String>) -> Self {
        self.pubkey = Some(pubkey_hex.into());
        self
    }

    /// The one archive for `corpus`: `Ok(None)` when there is none, an error
    /// when the directory cannot be listed or holds more than one (two pins of
    /// a corpus is not a choice the source makes silently).
    fn find_archive(&self, corpus: &str) -> Result<Option<PathBuf>, String> {
        let listing = match fs::read_dir(&self.dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", self.dir.display())),
        };
        let mut found: Vec<PathBuf> = Vec::new();
        for entry in listing {
            let entry = entry.map_err(|e| format!("{}: {e}", self.dir.display()))?;
            let name = entry.file_name();
            if name.to_str().and_then(archive_slug) == Some(corpus) && entry.path().is_file() {
                found.push(entry.path());
            }
        }
        found.sort();
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.pop()),
            n => Err(format!(
                "{} holds {n} packs for it ({}); keep one",
                self.dir.display(),
                found
                    .iter()
                    .filter_map(|p| p.file_name()?.to_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// `<slug>-<pin8>.hubpack.tar.zst` is the archive of `<slug>`; anything else
/// (the `.sig`, a `.tmp`, a name without an 8-hex pin) is not.
fn archive_slug(file_name: &str) -> Option<&str> {
    let (slug, pin8) = file_name.strip_suffix(ARCHIVE_SUFFIX)?.rsplit_once('-')?;
    (pin8.len() == 8 && pin8.bytes().all(|b| b.is_ascii_hexdigit())).then_some(slug)
}

impl RecipeCorpusSource for HubPackDir {
    fn fetch_corpus_hits(
        &self,
        corpus: &str,
        query: &str,
        max_hits: usize,
    ) -> Result<Vec<RecipeCorpusHit>, RecipeError> {
        let unreadable = |reason: String| RecipeError::CorpusUnreadable {
            corpus: corpus.to_string(),
            reason,
        };
        let Some(archive) = self.find_archive(corpus).map_err(unreadable)? else {
            return Err(RecipeError::CorpusNotInstalled {
                corpus: corpus.to_string(),
            });
        };
        let shown = archive.display();

        let Some(pubkey) = self.pubkey.clone().or_else(hub_pubkey) else {
            return Err(unreadable(format!(
                "{shown} cannot be verified. {NO_HUB_KEY_MESSAGE}"
            )));
        };
        let sig = sig_path_for(&archive);
        if !sig.is_file() {
            return Err(unreadable(format!(
                "{shown} has no detached signature {}; an unsigned pack is not read",
                sig.display()
            )));
        }
        let records = read_verified_pack_records(&archive, &sig, &pubkey)
            .map_err(|e| unreadable(format!("{shown}: {e:#}")))?;

        // The signature covers the archive's bytes, not its name: a pack renamed
        // to look like another corpus, or another pin, is not that corpus.
        let manifest = &records.manifest;
        if manifest.slug != corpus {
            return Err(unreadable(format!(
                "{shown} is the pack of `{}`, not of `{corpus}`",
                manifest.slug
            )));
        }
        let named_pin = archive
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(ARCHIVE_SUFFIX))
            .and_then(|stem| stem.rsplit_once('-'))
            .map(|(_, pin8)| pin8.to_ascii_lowercase());
        if named_pin.as_deref()
            != manifest
                .pin_sha
                .get(..8)
                .map(str::to_ascii_lowercase)
                .as_deref()
        {
            return Err(unreadable(format!(
                "{shown} is named for a pin its pack.json does not carry ({})",
                manifest.pin_sha
            )));
        }
        Ok(rank_records(&records, query, max_hits))
    }
}

/// The words of `text`: runs of letters and digits, lower-cased, of at least two
/// characters.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_lowercase)
}

/// The records of `pack` as recipe hits, best match for `query` first.
fn rank_records(pack: &PackRecords, query: &str, max_hits: usize) -> Vec<RecipeCorpusHit> {
    let wanted: HashSet<String> = words(query).collect();
    let score_of = |fields: &[&str]| -> Option<f64> {
        if wanted.is_empty() {
            return Some(0.0);
        }
        let haystack = fields.join(" ").to_lowercase();
        let present: HashSet<&str> = haystack
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        let shared = wanted
            .iter()
            .filter(|w| present.contains(w.as_str()))
            .count();
        (shared > 0).then(|| shared as f64 / wanted.len() as f64)
    };

    let mut hits: Vec<RecipeCorpusHit> = Vec::new();
    for def in &pack.defs {
        let value = def.value.as_deref().unwrap_or("");
        if let Some(score) = score_of(&[&def.symbol, &def.text, &def.file, &def.kind, value]) {
            hits.push(def_hit(pack, def, score));
        }
    }
    for file in &pack.files {
        if let Some(score) = score_of(&[&file.path, &file.lang, &file.licence]) {
            hits.push(file_hit(pack, file, score));
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.record_id.cmp(&b.record_id))
    });
    hits.truncate(max_hits);
    hits
}

fn pack_hit(
    pack: &PackRecords,
    record_id: String,
    title: String,
    excerpt: String,
) -> RecipeCorpusHit {
    let manifest = &pack.manifest;
    RecipeCorpusHit {
        corpus: manifest.slug.clone(),
        licence_class: manifest.licence_class,
        record_id,
        title,
        excerpt,
        score: 0.0,
        published_date: manifest.pin_date.clone(),
        study_grade_rank: None,
        pin_sha: manifest.pin_sha.clone(),
        fetched_at: manifest.built_at.clone(),
        fields: BTreeMap::new(),
        pinned_citation: None,
    }
}

fn def_hit(pack: &PackRecords, def: &DefRecord, score: f64) -> RecipeCorpusHit {
    let mut hit = pack_hit(
        pack,
        format!("{}:{}:{}", def.file, def.line, def.symbol),
        def.symbol.clone(),
        def.text.clone(),
    );
    hit.score = score;
    hit.fields = BTreeMap::from([
        ("kind".to_string(), def.kind.clone()),
        ("lang".to_string(), def.lang.clone()),
        ("file".to_string(), def.file.clone()),
        ("line".to_string(), def.line.to_string()),
        ("source_member".to_string(), "defs.jsonl".to_string()),
    ]);
    if let Some(value) = &def.value {
        hit.fields.insert("value".to_string(), value.clone());
    }
    hit
}

fn file_hit(pack: &PackRecords, file: &PackFileEntry, score: f64) -> RecipeCorpusHit {
    let language = if file.lang.is_empty() {
        String::new()
    } else {
        format!("{}, ", file.lang)
    };
    let mut hit = pack_hit(
        pack,
        file.path.clone(),
        file.path.clone(),
        format!(
            "{} ({language}{} bytes, licence {})",
            file.path, file.bytes, file.licence
        ),
    );
    hit.score = score;
    hit.fields = BTreeMap::from([
        ("kind".to_string(), "file".to_string()),
        ("lang".to_string(), file.lang.clone()),
        ("file".to_string(), file.path.clone()),
        ("bytes".to_string(), file.bytes.to_string()),
        ("licence".to_string(), file.licence.clone()),
        ("source_member".to_string(), "files.jsonl".to_string()),
    ]);
    hit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::write_pack;
    use std::path::Path;
    use xerj_autoindex::harvest::sign::generate_keypair;
    use xerj_common::hub::{LicenceClass, PackIndex, PackManifest, PACK_FORMAT_VERSION};

    const PIN: &str = "0774d05537f9762f838f7ab541b7765f1a729cb5";

    fn manifest(slug: &str, class: LicenceClass) -> PackManifest {
        PackManifest {
            format_version: PACK_FORMAT_VERSION,
            slug: slug.into(),
            pin_sha: PIN.into(),
            pin_date: "2026-10-02".into(),
            built_at: "2026-10-14T00:00:00Z".into(),
            builder_version: "xerj test".into(),
            index_prefix: format!("xc-{slug}-b1"),
            indices: vec![PackIndex {
                name: format!("xc-{slug}-b1-docs"),
                docs: 1,
                bytes: 1,
            }],
            snapshot_name: format!("{slug}-0774d055"),
            licence_spdx: "BSD-2-Clause".into(),
            licence_class: class,
            sources: Vec::new(),
            defs_count: 3,
            files_count: 2,
        }
    }

    fn def(symbol: &str, kind: &str, file: &str, line: u32, text: &str) -> DefRecord {
        DefRecord {
            symbol: symbol.into(),
            kind: kind.into(),
            lang: "c".into(),
            file: file.into(),
            line,
            text: text.into(),
            value: None,
        }
    }

    /// Write `<dir>/<slug>-0774d055.hubpack.tar.zst` (+ `.sig`) and return the
    /// public key it verifies under.
    fn write_test_pack(dir: &Path, slug: &str, class: LicenceClass) -> String {
        let (seed, public) = generate_keypair().unwrap();
        let snapshot = dir.join(format!("{slug}-snapshot"));
        fs::create_dir_all(&snapshot).unwrap();
        fs::write(snapshot.join("manifest.json"), b"{}").unwrap();
        let mut version = def(
            "LZ4_VERSION_RELEASE",
            "define",
            "lib/lz4.h",
            133,
            "#define LZ4_VERSION_RELEASE 0",
        );
        version.value = Some("0".into());
        let defs = vec![
            def(
                "LZ4_compress_default",
                "function",
                "lib/lz4.c",
                700,
                "int LZ4_compress_default(const char* src, char* dst, int n, int cap)",
            ),
            version,
            def(
                "XXH64_digest",
                "function",
                "lib/xxhash.c",
                12,
                "XXH64_hash_t XXH64_digest(const XXH64_state_t* state)",
            ),
        ];
        let files = vec![
            PackFileEntry {
                path: "lib/lz4.c".into(),
                lang: "c".into(),
                bytes: 100_000,
                licence: "BSD-2-Clause".into(),
            },
            PackFileEntry {
                path: "README.md".into(),
                lang: String::new(),
                bytes: 900,
                licence: "BSD-2-Clause".into(),
            },
        ];
        write_pack(
            &dir.join(format!("{slug}-0774d055{ARCHIVE_SUFFIX}")),
            &manifest(slug, class),
            &defs,
            &files,
            &snapshot,
            &seed,
        )
        .unwrap();
        public
    }

    fn fetch(
        source: &HubPackDir,
        corpus: &str,
        query: &str,
    ) -> Result<Vec<RecipeCorpusHit>, RecipeError> {
        source.fetch_corpus_hits(corpus, query, 100)
    }

    #[test]
    fn a_verified_pack_is_ranked_by_the_query_and_carries_the_pin_and_licence() {
        let dir = tempfile::tempdir().unwrap();
        let public = write_test_pack(dir.path(), "lz4", LicenceClass::AdaptWithAttribution);
        let source = HubPackDir::new(dir.path()).with_pubkey(public);

        let hits = fetch(&source, "lz4", "how does LZ4 compress").unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.record_id.as_str()).collect();
        // `lz4` + `compress` is both words for the compress function, one for the
        // version define and the lz4.c file entry; nothing else shares a word.
        assert_eq!(
            ids,
            [
                "lib/lz4.c:700:LZ4_compress_default",
                "lib/lz4.c",
                "lib/lz4.h:133:LZ4_VERSION_RELEASE",
            ]
        );
        assert!(hits[0].score > hits[1].score, "{hits:?}");
        let top = &hits[0];
        assert_eq!(top.corpus, "lz4");
        assert_eq!(top.title, "LZ4_compress_default");
        assert_eq!(top.pin_sha, PIN);
        assert_eq!(top.published_date, "2026-10-02");
        assert_eq!(top.fetched_at, "2026-10-14T00:00:00Z");
        assert_eq!(top.licence_class, LicenceClass::AdaptWithAttribution);
        assert_eq!(top.fields["kind"], "function");
        assert_eq!(top.fields["file"], "lib/lz4.c");
        assert_eq!(top.fields["line"], "700");
        assert_eq!(top.fields["source_member"], "defs.jsonl");
        let file = hits.iter().find(|h| h.record_id == "lib/lz4.c").unwrap();
        assert_eq!(file.fields["kind"], "file");
        assert_eq!(file.fields["source_member"], "files.jsonl");
        assert!(file.excerpt.contains("100000 bytes"), "{}", file.excerpt);
        let define = hits
            .iter()
            .find(|h| h.title == "LZ4_VERSION_RELEASE")
            .unwrap();
        assert_eq!(define.fields["value"], "0");
    }

    #[test]
    fn the_cap_keeps_the_best_hits_and_an_empty_query_matches_every_record() {
        let dir = tempfile::tempdir().unwrap();
        let public = write_test_pack(dir.path(), "lz4", LicenceClass::AdaptWithAttribution);
        let source = HubPackDir::new(dir.path()).with_pubkey(public);

        let capped = source.fetch_corpus_hits("lz4", "lz4 compress", 1).unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].title, "LZ4_compress_default");

        let all = fetch(&source, "lz4", "").unwrap();
        assert_eq!(all.len(), 5, "3 definitions and 2 files");
        assert!(all.iter().all(|h| h.score == 0.0));
        assert_eq!(all[0].record_id, "README.md", "ties fall to record_id");
    }

    #[test]
    fn a_pack_that_is_absent_is_not_installed_and_one_that_is_broken_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let public = write_test_pack(dir.path(), "lz4", LicenceClass::AdaptWithAttribution);
        let source = HubPackDir::new(dir.path()).with_pubkey(public.clone());
        let archive = dir.path().join(format!("lz4-0774d055{ARCHIVE_SUFFIX}"));

        assert!(matches!(
            fetch(&source, "zstd", "x"),
            Err(RecipeError::CorpusNotInstalled { corpus }) if corpus == "zstd"
        ));
        assert!(matches!(
            fetch(&HubPackDir::new(dir.path().join("absent")), "lz4", "x"),
            Err(RecipeError::CorpusNotInstalled { .. })
        ));

        let unreadable = |source: &HubPackDir| match fetch(source, "lz4", "lz4") {
            Err(RecipeError::CorpusUnreadable { corpus, reason }) => {
                assert_eq!(corpus, "lz4");
                reason
            }
            other => panic!("expected CorpusUnreadable, got {other:?}"),
        };

        // Another key: the signature does not verify.
        let (_, other_key) = generate_keypair().unwrap();
        let why = unreadable(&HubPackDir::new(dir.path()).with_pubkey(other_key));
        assert!(why.contains("signature"), "{why}");

        // One flipped byte after signing.
        let sig = sig_path_for(&archive);
        let good = fs::read(&archive).unwrap();
        let mut bad = good.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0xff;
        fs::write(&archive, &bad).unwrap();
        assert!(unreadable(&source).contains("signature"));
        fs::write(&archive, &good).unwrap();
        assert!(fetch(&source, "lz4", "lz4").is_ok());

        // No signature file: not read at all.
        let sig_bytes = fs::read(&sig).unwrap();
        fs::remove_file(&sig).unwrap();
        let why = unreadable(&source);
        assert!(why.contains("no detached signature"), "{why}");
        fs::write(&sig, sig_bytes).unwrap();
        assert!(fetch(&source, "lz4", "lz4").is_ok());

        // Two pins of one corpus: the source does not choose.
        let second = dir.path().join(format!("lz4-1111aaaa{ARCHIVE_SUFFIX}"));
        fs::copy(&archive, &second).unwrap();
        let why = unreadable(&source);
        assert!(
            why.contains("holds 2 packs") && why.contains("keep one"),
            "{why}"
        );
    }

    #[test]
    fn an_archive_named_for_another_corpus_or_pin_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let public = write_test_pack(dir.path(), "lz4", LicenceClass::AdaptWithAttribution);
        let source = HubPackDir::new(dir.path()).with_pubkey(public);
        let archive = dir.path().join(format!("lz4-0774d055{ARCHIVE_SUFFIX}"));
        let sig = sig_path_for(&archive);

        // The lz4 pack under the name of zstd, signature copied alongside.
        let renamed = dir.path().join(format!("zstd-0774d055{ARCHIVE_SUFFIX}"));
        fs::copy(&archive, &renamed).unwrap();
        fs::copy(&sig, sig_path_for(&renamed)).unwrap();
        let why = match fetch(&source, "zstd", "lz4") {
            Err(RecipeError::CorpusUnreadable { reason, .. }) => reason,
            other => panic!("{other:?}"),
        };
        assert!(why.contains("pack of `lz4`, not of `zstd`"), "{why}");

        // The same pack under another pin.
        let repinned = dir.path().join(format!("lz4-ffffffff{ARCHIVE_SUFFIX}"));
        fs::rename(&archive, &repinned).unwrap();
        fs::rename(&sig, sig_path_for(&repinned)).unwrap();
        let why = match fetch(&source, "lz4", "lz4") {
            Err(RecipeError::CorpusUnreadable { reason, .. }) => reason,
            other => panic!("{other:?}"),
        };
        assert!(why.contains("does not carry"), "{why}");
    }

    #[test]
    fn only_a_slug_and_an_eight_hex_pin_name_an_archive() {
        assert_eq!(archive_slug("lz4-0774d055.hubpack.tar.zst"), Some("lz4"));
        assert_eq!(
            archive_slug("rust-vulns-DEADBEEF.hubpack.tar.zst"),
            Some("rust-vulns")
        );
        for name in [
            "lz4-0774d055.hubpack.tar.zst.sig",
            "lz4-0774d05.hubpack.tar.zst",
            "lz4-0774d05g.hubpack.tar.zst",
            "lz4.hubpack.tar.zst",
            "lz4-0774d055.tar.zst",
        ] {
            assert_eq!(archive_slug(name), None, "{name}");
        }
    }
}
