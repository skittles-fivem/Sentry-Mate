// Where data came from: SMO (the Screenmate CAN Bridge's signed logs), the Tesla API (the Tesla Owner API this
// server reads), TeslaFi (TeslaFi's own polls of the car, read with the owner's TeslaFi token) or an import
// (TeslaMate's or TeslaFi's history). Every row of the API carries `sources` (a list of
// { kind: smo | tesla_api | teslafi_api | teslamate_local | import, site?, label }) and `source_label` (one line for all of them, "Data from SMO
// and the Tesla API"). Pages show that line, small, beside the data and never instead of it. The old one-word
// `source` (can, teslamate, tesla_api) is read only for an answer that has no `sources` (an older server).
//
//   src(row)                  '[smo+api]' in the input colours, the full line as its title (tables, tight lines)
//   src(row, { full: true })  '[smo+api] data from SMO and the Tesla API' (detail pages, chart heads)
//   srcText(row)              the line as text (CSV, titles)
//   est(row)                  '[est]' when the row's energy is estimated (Tesla API data alone), else null
//   inputsLine(inputs)        the car's live inputs: 'live inputs  [smo] 1 min ago  [api] 2 min ago  [tfi] ...'
//   via(value)                '[api]' after a live value the Tesla API sent ({ value, at_ms, sources }), else null
//   sourceWord(v)             a filter word (smo, api, teslafi, imported, teslamate, ...) -> the API's source= value
//   sourceCheck(v)            null for a known source word, else why it is left out (filterLine's check)

import { h } from '../shared/dom.js';
import { formatRelative } from '../shared/time.js';
import * as api from '../shared/api.js';

const SHORT = { smo: 'smo', tesla_api: 'api', teslafi_api: 'tfi', teslamate_local: 'tml', import: 'imp' };
const SITE = { teslamate: 'TeslaMate', teslafi: 'TeslaFi' };
const LIVE_NAME = { smo: 'SMO', tesla_api: 'the Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'local TeslaMate' };
const OLD = {
  can: [{ kind: 'smo', label: 'Data from SMO' }],
  teslamate: [{ kind: 'import', site: 'teslamate', label: 'Imported from TeslaMate' }],
  tesla_api: [{ kind: 'tesla_api', label: 'Data from the Tesla API' }],
};
const LABEL = { smo: 'Data from SMO', tesla_api: 'Data from the Tesla API', teslafi_api: 'Data from TeslaFi', teslamate_local: 'Data from local TeslaMate' };

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
  const live = [...new Set(list.filter((s) => s.kind !== 'import').map((s) => LIVE_NAME[s.kind] || s.kind))];
  const imp = [...new Set(list.filter((s) => s.kind === 'import').map((s) => SITE[s.site] || s.site || 'another site'))];
  const and = (a) => (a.length > 1 ? `${a.slice(0, -1).join(', ')} and ${a[a.length - 1]}` : a[0]);
  if (live.length && imp.length) return `Data from ${and(live)}; imported from ${and(imp)}`;
  return live.length ? `Data from ${and(live)}` : `Imported from ${and(imp)}`;
}

/** The short words, as text: 'smo', 'api', 'smo+api', 'imp'. */
export const srcShort = (r) => kindsOf(r).map((k) => SHORT[k] || k).join('+');

const marks = (kinds) => kinds.map((k, i) => [i ? h('span', { class: 'src-plus' }, '+') : null, h('span', { class: ['src-k', `src-k--${k}`] }, SHORT[k] || k)]);

/**
 * '[smo+api]' with the full line as its title; full: the line after it. typed: words for a value an admin typed
 * (empty sources on a config value), else nothing for no sources.
 */
export function src(r, { full = false, typed = null, cls = null } = {}) {
  const kinds = kindsOf(r);
  const line = srcText(r);
  if (!kinds.length) return typed ? h('span', { class: ['src', cls] }, h('span', { class: 'src-b' }, '[', h('span', { class: 'src-k src-k--typed' }, 'typed'), ']'), full ? h('span', { class: 'src-t' }, typed) : null) : null;
  return h('span', { class: ['src', cls], title: full ? null : line, dataset: { kinds: kinds.join(' ') } },
    h('span', { class: 'src-b', 'aria-hidden': full ? 'true' : null }, '[', marks(kinds), ']'),
    full ? h('span', { class: 'src-t' }, line.charAt(0).toLowerCase() + line.slice(1)) : h('span', { class: 'sr-only' }, line));
}

export const isEstimated = (r) => !!r && r.energy_basis === 'estimated';
export const EST_WHY = 'Only the Tesla API saw this: its power readings (every few seconds to a minute) are summed, so the energy is estimated.';
/** '[est]' for a row whose energy is an estimate, with why in its title. */
export const est = (r) => (isEstimated(r) ? h('span', { class: 'tag tag--warn src-est', title: apiOnly(r) ? EST_WHY : 'This energy is estimated, not measured.' }, '[est]') : null);

/** '[api]' after a live value that came from an input other than SMO. */
const SHOW_VIA = false;
export function via(v) {
  if (!SHOW_VIA) return null; // where a value came from is not marked beside it; the live inputs line names them
  const s = v && typeof v === 'object' && Array.isArray(v.sources) ? v.sources[0] : null;
  if (!s || s.kind === 'smo') return null;
  return h('span', { class: 'src src--via', title: s.label || labelOf(s) }, h('span', { class: 'src-b' }, '[', h('span', { class: ['src-k', `src-k--${s.kind}`] }, SHORT[s.kind] || s.kind), ']'));
}

/** The car's live inputs (/now inputs): 'live inputs  [smo] newest 1 min ago  [api] newest 2 min ago'. */
export function inputsLine(inputs, { cls = null } = {}) {
  const list = (Array.isArray(inputs) ? inputs : []).filter(Boolean);
  const now = api.now();
  return h('p', { class: ['src-inputs', cls] },
    h('span', { class: 'src-inputs-k' }, list.length === 1 ? 'live input' : 'live inputs'),
    list.length ? list.map((i) => h('span', { class: 'src-in', title: i.label || null },
      h('span', { class: 'src-b' }, '[', h('span', { class: ['src-k', `src-k--${i.kind}`] }, SHORT[i.kind] || i.kind), ']'),
      ` ${i.kind === 'smo' ? 'SMO' : i.kind === 'tesla_api' ? 'Tesla API' : i.kind === 'teslafi_api' ? 'TeslaFi' : i.kind === 'teslamate_local' ? 'local TeslaMate' : i.label || i.kind}`,
      h('span', { class: 'muted' }, i.last_ms != null ? `, newest ${formatRelative(i.last_ms, now)}` : ', nothing yet')))
      : h('span', { class: 'muted' }, 'none yet: no SMO log and no Tesla API reading'));
}

/** The filter words: smo, api (tesla, tesla_api), teslafi (tfi, teslafi_api: TeslaFi's live polls), tml (teslamate_local, local-teslamate: the local TeslaMate's MQTT), imported
 * (import), teslamate and teslafi-import (one import site), and the old can. */
export function sourceWord(v) {
  const w = String(v || '').toLowerCase();
  if (!w) return null;
  if (w === 'smo' || w === 'can') return 'smo';
  if (w === 'api' || w === 'tesla' || w === 'tesla_api' || w === 'tesla-api') return 'tesla_api';
  if (w === 'imported' || w === 'import' || w === 'imp') return 'import';
  if (w === 'teslafi' || w === 'tfi' || w === 'teslafi_api' || w === 'teslafi-api') return 'teslafi_api';
  if (w === 'tml' || w === 'teslamate_local' || w === 'teslamate-local' || w === 'local-teslamate') return 'teslamate_local';
  if (w === 'teslamate' || w === 'import:teslamate') return 'import:teslamate';
  if (w === 'teslafi-import' || w === 'import:teslafi') return 'import:teslafi';
  return null;
}
const SOURCE_WORDS = new Set(['smo', 'can', 'api', 'tesla', 'tesla_api', 'tesla-api', 'teslafi', 'tfi', 'teslafi_api', 'teslafi-api', 'tml', 'teslamate_local', 'teslamate-local', 'local-teslamate', 'imported', 'import', 'imp',
  'teslamate', 'import:teslamate', 'teslafi-import', 'import:teslafi', 'all']);
/** For filterLine's check: null for a word sourceWord knows (or all), else why it is not taken. */
export const sourceCheck = (v) => (SOURCE_WORDS.has(String(v || '').toLowerCase()) ? null : `not a source this page knows: smo, api, teslafi, tml, imported, teslamate, teslafi-import or all. It is left out, so every source is listed.`);
export const SOURCE_KEY = 'smo | api | teslafi | tml | imported | teslamate | teslafi-import';
