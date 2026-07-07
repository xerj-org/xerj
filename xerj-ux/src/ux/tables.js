// ============================================================
// XERJ.ai — Table + Markdown primitives
// Plain text, no borders, no zebra stripes, no cell backgrounds.
// ============================================================

import { esc } from './text.js';

/**
 * Table — borderless tabular data. Replaces Kibana "Data Table".
 *
 *   columns: ['NAME', 'COUNT', ...]
 *   rows:    [[cell, cell, ...], ...]
 *   align:   ['left'|'right'|'center', ...] — per column
 */
export const Table = ({ columns, rows, align = [] } = {}) => {
  if (!columns || !columns.length) return '<div class="faint mono">No columns.</div>';
  const ta = (i) => align[i] || (i === 0 ? 'left' : 'right');
  const cols = columns.length;
  const headCells = columns.map((c, i) =>
    `<span style="text-align:${ta(i)}">${esc(c)}</span>`
  ).join('');
  const bodyRows = (rows || []).map((r) => {
    const cells = r.map((c, i) =>
      `<span style="text-align:${ta(i)}">${esc(c)}</span>`
    ).join('');
    return `<div class="tbl-row">${cells}</div>`;
  }).join('');
  return `
  <div class="tbl" style="--tbl-cols:${cols};">
    <div class="tbl-row tbl-head">${headCells}</div>
    ${bodyRows}
  </div>`;
};

/**
 * Markdown — very light markdown for annotation panels.
 * Supports: **bold**, *italic*, `code`, # heading, ## eyebrow, blank-line paragraphs.
 * No HTML injection — everything runs through `esc` first.
 */
export const Markdown = (text = '') => {
  if (!text) return '';
  const paragraphs = text.split(/\n\n+/).map((p) => {
    const t = p.trim();
    if (!t) return '';
    if (t.startsWith('# '))  return `<div class="h-section" style="margin-bottom:10px;">${esc(t.slice(2))}</div>`;
    if (t.startsWith('## ')) return `<div class="key" style="margin-bottom:8px;">${esc(t.slice(3))}</div>`;
    const e = esc(t)
      .replace(/\*\*(.+?)\*\*/g, '<strong style="color:var(--z-ink);">$1</strong>')
      .replace(/\*(.+?)\*/g, '<em>$1</em>')
      .replace(/`([^`]+)`/g, '<span class="mono accent">$1</span>');
    return `<p style="max-width:60ch; margin-bottom:10px;">${e}</p>`;
  }).filter(Boolean).join('');
  return `<div class="md" style="font-family:var(--font-prose); font-size:var(--fs-13); line-height:1.6; color:var(--z-mute);">${paragraphs}</div>`;
};
