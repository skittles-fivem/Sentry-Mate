// Sentry Mate, "Logbook" UI pack.
//
// All behaviour lives in this file. Data comes only from mock-api.js, whose
// functions match the real Sentry Mate JSON API, so switching to live data is
// a change to that one file. Pages are drawn with DOM calls (no innerHTML),
// which keeps car names and place names from the API safe to show, and keeps
// the page working under a strict Content-Security-Policy.

import * as api from './mock-api.js';

const TZ = 'America/Chicago';
const root = document.documentElement;
const mainEl = document.getElementById('main');

/* =========================================================== preferences == */

const store = {
  get(key, fallback) {
    try {
      const v = localStorage.getItem('logbook:' + key);
      return v == null ? fallback : JSON.parse(v);
    } catch { return fallback; }
  },
  set(key, value) {
    try { localStorage.setItem('logbook:' + key, JSON.stringify(value)); } catch { /* storage can be blocked */ }
  },
};

const prefs = {
  units: store.get('units', 'us') === 'metric' ? 'metric' : 'us',
  theme: ['light', 'dark'].includes(store.get('theme', 'system')) ? store.get('theme', 'system') : 'system',
  density: ['comfortable', 'compact'].includes(store.get('density', '')) ? store.get('density', '') : null,
  recent: (Array.isArray(store.get('recent', [])) ? store.get('recent', []) : []).filter((x) => typeof x === 'string').slice(0, 8),
  lastCar: typeof store.get('lastCar', 'model-y') === 'string' ? store.get('lastCar', 'model-y') : 'model-y',
  // The server keeps this one (api.getPrefs); the copy here is what boot.js applied before the first paint.
  uiSize: store.get('uiSize', 'auto'),
};

function rememberCar(id) {
  prefs.lastCar = id;
  prefs.recent = [id, ...prefs.recent.filter((x) => x !== id)].slice(0, 8);
  store.set('lastCar', id);
  store.set('recent', prefs.recent);
}

/* ================================================================== DOM == */

function append(el, kids) {
  for (const k of kids.flat(Infinity)) {
    if (k == null || k === false || k === '') continue;
    el.append(k instanceof Node ? k : document.createTextNode(String(k)));
  }
  return el;
}

/** Build an element: h('a', { class: 'x', href: '#/' }, 'text', child). */
function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k === 'dataset') Object.assign(el.dataset, v);
      else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
      else if (v === true) el.setAttribute(k, '');
      else el.setAttribute(k, String(v));
    }
  }
  return append(el, kids);
}

const SVG_NS = 'http://www.w3.org/2000/svg';
function s(tag, attrs, ...kids) {
  const el = document.createElementNS(SVG_NS, tag);
  if (attrs) for (const [k, v] of Object.entries(attrs)) if (v != null && v !== false) el.setAttribute(k, String(v));
  return append(el, kids);
}

const sep = () => h('span', { class: 'sep', 'aria-hidden': 'true' }, ' · ');

/* ============================================================== display == */

/**
 * How big things are drawn. The size is set by one CSS rule (html font-size
 * in app.css: Auto, or the person's Interface size), and HTML and SVG follow
 * it by themselves. Canvases and maps are drawn in device pixels, so they
 * read the current rem and pixel ratio here and are told when either
 * changes: another Interface size, a window that grows past 2400 px, browser
 * zoom, or a move to a screen with another pixel density.
 *
 * Two standard signals, no polling: a 1rem square watched by a
 * ResizeObserver in device pixels (it changes size whenever the root size
 * or the pixel ratio does), and a resolution media query for browsers that
 * cannot observe device pixels.
 */
const display = (() => {
  const listeners = new Set();
  const read = () => ({ rem: parseFloat(getComputedStyle(root).fontSize) || 16, dpr: window.devicePixelRatio || 1 });
  let cur = read();
  function check() {
    const next = read();
    if (next.rem === cur.rem && next.dpr === cur.dpr) return;
    const change = { rem: next.rem !== cur.rem, dpr: next.dpr !== cur.dpr };
    cur = next;
    if (change.dpr) watchRatio();
    for (const fn of [...listeners]) { try { fn(change); } catch (err) { console.error(err); } }
  }
  const probe = h('div', { class: 'rem-probe', 'aria-hidden': 'true' });
  // The browser's own default size (font-size: medium), which "100%" means.
  const standardProbe = h('div', { class: 'standard-probe', 'aria-hidden': 'true' });
  document.body.append(probe, standardProbe);
  const ro = new ResizeObserver(check);
  try { ro.observe(probe, { box: 'device-pixel-content-box' }); } catch { ro.observe(probe); }
  let mq = null;
  function watchRatio() {
    if (mq) mq.removeEventListener('change', check);
    mq = matchMedia(`(resolution: ${cur.dpr}dppx)`);
    mq.addEventListener('change', check);
  }
  watchRatio();
  return {
    /** The root font size in CSS px. */
    get rem() { return cur.rem; },
    get dpr() { return cur.dpr; },
    /** The standard size in CSS px: the browser's default font size, 16 unless the person changed it. */
    get standard() { return parseFloat(getComputedStyle(standardProbe).fontSize) || 16; },
    /** For drawings designed at the standard 16px root: multiply their px by this. */
    get scale() { return cur.rem / 16; },
    /** fn({ rem, dpr }) is called with what changed. Returns a function that stops it. */
    onChange(fn) { listeners.add(fn); return () => listeners.delete(fn); },
    /** Look now rather than at the next frame, and tell the listeners if anything changed. */
    refresh: check,
  };
})();

/**
 * Keep the reader's place when the size changes. Whatever is at the middle of
 * the window (a row of the register, an entry of the day) stays at the same
 * height on screen, so going from 100% to 200% leaves row 100,000 in view
 * instead of somewhere far below. The browser's own scroll anchoring cannot
 * be relied on when every size changes at once: it may stand down, or hold
 * on to some other node.
 */
const readingPlace = (() => {
  let anchor = null;
  let frame = 0;
  function remember() {
    frame = 0;
    // A scroll caused by a new size (the browser shifting things about) must not move the anchor
    // before it has been used: deal with the size change first.
    display.refresh();
    anchor = null;
    if (window.scrollY < 2) return; // at the top: stay at the top
    const hit = document.elementFromPoint(innerWidth / 2, innerHeight / 2);
    const el = hit && hit.closest('tr, li, article, section, header, figure, p, dl, h1, h2');
    // Where its middle sits, as a share of the window's height, so a taller or shorter window keeps it at the same place.
    if (el && mainEl.contains(el)) { const b = el.getBoundingClientRect(); anchor = { el, at: (b.top + b.height / 2) / innerHeight }; }
  }
  addEventListener('scroll', () => { if (!frame) frame = requestAnimationFrame(remember); }, { passive: true });
  function restore() {
    if (!anchor || !anchor.el.isConnected) return;
    const box = anchor.el.getBoundingClientRect();
    // Its middle at the same share of the new height, kept on screen whole if it fits.
    const want = Math.max(0, Math.min(anchor.at * innerHeight - box.height / 2, innerHeight - Math.min(box.height, innerHeight)));
    window.scrollBy(0, box.top - want);
    remember();
  }
  display.onChange((change) => { if (change.rem) restore(); });
  // A window resized without a change of root size still reflows the page.
  let lastH = innerHeight, lastW = innerWidth;
  addEventListener('resize', () => {
    if (innerHeight === lastH && innerWidth === lastW) return;
    lastH = innerHeight; lastW = innerWidth;
    display.refresh(); // a new root size, if any, restores through the listener above
    restore();
  });
  return { remember };
})();

/* ======================================================= keyboard groups == */

/**
 * One tab stop for a group of items (a radio group, the bars of a chart):
 * Tab enters and leaves the group, arrow keys move between its items, Home
 * and End go to the first and last. `start()` names the item that holds the
 * tab stop when the group is entered (by default the first). With `onMove`,
 * moving also does something (a radio is picked as you move to it); with
 * `activate`, Enter and Space do it. Returns sync(), to call after the
 * starting item changes in code.
 */
function rovingKeys(group, selector, { start = null, onMove = null, activate = null } = {}) {
  if (!group) return () => {};
  const items = () => [...group.querySelectorAll(selector)];
  const sync = (to = null) => {
    const all = items();
    const on = to || (start && start(all)) || all[0];
    for (const el of all) el.tabIndex = el === on ? 0 : -1;
  };
  group.addEventListener('keydown', (e) => {
    const all = items();
    const i = all.indexOf(document.activeElement);
    if (i < 0) return;
    if (activate && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); activate(all[i]); return; }
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    let j = null;
    if (step) j = (i + step + all.length) % all.length;
    else if (e.key === 'Home') j = 0;
    else if (e.key === 'End') j = all.length - 1;
    if (j == null) return;
    e.preventDefault();
    sync(all[j]);
    all[j].focus();
    if (onMove) onMove(all[j]);
  });
  // Whatever was last focused keeps the tab stop.
  group.addEventListener('focusin', (e) => { const el = e.target.closest(selector); if (el && group.contains(el)) sync(el); });
  sync();
  return () => sync();
}

/** Keyboard for a group of role="radio" buttons, as people expect from radios: moving to one picks it. */
function radioKeys(group) {
  const sync = rovingKeys(group, '[role="radio"]', {
    start: (all) => all.find((b) => b.getAttribute('aria-checked') === 'true'),
    onMove: (b) => b.click(),
  });
  group.addEventListener('click', () => requestAnimationFrame(sync));
  return sync;
}

/* ======================================================= interface size == */

// Auto, or a percentage of the standard size (100% is the standard size on
// every screen; only Auto grows with the window). CSS does the sizing, from
// data-ui-size on <html>; this keeps the attribute, the controls and the
// server in step.
const UI_SIZES = ['auto', 75, 90, 100, 110, 125, 150, 175, 200];
if (!UI_SIZES.includes(prefs.uiSize)) prefs.uiSize = 'auto';
const sizeLabel = (v) => (v === 'auto' ? 'Auto' : `${v}%`);
const sizeSyncs = new Set();
let sizePicked = false;

function applyUiSize(v) {
  prefs.uiSize = UI_SIZES.includes(v) ? v : 'auto';
  if (prefs.uiSize === 'auto') delete root.dataset.uiSize; else root.dataset.uiSize = String(prefs.uiSize);
  store.set('uiSize', prefs.uiSize); // the copy boot.js applies before the next first paint
  for (const sync of sizeSyncs) sync();
}

/** The person picked a size: it applies at once, live, and is saved with their account. */
function setUiSize(v) {
  sizePicked = true;
  if (v === prefs.uiSize) return;
  applyUiSize(v);
  api.setPrefs({ uiSize: prefs.uiSize }).catch(() => { /* kept in this browser until the next try */ });
}

/** Fill a radio group with the sizes; it stays in step until the returned function is called. */
function sizeRadios(group) {
  const buttons = UI_SIZES.map((v) => h('button', {
    type: 'button', role: 'radio', class: 'segmented-option', dataset: { size: String(v) }, onclick: () => setUiSize(v),
  }, sizeLabel(v)));
  group.replaceChildren(...buttons);
  const keys = radioKeys(group);
  const sync = () => { buttons.forEach((b, i) => b.setAttribute('aria-checked', String(UI_SIZES[i] === prefs.uiSize))); keys(); };
  sync();
  sizeSyncs.add(sync);
  return () => sizeSyncs.delete(sync);
}

/** "Text on this screen: 145% of standard (Auto)." The standard size is the browser's own, 16px unless changed. */
function sizeNote() {
  const pct = Math.round((display.rem / display.standard) * 100);
  return `Text on this screen: ${pct}% of standard${prefs.uiSize === 'auto' ? ' (Auto)' : ''}.`;
}

/** "Aa" in the top bar: the sizes one click away. Settings has the same control. */
function setupSizeMenu() {
  const menu = document.getElementById('size-menu');
  const trigger = document.getElementById('size-trigger');
  const pop = document.getElementById('size-pop');
  const value = document.getElementById('size-trigger-value');
  const note = document.getElementById('size-pop-note');
  sizeRadios(document.getElementById('size-options'));
  const sync = () => {
    value.textContent = prefs.uiSize === 'auto' ? '' : sizeLabel(prefs.uiSize);
    trigger.title = `Interface size: ${sizeLabel(prefs.uiSize)}`;
    note.textContent = sizeNote();
  };
  sync();
  sizeSyncs.add(sync);
  display.onChange(sync);
  const show = (on) => {
    pop.hidden = !on;
    trigger.setAttribute('aria-expanded', String(on));
    if (on) { const b = pop.querySelector('[aria-checked="true"]'); if (b) b.focus({ preventScroll: true }); }
  };
  trigger.addEventListener('click', () => show(pop.hidden));
  document.addEventListener('pointerdown', (e) => { if (!pop.hidden && !menu.contains(e.target)) show(false); });
  // Focus goes back to "Aa" without scrolling: a bigger size can make the masthead scroll away
  // (narrow windows, app.css), and the reader's place matters more than showing the button.
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && !pop.hidden) { show(false); trigger.focus({ preventScroll: true }); } });
  // Tabbing out of it closes it, so it never covers what has the focus.
  menu.addEventListener('focusout', (e) => { if (!pop.hidden && e.relatedTarget && !menu.contains(e.relatedTarget)) show(false); });
  pop.querySelector('.size-pop-more').addEventListener('click', () => show(false));
  window.addEventListener('hashchange', () => show(false));
}

function icon(name, cls = 'icon') {
  const paths = {
    search: [s('circle', { cx: 8.5, cy: 8.5, r: 5.5 }), s('path', { d: 'm13 13 4 4' })],
    chevronRight: [s('path', { d: 'm8 5 5 5-5 5' })],
    chevronLeft: [s('path', { d: 'm12 5-5 5 5 5' })],
    chevronsLeft: [s('path', { d: 'm10 5-5 5 5 5M15 5l-5 5 5 5' })],
    chevronsRight: [s('path', { d: 'm5 5 5 5-5 5M10 5l5 5-5 5' })],
    arrowRight: [s('path', { d: 'M4 10h12M11 5l5 5-5 5' })],
    close: [s('path', { d: 'm5 5 10 10M15 5 5 15' })],
    plug: [s('path', { d: 'M7 3v4M13 3v4M5 7h10v3a5 5 0 0 1-10 0V7ZM10 15v3' })],
    reset: [s('path', { d: 'M4 10a6 6 0 1 0 2-4.5M4 3v3.5h3.5' })],
  }[name] || [];
  return s('svg', { class: cls, viewBox: '0 0 20 20', 'aria-hidden': 'true', focusable: 'false' }, paths);
}

/* =========================================================== formatting == */

const fmt = {
  time: new Intl.DateTimeFormat('en-US', { timeZone: TZ, hour: 'numeric', minute: '2-digit' }),
  timeSec: new Intl.DateTimeFormat('en-US', { timeZone: TZ, hour: 'numeric', minute: '2-digit', second: '2-digit' }),
  dayLong: new Intl.DateTimeFormat('en-US', { timeZone: TZ, weekday: 'long', month: 'long', day: 'numeric' }),
  dayLongYear: new Intl.DateTimeFormat('en-US', { timeZone: TZ, weekday: 'long', month: 'long', day: 'numeric', year: 'numeric' }),
  dayShort: new Intl.DateTimeFormat('en-US', { timeZone: TZ, weekday: 'short', month: 'short', day: 'numeric' }),
  monthDay: new Intl.DateTimeFormat('en-US', { timeZone: TZ, month: 'short', day: 'numeric' }),
  month: new Intl.DateTimeFormat('en-US', { timeZone: TZ, month: 'long', year: 'numeric' }),
  clock: new Intl.DateTimeFormat('en-US', { timeZone: TZ, weekday: 'short', month: 'short', day: 'numeric' }),
  key: new Intl.DateTimeFormat('en-CA', { timeZone: TZ, year: 'numeric', month: '2-digit', day: '2-digit' }),
  hour: new Intl.DateTimeFormat('en-US', { timeZone: TZ, hour: 'numeric' }),
};

const nfCache = new Map();
function n(v, digits = 0) {
  if (v == null || !Number.isFinite(v)) return '—';
  let f = nfCache.get(digits);
  if (!f) { f = new Intl.NumberFormat('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits }); nfCache.set(digits, f); }
  const out = f.format(Math.abs(v) < 0.5 * 10 ** -digits ? 0 : v);
  return out.startsWith('-') ? '−' + out.slice(1) : out;
}

const dayKeyOf = (ms) => fmt.key.format(ms);
const keyToMs = (key) => Date.parse(key + 'T18:00:00Z');
function addDays(key, k) {
  const [y, m, d] = key.split('-').map(Number);
  return new Date(Date.UTC(y, m - 1, d + k)).toISOString().slice(0, 10);
}

function timeParts(ms) {
  const parts = fmt.time.formatToParts(ms);
  const ap = (parts.find((p) => p.type === 'dayPeriod') || {}).value || '';
  const hm = parts.filter((p) => p.type === 'hour' || p.type === 'minute' || p.type === 'literal').map((p) => p.value).join('').replace(/[\s ]+$/, '');
  return { hm, ap };
}
const clockTime = (ms) => { const t = timeParts(ms); return `${t.hm} ${t.ap}`; };
function timeRange(a, b) {
  const ta = timeParts(a);
  if (b == null) return `${ta.hm} ${ta.ap} – now`;
  const tb = timeParts(b);
  return ta.ap === tb.ap ? `${ta.hm} – ${tb.hm} ${tb.ap}` : `${ta.hm} ${ta.ap} – ${tb.hm} ${tb.ap}`;
}

// Numbers and their units never part at a line break.
const NB = '\u00a0';

/** Whole minutes, as a reader would add them up from the durations shown. */
const minutesOf = (sec) => Math.max(0, Math.round(sec / 60));
function dur(sec) {
  const t = Math.max(0, Math.round(sec));
  if (t < 60) return `<${NB}1${NB}min`;
  const m = minutesOf(t);
  if (m < 60) return `${m}${NB}min`;
  const hh = Math.floor(m / 60), mm = m % 60;
  if (hh < 24) return mm ? `${hh}${NB}h ${mm}${NB}min` : `${hh}${NB}h`;
  const d = Math.floor(hh / 24), rh = hh % 24;
  return rh ? `${d}${NB}d ${rh}${NB}h` : `${d}${NB}d`;
}
function durShort(sec) {
  const m = minutesOf(sec);
  if (m < 60) return `${m}${NB}min`;
  return `${Math.floor(m / 60)}${NB}h ${String(m % 60).padStart(2, '0')}`;
}

/**
 * How long ago, always relative. Under an hour it counts clock minutes, so
 * "2 min ago" agrees with the times printed beside it (7:22 PM at 7:24 PM).
 */
function ago(ms, nowMs = clock.now()) {
  const sec = Math.max(0, (nowMs - ms) / 1000);
  const mins = Math.max(0, Math.floor(nowMs / 60000) - Math.floor(ms / 60000));
  if (mins === 0) return 'just now';
  if (mins < 60) return `${mins}${NB}min ago`;
  if (sec < 86400) {
    const hh = Math.floor(sec / 3600), mm = Math.floor((sec % 3600) / 60);
    return hh < 3 && mm ? `${hh}${NB}h ${mm}${NB}min ago` : `${Math.floor(sec / 3600)}${NB}h ago`;
  }
  const d = Math.floor(sec / 86400);
  return `${d}${NB}day${d === 1 ? '' : 's'} ago`;
}
/** "7:22 PM" today, "Yesterday, 7:22 PM", or "Oct 22, 8:07 AM". */
function whenShort(ms, nowMs = clock.now()) {
  const k = dayKeyOf(ms), today = dayKeyOf(nowMs);
  if (k === today) return clockTime(ms);
  if (k === addDays(today, -1)) return `Yesterday, ${clockTime(ms)}`;
  return `${fmt.monthDay.format(ms)}, ${clockTime(ms)}`;
}

function dayName(key, todayKey) {
  if (key === todayKey) return 'Today';
  if (key === addDays(todayKey, -1)) return 'Yesterday';
  return fmt.dayLong.format(keyToMs(key));
}

/* ================================================================ units == */

const UNITS = {
  us: { dist: 'mi', distK: 1 / 1.609344, speed: 'mph', temp: '°F', press: 'psi', eff: 'Wh/mi', effK: 1.609344, elev: 'ft', elevK: 3.28084 },
  metric: { dist: 'km', distK: 1, speed: 'km/h', temp: '°C', press: 'bar', eff: 'Wh/km', effK: 1, elev: 'm', elevK: 1 },
};
const U = () => UNITS[prefs.units];
const conv = {
  dist: (km) => km * U().distK,
  speed: (kph) => kph * U().distK,
  temp: (c) => (prefs.units === 'us' ? (c * 9) / 5 + 32 : c),
  press: (bar) => (prefs.units === 'us' ? bar * 14.5038 : bar),
  eff: (whkm) => whkm * U().effK,
  elev: (m) => m * U().elevK,
};
const distDigits = (v) => (Math.abs(v) < 100 ? 1 : 0);

/** A number with its unit, set so the unit reads quieter than the figure. */
function q(value, unit, cls) {
  return h('span', { class: 'q' + (cls ? ' ' + cls : '') }, h('span', { class: 'q-v' }, value), unit ? h('span', { class: 'q-u' }, unit) : null);
}
const Q = {
  dist: (km, d) => { const v = conv.dist(km); return q(n(v, d ?? distDigits(v)), U().dist); },
  range: (km) => q(n(conv.dist(km)), U().dist),
  speed: (kph) => q(n(conv.speed(kph)), U().speed),
  temp: (c) => q(n(conv.temp(c)), U().temp),
  press: (bar) => q(n(conv.press(bar), prefs.units === 'us' ? 0 : 1), U().press),
  eff: (whkm) => (whkm == null ? q('—') : q(n(conv.eff(whkm)), U().eff)),
  kwh: (k, d = 1) => q(n(k, d), 'kWh'),
  kw: (k, d = 1) => q(n(k, d), 'kW'),
  pct: (p, d = 0) => q(n(p, d), '%', 'q-tight'),
};
/** "+15.2 kWh" with a lighter plus sign. */
const plus = (node) => h('span', { class: 'plus' }, h('span', { class: 'plus-sign', 'aria-hidden': 'true' }, '+'), h('span', { class: 'sr-only' }, 'plus '), node);
/** The same, as plain text for running sentences and meta lines. */
const T = {
  dist: (km, d) => { const v = conv.dist(km); return `${n(v, d ?? distDigits(v))} ${U().dist}`; },
  range: (km) => `${n(conv.dist(km))} ${U().dist}`,
  speed: (kph) => `${n(conv.speed(kph))} ${U().speed}`,
  temp: (c) => `${n(conv.temp(c))}${U().temp}`,
  eff: (whkm) => (whkm == null ? '—' : `${n(conv.eff(whkm))} ${U().eff}`),
  kwh: (k, d = 1) => `${n(k, d)} kWh`,
  elev: (m) => `${n(conv.elev(m))} ${U().elev}`,
  kw: (k, d = 1) => `${n(k, d)} kW`,
};
/**
 * Sums that match their parts as shown: each part is rounded the way it is
 * printed before it is added, so a day's total is the sum of its rows.
 */
const shown = {
  dist: (kms) => kms.reduce((a, km) => { const v = conv.dist(km); return a + +v.toFixed(distDigits(v)); }, 0) / U().distK,
  kwh: (vals, d = 2) => vals.reduce((a, v) => a + +v.toFixed(d), 0),
  minutes: (secs) => secs.reduce((a, x) => a + minutesOf(x), 0) * 60,
};

/* ================================================================ clock == */

const clock = {
  skew: 0,
  now() { return Date.now() + this.skew; },
  set(serverNow) { this.skew = serverNow - Date.now(); },
  todayKey() { return dayKeyOf(this.now()); },
};

function tick() {
  const t = clock.now();
  const el = document.getElementById('clock');
  if (el) {
    el.textContent = `${fmt.clock.format(t)}, ${clockTime(t)}`;
    el.setAttribute('datetime', new Date(t).toISOString());
  }
  for (const a of document.querySelectorAll('[data-ago]')) {
    const txt = ago(+a.dataset.ago, t);
    if (a.textContent !== txt) a.textContent = txt;
  }
}

/* =========================================================== car states == */

const STATE_LABEL = { driving: 'Driving', charging: 'Charging', parked: 'Parked', asleep: 'Asleep', unknown: 'Offline', offline: 'Offline' };

/** One label for a car's state, used everywhere: "Parked", "Charging · stale", "Offline". */
function stateLabelOf(state, stale) {
  if (state === 'unknown' || state === 'offline') return 'Offline';
  return stale ? `${STATE_LABEL[state]} · stale` : STATE_LABEL[state] || state;
}
const dotClassOf = (state, stale) => (state === 'unknown' || state === 'offline' ? 'state-offline' : `state-${state}${stale ? ' is-stale' : ''}`);

function stateMark(state, extra) {
  return h('span', { class: `state state-${state}` }, h('span', { class: 'state-dot', 'aria-hidden': 'true' }), STATE_LABEL[state] || state, extra);
}

/* =============================================================== router == */

const routes = [
  [/^\/fleet$/, 'fleet', fleetPage],
  [/^\/cars\/([^/]+)\/now$/, 'now', nowPage],
  [/^\/cars\/([^/]+)\/drives$/, 'drives', drivesPage],
  [/^\/cars\/([^/]+)\/drives\/([^/]+)$/, 'drives', drivePage],
  [/^\/cars\/([^/]+)\/timeline$/, 'timeline', timelinePage],
  [/^\/cars\/([^/]+)\/(charges|battery|stats|signals|logs)$/, null, placeholderPage],
  [/^\/settings$/, 'settings', settingsPage],
];

let view = { token: 0, cleanups: [] };
let firstRoute = true;
// A control that redraws the page (units, theme, density) gets the focus back afterwards.
let refocusAfterRender = null;

function parseHash() {
  const raw = location.hash.replace(/^#/, '') || '/fleet';
  const [path, qs] = raw.split('?');
  return { path: path || '/fleet', query: new URLSearchParams(qs || '') };
}

function setQuery(params, { replace = true } = {}) {
  const { path } = parseHash();
  const qs = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v != null && v !== '') qs.set(k, v);
  const str = qs.toString();
  const url = '#' + path + (str ? '?' + str : '');
  if (replace) history.replaceState(null, '', url); else history.pushState(null, '', url);
}

async function render() {
  for (const fn of view.cleanups.splice(0)) { try { fn(); } catch { /* ignore */ } }
  const token = ++view.token;
  root.dataset.ready = '0';
  const { path, query } = parseHash();
  let match = null;
  for (const [re, section, page] of routes) {
    const m = path.match(re);
    if (m) { match = { section: section || m[2], page, params: m.slice(1).map(decodeURIComponent) }; break; }
  }
  if (!match) { location.replace('#/fleet'); return; }
  const carId = path.startsWith('/cars/') ? match.params[0] : null;
  const waits = [];
  const ctx = {
    section: match.section, params: match.params, query, carId, main: mainEl,
    alive: () => token === view.token,
    onCleanup: (fn) => view.cleanups.push(fn),
    waitFor: (p) => waits.push(p),
  };
  updateChrome(ctx);
  mainEl.setAttribute('aria-busy', 'true');
  mainEl.replaceChildren(loadingView(match.section));
  if (!firstRoute) { window.scrollTo(0, 0); }
  try {
    await match.page(ctx);
  } catch (err) {
    if (ctx.alive()) mainEl.replaceChildren(errorView(err));
  }
  if (!ctx.alive()) return;
  mainEl.removeAttribute('aria-busy');
  const again = refocusAfterRender && document.querySelector(refocusAfterRender);
  refocusAfterRender = null;
  if (again) again.focus({ preventScroll: true });
  else if (!firstRoute) mainEl.focus({ preventScroll: true });
  firstRoute = false;
  // Pages register what has to finish drawing (maps, charts); some register more while they wait.
  while (waits.length) await Promise.allSettled(waits.splice(0));
  await document.fonts.ready;
  if (ctx.alive()) root.dataset.ready = '1';
}

function loadingView(section) {
  const lines = section === 'fleet' ? 9 : 6;
  return h('div', { class: 'loading', role: 'status' },
    h('span', { class: 'sr-only' }, 'Loading'),
    h('div', { class: 'skeleton skeleton-eyebrow' }),
    h('div', { class: 'skeleton skeleton-title' }),
    h('div', { class: 'skeleton skeleton-deck' }),
    h('div', { class: 'skeleton-rule' }),
    Array.from({ length: lines }, (_, i) => h('div', { class: 'skeleton skeleton-line' + (i % 3 === 2 ? ' is-short' : '') })));
}

function errorView(err) {
  const notFound = err && err.status === 404;
  return h('section', { class: 'empty empty-page' },
    h('p', { class: 'eyebrow' }, notFound ? 'Not found' : 'Something went wrong'),
    h('h1', { class: 'page-title' }, notFound ? 'There is nothing here.' : 'This page could not be loaded.'),
    h('p', { class: 'deck' }, notFound ? 'The car or drive you asked for does not exist, or it was removed.' : String((err && err.message) || err)),
    h('p', null, h('a', { class: 'button', href: '#/fleet' }, 'Back to all cars')));
}

/* ============================================================== chrome === */

const carCache = new Map();
async function carInfo(id) {
  if (!carCache.has(id)) carCache.set(id, api.getCar(id).catch(() => null));
  return carCache.get(id);
}

function setCarLinks(carId, section) {
  for (const a of document.querySelectorAll('.nav-link')) {
    const key = a.dataset.nav;
    if (a.hasAttribute('data-car-link')) a.setAttribute('href', `#/cars/${encodeURIComponent(carId)}/${key}`);
    if (key === section) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
}

function updateChrome(ctx) {
  const nav = document.querySelector('.primary-nav');
  const navCar = document.getElementById('nav-car');
  const nameEl = document.getElementById('switcher-trigger-name');
  const metaEl = document.getElementById('switcher-trigger-meta');
  // Car tabs open the car on screen, or on other pages the last car opened.
  setCarLinks(ctx.carId || prefs.lastCar, ctx.section);
  nav.classList.toggle('is-fleet', !ctx.carId);
  if (!ctx.carId) {
    nameEl.textContent = 'Choose a car';
    nameEl.removeAttribute('title');
    metaEl.replaceChildren('Search the whole fleet');
    metaEl.removeAttribute('title');
    document.title = `${titleOf(ctx.section)} · Sentry Mate`;
    // Say which car the car tabs open.
    carInfo(prefs.lastCar).then((car) => {
      if (!ctx.alive()) return;
      navCar.textContent = car ? `${car.name}:` : '';
      for (const a of document.querySelectorAll('.nav-link[data-car-link]')) {
        if (car) a.setAttribute('title', `Opens ${car.name}`); else a.removeAttribute('title');
      }
    });
    return;
  }
  for (const a of document.querySelectorAll('.nav-link[data-car-link]')) a.removeAttribute('title');
  carInfo(ctx.carId).then((car) => {
    if (!ctx.alive()) return;
    if (!car) {
      // No such car: nothing to remember, and the car tabs keep pointing at the last good one.
      nameEl.textContent = 'Choose a car';
      metaEl.replaceChildren('No car with that key');
      setCarLinks(prefs.lastCar, null);
      document.title = 'Not found · Sentry Mate';
      return;
    }
    rememberCar(ctx.carId);
    nameEl.textContent = car.name;
    nameEl.title = car.name;
    metaEl.replaceChildren(...triggerMeta(car));
    metaEl.title = `${car.year} ${car.model} ${car.trim}`;
    document.title = `${car.name} · ${titleOf(ctx.section)} · Sentry Mate`;
  });
}

/** Under the car's name in the top bar: trim, state and how old the figures are. */
function triggerMeta(car) {
  const stale = car.stale || car.state === 'unknown';
  return [car.trim, ' · ', stateLabelOf(car.state, car.stale && car.state !== 'unknown'), ' · ',
    h('span', { class: 'trigger-age' + (stale ? ' is-stale' : ''), dataset: { ago: car.lastSeen } }, ago(car.lastSeen))];
}

function titleOf(section) {
  return { fleet: 'All cars', now: 'Now', drives: 'Drives', charges: 'Charges', timeline: 'Timeline', battery: 'Battery', stats: 'Stats', signals: 'Signals', logs: 'Logs', settings: 'Settings' }[section] || 'Sentry Mate';
}

function setupUnits() {
  const buttons = document.querySelectorAll('.units-option');
  const sync = () => buttons.forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.units === prefs.units)));
  buttons.forEach((b) => b.addEventListener('click', () => {
    if (prefs.units === b.dataset.units) return;
    prefs.units = b.dataset.units;
    store.set('units', prefs.units);
    sync();
    render();
  }));
  sync();
}

/* ======================================================== car switcher === */

const switcher = (() => {
  const dlg = document.getElementById('switcher');
  const input = document.getElementById('switcher-input');
  const list = document.getElementById('switcher-list');
  const count = document.getElementById('switcher-count');
  const trigger = document.getElementById('switcher-trigger');
  let options = [];
  let active = 0;
  let seq = 0;
  let timer = 0;

  function highlight(textValue, query) {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length) return textValue;
    const lower = textValue.toLowerCase();
    const marks = [];
    for (const w of words) { const i = lower.indexOf(w); if (i >= 0) marks.push([i, i + w.length]); }
    if (!marks.length) return textValue;
    marks.sort((a, b) => a[0] - b[0]);
    const out = [];
    let pos = 0;
    for (const [a, b] of marks) {
      if (a < pos) continue;
      out.push(textValue.slice(pos, a), h('mark', null, textValue.slice(a, b)));
      pos = b;
    }
    out.push(textValue.slice(pos));
    return out;
  }

  function optionEl(car, i, query, current) {
    const stale = car.stale && car.state !== 'unknown';
    return h('li', {
      class: 'switcher-option', role: 'option', id: 'switcher-opt-' + i, 'aria-selected': 'false', dataset: { id: car.id, index: i },
    },
    h('span', { class: `state-dot ${dotClassOf(car.state, stale)}`, 'aria-hidden': 'true' }),
    h('span', { class: 'switcher-option-main' },
      h('span', { class: 'switcher-option-name', title: car.name }, highlight(car.name, query), current ? h('span', { class: 'tag' }, 'Open now') : null),
      h('span', { class: 'switcher-option-meta', title: `${car.year} ${car.model} ${car.trim} · ${car.key}` }, `${car.year} ${car.model} ${car.trim}`, sep(), h('span', { class: 'mono' }, highlight(car.key, query)))),
    h('span', { class: 'switcher-option-side' },
      h('span', { class: 'switcher-option-state' + (car.stale ? ' is-stale' : '') }, stateLabelOf(car.state, stale), car.state !== 'unknown' ? ` · ${n(car.battery.soc)}%` : ''),
      h('span', { class: 'switcher-option-place', title: car.place.name }, car.stale ? `last at ${car.place.name}` : '', car.stale ? '' : highlight(car.place.name, query))));
  }

  function skeletonRows() {
    return Array.from({ length: 5 }, () => h('li', { class: 'switcher-skeleton', role: 'presentation', 'aria-hidden': 'true' },
      h('span', { class: 'skeleton skeleton-dot' }),
      h('span', { class: 'skeleton-lines' }, h('span', { class: 'skeleton skeleton-name' }), h('span', { class: 'skeleton skeleton-meta' })),
      h('span', { class: 'skeleton skeleton-side' })));
  }

  function setActive(i, scroll = true) {
    if (!options.length) return;
    active = (i + options.length) % options.length;
    options.forEach((o, k) => o.setAttribute('aria-selected', String(k === active)));
    input.setAttribute('aria-activedescendant', options[active].id);
    if (scroll) options[active].scrollIntoView({ block: 'nearest' });
  }

  async function fill() {
    const query = input.value.trim();
    const my = ++seq;
    list.setAttribute('aria-busy', 'true');
    // Placeholder rows while the answer is on its way (straight away when the list is empty).
    const slow = setTimeout(() => { if (my === seq) { list.replaceChildren(...skeletonRows()); options = []; } }, options.length ? 180 : 0);
    const openId = (parseHash().path.match(/^\/cars\/([^/]+)/) || [])[1];
    let groups, total, more = null;
    if (query) {
      const res = await api.listCars({ query, limit: 50, sort: 'name', rows: 'brief' });
      groups = [{ title: `Matches · ${n(res.total)}`, items: res.items }];
      total = res.total;
      if (res.total > res.items.length) more = `Showing ${res.items.length} of ${n(res.total)}. Keep typing to narrow the list.`;
    } else {
      // Without a query: the cars you were just looking at, then the ones worth a look now.
      const ids = prefs.recent.slice(0, 5);
      const [info, recent, driving, attention] = await Promise.all([
        api.serverInfo(),
        Promise.all(ids.map((id) => api.getCar(id).catch(() => null))),
        api.listCars({ state: 'driving', sort: 'lastSeen', limit: 10, rows: 'brief' }),
        api.listCars({ state: 'offline', sort: 'lastSeen', limit: 10, rows: 'brief' }),
      ]);
      if (recent.some((c) => !c)) { prefs.recent = prefs.recent.filter((id, k) => k >= ids.length || recent[k]); store.set('recent', prefs.recent); }
      const listed = new Set();
      const take = (arr) => { const out = []; for (const c of arr) { if (c && !listed.has(c.id) && out.length < 5) { listed.add(c.id); out.push(c); } } return out; };
      groups = [
        { title: 'Recent', items: take(recent) },
        { title: `Driving now · ${n(driving.total)}`, items: take(driving.items) },
        { title: `Needs attention: offline or stale · ${n(attention.total)}`, items: take(attention.items) },
      ];
      if (!groups.some((g) => g.items.length)) groups = [{ title: 'All cars', items: (await api.listCars({ limit: 10, rows: 'brief' })).items }];
      total = info.carCount;
    }
    clearTimeout(slow);
    if (my !== seq) return;
    list.removeAttribute('aria-busy');
    const kids = [];
    let i = 0;
    for (const g of groups) {
      if (!g.items.length) continue;
      kids.push(h('li', { class: 'switcher-group', role: 'presentation' }, g.title));
      for (const car of g.items) kids.push(optionEl(car, i++, query, car.id === openId));
    }
    if (more) kids.push(h('li', { class: 'switcher-more', role: 'presentation' }, more));
    if (!i) kids.push(h('li', { class: 'switcher-empty', role: 'presentation' }, h('strong', null, 'No car matches “', query, '”.'), h('span', null, 'Try part of a name, a key such as chi-014, a model or a place.')));
    list.replaceChildren(...kids);
    options = [...list.querySelectorAll('.switcher-option')];
    count.textContent = query ? `${n(total)} found` : `${n(total)} cars · type to search them all`;
    setActive(0, false);
  }

  function go(id) {
    const { path } = parseHash();
    const m = path.match(/^\/cars\/[^/]+\/([a-z]+)/);
    const section = m && m[1] !== 'drives' ? m[1] : (m ? 'drives' : 'now');
    close();
    location.hash = `#/cars/${encodeURIComponent(id)}/${section}`;
  }

  function open() {
    if (dlg.open) return;
    input.value = '';
    list.replaceChildren();
    options = [];
    dlg.showModal();
    input.focus();
    fill();
  }
  function close() { if (dlg.open) dlg.close(); }

  input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(fill, 90); });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setActive(active + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setActive(active - 1); }
    else if (e.key === 'Home' && e.ctrlKey) { e.preventDefault(); setActive(0); }
    else if (e.key === 'End' && e.ctrlKey) { e.preventDefault(); setActive(options.length - 1); }
    else if (e.key === 'Enter') { e.preventDefault(); if (options[active]) go(options[active].dataset.id); }
  });
  list.addEventListener('mousemove', (e) => {
    const o = e.target.closest('.switcher-option');
    if (o && +o.dataset.index !== active) setActive(+o.dataset.index, false);
  });
  list.addEventListener('click', (e) => { const o = e.target.closest('.switcher-option'); if (o) go(o.dataset.id); });
  dlg.addEventListener('click', (e) => { if (e.target === dlg) close(); });
  dlg.addEventListener('close', () => trigger.focus({ preventScroll: true }));
  trigger.addEventListener('click', open);

  const mac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
  document.getElementById('switcher-kbd').textContent = mac ? '⌘ K' : 'Ctrl K';

  document.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      if (dlg.open) close(); else open();
    }
  });
  return { open, close };
})();

/* ===================================================== shared drawings === */

/** A thin battery bar: SVG with percentage widths, so it needs no styles. */
function batteryBar(soc, { from = null, limit = null, cls = '' } = {}) {
  const pct = Math.max(0, Math.min(100, soc));
  const low = pct < 20;
  return s('svg', { class: 'bar ' + cls, width: '100%', height: '100%', 'aria-hidden': 'true', focusable: 'false' },
    s('rect', { class: 'bar-track', x: 0, y: 0, width: '100%', height: '100%' }),
    from != null ? s('rect', { class: 'bar-fill', x: 0, y: 0, width: `${Math.min(from, pct)}%`, height: '100%' }) : null,
    from != null
      ? s('rect', { class: 'bar-added', x: `${Math.min(from, pct)}%`, y: 0, width: `${Math.max(0, pct - from)}%`, height: '100%' })
      : s('rect', { class: 'bar-fill' + (low ? ' is-low' : ''), x: 0, y: 0, width: `${pct}%`, height: '100%' }),
    limit != null ? s('rect', { class: 'bar-limit', x: `${limit}%`, y: 0, width: 2, height: '100%' }) : null);
}

/** A small drawing of a route's shape. */
function routeSketch(points, cls = 'sketch') {
  const W = 64, H = 44, pad = 5;
  if (!points || points.length < 2) return s('svg', { class: cls, viewBox: `0 0 ${W} ${H}`, 'aria-hidden': 'true' });
  const lat0 = points[0][1] * Math.PI / 180;
  const xs = points.map((p) => p[0] * Math.cos(lat0)), ys = points.map((p) => -p[1]);
  const minX = Math.min(...xs), maxX = Math.max(...xs), minY = Math.min(...ys), maxY = Math.max(...ys);
  const k = Math.min((W - 2 * pad) / ((maxX - minX) || 1e-9), (H - 2 * pad) / ((maxY - minY) || 1e-9));
  const ox = (W - (maxX - minX) * k) / 2, oy = (H - (maxY - minY) * k) / 2;
  const pts = xs.map((x, i) => [ox + (x - minX) * k, oy + (ys[i] - minY) * k]);
  const d = pts.map((p, i) => (i ? 'L' : 'M') + p[0].toFixed(1) + ' ' + p[1].toFixed(1)).join('');
  const a = pts[0], b = pts[pts.length - 1];
  return s('svg', { class: cls, viewBox: `0 0 ${W} ${H}`, 'aria-hidden': 'true', focusable: 'false' },
    s('path', { class: 'sketch-line', d }),
    s('circle', { class: 'sketch-start', cx: a[0].toFixed(1), cy: a[1].toFixed(1), r: 2.4 }),
    s('circle', { class: 'sketch-end', cx: b[0].toFixed(1), cy: b[1].toFixed(1), r: 2.6 }));
}

/** A charge drawn as the battery level rising and flattening near the limit. */
function chargeSketch(from, to, cls = 'sketch') {
  const W = 64, H = 44, pad = 6;
  const y = (p) => H - pad - ((H - 2 * pad) * p) / 100;
  const pts = [];
  for (let i = 0; i <= 24; i++) {
    const f = i / 24;
    const eased = 1 - (1 - f) ** 1.35;
    pts.push([pad + (W - 2 * pad) * f, y(from + (to - from) * eased)]);
  }
  const d = pts.map((p, i) => (i ? 'L' : 'M') + p[0].toFixed(1) + ' ' + p[1].toFixed(1)).join('');
  return s('svg', { class: cls, viewBox: `0 0 ${W} ${H}`, 'aria-hidden': 'true', focusable: 'false' },
    s('line', { class: 'sketch-base', x1: pad, x2: W - pad, y1: H - pad, y2: H - pad }),
    s('path', { class: 'sketch-area', d: d + `L${W - pad} ${H - pad}L${pad} ${H - pad}Z` }),
    s('path', { class: 'sketch-line sketch-charge', d }),
    s('circle', { class: 'sketch-end sketch-charge-end', cx: (W - pad).toFixed(1), cy: y(to).toFixed(1), r: 2.6 }));
}

function label(textValue, extra) {
  return h('h2', { class: 'label' }, textValue, extra);
}

function figure(name, value, note, cls = '') {
  return h('div', { class: 'fig ' + cls }, h('dt', { class: 'fig-name' }, name), h('dd', { class: 'fig-value' }, value), note ? h('dd', { class: 'fig-note', title: typeof note === 'string' ? note : null }, note) : null);
}

/** Text that may be cut short with an ellipsis, with all of it in a tooltip. */
const clipped = (tag, cls, text) => h(tag, { class: cls, title: text }, text);

/* ============================================================ timeline === */

function placeName(p) { return h('span', { class: 'tl-place' }, p.name); }

function timelineItem(e, carId, { full = false } = {}) {
  const nowMs = clock.now();
  const end = e.end == null ? nowMs : e.end;
  const lenS = (end - e.start) / 1000;
  let title, meta = [], cls = e.type, href = null;
  if (e.type === 'drive') {
    const d = e.drive;
    const done = !e.ongoing && !e.cutOff;
    href = done ? `#/cars/${encodeURIComponent(carId)}/drives/${encodeURIComponent(d.id)}` : null;
    // A drive's destination is only known in advance when navigation has one.
    if (e.ongoing) title = d.to ? ['Driving from ', placeName(d.from), ', navigating to ', placeName(d.to)] : ['Driving from ', placeName(d.from)];
    else if (e.cutOff) title = ['Was driving from ', placeName(d.from), d.to ? [' toward ', placeName(d.to)] : '', ' when reports stopped'];
    else title = ['Drove from ', placeName(d.from), ' to ', placeName(d.to)];
    meta = done ? [T.dist(d.distanceKm), dur(d.durationS), T.eff(d.efficiencyWhKm)] : [`${T.dist(d.distanceKm)} so far`, dur(lenS)];
    if (full && done) meta.push(T.kwh(d.energyKwh, 2), `${n(d.startSoc)}% → ${n(d.endSoc)}%`);
  } else if (e.type === 'charge') {
    const c = e.charge;
    title = e.ongoing ? ['Charging at ', placeName(e.place)] : e.cutOff ? ['Was charging at ', placeName(e.place), ' when reports stopped'] : ['Charged at ', placeName(e.place)];
    meta = [`${n(c.socStart)}% → ${n(c.socEnd)}%${e.ongoing ? ' so far' : ''}`, `+${T.kwh(c.energyAddedKwh)}`, e.ongoing || e.cutOff ? `${T.kw(c.powerKw)} ${c.kind}` : dur(lenS)];
    if (full && !e.ongoing && !e.cutOff) meta.push(`${c.kind}, up to ${T.kw(c.maxPowerKw)}`);
  } else if (e.type === 'asleep') {
    title = [e.end == null ? 'Asleep at ' : 'Slept at ', placeName(e.place)];
    meta = [dur(lenS)];
    if (e.plugged) meta.push('plugged in');
  } else if (e.type === 'parked') {
    if (e.woke) { title = ['Woke up at ', placeName(e.place)]; meta = [e.end == null ? 'awake now' : `${dur(lenS)} before leaving`]; cls = 'awake'; }
    else {
      title = ['Parked at ', placeName(e.place), e.plugged ? ', plugged in' : ''];
      meta = [e.end == null ? `since ${clockTime(e.start)}` : dur(lenS)];
      if (e.asleepS > 20 * 60) meta.push(`asleep ${dur(e.asleepS)} of it`);
    }
  } else if (e.type === 'nodata') {
    const since = e.since || e.start;
    title = ['No data since ', dayKeyOf(since) === dayKeyOf(e.start) ? clockTime(since) : `${fmt.dayShort.format(since)}, ${clockTime(since)}`];
    meta = [`nothing heard for ${dur((nowMs - since) / 1000)}`];
    cls = 'nodata';
  }
  // Lines break only between the "·"-separated parts, never inside one.
  const body = h('div', { class: 'tl-body' },
    h('p', { class: 'tl-title' }, href ? h('a', { href }, title) : title),
    h('p', { class: 'tl-meta' }, meta.flatMap((m, i) => { const seg = h('span', { class: 'tl-seg' }, m); return i ? [sep(), seg] : [seg]; })));
  return h('li', { class: `tl-item tl-${cls}${e.end == null ? ' is-now' : ''}` },
    h('div', { class: 'tl-time' },
      h('time', { datetime: new Date(e.start).toISOString() }, clockTime(e.start)),
      full ? h('span', { class: 'tl-end' }, e.end == null ? 'now' : clockTime(e.end)) : null),
    h('div', { class: 'tl-mark', 'aria-hidden': 'true' }),
    body,
    full && e.type === 'drive' ? routeSketch(e.drive.sketch, 'sketch tl-sketch') : null,
    full && e.type === 'charge' ? chargeSketch(e.charge.socStart, e.charge.socEnd, 'sketch tl-sketch') : null);
}

function timelineList(events, carId, opts = {}) {
  if (!events.length) return h('p', { class: 'empty-note' }, 'Nothing recorded for this day yet.');
  return h('ol', { class: 'tl' + (opts.full ? ' tl-full' : '') }, events.map((e) => timelineItem(e, carId, opts)));
}

/* ================================================================ maps === */

let maplibre = null;
async function mapLib() {
  if (!maplibre) {
    maplibre = await import('./vendor/maplibre-gl/maplibre-gl.mjs');
    maplibre.setWorkerUrl(new URL('./vendor/maplibre-gl/maplibre-gl-worker.mjs', import.meta.url).href);
  }
  return maplibre;
}

function isDark() {
  const t = root.dataset.theme;
  if (t === 'dark') return true;
  if (t === 'light') return false;
  return matchMedia('(prefers-color-scheme: dark)').matches;
}
const cssVar = (name) => getComputedStyle(root).getPropertyValue(name).trim();

/**
 * The prototype's map is raster tiles, whose labels are drawn into the
 * images at 256 px per tile. When the interface is well above the standard
 * size (a 4K screen on Auto, or a big Interface size), each tile is shown
 * that much larger, so the map's street names grow with the rest of the
 * page instead of staying 8 to 10 px. (Never smaller: below 1.2 it stays
 * 256.) Vector tiles in the real server will draw labels at any size.
 */
const tilePx = () => (display.scale >= 1.2 ? Math.round(256 * display.scale) : 256);
const osmSource = () => ({
  type: 'raster', tiles: ['https://tile.openstreetmap.org/{z}/{x}/{y}.png'], tileSize: tilePx(), maxzoom: 19,
  attribution: '<a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noopener">© OpenStreetMap contributors</a>',
});
const osmLayer = (dark) => ({
  id: 'osm', type: 'raster', source: 'osm',
  paint: dark
    ? { 'raster-saturation': -1, 'raster-brightness-min': 0.78, 'raster-brightness-max': 0.07, 'raster-contrast': 0.08, 'raster-opacity': 0.92, 'raster-fade-duration': 0 }
    : { 'raster-saturation': -0.88, 'raster-contrast': -0.16, 'raster-brightness-min': 0.2, 'raster-opacity': 0.7, 'raster-fade-duration': 0 },
});

function mapStyle() {
  return {
    version: 8,
    sources: { osm: osmSource() },
    layers: [{ id: 'paper', type: 'background', paint: { 'background-color': cssVar('--map-paper') } }, osmLayer(isDark())],
  };
}

const coordsText = ([lon, lat]) => `${Math.abs(lat).toFixed(4)}° ${lat >= 0 ? 'N' : 'S'}, ${Math.abs(lon).toFixed(4)}° ${lon >= 0 ? 'E' : 'W'}`;

/**
 * Make a muted map in `el`. Resolves `ready` once tiles have drawn. The box
 * shimmers until then; if tiles cannot be had, a note says so and gives the
 * position in words instead.
 *
 * Sizes: the box is sized in rem by app.css and MapLibre follows it (it
 * watches its container). It does not watch the pixel ratio, so a change of
 * ratio (another screen, browser zoom) calls resize(), which reads it again.
 * Lines and padding are given in px at the standard size and scaled with the
 * interface size; until the person moves the map, a resized map frames the
 * same bounds again, so a bigger map shows the same route, larger.
 */
async function makeMap(el, ctx, { center, zoom = 14, bounds = null, padding = 40, interactive = true, maxZoom = 16, coordText = null } = {}) {
  el.classList.add('is-loading');
  const ml = await mapLib();
  if (!ctx.alive()) return null;
  const pad = () => Math.round(padding * display.scale);
  const map = new ml.Map({
    container: el, style: mapStyle(), center, zoom,
    bounds: bounds || undefined, fitBoundsOptions: { padding: pad(), maxZoom },
    attributionControl: { compact: false },
    refreshExpiredTiles: false, // lets tiles load as images, which the page's CSP allows
    scrollZoom: false, dragRotate: false, pitchWithRotate: false, touchPitch: false,
    interactive, fadeDuration: 0, maxZoom: 18,
    maxCanvasSize: [8192, 8192], // big 4K maps at a pixel ratio of 2 stay full resolution
  });
  map.touchZoomRotate.disableRotation();
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  const lineWidths = new Map(); // layer id -> width in px at the standard size
  let framing = bounds;
  let moved = false;
  map.on('dragstart', () => { moved = true; });
  map.on('zoomstart', (e) => { if (e.originalEvent) moved = true; });
  const reframe = () => { if (framing && !moved) map.fitBounds(framing, { padding: pad(), maxZoom, animate: false }); };
  map.on('resize', reframe);
  let tiles = tilePx();
  const stopWatching = display.onChange((change) => {
    // MapLibre follows its box but not the pixel ratio: resize() makes it read the ratio again.
    if (change.dpr) map.resize();
    if (change.rem) {
      for (const [id, w] of lineWidths) if (map.getLayer(id)) map.setPaintProperty(id, 'line-width', w * display.scale);
      if (tilePx() !== tiles && map.isStyleLoaded() && map.getSource('osm')) {
        // Swap the tiles for ones drawn at the new size, under the same lines and pins.
        tiles = tilePx();
        const above = map.getStyle().layers.map((l) => l.id);
        const next = above[above.indexOf('osm') + 1];
        map.removeLayer('osm');
        map.removeSource('osm');
        map.addSource('osm', osmSource());
        map.addLayer(osmLayer(isDark()), next);
      }
      reframe();
    }
  });
  ctx.onCleanup(() => { stopWatching(); map.remove(); });
  let failed = 0;
  map.on('error', (e) => {
    if (!(e && (e.tile || e.sourceId === 'osm'))) return;
    if (++failed === 1) {
      const where = coordText || coordsText(map.getCenter().toArray());
      el.append(h('p', { class: 'map-note', role: 'note' }, `Map tiles unavailable; position ${where}`));
    }
  });
  const ready = new Promise((resolve) => {
    let done = false;
    const finish = () => { if (!done) { done = true; el.classList.remove('is-loading'); resolve(); } };
    map.once('idle', finish);
    setTimeout(finish, 15000);
  });
  const loaded = new Promise((resolve) => map.once('load', resolve));
  ctx.waitFor(ready);
  // Pin labels go to whichever side has room, checked again whenever the map moves or changes size.
  const pins = [];
  const placePins = () => { for (const p of pins) flipPin(map, p); };
  map.on('resize', placePins);
  map.on('moveend', placePins);
  return {
    map, ml, ready, loaded, pins, placePins,
    /** Add a line layer whose width (px at the standard size) follows the interface size. */
    addLine(layer, width) {
      lineWidths.set(layer.id, width);
      map.addLayer({ ...layer, type: 'line', paint: { ...layer.paint, 'line-width': width * display.scale } });
    },
    /** Frame other bounds, now and after each resize. */
    frame(b) { framing = b; moved = false; reframe(); },
    /** Resolves once the map has drawn again after its box changed size (MapLibre follows it within 50ms). */
    settled() {
      return new Promise((resolve) => {
        const done = setTimeout(resolve, 15000);
        setTimeout(() => { map.once('idle', () => { clearTimeout(done); resolve(); }); map.triggerRepaint(); }, 120);
      });
    },
  };
}

function pinEl(text, cls) {
  return h('div', { class: 'pin ' + cls }, h('span', { class: 'pin-dot' }), text ? h('span', { class: 'pin-label' }, text) : null);
}

/**
 * A labelled pin. The marker is anchored at the place by its left edge, and
 * app.css centres the dot on it (in rem). When the label would run past the
 * map's right edge and there is more room on the left, .pin-flip puts the
 * label on the left instead (a CSS shift by the pin's own width).
 */
function addPin(m, lngLat, text, cls) {
  const el = pinEl(text, cls);
  const marker = new m.ml.Marker({ element: el, anchor: 'left' }).setLngLat(lngLat).addTo(m.map);
  const pin = { el, lngLat, labelled: !!text };
  m.pins.push(pin);
  flipPin(m.map, pin);
  return marker;
}

function flipPin(map, pin) {
  if (!pin.labelled) return;
  const x = map.project(pin.lngLat).x;
  const w = map.getContainer().clientWidth;
  const need = pin.el.offsetWidth + display.rem; // the pin and a little air
  pin.el.classList.toggle('pin-flip', x + need > w && x > w - x);
}

function lineFeature(coords) {
  return { type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: coords } };
}

const ROUND = { 'line-cap': 'round', 'line-join': 'round' };
function addRoute(m, id, coords, { kind = 'route' } = {}) {
  m.map.addSource(id, { type: 'geojson', data: lineFeature(coords) });
  if (kind === 'ahead') {
    m.addLine({ id: id + '-line', source: id, layout: ROUND, paint: { 'line-color': cssVar('--ink-3'), 'line-dasharray': [1.5, 2] } }, 2.2);
    return;
  }
  m.addLine({ id: id + '-casing', source: id, layout: ROUND, paint: { 'line-color': cssVar('--paper'), 'line-opacity': 0.9 } }, kind === 'faint' ? 5 : 8);
  m.addLine({ id: id + '-line', source: id, layout: ROUND, paint: { 'line-color': cssVar(kind === 'faint' ? '--ink-3' : '--accent') } }, kind === 'faint' ? 2.4 : 3.6);
}

function boundsOf(coords) {
  let w = Infinity, so = Infinity, e = -Infinity, nn = -Infinity;
  for (const [x, y] of coords) { w = Math.min(w, x); e = Math.max(e, x); so = Math.min(so, y); nn = Math.max(nn, y); }
  return [[w, so], [e, nn]];
}

/** "wide", "landscape", "square" or "tall": a route's width over its height, as the map draws it. */
function routeShape(coords) {
  const [[w, so], [e, nn]] = boundsOf(coords);
  const ratio = ((e - w) * Math.cos(((so + nn) / 2) * Math.PI / 180)) / Math.max(nn - so, 1e-6);
  return ratio >= 1.75 ? 'wide' : ratio >= 1.2 ? 'landscape' : ratio >= 0.85 ? 'square' : 'tall';
}

/* ============================================================ fleet page == */

const SORTS = [
  ['name:asc', 'Name, A to Z'],
  ['name:desc', 'Name, Z to A'],
  ['lastSeen:asc', 'Last seen, newest first'],
  ['lastSeen:desc', 'Last seen, oldest first'],
  ['battery:asc', 'Battery, lowest first'],
  ['battery:desc', 'Battery, highest first'],
  ['today:desc', 'Distance today, most first'],
  ['state:asc', 'State'],
  ['place:asc', 'Place, A to Z'],
];

async function fleetPage(ctx) {
  const qp = ctx.query;
  const [info, summary] = await Promise.all([api.serverInfo(), api.fleetSummary()]);
  if (!ctx.alive()) return;
  clock.set(info.now);
  const c = summary.counts;
  // A big fleet reads best one line per car; a household's few cars get room to breathe.
  const density = prefs.density || (summary.total > 60 ? 'compact' : 'comfortable');
  const compact = density === 'compact';
  const defaultRows = compact ? 50 : 25;
  const st = {
    q: qp.get('q') || '',
    state: ['all', 'driving', 'charging', 'parked', 'asleep', 'offline'].includes(qp.get('state')) ? qp.get('state') : 'all',
    sort: qp.get('sort') || 'name', dir: qp.get('dir') === 'desc' ? 'desc' : 'asc',
    cursor: qp.get('cursor') || null,
    rows: [25, 50, 100].includes(+qp.get('rows')) ? +qp.get('rows') : defaultRows,
  };

  const writeUrl = () => setQuery({ q: st.q, state: st.state === 'all' ? '' : st.state, sort: st.sort === 'name' ? '' : st.sort, dir: st.dir === 'asc' ? '' : st.dir, rows: st.rows === defaultRows ? '' : st.rows, cursor: st.cursor });

  const tallyDefs = [
    ['all', 'All cars', summary.total, `${summary.cities} cities`], ['driving', 'Driving', c.driving], ['charging', 'Charging', c.charging],
    ['parked', 'Parked', c.parked], ['asleep', 'Asleep', c.asleep], ['offline', 'Offline', c.offline, 'no recent report'],
  ];
  const tallies = h('div', { class: 'tallies', role: 'group', 'aria-label': 'Show cars by state' },
    tallyDefs.map(([key, name, count, note]) => h('button', {
      type: 'button', class: `tally tally-${key}`, 'aria-pressed': String(st.state === key), dataset: { state: key },
      onclick: () => { st.state = key; st.cursor = null; syncTallies(); writeUrl(); load(); },
    },
    h('span', { class: 'tally-count' }, n(count)),
    h('span', { class: 'tally-name' }, key === 'all' ? null : h('span', { class: `state-dot state-${key}`, 'aria-hidden': 'true' }), name,
      note ? h('span', { class: 'tally-note', title: note }, note) : null))));
  const syncTallies = () => tallies.querySelectorAll('.tally').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.state === st.state)));

  const search = h('input', {
    class: 'field-input', type: 'search', id: 'fleet-search', value: st.q, placeholder: 'Name, key, model, VIN or place',
    autocomplete: 'off', spellcheck: 'false', 'aria-describedby': 'fleet-search-hint',
  });
  let searchTimer = 0;
  search.addEventListener('input', () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => { st.q = search.value.trim(); st.cursor = null; writeUrl(); load(); }, 140);
  });
  const sortSel = h('select', { class: 'field-select', id: 'fleet-sort' },
    SORTS.map(([v, t]) => h('option', { value: v, selected: v === `${st.sort}:${st.dir}` }, t)));
  if (!SORTS.some(([v]) => v === `${st.sort}:${st.dir}`)) sortSel.prepend(h('option', { value: `${st.sort}:${st.dir}`, selected: true }, 'Custom'));
  sortSel.addEventListener('change', () => { [st.sort, st.dir] = sortSel.value.split(':'); st.cursor = null; writeUrl(); load(); syncHeads(); });
  const rowsSel = h('select', { class: 'field-select field-select-narrow', id: 'fleet-rows' }, [25, 50, 100].map((v) => h('option', { value: v, selected: v === st.rows }, `${v} rows`)));
  rowsSel.addEventListener('change', () => { st.rows = +rowsSel.value; st.cursor = null; writeUrl(); load(); });
  const densityGroup = h('div', { class: 'segmented segmented-field', role: 'radiogroup', 'aria-labelledby': 'fleet-density-label' },
    [['comfortable', 'Comfortable'], ['compact', 'Compact']].map(([k, t]) => h('button', {
      type: 'button', role: 'radio', class: 'segmented-option', 'aria-checked': String(density === k), dataset: { density: k },
      onclick: () => {
        if (k === density) return;
        prefs.density = k; store.set('density', k);
        refocusAfterRender = `[data-density="${k}"]`;
        setQuery({ q: st.q, state: st.state === 'all' ? '' : st.state, sort: st.sort === 'name' ? '' : st.sort, dir: st.dir === 'asc' ? '' : st.dir });
        render();
      },
    }, t)));
  radioKeys(densityGroup);

  const toolbar = h('div', { class: 'toolbar' },
    h('div', { class: 'field field-search' },
      h('label', { class: 'field-label', for: 'fleet-search' }, 'Find a car'),
      h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), search),
      h('span', { class: 'sr-only', id: 'fleet-search-hint' }, 'Press slash to jump here. Results update as you type.')),
    h('div', { class: 'toolbar-end' },
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'fleet-sort' }, 'Order'), sortSel),
      h('div', { class: 'field' }, h('span', { class: 'field-label', id: 'fleet-density-label' }, 'Density'), densityGroup),
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'fleet-rows' }, 'Page size'), rowsSel)));

  // Model, Range and Area are their own columns only on wide screens, and Odometer, Software, Cabin and
  // Charger only on the widest; app.css hides them elsewhere, where the first three sit under the car's
  // name, beside the battery and in the place's tooltip.
  const extra = [['odo', 'Odometer', null], ['sw', 'Software', null], ['cabin', 'Cabin', null], ['plug', 'Charger', null]];
  const cols = compact
    ? [['no', 'No.', null], ['car', 'Car', 'name'], ['model', 'Model', null], ['state', 'State', 'state'], ['battery', 'Battery', 'battery'], ['range', 'Range', null],
      ['where', 'Where', 'place'], ['area', 'Area', null], ...extra, ['seen', 'Last seen', 'lastSeen'], ['today', 'Today', 'today']]
    : [['no', 'No.', null], ['car', 'Car', 'name'], ['model', 'Model', null], ['state', 'State', 'state'], ['battery', 'Battery', 'battery'],
      ['where', 'Where', 'place'], ...extra, ['seen', 'Last seen', 'lastSeen'], ['today', 'Today', 'today']];
  const firstDir = { name: 'asc', state: 'asc', battery: 'asc', place: 'asc', lastSeen: 'asc', today: 'desc' };
  const heads = cols.map(([k, t, sortKey]) => h('th', { scope: 'col', class: 'c-' + k, dataset: { sort: sortKey || '' } },
    sortKey ? h('button', {
      type: 'button', class: 'sort-button',
      onclick: () => {
        if (st.sort === sortKey) st.dir = st.dir === 'asc' ? 'desc' : 'asc'; else { st.sort = sortKey; st.dir = firstDir[sortKey]; }
        st.cursor = null; writeUrl(); syncHeads(); load();
        const opt = [...sortSel.options].find((o) => o.value === `${st.sort}:${st.dir}`);
        if (opt) sortSel.value = opt.value;
      },
    }, t, h('span', { class: 'sort-mark', 'aria-hidden': 'true' })) : h('span', { class: k === 'no' ? 'sr-only-sm' : null }, t)));
  const syncHeads = () => heads.forEach((th) => {
    const k = th.dataset.sort;
    if (!k) return;
    const on = k === st.sort;
    const asc = k === 'lastSeen' ? st.dir === 'desc' : st.dir === 'asc';
    if (on) th.setAttribute('aria-sort', asc ? 'ascending' : 'descending'); else th.removeAttribute('aria-sort');
  });
  syncHeads();

  const tbody = h('tbody', null, skeletonRows(compact ? 14 : 8));
  const table = h('table', { class: 'ledger' + (compact ? ' is-compact' : '') },
    h('caption', { class: 'sr-only' }, 'All cars. Select a car to open its Now page.'),
    h('colgroup', null, cols.map(([k]) => h('col', { class: 'col-' + k }))),
    h('thead', null, h('tr', null, heads)), tbody);
  tbody.addEventListener('click', (e) => {
    if (e.target.closest('a, button')) return;
    const tr = e.target.closest('tr[data-href]');
    if (tr) location.hash = tr.dataset.href;
  });

  // Above the table: where we are, and a thumb index by initial when sorted by name
  // (26 letters, or on narrow windows a menu of them).
  const rowsLine = h('p', { class: 'pager-status', 'aria-live': 'polite' });
  const thumb = h('nav', { class: 'thumb', 'aria-label': 'Jump to cars by initial' });
  const indexBar = h('div', { class: 'ledger-index' }, rowsLine, thumb);
  let thumbAt = new Map();

  const status = h('p', { class: 'pager-status' });
  const mk = (label, iconName, after = false) => h('button', { type: 'button', class: 'button button-quiet', disabled: true }, after ? null : icon(iconName), label, after ? icon(iconName) : null);
  const first = mk('First', 'chevronsLeft'), prev = mk('Previous', 'chevronLeft'), next = mk('Next', 'chevronRight', true), last = mk('Last', 'chevronsRight', true);
  const pageNo = h('span', { class: 'pager-page' });
  let cursors = {};
  const goTo = (which) => { st.cursor = cursors[which]; writeUrl(); load(true); };
  first.addEventListener('click', () => goTo('first'));
  prev.addEventListener('click', () => goTo('prev'));
  next.addEventListener('click', () => goTo('next'));
  last.addEventListener('click', () => goTo('last'));
  const pager = h('nav', { class: 'pager', 'aria-label': 'Pages' }, status, h('div', { class: 'pager-controls' }, first, prev, pageNo, next, last));

  const head = h('header', { class: 'page-head page-head-fleet' },
    h('div', { class: 'page-head-main' },
      h('p', { class: 'eyebrow' }, 'The register'),
      h('h1', { class: 'page-title' }, 'All cars'),
      h('p', { class: 'deck' }, `${n(summary.total)} cars on the books. Between them, ${T.dist(summary.distanceTodayKm, 0)} driven today.`)),
    h('p', { class: 'page-head-aside asof' },
      h('span', { class: 'asof-dot', 'aria-hidden': 'true' }),
      h('span', { class: 'asof-time' }, 'Updated ', h('time', { datetime: new Date(summary.asOf).toISOString() }, clockTime(summary.asOf)), ' CT'),
      h('span', { class: 'asof-note' }, 'Reports reach us about a minute after the car sends them.')));

  // On wide screens the title and the tallies share a row (app.css).
  ctx.main.replaceChildren(h('div', { class: 'fleet-top' }, head, tallies), toolbar, indexBar, h('div', { class: 'ledger-frame' }, table), pager);

  const onSlash = (e) => {
    if (e.key === '/' && !e.ctrlKey && !e.metaKey && !/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName) && !document.getElementById('switcher').open) {
      e.preventDefault(); search.focus();
    }
  };
  document.addEventListener('keydown', onSlash);
  ctx.onCleanup(() => document.removeEventListener('keydown', onSlash));

  let seq = 0;
  async function load(scrollUp = false) {
    const my = ++seq;
    table.classList.add('is-loading');
    tbody.setAttribute('aria-busy', 'true');
    const res = await api.listCars({ query: st.q, state: st.state, sort: st.sort, dir: st.dir, cursor: st.cursor, limit: st.rows });
    if (my !== seq || !ctx.alive()) return;
    table.classList.remove('is-loading');
    tbody.removeAttribute('aria-busy');
    const t = clock.now();
    if (!res.items.length) {
      const what = st.q ? ['No cars match “', h('strong', null, st.q), '”', st.state !== 'all' ? ` among ${STATE_LABEL[st.state].toLowerCase()} cars` : '', '.'] : [`No cars are ${STATE_LABEL[st.state].toLowerCase()} right now.`];
      tbody.replaceChildren(h('tr', { class: 'ledger-empty' }, h('td', { colspan: 99 },
        h('p', { class: 'empty-title' }, what),
        h('p', { class: 'empty-body' }, st.q ? 'Try part of a name, a key such as chi-014, a model, a VIN or a place.' : 'Choose another state above to see more of the register.'),
        st.q || st.state !== 'all' ? h('button', {
          type: 'button', class: 'button',
          onclick: () => { st.q = ''; search.value = ''; st.state = 'all'; st.cursor = null; syncTallies(); writeUrl(); load(); },
        }, 'Show all cars') : null)));
    } else {
      tbody.replaceChildren(...res.items.map((car, i) => carRow(car, res.offset + i + 1, t, compact)));
      // The No. column is as wide as the longest number on this page (app.css).
      table.style.setProperty('--no-chars', String(n(res.offset + res.items.length).length));
    }
    cursors = { first: res.firstCursor, prev: res.prevCursor, next: res.nextCursor, last: res.lastCursor };
    first.disabled = !res.firstCursor; prev.disabled = !res.prevCursor; next.disabled = !res.nextCursor; last.disabled = !res.lastCursor;
    const pages = Math.max(1, Math.ceil(res.total / st.rows));
    const page = Math.min(pages, Math.floor(res.offset / st.rows) + 1);
    const rowsText = res.total ? `Rows ${n(res.offset + 1)}–${n(res.offset + res.items.length)} of ${n(res.total)}` : 'No rows';
    status.replaceChildren(rowsText);
    rowsLine.replaceChildren(rowsText);
    pageNo.textContent = `Page ${n(page)} of ${n(pages)}`;
    drawThumb(res);
    if (scrollUp) table.scrollIntoView({ block: 'start' });
  }

  function drawThumb(res) {
    if (!res.index || !res.items.length) { thumb.replaceChildren(); return; }
    thumbAt = new Map(res.index.map((x) => [x.letter, x.cursor]));
    const here = res.items[0].name.charAt(0).toUpperCase();
    const letters = st.dir === 'desc' ? [...'ZYXWVUTSRQPONMLKJIHGFEDCBA'] : [...'ABCDEFGHIJKLMNOPQRSTUVWXYZ'];
    const jump = (L) => { st.cursor = thumbAt.get(L); writeUrl(); load(); };
    const select = h('select', { class: 'field-select thumb-select', 'aria-label': 'Jump to cars starting with' },
      letters.filter((L) => thumbAt.has(L)).map((L) => h('option', { value: L, selected: L === here }, `Jump to ${L}`)));
    select.addEventListener('change', () => jump(select.value));
    thumb.replaceChildren(
      h('span', { class: 'thumb-letters' },
        h('span', { class: 'thumb-label', 'aria-hidden': 'true' }, 'Jump to'),
        ...letters.map((L) => (thumbAt.has(L)
          ? h('button', { type: 'button', class: 'thumb-letter', 'aria-current': L === here ? 'true' : null, 'aria-label': `Cars starting with ${L}`, onclick: () => jump(L) }, L)
          : h('span', { class: 'thumb-letter is-empty', 'aria-hidden': 'true' }, L)))),
      select);
  }
  await load();
}

// A colspan larger than the table spans all the columns shown, however many that is.
function skeletonRows(k) {
  return Array.from({ length: k }, () => h('tr', { class: 'ledger-skeleton', 'aria-hidden': 'true' },
    h('td', { colspan: 99 }, h('div', { class: 'skeleton skeleton-row' }))));
}

const plugMark = () => h('span', { class: 'plug-mark', title: 'Plugged in' }, icon('plug', 'icon icon-plug'), h('span', { class: 'sr-only' }, 'plugged in'));

function carRow(car, no, nowMs, compact = false) {
  const href = `#/cars/${encodeURIComponent(car.id)}/now`;
  const offline = car.state === 'unknown';
  const stale = car.stale && !offline;
  const st = offline ? 'offline' : car.state;
  const sinceText = () => `since ${dayKeyOf(car.since) === dayKeyOf(nowMs) ? clockTime(car.since) : fmt.monthDay.format(car.since)}`;
  // One pattern for the second line: what the state is doing, then whether it is plugged in.
  let sub;
  if (stale) sub = ['last known'];
  else if (car.state === 'driving') sub = [car.detail.speedKph != null ? T.speed(car.detail.speedKph) : ''];
  else if (car.state === 'charging') sub = [car.detail.powerKw != null ? T.kw(car.detail.powerKw) : ''];
  else if (car.state === 'parked' || car.state === 'asleep') sub = [sinceText(), car.detail.pluggedIn ? [sep(), plugMark()] : null];
  else sub = ['no recent data'];
  const flag = car.stale ? h('span', { class: 'flag' }, offline ? 'No signal' : 'Stale') : null;
  const absolute = `${fmt.dayLong.format(car.lastSeen)}, ${clockTime(car.lastSeen)} CT`;
  const age = h('span', { class: 'age' + (car.stale ? ' is-stale' : ''), dataset: { ago: car.lastSeen }, title: absolute }, ago(car.lastSeen, nowMs));
  const today = h('td', { class: 'c-today' }, car.todayKm > 0.05 ? Q.dist(car.todayKm) : h('span', { class: 'nil', 'aria-label': 'none' }, '—'));
  const rowCls = 'ledger-row' + (car.stale ? ' is-stale' : '') + (offline ? ' is-offline' : '');
  if (compact) {
    return h('tr', { class: rowCls, dataset: { href } },
      h('td', { class: 'c-no' }, n(no)),
      h('th', { class: 'c-car', scope: 'row' }, h('span', { class: 'car-line-1' }, h('a', { class: 'car-name', href, title: car.name }, car.name), h('span', { class: 'mono car-key' }, car.key))),
      h('td', { class: 'c-model', title: `${car.year} ${car.model} ${car.trim}` }, `${car.year} ${car.model}`),
      h('td', { class: 'c-state' }, h('span', { class: 'state-inline' }, stateMark(st), stale ? h('span', { class: 'inline-note' }, 'last known') : null,
        !car.stale && (car.state === 'parked' || car.state === 'asleep') && car.detail.pluggedIn ? plugMark() : null)),
      h('td', { class: 'c-battery' }, h('span', { class: 'batt-row' }, Q.pct(car.battery.soc), batteryBar(car.battery.soc, { cls: 'bar-cell' }), h('span', { class: 'inline-note' }, T.range(car.battery.rangeKm)))),
      h('td', { class: 'c-range' }, Q.range(car.battery.rangeKm)),
      h('td', { class: 'c-where', title: `${car.place.name}, ${car.place.area}` }, h('span', { class: 'place' }, car.place.name)),
      h('td', { class: 'c-area', title: car.place.area }, car.place.area),
      extraCells(car),
      h('td', { class: 'c-seen' }, h('span', { class: 'seen-line' }, age, flag)),
      today);
  }
  return h('tr', { class: rowCls, dataset: { href } },
    h('td', { class: 'c-no' }, n(no)),
    h('th', { class: 'c-car', scope: 'row' },
      h('a', { class: 'car-name', href, title: car.name }, car.name),
      h('span', { class: 'cell-sub cell-sub-car' }, h('span', { class: 'cell-sub-model', title: `${car.year} ${car.model} ${car.trim}` }, `${car.year} ${car.model} ${car.trim}`), sep(), h('span', { class: 'mono car-key' }, car.key))),
    h('td', { class: 'c-model' }, h('span', { class: 'model-name' }, `${car.year} ${car.model}`), h('span', { class: 'cell-sub', title: car.trim }, car.trim)),
    h('td', { class: 'c-state' }, stateMark(st), h('span', { class: 'cell-sub' }, sub)),
    h('td', { class: 'c-battery' },
      h('span', { class: 'batt-row' }, Q.pct(car.battery.soc), batteryBar(car.battery.soc, { cls: 'bar-cell' })),
      h('span', { class: 'cell-sub' }, T.range(car.battery.rangeKm))),
    h('td', { class: 'c-where' }, h('span', { class: 'place', title: car.place.name }, car.place.name), h('span', { class: 'cell-sub', title: car.place.area }, car.place.area)),
    extraCells(car),
    h('td', { class: 'c-seen' }, h('span', { class: 'seen-line' }, age, flag), h('span', { class: 'cell-sub' }, whenShort(car.lastSeen, nowMs))),
    today);
}

/** Odometer, software, cabin temperature and charger: the register's columns for the widest screens. */
function extraCells(car) {
  const plug = car.state === 'unknown' ? null : car.detail.powerKw != null ? T.kw(car.detail.powerKw) : car.detail.pluggedIn ? 'Plugged in' : null;
  return [
    h('td', { class: 'c-odo' }, car.odometerKm != null ? Q.dist(car.odometerKm, 0) : h('span', { class: 'nil' }, '—')),
    h('td', { class: 'c-sw', title: car.software || null }, car.software ? h('span', { class: 'mono' }, car.software) : h('span', { class: 'nil' }, '—')),
    h('td', { class: 'c-cabin' }, car.cabinC != null ? Q.temp(car.cabinC) : h('span', { class: 'nil' }, '—')),
    h('td', { class: 'c-plug' }, plug || h('span', { class: 'nil', 'aria-label': 'not plugged in' }, '—')),
  ];
}

/* ============================================================== now page == */

/** "36 min", "2 h 5 min", "25 days": how old something is, without "ago" (counted like ago()). */
function ageWords(then, nowMs) {
  const m = Math.floor((nowMs - then) / 60000);
  if (m < 60) return `${Math.max(1, Math.floor(nowMs / 60000) - Math.floor(then / 60000))}${NB}min`;
  if (m < 24 * 60) return m % 60 && m < 180 ? `${Math.floor(m / 60)}${NB}h ${m % 60}${NB}min` : `${Math.floor(m / 60)}${NB}h`;
  const d = Math.floor(m / 1440);
  return `${d}${NB}day${d === 1 ? '' : 's'}`;
}

/** Demo-only controls, kept out of the page itself in a dashed strip below the colophon. */
function protoControls(ctx, carId, current) {
  const strip = document.getElementById('proto');
  const box = document.getElementById('proto-controls');
  box.replaceChildren(h('div', { class: 'segmented segmented-proto', role: 'group', 'aria-label': 'Sample state' },
    ['parked', 'driving', 'charging'].map((k) => h('a', { class: 'segmented-option', href: `#/cars/${encodeURIComponent(carId)}/now${k === 'parked' ? '' : '?state=' + k}`, 'aria-current': k === current ? 'true' : null }, STATE_LABEL[k]))));
  strip.hidden = false;
  ctx.onCleanup(() => { strip.hidden = true; box.replaceChildren(); });
}

async function nowPage(ctx) {
  const carId = ctx.carId;
  const demoStates = ['parked', 'driving', 'charging'];
  const demo = api.isMock && carId === 'model-y' ? (demoStates.includes(ctx.query.get('state')) ? ctx.query.get('state') : 'parked') : null;
  if (demo) api.setDemoState(demo);
  const [info, car, now, tl] = await Promise.all([api.serverInfo(), api.getCar(carId), api.getNow(carId), api.getTimeline(carId)]);
  if (!ctx.alive()) return;
  clock.set(info.now);
  carCache.set(carId, Promise.resolve(car));
  const nowMs = clock.now();
  const offline = now.state === 'unknown';
  const stale = now.stale || offline;
  const stLabel = offline ? 'offline' : now.state;
  if (demo) protoControls(ctx, carId, demo);

  const sameDay = dayKeyOf(now.measuredAt) === dayKeyOf(nowMs);
  const head = h('header', { class: 'page-head car-head' },
    h('div', { class: 'page-head-main' },
      h('p', { class: 'eyebrow' }, 'Now'),
      h('h1', { class: 'page-title' }, car.name),
      h('p', { class: 'car-line' }, `${car.year} ${car.model} ${car.trim}`, sep(), car.color, sep(), h('span', { class: 'mono' }, car.key))),
    h('div', { class: 'page-head-aside' },
      h('div', { class: 'freshness' + (stale ? ' is-stale' : '') },
        // The age counts from when the car measured the figures, not from when they reached us.
        h('p', { class: 'freshness-line' }, h('span', { class: 'freshness-dot', 'aria-hidden': 'true' }), 'Last seen ', h('strong', { dataset: { ago: now.measuredAt } }, ago(now.measuredAt, nowMs))),
        h('p', { class: 'freshness-note' }, sameDay
          ? `Measured ${clockTime(now.measuredAt)}, received ${clockTime(now.receivedAt)} CT`
          : `Last report ${fmt.dayShort.format(now.measuredAt)}, ${clockTime(now.measuredAt)} CT`))));

  const banner = stale ? h('p', { class: 'stale-banner', role: 'note' },
    h('strong', null, `Last known values, ${ageWords(now.measuredAt, nowMs)} old.`),
    ` Last report ${whenShort(now.measuredAt, nowMs)} CT; nothing has arrived since.`) : null;
  const lede = h('p', { class: 'lede' }, ledeFor(now, stale));
  const hero = heroFor(now, car, stale);
  const vitals = vitalsFor(now, car, stale);
  const where = whereFor(now, ctx, stale);
  const latest = latestFor(now, carId, stale);

  // Today's figures add up the entries listed under them, rounded as shown.
  const evs = tl.events;
  const dr = evs.filter((e) => e.type === 'drive'), chg = evs.filter((e) => e.type === 'charge');
  const known = now.today.reported;
  const todayNote = !known ? 'No reports today.' : stale ? `Up to the last report at ${clockTime(now.measuredAt)}.` : null;
  const today = h('section', { class: 'today', 'aria-labelledby': 'today-title' },
    h('div', { class: 'today-head' },
      h('p', { class: 'eyebrow' }, 'Today'),
      h('h2', { class: 'today-date', id: 'today-title' }, fmt.dayLong.format(nowMs)),
      todayNote ? h('p', { class: 'today-note' }, todayNote) : null),
    h('dl', { class: 'today-figs' + (known ? '' : ' is-unknown') },
      figure('Drives', known ? q(n(dr.length)) : q('—')),
      figure('Distance', known ? Q.dist(shown.dist(dr.map((e) => e.drive.distanceKm))) : q('—')),
      figure('Energy used', known ? Q.kwh(shown.kwh(dr.map((e) => e.drive.energyKwh))) : q('—')),
      figure('Charged', known && chg.length ? plus(Q.kwh(shown.kwh(chg.map((e) => e.charge.energyAddedKwh), 1))) : q('—'))),
    timelineList(tl.events, carId),
    h('a', { class: 'more-link', href: `#/cars/${encodeURIComponent(carId)}/timeline` }, 'The whole day on the timeline', icon('arrowRight')));

  ctx.main.replaceChildren(head, h('div', { class: 'now-grid now-' + stLabel + (stale ? ' is-stale' : '') },
    h('div', { class: 'now-main' }, banner, lede, hero, vitals, where, latest),
    h('aside', { class: 'now-aside', 'aria-label': 'Today so far' }, today)));
}

function ledeFor(now, stale) {
  const p = now.location.place;
  const ch = now.charging;
  const em = (t) => h('em', null, t);
  const soc = `${n(now.battery.soc)}%`;
  if (now.state === 'unknown') return ['Last heard from ', ago(now.measuredAt), ', at ', em(p.name), '.'];
  if (stale) {
    // Old news is told as old news: when we last heard, and what it was doing then.
    const what = { driving: 'driving on ', charging: 'charging at ', parked: 'parked at ', asleep: 'asleep at ' }[now.state];
    return ['Last heard ', ago(now.measuredAt), ', ', what, em(p.name), ` (${soc}).`];
  }
  if (now.state === 'driving') {
    const d = now.drive;
    const tail = d.nav ? `, navigating to ${d.nav.to.name}.` : `, heading ${compass(now.location.heading)}.`;
    return ['Driving on ', em(p.name), `, ${T.dist(d.distanceKm)} from ${d.from.name}`, tail];
  }
  if (now.state === 'charging') {
    return ['Charging at ', em(p.name), `: ${soc} now, ${n(ch.limit)}% by about ${clockTime(now.measuredAt + ch.minutesLeft * 60000)}.`];
  }
  const verb = now.state === 'asleep' ? 'Asleep at ' : 'Parked at ';
  const plug = ch && ch.pluggedIn ? ', plugged in.' : '.';
  const tail = ch && ch.state === 'complete' ? ` Charge complete at ${soc}.` : '';
  return [verb, em(p.name), ` since ${clockTime(now.since)}${plug}`, tail];
}

function heroFor(now, car, stale) {
  const b = now.battery, ch = now.charging;
  const figs = [];
  const asOf = `as of ${clockTime(now.measuredAt)}`;
  if (stale) {
    // Only what was true at the last report: no "now", no time left, no arrival time.
    const today = dayKeyOf(now.measuredAt) === clock.todayKey();
    const tp = timeParts(now.measuredAt);
    figs.push(figure('Battery', Q.pct(b.soc), 'last known', 'fig-hero'));
    figs.push(figure('Range', Q.range(b.rangeKm), 'last known, estimated', 'fig-hero'));
    figs.push(figure('Last report', today ? q(tp.hm, tp.ap) : q(fmt.monthDay.format(now.measuredAt)), ago(now.measuredAt), 'fig-hero'));
  } else if (now.state === 'driving') {
    const d = now.drive;
    figs.push(figure('Speed', Q.speed(d.speedKph), `${asOf} · ${d.powerKw < 0 ? 'regenerating ' + n(-d.powerKw, 0) : 'drawing ' + n(d.powerKw, 0)}${NB}kW`, 'fig-hero'));
    figs.push(figure('Battery', Q.pct(b.soc), `${T.range(b.rangeKm)} estimated`, 'fig-hero'));
    if (d.nav) figs.push(figure('To go', Q.dist(d.nav.remainingKm), `${asOf} · ETA ${clockTime(d.nav.eta)}`, 'fig-hero'));
    else figs.push(figure('Driven', Q.dist(d.distanceKm), `since ${d.from.name}, ${clockTime(d.startedAt)}`, 'fig-hero'));
  } else if (now.state === 'charging') {
    figs.push(figure('Battery', Q.pct(b.soc), `${T.range(b.rangeKm)} estimated`, 'fig-hero'));
    figs.push(figure('Charging at', Q.kw(ch.powerKw), `${ch.kind} · ${n(ch.voltage)}${NB}V${ch.currentA ? ` · ${n(ch.currentA)}${NB}A` : ''}`, 'fig-hero'));
    figs.push(figure('Time left', q(n(ch.minutesLeft), 'min'), `+${T.kwh(ch.addedKwh)} since ${clockTime(ch.startedAt)}`, 'fig-hero'));
  } else {
    figs.push(figure('Battery', Q.pct(b.soc), ch && ch.state === 'complete' ? 'charge complete' : ch && ch.state === 'stopped' ? 'plugged in, not charging' : 'not charging', 'fig-hero'));
    figs.push(figure('Range', Q.range(b.rangeKm), 'estimated', 'fig-hero'));
    if (ch && ch.state === 'complete') figs.push(figure('Last charge', plus(Q.kwh(ch.addedKwh)), `${n(ch.socStart)}% to ${n(b.soc)}%, finished ${clockTime(ch.endedAt)}`, 'fig-hero'));
    else figs.push(figure('Odometer', Q.dist(now.odometerKm, 0), `software ${now.software}`, 'fig-hero'));
  }
  const charging = now.state === 'charging' && !stale;
  const gauge = h('div', { class: 'gauge' + (charging ? ' is-charging' : '') },
    h('div', { class: 'gauge-bar' }, batteryBar(b.soc, { from: charging ? ch.socStart : null, limit: b.limit, cls: 'bar-gauge' })),
    // Drawn in percentages of its box, which app.css sizes in rem, so it scales with the text.
    s('svg', { class: 'gauge-scale', width: '100%', 'aria-hidden': 'true', focusable: 'false' },
      [0, 25, 50, 75, 100].map((t) => [
        s('line', { class: 'gauge-tick', x1: `${t}%`, x2: `${t}%`, y1: 0, y2: '23%' }),
        Math.abs(t - b.limit) > 6 ? s('text', { class: 'gauge-num', x: `${t}%`, y: '77%', 'text-anchor': t === 0 ? 'start' : t === 100 ? 'end' : 'middle' }, `${t}`) : null,
      ]),
      s('text', { class: 'gauge-limit', x: `${b.limit}%`, y: '77%', 'text-anchor': 'middle' }, `limit ${b.limit}`)),
    h('p', { class: 'sr-only' }, `Battery at ${n(b.soc)} percent, charge limit ${b.limit} percent.`));
  const status = [];
  if (ch && ch.pluggedIn) status.push(h('span', { class: 'chip chip-plug' }, icon('plug'), now.state === 'charging' ? 'Charging' : 'Plugged in'));
  else if (now.state !== 'driving') status.push(h('span', { class: 'chip' }, 'Unplugged'));
  if (now.locked) status.push(h('span', { class: 'chip' }, 'Locked'));
  if (now.climate.climateOn) status.push(h('span', { class: 'chip' }, 'Climate on'));
  return h('section', { class: 'hero' + (stale ? ' is-stale' : ''), 'aria-label': 'Battery and range' },
    h('dl', { class: 'hero-figs' }, figs),
    gauge,
    h('div', { class: 'chips' }, stale ? h('span', { class: 'chips-label' }, 'Last known') : null, status));
}

function vitalsFor(now, car, stale) {
  const c = now.climate, t = now.tires;
  const row = (name, value, note) => h('div', { class: 'spec-row' }, h('dt', null, name), h('dd', null, value, note ? h('span', { class: 'spec-note' }, note) : null));
  const temps = h('section', { class: 'vital' }, label('Temperature', stale ? h('span', { class: 'label-note' }, 'last known') : null),
    h('dl', { class: 'spec' }, row('Outside', Q.temp(c.outsideC)), row('Cabin', Q.temp(c.cabinC), c.climateOn ? 'climate on' : null), row('Battery', Q.temp(c.batteryC))));
  const carSpec = h('section', { class: 'vital' }, label('The car'),
    h('dl', { class: 'spec' },
      row('12 V battery', q(n(now.lowVoltage.voltage, 1), 'V'), now.lowVoltage.health === 'good' ? 'healthy' : 'check'),
      row('Odometer', Q.dist(now.odometerKm, 0)),
      row('Software', h('span', { class: 'mono' }, now.software)),
      row('Doors', now.locked ? 'Locked' : 'Unlocked')));
  return h('div', { class: 'vitals' + (stale ? ' is-stale' : '') }, temps, tiresFor(t, stale), carSpec);
}

function tiresFor(t, stale) {
  const rec = t.recommendedBar;
  const low = (v) => v < rec - 0.2;
  const val = (v) => n(conv.press(v), prefs.units === 'us' ? 0 : 1);
  const tire = (x, y) => s('rect', { class: 'tire-wheel', x, y, width: 9, height: 24, rx: 3 });
  const reading = (v, x, y, anchor) => s('text', { class: 'tire-value' + (low(v) ? ' is-low' : ''), x, y, 'text-anchor': anchor }, val(v));
  const anyLow = [t.fl, t.fr, t.rl, t.rr].some(low);
  // The car seen from above, nose up; FRONT and REAR sit on its centre line.
  return h('section', { class: 'vital vital-tires' }, label('Tires', h('span', { class: 'label-note' }, stale ? `${U().press}, last known` : U().press)),
    h('div', { class: 'tires-figure' },
      h('span', { class: 'tire-axle', 'aria-hidden': 'true' }, 'front'),
      s('svg', { class: 'tires', viewBox: '0 15 220 124', role: 'img', 'aria-label': `Tire pressures in ${U().press}: front left ${val(t.fl)}, front right ${val(t.fr)}, rear left ${val(t.rl)}, rear right ${val(t.rr)}.` },
        s('rect', { class: 'tire-body', x: 84, y: 19, width: 52, height: 116, rx: 22 }),
        s('path', { class: 'tire-glass', d: 'M92 53c8-6 28-6 36 0M92 105c8 5 28 5 36 0' }),
        tire(76, 33), tire(135, 33), tire(76, 99), tire(135, 99),
        reading(t.fl, 64, 52, 'end'), reading(t.fr, 156, 52, 'start'), reading(t.rl, 64, 118, 'end'), reading(t.rr, 156, 118, 'start')),
      h('span', { class: 'tire-axle', 'aria-hidden': 'true' }, 'rear')),
    h('p', { class: 'spec-foot' + (anyLow ? ' is-low' : '') }, anyLow ? 'One tire is low' : 'All four in range', ` · ${val(rec)}${NB}${U().press} recommended`));
}

function whereFor(now, ctx, stale) {
  const L = now.location;
  const mapBox = h('div', { class: 'map map-where is-loading', role: 'region', 'aria-label': `Map of ${L.place.name}` });
  const lat = Math.abs(L.lat).toFixed(4) + '° ' + (L.lat >= 0 ? 'N' : 'S');
  const lon = Math.abs(L.lon).toFixed(4) + '° ' + (L.lon >= 0 ? 'E' : 'W');
  const coordText = `${lat}, ${lon}`;
  const driving = now.state === 'driving' || (stale && now.drive);
  let lines;
  if (driving && now.drive) {
    const d = now.drive;
    lines = [
      h('p', { class: 'where-place' }, L.place.name),
      h('p', { class: 'where-area' }, L.place.area),
      // A destination is only known when the car's navigation has one.
      !stale && d.nav ? h('p', { class: 'where-since' }, `Navigating to ${d.nav.to.name}`, sep(), `ETA ${clockTime(d.nav.eta)}`)
        : h('p', { class: 'where-since' }, `Heading ${compass(L.heading)} on ${L.place.name}`),
      h('p', { class: 'where-since' }, `Left ${d.from.name} at ${clockTime(d.startedAt)}`)];
  } else {
    const arrived = L.arrivedAt || now.since;
    lines = [
      h('p', { class: 'where-place' }, L.place.name),
      h('p', { class: 'where-area' }, L.place.area),
      stale ? h('p', { class: 'where-since' }, `Arrived ${whenShort(arrived)}`)
        : h('p', { class: 'where-since' }, `Here since ${whenShort(arrived)}`, sep(), dur((clock.now() - arrived) / 1000))];
    const ch = now.charging;
    const from = now.latestDrive && now.latestDrive.to && now.latestDrive.to.name === L.place.name ? now.latestDrive : null;
    lines.push(h('dl', { class: 'where-facts' },
      from ? h('div', null, h('dt', null, 'Arrived from'), h('dd', null, from.from.name)) : null,
      h('div', null, h('dt', null, 'Charger'), h('dd', null, ch && ch.pluggedIn ? (now.state === 'charging' ? `Charging, ${ch.kind}` : `Plugged in, ${ch.kind}`) : 'Not plugged in')),
      h('div', null, h('dt', null, 'Sentry mode'), h('dd', null, now.sentry ? 'On' : `Off${L.place.name === 'Home' ? ' at home' : ''}`))));
  }
  const title = stale ? 'Last known place' : driving ? 'On the road' : 'Where';
  const section = h('section', { class: 'where', 'aria-labelledby': 'where-title' },
    h('div', { class: 'where-text' }, h('h2', { class: 'label', id: 'where-title' }, title), lines, h('p', { class: 'where-coords mono' }, coordText)),
    mapBox);
  ctx.waitFor((async () => {
    await Promise.resolve();
    const coords = [L.lon, L.lat];
    if (driving && now.drive) {
      const d = now.drive;
      const ahead = !stale && d.nav ? d.nav.route : [];
      const m = await makeMap(mapBox, ctx, { bounds: boundsOf(d.trail.concat(ahead)), padding: 36, maxZoom: d.routeDetail === 'sketch' ? 12.5 : 16, coordText });
      if (!m) return;
      await m.loaded;
      if (ahead.length) addRoute(m, 'ahead', ahead, { kind: 'ahead' });
      addRoute(m, 'trail', d.trail);
      addPin(m, d.trail[0], d.from.name, 'pin-start');
      if (ahead.length) addPin(m, ahead[ahead.length - 1], d.nav.to.name, 'pin-end');
      new m.ml.Marker({ element: h('div', { class: 'car-dot' }), anchor: 'center' }).setLngLat(coords).addTo(m.map);
    } else {
      const m = await makeMap(mapBox, ctx, { center: coords, zoom: 14.6, coordText });
      if (!m) return;
      await m.loaded;
      addPin(m, coords, L.place.name, 'pin-here');
    }
  })());
  return section;
}

function compass(deg) {
  return ['north', 'northeast', 'east', 'southeast', 'south', 'southwest', 'west', 'northwest'][Math.round(((deg % 360) + 360) % 360 / 45) % 8];
}

function latestFor(now, carId, stale) {
  const d = now.latestDrive, c = now.latestCharge, ch = now.charging;
  const todayKey = clock.todayKey();
  const when = (a, b) => `${dayName(dayKeyOf(a), todayKey)}, ${timeRange(a, b)}`;
  const empty = (title, note) => h('article', { class: 'entry entry-empty' }, h('p', { class: 'eyebrow' }, title), h('p', { class: 'empty-note' }, note));
  const driveCard = d ? h('article', { class: 'entry' },
    h('a', { class: 'entry-link', href: `#/cars/${encodeURIComponent(carId)}/drives/${encodeURIComponent(d.id)}` },
      h('p', { class: 'eyebrow' }, 'Latest drive'),
      h('div', { class: 'entry-head' },
        h('div', { class: 'entry-head-text' }, h('h3', { class: 'entry-title' }, h('em', null, d.from.name), h('span', { class: 'to' }, ' to '), h('em', null, d.to.name)), clipped('p', 'entry-when', when(d.start, d.end))),
        routeSketch(d.sketch, 'sketch entry-sketch')),
      h('dl', { class: 'entry-figs' },
        figure('Distance', Q.dist(d.distanceKm)), figure('Time', q(dur(d.durationS))),
        figure('Efficiency', Q.eff(d.efficiencyWhKm)), figure('Energy', Q.kwh(d.energyKwh, 2))),
      h('span', { class: 'entry-more' }, 'Open the drive', icon('arrowRight')))) : empty('Latest drive', 'No drives yet.');
  let chargeCard;
  const chargesHref = `#/cars/${encodeURIComponent(carId)}/charges`;
  if (now.state === 'charging' && ch && ch.state === 'charging') {
    // While it charges, this card follows the charge under way.
    chargeCard = h('article', { class: 'entry entry-live' },
      h('a', { class: 'entry-link', href: chargesHref },
        h('p', { class: 'eyebrow' }, 'This charge', h('span', { class: 'eyebrow-note' }, stale ? ' · last known' : ' · in progress')),
        h('div', { class: 'entry-head' },
          h('div', { class: 'entry-head-text' }, h('h3', { class: 'entry-title' }, 'At ', h('em', null, now.location.place.name)),
            clipped('p', 'entry-when', stale ? `${dayName(dayKeyOf(ch.startedAt), todayKey)}, ${clockTime(ch.startedAt)} – last report ${clockTime(now.measuredAt)}` : when(ch.startedAt, null))),
          chargeSketch(ch.socStart, now.battery.soc, 'sketch entry-sketch')),
        h('dl', { class: 'entry-figs' },
          figure('Added', plus(Q.kwh(ch.addedKwh)), 'so far'), figure('Battery', q(`${n(ch.socStart)}–${n(now.battery.soc)}`, '%'), `to ${n(ch.limit)}%`),
          figure('Power', Q.kw(ch.powerKw), ch.kind === 'DC' ? 'fast charger' : `AC, ${n(ch.currentA || 48)}${NB}A`), figure('Range', plus(Q.range(ch.addedRangeKm)))),
        h('span', { class: 'entry-more' }, 'All charges', icon('arrowRight'))));
  } else if (c) {
    chargeCard = h('article', { class: 'entry' },
      h('a', { class: 'entry-link', href: chargesHref },
        h('p', { class: 'eyebrow' }, 'Latest charge'),
        h('div', { class: 'entry-head' },
          h('div', { class: 'entry-head-text' }, h('h3', { class: 'entry-title' }, 'At ', h('em', null, c.place.name)), clipped('p', 'entry-when', when(c.start, c.end))),
          chargeSketch(c.socStart, c.socEnd, 'sketch entry-sketch')),
        h('dl', { class: 'entry-figs' },
          figure('Added', plus(Q.kwh(c.energyAddedKwh))), figure('Battery', q(`${n(c.socStart)}–${n(c.socEnd)}`, '%')),
          figure('Power', Q.kw(c.maxPowerKw), c.kind === 'DC' ? 'fast charger' : `AC, ${n(c.currentA || 48)}${NB}A`), figure('Range', plus(Q.range(c.rangeAddedKm)))),
        h('span', { class: 'entry-more' }, 'All charges', icon('arrowRight'))));
  } else chargeCard = empty('Latest charge', 'No charges yet.');
  return h('section', { class: 'latest', 'aria-label': 'Latest drive and charge' }, driveCard, chargeCard);
}

/* =========================================================== drives page == */

const PERIODS = [['7d', 'Last 7 days', 6], ['30d', 'Last 30 days', 29], ['month', 'This month', null], ['90d', 'Last 90 days', 89], ['all', 'All time', null]];

function periodRange(key, todayKey) {
  if (key === 'all') return { from: null, to: null };
  if (key === 'month') return { from: todayKey.slice(0, 8) + '01', to: todayKey };
  const p = PERIODS.find((x) => x[0] === key) || PERIODS[0];
  return { from: addDays(todayKey, -p[2]), to: todayKey };
}

async function drivesPage(ctx) {
  const carId = ctx.carId;
  const [info, car] = await Promise.all([api.serverInfo(), api.getCar(carId)]);
  if (!ctx.alive()) return;
  clock.set(info.now);
  const todayKey = clock.todayKey();
  const qp = ctx.query;
  const custom = qp.get('from') || qp.get('to');
  let period = custom ? 'custom' : (PERIODS.some((p) => p[0] === qp.get('period')) ? qp.get('period') : '7d');
  let range = custom ? { from: qp.get('from') || null, to: qp.get('to') || null } : periodRange(period, todayKey);

  const fromIn = h('input', { class: 'field-input field-date', type: 'date', id: 'drives-from', value: range.from || '', max: todayKey });
  const toIn = h('input', { class: 'field-input field-date', type: 'date', id: 'drives-to', value: range.to || '', max: todayKey });
  const presets = h('div', { class: 'segmented', role: 'group', 'aria-label': 'Period' },
    PERIODS.map(([k, t]) => h('button', { type: 'button', class: 'segmented-option', 'aria-pressed': String(period === k), dataset: { period: k } }, t)));
  presets.addEventListener('click', (e) => {
    const b = e.target.closest('[data-period]');
    if (!b) return;
    period = b.dataset.period;
    range = periodRange(period, todayKey);
    fromIn.value = range.from || ''; toIn.value = range.to || '';
    setQuery({ period: period === '7d' ? '' : period });
    syncPresets(); load();
  });
  const syncPresets = () => presets.querySelectorAll('[data-period]').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.period === period)));
  const onDates = () => {
    let a = fromIn.value || null, b = toIn.value || null;
    if (a && b && a > b) [a, b] = [b, a];
    period = 'custom'; range = { from: a, to: b };
    setQuery({ from: a, to: b });
    syncPresets(); load();
  };
  fromIn.addEventListener('change', onDates);
  toIn.addEventListener('change', onDates);

  const totals = h('dl', { class: 'totals' });
  const listBox = h('div', { class: 'drives-list' });
  const more = h('div', { class: 'list-more' });
  // Beside the list on wide screens (app.css shows it from 112.5rem): the drives listed on a map, and
  // the whole period's distance by day.
  const periodMap = h('div', { class: 'map map-period', role: 'region', 'aria-label': 'Map of the drives listed' });
  const mapNote = h('span', { class: 'label-note' });
  const byDay = h('figure', { class: 'by-day' });
  const side = h('aside', { class: 'drives-side', 'aria-label': 'The period at a glance' },
    h('h2', { class: 'label' }, 'On the map', mapNote), periodMap,
    h('h2', { class: 'label label-by-day' }, 'Distance by day'), byDay);

  const head = h('header', { class: 'page-head' },
    h('div', { class: 'page-head-main' },
      h('p', { class: 'eyebrow' }, car.name),
      h('h1', { class: 'page-title' }, 'Drives'),
      h('p', { class: 'deck' }, `Every trip ${car.name} has made: where it began and ended, how long it took and what it cost the battery.`)));
  const filters = h('div', { class: 'toolbar toolbar-drives' },
    h('div', { class: 'field' }, h('span', { class: 'field-label', id: 'period-label' }, 'Period'), presets),
    h('div', { class: 'toolbar-end' },
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'drives-from' }, 'From'), fromIn),
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'drives-to' }, 'To'), toIn)));

  ctx.main.replaceChildren(head, filters, totals, h('div', { class: 'drives-body' }, h('div', { class: 'drives-main' }, listBox, more), side));

  let items = [];
  let seq = 0;
  let nextCursor = null;
  async function load(cursor = null) {
    const my = ++seq;
    if (!cursor) { listBox.classList.add('is-loading'); if (!items.length) listBox.replaceChildren(h('div', { class: 'skeleton-block' }, Array.from({ length: 6 }, () => h('div', { class: 'skeleton skeleton-row' })))); }
    const res = await api.listDrives(carId, { from: range.from, to: range.to, cursor, limit: 50 });
    if (my !== seq || !ctx.alive()) return false;
    listBox.classList.remove('is-loading');
    items = cursor ? items.concat(res.items) : res.items;
    nextCursor = res.nextCursor;
    if (!cursor) drawTotals(res);
    drawList(res);
    drawSide(res);
    return true;
  }

  /** Go to a day in the list, loading earlier drives first when it is not listed yet. */
  async function showDay(key) {
    let group = document.getElementById('drives-day-' + key);
    while (!group && nextCursor && ctx.alive()) {
      if (!(await load(nextCursor))) return;
      group = document.getElementById('drives-day-' + key);
    }
    if (!group) return;
    group.scrollIntoView({ block: 'start', behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth' });
    // The day's first drive takes the focus, so keyboard users land where they asked to go.
    const link = group.querySelector('.route-name');
    if (link) link.focus({ preventScroll: true });
  }

  // The side panel is drawn only while it is shown: no map is made, and no tiles are fetched, for a hidden box.
  let sideMap = null, sideMapComing = false, lastRes = null;
  const hovered = { id: '' };
  function drawSide(res) {
    lastRes = res;
    if (!side.clientWidth) return;
    const listed = loadedByDay(items);
    // The whole period, from the server's per-day sums; a day whose drives are all listed is summed
    // as its rows are printed, so the bar and the day's line in the list always agree.
    const days = res.byDay.map((t) => ({ ...t, distanceKm: listed.get(t.day) && listed.get(t.day).n === t.drives ? shown.dist(listed.get(t.day).km) : t.distanceKm }));
    const fromKey = range.from || (days.length ? days[0].day : todayKey);
    const focused = byDay.contains(document.activeElement) ? document.activeElement.dataset.day : null;
    byDay.replaceChildren(...dayBars(days, fromKey, range.to || todayKey, todayKey));
    // One tab stop for the chart, on the latest day (or the day that had the focus before it was redrawn).
    const bars = byDay.querySelector('.by-day-bars');
    rovingKeys(bars, '[data-day]', {
      start: (all) => all.find((el) => el.dataset.day === focused) || all[all.length - 1],
      activate: (el) => showDay(el.dataset.day),
    });
    if (focused) { const again = bars.querySelector(`[data-day="${focused}"]`); if (again) again.focus({ preventScroll: true }); }
    mapNote.textContent = items.length === res.total ? (res.total === 1 ? 'the one drive' : `all ${n(res.total)} drives`) : `the latest ${n(items.length)} of ${n(res.total)} drives, as listed`;
    const lines = items.filter((d) => d.sketch && d.sketch.length > 1);
    const fc = { type: 'FeatureCollection', features: lines.map((d) => ({ ...lineFeature(d.sketch), properties: { id: d.id } })) };
    const bounds = lines.length ? boundsOf(lines.flatMap((d) => d.sketch)) : null;
    if (sideMap) {
      sideMap.map.getSource('period').setData(fc);
      if (bounds) sideMap.frame(bounds);
      return;
    }
    if (sideMapComing || !bounds) return;
    sideMapComing = true;
    ctx.waitFor((async () => {
      const m = await makeMap(periodMap, ctx, { bounds, padding: 28, maxZoom: 15 });
      if (!m) return;
      await m.loaded;
      if (!ctx.alive()) return;
      m.map.addSource('period', { type: 'geojson', data: fc });
      m.addLine({ id: 'period-casing', source: 'period', layout: ROUND, paint: { 'line-color': cssVar('--paper'), 'line-opacity': 0.9 } }, 5);
      m.addLine({ id: 'period-line', source: 'period', layout: ROUND, paint: { 'line-color': cssVar('--ink-3'), 'line-opacity': 0.8 } }, 2.2);
      m.addLine({ id: 'period-hover', source: 'period', layout: ROUND, filter: ['==', ['get', 'id'], hovered.id], paint: { 'line-color': cssVar('--accent') } }, 4.5);
      sideMap = m;
      drawSide(lastRes); // drives may have been added while the map was on its way
      await m.ready;
    })());
  }
  const sideWatch = new ResizeObserver(() => { if (lastRes && side.clientWidth && !sideMap && !sideMapComing) drawSide(lastRes); });
  sideWatch.observe(side);
  ctx.onCleanup(() => sideWatch.disconnect());
  // Pointing at a drive in the list, or moving the keyboard focus to it, picks it out on the map.
  const hover = (id) => {
    if (id === hovered.id) return;
    hovered.id = id;
    if (sideMap) sideMap.map.setFilter('period-hover', ['==', ['get', 'id'], id]);
  };
  const rowId = (e) => { const tr = e.target.closest('tr[data-id]'); return tr ? tr.dataset.id : ''; };
  listBox.addEventListener('mouseover', (e) => hover(rowId(e)));
  listBox.addEventListener('mouseleave', () => hover(listBox.contains(document.activeElement) ? rowId({ target: document.activeElement }) : ''));
  listBox.addEventListener('focusin', (e) => hover(rowId(e)));
  listBox.addEventListener('focusout', (e) => { if (!listBox.contains(e.relatedTarget)) hover(''); });
  byDay.addEventListener('click', (e) => {
    const bar = e.target.closest('[data-day]');
    if (bar) showDay(bar.dataset.day);
  });

  function drawTotals(res) {
    const t = { ...res.totals };
    if (!res.nextCursor) {
      // Every drive of the period is listed: add them up as they are printed.
      t.distanceKm = shown.dist(items.map((d) => d.distanceKm));
      t.energyKwh = shown.kwh(items.map((d) => d.energyKwh));
      t.durationS = shown.minutes(items.map((d) => d.durationS));
    }
    const span = range.from && range.to ? (range.from === range.to ? fmt.dayLong.format(keyToMs(range.from)) : `${fmt.monthDay.format(keyToMs(range.from))} – ${fmt.monthDay.format(keyToMs(range.to))}`) : range.from ? `Since ${fmt.monthDay.format(keyToMs(range.from))}` : range.to ? `Until ${fmt.monthDay.format(keyToMs(range.to))}` : `Since ${fmt.monthDay.format(keyToMs(res.earliestDay))}`;
    totals.replaceChildren(
      h('div', { class: 'totals-caption' }, h('p', { class: 'eyebrow' }, 'For the period'), h('p', { class: 'totals-span' }, span)),
      figure('Drives', q(n(t.drives))),
      figure('Distance', Q.dist(t.distanceKm, 1)),
      figure('Driving time', q(durShort(t.durationS))),
      figure('Energy used', Q.kwh(t.energyKwh)),
      figure('Efficiency', Q.eff(t.efficiencyWhKm), 'average'),
      figure('Longest', Q.dist(t.longestKm)));
  }

  function drawList(res) {
    if (!items.length) {
      listBox.replaceChildren(h('div', { class: 'empty' },
        h('p', { class: 'empty-title' }, 'No drives in this period.'),
        h('p', { class: 'empty-body' }, `${car.name} did not move between these dates. Try a longer period.`),
        h('button', { type: 'button', class: 'button', onclick: () => { period = 'all'; range = periodRange('all', todayKey); fromIn.value = ''; toIn.value = ''; setQuery({ period: 'all' }); syncPresets(); load(); } }, 'Show all drives')));
      more.replaceChildren();
      return;
    }
    const groups = new Map();
    for (const d of items) {
      const k = dayKeyOf(d.start);
      if (!groups.has(k)) groups.set(k, []);
      groups.get(k).push(d);
    }
    // Battery used and average speed are columns only wide screens show (app.css).
    const cols = ['time', 'route', 'dist', 'dur', 'batt', 'speed', 'eff', 'energy', 'go'];
    const table = h('table', { class: 'drives' },
      h('caption', { class: 'sr-only' }, 'Drives, newest first, grouped by day. Select a drive to open it.'),
      h('colgroup', null, cols.map((k) => h('col', { class: 'col-' + k }))),
      h('thead', null, h('tr', null,
        h('th', { scope: 'col', class: 'c-time' }, 'Time'), h('th', { scope: 'col', class: 'c-route' }, 'From and to'),
        h('th', { scope: 'col', class: 'c-num c-dist' }, 'Distance'), h('th', { scope: 'col', class: 'c-num c-dur' }, 'Duration'),
        h('th', { scope: 'col', class: 'c-num c-batt' }, 'Battery'), h('th', { scope: 'col', class: 'c-num c-speed' }, 'Avg speed'),
        h('th', { scope: 'col', class: 'c-num c-eff' }, 'Efficiency'), h('th', { scope: 'col', class: 'c-num c-energy' }, 'Energy'),
        h('th', { scope: 'col', class: 'c-go' }, h('span', { class: 'sr-only' }, 'Open')))),
      [...groups.entries()].map(([k, list]) => {
        // The day's sum is the sum of its rows as printed.
        const km = shown.dist(list.map((d) => d.distanceKm)), kwh = shown.kwh(list.map((d) => d.energyKwh));
        return h('tbody', { class: 'day', id: 'drives-day-' + k },
          // The heading spans Time and From-and-to, which every width shows, and an empty cell stands in
          // each other column. (A colspan over columns a width hides would add empty columns instead.)
          // The day's sum sits at the row's right end (app.css).
          h('tr', { class: 'day-row' },
            h('th', { colspan: 2, scope: 'rowgroup' },
              h('span', { class: 'day-name' }, dayName(k, todayKey) === 'Today' || dayName(k, todayKey) === 'Yesterday' ? [dayName(k, todayKey), h('span', { class: 'day-date' }, fmt.dayLong.format(keyToMs(k)))] : fmt.dayLong.format(keyToMs(k))),
              h('span', { class: 'day-sum' }, h('span', { class: 'day-sum-text' }, `${list.length} drive${list.length === 1 ? '' : 's'}`, sep(), T.dist(km), sep(), T.kwh(kwh)))),
            cols.slice(2).map((c) => h('td', { class: 'c-' + c, 'aria-hidden': 'true' }))),
          list.map((d) => driveRow(d, carId)));
      }));
    table.addEventListener('click', (e) => {
      if (e.target.closest('a')) return;
      const tr = e.target.closest('tr[data-href]');
      if (tr) location.hash = tr.dataset.href;
    });
    listBox.replaceChildren(table);
    if (res.nextCursor) {
      const btn = h('button', { type: 'button', class: 'button' }, `Show ${Math.min(50, res.total - items.length)} earlier drives`);
      btn.addEventListener('click', () => { btn.disabled = true; btn.textContent = 'Loading…'; load(res.nextCursor); });
      more.replaceChildren(h('p', { class: 'list-more-note' }, `Showing ${n(items.length)} of ${n(res.total)} drives`), btn);
    } else {
      more.replaceChildren(h('p', { class: 'list-more-note' }, items.length > 1 ? `All ${n(items.length)} drives in this period are shown.` : 'The only drive in this period.'));
    }
  }
  await load();
}

function driveRow(d, carId) {
  const href = `#/cars/${encodeURIComponent(carId)}/drives/${encodeURIComponent(d.id)}`;
  return h('tr', { class: 'drive-row', dataset: { href, id: d.id } },
    h('td', { class: 'c-time' }, timeRange(d.start, d.end)),
    h('td', { class: 'c-route' },
      h('div', { class: 'route-cell' },
        routeSketch(d.sketch, 'sketch row-sketch'),
        h('div', null,
          h('a', { class: 'route-name', href, title: `${d.from.name} to ${d.to.name}` }, d.from.name, h('span', { class: 'to' }, ' to '), d.to.name),
          clipped('span', 'cell-sub', d.from.area === d.to.area ? d.from.area : `${shortArea(d.from.area)} to ${shortArea(d.to.area)}`)))),
    h('td', { class: 'c-num c-dist' }, Q.dist(d.distanceKm)),
    h('td', { class: 'c-num c-dur' }, dur(d.durationS)),
    h('td', { class: 'c-num c-batt', title: `${n(d.startSoc, 1)}% to ${n(d.endSoc, 1)}%` }, q(`${n(d.startSoc)}${NB}→${NB}${n(d.endSoc)}`, '%', 'q-tight')),
    h('td', { class: 'c-num c-speed' }, Q.speed(d.avgSpeedKph)),
    h('td', { class: 'c-num c-eff' }, Q.eff(d.efficiencyWhKm)),
    h('td', { class: 'c-num c-energy' }, Q.kwh(d.energyKwh, 2)),
    h('td', { class: 'c-go' }, icon('chevronRight')));
}
const shortArea = (a) => a.replace(/, San Francisco$/, '');

/** Per day, the distances of the drives listed: day -> { km: [...], n }. */
function loadedByDay(items) {
  const sums = new Map();
  for (const d of items) {
    const k = dayKeyOf(d.start);
    const t = sums.get(k) || { km: [], n: 0 };
    t.km.push(d.distanceKm); t.n++;
    sums.set(k, t);
  }
  return sums;
}

const DAY_BARS_MAX = 92; // a quarter; "All time" shows its latest 92 days

/**
 * Distance driven each day of a period, as bars: `days` is the server's
 * per-day sums for the whole period ({ day, drives, distanceKm }), not only
 * the drives listed so far. Drawn in percentages of a box app.css sizes in
 * rem. Each day with drives is a button: one tab stop for the chart, arrow
 * keys move between days (see rovingKeys), Enter goes to the day in the list.
 */
function dayBars(days, fromKey, toKey, todayKey) {
  const byKey = new Map(days.map((t) => [t.day, t]));
  const keys = [];
  for (let k = fromKey; k <= toKey && keys.length < 4000; k = addDays(k, 1)) keys.push(k);
  const shownDays = keys.slice(-DAY_BARS_MAX);
  const vals = shownDays.map((k) => (byKey.has(k) ? byKey.get(k).distanceKm : 0));
  const max = Math.max(...vals.map((v) => conv.dist(v)), 0.1);
  const nDays = shownDays.length;
  const every = Math.ceil(nDays / 10);
  const w = 100 / nDays;
  const dateOf = (k) => { const nm = dayName(k, todayKey); return nm === 'Today' || nm === 'Yesterday' ? `${nm}, ${fmt.monthDay.format(keyToMs(k))}` : nm; };
  const bars = shownDays.map((k, i) => {
    const v = conv.dist(vals[i]);
    const hPct = (v / max) * 62;
    const x = i * w;
    const t = byKey.get(k);
    const tip = `${dateOf(k)}: ${t ? `${T.dist(vals[i])}, ${t.drives} drive${t.drives === 1 ? '' : 's'}` : 'no drives'}`;
    const label = nDays <= 10 ? fmt.dayShort.format(keyToMs(k)).split(',')[0] : (i % every === 0 ? fmt.monthDay.format(keyToMs(k)) : null);
    return s('g', { class: 'by-day-bar' + (t ? '' : ' is-empty'), 'data-day': t ? k : null, role: t ? 'button' : null, 'aria-label': t ? `${tip}. Go to the day in the list.` : null },
      s('rect', { class: 'by-day-hit', x: `${x}%`, y: 0, width: `${w}%`, height: '100%' }),
      s('rect', { class: 'by-day-fill', x: `${(x + w * 0.18).toFixed(3)}%`, y: `${(78 - Math.max(hPct, 0.6)).toFixed(2)}%`, width: `${(w * 0.64).toFixed(3)}%`, height: `${Math.max(hPct, 0.6).toFixed(2)}%` }),
      t && nDays <= 14 ? s('text', { class: 'by-day-value', x: `${(x + w / 2).toFixed(3)}%`, y: `${(74 - hPct).toFixed(2)}%`, 'text-anchor': 'middle', 'aria-hidden': 'true' }, n(v, v < 10 ? 1 : 0)) : null,
      label ? s('text', { class: 'by-day-name', x: `${(x + w / 2).toFixed(3)}%`, y: '93%', 'text-anchor': 'middle', 'aria-hidden': 'true' }, label) : null,
      s('title', null, tip));
  });
  const total = vals.reduce((a, b) => a + b, 0);
  const driven = shownDays.filter((k) => byKey.has(k)).length;
  const span = `${fmt.monthDay.format(keyToMs(shownDays[0]))} – ${fmt.monthDay.format(keyToMs(shownDays[nDays - 1]))}`;
  return [
    s('svg', { class: 'by-day-svg by-day-bars', width: '100%', role: 'group', 'aria-label': `Distance by day, ${span}: ${T.dist(total)} on ${driven} of ${nDays} days. Arrow keys move between days.` },
      s('line', { class: 'by-day-base', x1: 0, x2: '100%', y1: '78%', y2: '78%' }), bars),
    h('figcaption', { class: 'by-day-note' },
      `${U().dist} per day, ${span}${nDays < keys.length ? ` (the latest ${nDays} of ${n(keys.length)} days)` : nDays > 1 ? `, all ${nDays} days of the period` : ''}. Select a day to go to it in the list.`),
  ];
}

/* ===================================================== drive detail page == */

async function drivePage(ctx) {
  const [carId, driveId] = ctx.params;
  const [info, car, d] = await Promise.all([api.serverInfo(), api.getCar(carId), api.getDrive(carId, driveId)]);
  if (!ctx.alive()) return;
  clock.set(info.now);
  const todayKey = clock.todayKey();
  const dayKey = dayKeyOf(d.start);
  const base = `#/cars/${encodeURIComponent(carId)}/drives`;

  const head = h('header', { class: 'page-head drive-head' },
    h('div', { class: 'page-head-main' },
      h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' },
        h('a', { href: base }, 'Drives'), h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/'),
        h('span', { 'aria-current': 'page' }, fmt.dayLongYear.format(d.start))),
      h('h1', { class: 'page-title' }, d.from.name, h('span', { class: 'to' }, ' to '), d.to.name),
      h('p', { class: 'deck' }, `${dayName(dayKey, todayKey)}, ${timeRange(d.start, d.end)}`, sep(), dur(d.durationS), sep(), d.from.area === d.to.area ? d.from.area : `${shortArea(d.from.area)} to ${d.to.area}`)),
    h('nav', { class: 'page-head-aside step-nav', 'aria-label': 'Other drives' },
      d.prevId ? h('a', { class: 'button button-quiet', href: `${base}/${encodeURIComponent(d.prevId)}` }, icon('chevronLeft'), 'Earlier') : h('span', { class: 'button button-quiet is-disabled', 'aria-disabled': 'true' }, icon('chevronLeft'), 'Earlier'),
      d.nextId ? h('a', { class: 'button button-quiet', href: `${base}/${encodeURIComponent(d.nextId)}` }, 'Later', icon('chevronRight')) : h('span', { class: 'button button-quiet is-disabled', 'aria-disabled': 'true' }, 'Later', icon('chevronRight'))));

  // On wide screens the map takes the route's shape (app.css), so an east-west drive gets a wide map.
  const mapBox = h('div', { class: 'map map-drive is-loading', role: 'region', 'aria-label': `Map of the route from ${d.from.name} to ${d.to.name}`, dataset: { shape: routeShape(d.route) } });
  const stats = h('dl', { class: 'drive-stats' },
    figure('Distance', Q.dist(d.distanceKm)),
    figure('Duration', q(dur(d.durationS))),
    figure('Average speed', Q.speed(d.avgSpeedKph)),
    figure('Top speed', Q.speed(d.maxSpeedKph)),
    figure('Energy used', Q.kwh(d.energyKwh, 2)),
    figure('Efficiency', Q.eff(d.efficiencyWhKm)),
    figure('Outside', Q.temp(d.outsideC)),
    figure('Cabin', Q.temp(d.cabinC)),
    // To one decimal, and the points used is the difference of the two figures as printed.
    figure('Battery', q(`${n(d.startSoc, 1)} → ${n(d.endSoc, 1)}`, '%', 'q-tight'), `${n(+d.startSoc.toFixed(1) - +d.endSoc.toFixed(1), 1)} points used`),
    figure('Range', q(`${n(conv.dist(d.startRangeKm))} → ${n(conv.dist(d.endRangeKm))}`, U().dist), `odometer ${T.dist(d.startOdometerKm, 0)}`));

  const readout = h('p', { class: 'readout', 'aria-live': 'off' });
  const resetBtn = h('button', { type: 'button', class: 'button button-quiet', disabled: true }, icon('reset'), 'Show the whole drive');
  const charts = h('div', { class: 'charts' });
  const chartsSection = h('section', { class: 'along', 'aria-labelledby': 'along-title' },
    h('div', { class: 'along-head' },
      h('h2', { class: 'label', id: 'along-title' }, 'Along the drive'),
      readout,
      resetBtn),
    charts,
    h('p', { class: 'along-hint' }, 'Point at a chart to read every value at that moment and see where the car was. Drag across a chart to zoom in; double-click to zoom out.'));

  // On wide screens the map, the charts and the figures stand side by side (app.css).
  ctx.main.replaceChildren(head, h('div', { class: 'drive-body' },
    h('div', { class: 'drive-top' }, mapBox, h('section', { class: 'drive-side', 'aria-label': 'Drive figures' }, stats)), chartsSection));

  const ser = d.series;
  const showReadout = (i) => {
    if (i == null) {
      readout.replaceChildren(h('span', { class: 'readout-idle' }, `${n(ser.t.length)} samples, ${clockTime(d.start)} to ${clockTime(d.end)}`));
      return;
    }
    readout.replaceChildren(
      h('time', { class: 'readout-time' }, fmt.timeSec.format(ser.t[i])),
      h('span', null, h('b', null, n(conv.speed(ser.speedKph[i]))), ` ${U().speed}`),
      h('span', null, h('b', null, n(ser.powerKw[i], 1)), ' kW'),
      h('span', null, h('b', null, n(ser.soc[i], 1)), ' %'),
      h('span', null, h('b', null, n(conv.elev(ser.elevationM[i]))), ` ${U().elev}`));
  };
  showReadout(null);

  // Map first, then charts share its hover marker.
  let marker = null, mapRef = null;
  const m = await makeMap(mapBox, ctx, { bounds: boundsOf(d.route), padding: 56, maxZoom: d.routeDetail === 'sketch' ? 12.5 : 17, coordText: coordsText(d.route[0]) });
  if (!m || !ctx.alive()) return;
  mapRef = m;
  await m.loaded;
  if (!ctx.alive()) return;
  addRoute(m, 'route', d.route);
  m.map.addSource('sel', { type: 'geojson', data: lineFeature([]) });
  m.addLine({ id: 'sel-line', source: 'sel', layout: ROUND, paint: { 'line-color': cssVar('--accent-strong') } }, 6.5);
  addPin(m, d.route[0], d.from.name, 'pin-start');
  addPin(m, d.route[d.route.length - 1], d.to.name, 'pin-end');
  const markerEl = h('div', { class: 'car-dot is-hidden' });
  marker = new m.ml.Marker({ element: markerEl, anchor: 'center' }).setLngLat(d.route[0]).addTo(m.map);

  const onHover = (i) => {
    showReadout(i);
    if (i == null) { markerEl.classList.add('is-hidden'); return; }
    markerEl.classList.remove('is-hidden');
    marker.setLngLat([ser.lon[i], ser.lat[i]]);
  };
  const xs = ser.t.map((t) => t / 1000);
  const onZoom = (min, max) => {
    const full = min <= xs[0] + 0.5 && max >= xs[xs.length - 1] - 0.5;
    resetBtn.disabled = full;
    const coords = [];
    if (!full) for (let i = 0; i < xs.length; i++) if (xs[i] >= min && xs[i] <= max) coords.push([ser.lon[i], ser.lat[i]]);
    const src = mapRef.map.getSource('sel');
    if (src) src.setData(lineFeature(coords.length > 1 ? coords : []));
  };
  const plotted = await driveCharts(charts, d, ctx, { onHover, onZoom });
  if (plotted) resetBtn.addEventListener('click', plotted.resetZoom);
  // Side by side, the map is as tall as the charts beside it: let it draw at its final size.
  ctx.waitFor(m.settled());
}

function uplotReady() {
  if (window.uPlot) return Promise.resolve(window.uPlot);
  return new Promise((resolve) => window.addEventListener('load', () => resolve(window.uPlot), { once: true }));
}

const fmtHMS = new Intl.DateTimeFormat('en-US', { timeZone: TZ, hour: 'numeric', minute: '2-digit', second: '2-digit' });
/** "4:52" or "4:52:30", and its AM/PM, for chart ticks. */
function tickParts(ms, withSeconds) {
  const p = (withSeconds ? fmtHMS : fmt.time).formatToParts(ms);
  const get = (t) => (p.find((x) => x.type === t) || {}).value;
  return { text: withSeconds ? `${get('hour')}:${get('minute')}:${get('second')}` : `${get('hour')}:${get('minute')}`, ap: get('dayPeriod') };
}

/**
 * The four charts under a drive, drawn with uPlot on canvases.
 *
 * Sizes: each chart fills a box that app.css sizes in rem, and is resized
 * whenever its box is (window, layout, interface size). Pixel ratio: uPlot
 * draws in device pixels and follows the ratio by itself (it listens to a
 * resolution media query and redraws), so nothing here has to. Fonts, axis
 * room and line widths are given in px at the standard size and scaled with
 * the interface size; when that changes the charts are built again, keeping
 * the part of the drive that is zoomed in.
 */
async function driveCharts(container, d, ctx, { onHover, onZoom }) {
  const uPlot = await uplotReady();
  await document.fonts.ready;
  if (!ctx.alive()) return null;
  const col = (v) => cssVar(v);
  const C = { ink: col('--ink'), ink2: col('--ink-2'), ink3: col('--ink-3'), rule: col('--rule'), strong: col('--rule-strong'), accent: col('--accent'), wash: col('--chart-wash'), sunk: col('--chart-sunk') };
  const ser = d.series;
  const x = ser.t.map((t) => t / 1000);
  // Power: use above zero, regeneration below. Ochre is kept for charging, so use is a neutral ink wash.
  const defs = [
    { key: 'speed', name: 'Speed', unit: U().speed, data: ser.speedKph.map(conv.speed), stroke: C.accent, fill: C.wash, digits: 0, floor: 0, ticks: 5 },
    { key: 'power', name: 'Power', unit: 'kW', note: 'use above zero, regen below', data: ser.powerKw, stroke: C.ink2, digits: 0, ticks: 5 },
    { key: 'soc', name: 'Battery', unit: '%', data: ser.soc, stroke: C.ink, digits: 1, ticks: 4 },
    { key: 'elev', name: 'Elevation', unit: U().elev, data: ser.elevationM.map(conv.elev), stroke: C.ink3, fill: C.sunk, digits: 0, floor: 0, ticks: 5 },
  ];
  const rows = defs.map((def) => {
    const box = h('div', { class: 'chart-box' });
    container.append(h('div', { class: 'chart-row chart-' + def.key },
      h('div', { class: 'chart-label' }, h('span', { class: 'chart-name' }, def.name), h('span', { class: 'chart-unit' }, def.unit), def.note ? h('span', { class: 'chart-note' }, def.note) : null),
      h('div', { class: 'chart-plot' }, box)));
    return { def, box, plot: null, ...chartData(def, x) };
  });
  const syncKey = uPlot.sync('drive-' + d.id + '-' + Math.random().toString(36).slice(2));
  const plots = [];
  let syncing = false;

  function options(row, last, k) {
    const { def, yRange, lo, hi } = row;
    const px = (v) => Math.round(v * k * 10) / 10;
    // uPlot scales the first number in the font string by the pixel ratio, so the size comes first.
    // Never under 10px, like the page's other small print (app.css --small).
    const font = `${Math.max(10, px(12))}px "Logbook Sans", Inter, system-ui, sans-serif`;
    const series = [{}];
    if (def.key === 'power') {
      series.push(
        { stroke: 'rgba(0,0,0,0)', width: 0, fill: C.sunk, points: { show: false } },
        { stroke: 'rgba(0,0,0,0)', width: 0, fill: C.wash, points: { show: false } },
        { stroke: def.stroke, width: 1.25 * k, points: { show: false } });
    } else {
      series.push({ stroke: def.stroke, width: 1.6 * k, fill: def.fill, points: { show: false } });
    }
    return {
      width: Math.max(1, row.box.clientWidth), height: Math.max(1, row.box.clientHeight),
      // Room above and below so the top and bottom tick labels are not clipped.
      padding: [px(10), px(28), last ? 0 : px(8), px(2)],
      legend: { show: false },
      tzDate: (ts) => uPlot.tzDate(new Date(ts * 1000), TZ),
      cursor: { sync: { key: syncKey.key, setSeries: false }, drag: { x: true, y: false, setScale: true }, points: { show: false }, y: false },
      select: { show: true },
      scales: { x: { time: true }, y: { range: () => yRange } },
      axes: [
        {
          stroke: C.ink3, font, size: last ? px(30) : px(2), gap: px(6),
          // Ticks every 15 s to an hour, at least 80 px apart (at the standard size); seconds appear when ticks are under a minute apart.
          space: px(80), incrs: [15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200],
          ticks: { show: last, stroke: C.strong, width: 1, size: px(4) },
          grid: { stroke: C.rule, width: 1 },
          values: last ? (u, splits) => {
            const withSeconds = splits.length > 1 && splits[1] - splits[0] < 60;
            let prevAp = null;
            // AM/PM only on the first label and wherever it changes.
            return splits.map((v) => { const p = tickParts(v * 1000, withSeconds); const out = p.ap !== prevAp ? `${p.text} ${p.ap}` : p.text; prevAp = p.ap; return out; });
          } : (u, splits) => splits.map(() => ''),
        },
        {
          stroke: C.ink3, font, size: px(48), gap: px(8), ticks: { show: false }, grid: { stroke: C.rule, width: 1 },
          splits: (u, ax, min, max) => niceSplits(min, max, def.ticks),
          values: (u, splits) => splits.map((v) => n(v, def.key === 'soc' && hi - lo < 4 ? 1 : 0)),
        },
      ],
      series,
      hooks: {
        setCursor: row === rows[0] ? [(u) => onHover(u.cursor.idx == null || u.cursor.left < 0 ? null : u.cursor.idx)] : [],
        setScale: [(u, key) => {
          if (key !== 'x' || syncing) return;
          syncing = true;
          const { min, max } = u.scales.x;
          for (const p of plots) if (p !== u) p.setScale('x', { min, max });
          syncing = false;
          onZoom(min, max);
        }],
        // Power's zero line is the baseline between use and regeneration. uPlot draws in device pixels at
        // uPlot.pxRatio: a whole number of device pixels wide (about 1 CSS px), centred so it covers whole
        // pixel rows, is sharp at any ratio.
        draw: def.key === 'power' ? [(u) => {
          const lw = Math.max(1, Math.round(uPlot.pxRatio));
          const y0 = Math.round(u.valToPos(0, 'y', true)) + (lw % 2) / 2;
          const c = u.ctx;
          c.save(); c.strokeStyle = C.ink3; c.lineWidth = lw; c.beginPath();
          c.moveTo(u.bbox.left, y0); c.lineTo(u.bbox.left + u.bbox.width, y0); c.stroke(); c.restore();
        }] : [],
      },
    };
  }

  function build() {
    const shown = plots.length ? { min: plots[0].scales.x.min, max: plots[0].scales.x.max } : null;
    for (const p of plots.splice(0)) p.destroy();
    const k = display.scale;
    rows.forEach((row, i) => {
      row.plot = new uPlot(options(row, i === rows.length - 1, k), row.data, row.box);
      plots.push(row.plot);
    });
    if (shown) plots[0].setScale('x', shown);
  }
  build();

  const ro = new ResizeObserver(() => {
    for (const row of rows) {
      const w = row.box.clientWidth, hh = row.box.clientHeight;
      if (w && hh && (Math.abs(w - row.plot.width) > 1 || Math.abs(hh - row.plot.height) > 1)) row.plot.setSize({ width: w, height: hh });
    }
  });
  rows.forEach((row) => ro.observe(row.box));
  // A new root size: fonts, axis room and line widths change, so the charts are built again.
  const stopWatching = display.onChange((change) => { if (change.rem) build(); });
  ctx.onCleanup(() => { stopWatching(); ro.disconnect(); plots.forEach((p) => p.destroy()); });
  return {
    resetZoom: () => { for (const p of plots) p.setScale('x', { min: x[0], max: x[x.length - 1] }); },
  };
}

/** A chart's series and its y range: whole steps at both ends, so the top and bottom ticks are labelled. */
function chartData(def, x) {
  const vals = def.data;
  let lo = Math.min(...vals), hi = Math.max(...vals);
  let yRange;
  if (def.key === 'soc') {
    const pad = (hi - lo) * 0.12 || 1;
    yRange = [lo - pad, hi + pad];
  } else {
    // Power always shows a step below zero.
    if (def.key === 'power') lo = Math.min(lo, -0.01);
    else if (lo >= 0 && def.floor != null && (def.key === 'speed' || lo < (hi - lo))) lo = def.floor;
    const step = niceStep(hi - lo, def.ticks);
    yRange = [Math.floor(lo / step) * step, Math.ceil(hi / step) * step];
  }
  const data = def.key === 'power'
    ? [x, vals.map((v) => Math.max(0, v)), vals.map((v) => Math.min(0, v)), vals]
    : [x, vals];
  return { data, yRange, lo, hi };
}

function niceStep(span, count) {
  if (!(span > 0)) return 1;
  const raw = span / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  return [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s_) => s_ >= raw - 1e-12) || raw;
}
function niceSplits(min, max, count) {
  const span = max - min;
  if (!(span > 0)) return [min];
  const step = niceStep(span, count);
  const out = [];
  for (let v = Math.ceil(min / step - 1e-9) * step; v <= max + 1e-9; v += step) out.push(Math.round(v / step) * step);
  return out;
}

/* ========================================================= timeline page == */

/** A day's figures, each a sum of the entries as they are printed. */
function dayFigures(events) {
  const dr = events.filter((e) => e.type === 'drive'), ch = events.filter((e) => e.type === 'charge');
  const asleep = [];
  for (const e of events) {
    if (e.type === 'asleep') asleep.push(((e.end == null ? clock.now() : e.end) - e.start) / 1000);
    if (e.type === 'parked' && e.asleepS > 20 * 60) asleep.push(e.asleepS);
  }
  return {
    drives: dr.length,
    distanceKm: shown.dist(dr.map((e) => e.drive.distanceKm)),
    energyKwh: shown.kwh(dr.map((e) => e.drive.energyKwh)),
    chargedKwh: shown.kwh(ch.map((e) => e.charge.energyAddedKwh), 1),
    asleepS: shown.minutes(asleep),
  };
}

async function timelinePage(ctx) {
  const carId = ctx.carId;
  const [info, car] = await Promise.all([api.serverInfo(), api.getCar(carId)]);
  if (!ctx.alive()) return;
  clock.set(info.now);
  const todayKey = clock.todayKey();
  let day = /^\d{4}-\d{2}-\d{2}$/.test(ctx.query.get('day') || '') ? ctx.query.get('day') : todayKey;
  if (day > todayKey) day = todayKey;
  const tl = await api.getTimeline(carId, day);
  if (!ctx.alive()) return;
  const base = `#/cars/${encodeURIComponent(carId)}/timeline`;
  const prevDay = addDays(day, -1), nextDay = addDays(day, 1);
  const canPrev = prevDay >= tl.earliestDay, canNext = day < todayKey;
  const sm = dayFigures(tl.events);
  const sentence = [];
  if (sm.drives) sentence.push(`${sm.drives} drive${sm.drives === 1 ? '' : 's'}, ${T.dist(sm.distanceKm)} in all.`); else sentence.push('No drives.');
  if (sm.asleepS > 3600) sentence.push(`Asleep for ${dur(sm.asleepS)}.`);
  if (sm.chargedKwh) sentence.push(`Charged ${T.kwh(sm.chargedKwh)}.`);
  if (tl.lastReport != null && tl.lastReport < tl.end) sentence.push(`Nothing heard since ${whenShort(tl.lastReport)}.`);

  // Day buttons: the same two-line form on both sides, the date underneath.
  const dayButton = (dir, key, enabled) => {
    const kids = [dir < 0 ? icon('chevronLeft') : null,
      h('span', { class: 'day-button-text' }, h('span', { class: 'day-button-name' }, dir < 0 ? 'Previous day' : 'Next day'), h('span', { class: 'day-button-date' }, fmt.dayShort.format(keyToMs(key)))),
      dir > 0 ? icon('chevronRight') : null];
    const href = key === todayKey ? base : `${base}?day=${key}`;
    return enabled ? h('a', { class: 'button button-quiet button-day', href }, kids) : h('span', { class: 'button button-quiet button-day is-disabled', 'aria-disabled': 'true' }, kids);
  };
  const dayNav = h('nav', { class: 'page-head-aside step-nav', 'aria-label': 'Other days' },
    dayButton(-1, prevDay, canPrev),
    day !== todayKey ? h('a', { class: 'button button-quiet button-day button-today', href: base }, 'Today') : null,
    dayButton(1, nextDay, canNext));

  const head = h('header', { class: 'page-head' },
    h('div', { class: 'page-head-main' },
      h('p', { class: 'eyebrow' }, `${car.name} · Timeline`),
      h('h1', { class: 'page-title' }, day === todayKey ? ['Today, ', h('span', { class: 'title-soft' }, fmt.dayLong.format(keyToMs(day)))] : fmt.dayLongYear.format(keyToMs(day))),
      h('p', { class: 'deck' }, sentence.join(' '))),
    dayNav);

  const mapBox = h('div', { class: 'map map-day is-loading', role: 'region', 'aria-label': 'Map of the day’s drives' });
  const side = h('aside', { class: 'day-side', 'aria-label': 'The day at a glance' },
    h('h2', { class: 'label' }, 'The day’s roads'),
    mapBox,
    h('dl', { class: 'today-figs' },
      figure('Drives', q(n(sm.drives))), figure('Distance', Q.dist(sm.distanceKm)),
      figure('Energy used', Q.kwh(sm.energyKwh)), figure('Charged', sm.chargedKwh ? plus(Q.kwh(sm.chargedKwh)) : q('—'))));

  ctx.main.replaceChildren(head, dayStrip(tl), h('div', { class: 'timeline-grid' },
    h('section', { class: 'timeline-main', 'aria-label': 'Events of the day' }, timelineList(tl.events, carId, { full: true })), side));

  const drives = tl.events.filter((e) => e.type === 'drive');
  const all = drives.flatMap((e) => e.drive.sketch);
  const coarse = drives.some((e) => e.drive.routeDetail === 'sketch');
  // A day without drives shows where the car spent it.
  const stay = tl.events.find((e) => e.place && e.place.lat != null);
  const m = await makeMap(mapBox, ctx, all.length ? { bounds: boundsOf(all), padding: 28, maxZoom: coarse ? 12.5 : 16 }
    : { center: stay ? [stay.place.lon, stay.place.lat] : [car.cityLon ?? -122.43, car.cityLat ?? 37.76], zoom: stay ? 13.5 : 11 });
  if (!m || !ctx.alive()) return;
  await m.loaded;
  if (!all.length && stay) addPin(m, [stay.place.lon, stay.place.lat], stay.place.name, 'pin-here');
  drives.forEach((e, i) => addRoute(m, 'd' + i, e.drive.sketch));
  const pinned = new Set();
  for (const e of drives) {
    for (const [p, pt] of [[e.drive.from, e.drive.sketch[0]], [e.drive.to, e.drive.sketch[e.drive.sketch.length - 1]]]) {
      if (p && !pinned.has(p.name)) { pinned.add(p.name); addPin(m, pt, p.name, 'pin-stop'); }
    }
  }
}

/** The 24 hours of a day as one ruled strip, coloured by what the car was doing. */
function dayStrip(tl) {
  const d0 = tl.start, span = 24 * 3600e3;
  const nowMs = clock.now();
  const pastEnd = tl.isToday ? Math.min(nowMs, d0 + span) : d0 + span;
  const x = (t) => ((Math.max(d0, Math.min(d0 + span, t)) - d0) / span) * 100;
  const NAMES = { drive: 'Driving', charge: 'Charging', parked: 'Parked, awake', asleep: 'Asleep', nodata: 'No data' };
  // Drawn in percentages of its box, which app.css sizes in rem: the strip scales with the text and the window.
  const rect = (kind, a, b, end) => s('rect', { class: 'strip-seg strip-' + kind, x: `${x(a).toFixed(3)}%`, y: '18.5%', width: `${Math.max(0.12, x(b) - x(a)).toFixed(3)}%`, height: '33.3%' },
    s('title', null, `${NAMES[kind]}, ${timeRange(a, end)}`));
  const segs = [];
  const kinds = new Set();
  for (const e of tl.events) {
    const end = e.end == null ? nowMs : e.end;
    const kind = e.type === 'parked' ? 'parked' : e.type;
    kinds.add(kind);
    segs.push(rect(kind, e.start, end, e.end));
    // Sleep inside a parked stretch, where the data says it slept.
    for (const z of e.asleep || []) { kinds.add('asleep'); segs.push(rect('asleep', z.from, z.to == null ? end : z.to, z.to)); }
  }
  const hasFuture = pastEnd < d0 + span - 60000;
  // Every hour is marked; app.css names only every third one unless the strip is wide enough for all.
  const hours = Array.from({ length: 25 }, (_, i) => i);
  const hourName = (hh) => (hh === 0 || hh === 24 ? 'midnight' : hh === 12 ? 'noon' : `${hh % 12} ${hh < 12 ? 'am' : 'pm'}`);
  const keys = [['drive', 'Driving'], ['charge', 'Charging'], ['parked', 'Parked, awake'], ['asleep', 'Asleep']];
  if (kinds.has('nodata')) keys.push(['nodata', 'No data']);
  if (hasFuture) keys.push(['future', 'Not yet']);
  return h('figure', { class: 'strip' },
    s('svg', { class: 'strip-svg', width: '100%', role: 'img', 'aria-label': 'The day hour by hour, coloured by what the car was doing.' },
      segs,
      hasFuture ? s('rect', { class: 'strip-future', x: `${x(pastEnd).toFixed(3)}%`, y: '19.4%', width: `${(100 - x(pastEnd)).toFixed(3)}%`, height: '31.5%' }) : null,
      hours.map((hh) => {
        const minor = hh % 3 ? ' is-minor' : '';
        return [
          s('line', { class: 'strip-tick' + minor, x1: `${(hh / 24) * 100}%`, x2: `${(hh / 24) * 100}%`, y1: '55.6%', y2: hh % 3 ? '61%' : '64.8%' }),
          s('text', { class: 'strip-hour' + minor, x: `${(hh / 24) * 100}%`, y: '90.7%', 'text-anchor': hh === 0 ? 'start' : hh === 24 ? 'end' : 'middle' }, hourName(hh)),
        ];
      }),
      tl.isToday ? s('line', { class: 'strip-now', x1: `${x(nowMs)}%`, x2: `${x(nowMs)}%`, y1: '7.4%', y2: '59.3%' }) : null),
    h('figcaption', { class: 'strip-key' },
      keys.map(([k, t]) => h('span', { class: 'key' }, h('span', { class: 'key-swatch strip-' + k, 'aria-hidden': 'true' }), t))));
}

/* ======================================================= placeholder pages == */

const PLACEHOLDERS = {
  charges: {
    title: 'Charges', deck: (car) => `Every time ${car.name} was plugged in: where, how fast, how much energy went in and what it cost.`,
    items: [
      ['Sessions', 'A list like Drives, by day: place, start and end battery, energy added, peak power, AC or DC.'],
      ['Charge curve', 'Power against battery level for each session, to see where it tapered and how hot the pack ran.'],
      ['Cost', 'Energy prices per place, so each session and each month has a figure in dollars.'],
      ['Places', 'Home, work and fast chargers ranked by energy and time spent.'],
    ],
  },
  battery: {
    title: 'Battery', deck: () => 'The long view of the pack: how much range a full charge gives today compared with when the car was new.',
    items: [
      ['Capacity over time', 'Rated range at 100% for every charge that reached the limit, with a fitted trend line.'],
      ['Overnight losses', 'How much charge the car loses while parked, by night and by temperature.'],
      ['Temperatures', 'Pack temperature while driving, charging and parked.'],
      ['12 V battery', 'Low-voltage battery readings, with a note when it starts to sag.'],
    ],
  },
  stats: {
    title: 'Stats', deck: () => 'Months and years at a glance: distance, energy, efficiency and time on the road.',
    items: [
      ['By month', 'Distance, drives, energy and efficiency for each month, side by side.'],
      ['Efficiency and weather', 'Wh per mile against outside temperature, to see what winter costs.'],
      ['Where the time goes', 'Hours driving, charging, parked and asleep.'],
      ['Places', 'The places the car visits most and how long it stays.'],
    ],
  },
  signals: {
    title: 'Signals', deck: (car) => `The raw values decoded from ${car.name}'s CAN bus, as they arrive.`,
    items: [
      ['Signal browser', 'Search every decoded signal by name or message, with its latest value and unit.'],
      ['Live values', 'Pin a few signals and watch them update about once a second.'],
      ['History', 'Chart any signal over a drive or a day.'],
      ['Buses', 'Which bus each message came from and how often it is seen.'],
    ],
  },
  logs: {
    title: 'Logs', deck: () => 'The CAN logs the car has sent, with their signatures checked and gaps marked.',
    items: [
      ['Uploads', 'Each log file by time, size, format and the bus it covers.'],
      ['Integrity', 'Signature and hash-chain status for every file; a missing link shows as a gap.'],
      ['Download', 'Fetch a log as candump text for your own tools.'],
      ['Retention', 'How long logs are kept and how much space they use.'],
    ],
  },
};

async function placeholderPage(ctx) {
  const [carId, section] = ctx.params;
  const car = await api.getCar(carId);
  if (!ctx.alive()) return;
  const p = PLACEHOLDERS[section];
  ctx.main.replaceChildren(
    h('header', { class: 'page-head' },
      h('div', { class: 'page-head-main' },
        h('p', { class: 'eyebrow' }, car.name),
        h('h1', { class: 'page-title' }, p.title),
        h('p', { class: 'deck' }, p.deck(car)))),
    plannedList(p.items, `${p.title} is not part of this preview yet. This is what the page will hold.`));
}

function plannedList(items, note) {
  return h('section', { class: 'planned', 'aria-labelledby': 'planned-title' },
    h('div', { class: 'planned-note' }, h('h2', { class: 'label', id: 'planned-title' }, 'Coming to this page'), h('p', null, note)),
    h('ol', { class: 'planned-list' }, items.map(([t, d], i) => h('li', { class: 'planned-item' },
      h('span', { class: 'planned-no', 'aria-hidden': 'true' }, String(i + 1).padStart(2, '0')),
      h('div', null, h('h3', { class: 'planned-title' }, t), h('p', { class: 'planned-desc' }, d))))));
}

async function settingsPage(ctx) {
  const info = await api.serverInfo();
  if (!ctx.alive()) return;
  const unitsGroup = h('div', { class: 'segmented', role: 'radiogroup', 'aria-label': 'Units' },
    [['us', 'Miles, °F, psi'], ['metric', 'Kilometres, °C, bar']].map(([k, t]) => h('button', {
      type: 'button', role: 'radio', class: 'segmented-option', 'aria-checked': String(prefs.units === k), dataset: { unitsChoice: k },
      onclick: () => {
        if (prefs.units === k) return;
        prefs.units = k; store.set('units', k);
        document.querySelectorAll('.units-option').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.units === k)));
        refocusAfterRender = `[data-units-choice="${k}"]`;
        render();
      },
    }, t)));
  const themeGroup = h('div', { class: 'segmented', role: 'radiogroup', 'aria-label': 'Appearance' },
    [['system', 'System'], ['light', 'Paper'], ['dark', 'Night ink']].map(([k, t]) => h('button', {
      type: 'button', role: 'radio', class: 'segmented-option', 'aria-checked': String(prefs.theme === k), dataset: { themeChoice: k },
      onclick: () => { if (prefs.theme !== k) refocusAfterRender = `[data-theme-choice="${k}"]`; setTheme(k); },
    }, t)));
  radioKeys(unitsGroup);
  radioKeys(themeGroup);
  // Interface size: the same choice as under "Aa" in the top bar, saved with the person's account.
  const sizeGroup = h('div', { class: 'segmented segmented-sizes', role: 'radiogroup', 'aria-label': 'Interface size' });
  ctx.onCleanup(sizeRadios(sizeGroup));
  const sizeNow = h('p', { class: 'setting-note', 'aria-live': 'polite' }, sizeNote());
  const syncNote = () => { sizeNow.textContent = sizeNote(); };
  sizeSyncs.add(syncNote);
  ctx.onCleanup(() => sizeSyncs.delete(syncNote));
  ctx.onCleanup(display.onChange(syncNote));
  ctx.main.replaceChildren(
    h('header', { class: 'page-head' },
      h('div', { class: 'page-head-main' },
        h('p', { class: 'eyebrow' }, 'Sentry Mate'),
        h('h1', { class: 'page-title' }, 'Settings'),
        h('p', { class: 'deck' }, 'How this UI shows things to you. Cars, people and storage will be managed here too.'))),
    h('div', { class: 'settings-page' },
      h('section', { class: 'settings' },
        h('div', { class: 'setting' }, h('div', null, h('h2', { class: 'setting-name' }, 'Units'), h('p', { class: 'setting-desc' }, 'Used on every page. Also in the top bar.')), unitsGroup),
        h('div', { class: 'setting' }, h('div', null, h('h2', { class: 'setting-name' }, 'Time zone'), h('p', { class: 'setting-desc' }, 'All times are shown in this zone, whatever the car’s location.')), h('p', { class: 'setting-value' }, info.timeZone.replace('_', ' '), h('span', { class: 'cell-sub' }, 'Central Time'))),
        h('div', { class: 'setting' }, h('div', null, h('h2', { class: 'setting-name' }, 'Appearance'), h('p', { class: 'setting-desc' }, 'Paper by day and night ink after dark, following your system, or always one of the two.')), themeGroup),
        h('div', { class: 'setting setting-size' },
          h('div', null,
            h('h2', { class: 'setting-name' }, 'Interface size'),
            h('p', { class: 'setting-desc' }, 'Text, controls, tables, charts and maps grow or shrink together. Auto suits the screen: the standard size on laptops and most monitors, larger on very wide windows such as a 4K screen at 100% scaling. A percentage is that share of the standard size on any screen. Browser zoom works on top of either. Also under “Aa” in the top bar.'),
            sizeNow),
          sizeGroup)),
      plannedList([
      ['Cars', 'Add a car, pair its Sentry Manager, rename it, or retire it from the register.'],
      ['People', 'Who can see which cars, and who can change settings.'],
      ['UI packs', 'Install, switch and remove packs like this one.'],
      ['Storage', 'How long drives, signals and raw logs are kept.'],
    ], 'Only the display settings above work in this preview.')));
}

/* ================================================================= theme == */

function applyTheme() {
  if (prefs.theme === 'system') delete root.dataset.theme; else root.dataset.theme = prefs.theme;
}
/** Switch between paper and night ink. Maps and charts take their colours when drawn, so the page is drawn again. */
function setTheme(theme) {
  if (theme === prefs.theme) return;
  prefs.theme = theme;
  store.set('theme', theme);
  applyTheme();
  const y = window.scrollY;
  render().then(() => window.scrollTo(0, y));
}

/* ================================================================== init == */

function init() {
  applyTheme();
  applyUiSize(prefs.uiSize);
  setupUnits();
  setupSizeMenu();
  // The size saved with the person's account wins over this browser's copy, unless they have just picked one.
  api.getPrefs().then((p) => { if (!sizePicked && p.uiSize !== prefs.uiSize) applyUiSize(p.uiSize); }).catch(() => {});
  // A car remembered from an earlier visit may since have gone.
  carInfo(prefs.lastCar).then((c) => { if (!c) { prefs.lastCar = 'model-y'; store.set('lastCar', 'model-y'); } });
  window.addEventListener('hashchange', render);
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => { if (prefs.theme === 'system') render(); });
  api.serverInfo().then((i) => { clock.set(i.now); tick(); });
  tick();
  setInterval(tick, 15000);
  if (!location.hash || location.hash === '#' || location.hash === '#/') history.replaceState(null, '', '#/fleet');
  render();
}

init();
