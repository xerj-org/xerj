//! Drift guard: the MCP tool counts the docs state for main vs the tools the
//! server serves.
//!
//! `published_schema_drift.rs` pins `mcp-tools.json` to [`xerj_mcp::tool_specs`],
//! but the prose never had a guard. When #3337 added `xerj_references`, the
//! schema was regenerated and the server test was bumped (24 -> 25 with a hub
//! key, 21 -> 22 without), while README, AGENTS.md, llms.txt, llms-full.txt,
//! the home page and about 40 article pages kept saying "24 tools on main" and
//! "21 in a build with no hub key". Nothing compared them, so nothing failed.
//!
//! This test reads the hand-written agent-facing docs and the article sources
//! and checks every count a sentence states FOR MAIN: "N tools on main",
//! "on main it serves N", "N in a build with no hub key", "a build with no hub
//! key serves N". Counts tied to a release ("rc.104 serves 24", "17 tools on
//! rc.97") are history and are not checked. Blog posts are dated and are not
//! scanned. Generated pages (`landing/answers`, `landing/compare`) are not
//! scanned either: their source is `content/`, and `build_articles.py --check`
//! keeps them in step with it.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate manifest dir must be <repo>/engine/crates/xerj-mcp")
        .to_path_buf()
}

fn served(include_hub: bool) -> usize {
    xerj_mcp::tool_specs_with(include_hub)
        .as_array()
        .expect("tool_specs is an array")
        .len()
}

/// The files to scan. A missing directory is a hard failure: a guard that
/// quietly scans nothing is the failure mode it exists to prevent.
fn doc_files(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = [
        "README.md",
        "AGENTS.md",
        "landing/llms.txt",
        "landing/llms-full.txt",
    ]
    .iter()
    .map(|p| root.join(p))
    .collect();
    let mut add_dir = |dir: &str, ext: &str, recurse: bool| {
        let mut stack = vec![root.join(dir)];
        let mut found = 0;
        while let Some(d) = stack.pop() {
            let entries = std::fs::read_dir(&d)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", d.display()));
            for entry in entries {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    if recurse {
                        stack.push(path);
                    }
                } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
                    out.push(path);
                    found += 1;
                }
            }
        }
        assert!(
            found > 0,
            "no .{ext} files under {dir}: the layout moved, fix this test"
        );
    };
    add_dir("landing", "html", false);
    add_dir("landing/docs", "html", true);
    add_dir("content", "md", true);
    out
}

/// One word of prose: lowercased, with markup, backticks and punctuation
/// removed, and a number word turned into digits. `stop` marks a word that
/// ended a clause (`.`, `;`, `:`, `!`, `?`, `)`, `,`): a claim never spans
/// one. `sentence` numbers the sentence the word is in.
struct Word {
    w: String,
    stop: bool,
    sentence: usize,
}

fn words(text: &str) -> Vec<Word> {
    let mut plain = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                plain.push(' ');
            }
            _ if in_tag => {}
            _ => plain.push(c),
        }
    }
    const UNITS: [&str; 10] = [
        "", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ];
    const TEENS: [&str; 10] = [
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    let mut sentence = 0;
    let mut out = Vec::new();
    for raw in plain.split_whitespace() {
        let tail = raw.trim_end_matches(['`', '*', '"', '\'', ']']);
        let stop = tail.ends_with(['.', ';', ':', '!', '?', ')', ',']);
        let ends_sentence = tail.ends_with(['.', ';', '!', '?']) || tail.ends_with(").");
        let w: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '.')
            .collect::<String>()
            .trim_matches(|c| c == '.' || c == '-')
            .to_lowercase();
        let w = if let Some(i) = TEENS.iter().position(|t| *t == w) {
            (10 + i).to_string()
        } else {
            let mut v = w;
            for (tens, base) in [("twenty", 20), ("thirty", 30)] {
                if v == tens {
                    v = base.to_string();
                } else if let Some(i) = v
                    .strip_prefix(tens)
                    .and_then(|r| r.strip_prefix('-'))
                    .and_then(|unit| UNITS.iter().position(|u| *u == unit))
                {
                    v = (base + i).to_string();
                }
            }
            v
        };
        out.push(Word { w, stop, sentence });
        if ends_sentence {
            sentence += 1;
        }
    }
    out
}

#[derive(Debug, PartialEq)]
struct Claim {
    count: usize,
    hub: bool,
    context: String,
}

/// Every MCP tool count claimed for main in `text`, with whether it is the
/// with-hub-key or the no-hub-key figure. Deliberately narrow: a miss costs a
/// stale sentence, a false hit costs a red test on correct prose.
fn claims(text: &str) -> Vec<Claim> {
    let w = words(text);
    let n_words = w.len();
    let num = |i: usize| {
        w.get(i)
            .and_then(|x| x.w.parse::<usize>().ok())
            .filter(|n| (1..100).contains(n))
    };
    let is = |i: usize, s: &str| w.get(i).is_some_and(|x| x.w == s);
    // No clause boundary on any word in [a, b).
    let joined = |a: usize, b: usize| (a..b).all(|t| w.get(t).is_some_and(|x| !x.stop));
    // The HTTP transport lists a read-only subset; its counts are not this one.
    let mut http = vec![false; w.last().map_or(0, |x| x.sentence) + 1];
    for x in &w {
        if x.w == "http" {
            http[x.sentence] = true;
        }
    }
    let http_sentence = |i: usize| http[w[i].sentence];
    let ctx = |i: usize| {
        let a = i.saturating_sub(8);
        let b = (i + 8).min(n_words);
        w[a..b]
            .iter()
            .map(|x| x.w.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    // "no hub key" / "without a hub key" ends at word `j` ("key").
    let no_hub_key_at = |j: usize| {
        is(j, "key")
            && is(j.wrapping_sub(1), "hub")
            && (is(j.wrapping_sub(2), "no") || is(j.wrapping_sub(3), "without"))
    };
    let mut out = Vec::new();
    for i in 0..n_words {
        if http_sentence(i) {
            continue;
        }
        if let Some(n) = num(i) {
            // "N tools on main", "N MCP tools on main", "N on main".
            let mut k = i + 1;
            if is(k, "mcp") {
                k += 1;
            }
            if is(k, "tools") {
                k += 1;
            }
            if is(k, "on") && is(k + 1, "main") && !is(k + 2, "ci") && joined(i, k + 1) {
                // "(22 tools on main, ...)" after "no hub key omits the three
                // hub tools" is the no-key figure.
                let a = i.saturating_sub(12);
                let hub = !(a..i).any(|j| no_hub_key_at(j) && joined(j, i));
                out.push(Claim {
                    count: n,
                    hub,
                    context: ctx(i),
                });
            }
            // "N in a build with no hub key", "N with no hub key",
            // "N without a hub key", "N tools in a build with no hub key".
            if let Some(j) = (i + 2..(i + 9).min(n_words)).find(|&j| no_hub_key_at(j)) {
                if joined(i, j) && (i + 1..j).all(|t| num(t).is_none()) {
                    out.push(Claim {
                        count: n,
                        hub: false,
                        context: ctx(i),
                    });
                }
            }
        }
        // "On main it serves N", "on main `xerj mcp` serves N", "On main, N
        // tools are exposed", "on main the list has N".
        if is(i, "on") && is(i + 1, "main") {
            for k in i + 2..(i + 7).min(n_words) {
                if !joined(k - 1, k) && k > i + 2 {
                    break;
                }
                if let Some(n) = num(k) {
                    let verb = ["serves", "has", "lists", "exposes", "registers"]
                        .iter()
                        .any(|v| is(k - 1, v));
                    let bare = k == i + 2 && is(k + 1, "tools");
                    if (verb && joined(i + 1, k)) || bare {
                        out.push(Claim {
                            count: n,
                            hub: true,
                            context: ctx(i),
                        });
                    }
                    break;
                }
            }
        }
        // "a build with no hub key serves N", "no hub key omits the three hub
        // tools and serves N".
        if no_hub_key_at(i) {
            for k in i + 1..(i + 10).min(n_words) {
                if is(k, "serves") {
                    if let Some(n) = num(k + 1).filter(|_| joined(i, k + 1)) {
                        out.push(Claim {
                            count: n,
                            hub: false,
                            context: ctx(k),
                        });
                    }
                    break;
                }
            }
        }
    }
    out
}

fn pairs(text: &str) -> Vec<(usize, bool)> {
    claims(text).iter().map(|c| (c.count, c.hub)).collect()
}

#[test]
fn claims_parser_reads_the_phrasings_the_docs_use() {
    assert_eq!(
        pairs("On main `xerj mcp` serves 25 tools, or 22 in a build with no hub key."),
        [(25, true), (22, false)]
    );
    assert_eq!(
        pairs(
            "A binary with no hub key omits the three hub tools (22 tools on main, 20 on rc.101)."
        ),
        [(22, false)]
    );
    assert_eq!(
        pairs("Twenty-five tools on main. A build with no hub key serves twenty-two."),
        [(25, true), (22, false)]
    );
    assert_eq!(
        pairs("<td>On main it serves 24 tools</td> and 17 tools, version 1.0.0-rc.97 (25 on main)"),
        [(24, true), (25, true)]
    );
    assert_eq!(
        pairs("On main, 25 tools are exposed when Hub tools are enabled, and 22 in a build with no hub key."),
        [(25, true), (22, false)]
    );
    assert_eq!(
        pairs("| MCP tools | 4 | 25 on main, 22 with no hub key |"),
        [(25, true), (22, false)]
    );
}

#[test]
fn claims_parser_ignores_counts_that_are_not_main_mcp_tool_counts() {
    // Release-tied counts are history.
    assert!(pairs("The rc.104 release serves 24; rc.97 returned 17 tools.").is_empty());
    assert!(pairs("(on main; the rc.104 release serves 24).").is_empty());
    // A different subject that happens to sit next to "on main".
    assert!(pairs("1,444 passed / 0 failed of 1454 on main CI as of 2026-10-23").is_empty());
    assert!(pairs("0 failed on main: 1448 of 1458 cases passed").is_empty());
    assert!(pairs("In August it returned 10. On main the list has 25.") == [(25, true)]);
    assert!(pairs("25 tools are served on main (22 in a build with no hub key).") == [(22, false)]);
    // The HTTP transport's read-only subset.
    assert!(pairs("Over HTTP only the 15 read tools are listed (14 with no hub key).").is_empty());
    // The facts sentence after a no-key clause is not a no-key count.
    assert!(pairs(
        "22 in a build with no hub key; --facts-model-dir adds one. The rc.104 release serves 24."
    ) == [(22, false)]);
}

#[test]
fn every_tool_count_the_docs_state_for_main_matches_tool_specs() {
    let root = repo_root();
    let (with_hub, without_hub) = (served(true), served(false));
    let mut checked = 0;
    let mut wrong = Vec::new();
    for path in doc_files(&root) {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        for c in claims(&text) {
            checked += 1;
            let want = if c.hub { with_hub } else { without_hub };
            if c.count != want {
                wrong.push(format!(
                    "{}: says {} ({}), main serves {want}: \"…{}…\"",
                    path.strip_prefix(&root).unwrap_or(&path).display(),
                    c.count,
                    if c.hub {
                        "with a hub key"
                    } else {
                        "no hub key"
                    },
                    c.context
                ));
            }
        }
    }
    eprintln!("{checked} MCP tool count claim(s) checked");
    assert!(
        checked >= 20,
        "only {checked} count claims found: the parser or the docs moved"
    );
    assert!(
        wrong.is_empty(),
        "{} stale MCP tool count(s) out of {checked} (tool_specs: {with_hub} with a hub key, \
         {without_hub} without). Fix the content/ source, then run \
         `python3 scripts/seo/build_articles.py --write`:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
