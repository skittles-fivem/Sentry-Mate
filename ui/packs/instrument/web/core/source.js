// Where data came from: SMO (the Screenmate CAN Bridge's signed logs), the Tesla API (the Tesla Owner API this
// server reads) or an import (TeslaMate's history). Every row of the API carries `sources` (a list of
// { kind: smo | tesla_api | import, site?, label }) and `source_label` (one line for all of them). Pages show
// that line, never the old `source` word, which the server drops in a later release (read here only as a
// fallback for an answer without `sources`).
//
//   srcMark(row)           a small chip: a mark per input and the server's line ("Data from SMO and the Tesla API")
//   srcMark(row, { short: true })   the same with short words ("SMO + Tesla API"), the full line as its title
//   srcText(row)           the line as text (for CSV and titles)
//   srcShort(row)          the short words as text
//   estimated(row)         true when the row's energy is an estimate (Tesla API data alone: energy_basis estimated)
//   SOURCE_FILTER          the options of a source filter: every source, SMO, Tesla API, imported

import { h } from '../shared/dom.js';

export const SOURCE_FILTER = [['', 'Every source'], ['smo', 'SMO'], ['tesla_api', 'Tesla API'], ['import', 'Imported'], ['import:teslamate', 'Imported from TeslaMate']];
// the old filter words still work on the server: read them from an old address as the new ones
export const filterWord = (v) => ({ can: 'smo', teslamate: 'import:teslamate', all: '' }[v] ?? v ?? '');
export const FILTER_WORDS = { smo: 'SMO data only', tesla_api: 'Tesla API data only', import: 'imported history only', 'import:teslamate': 'history imported from TeslaMate only' };
/** The options of a source select, with the value in use always among them (so the select never hides a filter). */
export function sourceOptions(v) {
  if (!v || SOURCE_FILTER.some(([x]) => x === v)) return SOURCE_FILTER;
  const site = /^import:(.+)$/.exec(v)?.[1];
  return [...SOURCE_FILTER, [v, site ? `Imported from ${site}` : v]];
}

const SHORT = { smo: 'SMO', tesla_api: 'Tesla API', import: 'Imported' };
const LABEL = { smo: 'Data from SMO', tesla_api: 'Data from the Tesla API', import: 'Imported' };
const SITE = { teslamate: 'TeslaMate' };
// the old single word, for an answer that has no sources yet
const LEGACY = { can: [{ kind: 'smo', label: 'Data from SMO' }], teslamate: [{ kind: 'import', site: 'teslamate', label: 'Imported from TeslaMate' }],
  tesla_api: [{ kind: 'tesla_api', label: 'Data from the Tesla API' }] };

/** The row's sources as a list (never null). */
export function sourcesOf(row) {
  if (Array.isArray(row?.sources) && row.sources.length) return row.sources;
  return LEGACY[row?.source] || [];
}
export const kindsOf = (row) => [...new Set(sourcesOf(row).map((s) => s.kind))];
export const hasKind = (row, kind) => sourcesOf(row).some((s) => s.kind === kind);
export const isImported = (row) => hasKind(row, 'import');
export const apiOnly = (row) => { const k = kindsOf(row); return k.length === 1 && k[0] === 'tesla_api'; };

const shortOf = (s) => (s.kind === 'import' && SITE[s.site] ? `${SITE[s.site]} import` : SHORT[s.kind] || s.label || s.kind);
const labelOf = (s) => s.label || (s.kind === 'import' && SITE[s.site] ? `Imported from ${SITE[s.site]}` : LABEL[s.kind] || s.kind);

/** The server's one line ("Data from SMO and the Tesla API"), else one made of the labels. */
export function srcText(row) {
  if (row?.source_label) return row.source_label;
  const list = sourcesOf(row);
  if (!list.length) return '';
  if (list.length === 1) return labelOf(list[0]);
  return list.map(labelOf).join(', ');
}
/** Short words: "SMO", "Tesla API", "SMO + Tesla API", "TeslaMate import". */
export function srcShort(row) {
  const list = sourcesOf(row);
  return [...new Set(list.map(shortOf))].join(' + ');
}

export const estimated = (row) => row?.energy_basis === 'estimated';

/**
 * The chip: one mark per input (SMO, Tesla API, import) and the words. Empty sources (an admin typed the
 * value) give `typed` words when given, else nothing.
 *   srcMark(drive)                     marks + "Data from SMO and the Tesla API"
 *   srcMark(drive, { short: true })    marks + "SMO + Tesla API" (title: the full line)
 *   srcMark(drive, { estimated: true }) adds "energy estimated" when the row's energy_basis is estimated
 */
export function srcMark(row, { short = false, estimated: est = false, typed = null, cls = null } = {}) {
  const list = sourcesOf(row);
  if (!list.length) return typed ? h('span', { class: ['src', 'src--typed', cls] }, h('span', { class: 'src-text' }, typed)) : null;
  const kinds = [...new Set(list.map((s) => s.kind))];
  const full = srcText(row);
  const showEst = est && estimated(row);
  return h('span', { class: ['src', cls], dataset: { kinds: kinds.join(' ') }, title: short ? full + (showEst ? '. Energy is estimated from the Tesla API\'s power readings.' : '') : null },
    h('span', { class: 'src-marks', 'aria-hidden': 'true' }, kinds.map((k) => h('i', { class: `src-m src-m--${k}` }))),
    h('span', { class: 'src-text' }, short ? srcShort(row) : full),
    showEst ? h('span', { class: 'src-est' }, short ? 'est.' : 'energy estimated') : null);
}

/** "estimated" words for an energy figure of a Tesla-only row, else null. */
export const estNote = (row) => (estimated(row) ? 'Estimated: Tesla API power readings, summed' : null);

/** The live inputs line of a car (/now inputs): "Live inputs: SMO 1 min ago · Tesla API 2 min ago". ago(ms) words a time. */
export function inputsLine(inputs, ago) {
  const list = (inputs || []).filter(Boolean);
  if (!list.length) return null;
  return h('span', { class: 'src-inputs' },
    h('span', { class: 'src-inputs-label' }, list.length > 1 ? 'Live inputs' : 'Live input'),
    list.map((i) => h('span', { class: 'src', dataset: { kinds: i.kind }, title: i.label || null },
      h('span', { class: 'src-marks', 'aria-hidden': 'true' }, h('i', { class: `src-m src-m--${i.kind}` })),
      h('span', { class: 'src-text' }, SHORT[i.kind] || i.label || i.kind),
      i.last_ms != null ? h('span', { class: 'src-age' }, ago(i.last_ms)) : h('span', { class: 'src-age' }, 'nothing yet'))));
}
