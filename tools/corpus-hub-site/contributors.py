"""Contributor pages for hub.xerj.org: one GitHub-profile-style page per author
who agreed to contribute, a wall of all of them, and a README badge each.

Sources: tools/xerj-code/hub/candidates/authors/CONTRIBUTORS.jsonl (written by
the outreach pipeline; one row per approved contribution), the themed candidate
corpora's CONTRIBUTORS-2026-10.md, and the registry manifests. A person is one
login however many corpora they appear in (see load()). Every number on a
page is read from those files; a number we do not have is left off the page,
never estimated. There is no per-author retrieval counter yet (nothing here is
live), so the pages show no retrieval figure at all rather than a placeholder.

  out/contributors/index.html          the wall: star treemap + first chunk of the list
  out/contributors/list.<n>.json       the rest of the list, CHUNK rows a file (infinite scroll)
  out/contributors/list.<n>.html       the same chunks as plain HTML pages (crawlable fallback)
  out/contributors/<login>.html        one author (or one upstream owner, see upstream())
  out/contributors/badge.svg          "XERJ Corpus Hub | contributor" badge, served at /contributors/<login>/badge.svg
  out/corpus/<slug>.html               candidate corpora (author / themed) that have no
                                       registry page yet, so every contributor's corpus links
"""
import datetime, html, json, pathlib, re, urllib.parse, collections
import treemap
import repo_pages

LOGIN_RE = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})$")   # GitHub's login alphabet: safe as a file name
SITE = "https://hub.xerj.org"

STYLE = """
/* contributor pages */
.cwrap{max-width:1180px;margin:0 auto;padding:var(--sp-10) var(--page-x)}
.prof{display:grid;grid-template-columns:240px 1fr;gap:var(--sp-10);align-items:start}
.prof .av{width:240px;height:240px;border-radius:50%;border:1px solid var(--z-line);background:#fff;display:block}
.prof h1{font-family:var(--font-display);font-weight:900;font-size:var(--fs-56);line-height:1;margin:0 0 var(--sp-2)}
.prof .login{font-family:var(--font-mono);color:var(--z-mute);font-size:var(--fs-20)}
.prof .login a{text-decoration:none}
.ribbon{display:inline-block;margin:var(--sp-4) 0;font-family:var(--font-data);font-size:var(--fs-11);font-weight:700;
text-transform:uppercase;letter-spacing:var(--track-ui);color:#fff;padding:.5em 1em;
background:linear-gradient(100deg,#6d28d9,#9333ea,#7c3aed,#6d28d9);background-size:280% 100%;animation:ctaflow 5s ease infinite}
.kpis{display:flex;flex-wrap:wrap;gap:1px;background:var(--z-line);border:1px solid var(--z-line);margin:var(--sp-6) 0}
.kpi{flex:1 1 150px;background:#fff;padding:var(--sp-5)}
.kpi b{display:block;font-family:var(--font-display);font-weight:800;font-size:var(--fs-32);line-height:1.1}
.kpi span{font-family:var(--font-data);font-size:var(--fs-11);text-transform:uppercase;letter-spacing:var(--track-label);color:var(--z-mute)}
.kpi.pending b{color:var(--z-mute);font-size:var(--fs-20);padding-top:.35em}
.contrib{border:1px solid var(--z-line);background:#fff;padding:var(--sp-5) var(--sp-6);margin:var(--sp-4) 0}
.contrib h3{margin:0 0 var(--sp-2);font-family:var(--font-data);font-size:var(--fs-20)}
.contrib h3 a{text-decoration:none}
.contrib .stars{float:right;font-family:var(--font-data);font-weight:600;color:var(--z-accent)}
.contrib .commit{font-family:var(--font-mono);font-size:var(--fs-13);color:var(--z-mute);margin:var(--sp-2) 0}
.contrib .repofacts p{margin:var(--sp-2) 0}
.chips{display:flex;flex-wrap:wrap;gap:var(--sp-2);margin-top:var(--sp-3)}
.chip{font-family:var(--font-data);font-size:var(--fs-11);text-transform:uppercase;letter-spacing:var(--track-label);
border:1px solid var(--z-line);padding:.25em .6em;color:var(--z-mute)}
.chip.ok{color:var(--ok);border-color:var(--ok)}.chip.warn{color:var(--warn);border-color:var(--warn)}
.share{display:flex;gap:var(--sp-3);flex-wrap:wrap;margin:var(--sp-6) 0}
.share a,.share button{font-family:var(--font-data);font-size:var(--fs-11);font-weight:600;text-transform:uppercase;
letter-spacing:var(--track-ui);text-decoration:none;color:var(--z-ink);border:1px solid var(--z-ink);background:none;padding:.6em 1em;cursor:pointer}
.share a:hover,.share button:hover{background:var(--z-ink);color:#fff}
.agentuse{border:1px solid var(--z-line);background:#fff;padding:var(--sp-5) var(--sp-6);margin:var(--sp-4) 0}
.agentuse h2{margin:0 0 var(--sp-3)}
.agentuse .rdesc{margin:0 0 var(--sp-3)}
.agentuse ul{margin:var(--sp-2) 0 var(--sp-4);padding-left:1.2em}
.agentuse li{margin:var(--sp-1) 0}
.xposts{margin:var(--sp-4) 0}
.xposts h2{font-size:var(--fs-13);text-transform:uppercase;letter-spacing:.04em;margin:0 0 var(--sp-2)}
.xposts blockquote{margin:0 0 var(--sp-3);padding:var(--sp-2) var(--sp-4);border-left:3px solid var(--z-line)}
.xposts blockquote p{margin:0 0 var(--sp-2);overflow-wrap:anywhere}
.xposts blockquote p.by{font-size:var(--fs-13);color:var(--z-mute);margin:0}
.agentuse pre{font-family:var(--font-mono);font-size:var(--fs-13);white-space:pre-wrap;word-break:break-word;margin:var(--sp-2) 0;background:var(--z-paper,#f6f6f4);padding:var(--sp-3)}
.agentuse .askhead{display:flex;justify-content:space-between;align-items:center;gap:var(--sp-3)}
.agentuse .askhead button{font-family:var(--font-data);font-size:var(--fs-11);font-weight:600;text-transform:uppercase;border:1px solid var(--z-ink);background:#fff;padding:2px 8px;cursor:pointer}
.badgebox{border:1px solid var(--z-line);background:#fff;padding:var(--sp-4) var(--sp-5);margin:var(--sp-4) 0}
.badgebox pre{font-family:var(--font-mono);font-size:var(--fs-13);white-space:pre-wrap;word-break:break-all;margin:var(--sp-3) 0 0}
.wall{display:grid;grid-template-columns:repeat(auto-fill,minmax(220px,1fr));gap:var(--sp-3);margin-top:var(--sp-6)}
.wall a{display:flex;gap:var(--sp-3);align-items:center;border:1px solid var(--z-line);background:#fff;padding:var(--sp-3);text-decoration:none}
.wall img{width:44px;height:44px;border-radius:50%;flex:none}
.wall div{min-width:0}.wall b,.wall span{overflow-wrap:anywhere}
.wall b{display:block;font-family:var(--font-data);font-size:var(--fs-13)}
.wall span{font-family:var(--font-mono);font-size:var(--fs-11);color:var(--z-mute)}
.whead h1{font-family:var(--font-display);font-weight:900;font-size:var(--fs-96);line-height:.95;margin:0}
.whead p{max-width:760px;color:var(--z-mute)}
.hubstar[hidden]{display:none}
section.more[hidden],.signin-hint[hidden]{display:none}
section.more{margin:var(--sp-8) 0}
.signin-hint{border:1px dashed var(--z-line);padding:var(--sp-3) var(--sp-5);color:var(--z-mute);font-size:var(--fs-13);background:#fff}
.moretable{width:100%;border-collapse:collapse;margin:var(--sp-3) 0;font-size:var(--fs-13);background:#fff}
.moretable th,.moretable td{border:1px solid var(--z-line);padding:var(--sp-1) var(--sp-3);text-align:left;vertical-align:top;word-break:break-word}
.moretable th:empty{display:none}
.thisisme{border:2px solid var(--z-accent);background:#fff;padding:var(--sp-5) var(--sp-6);margin:var(--sp-4) 0}
.thisisme h2{margin-top:0}
.kpi.hubstar b [data-n]{font:inherit;color:inherit;letter-spacing:normal;text-transform:none}
.hubstar button{margin-top:var(--sp-2);font-family:var(--font-data);font-size:var(--fs-11);font-weight:700;text-transform:uppercase;
letter-spacing:var(--track-ui);border:1px solid var(--z-accent);color:var(--z-accent);background:none;padding:.35em .8em;cursor:pointer}
.hubstar button[disabled]{opacity:.6;cursor:default}
.hubstar.inline{display:inline-flex;gap:var(--sp-3);align-items:center;border:1px solid var(--z-line);background:#fff;padding:var(--sp-2) var(--sp-4);margin:var(--sp-3) 0}
.hubstar.inline b{font-family:var(--font-data)}.hubstar.inline button{margin:0}
.whead h2,.wallh{font-family:var(--font-display);font-weight:800;font-size:var(--fs-32);line-height:1.1;margin:var(--sp-10) 0 0}
.wallmore{margin:var(--sp-6) 0;color:var(--z-mute);font-size:var(--fs-13)}
.wallmore button,.pager a{font-family:var(--font-data);font-size:var(--fs-13);font-weight:600;text-decoration:none;color:var(--z-ink);
border:1px solid var(--z-ink);background:#fff;padding:.5em 1em;cursor:pointer;margin-left:var(--sp-3)}
.wallmore button[hidden],.pager[hidden]{display:none}
.pager{display:flex;flex-wrap:wrap;gap:var(--sp-2);align-items:center;margin:var(--sp-6) 0}
.pager a{margin:0;min-width:44px;text-align:center}.pager a[aria-current]{background:var(--z-ink);color:#fff}
.people h3{font-family:var(--font-data);font-size:var(--fs-20);margin:var(--sp-6) 0 var(--sp-2)}
.chip.up{color:var(--z-cmp);border-color:var(--z-cmp)}
@media (max-width:820px){.prof{grid-template-columns:1fr}.prof .av{width:140px;height:140px}.kpi{flex-basis:40%}.whead h1{font-size:var(--fs-56)}}
@media (max-width:420px){.whead h1{font-size:44px}.wall{grid-template-columns:1fr}}
""" + treemap.STYLE

COPY_BADGE_JS = """<script>
document.querySelectorAll('[data-copy]').forEach(function(b){b.addEventListener('click',function(){
  navigator.clipboard.writeText(document.getElementById(b.dataset.copy).innerText).then(function(){var t=b.innerText;b.innerText='COPIED';setTimeout(function(){b.innerText=t},1400)})})});
</script>"""


REPO_JS = """<script>
document.querySelectorAll('button.copy').forEach(function(b){b.addEventListener('click',function(){
  var t=b.getAttribute('data-copy')||'',o=b.textContent;
  function done(){b.textContent='Copied';setTimeout(function(){b.textContent=o},1400)}
  if(navigator.clipboard&&navigator.clipboard.writeText){navigator.clipboard.writeText(t).then(done,function(){})}
  else{var a=document.createElement('textarea');a.value=t;document.body.appendChild(a);a.select();try{document.execCommand('copy');done()}catch(_){}document.body.removeChild(a)}
})});
</script>"""


THEMED_MD = "CONTRIBUTORS-2026-10.md"
# | [owner/repo](url) | @login | [sha10](commit_url) title | SPDX | YYYY-MM-DD |
THEMED_ROW = re.compile(r"^\| \[([^\]]+)\]\((https://github\.com/[^)]+)\) \| @([A-Za-z0-9-]+) \| "
                        r"\[([0-9a-f]{7,40})\]\(([^)]+)\) ?(.*?) \| ([^|]*) \| (\d{4}-\d{2}-\d{2}) \|$")
GH_OWNER = re.compile(r"^https://github\.com/([A-Za-z0-9-]+)/", re.I)


def _reviewed(review):
    """A source counts as licence-reviewed only when its review block records a
    verdict (use is not UNREVIEWED) and who made it. Never inferred."""
    use = (review or {}).get("use", "UNREVIEWED")
    return bool(use and use != "UNREVIEWED" and (review or {}).get("by"))


def _person(people, login, name=""):
    key = login.lower()                    # GitHub logins are case-insensitive
    if key not in people:
        people[key] = {"login": login, "name": name, "work": {}, "corpora": {}, "agreed": []}
    elif name and not people[key]["name"]:
        people[key]["name"] = name
    return people[key]


def load(hub_dir, registry=None):
    """Every contributor who agreed by email, keyed on login (case-insensitive),
    with their contributions gathered from EVERY corpus they appear in:

      * candidates/authors/CONTRIBUTORS.jsonl  one corpus per approved repo
      * candidates/CONTRIBUTORS-2026-10.md     authors in the themed candidate
                                               corpora (agent-engineering, ...)
      * the registry's lane-A manifests        corpora that pin a repository
                                               the contributor owns

    A contribution is one selected commit; the same commit reached through two
    corpora is counted once (files and lines too), but both corpora are listed.
    `registry` is gen.load_registry()'s list; only its slug/status/repos are used.
    """
    hub = pathlib.Path(hub_dir)
    people = {}
    # the ledger is split under the registry's 5 MB file limit: CONTRIBUTORS.jsonl, CONTRIBUTORS-2.jsonl, ...
    for ledger in sorted((hub / "candidates" / "authors").glob("CONTRIBUTORS*.jsonl"), key=lambda p: int(p.stem.partition("-")[2] or 1)):
        for line in ledger.read_text().splitlines():
            if not line.strip():
                continue
            r = json.loads(line)
            c = r.get("contributor") or {}
            login = c.get("login") or ""
            if not LOGIN_RE.match(login):
                continue
            pp = _person(people, login, c.get("name") or "")
            k = r.get("contribution") or {}
            lic = r.get("licence") or {}
            key = (k.get("commit") or f'{r["corpus"]}:nocommit').lower()
            w = pp["work"].setdefault(key, {
                "repo": r.get("repo", ""), "repo_url": r.get("repo_url", ""), "repo_stars": r.get("repo_stars"),
                "commit": k.get("commit") or "", "commit_url": k.get("commit_url") or "", "title": k.get("title") or "",
                "files": k.get("files"), "lines": k.get("lines"), "langs": k.get("langs") or [], "areas": k.get("areas") or [],
                "topics": r.get("topics") or [], "shelf": r.get("shelf") or "", "spdx": lic.get("spdx") or "",
                "reviewed": lic.get("reviewed") is True, "agreed": (r.get("consent") or {}).get("at", ""), "corpora": [],
                "committed_at": k.get("committed_at") or "", "suggested_use": lic.get("suggested_use") or "",
                "pin": (r.get("pin") or {}).get("sha") or "",
                "channel": (r.get("consent") or {}).get("channel") or "",
            })
            if r["corpus"] not in w["corpora"]:
                w["corpora"].append(r["corpus"])
            pp["corpora"].setdefault(r["corpus"], "author corpus")
            if w["agreed"]:
                pp["agreed"].append(w["agreed"])
    md = hub / "candidates" / THEMED_MD
    if md.exists():
        corpus = None
        for line in md.read_text().splitlines():
            if line.startswith("## "):
                corpus = line[3:].split(" (")[0].strip()
                continue
            m = THEMED_ROW.match(line.strip())
            if not (m and corpus):
                continue
            repo, url, login, sha, curl, title, spdx, date = m.groups()
            if not LOGIN_RE.match(login):
                continue
            pp = _person(people, login)
            manifest = hub / "candidates" / f"{corpus}.json"
            review = {}
            if manifest.exists():
                for src in json.loads(manifest.read_text()).get("repos", []):
                    if src.get("url", "").rstrip("/").lower() == url.rstrip("/").lower():
                        review = src.get("review") or {}
            # an abbreviated sha in the table matches the ledger's full sha
            key = next((k for k in pp["work"] if k.startswith(sha.lower())), sha.lower())
            w = pp["work"].setdefault(key, {
                "repo": repo, "repo_url": url, "repo_stars": None, "commit": sha, "commit_url": curl,
                "title": title.strip(), "files": None, "lines": None, "langs": [], "areas": [], "topics": [],
                "shelf": "", "spdx": spdx.strip(), "reviewed": _reviewed(review), "agreed": date, "corpora": [],
                "committed_at": "", "suggested_use": review.get("use", "") if _reviewed(review) else "", "pin": "",
            })
            if corpus not in w["corpora"]:
                w["corpora"].append(corpus)
            pp["corpora"].setdefault(corpus, "candidate corpus")
            pp["agreed"].append(date)
    # corpora in the registry that pin a repository the contributor owns
    for c in registry or []:
        for src in c.get("repos") or []:
            m = GH_OWNER.match(src.get("url", ""))
            if m and m.group(1).lower() in people:
                people[m.group(1).lower()]["corpora"][c["slug"]] = c.get("status") or "registry"
    return {pp["login"]: pp for pp in people.values() if pp["work"]}


def volume(pp, live_slugs=()):
    """The numbers on a profile, all summed over distinct commits / repos."""
    work = list(pp["work"].values())
    stars_by_repo = {}
    for w in work:
        if isinstance(w.get("repo_stars"), int):
            stars_by_repo[w["repo_url"].lower()] = w["repo_stars"]
    return {
        "commits": sum(1 for w in work if w["commit"]),
        "files": sum(w["files"] for w in work if isinstance(w.get("files"), int)),
        "lines": sum(w["lines"] for w in work if isinstance(w.get("lines"), int)),
        "corpora": len(pp["corpora"]),
        "live": sorted(s for s in pp["corpora"] if s in live_slugs),
        "repos": len({w["repo_url"].lower() for w in work}),
        "gh_stars": sum(stars_by_repo.values()),
        "since": min(pp["agreed"]) if pp["agreed"] else "",
        # every repo added by the owner on /add (GitHub sign-in), none by email reply
        "web_only": bool(work) and all("sign-in" in (w.get("channel") or "") for w in work),
    }


def pinned_repo_stars(people):
    """(total, repos): GitHub stars summed over DISTINCT pinned repositories that
    have a recorded star count. The only star figures the generator has are the
    outreach ledger's repo_stars (CONTRIBUTORS.jsonl, one per contributor repo,
    each pinned at a commit); registry manifests carry none, so those repos
    are left out rather than estimated. Same figures the treemap uses."""
    by_repo = {}
    for pp in people.values():
        for w in pp["work"].values():
            if isinstance(w.get("repo_stars"), int) and w.get("repo_url"):
                key = w["repo_url"].rstrip("/").lower()
                # Two consented commits can name one repository with different
                # snapshots. The wall shows the higher recorded count once.
                prev = by_repo.get(key)
                if prev is None or w["repo_stars"] > prev:
                    by_repo[key] = w["repo_stars"]
    return sum(by_repo.values()), len(by_repo)


def compact(n):
    """12,400,000 -> '12.4M', 5,501,659 -> '5.5M', 48,200 -> '48.2K', 950 -> '950'."""
    for div, suf in ((1_000_000_000, "B"), (1_000_000, "M"), (1_000, "K")):
        if n >= div:
            return f"{n / div:.1f}".rstrip("0").rstrip(".") + suf
    return f"{n:,}"


def badge_svg(label="XERJ Corpus Hub", value="contributor"):
    # shields-style flat badge; widths from a fixed 6.6 px/char estimate for 11px Verdana
    lw, vw = int(len(label) * 6.6) + 14, int(len(value) * 6.6) + 14
    w = lw + vw
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="20" role="img" aria-label="{label}: {value}">'
            f'<title>{label}: {value}</title><linearGradient id="s" x2="0" y2="100%"><stop offset="0" stop-color="#bbb" stop-opacity=".1"/>'
            f'<stop offset="1" stop-opacity=".1"/></linearGradient><clipPath id="r"><rect width="{w}" height="20" rx="3" fill="#fff"/></clipPath>'
            f'<g clip-path="url(#r)"><rect width="{lw}" height="20" fill="#11120f"/><rect x="{lw}" width="{vw}" height="20" fill="#7c3aed"/>'
            f'<rect width="{w}" height="20" fill="url(#s)"/></g><g fill="#fff" text-anchor="middle" font-family="Verdana,Geneva,DejaVu Sans,sans-serif" font-size="11">'
            f'<text x="{lw / 2:.0f}" y="14">{label}</text><text x="{lw + vw / 2:.0f}" y="14">{value}</text></g></svg>')


def _n(x):
    return f"{x:,}" if isinstance(x, int) else "—"


def xposts_section(posts, esc):
    """Contributors' own X posts (data/x_posts.json, see fetch_x_posts.py), as plain quotes."""
    if not posts:
        return ""
    items = []
    for x in posts:
        # Exact post text: X returns &amp; &lt; &gt; pre-escaped, so normalise and escape once.
        # html.escape, not esc(): esc() rewrites dashes and the quote must stay verbatim.
        body = html.escape(html.unescape(x["text"])).replace("\n", "<br>")
        d = datetime.datetime.fromisoformat(x["created_at"].replace("Z", "+00:00"))
        tag = "Featured by XERJ · " if x.get("match", "").startswith("featured") else ""
        items.append(f'<blockquote cite="{esc(x["url"])}"><p>{body}</p><p class="by">{tag}{html.escape(x["author_name"] or x["author_username"])} '
                     f'(@{html.escape(x["author_username"])}), <time datetime="{esc(x["created_at"])}">{d:%b} {d.day}, {d.year}</time> · '
                     f'<a href="{esc(x["url"])}" rel="nofollow noopener">View on X</a></p></blockquote>')
    return f'<section class="xposts"><h2>On X</h2>{"".join(items)}</section>\n'


def repo_facts_html(st, esc):
    """The repository as GitHub reports it (data/repo_stats.json): description, history, size and languages.
    Only fields GitHub returned are shown; nothing is filled in when a field is missing."""
    if not st:
        return ""
    desc = repo_pages.gh_text(st, "")
    tree = st.get("tree") if "files" in (st.get("tree") or {}) else {}
    bits = [f'{_n(st["commits"])} commits' if isinstance(st.get("commits"), int) else "",
            f'{_n(st["forks"])} forks' if isinstance(st.get("forks"), int) and st["forks"] else "",
            f'{_n(tree["files"])} files at the pinned commit' if tree.get("files") else "",
            f'created {st["created"][:7]}' if st.get("created") else "",
            f'last push {st["pushed"][:10]}' if st.get("pushed") else "",
            "archived" if st.get("archived") else ""]
    langs = ", ".join(f"{l} {round(p)}%" for l, p in (st.get("languages") or [])[:3] if p >= 1)
    line = " · ".join(b for b in bits if b)
    if not (desc or line or langs):
        return ""
    return (f'<div class="repofacts">' + (f'<p>{esc(desc)}</p>' if desc else "")
            + (f'<div class="commit">On GitHub: {esc(line)}</div>' if line else "")
            + (f'<div class="commit">Languages: {esc(langs)}</div>' if langs else "") + "</div>")


def person_body(pp, esc, live_slugs, pages=(), agent="", xposts="", gh=None):
    login = pp["login"]
    work = sorted(pp["work"].values(), key=lambda w: (-(w.get("repo_stars") or 0), w["repo"].lower()))
    name = pp["name"] or login
    v = volume(pp, live_slugs)
    url = f"{SITE}/contributors/{login}"
    repos = ", ".join(w["repo"] for w in work[:2])
    tweet = f"My work in {repos} was selected for the XERJ Corpus Hub: AI coding agents retrieve it as a worked reference instead of guessing. {url}"
    cards = []
    for w in work:
        live = [c for c in w["corpora"] if c in live_slugs]
        # licence cleared (ledger licence.reviewed: author consent + LICENSE at the pin) is not "in licence review":
        # the only step left is the registry entry + pack build (2026-10-27, a contributor read it as a licence problem)
        # a licence-cleared contributed corpus IS live: its page is up and its prompt lets any agent search it now. Only a
        # pre-built signed pack is extra (registry corpora). "awaiting pack build" read as "not live" (2026-10-29, on X)
        status = ('<span class="chip ok">live in the hub · signed pack</span>' if live
                  else '<span class="chip ok">live on the hub · licence cleared</span>' if w.get("reviewed")
                  else '<span class="chip warn">selected · in licence review</span>')
        # issue #1890: a corpus not in the registry has no manifest there, so
        # the documented install path (llms.txt step 3) 404s. Say so on the card.
        # no warning on licence-cleared work: "not installable" made contributors think something was wrong and
        # stopped them sharing the page (user, 2026-10-27)
        pending = ("" if live or w.get("reviewed") else
                   '<p class="note pending">Not installable yet: this corpus is not in the hub registry until its '
                   'licence review passes, so <code>xerj corpus add</code> and <code>xerj ask</code> cannot fetch it. '
                   'It becomes installable when this card says live in the hub.</p>')
        commit = (f'<div class="commit"><a href="{esc(w["commit_url"])}">{esc(w["commit"][:10])}</a> {esc(w["title"])}</div>'
                  if w["commit"] else "")
        size = " · ".join(x for x in [f'{w["files"]} files' if w.get("files") else "", f'{_n(w.get("lines"))} lines' if w.get("lines") else "",
                                         ", ".join(w["langs"])] if x)
        chips = "".join(f'<span class="chip">{esc(t)}</span>' for t in w["topics"][:6])
        corp = "".join(f'<a class="chip" href="../corpus/{esc(c)}">corpus {esc(c)}</a>' if c in pages
                       else f'<span class="chip">corpus {esc(c)}</span>' for c in w["corpora"])
        ghs = f'<span class="stars" title="GitHub stars on the repository">GitHub ★ {_n(w.get("repo_stars"))}</span>' if isinstance(w.get("repo_stars"), int) else ""
        facts = repo_facts_html((gh or {}).get(repo_pages.key(w["repo_url"])) or {}, esc)
        cards.append(f"""<div class="contrib">{ghs}
<h3><a href="{esc(w["repo_url"])}">{esc(w["repo"])}</a></h3>{facts}{commit}
<div class="commit">{"selected commit: " + esc(size) if size else ""}{" · " + esc(", ".join(w["areas"])[:120]) if w["areas"] else ""}</div>
<p class="note">The corpus is the whole repository at the pinned commit, not only the files of the selected commit.</p>
<div class="chips">{status}{corp}{f'<span class="chip">{esc(w["shelf"])}</span>' if w["shelf"] else ""}<span class="chip">{esc(w["spdx"])}</span>{chips}
<span class="chip">agreed {esc(w["agreed"][:10])}</span></div>{pending}</div>""")
    other = sorted(c for c in pp["corpora"] if not any(c in w["corpora"] for w in work))
    also = ""
    if other:
        links = ", ".join(f'<a href="../corpus/{esc(c)}">{esc(c)}</a>' if c in pages else esc(c) for c in other)
        also = f'<p class="note">Their repositories are also pinned in: {links}.</p>'
    md = f"[![XERJ Corpus Hub contributor]({SITE}/contributors/{login}/badge.svg)]({url})"
    kpi = lambda val, label: f'<div class="kpi"><b>{val}</b><span>{label}</span></div>'
    s_ = lambda n, w: w if n == 1 else w + "s"
    kpis = (kpi(v["corpora"], s_(v["corpora"], "corpus").replace("corpuss", "corpora"))
            + kpi(v["commits"], s_(v["commits"], "selected commit"))
            + (kpi(_n(v["files"]), s_(v["files"], "file")) if v["files"] else "")
            + (kpi(_n(v["lines"]), s_(v["lines"], "line")) if v["lines"] else "")
            + (kpi("★ " + _n(v["gh_stars"]), "GitHub stars on " + s_(v["repos"], "repo")) if v["gh_stars"] else "")
            + star_widget("contributor", login, esc))
    return f"""<main class="cwrap">
<section class="prof"><img class="av" src="https://github.com/{esc(login)}.png?size=480" alt="{esc(name)}">
<div><h1>{esc(name)}</h1><div class="login"><a href="https://github.com/{esc(login)}">@{esc(login)}</a></div>
<div class="ribbon">Corpus Hub contributor since {esc(v["since"][:10])}</div>
<p>{esc(name)}'s work was selected for the XERJ Corpus Hub, the open reference memory AI coding agents search before they write code.
When an agent meets the same problem, it gets this implementation back as a worked example, with the author and the repository attached.</p>
<div class="kpis">{kpis}</div>
<p class="note">Totals across every corpus @{esc(login)} appears in; a commit selected for two corpora counts once. Files and lines count the selected commits; each corpus indexes the whole repository at its pinned commit.</p>
<div class="share"><a href="https://twitter.com/intent/tweet?text={urllib.parse.quote(tweet)}" target="_blank" rel="noopener">Share on X</a>
<a href="https://www.linkedin.com/sharing/share-offsite/?url={urllib.parse.quote(url)}" target="_blank" rel="noopener">Share on LinkedIn</a>
<a href="https://news.ycombinator.com/submitlink?u={urllib.parse.quote(url)}&t={urllib.parse.quote(name + ' on the XERJ Corpus Hub')}" target="_blank" rel="noopener">Hacker News</a>
<button data-copy="badge-{esc(login)}">Copy README badge</button></div>
</div></section>
{agent}{xposts}<h2>Selected work</h2>{"".join(cards)}{also}
{more_block("contributor", login, esc)}
<div class="badgebox"><img src="{esc(login)}/badge.svg" alt="XERJ Corpus Hub contributor badge">
<pre id="badge-{esc(login)}">{esc(md)}</pre></div>
<p style="color:var(--z-mute);font-size:var(--fs-13)">{f'Added by @{esc(login)} on hub.xerj.org after signing in with GitHub ({esc(v["since"][:10])})' if v.get("web_only") else f'Included with the author&#39;s permission (email, {esc(v["since"][:10])})'}, under each repository's own licence, with attribution.
Own more repositories? <a href="/add">Sign in with GitHub and add them</a>.
Want something removed? {"Withdraw it on <a href=\"/add\">the same page</a>" if v.get("web_only") else "Reply to the email, withdraw it on <a href=\"/add\">/add</a>,"} or open an issue on <a href="https://github.com/xerj-org/xerj">xerj-org/xerj</a>.</p>
</main>{COPY_BADGE_JS}{STARS_JS}{MORE_JS}"""


# ---- SEO: per-page facts, read only from the ledger, the candidate manifests
# (their "domain" line carries the repository's GitHub description, written by
# the outreach pipeline) and the registry. A fact we do not have is left out.
DESC_RE = re.compile(r"^an agent working on (.*?) queries this for how (\S+) implements it"
                     r"(?:: (.*?))?(?: \(selected work: (.*)\))?$", re.S)
# xerj.org pages that match a domain; every page also links the reference-coding case study.
XERJ_LINKS = {
    "SEC": ("https://xerj.org/use-cases/code-security-audit", "Code security audit with XERJ"),
    "AI": ("https://xerj.org/use-cases/ai-search-retrieval", "AI search and retrieval with XERJ"),
    "OPS": ("https://xerj.org/use-cases/operational-intelligence", "Operational intelligence with XERJ"),
    "API": ("https://xerj.org/answers/search-openapi-spec-for-agent", "Search an OpenAPI spec from an agent"),
}
REFCODE = ("https://xerj.org/case-studies/reference-coding", "Case study: reference coding for agents")


def repo_descs(hub_dir):
    """corpus slug -> the repository's GitHub description (pipeline cuts it at 140 chars)."""
    out = {}
    for f in sorted((pathlib.Path(hub_dir) / "candidates" / "authors").glob("*.json")):
        try:
            d = json.loads(f.read_text())
        except ValueError:
            continue
        m = DESC_RE.match(d.get("domain") or "")
        if not (m and m.group(3)):
            continue
        t = " ".join(m.group(3).split())
        if len(t) >= 140:
            sp = t.rfind(" ")
            t = (t[:sp] if sp > 100 else t[:139]).rstrip(" ,;:-") + "…"
        out[d.get("corpus") or f.stem] = t
    return out


def clean_title(t):
    """Commit subject without the conventional-commit type and PR number."""
    t = re.sub(r"\s*\(#\d+\)\s*$", "", t or "").strip()
    t2 = re.sub(r"^[a-z]+(\([^)]*\))?!?:\s*", "", t)
    return (t2 or t).strip()


def _fit(s, n=160):
    s = " ".join(s.split())
    if len(s) <= n:
        return s
    cut = s[:n - 1]
    sp = cut.rfind(" ")
    return (cut[:sp] if sp > n * 0.6 else cut).rstrip(" ,;:.-") + "…"


def _h(s):
    return int.from_bytes(s.encode(), "little") % 1000003


def seo_facts(pp, live_slugs, reg, descs, cats, by_cat):
    """Everything the title, description, agent section and JSON-LD need, or None-ish fields."""
    login = pp["login"]
    name = pp["name"] or login
    work = sorted(pp["work"].values(), key=lambda w: (-(w.get("repo_stars") or 0), w["repo"].lower()))
    w = work[0]
    rank = lambda c: (0 if c in live_slugs else 1 if c in descs else 2, c)
    slug = sorted(w["corpora"], key=rank)[0] if w["corpora"] else ""
    short = w["repo"].split("/")[-1]
    shelf = w.get("shelf") or (reg.get(slug) or {}).get("cat") or ""
    domain = (cats or {}).get(shelf, ("", ""))[0]
    usecase = (reg.get(slug) or {}).get("usecase") or "" if slug in live_slugs else ""
    desc = next((descs[c] for c in sorted(w["corpora"], key=rank) if c in descs), "")
    topics = [t for t in w["topics"] if t and t.lower() not in (short.lower(), w["repo"].split("/")[0].lower())][:6]
    langs = w["langs"][:2]
    title = clean_title(w.get("title"))
    areas = [a for a in w["areas"] if a and a != "the project root"][:2]
    who = f"{name} (@{login})" if name != login else f"@{login}"
    # use cases: each one only when its data exists
    uses = []
    if usecase:
        uses.append(plain_cap(usecase) + ".")
    if topics:
        uses.append(f"Look up how {short} implements {', '.join(topics[:3])}, with answers cited to file and line.")
    if title:
        size = " · ".join(x for x in [f'{w["files"]} files' if w.get("files") else "", ", ".join(langs)] if x)
        uses.append(f"Reuse a worked example: the commit “{title}”" + (f" ({size})" if size else "") + ".")
    if domain and len(uses) < 3:
        uses.append(f"Answer from {short}'s real {langs[0] + ' ' if langs else ''}source instead of memory ({domain}).")
    uses = uses[:3]
    qs = []
    if title and len(title) <= 70 and not title.endswith(("…", "...")):
        qs.append(f"Where does {short} handle “{title}”?")
    if topics:
        qs.append(f"How does {short} implement {topics[0]}?")
    if areas:
        qs.append(f"What does {areas[0]} do in {short}?")
    qs = qs[:3]
    # related live corpora in the same domain: topic overlap first, then a stable spread
    rel = []
    if shelf in by_cat:
        tp = set(w["topics"])
        cand = [c for c in by_cat[shelf] if c["slug"] not in w["corpora"]]
        cand.sort(key=lambda c: (-len(tp & set((c.get("tags") or {}).get("topics") or [])), _h(login + c["slug"])))
        rel = cand[:4]
    thin = len(uses) < 2 or not (len(desc) >= 20 or topics or usecase)
    return dict(login=login, name=name, who=who, w=w, slug=slug, short=short, shelf=shelf, domain=domain,
                desc=desc, topics=topics, langs=langs, title=title, uses=uses, qs=qs, rel=rel, thin=thin,
                live=slug in live_slugs)


def plain_cap(s):
    s = " ".join(str(s).split()).rstrip(".")
    return s[:1].upper() + s[1:]


def seo_title(f):
    """Keeps the person's name: drops "corpus for AI agents" and the suffix first."""
    who, short, login, name = f["who"], f["short"], f["login"], f["name"]
    opts = [f"{who}: {short} corpus for AI agents | XERJ Hub",
            f"{who}: {short} corpus | XERJ Hub",
            f"{who}: {short} | XERJ Hub",
            f"{name}: {short} corpus | XERJ Hub",
            f"{name}: {short} | XERJ Hub",
            f"{who}: {short}",
            f"{name}: {short}",
            f"{name} (@{login}) | XERJ Hub",
            f"{name} | XERJ Hub",
            f"@{login}: {short} | XERJ Hub",
            f"@{login} | XERJ Hub"]
    for o in opts:
        if len(o) <= 62:
            return o
    return opts[-1][:62]


def seo_desc(f):
    lang = f["langs"][0] + " " if f["langs"] else ""
    head = f"{f['short']}: {f['desc'].rstrip('.…')}." if f["desc"] else f"{f['w']['repo']} on the XERJ Corpus Hub."
    tp = f" for {', '.join(f['topics'][:2])}" if f["topics"] else ""
    named = f" Work by {f['name']}."
    tails = [f" A {lang}corpus AI agents search{tp}, with work by {f['name']}.",
             f" A {lang}corpus AI agents search, with work by {f['name']}.",
             named, f" A {lang}corpus for AI agents.", " For AI agents."]
    for t in tails:
        if len(head + t) <= 160:
            d = head + t
            break
    else:
        # Keep the name when the head is too long. Two people on one repository
        # would otherwise share the generic tail and the same description.
        tail = named if len(named) <= 80 else tails[-1]
        d = _fit(head, 160 - len(tail)).rstrip(".") + tail
    if len(d) < 110:
        d += " Agents get it back as cited worked examples."
    return _fit(d, 160)


def agent_section(f, esc, pages):
    if f["thin"]:
        return ""
    login, slug, short = f["login"], f["slug"], f["short"]
    q = f["qs"][0] if f["qs"] else f"How does {short} work?"
    if f["live"]:
        cmd = f'xerj ask --corpus {slug} "{q}"'
        when = "Paste it into your AI agent:"
    else:
        # not live yet: clone at the pinned commit and index it locally
        w = f["w"]
        cmd = repo_pages.clone_prompt(w["repo"], w["repo_url"], w.get("pin") or w.get("commit") or "",
                                      f"{SITE}/corpus/{slug}" if slug in pages else f"{SITE}/contributors/{login}",
                                      w.get("spdx") or "", w.get("suggested_use") or "")
        when = "Paste it into your AI agent. It clones the repo at the pinned commit and indexes it on your machine."
    desc = (f'<p class="rdesc"><a href="{esc(f["w"]["repo_url"])}">{esc(f["w"]["repo"])}</a>: {esc(f["desc"])}</p>'
            if f["desc"] else "")
    uses = "".join(f"<li>{esc(u)}</li>" for u in f["uses"])
    qs = "".join(f"<li>{esc(x)}</li>" for x in f["qs"])
    links = []
    if slug:
        links.append(f'<a href="../corpus/{esc(slug)}">corpus {esc(slug)}</a>' if slug in pages else f"corpus {esc(slug)}")
    if f["shelf"] and f["domain"]:
        links.append(f'<a href="../category/{esc(f["shelf"])}">{esc(f["domain"])}</a>')
    rel = ", ".join(f'<a href="../corpus/{esc(c["slug"])}">{esc(c.get("label") or c["slug"])}</a>' for c in f["rel"])
    x = XERJ_LINKS.get(f["shelf"])
    more = f'<a href="{x[0]}">{esc(x[1])}</a> · ' if x else ""
    return f"""<section class="agentuse"><h2>What an agent can do with the {esc(short)} corpus</h2>{desc}
<ul>{uses}</ul>
{f'<b>Example questions</b><ul>{qs}</ul>' if qs else ''}
<div class="askhead"><span class="note">{esc(when)}</span><button data-copy="ask-{esc(login)}">{repo_pages.CTA}</button></div>
<pre id="ask-{esc(login)}">{esc(cmd)}</pre>
<p class="note">{" · ".join(links)}{f'<br>Related corpora: {rel}' if rel else ''}<br>{more}<a href="{REFCODE[0]}">{esc(REFCODE[1])}</a></p>
</section>"""


def seo_ld(f, posts=()):
    url = f"{SITE}/contributors/{f['login']}"
    pid = url + "#person"
    w = f["w"]
    person = {"@type": "Person", "@id": pid, "name": f["name"], "alternateName": "@" + f["login"],
              "identifier": f["login"], "url": url, "image": f"https://github.com/{f['login']}.png?size=400",
              "sameAs": [f"https://github.com/{f['login']}"] + sorted(
                  {f"https://x.com/{x['author_username']}" for x in posts if not x.get("match", "").startswith("featured")})}
    code = {"@type": "SoftwareSourceCode", "name": w["repo"], "codeRepository": w["repo_url"],
            **({"description": f["desc"]} if f["desc"] else {}),
            **({"programmingLanguage": w["langs"]} if w["langs"] else {}),
            **({"license": f"https://spdx.org/licenses/{w['spdx']}"} if w.get("spdx") and re.match(r"^[A-Za-z0-9.+-]+$", w["spdx"]) else {}),
            **({"keywords": w["topics"]} if w["topics"] else {}), "contributor": {"@id": pid}}
    about = [code]
    if f["slug"]:
        ds = {"@type": "Dataset", "name": f"{f['slug']} corpus", "url": f"{SITE}/corpus/{f['slug']}",
              "description": _fit(f"Reference corpus of {w['repo']} at a pinned commit for AI agents"
                                  + (f": {f['desc']}" if f["desc"] else "."), 300),
              "contributor": {"@id": pid}, "isBasedOn": w["repo_url"],
              "includedInDataCatalog": {"@type": "DataCatalog", "name": "XERJ Corpus Hub", "url": SITE + "/"}}
        if "license" in code:
            ds["license"] = code["license"]
        kw = [k for k in [f["domain"]] + w["topics"] if k]
        if kw:
            ds["keywords"] = kw
        about.append(ds)
    return {"mainEntity": person, "about": about}


# Hub stars: counts and the Star button come from /api/hub-stars
# (functions/api/hub-stars.js). The control ships hidden and is shown only when
# the endpoint answers enabled:true, so a site without the R2 binding shows
# no star figure at all rather than a fake zero.
STARS_JS = """<script>
(function(){
  var API='/api/hub-stars';
  function fmt(n){return Number(n).toLocaleString('en-US')}
  document.querySelectorAll('.hubstar[data-kind]').forEach(function(el){
    var kind=el.dataset.kind,id=el.dataset.id,num=el.querySelector('[data-n]'),btn=el.querySelector('button');
    function show(r){num.textContent=fmt(r.count||0);if(r.starred){btn.disabled=true;btn.textContent='Starred'}el.hidden=false}
    fetch(API+'?kind='+kind+'&id='+encodeURIComponent(id)).then(function(r){return r.json()}).then(function(r){
      if(!r||!r.ok||!r.enabled)return; show(r);
      btn.addEventListener('click',function(){btn.disabled=true;
        fetch(API,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({kind:kind,id:id})})
          .then(function(r){return r.json()}).then(function(r){if(r&&r.ok)show(r);else{btn.disabled=false;btn.title=(r&&r.error)||''}})
          .catch(function(){btn.disabled=false})});
    }).catch(function(){});
  });
  // hub-star counts in the people table's Hub ★ column (shown only when the
  // hub-stars API is on); hubStarMark() is called again by TABLE_JS for every
  // batch of rows it renders, and skips rows it has already marked
  var counts=null,wall=document.querySelector('[data-stars]');
  window.hubStarMark=function(root){if(!counts||!root)return;
    root.querySelectorAll('[data-id]').forEach(function(a){var s=a.querySelector('.hs');if(!s||s.dataset.done)return;
      s.dataset.done='1';s.textContent=fmt(counts[a.dataset.id]||0)})};
  if(wall)fetch(API+'?kind='+wall.dataset.stars).then(function(r){return r.json()}).then(function(r){
    if(!r||!r.ok||!r.enabled)return;
    counts=r.counts||{};wall.classList.add('hubon');window.hubStarMark(wall);
  }).catch(function(){});
})();
</script>"""


# Signed-in extras (functions/api/v1/contributors/[login].js, functions/api/v1/corpus/[slug].js).
# The static page carries only this empty block. Anonymous visitors on a hub with
# sign-in enabled see one "Sign in to see more" line; with sign-in disabled the
# block stays hidden and the page is exactly what it was. The details are fetched
# from the authenticated API after sign-in and never baked into the HTML.
MORE_HINT = {
    "contributor": "Sign in with GitHub or Hugging Face to see more: every selected commit in full "
                   "(commit date, every folder, licence review state, pin) and the pack builds of each corpus.",
    "corpus": "Sign in with GitHub or Hugging Face to see more: this corpus's pack build and pin history, "
              "full source pins and review records, and how its packs are signed.",
}


def more_block(kind, sid, esc):
    return (f'<section class="more" data-more="{kind}" data-id="{esc(sid)}" hidden>'
            f'<p class="signin-hint">{esc(MORE_HINT[kind])}</p><div class="more-body"></div></section>')


MORE_JS = """<script>
(function(){
  var sec=document.querySelector('section.more[data-more]'); if(!sec) return;
  var kind=sec.dataset.more,id=sec.dataset.id,hint=sec.querySelector('.signin-hint'),body=sec.querySelector('.more-body');
  function el(t,a,txt){var e=document.createElement(t);for(var k in a)if(a[k]!=null)e.setAttribute(k,a[k]);if(txt!=null)e.textContent=txt;return e}
  function add(p,c){p.appendChild(c);return c}
  function fmt(n){return typeof n==='number'?n.toLocaleString('en-US'):''}
  function mb(n){return typeof n==='number'?(n/1048576).toFixed(1)+' MB':''}
  function link(href,txt){return href?el('a',{href:href,rel:'noopener'},txt):el('span',{},txt)}
  function table(head,rows){var t=el('table',{'class':'moretable'}),tr=add(t,el('tr'));
    head.forEach(function(h){add(tr,el('th',{},h))});
    rows.forEach(function(r){var tr=add(t,el('tr'));r.forEach(function(c){var td=add(tr,el('td'));if(c&&c.nodeType)td.appendChild(c);else td.textContent=c==null?'':String(c)})});
    return t}
  function rel(tag){return tag?link('https://github.com/xerj-org/xerj/releases/tag/'+encodeURIComponent(tag),tag):''}
  function build(b){if(!b)return '';var s=el('span');add(s,link(b.url,(b.date||'')+' '+(b.result||'')));if(b.tag){s.appendChild(document.createTextNode(' · '));add(s,rel(b.tag))}return s}
  function contributor(r){
    var c=r.contributor;
    if(r.is_me){
      var me=add(body,el('div',{'class':'thisisme'}));
      add(me,el('h2',{},'This is you'));
      // live = licence cleared (its page and prompt are up); a signed pack is an extra, not a condition (2026-10-29)
      var live=0,packs=0,review=0;c.work.forEach(function(w){(w.corpora||[]).forEach(function(x){if(x.status==='live'||w.reviewed)live++;else review++;if(x.status==='live')packs++})});
      add(me,el('p',{},'You are signed in as @'+c.login+'. '+c.work.length+' of your commits are selected; '+live+' corpus entr'+(live===1?'y is':'ies are')+' live on the hub: any agent can search '+(live===1?'it':'them')+' now with the prompt on '+(live===1?'its':'each')+' page.'+(packs?' '+packs+' also '+(packs===1?'has':'have')+' a pre-built signed pack.':'')+(review?' '+review+' '+(review===1?'is':'are')+' still in licence review.':'')+' Each card below shows where your work is.'));
      var a=add(me,el('p',{}));
      add(a,link('https://github.com/xerj-org/xerj/issues/new?title='+encodeURIComponent('Corpus Hub: change or remove my work (@'+c.login+')'),'Ask for a change or removal'));
      a.appendChild(document.createTextNode(' · '));add(a,link('/me','Your hub stars'));
    }
    add(body,el('h2',{},'Every selected commit'));
    c.work.forEach(function(w){
      var d=add(body,el('div',{'class':'contrib'}));
      var h=add(d,el('h3'));add(h,link(w.repo_url,w.repo));
      var cm=add(d,el('div',{'class':'commit'}));add(cm,link(w.commit_url,w.commit||'no commit'));
      cm.appendChild(document.createTextNode(' '+(w.title||'')));
      var rows=[['committed',(w.committed_at||'').replace('T',' ').replace('Z',' UTC')],['pinned at',w.pin],
        ['size',[w.files?fmt(w.files)+' files':'',w.lines?fmt(w.lines)+' lines':''].filter(Boolean).join(' · ')],
        ['languages',(w.langs||[]).join(', ')],['folders',(w.areas||[]).join(', ')],['topics',(w.topics||[]).join(', ')],
        ['licence',[w.spdx,w.suggested_use,w.reviewed?'licence reviewed':'licence review pending'].filter(Boolean).join(' · ')],
        ['agreed',w.agreed]].filter(function(x){return x[1]});
      add(d,table(['',''],rows));
      if(w.corpora&&w.corpora.length)add(d,table(['corpus','status','manifest','last pack build'],w.corpora.map(function(x){
        return [link('/corpus/'+x.slug,x.slug),x.status==='live'?'live · signed pack':(w.reviewed?'live':'in licence review'),x.manifest||'',build(x.last_build)||(x.status==='live'?'':'not needed: the prompt indexes it on the agent\u2019s machine')]})));
    });
    var also=Object.keys(c.also_pinned_in||{});
    if(also.length)add(body,el('p',{'class':'note'},'Repositories of yours are also pinned in: '+also.join(', ')+'.'));
  }
  function corpus(r){
    var c=r.corpus;
    if(c.builds&&c.builds.length){
      add(body,el('h2',{},'Pack builds'));
      add(body,table(['date','run','pin','result','pack size','definitions','release / error'],c.builds.map(function(b){
        return [b.date,link(b.url,b.run),b.pin8,b.result,mb(b.bytes),fmt(b.defs),b.tag?rel(b.tag):[b.stage,b.error].filter(Boolean).join(': ')]})));
    }
    if(c.pins&&c.pins.length){
      add(body,el('h2',{},'Pin history'));
      add(body,table(['pin','first build','last build','builds','published'],c.pins.map(function(p){return [p.pin8,p.first,p.last,p.builds,p.published?rel(p.published):'not yet']})));
    }
    if(r.catalogue){var e=r.catalogue;add(body,el('h2',{},'Signed pack in the catalogue'));
      add(body,table(['',''],[['pack',link(e.pack_url,e.pack_url)],['size',mb(e.pack_bytes||e.bytes)],['sha256',e.pack_sha256],['signature',link(e.sig_url,e.sig_url)]].filter(function(x){return x[1]})))}
    add(body,el('h2',{},'Signing'));
    var sg=[['scheme',r.signing.scheme],['hub key (ed25519)',r.signing.hub_pubkey]];if(c.pack_pubkey)sg.push(['pack key',c.pack_pubkey]);
    add(body,table(['',''],sg.filter(function(x){return x[1]})));
    if(c.repos&&c.repos.length){add(body,el('h2',{},'Sources, full pins and review records'));
      add(body,table(['source','pin','files','size','licence','review'],c.repos.map(function(x){var v=x.review||{};
        return [link((x.url||'').replace(/\\.git$/,''),x.repo),x.sha,fmt(x.files),mb(x.bytes),v.spdx||x.licence||'',[v.use,v.by,v.at].filter(Boolean).join(' · ')]})))}
    if(c.repos_summary){var s=c.repos_summary;add(body,el('p',{'class':'note'},s.count+' sources, '+fmt(s.files)+' files, '+mb(s.bytes)+'; '+s.reviewed+' with a recorded licence review ('+s.licences.join(', ')+').'))}
    if(c.pack_stats){add(body,el('h2',{},'Pack measurements'));
      add(body,table(['',''],Object.keys(c.pack_stats).filter(function(k){return typeof c.pack_stats[k]!=='object'}).map(function(k){return [k.replace(/_/g,' '),String(c.pack_stats[k])]})))}
    if(!body.childNodes.length)add(body,el('p',{'class':'note'},'No pack has been built for this corpus yet.'));
  }
  (window.hubMe=window.hubMe||fetch('/api/me',{credentials:'same-origin'}).then(function(r){return r.ok?r.json():null}).catch(function(){return null})).then(function(me){
    if(!me||!me.enabled)return;
    sec.hidden=false;
    if(!me.user)return;
    hint.hidden=true;
    fetch('/api/v1/'+(kind==='corpus'?'corpus':'contributors')+'/'+encodeURIComponent(id),{credentials:'same-origin'})
      .then(function(r){return r.json()}).then(function(r){
        if(!r||!r.ok){add(body,el('p',{'class':'note'},(r&&r.error)||'Details are not available right now.'));return}
        add(body,el('p',{'class':'note'},'Shown because you are signed in. Registry state as of '+(r.generated||'the last deploy')+'.'));
        if(kind==='corpus')corpus(r);else contributor(r);
      }).catch(function(){add(body,el('p',{'class':'note'},'Details are not available right now.'))});
  });
})();
</script>"""


def star_widget(kind, sid, esc, inline=False):
    """Hidden until /api/hub-stars says it is enabled; see STARS_JS."""
    sid = esc(sid.lower())
    if inline:
        return (f'<div class="hubstar inline" data-kind="{kind}" data-id="{sid}" hidden>'
                f'<b>hub ★ <span data-n>0</span></b><button type="button">Star this {kind}</button></div>')
    return (f'<div class="kpi hubstar" data-kind="{kind}" data-id="{sid}" hidden><b>★ <span data-n>0</span></b>'
            f'<span>hub stars</span><br><button type="button">Star</button></div>')


def stars_targets(people, registry):
    """The ids /api/hub-stars accepts: only pages this deploy actually has."""
    return {"contributor": sorted(l.lower() for l in people),
            "corpus": sorted(c["slug"].lower() for c in registry or []),
            # page file names keep the login's case; ids are lower-case
            "pages": {l.lower(): l for l in sorted(people) if l != l.lower()}}


CHUNK = 500            # table rows per JSON chunk / plain-HTML page


def wall_order(people, vols):
    """The wall's order: GitHub stars on the contributor's repos, then login."""
    return sorted(people, key=lambda l: (-vols[l]["gh_stars"], l.lower()))


def wall_row(l, pp, v):
    """One people row in list.<n>.json:
    [login, name ("" if same as login), corpora, lines, GitHub stars, repos, signed up (YYYY-MM-DD or "")].
    TABLE_JS renders it exactly like people_tr()."""
    return [l, pp["name"] if pp["name"] and pp["name"] != l else "", v["corpora"], v["lines"] or 0,
            v["gh_stars"], v["repos"], (v["since"] or "")[:10]]


def repo_index(people, pages=()):
    """Distinct pinned repositories that have a recorded star count, most
    stars first: [{"repo", "url", "stars", "owner", "owner_page", "corpus",
    "by"}]. owner_page is the owner's contributor-page login or "" (then
    the owner links to GitHub); corpus is a slug with a hub page or "";
    by is the contributor whose work brought the repo in."""
    seen = {}
    for login, pp in people.items():
        for w in pp["work"].values():
            if not (isinstance(w.get("repo_stars"), int) and w.get("repo_url")):
                continue
            key = w["repo_url"].rstrip("/").lower()
            e = seen.get(key)
            if e is None:
                repo = w["repo"] or key.split("github.com/")[-1]
                e = seen[key] = {"repo": repo, "url": w["repo_url"], "stars": w["repo_stars"],
                                 "owner": repo.split("/")[0], "corpus": "", "by": login}
            elif w["repo_stars"] > e["stars"]:
                e["stars"] = w["repo_stars"]
            if not e["corpus"]:
                e["corpus"] = next((c for c in w["corpora"] if c in pages), "")
    by_low = {l.lower(): l for l in people}
    out = sorted(seen.values(), key=lambda e: (-e["stars"], e["repo"].lower()))
    for e in out:
        e["owner_page"] = by_low.get(e["owner"].lower(), "")
    return out


def repo_row(e):
    """One repos row in repos.<n>.json: [repo, url, stars, owner, owner page login or "", corpus slug or ""]."""
    return [e["repo"], e["url"], e["stars"], e["owner"], e["owner_page"], e["corpus"]]


def _av(login, esc, px=28):
    return (f'<img src="https://github.com/{esc(login)}.png?size={px * 2}" alt="" loading="lazy" decoding="async" '
            f'width="{px}" height="{px}">')


def people_tr(rank, l, pp, v, esc):
    nm = pp["name"] if pp["name"] and pp["name"] != l else ""
    return (f'<tr data-id="{esc(l.lower())}"><td class="n">{rank:,}</td><td class="who"><a href="{esc(l)}">{_av(l, esc)}'
            f'<span><b>{esc(nm or l)}</b>{"<small>@" + esc(l) + "</small>" if nm else ""}</span></a></td>'
            f'<td class="num">{_n(v["gh_stars"])}</td><td class="num o1 hub hs"></td><td class="num o1">{v["repos"]:,}</td><td class="num o1">{v["corpora"]:,}</td>'
            f'<td class="num o2">{_n(v["lines"]) if v["lines"] else "—"}</td><td class="o2 d">{esc((v["since"] or "")[:10]) or "—"}</td></tr>')


def repo_tr(rank, e, esc, rel=""):
    own = (f'<a href="{rel}{esc(e["owner_page"])}">' if e["owner_page"] else f'<a href="https://github.com/{esc(e["owner"])}">')
    corp = (f'<a href="{rel}../corpus/{esc(e["corpus"])}">{esc(e["corpus"])}</a>' if e["corpus"] else "—")
    return (f'<tr><td class="n">{rank:,}</td><td class="repo"><a href="{esc(e["url"])}">{esc(e["repo"])}</a></td>'
            f'<td class="who">{own}{_av(e["owner"], esc)}<span><b>{esc(e["owner"])}</b></span></a></td>'
            f'<td class="num">{_n(e["stars"])}</td><td class="o1 corp">{corp}</td></tr>')


PEOPLE_COLS = (("#", "rank", ""), ("Contributor", "who", ""), ("GitHub ★", "stars", "num"), ("Hub ★", "hub", "num o1 hub"), ("Repos", "repos", "num o1"),
               ("Corpora", "corpora", "num o1"), ("Lines", "lines", "num o2"), ("Signed up", "since", "o2"))
REPO_COLS = (("#", "rank", ""), ("Repository", "repo", ""), ("Owner", "owner", ""), ("GitHub ★", "stars", "num"),
             ("Corpus", "corpus", "o1"))


def thead(cols, sortable=True):
    ths = []
    for label, key, cls in cols:
        aria = ' aria-sort="ascending"' if key == "rank" else ""
        inner = f'<button type="button" data-k="{key}">{label}</button>' if sortable and key != "hub" else label
        ths.append(f'<th scope="col" class="{cls}"{aria}>{inner}</th>')
    return f'<thead><tr>{"".join(ths)}</tr></thead>'


def pager(n_pages, current, kind="people"):
    stem, first, label = (("list", "./", "All contributors, page by page") if kind == "people"
                          else ("repos", "repos.1", "All repositories, page by page"))
    links = " ".join(f'<a href="{first if i == 1 else f"{stem}.{i}"}"{" aria-current=page" if i == current else ""}>{i}</a>'
                     for i in range(1, n_pages + 1))
    return f'<nav class="pager" data-kind="{kind}" aria-label="{label}"><b>{"Pages" if kind == "people" else "Repository pages"}</b> {links}</nav>'


TABLE_STYLE = """
/* contributors: view switches + sortable table (borderless, light tints) */
.vbar{display:flex;flex-wrap:wrap;gap:var(--sp-3);align-items:center;margin:var(--sp-8) 0 0}
.vbar[hidden]{display:none}
.seg{display:inline-flex;gap:3px;padding:3px;background:#eeebe3;border-radius:999px}
.seg button{font-family:var(--font-data);font-size:var(--fs-13);font-weight:600;color:var(--z-ink);background:transparent;border:0;
border-radius:999px;padding:0 18px;min-height:40px;cursor:pointer}
.seg button:hover{background:#fff}
.seg button[aria-pressed=true]{background:var(--z-ink);color:#fff}
.seg button:focus-visible,.ctab button:focus-visible,.tfilter button:focus-visible{outline:3px solid var(--z-focus);outline-offset:2px}
.cview[hidden],.ctab[hidden],.tfilter[hidden],.tsort[hidden]{display:none}
.tablewrap{container-type:inline-size}
.ctab{width:100%;border-collapse:separate;border-spacing:0 2px;font-family:var(--font-data);font-size:var(--fs-16);margin-top:var(--sp-4)}
.ctab th{text-align:left;font-size:var(--fs-13);font-weight:600;color:var(--z-mute);padding:0 var(--sp-3) var(--sp-2);white-space:nowrap}
.ctab th button{font:inherit;color:inherit;background:none;border:0;padding:6px 0;cursor:pointer;text-decoration:underline;text-decoration-color:var(--z-line);text-underline-offset:4px}
.ctab th[aria-sort=ascending] button::after{content:" ↑"}.ctab th[aria-sort=descending] button::after{content:" ↓"}
.ctab th[aria-sort] button{color:var(--z-ink);text-decoration-color:var(--z-ink)}
.ctab td{padding:8px var(--sp-3);background:#f3f1ea;vertical-align:middle}
.ctab tr:nth-child(6n+1) td{background:#fff1c7}.ctab tr:nth-child(6n+2) td{background:#ebe4f7}.ctab tr:nth-child(6n+3) td{background:#dfebf5}
.ctab tr:nth-child(6n+4) td{background:#e2f0e4}.ctab tr:nth-child(6n+5) td{background:#f7e6de}.ctab tr:nth-child(6n) td{background:#eeebe3}
.ctab td.n{color:var(--z-mute);font-size:var(--fs-13);width:3.5em}
.ctab .num{text-align:right;white-space:nowrap;font-variant-numeric:tabular-nums}
.ctab td.d{white-space:nowrap;color:var(--z-mute);font-size:var(--fs-13)}
.ctab .who a{display:flex;gap:var(--sp-3);align-items:center;text-decoration:none;color:var(--z-ink);min-width:0}
.ctab .who img{width:28px;height:28px;border-radius:50%;flex:none;background:#fff}
.ctab .who span{min-width:0}.ctab .who b{display:block;font-weight:600;overflow-wrap:anywhere}
.ctab .who small{display:block;font-size:var(--fs-13);color:var(--z-mute);overflow-wrap:anywhere}
.ctab .who a:hover b{text-decoration:underline}
.ctab td.repo a,.ctab td.corp a{color:var(--z-ink);font-family:var(--font-mono);font-size:var(--fs-13);overflow-wrap:anywhere}
.tfilter{display:flex;flex-wrap:wrap;gap:var(--sp-3);align-items:center;background:#e7e5de;padding:var(--sp-3) var(--sp-4);margin:var(--sp-4) 0 0;font-family:var(--font-data)}
.tfilter button,.tsort select{font-family:var(--font-data);font-size:var(--fs-13);font-weight:600;color:var(--z-ink);background:#fff;border:0;padding:.5em 1em;min-height:40px;cursor:pointer}
.tsort{display:none;gap:var(--sp-2);align-items:center;font-family:var(--font-data);font-size:var(--fs-13);margin-top:var(--sp-4);color:var(--z-mute)}
.ctab .hub{display:none}.ctab.hubon .hub{display:table-cell}
@container (max-width:760px){.ctab .o2{display:none}}
@container (max-width:520px){.ctab .o1,.ctab.hubon .o1{display:none}.ctab{font-size:var(--fs-13)}.ctab td{padding:8px}.ctab th{padding:0 8px 8px}
.tsort:not([hidden]){display:flex}.ctab td.n{width:2.5em}}
"""

STYLE += TABLE_STYLE

# One script for the switches, the "N others" tile and the sortable table.
# Without JS the page is the People map, the first CHUNK table rows and the
# plain pagers (list.<n>.html, repos.<n>.html). With JS: [People | Repos]
# [Treemap | Table] switches (state in the URL hash), header-click sorting,
# rank filters from the "N others" tile, and rows appended CHUNK at a time
# as the "Showing" line nears the viewport. Sorting and filtering need every
# row, so the first of those fetches all chunks (list.<n>.json / repos.<n>.json).
TABLE_JS = """<script>
(function(){
  var bar=document.getElementById('vbar');if(!bar)return;bar.hidden=false;
  function $(s,r){return (r||document).querySelector(s)}
  function fmt(n){return Number(n).toLocaleString('en-US')}
  function el(t,a,txt){var e=document.createElement(t);for(var k in a)e.setAttribute(k,a[k]);if(txt!=null)e.textContent=txt;return e}
  var tw=$('#table'),more=$('#wallmore'),btn=more.querySelector('button'),st=more.querySelector('[data-st]'),
      flt=$('#tfilter'),sel=$('#tsort select'),io=null;
  document.querySelectorAll('nav.pager').forEach(function(p){p.hidden=true});
  var M={people:{t:$('table.ctab[data-mode=people]'),chunks:+tw.dataset.pchunks,total:+tw.dataset.ptotal,url:'/contributors/list.',
          keys:['rank','who','stars','repos','corpora','lines','since'],idx:{rank:7,who:0,stars:4,repos:5,corpora:2,lines:3,since:6},
          head:'All '+fmt(+tw.dataset.ptotal)+' contributors',rows:null},
         repos:{t:$('table.ctab[data-mode=repos]'),chunks:+tw.dataset.rchunks,total:+tw.dataset.rtotal,url:'/contributors/repos.',
          keys:['rank','repo','owner','stars','corpus'],idx:{rank:6,repo:0,owner:3,stars:2,corpus:5},
          head:'All '+fmt(+tw.dataset.rtotal)+' repositories',rows:null}};
  var S={mode:'people',view:'map',key:'rank',dir:1,from:0,list:null,shown:0,busy:false};
  function av(l){return el('img',{src:'https://github.com/'+encodeURIComponent(l)+'.png?size=56',alt:'',loading:'lazy',decoding:'async',width:'28',height:'28'})}
  function td(c,txt){return el('td',c?{'class':c}:{},txt)}
  function ptr(r){var tr=el('tr',{'data-id':r[0].toLowerCase()});tr.appendChild(td('n',fmt(r[7])));
    var c=td('who'),a=el('a',{href:r[0]}),s=el('span');a.appendChild(av(r[0]));s.appendChild(el('b',{},r[1]||r[0]));
    if(r[1])s.appendChild(el('small',{},'@'+r[0]));
    a.appendChild(s);c.appendChild(a);tr.appendChild(c);
    tr.appendChild(td('num',fmt(r[4])));tr.appendChild(td('num o1 hub hs',''));tr.appendChild(td('num o1',fmt(r[5])));tr.appendChild(td('num o1',fmt(r[2])));
    tr.appendChild(td('num o2',r[3]?fmt(r[3]):'—'));tr.appendChild(td('o2 d',r[6]||'—'));return tr}
  function rtr(r){var tr=el('tr');tr.appendChild(td('n',fmt(r[6])));
    var c=td('repo');c.appendChild(el('a',{href:r[1]},r[0]));tr.appendChild(c);
    c=td('who');var a=el('a',{href:r[4]?r[4]:'https://github.com/'+r[3]}),s=el('span');a.appendChild(av(r[3]));
    s.appendChild(el('b',{},r[3]));a.appendChild(s);c.appendChild(a);tr.appendChild(c);
    tr.appendChild(td('num',fmt(r[2])));c=td('o1 corp');
    if(r[5])c.appendChild(el('a',{href:'../corpus/'+r[5]},r[5]));else c.textContent='—';tr.appendChild(c);return tr}
  function load(m){var d=M[m];if(d.rows)return Promise.resolve(d.rows);if(d.p)return d.p;
    var ps=[];for(var i=1;i<=d.chunks;i++)ps.push(fetch(d.url+i+'.json').then(function(r){if(!r.ok)throw r.status;return r.json()}));
    return d.p=Promise.all(ps).then(function(parts){var rows=[].concat.apply([],parts);
      rows.forEach(function(r,i){r.push(i+1)});return d.rows=rows}).catch(function(e){d.p=null;throw e})}
  function cmp(i,dir){return function(a,b){var x=a[i],y=b[i];
    if(typeof x==='string'){x=x.toLowerCase();y=y.toLowerCase();if(x===y)return a[a.length-1]-b[b.length-1];
      if(x==='')return 1;if(y==='')return -1;return x<y?-dir:dir}
    return x===y?a[a.length-1]-b[b.length-1]:(x-y)*dir}}
  function build(){var d=M[S.mode],rows=d.rows,rk=rows.length?rows[0].length-1:0;
    var l=S.from?rows.filter(function(r){return r[rk]>=S.from}):rows.slice();
    var i=d.idx[S.key];if(S.key==='who')i=0;
    l.sort(cmp(S.key==='who'?(S.mode==='people'?0:0):i,S.dir));
    if(S.mode==='people'&&S.key==='who')l.sort(function(a,b){var x=(a[1]||a[0]).toLowerCase(),y=(b[1]||b[0]).toLowerCase();return x===y?0:(x<y?-S.dir:S.dir)});
    S.list=l;S.shown=0;var tb=d.t.tBodies[0];tb.textContent='';append()}
  function append(){var d=M[S.mode],tb=d.t.tBodies[0],f=document.createDocumentFragment(),end=Math.min(S.list.length,S.shown+%(chunk)d);
    for(var i=S.shown;i<end;i++)f.appendChild((S.mode==='people'?ptr:rtr)(S.list[i]));tb.appendChild(f);S.shown=end;
    if(S.mode==='people'&&window.hubStarMark)window.hubStarMark(tb);status()}
  function status(){var d=M[S.mode],n=S.list?S.list.length:d.total,sh=S.list?S.shown:d.t.tBodies[0].rows.length;
    st.textContent='Showing '+fmt(sh)+' of '+fmt(n)+(S.from?' (ranks '+fmt(S.from)+'+)':'')+'.';btn.hidden=sh>=n;
    if(!btn.hidden&&S.view==='table'&&more.getBoundingClientRect().top<innerHeight+800)setTimeout(next,50)}
  function next(){if(S.busy)return;var d=M[S.mode];
    if(S.list){if(S.shown<S.list.length)append();return}
    S.busy=true;btn.disabled=true;load(S.mode).then(function(){S.busy=false;btn.disabled=false;
      var keep=d.t.tBodies[0].rows.length;build();while(S.shown<=keep&&S.shown<S.list.length)append()}).catch(fail)}
  function fail(){S.busy=false;btn.disabled=false;document.querySelectorAll('nav.pager[data-kind='+S.mode+']').forEach(function(p){p.hidden=false});
    st.textContent='Could not load the rest here; use the pages below.'}
  function marks(){M[S.mode].t.querySelectorAll('th').forEach(function(th){var b=th.querySelector('button');if(!b)return;
      if(b.dataset.k===S.key)th.setAttribute('aria-sort',S.dir>0?'ascending':'descending');else th.removeAttribute('aria-sort')});
    if(sel)sel.value=S.key+':'+S.dir}
  function refresh(){marks();var d=M[S.mode];flt.hidden=!S.from;
    if(S.from)flt.querySelector('[data-t]').textContent='Ranks '+fmt(S.from)+'–'+fmt(d.total)+': the “others” from the map.';
    if(d.rows||S.from||S.key!=='rank'||S.dir!==1||S.mode==='repos'){st.textContent='Loading…';
      S.busy=true;load(S.mode).then(function(){S.busy=false;build()}).catch(fail)}else{S.list=null;status()}}
  function opts(){if(!sel)return;sel.textContent='';var d=M[S.mode];
    d.t.querySelectorAll('th button').forEach(function(b){[1,-1].forEach(function(dir){
      sel.appendChild(el('option',{value:b.dataset.k+':'+dir},b.textContent+(dir>0?' ↑':' ↓')))})})}
  function show(){var mp=S.view==='map';
    document.querySelectorAll('.tmwrap[data-mode]').forEach(function(w){w.hidden=!(mp&&w.dataset.mode===S.mode)});
    tw.hidden=mp;M.people.t.hidden=S.mode!=='people';M.repos.t.hidden=S.mode!=='repos';$('#tableh').textContent=M[S.mode].head;
    bar.querySelectorAll('button[data-mode]').forEach(function(b){b.setAttribute('aria-pressed',b.dataset.mode===S.mode)});
    bar.querySelectorAll('button[data-view]').forEach(function(b){b.setAttribute('aria-pressed',b.dataset.view===S.view)});
    var h=(S.mode==='repos'?'repos':'')+(S.view==='table'?(S.mode==='repos'?'-table':'table'):'');
    try{history.replaceState(null,'',h?'#'+h:location.pathname+location.search)}catch(e){}
    if(!mp){opts();refresh()}}
  bar.addEventListener('click',function(e){var b=e.target.closest('button');if(!b)return;
    if(b.dataset.mode&&b.dataset.mode!==S.mode){S.mode=b.dataset.mode;S.from=0;S.key='rank';S.dir=1;S.list=null}
    if(b.dataset.view)S.view=b.dataset.view;show()});
  tw.addEventListener('click',function(e){var b=e.target.closest('th button');if(!b)return;var k=b.dataset.k;
    if(S.key===k)S.dir=-S.dir;else{S.key=k;S.dir=(k==='stars'||k==='repos'||k==='corpora'||k==='lines')?-1:1}refresh()});
  if(sel)sel.addEventListener('change',function(){var v=sel.value.split(':');S.key=v[0];S.dir=+v[1];refresh()});
  flt.querySelector('button').addEventListener('click',function(){S.from=0;refresh()});
  document.querySelectorAll('a.tmo').forEach(function(a){a.addEventListener('click',function(e){e.preventDefault();
    var w=a.closest('.tmwrap').getBoundingClientRect().width,k=w>=1040?'l':(w>=680?'m':'s');
    S.mode=a.dataset.mode;S.view='table';S.key='rank';S.dir=1;S.from=+a.dataset['from'+k.toUpperCase()]||+a.getAttribute('data-from-'+k);
    S.list=null;show();tw.scrollIntoView({block:'start'})})});
  btn.addEventListener('click',next);
  if('IntersectionObserver' in window){io=new IntersectionObserver(function(e){if(e[0].isIntersecting&&S.view==='table'&&!btn.hidden)next()},{rootMargin:'0px 0px 800px 0px'});io.observe(more)}
  var h=location.hash.slice(1);
  if(h==='repos'||h==='repos-table'){S.mode='repos'}
  if(h==='table'||h==='repos-table'||h==='list')S.view='table';
  show();if(S.view==='map')status();
})();
</script>""" % {"chunk": CHUNK}


def _map_items(order, people, vols):
    return [{"names": ([people[l]["name"], l] if people[l]["name"] and people[l]["name"] != l else [l]),
             "stars": vols[l]["gh_stars"], "stars_txt": "★ " + _n(vols[l]["gh_stars"]), "href": l, "avatar": l,
             "title": f'{people[l]["name"] or l} (@{l}) · GitHub ★ {_n(vols[l]["gh_stars"])} across {vols[l]["repos"]} repo{"" if vols[l]["repos"] == 1 else "s"}'}
            for l in order]


def _repo_items(repos):
    return [{"names": [e["repo"], e["repo"].split("/", 1)[-1]], "stars": e["stars"], "stars_txt": "★ " + _n(e["stars"]),
             "href": (f'../corpus/{e["corpus"]}' if e["corpus"] else f'{e["owner_page"] or e["by"]}'),
             "avatar": e["owner"], "title": f'{e["repo"]} · GitHub ★ {_n(e["stars"])}' + (f' · corpus {e["corpus"]}' if e["corpus"] else "")}
            for e in repos]


def _cap(what, unit):
    def cap(info):
        names = {"l": "this width", "m": "this width", "s": "this width"}
        parts = "".join(f'<span class="on-{k}" hidden>The map at {names[k]} shows {v[1]:,} {what}: the top {v[0]} squarified (area proportional to {unit}), '
                        f'then rows where every tile is wide enough for its name and the rest of each row is shared out by stars. '
                        f'The last tile holds the other {v[2]:,} and opens them in the table.</span>' for k, v in info.items())
        return f'<p class="tmcap">{parts}</p>'
    return cap


def wall_body(people, esc, live_slugs=(), pages=()):
    vols = {l: volume(pp, live_slugs) for l, pp in people.items()}
    order = wall_order(people, vols)
    repos = repo_index(people, pages)
    n_pages = max(1, -(-len(order) // CHUNK))
    r_pages = max(1, -(-len(repos) // CHUNK))
    first = "".join(people_tr(i + 1, l, people[l], vols[l], esc) for i, l in enumerate(order[:CHUNK]))
    pmap, _ = treemap.render(_map_items(order, people, vols), esc, "people", _cap("contributors", "the GitHub stars on their repositories"), len(order))
    rmap, _ = treemap.render(_repo_items(repos), esc, "repos", _cap("repositories", "each repository's GitHub stars"), len(repos))
    n_repos = len({w["repo_url"].lower() for pp in people.values() for w in pp["work"].values()})
    corpora = len({c for pp in people.values() for c in pp["corpora"]})
    sort_opts = '<label class="tsort" id="tsort">Sort by <select></select></label>'
    return f"""<main class="cwrap"><section class="whead"><h1>The people behind the hub</h1>
<p>{len(people):,} developers agreed to let AI agents learn from their work: {n_repos:,} repositories across {corpora:,} corpora, each included with the author's permission,
under its own licence, with attribution. Ranked by GitHub stars on their repositories; hub stars are the ones visitors give here. Each card shows the selected work and its licence.</p>
<a class="cta" href="../contribute">Add yours</a></section>
<div class="vbar" id="vbar" hidden><div class="seg" role="group" aria-label="Map tiles"><button type="button" data-mode="people" aria-pressed="true">People</button><button type="button" data-mode="repos" aria-pressed="false">Repos</button></div>
<div class="seg" role="group" aria-label="View"><button type="button" data-view="map" aria-pressed="true">Treemap</button><button type="button" data-view="table" aria-pressed="false">Table</button></div></div>
<section class="tmwrap" data-mode="people" aria-labelledby="tmh"><h2 id="tmh" class="wallh">Contributors by GitHub stars</h2>{pmap}</section>
<section class="tmwrap" data-mode="repos" aria-labelledby="tmr" hidden><h2 id="tmr" class="wallh">Repositories by GitHub stars</h2>{rmap}</section>
<section class="cview tablewrap" id="table" data-pchunks="{n_pages}" data-ptotal="{len(order)}" data-rchunks="{r_pages}" data-rtotal="{len(repos)}">
<h2 id="tableh" class="wallh">All {len(order):,} contributors</h2>
<p class="tfilter" id="tfilter" hidden><span data-t></span><button type="button">Show everyone</button></p>{sort_opts}
<table class="ctab" data-mode="people" data-stars="contributor">{thead(PEOPLE_COLS)}<tbody>{first}</tbody></table>
<table class="ctab" data-mode="repos" hidden>{thead(REPO_COLS)}<tbody></tbody></table>
<p class="wallmore" id="wallmore" aria-live="polite"><span data-st>Showing {min(CHUNK, len(order)):,} of {len(order):,}.</span><button type="button" hidden>Load more</button></p>
{pager(n_pages, 1)}{pager(r_pages, 0, "repos")}</section></main>{STARS_JS}{TABLE_JS}"""


def wall_chunks(people, esc, live_slugs=(), pages=()):
    """[(file stem, rows, html_body)]: list.<n> for n = 1.. (list.1.html is
    not written: index.html is page 1) and repos.<n> for n = 1..; each
    .json feeds TABLE_JS, each .html is the plain no-JS table page."""
    vols = {l: volume(pp, live_slugs) for l, pp in people.items()}
    order = wall_order(people, vols)
    repos = repo_index(people, pages)
    n_pages = max(1, -(-len(order) // CHUNK))
    r_pages = max(1, -(-len(repos) // CHUNK))
    out = []
    for n in range(1, n_pages + 1):
        part = order[(n - 1) * CHUNK:n * CHUNK]
        trs = "".join(people_tr((n - 1) * CHUNK + i + 1, l, people[l], vols[l], esc) for i, l in enumerate(part))
        body = (f'<main class="cwrap"><p><a class="backlink" href="./">&larr; the star map and the full table</a></p>'
                f'<section class="whead"><h1>Contributors, page {n} of {n_pages}</h1>'
                f'<p>Contributors {(n - 1) * CHUNK + 1:,} to {(n - 1) * CHUNK + len(part):,} of {len(order):,}, ranked by GitHub stars on their repositories.</p></section>'
                f'<div class="tablewrap"><table class="ctab" data-stars="contributor">{thead(PEOPLE_COLS, False)}<tbody>{trs}</tbody></table></div>'
                f'{pager(n_pages, n)}{pager(r_pages, 0, "repos")}</main>{STARS_JS}')
        out.append((f"list.{n}", [wall_row(l, people[l], vols[l]) for l in part], body if n > 1 else None))
    for n in range(1, r_pages + 1):
        part = repos[(n - 1) * CHUNK:n * CHUNK]
        trs = "".join(repo_tr((n - 1) * CHUNK + i + 1, e, esc) for i, e in enumerate(part))
        body = (f'<main class="cwrap"><p><a class="backlink" href="./#repos">&larr; the repository map and the full table</a></p>'
                f'<section class="whead"><h1>Repositories, page {n} of {r_pages}</h1>'
                f'<p>Repositories {(n - 1) * CHUNK + 1:,} to {(n - 1) * CHUNK + len(part):,} of {len(repos):,} pinned repositories with a recorded GitHub star count, most stars first.</p></section>'
                f'<div class="tablewrap"><table class="ctab">{thead(REPO_COLS, False)}<tbody>{trs}</tbody></table></div>'
                f'{pager(r_pages, n, "repos")}{pager(n_pages, 0)}</main>')
        out.append((f"repos.{n}", [repo_row(e) for e in part], body))
    return out


def by_corpus(people):
    """corpus slug -> logins appearing in it, for the corpus pages' Contributors block."""
    out = {}
    for login, pp in people.items():
        for slug in pp["corpora"]:
            out.setdefault(slug, []).append(login)
    return {k: sorted(v, key=str.lower) for k, v in out.items()}


# Corpus pages credit people in three tiers, each linking /contributors/<login>:
#   contributors  signed up (CONTRIBUTORS.jsonl / themed table); work included with permission
#   upstream      the GitHub owner of each pinned repository who has not signed up:
#                 credited under the repository's licence on an attribution page
#   curator       CURATOR, for a corpus with no GitHub-hosted source (or an aggregate
#                 of more than AGGREGATE owners, whose owners are credited per row
#                 under Sources and pins rather than one page each)
CURATOR = "xerj-org"
AGGREGATE = 50
OWNER_URL = re.compile(r"^https?://github\.com/([A-Za-z0-9-]+)/", re.I)


def _owners(c):
    seen = {}
    for src in list(c.get("repos") or []) + list(c.get("sources") or []):
        m = OWNER_URL.match(src.get("url", "") or "")
        if m and LOGIN_RE.match(m.group(1)):
            seen.setdefault(m.group(1).lower(), (m.group(1), src.get("repo") or src.get("slug") or src.get("url", ""), src.get("url", "")))
    return seen


def credits(registry, people):
    """(per_corpus, upstream). per_corpus: slug -> {"contributors", "upstream",
    "curator", "aggregate"}; upstream: lower login -> {"login", "repos": [(slug,
    repo, url)], "curates": [slug]} for every upstream page to generate."""
    lower = {l.lower(): l for l in people}
    inc = by_corpus(people)
    per, up = {}, {}
    for c in registry or []:
        own = _owners(c)
        agg = len(own) if len(own) > AGGREGATE else 0
        ups = []
        if not agg:
            for key, (login, repo, url) in own.items():
                if key in lower:
                    continue
                u = up.setdefault(key, {"login": login, "repos": [], "curates": []})
                u["repos"].append((c["slug"], repo, url))
                ups.append(u["login"])
        contrib = inc.get(c["slug"], [])
        cur = bool(agg) or not (contrib or ups)
        if cur:
            up.setdefault(CURATOR.lower(), {"login": CURATOR, "repos": [], "curates": []})["curates"].append(c["slug"])
        per[c["slug"]] = {"contributors": contrib, "upstream": sorted(set(ups), key=str.lower),
                          "curator": cur, "aggregate": agg}
    return per, up


def _person_tile(login, label, sub, esc, rel):
    return (f'<a href="{rel}contributors/{esc(login)}"><img src="https://github.com/{esc(login)}.png?size=88" alt="" loading="lazy" width="44" height="44">'
            f'<div><b>{esc(label)}</b><span>{esc(sub)}</span></div></a>')


def corpus_section(logins, esc, people, upstream=(), curator=False, aggregate=0, rel="../"):
    """The people block on /corpus/<slug>.html (empty when nobody is listed).
    Every tile is avatar + name linking to /contributors/<login>."""
    if not (logins or upstream or curator):
        return ""
    parts = []
    if logins:
        tiles = "".join(_person_tile(l, people[l]["name"] or l, "@" + l, esc, rel) for l in logins)
        parts.append(f'<h3>Contributors ({len(logins)})</h3><p class="note">People whose work this corpus selects or pins, each with their permission.</p>'
                     f'<div class="wall">{tiles}</div>')
    if upstream:
        tiles = "".join(_person_tile(l, l, "@" + l + " · upstream", esc, rel) for l in upstream)
        parts.append(f'<h3>Upstream authors ({len(upstream)})</h3><p class="note">Owners of the pinned repositories, credited under each '
                     f'repository\'s licence. They have not signed up here; their pages are attribution.</p><div class="wall">{tiles}</div>')
    if aggregate:
        parts.append(f'<p class="note">This corpus pins repositories from {aggregate:,} upstream authors; each one is credited with its '
                     f'repository under Sources and pins above.</p>')
    if curator:
        why = ("assembled it from those repositories and reviewed each licence." if aggregate else
               "maintain this registry entry; its sources are not GitHub repositories with an owner to credit, so the "
               "original publishers are the ones listed under Sources and pins.")
        parts.append(f'<h3>Curated by</h3><p class="note">The hub maintainers {why}</p>'
                     f'<div class="wall">{_person_tile(CURATOR, "XERJ Corpus Hub maintainers", "@" + CURATOR + " · curator", esc, rel)}</div>')
    return (f'<section class="detail people" id="contributors"><h2>People behind this corpus</h2>{"".join(parts)}'
            f'<p class="note"><a href="{rel}contributors/">All contributors</a>.</p></section>')


def upstream_body(u, esc, pages):
    login = u["login"]
    cur = login.lower() == CURATOR.lower()
    rows = "".join(f'<li><a href="{esc(url)}">{esc(repo)}</a> in <a href="../corpus/{esc(slug)}">corpus {esc(slug)}</a></li>'
                   for slug, repo, url in sorted(u["repos"], key=lambda r: (r[0], r[1])) if slug in pages)
    cura = "".join(f'<li><a href="../corpus/{esc(slug)}">{esc(slug)}</a></li>' for slug in sorted(u["curates"]) if slug in pages)
    n = len({r[0] for r in u["repos"]} | set(u["curates"]))
    if cur:
        who = ("The XERJ Corpus Hub maintainers. Besides their own repositories, they curate the registry entries whose sources "
               "have no GitHub owner to credit, and the large aggregate corpora.")
        claim = ""
    else:
        who = (f"@{esc(login)} is credited here because their public repositories are pinned in the corpora below, under each "
               f"repository's own licence. This is an attribution page: @{esc(login)} has not signed up as a Corpus Hub contributor, "
               f"and nothing here says they endorse the hub.")
        issue = urllib.parse.quote(f"Corpus Hub: attribution for @{login}")
        claim = (f'<p class="note">Is this you? <a href="../contribute">Add your work</a>, or '
                 f'<a href="https://github.com/xerj-org/xerj/issues/new?title={issue}">ask for a change</a>.</p>')
    sec_rows = f"<h2>Pinned repositories</h2><ul>{rows}</ul>" if rows else ""
    sec_cura = f"<h2>Curated corpora</h2><ul>{cura}</ul>" if cura else ""
    role = "curator" if cur else "upstream author"
    return (f'<main class="cwrap"><section class="prof"><img class="av" src="https://github.com/{esc(login)}.png?size=480" alt="{esc(login)}">'
            f'<div><h1>{esc(login)}</h1><div class="login"><a href="https://github.com/{esc(login)}">@{esc(login)}</a></div>'
            f'<div class="chips"><span class="chip up">{role}</span><span class="chip">{n} corp{"us" if n == 1 else "ora"}</span></div>'
            f'<p>{who}</p>{claim}</div></section>{sec_rows}{sec_cura}'
            f'<p class="note"><a href="./">All contributors</a></p></main>')


def candidates(people, registry_slugs, hub_dir):
    """Corpora a contributor appears in that have no registry page (author
    corpora and themed candidate corpora, all in licence review):
    slug -> {"kind", "works": [(login, work)], "repos": manifest repos or []}."""
    out = {}
    for login, pp in people.items():
        for w in pp["work"].values():
            for slug in w["corpora"]:
                if slug in registry_slugs or not re.match(r"^[A-Za-z0-9][A-Za-z0-9._-]*$", slug):
                    continue
                e = out.setdefault(slug, {"kind": pp["corpora"].get(slug, "candidate corpus"), "works": [], "repos": []})
                e["works"].append((login, w))
    for slug, e in out.items():
        # author corpora live in candidates/authors/, themed ones in candidates/
        hub = pathlib.Path(hub_dir) / "candidates"
        m = next((p for p in (hub / "authors" / f"{slug}.json", hub / f"{slug}.json") if p.exists()), None)
        if m:
            try:
                e["repos"] = json.loads(m.read_text()).get("repos") or []
            except ValueError:
                pass
    return out


def candidate_body(slug, e, esc, people):
    logins = sorted({l for l, _ in e["works"]}, key=str.lower)
    rows = []
    for l, w in sorted(e["works"], key=lambda x: (x[1]["repo"].lower(), x[0].lower())):
        commit = (f'<a href="{esc(w["commit_url"])}">{esc(w["commit"][:10])}</a> {esc(w["title"])}' if w["commit"] else "")
        rows.append(f'<tr><td class="mono"><a href="{esc(w["repo_url"])}">{esc(w["repo"])}</a></td><td><a href="../contributors/{esc(l)}">@{esc(l)}</a></td>'
                    f'<td class="mono">{commit}</td><td>{esc(w["spdx"] or "?")}</td></tr>')
    pinned = "".join(f'<tr><td class="mono"><a href="{esc(r.get("url", ""))}">{esc(r.get("repo", ""))}</a></td><td class="mono">{esc((r.get("sha") or "")[:12])}</td>'
                     f'<td>{esc((r.get("review") or {}).get("spdx") or r.get("licence") or "?")}</td></tr>' for r in e["repos"])
    pins = (f'<section class="detail"><h2>Pinned sources ({len(e["repos"])})</h2><table><tr><th>source</th><th>pin</th><th>licence</th></tr>{pinned}</table></section>'
            if pinned else "")
    cleared = bool(e["works"]) and all(w.get("reviewed") for _, w in e["works"])
    head = (f'<p class="note">{esc(e["kind"])} · <b>selected for the XERJ Corpus Hub · licence cleared</b></p>'
            f'<div class="kbox">The corpus is each whole repository at its pinned commit, included with the author\'s permission and attribution.</div>'
            if cleared else
            f'<p class="note">{esc(e["kind"])} · status <b>selected · in licence review</b> · not in the hub registry yet</p>'
            f'<div class="kbox"><b>Not installable yet</b><br>This corpus is a candidate: it is not in the hub registry until its licence review passes, '
            f'so <code>xerj corpus add</code> and <code>xerj ask</code> cannot fetch it. The corpus is each whole repository at its pinned commit.</div>')
    return (f'<main><p><a class="backlink" href="../contributors/">&larr; all contributors</a></p>'
            f'<h1 class="slug">{esc(slug)}</h1>' + head +
            f'<section class="detail"><h2>Selected work ({len(rows)})</h2><table><tr><th>repository</th><th>contributor</th><th>selected commit</th><th>licence</th></tr>'
            f'{"".join(rows)}</table></section>{pins}{corpus_section(logins, esc, people)}</main>')


def generate(out, hub_dir, page, esc, live_slugs, registry=None, people=None, upstream=None, cats=None, lastmod=None):
    if people is None:
        people = load(hub_dir, registry)
    if not people:
        return 0
    reg_slugs = {c["slug"] for c in registry or []}
    cands = candidates(people, reg_slugs, hub_dir)
    pages = reg_slugs | set(cands)
    d = pathlib.Path(out) / "contributors"
    d.mkdir(exist_ok=True)
    (pathlib.Path(out) / "corpus").mkdir(exist_ok=True)
    reg = {c["slug"]: c for c in registry or []}
    descs = repo_descs(hub_dir)
    by_cat = collections.defaultdict(list)
    for c in registry or []:
        if c["slug"] in live_slugs and c.get("cat"):
            by_cat[c["cat"]].append(c)
    stats = collections.Counter()
    lastmod = {} if lastmod is None else lastmod
    fetched, gh = repo_pages.load_stats()
    seen = repo_pages.load_seen()
    # repo pages: one repository per candidate corpus (exploit-pocs aggregates are registry pages, never here)
    pocs = {repo_pages.key(r.get("url")) for c in registry or [] if c["slug"].startswith("exploit-pocs")
            for r in c.get("repos") or []}
    rfacts = {slug: repo_pages.repo_facts(slug, e, gh, descs) for slug, e in cands.items()
              if len({w["repo_url"].lower() for _, w in e["works"]}) == 1 and len(e.get("repos") or []) <= 1
              and repo_pages.key(e["works"][0][1]["repo_url"]) not in pocs}
    stats["skipped: in an exploit-pocs collection"] = sum(
        1 for e in cands.values() if repo_pages.key(e["works"][0][1]["repo_url"]) in pocs)
    related = repo_pages.Related([c for c in registry or [] if c["slug"] in live_slugs],
                                 {s: (f["repo"], f["lang"], f["topics"]) for s, f in rfacts.items()}, gh)
    # a contributor page whose every corpus is a weak (noindexed) repo page repeats that weak page's facts:
    # noindex it too, unless Google already shows it (GSC impressions)
    weak_slugs = {slug for slug, f in rfacts.items() if repo_pages.weak(f, seen)}
    seen_people = {l.lower() for l in repo_pages.load_seen_contributors()}
    xp = pathlib.Path(__file__).resolve().parent / "data" / "x_posts.json"
    xposts = json.loads(xp.read_text(encoding="utf-8")) if xp.exists() else {}
    for login, pp in people.items():
        name = pp["name"] or login
        first = sorted(pp["work"].values(), key=lambda w: -(w.get("repo_stars") or 0))[0]
        f = seo_facts(pp, live_slugs, reg, descs, cats, by_cat)
        # related corpora: same language and topic first, live corpora and repo pages
        st = gh.get(repo_pages.key(f["w"]["repo_url"])) or {}
        lv, rp = related.pick(login, repo_pages.lang_of(st, f["w"]), set(f["w"]["topics"]) | set(st.get("topics") or []),
                              exclude=set(f["w"]["corpora"]), n_live=2, n_repo=2)
        if lv or rp:
            f["rel"] = [{"slug": x[0], "label": x[1]} for x in lv + rp]
        title, desc = seo_title(f), seo_desc(f)
        posts = xposts.get(login, [])
        stats["x posts"] += len(posts)
        dates = [x[:10] for x in pp.get("agreed") or [] if re.match(r"^\d{4}-\d{2}-\d{2}", x or "")]
        ld = seo_ld(f, posts)
        if dates:
            ld["dateModified"] = max(dates)
        mine = {c for w in pp["work"].values() for c in w["corpora"]} | set(pp.get("corpora") or ())
        all_weak = bool(mine) and mine <= weak_slugs and login.lower() not in seen_people
        noindex = f["thin"] or all_weak
        body = person_body(pp, esc, live_slugs, pages, agent_section(f, esc, pages), xposts_section(posts, esc), gh)
        html = page(title, desc, body, rel="../", path=f"/contributors/{login}", image=f"https://github.com/{login}.png?size=400",
                    kind="ProfilePage", noindex=noindex, ld_extra=ld)
        stats["thin" if f["thin"] else "weak repos only" if all_weak else "indexable"] += 1
        if not noindex and 'class="repofacts"' in body:
            stats["indexable with GitHub repo facts"] += 1
        (d / f"{login}.html").write_text(html)
        if dates:
            lastmod[f"contributors/{login}.html"] = max(dates)
    # one badge for everyone, served at /contributors/<login>/badge.svg by a _redirects rewrite: a copy per login
    # (identical bytes) pushed the site past the 20,000-file Cloudflare Pages limit (deploys failed from 2026-10-27)
    (d / "badge.svg").write_text(badge_svg())
    # upstream owners and the curator: attribution pages, kept out of search indexes
    if upstream is None:
        upstream = credits(registry, people)[1]
    for u in upstream.values():
        title = f"{u['login']}: credited on the XERJ Corpus Hub"
        desc = f"Corpora on the XERJ Corpus Hub that pin repositories by {u['login']}"
        html = page(title, desc, upstream_body(u, esc, pages), rel="../",
                    path=f"/contributors/{u['login']}", noindex=True,
                    image=f"https://github.com/{u['login']}.png?size=400")
        (d / f"{u['login']}.html").write_text(html)
    # candidate corpora: one page each, so every contributor's corpus chip links somewhere
    # a repo that is also a single-repo registry corpus: keep this page distinct from the curated one
    reg_of = {repo_pages.key(c["repos"][0].get("url")): c["slug"] for c in (registry or [])
              if len(c.get("repos") or []) == 1 and repo_pages.key(c["repos"][0].get("url"))}
    descs_out = {s: repo_pages.repo_description(f, dup=repo_pages.key(f["url"]) in reg_of) for s, f in rfacts.items()}
    dup = {d for d, n in collections.Counter(descs_out.values()).items() if n > 1}
    for slug, e in cands.items():
        f = rfacts.get(slug)
        if f is None:
            html = page(repo_pages.title(slug, "candidate corpus"), f"Candidate corpus {slug}: selected work in licence review.",
                        candidate_body(slug, e, esc, people), rel="../", path=f"/corpus/{slug}")
        else:
            lv, rp = related.pick(slug, f["lang"], f["topics"], exclude={slug})
            why = repo_pages.weak(f, seen)
            extra = "" if why else repo_pages.enrich_html(f, esc, people, cats, pages, fetched)
            body, ld = repo_pages.repo_body(f, esc, people, cats, (lv, rp), fetched, extra=extra)
            dates = [x for x in [(f["w"].get("agreed") or "")[:10], fetched if f["st"] else ""] if re.match(r"^\d{4}-\d{2}-\d{2}$", x)]
            if dates and isinstance(ld, dict):
                ld["dateModified"] = max(dates)
                if isinstance(ld.get("mainEntity"), dict):
                    ld["mainEntity"]["dateModified"] = max(dates)
            dsc = descs_out[slug] if descs_out[slug] not in dup else repo_pages.repo_description(f, dup=True)
            rs = reg_of.get(repo_pages.key(f["url"]))
            ttl = repo_pages.title(f["repo"], f"the repository behind the {rs} corpus") if rs else repo_pages.repo_title(f)
            html = page(ttl, dsc, body + REPO_JS, rel="../", path=f"/corpus/{slug}",
                        ld_extra=ld, og_title=f"{f['repo']} on XERJ Hub", noindex=bool(why))
            stats["repo pages"] += 1
            stats["repo pages noindexed (weak)" if why else "repo pages enriched"] += 1
            if why:
                stats["weak: " + why] += 1
            stats["repo pages with GitHub stats"] += bool(f["st"])
            dates = [x for x in [(f["w"].get("agreed") or "")[:10], fetched if f["st"] else ""] if re.match(r"^\d{4}-\d{2}-\d{2}$", x)]
            if dates:
                lastmod[f"corpus/{slug}.html"] = max(dates)
        (pathlib.Path(out) / "corpus" / f"{slug}.html").write_text(html)
    (d / "stars-targets.json").write_text(json.dumps(stars_targets(people, registry), separators=(",", ":")))
    html = page("Contributors: XERJ Corpus Hub", "The developers whose work AI agents retrieve from the XERJ Corpus Hub",
                wall_body(people, esc, live_slugs, pages), rel="../", path="/contributors/", kind="CollectionPage",
                og_title="The people behind the XERJ Corpus Hub",
                og_desc="Developers who agreed to let AI agents learn from their work")
    (d / "index.html").write_text(html)
    for stem, rows, body in wall_chunks(people, esc, live_slugs, pages):
        (d / f"{stem}.json").write_text(json.dumps(rows, separators=(",", ":"), ensure_ascii=False))
        if body is None:
            continue
        what = "Contributors" if stem.startswith("list.") else "Repositories"
        n = stem.split(".")[1]
        html = page(f"{what}, page {n}: XERJ Corpus Hub",
                    f"{what} on the XERJ Corpus Hub, page {n}: the developers whose work AI agents retrieve, "
                    f"{'by name' if what == 'Contributors' else 'by repository'}.", body, rel="../",
                    path=f"/contributors/{stem}", kind="CollectionPage",
                    og_title=f"{what}, page {n}: XERJ Corpus Hub",
                    og_desc="Developers who agreed to let AI agents learn from their work")
        (d / f"{stem}.html").write_text(html)
    print(f"contributors: {stats['indexable']} indexable, {stats['thin']} thin (noindex), {stats['weak repos only']} noindexed (every corpus a weak repo page), "
          f"{stats['indexable with GitHub repo facts']} indexable with GitHub repo facts, {stats['x posts']} X posts; "
          f"repo pages: {stats['repo pages']}, {stats['repo pages with GitHub stats']} with GitHub stats, "
          f"{stats['skipped: in an exploit-pocs collection']} skipped (in an exploit-pocs collection); "
          f"{stats['repo pages enriched']} enriched, {stats['repo pages noindexed (weak)']} noindexed as weak "
          f"({', '.join(f'{k[6:]}: {v}' for k, v in sorted(stats.items()) if k.startswith('weak: '))})")
    return len(people)
