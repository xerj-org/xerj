//! The rust-vulns pack's public numbers must be numbers a real build produced.
//!
//! Same discipline as `security_tooling_claims.rs` (issue #207's lesson): a
//! README that advertises record counts nobody measured is a worse failure
//! than admitting the gap. `tools/packs/rust-vulns/pack-stats.json` is the
//! committed record of a measured build (`provenance: "measured"`); these
//! tests fail when the README, the stats and the recipe disagree — so a
//! rebuild with different numbers forces the docs to move with them, and a
//! recipe edit without a rebuild fails the sha pin.

use std::path::{Path, PathBuf};

/// Walk up from this crate to the repository root. Panics rather than
/// skipping: a claims check that turns into a no-op when it cannot find its
/// inputs is the tautological pass this file exists to prevent.
fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join(".github/workflows/ci.yml").is_file() {
            return dir;
        }
        assert!(
            dir.pop(),
            "no .github/workflows/ci.yml above {} — this test must run from a \
             repository checkout, and must not pass by failing to look",
            env!("CARGO_MANIFEST_DIR"),
        );
    }
}

fn pack_dir_named(root: &Path, name: &str) -> PathBuf {
    root.join("tools/packs").join(name)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn stats_named(root: &Path, name: &str) -> serde_json::Value {
    let dir = pack_dir_named(root, name);
    let text = read(&dir.join("pack-stats.json"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("pack-stats.json is not valid JSON: {e}"))
}

/// The README's table row for `label` must carry exactly the number in
/// pack-stats.json under `key`.
fn readme_row_pins_named(root: &Path, name: &str, label: &str, key: &str) {
    let readme = read(&pack_dir_named(root, name).join("README.md"));
    let want = stats_named(root, name)
        .get(key)
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| panic!("{name}/pack-stats.json has no numeric '{key}'"));
    // rows look like `| … label … | 1950 |` or `| … label … | 270 (707 …) |`,
    // so pin on maximal digit runs, not whole cells
    let needle = label.to_string();
    let row = readme
        .lines()
        .find(|l| l.contains(&needle))
        .unwrap_or_else(|| panic!("{name}/README has no row containing '{needle}'"));
    let numbers: Vec<String> = row
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    assert!(
        numbers.contains(&want.to_string()),
        "{name}/README row '{needle}' does not carry {want} (pack-stats.{key}); rebuild \
         the pack and move the docs with the numbers",
    );
}

#[test]
fn pack_stats_are_measured_not_asserted() {
    let root = repo_root();
    let s = stats_named(&root, "rust-vulns");
    assert_eq!(
        s["provenance"], "measured",
        "pack-stats.json must record provenance 'measured' — 'estimated' numbers \
         are exactly what this file exists to keep out of the README"
    );
    for key in [
        "envelopes",
        "records",
        "merged_from_both_sources",
        "records_with_functions",
        "function_paths_distinct",
        "function_entries_total",
        "records_withdrawn",
        "packages",
    ] {
        assert!(
            s.get(key).and_then(|v| v.as_u64()).is_some_and(|v| v > 0),
            "rust-vulns/pack-stats.json lacks a positive numeric '{key}'"
        );
    }
    // internal consistency: the parts cannot exceed the whole
    let records = s["records"].as_u64().unwrap();
    let merged = s["merged_from_both_sources"].as_u64().unwrap();
    let with_fn = s["records_with_functions"].as_u64().unwrap();
    assert!(merged <= records, "more merged records than records");
    assert!(with_fn <= records, "more function records than records");

    let kev = stats_named(&root, "cisa-kev");
    assert_eq!(kev["provenance"], "measured");
    for key in [
        "envelopes",
        "records",
        "vendors",
        "products",
        "records_with_cwe",
        "cwe_ids_distinct",
        "records_ransomware_known",
        "records_bytes",
    ] {
        assert!(
            kev.get(key).and_then(|v| v.as_u64()).is_some_and(|v| v > 0),
            "cisa-kev/pack-stats.json lacks a positive numeric '{key}'"
        );
    }
    let kev_records = kev["records"].as_u64().unwrap();
    assert!(kev["records_with_cwe"].as_u64().unwrap() <= kev_records);
    assert!(kev["records_ransomware_known"].as_u64().unwrap() <= kev_records);
}

#[test]
fn readme_counts_match_the_measured_stats() {
    let root = repo_root();
    readme_row_pins_named(&root, "rust-vulns", "Source records harvested", "envelopes");
    readme_row_pins_named(
        &root,
        "rust-vulns",
        "Records after identity resolution",
        "records",
    );
    readme_row_pins_named(
        &root,
        "rust-vulns",
        "…merged from both sources",
        "merged_from_both_sources",
    );
    readme_row_pins_named(
        &root,
        "rust-vulns",
        "Records with affected-function paths",
        "records_with_functions",
    );
    readme_row_pins_named(
        &root,
        "rust-vulns",
        "Records with affected-function paths",
        "function_paths_distinct",
    );
    readme_row_pins_named(
        &root,
        "rust-vulns",
        "Records withdrawn",
        "records_withdrawn",
    );
    readme_row_pins_named(&root, "rust-vulns", "Packages covered", "packages");

    readme_row_pins_named(&root, "cisa-kev", "Source records harvested", "envelopes");
    readme_row_pins_named(
        &root,
        "cisa-kev",
        "Records after identity resolution",
        "records",
    );
    readme_row_pins_named(&root, "cisa-kev", "Vendors covered", "vendors");
    readme_row_pins_named(&root, "cisa-kev", "Products covered", "products");
    readme_row_pins_named(
        &root,
        "cisa-kev",
        "Records with CWE classification",
        "records_with_cwe",
    );
    readme_row_pins_named(
        &root,
        "cisa-kev",
        "Records with CWE classification",
        "cwe_ids_distinct",
    );
    readme_row_pins_named(
        &root,
        "cisa-kev",
        "Records flagged known ransomware use",
        "records_ransomware_known",
    );
    readme_row_pins_named(&root, "cisa-kev", "Records JSONL bytes", "records_bytes");
}

#[test]
fn the_recipe_is_the_one_the_stats_measured() {
    let root = repo_root();
    for (pack, slugs) in [
        ("rust-vulns", &["rustsec", "osv"][..]),
        ("cisa-kev", &["kev"][..]),
    ] {
        let dir = pack_dir_named(&root, pack);
        let recipe = read(&dir.join("recipe.toml"));
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        hasher.update(recipe.as_bytes());
        let got = format!("{:x}", hasher.finalize());
        let want = stats_named(&root, pack)["recipe_sha256"]
            .as_str()
            .expect("pack-stats.json pins recipe_sha256")
            .to_string();
        assert_eq!(
            got, want,
            "{pack}/recipe.toml changed since pack-stats.json was measured — rebuild the \
             pack and refresh pack-stats.json (and the README numbers with it)"
        );

        for slug in slugs {
            assert!(
                recipe.contains(&format!("slug = \"{slug}\"")),
                "{pack}/recipe no longer declares source '{slug}'"
            );
            assert!(
                stats_named(&root, pack)["sources"][slug].is_object(),
                "{pack}/pack-stats.json no longer records source '{slug}'"
            );
        }
        assert!(
            !recipe.contains("CC-BY") && !recipe.contains("BUSL") && !recipe.contains("GPL"),
            "{pack} README says all record data is CC0; a new source with a \
             different licence must move that claim too"
        );
    }
}

#[test]
#[ignore = "fetches live CISA KEV feed over HTTPS to measure/refresh tools/packs/cisa-kev/pack-stats.json"]
fn measure_cisa_kev_pack_live() {
    use std::collections::BTreeSet;
    let root = repo_root();
    let recipe_path = root.join("tools/packs/cisa-kev/recipe.toml");
    let recipe_text = read(&recipe_path);
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(recipe_text.as_bytes());
    let recipe_sha256 = format!("{:x}", hasher.finalize());

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("XERJ_CODE_HOME", home.path());
    let t0 = std::time::Instant::now();
    let code = xerj_autoindex::harvest::run_build(&[
        "cisa-kev".to_string(),
        "--recipe".to_string(),
        recipe_path.to_string_lossy().to_string(),
    ]);
    let elapsed = t0.elapsed().as_secs().max(1);
    assert_eq!(code, 0, "cisa-kev build failed");

    let pack = home.path().join("builds/cisa-kev/pack/cisa-kev");
    let manifest: serde_json::Value =
        serde_json::from_str(&read(&pack.join("manifest.json"))).unwrap();
    let envelopes = manifest["counts"]["envelopes"].as_u64().unwrap();
    let records_count = manifest["counts"]["records"].as_u64().unwrap();

    let mut vendors = BTreeSet::new();
    let mut products = BTreeSet::new();
    let mut cwes_distinct = BTreeSet::new();
    let mut records_with_cwe = 0u64;
    let mut records_ransomware_known = 0u64;
    let mut records_bytes = 0u64;

    for name in manifest["files"].as_object().unwrap().keys() {
        let p = pack.join(name);
        records_bytes += std::fs::metadata(&p).unwrap().len();
        for line in read(&p).lines() {
            if line.trim().is_empty() {
                continue;
            }
            let rec: serde_json::Value = serde_json::from_str(line).unwrap();
            if let Some(v) = rec["vendorProject"].as_str().filter(|s| !s.is_empty()) {
                vendors.insert(v.to_string());
            }
            if let Some(p) = rec["product"].as_str().filter(|s| !s.is_empty()) {
                products.insert(p.to_string());
            }
            if let Some(arr) = rec["cwe_ids"].as_array() {
                if !arr.is_empty() {
                    records_with_cwe += 1;
                    for c in arr {
                        if let Some(s) = c.as_str() {
                            cwes_distinct.insert(s.to_string());
                        }
                    }
                }
            }
            if rec["ransomware_status"] == serde_json::json!("known") {
                records_ransomware_known += 1;
            }
        }
    }

    let stats_json = serde_json::json!({
        "provenance": "measured",
        "measured_at": "2026-10-15",
        "pack": "cisa-kev",
        "recipe_sha256": recipe_sha256,
        "sources": {
            "kev": { "kind": "http-json", "envelopes": envelopes, "licence": "CC0-1.0" }
        },
        "envelopes": envelopes,
        "records": records_count,
        "vendors": vendors.len(),
        "products": products.len(),
        "records_with_cwe": records_with_cwe,
        "cwe_ids_distinct": cwes_distinct.len(),
        "records_ransomware_known": records_ransomware_known,
        "records_bytes": records_bytes,
        "build_seconds": elapsed,
    });
    println!(
        "MEASURED_CISA_KEV_STATS:\n{}",
        serde_json::to_string_pretty(&stats_json).unwrap()
    );
}
