//! Docs-vs-source drift guard for the published capability lists (issue #211).
//!
//! A source review concluded XERJ had no pipeline aggregations. It has fifteen.
//! The reviewer had read a hand-maintained list that stopped at `composite`,
//! and nearly opened a roadmap item to build a feature that already shipped.
//! The same lists advertised `has_child` / `has_parent`, which the parser
//! rejects with a 400.
//!
//! The root cause is that "what XERJ supports" was written down in prose in
//! several places and derived from the code in none of them. It is now derived
//! in exactly one place per subsystem —
//! [`xerj_query::parser::SUPPORTED_QUERY_TYPES`] and
//! [`xerj_engine::aggs::SUPPORTED_AGG_TYPES`], each pinned to its own dispatch
//! table by a unit test — and every published list is a marked region checked
//! against those constants here.
//!
//! Adding a query type or an aggregation now fails this test until the docs
//! are updated. That is the point: the failure is cheap, and the wrong
//! conclusion it prevents is not.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Repository root, derived from this crate's manifest directory
/// (`<repo>/engine/crates/xerj-engine`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate manifest dir must be <repo>/engine/crates/xerj-engine")
        .to_path_buf()
}

/// Read a directory, or fail the test naming it.
///
/// The walkers below used `let Ok(entries) = read_dir(dir) else { return }` and
/// `entries.flatten()`, so an unreadable directory removed its whole subtree
/// from the scan and the test still reported success. The `> 20` floors on the
/// collected file counts do not catch it: `landing/docs/playbooks/` going
/// unreadable drops those pages while sixty others keep the total above the
/// floor. A guard that silently checks less than it was asked to is the
/// accepted-and-ignored pattern (#204) this file exists to prevent, so both
/// the directory and each entry inside it are now hard failures.
fn read_dir_or_panic(dir: &Path) -> Vec<std::fs::DirEntry> {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e} — this walk feeds the published-surface checks; a directory that cannot be read must fail the test, not shrink its scope", dir.display()));
    entries
        .map(|e| {
            e.unwrap_or_else(|err| {
                panic!(
                    "cannot read an entry of {}: {err} — see above; entries are not skipped",
                    dir.display()
                )
            })
        })
        .collect()
}

/// Read a published surface, or fail the test naming it.
///
/// `read_to_string(..).unwrap_or_default()` turned an unreadable or non-UTF-8
/// page into an empty string, which passes every check below by finding
/// nothing — a confidently wrong "no phantom query type here". Every one of
/// these paths came from a directory walk moments earlier, so a read failure
/// is a real fault and belongs in the failure output.
fn read_surface_or_panic(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "cannot read published surface {}: {e} — an unreadable page must fail \
             this test; treating it as empty would silently pass every check",
            path.display()
        )
    })
}

/// Documentation files that publish the **query**-type lists.
///
/// Paths are repo-relative. A file listed here that has lost its markers is a
/// hard failure, not a skip — a silently unchecked doc is the state this test
/// exists to end.
const QUERY_TYPE_DOCS: &[&str] = &[
    "engine/README.md",
    "landing/llms-full.txt",
    "landing/docs/queries.html",
];

/// Documentation files that publish the **aggregation**-type list.
const AGG_TYPE_DOCS: &[&str] = &[
    "engine/README.md",
    "landing/llms-full.txt",
    "landing/docs/aggregations.html",
];

/// Slice one `<!-- generated:<section> -->` … `<!-- /generated:<section> -->`
/// region out of a document.
fn region<'a>(doc: &'a str, path: &str, section: &str) -> &'a str {
    let open = format!("<!-- generated:{section} -->");
    let close = format!("<!-- /generated:{section} -->");

    let start = doc.find(&open).unwrap_or_else(|| {
        panic!(
            "{path} has no `{open}` marker. Every capability list is generated \
             from the source constants and delimited by these markers; if the \
             section was renamed or removed, update the *_DOCS lists in \
             this test rather than leaving the list unchecked."
        )
    }) + open.len();
    let end = doc[start..]
        .find(&close)
        .unwrap_or_else(|| panic!("{path} opens `{open}` but never closes it with `{close}`"))
        + start;
    &doc[start..end]
}

/// Reject anything inside a checked region that is not a bare ES type name.
///
/// Loudly, because a mis-parsed entry would silently satisfy neither side of
/// the comparison and re-open exactly the drift this file exists to close
/// (the accepted-and-ignored pattern tracked in #204).
fn bare_name(token: &str, path: &str, section: &str) -> String {
    assert!(
        !token.is_empty()
            && token
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
        "{path} section `{section}` contains `{token}`, which is not a bare type name. \
         The region is machine-checked and may hold only type names plus plain-text \
         family labels — put prose outside the markers."
    );
    token.to_string()
}

/// Markdown / plain-text regions: only backtick-delimited tokens count, so the
/// region can carry readable family labels ("Full-text:", "Pipeline:") without
/// them being mistaken for capability names.
fn documented_md(doc: &str, path: &str, section: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut rest = region(doc, path, section);
    while let Some(o) = rest.find('`') {
        let after = &rest[o + 1..];
        let Some(c) = after.find('`') else {
            panic!("{path} section `{section}` has an unclosed backtick");
        };
        names.insert(bare_name(after[..c].trim(), path, section));
        rest = &after[c + 1..];
    }
    names
}

/// HTML regions on the docs site: one name per `<div class="item">` card, read
/// up to the card's optional `<span class="dsc">` description.
///
/// Descriptions and headings are prose and are deliberately not scanned — only
/// the card's own name is a capability claim.
fn documented_html(doc: &str, path: &str, section: &str) -> BTreeSet<String> {
    const CARD: &str = "<div class=\"item\"";
    let reg = region(doc, path, section);
    let mut names = BTreeSet::new();
    let mut rest = reg;
    while let Some(o) = rest.find(CARD) {
        let after = &rest[o + CARD.len()..];
        let Some(gt) = after.find('>') else {
            panic!("{path} section `{section}` has an unterminated `{CARD}` tag");
        };
        let body = &after[gt + 1..];
        // The name runs to the description span, or to the closing tag.
        let cut = body.find('<').unwrap_or_else(|| {
            panic!("{path} section `{section}` has a card that is never closed")
        });
        names.insert(bare_name(body[..cut].trim(), path, section));
        rest = &body[cut..];
    }
    assert!(
        !names.is_empty(),
        "{path} section `{section}` contains no `{CARD}` cards — the markup changed \
         and this check is reading nothing"
    );
    names
}

fn documented(doc: &str, path: &str, section: &str) -> BTreeSet<String> {
    if path.ends_with(".html") {
        documented_html(doc, path, section)
    } else {
        documented_md(doc, path, section)
    }
}

fn assert_section(section: &str, docs: &[&str], expected: &[&str]) {
    let root = repo_root();
    let expected: BTreeSet<String> = expected.iter().map(|s| s.to_string()).collect();

    for rel in docs {
        let path = root.join(rel);
        let doc = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let listed = documented(&doc, rel, section);

        let missing: Vec<_> = expected.difference(&listed).collect();
        let extra: Vec<_> = listed.difference(&expected).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{rel} section `{section}` has drifted from the source of truth.\n  \
             implemented but undocumented: {missing:?}\n  \
             documented but not implemented: {extra:?}\n  \
             The lists are generated: copy them from the constants in \
             xerj-query/src/parser.rs and xerj-engine/src/aggs.rs."
        );
    }
}

#[test]
fn documented_query_types_match_the_parser() {
    assert_section(
        "query-types",
        QUERY_TYPE_DOCS,
        xerj_query::parser::SUPPORTED_QUERY_TYPES,
    );
}

/// The other direction of the same defect: types the docs must show as
/// *rejected*, so nobody plans around a `has_child` that answers 400.
#[test]
fn documented_rejected_query_types_match_the_parser() {
    assert_section(
        "rejected-query-types",
        QUERY_TYPE_DOCS,
        xerj_query::parser::REJECTED_QUERY_TYPES,
    );
}

#[test]
fn documented_agg_types_match_the_engine() {
    assert_section(
        "agg-types",
        AGG_TYPE_DOCS,
        xerj_engine::aggs::SUPPORTED_AGG_TYPES,
    );
}

/// The markers are load-bearing, so prove the extractor actually rejects a
/// drifted list rather than passing on an empty or unparsed region.
#[test]
fn the_extractor_notices_drift() {
    let doc = "<!-- generated:query-types -->\n`match`, `term`\n<!-- /generated:query-types -->";
    let listed = documented(doc, "synthetic", "query-types");
    assert_eq!(
        listed,
        ["match", "term"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>()
    );
    assert!(!listed.contains("bucket_script"));
}

/// Same proof for the HTML card grids on the docs site, which is where the
/// second half of #211 was hiding: `landing/docs/queries.html` shipped cards
/// for `boosted` and `semantic_search`, neither of which the parser has ever
/// dispatched, and `landing/docs/aggregations.html` showed fifteen of
/// sixty-two aggregations with the whole pipeline family absent.
#[test]
fn the_html_extractor_reads_card_names_and_ignores_prose() {
    let doc = "<!-- generated:query-types -->\n\
               <h2>Structural</h2>\n\
               <div class=\"enum-list\">\n\
               <div class=\"item\" id=\"match-all\">match_all<span class=\"dsc\">Everything, \
               unlike match_none.</span></div>\n\
               <div class=\"item\">term</div>\n\
               </div>\n\
               <!-- /generated:query-types -->";
    let listed = documented(doc, "synthetic.html", "query-types");
    assert_eq!(
        listed,
        ["match_all", "term"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
        "a name mentioned only in a card description must not count as a capability"
    );
}

/// A card whose name is prose rather than a bare type name must fail loudly,
/// for the same reason the markdown extractor refuses a backticked qualifier:
/// a mis-read entry would satisfy neither side of the comparison and quietly
/// stop checking anything.
#[test]
#[should_panic(expected = "is not a bare type name")]
fn an_html_card_that_is_not_a_type_name_is_refused() {
    let doc = "<!-- generated:query-types -->\n\
               <div class=\"item\">knn (HNSW-served)</div>\n\
               <!-- /generated:query-types -->";
    documented(doc, "synthetic.html", "query-types");
}

/// Every `engine/crates/<path>` the docs site cites must exist.
///
/// The docs pages footer each section with a `Source · …` pointer, and twenty-one
/// of them named crates that have not existed under those paths for as long as
/// the workspace has been `xerj-*`-prefixed (`engine/crates/logs/src/parse.rs`,
/// `engine/crates/api/src/es_compat.rs`, `engine/crates/otlp/src/lib.rs`, …).
/// A reader following one lands nowhere, which is the same failure as a list
/// naming a query type that does not exist.
#[test]
fn every_source_pointer_in_the_docs_site_resolves() {
    fn html_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in read_dir_or_panic(dir) {
            let p = e.path();
            if p.is_dir() {
                html_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "html") {
                out.push(p);
            }
        }
    }

    let root = repo_root();
    let mut pages = Vec::new();
    html_files(&root.join("landing"), &mut pages);
    assert!(
        pages.len() > 20,
        "only {} pages found under landing/ — this check is reading nothing",
        pages.len()
    );

    let mut broken = Vec::new();
    for page in &pages {
        let text = read_surface_or_panic(page);
        for (idx, _) in text.match_indices("engine/crates/") {
            let tail: String = text[idx..]
                .chars()
                .take_while(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '*')
                })
                .collect();
            let cited = tail.trim_end_matches('.').to_string();
            // `engine/crates/*` and a bare `engine/crates/` are prose, not pointers.
            if cited.ends_with('*') || cited == "engine/crates/" {
                continue;
            }
            if !root.join(&cited).exists() {
                broken.push(format!(
                    "{} → {cited}",
                    page.strip_prefix(&root).unwrap_or(page).display()
                ));
            }
        }
    }
    broken.sort();
    broken.dedup();
    assert!(
        broken.is_empty(),
        "the docs site cites source paths that do not exist:\n  {}",
        broken.join("\n  ")
    );
}

/// The crate map is the same defect in a different list: it named eleven of the
/// sixteen crates, so an agent orienting from it never learned that
/// `xerj-autoindex` — the flagship feature — exists.
///
/// Checked by containment rather than by markers, because the map is a table
/// with a prose column: every `crates/*` workspace member must be named in it,
/// and every crate it names must still be a member.
#[test]
fn the_readme_crate_map_lists_every_workspace_crate() {
    let root = repo_root();
    let manifest =
        std::fs::read_to_string(root.join("engine/Cargo.toml")).expect("engine manifest");
    let readme_path = root.join("engine/README.md");
    let readme = std::fs::read_to_string(&readme_path).expect("engine README");

    let members: BTreeSet<String> = manifest
        .lines()
        .filter_map(|l| l.trim().strip_prefix("\"crates/"))
        .filter_map(|l| l.split('"').next())
        .map(str::to_string)
        .collect();
    assert!(
        members.len() > 10,
        "only {} workspace members parsed out of engine/Cargo.toml — the manifest \
         layout changed and this check is reading nothing",
        members.len()
    );

    let map_start = readme
        .find("### Crate Map")
        .expect("engine/README.md lost its `### Crate Map` heading");
    let map = &readme[map_start..];
    let map = &map[..map.find("\n### ").unwrap_or(map.len())];

    // Only the first cell of a table row counts. A crate merely *mentioned* in
    // the surrounding prose is not documented, and an earlier draft of this
    // test accepted exactly that — the guard has to be harder to satisfy than
    // the thing it guards.
    let rows: BTreeSet<String> = map
        .lines()
        .filter_map(|l| l.trim().strip_prefix("| `"))
        .filter_map(|l| l.split('`').next())
        .map(str::to_string)
        .collect();

    let undocumented: Vec<_> = members.difference(&rows).collect();
    assert!(
        undocumented.is_empty(),
        "engine/README.md's crate map has no row for {undocumented:?} — every crate \
         under engine/crates/ needs its own `| `<crate>` | purpose |` row"
    );

    let phantom: Vec<_> = rows
        .iter()
        .filter(|r| r.starts_with("xerj-") && !members.contains(*r))
        .collect();
    assert!(
        phantom.is_empty(),
        "engine/README.md's crate map has a row for {phantom:?}, which is not a workspace member"
    );
}

/// A backticked qualifier — the shape the old prose lists used, e.g.
/// `knn (HNSW-served unfiltered / exact filtered)` — must fail loudly rather
/// than be absorbed as a capability name that then never matches anything.
/// Accepted-and-ignored input is the failure mode this repo tracks in issue
/// #204; the docs guard must not add a documentation-flavoured instance of it.
#[test]
#[should_panic(expected = "is not a bare type name")]
fn a_backticked_qualifier_inside_a_region_is_refused() {
    let doc =
        "<!-- generated:query-types -->\n`knn (HNSW-served)`\n<!-- /generated:query-types -->";
    documented(doc, "synthetic", "query-types");
}

// ---------------------------------------------------------------------------
// Beyond the marked regions: the *examples*.
//
// The marked-region checks above only see a list that opted in. That left a
// hole big enough for the original defect to survive the first fix: while
// `landing/docs/queries.html` stopped advertising a `semantic_search` card,
// `landing/docs/playbooks/vector-search.html` went on publishing
// `{"semantic_search": {"field": …, "text": …}}` as *the* worked example for
// the flagship AI-native feature, and `landing/docs/migration-from-es.html`
// went on naming it in the migrator's "what works day one" list. Sent to a
// live instance, that body answers
// `unknown query type `semantic_search`` with a 400.
//
// So the guard now also reads the prose and the samples, over every published
// surface rather than the three files that carry markers.
// ---------------------------------------------------------------------------

/// Every published surface whose text is scanned for capability *claims*
/// (as opposed to the marked lists): the whole docs site, the agent-facing
/// text files, and the two repository documents an evaluator reads first.
fn doc_surfaces() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in read_dir_or_panic(dir) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                let keep = p.extension().is_some_and(|x| x == "html")
                    || p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("llms") && n.ends_with(".txt"));
                if keep {
                    out.push(p);
                }
            }
        }
    }

    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("landing"), &mut files);
    files.push(root.join("engine/README.md"));
    files.push(root.join("ROADMAP.md"));
    files.sort();
    assert!(
        files.len() > 20,
        "only {} published surfaces found — this check is reading nothing",
        files.len()
    );
    files
}

fn known_query_types() -> BTreeSet<String> {
    xerj_query::parser::SUPPORTED_QUERY_TYPES
        .iter()
        .chain(xerj_query::parser::REJECTED_QUERY_TYPES)
        .map(|s| s.to_string())
        .collect()
}

/// `true` when the byte at `at` starts a token — i.e. it is not preceded by a
/// character that would make it part of a longer identifier. Keeps
/// `xerj_semantic_search` (a real MCP tool) from reading as `semantic_search`
/// (a query type that has never existed).
fn at_token_start(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn token_ends_at(text: &str, end: usize) -> bool {
    text[end..]
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn line_of(text: &str, byte: usize) -> usize {
    text[..byte].matches('\n').count() + 1
}

/// Walk `text` from `from`, skipping whitespace, and require it to spell out
/// `wanted` in order. Returns the byte offset just past the last character, or
/// `None` the moment the text diverges.
fn expect_seq(text: &str, from: usize, wanted: &[char]) -> Option<usize> {
    let mut want = wanted.iter();
    let mut next = want.next()?;
    for (off, ch) in text[from..].char_indices() {
        if ch.is_whitespace() {
            continue;
        }
        if ch != *next {
            return None;
        }
        match want.next() {
            Some(n) => next = n,
            None => return Some(from + off + ch.len_utf8()),
        }
    }
    None
}

/// Query-type names the docs have published that the parser has never
/// dispatched. `semantic_search` (the real clause is `semantic`) and `boosted`
/// (the real clause is `boosting`) both shipped on the docs site; a reader who
/// copied either got a 400.
///
/// This is a denylist rather than a general rule because prose is prose: there
/// is no position in an English sentence that reliably marks a word as a
/// capability claim. It is kept honest by
/// [`the_phantom_list_only_holds_names_the_parser_really_lacks`], which retires
/// an entry the moment the parser grows it.
const PHANTOM_QUERY_TYPES: &[&str] = &["boosted", "semantic_search"];

/// If XERJ ever implements one of these, the denylist must shrink, not silently
/// forbid documenting a real feature.
#[test]
fn the_phantom_list_only_holds_names_the_parser_really_lacks() {
    let known = known_query_types();
    let wrong: Vec<_> = PHANTOM_QUERY_TYPES
        .iter()
        .filter(|p| known.contains(**p))
        .collect();
    assert!(
        wrong.is_empty(),
        "{wrong:?} are now dispatched by the parser — remove them from \
         PHANTOM_QUERY_TYPES and document them instead of banning them"
    );
}

/// No published surface may name a query type that does not exist — not in a
/// list, not in a sentence, not in a copy-pasteable example.
#[test]
fn no_published_surface_names_a_phantom_query_type() {
    let root = repo_root();
    let mut hits = Vec::new();

    for file in doc_surfaces() {
        let text = read_surface_or_panic(&file);
        for phantom in PHANTOM_QUERY_TYPES {
            for (at, _) in text.match_indices(phantom) {
                if at_token_start(&text, at) && token_ends_at(&text, at + phantom.len()) {
                    hits.push(format!(
                        "{}:{} names `{phantom}`",
                        file.strip_prefix(&root).unwrap_or(&file).display(),
                        line_of(&text, at)
                    ));
                }
            }
        }
    }

    hits.sort();
    assert!(
        hits.is_empty(),
        "the published docs name query types the parser has never had:\n  {}\n\
         The real clauses are `semantic` (field + query, k defaults to 10) and \
         `boosting`. A reader who copies a phantom gets `unknown query type`.",
        hits.join("\n  ")
    );
}

/// Anything in *query position* in a published sample must be a real query
/// type. A clause sitting directly under a `"query": { … }` object is a query
/// type by construction — that is the one place in an ES request body where the
/// key's meaning is unambiguous — so it can be checked without a denylist.
#[test]
fn every_query_clause_in_a_published_sample_is_a_real_query_type() {
    let root = repo_root();
    let known = known_query_types();
    let mut bad = Vec::new();
    let mut checked = 0usize;

    for file in doc_surfaces() {
        let text = read_surface_or_panic(&file);
        for (at, _) in text.match_indices("\"query\"") {
            // `"query"` `:` `{` `"<name>"` — anything else (a string value, as
            // in `match` / `query_string` / `semantic`, or an array) is not a
            // clause position and is skipped.
            let Some(from) = expect_seq(&text, at + "\"query\"".len(), &[':', '{', '"']) else {
                continue;
            };
            let Some(len) = text[from..].find('"') else {
                continue;
            };
            let name = &text[from..from + len];
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                continue; // not an ES type name — a doc field, a label, prose
            }
            checked += 1;
            if !known.contains(name) {
                bad.push(format!(
                    "{}:{} uses `{name}` in query position",
                    file.strip_prefix(&root).unwrap_or(&file).display(),
                    line_of(&text, at)
                ));
            }
        }
    }

    assert!(
        checked > 20,
        "only {checked} query clauses found across the docs — this check is reading nothing"
    );
    bad.sort();
    bad.dedup();
    assert!(
        bad.is_empty(),
        "published request samples put a name in query position that \
         `parse_query` does not dispatch:\n  {}",
        bad.join("\n  ")
    );
}

/// Published *counts* drift exactly like published lists, and were drifting in
/// three different directions at once: `landing/docs/migration-from-es.html`
/// said 32 query types, `landing/pricing/index.html` and
/// `landing/demo/index.html` said 38, and the parser dispatches 50. So a number
/// a reader is expected to trust now lives in a marked region too.
///
/// Unlike the list regions, a file may carry the same count section more than
/// once (the pricing page prints the figure in a plan bullet and again in the
/// comparison table); every occurrence is checked.
///
/// `ROADMAP.md` is markdown rather than HTML, and was originally left out —
/// which re-created the defect inside its own fix: it published `50 query
/// types` and `62 aggregation types` as hand-typed literals, so adding query
/// type 51 would have left it silently saying 50 with every test green.
/// HTML comments are invisible in rendered markdown, so the same marker works
/// there unchanged.
const COUNT_DOCS: &[&str] = &[
    "landing/docs/migration-from-es.html",
    "landing/pricing/index.html",
    "landing/demo/index.html",
    "ROADMAP.md",
];

/// The count sections and where each number comes from.
fn count_source_of_truth(section: &str) -> usize {
    match section {
        "query-type-count" => xerj_query::parser::SUPPORTED_QUERY_TYPES.len(),
        "rejected-query-type-count" => xerj_query::parser::REJECTED_QUERY_TYPES.len(),
        "agg-type-count" => xerj_engine::aggs::SUPPORTED_AGG_TYPES.len(),
        other => panic!("no source of truth wired up for count section `{other}`"),
    }
}

const COUNT_SECTIONS: &[&str] = &[
    "query-type-count",
    "rejected-query-type-count",
    "agg-type-count",
];

#[test]
fn published_capability_counts_match_the_constants() {
    let root = repo_root();
    let mut seen = 0usize;

    for rel in COUNT_DOCS {
        let doc = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("cannot read {rel}: {e}"));

        for section in COUNT_SECTIONS {
            let open = format!("<!-- generated:{section} -->");
            let close = format!("<!-- /generated:{section} -->");
            for (at, _) in doc.match_indices(&open) {
                let from = at + open.len();
                let len = doc[from..].find(&close).unwrap_or_else(|| {
                    panic!(
                        "{rel} opens `{open}` at line {} but never closes it",
                        line_of(&doc, at)
                    )
                });
                let raw = doc[from..from + len].trim();
                let published: usize = raw.parse().unwrap_or_else(|_| {
                    panic!(
                        "{rel} section `{section}` holds `{raw}`, which is not a number. \
                         The region is machine-checked and may hold only the count."
                    )
                });
                assert_eq!(
                    published,
                    count_source_of_truth(section),
                    "{rel}:{} publishes {published} for `{section}`; the source has {}",
                    line_of(&doc, at),
                    count_source_of_truth(section)
                );
                seen += 1;
            }
        }
    }

    assert!(
        seen >= 9,
        "only {seen} marked counts found across {COUNT_DOCS:?} — a page dropped its \
         markers and its number is now unchecked"
    );
}
