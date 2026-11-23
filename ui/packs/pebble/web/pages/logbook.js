// Sentry Mate, "Pebble" UI pack: Logbook (/cars/{car}/logbook), a trip log for tax and expense claims.
//
// Totals per purpose on top (from the whole period), then one tile with the drives: a purpose filter, a search, Jump
// to date, the count and Export CSV, and a box about ten rows high that scrolls. Only the rows in view are in the page
// (shared/vlist.js); they load from the API a chunk at a time as the box reaches them. Each row is one height: the
// number (opens the drive), the date and times with distance, duration and odometer, from -> to (with a warning where
// the odometer moved with no drive logged), and a purpose chip you tap to change it in a sheet, with a relation and a
// note. Data: GET /api/v1/cars/{car}/logbook (paged: limit, after or offset, at_ms; purpose, q on the server; every
// page carries the period's totals and counts); PUT and DELETE /cars/{car}/drives/{drive}/purpose (admins).

import { h } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatTime, formatDate } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { windowList, dayJumpMs } from '../shared/vlist.js';
import { tile, num, chip, icon } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { openSheet } from '../core/sheet.js';
import { field, segmented } from '../core/form.js';
import { filterBar, dur, PURPOSES } from '../core/bits.js';

export const id = 'logbook';
export const css = true;
export const shape = [['full', 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], ['full', 'h2']];
export const title = () => 'Logbook';

const TONES = { business: 'sky', private: 'coral', commute: 'teal' };
const FILTERS = [['all', 'All'], ['business', 'Business'], ['private', 'Private'], ['commute', 'Commute'], ['none', 'None']];

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const path = api.apiPath`cars/${car.id}/logbook`;
  let filter = FILTERS.some(([k]) => k === ctx.query.purpose) ? ctx.query.purpose : 'all';
  let words = '';
  let data = null;   // the last page: totals and counts of the whole period
  let total = 0;

  const exp = h('a', { class: 'btn btn-primary', href: ctx.api.csvUrl(path, ctx.params), download: `${(car.name || 'car').toLowerCase().replace(/[^a-z0-9]+/g, '-')}-logbook.csv`, cover: 'dt.export.csv', title: 'Every drive of the period' }, icon('download'), h('span', { class: 'btn-text' }, 'Export CSV'));
  ctx.head({ actions: [exp] });

  const query = () => ({ ...ctx.params, purpose: filter === 'all' ? null : filter, q: words || null });

  /* ---------------------------------------------------------------- the rows */
  function rowEl(r, i) {
    const p = r.purpose;
    const purpose = ctx.admin
      ? h('button', { type: 'button', class: ['chip', 'chip-btn', 'lb-pchip', `tone-${TONES[p] || 'muted'}`], title: 'Change the purpose', onclick: () => edit(r) }, h('span', null, p ? PURPOSES[p] : h('span', null, 'Set', h('span', { class: 'lb-vmore' }, ' purpose'))), icon('chevronDown'))
      : chip(p ? PURPOSES[p] : 'No purpose', { tone: TONES[p] || 'muted' });
    const gap = (r.continuity_gap_km || 0) > 0.05;
    const extra = [r.relation, r.note].filter(Boolean).join(' · ');
    return h('div', { class: ['lb-vrow', { 'has-gap': gap }], role: 'listitem', dataset: { vlRow: String(i) }, title: extra || null },
      h('a', { class: 'lb-seq', cover: 'dt.col.id', href: state.buildUrl('drive', { car: car.id, drive: r.drive }), title: `Drive ${r.drive}: details`, 'aria-label': `Drive ${formatNumber(r.seq)}, details` }, formatNumber(r.seq)),
      h('div', { class: 'lb-vtext' },
        h('p', { class: 'lb-vline' },
          h('span', { class: 'lb-vwhen' }, formatDate(r.start_ms, tz, { weekday: false, year: 'auto', now: api.now() }), ' ',
            h('span', { cover: 'dt.col.start_time' }, formatTime(r.start_ms, tz)), '–', h('span', { cover: 'dt.col.end_time' }, r.end_ms ? formatTime(r.end_ms, tz) : 'now')),
          h('span', { class: 'lb-vsep', 'aria-hidden': 'true' }, ' · '), h('span', { class: 'lb-vnum', cover: 'dt.col.distance' }, format(r.distance_km, 'distance')),
          h('span', { class: 'lb-vmore' }, h('span', { class: 'lb-vsep', 'aria-hidden': 'true' }, ' · '), h('span', { class: 'lb-vnum', cover: 'dt.col.duration' }, dur(r.duration_ms))),
          h('span', { class: 'lb-vmore' }, h('span', { class: 'lb-vsep', 'aria-hidden': 'true' }, ' · '), h('span', { class: 'lb-vodo' },
            h('span', { cover: 'dt.col.start_odometer' }, format(r.start_odometer_km, 'odometer')), ' → ', h('span', { cover: 'dt.col.end_odometer' }, format(r.end_odometer_km, 'odometer'))))),
        h('p', { class: 'lb-vline lb-vsub' },
          gap ? h('span', { class: 'lb-vgap', title: 'The odometer moved since the drive before, with no drive logged' }, icon('warning'), `+${format(r.continuity_gap_km, 'distance')} `) : null,
          h('span', { cover: 'dt.col.start_address' }, r.start_place || r.start_address || 'Unnamed spot'), ' → ',
          h('span', { cover: 'dt.col.end_address' }, r.in_progress ? 'Still driving' : r.end_place || r.end_address || 'Unnamed spot'),
          r.purpose_source === 'rule' ? h('span', { class: 'lb-vrule' }, ' · by a rule') : null)),
      purpose);
  }
  const skel = (i) => h('div', { class: 'lb-vrow is-skel', role: 'listitem', 'aria-hidden': 'true', dataset: { vlSkel: String(i) } }, h('span', { class: 'lb-seq' }), h('div', { class: 'lb-vtext' }, h('p', { class: 'lb-vline' }, '…')));
  const gapEl = () => h('div', { class: 'vl-gap' });

  const body = h('div', { class: 'lb-vlist', role: 'list', 'aria-label': 'The drives' });
  const box = h('div', { class: 'vl-box lb-vbox', tabindex: '0', role: 'region', 'aria-label': 'The drives of the logbook (scrolls)', cover: 'dt.table' }, body);
  const count = h('p', { class: 'tile-line lb-count', 'aria-live': 'polite' });
  const emptyBox = h('div', { class: 'lb-empty', hidden: true });

  const list = windowList({
    box, body, limit: 50, overscan: 6, rowHeight: 64,
    row: rowEl, placeholder: skel, spacer: gapEl,
    load: (p) => ctx.api.get(path, { ...query(), ...pick(p) }, { signal: p.signal }),
    onPage: (pg) => { data = pg; if (pg.total !== total) { total = pg.total; paintCount(); } },
    onError: (e) => ctx.toast(e.message || 'The drives could not be loaded.', { kind: 'error' }),
  });
  ctx.onCleanup(() => list.destroy());

  function paintCount() {
    const all = data?.count || 0;
    count.textContent = total === all ? `${formatNumber(total)} ${total === 1 ? 'drive' : 'drives'}, oldest first.` : `${formatNumber(total)} of ${formatNumber(all)} drives, oldest first.`;
    box.hidden = !total;
    emptyBox.hidden = !!total;
    emptyBox.replaceChildren(total ? '' : h('p', { class: 'note' }, all ? 'No drive matches. Choose All, or clear the search.' : 'No drives in this period. Pick another period with the date pill.'));
    jumpIn.disabled = !total; jumpBtn.disabled = !total;
  }

  /* ---------------------------------------------------------------- the tools */
  const seg = segmented({ label: 'Purpose', name: 'purpose', value: filter, options: FILTERS, cls: 'lb-seg', onchange: (v) => { filter = v; ctx.setQuery({ purpose: v === 'all' ? null : v }); restart(); } });
  const search = h('input', { class: 'input lb-search', type: 'search', placeholder: 'Place, address or note', 'aria-label': 'Search the drives', autocomplete: 'off' });
  let typing = 0;
  search.addEventListener('input', () => {
    clearTimeout(typing);
    typing = setTimeout(() => { const v = search.value.trim(); if (v !== words) { words = v; restart(); } }, 300);
  });
  ctx.onCleanup(() => clearTimeout(typing));
  const jumpIn = h('input', { class: 'input lb-date', type: 'date', 'aria-label': 'Jump to date' });
  const jumpBtn = h('button', { type: 'button', class: 'btn btn-soft', onclick: () => jump() }, 'Jump to date');
  jumpIn.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); jump(); } });
  async function jump() {
    if (!jumpIn.value) { jumpIn.focus(); return; }
    try {
      await list.jump(dayJumpMs(jumpIn.value, tz));
      box.focus({ preventScroll: true });
    } catch (e) { if (!api.isAbort(e)) ctx.toast(e.message || 'Not found.', { kind: 'error' }); }
  }

  async function restart() {
    try {
      const pg = await list.start();
      if (!pg || !ctx.alive()) return;
      data = pg; total = pg.total;
      paintCount();
    } catch (e) { if (!api.isAbort(e)) ctx.toast(e.message || 'The drives could not be loaded.', { kind: 'error' }); }
  }

  /* ---------------------------------------------------------------- the page */
  function totalsTiles() {
    const tot = data?.totals_by_purpose || {};
    return ['business', 'private', 'commute', 'none'].map((k) => tile({ w: 1, title: k === 'none' ? 'No purpose yet' : PURPOSES[k], icon: k === 'none' ? 'help' : 'book', cls: ['tile-stat', `tone-${TONES[k] || 'muted'}`] },
      num(tot[k]?.distance_km ?? 0, 'distance'), h('p', { class: 'tile-line' }, `${formatNumber(tot[k]?.drives ?? 0)} drives`)));
  }
  let totals = [];
  function draw() {
    const bar = filterBar(ctx, { carCover: 'dt.control.per_car', periodCover: 'dt.filter.time_range' });
    if (!data?.count && !words && filter === 'all') {
      ctx.view.replaceChildren(bar, ...totalsTiles(), emptyTile({ icon: 'book', title: 'No drives in this period', text: 'Pick another period with the date pill.',
        cover: ['dt.table', 'dt.col.id', 'dt.col.start_time', 'dt.col.start_odometer', 'dt.col.start_address', 'dt.col.end_time', 'dt.col.end_odometer', 'dt.col.end_address', 'dt.col.duration', 'dt.col.distance'] }));
      return;
    }
    totals = totalsTiles();
    ctx.view.replaceChildren(bar, ...totals, tile({ w: 'full', title: 'Drives', icon: 'route', cls: 'lb-drives', side: count },
      h('div', { class: 'lb-tools' }, seg, h('div', { class: 'lb-tool' }, search), h('div', { class: 'lb-tool lb-jump' }, jumpIn, jumpBtn)),
      box, emptyBox));
  }
  function paintTotals() {
    const fresh = totalsTiles();
    totals.forEach((t, i) => t.replaceWith(fresh[i]));
    totals = fresh;
  }

  // the first chunk, before the page shows (the router's placeholder tiles stay up until then)
  try {
    const pg = await list.start();
    if (pg) { data = pg; total = pg.total; }
  } catch (e) {
    if (ctx.alive()) ctx.fail(e);
    return {};
  }
  if (!ctx.alive()) return {};
  draw();
  paintCount();
  // the window draws again once it is in the page (it has its height there)
  box.scrollTop = 0;
  box.dispatchEvent(new Event('scroll'));

  async function edit(r) {
    let chosen = r.purpose || 'none';
    const sg = segmented({ label: 'Purpose', name: 'purpose', value: chosen, options: [['business', 'Business'], ['private', 'Private'], ['commute', 'Commute'], ['none', 'None']], onchange: (v) => { chosen = v; } });
    const rel = field({ label: 'Relation', name: 'relation', value: r.relation || '', hint: 'Who the drive was for: a customer, an employer, a project.', attrs: { maxlength: 100, autocomplete: 'off' } });
    const nt = field({ label: 'Note', name: 'note', value: r.note || '', attrs: { maxlength: 500, autocomplete: 'off' } });
    const err = h('p', { class: 'form-error', role: 'alert', hidden: true });
    const saved = await openSheet({
      title: `Drive ${formatNumber(r.seq)}, ${formatDate(r.start_ms, tz, { now: api.now() })}`,
      body: h('div', { class: 'form' }, sg, rel, nt, err),
      actions: [{ label: 'Cancel', value: false }, { label: 'Save', kind: 'primary', value: true, onclick: async () => {
        try {
          const p = api.apiPath`cars/${car.id}/drives/${r.drive}/purpose`;
          if (chosen === 'none') await ctx.api.del(p);
          else await ctx.api.put(p, { purpose: chosen, relation: rel.control.value.trim() || null, note: nt.control.value.trim() || null });
          return true;
        } catch (e) { err.textContent = e.message || 'Not saved.'; err.hidden = false; return false; }
      } }],
    });
    if (!saved) return;
    await refresh();
    ctx.toast('The purpose is saved.');
  }

  async function refresh() {
    try {
      const pg = await list.refresh();
      if (!ctx.alive()) return;
      if (pg) { data = pg; total = pg.total; }
      paintTotals();
      paintCount();
    } catch (e) { if (!api.isAbort(e)) ctx.toast(e.message || 'The drives could not be loaded again.', { kind: 'error' }); }
  }
  ctx.events(['rows'], (ev) => { const d = ev?.data || {}; if (d.car === car.id && (d.kinds || []).includes('drives')) refresh(); });

  ctx.ready();
  return {};
}

// the paging parameters vlist asks with (offset, after or at_ms)
function pick(p) {
  const out = { limit: p.limit };
  if (p.after) out.after = p.after;
  else if (p.at_ms != null) out.at_ms = p.at_ms;
  else out.offset = p.offset || 0;
  return out;
}
