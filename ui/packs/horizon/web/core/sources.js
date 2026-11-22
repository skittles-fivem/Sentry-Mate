// Where data came from: SMO (the CAN Bridge's signed logs), the Tesla API this server reads, TeslaFi's polls
// this server reads (teslafi_api), the owner's local TeslaMate (teslamate_local), or history imported from another site (TeslaMate, TeslaFi). Every row, live value, series and total carries `sources`
// ([{ kind, site?, label }]) and rows carry `source_label` too ("Data from SMO and the Tesla API").
// Horizon shows them as a small band chip: a neon dot per input (cyan SMO, violet Tesla API, green TeslaFi,
// orange imported) and the words. The old `source` word (can, teslamate, tesla_api) is read only as a fallback.
//
//   srcChip(row)                 the compact chip for list cells: "SMO + Tesla API", full label as its title
//   srcChip(row, { full: true }) the whole label, for detail pages and charts
//   valSrc(value)                a live value's input, small, for a figure's label
//   estTag(row)                  "Energy estimated" when energy_basis is estimated (Tesla API alone)
//   inputsLine(now.inputs)       the car page's live inputs, with how old each one's newest value is
//   SOURCE_FILTER                options for a source filter: every, SMO, Tesla API, TeslaFi, Imported (and per site)

import { h } from '../shared/dom.js';
import { ago } from './fmt.js';

const LABEL = { smo: 'Data from SMO', tesla_api: 'Data from the Tesla API', teslafi_api: 'Data from TeslaFi', teslamate_local: 'Data from local TeslaMate', import: 'Imported' };
const SHORT = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'Local TeslaMate', import: 'Imported' };
const OLD = { can: [{ kind: 'smo', label: LABEL.smo }], teslamate: [{ kind: 'import', site: 'teslamate', label: 'Imported from TeslaMate' }], tesla_api: [{ kind: 'tesla_api', label: LABEL.tesla_api }] };
const SITE = { teslamate: 'TeslaMate', teslafi: 'TeslaFi' };

/** The list of inputs of a row (or a live value), from `sources`, else from the old `source` word. */
export function sourcesOf(x) {
  if (!x || typeof x !== 'object') return [];
  if (Array.isArray(x.sources) && x.sources.length) return x.sources.filter((s) => s && s.kind);
  return OLD[x.source] || [];
}

const labelOf = (s) => s.label || (s.kind === 'import' && s.site ? `Imported from ${SITE[s.site] || s.site}` : LABEL[s.kind] || s.kind);
const shortOf = (s) => (s.kind === 'import' && s.site && SITE[s.site] ? `Imported (${SITE[s.site]})` : SHORT[s.kind] || s.kind);

/** The one line for a row: its source_label, else the labels of its sources joined. */
export function sourceLabel(x) {
  if (x?.source_label) return x.source_label;
  const list = sourcesOf(x);
  if (!list.length) return '';
  if (list.length === 1) return labelOf(list[0]);
  return list.map((s, i) => (i ? labelOf(s).replace(/^Data from /, '') : labelOf(s))).join(' and ');
}

/** The short words: "SMO", "Tesla API", "SMO + Tesla API", "Imported (TeslaMate)". */
export const sourceShort = (x) => sourcesOf(x).map(shortOf).join(' + ');

/** The kinds of a row, for a test such as "only the Tesla API". */
export const kindsOf = (x) => sourcesOf(x).map((s) => s.kind);

/** The source chip: a dot per input and the words. Null when the row says nothing. */
export function srcChip(x, { full = false, cls } = {}) {
  const list = sourcesOf(x);
  if (!list.length) return null;
  const label = sourceLabel(x);
  return h('span', { class: ['src', full && 'src--full', cls], title: label, dataset: { kinds: list.map((s) => s.kind).join(' ') } },
    h('span', { class: 'src-dots', 'aria-hidden': 'true' }, list.map((s) => h('span', { class: 'src-dot', dataset: { kind: s.kind } }))),
    h('span', { class: 'src-t' }, full ? label : sourceShort(x)),
    full ? null : h('span', { class: 'sr-only' }, ` (${label})`));
}

/** A live value's input as a dot and a short word ("SMO", "Tesla API"), for a figure's label. */
export function valSrc(x) {
  const list = sourcesOf(x);
  if (!list.length) return null;
  const label = sourceLabel(x);
  return h('span', { class: 'vsrc', title: label },
    list.map((s) => h('span', { class: 'src-dot', dataset: { kind: s.kind }, 'aria-hidden': 'true' })),
    h('span', { class: 'vsrc-t' }, sourceShort(x)), h('span', { class: 'sr-only' }, ` (${label})`));
}

/** "Energy estimated" for a row whose energy comes from the Tesla API's power readings alone. */
export const estTag = (x, text = 'Energy estimated') => (x?.energy_basis === 'estimated'
  ? h('span', { class: 'tag tag--violet', title: 'Only the Tesla API saw this: its power readings, taken every few seconds to a minute, are summed. SMO measures it from the pack.' }, text)
  : null);

/** True when a row has the Tesla API alone (no SMO): its energy is estimated. */
export const estimated = (x) => x?.energy_basis === 'estimated';

/** The car's live inputs: "Live inputs: SMO 1 min ago · Tesla API 2 min ago". */
export function inputsLine(inputs, { cover } = {}) {
  const list = Array.isArray(inputs) ? inputs : [];
  return h('p', { class: 'inputs', cover },
    h('span', { class: 'inputs-k' }, 'Live inputs'),
    list.length
      ? list.map((i) => h('span', { class: 'inputs-i', dataset: { kind: i.kind } }, h('span', { class: 'src-dot', dataset: { kind: i.kind }, 'aria-hidden': 'true' }),
        h('span', null, i.label || LABEL[i.kind] || i.kind), h('span', { class: 'inputs-ago' }, i.last_ms ? ago(i.last_ms) : 'nothing yet')))
      : h('span', { class: 'inputs-i faint' }, 'None yet: no SMO log and no Tesla API reading.'));
}

/** Options for a source filter (the API also takes all). */
export const SOURCE_FILTER = Object.freeze([['', 'Every source'], ['smo', 'SMO'], ['tesla_api', 'Tesla API'], ['teslafi_api', 'TeslaFi (live)'], ['teslamate_local', 'Local TeslaMate (live)'], ['import', 'Imported'],
  ['import:teslamate', 'Imported from TeslaMate'], ['import:teslafi', 'Imported from TeslaFi']]);
/** The old filter words still arrive in saved addresses: read them as the new ones. */
export const sourceParam = (v) => ({ can: 'smo', teslamate: 'import' }[v] || v || '');
/** Words for a filter value in a "showing only" line. */
export const SOURCE_ONLY = Object.freeze({ smo: 'from SMO only', tesla_api: 'from the Tesla API only', import: 'imported only', 'import:teslamate': 'imported from TeslaMate only', teslafi_api: 'from TeslaFi only', teslamate_local: 'from local TeslaMate only', 'import:teslafi': 'imported from TeslaFi only' });

/** True when a row has a live input (SMO, the Tesla API or TeslaFi), false when it is imported only. */
export const isLive = (x) => kindsOf(x).some((k) => k === 'smo' || k === 'tesla_api' || k === 'teslafi_api' || k === 'teslamate_local');
