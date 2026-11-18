// Sentry Mate, "Logbook" UI pack: the shared period picker (in the masthead) and period resolution.
//
// One period for every report page, in the car's time zone: Last 24 hours, Last 7 days (today and the
// six days before), Last 30 days, This year, All time, or custom dates. It lives in the URL (?preset=7d or
// ?from=&to=, shared/state.js), so it carries over when the page or the car changes, and the tab remembers
// it for links that have none. A page that shows no period keeps the choice for the next one.
//
// resolveFor(info, state, tz) turns the state into { preset, from_ms, to_ms, label } for a page, using
// the page's own default (pages.js) when nothing was chosen; periodParams() then gives the API query.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { resolvePeriod, periodLabel, shiftPeriod, dayRange, periodDays, PRESETS, presetLabel, formatDateRange, zoneName, dateFormat } from '../shared/time.js';
import { icon } from './ui.js';
import { popover } from './dialog.js';

/** The period of a page now, resolved in tz; null for a page without a period. */
export function resolveFor(info, s, tz) {
  if (!info || !info.period) return null;
  return resolvePeriod(s.period || { preset: info.period }, { now: api.now(), tz, fallback: info.period });
}

/** 'Nov 10 – 16, 2026' for a resolved period (the dates it covers, in the zone). */
export function rangeText(r, tz) {
  if (!r) return '';
  if (r.from_ms == null) return `until ${formatDateRange(r.to_ms - 1, r.to_ms - 1, tz)}`;
  if (r.preset === '24h') return `since ${dateFormat(tz, { weekday: 'short', hour: 'numeric', minute: '2-digit' }).format(r.from_ms)}`;
  return formatDateRange(r.from_ms, Math.max(r.from_ms, r.to_ms - 1), tz);
}

/**
 * The picker in the masthead: createPeriodPicker(slot) -> { update(ctx) }. update() is called by the
 * router for every page: it shows the page's period, or that the page uses none.
 */
export function createPeriodPicker(slot) {
  const name = h('span', { class: 'trigger-name' });
  const meta = h('span', { class: 'trigger-meta' });
  const trigger = h('button', { type: 'button', class: 'period-trigger', 'aria-haspopup': 'dialog' },
    icon('calendar', 'icon trigger-icon'), h('span', { class: 'trigger-text' }, name, meta), icon('chevronDown', 'icon trigger-caret'));
  const presets = h('div', { class: 'period-presets', role: 'radiogroup', 'aria-label': 'Period' });
  const from = h('input', { type: 'date', class: 'field-input field-date', id: 'period-from', 'aria-label': 'From' });
  const to = h('input', { type: 'date', class: 'field-input field-date', id: 'period-to', 'aria-label': 'To' });
  const err = h('p', { class: 'field-error', hidden: true, role: 'alert' });
  const zone = h('p', { class: 'period-zone' });
  const earlier = h('button', { type: 'button', class: 'button button-quiet button-small' }, icon('chevronLeft'), 'Earlier');
  const later = h('button', { type: 'button', class: 'button button-quiet button-small' }, 'Later', icon('chevronRight'));
  const custom = h('form', { class: 'period-custom', novalidate: true },
    h('p', { class: 'pop-title' }, 'Custom dates'),
    h('div', { class: 'period-dates' },
      h('label', { class: 'field' }, h('span', { class: 'field-label' }, 'From'), from),
      h('label', { class: 'field' }, h('span', { class: 'field-label' }, 'To'), to),
      h('button', { type: 'submit', class: 'button button-small' }, 'Show')),
    err);
  const panel = h('div', { class: 'pop period-pop', id: 'period-pop', role: 'dialog', 'aria-label': 'Choose the period' },
    h('p', { class: 'pop-title' }, 'Period'), presets, custom,
    h('div', { class: 'period-shift' }, earlier, later), zone);
  const wrap = h('div', { class: 'pop-wrap period-picker' }, trigger, panel);
  slot.replaceChildren(wrap);
  const pop = popover(trigger, panel, { focus: '[aria-checked="true"]' });

  let cur = null; // { info, period, tz }
  const choose = (p) => { pop.close({ refocus: true }); state.setPeriod(p, { push: true }); };

  presets.append(...PRESETS.map((p) => h('button', {
    type: 'button', role: 'radio', class: 'period-preset', dataset: { preset: p }, 'aria-checked': 'false',
    onclick: () => choose({ preset: p }),
  }, presetLabel(p))));
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

  return {
    update({ info, period, tz }) {
      cur = { info, period, tz };
      const has = !!period;
      trigger.disabled = !has;
      wrap.classList.toggle('is-off', !has);
      const kept = state.current().period;
      if (has) {
        name.textContent = periodLabel(period, tz, api.now());
        meta.textContent = period.preset === 'custom' ? zoneName(api.now(), tz) : `${rangeText(period, tz)} · ${zoneName(api.now(), tz)}`;
        trigger.title = `Period: ${name.textContent}. Days are counted in ${tz.replace(/_/g, ' ')} time.`;
      } else {
        name.textContent = kept ? (kept.preset ? presetLabel(kept.preset) : 'Custom period') : 'Period';
        meta.textContent = 'Not used on this page';
        trigger.title = 'This page shows no period; the chosen one is kept for the next page.';
      }
      for (const b of presets.children) b.setAttribute('aria-checked', String(has && b.dataset.preset === period.preset));
      if (has) {
        const d = periodDays(period, tz);
        from.value = d.from || '';
        to.value = d.to || '';
        const canShift = period.from_ms != null;
        earlier.disabled = !canShift;
        later.disabled = !canShift || period.to_ms >= api.now() - 60000;
        zone.textContent = `Days start at midnight in the car's time zone (${tz.replace(/_/g, ' ')}).`;
      }
    },
    close: () => pop.close(),
  };
}
