// Sentry Mate, "Logbook" UI pack: where a row, a value or a chart came from.
//
// Every row of the API names its inputs (docs/ui-api.md, Conventions, "Sources"): `sources` is a list of
// { kind: 'smo' | 'tesla_api' | 'teslafi_api' | 'teslamate_local' | 'import', site?, label } and `source_label` says it in one line ("Data from SMO",
// "Data from the Tesla API", "Imported from TeslaMate", "Data from SMO and the Tesla API"). The pages show that
// line, small and quiet, beside the data; never instead of it. The old one-word `source` (can, teslamate,
// tesla_api) is read only when a row has no `sources` (a server from before them).

import { h } from '../shared/dom.js';
import { formatList } from '../shared/i18n.js';
import { formatDuration } from '../shared/time.js';
import * as api from '../shared/api.js';

const SITE = { teslamate: 'TeslaMate', teslafi: 'TeslaFi' };
const SHORT = { smo: 'SMO', tesla_api: 'the Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'local TeslaMate' };
const TAG = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'Local TeslaMate' };
const OLD = {
  can: [{ kind: 'smo', label: 'Data from SMO' }],
  teslamate: [{ kind: 'import', site: 'teslamate', label: 'Imported from TeslaMate' }],
  tesla_api: [{ kind: 'tesla_api', label: 'Data from the Tesla API' }],
  teslafi: [{ kind: 'import', site: 'teslafi', label: 'Imported from TeslaFi' }],
  teslafi_api: [{ kind: 'teslafi_api', label: 'Data from TeslaFi' }],
  teslamate_local: [{ kind: 'teslamate_local', label: 'Data from local TeslaMate' }],
};

/** The row's inputs, as `sources` (or the old `source` word). */
export function sourcesOf(r) {
  if (r && Array.isArray(r.sources) && r.sources.length) return r.sources;
  return (r && OLD[r.source]) || [];
}
export const kindsOf = (r) => new Set(sourcesOf(r).map((s) => s.kind));
/** History brought in from another site (TeslaMate, TeslaFi's data download). */
export const isImported = (r) => kindsOf(r).has('import');
/** Only the Tesla API saw it (no SMO, no import). */
export const teslaOnly = (r) => { const k = kindsOf(r); return k.size > 0 && [...k].every((x) => x === 'tesla_api'); };
/** SMO is among its inputs. */
export const hasSmo = (r) => kindsOf(r).has('smo');

/** One line for a list of sources, worded as the server words it. */
export function labelOf(list) {
  const live = list.filter((s) => s.kind !== 'import').map((s) => SHORT[s.kind] || s.kind);
  const imported = list.filter((s) => s.kind === 'import').map((s) => SITE[s.site] || s.site || 'another site');
  const uniq = (v) => [...new Set(v)];
  const l = uniq(live), i = uniq(imported);
  if (l.length && i.length) return `Data from ${formatList(l)}; imported from ${formatList(i)}`;
  if (l.length) return `Data from ${formatList(l)}`;
  if (i.length) return `Imported from ${formatList(i)}`;
  return 'Where this came from is not known';
}
/** The row's one line: its `source_label`, else worked out from its sources. */
export function sourceLabel(r) {
  if (r && typeof r.source_label === 'string' && r.source_label) return r.source_label;
  return labelOf(sourcesOf(r));
}
/** A short tag for tight places: "SMO", "Tesla API", "TeslaFi", "SMO + Tesla API", "TeslaMate import". */
export function sourceTag(r) {
  const list = sourcesOf(r);
  const live = [...new Set(list.filter((s) => s.kind !== 'import').map((s) => TAG[s.kind] || s.kind))];
  const imp = [...new Set(list.filter((s) => s.kind === 'import').map((s) => `${SITE[s.site] || s.site || 'Other'} import`))];
  return [...live, ...imp].join(' + ') || 'Not known';
}

const dots = (list) => {
  const ks = [...new Set(list.map((s) => s.kind))];
  return h('span', { class: 'src-dots', 'aria-hidden': 'true' }, ks.map((k) => h('span', { class: ['src-dot', `src-dot-${k}`] })));
};

/**
 * The mark: a dot per input and the line ("Data from SMO and the Tesla API"). short: the tag instead of the line,
 * the whole line kept in the title. block: on a line of its own.
 */
export function sourceMark(r, { short = false, block = false, cls = null, prefix = null } = {}) {
  const list = sourcesOf(r);
  const line = sourceLabel(r);
  if (!list.length && !(r && r.source_label)) return null;
  return h('span', { class: ['src', { 'src-block': block }, cls], title: short ? line : null },
    dots(list), h('span', { class: 'src-text' }, prefix, short ? sourceTag(r) : line));
}

/** For a live value ({ value, at_ms, sources }): the tag of the one input that sent it ("SMO", "Tesla API"). */
export function valueMark(v, { cls = null } = {}) {
  if (!v || typeof v !== 'object' || !Array.isArray(v.sources) || !v.sources.length) return null;
  return h('span', { class: ['src', 'src-value', cls], title: labelOf(v.sources) }, dots(v.sources), h('span', { class: 'src-text' }, sourceTag(v)));
}

/** Energy worked out from the Tesla API's power readings (no SMO): "estimated", with why. */
export const ESTIMATED_WHY = 'Only the Tesla API saw this: its power readings (every few seconds to a minute) are summed, so the energy is estimated.';
export const isEstimated = (r) => !!r && r.energy_basis === 'estimated';
/** Why a row's energy is estimated, in a few words. */
export function estimatedWhy(r) {
  if (isImported(r)) return 'imported, worked out from range';
  if (teslaOnly(r)) return 'from the Tesla API’s power readings';
  return 'estimated';
}

/** The source filter: the words `source=` takes (the old `can` and `teslamate` read as `smo` and `import`). */
export const SOURCE_FILTER = [['', 'All sources'], ['smo', 'SMO'], ['tesla_api', 'Tesla API'], ['teslafi_api', 'TeslaFi (live)'], ['teslamate_local', 'Local TeslaMate (live)'],
  ['import', 'Imported (any site)'], ['import:teslamate', 'Imported from TeslaMate'], ['import:teslafi', 'Imported from TeslaFi']];
export function sourceFilter(v) {
  const w = { can: 'smo', teslamate: 'import:teslamate', teslafi: 'import:teslafi', all: '' }[v] ?? v;
  return SOURCE_FILTER.some(([k]) => k === w) ? w : '';
}
/** "comes from SMO" for an empty list's reasons. */
export const SOURCE_WHY = {
  smo: 'has data from SMO', tesla_api: 'has data from the Tesla API', teslafi_api: 'has data from TeslaFi', teslamate_local: 'has data from local TeslaMate', import: 'was imported',
  'import:teslamate': 'was imported from TeslaMate', 'import:teslafi': 'was imported from TeslaFi',
};

/** The "live inputs" line of a car: /now `inputs` ([{ kind, label, last_ms }]). */
export function inputsLine(inputs, { cls = null } = {}) {
  const list = Array.isArray(inputs) ? inputs : [];
  const now = api.now();
  if (!list.length) return h('div', { class: ['src-inputs', cls] }, h('p', { class: 'src-inputs-name' }, 'Live inputs'), h('p', { class: 'src-age' }, 'None has sent a value yet.'));
  return h('div', { class: ['src-inputs', cls] },
    h('p', { class: 'src-inputs-name' }, list.length === 1 ? 'Live input' : 'Live inputs'),
    h('ul', { class: 'src-inputs-list' }, list.map((i) => h('li', null,
      h('span', { class: 'src', title: i.label || null }, dots([i]), h('span', { class: 'src-text' }, TAG[i.kind] || i.label || i.kind,
        h('span', { class: 'src-age' }, i.last_ms ? `, newest ${now - i.last_ms < 60000 ? 'under a minute' : formatDuration(Math.max(0, now - i.last_ms))} ago` : ', nothing yet')))))));
}
