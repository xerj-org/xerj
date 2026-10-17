//! The first-run banner names `<data_dir>/admin.key` and does not print the
//! key (see `first_run_admin_key_notice`). These pages used to say the
//! opposite, so an agent waited for a secret that never appears.

use std::path::PathBuf;

fn repo(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn first_run_docs_do_not_say_the_key_is_printed() {
    let security = repo("landing/docs/security.html");
    assert!(
        security.contains("does not print the key"),
        "security.html must say the banner does not print the key"
    );
    assert!(
        !security.contains("printed in a startup banner"),
        "security.html still says the key is printed"
    );

    let quickstart = repo("landing/docs/quickstart.html");
    assert!(quickstart.contains("does not print the key"));
    assert!(
        !quickstart.contains("prints it, and writes it"),
        "quickstart.html still says the key is printed"
    );

    let install = repo("landing/docs/install.html");
    assert!(install.contains("without printing the key"));
    assert!(
        !install.contains("prints an auto-generated admin API key"),
        "install.html still says the key is printed"
    );

    let full = repo("landing/llms-full.txt");
    assert!(full.contains("does not print it, and writes it to `<data_dir>/admin.key`"));
    assert!(
        !full.contains("prints it, and writes it to `<data_dir>/admin.key`"),
        "llms-full.txt still says the key is printed"
    );
}
