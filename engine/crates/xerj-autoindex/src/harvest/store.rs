//! The content-addressed envelope store — "skip already stored" as a
//! filesystem fact, not a promise.
//!
//! Each normalized record plus its provenance becomes ONE immutable JSON
//! file named `<slug>.<xxh3_128-of-canonical-json>.json`. File presence IS
//! the dedup index: a re-run recomputes the key and skips if the file
//! exists, the same recomputed-never-trusted philosophy as `ids.rs` (same
//! content ⇒ same key ⇒ the write converges). No database, no journal —
//! crash at any point and the next run converges.
//!
//! The canonical form is key-sorted JSON with f64 floats in
//! shortest-round-trip form — deterministic for the same logical content
//! regardless of the input file's key order. (Sorting is explicit: the
//! workspace enables serde_json's `preserve_order` for the engine's
//! document store, which would otherwise bake insertion order in.)

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use xxhash_rust::xxh3::xxh3_128;

/// Cap on flattened fields per envelope, matching the extractor's own cap
/// (`extract::MAX_FIELDS_PER_RECORD`): an envelope fatter than what the
/// index path would keep would silently disagree with the indexed record.
pub(crate) const MAX_FIELDS: usize = 512;

/// Canonical JSON for content hashing: sorted-key serialization. The
/// workspace enables serde_json's `preserve_order` (xerj-common), so `Map`
/// is an IndexMap and `to_string` would bake in INSERTION order — which is
/// exactly what canonical form must not do. Sorting through a `BTreeMap`
/// sidesteps the feature entirely.
pub fn canonical_json(env: &Map<String, Value>) -> String {
    let sorted: std::collections::BTreeMap<&String, &Value> = env.iter().collect();
    serde_json::to_string(&sorted).unwrap_or_default()
}

/// The store key for an envelope: xxh3_128 of the canonical JSON, hex.
/// 128-bit so an accidental collision across a multi-million-record store is
/// not a scenario to reason about.
pub fn key_of(env: &Map<String, Value>) -> String {
    format!("{:032x}", xxh3_128(canonical_json(env).as_bytes()))
}

/// Store one envelope. Returns `true` when it was NEW (written), `false`
/// when an identical envelope was already stored (skipped). The write is
/// atomic (tmp + rename): a partial file never exists under its final name.
pub fn put(store_dir: &Path, slug: &str, env: &Map<String, Value>) -> io::Result<(String, bool)> {
    let key = key_of(env);
    let path = file_for(store_dir, slug, &key);
    if path.exists() {
        return Ok((key, false));
    }
    std::fs::create_dir_all(store_dir)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, canonical_json(env))?;
    std::fs::rename(&tmp, &path)?;
    Ok((key, true))
}

fn file_for(store_dir: &Path, slug: &str, key: &str) -> PathBuf {
    store_dir.join(format!("{slug}.{key}.json"))
}

/// Every envelope whose `_src` is one of `slugs`, in filename order (the
/// deterministic iteration order every downstream stage relies on).
/// Envelopes from sources the current recipe no longer declares are left on
/// disk but not returned — a retired source must not haunt the cluster set.
/// Each returned map carries the file's store key under the `KEY` sentinel
/// so member ordering can fall back to it; `put` computes the key BEFORE
/// injection, so the sentinel is never part of any stored content.
///
/// Any unreadable or non-object `<slug>.<key>.json` for a live slug is a
/// hard error — silently skipping a corrupt store file would omit that
/// record from the emitted pack with a green exit (#1110 gap 4).
pub fn read_all(store_dir: &Path, slugs: &HashSet<String>) -> Result<Vec<Map<String, Value>>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(store_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => {
            return Err(e).with_context(|| format!("cannot read store dir {}", store_dir.display()))
        }
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let e = entry.with_context(|| format!("cannot read entry in {}", store_dir.display()))?;
        if e.path().extension().and_then(|x| x.to_str()) == Some("json") {
            if let Some(name) = e.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    for name in names {
        // "<slug>.<32-hex>.json" — split from the RIGHT: slugs may contain
        // dots themselves, the key never does.
        let Some(rest) = name.strip_suffix(".json") else {
            continue;
        };
        let Some((slug, key)) = rest.rsplit_once('.') else {
            continue;
        };
        if !slugs.contains(slug) {
            continue;
        }
        let path = store_dir.join(&name);
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read store envelope {}", path.display()))?;
        let v: Value = serde_json::from_str(&raw).with_context(|| {
            format!("corrupt store envelope {} (not valid JSON)", path.display())
        })?;
        let Value::Object(mut m) = v else {
            bail!(
                "corrupt store envelope {} (expected JSON object)",
                path.display()
            );
        };
        m.insert(super::identity::KEY.into(), Value::String(key.to_string()));
        out.push(m);
    }
    Ok(out)
}

/// Drop envelopes of `slug` that this run did NOT re-put — a file deleted
/// upstream, or a record the normalizer no longer emits. Returns how many
/// files were removed. Scoped to one slug so a PARTIAL harvest of another
/// source (network watermark, M2) can never prune what it did not walk.
pub fn prune(store_dir: &Path, slug: &str, live_keys: &HashSet<String>) -> usize {
    let Ok(entries) = std::fs::read_dir(store_dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(&format!("{slug}.")) else {
            continue;
        };
        let Some(key) = rest.strip_suffix(".json") else {
            continue;
        };
        if live_keys.contains(key) {
            continue;
        }
        if std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(id: &str, src: &str) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("id".into(), json!(id));
        m.insert("_src".into(), json!(src));
        m
    }

    #[test]
    fn same_content_same_key_regardless_of_key_order() {
        let mut a = Map::new();
        a.insert("id".into(), json!("x"));
        a.insert("zz".into(), json!(1));
        let mut b = Map::new();
        b.insert("zz".into(), json!(1));
        b.insert("id".into(), json!("x"));
        assert_eq!(key_of(&a), key_of(&b));
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn put_is_idempotent_and_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        let (k1, new1) = put(&dir, "a", &env("x", "a")).unwrap();
        let (_, new2) = put(&dir, "a", &env("x", "a")).unwrap();
        assert!(new1);
        assert!(!new2, "second put of identical content skips");
        let all = read_all(&dir, &HashSet::from(["a".to_string()])).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["id"], json!("x"));
        assert_eq!(k1.len(), 32);
    }

    #[test]
    fn different_source_is_a_different_envelope() {
        // identical record text from two sources is two evidence rows, not one
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        put(&dir, "a", &env("x", "a")).unwrap();
        put(&dir, "b", &env("x", "b")).unwrap();
        let all = read_all(&dir, &HashSet::from(["a".to_string(), "b".to_string()])).unwrap();
        assert_eq!(all.len(), 2);
        // retired source stays on disk but is not returned
        let only_a = read_all(&dir, &HashSet::from(["a".to_string()])).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0]["_src"], json!("a"));
    }

    #[test]
    fn prune_drops_only_dead_keys_of_its_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        let (ka, _) = put(&dir, "a", &env("x", "a")).unwrap();
        let (kb, _) = put(&dir, "a", &env("y", "a")).unwrap();
        let (_kc, _) = put(&dir, "b", &env("z", "b")).unwrap();
        let removed = prune(&dir, "a", &HashSet::from([ka.clone(), kb]));
        assert_eq!(removed, 0);
        let removed = prune(&dir, "a", &HashSet::from([ka]));
        assert_eq!(removed, 1, "y died, x lived, b untouched");
        assert_eq!(
            read_all(&dir, &HashSet::from(["a".into(), "b".into()]))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn read_all_refuses_corrupt_store_envelope_instead_of_dropping_it() {
        // #1110 gap 4: a truncated or non-object file in store/ for a live
        // slug must fail the build loudly, never silently omit the record.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        let (k, _) = put(&dir, "a", &env("x", "a")).unwrap();
        let path = dir.join(format!("a.{k}.json"));

        std::fs::write(&path, "{truncated").unwrap();
        let err = read_all(&dir, &HashSet::from(["a".to_string()]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("corrupt store envelope"), "{err}");

        std::fs::write(&path, "[1, 2, 3]").unwrap();
        let err = read_all(&dir, &HashSet::from(["a".to_string()]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected JSON object"), "{err}");

        // a corrupt file belonging to a RETIRED slug is ignored, matching the
        // retired-source contract above.
        assert!(read_all(&dir, &HashSet::from(["other".to_string()]))
            .unwrap()
            .is_empty());
    }
}
