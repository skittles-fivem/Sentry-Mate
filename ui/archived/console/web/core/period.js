// The period picker: one dialog shared by every page with a period. Presets as a row of keys
// ([24h] [7d] [30d] [year] [all]), earlier / later, and a from / to pair of dates. The chosen period goes
// in the address (shared/state.js setPeriod); the router builds the page again in it.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h } from '../shared/dom.js';
import { PRESETS, shiftPeriod, periodDays, dayRange } from '../shared/time.js';
import { openDialog } from './dialog.js';
import { range } from './fmt.js';

let shown = null;   // { period (resolved), tz }

/** The frame tells this module what period the page shows (null: the page has none). */
export function setPeriodShown(period, tz) { shown = period ? { period, tz } : null; }
export const periodShown = () => shown;

/** Short text for the command bar: '7d', or 'nov 1 - nov 14'. */
export function periodShort(p, tz) {
  if (!p) return null;
  if (p.preset && p.preset !== 'custom') return p.preset;
  return p.from_ms != null ? range(p.from_ms, p.to_ms - 1, tz) : p.label;
}

export function openPeriodPicker() {
  if (!shown) return null;
  const { period, tz } = shown;
  const choose = (p) => { d.close(); state.setPeriod(p, { push: true }); };
  const presets = h('div', { class: 'keys', role: 'group', 'aria-label': 'Presets' },
    PRESETS.map((p) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(period.preset === p), onclick: () => choose({ preset: p }) }, p)));
  const earlier = shiftPeriod(period, -1, { tz, now: api.now() });
  const later = shiftPeriod(period, 1, { tz, now: api.now() });
  const shift = h('div', { class: 'keys', role: 'group', 'aria-label': 'Move the period' },
    h('button', { class: 'key', type: 'button', disabled: !earlier, onclick: () => choose({ from: earlier.from_ms, to: earlier.to_ms }) }, '< earlier'),
    h('button', { class: 'key', type: 'button', disabled: !later || later.from_ms >= api.now(), onclick: () => choose({ from: later.from_ms, to: later.to_ms }) }, 'later >'));
  const days = periodDays(period, tz);
  const from = h('input', { class: 'in in--date', type: 'date', value: days.from || '', 'aria-label': 'From', required: true });
  const to = h('input', { class: 'in in--date', type: 'date', value: days.to || '', 'aria-label': 'To', required: true });
  const msg = h('p', { class: 'fld-err', hidden: true, role: 'alert' });
  const custom = h('form', { class: 'pp-custom', onsubmit: (e) => {
    e.preventDefault();
    if (!from.value || !to.value) { msg.hidden = false; msg.textContent = 'Give both days.'; return; }
    const r = dayRange(from.value, to.value, tz);
    choose({ from: r.from, to: r.to });
  } }, h('label', { class: 'pp-lab' }, 'from: ', from), h('label', { class: 'pp-lab' }, 'to: ', to), h('button', { class: 'btn btn--primary', type: 'submit' }, 'apply'), msg);
  const d = openDialog({
    title: 'period', cls: 'dlg--period',
    body: [
      h('p', { class: 'pp-now' }, h('span', { class: 'muted' }, 'now: '), period.from_ms != null ? range(period.from_ms, period.to_ms - 1, tz) : period.label, h('span', { class: 'muted' }, ` (${period.preset})`)),
      h('p', { class: 'pp-lab' }, 'preset:'), presets,
      h('p', { class: 'pp-lab' }, 'move:'), shift,
      h('p', { class: 'pp-lab' }, 'days:'), custom,
      h('p', { class: 'note' }, `Days are the car's days (${tz}).`),
    ],
  });
  presets.querySelector('[aria-pressed="true"]')?.focus();
  return d;
}
