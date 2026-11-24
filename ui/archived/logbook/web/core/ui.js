// Sentry Mate, "Logbook" UI pack: the parts every page is built from.
//
// Page heads, sections, figures, states, notes, chips, buttons, loading / empty / error views, icons and
// the small drawings (battery bar, route sketch). Everything is built with h() from shared/dom.js, so
// text from the API is always text. Styles are in app.css under the same class names.

import { h, svg, cover as markCover } from '../shared/dom.js';
import { format, parts } from '../shared/units.js';
import * as state from '../shared/state.js';
import * as api from '../shared/api.js';
import { PAGE_FAILED, isPageFailed } from '../shared/build.js';
import { formatRelative } from '../shared/time.js';

/* ------------------------------------------------------------------ icons */

const ICONS = {
  search: [['circle', { cx: 8.5, cy: 8.5, r: 5.5 }], ['path', { d: 'm13 13 4 4' }]],
  chevronRight: [['path', { d: 'm8 5 5 5-5 5' }]],
  chevronLeft: [['path', { d: 'm12 5-5 5 5 5' }]],
  chevronDown: [['path', { d: 'm5 8 5 5 5-5' }]],
  arrowRight: [['path', { d: 'M4 10h12M11 5l5 5-5 5' }]],
  close: [['path', { d: 'm5 5 10 10M15 5 5 15' }]],
  plug: [['path', { d: 'M7 3v4M13 3v4M5 7h10v3a5 5 0 0 1-10 0V7ZM10 15v3' }]],
  reset: [['path', { d: 'M4 10a6 6 0 1 0 2-4.5M4 3v3.5h3.5' }]],
  download: [['path', { d: 'M10 3v10M5.5 8.5 10 13l4.5-4.5M4 17h12' }]],
  menu: [['path', { d: 'M3.5 6h13M3.5 10h13M3.5 14h13' }]],
  calendar: [['rect', { x: 3, y: 4.5, width: 14, height: 12, rx: 1.5 }], ['path', { d: 'M3 8.5h14M7 2.5v4M13 2.5v4' }]],
  person: [['circle', { cx: 10, cy: 7, r: 3.2 }], ['path', { d: 'M3.8 17c.8-3.3 3.3-5 6.2-5s5.4 1.7 6.2 5' }]],
  warning: [['path', { d: 'M10 3 2.5 16.5h15L10 3Z' }], ['path', { d: 'M10 8v4M10 14.2v.1' }]],
  info: [['circle', { cx: 10, cy: 10, r: 7 }], ['path', { d: 'M10 9v5M10 6.2v.1' }]],
  check: [['path', { d: 'm4.5 10.5 3.5 3.5 7.5-8' }]],
  pin: [['path', { d: 'M10 17s-5-4.6-5-8.5a5 5 0 0 1 10 0c0 3.9-5 8.5-5 8.5Z' }], ['circle', { cx: 10, cy: 8.5, r: 1.8 }]],
  star: [['path', { d: 'm10 3 2.1 4.4 4.8.6-3.5 3.3.9 4.8-4.3-2.4-4.3 2.4.9-4.8L3.1 8l4.8-.6L10 3Z' }]],
  settings: [['circle', { cx: 10, cy: 10, r: 2.6 }], ['path', { d: 'M10 2.5v2.2M10 15.3v2.2M2.5 10h2.2M15.3 10h2.2M4.7 4.7l1.6 1.6M13.7 13.7l1.6 1.6M4.7 15.3l1.6-1.6M13.7 6.3l1.6-1.6' }]],
};

/** A line icon: icon('search'). Decorative (aria-hidden); say what it means in text next to it. */
export function icon(name, cls = 'icon') {
  return svg('svg', { class: cls, viewBox: '0 0 20 20', 'aria-hidden': 'true', focusable: 'false' },
    (ICONS[name] || []).map(([tag, attrs]) => svg(tag, attrs)));
}

/* ------------------------------------------------------------------ text bits */

/** A quieter separator dot between facts on one line. */
export const sep = () => h('span', { class: 'sep', 'aria-hidden': 'true' }, ' · ');

/**
 * A value with its unit, the unit quieter: q(12.3, 'distance') -> <span class="q">7.6 <span class="q-u">mi</span>.
 * A missing value shows '—' and, when opts.why is given, the reason in the open.
 */
export function q(value, kind, opts = {}) {
  const p = parts(value, kind, opts);
  if (p.missing) return h('span', { class: ['q', 'is-missing', opts.cls] }, h('span', { class: 'q-v' }, p.number), p.why ? [' ', h('span', { class: 'q-why' }, p.why)] : null);
  return h('span', { class: ['q', opts.cls] }, h('span', { class: 'q-v' }, p.number), p.unit ? h('span', { class: 'q-u' }, p.unit) : null);
}

/** '+15.2 kWh' with a quiet plus sign. */
export const plus = (node) => h('span', { class: 'plus' }, h('span', { class: 'plus-sign', 'aria-hidden': 'true' }, '+'), h('span', { class: 'sr-only' }, 'plus '), node);

/** A time that keeps saying how long ago it was ('5 min ago'); the frame refreshes [data-ago] every 20 s. */
export function ago(ms, { cls = null, stale = false } = {}) {
  if (ms == null) return h('span', { class: 'ago is-missing' }, '—');
  return h('time', { class: ['ago', cls, { 'is-stale': stale }], datetime: new Date(ms).toISOString(), dataset: { ago: String(ms) } }, formatRelative(ms, api.now()));
}
/** Update every [data-ago] under root (the frame calls this on a timer). */
export function refreshAges(root = document) {
  const now = api.now();
  for (const el of root.querySelectorAll('[data-ago]')) el.textContent = formatRelative(Number(el.dataset.ago), now);
}

/* ------------------------------------------------------------------ states of a car */

export const STATE_LABEL = Object.freeze({ driving: 'Driving', charging: 'Charging', parked: 'Parked', asleep: 'Asleep', unknown: 'Unknown', online: 'Awake', updating: 'Updating' });
/** 'Parked', or 'Parked · stale' when no upload came for over an hour. */
export const stateLabel = (s, stale = false) => (STATE_LABEL[s] || s || 'Unknown') + (stale && s !== 'unknown' ? ' · stale' : '');
/** A state with its dot: stateMark('charging'). */
export function stateMark(s, { stale = false, extra = null } = {}) {
  return h('span', { class: ['state', `state-${s || 'unknown'}`, { 'is-stale': stale }] },
    h('span', { class: 'state-dot', 'aria-hidden': 'true' }), stateLabel(s, stale), extra);
}

/* ------------------------------------------------------------------ page structure */

/**
 * The head of a page: pageHead({ eyebrow: 'Comet', title: 'Drives', deck: 'Every drive …', aside, cover }).
 * title may be a node (for a styled title). aside sits on the right (buttons, an "as of" note).
 */
export function pageHead({ eyebrow = null, title, deck = null, aside = null, cover = null, cls = null } = {}) {
  return h('header', { class: ['page-head', cls], cover },
    h('div', { class: 'page-head-main' },
      eyebrow ? h(typeof eyebrow === 'string' ? 'p' : 'div', { class: 'eyebrow' }, eyebrow) : null,
      h('h1', { class: 'page-title' }, title),
      deck ? h('p', { class: 'deck' }, deck) : null),
    aside ? h('div', { class: 'page-head-aside' }, aside) : null);
}

/**
 * A titled block of a page: section({ title: 'Tyres', note: 'psi', actions, cover }, ...children).
 * The title is a small-caps label with a rule above; actions sit at its right (a CSV button, a toggle).
 */
export function section({ title = null, note = null, actions = null, cover = null, cls = null, id = null } = {}, ...children) {
  const head = title || actions ? h('div', { class: 'block-head' },
    title ? h('h2', { class: 'label', id: id ? `${id}-title` : null }, title, note ? h('span', { class: 'label-note' }, note) : null) : h('span'),
    actions ? h('div', { class: 'block-actions' }, actions) : null) : null;
  return h('section', { class: ['block', cls], id, cover, 'aria-labelledby': id && title ? `${id}-title` : null }, head, children);
}

/** One figure: name, big value, a line saying how it was worked out (or why it is blank). */
export function figure(name, value, noteText = null, { cover = null, cls = null } = {}) {
  return h('div', { class: ['fig', cls], cover },
    h('p', { class: 'fig-name' }, name),
    h('p', { class: 'fig-value' }, value),
    noteText ? h('p', { class: 'fig-note' }, noteText) : null);
}
/** A row of figures with hairlines between them: figures([figure(...), ...]). */
export const figures = (list, { cls = null, cover = null } = {}) => h('div', { class: ['figs', cls], cover }, list);

/** A plain explanation line next to numbers (how a figure was worked out). */
export const note = (text, { cls = null } = {}) => h('p', { class: ['note', cls] }, text);

/** A small rounded label: chip('Plugged in', { kind: 'plug', icon: 'plug' }). */
export function chip(text, { kind = null, iconName = null, cover = null, title = null } = {}) {
  return h('span', { class: ['chip', kind && `chip-${kind}`], cover, title }, iconName ? icon(iconName) : null, text);
}
/** A short uppercase flag (STALE, ESTIMATED, IMPORTED). */
export const flag = (text, { kind = null, title = null } = {}) => h('span', { class: ['flag', kind && `flag-${kind}`], title }, text);

/** A link to a page that carries the car and period: pageLink('Drives', 'drives'). */
export function pageLink(text, page, params = {}, { cls = 'link-arrow', arrow = true, cover = null } = {}) {
  let href = null;
  try { href = state.linkTo(page, params); } catch { href = '/'; }
  return h('a', { class: cls, href, cover }, text, arrow ? icon('arrowRight') : null);
}

/** A button: button('Save', { onclick, kind: 'quiet' | 'primary' | 'danger', iconName }). */
export function button(text, { onclick = null, kind = 'quiet', iconName = null, type = 'button', disabled = false, cover = null, title = null, cls = null } = {}) {
  return h('button', { type, class: ['button', kind !== 'primary' && `button-${kind}`, cls], onclick, disabled, cover, title },
    iconName ? icon(iconName) : null, text);
}

/* ------------------------------------------------------------------ loading, empty, error */

/** The shimmering stand-in while a page loads (the router shows it; pages may use it for a part). */
export function loadingView({ lines = 6 } = {}) {
  return h('div', { class: 'loading', role: 'status' },
    h('span', { class: 'sr-only' }, 'Loading'),
    h('div', { class: 'skeleton skeleton-eyebrow' }),
    h('div', { class: 'skeleton skeleton-title' }),
    h('div', { class: 'skeleton skeleton-deck' }),
    h('div', { class: 'skeleton-rule' }),
    Array.from({ length: lines }, (_, i) => h('div', { class: ['skeleton', 'skeleton-line', { 'is-short': i % 3 === 2 }] })));
}
/** A small loading stand-in for one part of a page. */
export const loadingBlock = (lines = 3) => h('div', { class: 'loading loading-block', role: 'status' },
  h('span', { class: 'sr-only' }, 'Loading'), Array.from({ length: lines }, (_, i) => h('div', { class: ['skeleton', 'skeleton-line', { 'is-short': i % 3 === 2 }] })));

/**
 * Nothing to show, and why: empty({ title: 'No drives in this period.', text: 'Try a longer period.', action }).
 * Say which filter removed everything, or what to do next; never a bare "No data".
 */
export function empty({ title, text = null, action = null, cover = null, cls = null } = {}) {
  return h('div', { class: ['empty', cls], cover },
    h('p', { class: 'empty-title' }, title),
    text ? h('p', { class: 'empty-text' }, text) : null,
    action ? h('p', { class: 'empty-action' }, action) : null);
}

/** Words for an API error: what happened and what to do. */
export function errorWords(err) {
  const s = err && err.status;
  const code = err && err.code;
  if (s === 404) return { title: 'There is nothing here.', text: code === 'no_such_car' ? 'There is no car with this id, or your account may not see it.' : 'It does not exist, or it was removed.' };
  if (s === 401) return { title: 'You are signed out.', text: 'Sign in again to see this page.' };
  if (s === 403) return { title: 'Your account may not do this.', text: 'Ask an admin if you need it.' };
  if (isPageFailed(err)) return { title: `${PAGE_FAILED.title}.`, text: PAGE_FAILED.text };
  if (s === 0) return { title: 'The server does not answer.', text: 'Check the connection; the page tries again when you reload it.' };
  if (s === 429) return { title: 'Too many requests.', text: 'Wait a moment and try again.' };
  return { title: 'This page could not be loaded.', text: (err && err.message) || String(err), generic: true };
}

/**
 * The error state of a page or a part: errorView(err, { retry }). For a part of a page, pass its own
 * wording as title (used when the error has no more specific words, e.g. 'The odometer could not be
 * loaded.') and home: false to leave out the 'Go to Fleet' link.
 */
export function errorView(err, { retry = null, cls = null, eyebrow = 'Could not load', title = null, home = true } = {}) {
  const w = errorWords(err);
  return h('section', { class: ['empty', 'empty-error', cls], role: 'alert' },
    h('p', { class: 'eyebrow' }, eyebrow),
    h('p', { class: 'empty-title' }, title && w.generic ? title : w.title),
    h('p', { class: 'empty-text' }, w.text),
    h('p', { class: 'empty-action' },
      retry ? button(isPageFailed(err) ? PAGE_FAILED.action : 'Try again', { onclick: retry, iconName: 'reset' }) : null,
      home ? [' ', h('a', { class: 'link-arrow', href: '/' }, 'Go to Fleet', icon('arrowRight'))] : null));
}

/* ------------------------------------------------------------------ small drawings */

/** A thin battery bar (SVG with percentage widths, so it needs no inline styles). */
export function batteryBar(soc, { from = null, limit = null, cls = null } = {}) {
  if (soc == null) return h('span', { class: ['bar', 'bar-missing', cls], 'aria-hidden': 'true' });
  const pct = Math.max(0, Math.min(100, soc));
  return svg('svg', { class: ['bar', cls], width: '100%', height: '100%', 'aria-hidden': 'true', focusable: 'false', preserveAspectRatio: 'none' },
    svg('rect', { class: 'bar-track', x: 0, y: 0, width: '100%', height: '100%' }),
    from != null
      ? [svg('rect', { class: 'bar-fill', x: 0, y: 0, width: `${Math.min(from, pct)}%`, height: '100%' }),
        svg('rect', { class: 'bar-added', x: `${Math.min(from, pct)}%`, y: 0, width: `${Math.max(0, pct - from)}%`, height: '100%' })]
      : svg('rect', { class: ['bar-fill', { 'is-low': pct < 20 }], x: 0, y: 0, width: `${pct}%`, height: '100%' }),
    limit != null ? svg('rect', { class: 'bar-limit', x: `${Math.max(0, Math.min(99.4, limit))}%`, y: 0, width: '0.6%', height: '100%' }) : null);
}

/**
 * A route's shape as a small drawing (no map): routeSketch([[lon, lat], ...]). The start is a ring, the
 * end a dot. Returns null for fewer than two points.
 */
export function routeSketch(coords, { cls = 'sketch', label = null } = {}) {
  const pts = (coords || []).filter((c) => c && Number.isFinite(c[0]) && Number.isFinite(c[1]));
  if (pts.length < 2) return null;
  let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity;
  for (const [x, y] of pts) { w = Math.min(w, x); e = Math.max(e, x); s = Math.min(s, y); n = Math.max(n, y); }
  const k = Math.cos(((s + n) / 2) * Math.PI / 180);
  const span = Math.max((e - w) * k, n - s, 1e-6);
  const P = 4, S = 32;
  const xy = ([x, y]) => [P + (((x - w) * k) / span) * S + (S - ((e - w) * k / span) * S) / 2, P + ((n - y) / span) * S + (S - ((n - s) / span) * S) / 2];
  const step = Math.max(1, Math.floor(pts.length / 80));
  const line = pts.filter((_, i) => i % step === 0 || i === pts.length - 1).map((p) => xy(p).map((v) => v.toFixed(1)).join(',')).join(' ');
  const [x0, y0] = xy(pts[0]); const [x1, y1] = xy(pts[pts.length - 1]);
  return svg('svg', { class: cls, viewBox: '0 0 40 40', role: label ? 'img' : null, 'aria-label': label, 'aria-hidden': label ? null : 'true', focusable: 'false' },
    svg('polyline', { class: 'sketch-line', points: line }),
    svg('circle', { class: 'sketch-start', cx: x0.toFixed(1), cy: y0.toFixed(1), r: 1.9 }),
    svg('circle', { class: 'sketch-end', cx: x1.toFixed(1), cy: y1.toFixed(1), r: 1.9 }));
}

/* ------------------------------------------------------------------ placeholder pages */

/**
 * The part of a page that is not built yet: its sections from the page plan, each with what it will
 * show. Used by pages that are placeholders, and by built pages for the sections still to come.
 */
export function plannedSections(list, { title = 'Still to come on this page', noteText = null } = {}) {
  return h('section', { class: 'planned', 'aria-labelledby': 'planned-title' },
    h('div', { class: 'planned-note' },
      h('h2', { class: 'label', id: 'planned-title' }, title),
      noteText ? h('p', null, noteText) : null),
    h('ol', { class: 'planned-list' },
      list.map(([name, what], i) => h('li', { class: 'planned-item' },
        h('span', { class: 'planned-no', 'aria-hidden': 'true' }, String(i + 1).padStart(2, '0')),
        h('div', null, h('h3', { class: 'planned-name' }, name), h('p', { class: 'planned-what' }, what))))));
}

/** Mark an element as covering inventory items (data-cover). Only for parts that really work. */
export const cover = markCover;

/** A number with its unit as text: fmt(12.3, 'distance') -> '7.6 mi'. */
export const fmt = format;
