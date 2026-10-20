//! A build with no hub signing key can verify no catalogue and no pack. A real
//! key is compiled in now, so the test asks for that state explicitly with
//! `XERJ_HUB_PUBKEY=none` (`HUB_PUBKEY_NONE`). Every hub surface must say so
//! plainly with exit 5 — not fail on whatever the hub URL serves (a 200 HTML
//! page made `xerj ask` print "parse catalogue JSON: expected value at line 1
//! column 1", exit 1) and not send an MCP agent to `xerj hub sync`, which
//! cannot help.
//!
//! One test in its own binary: it owns the process environment.

use xerj_hub::ask::{self, EXIT_NO_KEY};
use xerj_hub::cli::{run_ask, run_hub_cli};

#[test]
fn every_hub_surface_says_the_build_has_no_key() {
    std::env::set_var("XERJ_HUB_PUBKEY", xerj_common::hub::HUB_PUBKEY_NONE);
    assert!(
        !xerj_common::hub::hub_key_configured(),
        "precondition: the override takes the key away"
    );
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("XERJ_HUB_HOME", home.path());
    // A hub that serves a 200 HTML page for every path, like hub.xerj.org's
    // site fallback does for the unpublished API — the case that misled.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hub_url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for conn in listener.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let body = "<!doctype html><html><body>hub</body></html>";
            let _ = write!(
                c,
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    std::env::set_var("XERJ_HUB_URL", &hub_url);

    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();

    assert_eq!(
        run_ask(&s(&["how does lz4 frame work"])),
        EXIT_NO_KEY,
        "xerj ask"
    );
    assert_eq!(run_hub_cli(&s(&["sync"])), EXIT_NO_KEY, "xerj hub sync");
    assert_eq!(
        run_hub_cli(&s(&["pull", "lz4"])),
        EXIT_NO_KEY,
        "xerj hub pull"
    );

    // The MCP `xerj_hub_find` path: the no-key message, not "run xerj hub sync".
    let err = ask::run_find("how does lz4 frame work", 5).unwrap_err();
    assert!(err.contains("not available in this build"), "{err}");
    assert!(!err.contains("xerj hub sync"), "{err}");

    // Usage errors are still usage errors.
    assert_eq!(run_hub_cli(&s(&["sync", "--bogus"])), 2);
    assert_eq!(run_ask(&s(&["--hosted", "maybe", "q x"])), ask::EXIT_USAGE);
}
