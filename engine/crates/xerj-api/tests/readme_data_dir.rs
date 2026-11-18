//! The root README quickstart must keep the node data directory outside any
//! folder an agent will autoindex. `landing/llms.txt` already says why:
//! `data/` is not in the built-in ignore list, so `--data-dir ./data` inside
//! that tree makes the node index its own storage. The root READMEs are the
//! page an agent copies first, and they used to show `./data`.

use std::path::PathBuf;

fn readme(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn quickstart_puts_the_data_dir_outside_the_indexed_tree() {
    for name in [
        "README.md",
        "README.zh-CN.md",
        "README.ja-JP.md",
        "README.es.md",
        "README.ar.md",
    ] {
        let text = readme(name);
        assert!(
            text.contains("xerj --insecure --data-dir ~/xerj-data &"),
            "{name} quickstart must start the node at ~/xerj-data"
        );
        assert!(
            text.contains("~/xerj-data/admin.key"),
            "{name} must read the admin key from the same directory"
        );
        assert!(
            !text.contains("xerj --insecure --data-dir ./data")
                && !text.contains("xerj --data-dir ./data"),
            "{name} still starts a node inside ./data"
        );
    }
    let en = readme("README.md");
    assert!(
        en.contains("outside any folder you will autoindex"),
        "the English README must say why ./data is the wrong place"
    );
}
