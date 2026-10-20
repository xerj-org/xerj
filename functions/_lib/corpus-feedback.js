// Pure helpers for /api/corpus-feedback (functions/api/corpus-feedback.js):
// validation and normalisation of one agent review, the ISO week a vote lands
// in, and the public aggregate (Bayesian agent score, found rate, freshness,
// k-thresholded coverage gaps, problem counts). No I/O here, so the Node tests
// (tools/corpus-hub-site/test_corpus_feedback.mjs) exercise it directly.

export const LIMITS = {
  pack: 64, family: 32, version: 40, template: 160, gap: 60, gaps: 5, note: 280,
};
export const FOUND = ['yes', 'partial', 'no'];
export const PROBLEMS = ['error', 'slow', 'stale', 'wrong'];
export const SOURCES = ['hub-pull', 'mcp', 'agent', 'cli'];

// Bayesian mean: (PRIOR_WEIGHT * PRIOR_MEAN + sum) / (PRIOR_WEIGHT + n).
// A corpus with two 5/5 reviews reads 3.57, not 5.0; it takes many agreeing
// reviews to move far from the neutral 3.
export const PRIOR_MEAN = 3;
export const PRIOR_WEIGHT = 5;
export const K_MIN = 3;             // a coverage gap / family mean is shown only with >= 3 distinct reviewers
export const WINDOW_WEEKS = 26;     // aggregate over the last ~6 months of votes

const SLUG_RE = /^[a-z0-9][a-z0-9._-]{0,99}$/;
const CTRL = /[\u0000-\u001f\u007f]/g;

const clean = (v, cap) => String(v).replace(CTRL, ' ').replace(/\s+/g, ' ').trim().slice(0, cap);
const intIn = (v, lo, hi) => (Number.isInteger(v) && v >= lo && v <= hi ? v : null);

// Words a query template may keep verbatim: question/structure words and
// generic programming nouns. Every other word becomes <term>, numbers <num>,
// quoted strings <str>, URLs/paths/emails <ref>. So even a client that sends a
// raw query by mistake cannot store it: only its shape survives.
const KEEP = new Set(('how what why where when which who is are do does can should to of in on for with without from by the a an and or not ' +
  'vs versus between example examples usage use using define definition def function method class type struct trait interface module ' +
  'api error errors config configuration option options flag flags default defaults install setup version versions migrate migration ' +
  'deprecated test tests rule rules section clause spec standard requirement requirements limit limits case cases list find get set ' +
  'call calls return returns implement implementation behaviour behavior meaning difference').split(' '));

export function normaliseTemplate(raw) {
  if (typeof raw !== 'string') return '';
  let s = clean(raw, 400).toLowerCase();
  s = s.replace(/"[^"]*"|'[^']*'|`[^`]*`/g, ' <str> ')
       .replace(/\b[\w.+-]+@[\w-]+\.[\w.-]+\b/g, ' <ref> ')
       .replace(/\b(?:https?:\/\/|www\.)\S+/g, ' <ref> ')
       .replace(/(?:^|\s)(?:[\w.-]*[\/\\][\w.\/\\-]*)/g, ' <ref> ');
  const out = [];
  for (const tok of s.split(/\s+/)) {
    if (!tok) continue;
    const ph = tok.match(/^[<{\[]([a-z_]{1,24})[>}\]]$/);       // <x>, {x}, [x]
    let t;
    if (ph) t = `<${ph[1]}>`;
    else if (/^[?:()=*+-]$/.test(tok)) t = tok;
    else {
      const w = tok.replace(/^[^\w<]+|[^\w>]+$/g, '');
      if (!w) continue;
      if (/^<[a-z_]{1,24}>$/.test(w)) t = w;
      else if (/^\d[\d.,]*$/.test(w)) t = '<num>';
      else t = KEEP.has(w) ? w : '<term>';
    }
    if (t === '<term>' && out[out.length - 1] === '<term>') continue;   // collapse runs
    out.push(t);
  }
  return out.join(' ').slice(0, LIMITS.template).trim();
}

export function normaliseGap(raw) {
  if (typeof raw !== 'string') return '';
  return clean(raw, 200).toLowerCase().normalize('NFKC')
    .replace(/[^\p{L}\p{N}\s.+#/-]/gu, ' ').replace(/\s+/g, ' ').trim().slice(0, LIMITS.gap);
}

export function normaliseFamily(raw) {
  const f = clean(raw || '', 64).toLowerCase().replace(/[^a-z0-9.-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, LIMITS.family);
  return f || 'unknown';
}

// Validate one POST body. Returns { ok:true, review } or { ok:false, error }.
export function validateReview(body) {
  if (!body || typeof body !== 'object' || Array.isArray(body)) return { ok: false, error: 'body must be a JSON object' };
  const corpus = typeof body.corpus === 'string' ? body.corpus.trim().toLowerCase() : '';
  if (!SLUG_RE.test(corpus)) return { ok: false, error: 'corpus: a hub corpus slug is required' };
  const source = typeof body.source === 'string' && SOURCES.includes(body.source) ? body.source : 'agent';
  const basis = body.basis === 'auto' ? 'auto' : 'agent';
  // A client's automatic note right after `xerj hub pull` knows only whether the
  // pull worked: it may omit usefulness/found. It never counts toward the score.
  const pullOnly = basis === 'auto' && source === 'hub-pull' && body.usefulness == null && body.found == null;
  let usefulness = null; let found = null;
  if (!pullOnly) {
    usefulness = intIn(body.usefulness, 1, 5);
    if (usefulness === null) return { ok: false, error: 'usefulness: integer 1-5 is required' };
    found = typeof body.found === 'string' ? body.found.toLowerCase() : '';
    if (!FOUND.includes(found)) return { ok: false, error: 'found: one of yes, partial, no is required' };
  }
  let freshness = null;
  if (body.freshness !== undefined && body.freshness !== null) {
    freshness = intIn(body.freshness, 1, 5);
    if (freshness === null) return { ok: false, error: 'freshness: integer 1-5' };
  }
  const agent = body.agent && typeof body.agent === 'object' ? body.agent : {};
  const family = normaliseFamily(typeof agent.family === 'string' ? agent.family : body.agent_family);
  const agentVersion = clean(typeof agent.version === 'string' ? agent.version : (body.agent_version || ''), LIMITS.version);
  const pack = clean(typeof body.pack === 'string' ? body.pack : (body.pin || ''), LIMITS.pack).replace(/[^A-Za-z0-9._:+@\/-]/g, '');

  let gaps = [];
  if (body.coverage_gaps !== undefined && body.coverage_gaps !== null) {
    if (!Array.isArray(body.coverage_gaps)) return { ok: false, error: 'coverage_gaps: array of short strings' };
    gaps = [...new Set(body.coverage_gaps.map(normaliseGap).filter((g) => g.length >= 2))].slice(0, LIMITS.gaps);
  }
  let kinds = []; let note = '';
  const p = body.problems;
  if (p !== undefined && p !== null) {
    const list = Array.isArray(p) ? p : (typeof p === 'object' ? (p.kinds || p.types || []) : null);
    if (!Array.isArray(list)) return { ok: false, error: 'problems: {kinds:[error|slow|stale|wrong], note}' };
    kinds = [...new Set(list.map((k) => String(k).toLowerCase()))].filter((k) => PROBLEMS.includes(k));
    if (!Array.isArray(p) && typeof p.note === 'string') note = clean(p.note, LIMITS.note);
  }
  let installId = '';
  if (typeof body.install_id === 'string' && body.install_id) {
    if (!/^[A-Za-z0-9_-]{16,64}$/.test(body.install_id)) return { ok: false, error: 'install_id: 16-64 chars of [A-Za-z0-9_-] (a random local id)' };
    installId = body.install_id;
  }
  return {
    ok: true,
    review: {
      corpus, pack, family, agent_version: agentVersion, usefulness, found, freshness,
      query_template: normaliseTemplate(body.query_template), coverage_gaps: gaps,
      problems: kinds, problem_note: note, source, basis, install_id: installId,
    },
  };
}

// ISO-8601 week, e.g. "2026-W43". Zero-padded so keys sort chronologically.
export function isoWeek(date = new Date()) {
  const d = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate()));
  const day = d.getUTCDay() || 7;
  d.setUTCDate(d.getUTCDate() + 4 - day);
  const y = d.getUTCFullYear();
  const wk = Math.ceil(((d - Date.UTC(y, 0, 1)) / 86400000 + 1) / 7);
  return `${y}-W${String(wk).padStart(2, '0')}`;
}
export function weekCutoff(now = new Date(), weeks = WINDOW_WEEKS) {
  return isoWeek(new Date(now.getTime() - weeks * 7 * 86400000));
}

// slow and stale share an initial, so each problem kind has its own one-letter code.
const P_CODE = { error: 'e', slow: 'l', stale: 't', wrong: 'w' };
const P_BY_CODE = Object.fromEntries(Object.entries(P_CODE).map(([k, c]) => [c, k]));

// Compact form kept in R2 customMetadata so the aggregate needs only LIST calls.
export function toMeta(r, ts) {
  return {
    u: r.usefulness ? String(r.usefulness) : '', f: r.found ? r.found[0] : '', r: r.freshness ? String(r.freshness) : '',
    b: r.basis === 'auto' ? 'u' : 'a',
    a: r.family, g: r.coverage_gaps.join('|').slice(0, 320), p: r.problems.map((k) => P_CODE[k]).join(''),
    t: String(ts),
  };
}

export function fromMeta(m) {
  return {
    usefulness: m.u ? parseInt(m.u, 10) : null, found: ({ y: 'yes', p: 'partial', n: 'no' })[m.f] || null,
    basis: m.b === 'u' ? 'auto' : 'agent',
    freshness: m.r ? parseInt(m.r, 10) : null, family: m.a || 'unknown',
    coverage_gaps: m.g ? m.g.split('|').filter(Boolean) : [],
    problems: (m.p || '').split('').map((c) => P_BY_CODE[c]).filter(Boolean),
    t: parseInt(m.t, 10) || 0,
  };
}

export const bayes = (sum, n, m = PRIOR_MEAN, c = PRIOR_WEIGHT) => (c * m + sum) / (c + n);
export function confidence(n) {
  if (n === 0) return 'none';
  if (n < K_MIN) return 'very low';
  if (n < 10) return 'low';
  if (n < 30) return 'medium';
  return 'high';
}
export function median(xs) {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b); const h = s.length >> 1;
  return s.length % 2 ? s[h] : (s[h - 1] + s[h]) / 2;
}
const r2 = (x) => Math.round(x * 100) / 100;

// votes: [{week, voter, ...fromMeta}] for ONE corpus. flagged: Set of voter ids
// whose volume was abnormal (see the write path). One voice per voter: its
// latest vote in the window counts; flagged voters are dropped.
export function aggregate(votes, { flagged = new Set(), cutoff = '' } = {}) {
  const latest = new Map();      // voter -> latest scored review
  const pulls = new Map();       // voter -> latest pull-only note (no usefulness)
  const droppedVoters = new Set();
  const newer = (a, b) => !b || a.week > b.week || (a.week === b.week && a.t > b.t);
  for (const v of votes) {
    if (cutoff && v.week < cutoff) continue;
    if (flagged.has(v.voter)) { droppedVoters.add(v.voter); continue; }
    if (v.usefulness == null) { if (newer(v, pulls.get(v.voter))) pulls.set(v.voter, v); continue; }
    if (!Number.isInteger(v.usefulness) || v.usefulness < 1 || v.usefulness > 5) continue;
    if (newer(v, latest.get(v.voter))) latest.set(v.voter, v);
  }
  const dropped = droppedVoters.size;
  const rs = [...latest.values()];
  const n = rs.length;
  const sum = rs.reduce((a, v) => a + v.usefulness, 0);
  const out = {
    reviews: n,
    score: n ? r2(bayes(sum, n)) : null,
    raw_mean: n ? r2(sum / n) : null,
    confidence: confidence(n),
    prior: { mean: PRIOR_MEAN, weight: PRIOR_WEIGHT },
    found: { yes: 0, partial: 0, no: 0, rate: null },
    freshness_median: median(rs.map((v) => v.freshness).filter((x) => Number.isInteger(x))),
    families: [], coverage_gaps: [],
    problems: Object.fromEntries(PROBLEMS.map((k) => [k, 0])),
    agent_reviews: rs.filter((v) => v.basis !== 'auto').length,
    pull_reports: pulls.size,
    dropped_outliers: dropped, k_min: K_MIN, window_weeks: WINDOW_WEEKS,
  };
  for (const v of rs) out.found[v.found] = (out.found[v.found] || 0) + 1;
  // Problems: each voter counts once per kind, from its latest review and its latest pull note.
  for (const voter of new Set([...latest.keys(), ...pulls.keys()])) {
    const kinds = new Set([...((latest.get(voter) || {}).problems || []), ...((pulls.get(voter) || {}).problems || [])]);
    for (const k of kinds) if (k in out.problems) out.problems[k]++;
  }
  if (n) out.found.rate = r2((out.found.yes + 0.5 * out.found.partial) / n);
  const fam = new Map();
  for (const v of rs) { const f = fam.get(v.family) || { n: 0, sum: 0 }; f.n++; f.sum += v.usefulness; fam.set(v.family, f); }
  let other = 0;
  for (const [family, f] of [...fam.entries()].sort((a, b) => b[1].n - a[1].n || (a[0] < b[0] ? -1 : 1))) {
    if (f.n >= K_MIN) out.families.push({ family, reviews: f.n, score: r2(bayes(f.sum, f.n)) });
    else other += f.n;
  }
  if (other) out.families.push({ family: 'other', reviews: other, score: null });
  const gaps = new Map();
  for (const v of rs) for (const g of new Set(v.coverage_gaps.map(normaliseGap).filter(Boolean))) gaps.set(g, (gaps.get(g) || 0) + 1);
  out.coverage_gaps = [...gaps.entries()].filter(([, c]) => c >= K_MIN)
    .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)).slice(0, 10).map(([gap, reviews]) => ({ gap, reviews }));
  return out;
}

// Parse an R2 vote key: v/<slug>/<week>/<voter>
export function parseVoteKey(key) {
  const m = /^v\/([^/]+)\/(\d{4}-W\d{2})\/([^/]+)$/.exec(key);
  return m ? { corpus: m[1], week: m[2], voter: m[3] } : null;
}
