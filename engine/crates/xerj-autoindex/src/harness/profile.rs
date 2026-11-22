//! Private state: per-stream rings (raw paths and tokens, 0600, purged 14
//! days after the stream ends), per-repo profiles (salted hashes only) and
//! the decisions log every replay and `status` reads.
use super::parse::Ev;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const RING: usize = 64;
const RING_BYTES: u64 = 192 * 1024;
pub const MAX_PAIRS: usize = 500;
pub const MAX_TERMS: usize = 2000;
pub const DECISIONS_ROTATE: u64 = 16 * 1024 * 1024;

pub fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(path)?;
    }
    Ok(())
}

fn open_private(path: &Path, append: bool) -> std::io::Result<fs::File> {
    let mut o = OpenOptions::new();
    o.create(true);
    if append {
        o.append(true);
    } else {
        o.write(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    o.open(path)
}

/// Write via temp + rename so a reader never sees half a file.
pub fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    private_dir(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("f"),
        std::process::id()
    ));
    {
        let mut f = open_private(&tmp, false)?;
        f.write_all(bytes)?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn append_line(path: &Path, line: &str) -> Result<()> {
    if let Some(d) = path.parent() {
        private_dir(d)?;
    }
    let mut f = open_private(path, true)?;
    // One write(2) under 4 KB is atomic for O_APPEND on local filesystems.
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line.as_bytes());
    buf.push(b'\n');
    f.write_all(&buf)?;
    Ok(())
}

pub fn hash(salt: &str, text: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update([0]);
    h.update(text.as_bytes());
    let d = h.finalize();
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn salt(root: &Path) -> String {
    let p = root.join("salt");
    if let Ok(s) = fs::read_to_string(&p) {
        if s.trim().len() >= 16 {
            return s.trim().to_string();
        }
    }
    let fresh = uuid::Uuid::new_v4().simple().to_string();
    let _ = atomic(&p, fresh.as_bytes());
    fresh
}

pub fn safe_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(80)
        .collect()
}

// ───────────────────────────── ring ─────────────────────────────

pub fn ring_path(root: &Path, key: &str) -> PathBuf {
    root.join("streams").join(format!("{key}.ring"))
}

pub fn read_ring(path: &Path) -> Vec<Ev> {
    let Ok(meta) = fs::metadata(path) else {
        return Vec::new();
    };
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let start = if meta.len() > RING_BYTES {
        bytes.len() - RING_BYTES as usize
    } else {
        0
    };
    let text = String::from_utf8_lossy(&bytes[start..]);
    let mut evs: Vec<Ev> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if evs.len() > RING {
        evs.drain(..evs.len() - RING);
    }
    evs
}

pub fn push_ring(path: &Path, ev: &Ev) -> Result<()> {
    append_line(path, &serde_json::to_string(ev)?)?;
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size > RING_BYTES / 2 {
        let keep = read_ring(path);
        let mut out = String::new();
        for e in keep {
            out.push_str(&serde_json::to_string(&e)?);
            out.push('\n');
        }
        atomic(path, out.as_bytes())?;
    }
    Ok(())
}

// ───────────────────────────── stream state ─────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Nudge {
    pub n: u64,
    pub rule: String,
    pub repo: String,
    pub paths: Vec<String>,
    pub resolved: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StreamState {
    pub n: u64,
    pub nudges: Vec<Nudge>,
    pub ignored: u32,
    pub tokens: u64,
    pub seen_tuids: Vec<String>,
    /// Intent->target pairs found so far: (repo id, target hash, term hashes).
    #[serde(default)]
    pub pairs: Vec<(String, String, Vec<String>)>,
    /// Searches whose next read/edit is already assigned.
    #[serde(default)]
    pub assigned: Vec<String>,
}

pub fn state_path(root: &Path, key: &str) -> PathBuf {
    root.join("streams").join(format!("{key}.state.json"))
}

pub fn read_state(path: &Path) -> StreamState {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Hot path (every tool call): overwrite in place, then shrink to length.
/// No rename and no truncate-to-zero, both of which make ext4 flush data
/// (`auto_da_alloc`) and stalled a busy disk 100+ ms per call in replay. A
/// torn write reads back as the default state, which is safe: soft state.
pub fn write_state(path: &Path, s: &StreamState) -> Result<()> {
    write_in_place(path, &serde_json::to_vec(s)?)
}

/// Overwrite a small soft-state file in place: no temp file, no rename, no
/// directory update. A rename-over on a busy ext4 disk stalled the hook
/// 0.4-1.7 s per call in replay; a torn write here only loses soft state.
pub fn write_in_place(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    if let Some(d) = path.parent() {
        private_dir(d)?;
    }
    let mut o = OpenOptions::new();
    o.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = o.open(path)?;
    f.seek(SeekFrom::Start(0))?;
    f.write_all(bytes)?;
    f.set_len(bytes.len() as u64)?;
    Ok(())
}

// ───────────────────────────── profile ─────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TermStat {
    pub w: f64,
    pub streams: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Pair {
    pub terms: HashMap<String, TermStat>,
    pub streams: u32,
    pub last: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuleStat {
    pub nudges: u32,
    pub uptake: u32,
    pub muted_until: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    pub updated: f64,
    /// Salted rel-path hash -> salted term hashes that led there.
    pub pairs: HashMap<String, Pair>,
    pub hot: HashMap<String, f64>,
    pub zero_terms: HashMap<String, f64>,
    pub rules: HashMap<String, RuleStat>,
    pub streams_folded: u32,
}

pub fn profile_path(root: &Path, repo: &str) -> PathBuf {
    root.join("repos").join(repo).join("profile.json")
}

pub fn read_profile(path: &Path) -> Profile {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Profile {
            version: 1,
            ..Profile::default()
        })
}

/// Stream end (`fold`): the whole profile, atomically.
pub fn write_profile(path: &Path, p: &Profile) -> Result<()> {
    atomic(path, &serde_json::to_vec(p)?)
}

/// Hot path (a firing call, a nudge resolving): per-rule counters only,
/// written in place beside the profile (BUILD v2 step 6: no rename-over-self
/// on the per-repo profile while the agent waits).
pub fn rules_path(profile: &Path) -> PathBuf {
    profile.with_file_name("rules.json")
}

pub fn read_rules(profile: &Path) -> HashMap<String, RuleStat> {
    fs::read(rules_path(profile))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn write_rules(profile: &Path, rules: &HashMap<String, RuleStat>) -> Result<()> {
    write_in_place(&rules_path(profile), &serde_json::to_vec(rules)?)
}

impl Profile {
    /// Rel-path hash -> (prior, streams) for the chain's term hashes. Only a
    /// term->target pair seen in >=2 streams counts (design §5.6).
    pub fn prior(&self, term_hashes: &[String], min_streams: u32) -> HashMap<String, (f64, u32)> {
        let mut out = HashMap::new();
        for (target, pair) in &self.pairs {
            let mut w = 0.0;
            let mut s = 0u32;
            let mut matched = 0;
            for th in term_hashes {
                if let Some(t) = pair.terms.get(th) {
                    if t.streams >= min_streams {
                        w += t.w;
                        s = s.max(t.streams);
                        matched += 1;
                    }
                }
            }
            if matched > 0 {
                out.insert(target.clone(), (w, s));
            }
        }
        out
    }

    pub fn decay(&mut self, now: f64, half_life_days: f64) {
        let dt = (now - self.updated).max(0.0);
        if self.updated > 0.0 && dt > 0.0 {
            let f = 0.5f64.powf(dt / (half_life_days * 86400.0));
            for p in self.pairs.values_mut() {
                for t in p.terms.values_mut() {
                    t.w *= f;
                }
            }
            for v in self.hot.values_mut() {
                *v *= f;
            }
            for v in self.zero_terms.values_mut() {
                *v *= f;
            }
        }
        self.updated = now;
    }

    pub fn cap(&mut self) {
        if self.pairs.len() > MAX_PAIRS {
            let mut v: Vec<(String, f64)> = self
                .pairs
                .iter()
                .map(|(k, p)| (k.clone(), p.terms.values().map(|t| t.w).sum()))
                .collect();
            v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let keep: HashSet<String> = v.into_iter().take(MAX_PAIRS).map(|x| x.0).collect();
            self.pairs.retain(|k, _| keep.contains(k));
        }
        let terms: usize = self.pairs.values().map(|p| p.terms.len()).sum();
        if terms > MAX_TERMS {
            let per = (MAX_TERMS / self.pairs.len().max(1)).max(4);
            for p in self.pairs.values_mut() {
                if p.terms.len() > per {
                    let mut v: Vec<(String, TermStat)> = p.terms.drain().collect();
                    v.sort_by(|a, b| {
                        b.1.w
                            .partial_cmp(&a.1.w)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    p.terms = v.into_iter().take(per).collect();
                }
            }
        }
        for m in [&mut self.hot, &mut self.zero_terms] {
            if m.len() > MAX_TERMS {
                let mut v: Vec<(String, f64)> = m.drain().collect();
                v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                *m = v.into_iter().take(MAX_TERMS).collect();
            }
        }
    }
}

// ───────────────────────────── repo identity ─────────────────────────────

/// (worktree root, repo id). The id is the hash of the git common dir, so
/// worktrees of one repository share a profile. Outside git: the dir itself.
pub fn repo_of(dir: &Path) -> Option<(PathBuf, String)> {
    for anc in dir.ancestors() {
        let dot = anc.join(".git");
        let Ok(meta) = fs::symlink_metadata(&dot) else {
            continue;
        };
        let common = if meta.is_dir() {
            dot.clone()
        } else {
            let text = fs::read_to_string(&dot).ok()?;
            let gitdir = text
                .lines()
                .find_map(|l| l.strip_prefix("gitdir:"))?
                .trim()
                .to_string();
            let gd = super::parse::normalize(anc, &gitdir);
            match fs::read_to_string(gd.join("commondir")) {
                Ok(c) => super::parse::normalize(&gd, c.trim()),
                Err(_) => gd,
            }
        };
        let id = hash("repo", &common.display().to_string());
        return Some((anc.to_path_buf(), id));
    }
    None
}

pub fn rel_to(root: &Path, abs: &str) -> Option<String> {
    Path::new(abs)
        .strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}
