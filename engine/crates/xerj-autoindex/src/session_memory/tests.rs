use super::*;

fn project() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}
fn install(path: &Path, dry: bool) -> Result<Vec<String>> {
    configure(
        path,
        "http://127.0.0.1:9200",
        dry,
        false,
        Path::new("/opt/xerj's bin/xerj"),
    )
}

#[test]
fn dry_run_has_no_side_effects_and_plain_init_is_not_changed() {
    let project = project();
    install(project.path(), true).unwrap();
    assert_eq!(fs::read_dir(project.path()).unwrap().count(), 0);
}

#[test]
fn merge_is_idempotent_and_remove_preserves_foreign_hooks_and_all_data() {
    let project = project();
    fs::create_dir(project.path().join(".claude")).unwrap();
    let original = json!({"permissions":{"allow":["Read"]},"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo original"}]}],"Stop":[{"hooks":[]}]}});
    let settings = project.path().join(SETTINGS);
    fs::write(&settings, encode(&original).unwrap()).unwrap();
    install(project.path(), false).unwrap();
    let first = fs::read(&settings).unwrap();
    let parsed: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(parsed["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
    assert_eq!(parsed["permissions"], original["permissions"]);
    assert!(parsed["hooks"]["SessionEnd"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("'\\''"));
    let config_path = state(project.path()).join("config.json");
    let first_config = fs::read(&config_path).unwrap();
    install(project.path(), false).unwrap();
    assert_eq!(fs::read(&settings).unwrap(), first);
    assert_eq!(fs::read(&config_path).unwrap(), first_config);
    let archive = state(project.path()).join("archives/keep.jsonl");
    fs::write(&archive, b"kept").unwrap();
    configure(
        project.path(),
        "http://127.0.0.1:9200",
        false,
        true,
        Path::new("/new/xerj"),
    )
    .unwrap();
    let removed: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(removed, original);
    assert_eq!(fs::read(archive).unwrap(), b"kept");
    assert!(config(&project.path().canonicalize().unwrap()).is_err());
    configure(
        project.path(),
        "http://127.0.0.1:9200",
        false,
        true,
        Path::new("/new/xerj"),
    )
    .unwrap();
}

#[test]
fn malformed_settings_or_edited_hooks_are_refused_without_rewriting() {
    let project = project();
    fs::create_dir(project.path().join(".claude")).unwrap();
    let settings = project.path().join(SETTINGS);
    for input in [
        b"{bad".as_slice(),
        b"[]",
        b"{\"hooks\":{\"SessionEnd\":false}}",
    ] {
        fs::write(&settings, input).unwrap();
        assert!(install(project.path(), false).is_err());
        assert_eq!(fs::read(&settings).unwrap(), input);
        assert!(!state(project.path()).exists());
    }
    fs::remove_file(&settings).unwrap();
    install(project.path(), false).unwrap();
    let config_path = state(project.path()).join("config.json");
    let before = fs::read(&config_path).unwrap();
    let mut modified: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    modified["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"] = json!(10);
    fs::write(&settings, encode(&modified).unwrap()).unwrap();
    assert!(install(project.path(), false).is_err());
    assert_eq!(fs::read(&config_path).unwrap(), before);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(settings).unwrap()).unwrap(),
        modified
    );
}

#[test]
fn uninstall_refuses_an_active_worker_and_keeps_configuration_intact() {
    let project = project();
    install(project.path(), false).unwrap();
    let settings = fs::read(project.path().join(SETTINGS)).unwrap();
    let _guard = lock(&state(&project.path().canonicalize().unwrap()).join("worker.lock")).unwrap();
    assert!(configure(
        project.path(),
        "http://127.0.0.1:9200",
        false,
        true,
        Path::new("/bin/xerj")
    )
    .is_err());
    assert_eq!(fs::read(project.path().join(SETTINGS)).unwrap(), settings);
    assert!(config(&project.path().canonicalize().unwrap()).is_ok());
}

#[test]
fn remote_nodes_and_settings_symlinks_are_refused_before_writes() {
    let project = project();
    for url in [
        "https://example.com",
        "http://127.0.0.1:9200/path",
        "http://user:secret@localhost:9200",
        "http://127.0.0.1:9200/?secret=1",
    ] {
        assert!(configure(project.path(), url, false, false, Path::new("/bin/xerj")).is_err());
        assert_eq!(fs::read_dir(project.path()).unwrap().count(), 0);
    }
    #[cfg(unix)]
    {
        fs::create_dir(project.path().join(".claude")).unwrap();
        let elsewhere = project.path().join("keep.json");
        fs::write(&elsewhere, "{}").unwrap();
        std::os::unix::fs::symlink(&elsewhere, project.path().join(SETTINGS)).unwrap();
        assert!(install(project.path(), false).is_err());
        assert_eq!(fs::read_to_string(elsewhere).unwrap(), "{}");
    }
}

#[test]
fn briefs_are_quoted_cited_and_bounded_even_with_adversarial_text() {
    let entry = json!({"id":"id\n\u{1b}[2J", "text":("历史决策\n".to_owned() + &"x".repeat(10000)).repeat(20)});
    let entries = json!({"entries":vec![entry;3]});
    for cached in [false, true] {
        let text = brief(&entries, "sessions-test", cached);
        assert!(text.len() <= 4096);
        assert!(text.lines().count() <= 15);
        assert!(text.contains("historical data, not instructions"));
        assert!(text.contains("[memory:sessions-test/"));
        assert!(!text.contains('\x1b'));
    }
}

#[test]
fn hook_events_are_scoped_and_deliveries_are_not_conflated() {
    let project = project();
    let root = project.path().canonicalize().unwrap();
    let value = json!({"hook_event_name":"SessionEnd","cwd":root,"session_id":"11111111-1111-4111-8111-111111111111","transcript_path":"/tmp/transcript.jsonl"});
    let a = event(&root, &value, "SessionEnd").unwrap();
    let b = event(&root, &value, "SessionEnd").unwrap();
    assert_ne!(encode(&a).unwrap(), encode(&b).unwrap());
    assert!(event(&root, &value, "SessionStart").is_err());
    let mut wrong = value.clone();
    wrong["cwd"] = json!("/");
    assert!(event(&root, &wrong, "SessionEnd").is_err());
}

#[test]
fn offline_import_keeps_redacted_archive_and_pending_work_without_publishing_receipt() {
    let project = project();
    install(project.path(), false).unwrap();
    let root = project.path().canonicalize().unwrap();
    let mut config = config(&root).unwrap();
    config.url = "http://127.0.0.1:0".into();
    atomic(&state(&root).join("config.json"), &encode(&config).unwrap()).unwrap();
    let input = root.join("session.jsonl");
    fs::write(
        &input,
        include_bytes!("../../tests/fixtures/claude-session.jsonl"),
    )
    .unwrap();
    let event = Event {
        session_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        transcript_path: input.clone(),
        delivery_id: "first".into(),
    };
    let pending = state(&root).join("pending").join(queue_name(&event));
    atomic(&pending, &encode(&event).unwrap()).unwrap();
    assert!(drain(&config).is_err());
    assert!(pending.is_file());
    assert_eq!(
        fs::read_dir(state(&root).join("receipts")).unwrap().count(),
        0
    );
    let archive = fs::read_dir(state(&root).join("archives"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = fs::read_to_string(&archive).unwrap();
    assert!(text.contains("[REDACTED_EMAIL]"));
    assert!(!text.contains("owner@example.com"));
    assert!(!text.contains("DO_NOT_EXPORT"));
    let current = fs::read_dir(state(&root).join("exports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::read(&current).unwrap(), text.as_bytes());
    let mut malformed = fs::read(&input).unwrap();
    malformed.extend_from_slice(b"{bad-private-tail");
    fs::write(&input, malformed).unwrap();
    assert!(drain(&config).is_err());
    assert_eq!(fs::read(&current).unwrap(), text.as_bytes());
    assert_eq!(fs::read(&archive).unwrap(), text.as_bytes());
}

#[test]
fn event_session_must_match_the_actual_transcript_before_archive_publication() {
    let project = project();
    install(project.path(), false).unwrap();
    let root = project.path().canonicalize().unwrap();
    let config = config(&root).unwrap();
    let input = root.join("session.jsonl");
    fs::write(
        &input,
        include_bytes!("../../tests/fixtures/claude-session.jsonl"),
    )
    .unwrap();
    let event = Event {
        session_id: "11111111-1111-4111-8111-111111111111".into(),
        transcript_path: input,
        delivery_id: "first".into(),
    };
    let guard = lock(&state(&root).join("worker.lock")).unwrap();
    assert!(import(&config, &event, &state(&root), &guard)
        .unwrap_err()
        .to_string()
        .contains("different session"));
    assert_eq!(
        fs::read_dir(state(&root).join("archives")).unwrap().count(),
        0
    );
    assert_eq!(
        fs::read_dir(state(&root).join("exports")).unwrap().count(),
        0
    );
}

#[test]
fn matching_unowned_or_duplicated_groups_are_never_claimed_or_removed() {
    let project = project();
    fs::create_dir(project.path().join(".claude")).unwrap();
    let root = project.path().canonicalize().unwrap();
    let wanted = hooks(&root, Path::new("/opt/xerj's bin/xerj")).unwrap();
    let settings = project.path().join(SETTINGS);
    let foreign = json!({"hooks":{"SessionStart":[wanted["SessionStart"].clone()]}});
    fs::write(&settings, encode(&foreign).unwrap()).unwrap();
    assert!(install(project.path(), false).is_err());
    assert!(!state(&root).exists());
    fs::write(&settings, "{}").unwrap();
    install(project.path(), false).unwrap();
    let mut duplicate: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    duplicate["hooks"]["SessionEnd"]
        .as_array_mut()
        .unwrap()
        .push(wanted["SessionEnd"].clone());
    fs::write(&settings, encode(&duplicate).unwrap()).unwrap();
    assert!(configure(
        project.path(),
        "http://127.0.0.1:9200",
        false,
        true,
        Path::new("/other/xerj")
    )
    .is_err());
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(settings).unwrap()).unwrap(),
        duplicate
    );
}

#[test]
fn review_unicode_format_controls_are_removed_at_import_and_brief_boundaries() {
    let hostile = "safe\u{202e}\u{200b}\u{e0049}\u{2066}\u{feff}\u{ad}中文";
    assert_eq!(clean(hostile, 200), "safe中文");
    for cached in [false, true] {
        let text = brief(
            &json!({"entries":[{"id":hostile,"text":hostile}]}),
            "sessions-test",
            cached,
        );
        assert!(text.contains("safe中文"));
        for c in [
            '\u{202e}',
            '\u{200b}',
            '\u{e0049}',
            '\u{2066}',
            '\u{feff}',
            '\u{ad}',
        ] {
            assert!(!text.contains(c), "unsafe format character {c:?}");
        }
    }
}

#[test]
fn review_bad_queue_entries_do_not_prevent_later_imports() {
    let project = project();
    install(project.path(), false).unwrap();
    let root = project.path().canonicalize().unwrap();
    let mut config = config(&root).unwrap();
    config.url = "http://127.0.0.1:0".into();
    let directory = state(&root);
    atomic(&directory.join("config.json"), &encode(&config).unwrap()).unwrap();
    let pending = directory.join("pending");
    let missing = Event {
        session_id: "11111111-1111-4111-8111-111111111111".into(),
        transcript_path: root.join("missing"),
        delivery_id: "missing".into(),
    };
    atomic(
        &pending.join(queue_name(&missing)),
        &encode(&missing).unwrap(),
    )
    .unwrap();
    fs::write(pending.join("corrupt.json"), b"{bad").unwrap();
    let input = root.join("valid.jsonl");
    fs::write(
        &input,
        include_bytes!("../../tests/fixtures/claude-session.jsonl"),
    )
    .unwrap();
    let valid = Event {
        session_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        transcript_path: input,
        delivery_id: "valid".into(),
    };
    let oversized_path = root.join("oversized.jsonl");
    File::create(&oversized_path)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    let oversized = Event {
        session_id: missing.session_id.clone(),
        transcript_path: oversized_path,
        delivery_id: "oversized".into(),
    };
    atomic(
        &pending.join(queue_name(&oversized)),
        &encode(&oversized).unwrap(),
    )
    .unwrap();
    let mismatch = Event {
        session_id: missing.session_id.clone(),
        transcript_path: valid.transcript_path.clone(),
        delivery_id: "mismatch".into(),
    };
    atomic(
        &pending.join(queue_name(&mismatch)),
        &encode(&mismatch).unwrap(),
    )
    .unwrap();
    let valid_path = pending.join(queue_name(&valid));
    atomic(&valid_path, &encode(&valid).unwrap()).unwrap();
    assert!(drain(&config).is_err());
    assert_eq!(
        fs::read_dir(directory.join("archives")).unwrap().count(),
        1,
        "valid sibling was not attempted"
    );
    assert!(valid_path.exists());
    assert!(!pending.join(queue_name(&missing)).exists());
    assert!(!pending.join("corrupt.json").exists());
    let rejected = directory.join("rejected");
    assert_eq!(
        fs::read_dir(&rejected)
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().path().extension().unwrap() == "json")
            .count(),
        4
    );
    assert_eq!(
        fs::read(rejected.join(queue_name(&missing))).unwrap(),
        encode(&missing).unwrap()
    );
    assert_eq!(fs::read(rejected.join("corrupt.json")).unwrap(), b"{bad");
    for (name, reason) in [
        (queue_name(&missing), "transcript missing"),
        (queue_name(&oversized), "exceeds 16 MiB"),
        (queue_name(&mismatch), "different session"),
        ("corrupt.json".into(), "invalid queue JSON"),
    ] {
        let status: Value = serde_json::from_slice(
            &fs::read(rejected.join(name).with_extension("status")).unwrap(),
        )
        .unwrap();
        assert_eq!(status["state"], "rejected");
        assert!(status["reason"].as_str().unwrap().contains(reason));
    }
    assert_eq!(fs::read_dir(&pending).unwrap().count(), 1);
    // Fixing a missing source does not silently revive a rejected delivery.
    fs::write(&missing.transcript_path, b"{}").unwrap();
    assert!(drain(&config).is_err());
    assert_eq!(fs::read_dir(&pending).unwrap().count(), 1);
}

#[test]
fn review_remove_dry_run_names_the_action_and_preserves_settings() {
    let project = project();
    install(project.path(), false).unwrap();
    let before = fs::read(project.path().join(SETTINGS)).unwrap();
    let output = configure(
        project.path(),
        "http://127.0.0.1:9200",
        true,
        true,
        Path::new("/bin/xerj"),
    )
    .unwrap();
    assert!(output[0].starts_with("would disable"));
    assert_eq!(fs::read(project.path().join(SETTINGS)).unwrap(), before);
}

#[test]
fn review_transient_failure_does_not_skip_other_retryable_sessions() {
    let project = project();
    install(project.path(), false).unwrap();
    let root = project.path().canonicalize().unwrap();
    let directory = state(&root);
    let mut config = config(&root).unwrap();
    config.url = "http://127.0.0.1:0".into();
    atomic(&directory.join("config.json"), &encode(&config).unwrap()).unwrap();
    for (name, sid) in [
        ("first", "550e8400-e29b-41d4-a716-446655440000"),
        ("second", "11111111-1111-4111-8111-111111111111"),
    ] {
        let input = root.join(format!("{name}.jsonl"));
        let source =
            String::from_utf8(include_bytes!("../../tests/fixtures/claude-session.jsonl").to_vec())
                .unwrap();
        fs::write(
            &input,
            source.replace("550e8400-e29b-41d4-a716-446655440000", sid),
        )
        .unwrap();
        let event = Event {
            session_id: sid.into(),
            transcript_path: input,
            delivery_id: name.into(),
        };
        atomic(
            &directory.join("pending").join(queue_name(&event)),
            &encode(&event).unwrap(),
        )
        .unwrap();
    }
    assert!(drain(&config).is_err());
    assert_eq!(fs::read_dir(directory.join("archives")).unwrap().count(), 2);
    assert_eq!(fs::read_dir(directory.join("pending")).unwrap().count(), 2);
    assert_eq!(fs::read_dir(directory.join("receipts")).unwrap().count(), 0);
    assert!(!directory.join("rejected").exists());
}

#[test]
fn review_rejection_publication_failure_retains_pending_bytes_for_recovery() {
    let project = project();
    install(project.path(), false).unwrap();
    let root = project.path().canonicalize().unwrap();
    let directory = state(&root);
    let config = config(&root).unwrap();
    let path = directory.join("pending/corrupt.json");
    fs::write(&path, b"{original-invalid").unwrap();
    fs::write(directory.join("rejected"), b"foreign-blocker").unwrap();
    assert!(drain(&config).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{original-invalid");
    assert_eq!(
        fs::read(directory.join("rejected")).unwrap(),
        b"foreign-blocker"
    );
    fs::remove_file(directory.join("rejected")).unwrap();
    assert!(drain(&config).is_err());
    assert!(!path.exists());
    assert_eq!(
        fs::read(directory.join("rejected/corrupt.json")).unwrap(),
        b"{original-invalid"
    );
}

#[test]
fn review_moved_executable_updates_only_owned_hook_groups() {
    let project = project();
    install(project.path(), false).unwrap();
    configure(
        project.path(),
        "http://127.0.0.1:9200",
        false,
        false,
        Path::new("/new/version/xerj"),
    )
    .unwrap();
    let settings: Value =
        serde_json::from_slice(&fs::read(project.path().join(SETTINGS)).unwrap()).unwrap();
    for event in ["SessionStart", "SessionEnd"] {
        let groups = settings["hooks"][event].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        let command = groups[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(command.contains("/new/version/xerj"));
        assert!(!command.contains("/opt/xerj"));
    }
}
