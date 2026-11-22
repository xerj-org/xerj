//! #3379: re-running an installer while an MCP client keeps `xerj mcp`
//! running. Windows refuses to overwrite a running `xerj.exe` but lets it be
//! renamed. Both installers now skip the replace when the installed file is
//! already the verified binary, and otherwise rename the running file aside
//! before moving the new one in.
//!
//! The `landing/get` cases run the real install block of the script under
//! `sh`, with `mv` replaced by a function that refuses to overwrite a file
//! marked as running (what Windows does). `get.ps1` has no PowerShell on the
//! Linux gate, so its checks read the script text.

use std::path::{Path, PathBuf};

fn repo(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The lines of `landing/get` from `dest=` up to the next-steps banner.
fn install_block() -> String {
    let sh = repo("landing/get");
    let start = sh
        .find("dest=\"${dir}/${bin}\"")
        .expect("landing/get sets dest");
    let end = sh[start..]
        .find("# --- next steps")
        .map(|n| start + n)
        .expect("landing/get has a next-steps section after the install");
    sh[start..end].to_string()
}

struct Run {
    status: i32,
    out: String,
}

/// Run the install block with `bin=xerj.exe` in `dir`, installing `src`.
/// A file named `<dest>.running` makes `mv` refuse to replace `dest`.
#[cfg(unix)]
fn run_install(dir: &Path, src: &Path) -> Run {
    let script = format!(
        r#"set -eu
BOLD=''; RST=''
say() {{ printf '%s\n' "$*"; }}
ok() {{ printf 'ok %s\n' "$*"; }}
die() {{ printf 'die %s\n' "$*"; exit 1; }}
mv() {{
  last=''
  for a in "$@"; do last="$a"; done
  if [ -e "$last.running" ] && [ -e "$last" ]; then
    echo "mv: cannot move to '$last': Permission denied" >&2
    return 1
  fi
  command mv "$@"
}}
dir='{dir}'
bin=xerj.exe
src='{src}'
tag=v9.9.9
{block}
echo END
"#,
        dir = dir.display(),
        src = src.display(),
        block = install_block(),
    );
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run sh");
    Run {
        status: output.status.code().unwrap_or(-1),
        out: format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

#[cfg(unix)]
#[test]
fn sh_installer_replaces_a_running_exe_by_renaming_it_aside() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("xerj.exe");
    std::fs::write(&dest, b"old build").unwrap();
    std::fs::write(dir.join("xerj.exe.running"), b"").unwrap();
    let src = tmp.path().join("new-xerj.exe");
    std::fs::write(&src, b"new build").unwrap();

    let run = run_install(&dir, &src);
    assert_eq!(run.status, 0, "install must succeed:\n{}", run.out);
    assert!(run.out.contains("END"), "{}", run.out);
    assert_eq!(std::fs::read(&dest).unwrap(), b"new build");
    assert!(
        run.out.contains("restart your MCP client"),
        "the user is told the running copy keeps the old version:\n{}",
        run.out
    );
    let asides: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("xerj.exe.old-"))
        .collect();
    assert_eq!(asides.len(), 1, "the old file is kept aside: {asides:?}");
    assert_eq!(
        std::fs::read(dir.join(&asides[0])).unwrap(),
        b"old build",
        "the aside file is the running build"
    );
    assert!(
        !std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".tmp.")),
        "no staging file is left behind"
    );

    // The next run (the old process has exited) removes the aside file and
    // sees the installed file is already the verified binary.
    std::fs::remove_file(dir.join("xerj.exe.running")).unwrap();
    let again = run_install(&dir, &src);
    assert_eq!(again.status, 0, "{}", again.out);
    assert!(
        again.out.contains("already up to date"),
        "a re-run with the same binary replaces nothing:\n{}",
        again.out
    );
    assert!(
        !dir.join(&asides[0]).exists(),
        "the aside file is removed on the next run"
    );
}

#[cfg(unix)]
#[test]
fn sh_installer_skips_the_replace_when_the_same_binary_is_installed_and_running() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("xerj.exe");
    std::fs::write(&dest, b"same build").unwrap();
    std::fs::write(dir.join("xerj.exe.running"), b"").unwrap();
    let src = tmp.path().join("new-xerj.exe");
    std::fs::write(&src, b"same build").unwrap();

    let run = run_install(&dir, &src);
    assert_eq!(run.status, 0, "{}", run.out);
    assert!(run.out.contains("already up to date"), "{}", run.out);
    assert!(!run.out.contains("restart your MCP client"), "{}", run.out);
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names.iter().any(|n| n.starts_with("xerj.exe.old-")),
        "nothing is renamed when nothing changes: {names:?}"
    );
}

#[cfg(unix)]
#[test]
fn sh_installer_still_replaces_a_stopped_exe_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("xerj.exe");
    std::fs::write(&dest, b"old build").unwrap();
    let src = tmp.path().join("new-xerj.exe");
    std::fs::write(&src, b"new build").unwrap();

    let run = run_install(&dir, &src);
    assert_eq!(run.status, 0, "{}", run.out);
    assert_eq!(std::fs::read(&dest).unwrap(), b"new build");
    assert!(run.out.contains("ok installed"), "{}", run.out);
    assert!(!run.out.contains("restart your MCP client"), "{}", run.out);
}

#[test]
fn powershell_installer_does_not_stop_on_a_running_exe() {
    let ps1 = repo("landing/get.ps1");
    assert!(
        ps1.contains("(Get-FileHash -Algorithm SHA256 -LiteralPath $dest).Hash -eq (Get-FileHash -Algorithm SHA256 -LiteralPath $src).Hash"),
        "get.ps1 compares the installed file with the verified download before replacing it"
    );
    assert!(
        ps1.contains("already up to date"),
        "get.ps1 says so when there is nothing to replace"
    );
    assert!(
        ps1.contains("Rename-Item -LiteralPath $dest -NewName $aside"),
        "get.ps1 renames a running xerj.exe aside instead of failing"
    );
    assert!(
        ps1.contains("Filter 'xerj.exe.old-*'"),
        "get.ps1 removes aside files an earlier run left"
    );
    assert!(
        ps1.contains("restart your MCP client"),
        "get.ps1 tells the user the running copy keeps the old version"
    );
    // The download and the .sha256 fetch still happen on every run: the
    // per-asset download counts are read as installer runs (metrics/README.md).
    let download = ps1
        .find("Invoke-WebRequest -UseBasicParsing -Uri \"$base/$asset\" -OutFile")
        .unwrap();
    let compare = ps1.find("$sameBinary = $false").unwrap();
    assert!(
        download < compare,
        "the replace check runs after the download, not instead of it"
    );
}
