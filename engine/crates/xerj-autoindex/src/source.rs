//! Where a run's documents come from: a local folder tree, or an object store.
//!
//! `xerj autoindex <folder>` had exactly one source — [`crate::walk`] over a
//! filesystem tree. Pointing it at `s3://bucket/prefix` needs the same two
//! answers from somewhere else: *what candidate documents are there* (with
//! enough metadata to tell an unchanged one from a changed one) and *how do I
//! read one*. That pair is [`DocSource`].
//!
//! What this module is NOT: a rewrite of the walk. The local run still calls
//! [`crate::walk::walk_reporting_opts`] directly and nothing about it changed —
//! [`LocalDirSource`] exists so the trait is implemented by both kinds of source
//! and so the object-store cache logic can be exercised against a filesystem
//! source in tests. The extract/sniff pipeline is not touched at all: an object
//! source hands it the same thing a folder does, a local file whose bytes are
//! the object's bytes (see [`crate::objsource`] for why the bytes are landed on
//! disk rather than piped, and what that costs).

use anyhow::{bail, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Which URL scheme named the store. Both speak the S3 API; the scheme only
/// changes the default region and the wording of the errors, because an
/// operator who typed `r2://` is not helped by advice about `AWS_REGION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectScheme {
    /// `s3://` — Amazon S3, or anything S3-compatible with `--endpoint-url`.
    S3,
    /// `r2://` — Cloudflare R2. Region is always `auto` there.
    R2,
}

impl ObjectScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectScheme::S3 => "s3",
            ObjectScheme::R2 => "r2",
        }
    }
}

/// A parsed `s3://bucket/prefix` (or `r2://…`) target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectSpec {
    pub scheme: ObjectScheme,
    pub bucket: String,
    /// Normalised listing prefix: either empty (whole bucket) or ending in `/`.
    ///
    /// A trailing slash is ADDED when the operator did not type one, so
    /// `s3://bucket/docs` lists `docs/…` and not also `docs-old/…`. Plain S3
    /// prefix matching is a string prefix, which surprises everyone the first
    /// time; a folder is what people mean by `s3://bucket/docs`, and the
    /// effective prefix is printed on every run so the choice is visible rather
    /// than magic.
    pub prefix: String,
    /// `--endpoint-url`, or `AWS_ENDPOINT_URL_S3` / `AWS_ENDPOINT_URL` from the
    /// environment. `None` means the SDK's own endpoint for the region.
    pub endpoint: Option<String>,
}

impl ObjectSpec {
    /// The string this source is known by: the journal's root identity, the
    /// seed of the default state-dir hash, and what progress/errors print.
    ///
    /// It deliberately does NOT include the endpoint. The same bucket reached
    /// through a different endpoint (a VPC endpoint, an account-specific R2
    /// host, `localhost:9000` in a test) is the same corpus, and hashing the
    /// endpoint into the state key would silently start a second index —
    /// re-downloading every object — the first time somebody's URL changed.
    pub fn identity(&self) -> String {
        format!("{}://{}/{}", self.scheme.as_str(), self.bucket, self.prefix)
    }

    /// Directory name for this source's local mirror, and therefore (through
    /// `derive_brain_name`) the default second-brain name. Bucket plus prefix,
    /// so two prefixes of one bucket do not collide.
    pub fn slug(&self) -> String {
        let mut raw = self.bucket.clone();
        if !self.prefix.is_empty() {
            raw.push('-');
            raw.push_str(self.prefix.trim_end_matches('/'));
        }
        let slug: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let slug = slug.trim_matches('-').to_string();
        // Keep it short enough to be a legal directory name everywhere, and
        // never empty (an unnamed mirror directory would collide with itself).
        let slug: String = slug.chars().take(120).collect();
        if slug.is_empty() {
            "objects".into()
        } else {
            slug
        }
    }
}

/// What the positional argument turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSpec {
    LocalDir(PathBuf),
    Object(ObjectSpec),
}

/// True when `arg` names an object store rather than a folder.
///
/// Only `s3://` and `r2://` count, matched case-insensitively on the scheme
/// alone — a Windows path (`C:\data`) has no `//`, and a key is never
/// lowercased.
pub fn looks_like_object_url(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower.starts_with("s3://") || lower.starts_with("r2://")
}

/// Parse the positional argument. Anything that is not an object URL is a
/// folder, exactly as before.
///
/// `endpoint_flag` is `--endpoint-url`; when absent, `AWS_ENDPOINT_URL_S3` then
/// `AWS_ENDPOINT_URL` are consulted, which is the precedence the AWS CLI uses.
pub fn parse_source(arg: &Path, endpoint_flag: Option<&str>) -> Result<SourceSpec> {
    let raw = arg.to_string_lossy();
    if !looks_like_object_url(&raw) {
        return Ok(SourceSpec::LocalDir(arg.to_path_buf()));
    }
    let scheme = if raw[..2].eq_ignore_ascii_case("s3") {
        ObjectScheme::S3
    } else {
        ObjectScheme::R2
    };
    let rest = &raw[5..];
    let (bucket, prefix) = match rest.split_once('/') {
        Some((bucket, prefix)) => (bucket, prefix),
        None => (rest, ""),
    };
    if bucket.is_empty() {
        bail!(
            "{raw} names no bucket. Write it as {}://<bucket>/<prefix> — for example \
             {}://my-bucket/docs",
            scheme.as_str(),
            scheme.as_str()
        );
    }
    // A bucket name is a hostname-ish label, and anything else in that position
    // is a paste accident with consequences: `s3://key:secret@bucket/p` would
    // otherwise be sent to a "bucket" called `key:secret@bucket`, putting a
    // credential in the request line and in every log that records it.
    if let Some(bad) = bucket
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
    {
        bail!(
            "{raw} has an invalid bucket name {bucket:?} (character {bad:?}). A bucket name may \
             hold letters, digits, '.', '-' and '_' only. Credentials do not belong in the URL — \
             they come from the environment (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY), and an \
             endpoint goes in --endpoint-url"
        );
    }
    let prefix = normalise_prefix(prefix);
    let endpoint = match endpoint_flag {
        Some(url) => Some(url.to_string()),
        None => std::env::var("AWS_ENDPOINT_URL_S3")
            .or_else(|_| std::env::var("AWS_ENDPOINT_URL"))
            .ok()
            .filter(|s| !s.trim().is_empty()),
    };
    Ok(SourceSpec::Object(ObjectSpec {
        scheme,
        bucket: bucket.to_string(),
        prefix,
        endpoint,
    }))
}

/// Strip a leading `/`, collapse an all-slash prefix to empty, and make sure a
/// non-empty prefix ends in exactly one `/` (see [`ObjectSpec::prefix`]).
fn normalise_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim_start_matches('/');
    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

/// Why a listed object is not a candidate document. These share the reporting
/// surface with [`crate::ignore_rules`]' labels, so "where did my file go" has
/// one answer shape for both kinds of source.
///
/// A key that is skipped is never downloaded, so a skip is also money not spent.
pub mod rules {
    /// The key names the prefix itself, or ends in `/`: a folder marker, not a
    /// document. Every S3 console creates these.
    pub const FOLDER_MARKER: &str = "object:folder-marker";
    /// The key cannot be represented as a relative path safely or portably.
    /// See [`super::classify_key`] for the exact list.
    pub const UNSAFE_KEY: &str = "object:unsafe-key";
    /// Two keys differ only in case, so they would be one file on a
    /// case-insensitive filesystem (macOS, Windows) and each run would
    /// overwrite the other.
    pub const CASE_COLLISION: &str = "object:case-collision";
}

/// The verdict on one listed key, already stripped of the listing prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyVerdict {
    /// Admitted as a candidate document with this root-relative path.
    Admit,
    /// Not a candidate, for the named rule.
    Skip(&'static str),
}

/// Longest single path component we will create. 255 bytes is the limit on
/// ext4, APFS and NTFS alike; a longer component would fail at `create_dir`
/// halfway through a run instead of being reported up front.
const MAX_COMPONENT_BYTES: usize = 255;

/// Basenames Windows cannot create, whatever the extension.
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Decide whether a prefix-stripped key can become a document.
///
/// An object key is an arbitrary UTF-8 string, not a path: `../../etc/passwd`,
/// `a//b`, `a\b`, a trailing dot and a 4 KB component are all legal keys. They
/// become a local path here, so each one is a decision rather than an accident:
///
/// - **`..`, `.`, an empty component, a leading `/`** — refused. A key that
///   escapes the mirror directory is the classic object-store traversal, and
///   `a//b` is a different key from `a/b` while being the same path.
/// - **A backslash** — refused. It is an ordinary character in a key and a
///   directory separator on Windows, so admitting it would index a different
///   set of documents per platform.
/// - **Control characters** — refused. They forge lines in the progress stream
///   (the #278 class) and are unrepresentable in half the tooling downstream.
/// - **A trailing `.` or space, or a Windows reserved basename** — refused
///   EVERYWHERE, not only on Windows, so one bucket produces one corpus
///   whatever machine indexes it. The report names the rule, so a refused key
///   is visible rather than silently missing.
/// - **A hidden (dot-prefixed) component** — skipped under
///   [`crate::ignore_rules::HIDDEN_RULE`], the same rule the walker applies at
///   depth > 0, and for the same reason: `.env`, `.git/config` and `.ssh/id_rsa`
///   live in buckets too, and "index this bucket" is not consent to publish
///   them into a queryable index. The two ignore FILES are the exception —
///   `.gitignore` and `.xerjignore` are fetched so the bucket's own ignore
///   rules still apply to it, and the walker then declines to index them
///   because of this same rule.
pub fn classify_key(rel: &str) -> KeyVerdict {
    if rel.is_empty() || rel.ends_with('/') {
        return KeyVerdict::Skip(rules::FOLDER_MARKER);
    }
    if rel.starts_with('/') || rel.contains('\\') {
        return KeyVerdict::Skip(rules::UNSAFE_KEY);
    }
    if rel.chars().any(|c| c.is_control()) {
        return KeyVerdict::Skip(rules::UNSAFE_KEY);
    }
    let components: Vec<&str> = rel.split('/').collect();
    let last = components.len() - 1;
    for (index, component) in components.iter().enumerate() {
        if component.is_empty() || *component == "." || *component == ".." {
            return KeyVerdict::Skip(rules::UNSAFE_KEY);
        }
        if component.len() > MAX_COMPONENT_BYTES {
            return KeyVerdict::Skip(rules::UNSAFE_KEY);
        }
        if component.ends_with('.') || component.ends_with(' ') || component.starts_with(' ') {
            return KeyVerdict::Skip(rules::UNSAFE_KEY);
        }
        let stem = component.split('.').next().unwrap_or(component);
        if WINDOWS_RESERVED
            .iter()
            .any(|reserved| stem.eq_ignore_ascii_case(reserved))
        {
            return KeyVerdict::Skip(rules::UNSAFE_KEY);
        }
        if component.starts_with('.') {
            let is_ignore_file = index == last
                && (*component == crate::ignore_rules::GITIGNORE
                    || *component == crate::ignore_rules::XERJIGNORE);
            if !is_ignore_file {
                return KeyVerdict::Skip(crate::ignore_rules::HIDDEN_RULE);
            }
        }
    }
    // The built-in build-output defaults, applied here as well as in the walk.
    // Same list, so the two can never drift; applying it early means a bucket
    // full of `node_modules/` costs no GET requests instead of costing them and
    // then being thrown away by the walker.
    if crate::ignore_rules::DEFAULT_IGNORE_PATTERNS
        .iter()
        .any(|pattern| {
            let dir = pattern.trim_end_matches('/');
            components[..last].contains(&dir)
        })
    {
        // Reported under the same label shape the walk uses for a pruned
        // directory, so `--dry-run` output reads the same for both sources.
        return KeyVerdict::Skip("default:build-output");
    }
    KeyVerdict::Admit
}

/// One candidate document as a source reports it, before any bytes are read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    /// Root-relative path with forward slashes — the `ax_path` value, and the
    /// path inside the local mirror for an object source.
    pub rel: String,
    pub size: u64,
    /// An opaque token that changes when the bytes change, if the source can
    /// answer cheaply. `None` means it cannot, and the bytes decide.
    ///
    /// For an object store this is built from the ETag and the size; see
    /// [`crate::objsource::change_token`] for what is and is not assumed about
    /// an ETag. A local folder returns `None`: the walk's own answer is a full
    /// content hash, because file metadata cannot prove byte identity across
    /// every filesystem XERJ runs on.
    pub change_token: Option<String>,
    /// RFC 3339 last-modified, when the source reports one. Recorded for the
    /// operator; never used as change identity on its own unless there is no
    /// ETag at all.
    pub last_modified: Option<String>,
}

/// Requests a source has made, so a run can print what it cost.
///
/// Object stores bill per request, and the two classes are priced very
/// differently (Cloudflare R2: 1M "Class A" list/write operations a month free,
/// 10M "Class B" reads). Keeping them apart is the difference between a number
/// an operator can act on and a number they cannot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceOps {
    /// LIST pages (R2/S3 "Class A" — the scarce one).
    ///
    /// **Wire attempts, not logical calls.** A page the store throttles twice
    /// before answering is three billed requests and counts as three, because
    /// this number exists to predict an invoice.
    pub list_requests: u64,
    /// GET/HEAD requests ("Class B"), counted the same way.
    pub read_requests: u64,
    /// How many of the above were retries of an earlier attempt. Informational:
    /// they are already included in the two counts.
    pub retried_requests: u64,
    pub bytes_read: u64,
}

/// What [`DocSource::list`] returned, plus why anything was left out.
#[derive(Debug, Clone, Default)]
pub struct SourceListing {
    pub entries: Vec<SourceEntry>,
    /// Rule label → how many keys it skipped. Rule labels come from
    /// [`rules`] and [`crate::ignore_rules`].
    pub skipped: std::collections::BTreeMap<String, u64>,
    /// Keys seen before filtering. `objects_listed` in the run's cost report.
    pub seen: u64,
}

/// A place documents come from: enumerate, then read one.
///
/// Implementors must be safe to call `open` on from several threads, which is
/// why it takes `&self`, returns an owned reader, and why the trait is
/// `Send + Sync`: the transfer phase fetches several objects at once.
pub trait DocSource: Send + Sync {
    /// What to print in progress and errors, e.g. `s3://bucket/docs/`.
    fn describe(&self) -> String;

    /// Enumerate candidate documents. Cheap metadata only — no bytes.
    fn list(&self) -> Result<SourceListing>;

    /// A sequential reader over one entry's bytes.
    ///
    /// The contract is streaming: the implementation must not hold the whole
    /// object in memory, because "point XERJ at my bucket" includes buckets
    /// with multi-gigabyte objects in them.
    fn open(&self, entry: &SourceEntry) -> Result<Box<dyn Read + Send>>;

    /// Requests made so far.
    fn ops(&self) -> SourceOps;
}

/// A filesystem folder as a [`DocSource`].
///
/// The production local path does not go through this — it calls
/// [`crate::walk`] and then hashes, exactly as it always has. This impl exists
/// so the trait has two implementations rather than one shaped like S3, and so
/// the mirroring logic in [`crate::objsource`] can be tested against a source
/// with no network at all.
pub struct LocalDirSource {
    root: PathBuf,
    follow_symlinks: bool,
    ignore: crate::ignore_rules::IgnoreOptions,
}

impl LocalDirSource {
    pub fn new(
        root: &Path,
        follow_symlinks: bool,
        ignore: crate::ignore_rules::IgnoreOptions,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            follow_symlinks,
            ignore,
        }
    }
}

impl DocSource for LocalDirSource {
    fn describe(&self) -> String {
        self.root.display().to_string()
    }

    fn list(&self) -> Result<SourceListing> {
        let (files, report) =
            crate::walk::walk_reporting(&self.root, self.follow_symlinks, self.ignore)?;
        let seen = files.len() as u64 + report.files_skipped;
        let mut skipped = std::collections::BTreeMap::new();
        if report.files_skipped > 0 {
            skipped.insert("walk:ignored".to_string(), report.files_skipped);
        }
        Ok(SourceListing {
            entries: files
                .into_iter()
                .map(|f| SourceEntry {
                    rel: f.rel,
                    size: f.size,
                    change_token: None,
                    last_modified: None,
                })
                .collect(),
            skipped,
            seen,
        })
    }

    fn open(&self, entry: &SourceEntry) -> Result<Box<dyn Read + Send>> {
        // `rel` came from this source's own walk, but it is still joined
        // defensively: a caller that round-trips entries through JSON could
        // hand back `../`, and a source must not read outside its root.
        match classify_key(&entry.rel) {
            KeyVerdict::Admit => {}
            KeyVerdict::Skip(rule) => {
                bail!("{} is not a readable relative path ({rule})", entry.rel)
            }
        }
        let path = self.root.join(&entry.rel);
        Ok(Box::new(std::fs::File::open(&path)?))
    }

    fn ops(&self) -> SourceOps {
        SourceOps::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(arg: &str) -> ObjectSpec {
        match parse_source(Path::new(arg), None).unwrap() {
            SourceSpec::Object(spec) => spec,
            SourceSpec::LocalDir(p) => panic!("{arg} parsed as a folder: {}", p.display()),
        }
    }

    #[test]
    fn plain_paths_stay_folders() {
        for arg in [
            "/home/me/docs",
            "./docs",
            "docs",
            "C:\\data",
            "s3-bucket",
            "my-s3://thing",
        ] {
            assert!(
                matches!(
                    parse_source(Path::new(arg), None).unwrap(),
                    SourceSpec::LocalDir(_)
                ),
                "{arg} should be a folder"
            );
        }
    }

    #[test]
    fn parses_bucket_and_prefix() {
        let spec = object("s3://my-bucket/docs/2026");
        assert_eq!(spec.scheme, ObjectScheme::S3);
        assert_eq!(spec.bucket, "my-bucket");
        assert_eq!(spec.prefix, "docs/2026/");
        assert_eq!(spec.identity(), "s3://my-bucket/docs/2026/");
    }

    #[test]
    fn whole_bucket_has_an_empty_prefix() {
        for arg in ["s3://my-bucket", "s3://my-bucket/", "s3://my-bucket///"] {
            let spec = object(arg);
            assert_eq!(spec.prefix, "", "{arg}");
            assert_eq!(spec.identity(), "s3://my-bucket/");
        }
    }

    #[test]
    fn r2_scheme_and_case_insensitive_scheme() {
        let spec = object("R2://Bucket-Name/Docs");
        assert_eq!(spec.scheme, ObjectScheme::R2);
        // The bucket and prefix keep their case; only the scheme is folded.
        assert_eq!(spec.bucket, "Bucket-Name");
        assert_eq!(spec.prefix, "Docs/");
    }

    #[test]
    fn credentials_in_the_url_are_refused_by_name() {
        let err = parse_source(Path::new("s3://key:secret@bucket/p"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid bucket name"), "{err}");
        assert!(
            err.contains("Credentials do not belong in the URL"),
            "{err}"
        );
    }

    #[test]
    fn empty_bucket_is_refused_with_the_shape_to_type() {
        let err = parse_source(Path::new("s3://"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("names no bucket"), "{err}");
        assert!(err.contains("s3://<bucket>/<prefix>"), "{err}");
    }

    #[test]
    fn endpoint_flag_wins_over_environment() {
        let spec = parse_source(Path::new("s3://b/p"), Some("http://127.0.0.1:9000")).unwrap();
        match spec {
            SourceSpec::Object(spec) => {
                assert_eq!(spec.endpoint.as_deref(), Some("http://127.0.0.1:9000"))
            }
            _ => panic!("expected an object source"),
        }
    }

    #[test]
    fn slug_is_a_stable_directory_name() {
        assert_eq!(
            object("s3://my-bucket/docs/2026").slug(),
            "my-bucket-docs-2026"
        );
        assert_eq!(object("s3://my-bucket").slug(), "my-bucket");
        // Two prefixes of one bucket never share a mirror directory.
        assert_ne!(object("s3://b/a").slug(), object("s3://b/c").slug());
    }

    #[test]
    fn folder_markers_are_not_documents() {
        assert_eq!(classify_key(""), KeyVerdict::Skip(rules::FOLDER_MARKER));
        assert_eq!(
            classify_key("docs/"),
            KeyVerdict::Skip(rules::FOLDER_MARKER)
        );
    }

    #[test]
    fn traversal_and_unportable_keys_are_refused() {
        for key in [
            "../etc/passwd",
            "a/../../b",
            "./a",
            "a//b",
            "/abs",
            "a\\b",
            "a\tb",
            "trailing.",
            "trailing ",
            " leading",
            "dir/CON",
            "dir/nul.txt",
        ] {
            assert_eq!(
                classify_key(key),
                KeyVerdict::Skip(rules::UNSAFE_KEY),
                "{key} should be refused"
            );
        }
        let long = format!("dir/{}", "x".repeat(256));
        assert_eq!(classify_key(&long), KeyVerdict::Skip(rules::UNSAFE_KEY));
    }

    #[test]
    fn dotfiles_are_skipped_but_ignore_files_are_fetched() {
        assert_eq!(
            classify_key(".env"),
            KeyVerdict::Skip(crate::ignore_rules::HIDDEN_RULE)
        );
        assert_eq!(
            classify_key(".git/config"),
            KeyVerdict::Skip(crate::ignore_rules::HIDDEN_RULE)
        );
        assert_eq!(
            classify_key("src/.ssh/id_rsa"),
            KeyVerdict::Skip(crate::ignore_rules::HIDDEN_RULE)
        );
        assert_eq!(classify_key(".gitignore"), KeyVerdict::Admit);
        assert_eq!(classify_key("sub/.xerjignore"), KeyVerdict::Admit);
        // …but not an ignore file inside a hidden directory.
        assert_eq!(
            classify_key(".git/.gitignore"),
            KeyVerdict::Skip(crate::ignore_rules::HIDDEN_RULE)
        );
    }

    #[test]
    fn build_output_directories_cost_no_requests() {
        assert_eq!(
            classify_key("app/node_modules/react/index.js"),
            KeyVerdict::Skip("default:build-output")
        );
        assert_eq!(
            classify_key("target/debug/thing"),
            KeyVerdict::Skip("default:build-output")
        );
        // A FILE named like a build directory is still a document.
        assert_eq!(classify_key("notes/target"), KeyVerdict::Admit);
    }

    #[test]
    fn ordinary_keys_are_admitted() {
        for key in ["a.md", "docs/alpha.md", "a b/c-d.txt", "ünïcode/文書.txt"] {
            assert_eq!(classify_key(key), KeyVerdict::Admit, "{key}");
        }
    }

    #[test]
    fn local_source_lists_and_opens_the_same_tree_the_walk_does() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), b"beta").unwrap();
        std::fs::write(dir.path().join(".env"), b"SECRET=1").unwrap();
        let source = LocalDirSource::new(
            dir.path(),
            false,
            crate::ignore_rules::IgnoreOptions::default(),
        );
        let listing = source.list().unwrap();
        let mut rels: Vec<&str> = listing.entries.iter().map(|e| e.rel.as_str()).collect();
        rels.sort_unstable();
        assert_eq!(rels, vec!["a.txt", "sub/b.txt"], "dotfiles stay out");
        let entry = listing
            .entries
            .iter()
            .find(|e| e.rel == "sub/b.txt")
            .unwrap();
        let mut body = String::new();
        source
            .open(entry)
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert_eq!(body, "beta");
        // A local folder cannot answer "did this change" cheaply, and says so.
        assert!(entry.change_token.is_none());
    }

    #[test]
    fn local_source_refuses_to_read_outside_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let source = LocalDirSource::new(
            dir.path(),
            false,
            crate::ignore_rules::IgnoreOptions::default(),
        );
        let err = match source.open(&SourceEntry {
            rel: "../../etc/passwd".into(),
            size: 0,
            change_token: None,
            last_modified: None,
        }) {
            Ok(_) => panic!("a source must not read outside its own root"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("not a readable relative path"), "{err}");
    }
}
