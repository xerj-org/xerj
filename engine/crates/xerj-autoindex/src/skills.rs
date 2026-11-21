//! `xerj autoindex --skills` (#2644): find the folders agents keep installed
//! skills in and index each one with the ordinary pipeline, under its own
//! prefix below one stem (`skills-claude`, `skills-codex`, ...).
//!
//! One prefix still owns one folder (#1634): the resume journal, the state
//! directory and the catalog's dataset ids are all keyed on a single root.
//! `xerj search --prefix skills` searches `skills-*`, so the folders are
//! searched together without a second root per prefix.

use std::path::{Path, PathBuf};

use crate::cli::IndexCfg;

/// One skill folder that exists on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFolder {
    /// The prefix suffix: `claude` indexes under `<stem>-claude`.
    pub label: &'static str,
    pub dir: PathBuf,
}

/// Where agents keep skills, in the order they are indexed. The home folders
/// come first, so a project opened at the home directory is the `claude`
/// folder and not a second copy of it.
pub fn candidates(home: Option<&Path>, cwd: Option<&Path>) -> Vec<SkillFolder> {
    let mut out = Vec::new();
    if let Some(home) = home {
        for (label, dot) in [
            ("claude", ".claude"),
            ("codex", ".codex"),
            ("agents", ".agents"),
            ("gemini", ".gemini"),
        ] {
            out.push(SkillFolder {
                label,
                dir: home.join(dot).join("skills"),
            });
        }
        out.push(SkillFolder {
            label: "antigravity",
            dir: home.join(".gemini").join("config").join("skills"),
        });
    }
    if let Some(cwd) = cwd {
        out.push(SkillFolder {
            label: "project",
            dir: cwd.join(".claude").join("skills"),
        });
    }
    out
}

/// The candidates that are folders, each real folder once. A missing folder,
/// or a file where the folder would be, is skipped without a word: most
/// machines have one agent, not four.
pub fn discover(home: Option<&Path>, cwd: Option<&Path>) -> Vec<SkillFolder> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for folder in candidates(home, cwd) {
        let Ok(real) = folder.dir.canonicalize() else {
            continue;
        };
        if !real.is_dir() || seen.contains(&real) {
            continue;
        }
        seen.push(real);
        out.push(folder);
    }
    out
}

/// The run for one folder: the caller's flags, this folder as the root, and
/// `<stem>-<label>` as the prefix.
pub fn run_for(template: &IndexCfg, stem: &str, folder: &SkillFolder) -> IndexCfg {
    let mut cfg = template.clone();
    cfg.root = folder.dir.clone();
    cfg.prefix = format!("{stem}-{}", folder.label);
    // Every one of these folders is named `skills`, and the brain name
    // defaults to the folder name, so the runs would share one edge index.
    if cfg.brain.is_none() {
        cfg.brain = Some(cfg.prefix.clone());
    }
    // The project slot belongs to whichever project the command runs in.
    cfg.replace_prefix |= folder.label == "project";
    cfg
}

/// `xerj autoindex --skills`: index every skill folder found, in order.
pub fn run(template: IndexCfg) -> anyhow::Result<i32> {
    let home = xerj_common::home::home_dir();
    let cwd = std::env::current_dir().ok();
    let found = discover(home.as_deref(), cwd.as_deref());
    let stem = template.prefix.clone();
    if found.is_empty() {
        let mut looked: Vec<String> = Vec::new();
        for f in candidates(home.as_deref(), cwd.as_deref()) {
            let dir = f.dir.display().to_string();
            if !looked.contains(&dir) {
                looked.push(dir);
            }
        }
        anyhow::bail!(
            "--skills found no skill folder. Looked in: {}. To index skills kept somewhere \
             else: xerj autoindex <folder> --prefix {stem}-<name>",
            looked.join(", ")
        );
    }
    for f in &found {
        eprintln!("skills: {} -> --prefix {stem}-{}", f.dir.display(), f.label);
        if let Some(note) = symlink_note(&f.dir, template.follow_symlinks) {
            eprintln!("{note}");
        }
    }
    let roots =
        crate::staleness::fetch_folder_roots(&template.url, template.api_key.as_deref(), &stem);
    if let Some(note) = roots.as_ref().and_then(|r| overlap_note(r, &stem)) {
        eprintln!("{note}");
    }
    let runs: Vec<(SkillFolder, IndexCfg)> = found
        .into_iter()
        .map(|f| {
            let cfg = run_for(&template, &stem, &f);
            (f, cfg)
        })
        .collect();
    let codes: Vec<i32> = if template.watch {
        let stem = stem.clone();
        watch_all(runs, crate::watch::run, move |f, e| {
            eprintln!(
                "skills: the watch of {} (--prefix {stem}-{}) stopped:",
                f.dir.display(),
                f.label
            );
            report(e, false)
        })
    } else {
        // One folder after another: a failed folder is reported and the
        // others still run.
        runs.into_iter()
            .map(|(_, cfg)| {
                let json = cfg.json;
                crate::run_index(cfg).unwrap_or_else(|e| report(&e, json))
            })
            .collect()
    };
    Ok(combined_exit(&codes))
}

/// `--watch` over every folder: a watch never returns while its folder is
/// being watched, so each folder gets its own thread; Ctrl-C stops them all.
pub fn watch_all<W, R>(runs: Vec<(SkillFolder, IndexCfg)>, watch: W, report: R) -> Vec<i32>
where
    W: Fn(IndexCfg) -> anyhow::Result<i32> + Send + Sync + Clone + 'static,
    R: Fn(&SkillFolder, &anyhow::Error) -> i32 + Send + Sync + Clone + 'static,
{
    let handles: Vec<_> = runs
        .into_iter()
        .map(|(folder, cfg)| {
            let (watch, report) = (watch.clone(), report.clone());
            // Reported here and not at the join: the joins run in order, and
            // a folder still being watched holds up every join after it.
            std::thread::spawn(move || {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| watch(cfg))) {
                    Ok(Ok(code)) => code,
                    Ok(Err(e)) => report(&folder, &e),
                    Err(_) => report(&folder, &anyhow::anyhow!("the watch stopped unexpectedly")),
                }
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().unwrap_or(1)).collect()
}

fn report(error: &anyhow::Error, json: bool) -> i32 {
    let route = crate::route_cli_error(error, json);
    if let Some(stdout) = route.stdout {
        println!("{stdout}");
    }
    if let Some(stderr) = route.stderr {
        eprintln!("{stderr}");
    }
    route.exit_code
}

/// A note when the stem itself already holds a folder (the one-folder recipe
/// before `--skills`): `xerj search --prefix <stem>` searches that index too,
/// so a skill kept in both shows up twice.
pub fn overlap_note(
    roots: &std::collections::BTreeMap<String, String>,
    stem: &str,
) -> Option<String> {
    let root = crate::search::display_root(roots.get(stem)?);
    Some(format!(
        "note: --prefix {stem} already holds {root}, and `xerj search --prefix {stem}` searches \
         it beside these folders, so a skill in both shows up twice. If it is one of the \
         folders above: xerj autoindex remove --prefix {stem}"
    ))
}

/// A note when a skill folder holds linked entries, which are not followed
/// by default.
pub fn symlink_note(dir: &Path, follow: bool) -> Option<String> {
    if follow {
        return None;
    }
    let linked = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_symlink()))
        .count();
    (linked > 0).then(|| {
        format!(
            "note: {} holds {linked} linked {}, skipped because links are not \
             followed by default. To index them add --follow-symlinks \
             --follow-symlinks-outside-root (a link into a hidden folder outside it stays \
             refused)",
            dir.display(),
            if linked == 1 { "entry" } else { "entries" }
        )
    })
}

/// One exit code for several runs: an error beats a decision request, which
/// beats completed-with-junk, which beats a clean run.
pub fn combined_exit(codes: &[i32]) -> i32 {
    let rank = |code: i32| match code {
        0 => 0,
        3 => 1,
        4 => 2,
        _ => 3,
    };
    codes.iter().copied().max_by_key(|c| rank(*c)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdirs(root: &Path, rels: &[&str]) {
        for rel in rels {
            std::fs::create_dir_all(root.join(rel)).unwrap();
        }
    }

    fn labels(found: &[SkillFolder]) -> Vec<&'static str> {
        found.iter().map(|f| f.label).collect()
    }

    #[test]
    fn the_four_folders_the_issue_names_in_order() {
        let home = Path::new("/h");
        let cwd = Path::new("/p");
        let got: Vec<(&str, PathBuf)> = candidates(Some(home), Some(cwd))
            .into_iter()
            .map(|f| (f.label, f.dir))
            .collect();
        assert_eq!(
            got,
            vec![
                ("claude", home.join(".claude").join("skills")),
                ("codex", home.join(".codex").join("skills")),
                ("agents", home.join(".agents").join("skills")),
                ("gemini", home.join(".gemini").join("skills")),
                (
                    "antigravity",
                    home.join(".gemini").join("config").join("skills")
                ),
                ("project", cwd.join(".claude").join("skills")),
            ]
        );
        // No home (neither HOME nor USERPROFILE): only the project folder.
        assert_eq!(labels(&candidates(None, Some(cwd))), vec!["project"]);
    }

    #[test]
    fn missing_folders_are_skipped() {
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        mkdirs(home.path(), &[".claude/skills/pdf", ".agents/skills"]);
        mkdirs(proj.path(), &[".claude/skills/review"]);
        let found = discover(Some(home.path()), Some(proj.path()));
        assert_eq!(labels(&found), vec!["claude", "agents", "project"]);
        assert_eq!(found[2].dir, proj.path().join(".claude").join("skills"));
        // Nothing installed anywhere: nothing found, and no error.
        let empty = tempfile::tempdir().unwrap();
        assert!(discover(Some(empty.path()), Some(empty.path())).is_empty());
    }

    #[test]
    fn a_file_where_the_folder_would_be_is_not_a_skill_folder() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        std::fs::write(home.path().join(".codex").join("skills"), "x").unwrap();
        assert!(discover(Some(home.path()), None).is_empty());
    }

    #[test]
    fn a_project_opened_at_home_is_indexed_once() {
        let home = tempfile::tempdir().unwrap();
        mkdirs(home.path(), &[".claude/skills/pdf"]);
        let found = discover(Some(home.path()), Some(home.path()));
        assert_eq!(labels(&found), vec!["claude"]);
    }

    fn template() -> IndexCfg {
        match crate::cli::parse(["--skills"].map(String::from).to_vec()) {
            Ok(crate::cli::Cmd::Skills(cfg)) => *cfg,
            other => panic!("`--skills` should parse to Cmd::Skills, got {other:?}"),
        }
    }

    #[test]
    fn each_folder_runs_under_its_own_prefix() {
        let t = template();
        let claude = SkillFolder {
            label: "claude",
            dir: PathBuf::from("/h/.claude/skills"),
        };
        let cfg = run_for(&t, "skills", &claude);
        assert_eq!(cfg.prefix, "skills-claude");
        assert_eq!(cfg.root, claude.dir);
        assert!(!cfg.replace_prefix);
        // Every folder is named `skills`, so the graph name is the prefix,
        // not the folder name; one the caller chose is kept.
        assert_eq!(cfg.brain.as_deref(), Some("skills-claude"));
        let mut named = t.clone();
        named.brain = Some("mine".into());
        assert_eq!(
            run_for(&named, "skills", &claude).brain.as_deref(),
            Some("mine")
        );
        // The project slot follows the current project: indexing another
        // project's skills replaces the last one instead of being refused.
        let project = SkillFolder {
            label: "project",
            dir: PathBuf::from("/p/.claude/skills"),
        };
        let cfg = run_for(&t, "team", &project);
        assert_eq!(cfg.prefix, "team-project");
        assert!(cfg.replace_prefix);
    }

    #[test]
    fn the_worst_exit_code_wins() {
        assert_eq!(combined_exit(&[]), 0);
        assert_eq!(combined_exit(&[0, 0]), 0);
        assert_eq!(combined_exit(&[0, 3]), 3);
        assert_eq!(combined_exit(&[3, 4, 0]), 4);
        assert_eq!(combined_exit(&[4, 1, 3]), 1);
        assert_eq!(combined_exit(&[2, 0]), 2);
    }
    /// A folder whose watch cannot start (a second `--skills --watch`, say)
    /// must be reported while the other folders are still being watched,
    /// not after they stop, and the report must say which folder it was.
    #[test]
    fn a_folder_whose_watch_fails_is_reported_while_the_others_watch() {
        use std::sync::{mpsc, Arc, Mutex};
        let t = template();
        let runs: Vec<(SkillFolder, IndexCfg)> = ["claude", "codex"]
            .into_iter()
            .map(|label| {
                let f = SkillFolder {
                    label,
                    dir: PathBuf::from(format!("/h/.{label}/skills")),
                };
                let cfg = run_for(&t, "skills", &f);
                (f, cfg)
            })
            .collect();
        let (release, released) = mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        let watch = move |cfg: IndexCfg| -> anyhow::Result<i32> {
            if cfg.prefix == "skills-claude" {
                // A watch that started returns only when it is stopped.
                let _ = released.lock().unwrap().recv();
                Ok(0)
            } else {
                anyhow::bail!("another watch holds this folder")
            }
        };
        let (seen, reports) = mpsc::channel::<String>();
        let seen = Arc::new(Mutex::new(seen));
        let report = move |f: &SkillFolder, e: &anyhow::Error| {
            let _ = seen.lock().unwrap().send(format!("{}: {e}", f.label));
            1
        };
        let all = std::thread::spawn(move || watch_all(runs, watch, report));
        let first = reports.recv_timeout(std::time::Duration::from_secs(5));
        release.send(()).unwrap();
        let codes = all.join().unwrap();
        assert_eq!(
            first.expect("the codex error must be reported while claude is still watched"),
            "codex: another watch holds this folder"
        );
        assert_eq!(codes, vec![0, 1]);
    }

    /// Links in a skill folder are not followed by default, so a skill
    /// installed as a link would be missing without a word.
    #[cfg(unix)]
    #[test]
    fn linked_skills_get_one_note_naming_the_flags() {
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        mkdirs(home.path(), &[".claude/skills/pdf"]);
        mkdirs(elsewhere.path(), &["review", "deploy"]);
        let dir = home.path().join(".claude").join("skills");
        assert_eq!(symlink_note(&dir, false), None, "no links: no note");
        for name in ["review", "deploy"] {
            std::os::unix::fs::symlink(elsewhere.path().join(name), dir.join(name)).unwrap();
        }
        let note = symlink_note(&dir, false).expect("two links: a note");
        assert!(note.contains("2 linked"), "{note}");
        assert!(
            note.contains("--follow-symlinks --follow-symlinks-outside-root"),
            "{note}"
        );
        assert_eq!(symlink_note(&dir, true), None, "following links: no note");
    }

    /// Windows roots are stored as `\\?\C:\...`; the note prints the
    /// path a person would type.
    #[test]
    fn the_duplicate_note_prints_a_plain_windows_path() {
        let mut roots = std::collections::BTreeMap::new();
        roots.insert(
            "skills".to_string(),
            r"\\?\C:\Users\me\.claude\skills".to_string(),
        );
        let note = overlap_note(&roots, "skills").unwrap();
        assert!(note.contains(r"C:\Users\me\.claude\skills"), "{note}");
        assert!(!note.contains(r"\\?\"), "{note}");
    }

    #[test]
    fn an_index_under_the_bare_stem_gets_a_duplicate_note() {
        let mut roots = std::collections::BTreeMap::new();
        roots.insert("skills-claude".to_string(), "/h/.claude/skills".to_string());
        assert_eq!(overlap_note(&roots, "skills"), None, "own folders: no note");
        roots.insert("skills".to_string(), "/h/.claude/skills".to_string());
        let note = overlap_note(&roots, "skills").expect("bare stem: a note");
        assert!(note.contains("/h/.claude/skills"), "{note}");
        assert!(
            note.contains("xerj autoindex remove --prefix skills"),
            "{note}"
        );
    }
}
