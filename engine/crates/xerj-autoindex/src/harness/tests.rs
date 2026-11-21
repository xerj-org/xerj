//! Synthetic fixtures only: no transcript text, paths or names from real
//! sessions appear here.
use super::detect::{detect, Rule};
use super::parse::{classify, event_from_call, parse_bash, tokens, Ev};
use super::*;
use serde_json::json;
use std::path::Path;

fn bash(cmd: &str, stdout: &str, cwd: &str, n: u64, t: f64) -> Ev {
    let mut ev = event_from_call(
        "Bash",
        &json!({"command": cmd}),
        &json!({"stdout": stdout, "stderr": ""}),
        cwd,
        &format!("tu{n}"),
        t,
    );
    ev.n = n;
    ev
}

fn read(path: &str, n: u64, t: f64) -> Ev {
    let mut ev = event_from_call(
        "Read",
        &json!({"file_path": path}),
        &json!({}),
        "/w",
        &format!("tu{n}"),
        t,
    );
    ev.n = n;
    ev
}

#[test]
fn tokens_split_identifiers_and_drop_regex() {
    let (toks, idents) = tokens(r"fn renewLease\|lease_table\b");
    assert!(
        toks.contains(&"renew".to_string())
            && toks.contains(&"lease".to_string())
            && toks.contains(&"table".to_string()),
        "{toks:?}"
    );
    assert!(
        idents.contains(&"renewlease".to_string()) && idents.contains(&"lease_table".to_string()),
        "{idents:?}"
    );
    assert!(!toks.contains(&"fn".to_string()));
    let (t2, _) = tokens("ghp_abcdefghijklmnopqrstuvwxyz0123456789");
    assert!(
        t2.iter().all(|t| !t.starts_with("ghp")),
        "secret-shaped token kept: {t2:?}"
    );
}

#[test]
fn classify_patterns() {
    assert_eq!(classify("renew_lease"), "identifier");
    assert_eq!(classify("lease"), "word");
    assert_eq!(classify(r"^\s*$"), "regex");
    assert_eq!(classify("how leases renew"), "natural");
    assert_eq!(classify(r"foo\|renewLease"), "identifier");
    assert_eq!(classify("$PAT"), "shell_var");
}

#[test]
fn bash_parse_tracks_cd_vars_markers_and_caps() {
    let p = parse_bash("cd /w/repo && echo '=== a ==='; grep -rn 'renew_lease' crates | head -20; echo '=== b ==='; rg -n leaseTable", Path::new("/"));
    assert_eq!(p.probes.len(), 2);
    assert_eq!(p.probes[0].root, "/w/repo/crates");
    assert!(p.probes[0].tree);
    assert_eq!(p.probes[0].cap, Some(20));
    assert_eq!(p.probes[1].root, "/w/repo");
    let v = parse_bash("WS=/w/x\ngrep -rn needle_thing $WS/src", Path::new("/"));
    assert_eq!(v.probes[0].root, "/w/x/src");
    let files = parse_bash("grep -n renew_lease src/a.rs src/b.rs", Path::new("/w"));
    assert!(!files.probes[0].tree, "named files are not a tree search");
    let filt = parse_bash("cargo test 2>&1 | grep -i failed", Path::new("/w"));
    assert!(filt.probes.is_empty(), "a stdin filter is not a search");
    let heredoc = parse_bash(
        "cat > x.txt <<'EOF'\ngrep -rn nope .\nEOF\nrg -n real_one",
        Path::new("/w"),
    );
    assert_eq!(heredoc.probes.len(), 1);
    assert_eq!(heredoc.probes[0].idents, vec!["real_one".to_string()]);
    let lister = parse_bash("git ls-files | grep -i lease_table", Path::new("/w"));
    assert!(
        lister.probes[0].path_search
            && lister.probes[0].idents.contains(&"lease_table".to_string())
    );
}

#[test]
fn empty_marker_section_is_a_zero_probe() {
    let cmd =
        "echo '=== one ==='; grep -rn alpha_beta src; echo '=== two ==='; grep -rn gamma_delta src";
    let ev = bash(
        cmd,
        "=== one ===\n=== two ===\nsrc/x.rs:3:gamma_delta()\n",
        "/w",
        1,
        0.0,
    );
    assert_eq!(ev.probes[0].zero, Some(true));
    assert_eq!(ev.probes[1].zero, Some(false));
    assert!(ev.shown.contains(&"/w/src/x.rs".to_string()));
    let single = bash("grep -rn alpha_beta src", "", "/w", 2, 0.0);
    assert_eq!(single.probes[0].zero, Some(true));
    let mixed = bash("cat a.txt; grep -rn alpha_beta src", "text\n", "/w", 3, 0.0);
    assert_eq!(
        mixed.probes[0].zero, None,
        "mixed producers: unknown, never a zero"
    );
}

// ── decision rule: R-8 style positives and negatives ──

fn pos_chains() -> Vec<Vec<Ev>> {
    let mut out = Vec::new();
    let names = [
        "lease_renew",
        "token_bucket",
        "frame_cull",
        "shard_merge",
        "wal_replay",
        "conn_track",
        "lod_level",
    ];
    for (i, n) in names.iter().enumerate() {
        let camel: String = n
            .split('_')
            .enumerate()
            .map(|(k, p)| {
                if k == 0 {
                    p.to_string()
                } else {
                    p[..1].to_uppercase() + &p[1..]
                }
            })
            .collect();
        out.push(vec![
            bash(&format!("grep -rn {n} src"), "", "/w", 1, 0.0),
            bash(
                &format!("grep -rn {camel} src"),
                "",
                "/w",
                2,
                10.0 + i as f64,
            ),
        ]);
    }
    for n in names.iter().take(7) {
        let part = n.split('_').next().unwrap_or(n);
        out.push(vec![
            bash(
                &format!("grep -rn '{part}' src | head -5"),
                &format!("src/a.rs:1:{part}\nsrc/b.rs:2:{part}\n"),
                "/w",
                1,
                0.0,
            ),
            bash(&format!("rg -n '{n}' src"), "src/c.rs:9:x\n", "/w", 2, 20.0),
            read("/w/src/unrelated.rs", 3, 30.0),
            bash(
                &format!("grep -rn 'fn {n}' src"),
                "src/d.rs:4:y\n",
                "/w",
                4,
                40.0,
            ),
        ]);
    }
    for n in names.iter().take(6) {
        let part = n.split('_').next().unwrap_or(n);
        let flood: String = (0..20).map(|k| format!("src/f{k}.rs:1:{part}\n")).collect();
        out.push(vec![
            bash(
                &format!("grep -rn {part} src | head -20"),
                &flood,
                "/w",
                1,
                0.0,
            ),
            bash(
                &format!("grep -rn {n} src/core | head -20"),
                "src/core/q.rs:2:z\n",
                "/w",
                2,
                15.0,
            ),
        ]);
    }
    out
}

fn neg_chains() -> Vec<Vec<Ev>> {
    let mut out = Vec::new();
    // Found: the agent read a file the first search showed.
    for n in ["lease_renew", "token_bucket", "frame_cull"] {
        out.push(vec![
            bash(&format!("grep -rn {n} src"), "src/a.rs:1:x\n", "/w", 1, 0.0),
            read("/w/src/a.rs", 2, 5.0),
            bash(&format!("grep -rn {n} src"), "src/a.rs:1:x\n", "/w", 3, 9.0),
        ]);
    }
    // Unrelated searches.
    for (a, b, c) in [
        ("alpha_one", "beta_two", "gamma_three"),
        ("disk_quota", "user_agent", "page_cache"),
        ("retry_policy", "color_theme", "mesh_bounds"),
    ] {
        out.push(vec![
            bash(&format!("grep -rn {a} src"), "src/a.rs:1:x\n", "/w", 1, 0.0),
            bash(&format!("grep -rn {b} src"), "src/b.rs:1:x\n", "/w", 2, 1.0),
            bash(&format!("grep -rn {c} src"), "src/c.rs:1:x\n", "/w", 3, 2.0),
        ]);
    }
    // Too far apart (calls and seconds).
    out.push(vec![
        bash("grep -rn lease_renew src", "", "/w", 1, 0.0),
        bash("grep -rn leaseRenew src", "", "/w", 9, 10.0),
    ]);
    out.push(vec![
        bash("grep -rn lease_renew src", "", "/w", 1, 0.0),
        bash("grep -rn leaseRenew src", "", "/w", 2, 900.0),
    ]);
    // Named files, regex-only, remote, ops roots.
    out.push(vec![
        bash("grep -n lease_renew a.rs", "", "/w", 1, 0.0),
        bash("grep -n leaseRenew a.rs", "", "/w", 2, 1.0),
    ]);
    out.push(vec![
        bash(r"grep -rn '^\s*$' src", "", "/w", 1, 0.0),
        bash(r"grep -rn '^\s+$' src", "", "/w", 2, 1.0),
    ]);
    out.push(vec![
        bash("ssh box grep -rn lease_renew /srv", "", "/w", 1, 0.0),
        bash("ssh box grep -rn leaseRenew /srv", "", "/w", 2, 1.0),
    ]);
    out.push(vec![
        bash("grep -rn lease_renew /var/log", "", "/w", 1, 0.0),
        bash("grep -rn leaseRenew /var/log", "", "/w", 2, 1.0),
    ]);
    // Two related searches, no zero, no cap: not yet a struggle.
    for n in ["shard_merge", "wal_replay", "conn_track"] {
        out.push(vec![
            bash(&format!("grep -rn {n} src"), "src/a.rs:1:x\n", "/w", 1, 0.0),
            bash(
                &format!("grep -rn 'fn {n}' src"),
                "src/b.rs:1:x\n",
                "/w",
                2,
                3.0,
            ),
        ]);
    }
    // One zero then a hit; cap hit then an unrelated search.
    out.push(vec![
        bash("grep -rn lod_level src", "", "/w", 1, 0.0),
        bash("grep -rn lodLevel src", "src/l.rs:1:x\n", "/w", 2, 1.0),
    ]);
    let flood: String = (0..20).map(|k| format!("src/f{k}.rs:1:x\n")).collect();
    out.push(vec![
        bash("grep -rn mesh src | head -20", &flood, "/w", 1, 0.0),
        bash(
            "grep -rn audio_mixer src/core",
            "src/core/m.rs:1:x\n",
            "/w",
            2,
            1.0,
        ),
    ]);
    // Cap not hit then narrower.
    out.push(vec![
        bash(
            "grep -rn mesh src | head -20",
            "src/a.rs:1:x\n",
            "/w",
            1,
            0.0,
        ),
        bash(
            "grep -rn mesh_bounds src/core",
            "src/core/m.rs:1:x\n",
            "/w",
            2,
            1.0,
        ),
    ]);
    // Three related searches, but the second one's output was read.
    out.push(vec![
        bash("grep -rn slot_pool src", "src/a.rs:1:x\n", "/w", 1, 0.0),
        bash("grep -rn slotPool src", "src/p.rs:1:x\n", "/w", 2, 1.0),
        read("/w/src/p.rs", 3, 2.0),
        bash(
            "grep -rn 'fn slot_pool' src",
            "src/p.rs:1:x\n",
            "/w",
            4,
            3.0,
        ),
    ]);
    // Glob/path-only searches are not content struggles.
    out.push(vec![
        bash("find . -name '*.rs'", "./a.rs\n", "/w", 1, 0.0),
        bash("find src -name '*.rs'", "", "/w", 2, 1.0),
    ]);
    out
}

fn fires(chain: &[Ev]) -> Option<Rule> {
    detect(chain)
        .ok()
        .filter(|t| t.rule.active())
        .map(|t| t.rule)
}

#[test]
fn t20_positive_and_negative_chains() {
    let pos = pos_chains();
    let neg = neg_chains();
    assert_eq!(pos.len(), 20);
    assert_eq!(neg.len(), 20);
    let pos_fired = pos.iter().filter(|c| fires(c).is_some()).count();
    let neg_silent = neg.iter().filter(|c| fires(c).is_none()).count();
    for (i, c) in neg.iter().enumerate() {
        if let Some(r) = fires(c) {
            eprintln!("negative {i} fired {r:?}");
        }
    }
    for (i, c) in pos.iter().enumerate() {
        if fires(c).is_none() {
            eprintln!("positive {i} silent: {:?}", detect(c).err());
        }
    }
    eprintln!("T20: positives fired {pos_fired}/20, negatives silent {neg_silent}/20");
    assert!(pos_fired >= 18, "positives fired {pos_fired}/20");
    assert!(neg_silent >= 19, "negatives silent {neg_silent}/20");
    assert_eq!(fires(&pos[0]), Some(Rule::R1));
    assert_eq!(fires(&pos[7]), Some(Rule::R2));
    assert_eq!(fires(&pos[14]), Some(Rule::R3));
}

// ── hook JSON I/O end to end ──

fn fixture_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::create_dir_all(dir.join("src/core")).unwrap();
    for i in 0..60 {
        std::fs::write(
            dir.join(format!("src/m{i}.rs")),
            format!("// module {i}\npub fn filler_{i}() {{}}\n"),
        )
        .unwrap();
    }
    std::fs::write(dir.join("src/core/lease.rs"), "use x;\n\npub fn renew_lease_slot(table: &mut LeaseTable, now: u64) {\n    table.renew(now);\n}\n").unwrap();
    std::fs::write(dir.join("src/core/table.rs"), "pub struct LeaseTable { slots: Vec<u64> }\nimpl LeaseTable { pub fn bump(&mut self) {} }\n").unwrap();
    std::fs::write(dir.join("src/core/noise.rs"), "// lease notes\n").unwrap();
    std::fs::write(dir.join(".env"), "SECRET_LEASE_RENEW=1\n").unwrap();
}

fn call(cmd: &str, stdout: &str, id: &str) -> Value {
    json!({"tool_name": "Bash", "tool_input": {"command": cmd}, "tool_response": {"stdout": stdout, "stderr": "", "interrupted": false}, "tool_use_id": id})
}

#[test]
fn batch_hook_emits_additional_context_only_on_struggle() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fixture_repo(&repo);
    let state = tmp.path().join("state");
    let mut ctx = Ctx::new(state.clone()).unwrap();
    ctx.cfg_override = Some(Config {
        mode: Mode::Active,
        budget_ms: 2000,
        ..Config::default()
    });
    let cwd = repo.display().to_string();
    let batch = |calls: Vec<Value>| json!({"session_id": "s1", "hook_event_name": "PostToolBatch", "cwd": cwd, "tool_calls": calls});
    let o1 = handle(
        &ctx,
        "batch",
        &batch(vec![call("grep -rn lease_renew src", "", "a")]),
        true,
    );
    assert!(o1.stdout.is_none(), "first miss must stay silent");
    let o2 = handle(
        &ctx,
        "batch",
        &batch(vec![call("grep -rn leaseRenew src", "", "b")]),
        true,
    );
    let out = o2
        .stdout
        .expect("second related miss with a confident answer speaks");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolBatch");
    let text = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        text.starts_with("[xerj hook · 2nd empty related search (R1)"),
        "{text}"
    );
    assert!(text.contains("src/core/lease.rs:3"), "{text}");
    assert!(
        !text.contains(".env"),
        "secret files are never scanned: {text}"
    );
    assert!(render::estimate_tokens(text) <= 300);
    assert!(text.lines().count() <= 1 + 5 + 3);
    // Replayed tool_use_id is ignored (dedupe), and min-gap keeps it quiet.
    let o3 = handle(
        &ctx,
        "batch",
        &batch(vec![call("grep -rn leaseRenew src", "", "b")]),
        true,
    );
    assert!(o3.stdout.is_none() && o3.decisions.is_empty());
    let o4 = handle(
        &ctx,
        "batch",
        &batch(vec![call("grep -rn lease_renewing src", "", "c")]),
        true,
    );
    assert!(o4.stdout.is_none());
    assert_eq!(o4.decisions[0].reason, "min_gap");
}

#[test]
fn shadow_mode_logs_but_prints_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fixture_repo(&repo);
    let mut ctx = Ctx::new(tmp.path().join("state")).unwrap();
    ctx.cfg_override = Some(Config {
        mode: Mode::Shadow,
        budget_ms: 2000,
        ..Config::default()
    });
    let cwd = repo.display().to_string();
    for (i, c) in ["grep -rn lease_renew src", "grep -rn leaseRenew src"]
        .iter()
        .enumerate()
    {
        let o = handle(
            &ctx,
            "post",
            &json!({"session_id": "s2", "cwd": cwd, "tool_name": "Bash", "tool_input": {"command": c}, "tool_response": {"stdout": ""}, "tool_use_id": format!("x{i}")}),
            true,
        );
        assert!(o.stdout.is_none());
        if i == 1 {
            assert!(
                o.decisions[0].emitted,
                "shadow records what it would have said"
            );
        }
    }
    let log = std::fs::read_to_string(tmp.path().join("state/decisions.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 2);
    assert!(!log.contains("lease"), "decisions log carries no raw text");
}

#[test]
fn small_repo_and_unseen_rule_stay_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("tiny");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join("a.rs"), "pub fn renew_lease_slot() {}\n").unwrap();
    let mut ctx = Ctx::new(tmp.path().join("state")).unwrap();
    ctx.cfg_override = Some(Config {
        mode: Mode::Active,
        budget_ms: 2000,
        ..Config::default()
    });
    let cwd = repo.display().to_string();
    let mk = |c: &str, id: &str| json!({"session_id": "s3", "cwd": cwd, "tool_calls": [call(c, "", id)]});
    handle(&ctx, "batch", &mk("grep -rn lease_renewal .", "a"), true);
    let o = handle(&ctx, "batch", &mk("grep -rn leaseRenewal .", "b"), true);
    assert!(o.stdout.is_none());
    assert_eq!(o.decisions[0].reason, "small_repo");
}

#[test]
fn history_pair_needs_two_streams_then_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fixture_repo(&repo);
    std::fs::write(
        repo.join("src/core/far.rs"),
        "// the slot keeper\npub fn keeper() {}\n",
    )
    .unwrap();
    let mut ctx = Ctx::new(tmp.path().join("state")).unwrap();
    ctx.cfg_override = Some(Config {
        mode: Mode::Active,
        budget_ms: 2000,
        ..Config::default()
    });
    let cwd = repo.display().to_string();
    let far = repo.join("src/core/far.rs").display().to_string();
    for s in ["h1", "h2"] {
        let sess = |calls: Vec<Value>| json!({"session_id": s, "cwd": cwd, "tool_calls": calls});
        handle(
            &ctx,
            "batch",
            &sess(vec![call("grep -rn slot_keeper src", "", &format!("{s}a"))]),
            true,
        );
        handle(
            &ctx,
            "batch",
            &sess(vec![
                json!({"tool_name": "Read", "tool_input": {"file_path": far}, "tool_response": {}, "tool_use_id": format!("{s}b")}),
            ]),
            true,
        );
        handle(&ctx, "end", &json!({"session_id": s}), false);
    }
    let (_, id) = profile::repo_of(&repo).unwrap();
    let prof = profile::read_profile(&profile::profile_path(&ctx.root, &id));
    assert_eq!(prof.streams_folded, 2);
    let keeper = ctx.h("keeper");
    assert!(prof
        .pairs
        .values()
        .any(|p| p.terms.get(&keeper).is_some_and(|t| t.streams == 2)));
    let raw = std::fs::read_to_string(profile::profile_path(&ctx.root, &id)).unwrap();
    assert!(
        !raw.contains("keeper") && !raw.contains("far.rs"),
        "profile holds hashes only"
    );
}

#[test]
fn install_merge_is_idempotent_and_removable() {
    let bin = Path::new("/opt/xerj/bin/xerj");
    let wanted = install::groups(bin, "batch").unwrap();
    let start = json!({"hooks": {"PostToolBatch": [{"hooks": [{"type": "command", "command": "other-tool"}]}]}, "model": "x"});
    let once = install::merge(start.clone(), &wanted, false).unwrap();
    let twice = install::merge(once.clone(), &wanted, false).unwrap();
    assert_eq!(once, twice);
    assert_eq!(once["hooks"]["PostToolBatch"].as_array().unwrap().len(), 2);
    assert!(once["hooks"]["SessionEnd"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("hook run end --host claude"));
    let removed = install::merge(twice, &wanted, true).unwrap();
    assert_eq!(removed, start);
}

#[test]
fn render_respects_token_cap() {
    let t = detect::Trigger {
        rule: Rule::R2,
        calls: 3,
        zeros: 0,
        toks: vec!["lease".into()],
        idents: vec![],
        root: "/w".into(),
        seen: Default::default(),
    };
    let long = "x".repeat(400);
    let lines: Vec<render::Line> = (0..9)
        .map(|i| render::Line {
            abs: format!("/w/f{i}.rs"),
            shown: format!("f{i}.rs"),
            line: i + 1,
            text: long.clone(),
            tag: render::Tag::Terms(2, 3),
        })
        .collect();
    let out = render::render(&t, &lines, &["nope_absent".into()], None, 60);
    assert!(render::estimate_tokens(&out) <= 60, "{out}");
    let full = render::render(&t, &lines, &[], None, 300);
    assert!(full.lines().count() <= 1 + render::MAX_LINES + 1);
    assert!(!full.to_lowercase().contains("must") && !full.to_lowercase().contains("required"));
}

#[test]
fn codex_string_response_and_grep_tool_parse() {
    let ev = event_from_call(
        "Bash",
        &json!({"command": "rg -n needle_word src"}),
        &json!("src/a.rs:1:needle_word\n"),
        "/w",
        "c1",
        0.0,
    );
    assert_eq!(ev.probes[0].zero, Some(false));
    assert!(ev.shown.contains(&"/w/src/a.rs".to_string()));
    let g = event_from_call(
        "Grep",
        &json!({"pattern": "needle_word", "path": "src", "head_limit": 2}),
        &json!({"filenames": ["src/a.rs", "src/b.rs"], "numFiles": 2}),
        "/w",
        "g1",
        0.0,
    );
    assert!(g.probes[0].tree && g.probes[0].cap_hit);
    let bad = handle(
        &Ctx::new(tempfile::tempdir().unwrap().path().join("s")).unwrap(),
        "batch",
        &json!({"tool_calls": "not-an-array"}),
        true,
    );
    assert!(bad.stdout.is_none());
}
