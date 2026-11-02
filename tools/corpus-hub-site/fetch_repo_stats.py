#!/usr/bin/env python3
"""Fetch GitHub facts for every single-repo corpus into data/repo_stats.json.

gen.py reads the saved file and never touches the network, so the site still
builds offline. A fact is shown only when it is in the file.

Token: GH_TOKEN or GITHUB_TOKEN, else `gh auth token`. Run from anywhere:
    python3 tools/corpus-hub-site/fetch_repo_stats.py            # GraphQL facts + trees
    python3 tools/corpus-hub-site/fetch_repo_stats.py --no-trees # GraphQL only

GraphQL: one query per 40 repos, using aliases (stars, forks, languages,
licence, topics, created, pushed, latest release, commits on the default
branch, and how far the default branch is past the pinned commit).
Trees: one REST call per corpus with no indexed file count (author corpora),
for the file count and bytes of the tree at the pinned commit. A truncated
tree is stored as truncated with no numbers. Aggregates (more than 50 repos,
like the exploit-pocs collections) are never fetched.
"""
import concurrent.futures, datetime, http.client, json, os, re, subprocess, sys, time, urllib.error, urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
OUT = HERE / "data/repo_stats.json"
API = "https://api.github.com"
BATCH = 25
AGGREGATE = 50
GH = re.compile(r"^https://github\.com/([^/]+)/([^/#?]+?)(?:\.git)?/?$")
FIELDS = """nameWithOwner stargazerCount forkCount description homepageUrl isArchived isFork createdAt pushedAt
licenseInfo{spdxId} primaryLanguage{name}
languages(first:6,orderBy:{field:SIZE,direction:DESC}){totalSize edges{size node{name}}}
repositoryTopics(first:12){nodes{topic{name}}} latestRelease{tagName publishedAt}
defaultBranchRef{name target{...on Commit{history{totalCount}}}%s}"""


def token():
    t = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if t:
        return t
    return subprocess.run(["gh", "auth", "token"], capture_output=True, text=True, check=True).stdout.strip()


def targets():
    """{"owner/repo" (lower): {"repo": "Owner/Repo", "sha": pin, "tree": need_tree}}"""
    import gen
    want = {}

    def add(url, sha, tree):
        m = GH.match(url or "")
        if not m:
            return
        name = f"{m.group(1)}/{m.group(2)}"
        k = name.lower()
        cur = want.setdefault(k, {"repo": name, "sha": sha or "", "tree": False})
        cur["sha"] = cur["sha"] or sha or ""
        cur["tree"] = cur["tree"] or tree
    for c in gen.load_registry():
        if 0 < len(c["repos"]) <= AGGREGATE:
            for r in c["repos"]:
                add(r.get("url"), r.get("sha"), not r.get("files"))
    for p in sorted((gen.HUB / "candidates" / "authors").glob("*.json")):
        d = json.loads(p.read_text())
        for r in d.get("repos", []):
            add(r.get("url"), r.get("sha"), True)
    for p in sorted((gen.HUB / "candidates").glob("*.json")):
        d = json.loads(p.read_text())
        if len(d.get("repos", [])) <= AGGREGATE:
            for r in d["repos"]:
                add(r.get("url"), r.get("sha"), True)
    return want


def post(tok, query, tries=4):
    body = json.dumps({"query": query}).encode()
    for i in range(tries):
        req = urllib.request.Request(API + "/graphql", data=body, headers={
            "Authorization": "bearer " + tok, "Content-Type": "application/json", "User-Agent": "xerj-hub-stats"})
        try:
            with urllib.request.urlopen(req, timeout=90) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            if e.code in (502, 503, 504, 403, 429) and i < tries - 1:
                time.sleep(10 * (i + 1))
                continue
            raise
        except (urllib.error.URLError, TimeoutError):
            if i < tries - 1:
                time.sleep(10 * (i + 1))
                continue
            raise


def gql_q(s):
    return json.dumps(s)


def shape(n, sha):
    if not n:
        return None
    langs = n.get("languages") or {}
    tot = langs.get("totalSize") or 0
    br = n.get("defaultBranchRef") or {}
    hist = ((br.get("target") or {}).get("history") or {}).get("totalCount")
    cmp_ = br.get("compare") or {}
    rel = n.get("latestRelease") or {}
    s = {
        "repo": n["nameWithOwner"], "stars": n["stargazerCount"], "forks": n["forkCount"],
        "description": (n.get("description") or "").strip(), "homepage": (n.get("homepageUrl") or "").strip(),
        "archived": n["isArchived"], "created": n["createdAt"][:10], "pushed": (n.get("pushedAt") or "")[:10],
        "licence": (n.get("licenseInfo") or {}).get("spdxId") or "",
        "language": (n.get("primaryLanguage") or {}).get("name") or "",
        "languages": [[e["node"]["name"], round(100 * e["size"] / tot, 1)] for e in langs.get("edges", []) if tot],
        "topics": [t["topic"]["name"] for t in (n.get("repositoryTopics") or {}).get("nodes", [])],
        "branch": br.get("name") or "", "commits": hist,
    }
    if rel:
        s["release"] = {"tag": rel["tagName"], "at": (rel.get("publishedAt") or "")[:10]}
    if sha and cmp_ and cmp_.get("aheadBy") == 0:
        s["pin_behind"] = cmp_.get("behindBy")       # commits on the default branch after the pin
    return {k: v for k, v in s.items() if v not in (None, "", [])}


def query(tok, want, chunk, done):
    """One aliased GraphQL document for chunk. A batch the API keeps timing out
    on (compare is the slow part) is split in two, down to one repository."""
    parts = []
    for j, k in enumerate(chunk):
        owner, name = want[k]["repo"].split("/", 1)
        sha = want[k]["sha"]
        cmp_ = f" compare(headRef:{gql_q(sha)}){{aheadBy behindBy}}" if re.fullmatch(r"[0-9a-f]{40}", sha) else ""
        parts.append(f"r{j}: repository(owner:{gql_q(owner)}, name:{gql_q(name)}){{{FIELDS % cmp_}}}")
    try:
        res = post(tok, "{" + "\n".join(parts) + "\nrateLimit{remaining cost}}", tries=2 if len(chunk) > 1 else 3)
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError) as e:
        if len(chunk) == 1:
            print(f"graphql: skipped {chunk[0]} ({e})", file=sys.stderr)
            return {}
        half = len(chunk) // 2
        rl = query(tok, want, chunk[:half], done)
        return query(tok, want, chunk[half:], done) or rl
    data = res.get("data") or {}
    missing = {e.get("path", [None])[0] for e in res.get("errors", []) if e.get("type") == "NOT_FOUND"}
    for j, k in enumerate(chunk):
        node = data.get(f"r{j}")
        if node:
            done[k] = shape(node, want[k]["sha"])
        elif f"r{j}" in missing:
            done[k] = {"repo": want[k]["repo"], "missing": True}
    return data.get("rateLimit") or {}


def fetch_graphql(tok, want, done):
    keys = [k for k in sorted(want) if k not in done]
    for i in range(0, len(keys), BATCH):
        rl = query(tok, want, keys[i:i + BATCH], done)
        print(f"graphql {min(i + BATCH, len(keys))}/{len(keys)} remaining={rl.get('remaining')}", file=sys.stderr)
        if (i // BATCH) % 10 == 9:
            save_partial(done)


PARTIAL = OUT.with_name(".repo_stats.partial.json")


def save_partial(done):
    PARTIAL.write_text(json.dumps({"date": datetime.date.today().isoformat(), "repos": done}), encoding="utf-8")


def core_remaining(tok):
    req = urllib.request.Request(API + "/rate_limit", headers={"Authorization": "bearer " + tok, "User-Agent": "xerj-hub-stats"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())["resources"]["core"]["remaining"]


def tree(tok, repo, sha):
    req = urllib.request.Request(f"{API}/repos/{repo}/git/trees/{sha}?recursive=1", headers={
        "Authorization": "bearer " + tok, "User-Agent": "xerj-hub-stats", "Accept": "application/vnd.github+json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            d = json.loads(r.read())
    except urllib.error.HTTPError as e:
        return {"error": e.code}
    except (http.client.IncompleteRead, urllib.error.URLError, TimeoutError, ConnectionError, ValueError):
        return {"error": "network"}  # one bad read should not lose the whole run
    if d.get("truncated"):
        return {"truncated": True}
    blobs = [e for e in d.get("tree", []) if e.get("type") == "blob"]
    return {"files": len(blobs), "bytes": sum(e.get("size", 0) for e in blobs)}


def fetch_trees(tok, want, done, floor=800):
    todo = [k for k in sorted(want) if want[k]["tree"] and re.fullmatch(r"[0-9a-f]{40}", want[k]["sha"])
            and k in done and not done[k].get("missing") and "tree" not in done[k]]
    left = core_remaining(tok)
    budget = max(0, left - floor)
    if len(todo) > budget:
        print(f"trees: {len(todo)} wanted, REST budget {budget} (keeping {floor}); rerun later for the rest", file=sys.stderr)
        todo = todo[:budget]
    with concurrent.futures.ThreadPoolExecutor(6) as ex:
        for n, (k, t) in enumerate(zip(todo, ex.map(lambda k: tree(tok, want[k]["repo"], want[k]["sha"]), todo)), 1):
            if "error" not in t:
                t["sha"] = want[k]["sha"]
                done[k]["tree"] = t
            if n % 200 == 0:
                print(f"trees {n}/{len(todo)}", file=sys.stderr)


def main():
    tok = token()
    want = targets()
    old = json.loads(OUT.read_text()).get("repos", {}) if OUT.exists() else {}
    done = {k: v for k, v in old.items() if k in want}
    # keep trees only when the pin is unchanged
    for k, v in done.items():
        if v.get("tree", {}).get("sha") not in (None, want[k]["sha"]):
            v.pop("tree")
    refresh = {k: v.get("tree") for k, v in done.items()}
    done = {}
    # resume a run that stopped part way (same day only)
    if PARTIAL.exists():
        p = json.loads(PARTIAL.read_text())
        if p.get("date") == datetime.date.today().isoformat():
            done = {k: v for k, v in p["repos"].items() if k in want}
            print(f"resuming: {len(done)} from {PARTIAL.name}", file=sys.stderr)
    fetch_graphql(tok, want, done)
    save_partial(done)
    for k, t in refresh.items():
        if t and k in done and not done[k].get("missing"):
            done[k]["tree"] = t
    if "--no-trees" not in sys.argv:
        fetch_trees(tok, want, done)
    OUT.parent.mkdir(exist_ok=True)
    body = ",\n".join(f"{json.dumps(k)}: {json.dumps(done[k], ensure_ascii=False, sort_keys=True)}" for k in sorted(done))
    OUT.write_text('{"fetched": "%s", "source": "GitHub GraphQL and REST git trees", "repos": {\n%s\n}}\n'
                   % (datetime.date.today().isoformat(), body), encoding="utf-8")
    PARTIAL.unlink(missing_ok=True)
    got = sum(1 for v in done.values() if not v.get("missing"))
    trees = sum(1 for v in done.values() if "files" in v.get("tree", {}))
    print(f"wanted {len(want)}, stats {got}, missing {len(done) - got}, trees {trees}", file=sys.stderr)


if __name__ == "__main__":
    main()
