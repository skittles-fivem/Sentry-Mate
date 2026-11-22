// Sentry Mate, "Pebble" UI pack: Logbook (/cars/{car}/logbook), a trip log for tax and expense claims.
//
// One tile per drive, numbered in order: the date and times, from -> to with the odometer at each end, the
// distance and duration, and a purpose chip (Business, Private, Commute, or none) you tap to change it in a sheet,
// with a relation and a note. Totals per purpose on top, an export button for the CSV, and a note where the
// odometer does not join up between two drives. Data: GET /api/v1/cars/{car}/logbook; PUT and DELETE
// /cars/{car}/drives/{drive}/purpose (admins).

import { h } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatTime, formatDate } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, chip, icon } from '../core/ui.js';
import { emptyTile } from '../core/states.js';
import { openSheet } from '../core/sheet.js';
import { field, segmented } from '../core/form.js';
import { byDay, dayHead, filterBar, moreButton, PURPOSES } from '../core/bits.js';

export const id = 'logbook';
export const css = true;
export const shape = [['full', 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [2, 'h1'], [2, 'h1']];
export const title = () => 'Logbook';

const TONES = { business: 'sky', private: 'coral', commute: 'teal' };

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const path = api.apiPath`cars/${car.id}/logbook`;
  let data = await ctx.api.get(path, ctx.params);
  if (!ctx.alive()) return {};
  const exp = h('a', { class: 'btn btn-primary', href: ctx.api.csvUrl(path, ctx.params), download: `${(car.name || 'car').toLowerCase().replace(/[^a-z0-9]+/g, '-')}-logbook.csv`, cover: 'dt.export.csv' }, icon('download'), h('span', { class: 'btn-text' }, 'Export CSV'));
  ctx.head({ actions: [exp] });

  function draw() {
    const rows = data.rows || [];
    const tot = data.totals_by_purpose || {};
    const bar = filterBar(ctx, { carCover: 'dt.control.per_car', periodCover: 'dt.filter.time_range' });
    const totals = ['business', 'private', 'commute', 'none'].map((k) => tile({ w: 1, title: k === 'none' ? 'No purpose yet' : PURPOSES[k], icon: k === 'none' ? 'help' : 'book', cls: ['tile-stat', `tone-${TONES[k] || 'muted'}`] },
      num(tot[k]?.distance_km ?? 0, 'distance'), h('p', { class: 'tile-line' }, `${formatNumber(tot[k]?.drives ?? 0)} drives`)));
    if (!rows.length) {
      ctx.view.replaceChildren(bar, ...totals, emptyTile({ icon: 'book', title: 'No drives in this period', text: 'Pick another period with the date pill.',
        cover: ['dt.table', 'dt.col.id', 'dt.col.start_time', 'dt.col.start_odometer', 'dt.col.start_address', 'dt.col.end_time', 'dt.col.end_odometer', 'dt.col.end_address', 'dt.col.duration', 'dt.col.distance'] }));
      return;
    }
    let shown = 0, lastDay = null;
    const more = moreButton(async () => { add(60); return shown < rows.length; }, { text: 'Show more drives' });
    const add = (n) => {
      const part = rows.slice(shown, shown + n);
      shown += part.length;
      for (const g of byDay(part, tz)) {
        if (g.key !== lastDay) {
          const km = rows.filter((r) => byDay([r], tz)[0].key === g.key).reduce((s, r) => s + (r.distance_km || 0), 0);
          ctx.view.insertBefore(dayHead(g.key, tz, format(km, 'distance')), more);
          lastDay = g.key;
        }
        for (const r of g.items) ctx.view.insertBefore(rowTile(r), more);
      }
      more.querySelector('button').hidden = shown >= rows.length;
    };
    ctx.view.replaceChildren(bar, ...totals, h('div', { class: 'tl-list-start w-full', cover: 'dt.table' }, h('p', { class: 'note' }, `${formatNumber(rows.length)} drives, oldest first.`)), more);
    add(60);
  }

  function rowTile(r) {
    const p = r.purpose;
    const purpose = ctx.admin
      ? h('button', { type: 'button', class: ['chip', 'chip-btn', `tone-${TONES[p] || 'muted'}`], title: 'Change the purpose', onclick: () => edit(r) }, h('span', null, p ? PURPOSES[p] : 'Set purpose'), icon('chevronDown'))
      : chip(p ? PURPOSES[p] : 'No purpose', { tone: TONES[p] || 'muted' });
    return tile({ w: 2, cls: ['item', 'lb-row'] },
      h('div', { class: 'item-top' },
        h('span', { class: 'lb-seq', cover: 'dt.col.id', title: `Drive ${r.drive}` }, formatNumber(r.seq)),
        h('div', { class: 'item-text' },
          h('p', { class: 'item-sub' },
            h('span', { cover: 'dt.col.start_time' }, formatTime(r.start_ms, tz)), ' – ', h('span', { cover: 'dt.col.end_time' }, r.end_ms ? formatTime(r.end_ms, tz) : 'now'))),
        purpose),
      h('div', { class: 'lb-ends' },
        h('div', { class: 'lb-end' }, h('span', { class: 'lb-dot tone-teal', 'aria-hidden': 'true' }),
          h('span', { class: 'lb-place', cover: 'dt.col.start_address' }, r.start_place || r.start_address || 'Unnamed spot', r.start_place && r.start_address ? h('span', { class: 'item-sub' }, r.start_address) : null),
          h('span', { class: 'lb-odo', cover: 'dt.col.start_odometer' }, format(r.start_odometer_km, 'odometer'))),
        h('div', { class: 'lb-end' }, h('span', { class: 'lb-dot tone-coral', 'aria-hidden': 'true' }),
          h('span', { class: 'lb-place', cover: 'dt.col.end_address' }, r.in_progress ? 'Still driving' : r.end_place || r.end_address || 'Unnamed spot', r.end_place && r.end_address ? h('span', { class: 'item-sub' }, r.end_address) : null),
          h('span', { class: 'lb-odo', cover: 'dt.col.end_odometer' }, format(r.end_odometer_km, 'odometer')))),
      h('div', { class: 'item-nums' },
        h('span', { cover: 'dt.col.distance' }, num(r.distance_km, 'distance', { size: 's' })),
        h('span', { cover: 'dt.col.duration' }, num(r.duration_ms, 'duration', { size: 's' }))),
      r.relation || r.note ? h('p', { class: 'item-sub' }, [r.relation, r.note].filter(Boolean).join(' · ')) : null,
      r.continuity_gap_km ? h('p', { class: 'lb-gap' }, icon('warning'), `The odometer moved ${format(r.continuity_gap_km, 'distance')} since the drive before, with no drive logged.`) : null,
      h('p', { class: 'item-links tile-foot' }, h('a', { href: state.buildUrl('drive', { car: car.id, drive: r.drive }) }, 'Drive details'),
        r.purpose_source === 'rule' ? h('span', { class: 'muted' }, 'purpose set by a rule') : null));
  }

  async function edit(r) {
    let chosen = r.purpose || 'none';
    const seg = segmented({ label: 'Purpose', name: 'purpose', value: chosen, options: [['business', 'Business'], ['private', 'Private'], ['commute', 'Commute'], ['none', 'None']], onchange: (v) => { chosen = v; } });
    const rel = field({ label: 'Relation', name: 'relation', value: r.relation || '', hint: 'Who the drive was for: a customer, an employer, a project.', attrs: { maxlength: 100, autocomplete: 'off' } });
    const nt = field({ label: 'Note', name: 'note', value: r.note || '', attrs: { maxlength: 500, autocomplete: 'off' } });
    const err = h('p', { class: 'form-error', role: 'alert', hidden: true });
    const saved = await openSheet({
      title: `Drive ${formatNumber(r.seq)}, ${formatDate(r.start_ms, tz, { now: api.now() })}`,
      body: h('div', { class: 'form' }, seg, rel, nt, err),
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
    data = await ctx.api.get(path, ctx.params);
    if (!ctx.alive()) return;
    draw();
    ctx.toast('The purpose is saved.');
  }

  draw();
  ctx.ready();
  return {};
}


