// xerj-metrics — PRIVATE analytics. Records GitHub + website metrics daily and
// serves a key-gated dashboard at its own URL: https://<worker>/?key=REFRESH_KEY
// Every route requires the key; nothing is public. No PII is stored or read
// (installs carry a Cloudflare country + user-agent only, no IP, no cookie).
//
// Secrets (wrangler secret put): GITHUB_TOKEN (PAT with traffic read), REFRESH_KEY.
// Binding: INSTALLS -> R2 bucket xerj-installs (shared with the installer).
//
// Routes (all key-gated): / (dashboard), /data.json, /refresh,
// /trending-history[?date=YYYY-MM-DD].
//
// Synced from the deployed worker: the live module was esbuild output, so the
// bundler helpers (__name/__template) were removed and the dashboard is
// String.raw again. The served HTML is byte-identical to the deployed one.

var OWNER = "xerj-org";
var REPO = "xerj";
var KEY = "metrics/dashboard.json";
function addDays(d, n) {
  const t = new Date(d + "T00:00:00Z");
  t.setUTCDate(t.getUTCDate() + n);
  return t.toISOString().slice(0, 10);
}
function lastDates(today, n) {
  const a = [];
  for (let i = n - 1; i >= 0; i--) a.push(addDays(today, -i));
  return a;
}
async function gh(env, path) {
  const r = await fetch(`https://api.github.com/repos/${OWNER}/${REPO}${path}`, {
    headers: { Authorization: `Bearer ${env.GITHUB_TOKEN}`, "User-Agent": "xerj-metrics", Accept: "application/vnd.github+json" }
  });
  if (!r.ok) throw new Error("gh " + path + " " + r.status);
  return r.json();
}
async function ghFetch(env, url) {
  return fetch(url, { headers: { Authorization: `Bearer ${env.GITHUB_TOKEN}`, "User-Agent": "xerj-metrics", Accept: "application/vnd.github+json" } });
}
function parseTrending(html) {
  const rows = [];
  const blocks = html.split('<article class="Box-row">').slice(1);
  let rank = 0;
  for (const raw of blocks) {
    const b = raw.split("</article>")[0];
    const m = b.match(/<h2[^>]*>\s*<a[^>]*href="\/([^"]+)"/);
    if (!m) continue;
    const full = m[1].trim().replace(/\/+$/, "");
    rank++;
    let lang = null;
    const ml = b.match(/itemprop="programmingLanguage"[^>]*>([^<]+)</);
    if (ml) lang = ml[1].trim();
    const esc = full.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const numAnchor = (kind) => {
      const re = new RegExp('href="/' + esc + "/" + kind + '"[^>]*>([\\s\\S]*?)<\\/a>');
      const am = b.match(re);
      if (am) {
        const after = am[1].split("</svg>").pop();
        const nm = after.match(/([\d,]+)/);
        if (nm) return parseInt(nm[1].replace(/,/g, ""), 10);
      }
      return null;
    };
    const stars = numAnchor("stargazers");
    const forks = numAnchor("forks");
    let velocity = null;
    const mv = b.match(/([\d,]+)\s+stars?\s+(today|this week|this month)/);
    if (mv) velocity = { count: parseInt(mv[1].replace(/,/g, ""), 10), window: mv[2] };
    rows.push({ rank, full, lang, stars, forks, velocity });
  }
  return rows;
}
async function fetchTrending(window) {
  try {
    const r = await fetch(`https://github.com/trending?since=${window}`, { headers: { "User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36", Accept: "text/html" } });
    if (!r.ok) return [];
    return parseTrending(await r.text());
  } catch (e) {
    return [];
  }
}
async function commitsSince(env, full, sinceISO) {
  try {
    const r = await ghFetch(env, `https://api.github.com/repos/${full}/commits?since=${encodeURIComponent(sinceISO)}&per_page=1`);
    if (!r.ok) return null;
    const link = r.headers.get("link") || "";
    const m = link.match(/[?&]page=(\d+)>;\s*rel="last"/);
    if (m) return parseInt(m[1], 10);
    const arr = await r.json().catch(() => []);
    return Array.isArray(arr) ? arr.length : 0;
  } catch (e) {
    return null;
  }
}
async function mergedPRsOn(env, day) {
  try {
    const q = `repo:${OWNER}/${REPO} is:pr is:merged merged:${day}`;
    const r = await ghFetch(env, `https://api.github.com/search/issues?q=${encodeURIComponent(q)}&per_page=1`);
    if (!r.ok) return null;
    const j = await r.json();
    return typeof j.total_count === "number" ? j.total_count : null;
  } catch (e) {
    return null;
  }
}
async function mainCommitsOn(env, day) {
  try {
    const u = `https://api.github.com/repos/${OWNER}/${REPO}/commits?sha=main&since=${day}T00:00:00Z&until=${day}T23:59:59Z&per_page=1`;
    const r = await ghFetch(env, u);
    if (!r.ok) return null;
    const link = r.headers.get("link") || "";
    const mm = link.match(/[?&]page=(\d+)>;\s*rel="last"/);
    if (mm) return parseInt(mm[1], 10);
    const arr = await r.json().catch(() => []);
    return Array.isArray(arr) ? arr.length : 0;
  } catch (e) {
    return null;
  }
}
// Fill mergedPRs/mainCommits: always the last 3 days (today still moving), plus up to
// `backfill` older days that lack them. Sequential: the search API allows 30/min.
async function devActivity(env, bd, touch, today, backfill) {
  const days = lastDates(today, 3);
  const older = Object.keys(bd).filter((d) => d < days[0] && bd[d].mergedPRs == null).sort().reverse().slice(0, backfill);
  for (const d of days.concat(older)) {
    const [p, c] = [await mergedPRsOn(env, d), await mainCommitsOn(env, d)];
    const e = touch(d);
    if (p != null) e.mergedPRs = p;
    if (c != null) e.mainCommits = c;
  }
}
function devTotals(series, today) {
  const from = addDays(today, -6);
  const w = series.filter((r) => r.date >= from && r.date <= today);
  return { mergedPRs7d: w.reduce((a, r) => a + (r.mergedPRs || 0), 0), mainCommits7d: w.reduce((a, r) => a + (r.mainCommits || 0), 0) };
}
async function searchRepos(env, q, perPage) {
  try {
    const r = await ghFetch(env, `https://api.github.com/search/repositories?q=${encodeURIComponent(q)}&sort=stars&order=desc&per_page=${perPage}`);
    if (!r.ok) return { total: 0, items: [] };
    const j = await r.json();
    return { total: j.total_count || 0, items: (j.items || []).map((it) => ({ full: it.full_name, stars: it.stargazers_count, forks: it.forks_count, lang: it.language, created: (it.created_at || "").slice(0, 10), pushed: (it.pushed_at || "").slice(0, 10) })) };
  } catch (e) {
    return { total: 0, items: [] };
  }
}
async function collectTrending(env) {
  const windows = ["daily", "weekly", "monthly"];
  const now = Date.now();
  const since = new Date(now - 7 * 864e5).toISOString();
  const monthAgo = new Date(now - 30 * 864e5).toISOString().slice(0, 10);
  const [daily, weekly, monthly, mom] = await Promise.all([
    fetchTrending("daily"), fetchTrending("weekly"), fetchTrending("monthly"), searchRepos(env, `created:>${monthAgo}`, 100)
  ]);
  const win = { daily, weekly, monthly };
  const union = [];
  const seen = {};
  for (const w of windows) for (const row of win[w]) if (!seen[row.full]) { seen[row.full] = 1; union.push(row.full); }
  const CAP = 45;
  const picked = union.slice(0, CAP);
  const capped = union.length - picked.length;
  const counts = [];
  for (let i = 0; i < picked.length; i += 8) {
    const batch = await Promise.all(picked.slice(i, i + 8).map((f) => commitsSince(env, f, since)));
    for (const c of batch) counts.push(c);
  }
  const push = {};
  picked.forEach((f, i) => { push[f] = counts[i]; });
  for (const w of windows) for (const row of win[w]) row.pushes7d = (row.full in push) ? push[row.full] : null;
  const dv = daily.map((r) => r.velocity && r.velocity.count).filter((x) => x != null);
  const floorDaily = dv.length ? Math.min.apply(null, dv) : null;
  return { generated: new Date().toISOString(), windows: win, momentum: { total: mom.total, items: mom.items }, floor_daily: floorDaily, capped, cap: CAP };
}
async function ghReleases(env) {
  let page = 1, count = 0, downloads = 0;
  for (; page <= 6; ) {
    const r = await gh(env, `/releases?per_page=100&page=${page}`);
    if (!r.length) break;
    count += r.length;
    for (const rel of r) for (const a of rel.assets || []) downloads += a.download_count || 0;
    if (r.length < 100) break;
    page++;
  }
  return { count, downloads };
}
function uaFamily(ua) {
  const u = (ua || "").toLowerCase();
  for (const a of ["claude", "cursor", "copilot", "cody", "aider", "continue", "windsurf", "devin", "openai", "anthropic"])
    if (u.includes(a)) return "agent:" + a;
  if (u.includes("powershell")) return "PowerShell";
  if (u.startsWith("curl") || u.includes("curl/")) return "curl";
  if (u.includes("wget")) return "Wget";
  if (u.includes("python-requests") || u.includes("python/")) return "Python";
  if (u.includes("go-http")) return "Go";
  if (u.includes("node") || u.includes("undici")) return "Node";
  if (u.includes("deno")) return "Deno";
  if (["bot", "spider", "crawl", "slurp", "bingpreview", "facebookexternal"].some((b) => u.includes(b))) return "Bot";
  if (["mozilla", "chrome", "safari", "firefox", "webkit"].some((b) => u.includes(b))) return "Browser";
  if (!u) return "Unknown";
  return "Other";
}
async function countInstalls(env, date) {
  let n = 0, cursor;
  do {
    const l = await env.INSTALLS.list({ prefix: `installs/${date}/`, cursor, limit: 1e3 });
    n += l.objects.length;
    cursor = l.truncated ? l.cursor : void 0;
  } while (cursor);
  return n;
}
async function dayRecords(env, date) {
  const out = [], keys = [];
  let cursor;
  do {
    const l = await env.INSTALLS.list({ prefix: `installs/${date}/`, cursor, limit: 1e3 });
    for (const o of l.objects) keys.push(o.key);
    cursor = l.truncated ? l.cursor : void 0;
  } while (cursor);
  for (let i = 0; i < keys.length; i += 40) {
    const batch = await Promise.all(keys.slice(i, i + 40).map((k) => env.INSTALLS.get(k).then((o) => o && o.json()).catch(() => null)));
    for (const r of batch) if (r) out.push(r);
  }
  return out;
}
async function load(env) {
  const o = await env.INSTALLS.get(KEY);
  return o ? await o.json() : {};
}
async function liveUpdate(env) {
  const o = await env.INSTALLS.get(KEY);
  if (!o) return {};
  const m = await o.json();
  if (m.liveUpdated && Date.now() - Date.parse(m.liveUpdated) < 25e3) return m;
  try {
    const today = (new Date()).toISOString().slice(0, 10), yest = addDays(today, -1);
    const [inst, repo] = await Promise.all([countInstalls(env, today), gh(env, "")]);
    m.series = m.series || [];
    let te = m.series.find((r) => r.date === today);
    if (!te) { te = { date: today }; m.series.push(te); }
    const yd = m.series.find((r) => r.date === yest) || {};
    te.installs = inst;
    te.cumStars = repo.stargazers_count;
    te.newStars = Math.max(0, repo.stargazers_count - (yd.cumStars != null ? yd.cumStars : repo.stargazers_count));
    m.totals = m.totals || {};
    m.totals.stars = repo.stargazers_count;
    m.totals.forks = repo.forks_count;
    m.totals.watchers = repo.subscribers_count;
    m.totals.openIssues = repo.open_issues_count;
    m.totals.installs = m.series.reduce((s, r) => s + (r.installs || 0), 0);
    if (!m.devLive || Date.now() - Date.parse(m.devLive) > 3e5) {
      const p = await mergedPRsOn(env, today), c = await mainCommitsOn(env, today);
      if (p != null) te.mergedPRs = p;
      if (c != null) te.mainCommits = c;
      Object.assign(m.totals, devTotals(m.series, today));
      m.devLive = (new Date()).toISOString();
    }
    m.liveUpdated = (new Date()).toISOString();
    await env.INSTALLS.put(KEY, JSON.stringify(m), { onlyIf: { etagMatches: o.etag }, httpMetadata: { contentType: "application/json" } });
  } catch (e) { m.liveError = String(e); }
  return m;
}
async function snapshot(env) {
  const m = await load(env);
  m.series = m.series || [];
  const bd = {};
  m.series.forEach((r) => bd[r.date] = r);
  const touch = (d) => bd[d] || (bd[d] = { date: d });
  const today = (new Date()).toISOString().slice(0, 10), yest = addDays(today, -1);
  const cl = await gh(env, "/traffic/clones");
  for (const d of cl.clones || []) {
    const e = touch(d.timestamp.slice(0, 10));
    e.clones = d.count;
    e.cloneUniques = d.uniques;
  }
  const vw = await gh(env, "/traffic/views");
  for (const d of vw.views || []) {
    const e = touch(d.timestamp.slice(0, 10));
    e.views = d.count;
    e.viewUniques = d.uniques;
  }
  for (const d of lastDates(today, 16)) touch(d).installs = await countInstalls(env, d);
  m.countryCounts = m.countryCounts || {};
  m.uaCounts = m.uaCounts || {};
  m.agentCounts = m.agentCounts || {};
  let from = m.aggThrough ? addDays(m.aggThrough, 1) : m.start || today;
  for (let d = from; d <= yest; d = addDays(d, 1)) {
    for (const r of await dayRecords(env, d)) {
      const c = r.country || "??";
      m.countryCounts[c] = (m.countryCounts[c] || 0) + 1;
      const fam = uaFamily(r.ua);
      if (fam.startsWith("agent:")) {
        const a = fam.slice(6);
        m.agentCounts[a] = (m.agentCounts[a] || 0) + 1;
        m.uaCounts["AI agent UA"] = (m.uaCounts["AI agent UA"] || 0) + 1;
      } else m.uaCounts[fam] = (m.uaCounts[fam] || 0) + 1;
    }
    m.aggThrough = d;
  }
  const repo = await gh(env, "");
  const te = touch(today);
  te.cumStars = repo.stargazers_count;
  const yd = bd[yest] || {};
  te.newStars = Math.max(0, repo.stargazers_count - (yd.cumStars != null ? yd.cumStars : repo.stargazers_count));
  const rel = await ghReleases(env);
  try { await devActivity(env, bd, touch, today, 20); } catch (e) { m.devError = String(e); }
  m.series = Object.keys(bd).sort().map((k) => bd[k]);
  m.start = m.series[0].date;
  const sum = (a, f) => a.reduce((s, x) => s + (x[f] || 0), 0);
  m.totals = {
    stars: repo.stargazers_count,
    forks: repo.forks_count,
    watchers: repo.subscribers_count,
    openIssues: repo.open_issues_count,
    clones14d: sum(cl.clones || [], "count"),
    cloneUniques14d: sum(cl.clones || [], "uniques"),
    views14d: sum(vw.views || [], "count"),
    viewUniques14d: sum(vw.views || [], "uniques"),
    releaseDownloads: rel.downloads,
    releases: rel.count,
    installs: m.series.reduce((s, r) => s + (r.installs || 0), 0),
    countries: Object.keys(m.countryCounts).filter((c) => c !== "??" && c !== "T1").length,
    ...devTotals(m.series, today)
  };
  try {
    m.trending = await collectTrending(env);
    await env.INSTALLS.put("trending/" + (m.trending.generated || "").slice(0, 10) + ".json", JSON.stringify(m.trending), { httpMetadata: { contentType: "application/json" } });
  } catch (e) { m.trendingError = String(e); }
  m.cadence = "every 3h";
  m.updated = (new Date()).toISOString();
  await env.INSTALLS.put(KEY, JSON.stringify(m), { httpMetadata: { contentType: "application/json" } });
  return m;
}

const DASH = String.raw`<!doctype html><html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>XERJ Analytics</title>
<link rel="preconnect" href="https://fonts.googleapis.com"><link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Big+Shoulders+Display:wght@400;700;800;900&family=Inter:wght@400;500;600;700&family=IBM+Plex+Sans:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;700&display=swap" rel="stylesheet">
<style>
:root{--z-bg:#f6f4ee;--z-ink:#11120f;--z-mute:#696762;--z-faint:#e3dfd4;--z-line:#cfcbbf;--z-accent:#7f5200;--z-cmp:#6f8aa8;--ok:#2e6b3a;--bad:#8c2f2f;--warn:#8a6d1a;
--fd:'Big Shoulders Display','Inter',system-ui,sans-serif;--fp:'Inter',system-ui,sans-serif;--fda:'IBM Plex Sans','Inter',sans-serif;--fm:'JetBrains Mono',monospace;--px:48px;--mw:1280px}
*{box-sizing:border-box}body{margin:0;background:var(--z-bg);color:var(--z-ink);font-family:var(--fp);-webkit-font-smoothing:antialiased}
.nav{display:flex;align-items:baseline;gap:20px;padding:16px var(--px);border-bottom:1px solid var(--z-line);font-family:var(--fda);font-size:11px;font-weight:600;text-transform:uppercase;letter-spacing:.14em;flex-wrap:wrap}
.nav .brand{font-family:var(--fd);font-weight:900;font-size:16px;letter-spacing:.24em;color:var(--z-ink);text-decoration:none}.nav .brand b{color:var(--z-accent)}
.nav a{color:var(--z-mute);text-decoration:none}.nav a:hover{color:var(--z-ink)}.nav .sp{flex:1}
.live{display:inline-flex;align-items:center;gap:6px;color:var(--z-mute)}.dot{width:7px;height:7px;border-radius:50%;background:var(--ok);display:inline-block;animation:p 2s infinite}
@keyframes p{0%,100%{opacity:1}50%{opacity:.3}}
main{max-width:var(--mw);margin:0 auto;padding:32px var(--px) 64px}
.kick{font-family:var(--fda);font-size:11px;font-weight:500;letter-spacing:.14em;text-transform:uppercase;color:var(--z-mute)}.kick .a{color:var(--z-accent)}
h1{font-family:var(--fd);font-weight:900;font-size:clamp(44px,8vw,96px);line-height:.9;letter-spacing:-.02em;margin:6px 0 2px}h1 .a{color:var(--z-accent)}
.sub{font-family:var(--fda);font-size:11px;text-transform:uppercase;letter-spacing:.14em;color:var(--z-mute);margin:0 0 28px}
.stats{display:flex;flex-wrap:wrap;border-block:1px solid var(--z-line);margin-bottom:40px}
.stat{padding:16px 24px 16px 0;margin-right:24px;border-right:1px solid var(--z-line);min-width:118px}.stat:last-child{border-right:0;margin-right:0}
.stat b{display:block;font-family:var(--fd);font-weight:800;font-size:32px;line-height:1}.stat b.a{color:var(--z-accent)}
.stat span{font-family:var(--fda);font-size:11px;font-weight:500;text-transform:uppercase;letter-spacing:.08em;color:var(--z-mute)}
section{margin-bottom:40px}
.sh{display:flex;align-items:baseline;justify-content:space-between;gap:16px;border-bottom:1px solid var(--z-line);padding-bottom:12px;margin-bottom:20px}
.sh h2{font-family:var(--fd);font-weight:800;font-size:32px;letter-spacing:-.01em;margin:0}.sh .hint{font-family:var(--fda);font-size:11px;text-transform:uppercase;letter-spacing:.08em;color:var(--z-mute)}
.actrow{display:grid;grid-template-columns:230px 1fr;gap:24px;align-items:start}
.leg{display:flex;flex-direction:column;gap:1px;background:var(--z-line);border:1px solid var(--z-line)}
.lt{background:var(--z-bg);padding:11px 13px;cursor:pointer;display:flex;align-items:center;gap:9px;user-select:none}
.lt:hover{background:#fff}.lt .sw{width:12px;height:12px;flex:0 0 12px;border-radius:2px}
.lt .nm{flex:1;min-width:0;font-family:var(--fda);font-size:10px;text-transform:uppercase;letter-spacing:.05em;color:var(--z-mute);line-height:1.25}
.lt .vl{font-family:var(--fd);font-weight:800;font-size:19px;line-height:1;color:var(--z-ink);text-align:right;white-space:nowrap;margin-left:auto}
.lt.off{opacity:.4}.lt.off .vl{text-decoration:line-through}
.legwrap{display:flex;flex-direction:column;gap:12px}
.logtog{display:flex;align-items:center;gap:8px;font-family:var(--fda);font-size:11px;text-transform:uppercase;letter-spacing:.08em;color:var(--z-mute);cursor:pointer;padding-left:2px}
.logtog input{accent-color:var(--z-accent);cursor:pointer}
.chartbox{position:relative;height:430px}
.grid2{display:grid;grid-template-columns:1.5fr 1fr;gap:32px;align-items:start}
.mapbox{position:relative;height:440px}
.toplist{font-family:var(--fm);font-size:13px}.toprow{display:flex;align-items:center;gap:12px;padding:6px 0;border-bottom:1px solid var(--z-faint)}
.toprow .cn{flex:1;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}.toprow .bar{height:8px;background:var(--z-accent);opacity:.85}.toprow .cv{width:48px;text-align:right;color:var(--z-mute)}
.bk{display:flex;flex-wrap:wrap;gap:1px;background:var(--z-line);border:1px solid var(--z-line)}.bk>div{background:var(--z-bg);padding:16px 20px;flex:1 1 150px}
.bk b{display:block;font-family:var(--fd);font-weight:800;font-size:32px;line-height:1}.bk span{font-family:var(--fda);font-size:11px;text-transform:uppercase;letter-spacing:.08em;color:var(--z-mute)}
.note{font-family:var(--fda);font-size:11px;color:var(--z-mute);line-height:1.6;border-top:1px solid var(--z-line);padding-top:20px;margin-top:40px}.note b{color:var(--z-ink)}
code{font-family:var(--fm);font-size:.92em;background:var(--z-faint);padding:1px 5px}
@media(max-width:900px){:root{--px:16px}.actrow,.grid2{grid-template-columns:1fr}.stat{width:46%}}
</style></head><body>
<nav class="nav"><a class="brand" href="https://xerj.org">XERJ<b>&middot;</b>ANALYTICS</a><span>PRIVATE</span>
<span class="sp"></span><span class="live"><span class="dot"></span><span id="live">live</span></span>
<a href="https://hub.xerj.org">HUB</a><a href="https://github.com/xerj-org/xerj">GITHUB</a></nav>
<main>
<div class="kick"><span class="a">XERJ</span> &middot; GROWTH &amp; ADOPTION</div>
<h1>GROWTH &amp; <span class="a">ADOPTION</span></h1>
<p class="sub" id="upd">loading&hellip;</p>
<div class="stats" id="stats"></div>
<section><div class="sh"><h2>Activity over time</h2><span class="hint">click a tile to toggle its series</span></div>
<div class="actrow"><div class="legwrap"><div class="leg" id="leg"></div><label class="logtog"><input type="checkbox" id="logscale"> Log scale</label><label class="logtog"><input type="checkbox" id="linetog"> Lines</label></div><div class="chartbox"><canvas id="ts"></canvas></div></div></section>
<section><div class="sh"><h2>Where installs come from</h2><span class="hint" id="geoh"></span></div>
<div class="grid2"><div class="mapbox"><canvas id="map"></canvas></div><div class="toplist" id="top"></div></div></section>
<section><div class="sh"><h2>How installs arrive</h2><span class="hint" id="uah"></span></div>
<div class="grid2"><div class="chartbox" style="height:320px"><canvas id="ua"></canvas></div>
<div><div class="bk" id="osbk"></div><p class="note" style="margin-top:20px;border:0;padding:0" id="agn"></p></div></div></section>
<section><div class="sh"><h2>Merged to main</h2><span class="hint" id="devh"></span></div>
<div class="grid2"><div class="chartbox" style="height:340px"><canvas id="dev"></canvas></div><div class="toplist" id="devt"></div></div></section>
<p class="note" id="foot"></p>
</main>
<script>const CM={"AW":["Aruba",533],"AF":["Afghanistan",4],"AO":["Angola",24],"AI":["Anguilla",660],"AX":["\u00c5land Islands",248],"AL":["Albania",8],"AD":["Andorra",20],"AE":["United Arab Emirates",784],"AR":["Argentina",32],"AM":["Armenia",51],"AS":["American Samoa",16],"AQ":["Antarctica",10],"TF":["French Southern Territories",260],"AG":["Antigua and Barbuda",28],"AU":["Australia",36],"AT":["Austria",40],"AZ":["Azerbaijan",31],"BI":["Burundi",108],"BE":["Belgium",56],"BJ":["Benin",204],"BQ":["Bonaire, Sint Eustatius and Saba",535],"BF":["Burkina Faso",854],"BD":["Bangladesh",50],"BG":["Bulgaria",100],"BH":["Bahrain",48],"BS":["Bahamas",44],"BA":["Bosnia and Herzegovina",70],"BL":["Saint Barth\u00e9lemy",652],"BY":["Belarus",112],"BZ":["Belize",84],"BM":["Bermuda",60],"BO":["Bolivia",68],"BR":["Brazil",76],"BB":["Barbados",52],"BN":["Brunei Darussalam",96],"BT":["Bhutan",64],"BV":["Bouvet Island",74],"BW":["Botswana",72],"CF":["Central African Republic",140],"CA":["Canada",124],"CC":["Cocos (Keeling) Islands",166],"CH":["Switzerland",756],"CL":["Chile",152],"CN":["China",156],"CI":["C\u00f4te d'Ivoire",384],"CM":["Cameroon",120],"CD":["Congo, The Democratic Republic of the",180],"CG":["Congo",178],"CK":["Cook Islands",184],"CO":["Colombia",170],"KM":["Comoros",174],"CV":["Cabo Verde",132],"CR":["Costa Rica",188],"CU":["Cuba",192],"CW":["Cura\u00e7ao",531],"CX":["Christmas Island",162],"KY":["Cayman Islands",136],"CY":["Cyprus",196],"CZ":["Czechia",203],"DE":["Germany",276],"DJ":["Djibouti",262],"DM":["Dominica",212],"DK":["Denmark",208],"DO":["Dominican Republic",214],"DZ":["Algeria",12],"EC":["Ecuador",218],"EG":["Egypt",818],"ER":["Eritrea",232],"EH":["Western Sahara",732],"ES":["Spain",724],"EE":["Estonia",233],"ET":["Ethiopia",231],"FI":["Finland",246],"FJ":["Fiji",242],"FK":["Falkland Islands (Malvinas)",238],"FR":["France",250],"FO":["Faroe Islands",234],"FM":["Micronesia, Federated States of",583],"GA":["Gabon",266],"GB":["United Kingdom",826],"GE":["Georgia",268],"GG":["Guernsey",831],"GH":["Ghana",288],"GI":["Gibraltar",292],"GN":["Guinea",324],"GP":["Guadeloupe",312],"GM":["Gambia",270],"GW":["Guinea-Bissau",624],"GQ":["Equatorial Guinea",226],"GR":["Greece",300],"GD":["Grenada",308],"GL":["Greenland",304],"GT":["Guatemala",320],"GF":["French Guiana",254],"GU":["Guam",316],"GY":["Guyana",328],"HK":["Hong Kong",344],"HM":["Heard Island and McDonald Islands",334],"HN":["Honduras",340],"HR":["Croatia",191],"HT":["Haiti",332],"HU":["Hungary",348],"ID":["Indonesia",360],"IM":["Isle of Man",833],"IN":["India",356],"IO":["British Indian Ocean Territory",86],"IE":["Ireland",372],"IR":["Iran",364],"IQ":["Iraq",368],"IS":["Iceland",352],"IL":["Israel",376],"IT":["Italy",380],"JM":["Jamaica",388],"JE":["Jersey",832],"JO":["Jordan",400],"JP":["Japan",392],"KZ":["Kazakhstan",398],"KE":["Kenya",404],"KG":["Kyrgyzstan",417],"KH":["Cambodia",116],"KI":["Kiribati",296],"KN":["Saint Kitts and Nevis",659],"KR":["South Korea",410],"KW":["Kuwait",414],"LA":["Lao People's Democratic Republic",418],"LB":["Lebanon",422],"LR":["Liberia",430],"LY":["Libya",434],"LC":["Saint Lucia",662],"LI":["Liechtenstein",438],"LK":["Sri Lanka",144],"LS":["Lesotho",426],"LT":["Lithuania",440],"LU":["Luxembourg",442],"LV":["Latvia",428],"MO":["Macao",446],"MF":["Saint Martin (French part)",663],"MA":["Morocco",504],"MC":["Monaco",492],"MD":["Moldova",498],"MG":["Madagascar",450],"MV":["Maldives",462],"MX":["Mexico",484],"MH":["Marshall Islands",584],"MK":["North Macedonia",807],"ML":["Mali",466],"MT":["Malta",470],"MM":["Myanmar",104],"ME":["Montenegro",499],"MN":["Mongolia",496],"MP":["Northern Mariana Islands",580],"MZ":["Mozambique",508],"MR":["Mauritania",478],"MS":["Montserrat",500],"MQ":["Martinique",474],"MU":["Mauritius",480],"MW":["Malawi",454],"MY":["Malaysia",458],"YT":["Mayotte",175],"NA":["Namibia",516],"NC":["New Caledonia",540],"NE":["Niger",562],"NF":["Norfolk Island",574],"NG":["Nigeria",566],"NI":["Nicaragua",558],"NU":["Niue",570],"NL":["Netherlands",528],"NO":["Norway",578],"NP":["Nepal",524],"NR":["Nauru",520],"NZ":["New Zealand",554],"OM":["Oman",512],"PK":["Pakistan",586],"PA":["Panama",591],"PN":["Pitcairn",612],"PE":["Peru",604],"PH":["Philippines",608],"PW":["Palau",585],"PG":["Papua New Guinea",598],"PL":["Poland",616],"PR":["Puerto Rico",630],"KP":["Korea, Democratic People's Republic of",408],"PT":["Portugal",620],"PY":["Paraguay",600],"PS":["Palestine, State of",275],"PF":["French Polynesia",258],"QA":["Qatar",634],"RE":["R\u00e9union",638],"RO":["Romania",642],"RU":["Russia",643],"RW":["Rwanda",646],"SA":["Saudi Arabia",682],"SD":["Sudan",729],"SN":["Senegal",686],"SG":["Singapore",702],"GS":["South Georgia and the South Sandwich Islands",239],"SH":["Saint Helena, Ascension and Tristan da Cunha",654],"SJ":["Svalbard and Jan Mayen",744],"SB":["Solomon Islands",90],"SL":["Sierra Leone",694],"SV":["El Salvador",222],"SM":["San Marino",674],"SO":["Somalia",706],"PM":["Saint Pierre and Miquelon",666],"RS":["Serbia",688],"SS":["South Sudan",728],"ST":["Sao Tome and Principe",678],"SR":["Suriname",740],"SK":["Slovakia",703],"SI":["Slovenia",705],"SE":["Sweden",752],"SZ":["Eswatini",748],"SX":["Sint Maarten (Dutch part)",534],"SC":["Seychelles",690],"SY":["Syria",760],"TC":["Turks and Caicos Islands",796],"TD":["Chad",148],"TG":["Togo",768],"TH":["Thailand",764],"TJ":["Tajikistan",762],"TK":["Tokelau",772],"TM":["Turkmenistan",795],"TL":["Timor-Leste",626],"TO":["Tonga",776],"TT":["Trinidad and Tobago",780],"TN":["Tunisia",788],"TR":["T\u00fcrkiye",792],"TV":["Tuvalu",798],"TW":["Taiwan",158],"TZ":["Tanzania",834],"UG":["Uganda",800],"UA":["Ukraine",804],"UM":["United States Minor Outlying Islands",581],"UY":["Uruguay",858],"US":["United States",840],"UZ":["Uzbekistan",860],"VA":["Holy See (Vatican City State)",336],"VC":["Saint Vincent and the Grenadines",670],"VE":["Venezuela",862],"VG":["Virgin Islands, British",92],"VI":["Virgin Islands, U.S.",850],"VN":["Vietnam",704],"VU":["Vanuatu",548],"WF":["Wallis and Futuna",876],"WS":["Samoa",882],"YE":["Yemen",887],"ZA":["South Africa",710],"ZM":["Zambia",894],"ZW":["Zimbabwe",716]};const KEYQ=location.search;</script>
<script src="https://cdnjs.cloudflare.com/ajax/libs/Chart.js/4.4.1/chart.umd.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/topojson-client@3.1.0/dist/topojson-client.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/chartjs-chart-geo@4.3.1/build/index.umd.min.js"></script>
<script>
var C={bg:'#f6f4ee',ink:'#11120f',mute:'#696762',faint:'#e3dfd4',line:'#cfcbbf',accent:'#7f5200',cmp:'#6f8aa8',ok:'#2e6b3a',bad:'#8c2f2f',warn:'#8a6d1a'};
var fmt=function(n){return (n||0).toLocaleString()};
var tsChart,mapChart,uaChart,DATA,lastFetch=0,mapDone=false;
function ago(ms){var s=Math.round((Date.now()-ms)/1000);if(s<60)return s+'s ago';var m=Math.round(s/60);if(m<60)return m+'m ago';return Math.round(m/60)+'h ago';}
function trim(series){var s=series.slice();while(s.length){var r=s[s.length-1];var empty=!(r.installs)&&r.clones==null&&r.views==null&&!(r.newStars);if(empty)s.pop();else break;}return s;}
function render(){
  var T=DATA.totals, S=trim(DATA.series);
  document.getElementById('upd').innerHTML='Snapshot '+((DATA.updated||'').slice(0,16).replace('T',' '))+' UTC &middot; recorded '+(DATA.cadence||'daily')+(DATA.liveUpdated?' &middot; installs+stars live '+DATA.liveUpdated.slice(11,19)+' UTC, every 30s':'')+' &middot; data from '+DATA.start+' &middot; <span id="fresh"></span>';
  var stats=[['stars',T.stars,1],['forks',T.forks],['unique cloners 14d',T.cloneUniques14d],['unique viewers 14d',T.viewUniques14d],['website installs',T.installs,1],['countries',T.countries],['release downloads',T.releaseDownloads],['open issues',T.openIssues],['PRs merged 7d',T.mergedPRs7d,1],['commits to main 7d',T.mainCommits7d]];
  document.getElementById('stats').innerHTML=stats.map(function(s){return '<div class="stat"><b'+(s[2]?' class="a"':'')+'>'+fmt(s[1])+'</b><span>'+s[0]+'</span></div>'}).join('');
  var labels=S.map(function(r){return r.date});
  function col(k){return S.map(function(r){return (k in r)?r[k]:null})}
  var grid={color:'#ece7db',drawTicks:false},tick={color:C.mute,font:{family:'IBM Plex Sans',size:10},maxRotation:0,autoSkip:true,maxTicksLimit:12};
  function sum(k){return S.reduce(function(a,r){return a+(r[k]||0)},0)}
  var defs=[
   {lbl:'clones / day',k:'clones',c:C.accent,ax:'yL',tot:sum('clones')},
   {lbl:'repo views / day',k:'views',c:C.cmp,ax:'yL',tot:sum('views')},
   {lbl:'unique cloners / day',k:'cloneUniques',c:C.warn,ax:'yL',dash:[4,3],off:true,tot:sum('cloneUniques')},
   {lbl:'unique viewers / day',k:'viewUniques',c:'#9ab0c6',ax:'yL',dash:[4,3],off:true,tot:sum('viewUniques')},
   {lbl:'stars (total)',k:'cumStars',c:'#b0724e',ax:'yL',off:true,tot:T.stars},
   {lbl:'installs / day',k:'installs',c:C.ok,ax:'yR',tot:sum('installs')},
   {lbl:'new stars / day',k:'newStars',c:C.bad,ax:'yR',dash:[2,2],off:true,tot:sum('newStars')},
   {lbl:'PRs merged / day',k:'mergedPRs',c:'#7a3fb0',ax:'yR',tot:sum('mergedPRs')},
   {lbl:'commits to main / day',k:'mainCommits',c:'#1f8a8a',ax:'yR',dash:[4,2],off:true,tot:sum('mainCommits')}
  ];
  if(!tsChart){
    var lt=document.getElementById('logscale').checked?'logarithmic':'linear';
    var dsets=defs.map(function(d){var LN=!!(document.getElementById('linetog')||{}).checked;return {label:d.lbl,data:col(d.k),backgroundColor:d.c,borderColor:d.c,yAxisID:d.ax,type:(LN?'line':'bar'),fill:false,tension:(LN?0.25:0),pointRadius:0,borderWidth:(LN?2:0),borderRadius:0,categoryPercentage:0.9,barPercentage:1,hidden:!!d.off}});
    tsChart=new Chart(document.getElementById('ts'),{type:'bar',data:{labels:labels,datasets:dsets},options:{responsive:true,maintainAspectRatio:false,interaction:{mode:'index',intersect:false},
      scales:{x:{grid:grid,ticks:tick},yL:{type:lt,position:'left',beginAtZero:true,grid:grid,ticks:tick,title:{display:true,text:'clones / views / stars',color:C.mute,font:{size:10}}},yR:{type:lt,position:'right',beginAtZero:true,grid:{drawOnChartArea:false},ticks:tick,title:{display:true,text:'installs / new stars / merges',color:C.mute,font:{size:10}}}},
      plugins:{legend:{display:false},tooltip:{backgroundColor:'#fff',titleColor:C.ink,bodyColor:C.ink,borderColor:C.line,borderWidth:1}}}});
  }else{
    tsChart.data.labels=labels;
    defs.forEach(function(d,i){tsChart.data.datasets[i].data=col(d.k)});
    tsChart.update();
  }
  // tile legend: shows totals (right-aligned) and the CURRENT toggle state,
  // which survives live refreshes because the chart is updated in place.
  document.getElementById('leg').innerHTML=defs.map(function(d,i){var off=!tsChart.isDatasetVisible(i);return '<div class="lt'+(off?' off':'')+'" data-i="'+i+'"><span class="sw" style="background:'+d.c+'"></span><span class="nm">'+d.lbl+'</span><span class="vl">'+fmt(d.tot)+'</span></div>'}).join('');
  [].forEach.call(document.querySelectorAll('.lt'),function(el){el.onclick=function(){var i=+el.dataset.i;var vis=tsChart.isDatasetVisible(i);tsChart.setDatasetVisibility(i,!vis);tsChart.update();el.classList.toggle('off',vis)}});
  // installers
  var uf=DATA.uaCounts||{};var ufa=Object.keys(uf).map(function(k){return {name:k,count:uf[k]}}).sort(function(a,b){return b.count-a.count});
  document.getElementById('uah').textContent=fmt(T.installs)+' /get requests';
  if(uaChart)uaChart.destroy();
  uaChart=new Chart(document.getElementById('ua'),{type:'bar',data:{labels:ufa.map(function(x){return x.name}),datasets:[{data:ufa.map(function(x){return x.count}),backgroundColor:C.accent,borderWidth:0}]},
    options:{indexAxis:'y',responsive:true,maintainAspectRatio:false,scales:{x:{beginAtZero:true,grid:grid,ticks:tick},y:{grid:{display:false},ticks:{color:C.ink,font:{family:'IBM Plex Sans',size:12}}}},plugins:{legend:{display:false},tooltip:{backgroundColor:'#fff',titleColor:C.ink,bodyColor:C.ink,borderColor:C.line,borderWidth:1}}}});
  var ag=DATA.agentCounts||{},agN=Object.keys(ag).reduce(function(a,k){return a+ag[k]},0);
  var unix=0,win=0;for(var k in uf){} // sh vs ps from countryless -- approximate via uaCounts
  document.getElementById('osbk').innerHTML=[['curl',uf.curl||0],['browser hits',uf.Browser||0],['powershell',uf.PowerShell||0],['named AI-agent UAs',agN]].map(function(x){return '<div><b>'+fmt(x[1])+'</b><span>'+x[0]+'</span></div>'}).join('');
  var agl=Object.keys(ag).map(function(k){return k+' '+ag[k]}).join(', ')||'none';
  document.getElementById('agn').innerHTML='<b>Which agent?</b> Mostly unknowable from the UA: an agent that runs <code>curl … | sh</code> sends curl’s user-agent, not its own, so Claude Code / Cursor fold into the curl bucket. Clients that set their own UA: '+agl+'. To attribute installs to agents, tag the install URL (a <code>?via=</code> parameter), not the UA.';
  // countries
  var cc=DATA.countryCounts||{};var cs=Object.keys(cc).filter(function(c){return c!=='??'&&c!=='T1'}).map(function(c){var e=CM[c]||[c,null];return {code:c,name:e[0],num:e[1],installs:cc[c]}}).sort(function(a,b){return b.installs-a.installs});
  var cmax=cs.length?cs[0].installs:1;
  document.getElementById('geoh').textContent=cs.length+' countries · '+fmt(T.installs)+' installs';
  document.getElementById('top').innerHTML=cs.slice(0,16).map(function(c){return '<div class="toprow"><span class="cn">'+c.name+'</span><span class="bar" style="width:'+Math.max(4,Math.round(c.installs*120/cmax))+'px"></span><span class="cv">'+c.installs+'</span></div>'}).join('');
  drawMap(cs,cmax);
  try{devRender()}catch(e){}
}
function drawMap(cs,cmax){
  if(mapDone)return;
  if(window.ChartGeo){try{Chart.register(ChartGeo.ChoroplethController,ChartGeo.ProjectionScale,ChartGeo.ColorScale,ChartGeo.GeoFeature)}catch(e){}}
  fetch('https://cdn.jsdelivr.net/npm/world-atlas@2.0.2/countries-50m.json').then(function(r){return r.json()}).then(function(world){
    mapDone=true;
    var feats=topojson.feature(world,world.objects.countries).features;
    var byNum={};cs.forEach(function(c){if(c.num!=null)byNum[String(c.num)]=c.installs});
    function ramp(t){var a=[227,223,212],b=[127,82,0];return 'rgb('+Math.round(a[0]+(b[0]-a[0])*t)+','+Math.round(a[1]+(b[1]-a[1])*t)+','+Math.round(a[2]+(b[2]-a[2])*t)+')'}
    mapChart=new Chart(document.getElementById('map'),{type:'choropleth',data:{labels:feats.map(function(f){return f.properties.name}),datasets:[{outline:feats,showOutline:true,outlineBorderColor:C.line,outlineBorderWidth:.4,borderColor:'#d9d4c6',borderWidth:.4,
      backgroundColor:function(ctx){var v=ctx.raw&&ctx.raw.value;if(!v)return '#efeadd';return ramp(Math.min(1,Math.sqrt(v/cmax)))},
      data:feats.map(function(f){return {feature:f,value:byNum[String(f.id)]||0}})}]},
      options:{responsive:true,maintainAspectRatio:false,showGraticule:false,scales:{projection:{axis:'x',projection:'equalEarth'},color:{axis:'x',display:false}},
        plugins:{legend:{display:false},tooltip:{backgroundColor:'#fff',titleColor:C.ink,bodyColor:C.ink,borderColor:C.line,borderWidth:1,callbacks:{label:function(ctx){var v=ctx.raw&&ctx.raw.value;return ctx.chart.data.labels[ctx.dataIndex]+': '+(v||0)+' installs'}}}}}});
  }).catch(function(e){document.querySelector('.mapbox').innerHTML='<p class="note">map unavailable ('+e+'); ranked list at right.</p>'});
}
function devRender(){
  var S=(DATA.series||[]).filter(function(r){return r.mergedPRs!=null||r.mainCommits!=null}).slice(-30);
  var L=S.map(function(r){return r.date.slice(5)});
  var P=S.map(function(r){return r.mergedPRs==null?null:r.mergedPRs});
  var M=S.map(function(r){return r.mainCommits==null?null:r.mainCommits});
  if(window.devChart)window.devChart.destroy();
  window.devChart=new Chart(document.getElementById('dev'),{type:'bar',data:{labels:L,datasets:[{label:'PRs merged',data:P,backgroundColor:'#7a3fb0',borderWidth:0},{label:'commits to main',data:M,backgroundColor:'#1f8a8a',borderWidth:0}]},
    options:{responsive:true,maintainAspectRatio:false,interaction:{mode:'index',intersect:false},scales:{x:{grid:{display:false},ticks:{color:C.mute,font:{size:10},autoSkip:false,maxRotation:60,minRotation:60}},y:{beginAtZero:true,grid:{color:'#ece7db'},ticks:{color:C.mute,font:{size:10}}}},
    plugins:{legend:{display:true,labels:{color:C.ink,font:{size:11}}},tooltip:{backgroundColor:'#fff',titleColor:C.ink,bodyColor:C.ink,borderColor:C.line,borderWidth:1}}}});
  var T=DATA.totals||{};
  document.getElementById('devh').textContent=fmt(T.mergedPRs7d)+' PRs and '+fmt(T.mainCommits7d)+' commits in the last 7 days (UTC days; today is partial)';
  var R=S.slice(-14).reverse();
  document.getElementById('devt').innerHTML='<div class="toprow"><span class="cn"><b>day (UTC)</b></span><span class="cv"><b>PRs merged</b></span><span class="cv" style="min-width:70px"><b>commits</b></span></div>'+R.map(function(r){return '<div class="toprow"><span class="cn">'+r.date+'</span><span class="cv">'+(r.mergedPRs==null?'-':r.mergedPRs)+'</span><span class="cv" style="min-width:70px">'+(r.mainCommits==null?'-':r.mainCommits)+'</span></div>'}).join('');
}
function footer(){document.getElementById('foot').innerHTML='GitHub stars/forks/releases from the API; clones, views and uniques from the traffic API (14-day window, snapshotted '+(DATA.cadence||'daily')+' so history persists). Website installs are the /get request log in R2 — country from Cloudflare geo, no IP, no cookie, no identifier (so no per-visitor dedup). Private dashboard, key-gated.';}
function fetchData(){return fetch('data.json'+KEYQ).then(function(r){return r.json()}).then(function(d){DATA=d;lastFetch=Date.now();render();footer()}).catch(function(e){document.getElementById('upd').textContent='fetch error: '+e})}
fetchData();
document.getElementById('logscale').onchange=function(){var t=this.checked?'logarithmic':'linear';if(tsChart){tsChart.options.scales.yL.type=t;tsChart.options.scales.yR.type=t;tsChart.update();}};document.getElementById('linetog').onchange=function(){var L=this.checked;if(tsChart){tsChart.data.datasets.forEach(function(ds){ds.type=L?'line':'bar';ds.borderWidth=L?2:0;ds.fill=false;ds.tension=L?0.25:0;ds.pointRadius=0;});tsChart.update();}};
setInterval(function(){var f=document.getElementById('fresh');if(f)f.textContent='view refreshed '+ago(lastFetch);document.getElementById('live').textContent='live';},1000);
setInterval(fetchData,30000);
(function(){
  var TW='daily',TD=null,tChart=null;
  function esc(s){return String(s==null?'':s).replace(/[&<>"]/g,function(c){return c=='&'?'&amp;':c=='<'?'&lt;':c=='>'?'&gt;':'&quot;'})}
  function fmtn(n){return (n==null?'-':Number(n).toLocaleString())}
  function mount(){
    if(document.getElementById('trendsec'))return true;
    var mn=document.querySelector('main');if(!mn)return false;
    var sec=document.createElement('section');sec.id='trendsec';
    var h='<div class="sh"><h2>GitHub trending</h2><span class="hint" id="trendh"></span></div>';
    h+='<div style="margin-bottom:14px;font-family:var(--fda);font-size:11px;text-transform:uppercase;letter-spacing:.08em;color:var(--z-mute)">window&nbsp; <span class="twb" data-w="daily">daily</span> &nbsp; <span class="twb" data-w="weekly">weekly</span> &nbsp; <span class="twb" data-w="monthly">monthly</span></div>';
    h+='<div class="grid2"><div class="chartbox" style="height:440px"><canvas id="trendc"></canvas></div><div class="toplist" id="trendlist"></div></div>';
    h+='<p class="note" id="trendfoot" style="margin-top:16px;border:0;padding:0"></p>';
    sec.innerHTML=h;
    var foot=document.getElementById('foot');
    if(foot&&foot.parentNode===mn)mn.insertBefore(sec,foot);else mn.appendChild(sec);
    [].forEach.call(sec.querySelectorAll('.twb'),function(el){el.style.cursor='pointer';el.onclick=function(){TW=el.getAttribute('data-w');paint()}});
    return true;
  }
  function xerjVel(){try{var s=DATA&&DATA.series;if(!s||!s.length)return null;for(var i=s.length-1;i>=0;i--){if(s[i].newStars!=null)return s[i].newStars}}catch(e){}return null}
  function paint(){
    if(!TD)return;if(!mount())return;
    var rows=(TD.windows&&TD.windows[TW])||[];
    var winlabel=TW=='daily'?'today':TW=='weekly'?'this week':'this month';
    [].forEach.call(document.querySelectorAll('.twb'),function(el){var on=el.getAttribute('data-w')==TW;el.style.color=on?'var(--z-accent)':'var(--z-mute)';el.style.fontWeight=on?'700':'400'});
    document.getElementById('trendh').textContent=rows.length+' repos · by stars '+winlabel;
    var html=rows.map(function(r,i){
      var vel=r.velocity?r.velocity.count:null;var nm=esc(r.full);
      return '<div class="toprow"><span class="cv" style="width:22px;text-align:left">'+(i+1)+'</span>'+
        '<a class="cn" href="https://github.com/'+nm+'" target="_blank" rel="noopener" style="color:var(--z-ink);text-decoration:none">'+nm+'</a>'+
        '<span class="cv" title="total stars" style="width:62px">★'+fmtn(r.stars)+'</span>'+
        '<span class="cv" title="stars '+winlabel+'" style="color:var(--z-accent);width:60px">+'+fmtn(vel)+'</span>'+
        '<span class="cv" title="commits pushed, last 7d" style="width:52px">'+(r.pushes7d==null?'—':('↳'+fmtn(r.pushes7d)))+'</span></div>';
    }).join('');
    document.getElementById('trendlist').innerHTML=html||'<p class="note">no trending data yet</p>';
    if(window.Chart){
      var labels=rows.map(function(r){return r.full});
      var vel=rows.map(function(r){return r.velocity?r.velocity.count:0});
      var tot=rows.map(function(r){return r.stars||0});
      if(tChart)tChart.destroy();
      tChart=new Chart(document.getElementById('trendc'),{type:'bar',data:{labels:labels,datasets:[
        {label:'stars '+winlabel,data:vel,backgroundColor:'#7f5200',borderWidth:0,xAxisID:'x'},
        {label:'total stars',data:tot,backgroundColor:'#6f8aa8',borderWidth:0,xAxisID:'x2'}]},
        options:{indexAxis:'y',responsive:true,maintainAspectRatio:false,
          scales:{x:{position:'bottom',beginAtZero:true,grid:{color:'#ece7db',drawTicks:false},ticks:{color:'#7f5200',font:{family:'IBM Plex Sans',size:10}},title:{display:true,text:'stars '+winlabel,color:'#7f5200',font:{size:9}}},
            x2:{position:'top',beginAtZero:true,grid:{display:false},ticks:{color:'#6f8aa8',font:{family:'IBM Plex Sans',size:10}},title:{display:true,text:'total stars',color:'#6f8aa8',font:{size:9}}},
            y:{grid:{display:false},ticks:{color:'#11120f',font:{family:'IBM Plex Sans',size:11}}}},
          plugins:{legend:{display:true,position:'bottom',labels:{boxWidth:10,font:{size:10},color:'#696762'}},tooltip:{backgroundColor:'#fff',titleColor:'#11120f',bodyColor:'#11120f',borderColor:'#cfcbbf',borderWidth:1}}}});
    }
    var floor=TD.floor_daily,xv=xerjVel();
    var ft='Scraped from github.com/trending ('+TW+'); stars-'+winlabel+' is GitHub’s own velocity number, commits-pushed is the last-7-day commit count on the default branch. ';
    if(floor!=null)ft+='Entry bar today ≈ '+fmtn(floor)+' stars/day (lowest velocity on the daily board)';
    if(floor!=null&&xv!=null)ft+='; xerj is at '+fmtn(xv)+' new stars/day';
    ft+='. ';
    if(TD.momentum)ft+='Momentum scan: top '+fmtn((TD.momentum.items||[]).length)+' of '+fmtn(TD.momentum.total)+' repos created in the last 30 days. ';
    ft+='Snapshot '+((TD.generated||'').slice(0,16).replace('T',' '))+' UTC.';
    if(TD.capped)ft+=' ('+TD.capped+' repos past the '+TD.cap+'-repo push-enrichment cap show no commit count.)';
    document.getElementById('trendfoot').innerHTML=ft;
  }
  function pull(){fetch('data.json'+location.search).then(function(r){return r.json()}).then(function(d){if(d&&d.trending){TD=d.trending;paint()}}).catch(function(){})}
  pull();setInterval(pull,60000);
})();
(function(){
  function snapshotFull(){try{if(window.tsChart&&tsChart.data){window._tsFull={labels:tsChart.data.labels.slice(),data:tsChart.data.datasets.map(function(d){return (d.data||[]).slice()})}}}catch(e){}}
  function applyTsRange(){
    try{
      if(!window._tsFull||!window.tsChart)return;
      var el=document.getElementById('tsrange');var v=el?el.value:'all';
      var full=window._tsFull,n=full.labels.length;
      var keep=(v=='all')?n:Math.min(n,parseInt(v,10)||n);
      var start=Math.max(0,n-keep);
      tsChart.data.labels=full.labels.slice(start);
      tsChart.data.datasets.forEach(function(ds,i){ds.data=(full.data[i]||[]).slice(start)});
      tsChart.update();
    }catch(e){}
  }
  function mountRange(){
    var ts=document.getElementById('ts');if(!ts||!ts.closest)return false;
    var sec=ts.closest('section');if(!sec)return false;
    if(document.getElementById('tsrange'))return true;
    var sh=sec.querySelector('.sh');if(!sh)return false;
    var sel=document.createElement('select');sel.id='tsrange';
    sel.innerHTML='<option value="7">last 7 days</option><option value="14">14 days</option><option value="30" selected>30 days</option><option value="90">90 days</option><option value="all">all time</option>';
    sel.style.cssText='font-family:var(--fda);font-size:11px;text-transform:uppercase;letter-spacing:.08em;color:var(--z-mute);background:var(--z-bg);border:1px solid var(--z-line);padding:4px 8px;cursor:pointer;margin-left:auto';
    sel.onchange=applyTsRange;sh.appendChild(sel);return true;
  }
  if(typeof render==='function'){var _o=render;render=function(){_o.apply(this,arguments);mountRange();snapshotFull();applyTsRange()}}
  setTimeout(function(){mountRange();snapshotFull();applyTsRange()},1500);
})();
</script></body></html>`;

export default {
  async scheduled(event, env, ctx) {
    ctx.waitUntil(snapshot(env));
  },
  async fetch(req, env) {
    const url = new URL(req.url);
    if (!env.REFRESH_KEY || url.searchParams.get("key") !== env.REFRESH_KEY)
      return new Response("forbidden - private metrics endpoint", { status: 403 });
    if (url.pathname === "/data.json") {
      const m = await liveUpdate(env);
      return new Response(JSON.stringify(m), { headers: { "content-type": "application/json", "cache-control": "no-store" } });
    }
    if (url.pathname === "/refresh") {
      const m = await snapshot(env);
      return new Response(JSON.stringify({ ok: true, days: (m.series || []).length, updated: m.updated }), { headers: { "content-type": "application/json" } });
    }
    if (url.pathname === "/trending-history") {
      const date = url.searchParams.get("date");
      if (date && /^\d{4}-\d{2}-\d{2}$/.test(date)) {
        const o2 = await env.INSTALLS.get("trending/" + date + ".json");
        return new Response(o2 ? await o2.text() : "{}", { headers: { "content-type": "application/json", "cache-control": "no-store" } });
      }
      const list = await env.INSTALLS.list({ prefix: "trending/" });
      return new Response(JSON.stringify({ dates: (list.objects || []).map((o3) => o3.key.slice(9).replace(/\.json$/, "")) }), { headers: { "content-type": "application/json" } });
    }
    return new Response(DASH, { headers: { "content-type": "text/html; charset=utf-8" } });
  }
};
