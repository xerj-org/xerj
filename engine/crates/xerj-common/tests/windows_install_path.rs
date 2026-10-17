//! `get.ps1` installs to `%LOCALAPPDATA%\Programs\xerj`. The install page
//! used to state the only default as `~/.local/bin`, which is the Unix path.

use std::path::PathBuf;

fn repo(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn install_docs_name_the_windows_default_directory() {
    let needle = "%LOCALAPPDATA%\\Programs\\xerj";
    for rel in [
        "landing/docs/install.html",
        "landing/docs/agents/quickstart.html",
        "landing/docs/cli.html",
    ] {
        let text = repo(rel);
        assert!(
            text.contains(needle),
            "{rel} must name the Windows install directory {needle}"
        );
        assert!(
            text.contains("~/.local/bin"),
            "{rel} must keep the Linux and macOS default"
        );
    }
}
