// grep-baseline.mjs — the BASELINE retriever for the doc-index use case.
//
// This is the honest "Claude Code with only shell scripts" approach: to answer a
// question, the agent runs ripgrep over the RAW corpus files and reads the matching
// lines (and, to actually quote an answer, the whole matched file) into its context.
//
// HONEST CAPABILITY CEILING (per SPEC.md — do not paper over these):
//   1. BINARY BLINDNESS. PDF and DOCX are binary containers (PDF streams are usually
//      FlateDecode-compressed; DOCX is a ZIP of deflated XML). ripgrep detects the NUL
//      bytes, treats the file as binary, and prints nothing. So any answer that lives
//      ONLY in a .pdf/.docx is structurally unreachable here. We do NOT special-case or
//      exclude those formats — we just run rg over the whole tree and let its real
//      binary detection skip them. The asymmetry is REAL, not simulated.
//   2. LITERAL ONLY (no meaning layer). rg matches literal substrings (we use
//      -F/fixed-strings, exactly how a naive agent greps salient terms). To be FAIR we
//      grep the UNION of the curated keywords AND the salient content tokens of the
//      QUESTION itself (see termsFor / questionTerms below) — a diligent shell agent
//      would obviously grep the words in its own question, not just a canned keyword
//      list. So when an answer is phrased differently but still shares surface words
//      with the question, this baseline finds it (and ties XERJ's shallow lexical
//      embedder). The REAL, residual gap: it is pure surface overlap. If the answer
//      shares NO literal token with the question or keywords, grep misses it — there is
//      no synonym/semantic understanding, and (see #4) no ranking of the matches.
//   3. CONTEXT BLOWUP. A matched line is not an answer; to quote/verify the answer the
//      agent must open the whole file into its context window. `bytesToRead` measures
//      that cost (total size of every file it must open). Big files = big token bills.
//   4. NO RANKING. rg returns matches in file/line order, not by relevance.
//   5. NO INDEX. Every query re-scans the entire tree from scratch.
//
// FAIRNESS NOTE (belongs in the writeup): a script agent *could* shell out to
// `pdftotext`/`soffice` to defeat problem #1. We deliberately do NOT do that here,
// because the point of the baseline is the naive shell-only agent. And even a
// pdftotext-augmented baseline still loses on #2 (no semantics), #3 (still must load
// whole extracted files to answer — no chunk-level retrieval), and #4 (no ranking).
// XERJ's win is not "it can read PDFs" alone; it is format-agnostic extraction PLUS
// chunked, ranked, context-cheap retrieval with a (shallow, lexical) semantic layer.
//
// Public API:
//   scoreQuery(query) -> { hits: [{path, line, text}], filesTouched: [path...], bytesToRead }
// CLI:
//   node grep-baseline.mjs "<query text | query id | keyword>"
//
// Env overrides:
//   DOC_INDEX_CORPUS   - corpus dir to grep (default: ./corpus)
//   DOC_INDEX_QUERIES  - queries.json path  (default: ./queries.json)
//   RG_BINARY          - explicit ripgrep binary (default: `rg` on PATH)
//
// `node --check grep-baseline.mjs` is clean.

import { execFileSync } from 'node:child_process';
import { statSync, existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative, isAbsolute } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));

// Corpus + query-set locations. Overridable so the compare harness can point us at a
// staged corpus, but default to the co-located deliverable files.
const CORPUS_DIR = resolveDir(process.env.DOC_INDEX_CORPUS, join(HERE, 'corpus'));
const QUERIES_PATH = process.env.DOC_INDEX_QUERIES || join(HERE, 'queries.json');

function resolveDir(envVal, fallback) {
  if (!envVal) return fallback;
  return isAbsolute(envVal) ? envVal : join(HERE, envVal);
}

/**
 * Turn an absolute path into a repo-/deliverable-relative one for readable output,
 * matching the "corpus/hr/handbook.pdf" style used in queries.json. Falls back to the
 * absolute path if it lives outside the deliverable dir.
 */
function prettyPath(absPath) {
  const rel = relative(HERE, absPath);
  return rel.startsWith('..') ? absPath : rel;
}

// English function words + interrogatives/auxiliaries that carry no retrieval signal.
// Dropped from QUESTION tokens so we grep the salient content, not "how"/"the"/"is".
// This is DELIBERATELY a conservative function-word list: it drops only articles,
// pronouns, prepositions, conjunctions, auxiliary/quantifier question-words, etc. It
// does NOT drop content nouns/verbs/number-words (e.g. "day", "one", "long"), because
// dropping a content word that happens to sit on the answer's line would re-introduce
// exactly the structural exclusion the audit condemned. When in doubt, KEEP the token.
// NOTE: this only filters tokens we mine from the question — the curated `keywords`
// are always kept verbatim (they may legitimately be short, e.g. "PTO", "SLA").
const QUESTION_STOPWORDS = new Set([
  'a', 'about', 'above', 'after', 'again', 'against', 'all', 'am', 'an', 'and', 'any',
  'are', 'as', 'at', 'be', 'because', 'been', 'before', 'being', 'below', 'between',
  'both', 'but', 'by', 'can', 'cannot', 'could', 'did', 'do', 'does', 'doing', 'done',
  'down', 'during', 'each', 'few', 'for', 'from', 'further', 'get', 'gets', 'getting',
  'got', 'had', 'has', 'have', 'having', 'he', 'her', 'here', 'hers', 'him', 'his', 'how',
  'i', 'if', 'in', 'into', 'is', 'it', 'its', 'let', 'many', 'may', 'me', 'might', 'more',
  'most', 'much', 'must', 'my', 'no', 'nor', 'not', 'of', 'off', 'on', 'once', 'only',
  'or', 'other', 'our', 'ours', 'out', 'over', 'own', 'per', 'same', 'shall', 'she',
  'should', 'so', 'some', 'such', 'than', 'that', 'the', 'their', 'theirs', 'them',
  'then', 'there', 'these', 'they', 'this', 'those', 'through', 'to', 'too', 'under',
  'until', 'up', 'upon', 'us', 'very', 'versus', 'vs', 'was', 'we', 'were', 'what',
  'when', 'where', 'which', 'while', 'who', 'whom', 'why', 'will', 'with', 'would', 'you',
  'your', 'yours',
]);

// Tokenize the question text. Lowercases, drops apostrophes (so "Northwind's" → the
// token "northwind"), and keeps word-internal hyphens/dots/percent/dollar so realistic
// literals survive whole: "16-inch", "sev-1", "99.9%", "$75". Returns tokens in their
// original order (used both for unigrams and for adjacent bigrams).
function tokenizeQuestion(question) {
  return String(question)
    .toLowerCase()
    .replace(/['’]/g, ' ') // possessives/contractions → boundaries, not glued tokens
    .match(/[a-z0-9](?:[a-z0-9%$.\-]*[a-z0-9%])?/g) || [];
}

// A question token is a "salient content token" (approximating a noun/number/keyword) if
// it is not a stopword AND is either numeric, contains a digit/%/$ (a literal like "$75"
// or "sev-1"), or is an alphabetic word of length ≥ 3. This keeps meaning-bearing words
// and drops filler without needing a POS tagger.
function isSalientToken(tok) {
  if (!tok || QUESTION_STOPWORDS.has(tok)) return false;
  if (/[0-9%$]/.test(tok)) return true; // numbers and numeric literals are salient
  return /^[a-z][a-z\-]*$/.test(tok) && tok.length >= 3;
}

/**
 * Derive salient search terms from the QUESTION itself: content unigrams (stopwords
 * dropped), quoted phrases kept whole, and adjacent content-word bigrams (e.g. "remote
 * work", "locked account") which a shell agent would naturally grep for. This is the
 * heart of the fairness fix — the baseline is NEVER restricted to a curated keyword list
 * that structurally dodges the answer's own line; it also greps the words in the
 * question, so a differently-phrased answer that still shares the question's surface
 * words IS found.
 */
function questionTerms(question) {
  if (typeof question !== 'string' || question.trim() === '') return [];
  const terms = [];
  // 1. Quoted phrases (single or double) are high-value literals — keep them whole.
  for (const m of question.matchAll(/["“”']([^"“”']+)["“”']/g)) {
    const phrase = m[1].trim();
    if (phrase) terms.push(phrase);
  }
  // 2. Content unigrams + adjacent content bigrams.
  const toks = tokenizeQuestion(question);
  for (let i = 0; i < toks.length; i++) {
    if (isSalientToken(toks[i])) terms.push(toks[i]);
    if (i + 1 < toks.length && isSalientToken(toks[i]) && isSalientToken(toks[i + 1])) {
      terms.push(`${toks[i]} ${toks[i + 1]}`); // bigram, matched literally with rg -F
    }
  }
  return terms;
}

/**
 * Search terms for a query = the UNION of the curated `keywords` AND the salient content
 * tokens mined from the `question` itself (deduped, case-insensitive). This is exactly
 * what a diligent shell-only agent would grep: its keyword hunches PLUS the words in its
 * own question. It is deliberately NOT capped to `keywords` — the v1 baseline was capped
 * to a curated list that dodged the answer line, which an audit correctly flagged as
 * rigged. A bare string caller (CLI ad-hoc) greps the whole string as one literal term.
 */
function termsFor(query) {
  if (typeof query === 'string') return [query];

  const union = [];
  if (Array.isArray(query.keywords)) {
    for (const k of query.keywords) {
      if (typeof k === 'string' && k.trim() !== '') union.push(k.trim());
    }
  }
  union.push(...questionTerms(query.question));

  // Dedupe case-insensitively, preserving first-seen order for stable/readable output.
  const seen = new Set();
  const terms = [];
  for (const t of union) {
    const key = t.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    terms.push(t);
  }

  // Last-resort fallback: if a query somehow has neither keywords nor a usable question,
  // grep the raw question string so we never silently search for nothing.
  if (terms.length === 0 && typeof query.question === 'string' && query.question.trim() !== '') {
    return [query.question.trim()];
  }
  return terms;
}

/**
 * Resolve how to invoke ripgrep, memoized. A community user just has `rg` on PATH —
 * that is the normal path. We also support `RG_BINARY=/path/to/rg` as an override, and
 * a fallback for the Claude Code sandbox, where `rg` is a shell function (not a binary
 * on PATH) that dispatches to ripgrep embedded in the `claude` executable via argv0=rg;
 * node can reproduce that with the spawn `argv0` option.
 * @returns {{cmd:string, opts:object}|null}
 */
let _rg; // memo: undefined = unprobed, null = unavailable, object = resolved
function resolveRg() {
  if (_rg !== undefined) return _rg;
  const probe = (cmd, opts) => {
    try {
      execFileSync(cmd, ['--version'], { stdio: 'ignore', ...opts });
      return true;
    } catch {
      return false;
    }
  };
  // 1. explicit override
  if (process.env.RG_BINARY && probe(process.env.RG_BINARY, {})) {
    return (_rg = { cmd: process.env.RG_BINARY, opts: {} });
  }
  // 2. a real `rg` binary on PATH (the normal community case)
  if (probe('rg', {})) return (_rg = { cmd: 'rg', opts: {} });
  // 3. Claude Code sandbox: ripgrep is embedded in the `claude` binary (argv0=rg)
  const claudeBin = process.env.CLAUDE_CODE_EXECPATH || '/home/claude/.local/bin/claude';
  if (probe(claudeBin, { argv0: 'rg' })) {
    return (_rg = { cmd: claudeBin, opts: { argv0: 'rg' } });
  }
  return (_rg = null);
}

/**
 * Run ripgrep for the given literal terms over the corpus and return raw matches.
 * Flags: -i (case-insensitive), -n (line numbers), --no-heading (path per line),
 * -F (fixed/literal strings — a naive substring grep), one `-e TERM` per search term (OR).
 * We do NOT pass -a/--text: default binary detection is what makes PDF/DOCX yield
 * nothing, which is the honest behavior of `rg` on those formats.
 */
function runRipgrep(terms) {
  if (!terms.length) return '';
  if (!existsSync(CORPUS_DIR)) {
    process.stderr.write(
      `[grep-baseline] corpus dir not found: ${CORPUS_DIR} (generate it with gen-corpus.mjs)\n`,
    );
    return '';
  }
  const rg = resolveRg();
  if (!rg) {
    process.stderr.write(
      '[grep-baseline] ripgrep not found. Install rg, or set RG_BINARY=/path/to/rg.\n',
    );
    return '';
  }
  const args = ['-i', '-n', '--no-heading', '-F'];
  for (const t of terms) args.push('-e', t);
  args.push(CORPUS_DIR);
  try {
    return execFileSync(rg.cmd, args, {
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
      ...rg.opts,
    });
  } catch (err) {
    // ripgrep exit codes: 0 = matches, 1 = no matches (NORMAL, not an error),
    // 2 = actual error. Only surface real errors.
    if (err && err.status === 1) return '';
    if (err && typeof err.status === 'number') {
      process.stderr.write(
        `[grep-baseline] rg exited ${err.status}: ${String(err.stderr || err.message).trim()}\n`,
      );
      return '';
    }
    // spawn failure.
    process.stderr.write(`[grep-baseline] could not run rg: ${err && err.message}\n`);
    return '';
  }
}

/**
 * Parse a single `path:line:text` ripgrep line (produced by -n --no-heading).
 * Absolute POSIX paths contain no ':', so the first two colons are the separators.
 */
function parseRgLine(raw) {
  const i1 = raw.indexOf(':');
  if (i1 < 0) return null;
  const rest = raw.slice(i1 + 1);
  const i2 = rest.indexOf(':');
  if (i2 < 0) return null;
  const lineNo = Number.parseInt(rest.slice(0, i2), 10);
  if (!Number.isFinite(lineNo)) return null;
  return {
    absPath: raw.slice(0, i1),
    line: lineNo,
    text: rest.slice(i2 + 1),
  };
}

/**
 * BASELINE retrieval for one query.
 * @param {object|string} query - a queries.json entry (or a bare keyword string).
 * @returns {{hits: Array<{path:string,line:number,text:string}>, filesTouched: string[], bytesToRead: number}}
 *   hits         - every matched {path, line, text} (binary files contribute none).
 *   filesTouched - unique files the agent would have to open to read answers.
 *   bytesToRead  - total bytes of those files (context cost; ~chars/4 => tokens).
 */
export function scoreQuery(query) {
  const terms = termsFor(query);
  const out = runRipgrep(terms);

  const hits = [];
  const touchedAbs = new Set();
  for (const raw of out.split('\n')) {
    if (raw === '') continue;
    const parsed = parseRgLine(raw);
    if (!parsed) continue;
    touchedAbs.add(parsed.absPath);
    hits.push({ path: prettyPath(parsed.absPath), line: parsed.line, text: parsed.text });
  }

  let bytesToRead = 0;
  const filesTouched = [];
  for (const abs of touchedAbs) {
    filesTouched.push(prettyPath(abs));
    try {
      bytesToRead += statSync(abs).size;
    } catch {
      /* file vanished mid-run; skip its bytes rather than crash */
    }
  }

  return { hits, filesTouched, bytesToRead };
}

/**
 * Load queries.json (if present) so the CLI can resolve a human query/id to its
 * ground-truth entry. Returns [] if the file is missing/unreadable.
 */
function loadQueries() {
  try {
    if (!existsSync(QUERIES_PATH)) return [];
    return JSON.parse(readFileSync(QUERIES_PATH, 'utf8'));
  } catch (err) {
    process.stderr.write(`[grep-baseline] could not read queries.json: ${err.message}\n`);
    return [];
  }
}

/** Resolve a CLI argument to a query object: match by id, exact question, or substring. */
function resolveCliQuery(arg) {
  const queries = loadQueries();
  const lc = arg.toLowerCase();
  const byId = queries.find((q) => q.id && q.id.toLowerCase() === lc);
  if (byId) return byId;
  const byQ = queries.find((q) => q.question && q.question.toLowerCase() === lc);
  if (byQ) return byQ;
  const byPartial = queries.find((q) => q.question && q.question.toLowerCase().includes(lc));
  if (byPartial) return byPartial;
  // Not a known query — treat the raw string as a single literal keyword.
  return { id: '(adhoc)', question: arg, keywords: [arg] };
}

function runCli(argv) {
  const arg = argv.slice(2).join(' ').trim();
  if (!arg) {
    process.stderr.write('usage: node grep-baseline.mjs "<query text | query id | keyword>"\n');
    process.exit(2);
  }
  const query = resolveCliQuery(arg);
  const terms = termsFor(query);
  const { hits, filesTouched, bytesToRead } = scoreQuery(query);

  const tokensApprox = Math.ceil(bytesToRead / 4);
  process.stdout.write(`query      : ${query.question || arg}\n`);
  if (query.id) process.stdout.write(`query id   : ${query.id}\n`);
  process.stdout.write(`grep terms : ${terms.map((t) => JSON.stringify(t)).join(', ')}\n`);
  process.stdout.write(`hits       : ${hits.length} line(s) across ${filesTouched.length} file(s)\n`);
  process.stdout.write(
    `bytesToRead: ${bytesToRead} bytes to open (~${tokensApprox} tokens to read answers)\n`,
  );
  process.stdout.write('---\n');
  const shown = hits.slice(0, 20);
  for (const h of shown) {
    process.stdout.write(`${h.path}:${h.line}: ${h.text}\n`);
  }
  if (hits.length > shown.length) {
    process.stdout.write(`... (${hits.length - shown.length} more)\n`);
  }
  if (hits.length === 0) {
    process.stdout.write(
      '(no matches — binary PDF/DOCX are invisible to rg, and literal grep finds no differently-phrased answers)\n',
    );
  }
}

// Only run the CLI when invoked directly (not when imported by compare.mjs).
if (import.meta.url === `file://${process.argv[1]}`) {
  runCli(process.argv);
}
