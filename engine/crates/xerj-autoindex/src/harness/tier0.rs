//! Tier 0: an in-process multi-token scan of the repo, budgeted. Works on
//! the first session in a new repo; no node, no index. ripgrep's `ignore`
//! walker (gitignore semantics) plus one case-insensitive alternation that
//! the regex crate compiles to a literal multi-pattern searcher.
use regex::bytes::{Regex, RegexBuilder};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_FILE: u64 = 1024 * 1024;
const SECRET_NAMES: &[&str] = &[
    ".env",
    ".pem",
    ".key",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    ".p12",
    ".pfx",
];

#[derive(Clone, Debug)]
pub struct Hit {
    pub abs: PathBuf,
    pub rel: String,
    pub line: usize,
    pub text: String,
    /// Distinct chain word-parts this file covers.
    pub covered: usize,
    pub def: bool,
    pub score: f64,
    /// History prior: streams in which this file ended a matching chain.
    pub hist: u32,
    pub seen: bool,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub hits: Vec<Hit>,
    pub files: usize,
    pub complete: bool,
    pub absent: Vec<String>,
    pub elapsed_ms: f64,
}

pub struct Query<'a> {
    pub root: &'a Path,
    pub toks: &'a [String],
    pub idents: &'a [String],
    pub seen: &'a HashSet<String>,
    /// Salted rel-path hash -> (prior weight, streams).
    pub hist: &'a HashMap<String, (f64, u32)>,
    pub hasher: &'a (dyn Fn(&str) -> String + Sync),
    pub budget: Duration,
}

fn secret(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(".env") || SECRET_NAMES.iter().any(|s| lower.ends_with(s))
}

pub fn looks_def(line: &str) -> bool {
    let t = line.trim_start();
    const KW: &[&str] = &[
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "async fn ",
        "pub async fn ",
        "def ",
        "async def ",
        "class ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "impl ",
        "impl<",
        "func ",
        "function ",
        "export function ",
        "export class ",
        "export const ",
        "interface ",
        "type ",
        "pub type ",
        "const ",
        "pub const ",
        "static ",
        "module ",
        "package ",
        "public ",
        "private ",
        "protected ",
        "void ",
        "#define ",
        "local function ",
    ];
    KW.iter().any(|k| t.starts_with(k))
        || (t.contains("::") && t.contains('(') && !t.ends_with(';') && !t.starts_with("//"))
}

struct FileRes {
    rel: String,
    abs: PathBuf,
    mask: u64,
    idents: usize,
    line: usize,
    text: String,
    def: bool,
    path_toks: usize,
    hist: Option<(f64, u32)>,
}

fn build(terms: &[String]) -> Option<Regex> {
    if terms.is_empty() {
        return None;
    }
    let mut sorted: Vec<&String> = terms.iter().collect();
    sorted.sort_by_key(|t| std::cmp::Reverse(t.len()));
    let alt = sorted
        .iter()
        .map(|t| regex::escape(t))
        .collect::<Vec<_>>()
        .join("|");
    RegexBuilder::new(&alt)
        .case_insensitive(true)
        .unicode(false)
        .build()
        .ok()
}

pub fn scan(q: &Query) -> Scan {
    let start = Instant::now();
    let deadline = start + q.budget;
    // Terms: word parts first (bit i), then whole identifiers.
    let mut terms: Vec<String> = q.toks.iter().take(40).cloned().collect();
    let nt = terms.len();
    for id in q.idents.iter().take(20) {
        if !terms.contains(id) {
            terms.push(id.clone());
        }
    }
    let index: HashMap<String, usize> = terms
        .iter()
        .enumerate()
        .map(|(i, t)| (t.clone(), i))
        .collect();
    // Identifier -> the word-part bits it implies.
    let ident_bits: Vec<u64> = terms
        .iter()
        .map(|t| {
            terms[..nt]
                .iter()
                .enumerate()
                .filter(|(_, p)| t.contains(p.as_str()))
                .fold(0u64, |m, (i, _)| m | (1 << i))
        })
        .collect();
    let Some(re) = build(&terms) else {
        return Scan::default();
    };
    let files = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let results: Mutex<Vec<FileRes>> = Mutex::new(Vec::new());
    let df: Mutex<Vec<usize>> = Mutex::new(vec![0; terms.len()]);
    let threads = xerj_common::resource::cores().clamp(1, 8);
    let walker = ignore::WalkBuilder::new(q.root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .require_git(false)
        .max_filesize(Some(MAX_FILE))
        .threads(threads)
        .build_parallel();
    let root = q.root;
    walker.run(|| {
        let re = &re;
        let index = &index;
        let ident_bits = &ident_bits;
        let files = &files;
        let aborted = &aborted;
        let results = &results;
        let df = &df;
        let terms = &terms;
        Box::new(move |entry| {
            if Instant::now() > deadline {
                aborted.store(true, Ordering::Relaxed);
                return ignore::WalkState::Quit;
            }
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return ignore::WalkState::Continue;
            }
            let name = entry.file_name().to_string_lossy();
            if secret(&name) {
                return ignore::WalkState::Continue;
            }
            files.fetch_add(1, Ordering::Relaxed);
            let path = entry.path();
            let Ok(bytes) = std::fs::read(path) else {
                return ignore::WalkState::Continue;
            };
            if memchr::memchr(0, &bytes[..bytes.len().min(8192)]).is_some() {
                return ignore::WalkState::Continue;
            }
            let mut mask = 0u64;
            let mut idmask = 0u64;
            let mut line_no = 1usize;
            let mut last = 0usize;
            let mut cur_line_mask = 0u64;
            let mut cur_line = 1usize;
            let mut best = (0u32, false, 0usize, 0usize);
            let flush_line =
                |ln: usize, lm: u64, pos: usize, best: &mut (u32, bool, usize, usize)| {
                    if lm == 0 {
                        return;
                    }
                    let ls = memchr::memrchr(b'\n', &bytes[..pos]).map_or(0, |i| i + 1);
                    let le = memchr::memchr(b'\n', &bytes[pos..]).map_or(bytes.len(), |i| pos + i);
                    let text = String::from_utf8_lossy(&bytes[ls..le]);
                    let def = looks_def(&text);
                    let pc = lm.count_ones();
                    if (pc, def) > (best.0, best.1) {
                        *best = (pc, def, ln, ls);
                    }
                };
            let mut last_pos = 0usize;
            for m in re.find_iter(&bytes) {
                line_no += memchr::memchr_iter(b'\n', &bytes[last..m.start()]).count();
                last = m.start();
                if line_no != cur_line {
                    flush_line(cur_line, cur_line_mask, last_pos, &mut best);
                    cur_line = line_no;
                    cur_line_mask = 0;
                }
                let lower = String::from_utf8_lossy(m.as_bytes()).to_ascii_lowercase();
                if let Some(&i) = index.get(&lower) {
                    let bits = if i < nt { 1u64 << i } else { ident_bits[i] };
                    mask |= bits;
                    cur_line_mask |= bits;
                    if i >= nt {
                        idmask |= 1 << (i - nt);
                    }
                }
                last_pos = m.start();
            }
            flush_line(cur_line, cur_line_mask, last_pos, &mut best);
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            let rel_lower = rel.to_ascii_lowercase();
            let path_toks = terms[..nt]
                .iter()
                .filter(|t| rel_lower.contains(t.as_str()))
                .count();
            let hist = if q.hist.is_empty() {
                None
            } else {
                q.hist.get(&(q.hasher)(&rel)).copied()
            };
            {
                let mut d = df.lock().unwrap_or_else(|e| e.into_inner());
                for (i, slot) in d.iter_mut().enumerate() {
                    let hit = if i < nt {
                        mask & (1 << i) != 0
                    } else {
                        idmask & (1 << (i - nt)) != 0
                    };
                    if hit {
                        *slot += 1;
                    }
                }
            }
            if mask != 0 || hist.is_some() {
                let (ls, ln) = (best.3, best.2);
                let le = memchr::memchr(b'\n', &bytes[ls..]).map_or(bytes.len(), |i| ls + i);
                let text = if mask != 0 {
                    String::from_utf8_lossy(&bytes[ls..le]).trim().to_string()
                } else {
                    String::new()
                };
                results
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(FileRes {
                        rel,
                        abs: path.to_path_buf(),
                        mask,
                        idents: idmask.count_ones() as usize,
                        line: if mask != 0 { ln } else { 1 },
                        text,
                        def: best.1,
                        path_toks,
                        hist,
                    });
            }
            ignore::WalkState::Continue
        })
    });
    let n = files.load(Ordering::Relaxed).max(1);
    let df = df.into_inner().unwrap_or_else(|e| e.into_inner());
    let idf: Vec<f64> = df
        .iter()
        .map(|&d| (1.0 + n as f64 / (d.max(1) as f64)).ln())
        .collect();
    let complete = !aborted.load(Ordering::Relaxed);
    let absent = if complete {
        terms[nt..]
            .iter()
            .zip(&df[nt..])
            .filter(|(_, d)| **d == 0)
            .map(|(t, _)| t.clone())
            .collect()
    } else {
        Vec::new()
    };
    let mut hits: Vec<Hit> = results
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .map(|r| {
            let mut score: f64 = (0..nt)
                .filter(|i| r.mask & (1 << i) != 0)
                .map(|i| idf[i])
                .sum();
            score +=
                r.idents as f64 * 1.0 + 0.5 * r.path_toks as f64 + if r.def { 0.5 } else { 0.0 };
            let (hw, hs) = r.hist.unwrap_or((0.0, 0));
            if hs >= 2 {
                score += 2.0 * hw.min(3.0);
            }
            let abs_s = r.abs.display().to_string();
            Hit {
                seen: q.seen.contains(&abs_s),
                abs: r.abs,
                rel: r.rel,
                line: r.line,
                text: r.text,
                covered: r.mask.count_ones() as usize,
                def: r.def,
                score,
                hist: hs,
            }
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.rel.cmp(&b.rel))
    });
    hits.truncate(50);
    Scan {
        hits,
        files: files.load(Ordering::Relaxed),
        complete,
        absent,
        elapsed_ms: start.elapsed().as_secs_f64() * 1e3,
    }
}

/// Re-read the cited line now; a line that no longer carries a chain term
/// is dropped (a stale `path:line` is a hard fail, design §5.7).
pub fn reanchor(abs: &Path, line: usize, terms: &[String]) -> bool {
    let Ok(bytes) = std::fs::read(abs) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    let Some(l) = text.lines().nth(line.saturating_sub(1)) else {
        return false;
    };
    let l = l.to_ascii_lowercase();
    terms.iter().any(|t| l.contains(t.as_str()))
}
