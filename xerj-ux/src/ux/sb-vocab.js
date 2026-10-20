// ============================================================
// XERJ.ai — SECOND BRAIN · corpus-aware vocabulary (issue #1100)
//
// The Second Brain page is the graph layer over WHATEVER was indexed —
// mail, PDFs, source code, JSON. Its "notes / believed" persona is only
// truthful for note-like corpora (personal notes, correspondence); on a
// source tree (95.8% `code` records) the same copy tells a user they are
// looking at notes and beliefs, which is the "weird knowledge-base mode"
// users report (#1100).
//
// This module is the ONE place that decides, from the corpus's own format
// distribution (the `ax_format` tally the page already runs), whether to
// speak the note persona or neutral wording — records, links, active. It
// is PURE: no DOM, no fetch, no module state, so it is unit-testable and
// has one definition both the ledger (ego-ledger.js) and the map
// (brain-map.js, via second-brain-api.js) read from.
//
// The neutral term for a NODE is "record", never "document": the map
// already uses "document" for a FILE and "note" for a record (one file
// holds many), so collapsing records into documents would double-count.
// ============================================================

/**
 * Families whose records read as personal notes or correspondence — the
 * only corpora for which "notes" and "believed" are honest. Markdown and
 * plain prose sniff as `txt-prose`; mail is `eml` / `mbox` (see
 * engine/crates/xerj-autoindex/src/sniff.rs Family::as_str). Everything
 * else (code, json, csv, logs, txt-lines, html, pdf, …) gets the neutral
 * frame — truthful whatever was indexed.
 */
export const NOTE_LIKE_FORMATS = new Set(['txt-prose', 'mbox', 'eml', 'md', 'markdown']);

/** The single most common format in a `[{ format, count }]` tally, or
 *  null when the tally is empty. Does not assume the list is sorted. */
export function dominantFormat(formats) {
  let best = null;
  let bestN = -1;
  for (const f of Array.isArray(formats) ? formats : []) {
    const n = Number(f && f.count) || 0;
    if (n > bestN) { bestN = n; best = f && f.format != null ? String(f.format) : null; }
  }
  return best;
}

/**
 * Is this corpus note-like? True only when note-like records are a STRICT
 * MAJORITY of the counted records. An unknown or empty tally (the stats
 * read has not run or failed) returns false: the neutral record/link frame
 * is the safe default, and it already matches the page's own kicker/caption
 * ("the documents you indexed"). `otherFormats` (records in formats past
 * the agg's bucket cap) count as non-note — a conservative, never-overclaim
 * reading of the persona.
 *
 * @param {Array<{format:string,count:number}>} formats
 * @param {number} [otherFormats]  records whose format fell past the cap
 */
export function isNoteLikeCorpus(formats, otherFormats = 0) {
  const list = Array.isArray(formats) ? formats : [];
  let note = 0;
  let total = Number(otherFormats) || 0;
  for (const f of list) {
    const n = Number(f && f.count) || 0;
    total += n;
    if (NOTE_LIKE_FORMATS.has(String(f && f.format))) note += n;
  }
  return total > 0 && note * 2 > total;
}

const NOTE_VOC = Object.freeze({
  noteLike: true,
  item: 'note', items: 'notes', Item: 'NOTE', Items: 'NOTES',
  // belief-time strip
  atMoment: 'BELIEVED AT THIS MOMENT',
  live: 'BELIEVED',
  timeAxis: 'BELIEF TIME',
  replayLine: 'THE PAGE REPLAYS WHAT THIS BRAIN BELIEVED AT THAT MOMENT',
  since: 'believed since',
  until: 'believed until',
  // map / ledger rows
  linksAtMoment: 'links believed at this moment',
  linksNotNow: 'links retired or not yet believed',
  readingFocus: 'READING WHAT THE BRAIN BELIEVES ABOUT ',
  // panel eyebrows
  typeDistEye: 'HOW NOTES CONNECT',
  egoEye: 'THE LEDGER · ONE NOTE, EVERYTHING IT TOUCHES',
});

const DOC_VOC = Object.freeze({
  noteLike: false,
  item: 'record', items: 'records', Item: 'RECORD', Items: 'RECORDS',
  atMoment: 'ACTIVE AT THIS MOMENT',
  live: 'ACTIVE',
  timeAxis: 'LINK TIME',
  replayLine: 'THE PAGE REPLAYS WHICH LINKS WERE ACTIVE AT THAT MOMENT',
  since: 'active since',
  until: 'active until',
  linksAtMoment: 'links active at this moment',
  linksNotNow: 'links retired or not yet active',
  readingFocus: 'READING THE LINKS FOR ',
  typeDistEye: 'HOW RECORDS CONNECT',
  egoEye: 'THE LEDGER · ONE RECORD, EVERYTHING IT LINKS TO',
});

/** The vocabulary for a known note-like flag. */
export function vocabFor(noteLike) {
  return noteLike ? NOTE_VOC : DOC_VOC;
}

/** The vocabulary for a format tally (classify + pick in one call). */
export function sbVocab(formats, otherFormats = 0) {
  return vocabFor(isNoteLikeCorpus(formats, otherFormats));
}
