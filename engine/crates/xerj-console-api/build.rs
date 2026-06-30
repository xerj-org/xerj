// Bundle the Xerj Console playground (typography-first observability UI)
// into the xerj binary at compile time.
//
// We walk a single source directory (the playground repo lives next
// to the engine in this monorepo: ../../../xerj.ai/playground) and
// emit `$OUT_DIR/xerj_console_assets.rs` with a const slice of
// (path, bytes) entries. The asset paths are relative to the
// playground root and used as the URL path under `/_xerj-console/`.
//
// We deliberately ship only HTML / CSS / JS / SVG / WOFF — anything
// the runtime would never serve to a browser stays out. Files larger
// than 4 MiB are skipped with a build-script warning so an accidental
// PDF in the playground tree doesn't bloat the binary.
//
// Re-run logic: cargo invokes this script when build.rs itself
// changes OR when any file under the playground source tree is
// modified (we emit `cargo:rerun-if-changed` for each one).

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

// Path-resolution strategy: try multiple relative paths so the build
// works in both the monorepo root layout (xerj.ai/) and the engine-
// worktree layout (xerj-es-compat-work/engine/, with xerj.ai
// alongside as a sibling worktree).
const PLAYGROUND_RELS: &[&str] = &[
    "../../../xerj.ai/playground",       // engine/crates/api → root → xerj.ai
    "../../../../xerj.ai/playground",    // worktree → parent → xerj.ai (sibling)
    "../../playground",                   // direct sibling layout
    "../../../playground",                // alternate
];
const ALLOWED_EXTS: &[&str] = &["html", "css", "js", "svg", "ico", "woff", "woff2", "png"];
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // Walk the candidate list and pick the first one that resolves to
    // an existing playground directory.
    let playground_root_canonical = PLAYGROUND_RELS
        .iter()
        .map(|rel| manifest_dir.join(rel))
        .find_map(|p| fs::canonicalize(&p).ok())
        .or_else(|| {
            // Last-ditch: env var override for CI / monorepo packaging.
            env::var("XERJ_CONSOLE_PLAYGROUND_DIR").ok().and_then(|p| fs::canonicalize(p).ok())
        });
    let playground_root_canonical = match playground_root_canonical {
        Some(p) => p,
        None => {
            // Playground checkout missing — emit an empty asset table
            // so the engine still builds (e.g. release tarballs that
            // omit the playground source).
            write_assets(&[], &manifest_dir);
            println!(
                "cargo:warning=xerj-console playground not found in any of: {:?} — asset table will be empty (set XERJ_CONSOLE_PLAYGROUND_DIR to override)",
                PLAYGROUND_RELS,
            );
            return;
        }
    };

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    walk(&playground_root_canonical, &playground_root_canonical, &mut entries);
    entries.sort();

    write_assets(&entries, &playground_root_canonical);
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read.flatten() {
        let path = entry.path();
        // Skip hidden files / dirs and node_modules (the playground
        // is currently a static SPA but a future build step might
        // pull deps).
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk(root, &path, out);
            continue;
        }
        if meta.len() > MAX_FILE_BYTES {
            println!(
                "cargo:warning=xerj-console skipping {} (>{} bytes)",
                path.display(),
                MAX_FILE_BYTES
            );
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !ALLOWED_EXTS.contains(&ext.as_str()) {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let url_path = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        println!("cargo:rerun-if-changed={}", path.display());
        out.push((url_path, path));
    }
}

fn write_assets(entries: &[(String, PathBuf)], _playground_root: &Path) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("xerj_console_assets.rs");
    let mut f = fs::File::create(&out_path).expect("create xerj_console_assets.rs");

    writeln!(
        f,
        "// auto-generated by build.rs — Xerj Console playground asset bundle\n\
         //\n\
         // (url_path, bytes, content_type) tuples. Order is sorted-by-path\n\
         // so the runtime can binary-search by path.\n\
         pub static XERJ_CONSOLE_ASSETS: &[(&str, &[u8], &str)] = &["
    ).unwrap();
    for (url_path, src_path) in entries {
        let ct = content_type_for(url_path);
        // Use absolute path so include_bytes! doesn't depend on the
        // build cwd. include_bytes! is a compile-time op so any change
        // to the source content invalidates the build via the
        // rerun-if-changed lines above.
        writeln!(
            f,
            "    (\"{}\", include_bytes!(r\"{}\"), \"{}\"),",
            url_path,
            src_path.display(),
            ct,
        ).unwrap();
    }
    writeln!(f, "];").unwrap();
}

fn content_type_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "css"  => "text/css; charset=utf-8",
        "js"   => "application/javascript; charset=utf-8",
        "svg"  => "image/svg+xml",
        "ico"  => "image/x-icon",
        "woff" => "font/woff",
        "woff2"=> "font/woff2",
        "png"  => "image/png",
        "json" => "application/json; charset=utf-8",
        _      => "application/octet-stream",
    }
}
