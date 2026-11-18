//! Two Windows installers, two directories. `landing/get` (the `sh` installer,
//! including Git Bash / MSYS / Cygwin) writes `xerj.exe` to `~/.local/bin`.
//! `landing/get.ps1` writes it to `%LOCALAPPDATA%\Programs\xerj` and adds that
//! directory to the user PATH. Docs that name only the PowerShell directory
//! send a Git Bash install looking in the wrong place (#2285).

use std::path::PathBuf;

fn repo(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Pages and agent-facing files that state where the binary lands.
const INSTALL_PATH_DOCS: &[&str] = &[
    "landing/docs/install.html",
    "landing/docs/agents/quickstart.html",
    "landing/docs/cli.html",
    "landing/llms.txt",
    "README.md",
    "AGENTS.md",
];

#[test]
fn install_docs_name_both_windows_installers() {
    let ps_dir = "%LOCALAPPDATA%\\Programs\\xerj";
    for rel in INSTALL_PATH_DOCS {
        let text = repo(rel);
        assert!(
            text.contains(ps_dir),
            "{rel} must name the PowerShell install directory {ps_dir}"
        );
        assert!(
            text.contains("~/.local/bin"),
            "{rel} must name the sh installer's directory, including on Windows Git Bash"
        );
        assert!(
            text.contains("Git Bash"),
            "{rel} must say the sh installer on Git Bash uses ~/.local/bin, not only the PowerShell path"
        );
        assert!(
            text.contains("PowerShell") || text.contains("get.ps1"),
            "{rel} must name the PowerShell installer that uses {ps_dir}"
        );
    }
}

#[test]
fn shell_installer_uses_home_local_bin_including_on_windows() {
    let sh = repo("landing/get");
    assert!(
        sh.contains("MINGW*|MSYS*|CYGWIN*"),
        "the sh installer must recognise Git Bash / MSYS / Cygwin"
    );
    assert!(
        sh.contains("bin=\"xerj.exe\""),
        "Windows via the sh installer installs xerj.exe"
    );
    assert!(
        sh.contains("dir=\"$HOME/.local/bin\""),
        "the sh installer's default directory is ~/.local/bin on every OS it supports"
    );
    // Comments may name the PowerShell directory so a later edit does not
    // retarget this script. The executable lines must not.
    let code: String = sh
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("LOCALAPPDATA"),
        "the sh installer must not install to %LOCALAPPDATA%; that directory is get.ps1 only"
    );
}

#[test]
fn powershell_installer_uses_localappdata_programs() {
    let ps1 = repo("landing/get.ps1");
    assert!(
        ps1.contains("Join-Path $env:LOCALAPPDATA 'Programs\\xerj'"),
        "get.ps1 must keep its default of %LOCALAPPDATA%\\Programs\\xerj"
    );
    assert!(
        ps1.contains("XERJ_NO_PATH"),
        "get.ps1 adds the install directory to the user PATH unless XERJ_NO_PATH is set"
    );
}
