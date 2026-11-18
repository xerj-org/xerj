//! Regression for zero-config routing against a catalogue the shape of the live
//! hub (468 corpora on 2026-10-21, `https://hub.xerj.org/api/v1/catalogue.json`).
//!
//! The fixture keeps each corpus's slug, topics, description and licence class.
//! Sketches are rebuilt here so that every corpus reports every token of the
//! two Swift questions: that is the saturated-Bloom failure, where `qwik` and
//! `docker-docs` tie with `swift-book`. Routing has to ignore that and follow
//! the name.

use std::fs;
use std::path::Path;

use xerj_hub::router::{route, Routing};
use xerj_hub::{Catalogue, CatalogueEntry, LicenceClass};

/// Tokens both failing questions tokenize to. Inserted into every sketch.
const POISON: &[&str] = &[
    "how", "use", "async", "let", "swift", "Swift", "result", "Result", "type", "map",
];

fn licence(class: &str) -> LicenceClass {
    match class {
        "approach-only" => LicenceClass::ApproachOnly,
        "excluded" => LicenceClass::Excluded,
        _ => LicenceClass::AdaptWithAttribution,
    }
}

fn catalogue() -> Catalogue {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hub-catalogue-identity.jsonl");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let corpora = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).expect("fixture row");
            let ask_for = row["ask_for"].as_str().unwrap_or("").to_string();
            let mut terms: Vec<String> = ask_for
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| w.len() >= 3)
                .map(|w| w.to_ascii_lowercase())
                .collect();
            terms.extend(POISON.iter().map(|s| (*s).to_string()));
            let topics = row["topics"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            CatalogueEntry {
                slug: row["slug"].as_str().unwrap_or("").to_string(),
                domain: row["domain"].as_str().unwrap_or("").to_string(),
                topics,
                ask_for,
                upstream: String::new(),
                pin_sha: "0".repeat(40),
                pin_date: "2026-10-21".to_string(),
                refresh_cadence_days: 7,
                licence_spdx: "Apache-2.0".to_string(),
                licence_class: licence(row["licence_class"].as_str().unwrap_or("")),
                pack_bytes: 0,
                pack_url: String::new(),
                pack_sha256: String::new(),
                sig_url: String::new(),
                dependency_names: Vec::new(),
                quality_grade: None,
                term_sketch: xerj_hub::sketch::build(terms),
            }
        })
        .collect();
    Catalogue {
        format_version: 1,
        generated_at: "2026-10-21T16:40:13Z".to_string(),
        corpora,
    }
}

fn picked(routing: &Routing) -> Vec<String> {
    match routing {
        Routing::Confident(slug) => vec![slug.clone()],
        Routing::Group(slugs) | Routing::Ambiguous(slugs) => slugs.clone(),
        Routing::NoCorpus | Routing::NoCoverage => Vec::new(),
    }
}

fn route_q(cat: &Catalogue, q: &str) -> Routing {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    route(q, dir.path(), cat, None, &[])
}

#[test]
fn swift_questions_route_to_the_swift_corpora_on_the_live_catalogue_shape() {
    let cat = catalogue();
    assert!(
        cat.corpora.len() >= 400,
        "fixture should mirror the live hub, got {} corpora",
        cat.corpora.len()
    );

    let async_let = route_q(&cat, "how do I use async let in Swift");
    assert_eq!(
        picked(&async_let),
        vec!["swift-book".to_string(), "swift-evolution".to_string()],
        "async let: {async_let:?}"
    );
    assert!(
        matches!(async_let, Routing::Group(_)),
        "a tie between the two Swift corpora is a group, not exit 4: {async_let:?}"
    );

    let result_map = route_q(&cat, "Swift Result type map");
    assert_eq!(
        picked(&result_map),
        vec!["swift-evolution".to_string(), "swift-book".to_string()],
        "Result map: {result_map:?}"
    );
    for unrelated in ["qwik", "docker-docs", "harper", "oss-review-toolkit"] {
        assert!(
            !picked(&async_let).iter().any(|s| s == unrelated),
            "{unrelated} tied with swift-book"
        );
        assert!(
            !picked(&result_map).iter().any(|s| s == unrelated),
            "{unrelated} tied on the Result query"
        );
    }
}

#[test]
fn a_broad_language_with_many_corpora_stays_ambiguous_and_short() {
    let cat = catalogue();
    let routing = route_q(&cat, "python");
    match routing {
        Routing::Ambiguous(slugs) => {
            assert!(
                (2..=5).contains(&slugs.len()),
                "a few ranked candidates, not the whole catalogue: {slugs:?}"
            );
            for slug in &slugs {
                let entry = cat
                    .corpora
                    .iter()
                    .find(|e| &e.slug == slug)
                    .unwrap_or_else(|| panic!("missing {slug}"));
                let blob = format!("{} {}", entry.slug, entry.ask_for).to_ascii_lowercase();
                assert!(
                    blob.split(|c: char| !c.is_ascii_alphanumeric())
                        .any(|w| w == "python"),
                    "{slug} is not a Python corpus: {}",
                    entry.ask_for
                );
            }
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}
