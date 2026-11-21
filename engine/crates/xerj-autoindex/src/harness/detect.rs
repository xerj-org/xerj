//! Strict struggle detection over one stream's ring. Fixed, pre-registered
//! rules (design §5.2-5.3); nothing here learns.
use super::parse::{is_stop, Ev, Probe};
use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Second zero-result probe in one chain.
    R1,
    /// Third search in a chain, no earlier output led to a read.
    R2,
    /// A capped search hit its cap, then a narrower search in the chain.
    R3,
    /// Three reads of different files under one root after a search, no edit.
    R4,
    /// `Binary file … matches` (shadow only).
    R5,
}

impl Rule {
    pub fn name(self) -> &'static str {
        match self {
            Rule::R1 => "R1",
            Rule::R2 => "R2",
            Rule::R3 => "R3",
            Rule::R4 => "R4",
            Rule::R5 => "R5",
        }
    }
    /// R4/R5 never speak in v1.
    pub fn active(self) -> bool {
        matches!(self, Rule::R1 | Rule::R2 | Rule::R3)
    }
}

pub const CHAIN_MAX_CALLS: u64 = 6;
pub const CHAIN_MAX_SECS: f64 = 300.0;

#[derive(Clone, Debug)]
pub struct Trigger {
    pub rule: Rule,
    pub calls: usize,
    pub zeros: usize,
    pub toks: Vec<String>,
    pub idents: Vec<String>,
    pub root: String,
    /// Files already shown or read in this stream: never re-suggested.
    pub seen: HashSet<String>,
}

const OPS_ROOTS: &[&str] = &["/proc", "/sys", "/dev", "/var/log", "/etc", "/run"];

/// Shared preconditions: tree scope, code domain, a pattern with content.
pub fn eligible(p: &Probe, remote: bool) -> Result<(), &'static str> {
    if remote {
        return Err("remote");
    }
    if !p.tree || p.root.is_empty() {
        return Err("not_tree");
    }
    if OPS_ROOTS
        .iter()
        .any(|r| p.root == *r || p.root.starts_with(&format!("{r}/")))
        || p.root == "/"
    {
        return Err("ops_root");
    }
    if matches!(p.class.as_str(), "regex" | "shell_var" | "none") {
        return Err("pattern_class");
    }
    if p.toks.is_empty() && p.idents.is_empty() {
        return Err("no_tokens");
    }
    Ok(())
}

/// A token that can link two searches: >= 3 chars, not a stop-word
/// (amendment A4: 3-letter domain words such as wal/lod/rpc are specific).
fn specific(t: &str) -> bool {
    t.len() >= 3 && !is_stop(t)
}

fn stem(s: &str) -> &str {
    &s[..s.len().min(6)]
}

fn related(a: &Probe, b: &Probe) -> bool {
    let shares = a.toks.iter().any(|t| specific(t) && b.toks.contains(t))
        || a.idents
            .iter()
            .any(|x| x.len() >= 6 && b.idents.iter().any(|y| y.len() >= 6 && stem(x) == stem(y)));
    let (ra, rb) = (Path::new(&a.root), Path::new(&b.root));
    shares && (ra.starts_with(rb) || rb.starts_with(ra))
}

fn narrower(now: &Probe, before: &Probe) -> bool {
    let (rn, rb) = (Path::new(&now.root), Path::new(&before.root));
    (rn != rb && rn.starts_with(rb))
        || (now.toks.len() > before.toks.len() && before.toks.iter().all(|t| now.toks.contains(t)))
        || (now.include && !before.include)
        || (now.idents.len() > before.idents.len())
}

pub fn seen_in(ring: &[Ev]) -> HashSet<String> {
    let mut seen = HashSet::new();
    for e in ring {
        seen.extend(e.shown.iter().cloned());
        seen.extend(e.reads.iter().cloned());
        seen.extend(e.edits.iter().cloned());
    }
    seen
}

/// Evaluate the newest event of `ring`. Returns the trigger, or the reason
/// for staying silent (logged; R-1 silence is computed from these).
pub fn detect(ring: &[Ev]) -> Result<Trigger, &'static str> {
    let Some(cur) = ring.last() else {
        return Err("empty");
    };
    let ci = ring.len() - 1;
    if !cur.is_search() {
        return detect_r4(ring).ok_or("no_search");
    }
    let mut last_reason = "not_eligible";
    let mut best: Option<Trigger> = None;
    for p in cur.probes.iter().rev() {
        if let Err(r) = eligible(p, cur.remote) {
            last_reason = r;
            continue;
        }
        // Walk back: members are (event index, probe).
        let mut members: Vec<(usize, &Probe)> = vec![(ci, p)];
        let mut last = ci;
        for e in (0..ci).rev() {
            let ev = &ring[e];
            if ring[last].n.saturating_sub(ev.n) > CHAIN_MAX_CALLS
                || ring[last].t - ev.t > CHAIN_MAX_SECS
            {
                break;
            }
            if !ev.is_search() {
                continue;
            }
            let rel: Vec<&Probe> = ev
                .probes
                .iter()
                .filter(|q| {
                    eligible(q, ev.remote).is_ok() && members.iter().any(|(_, m)| related(m, q))
                })
                .collect();
            if rel.is_empty() {
                continue;
            }
            // A file this search showed was read later: it found something.
            let found = ring[e + 1..=ci].iter().any(|later| {
                later
                    .reads
                    .iter()
                    .chain(&later.edits)
                    .any(|r| ev.shown.contains(r))
            });
            if found {
                break;
            }
            for q in rel {
                members.push((e, q));
            }
            last = e;
        }
        let calls = members
            .iter()
            .map(|(e, _)| *e)
            .collect::<HashSet<_>>()
            .len();
        let zeros = members.iter().filter(|(_, m)| m.zero == Some(true)).count();
        let mut toks: Vec<String> = Vec::new();
        let mut idents: Vec<String> = Vec::new();
        for (_, m) in members.iter().rev() {
            for t in &m.toks {
                if !toks.contains(t) {
                    toks.push(t.clone());
                }
            }
            for t in &m.idents {
                if !idents.contains(t) {
                    idents.push(t.clone());
                }
            }
        }
        let class_ok = matches!(
            p.class.as_str(),
            "identifier" | "phrase" | "natural" | "word"
        );
        let rule = if p.zero == Some(true) && zeros >= 2 && class_ok {
            Some(Rule::R1)
        } else if members[1..]
            .iter()
            .any(|(_, m)| m.cap_hit && narrower(p, m))
        {
            Some(Rule::R3)
        } else if calls >= 3 {
            Some(Rule::R2)
        } else if p.binary {
            Some(Rule::R5)
        } else {
            None
        };
        let Some(rule) = rule else {
            last_reason = if calls > 1 { "chain_short" } else { "no_chain" };
            continue;
        };
        // Widest member root that still contains the current one.
        let root = members
            .iter()
            .map(|(_, m)| m.root.as_str())
            .filter(|r| Path::new(&p.root).starts_with(r))
            .min_by_key(|r| r.len())
            .unwrap_or(&p.root)
            .to_string();
        let t = Trigger {
            rule,
            calls,
            zeros,
            toks,
            idents,
            root,
            seen: seen_in(ring),
        };
        let better = best.as_ref().is_none_or(|b| rank(t.rule) > rank(b.rule));
        if better {
            best = Some(t);
        }
    }
    best.ok_or(last_reason)
}

fn rank(r: Rule) -> u8 {
    match r {
        Rule::R1 => 5,
        Rule::R3 => 4,
        Rule::R2 => 3,
        Rule::R5 => 2,
        Rule::R4 => 1,
    }
}

/// R4 (shadow): three distinct reads under one root after a search, no edit.
fn detect_r4(ring: &[Ev]) -> Option<Trigger> {
    let cur = ring.last()?;
    if cur.reads.is_empty() {
        return None;
    }
    let mut reads: Vec<&String> = Vec::new();
    for e in ring.iter().rev() {
        if cur.n.saturating_sub(e.n) > CHAIN_MAX_CALLS {
            break;
        }
        if !e.edits.is_empty() {
            return None;
        }
        if let Some(p) = e.probes.iter().find(|p| eligible(p, e.remote).is_ok()) {
            let distinct: HashSet<&&String> =
                reads.iter().filter(|r| r.starts_with(&p.root)).collect();
            if distinct.len() >= 3 {
                return Some(Trigger {
                    rule: Rule::R4,
                    calls: 1,
                    zeros: 0,
                    toks: p.toks.clone(),
                    idents: p.idents.clone(),
                    root: p.root.clone(),
                    seen: seen_in(ring),
                });
            }
            return None;
        }
        reads.extend(e.reads.iter());
    }
    None
}
