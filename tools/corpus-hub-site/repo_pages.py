"""Repo pages, titles, descriptions and structured data for /corpus/<slug>.

Stdlib only, no network. GitHub facts come from data/repo_stats.json, written
by fetch_repo_stats.py. A fact is printed only when it is in that file or in
the hub registry; nothing is estimated.
"""
import collections, html as _html, json, pathlib, re

HERE = pathlib.Path(__file__).resolve().parent
STATS = HERE / "data" / "repo_stats.json"
SITE = "https://hub.xerj.org"
SUFFIX = " | XERJ Hub"
TITLE_MAX = 62
DESC_MAX = 160
POSITION = "A new way for AI to search for data"
CTA = "Copy the prompt"
GH = re.compile(r"^https://github\.com/([^/]+)/([^/#?]+?)(?:\.git)?/?$")
_EMOJI = re.compile("[\U0001F000-\U0001FAFF\u2600-\u27BF\uFE0F\u200D]")
_SHORTCODE = re.compile(r":[a-z0-9_+-]+:")
DASHES = ("\u2014", "\u2013", "\u2012", "\u2015")


def load_stats(path=STATS):
    """(fetched date, {"owner/repo" lower: facts}); empty when the file is missing."""
    p = pathlib.Path(path)
    if not p.exists():
        return "", {}
    d = json.loads(p.read_text(encoding="utf-8"))
    return d.get("fetched", ""), {k: v for k, v in d.get("repos", {}).items() if not v.get("missing")}


def key(url):
    m = GH.match(url or "")
    return f"{m.group(1)}/{m.group(2)}".lower() if m else ""


def repo_name(url):
    """'owner/repo' as GitHub spells it, from a GitHub URL; '' otherwise."""
    m = GH.match(url or "")
    return f"{m.group(1)}/{m.group(2)}" if m else ""


def nodash(s):
    s = str(s or "")
    s = re.sub(r"\s*[\u2014\u2015]\s*", ": ", s)
    s = re.sub(r"(\d)\s*[\u2013\u2012]\s*(\d)", r"\1-\2", s)
    return re.sub(r"\s*[\u2013\u2012]\s*", ", ", s)


def clean(s):
    """GitHub description as plain text: no emoji, no :shortcodes:, no dashes, no trailing dot."""
    s = _html.unescape(str(s or ""))
    s = _SHORTCODE.sub(" ", _EMOJI.sub(" ", s))
    s = nodash(" ".join(s.split()))
    s = re.sub(r"\s+([,.;:!?])", r"\1", s).strip(" .,;:-|")
    s = re.sub(r"^[^\w\"'(\[]+", "", s)
    return s


def clip(s, n, mark=""):
    """Cut s to n chars: at the end of a sentence if one fits, else at a word."""
    s = " ".join(str(s).split())
    if len(s) <= n:
        return s
    ends = [m.end() - 1 for m in re.finditer(r"[.!?](?=\s)", s[:n + 1])]
    if ends and ends[-1] >= n * 0.45:
        return s[:ends[-1]]
    m = max(0, n - len(mark))
    parts = [c.start() for c in re.finditer(r"[:;,]\s|\s\(|\s(?:and|with|for|that|which)\s", s[:m + 1])]
    if parts and parts[-1] >= n * 0.6:  # a clause boundary reads better than a cut word
        return s[:parts[-1]].rstrip(" ,;:") + mark
    cut = s[:m]
    if s[m:m + 1] != " ":              # mid-word: back up to the last space
        sp = cut.rfind(" ")
        cut = cut[:sp] if sp > 0 else ""
    stop = r"\s+(?:a|an|the|and|or|of|for|to|in|on|with|by|from|via|at|is|as|your|its|that|which|we|can|are|be|so|but|than|into|over|up|our|their)$"
    while re.search(stop, cut, flags=re.I):
        cut = re.sub(stop, "", cut, flags=re.I)
    return cut.rstrip(" ,;:.-/(&") + mark if cut else ""


def cap(s):
    s = str(s or "").strip()
    return s[:1].upper() + s[1:]


USECASE = [
    re.compile(r"^an? [^:]*?\bquer(?:y|ies) (?:this|these|it|[A-Z][\w ]{0,40}?) for (?:the )?(.+)$", re.I),
    re.compile(r"^an? (?:agent|[\w -]{0,40}?) (?:working on|retrieving from|building|reviewing|triaging|wiring|vetting) (.+)$", re.I),
]


def usecase_text(u):
    """'an agent ... queries this for X' -> 'X', capitalised, no dashes."""
    u = clean(u)
    for rx in USECASE:
        m = rx.match(u)
        if m:
            u = m.group(1)
            break
    return cap(u)


# words a title or phrase must not end on: it reads as cut off
DANGLING = frozenset("""a an the and or nor but so yet of for to in on at by from via with without within into onto
over under up about across through than as like is are was were be been being am can could will would shall should
may might must do does did has have had we its your our their my his her whose which that not no non
any some each every these those such own another other""".split())
QUESTION = re.compile(r"^(?:what|how|why|who|when|where|which|is|are|do|does|can|should|will|would)\b", re.I)


def dangling(text):
    """True when text ends on a word that leaves the phrase hanging (a/of/and/is/we/its/agent's ...)."""
    t = re.sub(r"\s*\|\s*XERJ Hub$", "", _html.unescape(str(text or ""))).strip()
    w = (t.split() or [""])[-1].strip(".,;:!?\"'()[]")
    lw = w.lower()
    return not w or lw in DANGLING or lw.endswith(("'s", "\u2019s", "s'")) or w.endswith(("/", "&"))


def phrases(desc):
    """Complete phrases of desc, longest first: the whole text, its sentences, and its leading
    clauses (cut only at . ! ? ; : , or an opening bracket). Never a cut mid-phrase."""
    d = clean(desc)
    if not d:
        return []
    out = [d]
    for m in re.finditer(r"[.!?;:,](?=\s)|\s[(\[]|\s[\u2014\u2013-]\s", d):
        out.append(d[:m.start()].rstrip(" .,;:"))
    # the head noun phrase before a trailing modifier: "A driver for Laravel" -> "A driver"
    for m in re.finditer(r"\s(?=(?:of|for|with|in|on|to|over|via|using|that|which|from|across|into|onto|by|at|"
                         r"under|without|where|when|so|built|written|powered|based)\s)", d):
        out.append(d[:m.start()].rstrip(" .,;:"))
    seen, res = set(), []
    for x in sorted(out, key=len, reverse=True):
        x = clean(x)
        if x and x not in seen:
            seen.add(x)
            res.append(x)
    return res


def good_phrase(x):
    """A phrase fit for a title: two words or more, not a question, not left hanging,
    brackets balanced."""
    return (len(x.split()) >= 2 and len(x) >= 10 and not QUESTION.match(x) and not dangling(x)
            and x.count("(") == x.count(")") and x.count('"') % 2 == 0)


def title(name, *descs, fallbacks=()):
    """'<name>: <phrase> | XERJ Hub', at most 62 chars. The phrase is a description, or a complete
    sentence or clause of one, never cut mid-phrase. If none fits, the first fallback that fits
    ('{name}' is replaced), then the bare name."""
    for desc in descs:
        for x in (x for x in phrases(desc) if good_phrase(x)):
            for suf in (SUFFIX, ""):
                if len(name) + 2 + len(x) + len(suf) <= TITLE_MAX:
                    return f"{name}: {x}{suf}"
    for fb in fallbacks:
        t = fb.replace("{name}", name)
        for suf in (SUFFIX, ""):
            if len(t) + len(suf) <= TITLE_MAX and not dangling(t):
                return t + suf
    if len(name) + len(SUFFIX) <= TITLE_MAX:
        return name + SUFFIX
    cut = name[:TITLE_MAX]                 # a very long repo name: cut at a separator, not mid-word
    i = max(cut.rfind(c) for c in "-_./")
    return (cut[:i] if i > TITLE_MAX // 2 else cut).rstrip("-_./ ")


def fallback_titles(lang="", stats=False):
    """Short titles from real data only, for when no description phrase fits."""
    fb = []
    if lang:
        fb.append("{name}: " + f"{lang} repo corpus for AI agents")
    if stats:
        fb.append("{name} source and stats")
    fb += ["{name}: repo corpus for AI agents", "{name}: corpus for AI agents"]
    return fb


def description(head, facts, tails):
    """'<head>. <facts>. <tail>' at most 160 chars; drops facts, then cuts head."""
    head = clean(head)
    facts = [f for f in facts if f]
    for tail in tails:
        for k in range(len(facts), -1, -1):
            fx = (" " + ", ".join(facts[:k]) + ".") if k else ""
            d = f"{head}.{fx} {tail}".strip()
            if len(d) <= DESC_MAX:
                return d
    tail = tails[-1]
    room = DESC_MAX - len(tail) - 2
    return f"{clip(head, room)}. {tail}".strip()


def stars_txt(n):
    return f"{n:,} GitHub star{'s' if n != 1 else ''}" if isinstance(n, int) and n > 0 else ""


def lang_of(st, w=None):
    return (st or {}).get("language") or ((w or {}).get("langs") or [""])[0]


def gh_text(st, fallback=""):
    return clean((st or {}).get("description") or fallback)


def human_bytes(n):
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024


def prefix_of(name):
    p = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")[:24].strip("-")
    return p or "repo"


def clone_prompt(repo, url, sha, page_url, spdx="", use=""):
    """The prompt for a repo not yet in the hub: clone at the pin, then autoindex it."""
    name = url.rstrip("/").split("/")[-1]
    p = prefix_of(name)
    pin = f" && git -C {name} checkout {sha}" if sha else ""
    lic = f"Licence: {spdx}." if spdx and spdx not in ("?", "NOASSERTION", "NONE") else "Licence: check the LICENSE file."
    use_line = (" Read it for the approach only, do not copy code." if use == "approach-only"
                else " Keep the copyright notice and credit the authors.")
    return (f"Search the GitHub repository {repo} with XERJ ({page_url}).\n"
            "Run these in a shell, in order, and check each result before the next:\n"
            "1. xerj --help  (success: it prints usage). If xerj is missing: curl -fsSL https://xerj.org/get | sh\n"
            f"2. git clone {url} {name}{pin}\n"
            "3. Start a local node and leave it running: xerj --insecure --data-dir ./xerj-data\n"
            f"4. xerj autoindex ./{name} --url http://127.0.0.1:9200 --prefix {p} --progress plain\n"
            "5. xerj autoindex map --url http://127.0.0.1:9200\n"
            "Then answer my question from the indexed files and cite the ax_path of every hit. "
            "Query syntax: https://xerj.org/llms.txt\n"
            f"{lic}{use_line}")


def questions(short, w):
    from contributors import clean_title
    topics = [t for t in (w.get("topics") or []) if t and t.lower() != short.lower()]
    generic = {"the project root", "src", "lib", "test", "tests", "spec", "docs", "doc", "app", "pkg", "internal", "examples", "scripts"}
    areas = [a for a in (w.get("areas") or []) if a and a.lower().strip("/") not in generic]
    t = clean_title(w.get("title"))
    qs = []
    if topics:
        qs.append(f"How does {short} implement {topics[0]}?")
    if t and len(t) <= 70 and not t.endswith(("…", "...")):
        qs.append(f"Where does {short} handle \u201c{nodash(t)}\u201d?")
    if areas:
        qs.append(f"What does {areas[0]} do in {short}?")
    if len(qs) < 3 and t and len(t) > 70:
        qs.append(f"What changed in {short} for \u201c{clip(nodash(t), 60)}\u201d?")
    if len(topics) > 1:
        qs.append(f"Show the {topics[1]} code in {short}.")
    return qs[:3]


# ---------------------------------------------------------------- related

class Related:
    """Live registry corpora and other repo pages, matched by language and topic."""

    def __init__(self, live, repo_pages, stats):
        self.items = []          # (slug, label, lang, topics, live)
        for c in live:
            st = stats.get(key((c["repos"] or [{}])[0].get("url"))) if len(c.get("repos") or []) == 1 else None
            tp = set((c.get("tags") or {}).get("topics") or []) | set((st or {}).get("topics") or [])
            self.items.append((c["slug"], c["slug"], (st or {}).get("language", ""), tp, True))
        for slug, (label, lang, tp) in repo_pages.items():
            self.items.append((slug, label, lang, set(tp), False))
        self.by_topic = collections.defaultdict(list)
        self.by_lang = collections.defaultdict(list)
        for i, it in enumerate(self.items):
            for t in it[3]:
                self.by_topic[t.lower()].append(i)
            if it[2]:
                self.by_lang[it[2]].append(i)

    def pick(self, seed, lang, topics, exclude=(), n_live=3, n_repo=3):
        tp = {t.lower() for t in topics if t}
        score = collections.Counter()
        for t in tp:
            if len(self.by_topic[t]) < 400:       # a topic on everything says nothing
                for i in self.by_topic[t]:
                    score[i] += 2
        if lang:
            for i in self.by_lang.get(lang, []):
                if score[i] or len(self.by_lang[lang]) < 60:
                    score[i] += 1
        h = lambda s: int.from_bytes((seed + s).encode(), "little") % 1000003
        ranked = sorted((i for i in score if self.items[i][0] not in exclude),
                        key=lambda i: (-score[i], h(self.items[i][0])))
        live = [self.items[i] for i in ranked if self.items[i][4] and score[i] >= 2][:n_live]
        live += [self.items[i] for i in ranked if self.items[i][4] and score[i] < 2][:n_live - len(live)]
        repo = [self.items[i] for i in ranked if not self.items[i][4]][:n_repo]
        return live, repo


# ---------------------------------------------------------------- JSON-LD

def spdx_url(s):
    return f"https://spdx.org/licenses/{s}" if s and re.match(r"^[A-Za-z0-9.+-]+$", s) and s not in ("NOASSERTION", "NONE") else ""


def crumbs_ld(items):
    return {"@type": "BreadcrumbList", "itemListElement": [
        {"@type": "ListItem", "position": i + 1, "name": n, **({"item": u} if u else {})}
        for i, (n, u) in enumerate(items)]}


def crumbs_html(items, esc):
    parts = [f'<a href="{esc(u)}">{esc(n)}</a>' if u else f"<span>{esc(n)}</span>" for n, u in items]
    return f'<nav class="crumbs" aria-label="Breadcrumb">{" / ".join(parts)}</nav>'


def repo_ld(slug, repo, url, desc, langs, spdx, topics, person, crumbs, sha="", files=None, nbytes=None, credit=""):
    page = f"{SITE}/corpus/{slug}"
    code = {"@type": "SoftwareSourceCode", "@id": page + "#code", "name": repo, "codeRepository": url}
    if desc:
        code["description"] = desc
    if langs:
        code["programmingLanguage"] = langs
    lic = spdx_url(spdx)
    if lic:
        code["license"] = lic
    if topics:
        code["keywords"] = topics
    ds = {"@type": "Dataset", "@id": page + "#dataset", "name": f"{repo} corpus",
          "description": clip(f"The {repo} repository at commit {sha[:10] or 'its pin'}, indexed so AI agents can search it."
                              + (f" {desc}." if desc else ""), 300),
          "url": page, "isBasedOn": url,
          "includedInDataCatalog": {"@type": "DataCatalog", "name": "XERJ Corpus Hub", "url": SITE + "/"}}
    if lic:
        ds["license"] = lic
    if topics:
        ds["keywords"] = topics
    if credit:
        ds["creditText"] = credit
    if sha:
        ds["version"] = sha[:12]
    out = {"breadcrumb": crumbs_ld(crumbs), "mainEntity": ds, "about": [code]}
    if person:
        code["contributor"] = person
        ds["contributor"] = person
    return out


# ---------------------------------------------------------------- weak pages
# A repo page is weak, and gets <meta name="robots" content="noindex,follow">
# (so it also leaves the sitemap), when the repository gives a reader too
# little to go on. Every number comes from data/repo_stats.json; the GSC list
# keeps any page Google already shows in results indexable.
WEAK_STARS = 5          # fewer GitHub stars than this
WEAK_COMMITS = 20       # and fewer commits on the default branch than this,
WEAK_FILES = 30         # or fewer files in the git tree at the pinned commit than this
SEEN = HERE / "data" / "gsc_seen_repo_pages.json"


def load_seen(path=SEEN):
    """Repo page slugs with at least one Google Search impression (GSC Pages exports)."""
    p = pathlib.Path(path)
    return set(json.loads(p.read_text(encoding="utf-8")).get("slugs", [])) if p.exists() else set()


def load_seen_contributors(path=SEEN):
    """Contributor logins whose /contributors/ page has at least one Google Search impression."""
    p = pathlib.Path(path)
    return set(json.loads(p.read_text(encoding="utf-8")).get("contributors", [])) if p.exists() else set()


def weak(f, seen=frozenset()):
    """'' for a page worth indexing, else the reason it is noindexed."""
    if f["slug"] in seen:
        return ""
    st = f["st"]
    if not st:
        return "no GitHub facts (repository missing, private or renamed)"
    stars = f["stars"] if isinstance(f["stars"], int) else 0
    if stars >= WEAK_STARS:
        return ""
    commits = st.get("commits")
    files = (f["tree"] or {}).get("files")
    if isinstance(commits, int) and commits < WEAK_COMMITS:
        return f"under {WEAK_STARS} stars and under {WEAK_COMMITS} commits"
    if isinstance(files, int) and files < WEAK_FILES:
        return f"under {WEAK_STARS} stars and under {WEAK_FILES} files at the pin"
    return ""


# ---------------------------------------------------------------- xerj.org guides
A, C = "https://xerj.org/answers/", "https://xerj.org/compare/"
GUIDE = {
    "ask": (A + "ask-questions-about-a-github-repo-without-cloning", "Ask questions about a GitHub repo without cloning it"),
    "reflib": (A + "index-reference-library-for-coding-agent", "Index a reference library for a coding agent"),
    "mcpbest": (C + "best-mcp-server-local-code-search-2026", "Best MCP servers for local code search in 2026, compared"),
    "rg": (C + "xerj-vs-ripgrep-for-code-agents", "ripgrep vs a code index for coding agents: 30 tasks"),
    "grep": (C + "best-grep-alternative-for-ai-coding-agents-2026", "Best grep alternative for AI coding agents (2026)"),
    "php": (A + "audit-php-codebase-with-an-index", "Audit a PHP codebase with an index, not grep"),
    "ipynb": (A + "search-jupyter-notebook-cells", "Search Jupyter notebooks (.ipynb) and the .py beside them"),
    "unity": (A + "search-unity-project-assets", "Search Unity scenes and the C# beside them"),
    "mcp": (A + "code-search-mcp-for-claude-code", "Code search MCP for Claude Code: cite file:line, not grep"),
    "mcpmem": (A + "what-is-an-mcp-memory-server", "What is an MCP memory server?"),
    "agents": (A + "local-search-mcp-for-codex-and-cursor", "A local search tool for Codex and Cursor"),
    "tokens": (A + "local-code-search-mcp-fewer-tokens-than-grep", "Local code search MCP that uses fewer tokens than grep"),
    "rag": (A + "rag-without-vector-database", "RAG without a vector database: one hybrid index"),
    "vdb": (C + "xerj-vs-vector-database", "Search API or a vector database for agent memory?"),
    "memory": (A + "what-is-agentic-memory", "What is agentic memory? A plain answer"),
    "memtools": (C + "xerj-vs-agent-memory-tools", "XERJ vs Mem0, Zep, Letta and Cognee for agent memory"),
    "es": (C + "xerj-vs-elasticsearch-local", "Elasticsearch-compatible engine that isn't Elasticsearch"),
    "esbest": (C + "best-lightweight-elasticsearch-compatible-server-2026", "Best lightweight Elasticsearch-compatible server (2026)"),
    "meili": (C + "xerj-vs-meilisearch", "XERJ vs Meilisearch for local and agent search"),
    "sqlite": (A + "full-text-search-sqlite-database", "Full-text search a SQLite database (FTS5 alternative)"),
    "sql": (A + "search-sql-dump-file", "Search a SQL dump without restoring it"),
    "openapi": (A + "search-openapi-spec-for-agent", "Search an OpenAPI spec so an agent can find endpoints"),
    "k8s": (A + "search-kubernetes-pod-logs-on-disk", "Search text across Kubernetes pod logs from many pods"),
    "config": (A + "search-yaml-xml-config-repository", "Find every place a config key is set in a repo"),
    "cve": (A + "batch-lookup-cve-ghsa-ids-offline", "Batch lookup CVE and GHSA IDs offline in one query"),
    "ast": (A + "syntax-aware-code-search-refactoring", "How do I run syntax-aware code search?"),
    "astgrep": (C + "xerj-vs-ast-grep", "XERJ vs ast-grep: syntax search and an index"),
    "logs": (A + "search-json-and-jsonl-logs", "Search JSON and JSONL logs plus gzip logs in one folder"),
    "obsidian": (C + "xerj-vs-obsidian-omnisearch", "XERJ vs Omnisearch for an Obsidian vault"),
    "monorepo": (A + "index-monorepo-for-agent", "Index a monorepo so your agent stops reading the whole repo"),
}
# GitHub topic -> guides, most specific first
TOPIC_GUIDES = [
    ({"mcp", "mcp-server", "model-context-protocol", "mcp-servers"}, ("mcp", "mcpmem")),
    ({"claude-code", "cursor", "codex", "coding-agent", "ai-coding", "ai-coding-assistant", "vibe-coding"}, ("agents", "tokens")),
    ({"rag", "retrieval-augmented-generation", "vector-database", "embeddings", "vector-search", "semantic-search"}, ("rag", "vdb")),
    ({"agent-memory", "llm-memory", "memory", "long-term-memory"}, ("memory", "memtools")),
    ({"elasticsearch", "opensearch", "search-engine", "full-text-search", "meilisearch", "typesense", "search"}, ("es", "meili", "esbest")),
    ({"sqlite", "sqlite3"}, ("sqlite",)),
    ({"sql", "mysql", "postgresql", "postgres", "database", "mariadb"}, ("sql",)),
    ({"openapi", "swagger", "rest-api", "openapi3"}, ("openapi",)),
    ({"kubernetes", "k8s"}, ("k8s", "config")),
    ({"helm", "ansible", "terraform", "devops", "infrastructure-as-code", "docker-compose"}, ("config",)),
    ({"unity", "unity3d", "unity-editor"}, ("unity",)),
    ({"obsidian", "obsidian-plugin", "obsidian-md"}, ("obsidian",)),
    ({"cve", "security", "vulnerability", "pentest", "exploit", "cybersecurity", "pentesting", "infosec"}, ("cve",)),
    ({"refactoring", "ast", "tree-sitter", "parser", "linter", "static-analysis", "compiler"}, ("ast", "astgrep")),
    ({"logging", "logs", "observability", "log-analysis"}, ("logs",)),
]
LANG_GUIDES = {"PHP": ("php",), "Jupyter Notebook": ("ipynb",)}
SHELF_GUIDES = {"API": ("openapi",), "SEC": ("cve",), "OPS": ("config",), "DB": ("sql",), "AI": ("rag",)}
MONOREPO_FILES = 2000
GENERAL = ("ask", "reflib", "rg", "mcpbest", "grep")


def guides(f, cats=None, n=4):
    """[(url, label, reason)]: xerj.org answers and comparisons that fit this repository,
    each with the fact that picked it. General guides fill the rest."""
    out, seen = [], set()

    def add(keys, reason):
        for k in keys:
            if k not in seen and len(out) < n:
                seen.add(k)
                out.append(GUIDE[k] + (reason,))
                reason = ""             # say why once per group
    tp = {t.lower() for t in f["topics"]}
    for tags, keys in TOPIC_GUIDES:
        hit = sorted(tp & tags)
        if hit:
            add(keys, f"{f['short']} is tagged {hit[0]} on GitHub.")
    langs = f["st"].get("languages") or [(l, None) for l in f["langs"]]
    for lang, pct in langs[:2]:
        if lang in LANG_GUIDES:
            add(LANG_GUIDES[lang], f"{lang} is {pct:g}% of the code by size." if isinstance(pct, (int, float)) else f"{f['short']} is written in {lang}.")
    files = (f["tree"] or {}).get("files")
    if isinstance(files, int) and files >= MONOREPO_FILES:
        add(("monorepo",), f"The pinned tree has {files:,} files.")
    if f["shelf"] in SHELF_GUIDES:
        cat = (cats or {}).get(f["shelf"], ("", ""))[0]
        add(SHELF_GUIDES[f["shelf"]], f"The hub files it under {cat}." if cat else "")
    gen = list(GENERAL)
    h = int.from_bytes(f["slug"].encode(), "little") % len(gen)
    add(gen[h:] + gen[:h], "")
    return out


def licence_text(f, esc, fetched=""):
    """GitHub's licence detection today against the licence the corpus uses at the pin."""
    gh = (f["st"].get("licence") or "").strip()
    idx = f["spdx"] if f["spdx"] not in ("", "?") else ""
    pin = f["sha"][:10] or "the pinned commit"
    reviewed = bool(f["w"].get("reviewed"))
    how = "was reviewed as" if reviewed else "reads as"
    if not idx:
        return ""
    when = f"on {esc(fetched)}" if fetched else "now"
    if gh and gh not in ("NOASSERTION", "NONE", "OTHER"):
        if gh.lower() == idx.lower():
            return (f"GitHub reported {esc(gh)} for {esc(f['repo'])} {when}, and the LICENSE file at commit {esc(pin)} {how} {esc(idx)}. "
                    f"They match, so answers from this corpus carry the same terms as the live repository.")
        return (f"GitHub reported {esc(gh)} for {esc(f['repo'])} {when}, but the LICENSE file at commit {esc(pin)} {how} {esc(idx)}. "
                f"The corpus is the pinned commit, so answers from it follow {esc(idx)}.")
    if gh:
        return (f"GitHub did not match the LICENSE file of {esc(f['repo'])} to a standard licence {when} ({esc(gh)}). "
                f"At commit {esc(pin)} it {how} {esc(idx)}, and that is the licence the corpus uses.")
    return f"GitHub reported no licence for {esc(f['repo'])} {when}. At commit {esc(pin)} the LICENSE file {how} {esc(idx)}."


def corpus_facts(f, esc):
    """<li> rows about what the indexed snapshot holds, from the tree and the selected commit."""
    st, w, tree = f["st"], f["w"], f["tree"]
    rows = []
    if tree:
        avg = tree["bytes"] / tree["files"] if tree["files"] else 0
        rows.append(f"{tree['files']:,} files, {human_bytes(tree['bytes'])} in total, about {human_bytes(avg)} per file.")
        if isinstance(w.get("files"), int) and w["files"] and tree["files"]:
            pct = 100 * w["files"] / tree["files"]
            rows.append(f"The selected commit changed {w['files']:,} of those files"
                        + (f" ({pct:.1f}%)" if pct >= 0.1 else "") + ".")
    langs = st.get("languages") or []
    if len(langs) > 1:
        rows.append("Languages by size: " + ", ".join(f"{esc(l)} {p:g}%" for l, p in langs[:5]) + ".")
    if st.get("created") and st.get("pushed"):
        rows.append(f"On GitHub since {esc(st['created'])}, last push {esc(st['pushed'])}"
                    + (f", {st['commits']:,} commits on {esc(st.get('branch') or 'the default branch')}" if isinstance(st.get("commits"), int) else "") + ".")
    ahead = st.get("pin_behind")
    if isinstance(ahead, int) and ahead > 0:
        rows.append(f"The pin is {ahead:,} commit{'s' if ahead != 1 else ''} behind {esc(st.get('branch') or 'the default branch')}; "
                    "the corpus answers from the pinned code, not the newest.")
    if st.get("archived"):
        rows.append("The repository is archived on GitHub, so the pinned code is also its final state.")
    return rows


def people_html(f, esc, people, pages, rel="../"):
    """The selected contributor, anyone else selected on the repo, and their other hub pages."""
    login = f["login"]
    pp = people.get(login) or {}
    name = pp.get("name") or login
    other = []
    for w in sorted((pp.get("work") or {}).values(), key=lambda w: (-(w.get("repo_stars") or 0), w["repo"].lower())):
        for s in w.get("corpora") or []:
            if s != f["slug"] and s in pages and s not in {x for x, _ in other}:
                other.append((s, w["repo"]))
    n = len(pp.get("work") or {})
    lines = [f'<a href="{rel}contributors/{esc(login)}">{esc(name)} (@{esc(login)})</a> has '
             f'{n} selected repositor{"y" if n == 1 else "ies"} on the hub.']
    if other:
        lines.append("Their other repository pages: " + ", ".join(
            f'<a href="{rel}corpus/{esc(s)}">{esc(r)}</a>' for s, r in other[:6]) + ".")
    for ol, _ in f.get("others") or []:
        op = people.get(ol) or {}
        k = len(op.get("work") or {})
        lines.append(f'<a href="{rel}contributors/{esc(ol)}">{esc(op.get("name") or ol)} (@{esc(ol)})</a> was also selected on {esc(f["repo"])}'
                     f' and has {k} selected repositor{"y" if k == 1 else "ies"} on the hub.')
    return "".join(f"<p>{x}</p>" for x in lines)


def enrich_html(f, esc, people, cats, pages, fetched="", rel="../"):
    """The data-only sections added to indexable repo pages."""
    out = []
    facts = corpus_facts(f, esc)
    if facts:
        out.append(f'<h2>Inside the indexed corpus</h2><ul class="rpfacts">{"".join(f"<li>{x}</li>" for x in facts)}</ul>')
    lic = licence_text(f, esc, fetched if f["st"] else "")
    if lic:
        out.append(f"<h2>Licence on GitHub and in the corpus</h2><p>{lic}</p>")
    out.append(f"<h2>Contributors</h2>{people_html(f, esc, people, pages, rel)}")
    g = guides(f, cats)
    if g:
        items = "".join(f'<li><a href="{esc(u)}">{esc(l)}</a>' + (f" {esc(r)}" if r else "") + "</li>" for u, l, r in g)
        out.append(f"<h2>Using XERJ with {esc(f['short'])}</h2><ul class=\"rpfacts\">{items}</ul>")
    return "\n".join(out)


# ---------------------------------------------------------------- repo page

USE = {"adapt-with-attribution": ("{spdx}, with credit", "Same terms as the repository. Answers name the repo, the commit and the file:line."),
       "approach-only": ("Approach only", "Agents read it for how it works, not to copy it. Answers name the repo, the commit and the file:line."),
       "verbatim-with-attribution": ("{spdx}, with credit", "Same terms as the repository. Answers name the repo, the commit and the file:line.")}


def repo_facts(slug, e, stats, descs):
    """Everything one repo page needs, from the registry and data/repo_stats.json."""
    src = (e.get("repos") or [{}])[0]
    sha_hint = (src.get("sha") or "").lower()

    def rank(item):
        who, work = item
        pin = (work.get("commit") or work.get("pin") or "").lower()
        # The manifest names one pin. Feature that commit so the person and the
        # snapshot are the same work when several people share the repository.
        # With one work, or when nothing matches the pin, this is the stars order.
        match = 0 if sha_hint and pin == sha_hint else 1
        return (match, -(work.get("repo_stars") or 0), who.lower())

    ordered = sorted(e["works"], key=rank)
    login, w = ordered[0]
    others = sorted(ordered[1:], key=lambda item: item[0].lower())
    url = src.get("url") or w["repo_url"]
    st = stats.get(key(url)) or {}
    repo = st.get("repo") or w["repo"] or "/".join(url.rstrip("/").split("/")[-2:])
    owner, short = repo.split("/", 1) if "/" in repo else ("", repo)
    sha = src.get("sha") or w.get("pin") or w.get("commit") or ""
    rv = src.get("review") or {}
    spdx = rv.get("spdx") or w.get("spdx") or st.get("licence") or ""
    use = w.get("suggested_use") or ""
    desc = gh_text(st, descs.get(slug, ""))
    topics = [t for t in (st.get("topics") or w.get("topics") or []) if t]
    langs = [l for l, _ in st.get("languages", [])] or list(w.get("langs") or [])
    stars = st.get("stars") if "stars" in st else w.get("repo_stars")
    return dict(slug=slug, login=login, w=w, others=others, url=url, st=st, repo=repo, owner=owner, short=short, sha=sha,
                spdx=spdx, use=use, desc=desc, topics=topics, langs=langs, stars=stars,
                lang=langs[0] if langs else "", shelf=w.get("shelf") or "",
                tree=st.get("tree") if "files" in (st.get("tree") or {}) else None)


def repo_title(f):
    return title(f["repo"], f["desc"], fallbacks=fallback_titles(f["lang"], bool(f["st"])))


def repo_description(f, dup=False):
    name = f["repo"] if dup else f["short"]
    head = f"{name}: {f['desc']}" if f["desc"] else f"{f['repo']} on GitHub, indexed for AI search"
    facts = [stars_txt(f["stars"]), f["lang"], f["spdx"] if f["spdx"] not in ("", "?", "NOASSERTION") else ""]
    return description(head, facts, ["See what XERJ indexed and copy the prompt to search it with AI.",
                                     "Copy the prompt to search it with AI."])


def _stat(v, label):
    return f"<div><b>{v}</b><span>{label}</span></div>"


def repo_body(f, esc, people, cats, related, fetched, rel="../", extra=""):
    from contributors import clean_title
    st, w, login = f["st"], f["w"], f["login"]
    page_url = f"{SITE}/corpus/{f['slug']}"
    shelf = f["shelf"]
    cat = (cats or {}).get(shelf, ("", ""))[0]
    crumbs = [("Hub", rel)] + ([(cat, f"{rel}category/{shelf}")] if cat else []) + [(f["repo"], "")]
    prompt = clone_prompt(f["repo"], f["url"], f["sha"], page_url, f["spdx"], f["use"])
    # stats: only what was fetched or is in the registry
    stats = []
    if isinstance(f["stars"], int):
        stats.append(_stat(f"{f['stars']:,}", "GitHub stars"))
    if isinstance(st.get("commits"), int):
        stats.append(_stat(f"{st['commits']:,}", f"commits on {esc(st.get('branch') or 'the default branch')}"))
    if isinstance(st.get("forks"), int):
        stats.append(_stat(f"{st['forks']:,}", "forks"))
    if st.get("release"):
        r = st["release"]
        stats.append(_stat(esc(clip(r["tag"], 14)), "latest release" + (f", {esc(r['at'])}" if r.get("at") else "")))
    stats_html = f'<section class="rpstats">{"".join(stats)}</section>' if stats else ""
    meta = []
    if st.get("languages"):
        meta.append(", ".join(f"{esc(l)} {p:g}%" for l, p in st["languages"][:4]))
    elif f["langs"]:
        meta.append(esc(", ".join(f["langs"][:4])))
    if st.get("created"):
        meta.append(f"created {esc(st['created'])}")
    if st.get("pushed"):
        meta.append(f"last push {esc(st['pushed'])}")
    if st.get("archived"):
        meta.append("archived")
    meta_html = f'<p class="mono rpmeta">{" · ".join(meta)}</p>' if meta else ""
    # what XERJ indexed
    sha = f["sha"]
    commit_link = (f'<a class="mono" href="{esc(f["url"])}/commit/{esc(sha)}">{esc(sha[:10])}</a>' if sha else "its pinned commit")
    when = (w.get("committed_at") or "")[:10] if sha and w.get("commit") == sha else ""
    snap_big = (f'{f["tree"]["files"]:,} files · {human_bytes(f["tree"]["bytes"])}' if f["tree"] else "Whole repository")
    ahead = st.get("pin_behind")
    ahead_txt = (f" {esc(st.get('branch') or 'The default branch')} is {ahead:,} commit{'s' if ahead != 1 else ''} ahead." if isinstance(ahead, int) and ahead > 0
                 else f" It is the newest commit on {esc(st.get('branch'))}." if ahead == 0 and st.get("branch") else "")
    snap = (f'<div class="rpbox"><h3>Snapshot</h3><div class="big">{snap_big}</div>'
            f'<p>The whole repository at commit {commit_link}{f" ({esc(when)})" if when else ""}.{ahead_txt}</p></div>')
    pp = people.get(login) or {}
    name = pp.get("name") or login
    t = clean_title(w.get("title"))
    size = ", ".join(x for x in [f'{w["files"]:,} files' if w.get("files") else "", f'{w["lines"]:,} lines' if w.get("lines") else ""] if x)
    areas = ", ".join(a for a in (w.get("areas") or [])[:3] if a)
    work_p = " ".join(x for x in [f"\u201c{esc(nodash(t))}\u201d." if t else "",
                                   (esc(size) + (f" in {esc(areas)}" if areas else "") + ".") if size else ""] if x)
    who = (f'<div class="rpwho"><img src="https://github.com/{esc(login)}.png?size=112" alt="" loading="lazy" width="56" height="56">'
           f'<div><a href="{rel}contributors/{esc(login)}"><b>{esc(name)}</b></a><br><span class="mono">@{esc(login)}</span></div></div>')
    also_bits = []
    for ol, ow in f.get("others") or []:
        pin = (ow.get("commit") or ow.get("pin") or "")[:12]
        also_bits.append(f'<a href="{rel}contributors/{esc(ol)}">@{esc(ol)}</a>' + (f" ({esc(pin)})" if pin else ""))
    also = f'<p>Also selected on this repository: {", ".join(also_bits)}.</p>' if also_bits else ""
    work = f'<div class="rpbox"><h3>Selected work</h3>{who}<p>{work_p}</p>{also}</div>'
    # licence
    spdx = f["spdx"] if f["spdx"] not in ("", "?") else "Not stated"
    reviewed = bool((w.get("reviewed")))
    lic1 = ("LICENSE file checked at the pinned commit; the author agreed to inclusion." if reviewed else
            "From GitHub's licence detection. A person still checks the LICENSE file at the pinned commit.")
    big2, p2 = USE.get(f["use"], ("Same as the repository", "Answers name the repo, the commit and the file:line."))
    agreed = (w.get("agreed") or "")[:10]
    channel = (w.get("channel") or "")
    # Email rows keep the historical sentence. A written consent that names
    # another channel (a GitHub discussion, an issue) must not be called email.
    if agreed and channel and "email" not in channel.lower():
        if "sign-in" in channel.lower():
            how = f"The owner added it on hub.xerj.org after signing in with GitHub, on {esc(agreed)}. "
        elif "discussion" in channel.lower():
            how = f"The author agreed in a GitHub discussion on {esc(agreed)}. "
        else:
            how = f"The author agreed on {esc(agreed)}. "
    elif agreed:
        how = f"The author agreed by email on {esc(agreed)}. "
    else:
        how = ""
    p2 = how + esc(p2)
    lic = (f'<div class="rptwo"><div class="rpbox"><h3>The repository</h3><div class="big">{esc(spdx)}</div><p>{lic1}</p></div>'
           f'<div class="rpbox"><h3>The XERJ corpus</h3><div class="big">{esc(big2.format(spdx=spdx))}</div><p>{p2}</p></div></div>'
           + (f'<p class="rpchips"><span class="chip ok">selected for the XERJ Corpus Hub</span><span class="chip ok">licence cleared</span>' if reviewed else
              f'<p class="rpchips"><span class="chip warn">in licence review</span><span class="chip">not in the hub yet</span>')
           + (f'<span class="chip">suggested use: {esc(f["use"].replace("-", " "))}</span>' if f["use"] else "") + "</p>")
    qs = questions(f["short"], dict(w, topics=f["topics"]))
    qs_html = f'<ul class="rpq">{"".join(f"<li>{esc(q)}</li>" for q in qs)}</ul>' if qs else ""
    live, repos = related
    links = [f'<a href="{rel}contributors/{esc(login)}">{esc(name)}</a>']
    if cat:
        links.append(f'<a href="{rel}category/{esc(shelf)}">{esc(cat)} corpora</a>')
    links += [f'<a href="{rel}corpus/{esc(s)}">{esc(lbl)}</a>' for s, lbl, *_ in live + repos]
    topics = "".join(f'<span class="chip">{esc(t)}</span>' for t in f["topics"][:8])
    src = ["Selected work and consent: the XERJ registry."]
    if st:
        src.insert(0, f"Stats: GitHub API{f', fetched {esc(fetched)}' if fetched else ''}.")
    if f["tree"]:
        src.insert(1, "Snapshot: the git tree at the pinned commit.")
    desc = esc(f["desc"]) if f["desc"] else "A GitHub repository, pinned at one commit so AI agents can search it."
    body = f'''<main class="rp">
{crumbs_html(crumbs, esc)}
<section class="rphero"><div>
<div class="rpowner"><a href="https://github.com/{esc(f["owner"])}">{esc(f["owner"])}</a> /</div>
<h1>{esc(f["short"])}</h1>
<p class="lede">{desc}</p>
<div class="pos">{POSITION}</div>
</div><div class="ctas">
<button class="copy cta1" data-copy="{esc(prompt)}">{CTA}</button>
<a class="cta2" href="{esc(f["url"])}">View on GitHub</a>
</div></section>
{stats_html}{meta_html}
<h2>What XERJ indexed</h2>
<div class="rptwo">{snap}{work}</div>
<h2>Licence</h2>
{lic}
{f'<p class="rpchips">{topics}</p>' if topics else ''}
{f'<h2>Example questions</h2>{qs_html}' if qs_html else ''}
{extra}
<h2>The prompt</h2>
<p>Paste it into your AI agent. It clones {esc(f["repo"])} at the pinned commit and indexes it on your machine.</p>
<pre class="rpprompt">{esc(prompt)}</pre>
<h2>Related</h2>
<p class="rplinks">{"".join(links)}</p>
<p class="rpsrc">{" ".join(src)}</p>
</main>'''
    person = {"@type": "Person", "name": name, "alternateName": "@" + login, "url": f"{SITE}/contributors/{login}",
              "sameAs": [f"https://github.com/{login}"]}
    ld = repo_ld(f["slug"], f["repo"], f["url"], f["desc"], f["langs"][:4], f["spdx"], f["topics"], person,
                 [(n, (SITE + "/" + u[len(rel):]) if u else page_url) for n, u in crumbs], sha,
                 credit=f"{f['repo']} by {f['owner']}, selected work by {name} (@{login})")
    return body, ld


STYLE = """
.rp{max-width:1080px;margin:0 auto;padding:var(--sp-6) var(--sp-6) var(--sp-16)}
.crumbs{font-family:var(--font-data);font-size:var(--fs-13);color:var(--z-mute);margin:0 0 var(--sp-6);overflow-wrap:anywhere}
.crumbs a{color:var(--z-mute)}
.rphero{display:grid;grid-template-columns:1fr auto;gap:var(--sp-8);align-items:end;border-bottom:1px solid var(--z-line);padding-bottom:var(--sp-8)}
.rpowner{font-family:var(--font-mono);font-size:var(--fs-16);color:var(--z-mute);overflow-wrap:anywhere}
.rphero h1{font-family:var(--font-display);font-weight:800;font-size:clamp(40px,7vw,88px);line-height:.95;margin:var(--sp-2) 0 var(--sp-4);letter-spacing:-.01em;text-transform:uppercase;overflow-wrap:anywhere}
.rp .lede{font-size:21px;margin:0;max-width:34em}
.rp .pos{font-family:var(--font-data);font-size:var(--fs-13);letter-spacing:var(--track-label);text-transform:uppercase;color:var(--z-accent);margin-top:var(--sp-4)}
.rp .ctas{display:flex;flex-direction:column;gap:var(--sp-3);min-width:240px}
.rp .cta1{background:var(--z-brand);color:var(--z-ink);border:0;font:700 16px var(--font-data);letter-spacing:var(--track-label);text-transform:uppercase;padding:18px 28px;cursor:pointer}
.rp .cta2{text-align:center;font:500 14px var(--font-data);padding:10px;border:1px solid var(--z-line);text-decoration:none;color:var(--z-ink)}
.rpstats{display:flex;flex-wrap:wrap;border-top:1px solid var(--z-line);border-left:1px solid var(--z-line);margin:var(--sp-8) 0 var(--sp-3)}
.rpstats div{flex:1 1 200px;max-width:50%;background:var(--z-bg);padding:var(--sp-5);border-right:1px solid var(--z-line);border-bottom:1px solid var(--z-line)}
.rpstats b{display:block;font-family:var(--font-display);font-size:40px;line-height:1;font-weight:700;overflow-wrap:anywhere}
.rpstats span{font-family:var(--font-data);font-size:var(--fs-13);color:var(--z-mute)}
.rp h2{font-family:var(--font-data);font-size:var(--fs-20);margin:var(--sp-10) 0 var(--sp-4)}
.rptwo{display:grid;grid-template-columns:1fr 1fr;gap:var(--sp-6)}
.rpbox{border:1px solid var(--z-line);padding:var(--sp-6);background:#fff;min-width:0}
.rpbox h3{margin:0 0 var(--sp-2);font:600 13px var(--font-data);letter-spacing:var(--track-label);text-transform:uppercase;color:var(--z-mute)}
.rpbox .big{font-family:var(--font-display);font-size:34px;font-weight:700;line-height:1.1;overflow-wrap:anywhere}
.rpbox p{margin:var(--sp-2) 0 0;font-size:16px;overflow-wrap:anywhere}
.rp .mono{font-family:var(--font-mono);font-size:14px}
.rpmeta{color:var(--z-mute);margin:0}
.rpchips .chip{display:inline-block;margin:0 6px 6px 0}
.rpq{padding-left:1.1em;margin:0}
.rpwho{display:flex;gap:var(--sp-4);align-items:center;margin-bottom:var(--sp-2)}
.rpwho img{width:56px;height:56px;border-radius:50%;background:#fff}
.rplinks a{display:inline-block;margin:0 var(--sp-4) var(--sp-2) 0}
.rpprompt{white-space:pre-wrap;overflow-wrap:anywhere;font-size:13px;background:#fff;border:1px solid var(--z-line);padding:var(--sp-4)}
.rpsrc{font-size:var(--fs-13);color:var(--z-mute);margin-top:var(--sp-10);border-top:1px solid var(--z-line);padding-top:var(--sp-4)}
.rpfacts{margin:var(--sp-4) 0}
@media (max-width:760px){.rphero{grid-template-columns:1fr}.rpstats div{flex-basis:40%}.rptwo{grid-template-columns:1fr}.rp .ctas{min-width:0}.rp .lede{font-size:18px}.rp{padding:var(--sp-4) var(--sp-4) var(--sp-12)}}
"""
