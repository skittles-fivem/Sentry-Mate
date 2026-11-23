// Sentry Mate, "Pebble" UI pack: where data came from.
//
// Every row of the API carries `sources` (a list of { kind: smo | tesla_api | teslafi_api | teslamate_local | import, site?, label }) and
// `source_label`, one line for all of them ("Data from SMO and the Tesla API"). SMO is the Screenmate CAN Bridge's
// signed logs, the Tesla API is the Tesla Owner API this server reads, TeslaFi (`teslafi_api`) is TeslaFi's own polls
// of the car read with the owner's TeslaFi token, an import is history brought in from TeslaMate or TeslaFi's data
// download. Pages show that line small beside the data, never instead of it: a soft pill with one coloured dot per
// input (SMO teal, Tesla API sky, TeslaFi coral, imported sun). The old one-word `source` (can, teslamate, tesla_api) is read only
// when an answer has no `sources`.
//
//   srcPill(row)                 the pill with the whole line ("Data from SMO and the Tesla API")
//   srcPill(row, { short })      the pill with short words ("SMO + Tesla API"), the whole line for screen readers
//   srcText(row)                 the line as text
//   rowTitle(row)                a list card's hover title: the line, plus why its energy is estimated
//   estPill(row)                 "Energy estimated" for a row the Tesla API alone saw (energy_basis), else null
//   viaMark(value)               a small "Tesla API" mark after a live value that came from the Tesla API
//   inputsLine(inputs, { extra }) the car's live inputs from /now: "Live inputs: SMO, 1 min ago · Tesla API, 2 min ago",
//                                extra (a link such as "Change order") at its end
//   SOURCE_OPTIONS, sourceParam  the source filter: All, SMO, Tesla API, TeslaFi, Imported -> the API's source= value

import { h } from '../shared/dom.js';
import { formatRelative } from '../shared/time.js';
import * as api from '../shared/api.js';

const SHORT = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'Local TeslaMate', import: 'Imported' };
const TONE = { smo: 'teal', tesla_api: 'sky', teslafi_api: 'coral', teslamate_local: 'muted', import: 'sun' };
const SITE = { teslamate: 'TeslaMate', teslafi: 'TeslaFi' };
const OLD = {
  can: [{ kind: 'smo', label: 'Data from SMO' }],
  teslamate: [{ kind: 'import', site: 'teslamate', label: 'Imported from TeslaMate' }],
  teslafi: [{ kind: 'import', site: 'teslafi', label: 'Imported from TeslaFi' }],
  tesla_api: [{ kind: 'tesla_api', label: 'Data from the Tesla API' }],
  teslafi_api: [{ kind: 'teslafi_api', label: 'Data from TeslaFi' }],
  teslamate_local: [{ kind: 'teslamate_local', label: 'Data from local TeslaMate' }],
};
const LABEL = { smo: 'Data from SMO', tesla_api: 'Data from the Tesla API', teslafi_api: 'Data from TeslaFi', teslamate_local: 'Data from local TeslaMate' };
/** The words of a live input in a list: "SMO", "the Tesla API", "TeslaFi". */
const LIVE_WORDS = { smo: 'SMO', tesla_api: 'the Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'local TeslaMate' };

/** The row's inputs (never null). */
export function sourcesOf(r) {
  if (r && Array.isArray(r.sources) && r.sources.length) return r.sources;
  return (r && OLD[r.source]) || [];
}
export const kindsOf = (r) => [...new Set(sourcesOf(r).map((s) => s.kind))];
export const hasKind = (r, k) => sourcesOf(r).some((s) => s.kind === k);
export const isImported = (r) => hasKind(r, 'import');
/** Only the Tesla API saw it. */
export const apiOnly = (r) => { const k = kindsOf(r); return k.length === 1 && k[0] === 'tesla_api'; };

const labelOf = (s) => s.label || (s.kind === 'import' ? `Imported from ${SITE[s.site] || s.site || 'another site'}` : LABEL[s.kind] || s.kind);

/** The one line: the row's source_label, else made of its sources. */
export function srcText(r) {
  if (r && typeof r.source_label === 'string' && r.source_label) return r.source_label;
  const list = sourcesOf(r);
  if (!list.length) return '';
  if (list.length === 1) return labelOf(list[0]);
  const live = [...new Set(list.filter((s) => s.kind !== 'import').map((s) => LIVE_WORDS[s.kind] || s.kind))];
  const imp = [...new Set(list.filter((s) => s.kind === 'import').map((s) => SITE[s.site] || s.site || 'another site'))];
  const and = (a) => (a.length > 1 ? `${a.slice(0, -1).join(', ')} and ${a[a.length - 1]}` : a[0]);
  if (live.length && imp.length) return `Data from ${and(live)}; imported from ${and(imp)}`;
  return live.length ? `Data from ${and(live)}` : `Imported from ${and(imp)}`;
}
/** Short words: "SMO + Tesla API", "Imported". */
export const srcShort = (r) => kindsOf(r).map((k) => SHORT[k] || k).join(' + ');

const dots = (kinds) => h('span', { class: 'src-dots', 'aria-hidden': 'true' }, kinds.map((k) => h('span', { class: ['src-dot', `tone-${TONE[k] || 'muted'}`] })));

/**
 * The pill. short: short words with the whole line as its title and for screen readers. typed: words for a value
 * an admin typed (a config value with empty sources); without it, a row with no sources shows nothing.
 */
export function srcPill(r, { short = false, typed = null, cls = null, cover = null } = {}) {
  const kinds = kindsOf(r);
  if (!kinds.length) return typed ? h('span', { class: ['src', 'is-typed', cls], cover }, dots(['typed']), h('span', null, typed)) : null;
  const line = srcText(r);
  return h('span', { class: ['src', cls], title: short ? line : null, cover, dataset: { kinds: kinds.join(' ') } },
    dots(kinds),
    short ? [h('span', { 'aria-hidden': 'true' }, srcShort(r)), h('span', { class: 'sr-only' }, line)] : h('span', null, line));
}

export const isEstimated = (r) => !!r && r.energy_basis === 'estimated';
export const EST_WHY = 'Only the Tesla API saw this: its power readings, every few seconds to a minute, are added up, so the energy is an estimate.';
/** "Energy estimated" for a row whose energy is an estimate. */
export function estPill(r, { text = 'Energy estimated' } = {}) {
  if (!isEstimated(r)) return null;
  return h('span', { class: 'src is-est', title: apiOnly(r) ? EST_WHY : 'This energy is estimated, not measured.' }, h('span', { class: 'src-dots', 'aria-hidden': 'true' }, h('span', { class: 'src-dot tone-sun' })), h('span', null, text));
}

/** A list card's hover title: where its data came from, and why its energy is estimated (list cards show no
 * source pills; the detail sheets and pages do). */
export function rowTitle(r) {
  const line = srcText(r);
  const e = isEstimated(r) ? (apiOnly(r) ? `Energy is estimated. ${EST_WHY}` : 'Energy is estimated, not measured.') : null;
  return [line ? `${line}.` : null, e].filter(Boolean).join(' ') || null;
}

/** A small mark after a live value that came from an input other than SMO ({ value, at_ms, sources }). */
const SHOW_VIA = false;
export function viaMark(val) {
  const s = val && typeof val === 'object' && Array.isArray(val.sources) ? val.sources[0] : null;
  // where a value came from is not marked beside it (the main view stays clean); the live inputs line names them
  if (!s || s.kind === 'smo' || !SHOW_VIA) return null;
  return h('span', { class: 'src src-via', title: labelOf(s) }, dots([s.kind]), h('span', null, SHORT[s.kind] || s.kind));
}

/** The car's live inputs (/now inputs): which inputs it has and the newest value of each. */
export function inputsLine(inputs, { cls = null, cover = null, extra = null } = {}) {
  const list = (Array.isArray(inputs) ? inputs : []).filter(Boolean);
  const now = api.now();
  return h('p', { class: ['src-inputs', cls], cover },
    h('span', { class: 'src-inputs-k' }, list.length === 1 ? 'Live input' : 'Live inputs'),
    list.length ? list.map((i) => h('span', { class: 'src', title: i.label || null },
      dots([i.kind]),
      h('span', null, `${SHORT[i.kind] === 'Imported' ? i.label || 'Imported' : SHORT[i.kind] || i.label || i.kind}, ${i.last_ms != null ? `newest ${formatRelative(i.last_ms, now)}` : 'nothing yet'}`)))
      : h('span', { class: 'muted' }, 'None yet: no SMO log and no Tesla API reading.'),
    extra);
}

/** The source filter: [value, words]. '' is every input. */
export const SOURCE_OPTIONS = Object.freeze([['', 'All'], ['smo', 'SMO'], ['tesla_api', 'Tesla API'], ['teslafi_api', 'TeslaFi'], ['teslamate_local', 'Local TeslaMate'], ['import', 'Imported']]);
export const SOURCE_WORDS = Object.freeze({ smo: 'from SMO', tesla_api: 'from the Tesla API', teslafi_api: 'from TeslaFi', teslamate_local: 'from local TeslaMate', import: 'imported', 'import:teslamate': 'imported from TeslaMate', 'import:teslafi': 'imported from TeslaFi' });
/** A query word (smo, tesla_api, api, teslafi_api, import, imported, teslamate, can) -> the API's source= value, or '' for all. */
export function sourceParam(v) {
  const w = String(v || '').toLowerCase();
  if (w === 'smo' || w === 'can') return 'smo';
  if (w === 'api' || w === 'tesla' || w === 'tesla_api') return 'tesla_api';
  if (w === 'import' || w === 'imported') return 'import';
  if (w === 'teslafi_api' || w === 'teslafi-api') return 'teslafi_api';
  if (w === 'teslamate_local' || w === 'teslamate-local' || w === 'local-teslamate') return 'teslamate_local';
  if (w === 'teslamate' || w === 'import:teslamate') return 'import:teslamate';
  if (w === 'teslafi' || w === 'import:teslafi') return 'import:teslafi';
  return '';
}

/** One row standing for many: the inputs of all of them, SMO first ({ sources }), for a chart or a list head. */
export function unionOf(rows) {
  const seen = new Map();
  for (const r of rows || []) for (const s of sourcesOf(r)) { const k = `${s.kind}:${s.site || ''}`; if (!seen.has(k)) seen.set(k, s); }
  const order = { smo: 0, tesla_api: 1, teslafi_api: 2, teslamate_local: 3, import: 4 };
  return { sources: [...seen.values()].sort((a, b) => (order[a.kind] ?? 9) - (order[b.kind] ?? 9)) };
}
