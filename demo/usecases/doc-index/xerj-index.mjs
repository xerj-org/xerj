#!/usr/bin/env node
// -----------------------------------------------------------------------------
// xerj-index.mjs — the INDEXER for the "Claude + XERJ, search a recursive folder
// of mixed-format documents" use case.
//
// Walks a folder recursively, extracts text from every supported format
// (.pdf/.docx/.html/.md/.txt), chunks it (~800 chars / ~100 overlap on
// sentence/paragraph boundaries), creates the `docfolder` index with the SPEC
// mappings, and bulk-indexes each chunk into a live XERJ node. Both the semantic
// `body` field and the lexical `body_text` field are set to the SAME chunk text.
//
// Contract: see demo/usecases/doc-index/SPEC.md ("XERJ index contract").
//
// No external npm dependencies. Uses:
//   - node:child_process (execFileSync) to shell out to pdftotext / soffice
//   - node:http                       to talk to XERJ (ES-compatible REST)
//
// Target node: http://localhost:${XERJ_PORT:-9209}  (XERJ started with --insecure)
//
// Usage:
//   node xerj-index.mjs [--dir <folder>] [--recreate] [--batch <n>] [--dry-run]
//
//   --dir <folder>   Folder to walk. Default: ./corpus (next to this script).
//   --recreate       If the `docfolder` index exists, DELETE + recreate it so
//                    runs are reproducible (no duplicate chunks on re-index).
//   --batch <n>      Docs per _bulk request. Default: 200.
//   --dry-run        Extract + chunk + print stats only. Do NOT touch XERJ.
//                    (Handy for testing extraction without a running node.)
//   --help           Show this help.
//
// Extraction tools required on PATH for full corpus coverage:
//   pdftotext   (poppler-utils)   — for .pdf
//   soffice     (LibreOffice)     — for .docx   (falls back to `unzip -p`)
// .html/.md/.txt are handled in pure Node with no external tools.
// -----------------------------------------------------------------------------

import { execFileSync } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// ---------------------------------------------------------------------------
// Config / constants (indexer and compare.mjs MUST agree on these).
// ---------------------------------------------------------------------------
const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const INDEX = 'docfolder';
const PORT = parseInt(process.env.XERJ_PORT || '9209', 10);
const HOST = 'localhost';
const SUPPORTED = new Set(['.pdf', '.docx', '.html', '.md', '.txt']);

const CHUNK_TARGET = 800;   // ~800 chars per chunk
const CHUNK_OVERLAP = 100;  // ~100 chars carried into the next chunk

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------
function parseArgs(argv) {
  const a = {
    dir: path.join(SCRIPT_DIR, 'corpus'),
    recreate: false,
    batch: 200,
    dryRun: false,
    help: false,
  };
  for (let i = 0; i < argv.length; i++) {
    const t = argv[i];
    if (t === '--dir') a.dir = path.resolve(argv[++i]);
    else if (t === '--recreate') a.recreate = true;
    else if (t === '--batch') a.batch = Math.max(1, parseInt(argv[++i], 10) || 200);
    else if (t === '--dry-run' || t === '--dryrun') a.dryRun = true;
    else if (t === '--help' || t === '-h') a.help = true;
    else console.warn(`[warn] ignoring unknown arg: ${t}`);
  }
  return a;
}

const HELP = `xerj-index.mjs — walk a folder, extract+chunk docs, bulk-index into XERJ.

Usage:
  node xerj-index.mjs [--dir <folder>] [--recreate] [--batch <n>] [--dry-run]

  --dir <folder>   Folder to walk (recursive). Default: ./corpus
  --recreate       Delete + recreate the '${INDEX}' index for a clean run.
  --batch <n>      Docs per _bulk request (default 200).
  --dry-run        Extract + chunk + print stats only; do not touch XERJ.
  --help           Show this help.

Env:
  XERJ_PORT        XERJ ES-compat port (default 9209).`;

// ---------------------------------------------------------------------------
// Filesystem walk — recursive, skips symlinks/hidden entries, target ext only.
// ---------------------------------------------------------------------------
function walk(dir) {
  const out = [];
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch (e) {
    console.warn(`[warn] cannot read dir ${dir}: ${e.message}`);
    return out;
  }
  for (const ent of entries) {
    if (ent.name.startsWith('.')) continue;          // skip hidden
    if (ent.isSymbolicLink()) continue;              // avoid loops
    const full = path.join(dir, ent.name);
    if (ent.isDirectory()) {
      out.push(...walk(full));
    } else if (ent.isFile()) {
      const ext = path.extname(ent.name).toLowerCase();
      if (SUPPORTED.has(ext)) out.push(full);
    }
  }
  return out;
}

// ---------------------------------------------------------------------------
// Extraction — one function per format. Each returns plain text or throws.
// The caller catches per-file so a single bad file never crashes the run.
// ---------------------------------------------------------------------------
const EXEC_OPTS = { encoding: 'utf8', maxBuffer: 128 * 1024 * 1024 };

function extractPdf(file) {
  // `pdftotext -layout <f> -`  → text on stdout (preserves column layout).
  return execFileSync('pdftotext', ['-layout', file, '-'], EXEC_OPTS);
}

// One shared LibreOffice user-profile dir per run avoids the per-invocation
// profile-lock dance; conversions run sequentially so a single profile is safe.
let SOFFICE_PROFILE = null;
function sofficeProfileDir() {
  if (!SOFFICE_PROFILE) {
    SOFFICE_PROFILE = fs.mkdtempSync(path.join(os.tmpdir(), 'xerj-lo-profile-'));
  }
  return SOFFICE_PROFILE;
}

function extractDocx(file) {
  // Primary: soffice --headless --convert-to txt into a fresh temp outdir,
  // then read the produced <base>.txt.
  const outdir = fs.mkdtempSync(path.join(os.tmpdir(), 'xerj-docx-'));
  try {
    execFileSync('soffice', [
      '--headless',
      `-env:UserInstallation=file://${sofficeProfileDir()}`,
      '--convert-to', 'txt:Text',
      '--outdir', outdir,
      file,
    ], { ...EXEC_OPTS, stdio: ['ignore', 'ignore', 'pipe'] });
    const base = path.basename(file, path.extname(file));
    const txtPath = path.join(outdir, `${base}.txt`);
    if (fs.existsSync(txtPath)) {
      return fs.readFileSync(txtPath, 'utf8');
    }
    throw new Error('soffice produced no .txt output');
  } catch (primaryErr) {
    // Fallback: unzip word/document.xml out of the .docx and strip its tags.
    try {
      const xml = execFileSync('unzip', ['-p', file, 'word/document.xml'], EXEC_OPTS);
      // <w:p> paragraphs → newlines; <w:tab> → space; then strip all tags.
      const text = xml
        .replace(/<\/w:p>/g, '\n')
        .replace(/<w:tab[^>]*\/>/g, ' ')
        .replace(/<[^>]+>/g, '')
        .replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>')
        .replace(/&quot;/g, '"').replace(/&#39;/g, "'");
      if (text.trim()) return text;
      throw new Error('empty document.xml after strip');
    } catch (fallbackErr) {
      throw new Error(`docx extract failed (soffice: ${primaryErr.message}; unzip fallback: ${fallbackErr.message})`);
    }
  } finally {
    try { fs.rmSync(outdir, { recursive: true, force: true }); } catch { /* ignore */ }
  }
}

const HTML_ENTITIES = {
  '&amp;': '&', '&lt;': '<', '&gt;': '>', '&quot;': '"', '&#39;': "'",
  '&apos;': "'", '&nbsp;': ' ', '&mdash;': '—', '&ndash;': '–', '&hellip;': '…',
};
function decodeEntities(s) {
  return s
    .replace(/&#(\d+);/g, (_, n) => String.fromCodePoint(parseInt(n, 10)))
    .replace(/&#x([0-9a-fA-F]+);/g, (_, n) => String.fromCodePoint(parseInt(n, 16)))
    .replace(/&[a-zA-Z]+;|&#39;/g, (m) => HTML_ENTITIES[m] ?? m);
}
function extractHtml(file) {
  const raw = fs.readFileSync(file, 'utf8');
  const text = raw
    .replace(/<!--[\s\S]*?-->/g, ' ')          // comments
    .replace(/<script[\s\S]*?<\/script>/gi, ' ') // drop scripts
    .replace(/<style[\s\S]*?<\/style>/gi, ' ')   // drop styles
    .replace(/<(?:br|p|div|li|tr|h[1-6])[^>]*>/gi, '\n') // block tags → newline
    .replace(/<[^>]+>/g, ' ')                    // strip remaining tags
    .replace(/[ \t]+/g, ' ');
  return decodeEntities(text);
}

function extractMd(file) {
  // Light markdown de-syntaxing: keep the words, drop the punctuation noise.
  const raw = fs.readFileSync(file, 'utf8');
  return raw
    .replace(/```[\s\S]*?```/g, (m) => m.replace(/```/g, ' ')) // keep fenced text, drop fences
    .replace(/^#{1,6}\s+/gm, '')                 // heading markers
    .replace(/^\s{0,3}[-*+]\s+/gm, '')           // bullet markers
    .replace(/^\s{0,3}\d+\.\s+/gm, '')           // ordered-list markers
    .replace(/^\s{0,3}>\s?/gm, '')               // blockquote markers
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')    // images → alt text
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')     // links → link text
    .replace(/[*_]{1,3}([^*_]+)[*_]{1,3}/g, '$1')// bold/italic
    .replace(/`([^`]+)`/g, '$1')                 // inline code
    .replace(/^\s*[-*_]{3,}\s*$/gm, '');         // horizontal rules
}

function extractTxt(file) {
  return fs.readFileSync(file, 'utf8');
}

function extractByFormat(file, format) {
  switch (format) {
    case 'pdf': return extractPdf(file);
    case 'docx': return extractDocx(file);
    case 'html': return extractHtml(file);
    case 'md': return extractMd(file);
    case 'txt': return extractTxt(file);
    default: throw new Error(`unsupported format: ${format}`);
  }
}

// ---------------------------------------------------------------------------
// Title derivation — first markdown/HTML heading, else prettified filename.
// ---------------------------------------------------------------------------
function deriveTitle(file, format, rawText) {
  if (format === 'md') {
    const m = rawText.match(/^\s{0,3}#{1,6}\s+(.+?)\s*$/m);
    if (m) return m[1].trim();
  }
  const base = path.basename(file, path.extname(file));
  return base.replace(/[-_]+/g, ' ').replace(/\s+/g, ' ').trim();
}

// ---------------------------------------------------------------------------
// Chunking — ~800 chars, ~100 char overlap, on sentence/paragraph boundaries.
// ---------------------------------------------------------------------------
function splitSentences(paragraph) {
  // Break after ., !, ? when followed by whitespace. Coarse but robust.
  const parts = paragraph.split(/(?<=[.!?])\s+(?=[^\s])/);
  return parts.map((s) => s.trim()).filter(Boolean);
}

function chunkText(text, target = CHUNK_TARGET, overlap = CHUNK_OVERLAP) {
  // Normalise: split into paragraphs on blank lines, collapse inner whitespace.
  const paragraphs = text
    .split(/\n\s*\n+/)
    .map((p) => p.replace(/\s+/g, ' ').trim())
    .filter(Boolean);

  // Flatten into sentence-level units (paragraph boundaries already respected
  // because we never merge across paragraphs without going through a sentence).
  const units = [];
  for (const p of paragraphs) units.push(...splitSentences(p));

  const chunks = [];
  let cur = [];
  let curLen = 0;

  const flush = () => {
    if (cur.length) {
      const c = cur.join(' ').trim();
      if (c) chunks.push(c);
    }
  };

  for (const unit of units) {
    if (unit.length > target) {
      // A single oversized sentence: flush, then hard-split it with overlap.
      flush(); cur = []; curLen = 0;
      const step = Math.max(1, target - overlap);
      for (let i = 0; i < unit.length; i += step) {
        chunks.push(unit.slice(i, i + target).trim());
      }
      continue;
    }
    if (cur.length && curLen + unit.length + 1 > target) {
      // Emit current chunk, then seed the next one with trailing sentences that
      // sum to ~overlap chars (sentence-boundary overlap, not a raw char cut).
      flush();
      const carry = [];
      let cl = 0;
      for (let i = cur.length - 1; i >= 0; i--) {
        if (cl >= overlap && carry.length) break;
        carry.unshift(cur[i]);
        cl += cur[i].length + 1;
      }
      cur = carry;
      curLen = cur.reduce((a, s) => a + s.length + 1, 0);
    }
    cur.push(unit);
    curLen += unit.length + 1;
  }
  flush();
  return chunks.filter((c) => c.trim().length > 0);
}

// ---------------------------------------------------------------------------
// XERJ HTTP helpers (node:http; ES-compatible REST on localhost).
// ---------------------------------------------------------------------------
function xerjRequest(method, reqPath, body, contentType = 'application/json') {
  return new Promise((resolve, reject) => {
    const payload = body == null
      ? null
      : (typeof body === 'string' ? body : JSON.stringify(body));
    const req = http.request(
      {
        host: HOST,
        port: PORT,
        method,
        path: reqPath,
        headers: payload == null ? {} : {
          'Content-Type': contentType,
          'Content-Length': Buffer.byteLength(payload),
        },
      },
      (res) => {
        let data = '';
        res.setEncoding('utf8');
        res.on('data', (d) => { data += d; });
        res.on('end', () => resolve({ status: res.statusCode, body: data }));
      },
    );
    req.on('error', reject);
    if (payload != null) req.write(payload);
    req.end();
  });
}

async function xerjJson(method, reqPath, body) {
  const { status, body: text } = await xerjRequest(method, reqPath, body);
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch { /* leave null */ }
  return { status, json, text };
}

const MAPPINGS = {
  mappings: {
    properties: {
      path: { type: 'keyword' },
      dir: { type: 'keyword' },
      format: { type: 'keyword' },
      title: { type: 'text' },
      chunk_id: { type: 'integer' },
      body: { type: 'semantic_text' }, // auto-embed (built-in lexical embedder)
      body_text: { type: 'text' },     // BM25 lexical / match / match_phrase
    },
  },
};

async function ensureIndex(recreate) {
  const head = await xerjRequest('HEAD', `/${INDEX}`);
  const exists = head.status === 200;
  if (exists && recreate) {
    console.log(`[index] '${INDEX}' exists → deleting (--recreate)`);
    const del = await xerjJson('DELETE', `/${INDEX}`);
    if (del.status >= 300) throw new Error(`delete ${INDEX} failed: ${del.status} ${del.text}`);
  } else if (exists) {
    console.log(`[index] '${INDEX}' exists → appending (pass --recreate for a clean run)`);
    return; // keep existing mappings
  }
  const create = await xerjJson('PUT', `/${INDEX}`, MAPPINGS);
  if (create.status >= 300) {
    throw new Error(`create ${INDEX} failed: ${create.status} ${create.text}`);
  }
  console.log(`[index] created '${INDEX}' with SPEC mappings`);
}

// Send one batch of docs via POST /docfolder/_bulk (NDJSON). Returns
// { indexed, errored } counts and logs per-item errors.
async function bulkIndex(docs) {
  const lines = [];
  for (const doc of docs) {
    lines.push(JSON.stringify({ index: { _index: INDEX } }));
    lines.push(JSON.stringify(doc));
  }
  const ndjson = lines.join('\n') + '\n';
  const { status, body } = await xerjRequest(
    'POST', `/${INDEX}/_bulk`, ndjson, 'application/x-ndjson',
  );
  if (status >= 300) {
    console.warn(`[bulk] HTTP ${status} for ${docs.length}-doc batch — skipped`);
    return { indexed: 0, errored: docs.length };
  }
  let resp = null;
  try { resp = JSON.parse(body); } catch { /* ignore */ }
  if (!resp || !Array.isArray(resp.items)) {
    console.warn('[bulk] unparseable response — counting batch as errored');
    return { indexed: 0, errored: docs.length };
  }
  let indexed = 0; let errored = 0;
  for (const item of resp.items) {
    const op = item.index || item.create || {};
    if (op.error) {
      errored++;
      if (errored <= 3) console.warn(`[bulk] item error: ${JSON.stringify(op.error)}`);
    } else {
      indexed++;
    }
  }
  return { indexed, errored };
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) { console.log(HELP); return; }

  const t0 = Date.now();
  console.log(`[start] dir=${args.dir} index=${INDEX} node=http://${HOST}:${PORT} dryRun=${args.dryRun}`);

  const files = walk(args.dir);
  if (files.length === 0) {
    console.warn(`[warn] no supported files found under ${args.dir}`);
  }

  // Reproducible order.
  files.sort();

  if (!args.dryRun) {
    await ensureIndex(args.recreate);
  }

  const perFormatFiles = {};   // format → files seen
  const perFormatChunks = {};  // format → chunks produced
  const skipped = [];
  let totalChunks = 0;
  let totalIndexed = 0;
  let totalErrored = 0;

  let batch = [];
  const flushBatch = async () => {
    if (args.dryRun || batch.length === 0) { batch = []; return; }
    const { indexed, errored } = await bulkIndex(batch);
    totalIndexed += indexed;
    totalErrored += errored;
    batch = [];
  };

  for (const file of files) {
    const format = path.extname(file).slice(1).toLowerCase();
    perFormatFiles[format] = (perFormatFiles[format] || 0) + 1;

    // Repo-relative path when the file lives under the deliverable dir, so the
    // stored `path` matches queries.json's answer_path (e.g. corpus/hr/x.pdf).
    let relPath = path.relative(SCRIPT_DIR, file);
    if (relPath.startsWith('..')) relPath = file; // outside → keep absolute
    const relDir = path.dirname(relPath);

    let rawText;
    try {
      rawText = extractByFormat(file, format);
    } catch (e) {
      console.warn(`[skip] ${relPath}: ${e.message}`);
      skipped.push({ path: relPath, reason: e.message });
      continue;
    }
    if (!rawText || !rawText.trim()) {
      console.warn(`[skip] ${relPath}: no text extracted`);
      skipped.push({ path: relPath, reason: 'empty extraction' });
      continue;
    }

    const title = deriveTitle(file, format, rawText);
    const chunks = chunkText(rawText);
    perFormatChunks[format] = (perFormatChunks[format] || 0) + chunks.length;
    totalChunks += chunks.length;

    console.log(`[ok] ${relPath} (${format}) → ${chunks.length} chunks`);

    for (let ci = 0; ci < chunks.length; ci++) {
      const text = chunks[ci];
      const doc = {
        path: relPath,
        dir: relDir,
        format,
        title,
        chunk_id: ci,
        body: text,       // semantic_text: auto-embedded on ingest
        body_text: text,  // same text for BM25 / match / match_phrase
      };
      batch.push(doc);
      if (batch.length >= args.batch) await flushBatch();
    }
  }
  await flushBatch();

  // Refresh once so freshly-indexed chunks are searchable for the compare phase.
  if (!args.dryRun && totalIndexed > 0) {
    await xerjRequest('POST', `/${INDEX}/_refresh`);
  }

  const elapsedMs = Date.now() - t0;

  // -------------------------------------------------------------------------
  // Summary
  // -------------------------------------------------------------------------
  console.log('\n===== xerj-index summary =====');
  console.log(`files walked      : ${files.length}`);
  console.log('per-format files  :', JSON.stringify(perFormatFiles));
  console.log('per-format chunks :', JSON.stringify(perFormatChunks));
  console.log(`chunks produced   : ${totalChunks}`);
  if (!args.dryRun) {
    console.log(`chunks indexed    : ${totalIndexed}`);
    console.log(`chunks errored    : ${totalErrored}`);
  } else {
    console.log('chunks indexed    : (dry-run — XERJ not contacted)');
  }
  console.log(`files skipped     : ${skipped.length}`);
  if (skipped.length) {
    for (const s of skipped) console.log(`  - ${s.path}: ${s.reason}`);
  }
  console.log(`elapsed ms        : ${elapsedMs}`);

  // Machine-readable one-liner for downstream tooling / compare.mjs.
  const summary = {
    index: INDEX,
    dir: args.dir,
    dryRun: args.dryRun,
    filesWalked: files.length,
    perFormatFiles,
    perFormatChunks,
    chunksProduced: totalChunks,
    chunksIndexed: args.dryRun ? null : totalIndexed,
    chunksErrored: args.dryRun ? null : totalErrored,
    filesSkipped: skipped.length,
    elapsedMs,
  };
  console.log('SUMMARY_JSON ' + JSON.stringify(summary));

  // Clean up the shared soffice profile dir.
  if (SOFFICE_PROFILE) {
    try { fs.rmSync(SOFFICE_PROFILE, { recursive: true, force: true }); } catch { /* ignore */ }
  }

  // Non-zero exit if a live run indexed nothing (something is wrong).
  if (!args.dryRun && files.length > 0 && totalIndexed === 0) {
    process.exitCode = 1;
  }
}

// Run only when invoked directly (`node xerj-index.mjs ...`); when imported by
// tooling/tests the extraction + chunking helpers below are reused instead.
const invokedDirectly = process.argv[1]
  && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invokedDirectly) {
  main().catch((e) => {
    console.error(`[fatal] ${e.stack || e.message}`);
    process.exitCode = 1;
  });
}

export {
  walk, extractByFormat, extractPdf, extractDocx, extractHtml, extractMd, extractTxt,
  chunkText, splitSentences, deriveTitle, MAPPINGS, INDEX,
};
