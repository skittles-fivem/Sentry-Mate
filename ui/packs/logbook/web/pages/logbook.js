// Sentry Mate, "Logbook" UI pack: Logbook (/cars/{car}/logbook).
//
// A trip log for tax and expense claims, for one car and the shared period (a tax year by default): every
// drive numbered in order, with start and end time, odometer at both ends, where it went, distance,
// duration and its purpose (business, private, commute) with relation and note. A continuity check flags
// kilometres the odometer moved between two drives that no drive covers, so the rows add up.
// Data:
//   GET /cars/{car}/logbook                  a page of the rows (limit, after or offset; at_ms for Jump to date;
//                                            purpose, gaps, q, dir on the server), each with the totals per
//                                            purpose, the counts, the continuity check and the rows per rule of the
//                                            whole period; format=csv (every row, or every row the filters leave)
//                                            and format=pdf for the exports
//   PUT, DELETE /cars/{car}/drives/{d}/purpose   admin: the purpose, relation and note of one drive
//   GET, POST, DELETE /cars/{car}/purpose-rules  rules such as "Home to Work on weekdays is commute"
//   GET /places                              names for the rules and the rule form
// Filters live in the address (?purpose=&gaps=1&order=desc), so a link or Back shows the same rows. The period is
// the frame's. The log is a box of about ten rows that scrolls under its head; only the rows in view are drawn
// (core/table.js windowLedger). Live: a drive that starts or ends ("rows" events naming drives) loads the rows in
// view again.

import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatDate, formatTime, formatDuration, periodLabel, MINUTE } from '../shared/time.js';
import { formatNumber, compareText } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, icon, empty, errorView, flag, button, sep, loadingBlock } from '../core/ui.js';
import { dataTable, windowLedger } from '../core/table.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { field, form, choice, switchField } from '../core/form.js';
import { column, csvFilename } from '../shared/csv.js';
import { zoneLabel } from '../core/format.js';
import { spanHref } from '../core/period.js';

export const id = 'logbook';
export const css = true;
export const title = () => 'Logbook';

/** The purposes, in the order they are shown, with their words. */
const PURPOSES = ['business', 'private', 'commute'];
const PURPOSE_WORD = { business: 'Business', private: 'Private', commute: 'Commute', none: 'Not set' };
const PURPOSE_NOTE = {
  business: 'Drives for work: to a customer, a supplier or a site.',
  private: 'Drives for yourself.',
  commute: 'Between home and the usual place of work.',
  none: 'Drives without a purpose yet: set one per drive, or add a rule below.',
};
const FILTERS = [['', 'All'], ['business', 'Business'], ['private', 'Private'], ['commute', 'Commute'], ['none', 'Not set']];
const DAYS = [['mon-fri', 'Monday to Friday'], ['sat,sun', 'Saturday and Sunday'], ['all', 'Every day'],
  ['mon', 'Mondays'], ['tue', 'Tuesdays'], ['wed', 'Wednesdays'], ['thu', 'Thursdays'], ['fri', 'Fridays'], ['sat', 'Saturdays'], ['sun', 'Sundays']];
const dayWords = (d) => (DAYS.find(([k]) => k === d) || [d, d])[1];
/** A gap shorter than this (50 m) is rounding in the odometer, not a missing drive. */
const GAP_MIN_KM = 0.05;
const oneOf = (list, v, dflt) => (list.some(([k]) => k === v) ? v : dflt);

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const carName = car.name || cid;
  const admin = !!(ctx.me && ctx.me.role === 'admin');
  const path = api.apiPath`cars/${cid}/logbook`;
  const now = api.now();
  const f = { purpose: oneOf(FILTERS, ctx.query.purpose, ''), gaps: ctx.query.gaps === '1', order: ctx.query.order === 'desc' ? 'desc' : 'asc' };

  // the last page of the log: the totals, counts, continuity check and rows per rule of the whole period
  let data = { count: 0, totals_by_purpose: {}, counts_by_purpose: {}, continuity: null, rules_used: {} };
  let places = [];
  let rules = [];

  /* ---------------------------------------------------------------- head */

  const asOf = h('p', { class: 'lb-asof' });
  const drawAsOf = () => replace(asOf, 'As of ', formatTime(api.now(), tz), ' · loads again by itself when a drive starts or ends.');
  drawAsOf();
  const pdfHref = () => ctx.api.url(path, { ...ctx.params, format: 'pdf' });
  const csvHref = () => ctx.api.csvUrl(path, ctx.params);
  const exportButtons = (cls) => h('div', { class: ['lb-export-buttons', cls], cover: 'dt.export.csv' },
    h('a', { class: 'button button-small', href: csvHref(), download: '', title: 'Every row of the period as a spreadsheet file, metric units, times as epoch ms' }, icon('download'), 'Download CSV'),
    h('a', { class: 'button button-quiet button-small', href: pdfHref(), download: '', title: 'A printable landscape table with the continuity warnings and the totals per purpose' }, icon('download'), 'Download PDF'));

  ctx.main.append(pageHead({
    eyebrow: h('span', { class: 'lb-scope' }, 'Driving', sep(), h('span', { class: 'lb-car-name' }, carName)),
    title: h('span', null, 'Logbook of ', h('span', { class: 'lb-title-car' }, carName)),
    deck: 'Every drive of the period in order, with odometer readings, addresses and a purpose, ready for a tax or expense claim. The continuity check shows where the odometer moved with no drive recorded, so the rows add up.',
    aside: h('div', { class: 'lb-head-aside' }, exportButtons('lb-export-head'), asOf),
  }));

  /* ---------------------------------------------------------------- filters */

  const purposeChoice = choice({
    label: 'Purpose', name: 'purpose', value: f.purpose, options: FILTERS,
    onchange: (v) => { f.purpose = v; ctx.setQuery({ purpose: v || null }); drawRows(); },
  });
  const gapSwitch = switchField({
    label: 'Only drives after kilometres not logged', checked: f.gaps,
    hint: 'Shows the drives the continuity check flags, to explain them.',
    onchange: (on) => { f.gaps = on; ctx.setQuery({ gaps: on ? '1' : null }); drawRows(); },
  });
  ctx.main.append(h('section', { class: 'lb-filters', 'aria-label': 'What the log covers' },
    h('div', { class: 'lb-filter', cover: 'dt.filter.time_range' },
      h('span', { class: 'field-label' }, 'Period'),
      h('p', { class: 'lb-filter-value' }, periodLabel(ctx.period, tz, now)),
      h('p', { class: 'field-hint' }, 'A drive belongs to the period it started in. Times in ', zoneLabel(tz), '. “This year” is the calendar year; any dates work.'),
      button('Change the period', { cls: 'button-small', iconName: 'calendar', onclick: () => document.querySelector('.period-trigger')?.click() })),
    h('div', { class: 'lb-filter', cover: 'dt.control.per_car' },
      h('span', { class: 'field-label' }, 'Car'),
      h('p', { class: 'lb-filter-value' }, carName, car.vin_prefix ? h('span', { class: 'lb-vin mono' }, car.vin_prefix) : null),
      h('p', { class: 'field-hint' }, 'One log per car, its name on the log and on both exports. Any number of cars: pick another one here.'),
      button('Choose another car', { cls: 'button-small', iconName: 'search', onclick: () => document.querySelector('.switcher-trigger')?.click() })),
    h('div', { class: 'lb-filter lb-filter-rows' },
      purposeChoice,
      h('p', { class: 'field-hint' }, 'Only hides rows here and in the table’s CSV: the numbers stay those of the whole log, and the exports above hold every row.'),
      gapSwitch)));

  /* ---------------------------------------------------------------- totals and export */

  const totalsBox = h('div', { class: 'lb-totals' }, loadingBlock(3));
  const checkBox = h('div', { class: 'lb-check' });
  ctx.main.append(h('section', { class: 'block lb-summary', 'aria-labelledby': 'lb-sum-title' },
    h('div', { class: 'block-head' },
      h('h2', { class: 'label', id: 'lb-sum-title' }, 'Totals per purpose', h('span', { class: 'label-note' }, `${carName}, ${periodLabel(ctx.period, tz, now)}`)),
      h('div', { class: 'block-actions' }, exportButtons('lb-export-sum'))),
    totalsBox, checkBox,
    h('p', { class: 'note lb-export-note' }, icon('info'),
      h('span', null, 'The CSV holds every row of the period with exact odometer readings (to the metre, in km) and times as epoch milliseconds; the PDF is a landscape table in ', zoneLabel(tz),
        ' with the continuity warnings and these totals. The table’s own Download CSV holds every row its filters and search leave, as the server writes it.'))));

  function drawTotals() {
    const t = data.totals_by_purpose || {};
    const all = ['business', 'private', 'commute', 'none'];
    const sumKm = all.reduce((s, k) => s + ((t[k] && t[k].distance_km) || 0), 0);
    const nDrives = all.reduce((s, k) => s + ((t[k] && t[k].drives) || 0), 0);
    replace(totalsBox, h('div', { class: 'lb-purpose-grid' }, all.map((k) => {
      const x = t[k] || { drives: 0, distance_km: 0 };
      const share = sumKm > 0 ? (100 * (x.distance_km || 0)) / sumKm : 0;
      return h('button', {
        type: 'button', class: ['lb-purpose-card', `lb-p-${k}`, { 'is-on': f.purpose === k }],
        'aria-pressed': String(f.purpose === k), title: `Show only ${PURPOSE_WORD[k].toLowerCase()} drives in the table`,
        onclick: () => { f.purpose = f.purpose === k ? '' : k; purposeChoice.set(f.purpose); ctx.setQuery({ purpose: f.purpose || null }); drawRows(); drawTotals(); },
      },
      h('span', { class: 'lb-card-name' }, h('span', { class: 'lb-dot', 'aria-hidden': 'true' }), PURPOSE_WORD[k]),
      h('span', { class: 'lb-card-value' }, q(x.distance_km, 'distance')),
      h('span', { class: 'lb-card-sub' }, `${formatNumber(x.drives)} ${x.drives === 1 ? 'drive' : 'drives'} · ${formatNumber(share, { maximumFractionDigits: 1 })} % of the distance`),
      h('span', { class: 'lb-bar', 'aria-hidden': 'true' }, h('span', { class: 'lb-bar-fill', vars: { '--w': `${share.toFixed(2)}%` } })),
      h('span', { class: 'lb-card-note' }, PURPOSE_NOTE[k]));
    })));

    const c = data.continuity || {};
    const n = data.count || 0;
    const odoKm = n && c.last_end_odometer_km != null && c.first_start_odometer_km != null ? c.last_end_odometer_km - c.first_start_odometer_km : null;
    const gapCount = c.gaps || 0;
    const gapKm = c.missing_km || 0;
    const byRule = Object.values(data.rules_used || {}).reduce((s, x) => s + x, 0);
    const byOwner = Math.max(0, n - ((data.counts_by_purpose || {}).none || 0) - byRule);
    replace(checkBox, h('div', { class: 'figs lb-check-figs' },
      figure('Odometer moved', q(odoKm, 'distance', { why: n ? 'no reading' : 'no drive in the period' }),
        n ? `From ${format(c.first_start_odometer_km, 'odometer', { digits: 1 })} at the start of drive 1 to ${format(c.last_end_odometer_km, 'odometer', { digits: 1 })} at the end of drive ${formatNumber(c.last_seq)}.` : 'The odometer at the start of the first drive to the end of the last.'),
      figure('In the drives', q(sumKm, 'distance'), `The sum of the ${formatNumber(nDrives)} ${nDrives === 1 ? 'drive' : 'drives'} below, every purpose together.`),
      figure('Not logged between drives', h('span', { class: ['lb-gap-total', { 'is-warn': gapKm > 0 }] }, q(gapKm, 'distance')),
        gapCount
          ? `${formatNumber(gapCount)} ${gapCount === 1 ? 'place' : 'places'} where a drive starts further on than the one before ended (marked in the table). Odometer moved = in the drives + not logged.`
          : n > 1 ? 'Every drive starts where the one before ended: the rows add up.' : 'Needs two drives to compare.'),
      figure('Purposes given', h('span', null, formatNumber(byOwner + byRule), h('span', { class: 'lb-of' }, ` of ${formatNumber(n)}`)),
        `${formatNumber(byOwner)} set by hand, ${formatNumber(byRule)} by a rule. A purpose set by hand wins over a rule and survives rebuilds.`)));
  }

  /* ---------------------------------------------------------------- the log */

  const placeName = (pid) => (places.find((p) => p.id === pid) || {}).name || pid || 'anywhere';
  const ruleOf = (rid) => rules.find((r) => r.id === rid);
  // The window rides as a one-off span (core/period.js), so the shared period, Back and the drive links keep the year.
  const timelineHref = (a, b) => spanHref('timeline', { car: cid, from: a - 10 * MINUTE, to: b + 10 * MINUTE });
  const driveHref = (r) => state.buildUrl('drive', { car: cid, drive: r.drive, period: ctx.state.period });

  function whereCell(name, address) {
    return h('div', { class: 'cell-main lb-where' },
      h('span', { class: 'lb-where-main' }, name || address || 'Address not known'),
      name && address ? h('span', { class: 'cell-sub' }, address) : !name ? h('span', { class: 'cell-sub' }, 'no saved place here') : null);
  }

  function purposeCell(r) {
    const k = r.purpose || 'none';
    const rule = r.purpose_source === 'rule' ? ruleOf(r.purpose_rule) : null;
    const how = r.purpose_source === 'owner' ? 'set by hand'
      : r.purpose_source === 'rule' ? (rule ? `by rule: ${placeName(rule.from_place)} → ${placeName(rule.to_place)}, ${dayWords(rule.days)}` : 'by a rule')
        : null;
    const more = [how, r.relation, r.note].filter(Boolean);
    return h('div', { class: 'cell-main lb-purpose-cell' },
      h('span', { class: 'lb-purpose-line' },
        h('span', { class: ['lb-purpose', `lb-p-${k}`] }, h('span', { class: 'lb-dot', 'aria-hidden': 'true' }), PURPOSE_WORD[k]),
        admin ? h('button', { type: 'button', class: 'lb-edit', onclick: () => editPurpose(r), title: 'Set the purpose, relation and note of this drive' },
          icon('settings'), r.purpose_source === 'owner' ? 'Change' : 'Set') : null),
      more.length ? h('span', { class: 'cell-sub lb-purpose-more', title: more.join(' · ') },
        how ? h('span', null, how) : null,
        r.relation ? [how ? ' · ' : null, h('span', { class: 'lb-relation' }, r.relation)] : null,
        r.note ? [how || r.relation ? ' · ' : null, h('span', { class: 'lb-note' }, r.note)] : null) : null);
  }

  function startOdoCell(r) {
    const gap = r.continuity_gap_km;
    return h('div', { class: 'cell-main' },
      h('span', { title: r.start_odometer_km != null ? `${formatNumber(r.start_odometer_km, { maximumFractionDigits: 3 })} km exactly` : null }, q(r.start_odometer_km, 'odometer', { digits: 1, why: 'no reading' })),
      gap != null && gap > GAP_MIN_KM
        ? h('a', { class: 'lb-gap', href: timelineHref(r.prev_end_ms ?? r.start_ms - 6 * 60 * MINUTE, r.start_ms), title: 'The timeline between the drive before and this one' },
          icon('warning'), h('span', null, '+', format(gap, 'distance'), ' not logged before'))
        : gap == null ? h('span', { class: 'cell-sub' }, 'first of the period') : null);
  }

  const columns = [
    {
      key: 'seq', label: 'No.', th: true, num: true, sort: 'seq', firstDir: 'asc', cover: 'dt.col.id', hint: 'The drive’s number in this log (1 is the first drive of the period). Opens the drive.',
      cell: (r) => h('div', { class: 'cell-main' }, h('a', { href: driveHref(r), title: 'This drive in full' }, formatNumber(r.seq)), h('span', { class: 'cell-sub mono lb-id', title: 'The drive’s id' }, `#${String(r.drive).slice(-6)}`)),
      value: (r) => r.seq,
    },
    {
      key: 'start', label: 'Started', sort: 'seq', firstDir: 'asc', cover: 'dt.col.start_time', hint: `Date and time the drive began, in ${zoneLabel(tz)}.`,
      cell: (r) => h('div', { class: 'cell-main lb-when' }, formatDate(r.start_ms, tz, { now, year: true }), h('span', { class: 'cell-sub' }, formatTime(r.start_ms, tz))),
      value: (r) => r.start_ms,
    },
    {
      key: 'end', label: 'Ended', cover: 'dt.col.end_time', hint: 'Time the drive ended (with its date when that is another day).',
      cell: (r) => (r.end_ms == null || r.in_progress
        ? h('div', { class: 'cell-main' }, flag('Driving now', { kind: 'warn' }), h('span', { class: 'cell-sub' }, 'figures so far'))
        : h('div', { class: 'cell-main lb-when' }, formatTime(r.end_ms, tz),
          formatDate(r.end_ms, tz, { year: true }) !== formatDate(r.start_ms, tz, { year: true }) ? h('span', { class: 'cell-sub' }, formatDate(r.end_ms, tz, { now, year: true })) : null)),
      value: (r) => r.end_ms,
    },
    {
      key: 'odo_start', label: 'Odometer at start', num: true, sort: 'odometer', cover: 'dt.col.start_odometer', hint: 'The car’s odometer when the drive began, to 0.1 (hover for the exact reading). A warning means it moved since the drive before ended.',
      cell: startOdoCell, value: (r) => r.start_odometer_km,
    },
    {
      key: 'odo_end', label: 'Odometer at end', num: true, cover: 'dt.col.end_odometer', hint: 'The car’s odometer when the drive ended, to 0.1 (hover for the exact reading).',
      cell: (r) => h('div', { class: 'cell-main' },
        h('span', { title: r.end_odometer_km != null ? `${formatNumber(r.end_odometer_km, { maximumFractionDigits: 3 })} km exactly` : null }, q(r.end_odometer_km, 'odometer', { digits: 1, why: 'no reading' })),
        r.in_progress ? h('span', { class: 'cell-sub' }, 'so far') : null),
      value: (r) => r.end_odometer_km,
    },
    {
      key: 'from', label: 'From', sort: 'from', cover: 'dt.col.start_address', hint: 'Where the drive began: the saved place, and the street address under it.',
      cell: (r) => whereCell(r.start_place, r.start_address), value: (r) => r.start_place || r.start_address,
    },
    {
      key: 'to', label: 'To', sort: 'to', cover: 'dt.col.end_address', hint: 'Where the drive ended: the saved place, and the street address under it.',
      cell: (r) => whereCell(r.end_place, r.end_address), value: (r) => r.end_place || r.end_address,
    },
    {
      key: 'distance', label: 'Distance', num: true, sort: 'distance', cover: 'dt.col.distance', hint: 'Odometer at end minus odometer at start.',
      cell: (r) => q(r.distance_km, 'distance', { why: 'no reading' }), value: (r) => r.distance_km,
    },
    {
      key: 'duration', label: 'Duration', num: true, cover: 'dt.col.duration', hint: 'From the start of the drive to its end, in hours and minutes.',
      cell: (r) => h('span', { class: 'lb-dur' }, formatDuration(r.duration_ms)), value: (r) => r.duration_ms,
    },
    {
      key: 'purpose', label: 'Purpose', sort: 'purpose', hint: 'Business, private or commute, with the relation (customer, employer) and a note. Set by hand, or by a rule.',
      cell: purposeCell, value: (r) => (r.purpose ? PURPOSE_WORD[r.purpose] : null),
    },
  ];


  const orderChoice = choice({
    label: 'Order', name: 'order', value: f.order, options: [['asc', 'Oldest first'], ['desc', 'Newest first']], labelHidden: true,
    onchange: (v) => { f.order = v; ctx.setQuery({ order: v === 'asc' ? null : v }); drawRows(true); },
  });
  const table = windowLedger(ctx, {
    path, params: filterParams(), columns, cls: 'lb-table', cover: ['dt.table'], label: `Logbook of ${carName}`,
    boxLabel: `The drives of ${carName}'s log (scrolls; the arrow keys scroll it)`, itemName: ['drive', 'drives'],
    sort: { key: 'seq', dir: f.order }, firstSort: { key: 'seq', dir: 'asc' }, limit: 50, jump: true, tools: orderChoice,
    search: { label: 'Search the log', placeholder: 'Place, address, relation, note', hint: 'Shows the drives with these words in a place, an address, the relation or the note' },
    caption: 'The number opens the drive; a warning opens the timeline where the kilometres went. The box scrolls: drives load as it comes to them.',
    rowAttrs: (r) => ({ class: [(r.continuity_gap_km || 0) > GAP_MIN_KM ? 'has-gap' : null, r.in_progress ? 'is-live' : null].filter(Boolean).join(' ') || null }),
    onSort: (s2) => { if (s2.key === 'seq' && s2.dir !== f.order) { f.order = s2.dir; orderChoice.set(s2.dir); ctx.setQuery({ order: s2.dir === 'asc' ? null : s2.dir }); } },
    onPage: (pg) => { data = pg; },
    onError: (err) => ctx.flash(`The log did not load: ${err.message || err}`, { kind: 'error' }),
    empty: () => emptyText(),
  });

  function emptyText() {
    if (data.count) {
      const what = [f.purpose ? `${PURPOSE_WORD[f.purpose].toLowerCase()} drives` : null, f.gaps ? 'drives after kilometres not logged' : null].filter(Boolean).join(' that are ');
      return empty({
        title: `No ${what || 'drive'} in this period.`,
        text: `The log has ${formatNumber(data.count)} drives; choose All above${f.gaps ? ' and turn off the continuity switch' : ''}${table.params().q ? ', or clear the search,' : ''} to see them.`,
        action: button('Show every drive', { onclick: () => { f.purpose = ''; f.gaps = false; purposeChoice.set(''); gapSwitch.set(false); ctx.setQuery({ purpose: null, gaps: null }); drawRows(); drawTotals(); } }),
      });
    }
    return empty({
      title: 'No drive in this period.',
      text: ctx.period && ctx.period.preset === 'all'
        ? `Nothing is recorded for ${carName} yet: drives show here once the car drives while its CAN Bridge records, or with an imported TeslaMate history.`
        : `${carName} made no drive that started in this period. Choose another period with the period picker (All time shows every drive).`,
    });
  }

  ctx.main.append(h('section', { class: 'block lb-log', 'aria-labelledby': 'lb-log-title' },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'lb-log-title' }, 'The log', h('span', { class: 'label-note' }, `${carName}, every drive that started in the period`))),
    h('div', { class: 'lb-table-box' }, table.el)));

  function filterParams() {
    return { ...ctx.params, purpose: f.purpose || null, gaps: f.gaps ? 'true' : null };
  }
  /** The filters or the order changed: the log from its top. */
  function drawRows(order = false) {
    const p = { purpose: f.purpose || null, gaps: f.gaps ? 'true' : null };
    table.setParams(p, order ? { sort: { key: 'seq', dir: f.order } } : {}).then(() => drawTotalsMarks(), (err) => ctx.flash(`The log did not load: ${err.message || err}`, { kind: 'error' }));
    drawTotalsMarks();
  }
  function drawTotalsMarks() {
    for (const b of totalsBox.querySelectorAll('.lb-purpose-card')) {
      const on = b.classList.contains(`lb-p-${f.purpose}`);
      b.classList.toggle('is-on', on);
      b.setAttribute('aria-pressed', String(on));
    }
  }

  /** The page's figures from the last answer; again = the rows in view loaded again first (a live change). */
  async function loadLog(again = false) {
    await (again ? table.refresh() : table.ready);
    if (!ctx.alive()) return;
    drawTotals();
    drawTotalsMarks();
    drawAsOf();
  }

  /* ---------------------------------------------------------------- purpose of one drive (admin) */

  async function editPurpose(r) {
    let chosen = r.purpose || 'business';
    const pick = choice({ label: 'Purpose', name: 'purpose', value: chosen, options: PURPOSES.map((k) => [k, PURPOSE_WORD[k]]), onchange: (v) => { chosen = v; } });
    const relation = field({ label: 'Relation', name: 'relation', value: r.relation || '', hint: 'Who the drive was for: a customer, an employer, a project. Up to 100 characters.', attrs: { maxlength: 100, autocomplete: 'off' } });
    const noteF = field({ label: 'Note', name: 'note', type: 'textarea', value: r.note || '', hint: 'Anything a tax office may ask about. Up to 500 characters.', attrs: { maxlength: 500, rows: 3 } });
    let closeIt = null;
    const fm = form({
      fields: [relation, noteF], extra: null, submitLabel: 'Save the purpose',
      onsubmit: async (v) => {
        const body = { purpose: chosen, relation: String(v.relation || '').trim() || null, note: String(v.note || '').trim() || null };
        await ctx.api.put(api.apiPath`cars/${cid}/drives/${r.drive}/purpose`, body);
        if (!ctx.alive()) return;
        ctx.flash(`Drive ${formatNumber(r.seq)} is now ${PURPOSE_WORD[chosen].toLowerCase()}.`);
        if (closeIt) closeIt(true);
        await loadLog(true).catch((err) => ctx.flash(`The log did not load again: ${err.message || err}`, { kind: 'error' }));
      },
    });
    const rule = r.purpose_source === 'rule' ? ruleOf(r.purpose_rule) : null;
    await openDialog({
      title: `Drive ${formatNumber(r.seq)}: ${formatDate(r.start_ms, tz, { year: true })}, ${formatTime(r.start_ms, tz)}`,
      body: (close) => {
        closeIt = close;
        return [
          h('p', { class: 'note' }, `${r.start_place || r.start_address || 'Unknown'} → ${r.end_place || r.end_address || 'unknown'}, `, format(r.distance_km, 'distance'), '.'),
          r.purpose_source === 'rule'
            ? h('p', { class: 'note' }, `A rule makes this drive ${PURPOSE_WORD[r.purpose].toLowerCase()}${rule ? ` (${placeName(rule.from_place)} → ${placeName(rule.to_place)}, ${dayWords(rule.days)})` : ''}. What you save here wins over the rule.`)
            : null,
          h('div', { class: 'lb-dialog-pick' }, pick),
          fm,
          r.purpose_source === 'owner'
            ? h('p', { class: 'lb-dialog-remove' }, button('Remove the purpose', {
              kind: 'danger', cls: 'button-small',
              onclick: async () => {
                if (!(await confirmDialog({ title: 'Remove the purpose?', text: `Drive ${formatNumber(r.seq)} loses the purpose, relation and note set by hand. A rule that fits it gives it a purpose again.`, yes: 'Remove', danger: true }))) return;
                await ctx.api.del(api.apiPath`cars/${cid}/drives/${r.drive}/purpose`);
                if (!ctx.alive()) return;
                ctx.flash(`Drive ${formatNumber(r.seq)} has no purpose set by hand now.`);
                close(true);
                await loadLog(true).catch(() => {});
              },
            }))
            : null,
        ];
      },
      actions: [],
      signal: ctx.signal,
    });
  }

  /* ---------------------------------------------------------------- purpose rules */

  const rulesBox = h('div', { class: 'lb-rules-box' }, loadingBlock(2));
  const ruleFormBox = h('div', { class: 'lb-rule-form' });
  ctx.main.append(h('section', { class: 'block lb-rules', 'aria-labelledby': 'lb-rules-title' },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'lb-rules-title' }, 'Purpose rules', h('span', { class: 'label-note' }, `give ${carName}’s regular drives their purpose by themselves`))),
    h('p', { class: 'note lb-rules-note' }, 'A rule names the start and end place and the days, such as Home to Work on weekdays is commute. It applies to every drive that fits, past and future. A purpose set by hand on a drive wins over any rule, and purposes are kept apart from the drives, so they survive a rebuild.'),
    h('div', { class: 'lb-rules-grid' }, rulesBox, ruleFormBox)));

  const ruleCount = (rid) => (data.rules_used || {})[rid] || 0;
  function drawRules() {
    const t = dataTable(ctx, {
      rows: rules, cls: 'lb-rules-table', label: 'Purpose rules', itemName: ['rule', 'rules'],
      csv: { filename: csvFilename(carName, 'purpose-rules'), columns: [column('From', (r) => placeName(r.from_place)), column('To', (r) => placeName(r.to_place)), column('Days', 'days'), column('Purpose', 'purpose'), column('Drives in the period', (r) => ruleCount(r.id))] },
      columns: [
        { key: 'from', label: 'From', th: true, cell: (r) => placeName(r.from_place), value: (r) => placeName(r.from_place) },
        { key: 'to', label: 'To', cell: (r) => placeName(r.to_place), value: (r) => placeName(r.to_place) },
        { key: 'days', label: 'Days', cell: (r) => dayWords(r.days), value: (r) => r.days },
        { key: 'purpose', label: 'Purpose', cell: (r) => h('span', { class: ['lb-purpose', `lb-p-${r.purpose}`] }, h('span', { class: 'lb-dot', 'aria-hidden': 'true' }), PURPOSE_WORD[r.purpose] || r.purpose), value: (r) => r.purpose },
        { key: 'drives', label: 'Drives it names', num: true, hint: 'Drives in this period whose purpose comes from this rule (a purpose set by hand wins).', cell: (r) => formatNumber(ruleCount(r.id)), value: (r) => ruleCount(r.id) },
        ...(admin ? [{ key: 'act', label: 'Remove', cell: (r) => h('button', { type: 'button', class: 'lb-edit lb-remove', onclick: () => removeRule(r), title: 'Remove this rule' }, icon('close'), 'Remove') }] : []),
      ],
      empty: () => empty({ title: 'No rule yet.', text: admin ? 'Add one on the right: every drive that fits gets its purpose by itself.' : 'An admin can add rules; until then purposes are set per drive.' }),
    });
    replace(rulesBox, t.el);
    t.load();
  }

  async function removeRule(r) {
    const n = ruleCount(r.id);
    if (!(await confirmDialog({
      title: 'Remove this rule?',
      text: `${placeName(r.from_place)} → ${placeName(r.to_place)}, ${dayWords(r.days)}, is ${PURPOSE_WORD[r.purpose].toLowerCase()}. ${n ? `${formatNumber(n)} ${n === 1 ? 'drive' : 'drives'} in this period lose that purpose` : 'No drive in this period gets its purpose from it'}; purposes set by hand stay.`,
      yes: 'Remove the rule', danger: true,
    }))) return;
    try {
      await ctx.api.del(api.apiPath`cars/${cid}/purpose-rules/${r.id}`);
      if (!ctx.alive()) return;
      ctx.flash('Rule removed.');
      await reloadAll();
    } catch (err) {
      ctx.flash(`Not removed: ${err.message || err}`, { kind: 'error' });
    }
  }

  function drawRuleForm() {
    if (!admin) {
      replace(ruleFormBox, h('p', { class: 'note' }, 'Purposes and rules change the car’s data, so only an admin can set them. You see every purpose and rule.'));
      return;
    }
    const opts = [['', 'Anywhere (no saved place)'], ...[...places].sort((a, b) => compareText(a.name, b.name)).map((p) => [p.id, p.name])];
    const from = field({ label: 'From', name: 'from_place', type: 'select', options: opts, value: places.some((p) => p.id === 'p1') ? 'p1' : '' });
    const to = field({ label: 'To', name: 'to_place', type: 'select', options: opts, value: '' });
    const days = field({ label: 'Days', name: 'days', type: 'select', options: DAYS, value: 'mon-fri', hint: 'In the car’s time zone, by the day the drive started.' });
    const purpose = field({ label: 'Purpose', name: 'purpose', type: 'select', options: PURPOSES.map((k) => [k, PURPOSE_WORD[k]]), value: 'commute' });
    replace(ruleFormBox, h('h3', { class: 'lb-rule-title' }, 'Add a rule'), form({
      fields: [from, to, days, purpose], submitLabel: 'Add the rule', busyLabel: 'Adding…',
      onsubmit: async (v) => {
        const body = { from_place: v.from_place || null, to_place: v.to_place || null, days: v.days, purpose: v.purpose };
        if (!body.from_place && !body.to_place) throw Object.assign(new Error('Name a place.'), { fields: { from_place: 'Name the start or the end place (or both).' } });
        await ctx.api.post(api.apiPath`cars/${cid}/purpose-rules`, body);
        if (!ctx.alive()) return;
        ctx.flash(`Rule added: ${placeName(body.from_place)} → ${placeName(body.to_place)} is ${PURPOSE_WORD[body.purpose].toLowerCase()}.`);
        await reloadAll();
      },
    }), h('p', { class: 'field-hint' }, 'Missing a place? Name it on ', h('a', { href: state.buildUrl('places') }, 'Places'), ' first.'));
  }

  async function loadRules() {
    const [pl, ru] = await Promise.all([
      ctx.api.get('places', { limit: 500, sort: 'name' }, { maxAgeMs: 60000 }).catch(() => null),
      ctx.api.get(api.apiPath`cars/${cid}/purpose-rules`, { limit: 500 }),
    ]);
    if (!ctx.alive()) return;
    places = (pl && pl.items) || [];
    rules = (ru && ru.items) || [];
  }

  async function reloadAll() {
    await loadRules();
    await loadLog(true);
    drawRules();
    drawRuleForm();
  }

  /* ---------------------------------------------------------------- first load and live */

  let rowsTimer = 0;
  ctx.events(['rows'], (ev) => {
    const d = (ev && ev.data) || {};
    if (d.car && d.car !== cid) return;
    if (d.kinds && !d.kinds.includes('drives')) return;
    clearTimeout(rowsTimer);
    rowsTimer = setTimeout(() => { if (ctx.alive()) loadLog(true).catch(() => {}); }, 1500);
  });
  ctx.onCleanup(() => clearTimeout(rowsTimer));

  let rulesErr = null;
  try {
    await loadRules();
  } catch (err) {
    rulesErr = err;
  }
  if (!ctx.alive()) return {};
  await loadLog();
  if (!ctx.alive()) return {};
  if (rulesErr) replace(rulesBox, errorView(rulesErr, { retry: () => reloadAll().catch(() => {}), eyebrow: 'Could not load the rules' }));
  else drawRules();
  drawRuleForm();
  ctx.ready();
  return {};
}
