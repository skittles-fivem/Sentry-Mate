// Sentry Mate shared module: csv.js (CSV built in the browser).
// Canonical source: ui/shared/csv.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// For chart data that has no list resource (lists download from the server: api.csvUrl). toCsv() writes
// RFC 4180 text: fields with commas, quotes or line breaks are quoted, quotes doubled, lines end in CRLF.
// Text that a spreadsheet would run as a formula (=, +, -, @ at the start) gets a leading apostrophe;
// numbers are written plainly with a dot, never in the locale. Column helpers put the unit in the header
// ('Distance (mi)') and convert to the person's units; times are written as ISO 8601 in the car's zone.

import { convert, unitLabel } from './units.js';
import { toIsoZoned } from './time.js';

const NEEDS_QUOTES = /[",\r\n]|^\s|\s$/;
const FORMULA = /^[=+\-@\t\r]/;
const PLAIN_NUMBER = /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/;

/** One CSV field. null, undefined, NaN and Infinity give an empty field. */
export function csvCell(v) {
  if (v == null) return '';
  if (typeof v === 'number') return Number.isFinite(v) ? String(v) : '';
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  if (v instanceof Date) return Number.isFinite(v.getTime()) ? v.toISOString() : '';
  let s = typeof v === 'object' ? JSON.stringify(v) : String(v);
  if (FORMULA.test(s) && !PLAIN_NUMBER.test(s)) s = "'" + s;
  return NEEDS_QUOTES.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

function valueOf(row, col, i) {
  if (typeof col === 'string' || typeof col === 'number') return Array.isArray(row) ? row[i] : row?.[col];
  if (typeof col.value === 'function') return col.value(row, i);
  if (col.key != null) return Array.isArray(row) ? row[col.key] : row?.[col.key];
  return Array.isArray(row) ? row[i] : undefined;
}
const labelOf = (col) => (typeof col === 'string' || typeof col === 'number' ? String(col) : col.label ?? String(col.key ?? ''));

/**
 * Rows to CSV text. rows: objects or arrays. columns: keys ('distance_km'), or { label, key } or
 * { label, value(row) } (see column() and timeColumn()); without columns the first row's keys are used.
 * opts: header (true), eol ('\r\n'), bom (false; downloadCsv adds one so spreadsheets read UTF-8).
 */
export function toCsv(rows, columns = null, { header = true, eol = '\r\n', bom = false } = {}) {
  const list = Array.isArray(rows) ? rows : [];
  const cols = columns || (list.length && !Array.isArray(list[0]) ? Object.keys(list[0]) : null);
  const lines = [];
  if (cols && header) lines.push(cols.map((c) => csvCell(labelOf(c))).join(','));
  list.forEach((row) => {
    if (cols) lines.push(cols.map((c, i) => csvCell(valueOf(row, c, i))).join(','));
    else lines.push((Array.isArray(row) ? row : [row]).map(csvCell).join(','));
  });
  return (bom ? '\ufeff' : '') + lines.join(eol) + (lines.length ? eol : '');
}

const round = (v, d) => (v == null ? null : Math.round(v * 10 ** d) / 10 ** d);

/**
 * A column in the person's units, its unit in the header: column('Distance', 'distance_km', 'distance')
 * -> header 'Distance (mi)', values converted and rounded to opts.digits (4) decimals. key can be a
 * function(row). Without a kind the value is written as it is.
 */
export function column(label, key, kind = null, { digits = 4 } = {}) {
  const get = typeof key === 'function' ? key : (row) => row?.[key];
  if (!kind) return { label, value: get };
  const unit = unitLabel(kind);
  return {
    label: unit && unit !== '%' ? `${label} (${unit})` : unit === '%' ? `${label} (%)` : label,
    value: (row, i) => round(convert(get(row, i), kind), digits),
  };
}

/** A time column: ISO 8601 with the zone's offset ('2026-11-16T11:30:00-06:00'). */
export function timeColumn(label, key, tz) {
  const get = typeof key === 'function' ? key : (row) => row?.[key];
  return { label, value: (row, i) => toIsoZoned(get(row, i), tz) };
}

/**
 * Columnar API data ({ t_ms: [], values: { field: [] } }, as /series sends it) to row objects:
 * [{ t_ms, field: v, ... }]. Extra parallel arrays (state, coverage_pct) can be passed as more.
 */
export function rowsFromColumns(data, more = {}) {
  const t = (data && data.t_ms) || [];
  const values = { ...((data && data.values) || {}), ...more };
  const names = Object.keys(values);
  return t.map((ms, i) => {
    const row = { t_ms: ms };
    for (const n of names) row[n] = values[n]?.[i] ?? null;
    return row;
  });
}

/** A safe file name from parts: csvFilename('Model Y', 'drives', '2026-11-01') -> 'model-y-drives-2026-11-01.csv'. */
export function csvFilename(...parts) {
  const base = parts.flat().filter((p) => p != null && p !== '')
    .map((p) => String(p).normalize('NFKD').replace(/[\u0300-\u036f]/g, '').toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/^-+|-+$/g, ''))
    .filter(Boolean).join('-') || 'data';
  return base.slice(0, 120) + '.csv';
}

/**
 * Build the CSV and hand it to the browser as a download (UTF-8 with a BOM). Returns the text.
 * Needs a document; in other places use toCsv().
 */
export function downloadCsv(filename, rows, columns = null, opts = {}) {
  const textOut = toCsv(rows, columns, { bom: true, ...opts });
  const doc = globalThis.document;
  if (!doc || typeof doc.createElement !== 'function') return textOut;
  const blob = new Blob([textOut], { type: 'text/csv;charset=utf-8' });
  const href = URL.createObjectURL(blob);
  const a = doc.createElement('a');
  a.href = href;
  a.download = filename.endsWith('.csv') ? filename : filename + '.csv';
  a.rel = 'noopener';
  a.hidden = true;
  doc.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(href), 30000);
  return textOut;
}
