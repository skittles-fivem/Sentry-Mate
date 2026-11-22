// Sentry Mate, "Pebble" UI pack: parts the data pages share.
//
// Day groups with friendly names ('Today', 'Tuesday'), the day header that sits across the grid, number tiles,
// fact lists, the SOC bar (start to end), the activity strip (what the car did, as coloured spans), filter pill
// rows, a "Show more" button for paged lists, place words and links, and the words for awake reasons and gaps.

import { h, cover } from '../shared/dom.js';
import { format, formatMoney } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import { dayKey, formatDayKey, formatTime, formatDuration, addDays, daysBetween, startOf, addPeriod, zonedParts, dateFormat } from '../shared/time.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { icon, num, btn, tile } from './ui.js';
import { display } from './display.js';
import { openSwitcher } from './switcher.js';
import { openPeriod, rangeText } from './period.js';

/** The value of a { value, at_ms } pair (or the plain value). */
export const v = (x) => (x && typeof x === 'object' && !Array.isArray(x) && 'value' in x ? x.value : x ?? null);
/** When a { value, at_ms } pair was read. */
export const at = (x) => (x && typeof x === 'object' ? x.at_ms ?? null : null);

export const AWAKE_WORDS = Object.freeze({
  sentry: 'Sentry mode', keep_climate_on: 'climate kept on', keep_climate_dog: 'Dog mode', keep_climate_camp: 'Camp mode',
  user_present: 'someone in the car', preconditioning_cabin: 'cabin preconditioning', preconditioning_battery: 'battery preconditioning',
  charging: 'charging', keep_warm: 'battery kept warm', door_open: 'a door open', accessory_power: 'accessory power on',
});
export const GAP_WORDS = Object.freeze({
  no_log: 'no log arrived for this time', not_recorded: 'recording was paused', failed_check: 'the log failed its check', dropped_frames: 'frames were dropped',
});
export const PURPOSES = Object.freeze({ business: 'Business', private: 'Private', commute: 'Commute' });

/** A drive's start or end in words: its label, saved place or short address. */
export const endLabel = (e) => (e && (e.label || (e.place && e.place.name) || (typeof e.address === 'string' ? e.address : e.address && e.address.short))) || 'Unnamed spot';
/** The line under a place name: the street or area, when it says more than the name. */
export function endSub(e) {
  if (!e) return null;
  const a = typeof e.address === 'string' ? e.address : e.address && (e.address.short || e.address.locality);
  return a && a !== endLabel(e) ? a : null;
}
/** Where to edit a saved place, or name a spot (Places with ?place= or ?new=lat,lon). */
export function placeHref(e) {
  if (!e) return null;
  const p = e.place && typeof e.place === 'object' ? e.place : null;
  if (p && p.id) return state.buildUrl('places', { query: { place: p.id } });
  if (e.lat != null && e.lon != null) return state.buildUrl('places', { query: { new: `${Number(e.lat).toFixed(5)},${Number(e.lon).toFixed(5)}` } });
  return null;
}
/** A place name that links to its place (edit) or to naming it. */
export function placeLink(e, { cover = null, still = false } = {}) {
  const href = still ? null : placeHref(e);
  const text = endLabel(e);
  return href ? h('a', { class: 'place-link', href, cover, title: e.place ? `Edit ${text}` : 'Name this place' }, text) : h('span', { class: 'place-link', cover }, text);
}

/** 'Today', 'Yesterday', a weekday within the week, else 'Tue, Nov 3'. */
export function friendlyDay(key, tz) {
  const today = dayKey(api.now(), tz);
  const n = daysBetween(key, today);
  if (n === 0) return 'Today';
  if (n === 1) return 'Yesterday';
  const [y, m, d] = key.split('-').map(Number);
  return dateFormat('UTC', { weekday: 'long' }).format(Date.UTC(y, m - 1, d, 12));
}
/** Items grouped by their day in tz, in the order given: [{ key, items }]. */
export function byDay(items, tz, when = (x) => x.start_ms) {
  const out = [];
  for (const it of items) {
    const k = dayKey(when(it), tz);
    const last = out[out.length - 1];
    if (last && last.key === k) last.items.push(it); else out.push({ key: k, items: [it] });
  }
  return out;
}
/** A day header across the grid: 'Tuesday' with the date and a summary on the right. */
export function dayHead(key, tz, side = null, { cover = null } = {}) {
  const name = friendlyDay(key, tz);
  const date = formatDayKey(key, { weekday: false, month: 'long', year: key.slice(0, 4) !== dayKey(api.now(), tz).slice(0, 4) });
  return h('div', { class: 'day-head w-full', cover },
    h('h2', { class: 'day-name' }, name, h('span', { class: 'day-date' }, date)),
    side ? h('p', { class: 'day-side' }, side) : null);
}

/** A number tile: statTile({ label, value, kind, icon, sub, w, cover, why, size, href }). */
export function statTile({ label, value = null, kind = 'number', icon: ic = null, sub = null, w = 1, cover = null, why = null, size = 'm', href = null, digits, node = null, tone = null } = {}) {
  return tile({ w, title: label, icon: ic, cover, href, cls: ['tile-stat', tone && `tone-${tone}`] },
    node || num(value, kind, { size, why, digits }),
    sub ? h('p', { class: 'tile-line' }, sub) : null);
}

/** A list of label / value rows: facts([[label, value, cover], ...]). A null value is left out. */
export function facts(rows, { cls = null, cover = null } = {}) {
  return h('dl', { class: ['facts', cls], cover }, rows.filter((r) => r && r[1] != null && r[1] !== false).map(([k, val, c]) => h('div', { class: 'fact', cover: c || null },
    h('dt', null, k), h('dd', null, val))));
}

/** The charge level from start to end as a bar: the kept part solid, the used or added part light. */
export function socBar(start, end, { cover = null, tone = 'coral' } = {}) {
  const a = Number.isFinite(start) ? Math.max(0, Math.min(100, start)) : null;
  const b = Number.isFinite(end) ? Math.max(0, Math.min(100, end)) : null;
  const bar = h('span', { class: ['socbar', `tone-${tone}`], 'aria-hidden': 'true' });
  if (a != null && b != null) {
    const lo = Math.min(a, b), hi = Math.max(a, b);
    const keep = h('span', { class: 'socbar-keep' }), delta = h('span', { class: 'socbar-delta' });
    keep.style.setProperty('--to', (lo / 100).toFixed(4));
    delta.style.setProperty('--from', (lo / 100).toFixed(4));
    delta.style.setProperty('--to', (hi / 100).toFixed(4));
    bar.append(keep, delta);
  }
  return h('div', { class: 'soc', cover, role: 'img', 'aria-label': `Battery ${a == null ? 'not known' : `${Math.round(a)} %`} to ${b == null ? 'not known' : `${Math.round(b)} %`}` },
    h('span', { class: 'soc-n' }, format(start, 'percent')), bar, h('span', { class: 'soc-n' }, format(end, 'percent')));
}

/** A row of filter pills (radios): pills({ label, value, options: [[v, text]], onchange, cover }). el.set(v). */
export function pills({ label, value, options, onchange, cover = null } = {}) {
  const el = h('div', { class: 'pills', role: 'radiogroup', 'aria-label': label, cover });
  const buttons = options.map(([val, text]) => h('button', {
    type: 'button', role: 'radio', class: 'fpill', dataset: { value: String(val) }, 'aria-checked': String(String(val) === String(value)),
    onclick: () => { pick(val); onchange?.(val); },
  }, text));
  el.append(...buttons);
  function pick(val) { for (const b of buttons) b.setAttribute('aria-checked', String(b.dataset.value === String(val))); }
  el.set = pick;
  return el;
}

/** A 'Show more' button for a paged list: more(async () => hasMore). Hidden when the list is done. */
export function moreButton(load, { text = 'Show more' } = {}) {
  const b = btn(text, { kind: 'soft', icon: 'chevronDown', cls: 'more-btn' });
  b.addEventListener('click', async () => {
    b.disabled = true;
    try { const left = await load(); b.hidden = !left; } catch { /* the toast says why */ } finally { b.disabled = false; }
  });
  return h('div', { class: 'more-row w-full' }, b);
}

/** '12 min', '2 h 05 min'. */
export const dur = (ms) => (ms == null ? '—' : formatDuration(ms));
/** '7:45 AM'. */
export const time = (ms, tz) => (ms == null ? '—' : formatTime(ms, tz));
/** A small number with its unit as text: '12.3 km'. */
export const q = (value, kind, opts) => format(value, kind, opts);
/** A count: '1,204'. */
export const count = (n) => formatNumber(n || 0);

/* ------------------------------------------------------------------ the activity strip */

export const STRIP = Object.freeze({
  driving: ['Driving', 'driving'], charging: ['Charging', 'charging'], online: ['Awake, parked', 'parked'],
  online_vehicle_bus_only: ['Awake, vehicle bus only', 'bus'], asleep: ['Asleep', 'asleep'], unknown: ['No data', 'unknown'],
  updating: ['Updating', 'updating'],
});

function stripTicks(from, to, tz) {
  const span = to - from;
  const out = [];
  if (span <= 36 * 3600e3) {
    const step = span <= 13 * 3600e3 ? 2 : 6;
    let t = startOf(from, 'hour', tz);
    while (t <= to) {
      const hr = zonedParts(t, tz).hour;
      if (t > from && t < to && hr % step === 0) out.push({ at: t, label: formatTime(t, tz).replace(':00', '') });
      t = addPeriod(t, 'hour', 1, tz);
    }
  } else if (span <= 45 * 864e5) {
    const every = span <= 10 * 864e5 ? 1 : 7;
    const fmt = dateFormat(tz, every === 1 ? { weekday: 'short' } : { month: 'short', day: 'numeric' });
    let t = startOf(from, 'day', tz);
    while (t <= to) {
      if (t > from && t < to && (every === 1 || zonedParts(t, tz).weekday === 1)) out.push({ at: t, label: fmt.format(t) });
      t = addPeriod(t, 'day', 1, tz);
    }
  } else {
    const fmt = dateFormat(tz, { month: 'short' });
    let t = startOf(from, 'month', tz);
    while (t <= to) { if (t > from && t < to) out.push({ at: t, label: fmt.format(t) }); t = addPeriod(t, 'month', 1, tz); }
  }
  return out;
}

/**
 * The activity strip: strip({ items, from_ms, to_ms }, { tz, link(seg) -> href, cover, legendCover }). Each span is
 * a coloured piece of one rounded bar (a link when it has one); ticks under it; a key of the kinds that show.
 */
export function strip(act, { tz, link = null, cover = null, legendCover = null, maxTicks = null } = {}) {
  const from = act.from_ms, to = Math.max(act.from_ms + 1, Math.min(act.to_ms, api.now()));
  const end = act.to_ms;
  const pct = (t) => Math.max(0, Math.min(1, (t - from) / (end - from)));
  const kinds = new Set();
  const segs = (act.items || []).map((s) => {
    const a = pct(s.start_ms), b = pct(s.end_ms ?? to);
    if (b <= a) return null;
    const [name, cls] = STRIP[s.kind] || STRIP.unknown;
    kinds.add(s.kind in STRIP ? s.kind : 'unknown');
    const href = link ? link(s) : null;
    const title = `${name}: ${formatTime(s.start_ms, tz)} to ${s.end_ms ? formatTime(s.end_ms, tz) : 'now'}`;
    const el = h(href ? 'a' : 'span', { class: ['strip-seg', `sk-${cls}`], href, title, 'aria-label': href ? title : null });
    el.style.setProperty('--a', a.toFixed(5));
    el.style.setProperty('--b', b.toFixed(5));
    return el;
  }).filter(Boolean);
  const future = pct(to) < 1 ? h('span', { class: 'strip-seg sk-future', title: 'Still to come' }) : null;
  if (future) { future.style.setProperty('--a', pct(to).toFixed(5)); future.style.setProperty('--b', '1'); }
  let tickList = stripTicks(from, end, tz);
  const most = maxTicks ?? (display.grid.phone ? 4 : 7);
  if (tickList.length > most) { const k = Math.ceil(tickList.length / most); tickList = tickList.filter((_, i) => i % k === k - 1); }
  const ticks = tickList.map((t) => { const el = h('span', { class: 'strip-tick' }, t.label); el.style.setProperty('--a', pct(t.at).toFixed(5)); return el; });
  return h('figure', { class: 'strip', cover },
    h('div', { class: 'strip-bar', role: 'img', 'aria-label': `What the car did from ${formatTime(from, tz)} to ${formatTime(end, tz)}` }, segs, future),
    h('div', { class: 'strip-ticks', 'aria-hidden': 'true' }, ticks),
    h('figcaption', { class: 'strip-key', cover: legendCover },
      [...kinds].map((k) => h('span', { class: 'strip-k' }, h('span', { class: ['strip-sw', `sk-${STRIP[k][1]}`], 'aria-hidden': 'true' }), STRIP[k][0]))));
}

/** Time spent per kind ('Asleep 18 h'), biggest first, from a strip's items within [from, to]. */
export function stripShares(act) {
  const tot = {};
  const end = Math.min(act.to_ms, api.now());
  for (const s of act.items || []) {
    const a = Math.max(s.start_ms, act.from_ms), b = Math.min(s.end_ms ?? end, end);
    if (b > a) tot[s.kind] = (tot[s.kind] || 0) + (b - a);
  }
  return Object.entries(tot).sort((x, y) => y[1] - x[1]);
}

export { addDays, icon };

/**
 * The filter bar under a page's header, across the grid: filterBar(ctx, { carCover, periodCover }, ...extra).
 * The car and the period are the header's own pills: the bar marks them with the page's items instead of showing
 * them a second time (core/frame.js clears the marks when the page changes). Only when the header shows no pill
 * (a page without a car or a period) does the bar get its own, then the other controls.
 */
export function filterBar(ctx, { carCover = null, periodCover = null, cls = null } = {}, ...extra) {
  const headPill = (sel) => document.querySelector(`#head ${sel}:not([hidden])`);
  let carB = null, perB = null;
  const carPill = (ctx.car || carCover) && headPill('.car-pill');
  if (carPill) { if (carCover) cover(carPill, ...[].concat(carCover)); } else {
    carB = ctx.car ? h('button', { type: 'button', class: 'fpill fpill-ic', cover: carCover, 'aria-label': `Car: ${ctx.car.name}. Switch car`, onclick: (e) => openSwitcher(e.currentTarget) },
      icon('car'), ctx.car.name) : carCover ? h('span', { class: 'fpill fpill-ic', cover: carCover }, icon('fleet'), 'Every car') : null;
  }
  const datePill = ctx.period && headPill('.date-pill');
  if (datePill) { if (periodCover) cover(datePill, ...[].concat(periodCover)); } else {
    perB = ctx.period ? h('button', { type: 'button', class: 'fpill fpill-ic', cover: periodCover, 'aria-label': 'Change the period', onclick: (e) => openPeriod(ctx.period, ctx.tz, e.currentTarget) },
      icon('calendar'), rangeText(ctx.period, ctx.tz)) : null;
  }
  const rest = extra.flat().filter(Boolean);
  return h('div', { class: ['page-bar', 'w-full', cls], hidden: !carB && !perB && !rest.length }, carB, perB, ...rest);
}

/* ------------------------------------------------------------------ money, split bars, checks */

/**
 * Money in one or more currencies, one per line, never added across: money([{ amount, currency }], { size }). With
 * more than one currency each line names its code. An empty list is a dash (with why, when given).
 */
export function money(list, { size = 'm', why = null, cover = null, kind = 'money', key = 'amount' } = {}) {
  const items = (list || []).filter((x) => x && x[key] != null);
  if (!items.length) return num(null, 'number', { size, why, cover });
  const fmt = (x) => (kind === 'money' ? formatMoney(x[key], x.currency) : format({ amount: x[key], currency: x.currency }, kind));
  return h('span', { class: ['num', `num-${size}`, 'money'], cover },
    items.map((x) => h('span', { class: 'num-n' }, fmt(x), items.length > 1 ? h('span', { class: 'money-cur' }, ` ${x.currency}`) : null)));
}

/** A bar split in parts with its key: splitBar({ label, total, parts: [{ name, value, text, tone }], cover }). */
export function splitBar({ label, total = null, parts = [], cover = null } = {}) {
  const sum = parts.reduce((a, p) => a + (p.value || 0), 0);
  const share = (p) => (sum > 0 ? (p.value || 0) / sum : 0);
  return h('div', { class: 'split', cover },
    h('div', { class: 'split-head' }, h('span', null, label), total != null ? h('b', null, total) : null),
    h('div', { class: 'split-bar', role: 'img', 'aria-label': `${label}: ${parts.map((p) => `${p.name} ${Math.round(share(p) * 100)} %`).join(', ')}` },
      parts.map((p) => { const el = h('span', { class: ['split-part', `tone-${p.tone || 'c1'}`] }); el.style.setProperty('--v', share(p).toFixed(4)); return el; })),
    h('ul', { class: 'split-key' }, parts.map((p) => h('li', null, h('span', { class: ['split-sw', `tone-${p.tone || 'c1'}`], 'aria-hidden': 'true' }),
      p.name, h('b', null, p.text ?? ''), sum > 0 ? `${Math.round(share(p) * 100)} %` : '—'))));
}

/** One check with a status word and its dot: checkRow({ name, sub, word, level: 'ok' | 'warn' | 'bad' | 'off', cover }). */
export function checkRow({ name, sub = null, word, level = 'ok', cover = null, side = null } = {}) {
  return h('li', { class: ['check', `is-${level}`], cover },
    h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
    h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, name), sub ? h('span', { class: 'check-sub' }, sub) : null),
    side || h('span', { class: 'check-word' }, word));
}

/** A heading across the grid with a line under it: sectionHead(title, text, { cover }). */
export function sectionHead(title, text = null, { cover = null } = {}) {
  return h('div', { class: 'section-head w-full', cover }, h('h2', { class: 'section-title' }, title), text ? h('p', { class: 'note' }, text) : null);
}

/** A few labelled numbers in a tile: nums([[label, node, cover]]). */
export const nums = (rows) => h('div', { class: 'tile-nums' }, rows.filter(Boolean).map(([k, val, c]) => h('div', { cover: c || null }, h('span', { class: 'hero-k' }, k), val)));
