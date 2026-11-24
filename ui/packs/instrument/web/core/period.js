// The shared period picker in the top bar: presets (24 h, 7 days, 30 days, this year, all), a custom
// span of whole days, and steps back and forward. Days are the car's own days (its time zone). The
// choice goes into the address (?preset= or ?from=&to=) and stays with the browser tab, so every page
// that shows a period opens with it. The frame shows it only on pages with a period.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { PRESETS, presetLabel, shiftPeriod, dayRange, periodDays, formatDateRange, zoneName, dayKey } from '../shared/time.js';
import { icon } from './icons.js';
import { placePopover } from './popover.js';

let root = null, btn = null, pop = null, prevBtn = null, nextBtn = null;
let cur = null; // { period (resolved), tz }

const SHORT = { '24h': '24 h', '7d': '7 days', '30d': '30 days', year: 'This year', all: 'All', custom: 'Custom' };

/** The picker's element (the frame puts it in the top bar once). */
export function periodPicker() {
  btn = h('button', { class: 'period-btn', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', onclick: toggle },
    icon('calendar'), h('span', { class: 'period-text' }, h('span', { class: 'period-label' }), h('span', { class: 'period-sub' })),
    icon('chev-d', 'ico--sm'));
  prevBtn = h('button', { class: 'btn btn--ghost btn--icon period-step', type: 'button', 'aria-label': 'Earlier period', title: 'Earlier period', onclick: () => step(-1) }, icon('chev-l'));
  nextBtn = h('button', { class: 'btn btn--ghost btn--icon period-step', type: 'button', 'aria-label': 'Later period', title: 'Later period', onclick: () => step(1) }, icon('chev-r'));
  pop = h('div', { class: 'popover period-pop', popover: 'auto', role: 'dialog', 'aria-label': 'Choose the period' });
  pop.addEventListener('toggle', (e) => { btn.setAttribute('aria-expanded', String(e.newState === 'open')); });
  root = h('div', { class: 'period', role: 'group', 'aria-label': 'Period' }, prevBtn, btn, nextBtn, pop);
  return root;
}

/** Show the period of the page now (resolved, in tz), or hide the picker (null). */
export function setPeriodShown(period, tz) {
  if (!root) return;
  root.hidden = !period;
  cur = period ? { period, tz } : null;
  if (!period) { if (pop.matches(':popover-open')) pop.hidePopover(); return; }
  const label = period.preset === 'custom' ? 'Custom' : presetLabel(period.preset);
  const now = api.now();
  const span = period.from_ms != null ? formatDateRange(period.from_ms, Math.max(period.from_ms, (period.to_ms ?? now) - 1), tz, { year: true }) : 'Since the first data';
  btn.querySelector('.period-label').textContent = period.preset === 'custom' ? span : label;
  btn.querySelector('.period-sub').textContent = `${period.preset === 'custom' ? 'Custom' : span} · ${zoneName(now, tz)}`;
  btn.title = `${label}: ${span}, in the car's time zone (${tz.replace(/_/g, ' ')})`;
  const back = shiftPeriod(period, -1, { tz, now });
  const fwd = shiftPeriod(period, 1, { tz, now });
  prevBtn.disabled = !back;
  nextBtn.disabled = !fwd || (period.to_ms != null && period.to_ms >= now - 1000);
}

// the period now shown, as state.setPeriod takes it
function shown() {
  const p = cur?.period;
  if (!p) return null;
  return p.preset && p.preset !== 'custom' ? { preset: p.preset } : { from: p.from_ms, to: p.to_ms ?? null };
}

function step(n) {
  if (!cur) return;
  const p = shiftPeriod(cur.period, n, { tz: cur.tz, now: api.now() });
  if (!p) return;
  const to = p.to_ms ?? p.to;
  const from = p.from_ms ?? p.from;
  // a step that reaches now becomes "until now" again
  state.setPeriod({ from, to: to != null && to >= api.now() ? null : to }, { push: true, was: shown() });
}

function toggle() {
  if (pop.matches(':popover-open')) pop.hidePopover();
  else { fill(); pop.showPopover(); placePopover(pop, btn, { align: 'end' }); }
}

/** Open the shared period picker from a button on a page (it is the same picker as in the top bar). */
export function openPeriodPicker(anchor = null) {
  if (!pop || !cur) return;
  if (pop.matches(':popover-open')) { pop.hidePopover(); return; }
  fill();
  pop.showPopover();
  placePopover(pop, anchor || btn, { align: anchor ? 'start' : 'end' });
}

const FIRST_DAY = '1970-01-02';

function fill() {
  if (!cur) return;
  const { period, tz } = cur;
  const presets = h('div', { class: 'period-presets' }, PRESETS.map((p) => h('button', {
    class: 'period-preset', type: 'button', 'aria-pressed': String(period.preset === p),
    onclick: () => { pop.hidePopover(); state.setPeriod({ preset: p }, { push: true, was: shown() }); },
  }, h('span', null, presetLabel(p)), h('span', { class: 'faint' }, SHORT[p]))));

  const days = period.from_ms != null ? periodDays(period, tz) : { from: dayKey(api.now() - 29 * 86400000, tz), to: dayKey(api.now(), tz) };
  const today = dayKey(api.now(), tz);
  // no day before 1970 (a year mistyped as 0206): the API has no such time; the day after, so it holds in every zone
  const fromIn = h('input', { class: 'input', type: 'date', value: days.from, min: FIRST_DAY, max: today, id: 'period-from', required: true });
  const toIn = h('input', { class: 'input', type: 'date', value: days.to, min: FIRST_DAY, max: today, id: 'period-to', required: true });
  const err = h('p', { class: 'form-error', role: 'alert', hidden: true });
  const apply = (e) => {
    e.preventDefault();
    let a = fromIn.value, b = toIn.value;
    if (!a || !b) { err.hidden = false; replace(err, icon('warn'), 'Pick a first and a last day.'); return; }
    if (a > b) [a, b] = [b, a];
    if (a < FIRST_DAY) { err.hidden = false; replace(err, icon('warn'), 'Pick days from 1970 on.'); (fromIn.value < FIRST_DAY ? fromIn : toIn).focus(); return; }
    const r = dayRange(a, b, tz);
    pop.hidePopover();
    state.setPeriod({ from: r.from ?? r.from_ms, to: b >= today ? null : (r.to ?? r.to_ms) }, { push: true, was: shown() });
  };
  const custom = h('form', { class: 'period-custom', onsubmit: apply },
    h('label', { class: 'form-label', for: 'period-from' }, 'First day'),
    h('label', { class: 'form-label', for: 'period-to' }, 'Last day'),
    fromIn, toIn, err,
    h('div', { class: 'form-actions' }, h('button', { class: 'btn btn--primary btn--sm', type: 'submit' }, 'Show these days')));
  replace(pop,
    h('div', { class: 'account-menu-label' }, 'Period'),
    presets, custom,
    h('p', { class: 'period-tz' }, `Days start at midnight in the car's time zone, ${tz.replace(/_/g, ' ')} (${zoneName(api.now(), tz)}).`));
}
