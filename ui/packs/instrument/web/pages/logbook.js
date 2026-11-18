// Logbook (/cars/{car}/logbook): a trip log for tax and expense claims, for any country.
// - GET /cars/{car}/logbook (period; every drive in it, numbered from 1, oldest first, with odometer at
//   start and end and the odometer kilometres missing before each drive), as JSON, CSV or PDF.
// - A purpose per drive (business, commute, private) with a relation and a note: typed by an admin
//   (PUT/DELETE .../drives/{drive}/purpose) or filled in by rules (GET/POST/DELETE .../purpose-rules).
//   Both are kept apart from the drive rows, so a rebuild of the history keeps them.
// - The purpose filter (?purpose=) only narrows the table; totals and continuity always cover the period.
// - Live: a drive that starts or ends ("rows" events with kind "drives") loads the logbook again.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { column, timeColumn } from '../shared/csv.js';
import { formatDate, formatTime, formatDuration, dayKey, MINUTE } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, figures } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState } from '../core/states.js';
import { openPeriodPicker } from '../core/period.js';
import { openCarSwitcher } from '../core/carswitch.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { field, form, showApiError } from '../core/form.js';
import { flash } from '../core/flash.js';
import { isAdmin } from '../core/prefs.js';
import { q, qt, DASH } from '../core/fmt.js';

export const id = 'logbook';
export const css = true;

const enc = encodeURIComponent;
const GAP_KM = 0.05; // below 50 m the odometer and the logged start agree (rounding, a car moved in a car park)

const PURPOSES = {
  business: { label: 'Business', text: 'Work trips you can claim: customer visits, errands for an employer.' },
  commute: { label: 'Commute', text: 'Between home and the usual place of work. Many countries treat it apart from business.' },
  private: { label: 'Private', text: 'Everything personal.' },
  none: { label: 'Not set', text: 'Drives nobody has given a purpose and no rule matches.' },
};
const ORDER = ['business', 'commute', 'private', 'none'];
const DAYS = [
  ['mon-fri', 'Weekdays (Mon to Fri)'], ['sat,sun', 'Weekends (Sat and Sun)'], ['all', 'Every day'],
  ['mon', 'Mondays'], ['tue', 'Tuesdays'], ['wed', 'Wednesdays'], ['thu', 'Thursdays'], ['fri', 'Fridays'], ['sat', 'Saturdays'], ['sun', 'Sundays'],
];
const dayLabel = (d) => (DAYS.find((x) => x[0] === d) || [d, d])[1];

const purposeChip = (p) => h('span', { class: ['lb-purpose', `lb-p--${p || 'none'}`] }, h('i', { 'aria-hidden': 'true' }), PURPOSES[p || 'none'].label);

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const admin = isAdmin();
  const path = `cars/${enc(cid)}/logbook`;
  const rulesPath = `cars/${enc(cid)}/purpose-rules`;
  let filter = ORDER.includes(ctx.query.purpose) ? ctx.query.purpose : 'all';
  let data = null;     // the last logbook answer
  let places = [];     // for rule names and the rule form
  let rules = [];

  const root = h('div', { class: 'lb-page' });

  /* ---------- head: the car, the period and the exports */
  const pdfLink = h('a', {
    class: 'btn', href: ctx.api.url(path, { ...ctx.params, format: 'pdf' }), download: `logbook-${cid}.pdf`,
    title: 'A plain landscape table for a tax office: times in the car’s zone, the warnings about missing kilometres and the totals per purpose.',
  }, icon('download', 'ico--sm'), 'Download PDF');
  const head = pageHead({
    eyebrow: car.name, title: 'Logbook',
    sub: [period.label, `times in ${tz.replace(/_/g, ' ')}`, 'distances by odometer'],
    purpose: 'Every drive of the period with its odometer readings, addresses and purpose, numbered for a tax or expense claim. The continuity check shows where kilometres were driven without a log, so the rows add up to the odometer.',
    actions: [pdfLink],
  });

  /* ---------- filters */
  const segBtns = {};
  const seg = h('div', { class: 'seg lb-seg', role: 'group', 'aria-label': 'Show drives with this purpose' },
    ['all', ...ORDER].map((k) => (segBtns[k] = h('button', {
      class: ['seg-btn', k !== 'all' && `lb-p--${k}`], type: 'button', 'aria-pressed': String(filter === k),
      title: k === 'all' ? 'Every drive of the period' : PURPOSES[k].text,
      onclick: () => setFilter(k),
    }, k !== 'all' ? h('span', { class: 'dot', 'aria-hidden': 'true' }) : null, k === 'all' ? 'All' : PURPOSES[k].label, h('span', { class: 'count' })))));
  const filters = h('section', { class: 'panel panel-pad lb-filters', 'aria-label': 'What the logbook covers' },
    h('div', { class: 'filter-bar' },
      h('button', {
        class: 'btn', type: 'button', cover: 'dt.filter.time_range', onclick: (e) => openPeriodPicker(e.currentTarget),
        title: 'The period of the logbook (the same picker as in the top bar). This year is the default; any year or range works. A drive belongs to it when it started in it.',
      }, icon('calendar', 'ico--sm'), period.label, icon('chev-d', 'ico--sm')),
      h('button', {
        class: 'btn', type: 'button', cover: 'dt.control.per_car', onclick: openCarSwitcher,
        title: 'A logbook is kept per car. Switch to another car; the period stays.',
      }, icon('car', 'ico--sm'), car.name, icon('chev-d', 'ico--sm')),
      h('span', { class: 'lb-filter-label' }, 'Purpose'), seg),
    h('p', { class: 'note' }, icon('info', 'ico--sm'),
      ' One car per logbook, as a tax office asks. The purpose filter narrows the table and its CSV only; totals, numbers and the continuity check always cover the whole period.'));

  /* ---------- totals per purpose and export */
  const totalsSlot = h('div', { class: 'lb-totals' });
  const exportCsv = h('a', { class: 'btn btn--sm', download: '', title: 'Every drive of the period as the server writes it (metric, times in UTC epoch ms), whatever the purpose filter.' },
    icon('download', 'ico--sm'), 'Full CSV');
  const exportPdf = h('a', { class: 'btn btn--sm', href: pdfLink.href, download: `logbook-${cid}.pdf`, title: pdfLink.title }, icon('download', 'ico--sm'), 'PDF');
  const exportPanel = panel({
    title: 'Totals per purpose', icon: 'book', cls: 'lb-export', cover: 'dt.export.csv',
    side: h('div', { class: 'lb-export-btns' }, exportCsv, exportPdf),
  }, totalsSlot,
  h('p', { class: 'note' }, 'The table below has its own Download CSV (in your units, with the filter applied). The full CSV and the PDF hold every drive of the period.'));

  /* ---------- continuity */
  const contSlot = h('div');
  const contPanel = panel({ title: 'Continuity check', icon: 'odo', cls: 'lb-cont' }, contSlot);

  function paintTotals() {
    const rows = data?.rows || [];
    const tot = data?.totals_by_purpose || {};
    const all = ORDER.reduce((s, k) => s + (tot[k]?.distance_km || 0), 0);
    for (const k of ['all', ...ORDER]) {
      const n = k === 'all' ? rows.length : rows.filter((r) => (r.purpose || 'none') === k).length;
      segBtns[k].querySelector('.count').textContent = formatNumber(n);
    }
    replace(totalsSlot, ORDER.map((k) => {
      const t = tot[k] || { drives: 0, distance_km: 0 };
      const share = all > 0 ? (100 * t.distance_km) / all : 0;
      return h('div', { class: ['lb-total', `lb-p--${k}`, !t.drives && 'is-zero'] },
        h('div', { class: 'lb-total-head' }, purposeChip(k === 'none' ? null : k),
          h('span', { class: 'lb-total-km' }, q(t.distance_km, 'distance', { digits: 1 }))),
        h('div', { class: 'bar lb-bar', vars: { '--w': `${share.toFixed(2)}%` }, role: 'img', 'aria-label': `${formatNumber(share, { maximumFractionDigits: 1 })} % of the distance` }, h('i')),
        h('span', { class: 'explain' }, `${formatNumber(t.drives)} ${t.drives === 1 ? 'drive' : 'drives'} · ${formatNumber(share, { maximumFractionDigits: 1 })} % of the distance`));
    }));
    const unset = tot.none?.drives || 0;
    if (unset && rows.length) {
      totalsSlot.append(h('p', { class: 'note lb-unset' }, icon('warn', 'ico--sm'),
        ` ${formatNumber(unset)} ${unset === 1 ? 'drive has' : 'drives have'} no purpose yet. `,
        admin ? 'Give them one in the table, or add a rule below.' : 'An admin can give them one, or add a rule.'));
    }
  }

  const prevOf = (r) => (data?.rows || []).find((x) => x.seq === r.seq - 1) || null;
  const gapHref = (r) => {
    const p = prevOf(r);
    const from = (p?.end_ms ?? r.start_ms - 6 * 60 * MINUTE) - 10 * MINUTE;
    return state.buildUrl('timeline', { car: cid, period: { from, to: r.start_ms + 10 * MINUTE } });
  };
  const hasGap = (r) => r.continuity_gap_km != null && r.continuity_gap_km > GAP_KM;

  function paintContinuity() {
    const rows = data?.rows || [];
    if (!rows.length) { replace(contSlot, h('p', { class: 'note' }, 'Nothing to check: no drive in this period.')); return; }
    const first = rows[0], last = rows[rows.length - 1];
    const logged = rows.reduce((s, r) => s + (r.distance_km || 0), 0);
    const gaps = rows.filter(hasGap);
    const missing = gaps.reduce((s, r) => s + r.continuity_gap_km, 0);
    const span = last.end_odometer_km != null && first.start_odometer_km != null ? last.end_odometer_km - first.start_odometer_km : null;
    const worst = [...gaps].sort((a, b) => b.continuity_gap_km - a.continuity_gap_km).slice(0, 4);
    replace(contSlot,
      figures([
        metric('Odometer span', span, 'distance', { digits: 1, explain: `From ${qt(first.start_odometer_km, 'distance', { digits: 1 })} at the start of drive 1 to ${qt(last.end_odometer_km, 'distance', { digits: 1 })} at the end of drive ${formatNumber(last.seq)}.` }),
        metric('In the logbook', logged, 'distance', { digits: 1, explain: 'The sum of the drives’ distances (each end odometer minus its start).' }),
        metric('Not logged', missing, 'distance', {
          digits: 1, cls: gaps.length ? 'is-warn' : 'is-ok',
          explain: gaps.length ? `${formatNumber(gaps.length)} ${gaps.length === 1 ? 'place' : 'places'} where the odometer moved between two drives with no log (more than 50 m).` : 'Every drive starts where the one before ended: the rows add up to the odometer.',
        }),
      ]),
      worst.length ? h('ul', { class: 'lb-gaps', 'aria-label': 'The largest missing stretches' }, worst.map((r) => h('li', null,
        icon('warn', 'ico--sm'),
        h('span', null, q(r.continuity_gap_km, 'distance', { digits: 1 }), ` before drive ${formatNumber(r.seq)}, ${formatDate(r.start_ms, tz, { year: true })}`),
        h('a', { class: 'panel-link', href: gapHref(r), title: 'What the timeline knows about this stretch' }, 'Timeline', icon('chev-r', 'ico--sm'))))) : null,
      h('p', { class: 'note' }, 'Missing kilometres come from time without a log: the recorder was off, or the car drove before it was installed. They are flagged, never filled in, so the logbook stays truthful.'));
  }

  /* ---------- the logbook table */
  const placeCell = (name, address, pid) => h('span', { class: 'cell-text lb-where' },
    pid ? h('a', { class: 'main link', href: state.buildUrl('places', { query: { place: pid } }), title: 'This place in Places' }, name)
      : h('span', { class: 'main', title: address || null }, address || 'Address not known'),
    pid && address && address !== name ? h('span', { class: 'sub', title: address }, address) : !pid ? h('span', { class: 'sub' }, 'not a saved place') : null);

  function purposeCell(r) {
    const src = r.purpose_source === 'rule' ? 'by a rule' : r.purpose_source === 'owner' ? 'set by hand' : null;
    return h('span', { class: 'cell-text lb-pcell' },
      h('span', { class: 'lb-prow' }, purposeChip(r.purpose),
        admin ? h('button', { class: 'btn btn--sm btn--ghost btn--icon lb-edit', type: 'button', onclick: () => editPurpose(r), title: r.purpose ? 'Change the purpose, relation and note of this drive' : 'Set the purpose of this drive', 'aria-label': `Purpose of drive ${r.seq}` },
          icon('settings', 'ico--sm')) : null),
      src || r.relation ? h('span', { class: 'sub' }, [r.relation ? h('span', { class: 'lb-rel' }, r.relation) : null, r.relation && src ? ' · ' : null, src]) : null,
      r.note ? h('span', { class: 'sub lb-note', title: r.note }, r.note) : null);
  }

  function continuityCell(r) {
    if (r.continuity_gap_km == null) return h('span', { class: 'cell-text' }, h('span', { class: 'faint', title: 'The first drive of the period has nothing before it to compare with.' }, 'first'));
    if (!hasGap(r)) return h('span', { class: 'cell-text lb-ok', title: 'This drive starts where the one before ended.' }, h('span', null, icon('check', 'ico--sm'), ' adds up'));
    return h('span', { class: 'cell-text' },
      h('a', { class: 'lb-gap', href: gapHref(r), title: 'The odometer moved between the drive before and this one while nothing was logged. Opens the timeline at that time.' },
        icon('warn', 'ico--sm'), q(r.continuity_gap_km, 'distance', { digits: 1 })),
      h('span', { class: 'sub' }, 'not logged before it'));
  }

  const odo = (v) => (v == null ? h('span', { class: 'faint' }, DASH) : h('span', { title: `${qt(v, 'distance', { digits: 3 })} (to the metre)` }, q(v, 'distance', { digits: 1 })));
  const columns = [
    {
      key: 'seq', label: '#', num: true, sort: true, cover: 'dt.col.id', title: 'The number of the drive in this period, from 1. The PDF and the CSV use the same numbers. Opens the drive.',
      render: (r) => h('a', { class: 'link lb-seq', href: state.buildUrl('drive', { car: cid, drive: r.drive }), title: 'Open this drive' }, formatNumber(r.seq)),
      csv: { label: 'Number', value: (r) => r.seq },
    },
    {
      key: 'start_ms', label: 'When', sort: true, cover: ['dt.col.start_time', 'dt.col.end_time'], title: 'The day, the start time and the end time of the drive, in the car’s time zone.',
      render: (r) => h('span', { class: 'cell-text' }, h('span', { class: 'main nowrap' }, formatDate(r.start_ms, tz, { year: true })),
        h('span', { class: 'sub nowrap' }, h('span', { title: 'Start' }, formatTime(r.start_ms, tz)), ' – ',
          r.end_ms == null ? h('span', { class: 'lb-now' }, 'driving now')
            : h('span', { title: 'End' }, formatTime(r.end_ms, tz), dayKey(r.end_ms, tz) !== dayKey(r.start_ms, tz) ? ` (${formatDate(r.end_ms, tz, { weekday: false })})` : ''))),
      csv: false,
    },
    {
      key: 'start_odometer_km', label: 'Odometer', num: true, sort: true, cover: ['dt.col.start_odometer', 'dt.col.end_odometer'], title: 'The odometer at the start (top) and the end (below) of the drive, kept to the metre: hover a value, or see the CSV.',
      render: (r) => h('span', { class: 'cell-text' }, h('span', { class: 'main' }, odo(r.start_odometer_km)), h('span', { class: 'sub' }, h('span', { class: 'lb-arrow', 'aria-label': 'to' }, '→ '), odo(r.end_odometer_km))),
      csv: false,
    },
    {
      key: 'from', label: 'From', sort: true, value: (r) => r.start_place || r.start_address, cover: 'dt.col.start_address', title: 'Where the drive started: the saved place, else the address (looked up on this server).',
      render: (r) => placeCell(r.start_place, r.start_address, r.start_place_id),
      csv: false,
    },
    {
      key: 'to', label: 'To', sort: true, value: (r) => r.end_place || r.end_address, cover: 'dt.col.end_address', title: 'Where the drive ended: the saved place, else the address.',
      render: (r) => placeCell(r.end_place, r.end_address, r.end_place_id),
      csv: false,
    },
    {
      key: 'distance_km', label: 'Distance', num: true, sort: true, cover: ['dt.col.distance', 'dt.col.duration'], title: 'The distance (end odometer minus start odometer) and, below it, how long the drive took.',
      render: (r) => h('span', { class: 'cell-text' }, h('span', { class: 'main' }, q(r.distance_km, 'distance', { digits: 1 })),
        h('span', { class: 'sub nowrap', title: 'Duration' }, r.duration_ms == null ? DASH : formatDuration(r.duration_ms), r.end_ms == null ? ' so far' : '')),
      csv: false,
    },
    {
      key: 'purpose', label: 'Purpose', sort: true, value: (r) => PURPOSES[r.purpose || 'none'].label, title: 'Business, commute or private, with the relation (who or what for) and a note. Set by an admin or by a rule; the owner’s choice wins.',
      render: purposeCell, csv: false,
    },
    {
      key: 'continuity_gap_km', label: 'Continuity', num: true, sort: true, title: 'The continuity check: kilometres the odometer moved since the drive before, with no log. Opens the timeline at that time.',
      render: continuityCell, csv: false,
    },
  ];
  const csvColumns = [
    { label: 'Number', value: (r) => r.seq },
    timeColumn('Start', 'start_ms', tz), timeColumn('End', 'end_ms', tz),
    column('Odometer start', 'start_odometer_km', 'distance', { digits: 3 }), column('Odometer end', 'end_odometer_km', 'distance', { digits: 3 }),
    { label: 'From place', value: (r) => r.start_place || '' }, { label: 'From address', value: (r) => r.start_address || '' },
    { label: 'To place', value: (r) => r.end_place || '' }, { label: 'To address', value: (r) => r.end_address || '' },
    column('Distance', 'distance_km', 'distance', { digits: 3 }),
    { label: 'Duration (minutes)', value: (r) => (r.duration_ms == null ? '' : Math.round(r.duration_ms / 6000) / 10) },
    { label: 'Purpose', value: (r) => r.purpose || '' }, { label: 'Purpose set by', value: (r) => r.purpose_source || '' },
    { label: 'Relation', value: (r) => r.relation || '' }, { label: 'Note', value: (r) => r.note || '' },
    column('Not logged before', 'continuity_gap_km', 'distance', { digits: 3 }),
  ];

  const shownRows = () => (data?.rows || []).filter((r) => filter === 'all' || (r.purpose || 'none') === filter);
  const table = dataTable(ctx, {
    title: 'Drives', icon: 'route', rows: [], cls: 'lb-table', cover: 'dt.table', csvName: 'logbook',
    columns, sort: { key: 'seq', dir: 'asc' },
    csv: { columns: csvColumns },
    rowHref: (r) => state.buildUrl('drive', { car: cid, drive: r.drive }),
    note: 'Oldest first, as a logbook is kept. Each row opens its drive; a place opens it in Places; a warning in the last column opens the timeline where kilometres went unlogged.',
    empty: () => (data?.rows?.length
      ? emptyState({ icon: 'book', title: filter === 'none' ? 'Every drive has a purpose' : `No ${PURPOSES[filter].label.toLowerCase()} drives`, text: filter === 'none' ? `All ${formatNumber(data.rows.length)} drives of this period have a purpose. Choose All to see them.` : `None of the ${formatNumber(data.rows.length)} drives of this period is ${PURPOSES[filter].label.toLowerCase()}. Choose All to see them.` })
      : emptyState({
        icon: 'book', title: 'No drives in this period',
        text: period.preset === 'all' ? `Nothing has been recorded for ${car.name} yet. Drives show here once the car drives while recording, or after a TeslaMate import.`
          : `${car.name} did not start a drive in ${period.label.toLowerCase()}. Choose another year or All time with the period button.`,
      })),
  });

  function setFilter(k) {
    filter = k;
    for (const [key, b] of Object.entries(segBtns)) b.setAttribute('aria-pressed', String(key === k));
    ctx.setQuery({ purpose: k === 'all' ? null : k });
    table.setRows(shownRows());
  }

  /* ---------- purpose rules */
  const rulesSlot = h('div', { class: 'lb-rules' });
  const addRuleBtn = admin ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => addRule() }, icon('plus', 'ico--sm'), 'Add a rule') : null;
  const rulesPanel = panel({ title: 'Purpose rules', icon: 'flag', cls: 'lb-rules-panel', side: addRuleBtn },
    h('p', { class: 'note' }, 'A rule gives a purpose to every drive between two places on some days, such as Home to Office on weekdays is commute. Rules fill in drives nobody has set; a purpose set by hand always wins. Rules and purposes are stored apart from the drives, so they survive a rebuild of the history.'),
    rulesSlot);
  const placeName = (pid) => (pid == null ? 'anywhere that is not a saved place' : (places.find((p) => p.id === pid)?.name || `place ${pid}`));

  function paintRules() {
    if (!rules.length) {
      replace(rulesSlot, emptyState({ icon: 'flag', title: 'No rules yet', text: admin ? 'Add one for the trips you make every week, and their drives get a purpose by themselves.' : 'An admin can add rules for trips that repeat.' }));
      return;
    }
    const used = new Map();
    for (const r of data?.rows || []) if (r.purpose_rule) used.set(r.purpose_rule, (used.get(r.purpose_rule) || 0) + 1);
    replace(rulesSlot, h('ul', { class: 'lb-rule-list' }, rules.map((x) => h('li', { class: 'lb-rule' },
      purposeChip(x.purpose),
      h('span', { class: 'lb-rule-text' },
        h('span', null, 'From ', h('b', null, placeName(x.from_place)), ' to ', h('b', null, placeName(x.to_place))),
        h('span', { class: 'sub' }, `${dayLabel(x.days)} · ${formatNumber(used.get(x.id) || 0)} ${used.get(x.id) === 1 ? 'drive' : 'drives'} in this period`)),
      admin ? h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => removeRule(x), title: 'Remove this rule. Drives it named lose that purpose; purposes set by hand stay.' }, icon('x', 'ico--sm'), 'Remove') : null))));
  }

  /* ---------- loading */
  async function loadLogbook() {
    data = await ctx.api.get(path, ctx.params);
    if (!ctx.alive()) return;
    exportCsv.href = ctx.api.csvUrl(path, ctx.params);
    paintTotals();
    paintContinuity();
    table.setRows(shownRows());
    paintRules();
  }
  async function loadRules() {
    const [rl, pl] = await Promise.all([
      ctx.api.get(rulesPath, { limit: 500 }).catch(() => ({ items: [] })),
      ctx.api.list('places', { sort: 'name' }, { limit: 500 }).all({ max: 2000 }).catch(() => []),
    ]);
    if (!ctx.alive()) return;
    rules = rl.items || [];
    places = pl;
    paintRules();
  }

  replace(ctx.view, root);
  root.append(head, filters, h('div', { class: 'lb-summary' }, exportPanel, contPanel), table.el, rulesPanel);

  try {
    await Promise.all([loadLogbook(), loadRules()]);
  } catch (err) {
    if (ctx.alive()) ctx.fail(err, { retry: () => ctx.reload() });
    return;
  }
  if (!ctx.alive()) return;

  /* ---------- editing (admin) */
  async function editPurpose(r) {
    const purpose = field({
      label: 'Purpose', name: 'purpose', type: 'select', path: 'purpose', value: r.purpose || 'business', required: true,
      options: ['business', 'commute', 'private'].map((k) => [k, PURPOSES[k].label]),
      hint: 'Business and commute are what most tax offices ask to see apart from private driving.',
    });
    const relation = field({ label: 'Relation', name: 'relation', path: 'relation', value: r.relation || '', hint: 'Who or what the drive was for: a customer, a project, an employer. Up to 100 characters.', attrs: { maxlength: '100' } });
    const noteF = field({ label: 'Note', name: 'note', type: 'textarea', path: 'note', value: r.note || '', hint: 'Anything a reader of the logbook needs. Up to 500 characters.', attrs: { maxlength: '500', rows: '3' } });
    const removeBtn = r.purpose_source === 'owner' ? h('button', {
      class: 'btn btn--ghost', type: 'button',
      onclick: async () => {
        try {
          await ctx.api.del(`cars/${enc(cid)}/drives/${enc(r.drive)}/purpose`);
          flash(`Drive ${formatNumber(r.seq)} has no purpose set by hand now${rules.length ? '; a rule may still name it' : ''}.`);
          f.el.closest('dialog')?.close();
          refresh();
        } catch (err) { showApiError(f, err); }
      },
    }, 'Remove the purpose') : null;
    const f = form({
      fields: [purpose, relation, noteF], submit: 'Save', secondary: removeBtn,
      async onSubmit(v) {
        await ctx.api.put(`cars/${enc(cid)}/drives/${enc(r.drive)}/purpose`, {
          purpose: v.purpose, relation: v.relation.trim() || null, note: v.note.trim() || null,
        });
        flash(`Drive ${formatNumber(r.seq)} is ${PURPOSES[v.purpose].label.toLowerCase()} now.`);
        f.el.closest('dialog')?.close();
        refresh();
      },
    });
    await openDialog({
      title: `Drive ${formatNumber(r.seq)}, ${formatDate(r.start_ms, tz, { year: true })} ${formatTime(r.start_ms, tz)}`,
      body: [h('p', { class: 'note' }, `From ${r.start_place || r.start_address || 'an unknown address'} to ${r.end_place || r.end_address || 'an unknown address'}, ${qt(r.distance_km, 'distance', { digits: 1 })}.`,
        r.purpose_source === 'rule' ? ' A rule gives it its purpose now; saving here sets it by hand, which wins over the rule.' : ''), f.el],
      actions: [],
    });
  }

  async function addRule() {
    const opts = [['', 'Anywhere that is not a saved place'], ...places.map((p) => [p.id, p.name])];
    const from = field({ label: 'From', name: 'from_place', type: 'select', path: 'from_place', options: opts, value: places.find((p) => p.kind === 'home')?.id || '' });
    const to = field({ label: 'To', name: 'to_place', type: 'select', path: 'to_place', options: opts, value: places.find((p) => p.kind === 'work')?.id || '' });
    const days = field({ label: 'Days', name: 'days', type: 'select', path: 'days', options: DAYS, value: 'mon-fri', hint: 'Days in the car’s time zone, by the start of the drive.' });
    const purpose = field({ label: 'Purpose', name: 'purpose', type: 'select', path: 'purpose', options: ['commute', 'business', 'private'].map((k) => [k, PURPOSES[k].label]), value: 'commute' });
    const f = form({
      fields: [from, to, days, purpose], submit: 'Add the rule',
      async onSubmit(v) {
        await ctx.api.post(rulesPath, { from_place: v.from_place || null, to_place: v.to_place || null, days: v.days, purpose: v.purpose });
        flash('Rule added. Matching drives without a purpose set by hand have it now.');
        f.el.closest('dialog')?.close();
        refresh();
      },
    });
    await openDialog({ title: 'Add a purpose rule', body: [h('p', { class: 'note' }, 'Drives that start at From and end at To on these days get this purpose, unless one was set by hand.'), f.el], actions: [] });
  }

  async function removeRule(x) {
    const ok = await confirmDialog(`Drives from ${placeName(x.from_place)} to ${placeName(x.to_place)} lose the purpose this rule gave them. Purposes set by hand stay.`, { title: 'Remove this rule?', ok: 'Remove', danger: true });
    if (!ok || !ctx.alive()) return;
    try {
      await ctx.api.del(`${rulesPath}/${enc(x.id)}`);
      flash('Rule removed.');
      refresh();
    } catch (err) { flash(err?.message || 'The rule could not be removed.', { kind: 'error' }); }
  }

  let busy = null;
  function refresh() {
    if (busy) return busy;
    busy = Promise.all([loadLogbook(), loadRules()]).catch((err) => { if (ctx.alive()) flash(err?.message || 'The logbook could not be loaded again.', { kind: 'error' }); })
      .finally(() => { busy = null; });
    return busy;
  }

  ctx.events('rows', (ev) => {
    const d = ev?.data || {};
    if (d.car === cid && (d.kinds || []).includes('drives')) refresh();
  });

  ctx.ready();
}
