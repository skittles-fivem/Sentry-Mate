// Logbook (/cars/{car}/logbook): the trip log for tax and expense claims. Every drive in the range,
// numbered from 1, oldest first: date, start and end time, odometer at start and end, from, to, the
// purpose and km (GET /cars/{car}/logbook a page at a time, also as CSV). The trip log is a box of about ten
// rows that scrolls under its head line; only the rows in view are drawn, loaded in chunks (core/table.js
// windowTable), with the purpose keys, the search and the order applied on the server, and Jump to date. The purpose is a field in each row: an admin picks
// business, commute or private there (PUT / DELETE .../drives/{drive}/purpose). Rules fill purposes in by
// themselves (.../purpose-rules). The purpose keys only narrow the table; totals and the odometer check
// always cover the whole range.

import { h, replace } from '../shared/dom.js';
import { formatTime, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { unitLabel } from '../shared/units.js';
import { head, section, kv, note } from '../core/ui.js';
import { num, tag, date } from '../core/fmt.js';
import { windowTable } from '../core/table.js';
import { empty } from '../core/states.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { field, form } from '../core/form.js';
import { toast } from '../core/toast.js';
import { isAdmin } from '../core/prefs.js';

export const id = 'logbook';

const enc = encodeURIComponent;
const GAP_KM = 0.05;
const PURPOSES = { business: 'business', commute: 'commute', private: 'private', none: 'not set' };
const ORDER = ['business', 'commute', 'private', 'none'];
const DAYS = [['mon-fri', 'weekdays (mon to fri)'], ['sat,sun', 'weekends'], ['all', 'every day'], ['mon', 'mondays'], ['tue', 'tuesdays'], ['wed', 'wednesdays'], ['thu', 'thursdays'], ['fri', 'fridays'], ['sat', 'saturdays'], ['sun', 'sundays']];
const dayLabel = (d) => (DAYS.find((x) => x[0] === d) || [d, d])[1];

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const admin = isAdmin();
  const path = `cars/${enc(cid)}/logbook`;
  const rulesPath = `cars/${enc(cid)}/purpose-rules`;
  let filter = ORDER.includes(ctx.query.purpose) ? ctx.query.purpose : 'all';
  let data = null, places = [], rules = [];   // data: the last page (totals, counts, check, rules used of the whole range)

  const hd = head(ctx, { title: 'Logbook' });
  const btns = hd.querySelectorAll('.ph-btn');
  if (btns[0]) btns[0].dataset.cover = 'dt.filter.time_range';
  if (btns[1]) btns[1].dataset.cover = 'dt.control.per_car';

  const keys = h('span', { class: 'keys', role: 'group', 'aria-label': 'Purpose' });
  const paintKeys = () => replace(keys, ['all', ...ORDER].map((p) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(filter === p),
    onclick: () => { filter = p; ctx.setQuery({ purpose: p === 'all' ? null : p }); paintKeys(); tbl.setParams({ purpose: p === 'all' ? null : p }).then(paintAll, fail); } }, p === 'all' ? 'all' : PURPOSES[p])));
  paintKeys();
  const csvHref = () => ctx.api.csvUrl(path, ctx.params);
  const csvA = h('a', { class: 'lnk', href: '#', download: '', cover: 'dt.export.csv', title: 'every drive of the range, whatever the purpose keys' }, 'export as csv');
  const pdfA = h('a', { class: 'lnk', href: ctx.api.url(path, { ...ctx.params, format: 'pdf' }), download: '' }, 'pdf');
  const totals = h('div');
  const cont = h('div');
  const rulesSlot = h('div');

  const fail = (e) => { if (ctx.alive()) toast(e?.message || 'The logbook could not be loaded.', { kind: 'error' }); };

  const purposeField = (r) => {
    if (!admin) return h('span', { class: `pur pur--${r.purpose || 'none'}` }, PURPOSES[r.purpose || 'none']);
    const sel = h('select', { class: 'in in--cell', 'aria-label': `Purpose of drive ${r.seq}` },
      h('option', { value: '', selected: !r.purpose }, 'not set'),
      ['business', 'commute', 'private'].map((p) => h('option', { value: p, selected: r.purpose === p }, p)));
    sel.addEventListener('change', async () => {
      try {
        if (sel.value) await ctx.api.put(`cars/${enc(cid)}/drives/${enc(r.drive)}/purpose`, { purpose: sel.value, relation: r.relation || null, note: r.note || null });
        else await ctx.api.del(`cars/${enc(cid)}/drives/${enc(r.drive)}/purpose`);
        toast(`Drive ${formatNumber(r.seq)}: ${sel.value || 'no purpose set by hand'}.`);
        refresh();
      } catch (e) { toast(e?.message || 'The purpose could not be saved.', { kind: 'error' }); }
    });
    return h('span', { class: 'pur-cell' }, sel, r.purpose_source === 'rule' ? h('span', { class: 'muted', title: 'a rule gives this purpose; picking one here wins' }, 'rule') : null,
      h('button', { class: 'btn btn--line muted', type: 'button', title: 'relation and note', onclick: () => editNote(r) }, r.relation || r.note ? '[note*]' : '[note]'));
  };
  const placeOf = (name, addr) => (name ? h('span', null, name) : addr ? h('span', { class: 'muted' }, addr) : h('span', { class: 'muted' }, 'unknown'));

  const tbl = windowTable(ctx, {
    title: 'trip log', path, params: { ...ctx.params, purpose: filter === 'all' ? null : filter }, limit: 50, cover: 'dt.table', csvName: 'logbook',
    csvLabel: 'csv (filtered)', boxLabel: 'the trip log (scrolls; arrow keys scroll it)',
    sort: { key: 'seq', dir: 'asc' }, firstSort: { key: 'seq', dir: 'asc' }, jump: true,
    search: { label: 'search the drives', placeholder: 'place, address, note' },
    onPage: (pg) => { data = pg; },
    onError: fail,
    rowHref: (r) => ctx.link('drive', { drive: r.drive }),
    columns: [
      { key: 'seq', label: '#', num: true, sort: 'seq', firstDir: 'asc', cover: 'dt.col.id', render: (r) => h('a', { class: 'lnk-plain', href: ctx.link('drive', { drive: r.drive }) }, formatNumber(r.seq)) },
      { key: 'start_ms', label: 'date', sort: 'seq', firstDir: 'asc', ph: 'main', cover: ['dt.col.start_time', 'dt.col.end_time'], render: (r) => h('span', null, date(r.start_ms, tz), ' ', formatTime(r.start_ms, tz), h('span', { class: 'muted' }, `-${r.in_progress ? 'now' : formatTime(r.end_ms, tz)}`)) },
      { key: 'start_odometer_km', label: 'odometer', num: true, sort: 'odometer', cover: ['dt.col.start_odometer', 'dt.col.end_odometer'],
        render: (r) => h('span', null, num(r.start_odometer_km, 'odometer'), h('span', { class: 'arrow' }, ' -> '), r.end_odometer_km != null ? num(r.end_odometer_km, 'odometer') : '—',
          r.continuity_gap_km != null && Math.abs(r.continuity_gap_km) > GAP_KM ? [' ', tag(`gap ${formatNumber(r.continuity_gap_km, { maximumFractionDigits: 1 })}`, 'bad')] : null) },
      { key: 'from', label: 'from', sort: 'from', cls: 'vl-place', ph: 'sub', value: (r) => r.start_place || r.start_address, cover: 'dt.col.start_address', render: (r) => placeOf(r.start_place, r.start_address) },
      { key: 'to', label: 'to', sort: 'to', cls: 'vl-place', grow: true, ph: 'sub', value: (r) => r.end_place || r.end_address, cover: 'dt.col.end_address', render: (r) => (r.in_progress ? h('span', { class: 'tone--accent' }, 'driving now') : placeOf(r.end_place, r.end_address)) },
      { key: 'purpose', label: 'purpose', sort: 'purpose', render: purposeField },
      { key: 'duration_ms', label: 'time', num: true, cover: 'dt.col.duration', render: (r) => formatDuration(r.duration_ms) },
      { key: 'distance_km', label: unitLabel('distance'), num: true, sort: 'distance', ph: 'num', cover: 'dt.col.distance', render: (r) => num(r.distance_km, 'distance') },
    ],
    empty: () => (data?.count
      ? empty(filter === 'all' ? 'no drive matches' : `no ${PURPOSES[filter]} drives${tbl.params().q ? ' matching' : ''} in this range`, `the range has ${formatNumber(data.count)} drives; pick all or clear the search`)
      : empty('no drives in this range', 'pick a longer range')),
  });

  function paintTotals() {
    const t = data?.totals_by_purpose || {};
    const all = ORDER.reduce((s, k) => s + (t[k]?.distance_km || 0), 0) || 1;
    replace(totals, section({ title: 'totals', side: [csvA, pdfA] },
      kv(ORDER.map((k) => [PURPOSES[k], h('span', null, num(t[k]?.distance_km ?? 0, 'distance'), h('span', { class: 'muted' }, ` ${formatNumber(t[k]?.drives ?? 0)} drives, ${formatNumber(((t[k]?.distance_km || 0) / all) * 100, { maximumFractionDigits: 0 })}%`))]), { cols: 1 }),
      note('One car per logbook, as a tax office asks. The purpose keys and the search narrow the table and its csv; totals and the odometer check cover the whole range.')));
  }
  function paintCont() {
    const c = data?.continuity || {};
    const n = data?.count || 0;
    replace(cont, section({ title: 'odometer check' },
      kv([
        ['drives', formatNumber(n)],
        ['first odometer', n ? num(c.first_start_odometer_km, 'odometer') : null],
        ['last odometer', n ? num(c.last_end_odometer_km, 'odometer') : null],
        ['unlogged', c.gaps ? h('span', { class: 'tone--bad' }, num(c.missing_km, 'distance'), ` in ${formatNumber(c.gaps)} gaps`) : [tag('ok', 'ok'), ' every drive starts where the last one ended']],
      ], { cols: 1 })));
  }
  function paintRules() {
    const name = (pid) => (pid ? places.find((p) => p.id === pid)?.name || 'a removed place' : 'anywhere not saved');
    const used = new Map(Object.entries(data?.rules_used || {}));
    replace(rulesSlot, section({ title: 'purpose rules', side: admin ? h('button', { class: 'btn btn--line tone--accent', type: 'button', onclick: addRule }, '[add a rule]') : null },
      rules.length ? h('div', { class: 'lines' }, rules.map((x) => h('div', { class: 'ln ln--rule' },
        h('span', { class: 'ln-t' }, tag(PURPOSES[x.purpose] || x.purpose, 'accent')),
        h('span', { class: 'ln-what' }, name(x.from_place), h('span', { class: 'arrow' }, ' -> '), name(x.to_place), h('span', { class: 'muted' }, `  ${dayLabel(x.days)}, ${formatNumber(used.get(x.id) || 0)} drives in range`)),
        admin ? h('button', { class: 'btn btn--line muted ln-num', type: 'button', onclick: () => removeRule(x) }, '[remove]') : h('span')))) : empty('no rules', 'a rule gives matching drives a purpose by itself, unless one was set by hand'),
      note('Drives that start at from and end at to on those days get the purpose, unless one was set by hand.')));
  }

  function paintAll() { if (ctx.alive()) { paintTotals(); paintCont(); paintRules(); } }
  async function load(again = false) {
    const [, rl, pl] = await Promise.all([
      again ? tbl.refresh() : tbl.ready,
      ctx.api.get(rulesPath, { limit: 500 }).catch(() => ({ items: [] })),
      ctx.api.list('places', { sort: 'name' }, { limit: 500 }).all({ max: 2000 }).catch(() => []),
    ]);
    if (!ctx.alive()) return;
    rules = rl.items || []; places = pl;
    csvA.href = csvHref();
    paintAll();
  }
  let busy = null;
  const refresh = () => { busy ??= load(true).catch((e) => toast(e?.message || 'The logbook could not be loaded again.', { kind: 'error' })).finally(() => { busy = null; }); return busy; };

  async function editNote(r) {
    const rel = field({ label: 'relation', name: 'relation', value: r.relation || '', hint: 'who or what the drive was for, up to 100 characters', attrs: { maxlength: '100' } });
    const nt = field({ label: 'note', name: 'note', type: 'textarea', value: r.note || '', attrs: { maxlength: '500', rows: '3' } });
    let dlg;
    const f = form({ fields: [rel, nt], submit: 'save', async onSubmit(v) {
      await ctx.api.put(`cars/${enc(cid)}/drives/${enc(r.drive)}/purpose`, { purpose: r.purpose || 'business', relation: v.relation.trim() || null, note: v.note.trim() || null });
      toast(`Drive ${formatNumber(r.seq)} saved.`); dlg.close(); refresh();
    } });
    dlg = openDialog({ title: `drive ${formatNumber(r.seq)}`, body: [note(r.purpose ? `purpose: ${r.purpose}` : 'no purpose yet: saving sets business; change it in the row.'), f.el] });
  }
  async function addRule() {
    const opts = [['', 'anywhere not saved'], ...places.map((p) => [p.id, p.name])];
    const from = field({ label: 'from', name: 'from_place', type: 'select', options: opts, value: places.find((p) => p.kind === 'home')?.id || '' });
    const to = field({ label: 'to', name: 'to_place', type: 'select', options: opts, value: places.find((p) => p.kind === 'work')?.id || '' });
    const days = field({ label: 'days', name: 'days', type: 'select', options: DAYS, value: 'mon-fri', hint: 'in the car’s time zone, by the start of the drive' });
    const pur = field({ label: 'purpose', name: 'purpose', type: 'select', options: ['commute', 'business', 'private'].map((k) => [k, k]), value: 'commute' });
    let dlg;
    const f = form({ fields: [from, to, days, pur], submit: 'add the rule', async onSubmit(v) {
      await ctx.api.post(rulesPath, { from_place: v.from_place || null, to_place: v.to_place || null, days: v.days, purpose: v.purpose });
      toast('Rule added.'); dlg.close(); refresh();
    } });
    dlg = openDialog({ title: 'add a purpose rule', body: f.el });
  }
  async function removeRule(x) {
    const ok = await confirmDialog('Drives this rule named lose that purpose. Purposes set by hand stay.', { title: 'remove this rule?', ok: 'remove', danger: true });
    if (!ok || !ctx.alive()) return;
    try { await ctx.api.del(`${rulesPath}/${enc(x.id)}`); toast('Rule removed.'); refresh(); } catch (e) { toast(e?.message || 'The rule could not be removed.', { kind: 'error' }); }
  }

  try { await load(); } catch (e) { ctx.fail(e, { retry: () => ctx.reload() }); return; }
  if (!ctx.alive()) return;
  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'logbook' } },
    hd,
    h('div', { class: 'optline' }, h('span', { class: 'optline-k' }, 'purpose:'), keys),
    h('div', { class: 'cols2' }, totals, cont),
    tbl.el, rulesSlot));
  ctx.ready();
  ctx.events('rows', (ev) => { const d = ev?.data || {}; if (d.car === cid && (d.kinds || []).includes('drives')) refresh(); });
}
