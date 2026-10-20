//! #1586: a usage error is the error plus one line pointing at `--help`,
//! never the help page itself.
//!
//! Before this, any `xerj autoindex` usage error wrote one throwaway line to
//! stderr and the 390-line help page to stdout. An agent that reads stdout, or
//! the tail of combined output, saw only help — nothing said the run had done
//! nothing, and the field report's agent concluded the first folder had been
//! indexed.
//!
//! This test drives the real binary: the exit code, the empty stdout, and the
//! two-line stderr are the contract. It uses `--approve full` (a value error)
//! so it does not depend on the second-folder message from the sibling PR.

use std::process::Command;

#[test]
fn autoindex_usage_error_prints_the_error_and_one_help_pointer() {
    let out = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args(["autoindex", "--approve", "full"])
        .output()
        .expect("run the xerj binary");

    assert_eq!(out.status.code(), Some(2), "usage errors exit 2");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.is_empty(),
        "a usage error must not write the help page to stdout:\n{stdout}"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "the whole stderr is the error and one pointer, got:\n{stderr}"
    );
    assert!(
        lines[0].starts_with("error: "),
        "first line must be the error: {stderr}"
    );
    assert!(
        lines[0].contains("--approve full is not one of"),
        "the actual usage error must survive: {stderr}"
    );
    assert!(
        lines[1].contains("xerj autoindex --help"),
        "second line must point at the full help: {stderr}"
    );
}
