//! Startup diagnostics only; neither discovers nor chooses a different data directory.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub(crate) struct Origin {
    pub source: String,
    pub requested: String,
}

pub(crate) struct Report {
    pub path: PathBuf,
    pub source: String,
    pub created: bool,
}

impl Report {
    pub fn path_text(&self) -> String {
        self.path.display().to_string().escape_debug().to_string()
    }

    pub fn status(&self) -> &'static str {
        if self.created {
            "created this launch"
        } else {
            "existing directory (may be empty)"
        }
    }
}

pub(crate) const NEW_DIRECTORY_HINT: &str = "New data directory: if you expected existing indices or an owner account, restart with your previous --data-dir or --config. This node does not search for or migrate another directory.";

pub(crate) fn prepare(path: &Path, origin: Origin) -> Result<Report> {
    let selected = path;
    // create_dir_all("") previously accepted the current directory.
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    // create_dir_all cannot say who created the leaf. A single create_dir
    // distinguishes our creation from an existing/concurrently supplied dir.
    // See std::fs::create_dir; parent creation preserves the prior nested-path workflow.
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create data dir parent {}", parent.display()))?;
    }
    let created = match std::fs::create_dir(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => false,
        Err(e) => return Err(e).with_context(|| format!("create data dir {}", path.display())),
    };
    // Resolving a display path must not become a new startup dependency.
    let resolved = std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    let mut source = origin.source;
    if selected != Path::new(&origin.requested) {
        source.push_str("; auto-port selected an available sibling directory");
    }
    Ok(Report {
        path: resolved,
        source,
        created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(path: &Path) -> Origin {
        Origin {
            source: "--data-dir".into(),
            requested: path.display().to_string(),
        }
    }

    #[test]
    fn creates_nested_directory_then_preserves_existing_contents() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested/data");
        assert!(prepare(&path, origin(&path)).unwrap().created);
        let sentinel = path.join("operator-sentinel");
        std::fs::write(&sentinel, "keep").unwrap();
        let report = prepare(&path, origin(&path)).unwrap();
        assert!(!report.created);
        assert!(report.path.is_absolute());
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "keep");
    }

    #[test]
    fn refuses_file_without_replacing_it_and_recovers_at_a_new_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data");
        std::fs::write(&path, "keep").unwrap();
        assert!(prepare(&path, origin(&path)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
        let recovery = temp.path().join("new-data");
        assert!(prepare(&recovery, origin(&recovery)).unwrap().created);
    }

    #[test]
    fn relocation_is_explicit_and_control_characters_are_escaped() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected");
        let report = prepare(&path, origin(&temp.path().join("requested"))).unwrap();
        assert!(report.source.contains("auto-port"));
        let display = Report {
            path: PathBuf::from("line\nnext"),
            source: String::new(),
            created: false,
        };
        assert!(!display.path_text().contains('\n'));
    }

    #[cfg(unix)]
    #[test]
    fn existing_symlink_directory_is_reused_and_resolved_for_display() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let report = prepare(&link, origin(&link)).unwrap();
        assert!(!report.created);
        assert_eq!(report.path, std::fs::canonicalize(target).unwrap());
    }
}
