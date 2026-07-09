// compare.mjs — the COMPARISON HARNESS for the doc-index use case.
//
// Scores TWO retrieval approaches over the same ground-truth query set
// (queries.json) and emits a measured, honest scorecard:
//
//   * XERJ         — Claude + XERJ. Queries a LIVE XERJ node (ES-compat REST) with a
//                    HYBRID query (BM25 match on `body_text` + semantic on `body`,
//                    fused with RRF). If the live node rejects the hybrid shape, it
//                    falls back to running the semantic and match queries separately
//                    and fusing them client-side with RRF. size=5.
//   * Baseline     — "Claude Code with only shell scripts". Delegates to
//                    grep-baseline.mjs's scoreQuery() (ripgrep over the RAW corpus).
//                    Binary PDF/DOCX are invisible to rg; literal-only, no ranking.
//
// A query "hits" for an approach iff any returned passage/line contains the query's
// ground-truth `expect_substring` (case-insensitive).
//
// Outputs (both MEASURED from this live run — nothing fabricated):
//   * results.json  — machine-readable rows + aggregates + gate result.
//   * SCORECARD.md  — human-readable per-query table, aggregate table, honest verdict.
//
// Honesty caveats carried into the verdict (per SPEC.md, non-negotiable):
//   * XERJ's built-in embedder is LEXICAL feature-hashing (384-dim cosine), NOT neural.
//     "Semantic" wins here are word/sub-word overlap, not deep meaning.
//   * kNN is EXACT brute-force at query time (fine at this corpus size).
//
// Exit codes:
//   0  — scored successfully (WHETHER OR NOT the gate passed; a gate failure is reported
//        honestly in results.json/SCORECARD.md, never massaged).
//   1  — could not run: XERJ node unreachable, `docfolder` index missing, queries.json
//        or grep-baseline.mjs missing/broken. The runner must notice these.
//
// `node --check compare.mjs` is clean. Pure Node (v18+); no external deps.

import { readFileSync, writeFileSync, existsSync, statSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join, isAbsolute } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));

// ── Configuration (all overridable via env so a runner can stage things) ──────────────
const PORT = process.env.XERJ_PORT || '9209';
const BASE = process.env.XERJ_URL || `http://localhost:${PORT}`;
const INDEX = process.env.DOC_INDEX_INDEX || 'docfolder';
const SIZE = 5; // top-K passages per query (SPEC-fixed)
const RRF_K = 60; // reciprocal-rank-fusion constant (matches XERJ's server-side RRF default)
const REQ_TIMEOUT_MS = Number(process.env.DOC_INDEX_TIMEOUT_MS || 20000);

const QUERIES_PATH = resolvePath(process.env.DOC_INDEX_QUERIES, join(HERE, 'queries.json'));
const BASELINE_PATH = join(HERE, 'grep-baseline.mjs');
const RESULTS_PATH = join(HERE, 'results.json');
const SCORECARD_PATH = join(HERE, 'SCORECARD.md');
const INDEX_BUILD_FILE = join(HERE, '.index_build_ms');
// The real corpus dir the baseline greps (staged elsewhere via DOC_INDEX_CORPUS, or the
// co-located ./corpus). Used to resolve an answer_path to the SAME file on disk the baseline
// reads, so the honest context charge stats the right bytes.
const CORPUS_DIR = resolvePath(process.env.DOC_INDEX_CORPUS, join(HERE, 'corpus'));

function resolvePath(envVal, fallback) {
  if (!envVal) return fallback;
  return isAbsolute(envVal) ? envVal : join(HERE, envVal);
}

// Resolve a queries.json answer_path ("corpus/hr/handbook.pdf") to the absolute path of the
// real file on disk, honoring a staged corpus dir (DOC_INDEX_CORPUS) so the honest baseline
// charge stats the SAME file the baseline greps.
function answerAbsPath(answerPath) {
  if (!answerPath || typeof answerPath !== 'string') return null;
  if (isAbsolute(answerPath)) return answerPath;
  const norm = answerPath.replace(/^\.[\\/]/, '').replace(/\\/g, '/');
  if (norm.startsWith('corpus/')) return join(CORPUS_DIR, norm.slice('corpus/'.length));
  return join(HERE, answerPath);
}

// HONEST baseline context charge (SPEC REVISION 2 audit fix): the size of the SINGLE
// answer-containing file the baseline must open to read the answer. Returns the file's byte
// size (0 if unreadable/missing). The caller charges this ONLY when the baseline actually
// hits — else 0, because it cannot answer and so reads nothing toward an answer.
function answerFileBytes(answerPath) {
  const abs = answerAbsPath(answerPath);
  if (!abs) return 0;
  try {
    return statSync(abs).size;
  } catch {
    return 0; // file vanished/unreadable — charge nothing rather than crash
  }
}

function fatal(msg, code = 1) {
  process.stderr.write(`[compare] FATAL: ${msg}\n`);
  process.exit(code);
}

// ── XERJ query bodies (shapes verified against xerj-query/src/parser.rs) ──────────────
//
// Hybrid: fan out a BM25 match on the lexical `body_text` field and a `semantic` kNN on
// the auto-embedded `body` field, then fuse with RRF server-side.
function hybridBody(text) {
  return {
    size: SIZE,
    query: {
      hybrid: {
        queries: [
          { query: { match: { body_text: text } } },
          { query: { semantic: { field: 'body', query: text, k: SIZE } } },
        ],
        fusion: 'rrf',
      },
    },
  };
}
function semanticBody(text) {
  return { size: SIZE, query: { semantic: { field: 'body', query: text, k: SIZE } } };
}
function matchBody(text) {
  return { size: SIZE, query: { match: { body_text: text } } };
}

// ── Low-level: POST a search body, return {ok, status, json, latencyMs, networkError} ──
async function esSearch(body) {
  const start = performance.now();
  let res;
  try {
    res = await fetch(`${BASE}/${INDEX}/_search`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(REQ_TIMEOUT_MS),
    });
  } catch (err) {
    return { ok: false, networkError: err, latencyMs: performance.now() - start };
  }
  const latencyMs = performance.now() - start;
  const raw = await res.text();
  let json = null;
  try {
    json = JSON.parse(raw);
  } catch {
    /* non-JSON body (e.g. a plain error); leave json null */
  }
  return { ok: res.ok, status: res.status, json, raw, latencyMs };
}

function hitsFrom(json) {
  const h = json && json.hits && json.hits.hits;
  return Array.isArray(h) ? h : [];
}

// A passage's text — prefer the lexical chunk (`body_text`), then the semantic `body`,
// then the whole _source. Used both for byte-cost accounting and the substring check.
function passageText(hit) {
  const s = (hit && hit._source) || {};
  if (typeof s.body_text === 'string') return s.body_text;
  if (typeof s.body === 'string') return s.body;
  return JSON.stringify(s);
}

// Byte length of the SINGLE most-relevant returned passage: the first returned hit whose
// text actually contains the expected answer substring (the passage the agent would quote),
// else the top-ranked hit. Used to make the large_literal context win vivid (one passage
// vs. a whole ≥60 KB file). Returns 0 when there are no hits.
function bestPassageBytes(hits, expectLc) {
  if (!hits.length) return 0;
  let best = hits[0];
  if (expectLc) {
    const m = hits.find((h) => passageText(h).toLowerCase().includes(expectLc));
    if (m) best = m;
  }
  return Buffer.byteLength(passageText(best), 'utf8');
}

// Everything a returned hit exposes, concatenated, for the case-insensitive
// expect_substring test (body_text + body + highlight + full _source).
function searchableBlob(hit) {
  const s = (hit && hit._source) || {};
  const parts = [];
  if (typeof s.body_text === 'string') parts.push(s.body_text);
  if (typeof s.body === 'string') parts.push(s.body);
  if (hit && hit.highlight) parts.push(JSON.stringify(hit.highlight));
  parts.push(JSON.stringify(s));
  return parts.join('\n');
}

function citePaths(hits) {
  const seen = [];
  for (const h of hits) {
    const s = (h && h._source) || {};
    const p = s.path || s.dir || null;
    if (p && !seen.includes(p)) seen.push(p);
  }
  return seen;
}

// Client-side RRF over two ranked hit lists (used only in the fallback path).
function rrfMerge(listA, listB, k = RRF_K, size = SIZE) {
  const acc = new Map(); // id -> { score, hit }
  for (const list of [listA, listB]) {
    list.forEach((hit, i) => {
      const id = (hit && hit._id) || JSON.stringify(hit && hit._source);
      const rank = i + 1;
      const cur = acc.get(id) || { score: 0, hit };
      cur.score += 1 / (k + rank);
      if (!cur.hit) cur.hit = hit;
      acc.set(id, cur);
    });
  }
  return [...acc.values()].sort((a, b) => b.score - a.score).slice(0, size).map((x) => x.hit);
}

// Retrieval mode is decided once, on first use, and reused for consistent latency.
//   null -> undecided; 'hybrid' -> server-side RRF works; 'fallback' -> client-side RRF.
let RETRIEVAL_MODE = null;
let SEMANTIC_ERRORED = false; // note if the semantic sub-query itself failed (fallback degrades to lexical)

/**
 * Retrieve top-SIZE passages for `text` from the live XERJ node.
 * @returns {{hits: object[], latencyMs: number, mode: string, error: string|null}}
 * @throws the underlying network error if the node is unreachable (caller treats as fatal).
 */
async function xerjRetrieve(text) {
  // Try the hybrid shape first (unless we've already learned it's unsupported).
  if (RETRIEVAL_MODE === null || RETRIEVAL_MODE === 'hybrid') {
    const r = await esSearch(hybridBody(text));
    if (r.networkError) throw r.networkError;
    if (r.ok && r.json && !r.json.error) {
      RETRIEVAL_MODE = 'hybrid';
      return { hits: hitsFrom(r.json).slice(0, SIZE), latencyMs: r.latencyMs, mode: 'hybrid', error: null };
    }
    // Hybrid rejected by this node — fall back for this and all subsequent queries.
    if (RETRIEVAL_MODE === null) {
      const why = (r.json && r.json.error && (r.json.error.reason || JSON.stringify(r.json.error))) || `HTTP ${r.status}`;
      process.stderr.write(`[compare] hybrid query not supported by node (${why}); falling back to client-side RRF of semantic+match.\n`);
    }
    RETRIEVAL_MODE = 'fallback';
  }

  // Fallback: run semantic + match separately, fuse client-side with RRF.
  const [rs, rm] = await Promise.all([esSearch(semanticBody(text)), esSearch(matchBody(text))]);
  if (rs.networkError) throw rs.networkError;
  if (rm.networkError) throw rm.networkError;

  let error = null;
  const semHits = rs.ok && rs.json && !rs.json.error ? hitsFrom(rs.json) : [];
  const matchHits = rm.ok && rm.json && !rm.json.error ? hitsFrom(rm.json) : [];
  if (!(rs.ok && rs.json && !rs.json.error)) {
    SEMANTIC_ERRORED = true;
    error = 'semantic sub-query failed; scoring lexical-only';
  }
  if (!(rm.ok && rm.json && !rm.json.error) && semHits.length === 0) {
    error = 'both semantic and match sub-queries failed';
  }
  const merged = rrfMerge(semHits, matchHits);
  return { hits: merged.slice(0, SIZE), latencyMs: rs.latencyMs + rm.latencyMs, mode: 'fallback', error };
}

// ── Preflight: is the node up and does the index exist? (fatal, non-zero, if not) ─────
async function preflight() {
  try {
    await fetch(`${BASE}/`, { signal: AbortSignal.timeout(5000) });
  } catch (err) {
    fatal(`cannot reach XERJ node at ${BASE} (${err.message}). Is the node running with --insecure on port ${PORT}?`);
  }
  let countRes;
  try {
    countRes = await fetch(`${BASE}/${INDEX}/_count`, { signal: AbortSignal.timeout(5000) });
  } catch (err) {
    fatal(`cannot reach XERJ node at ${BASE} (${err.message}).`);
  }
  if (countRes.status === 404) {
    fatal(`index '${INDEX}' not found at ${BASE}. Run xerj-index.mjs to build it first.`);
  }
  let docCount = null;
  try {
    const cj = JSON.parse(await countRes.text());
    docCount = typeof cj.count === 'number' ? cj.count : null;
  } catch {
    /* ignore */
  }
  return { docCount };
}

// ── index_build_ms: env INDEX_BUILD_MS / --index-build-ms=N / file .index_build_ms ────
function readIndexBuildMs() {
  if (process.env.INDEX_BUILD_MS && !Number.isNaN(Number(process.env.INDEX_BUILD_MS))) {
    return Number(process.env.INDEX_BUILD_MS);
  }
  const arg = process.argv.find((a) => a.startsWith('--index-build-ms='));
  if (arg) {
    const n = Number(arg.split('=')[1]);
    if (!Number.isNaN(n)) return n;
  }
  if (existsSync(INDEX_BUILD_FILE)) {
    const n = Number(readFileSync(INDEX_BUILD_FILE, 'utf8').trim());
    if (!Number.isNaN(n)) return n;
  }
  return null;
}

// ── Small numeric helpers ─────────────────────────────────────────────────────────────
function pct(n, d) {
  return d === 0 ? 0 : (n / d) * 100;
}
function round(n, dp = 1) {
  if (n === null || n === undefined || Number.isNaN(n)) return null;
  const f = 10 ** dp;
  return Math.round(n * f) / f;
}
function median(nums) {
  if (!nums.length) return null;
  const s = [...nums].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}
function coverage(rows, key) {
  const total = rows.length;
  const hits = rows.filter((r) => r[key]).length;
  return { total, hits, pct: round(pct(hits, total)) };
}

// ── Main ──────────────────────────────────────────────────────────────────────────────
async function main() {
  // Load inputs (any missing → fatal non-zero).
  if (!existsSync(QUERIES_PATH)) fatal(`queries.json not found at ${QUERIES_PATH}. Generate it with gen-corpus.mjs.`);
  let queries;
  try {
    queries = JSON.parse(readFileSync(QUERIES_PATH, 'utf8'));
  } catch (err) {
    fatal(`could not parse queries.json (${err.message}).`);
  }
  if (!Array.isArray(queries) || queries.length === 0) fatal('queries.json is empty or not an array.');

  if (!existsSync(BASELINE_PATH)) fatal(`grep-baseline.mjs not found at ${BASELINE_PATH}.`);
  let scoreQuery;
  try {
    const mod = await import(pathToFileURL(BASELINE_PATH).href);
    scoreQuery = mod.scoreQuery || (mod.default && mod.default.scoreQuery) || mod.default;
  } catch (err) {
    fatal(`could not import grep-baseline.mjs (${err.message}).`);
  }
  if (typeof scoreQuery !== 'function') fatal('grep-baseline.mjs must export a scoreQuery() function.');

  // Node + index must be live.
  const { docCount } = await preflight();

  // Score every query.
  const rows = [];
  for (const q of queries) {
    const expect = String(q.expect_substring || '');
    const expectLc = expect.toLowerCase();
    const question = String(q.question || '');

    // XERJ retrieval (network error here is fatal — the runner must notice).
    let xerj;
    try {
      xerj = await xerjRetrieve(question);
    } catch (err) {
      fatal(`lost connection to XERJ node mid-run (${err.message}).`);
    }
    const xerjBlobs = xerj.hits.map(searchableBlob);
    const xerjHit = expectLc !== '' && xerjBlobs.some((b) => b.toLowerCase().includes(expectLc));
    const xerjReturnedBytes = xerj.hits.reduce((sum, h) => sum + Buffer.byteLength(passageText(h), 'utf8'), 0);
    const xerjBestPassageBytes = bestPassageBytes(xerj.hits, expectLc);

    // Baseline retrieval (grep-baseline.scoreQuery is synchronous).
    let base = { hits: [], bytesToRead: 0 };
    try {
      const r = scoreQuery(q);
      if (r && typeof r === 'object') base = r;
    } catch (err) {
      process.stderr.write(`[compare] baseline scoreQuery failed for ${q.id}: ${err.message}\n`);
    }
    const baseLines = Array.isArray(base.hits) ? base.hits.map((h) => String((h && h.text) ?? h ?? '')) : [];
    const baselineHit = expectLc !== '' && baseLines.some((t) => t.toLowerCase().includes(expectLc));

    // HONEST baseline context charge (SPEC REVISION 2): the size of the SINGLE answer-
    // containing file the baseline must open to read the answer, and ONLY when the baseline
    // actually hits — else 0 (it cannot answer, so it reads nothing toward an answer). We do
    // NOT charge the false-positive files its broad terms also matched (that inflated every
    // ratio to a flat ~52× and broke the scale-dependence claim — the defect this fixes).
    const answerBytes = answerFileBytes(q.answer_path);
    const baselineContextBytes = baselineHit ? answerBytes : 0;
    // DIAGNOSTIC ONLY — grep-baseline's bytesToRead: the sum of EVERY file the broad terms
    // matched (false-positive files included). Kept for transparency; NEVER a charge/claim.
    const baselineAllMatchedBytes = Number(
      base.bytesToRead ?? base.bytes_to_read ?? base.baseline_bytes_to_read ?? base.bytes ?? 0,
    ) || 0;

    rows.push({
      id: q.id,
      question,
      match_type: q.match_type || 'unknown',
      answer_format: q.answer_format || 'unknown',
      answer_path: q.answer_path || null,
      expect_substring: expect,
      xerj_hit: xerjHit,
      baseline_hit: baselineHit,
      xerj_latency_ms: round(xerj.latencyMs, 2),
      xerj_returned_bytes: xerjReturnedBytes,
      xerj_best_passage_bytes: xerjBestPassageBytes,
      answer_file_bytes: answerBytes, // size of the answer-containing file (regardless of hit)
      baseline_context_bytes: baselineContextBytes, // HONEST charge: answer file size if hit, else 0
      baseline_all_matched_bytes: baselineAllMatchedBytes, // DIAGNOSTIC: every matched file (FP incl.)
      xerj_returned: xerj.hits.length,
      xerj_cite_paths: citePaths(xerj.hits),
      xerj_error: xerj.error,
    });
  }

  // ── Aggregate ──────────────────────────────────────────────────────────────────────
  const N = rows.length;
  const xerjCov = coverage(rows, 'xerj_hit');
  const baseCov = coverage(rows, 'baseline_hit');

  // REVISION 2 canonical match types (SPEC §Query set). Any other type found in
  // queries.json is still aggregated, appended after these.
  const MATCH_TYPES = ['binary_only', 'large_literal', 'robustness', 'literal'];
  const perMatchType = {};
  for (const mt of MATCH_TYPES.concat(
    [...new Set(rows.map((r) => r.match_type))].filter((m) => !MATCH_TYPES.includes(m)),
  )) {
    const sub = rows.filter((r) => r.match_type === mt);
    if (!sub.length) continue;
    perMatchType[mt] = {
      total: sub.length,
      xerj_hits: sub.filter((r) => r.xerj_hit).length,
      baseline_hits: sub.filter((r) => r.baseline_hit).length,
      xerj_only: sub.filter((r) => r.xerj_hit && !r.baseline_hit).length, // "wins"
      xerj_coverage_pct: coverage(sub, 'xerj_hit').pct,
      baseline_coverage_pct: coverage(sub, 'baseline_hit').pct,
    };
  }

  const perFormat = {};
  for (const fmt of [...new Set(rows.map((r) => r.answer_format))].sort()) {
    const sub = rows.filter((r) => r.answer_format === fmt);
    perFormat[fmt] = {
      total: sub.length,
      xerj_hits: sub.filter((r) => r.xerj_hit).length,
      baseline_hits: sub.filter((r) => r.baseline_hit).length,
      xerj_coverage_pct: coverage(sub, 'xerj_hit').pct,
      baseline_coverage_pct: coverage(sub, 'baseline_hit').pct,
    };
  }

  const g = (mt, key) => (perMatchType[mt] ? perMatchType[mt][key] : 0);
  const mtHits = (mt) => ({
    total: g(mt, 'total'),
    xerj: g(mt, 'xerj_hits'),
    baseline: g(mt, 'baseline_hits'),
    xerj_only_wins: g(mt, 'xerj_only'),
  });

  // ── Context cost (bytes → tokens ≈ /4; the /4 cancels in the ratio) ──────────────────
  // HONEST baseline charge (SPEC REVISION 2 audit fix): per query, the baseline's context
  // cost = the size of the SINGLE answer-containing file it must open, charged ONLY when the
  // baseline actually hits (else 0 — it reads nothing toward an answer it cannot find). The
  // false-positive files the broad terms also matched are NOT charged; they are kept ONLY as
  // a labelled diagnostic (`baseline_all_matched_bytes`). Charging them inflated every ratio
  // to a flat ~52× and made the scale-dependence claim false — the defect this fixes.
  //
  // Views, per SPEC §Scoring / REVISION 2:
  //   1. NAIVE overall — every query. FLATTERS the blind baseline: on queries it cannot
  //      answer it opens 0 bytes (now literally true), dragging its total down. Not a claim.
  //   2. ANSWERABLE — only queries the baseline actually answers.
  //   3. LARGE_LITERAL — the real context win: answers buried in ≥60 KB docs a fair grep DOES
  //      find but must open the whole file to quote; XERJ returns one ranked passage. ≫ 1×.
  //   4. LITERAL — small plaintext files: XERJ's 5 returned passages ≈/exceed a tiny file, so
  //      the ratio is ~1× or below. Shown beside large_literal to make the scale-dependence
  //      VISIBLE (headline the conservative returned-passages number, best-passage too).
  const contextBucket = (pred) => {
    const sub = rows.filter(pred);
    const baseline_bytes = sub.reduce((s, r) => s + r.baseline_context_bytes, 0);
    const xerj_returned_bytes = sub.reduce((s, r) => s + r.xerj_returned_bytes, 0);
    const xerj_best_passage_bytes = sub.reduce((s, r) => s + r.xerj_best_passage_bytes, 0);
    return {
      queries: sub.length,
      baseline_bytes,
      xerj_returned_bytes,
      xerj_best_passage_bytes,
      baseline_tokens_approx: Math.round(baseline_bytes / 4),
      xerj_tokens_approx: Math.round(xerj_returned_bytes / 4),
      context_ratio: xerj_returned_bytes > 0 ? round(baseline_bytes / xerj_returned_bytes, 2) : null,
      context_ratio_best_passage:
        xerj_best_passage_bytes > 0 ? round(baseline_bytes / xerj_best_passage_bytes, 2) : null,
    };
  };

  const naiveBucket = contextBucket(() => true);
  const answerableBucket = contextBucket((r) => r.baseline_hit);
  // Only queries the baseline actually answers count toward each context win (by design a
  // fair grep finds the literal/large_literal lines; guarded in case one is missed).
  const largeLiteralBucket = contextBucket((r) => r.match_type === 'large_literal' && r.baseline_hit);
  const literalBucket = contextBucket((r) => r.match_type === 'literal' && r.baseline_hit);

  const naiveRatio = naiveBucket.context_ratio;
  const contextRatioAnswerable = answerableBucket.context_ratio;
  const contextRatioLargeLiteral = largeLiteralBucket.context_ratio;
  const contextRatioLargeLiteralBest = largeLiteralBucket.context_ratio_best_passage;
  const contextRatioLiteral = literalBucket.context_ratio;
  const contextRatioLiteralBest = literalBucket.context_ratio_best_passage;

  // DIAGNOSTIC ONLY — the OLD (inflated) charge: sum over ALL queries of every file the
  // baseline's broad terms matched (false positives included). Never used in a ratio/claim.
  const baselineAllMatchedTotal = rows.reduce((s, r) => s + r.baseline_all_matched_bytes, 0);

  const latencies = rows.map((r) => r.xerj_latency_ms).filter((x) => typeof x === 'number');
  const latency = {
    xerj_p50_ms: round(median(latencies), 2),
    xerj_mean_ms: round(latencies.reduce((s, x) => s + x, 0) / (latencies.length || 1), 2),
    xerj_max_ms: latencies.length ? round(Math.max(...latencies), 2) : null,
  };

  const indexBuildMs = readIndexBuildMs();

  // ── GATE (SPEC §GATE, REVISION 2) ───────────────────────────────────────────────────
  //   #2 overall coverage ≥ baseline AND strictly higher on binary_only (the decisive,
  //      capability-based differentiator: grep structurally cannot read PDF/DOCX bytes).
  //   #3 context_ratio on the large_literal set is > 1× (XERJ returns far fewer tokens
  //      than the whole large files the baseline must load).
  // `robustness` is reported HONESTLY and may TIE under a fair baseline — it is NOT gated.
  const checkOverall = xerjCov.hits >= baseCov.hits;
  const checkBinary = g('binary_only', 'xerj_hits') > g('binary_only', 'baseline_hits');
  const checkLargeLiteralContext =
    contextRatioLargeLiteral !== null && contextRatioLargeLiteral > 1;
  const gatePass = checkOverall && checkBinary && checkLargeLiteralContext;

  const results = {
    generated_at: new Date().toISOString(),
    node: BASE,
    index: INDEX,
    index_doc_count: docCount,
    size: SIZE,
    retrieval_mode: RETRIEVAL_MODE || 'none',
    semantic_subquery_errored: SEMANTIC_ERRORED,
    xerj_query_example: RETRIEVAL_MODE === 'fallback' ? { semantic: semanticBody('<question>'), match: matchBody('<question>') } : hybridBody('<question>'),
    index_build_ms: indexBuildMs,
    honesty: {
      embedder: 'XERJ built-in embedder is lexical feature-hashing (384-dim cosine), NOT neural; "semantic"/robustness wins are word/sub-word overlap, not deep meaning. On plaintext a fair grep of the question\'s own terms ties it.',
      knn: 'kNN is exact brute-force at query time (fine at this corpus size).',
      baseline: 'Baseline is a FAIR shell-only agent: ripgrep over raw bytes for the union of the curated keywords AND the salient tokens of the question itself. It could shell out to pdftotext, but would still lose on format coverage at scale, ranking, per-chunk retrieval, and whole-file context cost.',
      context: 'Context efficiency is measured ONLY over queries the baseline answers, charging the baseline ONLY the single answer-containing file it must open (statSync(answer_path).size) — never the false-positive files its broad terms also matched (kept as a labelled diagnostic). It is a real win only on large documents (large_literal ≫ 1×) and INVERTS on tiny plaintext files (literal ≈ 1× or below), which is why BOTH buckets are shown side by side. The naive overall ratio flatters the blind baseline — it now literally opens 0 bytes on every query it cannot answer — and is reported for transparency, not as a claim.',
    },
    totals: {
      queries: N,
      xerj_hits: xerjCov.hits,
      baseline_hits: baseCov.hits,
      xerj_coverage_pct: xerjCov.pct,
      baseline_coverage_pct: baseCov.pct,
    },
    per_match_type: perMatchType,
    per_format: perFormat,
    match_type_hits: {
      binary_only: mtHits('binary_only'),
      large_literal: mtHits('large_literal'),
      robustness: mtHits('robustness'),
      literal: mtHits('literal'),
    },
    context_cost: {
      method:
        'Baseline is charged ONLY the size of the single answer-containing file it must open (statSync(answer_path).size) when it hits, else 0. False-positive term-matched files are NOT charged; their total is kept as a labelled diagnostic (diagnostic_all_matched_bytes). grep -n yields a matching line, but to quote/verify an answer reliably an agent opens the whole answer file — that single file is the charge.',
      // (1) NAIVE overall — labelled; flatters the blind baseline.
      naive_overall: {
        note: 'flatters the blind baseline — it now literally opens 0 bytes on every query it cannot answer — NOT a claim',
        baseline_total_bytes: naiveBucket.baseline_bytes,
        xerj_total_bytes: naiveBucket.xerj_returned_bytes,
        baseline_tokens_approx: naiveBucket.baseline_tokens_approx,
        xerj_tokens_approx: naiveBucket.xerj_tokens_approx,
        ratio_baseline_over_xerj: naiveRatio,
      },
      // (2) ANSWERABLE — only queries the baseline actually answers.
      answerable: {
        queries: answerableBucket.queries,
        baseline_bytes: answerableBucket.baseline_bytes,
        xerj_returned_bytes: answerableBucket.xerj_returned_bytes,
        baseline_tokens_approx: answerableBucket.baseline_tokens_approx,
        xerj_tokens_approx: answerableBucket.xerj_tokens_approx,
        context_ratio_answerable: contextRatioAnswerable,
      },
      // (3) LARGE_LITERAL — the real, honest context win (expected ≫ 1×).
      large_literal: {
        queries: largeLiteralBucket.queries,
        baseline_bytes: largeLiteralBucket.baseline_bytes,
        xerj_returned_bytes: largeLiteralBucket.xerj_returned_bytes,
        xerj_best_passage_bytes: largeLiteralBucket.xerj_best_passage_bytes,
        context_ratio_large_literal: contextRatioLargeLiteral,
        context_ratio_large_literal_best_passage: contextRatioLargeLiteralBest,
      },
      // (4) LITERAL — small plaintext files; the win INVERTS to ≈1× or below. Shown beside
      // large_literal to demonstrate scale-dependence. Headline = returned-passages ratio
      // (conservative — does not flatter XERJ); the single-best-passage variant is shown too.
      literal: {
        queries: literalBucket.queries,
        baseline_bytes: literalBucket.baseline_bytes,
        xerj_returned_bytes: literalBucket.xerj_returned_bytes,
        xerj_best_passage_bytes: literalBucket.xerj_best_passage_bytes,
        context_ratio_literal: contextRatioLiteral,
        context_ratio_literal_best_passage: contextRatioLiteralBest,
      },
      // DIAGNOSTIC — the OLD inflated all-matched-bytes total. Labelled; NOT a claim.
      diagnostic_all_matched_bytes: {
        note: "DIAGNOSTIC ONLY — sum over ALL queries of the size of EVERY file the baseline's broad terms matched (false-positive files included). This is the pre-fix inflated charge; NOT used in any ratio or claim.",
        baseline_all_matched_bytes_total: baselineAllMatchedTotal,
      },
    },
    latency,
    gate: {
      pass: gatePass,
      checks: {
        overall_coverage_ge_baseline: checkOverall,
        binary_only_strictly_higher: checkBinary,
        large_literal_context_ratio_gt_1: checkLargeLiteralContext,
      },
      // Informational (NOT gated): robustness is expected to ≈ tie under a fair baseline.
      robustness_tie_note: `robustness reported honestly (XERJ ${g('robustness', 'xerj_hits')}/${g('robustness', 'total')} vs baseline ${g('robustness', 'baseline_hits')}/${g('robustness', 'total')}); a lexical embedder ties a fair grep — not gated.`,
    },
    queries: rows,
  };

  writeFileSync(RESULTS_PATH, JSON.stringify(results, null, 2));
  writeFileSync(SCORECARD_PATH, renderScorecard(results));

  // Console summary.
  process.stdout.write(
    `[compare] mode=${results.retrieval_mode} | XERJ ${xerjCov.hits}/${N} (${xerjCov.pct}%) vs baseline ${baseCov.hits}/${N} (${baseCov.pct}%) | ` +
      `binary_only ${g('binary_only', 'xerj_hits')}-${g('binary_only', 'baseline_hits')} (headline) | robustness ${g('robustness', 'xerj_hits')}-${g('robustness', 'baseline_hits')} (≈tie) | ` +
      `ctx large_literal ${contextRatioLargeLiteral ?? 'n/a'}× (naive overall ${naiveRatio ?? 'n/a'}×) | GATE ${gatePass ? 'PASS' : 'FAIL'}\n`,
  );
  process.stdout.write(`[compare] wrote ${RESULTS_PATH} and ${SCORECARD_PATH}\n`);
  // Gate failures are reported honestly, NOT turned into a non-zero exit (SPEC).
  process.exit(0);
}

// ── SCORECARD.md renderer (all values come straight from `results`) ───────────────────
function renderScorecard(r) {
  const yn = (b) => (b ? 'hit' : '—');
  const t = r.totals;
  const mth = r.match_type_hits;
  const cc = r.context_cost;
  const no = cc.naive_overall;
  const ans = cc.answerable;
  const ll = cc.large_literal;
  const lit = cc.literal;
  const diag = cc.diagnostic_all_matched_bytes;
  const num = (x) => (typeof x === 'number' ? x.toLocaleString() : String(x ?? 'n/a'));
  const x = (v) => (v === null || v === undefined ? 'n/a' : `${v}×`);

  const lines = [];
  lines.push('# Doc-index scorecard — Claude + XERJ vs. shell-only (grep) baseline');
  lines.push('');
  lines.push(`_Measured live on ${r.generated_at} against \`${r.node}\` (index \`${r.index}\`, ${r.index_doc_count ?? '?'} chunks). Retrieval mode: **${r.retrieval_mode}**. Every number below is from this live run._`);
  lines.push('');
  lines.push('> **Honest framing (REVISION 2).** The decisive, capability-based win is **binary_only** — a fair grep structurally cannot read PDF/DOCX bytes. **Context efficiency** is a real win, but only shows up on **large_literal** (big documents). **robustness** ≈ **TIE** under a fair baseline, because XERJ\'s built-in embedder is a LEXICAL feature-hashing model (384-dim cosine), NOT neural — a diligent grep of the question\'s own terms matches the same lines. **literal** is the honest control. No overstated semantic claim is made.');
  lines.push('');

  // Per-query table.
  lines.push('## Per-query results');
  lines.push('');
  lines.push('| ID | Type | Fmt | XERJ | Base | XERJ ms | XERJ psg B | Best psg B | Base answer-file B | Base matched B (diag) | Question |');
  lines.push('|----|------|-----|------|------|--------:|-----------:|-----------:|-------------------:|----------------------:|----------|');
  for (const q of r.queries) {
    const ql = q.question.length > 60 ? q.question.slice(0, 57) + '...' : q.question;
    lines.push(
      `| ${q.id} | ${q.match_type} | ${q.answer_format} | ${yn(q.xerj_hit)} | ${yn(q.baseline_hit)} | ${q.xerj_latency_ms ?? '?'} | ${q.xerj_returned_bytes} | ${q.xerj_best_passage_bytes ?? '?'} | ${q.baseline_context_bytes ?? 0} | ${q.baseline_all_matched_bytes ?? 0} | ${ql.replace(/\|/g, '\\|')} |`,
    );
  }
  lines.push('');
  lines.push('_"Base answer-file B" is the HONEST context charge: the size of the single answer-containing file the baseline must open, and **0 when the baseline cannot answer** (it reads nothing toward an answer it cannot find). "Base matched B (diag)" is a DIAGNOSTIC only — the size of every file the baseline\'s broad terms matched, false positives included — and is used in no ratio or claim._');
  lines.push('');

  // Aggregate table.
  lines.push('## Aggregate coverage by match type');
  lines.push('');
  lines.push('| Metric | XERJ | Baseline | Note |');
  lines.push('|--------|------|----------|------|');
  lines.push(`| Overall coverage | ${t.xerj_hits}/${t.queries} (${t.xerj_coverage_pct}%) | ${t.baseline_hits}/${t.queries} (${t.baseline_coverage_pct}%) | XERJ ≥ baseline |`);
  lines.push(`| **binary_only** (answer only in PDF/DOCX) | ${mth.binary_only.xerj}/${mth.binary_only.total} | ${mth.binary_only.baseline}/${mth.binary_only.total} | **HEADLINE — capability grep lacks** |`);
  lines.push(`| large_literal (buried in a ≥60 KB doc) | ${mth.large_literal.xerj}/${mth.large_literal.total} | ${mth.large_literal.baseline}/${mth.large_literal.total} | both hit; see context win below |`);
  lines.push(`| robustness (differently-phrased) | ${mth.robustness.xerj}/${mth.robustness.total} | ${mth.robustness.baseline}/${mth.robustness.total} | ≈ TIE under fair baseline (lexical embedder) |`);
  lines.push(`| literal (plaintext substring) | ${mth.literal.xerj}/${mth.literal.total} | ${mth.literal.baseline}/${mth.literal.total} | honest control — both read plain text |`);
  lines.push('');

  // Per-format coverage.
  lines.push('### Coverage by answer format');
  lines.push('');
  lines.push('| Format | Queries | XERJ | Baseline |');
  lines.push('|--------|--------:|------|----------|');
  for (const [fmt, v] of Object.entries(r.per_format)) {
    lines.push(`| ${fmt} | ${v.total} | ${v.xerj_hits}/${v.total} (${v.xerj_coverage_pct}%) | ${v.baseline_hits}/${v.total} (${v.baseline_coverage_pct}%) |`);
  }
  lines.push('');

  // Context cost — the HONEST views (scale-dependence shown, not asserted).
  lines.push('### Context efficiency (measured honestly)');
  lines.push('');
  lines.push('Context ratio = baseline bytes to open ÷ XERJ bytes returned. The baseline is charged ONLY the single answer-containing file it must open (`statSync(answer_path)`), **never** the false-positive files its broad terms also matched. `grep -n` yields a matching line, but to quote/verify an answer reliably an agent opens the whole answer file — so that single file is the charge (stated plainly as the assumption).');
  lines.push('');
  lines.push('| View | Baseline bytes | XERJ bytes | Ratio | What it means |');
  lines.push('|------|---------------:|-----------:|:-----:|---------------|');
  lines.push(`| **large_literal** (returned passages) | ${num(ll.baseline_bytes)} | ${num(ll.xerj_returned_bytes)} | **${x(ll.context_ratio_large_literal)}** | THE REAL WIN — big files vs. ranked passages (over ${ll.queries} query/queries the baseline answers) |`);
  lines.push(`| large_literal (single best passage) | ${num(ll.baseline_bytes)} | ${num(ll.xerj_best_passage_bytes)} | ${x(ll.context_ratio_large_literal_best_passage)} | one passage the agent would actually quote |`);
  lines.push(`| **literal** (returned passages) | ${num(lit.baseline_bytes)} | ${num(lit.xerj_returned_bytes)} | **${x(lit.context_ratio_literal)}** | SCALE-DEPENDENCE: on tiny plaintext files the win INVERTS — 5 returned passages ≈/exceed the whole small file (over ${lit.queries} query/queries) |`);
  lines.push(`| literal (single best passage) | ${num(lit.baseline_bytes)} | ${num(lit.xerj_best_passage_bytes)} | ${x(lit.context_ratio_literal_best_passage)} | even a single passage ≈ a tiny file |`);
  lines.push(`| answerable (all queries baseline answers) | ${num(ans.baseline_bytes)} | ${num(ans.xerj_returned_bytes)} | ${x(ans.context_ratio_answerable)} | fair whole-corpus view over ${ans.queries} answerable queries |`);
  lines.push(`| naive overall (all queries) | ${num(no.baseline_total_bytes)} | ${num(no.xerj_total_bytes)} | ${x(no.ratio_baseline_over_xerj)} | **flatters the blind baseline — it literally opens 0 bytes on every query it cannot answer** — NOT a claim |`);
  lines.push('');
  lines.push(`**Scale-dependence, side by side:** large_literal **${x(ll.context_ratio_large_literal)}** (answers buried in ≥60 KB docs) vs. literal **${x(lit.context_ratio_literal)}** (tiny plaintext files). The context win is real on large documents and INVERTS on small ones — demonstrated on measured data, not asserted.`);
  lines.push('');
  lines.push(`_Diagnostic (NOT a claim): had the baseline instead been charged every file its broad terms matched — false positives included — the total would be ${num(diag.baseline_all_matched_bytes_total)} bytes. That inflated charge is exactly what this scorecard does NOT use._`);
  lines.push('');
  lines.push(`_Latency: XERJ query p50 / mean / max = ${r.latency.xerj_p50_ms} / ${r.latency.xerj_mean_ms} / ${r.latency.xerj_max_ms} ms. Index build time: ${r.index_build_ms ?? 'not recorded'} ms._`);
  lines.push('');

  // Gate.
  lines.push('### Gate');
  lines.push('');
  lines.push(`- (#2) Overall coverage ≥ baseline: **${r.gate.checks.overall_coverage_ge_baseline ? 'PASS' : 'FAIL'}**`);
  lines.push(`- (#2) binary_only strictly higher (the capability win): **${r.gate.checks.binary_only_strictly_higher ? 'PASS' : 'FAIL'}**`);
  lines.push(`- (#3) large_literal context ratio > 1×: **${r.gate.checks.large_literal_context_ratio_gt_1 ? 'PASS' : 'FAIL'}** (${x(ll.context_ratio_large_literal)})`);
  lines.push(`- (context, informational) literal ratio ≈1× or below — the small-file inversion that proves scale-dependence: ${x(lit.context_ratio_literal)} — **NOT gated**.`);
  lines.push(`- robustness is reported honestly and may TIE — **NOT gated**.`);
  lines.push(`- **GATE: ${r.gate.pass ? 'PASS' : 'FAIL'}**`);
  lines.push('');

  // Honest verdict paragraph — generated from the measured numbers.
  lines.push('## Verdict');
  lines.push('');
  const winBinary = mth.binary_only.xerj - mth.binary_only.baseline;
  const winRobust = mth.robustness.xerj - mth.robustness.baseline;
  const verdict = [];
  verdict.push(
    `On this ${t.queries}-query set, XERJ answered ${t.xerj_hits}/${t.queries} (${t.xerj_coverage_pct}%) versus the fair shell-only baseline's ${t.baseline_hits}/${t.queries} (${t.baseline_coverage_pct}%).`,
  );
  verdict.push(
    `**Headline (the one decisive, capability-based win): binary_only.** Answers that live only inside PDF/DOCX are invisible to ripgrep — it sees compressed/binary bytes and matches nothing — so the baseline gets ${mth.binary_only.baseline}/${mth.binary_only.total}, while XERJ, which extracted and indexed that text, gets ${mth.binary_only.xerj}/${mth.binary_only.total} (+${winBinary}). This is a capability grep structurally lacks, not a tuning artifact.`,
  );
  if (ll.queries > 0 && ll.context_ratio_large_literal !== null) {
    const litClause = lit.context_ratio_literal !== null
      ? `On the ${lit.queries} tiny-file **literal** query/queries the SAME metric INVERTS: XERJ returns ${num(lit.xerj_returned_bytes)} bytes of passages against the baseline's ${num(lit.baseline_bytes)} bytes of tiny answer files — just **${x(lit.context_ratio_literal)}** (${x(lit.context_ratio_literal_best_passage)} counting only the single best passage), because five returned passages meet or exceed a small whole file.`
      : `(No answerable literal queries were available to show the small-file inversion in this run.)`;
    verdict.push(
      `**Context efficiency — real, but scale-dependent (shown, not asserted).** Over the ${ll.queries} large_literal query/queries (answers buried in ≥60 KB docs that a fair grep DOES find), the baseline must open ${num(ll.baseline_bytes)} bytes of whole answer files to quote/verify the line, while XERJ returns ${num(ll.xerj_returned_bytes)} bytes of ranked passages — **${x(ll.context_ratio_large_literal)}** less context (${x(ll.context_ratio_large_literal_best_passage)} counting only the single best passage the agent would actually quote). ${litClause} Side by side — large_literal **${x(ll.context_ratio_large_literal)}** vs. literal **${x(lit.context_ratio_literal)}** — IS the scale-dependence, measured rather than claimed. Throughout, the baseline is charged ONLY the single answer-containing file it must open (\`statSync(answer_path)\`), never the false-positive files its broad terms also matched; \`grep -n\` yields a matching line, but to quote/verify an answer reliably an agent opens the whole file. The naive whole-corpus ratio is ${x(no.ratio_baseline_over_xerj)}, but it flatters the blind baseline — which now literally opens 0 bytes on every query it cannot answer.`,
    );
  } else {
    const why = mth.large_literal.total === 0
      ? 'No large_literal queries were present in this run'
      : `large_literal queries were present (${mth.large_literal.total}) but the baseline answered none of them, so the whole-file-vs-passage ratio could not be measured on the answerable basis`;
    verdict.push(
      `**Context efficiency — real, but only on large documents.** ${why}, so the scale-dependent context win could not be demonstrated here; the naive whole-corpus ratio (${x(no.ratio_baseline_over_xerj)}) is reported for transparency only and flatters the blind baseline, which now literally opens 0 bytes on every query it cannot answer. Throughout, the baseline is charged ONLY the single answer-containing file it must open, never the false-positive files its broad terms also matched.`,
    );
  }
  verdict.push(
    `**robustness ≈ TIE (honest).** On differently-phrased answers the fair baseline — which greps the union of the curated keywords AND the salient tokens of the question itself — gets ${mth.robustness.baseline}/${mth.robustness.total}, and XERJ gets ${mth.robustness.xerj}/${mth.robustness.total} (${winRobust >= 0 ? '+' : ''}${winRobust}). XERJ's built-in embedder is a LEXICAL feature-hashing model (384-dim cosine), NOT neural, so its "semantic" matching is word/sub-word overlap, not deep understanding — a diligent grep of the question's own terms matches the same lines. We report this as a single-query convenience/robustness tie, NOT a semantic-understanding win.`,
  );
  verdict.push(
    `**literal — honest control.** On plaintext substring cases both approaches read plain text (XERJ ${mth.literal.xerj}/${mth.literal.total}, baseline ${mth.literal.baseline}/${mth.literal.total}); this confirms XERJ is not inflating the easy cases.`,
  );
  verdict.push(
    `**Caveats.** kNN is exact brute-force at query time (fine at this corpus size). The baseline could be upgraded to shell out to \`pdftotext\`/\`soffice\` to close the binary gap — but it would still lack ranking, per-chunk retrieval, and the (shallow, lexical) semantic layer, and would keep paying the whole-file context cost on large documents.`,
  );
  verdict.push(`**Gate: ${r.gate.pass ? 'PASS' : 'FAIL'}.**`);
  if (r.semantic_subquery_errored) {
    verdict.push(`_Note: the semantic sub-query errored on this run; XERJ was scored lexical-only._`);
  }
  lines.push(verdict.join(' '));
  lines.push('');

  return lines.join('\n');
}

main().catch((err) => {
  fatal(`unexpected error: ${err && err.stack ? err.stack : err}`);
});
