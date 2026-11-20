// The period dial in the console row: one picker for every page that has a period. Presets (24 h, 7 days,
// 30 days, this year, all), whole days of your own choice, and steps back and forward. Days are the car's
// days (its time zone). The choice goes into the address (?preset= or ?from=&to=) and stays with the tab,
// so the next page opens with it. Hidden on pages without a period.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { PRESETS, presetLabel, shiftPeriod, dayRange, periodDays, formatDateRange, zoneName, dayKey } from '../shared/time.js';
import { icon } from './ui.js';
import { popover } from './layer.js';

const HINT = { '24h': 'The last 24 hours', '7d': 'Today and 6 days before', '30d': 'Today and 29 days before', year: 'Since 1 January', all: 'Since the first data' };
let el = null, face = null, back = null, fwd = null, pop = null;
let cur = null;   // { period, tz }

/** The dial element (the frame puts it in the console once). */
export function periodDial() {
  face = h('button', { class: 'dial-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', onclick: () => pop.toggle(face, { align: 'end' }) },
    icon('calendar'), h('span', { class: 'dial-text' }, h('span', { class: 'dial-label' }), h('span', { class: 'dial-sub' })), icon('chev-d', 'i--sm'));
  back = h('button', { class: 'dial-step', type: 'button', 'aria-label': 'Earlier period', title: 'Earlier period', onclick: () => step(-1) }, icon('chev-l'));
  fwd = h('button', { class: 'dial-step', type: 'button', 'aria-label': 'Later period', title: 'Later period', onclick: () => step(1) }, icon('chev-r'));
  pop = popover({ cls: 'period-pop', label: 'Choose the period', fill, align: 'end' });
  el = h('div', { class: 'dial', role: 'group', 'aria-label': 'Period', hidden: true }, back, face, fwd);
  return el;
}

/** Open the same picker from a button on a page. */
export function openPeriod(anchor) { if (cur) pop.toggle(anchor || face, { align: anchor ? 'start' : 'end' }); }

/** Show the page's period (resolved, in tz), or hide the dial (null). */
export function showPeriod(period, tz) {
  if (!el) return;
  el.hidden = !period;
  cur = period ? { period, tz } : null;
  if (!period) { pop.hide(); return; }
  const now = api.now();
  const span = period.from_ms != null ? formatDateRange(period.from_ms, Math.max(period.from_ms, (period.to_ms ?? now) - 1), tz, { year: true }) : 'Since the first data';
  const custom = period.preset === 'custom';
  face.querySelector('.dial-label').textContent = custom ? span : presetLabel(period.preset);
  face.querySelector('.dial-sub').textContent = `${custom ? 'Your days' : span} · ${zoneName(now, tz)}`;
  face.title = `${custom ? 'Your days' : presetLabel(period.preset)}: ${span}, in the car's time zone (${tz.replace(/_/g, ' ')})`;
  back.disabled = !shiftPeriod(period, -1, { tz, now });
  fwd.disabled = !shiftPeriod(period, 1, { tz, now }) || (period.to_ms != null && period.to_ms >= now - 1000);
}

function step(n) {
  if (!cur) return;
  const p = shiftPeriod(cur.period, n, { tz: cur.tz, now: api.now() });
  if (!p) return;
  const to = p.to_ms ?? p.to;
  state.setPeriod({ from: p.from_ms ?? p.from, to: to != null && to >= api.now() ? null : to }, { push: true });
}

function fill(box) {
  if (!cur) return;
  const { period, tz } = cur;
  const now = api.now();
  const presets = h('div', { class: 'presets' }, PRESETS.map((p) => h('button', {
    class: 'preset', type: 'button', 'aria-pressed': String(period.preset === p),
    onclick: () => { pop.hide(); state.setPeriod({ preset: p }, { push: true }); },
  }, h('span', { class: 'preset-name' }, presetLabel(p)), h('span', { class: 'preset-sub' }, HINT[p] || ''))));
  const days = period.from_ms != null ? periodDays(period, tz) : { from: dayKey(now - 29 * 86400000, tz), to: dayKey(now, tz) };
  const today = dayKey(now, tz);
  const from = h('input', { class: 'input', type: 'date', id: 'hz-from', value: days.from, max: today, required: true });
  const to = h('input', { class: 'input', type: 'date', id: 'hz-to', value: days.to, max: today, required: true });
  const err = h('p', { class: 'field-error', role: 'alert', hidden: true });
  const own = h('form', {
    class: 'span-days',
    onsubmit: (e) => {
      e.preventDefault();
      let a = from.value, b = to.value;
      if (!a || !b) { err.hidden = false; replace(err, icon('warn', 'i--sm'), h('span', null, 'Pick a first and a last day.')); return; }
      if (a > b) [a, b] = [b, a];
      const r = dayRange(a, b, tz);
      pop.hide();
      state.setPeriod({ from: r.from ?? r.from_ms, to: b >= today ? null : (r.to ?? r.to_ms) }, { push: true });
    },
  },
  h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'hz-from' }, 'First day'), from),
  h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'hz-to' }, 'Last day'), to),
  err,
  h('div', { class: 'form-acts' }, h('button', { class: 'btn btn--go btn--sm', type: 'submit' }, h('span', null, 'Show these days'))));
  replace(box,
    h('p', { class: 'pop-label' }, 'Presets'), presets,
    h('p', { class: 'pop-label' }, 'Your own days'), own,
    h('p', { class: 'acct-note' }, `Days run from midnight in the car's time zone, ${tz.replace(/_/g, ' ')} (${zoneName(now, tz)}).`));
}
