//! Source adapters: turn a recipe `[[sources]]` entry into a directory to
//! walk, plus the upstream watermark that lets an unchanged source be
//! skipped wholesale on the next run.
//!
//! Kinds:
//! - `dir`     — walk a recipe-relative directory (the fixture/local kind)
//! - `http-zip`— download a zip, extract into the cache, walk the
//!   extraction. No watermark: the zip is re-downloaded every run and the
//!   content store dedups what did not change (correctness lives there,
//!   watermarks are only an optimization).
//! - `git`     — promisor shallow checkout at `rev` (pinned) or the remote
//!   HEAD (tracking). Watermark = HEAD sha: unchanged tree ⇒ the walk is
//!   skipped entirely this run.
//!
//! Zip extraction is defensive by default: entry names are normalized and
//! any path escaping the extraction root (zip-slip), absolute path, or
//! symlink entry is skipped and COUNTED, never trusted — dump URLs are
//! fetched from the open internet.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::httpget;
use super::recipe::SourceSpec;

/// Total decompressed cap for one zip. Real advisory dumps are well under
/// this; the cap exists because a zip's declared sizes are untrusted and a
/// 3 MiB file can claim to expand to terabytes.
const MAX_EXTRACT_BYTES: u64 = 2 << 30;

#[derive(Debug)]
pub struct Fetched {
    /// The directory the glob walk starts from.
    pub root: PathBuf,
    /// Current upstream version marker, if the kind has one.
    pub watermark: Option<String>,
    /// The watermark matches the previous run's: the caller should skip
    /// walking AND pruning this source (everything it stored is still live).
    pub unchanged: bool,
}

/// Where a source's watermark from the previous run is kept.
fn watermark_path(cache_dir: &Path, slug: &str) -> PathBuf {
    cache_dir.join(format!("{slug}.watermark"))
}

/// Materialize a source. `cache_dir` is `<build>/cache`; it only exists to
/// make network kinds incremental.
pub fn fetch(src: &SourceSpec, recipe_dir: &Path, cache_dir: &Path) -> Result<Fetched> {
    match src.kind {
        super::recipe::SourceKind::Dir => Ok(Fetched {
            root: recipe_dir.join(src.path.as_deref().unwrap_or(".")),
            watermark: None,
            unchanged: false,
        }),
        super::recipe::SourceKind::HttpZip => {
            let url = src.url.as_deref().unwrap_or_default();
            let zip_path = cache_dir.join(format!("{}.zip", src.slug));
            httpget::download(url, &zip_path)
                .with_context(|| format!("source '{}': fetch", src.slug))?;
            let root = cache_dir.join(&src.slug);
            extract_zip(&zip_path, &root)
                .with_context(|| format!("source '{}': extract", src.slug))?;
            Ok(Fetched {
                root,
                watermark: None,
                unchanged: false,
            })
        }
        super::recipe::SourceKind::HttpJson => {
            let url = src.url.as_deref().unwrap_or_default();
            let root = cache_dir.join(&src.slug);
            if root.exists() {
                std::fs::remove_dir_all(&root)
                    .with_context(|| format!("source '{}': clear {}", src.slug, root.display()))?;
            }
            std::fs::create_dir_all(&root)
                .with_context(|| format!("source '{}': create {}", src.slug, root.display()))?;
            let json_path = root.join(format!("{}.json", src.slug));
            httpget::download(url, &json_path)
                .with_context(|| format!("source '{}': fetch", src.slug))?;
            Ok(Fetched {
                root,
                watermark: None,
                unchanged: false,
            })
        }
        super::recipe::SourceKind::Git => {
            let url = src.url.as_deref().unwrap_or_default();
            let root = cache_dir.join(&src.slug);
            let rev = src.rev.as_deref().unwrap_or("HEAD");
            let watermark = git_checkout(&root, url, rev, &src.slug)?;
            let prev = std::fs::read_to_string(watermark_path(cache_dir, &src.slug))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            Ok(Fetched {
                root,
                watermark: Some(watermark.clone()),
                unchanged: !watermark.is_empty() && watermark == prev,
            })
        }
    }
}

/// Persist a source's watermark after its harvest fully succeeded (never
/// before — a failed run must re-walk).
pub fn save_watermark(cache_dir: &Path, slug: &str, watermark: &str) -> Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    std::fs::write(watermark_path(cache_dir, slug), watermark)
        .with_context(|| format!("save watermark for '{slug}'"))
}

/// Shallow promisor checkout of `rev` into `target`, reusing the
/// corpus-clone machinery: init + blob:none filter so a pin at an old sha
/// costs one small fetch, never a full history. `rev` is a full sha, a
/// branch/tag name, or `HEAD` (the remote's default). Returns HEAD.
fn git_checkout(target: &Path, url: &str, rev: &str, slug: &str) -> Result<String> {
    if !target.join(".git").exists() {
        std::fs::create_dir_all(target)
            .with_context(|| format!("source '{slug}': create {}", target.display()))?;
        crate::xc::git(Some(target), &["init", "--quiet"])?;
        crate::xc::git(Some(target), &["remote", "add", "origin", url]).ok();
        crate::xc::git(Some(target), &["config", "remote.origin.promisor", "true"]).ok();
        crate::xc::git(
            Some(target),
            &["config", "remote.origin.partialclonefilter", "blob:none"],
        )
        .ok();
    }
    // A pinned rev (full sha) can reuse checkout_at_sha; a named ref (branch
    // or tag, e.g. RustSec's `osv` branch) and plain HEAD go through
    // FETCH_HEAD, which checkout_at_sha cannot name. Anything else used to
    // silently fetch HEAD — a typo'd branch name now fails loudly instead.
    let ok = |r: Result<(i32, String)>| r.map(|(rc, _)| rc == 0).unwrap_or(false);
    let looks_like_sha = rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit());
    if looks_like_sha {
        crate::xc::checkout_at_sha(target, url, rev)
            .with_context(|| format!("source '{slug}': checkout {rev}"))?;
    } else {
        // `rev` works directly as the refspec: a branch/tag name fetches that
        // ref, and the literal "HEAD" fetches the remote's default HEAD.
        match crate::xc::git(Some(target), &["fetch", "--depth", "1", "origin", rev]) {
            Ok((0, _)) => {}
            Ok((_, output)) => {
                bail!(
                    "source '{slug}': {}",
                    crate::xc::git_fetch_failure(rev, &output)
                )
            }
            Err(e) => bail!(
                "source '{slug}': {}",
                crate::xc::git_fetch_failure(rev, &e.to_string())
            ),
        }
        if !ok(crate::xc::git(
            Some(target),
            &["checkout", "--quiet", "--force", "--detach", "FETCH_HEAD"],
        )) {
            bail!("source '{slug}': git checkout FETCH_HEAD ({rev}) failed");
        }
        crate::xc::git(Some(target), &["clean", "-qfd"]).ok();
    }
    let (_, head) = crate::xc::git(Some(target), &["rev-parse", "HEAD"])
        .with_context(|| format!("source '{slug}': rev-parse"))?;
    if head.is_empty() {
        bail!("source '{slug}': empty HEAD after checkout");
    }
    Ok(head)
}

/// Extract `zip` into `dest`, replacing any previous extraction of the same
/// source. Defensive: entry paths are sanitized, escapes are skipped and
/// counted (returned), total decompressed bytes are capped.
pub fn extract_zip(zip: &Path, dest: &Path) -> Result<usize> {
    let f = std::fs::File::open(zip).with_context(|| format!("open {}", zip.display()))?;
    let mut z = zip::ZipArchive::new(f).context("open zip")?;

    // clear the previous extraction first: a file renamed upstream must not
    // survive as a stale duplicate
    if dest.exists() {
        std::fs::remove_dir_all(dest).with_context(|| format!("clear {}", dest.display()))?;
    }
    std::fs::create_dir_all(dest)?;

    let mut skipped = 0usize;
    let mut total: u64 = 0;
    for i in 0..z.len() {
        let mut entry = z.by_index(i).context("zip entry")?;
        if entry.is_dir() {
            continue;
        }
        if entry.is_symlink() {
            skipped += 1;
            continue;
        }
        let raw = entry.name().to_string();
        let Some(safe) = sanitize_entry_path(&raw) else {
            skipped += 1;
            continue;
        };
        let out = dest.join(&safe);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        // bounded read: `entry.size()` is the DECLARED size, so read via
        // take() + counter and stop the whole extraction past the cap
        let limit = MAX_EXTRACT_BYTES.saturating_sub(total);
        let mut reader = (&mut entry).take(limit + 1);
        reader.read_to_end(&mut buf).context("decompress entry")?;
        total += buf.len() as u64;
        if total > MAX_EXTRACT_BYTES {
            bail!(
                "zip {} expands past the {} cap",
                zip.display(),
                MAX_EXTRACT_BYTES
            );
        }
        std::fs::write(&out, &buf)?;
    }
    Ok(skipped)
}

/// Reject absolute paths, drive letters, `..`, and anything not expressible
/// as normal relative components (the zip-slip family). Returns the
/// posix-style relative path. Windows-style names (`C:/x`, `a\\b`) are
/// rejected on EVERY os — a pack may be built anywhere, and a name that is
/// a harmless subdir on linux is an escape on windows.
fn sanitize_entry_path(raw: &str) -> Option<String> {
    let path = Path::new(raw);
    let mut parts: Vec<&str> = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => {
                let s = p.to_str()?;
                if s.is_empty()
                    || s == "."
                    || s.contains('\0')
                    || s.contains(':')
                    || s.contains('\\')
                {
                    return None;
                }
                parts.push(s);
            }
            // a leading `/` makes it absolute on unix; Prefix is a windows
            // drive; CurDir/ParentDir have no business in an entry name
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in entries {
            z.start_file(*name, opts).unwrap();
            use std::io::Write;
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn zip_extracts_nested_and_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let zp = tmp.path().join("d.zip");
        write_zip(
            &zp,
            &[
                ("a.json", "{\"id\":\"x\"}"),
                ("nested/b.json", "{\"id\":\"y\"}"),
            ],
        );
        let dest = tmp.path().join("out");
        assert_eq!(extract_zip(&zp, &dest).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(dest.join("a.json")).unwrap(),
            "{\"id\":\"x\"}"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("nested/b.json")).unwrap(),
            "{\"id\":\"y\"}"
        );

        // a re-extract with a renamed file must not leave the stale one
        write_zip(&zp, &[("renamed.json", "{}")]);
        extract_zip(&zp, &dest).unwrap();
        assert!(!dest.join("a.json").exists());
        assert!(dest.join("renamed.json").exists());
    }

    #[test]
    fn zip_slip_and_absolute_are_skipped_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.txt");
        let zp = tmp.path().join("evil.zip");
        write_zip(
            &zp,
            &[
                ("ok.json", "{}"),
                ("../outside.txt", "pwned"),
                ("/abs.json", "{}"),
                ("C:/windows/evil.txt", "{}"),
            ],
        );
        let dest = tmp.path().join("out");
        let skipped = extract_zip(&zp, &dest).unwrap();
        assert!(dest.join("ok.json").exists());
        assert!(!outside.exists(), "nothing escaped the extraction root");
        assert_eq!(skipped, 3, "absolute + escape entries counted, not written");
    }

    #[test]
    fn git_source_checks_out_and_watermarks() {
        // a real local repo via `git init` — no daemon needed
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        assert!(crate::xc::git(Some(&origin), &["init", "--quiet", "-b", "main"]).is_ok());
        std::fs::write(origin.join("one.json"), r#"{"id":"R1","aliases":["C1"]}"#).unwrap();
        assert!(crate::xc::git(Some(&origin), &["add", "."]).is_ok());
        let git_user = |args: &[&str]| crate::xc::git(Some(&origin), args);
        git_user(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--quiet",
            "-m",
            "one",
        ])
        .ok();

        let cache = tmp.path().join("cache");
        let spec = super::super::recipe::SourceSpec {
            slug: "repo".into(),
            kind: super::super::recipe::SourceKind::Git,
            path: None,
            url: Some(origin.to_string_lossy().to_string()),
            rev: None,
            glob: "**/*.json".into(),
            format: super::super::recipe::Format::Flat,
            records_field: None,
            licence: "CC0-1.0".into(),
        };
        let f1 = fetch(&spec, tmp.path(), &cache).unwrap();
        assert!(f1.root.join("one.json").exists());
        assert!(!f1.unchanged, "first fetch has no previous watermark");
        let wm = f1.watermark.unwrap();
        assert_eq!(wm.len(), 40, "HEAD sha");

        save_watermark(&cache, &spec.slug, &wm).unwrap();
        let f2 = fetch(&spec, tmp.path(), &cache).unwrap();
        assert!(f2.unchanged, "same HEAD ⇒ skip the walk");
        assert_eq!(f2.watermark.as_deref(), Some(wm.as_str()));

        // a new commit upstream moves the watermark
        std::fs::write(origin.join("two.json"), r#"{"id":"R2"}"#).unwrap();
        crate::xc::git(Some(&origin), &["add", "."]).ok();
        git_user(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--quiet",
            "-m",
            "two",
        ])
        .ok();
        let f3 = fetch(&spec, tmp.path(), &cache).unwrap();
        assert!(!f3.unchanged);
        assert_ne!(f3.watermark.as_deref(), Some(wm.as_str()));
        assert!(
            f3.root.join("two.json").exists(),
            "checkout moved to the new tip"
        );
    }

    #[test]
    fn git_source_checks_out_a_named_branch() {
        // RustSec's useful branch (`osv`) is not the default: `rev` must be
        // able to name it. Before this was supported, a non-sha rev silently
        // fetched the default branch's HEAD — the wrong tree, quietly.
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        assert!(crate::xc::git(Some(&origin), &["init", "--quiet", "-b", "main"]).is_ok());
        std::fs::write(origin.join("main.json"), r#"{"id":"M"}"#).unwrap();
        let commit = |m: &str| {
            crate::xc::git(Some(&origin), &["add", "."]).ok();
            crate::xc::git(
                Some(&origin),
                &[
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "--quiet",
                    "-m",
                    m,
                ],
            )
            .ok();
        };
        commit("main only");
        // branch `export` carries a different tree
        assert!(crate::xc::git(Some(&origin), &["checkout", "--quiet", "-b", "export"]).is_ok());
        std::fs::write(origin.join("export.json"), r#"{"id":"E"}"#).unwrap();
        std::fs::remove_file(origin.join("main.json")).unwrap();
        commit("export tree");
        assert!(crate::xc::git(Some(&origin), &["checkout", "--quiet", "main"]).is_ok());

        let mut spec = super::super::recipe::SourceSpec {
            slug: "repo".into(),
            kind: super::super::recipe::SourceKind::Git,
            path: None,
            url: Some(origin.to_string_lossy().to_string()),
            rev: Some("export".into()),
            glob: "**/*.json".into(),
            format: super::super::recipe::Format::Flat,
            records_field: None,
            licence: "CC0-1.0".into(),
        };
        let f = fetch(&spec, tmp.path(), &tmp.path().join("cache")).unwrap();
        assert!(
            f.root.join("export.json").exists(),
            "the named branch's tree"
        );
        assert!(
            !f.root.join("main.json").exists(),
            "not the default branch's"
        );

        // a rev that names nothing fails loudly, not as a silent HEAD fetch
        spec.rev = Some("no-such-branch".into());
        let err = fetch(&spec, tmp.path(), &tmp.path().join("cache"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no-such-branch"), "{err}");
    }
}
