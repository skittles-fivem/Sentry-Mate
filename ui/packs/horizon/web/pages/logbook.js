// Logbook (/cars/{car}/logbook): the pit log for tax and expense claims. Every drive of the period, in
// order, numbered, with its start and end time, odometer and address, duration, distance and purpose
// (GET /cars/{car}/logbook). A purpose (business, commute, private) is set by hand by an admin, with a
// relation and a note, or by a rule (GET/POST/DELETE /cars/{car}/purpose-rules); the hand choice wins.
// Totals per purpose sit on top as counters; the log exports as CSV or PDF.

import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatTime, formatDate, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { csvFilename } from '../shared/csv.js';
import { icon, hud, plate, btn } from '../core/ui.js';
import { controls, slot, periodBtn, carBtn, counters, plural, placeHref } from '../core/kit.js';
import { tracks } from '../core/tracks.js';
import { emptyState } from '../core/states.js';
import { field, form } from '../core/form.js';
import { openDialog, confirmDialog, toast } from '../core/layer.js';
import { isAdmin } from '../core/prefs.js';

export const id = 'logbook';
export const css = true;

const enc = encodeURIComponent;
const PURPOSE = {
  business: { word: 'Business', tone: 'cyan' },
  commute: { word: 'Commute', tone: 'mag' },
  private: { word: 'Private', tone: 'org' },
  none: { word: 'Not set', tone: null },
};
const DAYS = [['mon-fri', 'Monday to Friday'], ['sat-sun', 'Saturday and Sunday'], ['mon-sun', 'Every day']];

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const path = `cars/${enc(cid)}/logbook`;
  const rulesPath = `cars/${enc(cid)}/purpose-rules`;
  const admin = isAdmin();
  let only = ['business', 'commute', 'private', 'none'].includes(ctx.query.purpose) ? ctx.query.purpose : 'all';
  let data = { rows: [], totals_by_purpose: {} }, rules = [], places = [];

  const totalsEl = h('div');
  const chipsEl = h('div', { class: 'chips', role: 'group', 'aria-label': 'Show drives of one purpose' });
  const rulesEl = h('div');
  const csvA = h('a', { class: 'btn btn--sm', download: '', cover: 'dt.export.csv' }, icon('download', 'i--sm'), h('span', null, 'CSV'));
  const pdfA = h('a', { class: 'btn btn--sm btn--go', download: `logbook-${cid}.pdf`, href: ctx.api.url(path, { ...ctx.params, format: 'pdf' }) }, icon('download', 'i--sm'), h('span', null, 'PDF'));
  const placeName = (pid) => (pid ? places.find((p) => p.id === pid)?.name || 'a removed place' : 'anywhere unsaved');

  const addr = (name, address, pid) => h('span', { class: 'cell' },
    name ? (pid ? h('a', { class: 'plc', href: placeHref({ place_id: pid }), title: `Edit the place ${name}` }, name) : h('span', { class: 'plc plc--bare' }, name)) : null,
    h('span', { class: name ? 'cell-sub' : null }, address || (name ? '' : 'address not known')));
  const log = tracks(ctx, {
    title: 'Pit log', icon: 'book', rows: [], csv: false, dense: true, sort: { key: 'seq', dir: 'asc' }, cover: 'dt.table',
    note: 'Numbered in order of the drives. The odometer is the car\'s own; a jump between one drive\'s end and the next start is marked.',
    side: [csvA, pdfA],
    columns: [
      { key: 'seq', label: 'No.', num: true, sort: true, cover: 'dt.col.id', render: (r) => h('span', { class: 'trk-no' }, String(r.seq).padStart(3, '0')) },
      { key: 'start', label: 'Start', sort: true, lead: true, value: (r) => r.start_ms, cover: ['dt.col.start_time', 'dt.col.start_odometer'],
        render: (r) => h('span', { class: 'cell' }, h('span', { class: 'cell-main' }, `${formatDate(r.start_ms, tz, { year: true })}, ${formatTime(r.start_ms, tz)}`), h('span', { class: 'cell-sub mono' }, format(r.start_odometer_km, 'odometer', { digits: 1 }))) },
      { key: 'from', label: 'From', cover: 'dt.col.start_address', render: (r) => addr(r.start_place, r.start_address, r.start_place_id) },
      { key: 'end', label: 'End', cover: ['dt.col.end_time', 'dt.col.end_odometer'],
        render: (r) => h('span', { class: 'cell' }, h('span', { class: 'cell-main' }, r.end_ms ? formatTime(r.end_ms, tz) : 'still driving'), h('span', { class: 'cell-sub mono' }, format(r.end_odometer_km, 'odometer', { digits: 1 }))) },
      { key: 'to', label: 'To', cover: 'dt.col.end_address', render: (r) => addr(r.end_place, r.end_address, r.end_place_id) },
      { key: 'duration', label: 'Time', num: true, sort: true, value: (r) => r.duration_ms, cover: 'dt.col.duration', render: (r) => formatDuration(r.duration_ms) },
      { key: 'distance', label: 'Distance', num: true, sort: true, value: (r) => r.distance_km, cover: 'dt.col.distance',
        render: (r) => h('span', { class: 'cell' }, h('span', null, format(r.distance_km, 'distance', { digits: 1 })), r.continuity_gap_km ? h('span', { class: 'cell-sub warn' }, `${format(r.continuity_gap_km, 'distance', { digits: 1 })} jump`) : null) },
      { key: 'purpose', label: 'Purpose', sort: true, value: (r) => PURPOSE[r.purpose || 'none'].word,
        render: (r) => h('span', { class: 'cell' }, admin
          ? h('button', { class: 'lg-p lg-p--btn', type: 'button', dataset: { p: r.purpose || 'none' }, title: 'Set the purpose, relation and note', onclick: () => editPurpose(r) }, PURPOSE[r.purpose || 'none'].word)
          : h('span', { class: 'lg-p', dataset: { p: r.purpose || 'none' } }, PURPOSE[r.purpose || 'none'].word),
          r.relation || r.purpose_source ? h('span', { class: 'cell-sub' }, [r.relation, r.purpose_source === 'rule' ? 'by a rule' : r.purpose_source === 'owner' ? 'set by hand' : null].filter(Boolean).join(' · ')) : null) },
    ],
    empty: () => (data.rows.length
      ? emptyState({ icon: 'book', title: `No ${PURPOSE[only].word.toLowerCase()} drives`, text: `None of the ${plural(data.rows.length, 'drive')} of this period fits. Choose All.` })
      : emptyState({ icon: 'book', title: 'An empty log', text: `${car.name} did not drive in ${period.label.toLowerCase()}. Pick a longer period.` })),
  });

  function paint() {
    const t = data.totals_by_purpose || {};
    replace(totalsEl, counters(['business', 'commute', 'private', 'none'].map((k) => ({
      label: PURPOSE[k].word, value: t[k]?.distance_km ?? 0, kind: 'distance', digits: 1, how: plural(t[k]?.drives ?? 0, 'drive'),
    }))));
    const n = (k) => (k === 'all' ? data.rows.length : t[k]?.drives ?? 0);
    replace(chipsEl, ['all', 'business', 'commute', 'private', 'none'].map((k) => h('button', { class: 'chip', type: 'button', 'aria-pressed': String(only === k),
      onclick: () => { only = k; ctx.setQuery({ purpose: k === 'all' ? null : k }); paint(); } }, `${k === 'all' ? 'All' : PURPOSE[k].word} (${formatNumber(n(k))})`)));
    log.setRows(only === 'all' ? data.rows : data.rows.filter((r) => (r.purpose || 'none') === only));
    replace(rulesEl, hud({ title: 'Purpose rules', icon: 'sliders', side: admin ? btn('Add a rule', addRule, { sm: true, icon: 'plus' }) : null },
      rules.length ? h('ul', { class: 'lg-rules' }, rules.map((x) => h('li', { class: 'lg-rule' },
        h('span', { class: 'lg-p', dataset: { p: x.purpose } }, PURPOSE[x.purpose]?.word || x.purpose),
        h('span', null, `From ${placeName(x.from_place)} to ${placeName(x.to_place)}, ${(DAYS.find((d) => d[0] === x.days) || [, x.days])[1].toLowerCase()}`),
        admin ? btn(null, () => removeRule(x), { icon: 'x', sm: true, kind: 'ghost', label: 'Remove this rule' }) : null)))
        : h('p', { class: 'empty-line' }, icon('info', 'i--sm'), h('span', null, 'No rules yet. A rule gives drives between two places a purpose by itself, such as home to work on weekdays as commute.')),
      h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, 'A purpose set by hand always wins over a rule.'))));
  }

  async function load() {
    const [d, r, p] = await Promise.all([
      ctx.api.get(path, ctx.params),
      ctx.api.get(rulesPath, { limit: 500 }).catch(() => ({ items: [] })),
      places.length ? Promise.resolve(places) : ctx.api.list('places', { sort: 'name' }, { limit: 500 }).all({ max: 2000 }).catch(() => []),
    ]);
    if (!ctx.alive()) return;
    data = d; rules = r.items || []; places = p;
    csvA.setAttribute('href', ctx.api.csvUrl(path, ctx.params));
    csvA.setAttribute('download', `${csvFilename(car.name, 'logbook', period.label)}`);
    paint();
  }

  async function editPurpose(r) {
    const purpose = field({ label: 'Purpose', name: 'purpose', type: 'select', value: r.purpose || 'business', required: true, options: ['business', 'commute', 'private'].map((k) => [k, PURPOSE[k].word]) });
    const relation = field({ label: 'Relation', name: 'relation', value: r.relation || '', hint: 'Who or what the drive was for: a customer, a project, an employer.', attrs: { maxlength: '100' } });
    const note = field({ label: 'Note', name: 'note', type: 'textarea', value: r.note || '', attrs: { maxlength: '500', rows: '3' } });
    let d = null;
    const f = form({
      fields: [purpose, relation, note], submit: 'Save',
      secondary: r.purpose_source === 'owner' ? btn('Remove the purpose', async () => {
        try { await ctx.api.del(`cars/${enc(cid)}/drives/${enc(r.drive)}/purpose`); toast(`Drive ${r.seq} has no purpose set by hand now.`); d.close(); load(); } catch (e) { f.setMessage(e.message); }
      }, { kind: 'ghost' }) : null,
      async onSubmit(v) {
        await ctx.api.put(`cars/${enc(cid)}/drives/${enc(r.drive)}/purpose`, { purpose: v.purpose, relation: v.relation.trim() || null, note: v.note.trim() || null });
        toast(`Drive ${r.seq} is ${PURPOSE[v.purpose].word.toLowerCase()} now.`);
        d.close(); load();
      },
    });
    d = openDialog({ title: `Drive ${r.seq}, ${formatDate(r.start_ms, tz, { year: true })} ${formatTime(r.start_ms, tz)}`,
      body: [h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, `From ${r.start_place || r.start_address || 'an unknown address'} to ${r.end_place || r.end_address || 'an unknown address'}, ${format(r.distance_km, 'distance', { digits: 1 })}.`)), f.el] });
  }
  async function addRule() {
    const opts = [['', 'Anywhere that is not a saved place'], ...places.map((p) => [p.id, p.name])];
    const fromF = field({ label: 'From', name: 'from_place', type: 'select', options: opts, value: places.find((p) => p.kind === 'home')?.id || '' });
    const toF = field({ label: 'To', name: 'to_place', type: 'select', options: opts, value: places.find((p) => p.kind === 'work')?.id || '' });
    const daysF = field({ label: 'Days', name: 'days', type: 'select', options: DAYS, value: 'mon-fri', hint: "Days in the car's time zone, by the start of the drive." });
    const pF = field({ label: 'Purpose', name: 'purpose', type: 'select', options: ['commute', 'business', 'private'].map((k) => [k, PURPOSE[k].word]), value: 'commute' });
    let d = null;
    const f = form({ fields: [fromF, toF, daysF, pF], submit: 'Add the rule', async onSubmit(v) {
      await ctx.api.post(rulesPath, { from_place: v.from_place || null, to_place: v.to_place || null, days: v.days, purpose: v.purpose });
      toast('Rule added. Matching drives without a hand-set purpose have it now.'); d.close(); load();
    } });
    d = openDialog({ title: 'Add a purpose rule', body: f.el });
  }
  async function removeRule(x) {
    if (!(await confirmDialog(`Drives from ${placeName(x.from_place)} to ${placeName(x.to_place)} lose the purpose this rule gave them. Purposes set by hand stay.`, { title: 'Remove this rule?', ok: 'Remove', danger: true }))) return;
    try { await ctx.api.del(`${rulesPath}/${enc(x.id)}`); toast('Rule removed.'); load(); } catch (e) { toast(e.message, { kind: 'error' }); }
  }

  replace(ctx.view, h('div', { class: 'page lg' },
    plate({ kicker: `Driving · ${car.name}`, title: 'Logbook', aim: ctx.info.aim }),
    controls({ label: 'Filters', note: 'Times are in the car\'s time zone. The CSV and the PDF hold the whole period, every purpose.' },
      slot('Period', periodBtn(ctx, 'dt.filter.time_range')),
      slot('Car', carBtn(ctx, 'dt.control.per_car')),
      slot('Purpose', chipsEl)),
    hud({ title: 'Totals by purpose', icon: 'stats', tone: 'cyan' }, totalsEl),
    log.el, rulesEl));

  try { await load(); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  ctx.ready();
  let t = 0;
  ctx.events('rows', (ev) => { if (ev.data?.car !== cid) return; clearTimeout(t); t = setTimeout(() => { if (ctx.alive()) load().catch(() => {}); }, 2000); });
  ctx.onCleanup(() => clearTimeout(t));
}
