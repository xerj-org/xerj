use super::*;
fn project() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}
fn install(p: &Path, dry: bool, remove: bool) -> Result<Vec<String>> {
    configure(
        Host::Claude,
        p,
        "http://127.0.0.1:9200",
        dry,
        remove,
        Path::new("/opt/xerj's bin/xerj"),
    )
}
fn event(p: &Path, sid: &str, tool: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"hook_event_name":"PreToolUse","cwd":p.canonicalize().unwrap(), "session_id":sid,"tool_name":tool,"tool_input":{"pattern":"do not send or persist"}})).unwrap()
}
fn fire(p: &Path, sid: &str, tool: &str) -> Option<Value> {
    handle_with(Host::Claude, p, &event(p, sid, tool), |_| {
        Ok(Some("my-repo".into()))
    })
    .unwrap()
}
#[test]
fn dry_run_merge_reconfigure_remove_and_memory_coexistence() {
    let p = project();
    install(p.path(), true, false).unwrap();
    assert_eq!(fs::read_dir(p.path()).unwrap().count(), 0);
    fs::create_dir(p.path().join(".claude")).unwrap();
    let original = json!({"permissions":{"allow":["Read"]},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[]}],"Stop":[{"hooks":[]}]}});
    let settings = p.path().join(SETTINGS);
    fs::write(&settings, encode(&original).unwrap()).unwrap();
    crate::session_memory::configure(
        p.path(),
        "http://127.0.0.1:9200",
        false,
        false,
        Path::new("/bin/xerj"),
    )
    .unwrap();
    let memory = fs::read(p.path().join(".claude/settings.local.json")).unwrap();
    install(p.path(), false, false).unwrap();
    let first = fs::read(&settings).unwrap();
    install(p.path(), false, false).unwrap();
    assert_eq!(first, fs::read(&settings).unwrap());
    let groups: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(groups["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
    let cmd = groups["hooks"]["PreToolUse"][1]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(cmd.contains("'\\''"));
    configure(
        Host::Claude,
        p.path(),
        "http://127.0.0.1:9201",
        false,
        false,
        Path::new("/new/bin/xerj"),
    )
    .unwrap();
    let moved: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert!(moved["hooks"]["PreToolUse"][1]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("/new/bin/xerj"));
    let backups = fs::read_dir(p.path().join(DIRECTORY))
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension().is_some_and(|s| s == "bak")).then_some(p)
        })
        .collect::<Vec<_>>();
    assert!(backups
        .iter()
        .any(|b| fs::read(b).unwrap() == encode(&original).unwrap()));
    install(p.path(), false, true).unwrap();
    install(p.path(), false, true).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&settings).unwrap()).unwrap(),
        original
    );
    assert_eq!(
        fs::read(p.path().join(".claude/settings.local.json")).unwrap(),
        memory
    );
    assert!(fire_after_removal(p.path()).is_err());
}
fn fire_after_removal(p: &Path) -> Result<Option<Value>> {
    handle_with(Host::Claude, p, &event(p, "x", "Grep"), |_| {
        panic!("disabled hook contacted node")
    })
}
#[test]
fn malformed_edited_duplicated_and_foreign_configuration_refuse_without_overwrite() {
    for bad in [
        json!([]),
        json!({"hooks":false}),
        json!({"hooks":{"PreToolUse":false}}),
    ] {
        let p = project();
        fs::create_dir(p.path().join(".claude")).unwrap();
        let path = p.path().join(SETTINGS);
        let bytes = encode(&bad).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(install(p.path(), false, false).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert!(!p.path().join(DIRECTORY).exists());
    }
    for duplicated in [false, true] {
        let p = project();
        install(p.path(), false, false).unwrap();
        let path = p.path().join(SETTINGS);
        let mut settings: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if duplicated {
            let group = settings["hooks"]["PreToolUse"][0].clone();
            settings["hooks"]["PreToolUse"]
                .as_array_mut()
                .unwrap()
                .push(group);
        } else {
            settings["hooks"]["PreToolUse"][0]["matcher"] = json!("Bash");
        }
        let bytes = encode(&settings).unwrap();
        fs::write(&path, &bytes).unwrap();
        let config = fs::read(p.path().join(DIRECTORY).join("config.json")).unwrap();
        assert!(install(p.path(), false, true).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            fs::read(p.path().join(DIRECTORY).join("config.json")).unwrap(),
            config
        );
        // Restore exactly the owned group, then the documented removal succeeds.
        let owned: Config = serde_json::from_slice(&config).unwrap();
        settings["hooks"]["PreToolUse"] = json!([owned.group]);
        fs::write(&path, encode(&settings).unwrap()).unwrap();
        install(p.path(), false, true).unwrap();
    }
    let p = project();
    fs::create_dir(p.path().join(".claude")).unwrap();
    let wanted = group(
        Host::Claude,
        &p.path().canonicalize().unwrap(),
        Path::new("/opt/xerj's bin/xerj"),
    )
    .unwrap();
    fs::write(
        p.path().join(SETTINGS),
        encode(&json!({"hooks":{"PreToolUse":[wanted]}})).unwrap(),
    )
    .unwrap();
    assert!(install(p.path(), false, false).is_err());
}
#[test]
fn rate_limit_and_read_threshold_survive_process_reloads_and_isolate_sessions() {
    let p = project();
    install(p.path(), false, false).unwrap();
    assert!(fire(p.path(), "a", "Read").is_none());
    assert!(fire(p.path(), "a", "Read").is_none());
    let output = fire(p.path(), "a", "Read").unwrap();
    let specific = &output["hookSpecificOutput"];
    assert_eq!(specific.as_object().unwrap().len(), 2);
    assert_eq!(specific["hookEventName"], "PreToolUse");
    assert!(specific["additionalContext"]
        .as_str()
        .unwrap()
        .contains("--prefix my-repo"));
    assert!(specific["additionalContext"]
        .as_str()
        .unwrap()
        .contains("--url 'http://127.0.0.1:9200'"));
    for _ in 0..9 {
        assert!(fire(p.path(), "a", "Grep").is_none());
    }
    assert!(fire(p.path(), "a", "Glob").is_some());
    assert!(fire(p.path(), "b", "Grep").is_some());
    let counters = fs::read_to_string(p.path().join(DIRECTORY).join("counters.json")).unwrap();
    assert!(!counters.contains("do not send or persist"));
    let parsed: Counters = serde_json::from_str(&counters).unwrap();
    assert_eq!(parsed.sessions.len(), 2);
    assert_eq!(parsed.sessions[0].calls, 13);
}
#[test]
fn bounded_sessions_and_counters_busy_or_corrupt_abstain() {
    let mut counters = Counters::default();
    for i in 0..1000 {
        counters.visit(&digest(i.to_string().as_bytes()), "Grep");
    }
    assert_eq!(counters.sessions.len(), MAX_SESSIONS);
    let p = project();
    install(p.path(), false, false).unwrap();
    let dir = p.path().join(DIRECTORY);
    let lease = lock(&dir.join("state.lock")).unwrap();
    assert!(handle_with(
        Host::Claude,
        p.path(),
        &event(p.path(), "x", "Grep"),
        |_| panic!("busy probe")
    )
    .is_err());
    drop(lease);
    fs::write(dir.join("counters.json"), b"{bad").unwrap();
    assert!(handle_with(
        Host::Claude,
        p.path(),
        &event(p.path(), "x", "Grep"),
        |_| panic!("corrupt probe")
    )
    .is_err());
}
#[test]
fn offline_and_unindexed_calls_are_silent_and_do_not_consume_hint_slot() {
    let p = project();
    install(p.path(), false, false).unwrap();
    let e = event(p.path(), "x", "Grep");
    assert!(
        handle_with(Host::Claude, p.path(), &e, |_| anyhow::bail!("offline"))
            .unwrap()
            .is_none()
    );
    assert!(handle_with(Host::Claude, p.path(), &e, |_| Ok(None))
        .unwrap()
        .is_none());
    assert!(fire(p.path(), "x", "Grep").is_some());
    for bad in [
        b"invalid".as_slice(),
        b"{}",
        &vec![b'x'; LIMIT as usize + 1],
    ] {
        assert!(handle_with(Host::Claude, p.path(), bad, |_| panic!("invalid probe")).is_err());
    }
    let mut e: Value = serde_json::from_slice(&e).unwrap();
    for (field, value) in [
        ("tool_name", json!("Bash")),
        ("hook_event_name", json!("PostToolUse")),
        ("cwd", json!("/")),
        ("session_id", json!("")),
    ] {
        let old = e[field].clone();
        e[field] = value;
        assert!(handle_with(
            Host::Claude,
            p.path(),
            &serde_json::to_vec(&e).unwrap(),
            |_| panic!("mismatched probe")
        )
        .is_err());
        e[field] = old;
    }
}
fn catalog(runs: Vec<Value>) -> Value {
    json!({"hits":{"total":{"value":runs.len(),"relation":"eq"},"hits":runs.into_iter().map(|s|json!({"_source":s})).collect::<Vec<_>>()}})
}
fn run(root: &str, prefix: &str, started: &str) -> Value {
    json!({"doc_kind":"run","root":root,"prefix":prefix,"started":started,"records_total":3})
}
#[test]
fn catalog_uses_latest_root_ownership_and_refuses_incomplete_or_injected_metadata() {
    let root = Path::new("/work");
    let a = run("/work", "project", "2026-01-01");
    assert_eq!(
        indexed_prefix(&catalog(vec![a.clone()]), root).as_deref(),
        Some("project")
    );
    assert!(indexed_prefix(
        &catalog(vec![run("/elsewhere", "project", "2026-01-01")]),
        root
    )
    .is_none());
    assert!(indexed_prefix(
        &catalog(vec![a.clone(), run("/elsewhere", "project", "2026-01-02")]),
        root
    )
    .is_none());
    assert!(indexed_prefix(
        &catalog(vec![a.clone(), run("/elsewhere", "project", "2026-01-01")]),
        root
    )
    .is_none());
    for prefix in ["bad;echo", "bad\n", "$(echo)", "*", "UPPER"] {
        assert!(indexed_prefix(&catalog(vec![run("/work", prefix, "2026-01-01")]), root).is_none());
    }
    let mut empty = run("/work", "project", "2026-01-02");
    empty["records_total"] = json!(0);
    assert!(indexed_prefix(&catalog(vec![a.clone(), empty]), root).is_none());
    for (pointer, value) in [
        ("/hits/total/value", json!(201)),
        ("/hits/total/relation", json!("gte")),
        ("/timed_out", json!(true)),
    ] {
        let mut v = catalog(vec![a.clone()]);
        if pointer == "/timed_out" {
            v["timed_out"] = value;
        } else {
            *v.pointer_mut(pointer).unwrap() = value;
        }
        assert!(indexed_prefix(&v, root).is_none());
    }
}
#[test]
fn remote_urls_and_symlinks_refuse_before_settings_changes() {
    let p = project();
    for url in [
        "https://example.com",
        "http://127.0.0.1:9200/path",
        "http://user:secret@127.0.0.1:9200",
        "http://127.0.0.1:9200/?key=x",
    ] {
        assert!(configure(
            Host::Claude,
            p.path(),
            url,
            false,
            false,
            Path::new("/bin/xerj")
        )
        .is_err());
        assert_eq!(fs::read_dir(p.path()).unwrap().count(), 0);
    }
    #[cfg(unix)]
    {
        fs::create_dir(p.path().join(".claude")).unwrap();
        let other = project();
        let file = other.path().join("settings");
        fs::write(&file, b"{}").unwrap();
        std::os::unix::fs::symlink(&file, p.path().join(SETTINGS)).unwrap();
        assert!(install(p.path(), false, false).is_err());
        assert_eq!(fs::read(file).unwrap(), b"{}");
    }
}

#[test]
fn http_probe_bounds_catalog_and_never_follows_redirects_or_sends_tool_input() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    for shape in ["ok", "redirect", "oversize", "partial"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let body = match shape {
            "oversize" => "x".repeat(256 * 1024 + 1),
            "partial" => {
                json!({"hits":{"total":{"value":201,"relation":"eq"},"hits":[]}}).to_string()
            }
            _ => catalog(vec![run("/work", "fixture", "2026-01-01")]).to_string(),
        };
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 2048];
            loop {
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buffer[..n]);
                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..i]).to_lowercase();
                    let len: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= i + 4 + len {
                        break;
                    }
                }
            }
            let status = if shape == "redirect" {
                "302 Found\r\nLocation: http://127.0.0.1:1/never"
            } else {
                "200 OK"
            };
            let _ = write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            String::from_utf8(request).unwrap()
        });
        let config = Config {
            version: 1,
            enabled: true,
            project: PathBuf::from("/work"),
            url,
            group: json!({}),
        };
        let got = probe(&config);
        if shape == "ok" {
            assert_eq!(got.unwrap().as_deref(), Some("fixture"));
        } else if shape == "partial" {
            assert!(got.unwrap().is_none());
        } else {
            assert!(got.is_err(), "{shape}");
        }
        let request = task.join().unwrap();
        assert!(request.starts_with("POST /autoindex-catalog/_search HTTP/1.1"));
        let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["query"], json!({"term":{"doc_kind":"run"}}));
        assert_eq!(body["size"], 200);
        assert!(
            !request.contains("PRIVATE_QUERY")
                && !request.contains("session_id")
                && !request.contains("tool_input")
        );
    }
}

#[test]
fn failed_settings_publication_rolls_back_config_and_allows_retry() {
    let p = project();
    install(p.path(), false, false).unwrap();
    let settings = fs::read(p.path().join(SETTINGS)).unwrap();
    let config = fs::read(p.path().join(DIRECTORY).join("config.json")).unwrap();
    let error = configure_with(
        Host::Claude,
        p.path(),
        "http://127.0.0.1:9200",
        (false, true),
        Path::new("/bin/xerj"),
        |_, _| anyhow::bail!("injected publication failure"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("injected publication failure"));
    assert_eq!(fs::read(p.path().join(SETTINGS)).unwrap(), settings);
    assert_eq!(
        fs::read(p.path().join(DIRECTORY).join("config.json")).unwrap(),
        config
    );
    assert!(fire(p.path(), "after-failure", "Grep").is_some());
    install(p.path(), false, true).unwrap();
    assert!(fire_after_removal(p.path()).is_err());
}

#[test]
fn installing_into_near_limit_settings_refuses_before_creating_unreadable_state() {
    let p = project();
    fs::create_dir(p.path().join(".claude")).unwrap();
    let before = serde_json::to_vec(&json!({"padding":"x".repeat(1024*1024-100)})).unwrap();
    assert!(before.len() < 1024 * 1024);
    fs::write(p.path().join(SETTINGS), &before).unwrap();
    let error = install(p.path(), false, false).unwrap_err();
    assert!(error.to_string().contains("exceeds its read budget"));
    assert_eq!(fs::read(p.path().join(SETTINGS)).unwrap(), before);
    assert!(!p.path().join(DIRECTORY).exists());
}

fn gemini_event(p: &Path, sid: &str, tool: &str) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"hook_event_name":"AfterTool", "cwd":p.canonicalize().unwrap(),
        "session_id":sid, "tool_name":tool, "tool_input":{"pattern":"PRIVATE_INPUT"},
        "tool_response":{"llmContent":"PRIVATE_RESULT"}}),
    )
    .unwrap()
}
fn gemini_install(p: &Path, dry: bool, remove: bool) -> Result<Vec<String>> {
    configure(
        Host::Gemini,
        p,
        "http://127.0.0.1:9200",
        dry,
        remove,
        Path::new("/opt/xerj's bin/xerj"),
    )
}
#[test]
fn gemini_install_uses_after_tool_and_milliseconds_and_preserves_both_hosts() {
    let p = project();
    gemini_install(p.path(), true, false).unwrap();
    assert_eq!(fs::read_dir(p.path()).unwrap().count(), 0);
    install(p.path(), false, false).unwrap();
    let claude = fs::read(p.path().join(SETTINGS)).unwrap();
    fs::create_dir(p.path().join(".gemini")).unwrap();
    let settings = p.path().join(Host::Gemini.settings());
    let dir = p.path().join(Host::Gemini.directory());
    let original = json!({"mcpServers":{"other":{"command":"other"}},"hooks":{
        "BeforeTool":[{"matcher":".*","hooks":[]}],
        "AfterTool":[{"matcher":"read_file","hooks":[]}]}});
    let original_bytes = encode(&original).unwrap();
    fs::write(&settings, &original_bytes).unwrap();
    gemini_install(p.path(), false, false).unwrap();
    let first = fs::read(&settings).unwrap();
    gemini_install(p.path(), false, false).unwrap();
    assert_eq!(first, fs::read(&settings).unwrap());
    let value: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(
        value["hooks"]["BeforeTool"],
        original["hooks"]["BeforeTool"]
    );
    let group = &value["hooks"]["AfterTool"][1];
    assert_eq!(group["hooks"][0]["timeout"], 2000);
    let matcher = regex::Regex::new(group["matcher"].as_str().unwrap()).unwrap();
    for name in ["grep_search", "search_file_content", "glob", "read_file"] {
        assert!(matcher.is_match(name));
    }
    for name in [
        "mcp_other_grep_search",
        "read_file_extra",
        "read_many_files",
        "run_shell_command",
    ] {
        assert!(!matcher.is_match(name));
    }
    let cmd = group["hooks"][0]["command"].as_str().unwrap();
    assert!(cmd.ends_with(" --host gemini") && cmd.contains("'\\''"));
    assert_eq!(fs::read(p.path().join(SETTINGS)).unwrap(), claude);
    assert!(fs::read_dir(&dir)
        .unwrap()
        .any(|f| fs::read(f.unwrap().path()).ok().as_deref() == Some(original_bytes.as_slice())));
    configure(
        Host::Gemini,
        p.path(),
        "http://127.0.0.1:9201",
        false,
        false,
        Path::new("/new/xerj"),
    )
    .unwrap();
    let updated: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(updated["hooks"]["AfterTool"].as_array().unwrap().len(), 2);
    assert!(updated["hooks"]["AfterTool"][1]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("/new/xerj"));
    gemini_install(p.path(), false, true).unwrap();
    gemini_install(p.path(), false, true).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&settings).unwrap()).unwrap(),
        original
    );
    assert_eq!(fs::read(p.path().join(SETTINGS)).unwrap(), claude);
    assert!(fire(p.path(), "still-enabled", "Grep").is_some());
    assert!(handle_with(
        Host::Gemini,
        p.path(),
        &gemini_event(p.path(), "disabled", "glob"),
        |_| panic!("disabled probe")
    )
    .is_err());
}

#[test]
fn gemini_advisory_quota_and_read_threshold_are_host_and_session_local() {
    let p = project();
    install(p.path(), false, false).unwrap();
    gemini_install(p.path(), false, false).unwrap();
    let fire_gemini = |sid, tool| {
        handle_with(
            Host::Gemini,
            p.path(),
            &gemini_event(p.path(), sid, tool),
            |_| Ok(Some("gemini-repo".into())),
        )
        .unwrap()
    };
    assert!(fire_gemini("same-session", "read_file").is_none());
    assert!(fire_gemini("same-session", "read_file").is_none());
    let value = fire_gemini("same-session", "read_file").unwrap();
    assert_eq!(value.as_object().unwrap().len(), 1);
    assert_eq!(value["hookSpecificOutput"].as_object().unwrap().len(), 2);
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "AfterTool");
    let hint = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(hint.contains("--prefix gemini-repo --url 'http://127.0.0.1:9200'"));
    for _ in 0..9 {
        assert!(fire_gemini("same-session", "grep_search").is_none());
    }
    assert!(fire_gemini("same-session", "glob").is_some());
    assert!(fire(p.path(), "same-session", "Grep").is_some());
    assert!(fire_gemini("other-session", "search_file_content").is_some());
    let e = gemini_event(p.path(), "offline", "grep_search");
    assert!(
        handle_with(Host::Gemini, p.path(), &e, |_| anyhow::bail!("offline"))
            .unwrap()
            .is_none()
    );
    assert!(fire_gemini("offline", "grep_search").is_some());
    let dir = p.path().join(Host::Gemini.directory());
    let counters = fs::read_to_string(dir.join("counters.json")).unwrap();
    for private in ["PRIVATE_INPUT", "PRIVATE_RESULT", "same-session"] {
        assert!(!counters.contains(private));
    }
    let before = counters.into_bytes();
    for (field, v) in [
        ("tool_name", json!("Grep")),
        ("tool_name", json!("mcp_other_grep_search")),
        ("hook_event_name", json!("BeforeTool")),
        ("hook_event_name", json!("PreToolUse")),
        ("cwd", json!("/")),
    ] {
        let mut bad: Value = serde_json::from_slice(&e).unwrap();
        bad[field] = v;
        assert!(
            handle_with(Host::Gemini, p.path(), &encode(&bad).unwrap(), |_| panic!(
                "wrong event probe"
            ))
            .is_err()
        );
        assert_eq!(fs::read(dir.join("counters.json")).unwrap(), before);
    }
    let lease = lock(&dir.join("state.lock")).unwrap();
    assert!(handle_with(Host::Gemini, p.path(), &e, |_| panic!("busy probe")).is_err());
    drop(lease);
    fs::write(dir.join("counters.json"), b"{bad").unwrap();
    assert!(handle_with(Host::Gemini, p.path(), &e, |_| panic!("corrupt probe")).is_err());
}

#[test]
fn gemini_refusals_preserve_settings_and_failed_publication_is_recoverable() {
    for bad in [
        json!([]),
        json!({"hooks":false}),
        json!({"hooks":{"AfterTool":false}}),
        json!({"hooksConfig":{"enabled":false}}),
    ] {
        let p = project();
        fs::create_dir(p.path().join(".gemini")).unwrap();
        let settings = p.path().join(Host::Gemini.settings());
        let bytes = encode(&bad).unwrap();
        fs::write(&settings, &bytes).unwrap();
        assert!(gemini_install(p.path(), false, false).is_err());
        assert_eq!(fs::read(settings).unwrap(), bytes);
        assert!(!p.path().join(Host::Gemini.directory()).exists());
    }
    let p = project();
    gemini_install(p.path(), false, false).unwrap();
    let settings = p.path().join(Host::Gemini.settings());
    let config_path = p.path().join(Host::Gemini.directory()).join("config.json");
    let config = fs::read(&config_path).unwrap();
    let first = fs::read(&settings).unwrap();
    let error = configure_with(
        Host::Gemini,
        p.path(),
        "http://127.0.0.1:9200",
        (false, true),
        Path::new("/bin/xerj"),
        |_, _| anyhow::bail!("injected failure"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("injected failure"));
    assert_eq!(fs::read(&config_path).unwrap(), config);
    assert_eq!(fs::read(&settings).unwrap(), first);
    for duplicate in [false, true] {
        let mut changed: Value = serde_json::from_slice(&first).unwrap();
        if duplicate {
            let g = changed["hooks"]["AfterTool"][0].clone();
            changed["hooks"]["AfterTool"]
                .as_array_mut()
                .unwrap()
                .push(g);
        } else {
            changed["hooks"]["AfterTool"][0]["matcher"] = json!(".*");
        }
        let changed = encode(&changed).unwrap();
        fs::write(&settings, &changed).unwrap();
        assert!(gemini_install(p.path(), false, true).is_err());
        assert_eq!(fs::read(&settings).unwrap(), changed);
        assert_eq!(fs::read(&config_path).unwrap(), config);
    }
    fs::write(&settings, first).unwrap();
    gemini_install(p.path(), false, true).unwrap();
}
