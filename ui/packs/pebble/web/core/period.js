// Sentry Mate, "Pebble" UI pack: the period, resolved and picked.
//
// One period for every page that shows one, in the car's time zone: Last 24 hours, Last 7 days, Last 30 days,
// This year, All time, or dates of your own. It lives in the address (?preset=7d or ?from=&to=, shared/state.js),
// so it goes with you to the next page and the next car. resolveFor() turns the address into { preset, from_ms,
// to_ms, label } for a page, with the page's own default when nothing was chosen. The date-range pill in each
// page's header opens the period sheet (openPeriod()).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { resolvePeriod, periodLabel, shiftPeriod, dayRange, periodDays, PRESETS, presetLabel, formatDateRange, dateFormat, zoneName } from '../shared/time.js';
import { createSheet, sheetRow, sheetGroup } from './sheet.js';
import { btn } from './ui.js';

/**
 * A link that opens a page on one span without making it the tab's period: spanHref('trip', { car, from, to }).
 * The span rides in the page's own query (span_from, span_to), which the period of the tab does not keep.
 */
export function spanHref(page, { car = null, from, to, query = null } = {}) {
  return state.buildUrl(page, { car, query: { ...(query || {}), span_from: String(Math.round(from)), span_to: String(Math.round(to)) } });
}
function spanOf(s) {
  if (!s || s.periodFromUrl || !s.query) return null;
  const a = Number(s.query.span_from), b = Number(s.query.span_to);
  return Number.isFinite(a) && Number.isFinite(b) && a >= 0 && b > a ? { from: a, to: b } : null;
}

/** The period of a page now, resolved in tz; null for a page without a period. */
export function resolveFor(info, s, tz) {
  if (!info || !info.period) return null;
  return resolvePeriod(spanOf(s) || s.period || { preset: info.period }, { now: api.now(), tz, fallback: info.period });
}

/** 'Nov 10 – 16, 2026' for a resolved period. */
export function rangeText(r, tz) {
  if (!r) return '';
  if (r.from_ms == null) return `until ${formatDateRange(r.to_ms - 1, r.to_ms - 1, tz)}`;
  if (r.preset === '24h') return `since ${dateFormat(tz, { weekday: 'short', hour: 'numeric', minute: '2-digit' }).format(r.from_ms)}`;
  return formatDateRange(r.from_ms, Math.max(r.from_ms, r.to_ms - 1), tz);
}

/** The words on the date-range pill: 'Last 7 days' and 'Nov 10 – 16'. */
export function pillWords(r, tz) {
  if (!r) return { name: 'No period', dates: '' };
  return { name: periodLabel(r, tz, api.now()), dates: r.preset === 'custom' ? '' : rangeText(r, tz) };
}

let sheet = null;
let cur = null; // { period, tz }
let parts = null;

function build() {
  const presets = h('div', { class: 'rows-list', role: 'radiogroup', 'aria-label': 'Period' });
  const from = h('input', { type: 'date', class: 'input', id: 'period-from', name: 'from' });
  const to = h('input', { type: 'date', class: 'input', id: 'period-to', name: 'to' });
  const err = h('p', { class: 'field-error', role: 'alert', hidden: true });
  const custom = h('form', { class: 'period-custom', novalidate: true },
    h('div', { class: 'period-dates' },
      h('label', { class: 'field' }, h('span', { class: 'label' }, 'From'), from),
      h('label', { class: 'field' }, h('span', { class: 'label' }, 'To'), to)),
    err,
    h('div', { class: 'sheet-actions' }, btn('Show these dates', { kind: 'primary', type: 'submit', icon: 'calendar' })));
  const earlier = btn('Earlier', { kind: 'soft', icon: 'chevronLeft' });
  const later = btn('Later', { kind: 'soft', icon: 'chevronRight', cls: 'btn-end' });
  const zone = h('p', { class: 'sheet-note' });
  sheet = createSheet({ title: 'Period', cls: 'period-sheet' });
  sheet.body.append(
    h('section', { class: 'rows' }, h('h3', { class: 'rows-name' }, 'Quick picks'), presets),
    h('div', { class: 'period-shift' }, earlier, later),
    sheetGroup('Pick dates', [custom]),
    zone);
  const choose = (p) => { sheet.close(); state.setPeriod(p, { push: true }); };
  presets.append(...PRESETS.map((p) => sheetRow({ name: presetLabel(p), icon: 'calendar', role: 'radio', checked: false, onclick: () => choose({ preset: p }) })));
  presets.querySelectorAll('.row').forEach((r, i) => { r.dataset.preset = PRESETS[i]; });
  custom.addEventListener('submit', (e) => {
    e.preventDefault();
    if (!cur) return;
    if (!from.value || !to.value) { err.textContent = 'Choose both dates.'; err.hidden = false; return; }
    if (from.value > to.value) { err.textContent = 'The first date is after the second.'; err.hidden = false; return; }
    err.hidden = true;
    choose(dayRange(from.value, to.value, cur.tz));
  });
  earlier.addEventListener('click', () => { if (cur?.period) { const p = shiftPeriod(cur.period, -1, { tz: cur.tz, now: api.now() }); if (p) choose({ from: p.from_ms, to: p.to_ms }); } });
  later.addEventListener('click', () => {
    if (!cur?.period) return;
    const p = shiftPeriod(cur.period, 1, { tz: cur.tz, now: api.now() });
    if (p) choose({ from: p.from_ms, to: Math.min(p.to_ms, api.now()) });
  });
  parts = { presets, from, to, earlier, later, zone, err };
}

/** Open the period sheet for a page's period (from the date-range pill, or a button on a page). */
export function openPeriod(period, tz, opener) {
  if (!period) return;
  if (!sheet) build();
  cur = { period, tz };
  for (const r of parts.presets.children) {
    const on = r.dataset.preset === period.preset;
    r.setAttribute('aria-checked', String(on));
    r.classList.toggle('is-current', on);
  }
  const d = periodDays(period, tz);
  parts.from.value = d.from || '';
  parts.to.value = d.to || '';
  parts.err.hidden = true;
  const canShift = period.from_ms != null;
  parts.earlier.disabled = !canShift;
  parts.later.disabled = !canShift || period.to_ms >= api.now() - 60000;
  parts.zone.textContent = `Days start at midnight in ${tz.replace(/_/g, ' ')} time (${zoneName(api.now(), tz)}), the car's own time zone.`;
  sheet.open(opener);
}
