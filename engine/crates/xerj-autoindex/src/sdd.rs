//! Spec-driven development (SDD) layout detection and corpus routing (#2966).
//!
//! When `xerj autoindex` sniffs an OpenSpec (`Fission-AI/OpenSpec`) or
//! `github/spec-kit` (`github/spec-kit`) project in a repository scope, it:
//! 1. Admits `.specify/` during the folder walk when `.specify/` has a real
//!    spec-kit directory structure (`.specify/memory`, `.specify/specs`,
//!    `.specify/templates`, or `.specify/scripts`), while keeping the general
//!    dotfile/dot-directory rule enforced for everything else (including any
//!    dotfile inside `.specify/`).
//! 2. Routes SDD requirements, proposals, constitutions, and ADRs
//!    (`openspec/**`, `.specify/**`, `specs/**`, `adr/**`, `adrs/**`,
//!    `docs/adr/**`, `docs/adrs/**`, `docs/decisions/**`, `docs/specs/**`,
//!    `rfcs/**`) into a first-class `<prefix>-specs` document corpus (or
//!    `<prefix>-<scope>-specs` for nested repositories) separate from the
//!    general `<prefix>-docs` source/prose corpus.
//! 3. Prints a one-line recipe showing how coding agents query the dedicated
//!    specs corpus with file-and-line citations.

use std::fs;
use std::path::Path;

/// Detected spec-driven development layout for a repository scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SddKind {
    /// `Fission-AI/OpenSpec` layout (`openspec/project.md`, `openspec/AGENTS.md`,
    /// `openspec/specs/`, or `openspec/changes/`).
    OpenSpec,
    /// `github/spec-kit` layout (`.specify/` with `memory`/`specs`/`templates`
    /// or `specs/<feature>/spec.md` / `plan.md` / `tasks.md`).
    SpecKit,
    /// Both `OpenSpec` and `github/spec-kit` markers are present.
    Both,
}

impl SddKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SddKind::OpenSpec => "openspec",
            SddKind::SpecKit => "spec-kit",
            SddKind::Both => "openspec+spec-kit",
        }
    }
}

/// Canonical scope suffix used to distinguish a scope's dedicated SDD `specs`
/// corpus from its general `docs` corpus.
pub const SPECS_SCOPE_NAME: &str = "specs";

/// Returns `true` when `dir` is a `.specify` directory with at least one
/// canonical `github/spec-kit` sub-entry (`memory`, `specs`, `templates`, or
/// `scripts`).
///
/// Checking for the sub-entries ensures that an unrelated hidden file or folder
/// is never admitted by accident, and the walker still prunes any dot-prefixed
/// file or directory *inside* `.specify` (such as `.specify/.env`).
pub fn is_specify_dir(dir: &Path) -> bool {
    if dir.file_name().and_then(|n| n.to_str()) != Some(".specify") || !dir.is_dir() {
        return false;
    }
    ["memory", "specs", "templates", "scripts"]
        .iter()
        .any(|sub| dir.join(sub).is_dir())
}

/// Returns `true` when `scope_dir` contains an OpenSpec (`openspec/`) layout.
pub fn has_openspec_layout(scope_dir: &Path) -> bool {
    let openspec = scope_dir.join("openspec");
    if !openspec.is_dir() {
        return false;
    }
    openspec.join("project.md").is_file()
        || openspec.join("AGENTS.md").is_file()
        || openspec.join("config.yaml").is_file()
        || openspec.join("specs").is_dir()
        || openspec.join("changes").is_dir()
}

/// Returns `true` when `scope_dir` contains a `github/spec-kit` layout
/// (`.specify/` or `specs/<feature>/{spec.md,plan.md,tasks.md}`).
pub fn has_speckit_layout(scope_dir: &Path) -> bool {
    if is_specify_dir(&scope_dir.join(".specify")) {
        return true;
    }
    let specs = scope_dir.join("specs");
    if !specs.is_dir() {
        return false;
    }
    if specs.join("constitution.md").is_file() || specs.join("spec.md").is_file() {
        return true;
    }
    let Ok(entries) = fs::read_dir(&specs) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && (path.join("spec.md").is_file()
                || path.join("plan.md").is_file()
                || path.join("tasks.md").is_file())
        {
            return true;
        }
    }
    false
}

/// Detect whether `scope_dir` uses OpenSpec, `github/spec-kit`, or both.
pub fn detect_sdd_layout(scope_dir: &Path) -> Option<SddKind> {
    let openspec = has_openspec_layout(scope_dir);
    let speckit = has_speckit_layout(scope_dir);
    match (openspec, speckit) {
        (true, true) => Some(SddKind::Both),
        (true, false) => Some(SddKind::OpenSpec),
        (false, true) => Some(SddKind::SpecKit),
        (false, false) => None,
    }
}

/// Returns `true` when a repository-relative path (`/`-separated) belongs to
/// the SDD specs/ADR corpus in a detected OpenSpec or `github/spec-kit` repo.
pub fn is_sdd_spec_rel(rel_in_repo: &str) -> bool {
    let rel = rel_in_repo.trim_start_matches('/');
    const TOP_DIRS: &[&str] = &["openspec/", ".specify/", "specs/", "adr/", "adrs/", "rfcs/"];
    if TOP_DIRS.iter().any(|p| rel.starts_with(p)) {
        return true;
    }
    const DOCS_SUBDIRS: &[&str] = &["docs/adr/", "docs/adrs/", "docs/decisions/", "docs/specs/"];
    DOCS_SUBDIRS.iter().any(|p| rel.starts_with(p))
}

/// Construct the synthetic SDD scope string for a repository `repo_scope`.
///
/// Root repository (`""`) -> `"specs"`.
/// Nested repository (`"services/auth"`) -> `"services/auth/specs"`.
pub fn specs_scope_for(repo_scope: &str) -> String {
    if repo_scope.is_empty() {
        SPECS_SCOPE_NAME.to_string()
    } else {
        format!("{repo_scope}/{SPECS_SCOPE_NAME}")
    }
}

/// Split a scope string into `Some(repo_scope)` if it is a synthetic SDD
/// `specs` scope (`"specs"` -> `Some("")`, `"sub/specs"` -> `Some("sub")`).
pub fn parse_specs_scope(scope: &str) -> Option<&str> {
    if scope == SPECS_SCOPE_NAME {
        Some("")
    } else {
        scope.strip_suffix("/specs")
    }
}

/// Compute `reconcile_plan` scope affinity when `scope` is an SDD `specs`
/// scope (`"specs"` or `"{repo}/specs"`).
///
/// Returns `Some(repo_scope.len() + 6)` when `rel` lives inside `repo_scope`
/// and satisfies [`is_sdd_spec_rel`], so the `specs` dataset strictly outranks
/// the enclosing repository's general `docs` dataset (`Some(repo_scope.len())`)
/// for SDD files and returns `None` for non-SDD files.
pub fn sdd_scope_affinity(scope: &str, rel: &str) -> Option<usize> {
    let repo_scope = parse_specs_scope(scope)?;
    let rel_in_repo = if repo_scope.is_empty() {
        rel
    } else {
        rel.strip_prefix(repo_scope)?.strip_prefix('/')?
    };
    if is_sdd_spec_rel(rel_in_repo) {
        Some(repo_scope.len() + 6)
    } else {
        None
    }
}

/// Render a one-line search recipe hint for coding agents when a run indexed
/// a dedicated SDD `specs` corpus.
pub fn specs_recipe_hint(url: &str, prefix: &str) -> String {
    format!(
        "specs: search requirements and ADRs with citations via `xerj search --url {url} --prefix {prefix}-specs \"<requirement or ADR>\"`"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_openspec_and_speckit_layouts() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(detect_sdd_layout(tmp.path()), None);

        // OpenSpec layout
        let openspec_specs = tmp.path().join("openspec").join("specs").join("auth");
        std::fs::create_dir_all(&openspec_specs).unwrap();
        std::fs::write(openspec_specs.join("spec.md"), "# Auth Spec\n").unwrap();
        assert_eq!(detect_sdd_layout(tmp.path()), Some(SddKind::OpenSpec));

        // Add github/spec-kit .specify layout -> Both
        let specify_mem = tmp.path().join(".specify").join("memory");
        std::fs::create_dir_all(&specify_mem).unwrap();
        std::fs::write(specify_mem.join("constitution.md"), "# Constitution\n").unwrap();
        assert!(is_specify_dir(&tmp.path().join(".specify")));
        assert_eq!(detect_sdd_layout(tmp.path()), Some(SddKind::Both));
    }

    #[test]
    fn classifies_sdd_paths_and_reconcile_affinity() {
        assert!(is_sdd_spec_rel("openspec/specs/auth/spec.md"));
        assert!(is_sdd_spec_rel(".specify/memory/constitution.md"));
        assert!(is_sdd_spec_rel("specs/001-login/spec.md"));
        assert!(is_sdd_spec_rel("docs/adr/0001-record-architecture.md"));
        assert!(is_sdd_spec_rel("docs/decisions/002-storage.md"));
        assert!(is_sdd_spec_rel("rfcs/0003-query.md"));
        assert!(!is_sdd_spec_rel("src/lib.rs"));
        assert!(!is_sdd_spec_rel("docs/getting-started.md"));

        assert_eq!(
            sdd_scope_affinity("specs", "openspec/specs/auth/spec.md"),
            Some(6)
        );
        assert_eq!(sdd_scope_affinity("specs", "src/lib.rs"), None);
        assert_eq!(
            sdd_scope_affinity("sub/repo/specs", "sub/repo/specs/001/spec.md"),
            Some("sub/repo".len() + 6)
        );
        assert_eq!(
            sdd_scope_affinity("sub/repo/specs", "sub/repo/src/main.rs"),
            None
        );
    }
}
