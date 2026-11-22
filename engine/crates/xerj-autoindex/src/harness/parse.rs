//! Hook tool call -> profiler event. Bash first: 99.99% of measured agent
//! search runs through Bash, so a hook that only saw Grep/Glob saw nothing.
//! The parser is a small shell lexer, not a shell: anything it cannot follow
//! (unexpanded variables, PowerShell) yields no probe, which means silence.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// One search inside a tool call. A marker batch holds several.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Probe {
    pub cmd: String,
    /// Absolute search root (tree) or the common parent of named files.
    pub root: String,
    pub tree: bool,
    /// Lower-cased word parts (camelCase/snake_case split, stop-words dropped).
    pub toks: Vec<String>,
    /// Whole identifiers (lower-cased), kept for exact definition lookups.
    pub idents: Vec<String>,
    pub class: String,
    /// `Some(true)` only when this probe's own output section is provably empty.
    pub zero: Option<bool>,
    pub cap: Option<u32>,
    pub cap_hit: bool,
    pub path_search: bool,
    pub include: bool,
    pub binary: bool,
    /// v2: what the probe looks for, from its include globs/types and named
    /// files: "doc", "code" or "" (unknown, ranked as code).
    #[serde(default)]
    pub want: String,
}

/// One hook-observed tool call, as stored in the per-stream ring.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Ev {
    pub t: f64,
    pub n: u64,
    pub tuid: String,
    pub tool: String,
    pub cwd: String,
    pub probes: Vec<Probe>,
    pub reads: Vec<String>,
    pub edits: Vec<String>,
    pub shown: Vec<String>,
    pub remote: bool,
}

impl Ev {
    pub fn is_search(&self) -> bool {
        !self.probes.is_empty()
    }
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "from", "this", "that", "into", "let", "mut", "pub", "use",
    "impl", "self", "return", "true", "false", "none", "some", "null", "nil", "def", "class",
    "func", "function", "const", "var", "str", "string", "int", "bool", "type", "struct", "enum",
    "err", "test", "tests", "todo", "fixme", "new", "get", "set", "http", "https", "www", "com",
    "org", "src", "lib", "main", "mod", "py", "js", "ts", "go", "java", "cpp", "hpp", "json",
    "toml", "yaml", "yml", "txt", "log", "echo", "grep", "head", "tail", "sed", "awk", "cat",
    "find", "xargs", "sort", "uniq", "not", "was", "are", "has", "have", "but", "you", "all",
    "any", "can", "will", "when", "then", "else", "elif", "done", "case", "esac", "print",
    "printf", "value", "data", "name", "path", "file", "files", "line", "lines", "item", "key",
    "info", "debug", "warn", "time", "date", "num", "len", "size", "count", "max", "min", "sum",
    "async", "await", "static", "void", "public", "private", "import", "export", "include",
    "define", "where", "which", "what", "how", "does", "there", "only", "also", "each", "should",
    "must", "would",
];

pub fn is_stop(word: &str) -> bool {
    STOP.contains(&word)
}

fn split_ident(word: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for chunk in word.split('_') {
        let chars: Vec<char> = chunk.chars().collect();
        let mut start = 0;
        for i in 1..chars.len() {
            let (a, b) = (chars[i - 1], chars[i]);
            let next_lower = chars.get(i + 1).is_some_and(|c| c.is_lowercase());
            if (a.is_lowercase() && b.is_uppercase())
                || (a.is_uppercase() && b.is_uppercase() && next_lower)
                || (a.is_alphabetic() && b.is_ascii_digit())
            {
                parts.push(chars[start..i].iter().collect::<String>());
                start = i;
            }
        }
        if start < chars.len() {
            parts.push(chars[start..].iter().collect());
        }
    }
    parts
}

fn looks_secret(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if ["sk-", "ghp_", "gho_", "github_pat", "xox", "akia", "eyj"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return true;
    }
    word.len() >= 24
        && word.chars().filter(|c| c.is_ascii_digit()).count() >= 4
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Pattern text -> (word parts, whole identifiers). Regex syntax is dropped.
pub fn tokens(text: &str) -> (Vec<String>, Vec<String>) {
    let mut cleaned = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&n) = chars.peek() {
                chars.next();
                if (n.is_ascii_alphanumeric() && "bBwWsSdDAzZnt".contains(n))
                    || n == '<'
                    || n == '>'
                {
                    cleaned.push(' ');
                } else {
                    cleaned.push(if n.is_alphanumeric() || n == '_' {
                        n
                    } else {
                        ' '
                    });
                }
            }
            continue;
        }
        cleaned.push(if c.is_alphanumeric() || c == '_' {
            c
        } else {
            ' '
        });
    }
    let (mut toks, mut idents) = (Vec::new(), Vec::new());
    for word in cleaned.split_whitespace() {
        if word.len() < 3
            || word.chars().all(|c| c.is_ascii_digit() || c == '_')
            || looks_secret(word)
        {
            continue;
        }
        let has_case =
            word.chars().skip(1).any(char::is_uppercase) && word.chars().any(char::is_lowercase);
        if word.contains('_') || has_case {
            let lower = word.to_lowercase();
            if !idents.contains(&lower) {
                idents.push(lower);
            }
        }
        for part in split_ident(word) {
            let part = part.to_lowercase();
            if part.len() >= 3
                && !is_stop(&part)
                && !part.chars().all(|c| c.is_ascii_digit())
                && !toks.contains(&part)
            {
                toks.push(part);
            }
        }
    }
    (toks, idents)
}

/// Branch class used by the trigger preconditions.
pub fn classify(pat: &str) -> &'static str {
    let branches: Vec<&str> = pat
        .split("\\|")
        .flat_map(|b| b.split('|'))
        .filter(|b| !b.is_empty())
        .collect();
    if branches.is_empty() {
        return "none";
    }
    let mut best = classify_branch(branches[0]);
    for b in branches {
        let class = classify_branch(b);
        let rank = |c: &str| match c {
            "identifier" => 5,
            "natural" => 4,
            "phrase" => 3,
            "word" => 2,
            "path_or_glob" => 1,
            _ => 0,
        };
        if rank(class) > rank(best) {
            best = class;
        }
    }
    best
}

fn classify_branch(b: &str) -> &'static str {
    let b = b.trim();
    if b.starts_with('$') {
        return "shell_var";
    }
    if b.contains('/') || b.starts_with("*.") {
        return "path_or_glob";
    }
    let literal: String = b
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == ' ')
        .collect();
    let meta = b.chars().filter(|c| "[]*+?{}()".contains(*c)).count();
    let longest = literal.split(' ').map(str::len).max().unwrap_or(0);
    if meta > 0 && longest < 4 {
        return "regex";
    }
    let words = literal.split_whitespace().count();
    if words >= 3 {
        return "natural";
    }
    if words == 2 {
        return "phrase";
    }
    let w = literal.trim();
    if w.len() < 3 {
        return "regex";
    }
    let has_case = w.chars().skip(1).any(char::is_uppercase) && w.chars().any(char::is_lowercase);
    if w.contains('_') || has_case || w.chars().any(|c| c.is_ascii_digit()) {
        "identifier"
    } else {
        "word"
    }
}

// ───────────────────────────── shell lexer ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    W(String),
    Op(&'static str),
    Redir,
}

#[allow(unused_assignments)]
fn lex(src: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut word = String::new();
    let mut in_word = false;
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut pending_heredoc: Option<bool> = None;
    macro_rules! flush {
        () => {
            if in_word {
                let w = std::mem::take(&mut word);
                if let Some(strip) = pending_heredoc.take() {
                    heredocs.push((w.clone(), strip));
                }
                out.push(Tok::W(w));
                in_word = false;
            }
        };
    }
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\r' => {
                flush!();
                i += 1;
            }
            '\n' => {
                flush!();
                out.push(Tok::Op("\n"));
                i += 1;
                for (delim, strip) in heredocs.drain(..) {
                    loop {
                        if i >= chars.len() {
                            break;
                        }
                        let end = chars[i..]
                            .iter()
                            .position(|&c| c == '\n')
                            .map_or(chars.len(), |p| i + p);
                        let line: String = chars[i..end].iter().collect();
                        i = (end + 1).min(chars.len());
                        let probe = if strip {
                            line.trim_start()
                        } else {
                            line.as_str()
                        };
                        if probe.trim_end() == delim {
                            break;
                        }
                    }
                }
            }
            '#' if !in_word => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\'' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    word.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '"' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() && "\"\\$`".contains(chars[i + 1]) {
                        word.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    word.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if i + 1 < chars.len() {
                    if chars[i + 1] != '\n' {
                        in_word = true;
                        word.push(chars[i + 1]);
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '$' if i + 1 < chars.len() && chars[i + 1] == '(' => {
                in_word = true;
                let mut depth = 0;
                while i < chars.len() {
                    let ch = chars[i];
                    word.push(ch);
                    i += 1;
                    if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            '`' => {
                in_word = true;
                word.push('`');
                i += 1;
                while i < chars.len() && chars[i] != '`' {
                    word.push(chars[i]);
                    i += 1;
                }
                word.push('`');
                i += 1;
            }
            ';' | '&' | '|' | '(' | ')' => {
                flush!();
                let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
                let op = match two.as_str() {
                    "&&" => "&&",
                    "||" => "||",
                    "|&" => "|",
                    ";;" => ";",
                    "&>" => {
                        out.push(Tok::Redir);
                        i += 2;
                        continue;
                    }
                    _ => match c {
                        ';' => ";",
                        '&' => "&",
                        '|' => "|",
                        _ => ";",
                    },
                };
                i += if op.len() == 2 { 2 } else { 1 };
                out.push(Tok::Op(op));
            }
            '<' | '>' => {
                if in_word && word.chars().all(|c| c.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                flush!();
                let mut j = i;
                while j < chars.len() && "<>&-".contains(chars[j]) && j - i < 3 {
                    j += 1;
                }
                let op: String = chars[i..j].iter().collect();
                i = j;
                if op.starts_with("<<") && !op.starts_with("<<<") {
                    pending_heredoc = Some(op.ends_with('-'));
                }
                out.push(Tok::Redir);
            }
            _ => {
                in_word = true;
                word.push(c);
                i += 1;
            }
        }
    }
    flush!();
    out
}

/// Split into commands; each carries its pipeline position and words
/// (redirection targets removed).
fn commands(toks: &[Tok]) -> Vec<(usize, Vec<String>)> {
    let mut out = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut pos = 0usize;
    let mut skip_next = false;
    for t in toks {
        match t {
            Tok::W(w) => {
                if skip_next {
                    skip_next = false;
                } else {
                    cur.push(w.clone());
                }
            }
            Tok::Redir => skip_next = true,
            Tok::Op(op) => {
                if !cur.is_empty() {
                    out.push((pos, std::mem::take(&mut cur)));
                }
                pos = if *op == "|" { pos + 1 } else { 0 };
            }
        }
    }
    if !cur.is_empty() {
        out.push((pos, cur));
    }
    out
}

fn expand(word: &str, vars: &HashMap<String, String>) -> Option<String> {
    if !word.contains('$') && !word.contains('`') {
        return Some(word.to_string());
    }
    let mut out = String::new();
    let chars: Vec<char> = word.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            let braced = chars.get(i + 1) == Some(&'{');
            let start = i + 1 + usize::from(braced);
            let mut j = start;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let name: String = chars[start..j].iter().collect();
            if name.is_empty() {
                return None;
            }
            if braced {
                if chars.get(j) != Some(&'}') {
                    return None;
                }
                j += 1;
            }
            let value = vars.get(&name).cloned().or_else(|| {
                (name == "HOME")
                    .then(|| std::env::var("HOME").ok())
                    .flatten()
            })?;
            out.push_str(&value);
            i = j;
        } else if chars[i] == '`' {
            return None;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    Some(out)
}

pub fn normalize(base: &Path, p: &str) -> PathBuf {
    let p = if let Some(rest) = p.strip_prefix("~/") {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest)
    } else {
        PathBuf::from(p)
    };
    let joined = if p.is_absolute() { p } else { base.join(p) };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn base(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

const WRAPPERS: &[&str] = &[
    "do", "then", "else", "elif", "if", "while", "until", "!", "time", "{", "}", "exec", "command",
    "builtin", "nohup", "stdbuf",
];

/// Strip keywords, assignments and wrappers (`sudo`, `timeout 5`, `xargs -0`).
fn strip<'a>(
    words: &'a [String],
    vars: &mut HashMap<String, String>,
    standalone: bool,
) -> (&'a [String], bool) {
    let mut w = words;
    let mut via_xargs = false;
    while let Some(first) = w.first() {
        let f = first.as_str();
        if WRAPPERS.contains(&f) {
            w = &w[1..];
        } else if let Some((k, v)) = f
            .split_once('=')
            .filter(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_'))
        {
            if standalone && w.len() == 1 {
                if let Some(v) = expand(v, vars) {
                    vars.insert(k.to_string(), v);
                }
            }
            w = &w[1..];
        } else if matches!(
            f,
            "sudo" | "env" | "nice" | "ionice" | "timeout" | "xargs" | "parallel"
        ) {
            via_xargs |= matches!(f, "xargs" | "parallel");
            let takes: &[&str] = match f {
                "sudo" => &["-u", "-g", "-C", "-D"],
                "nice" => &["-n"],
                "ionice" => &["-c", "-n", "-p"],
                "timeout" => &["-s", "-k", "--signal", "--kill-after"],
                "xargs" => &["-I", "-n", "-P", "-d", "-L", "-s", "-E", "-a"],
                _ => &[],
            };
            w = &w[1..];
            while let Some(a) = w.first() {
                if a.starts_with('-') {
                    w = &w[if takes.contains(&a.as_str()) {
                        2.min(w.len())
                    } else {
                        1
                    }..];
                } else if f == "env" && a.contains('=') {
                    w = &w[1..];
                } else {
                    break;
                }
            }
            if f == "timeout"
                && w.first()
                    .is_some_and(|a| a.chars().next().is_some_and(|c| c.is_ascii_digit()))
            {
                w = &w[1..];
            }
        } else {
            break;
        }
    }
    (w, via_xargs)
}

struct GrepArgs {
    pats: Vec<String>,
    paths: Vec<String>,
    recursive: bool,
    include: bool,
    globs: Vec<String>,
    max_count: Option<u32>,
    files_only: bool,
}

fn parse_grep(args: &[String], flavour: &str) -> GrepArgs {
    let (short_val, long_val): (&str, &[&str]) = match flavour {
        "rg" => (
            "efgtTmABCjMrE",
            &[
                "--glob",
                "--iglob",
                "--type",
                "--type-not",
                "--max-count",
                "--context",
                "--after-context",
                "--before-context",
                "--max-depth",
                "--threads",
                "--max-columns",
                "--regexp",
                "--file",
                "--replace",
                "--sort",
                "--sortr",
                "--type-add",
                "--ignore-file",
                "--pre",
                "--encoding",
                "--color",
                "--colors",
                "--max-filesize",
                "--path-separator",
                "--context-separator",
                "--field-match-separator",
            ],
        ),
        _ => (
            "efmABCdD",
            &[
                "--include",
                "--exclude",
                "--exclude-dir",
                "--regexp",
                "--file",
                "--max-count",
                "--context",
                "--after-context",
                "--before-context",
                "--color",
                "--colour",
                "--devices",
                "--directories",
                "--label",
                "--binary-files",
            ],
        ),
    };
    let mut g = GrepArgs {
        pats: Vec::new(),
        paths: Vec::new(),
        recursive: matches!(flavour, "rg" | "git-grep" | "ag" | "ack"),
        include: false,
        globs: Vec::new(),
        max_count: None,
        files_only: false,
    };
    let mut explicit_pat = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    let mut opts_done = false;
    let take_val = |name: &str, val: &str, g: &mut GrepArgs, explicit: &mut bool| match name {
        "e" | "--regexp" => {
            g.pats.push(val.to_string());
            *explicit = true;
        }
        "f" | "--file" => *explicit = true,
        "m" | "--max-count" => g.max_count = val.parse().ok(),
        "g" | "--glob" | "--iglob" | "t" | "--type" | "--include" => {
            g.include = true;
            g.globs.push(val.to_string());
        }
        "d" | "--directories" if val == "recurse" => g.recursive = true,
        _ => {}
    };
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if opts_done || !a.starts_with('-') || a == "-" {
            positional.push(a.clone());
            continue;
        }
        if a == "--" {
            opts_done = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (a.clone(), None),
            };
            match name.as_str() {
                "--recursive" | "--dereference-recursive" => g.recursive = true,
                "--files-with-matches" | "--files-without-match" | "--count" => g.files_only = true,
                _ => {}
            }
            if long_val.contains(&name.as_str()) {
                let v = match val {
                    Some(v) => v,
                    None => {
                        i += 1;
                        args.get(i - 1).cloned().unwrap_or_default()
                    }
                };
                take_val(&name, &v, &mut g, &mut explicit_pat);
            }
            continue;
        }
        let cluster: Vec<char> = a[1..].chars().collect();
        let mut k = 0;
        while k < cluster.len() {
            let c = cluster[k];
            k += 1;
            match c {
                'r' | 'R' if flavour != "rg" => g.recursive = true,
                'l' | 'L' | 'c' => g.files_only = true,
                _ => {}
            }
            if short_val.contains(c) {
                let rest: String = cluster[k..].iter().collect();
                let v = if !rest.is_empty() {
                    rest
                } else {
                    i += 1;
                    args.get(i - 1).cloned().unwrap_or_default()
                };
                take_val(&c.to_string(), &v, &mut g, &mut explicit_pat);
                break;
            }
        }
    }
    let mut pos = positional.into_iter();
    if !explicit_pat {
        if let Some(p) = pos.next() {
            g.pats.push(p);
        }
    }
    g.paths = pos.collect();
    g
}

/// File kinds for v2 candidate filtering (design §5, BUILD v2 step 2).
pub const CODE_EXT: &[&str] = &[
    "rs", "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "c", "h", "cc", "cpp", "cxx",
    "hpp", "hh", "java", "kt", "kts", "rb", "php", "cs", "swift", "scala", "sh", "bash", "zsh",
    "lua", "zig", "sql", "vue", "svelte", "ex", "exs", "erl", "hs", "ml", "dart", "r", "jl",
    "proto", "m", "mm", "pl", "ps1", "nim", "v", "sv", "cu",
];
pub const DOC_EXT: &[&str] = &[
    "md", "mdx", "markdown", "txt", "rst", "adoc", "org", "html", "htm",
];
/// ripgrep `--type` names that mean documentation.
const DOC_TYPES: &[&str] = &[
    "md", "markdown", "txt", "rst", "html", "asciidoc", "org", "doc",
];

fn ext_of(p: &str) -> String {
    let b = base(p);
    let b = b.trim_start_matches('*');
    b.rsplit_once('.')
        .map(|(_, e)| {
            e.trim_matches(|c| c == '}' || c == '{')
                .to_ascii_lowercase()
        })
        .unwrap_or_default()
}

/// "doc" when the file is documentation, "code" when it is source, else "".
pub fn file_kind(path: &str) -> &'static str {
    let e = ext_of(path);
    if DOC_EXT.contains(&e.as_str()) {
        "doc"
    } else if CODE_EXT.contains(&e.as_str()) {
        "code"
    } else {
        ""
    }
}

const DOC_DIRS: &[&str] = &[
    "docs",
    "doc",
    "documentation",
    "landing",
    "reports",
    "notes",
    "wiki",
];

/// What a probe looks for: its include globs/types first, then named
/// files, then a search root inside a documentation directory.
pub fn want_of(globs: &[String], paths: &[String], root: &str) -> &'static str {
    let mut doc = 0;
    let mut code = 0;
    for g in globs {
        for part in g.split([',', '{', '}']).filter(|x| !x.is_empty()) {
            let kind = if part.contains('.') || part.contains('*') {
                file_kind(part)
            } else if DOC_TYPES.contains(&part) {
                "doc"
            } else {
                "code"
            };
            match kind {
                "doc" => doc += 1,
                "code" => code += 1,
                _ => {}
            }
        }
    }
    for p in paths.iter().filter(|p| looks_like_file(p)) {
        match file_kind(p) {
            "doc" => doc += 1,
            "code" => code += 1,
            _ => {}
        }
    }
    if doc > 0 && code == 0 {
        "doc"
    } else if code > 0 && doc == 0 {
        "code"
    } else if doc == 0
        && code == 0
        && Path::new(root)
            .components()
            .any(|c| DOC_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
    {
        "doc"
    } else {
        ""
    }
}

fn looks_like_file(p: &str) -> bool {
    let last = base(p);
    last.contains('*')
        || last.contains('?')
        || (last.contains('.') && !last.starts_with('.') && last.len() > 2 && !last.ends_with('.'))
}

/// Parse one Bash command line into probes, reads and markers.
pub struct BashParse {
    pub probes: Vec<Probe>,
    pub reads: Vec<String>,
    pub remote: bool,
    /// Ordered layout: `Ok(text)` marker, `Err(i)` probe index, for section split.
    layout: Vec<Result<String, usize>>,
    /// Commands that print output, other than markers and searches.
    other_producers: Vec<usize>,
}

pub fn parse_bash(command: &str, cwd: &Path) -> BashParse {
    let toks = lex(command);
    let cmds = commands(&toks);
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut cwd = cwd.to_path_buf();
    let mut out = BashParse {
        probes: Vec::new(),
        reads: Vec::new(),
        remote: false,
        layout: Vec::new(),
        other_producers: Vec::new(),
    };
    let mut prev_lister: Option<usize> = None;
    let mut section = 0usize;
    for (idx, (pos, words)) in cmds.iter().enumerate() {
        let standalone = *pos == 0 && cmds.get(idx + 1).is_none_or(|(p, _)| *p == 0);
        let (w, _via_xargs) = strip(words, &mut vars, standalone);
        let Some(name) = w.first() else { continue };
        let name = base(name).to_string();
        let args: Vec<String> = w[1..].to_vec();
        let in_pipe = *pos > 0;
        if *pos == 0 {
            prev_lister = None;
        }
        match name.as_str() {
            "cd" | "pushd" => {
                if let Some(d) = args.first().and_then(|d| expand(d, &vars)) {
                    cwd = normalize(&cwd, &d);
                }
            }
            "echo" | "printf" => {
                if !in_pipe {
                    let text = args
                        .iter()
                        .filter(|a| !(a.starts_with('-') && a.len() == 2))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ");
                    let text = text.replace("\\n", "\n");
                    for line in text.lines() {
                        if !line.trim().is_empty() {
                            out.layout.push(Ok(line.trim().to_string()));
                        }
                    }
                    section += 1;
                }
            }
            "ssh" | "scp" | "kubectl" | "docker" | "podman" | "gcloud" | "aws" => {
                out.remote = true;
            }
            "head" | "tail" if in_pipe => {
                let mut n: Option<u32> = Some(10);
                let mut it = args.iter();
                while let Some(a) = it.next() {
                    if a == "-n" || a == "-c" {
                        n = it
                            .next()
                            .and_then(|v| v.trim_start_matches('+').parse().ok());
                    } else if let Some(v) = a.strip_prefix("-n") {
                        n = v.parse().ok();
                    } else if let Some(v) = a.strip_prefix('-') {
                        if let Ok(v) = v.parse() {
                            n = Some(v);
                        }
                    }
                }
                if let Some(p) = out.probes.last_mut() {
                    if p.cap.is_none() && idx > 0 {
                        p.cap = n;
                    }
                }
            }
            "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "git" => {
                let (flavour, gargs) = if name == "git" {
                    match args.first().map(String::as_str) {
                        Some("grep") => ("git-grep", args[1..].to_vec()),
                        Some("ls-files") => {
                            prev_lister = Some(out.probes.len());
                            out.probes.push(Probe {
                                cmd: "git-ls-files".into(),
                                root: cwd.display().to_string(),
                                tree: true,
                                path_search: true,
                                class: "path_or_glob".into(),
                                ..Probe::default()
                            });
                            out.layout.push(Err(out.probes.len() - 1));
                            continue;
                        }
                        _ => {
                            if !in_pipe {
                                out.other_producers.push(section);
                            }
                            continue;
                        }
                    }
                } else if name == "rg" || name == "ag" || name == "ack" {
                    ("rg", args.clone())
                } else {
                    ("grep", args.clone())
                };
                let g = parse_grep(&gargs, flavour);
                let pats: Vec<String> = g
                    .pats
                    .iter()
                    .filter_map(|p| expand(p, &vars).or(Some(p.clone())))
                    .collect();
                if pats.is_empty() {
                    continue;
                }
                let joined = pats.join("|");
                let (toks, idents) = tokens(&joined);
                let mut class = classify(&joined).to_string();
                if pats.iter().any(|p| p.starts_with('$')) {
                    class = "shell_var".into();
                }
                if in_pipe && g.paths.is_empty() {
                    if let Some(li) = prev_lister {
                        let p = &mut out.probes[li];
                        p.toks.extend(toks);
                        p.idents.extend(idents);
                        p.class = class;
                    }
                    continue;
                }
                let mut paths = Vec::new();
                let mut unknown = false;
                for p in &g.paths {
                    match expand(p, &vars) {
                        Some(e) => paths.push(e),
                        None => unknown = true,
                    }
                }
                let (tree, root) = if paths.is_empty() {
                    (g.recursive, Some(cwd.clone()))
                } else {
                    let all_files = paths.iter().all(|p| looks_like_file(p));
                    let tree = g.recursive && !all_files;
                    let resolved: Vec<PathBuf> = paths
                        .iter()
                        .map(|p| {
                            let n = normalize(&cwd, p);
                            if looks_like_file(p) {
                                n.parent().map(Path::to_path_buf).unwrap_or(n)
                            } else {
                                n
                            }
                        })
                        .collect();
                    (tree, common_parent(&resolved))
                };
                if !g.recursive && paths.is_empty() {
                    continue;
                }
                let root = if unknown { None } else { root };
                let want = want_of(
                    &g.globs,
                    &g.paths,
                    &root
                        .as_ref()
                        .map(|r| r.display().to_string())
                        .unwrap_or_default(),
                )
                .to_string();
                out.probes.push(Probe {
                    cmd: flavour.into(),
                    root: root.map(|r| r.display().to_string()).unwrap_or_default(),
                    tree,
                    toks,
                    idents,
                    class,
                    cap: g.max_count,
                    include: g.include,
                    path_search: g.files_only,
                    want,
                    ..Probe::default()
                });
                out.layout.push(Err(out.probes.len() - 1));
            }
            "find" | "fd" | "fdfind" => {
                let mut roots = Vec::new();
                let mut pats = Vec::new();
                let mut it = args.iter();
                let mut in_roots = true;
                let mut fd_pat_taken = false;
                while let Some(a) = it.next() {
                    if name == "find" {
                        if a.starts_with('-') || a == "(" || a == "!" {
                            in_roots = false;
                            if matches!(
                                a.as_str(),
                                "-name" | "-iname" | "-path" | "-ipath" | "-regex"
                            ) {
                                if let Some(v) = it.next() {
                                    pats.push(v.clone());
                                }
                            }
                        } else if in_roots {
                            if let Some(e) = expand(a, &vars) {
                                roots.push(e);
                            }
                        }
                    } else if !a.starts_with('-') {
                        if !fd_pat_taken {
                            pats.push(a.clone());
                            fd_pat_taken = true;
                        } else if let Some(e) = expand(a, &vars) {
                            roots.push(e);
                        }
                    }
                }
                let root = if roots.is_empty() {
                    Some(cwd.clone())
                } else {
                    common_parent(&roots.iter().map(|r| normalize(&cwd, r)).collect::<Vec<_>>())
                };
                let (toks, idents) = tokens(&pats.join(" "));
                prev_lister = Some(out.probes.len());
                out.probes.push(Probe {
                    cmd: name.clone(),
                    root: root.map(|r| r.display().to_string()).unwrap_or_default(),
                    tree: true,
                    toks,
                    idents,
                    class: "path_or_glob".into(),
                    path_search: true,
                    ..Probe::default()
                });
                out.layout.push(Err(out.probes.len() - 1));
            }
            "cat" | "sed" | "nl" | "bat" | "less" | "more" | "view" | "head" | "tail" => {
                if !in_pipe {
                    out.other_producers.push(section);
                    let files: Vec<&String> = args
                        .iter()
                        .filter(|a| {
                            !a.starts_with('-')
                                && !(name == "sed"
                                    && (a.contains('p') || a.contains(',') || a.starts_with('s'))
                                    && !looks_like_file(a))
                        })
                        .collect();
                    for f in files {
                        if let Some(e) = expand(f, &vars) {
                            if looks_like_file(&e) && !e.contains('*') {
                                out.reads.push(normalize(&cwd, &e).display().to_string());
                            }
                        }
                    }
                }
            }
            _ => {
                if !in_pipe
                    && !matches!(
                        name.as_str(),
                        "set" | "export" | "true" | "false" | "sleep" | ":" | "for" | "done" | "fi"
                    )
                {
                    out.other_producers.push(section);
                }
            }
        }
    }
    for p in &mut out.probes {
        if p.root.is_empty() {
            p.tree = false;
        }
    }
    out
}

fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?.clone();
    let mut common = first;
    for p in &paths[1..] {
        while !p.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    Some(common)
}

/// Lines that start with a `path:` or are a bare path.
fn output_paths(text: &str, cwd: &Path, out: &mut Vec<String>) {
    for line in text.lines().take(2000) {
        let line = line.trim_start_matches("./");
        let cand = match line.find(':') {
            Some(i) if i > 0 => &line[..i],
            _ => line.trim(),
        };
        if cand.is_empty() || cand.contains(char::is_whitespace) || cand.len() > 400 {
            continue;
        }
        if !(cand.contains('/') || looks_like_file(cand)) || cand.starts_with("http") {
            continue;
        }
        let p = normalize(cwd, cand).display().to_string();
        if !out.contains(&p) && out.len() < 200 {
            out.push(p);
        }
    }
}

/// Section line counts per probe, from echo markers. `None` when unknown.
fn sections(bp: &BashParse, stdout: &str, truncated: bool) -> Vec<(Option<usize>, bool)> {
    let mut res = vec![(None, false); bp.probes.len()];
    let markers: Vec<&str> = bp
        .layout
        .iter()
        .filter_map(|x| x.as_ref().ok().map(String::as_str))
        .collect();
    let lines: Vec<&str> = stdout.lines().collect();
    if markers.is_empty() {
        if bp.probes.len() == 1 && bp.other_producers.is_empty() {
            let n = lines.iter().filter(|l| !l.trim().is_empty()).count();
            res[0] = (Some(n), !(truncated && n == 0));
        }
        return res;
    }
    // Section index per layout element, and line counts per section.
    let mut counts = vec![0usize; markers.len() + 1];
    let mut sec = 0usize;
    let mut next = 0usize;
    for l in &lines {
        let t = l.trim();
        if let Some(off) = markers[next.min(markers.len())..]
            .iter()
            .position(|m| *m == t)
        {
            next += off + 1;
            sec = next;
            continue;
        }
        if !t.is_empty() {
            counts[sec] += 1;
        }
    }
    let reached_end = sec;
    let mut s = 0usize;
    let mut probes_in: Vec<Vec<usize>> = vec![Vec::new(); markers.len() + 1];
    for item in &bp.layout {
        match item {
            Ok(_) => s += 1,
            Err(i) => probes_in[s].push(*i),
        }
    }
    let mut producer_sections = vec![0usize; markers.len() + 2];
    for &p in &bp.other_producers {
        if p < producer_sections.len() {
            producer_sections[p] += 1;
        }
    }
    for (sec, ps) in probes_in.iter().enumerate() {
        if sec > reached_end {
            continue;
        }
        let last_and_cut = truncated && sec == reached_end;
        let single = ps.len() == 1 && producer_sections.get(sec).copied().unwrap_or(0) == 0;
        for &i in ps {
            if counts[sec] == 0 && !last_and_cut {
                res[i] = (Some(0), true);
            } else if single && !last_and_cut {
                res[i] = (Some(counts[sec]), true);
            }
        }
    }
    res
}

fn str_field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

/// Build a ring event from one hook tool call (Claude Code or Codex shape).
pub fn event_from_call(
    tool: &str,
    input: &Value,
    response: &Value,
    cwd: &str,
    tuid: &str,
    t: f64,
) -> Ev {
    let cwd_path = PathBuf::from(cwd);
    let mut ev = Ev {
        t,
        tuid: tuid.to_string(),
        tool: tool.to_string(),
        cwd: cwd.to_string(),
        ..Ev::default()
    };
    let stdout = match response {
        Value::String(s) => s.clone(),
        Value::Object(_) => str_field(response, &["stdout", "output", "content"])
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    };
    let truncated = response
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match tool {
        "Bash" | "bash" | "shell" | "exec_command" | "local_shell" => {
            let command = match input.get("command") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            };
            let bp = parse_bash(&command, &cwd_path);
            let secs = sections(&bp, &stdout, truncated);
            ev.remote = bp.remote;
            ev.reads = bp.reads.clone();
            let binary = stdout
                .lines()
                .any(|l| l.starts_with("Binary file ") && l.ends_with(" matches"));
            let mut probes = bp.probes.clone();
            for (p, (count, known)) in probes.iter_mut().zip(secs) {
                if known {
                    p.zero = count.map(|c| c == 0);
                    if let (Some(c), Some(cap)) = (count, p.cap) {
                        p.cap_hit = c as u32 >= cap;
                    }
                }
                p.binary = binary;
            }
            ev.probes = probes;
            let base = ev
                .probes
                .first()
                .map(|p| PathBuf::from(&p.root))
                .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
                .unwrap_or(cwd_path.clone());
            output_paths(&stdout, &base, &mut ev.shown);
        }
        "Grep" | "grep_search" | "search_file_content" => {
            let pat = str_field(input, &["pattern"]).unwrap_or("");
            let path = str_field(input, &["path", "dir_path"]).unwrap_or("");
            let root = normalize(&cwd_path, if path.is_empty() { "." } else { path });
            let tree = path.is_empty() || !looks_like_file(path);
            let (toks, idents) = tokens(pat);
            let mut shown = Vec::new();
            if let Some(files) = response.get("filenames").and_then(Value::as_array) {
                for f in files.iter().filter_map(Value::as_str) {
                    shown.push(normalize(&cwd_path, f).display().to_string());
                }
            }
            let content = str_field(response, &["content"]).unwrap_or("");
            output_paths(content, &cwd_path, &mut shown);
            let num = response
                .get("numFiles")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let lines = response
                .get("numLines")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let total = shown.len().max(num).max(lines);
            let cap = input
                .get("head_limit")
                .and_then(Value::as_u64)
                .map(|c| c as u32);
            ev.probes.push(Probe {
                cmd: "Grep".into(),
                root: if tree {
                    root.display().to_string()
                } else {
                    root.parent().unwrap_or(&root).display().to_string()
                },
                tree,
                toks,
                idents,
                class: classify(pat).into(),
                zero: Some(total == 0 && content.trim().is_empty()),
                cap,
                cap_hit: cap.is_some_and(|c| total as u32 >= c),
                include: input.get("glob").is_some() || input.get("type").is_some(),
                want: {
                    let globs: Vec<String> = ["glob", "type"]
                        .iter()
                        .filter_map(|k| str_field(input, &[k]).map(str::to_string))
                        .collect();
                    let paths: Vec<String> = if path.is_empty() {
                        Vec::new()
                    } else {
                        vec![path.to_string()]
                    };
                    want_of(&globs, &paths, &root.display().to_string()).to_string()
                },
                ..Probe::default()
            });
            ev.shown = shown;
        }
        "Glob" | "glob" => {
            let pat = str_field(input, &["pattern"]).unwrap_or("");
            let path = str_field(input, &["path"]).unwrap_or(".");
            let (toks, idents) = tokens(&pat.replace(['*', '?', '{', '}', ','], " "));
            let mut shown = Vec::new();
            if let Some(files) = response.get("filenames").and_then(Value::as_array) {
                for f in files.iter().filter_map(Value::as_str).take(200) {
                    shown.push(normalize(&cwd_path, f).display().to_string());
                }
            }
            ev.probes.push(Probe {
                cmd: "Glob".into(),
                root: normalize(&cwd_path, path).display().to_string(),
                tree: true,
                toks,
                idents,
                class: "path_or_glob".into(),
                zero: Some(shown.is_empty()),
                path_search: true,
                ..Probe::default()
            });
            ev.shown = shown;
        }
        "Read" | "read_file" | "view" => {
            if let Some(f) = str_field(input, &["file_path", "path", "absolute_path"]) {
                ev.reads.push(normalize(&cwd_path, f).display().to_string());
            }
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "replace" | "write_file"
        | "apply_patch" => {
            if let Some(f) = str_field(input, &["file_path", "notebook_path", "path"]) {
                ev.edits.push(normalize(&cwd_path, f).display().to_string());
            }
        }
        _ => {}
    }
    ev
}
