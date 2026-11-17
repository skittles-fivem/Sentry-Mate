// Sentry Mate · "Instrument" UI pack.
// Plain JavaScript module, no build step. All data comes from ./mock-api.js, which has the same
// shapes as the server's /api/v1: replace that one file to use the real API.

import * as rawApi from './mock-api.js';

// Every call to the server notes when it last answered, so the top bar can say honestly how fresh
// the page is. A 404 is still an answer.
const net = { lastOk: Date.now(), failing: false };
const api = Object.fromEntries(Object.entries(rawApi).map(([k, v]) => [k,
  typeof v === 'function' && v.constructor && v.constructor.name === 'AsyncFunction'
    ? (...args) => v(...args).then(
      (res) => { net.lastOk = Date.now(); net.failing = false; return res; },
      (err) => { if (err && err.status) net.lastOk = Date.now(); else net.failing = true; throw err; })
    : v]));

const TZ = api.TIME_ZONE || 'America/Chicago';
const KM_MI = 1.609344;
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || '');
const SEP = ' · ';

// ------------------------------------------------------------------ small helpers
const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const icon = (id, cls = '') => `<svg class="ico ${cls}" aria-hidden="true" focusable="false"><use href="#i-${id}"/></svg>`;
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));

// Per-viewer conveniences only (units, recent cars). The page works without storage.
const store = {
  get(k, d) { try { const v = localStorage.getItem('sentrymate.' + k); return v == null ? d : JSON.parse(v); } catch { return d; } },
  set(k, v) { try { localStorage.setItem('sentrymate.' + k, JSON.stringify(v)); } catch { /* storage blocked */ } },
};

// CSP-safe dynamic sizes: templates carry data-w / data-x / data-y / data-n / data-a; this turns them into CSS variables.
function applyVars(root) {
  for (const el of root.querySelectorAll('[data-w],[data-x],[data-y],[data-n],[data-a]')) {
    if (el.dataset.w != null) el.style.setProperty('--w', el.dataset.w + '%');
    if (el.dataset.x != null) el.style.setProperty('--x', el.dataset.x + '%');
    if (el.dataset.y != null) el.style.setProperty('--y', el.dataset.y + '%');
    if (el.dataset.a != null) el.style.setProperty('--a', el.dataset.a + '%');
    if (el.dataset.n != null) el.style.setProperty('--n', el.dataset.n);
  }
}
function html(el, markup) { el.innerHTML = markup; applyVars(el); return el; }
// theme tokens live in app.css; charts and maps read them so a pack's colours follow the theme
const cssVar = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
function hexA(hex, a) {
  const h = hex.replace('#', '');
  const n = parseInt(h.length === 3 ? h.split('').map((c) => c + c).join('') : h, 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
}
function hl(text, q) {
  const t = esc(text);
  const words = String(q || '').toLowerCase().split(/\s+/).filter(Boolean);
  let out = t;
  for (const w of words) {
    const i = out.toLowerCase().indexOf(esc(w));
    if (i >= 0 && !out.slice(0, i).includes('<mark')) out = `${out.slice(0, i)}<mark>${out.slice(i, i + esc(w).length)}</mark>${out.slice(i + esc(w).length)}`;
  }
  return out;
}

// ------------------------------------------------------------------ display: interface size, root size, pixel ratio
// The root font size is set in one place (app.css "root size"): browser default x Auto factor x Interface
// size. boot.js (window.uiRoot) owns the two factors and applies them before the first paint; every other
// size is in rem, so layout follows by itself. Canvases and maps are pixels, so this module tells them
// when to redraw:
// - the pixel ratio changed: a window moved to another monitor, or browser zoom. Browsers report this
//   with the "(resolution: ...)" media query. DevTools/CDP device emulation (and some embedded browsers)
//   change devicePixelRatio without any event, so until the browser has fired that query once, a cheap
//   comparison every half second stands in for it, and then stops;
// - the Interface size changed (Settings, account menu, another device);
// - the root size changed for another reason (zoom, the window moved to a screen with another Auto factor).
// Window resizes do not change the root size: Auto follows the screen, not the window.
// Subscribers get one call per frame, after the new layout exists, with what changed.
const ui = window.uiRoot || { SIZES: ['auto', 75, 90, 100, 110, 125, 150, 175, 200], autoFactor: () => 1, apply: (s) => s };

// uPlot keeps the pixel ratio in a variable of its own and follows it with its own "(resolution)" media
// query; it has no option to pass a ratio. When display notices a change that query did not report (see
// above), it replays a 'change' event on uPlot's query. The query is found by watching matchMedia only
// while uPlot's own code runs (its script, and its handler during the replay), never for the whole page.
const uplotRatio = (() => {
  let query = null, native = null;
  const capture = () => {
    if (native) return;
    native = window.matchMedia;
    window.matchMedia = (q) => { const m = native.call(window, q); if (/resolution/.test(q)) query = m; return m; };
  };
  const release = () => { if (native) window.matchMedia = native; native = null; };
  return {
    capture, release,
    sync() {
      if (!window.uPlot || !query || window.uPlot.pxRatio === window.devicePixelRatio) return;
      capture();
      try { query.dispatchEvent(new Event('change')); } finally { release(); }
    },
  };
})();
const UI_SIZES = ui.SIZES;
const uiSizeLabel = (s) => (s === 'auto' ? 'Auto' : `${s}%`);

const display = (() => {
  const root = document.documentElement;
  const subs = new Set(), sizeSubs = new Set();
  const read = () => ({ rootPx: parseFloat(getComputedStyle(root).fontSize) || 16, dpr: window.devicePixelRatio || 1 });
  let size = UI_SIZES.includes(+root.dataset.uiSize) ? +root.dataset.uiSize : 'auto'; // what boot.js applied
  let state = read(), frame = 0, mq = null, causes = new Set();
  let screenKey = `${screen.width}x${screen.height}`;

  function check(cause) {
    const now = read();
    if (Math.abs(now.rootPx - state.rootPx) < 0.005 && now.dpr === state.dpr) return;
    if (now.dpr !== state.dpr) { causes.add('dpr'); watchDpr(now.dpr); uplotRatio.sync(); }
    causes.add(cause);
    state = now;
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      const ev = { ...state, dprChanged: causes.has('dpr'), sizeChanged: causes.has('size') };
      causes = new Set();
      for (const f of subs) { try { f(ev); } catch (e) { console.warn(e); } }
    });
  }
  // the window may be on another screen now: Auto follows the screen
  function screenCheck() {
    const k = `${screen.width}x${screen.height}`;
    if (k === screenKey) return;
    screenKey = k;
    ui.apply(size);
  }
  // the browser reports ratio changes itself: the stand-in check is no longer needed
  function onDpr() { clearInterval(silentCheck); screenCheck(); check('dpr'); }
  function watchDpr(dpr) {
    mq?.removeEventListener('change', onDpr);
    mq = window.matchMedia(`(resolution: ${dpr}dppx)`);
    mq.addEventListener('change', onDpr);
  }
  watchDpr(state.dpr);
  const silentCheck = setInterval(() => { if ((window.devicePixelRatio || 1) !== state.dpr) check('dpr'); }, 500);
  const onScreen = () => { screenCheck(); check('root'); };
  window.addEventListener('resize', onScreen);
  screen.addEventListener?.('change', onScreen); // where the browser has it (Chromium)
  document.addEventListener('visibilitychange', () => { if (!document.hidden) onScreen(); });

  function apply(s) {
    size = ui.apply(s);
    store.set('ui-size', size); // boot.js applies this before the next first paint
    check('size');
    for (const f of sizeSubs) f(size);
  }
  return {
    get size() { return size; },
    /** Apply an Interface size now ('auto' or a percentage). Saving it is the caller's job. */
    apply,
    rootPx: () => state.rootPx,
    dpr: () => state.dpr,
    /** px at the 16 px design size -> CSS px now. For the few sizes JS hands to canvases and maps. */
    rpx: (n) => (n * state.rootPx) / 16,
    /** what Auto means on this screen, as a percentage of the standard size (100 unless the screen is very large) */
    autoPercent: () => Math.round(ui.autoFactor() * 100),
    /** f({rootPx, dpr, dprChanged, sizeChanged}) after the root size or the pixel ratio changed; returns an unsubscribe function */
    onChange(f) { subs.add(f); return () => subs.delete(f); },
    /** f(size) after the Interface size changed */
    onSize(f) { sizeSubs.add(f); return () => sizeSubs.delete(f); },
  };
})();
const rpx = display.rpx;

// Interface size: the server keeps it per person (api.getPrefs / setPrefs); boot.js keeps a copy in
// this browser so the first frame is right. Changing it applies at once, then saves.
async function setInterfaceSize(s) {
  if (s === display.size) return;
  display.apply(s);
  try { await api.setPrefs({ interfaceSize: s }); } catch (e) { console.warn('could not save the interface size', e); }
}
// radios for the Interface size: in Settings and in the account menu
function sizePickerHtml(name) {
  return UI_SIZES.map((s) => `<label class="size-opt"><input type="radio" name="${name}" value="${s}"${s === display.size ? ' checked' : ''}><span>${uiSizeLabel(s)}</span></label>`).join('');
}
function bindSizePicker(host, onSize = null) {
  host.addEventListener('change', (e) => {
    if (!e.target.matches('input[type="radio"]')) return;
    const v = e.target.value;
    setInterfaceSize(v === 'auto' ? 'auto' : +v);
  });
  const sync = (s) => {
    for (const input of $$('input[type="radio"]', host)) input.checked = input.value === String(s);
    onSize?.(s);
  };
  return display.onSize(sync);
}
// What the sizes mean on this screen. Every size is a percentage of Auto (so 100% is the same as Auto),
// and Auto itself is larger than the standard size only on very large screens (145% on native 4K).
function autoNote() {
  const pct = display.autoPercent();
  const auto = pct === 100 ? 'Auto is the standard size on this screen' : `Auto is ${pct}% of the standard size on this screen`;
  return display.size === 'auto' || pct === 100 ? auto : `${auto}, so ${display.size}% here is ${Math.round((pct * display.size) / 100)}%`;
}

// ------------------------------------------------------------------ charts (uPlot, loaded on first use)
let uPlotLoad = null;
function loadUPlot() {
  uPlotLoad ??= window.uPlot ? Promise.resolve(window.uPlot) : new Promise((res, rej) => {
    const s = document.createElement('script');
    s.src = 'vendor/uplot/uPlot.iife.min.js';
    s.onload = () => { uplotRatio.release(); res(window.uPlot); };
    s.onerror = () => { uplotRatio.release(); uPlotLoad = null; rej(new Error('uPlot did not load')); };
    uplotRatio.capture(); // uPlot makes its pixel-ratio query while its script runs
    document.head.append(s);
  });
  return uPlotLoad;
}
// A uPlot chart that fills its box. CSS sizes the box (in rem, per layout); the chart follows it:
// - the box changed size (window resize, layout tier): uPlot's setSize, cheap, keeps everything;
// - the pixel ratio or the Interface size changed: built again at once, because uPlot draws axis text,
//   line widths and gaps in pixels and takes its pixel ratio when it is built;
// - the root size changed for another reason: built again once it settles (150 ms), not on every frame.
// A rebuild keeps the cursor where it was (the hover readout stays). make(width, height) returns the
// options; onBuild(u) runs after every build (to restore a zoom, for example).
function mountPlot(target, make, data, ctx, onBuild = null) {
  let u = null, later = 0;
  const box = () => ({ width: Math.max(10, Math.floor(target.clientWidth)), height: Math.max(10, Math.floor(target.clientHeight)) });
  // the cursor as a time and a fraction of the height, put back on the same point a frame later (uPlot
  // takes a cursor only once it has drawn)
  const cursorAt = () => (u && u.cursor.idx != null && u.cursor.left >= 0 ? { x: u.posToVal(u.cursor.left, 'x'), fy: u.cursor.top / Math.max(1, u.over.clientHeight) } : null);
  const putCursor = (c) => { if (c) requestAnimationFrame(() => { if (u) u.setCursor({ left: u.valToPos(c.x, 'x'), top: c.fy * u.over.clientHeight }); }); };
  const build = () => {
    clearTimeout(later);
    const c = cursorAt();
    u?.destroy();
    const { width, height } = box();
    u = new uPlot(make(width, height), data, target);
    onBuild?.(u);
    putCursor(c);
  };
  build();
  const ro = new ResizeObserver(() => {
    if (!u) return;
    const { width, height } = box();
    if (width !== u.width || height !== u.height) { const c = cursorAt(); u.setSize({ width, height }); putCursor(c); }
  });
  ro.observe(target);
  const off = display.onChange((ev) => {
    if (ev.dprChanged || ev.sizeChanged) build();
    else { clearTimeout(later); later = setTimeout(build, 150); }
  });
  ctx.onCleanup(() => { clearTimeout(later); ro.disconnect(); off(); u?.destroy(); u = null; });
  return { get u() { return u; } };
}
// whole pixels (uPlot scales the size in this string by the pixel ratio and reads whole numbers only),
// never under the smallest text size in app.css (--fs-floor)
const chartFont = () => `500 ${Math.max(10, Math.round(rpx(11)))}px "SM Inter"`;

// ------------------------------------------------------------------ units
let units = store.get('units', 'imperial') === 'metric' ? 'metric' : 'imperial';
const imp = () => units === 'imperial';
const U = {
  dist: (km) => (km == null ? null : imp() ? km / KM_MI : km),
  distU: () => (imp() ? 'mi' : 'km'),
  speed: (kph) => (kph == null ? null : imp() ? kph / KM_MI : kph),
  speedU: () => (imp() ? 'mph' : 'km/h'),
  temp: (c) => (c == null ? null : imp() ? c * 9 / 5 + 32 : c),
  tempU: () => (imp() ? '°F' : '°C'),
  press: (bar) => (bar == null ? null : imp() ? bar * 14.5038 : bar),
  pressU: () => (imp() ? 'psi' : 'bar'),
  pressD: () => (imp() ? 0 : 2),
  eff: (whkm) => (whkm == null ? null : imp() ? whkm * KM_MI : whkm),
  effU: () => (imp() ? 'Wh/mi' : 'Wh/km'),
  elev: (m) => (m == null ? null : imp() ? m * 3.28084 : m),
  elevU: () => (imp() ? 'ft' : 'm'),
};
const nf = (v, d = 0) => (v == null || Number.isNaN(v) ? '—' : Number(v).toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d }));
const compactFmt = new Intl.NumberFormat('en-US', { notation: 'compact', maximumFractionDigits: 1 });
// exact up to 99,999; above that "36.5K" with the exact number in the tooltip and for screen readers
const nfc = (v) => (v == null ? '—' : Math.abs(v) > 99999 ? compactFmt.format(v) : nf(v));
const bigNum = (v) => (v == null || Math.abs(v) <= 99999 ? nf(v) : `<span title="${nf(v)}" aria-label="${nf(v)}">${compactFmt.format(v)}</span>`);
const withUnit = (num, unit) => `${num}<span class="unit">${esc(unit)}</span>`;
// "a · b · c" that wraps between its parts rather than inside a short one ("55% → / 55%"); a sentence
// still wraps normally. Parts are HTML.
const parts = (list) => list.filter(Boolean).map((p) => (p.length <= 28 ? `<span class="nowrap">${p}</span>` : p)).join(SEP);
const fDist = (km, d = 1) => withUnit(nf(U.dist(km), d), U.distU());
const fSpeed = (kph) => withUnit(nf(U.speed(kph), 0), U.speedU());
const fTemp = (c) => withUnit(nf(U.temp(c), 0), U.tempU());
const fEff = (whkm) => withUnit(nf(U.eff(whkm), 0), U.effU());
const fKwh = (k, d = 1) => withUnit(nf(k, d), 'kWh');
const tDist = (km, d = 1) => `${nf(U.dist(km), d)} ${U.distU()}`;
const tEff = (whkm) => `${nf(U.eff(whkm), 0)} ${U.effU()}`;

// ------------------------------------------------------------------ time (America/Chicago)
const fmt = {
  time: new Intl.DateTimeFormat('en-US', { timeZone: TZ, hour: 'numeric', minute: '2-digit' }),
  timeSec: new Intl.DateTimeFormat('en-US', { timeZone: TZ, hour: 'numeric', minute: '2-digit', second: '2-digit' }),
  date: new Intl.DateTimeFormat('en-US', { timeZone: TZ, weekday: 'short', month: 'short', day: 'numeric' }),
  dateLong: new Intl.DateTimeFormat('en-US', { timeZone: TZ, weekday: 'long', month: 'long', day: 'numeric' }),
  dayShort: new Intl.DateTimeFormat('en-US', { timeZone: TZ, month: 'short', day: 'numeric' }),
  key: new Intl.DateTimeFormat('en-CA', { timeZone: TZ, year: 'numeric', month: '2-digit', day: '2-digit' }),
  tz: new Intl.DateTimeFormat('en-US', { timeZone: TZ, timeZoneName: 'short' }),
};
const tzName = () => (fmt.tz.formatToParts(Date.now()).find((p) => p.type === 'timeZoneName') || {}).value || 'CT';
const dayKey = (ms) => fmt.key.format(ms);
const todayKey = () => dayKey(Date.now());
function addDays(day, n) { const [y, m, d] = day.split('-').map(Number); return new Date(Date.UTC(y, m - 1, d + n)).toISOString().slice(0, 10); }
const dayNoon = (day) => { const [y, m, d] = day.split('-').map(Number); return Date.UTC(y, m - 1, d, 18); };
function relDay(ms) {
  const k = dayKey(ms), t = todayKey();
  if (k === t) return 'Today';
  if (k === addDays(t, -1)) return 'Yesterday';
  return fmt.date.format(ms);
}
const tm = (ms) => fmt.time.format(ms);
const whenText = (ms) => (relDay(ms) === 'Today' ? tm(ms) : `${relDay(ms)}, ${tm(ms)}`);
function ago(ms) {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 45) return 'just now';
  if (s < 90) return '1 min ago';
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 6 * 3600) { const h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60); return m ? `${h} h ${m} min ago` : `${h} h ago`; }
  if (s < 86400 * 2) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} d ago`;
}
// "2 h 27 min" (how long since), for "No data for …"
const forText = (ms) => { const a = ago(ms); return a === 'just now' ? 'under a minute' : a.replace(/ ago$/, ''); };
function dur(sec) {
  sec = Math.max(0, Math.round(sec));
  if (sec < 60) return `${sec} s`;
  const h = Math.floor(sec / 3600), m = Math.round((sec % 3600) / 60);
  if (!h) return `${m} min`;
  return `${h} h ${String(m).padStart(2, '0')} min`;
}
// "Mason St, Presidio" -> "Mason St" (the place name above already says where)
const street = (detail) => String(detail || '').split(',')[0];

// ------------------------------------------------------------------ car states and data age
const STATES = {
  driving: { label: 'Driving', icon: 'steer' },
  charging: { label: 'Charging', icon: 'bolt' },
  parked: { label: 'Parked', icon: 'park' },
  asleep: { label: 'Asleep', icon: 'moon' },
  unknown: { label: 'Offline', icon: 'question' },
};
const TIRE_NAMES = { fl: 'front left', fr: 'front right', rl: 'rear left', rr: 'rear right' };
function pill(state, { stale = false, lg = false, text = null } = {}) {
  const s = STATES[state] || STATES.unknown;
  return `<span class="pill${lg ? ' pill--lg' : ''}${stale ? ' is-stale' : ''}" data-state="${esc(state)}"><span class="dot" aria-hidden="true"></span>${esc(text || s.label)}${stale ? '<span class="sr-only"> (last known, data is stale)</span>' : ''}</span>`;
}
// Data age, worked out in the browser so it stays right between server refreshes:
// fresh under 5 min, stale over 10 min for a car that should be reporting (driving, charging,
// parked and awake). Asleep and offline cars are not expected to report: their age is just "old".
const FRESH_MS = 5 * 60000, STALE_MS = 10 * 60000;
const REPORTING = new Set(['driving', 'charging', 'parked']);
function ageClass(car) {
  if (!REPORTING.has(car.state)) return 'old';
  const age = Date.now() - car.lastSeen;
  if (car.stale || age > STALE_MS) return 'stale';
  return age < FRESH_MS ? 'fresh' : 'old';
}
const isStale = (car) => ageClass(car) === 'stale';
function ageHtml(car, { label = '', title = '', ts = '' } = {}) {
  const c = ageClass(car);
  return `<span class="age age--${c}" data-age-for="${esc(car.state)}" data-age-stale="${car.stale ? 1 : 0}"${title ? ` title="${esc(title)}"` : ''}>${c === 'stale' ? icon('clock') : ''}${label}<span data-ago="${car.lastSeen}"${ts ? ` data-ts="${ts}"` : ''}>${ago(car.lastSeen)}</span></span>`;
}
function refreshAge(el) {
  const ts = +(el.querySelector('[data-ago]')?.dataset.ago || 0);
  const c = ageClass({ state: el.dataset.ageFor, stale: el.dataset.ageStale === '1', lastSeen: ts });
  const cur = (/age--(\w+)/.exec(el.className) || [])[1];
  if (cur === c) return;
  el.classList.remove('age--' + cur);
  el.classList.add('age--' + c);
  const ic = el.querySelector(':scope > .ico');
  if (c === 'stale' && !ic) el.insertAdjacentHTML('afterbegin', icon('clock'));
  if (c !== 'stale' && ic) ic.remove();
}
const keyChip = (key, q = '') => `<span class="key-chip" title="Car key">${hl(key, q)}</span>`;
// one meta line for a car everywhere: "Model Y · 2025 [7C1E 04AF]" (+ " · place")
const carMeta = (c, { q = '', place = false } = {}) => `${hl(c.model, q)}${SEP}${c.year} ${keyChip(c.key, q)}${place ? `${SEP}${hl(c.place, q)}` : ''}`;
function attentionText(a) {
  if (a.kind === 'battery') return `Battery ${a.soc}%`;
  if (a.kind === 'stale') return `No data for ${forText(a.since)}`;
  if (a.kind === 'offline') return `Offline for ${forText(a.since)}`;
  if (a.kind === 'tire') return `Tire low · ${TIRE_NAMES[a.tire] || ''}`;
  return '';
}

// ------------------------------------------------------------------ app state
const recent = {
  list() { const v = store.get('recent', null); return Array.isArray(v) && v.length ? v.slice(0, 8) : ['model-y', 'model-3', 'c0007', 'c0042', 'c0118']; },
  push(id) { const l = this.list().filter((x) => x !== id); l.unshift(id); store.set('recent', l.slice(0, 8)); },
};
const carCache = new Map();
async function carById(id) {
  if (carCache.has(id)) return carCache.get(id);
  const c = await api.getCar(id);
  if (c) carCache.set(id, c);
  return c;
}
let currentCarId = recent.list()[0] || 'model-y';
let lastSummary = null;

// ------------------------------------------------------------------ shell
const CAR_PAGES = ['now', 'drives', 'charges', 'timeline', 'battery', 'stats', 'signals', 'logs'];
const PAGE_TITLES = { now: 'Now', drives: 'Drives', charges: 'Charges', timeline: 'Timeline', battery: 'Battery', stats: 'Stats', signals: 'Signals', logs: 'Logs', settings: 'Settings', fleet: 'All cars' };

// live: the Now page's newer {state, stale} for the car, when it has them
function showCarInSidebar(car, live = null) {
  const st = live ? live.state : car.state;
  const stale = live ? live.stale : isStale(car);
  $('#car-switch-name').textContent = car.name;
  html($('#car-switch-sub'), `<span class="car-switch-sub-text">${esc(car.model)}${SEP}${esc((STATES[st] || STATES.unknown).label)}</span>${stale
    ? `<span class="car-switch-stale" title="Stale: no data for over 10 minutes">${icon('clock')}<span class="sr-only">, data is stale</span></span>` : ''}`);
  const dot = $('#car-switch-dot');
  dot.dataset.state = st;
  dot.classList.toggle('is-stale', stale);
}
async function setShell({ page, carId = null, crumbs = [] }) {
  if (carId) currentCarId = carId;
  for (const a of $$('.nav-link[data-nav]')) {
    const p = a.dataset.nav;
    if (CAR_PAGES.includes(p)) a.href = `#/cars/${encodeURIComponent(currentCarId)}/${p}`;
    if (p === page) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  html($('#crumbs'), crumbs.map((c, i) => (i === crumbs.length - 1
    ? `<span aria-current="page">${esc(c.label)}</span>`
    : `<a href="${esc(c.href)}">${esc(c.label)}</a><span class="sep" aria-hidden="true">/</span>`)).join(''));
  document.title = [crumbs.length ? crumbs[crumbs.length - 1].label : 'Sentry Mate', crumbs.length > 1 ? crumbs[crumbs.length - 2].label : null, 'Sentry Mate'].filter(Boolean).join(' · ');
  const car = await carById(currentCarId).catch(() => null);
  if (car) showCarInSidebar(car); // the Now page refines this with the car's newest state once it has it
}
const setNavCount = (n) => html($('#nav-fleet-count'), bigNum(n));

// the top bar's freshness pill follows the last successful answer from the server
function updateFreshness() {
  const el = $('#freshness');
  const age = Date.now() - net.lastOk;
  const late = age > 2 * 60000;
  el.classList.toggle('is-late', late || net.failing);
  $('#freshness-text').textContent = late ? `Not updated for ${Math.round(age / 60000)} min` : net.failing ? 'Server not answering' : 'Data arrives ≈1 min behind';
  el.title = `Cars upload their logs about once a minute, so the newest data is about a minute old. Last answer from the server: ${fmt.timeSec.format(net.lastOk)}.`;
}

// a view registers what to do on the 15 s tick, on the 30 s refresh and on a units change
let hooks = { tick: [], refresh: [], units: [] };
let summaryEtag = null;
async function heartbeat() {
  try {
    const s = await api.fleetSummary({ ifNoneMatch: summaryEtag });
    if (s.notModified) { if (lastSummary) lastSummary = { ...lastSummary, updatedAt: s.updatedAt }; }
    else { lastSummary = s; summaryEtag = s.etag; setNavCount(s.total); }
    const c = await api.getCar(currentCarId).catch(() => null);
    if (c) { carCache.set(c.id, c); if (!location.hash.includes('/now')) showCarInSidebar(c); }
    for (const f of hooks.refresh) await f(lastSummary);
  } catch (e) { console.warn(e); }
  updateFreshness();
}
function startClock() {
  const tick = () => {
    $('#clock').textContent = `${tm(Date.now())} ${tzName()}`;
    for (const el of $$('[data-ago]')) el.textContent = ago(+el.dataset.ago);
    for (const el of $$('[data-for-since]')) el.textContent = forText(+el.dataset.forSince);
    for (const el of $$('.age[data-age-for]')) refreshAge(el);
    updateFreshness();
    for (const f of hooks.tick) f();
  };
  tick();
  setInterval(tick, 15000);
  setInterval(heartbeat, 30000);
}

function setupUnits() {
  const sync = () => { for (const b of $$('[data-units]')) b.setAttribute('aria-pressed', String(b.dataset.units === units)); };
  sync();
  for (const b of $$('[data-units]')) {
    b.addEventListener('click', () => {
      if (units === b.dataset.units) return;
      units = b.dataset.units;
      store.set('units', units);
      sync();
      // a page that can redraw in place keeps its place (the fleet list keeps its row); others re-render
      if (hooks.units.length) for (const f of hooks.units) f();
      else route({ keepScroll: true });
    });
  }
}

async function showNotices() {
  try {
    const list = await api.getNotices();
    const box = $('#notices');
    if (!list.length) { box.hidden = true; return; }
    box.hidden = false;
    html(box, list.map((n) => `<div class="notice" role="alert">${icon('warn')}<span>${esc(n.text)}</span></div>`).join(''));
  } catch { /* notices are best effort */ }
}

// ------------------------------------------------------------------ router
const routes = [
  [/^\/fleet$/, viewFleet],
  [/^\/cars\/([^/]+)\/now$/, viewNow],
  [/^\/cars\/([^/]+)\/drives$/, viewDrives],
  [/^\/cars\/([^/]+)\/drives\/([^/]+)$/, viewDrive],
  [/^\/cars\/([^/]+)\/timeline$/, viewTimeline],
  [/^\/cars\/([^/]+)\/(charges|battery|stats|signals|logs)$/, viewSoon],
  [/^\/settings$/, viewSettings],
];
let renderToken = 0;
let cleanups = [];
function parseHash() {
  const h = decodeURI(location.hash.replace(/^#/, '')) || '/fleet';
  const [path, qs] = h.split('?');
  return { path: path || '/fleet', query: new URLSearchParams(qs || '') };
}
async function route({ keepScroll = false } = {}) {
  for (const f of cleanups) { try { f(); } catch { /* ignore */ } }
  cleanups = [];
  hooks = { tick: [], refresh: [], units: [] };
  const { path, query } = parseHash();
  const token = ++renderToken;
  const view = $('#view');
  const y = window.scrollY;
  view.className = 'view';
  view.setAttribute('aria-busy', 'true');
  const ctx = {
    query, view, alive: () => token === renderToken, onCleanup: (f) => cleanups.push(f),
    onTick: (f) => hooks.tick.push(f), onRefresh: (f) => hooks.refresh.push(f), onUnits: (f) => hooks.units.push(f),
  };
  try {
    let matched = false;
    for (const [re, fn] of routes) {
      const m = re.exec(path);
      if (m) { matched = true; ctx.params = m.slice(1).map(decodeURIComponent); await fn(ctx); break; }
    }
    if (!matched) await viewNotFound(ctx);
  } catch (err) {
    if (ctx.alive()) showError(view, err);
    console.warn(err);
  }
  if (ctx.alive()) {
    view.setAttribute('aria-busy', 'false');
    window.scrollTo(0, keepScroll ? y : 0);
  }
}
function showError(view, err) {
  view.className = 'view';
  const notFound = err && err.status === 404;
  html(view, `<div class="panel"><div class="empty">
    <div class="empty-icon">${icon(notFound ? 'question' : 'warn')}</div>
    <h3>${notFound ? 'Not found' : 'Could not load this page'}</h3>
    <p>${esc(notFound ? (err.message || 'Nothing here.') + '. It may have been removed, or the link is wrong.' : 'The server did not answer. Check that Sentry Mate is running, then try again.')}</p>
    <div class="page-actions"><a class="btn" href="#/fleet">${icon('fleet')} All cars</a>${notFound ? '' : `<button class="btn" type="button" data-retry>${icon('reset')} Try again</button>`}</div>
  </div></div>`);
  const r = $('[data-retry]', view);
  if (r) r.addEventListener('click', () => route());
}
async function viewNotFound(ctx) {
  await setShell({ page: null, crumbs: [{ label: 'Not found' }] });
  showError(ctx.view, { status: 404, message: 'There is no page at this address' });
}

// ------------------------------------------------------------------ FLEET (all cars)
const SORT_LABELS = { state: 'State', name: 'Name', soc: 'Battery', lastSeen: 'Last seen', todayKm: 'Today', place: 'Location' };
const FILTERS = [['', 'All states'], ['driving', 'Driving'], ['charging', 'Charging'], ['parked', 'Parked'], ['asleep', 'Asleep'], ['unknown', 'Offline'], ['stale', 'Stale data']];
// The tiles split the fleet into groups that add up to the total: a car whose data stopped
// counts once, under Stale, not under the state it was last seen in.
const TILES = [
  ['driving', 'Driving', 'steer', (s, pct) => `${pct(s.states.driving)} of cars`],
  ['charging', 'Charging', 'bolt', (s) => `${nfc(s.chargingKw)} kW now`],
  ['parked', 'Parked', 'park', () => 'awake, not moving'],
  ['asleep', 'Asleep', 'moon', () => 'sleeping'],
  ['unknown', 'Offline', 'question', () => 'state unknown'],
  ['stale', 'Stale', 'clock', () => 'silent > 10 min'],
];

async function viewFleet(ctx) {
  const { view, query } = ctx;
  view.classList.add('view--fill');
  setShell({ page: 'fleet', crumbs: [{ label: 'All cars' }] });
  const st = {
    q: query.get('q') || '',
    state: query.get('state') || '',
    sort: SORT_LABELS[query.get('sort')] ? query.get('sort') : 'state',
    dir: query.get('dir') === 'desc' ? 'desc' : 'asc',
  };
  html(view, `
    <div class="page-head page-head--fleet">
      <div>
        <h1 class="page-title">All cars</h1>
        <p class="page-sub" id="fleet-sub"><span class="skel skel--text skel--w320"></span></p>
      </div>
      <dl class="head-stats" id="fleet-stats" aria-label="Fleet totals today">
        <div class="head-stat"><dt><span class="skel skel--text"></span></dt><dd><span class="skel skel--num"></span></dd></div>
        <div class="head-stat"><dt><span class="skel skel--text"></span></dt><dd><span class="skel skel--num"></span></dd></div>
      </dl>
    </div>
    <section class="fleet-board" id="fleet-board" aria-label="Cars by state">${TILES.map(() => '<div class="state-tile"><span class="skel skel--text"></span><span class="skel skel--num"></span><span class="skel skel--text"></span></div>').join('')}</section>
    <div class="toolbar" role="search">
      <label class="field field--search">
        <span class="sr-only">Search cars</span>
        ${icon('search')}
        <input class="input" id="fleet-q" type="search" placeholder="Search name, key or place" value="${esc(st.q)}" autocomplete="off" spellcheck="false">
        <kbd class="kbd" aria-hidden="true">/</kbd>
      </label>
      <label class="field"><span class="sr-only">State</span>
        <select class="select" id="fleet-state">${FILTERS.map(([v, l]) => `<option value="${v}"${v === st.state ? ' selected' : ''}>${l}</option>`).join('')}</select>
      </label>
      <label class="field"><span class="sr-only">Sort by</span>
        <select class="select" id="fleet-sort">${Object.entries(SORT_LABELS).map(([v, l]) => `<option value="${v}"${v === st.sort ? ' selected' : ''}>Sort: ${l}</option>`).join('')}</select>
      </label>
      <button class="btn btn--icon" type="button" id="fleet-dir" aria-label="Reverse sort order">${icon('sort')}</button>
      <button class="btn btn--ghost btn--sm" type="button" id="fleet-clear" hidden>${icon('x', 'ico--sm')} Clear</button>
      <span class="grow"></span>
      <span class="toolbar-note" id="fleet-count" aria-live="polite"></span>
    </div>
    <div class="vt" id="fleet-table"></div>`);

  const syncUrl = () => {
    const p = new URLSearchParams();
    if (st.q) p.set('q', st.q);
    if (st.state) p.set('state', st.state);
    if (st.sort !== 'state') p.set('sort', st.sort);
    if (st.dir !== 'asc') p.set('dir', st.dir);
    const s = p.toString();
    history.replaceState(null, '', '#/fleet' + (s ? '?' + s : ''));
    $('#fleet-clear').hidden = !(st.q || st.state);
    for (const t of $$('[data-filter]', view)) t.setAttribute('aria-pressed', String(t.dataset.filter === st.state));
  };
  let shownTotal = null;
  const showCount = () => {
    if (shownTotal == null) return;
    const all = lastSummary ? lastSummary.total : shownTotal;
    $('#fleet-count').textContent = st.q || st.state ? `${nf(shownTotal)} of ${nf(all)} cars match` : `${nf(shownTotal)} cars`;
  };

  const table = createCarTable($('#fleet-table'), {
    fetchPage: (offset, limit, asOf) => api.listCars({ query: st.q, state: st.state, sort: st.sort, dir: st.dir, offset, limit, asOf }),
    sort: () => st,
    onSort: (key) => {
      if (st.sort === key) st.dir = st.dir === 'asc' ? 'desc' : 'asc'; else { st.sort = key; st.dir = 'asc'; }
      $('#fleet-sort').value = st.sort;
      syncUrl(); table.reset();
    },
    onCount: (total) => { shownTotal = total; showCount(); },
    emptyHtml: () => `<div class="empty"><div class="empty-icon">${icon('search')}</div><h3>No cars match${st.q ? ` “${esc(st.q)}”` : ''}</h3><p>Try part of the name, the car's key (for example <span class="mono">7C1E</span>), a model or a place. ${st.state ? 'The state filter is also on.' : ''}</p><button class="btn" type="button" data-clear-all>${icon('x', 'ico--sm')} Clear search and filters</button></div>`,
  });
  ctx.onCleanup(() => table.destroy());

  let deb = 0;
  $('#fleet-q').addEventListener('input', (e) => {
    clearTimeout(deb);
    deb = setTimeout(() => { st.q = e.target.value.trim(); syncUrl(); table.reset(); }, 160);
  });
  $('#fleet-state').addEventListener('change', (e) => { st.state = e.target.value; syncUrl(); table.reset(); });
  $('#fleet-sort').addEventListener('change', (e) => { st.sort = e.target.value; syncUrl(); table.reset(); });
  $('#fleet-dir').addEventListener('click', () => { st.dir = st.dir === 'asc' ? 'desc' : 'asc'; syncUrl(); table.reset(); });
  const clearAll = () => { st.q = ''; st.state = ''; $('#fleet-q').value = ''; $('#fleet-state').value = ''; syncUrl(); table.reset(); };
  $('#fleet-clear').addEventListener('click', clearAll);
  view.addEventListener('click', (e) => { if (e.target.closest('[data-clear-all]')) clearAll(); });
  $('#fleet-board').addEventListener('click', (e) => {
    const t = e.target.closest('[data-filter]');
    if (!t) return;
    st.state = st.state === t.dataset.filter ? '' : t.dataset.filter;
    $('#fleet-state').value = st.state;
    syncUrl(); table.reset();
  });
  const slash = (e) => {
    if (e.key === '/' && !e.ctrlKey && !e.metaKey && !/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName) && !$('#switcher').open) {
      e.preventDefault(); $('#fleet-q').focus();
    }
  };
  document.addEventListener('keydown', slash);
  ctx.onCleanup(() => document.removeEventListener('keydown', slash));

  syncUrl();
  table.reset();

  const renderSummary = (sum) => {
    const pct = (n) => `${sum.total ? Math.round((n / sum.total) * 100) : 0}%`;
    const value = (k) => (k === 'stale' ? sum.stale : sum.states[k]);
    html($('#fleet-board'), TILES.map(([k, l, ic, sub]) => `
      <button class="state-tile${k === 'stale' ? ' state-tile--stale' : ''}" type="button"${k === 'stale' ? '' : ` data-state="${k}"`} data-filter="${k}" aria-pressed="${st.state === k}">
        <span class="state-tile-label">${icon(ic)}${l}</span>
        <span class="state-tile-value">${bigNum(value(k))}</span>
        <span class="state-tile-sub">${sub(sum, pct)}</span>
      </button>`).join(''));
    html($('#fleet-stats'), `
      <div class="head-stat"><dt>Driven today</dt><dd>${withUnit(nfc(U.dist(sum.todayKm)), U.distU())}</dd></div>
      <div class="head-stat" title="Average of the ${nf(sum.avgSocCars)} cars seen in the last 24 hours. Cars offline for longer are left out."><dt>Average battery <span class="head-stat-note">24 h</span></dt><dd>${withUnit(nf(sum.avgSoc), '%')}</dd></div>`);
    html($('#fleet-sub'), `<span>${nf(sum.total)} cars</span><span class="sep"></span><span>${nf(sum.states.driving)} driving now</span>${sum.stale ? `<span class="sep"></span><span class="sub-warn">${icon('clock', 'ico--sm')}${nf(sum.stale)} stale, last known state shown</span>` : ''}<span class="sep"></span><span>Updated <span data-ago="${sum.updatedAt}">${ago(sum.updatedAt)}</span></span>`);
    setNavCount(sum.total);
    showCount();
  };
  const sum = await api.fleetSummary();
  if (!ctx.alive()) return;
  lastSummary = sum; summaryEtag = sum.etag;
  renderSummary(sum);
  ctx.onRefresh(async (s) => { if (s && ctx.alive()) { renderSummary(s); await table.refresh(); } });
  ctx.onTick(() => table.draw());
  ctx.onUnits(() => { if (lastSummary) renderSummary(lastSummary); table.draw(); });
}

// A virtual table: only the visible rows exist in the page, and rows come from the API in pages of
// 100 as they scroll into view, so it stays fast with any number of cars.
// - All pages come from one ordering (the asOf snapshot the first page returns).
// - Rows are asked for 120 ms after scrolling stops, and at most 10 pages stay in memory.
// - Row and header heights are not numbers in this file: they are in rem in app.css, so they follow the
//   Interface size, browser zoom and fonts. measure() reads them from layout after every draw (and the
//   draw that follows a resize, a pixel-ratio or a root-size change), and when they change the same row
//   stays at the top. The row height comes from a hidden probe row kept at the top of the table (the
//   real rows sit millions of px down, where a transformed box is rounded to a quarter or half pixel),
//   is snapped to whole device pixels, and is given back to the rows (--vt-row-h), so every row is
//   exactly one step tall and lands on whole device pixels at any size (84.1 px at 4K, 52.2 px at 90%).
// - Past 8M px the scroll area stops growing (browsers stop drawing taller elements) and the scroll
//   position maps to a row by ratio.
// - Wider tables show more columns (app.css, container "vt"): every row carries them, CSS shows them.
function createCarTable(host, { fetchPage, sort, onSort, onCount, emptyHtml }) {
  const PAGE = 100, OVER = 6;
  const MAX_SPACER = 8e6, MAX_PAGES = 10, SETTLE_MS = 120;
  let ROW = 58, HEAD = 42; // first guesses until measure() has read the drawn rows
  // [sort key or null, label, extra classes]
  const cols = [['name', 'Car'], ['state', 'State'], ['soc', 'Battery'], [null, 'Range', 'vt-x vt-x--range'],
    ['place', 'Location'], [null, 'Activity', 'vt-x vt-x--activity'], [null, 'Charge port', 'vt-x vt-x--port'],
    [null, 'Tires', 'vt-x vt-x--tires'], ['lastSeen', 'Last seen'], ['todayKm', 'Today', 'vt-th--num vt-c-today'], [null, '']];
  html(host, `
    <div class="vt-scroll" role="table" aria-label="Cars" aria-rowcount="-1">
      <div class="vt-head vt-cols" role="row" aria-rowindex="1">
        ${cols.map(([k, l, cls = '']) => (k
          ? `<div class="vt-th ${cls}" role="columnheader" data-col="${k}"><button class="vt-sort" type="button" data-sort="${k}">${l}<svg class="ico arrow" aria-hidden="true"><use href="#i-chev-ud"/></svg></button></div>`
          : `<div class="vt-th ${cls}" role="columnheader">${l || '<span class="sr-only">Open</span>'}</div>`)).join('')}
      </div>
      <div class="vt-body" role="rowgroup"></div>
    </div>
    <div class="vt-row vt-probe" aria-hidden="true"></div>
    <div class="vt-empty" hidden></div>
    <div class="vt-foot"><span class="vt-range" aria-live="off"></span>
      <label class="vt-jump" hidden><span>Go to row</span><input class="input input--sm" type="number" min="1" step="1" inputmode="numeric" placeholder="#"></label></div>`);
  const scroll = $('.vt-scroll', host), head = $('.vt-head', host), body = $('.vt-body', host), emptyEl = $('.vt-empty', host), probe = $('.vt-probe', host);
  const foot = $('.vt-foot', host), rangeEl = $('.vt-range', host), jump = $('.vt-jump', host), jumpIn = $('input', jump);
  const pages = new Map(); // page -> rows (null while loading); kept in least-recently-used order
  let total = null, gen = 0, raf = 0, focusIndex = null, asOf = null, settle = 0, spacerH = 0, visiblePages = [];
  // The row at the top of the list (with its fraction). Scrolling changes it; a change of size does not:
  // when the row height, header or list height changed since the last draw, this row is put back on top.
  let topRow = 0, geom = '';

  const viewH = () => Math.max(ROW, scroll.clientHeight - HEAD);
  const scaled = () => total * ROW > MAX_SPACER;
  // scrollTop is rounded (and maps to several px of list by ratio past 8M px), so a position this code
  // set itself is kept exactly until the person scrolls: a row it puts on top is exactly on top
  let pin = null;
  function vTop() { // the position in the full list, in px, for the current scrollTop
    const s = scroll.scrollTop;
    if (pin && pin.s === s) return pin.v;
    pin = null;
    if (!scaled()) return s;
    const sMax = spacerH - viewH(), vMax = total * ROW - viewH();
    return sMax > 0 ? Math.min(vMax, (s / sMax) * vMax) : 0;
  }
  // every programmatic scroll goes through here, and moves the remembered top row with it
  function scrollToV(v) {
    const sMax = spacerH - viewH(), vMax = total * ROW - viewH();
    v = clamp(v, 0, Math.max(0, vMax));
    scroll.scrollTop = !scaled() ? v : vMax > 0 ? (v / vMax) * sMax : 0;
    pin = { s: scroll.scrollTop, v };
    topRow = v / ROW;
  }
  function setSpacer() {
    spacerH = total == null ? 12 * ROW : Math.min(total * ROW, MAX_SPACER);
    body.style.height = `${spacerH}px`;
  }
  // Reads the laid-out row and header heights (interface size, zoom, pixel ratio, a font that loaded all
  // change them) and resizes the scroll area to match. True if they changed.
  const snap = (px) => { const d = window.devicePixelRatio || 1; return Math.round(px * d) / d; };
  const layoutH = (el) => parseFloat(getComputedStyle(el).height);
  function measure() {
    const rowH = snap(layoutH(probe));
    const headH = layoutH(head);
    // snapped values are whole device pixels, so any real change is at least a third of a CSS px
    if (!(rowH > 0) || (Math.abs(rowH - ROW) < 0.01 && Math.abs(headH - HEAD) < 0.01)) return false;
    if (Math.abs(rowH - ROW) >= 0.01) body.style.setProperty('--vt-row-h', `${rowH}px`);
    ROW = rowH; HEAD = headH;
    setSpacer();
    return true;
  }
  // Before reading the scroll position: if the geometry changed since the last draw, the scroll
  // position no longer means the same row (it is mapped by ratio past 8M px), so put topRow back.
  function keepTopRow() {
    const g = `${ROW}|${HEAD}|${scroll.clientHeight}`;
    if (g === geom) return false;
    const first = !geom;
    geom = g;
    if (!first && total) scrollToV(topRow * ROW);
    return !first;
  }
  const updateSortHeads = () => {
    const s = sort();
    for (const th of $$('[data-col]', host)) {
      if (th.dataset.col === s.sort) th.setAttribute('aria-sort', s.dir === 'asc' ? 'ascending' : 'descending'); else th.removeAttribute('aria-sort');
    }
  };
  for (const b of $$('[data-sort]', host)) b.addEventListener('click', () => onSort(b.dataset.sort));

  function setTotal(n) {
    if (total === n) return;
    total = n;
    setSpacer();
    scroll.setAttribute('aria-rowcount', String(n + 1));
    onCount(n);
    emptyEl.hidden = n !== 0;
    foot.hidden = n === 0;
    jump.hidden = n <= 1000;
    jumpIn.max = String(n);
    if (!n) html(emptyEl, emptyHtml());
  }
  function trim() {
    for (const p of [...pages.keys()]) {
      if (pages.size <= MAX_PAGES) break;
      if (!visiblePages.includes(p)) pages.delete(p);
    }
  }
  async function load(p, refresh = false) {
    if (!refresh && pages.has(p)) return;
    if (!refresh) pages.set(p, null);
    const g = gen;
    try {
      const res = await fetchPage(p * PAGE, PAGE, asOf);
      if (g !== gen) return; // a newer search, filter or sort replaced this list
      if (asOf && res.asOf !== asOf) pages.clear(); // the server let our snapshot go: start over from its new order, same place
      asOf = res.asOf;
      pages.delete(p);
      pages.set(p, res.items);
      setTotal(res.total);
      trim();
      schedule();
    } catch (e) {
      if (g === gen && !refresh) pages.delete(p);
      console.warn(e);
    }
  }
  function requestPages(need) {
    clearTimeout(settle);
    settle = setTimeout(() => { for (const p of need) if (!pages.has(p)) load(p); }, SETTLE_MS);
  }
  function schedule() { if (!raf) raf = requestAnimationFrame(() => { raf = 0; draw(); }); }
  // full text for cells that may be cut short with an ellipsis
  const tip = (s) => `title="${esc(s)}"`;
  function rowHtml(c, i) {
    const age = ageClass(c), stale = age === 'stale';
    let sub;
    if (stale || c.state === 'unknown') sub = 'Last known place';
    else if (c.state === 'driving') sub = `${nf(U.speed(c.speedKph))} ${U.speedU()} now`;
    else if (c.state === 'charging') sub = `${nf(c.chargeKw, 1)} kW · ${c.chargeKw > 20 ? 'DC' : 'AC'}`;
    else if (c.state === 'parked') sub = `Parked ${dur((Date.now() - c.since) / 1000)}${c.plugged ? ' · plugged in' : ''}`;
    else sub = `Asleep ${dur((Date.now() - c.since) / 1000)}`;
    const low = c.soc < 20;
    const known = c.state !== 'unknown';
    const href = `#/cars/${encodeURIComponent(c.id)}/now`;
    const since = stale ? 'stale · last known state' : c.state === 'unknown' ? 'offline' : c.state === 'asleep' ? 'when it fell asleep' : '';
    const port = !known ? '<span class="faint">Unknown</span>'
      : c.plugged ? `<span>${icon('plug')}Plugged in</span>${stale ? '<span class="sub">last known</span>' : ''}` : '<span class="muted">Unplugged</span>';
    const tires = !known ? '<span class="faint">Unknown</span>'
      : c.tireWarning ? `<span class="warn">${icon('warn')}Low</span><span class="sub">${esc(TIRE_NAMES[c.tireWarning] || '')}</span>` : `<span class="muted">${icon('check')}OK</span>`;
    const meta = `${c.model}${SEP}${c.year} ${c.key}`; // the meta line as text, for its tooltip
    return `<div class="vt-row vt-cols${stale ? ' is-stale' : ''}${c.state === 'unknown' ? ' is-unknown' : ''}" role="row" aria-rowindex="${i + 2}" data-i="${i}" data-href="${href}">
      <div class="vt-td" role="cell"><div class="car-cell"><a class="car-name" href="${href}" ${tip(c.name)}>${esc(c.name)}</a><span class="car-meta" ${tip(meta)}>${carMeta(c)}</span></div></div>
      <div class="vt-td" role="cell">${pill(c.state, { stale })}</div>
      <div class="vt-td" role="cell"><div class="cell-batt"><div class="bar${stale || c.state === 'unknown' ? ' bar--muted' : c.state === 'charging' ? ' bar--warm' : low ? ' bar--low' : ''}" data-w="${c.soc}"><i></i></div><div class="cell-batt-text"><b>${c.soc}%</b><span>${tDist(c.rangeKm, 0)}</span></div></div></div>
      <div class="vt-td vt-x vt-x--range" role="cell"><span class="nowrap">${tDist(c.rangeKm, 0)}</span></div>
      <div class="vt-td" role="cell"><div class="cell-place"><b ${tip(c.place)}>${esc(c.place)}</b><span ${tip(sub)}>${esc(sub)}</span></div></div>
      <div class="vt-td vt-x vt-x--activity" role="cell"><div class="cell-text"><span ${tip(sub)}>${esc(sub)}</span></div></div>
      <div class="vt-td vt-x vt-x--port" role="cell"><div class="cell-text">${port}</div></div>
      <div class="vt-td vt-x vt-x--tires" role="cell"><div class="cell-text">${tires}</div></div>
      <div class="vt-td" role="cell"><div class="cell-since">${ageHtml(c)}${since ? `<span ${tip(since)}>${since}</span>` : ''}</div></div>
      <div class="vt-td vt-td--num vt-c-today" role="cell">${c.todayKm ? tDist(c.todayKm, 1) : `<span class="faint">0 ${U.distU()}</span>`}</div>
      <div class="vt-td row-chev" role="cell" aria-hidden="true">${icon('chev-r')}</div>
    </div>`;
  }
  function skelRow(i) {
    return `<div class="vt-row vt-cols vt-row--skel" role="row" aria-rowindex="${i + 2}" data-i="${i}" aria-busy="true">
      <div class="vt-td" role="cell"><span class="skel" data-skw="70"></span></div><div class="vt-td" role="cell"><span class="skel" data-skw="60"></span></div>
      <div class="vt-td" role="cell"><span class="skel" data-skw="80"></span></div><div class="vt-td vt-x vt-x--range" role="cell"></div>
      <div class="vt-td" role="cell"><span class="skel" data-skw="75"></span></div><div class="vt-td vt-x vt-x--activity" role="cell"></div>
      <div class="vt-td vt-x vt-x--port" role="cell"></div><div class="vt-td vt-x vt-x--tires" role="cell"></div>
      <div class="vt-td" role="cell"><span class="skel" data-skw="55"></span></div><div class="vt-td vt-c-today" role="cell"></div><div class="vt-td" role="cell"></div></div>`;
  }
  function placeRows(shift) {
    for (const r of body.children) r.style.transform = `translateY(${+r.dataset.i * ROW + shift}px)`;
    for (const s of $$('[data-skw]', body)) s.style.width = `${s.dataset.skw}%`;
  }
  function draw(again = 0) {
    if (total == null) {
      html(body, Array.from({ length: 12 }, (_, i) => skelRow(i)).join(''));
      setSpacer();
      placeRows(0);
      if (measure() && again < 2) draw(again + 1);
      return;
    }
    measure(); // the rows already on the page are laid out at the current size
    if (!keepTopRow()) topRow = vTop() / ROW; // else the size changed: same top row as before
    const s = scroll.scrollTop, v = vTop(), vh = viewH();
    const first = Math.max(0, Math.floor(v / ROW) - OVER);
    const last = Math.min(total - 1, Math.ceil((v + vh) / ROW) + OVER);
    const need = new Set(), vis = new Set();
    let out = '';
    for (let i = first; i <= last; i++) {
      const p = Math.floor(i / PAGE);
      vis.add(p);
      const items = pages.get(p);
      if (items === undefined) need.add(p);
      const c = items ? items[i - p * PAGE] : null;
      out += c ? rowHtml(c, i) : skelRow(i);
    }
    visiblePages = [...vis];
    for (const p of visiblePages) { if (pages.has(p)) { const it = pages.get(p); pages.delete(p); pages.set(p, it); } }
    const active = document.activeElement;
    const hadFocus = active && body.contains(active) ? +active.closest('[data-i]')?.dataset.i : null;
    html(body, out);
    placeRows(s - v);
    // the rows as drawn decide the row height; if it changed, draw again around the same top row
    if (measure() && again < 2) { if (hadFocus != null) focusIndex ??= hadFocus; draw(again + 1); return; }
    if (need.size) requestPages(need);
    const vFirst = Math.min(total, Math.floor(v / ROW + 0.01) + 1), vLast = Math.min(total, Math.floor((v + vh) / ROW));
    rangeEl.innerHTML = total ? `Rows <b>${nf(vFirst)}–${nf(Math.max(vFirst, vLast))}</b> of <b>${nf(total)}</b>` : '';
    const want = focusIndex ?? hadFocus;
    if (want != null) {
      const link = $(`[data-i="${want}"] .car-name`, body);
      if (link) { link.focus({ preventScroll: true }); focusIndex = null; }
    }
  }
  function goToRow(i, focus) {
    const v = vTop(), vh = viewH(), y = i * ROW;
    if (y < v) scrollToV(y);
    else if (y + ROW > v + vh) scrollToV(y + ROW - vh);
    if (focus) focusIndex = i;
    draw();
  }
  scroll.addEventListener('scroll', schedule, { passive: true });
  const ro = new ResizeObserver(schedule);
  ro.observe(scroll);
  const offDisplay = display.onChange(schedule); // row heights follow the root size: draw() re-measures
  body.addEventListener('click', (e) => {
    if (e.target.closest('a')) return;
    const row = e.target.closest('[data-href]');
    if (row) location.hash = row.dataset.href;
  });
  body.addEventListener('keydown', (e) => {
    if (!['ArrowDown', 'ArrowUp', 'PageDown', 'PageUp', 'Home', 'End'].includes(e.key)) return;
    const row = e.target.closest('[data-i]');
    if (!row || total == null) return;
    e.preventDefault();
    const pg = Math.max(1, Math.floor(viewH() / ROW) - 1);
    const delta = { ArrowDown: 1, ArrowUp: -1, PageDown: pg, PageUp: -pg }[e.key];
    goToRow(e.key === 'Home' ? 0 : e.key === 'End' ? total - 1 : clamp(+row.dataset.i + delta, 0, total - 1), true);
  });
  jumpIn.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' || !total) return;
    e.preventDefault();
    const n = clamp(Math.round(+jumpIn.value || 1), 1, total);
    jumpIn.value = String(n);
    scrollToV((n - 1) * ROW);
    focusIndex = n - 1;
    draw();
  });
  return {
    reset() { gen++; pages.clear(); total = null; asOf = null; topRow = 0; pin = null; clearTimeout(settle); scroll.scrollTop = 0; emptyEl.hidden = true; foot.hidden = false; updateSortHeads(); draw(); load(0); },
    // new values for the rows on screen, same snapshot, same scroll position
    refresh() { return Promise.all(visiblePages.filter((p) => pages.get(p)).map((p) => load(p, true))); },
    draw: schedule,
    destroy() { ro.disconnect(); offDisplay(); gen++; clearTimeout(settle); },
  };
}

// ------------------------------------------------------------------ car switcher (Ctrl/Cmd+K)
const switcher = (() => {
  const dlg = $('#switcher'), input = $('#sw-input'), list = $('#sw-list');
  let options = [], active = 0, seq = 0, deb = 0;

  function opt(c, n, q, { current = false, attention = null } = {}) {
    const stale = isStale(c);
    const stateText = `${(STATES[c.state] || STATES.unknown).label}${SEP}${c.soc}%`;
    // an attention row says what is wrong, then what is known (a silent car's age is already in the reason)
    const side = attention
      ? `<span class="sw-reason${attention.severity >= 6 ? ' sw-reason--danger' : ''}">${icon(attention.kind === 'tire' ? 'tire' : attention.kind === 'battery' ? 'battery' : 'clock', 'ico--sm')}${esc(attentionText(attention))}</span>${attention.kind === 'stale' || attention.kind === 'offline'
        ? `<span class="sw-known">Last known: ${esc(stateText.toLowerCase())}</span>` : ageHtml(c)}`
      : `<span>${stateText}${stale ? `${SEP}stale` : ''}</span>${ageHtml(c)}`;
    return `<div class="sw-opt${current ? ' is-current' : ''}" role="option" id="sw-opt-${n}" data-n="${n}" data-id="${esc(c.id)}" data-state="${esc(c.state)}" aria-selected="false">
      <span class="dot${stale ? ' is-stale' : ''}" aria-hidden="true"></span>
      <span class="sw-opt-main"><span class="sw-opt-name"><span class="sw-opt-name-text">${hl(c.name, q)}</span>${current ? '<span class="tag tag--plain">Current</span>' : ''}</span>
      <span class="sw-opt-meta">${carMeta(c, { q, place: true })}</span></span>
      <span class="sw-opt-side">${side}</span>
    </div>`;
  }
  async function render(q) {
    const my = ++seq;
    let markup = '';
    const cars = [];
    if (!q) {
      // recent cars first (the one on screen last, marked), then the cars that need someone to look
      const ids = recent.list().filter((id) => id !== currentCarId).slice(0, 3);
      const [rc, cur, att] = await Promise.all([
        Promise.all(ids.map((id) => carById(id).catch(() => null))),
        carById(currentCarId).catch(() => null),
        api.needsAttention({ limit: 4 }),
      ]);
      if (my !== seq) return;
      const recents = rc.filter(Boolean);
      const total = lastSummary ? lastSummary.total : null;
      markup += `<div role="group" aria-labelledby="sw-g1"><div class="sw-group-label" id="sw-g1"><span>Recent</span><span>${total ? `${nf(total)} cars · type to search all` : 'type to search all cars'}</span></div>`;
      recents.forEach((c) => { markup += opt(c, cars.length, ''); cars.push(c); });
      if (cur) { markup += opt(cur, cars.length, '', { current: true }); cars.push(cur); }
      markup += '</div>';
      if (att.items.length) {
        markup += `<div role="group" aria-labelledby="sw-g2"><div class="sw-group-label" id="sw-g2"><span>Needs attention <b class="sw-count">${nf(att.total)}</b></span><span>worst first</span></div>`;
        att.items.forEach((c) => { markup += opt(c, cars.length, '', { attention: c.attention }); cars.push(c); });
        markup += '</div>';
        if (att.total > att.items.length) markup += `<p class="sw-more">${nf(att.total - att.items.length)} more need attention. <a href="#/fleet?state=stale" data-close>Stale cars in All cars</a>.</p>`;
      }
    } else {
      const res = await api.listCars({ query: q, sort: 'name', limit: resultLimit() });
      if (my !== seq) return;
      if (!res.items.length) {
        markup = `<div class="empty"><div class="empty-icon">${icon('search')}</div><h3>No car matches “${esc(q)}”</h3><p>Search by name, the car's key, model, year or place.</p></div>`;
      } else {
        markup += `<div role="group" aria-labelledby="sw-g3"><div class="sw-group-label" id="sw-g3"><span>Matches</span><span>${nf(res.total)} ${res.total === 1 ? 'car' : 'cars'}</span></div>`;
        res.items.forEach((c) => { markup += opt(c, cars.length, q, { current: c.id === currentCarId }); cars.push(c); });
        markup += '</div>';
        if (res.total > res.items.length) markup += `<p class="sw-more">Showing ${res.items.length} of ${nf(res.total)}. Keep typing, or <a href="#/fleet?q=${encodeURIComponent(q)}" data-close>see all ${nf(res.total)} in All cars</a>.</p>`;
      }
    }
    options = cars;
    html(list, markup);
    for (const a of $$('[data-close]', list)) a.addEventListener('click', () => close());
    setActive(0);
  }
  // as many matches as fit in the dialog at its tallest (its max-height grows with the window), at least 6
  function resultLimit() {
    const maxH = parseFloat(getComputedStyle(dlg).maxHeight) || innerHeight * 0.7;
    const chrome = ($('.sw-search', dlg)?.offsetHeight || 0) + ($('.sw-foot', dlg)?.offsetHeight || 0);
    const optH = $('.sw-opt', list)?.offsetHeight || rpx(58);
    // the group label, the "Showing n of m" line and the list's padding
    return clamp(Math.floor((maxH - chrome - rpx(34) - rpx(44) - rpx(18)) / optH), 6, 40);
  }
  function setActive(n, wrap = true) {
    if (!options.length) { input.removeAttribute('aria-activedescendant'); return; }
    active = wrap ? (n + options.length) % options.length : clamp(n, 0, options.length - 1);
    for (const o of $$('.sw-opt', list)) o.setAttribute('aria-selected', String(+o.dataset.n === active));
    const el = $(`#sw-opt-${active}`);
    input.setAttribute('aria-activedescendant', `sw-opt-${active}`);
    el?.scrollIntoView({ block: 'nearest' });
  }
  function choose(n) {
    const c = options[n];
    if (!c) return;
    const { path } = parseHash();
    const m = /^\/cars\/[^/]+\/([a-z]+)/.exec(path);
    const page = m && CAR_PAGES.includes(m[1]) ? m[1] : 'now';
    carCache.set(c.id, c);
    recent.push(c.id);
    close();
    location.hash = `#/cars/${encodeURIComponent(c.id)}/${page}`;
  }
  function open() {
    if (dlg.open) return;
    input.value = '';
    dlg.showModal();
    input.focus();
    render('');
  }
  function close() { if (dlg.open) dlg.close(); }
  input.addEventListener('input', () => { clearTimeout(deb); deb = setTimeout(() => render(input.value.trim()), 90); });
  input.addEventListener('keydown', (e) => {
    const step = { ArrowDown: 1, ArrowUp: -1 }[e.key];
    const jumpBy = { PageDown: 5, PageUp: -5 }[e.key];
    if (step) { e.preventDefault(); setActive(active + step); }
    else if (jumpBy) { e.preventDefault(); setActive(active + jumpBy, false); }
    else if ((e.key === 'Home' || e.key === 'End') && options.length) { e.preventDefault(); setActive(e.key === 'Home' ? 0 : options.length - 1, false); }
    else if (e.key === 'Enter') { e.preventDefault(); choose(active); }
  });
  list.addEventListener('click', (e) => { const o = e.target.closest('.sw-opt'); if (o) choose(+o.dataset.n); });
  list.addEventListener('mousemove', (e) => { const o = e.target.closest('.sw-opt'); if (o && +o.dataset.n !== active) setActive(+o.dataset.n); });
  dlg.addEventListener('click', (e) => { if (e.target === dlg) close(); });
  $('#sw-all').addEventListener('click', () => close());
  $('#car-switch').addEventListener('click', open);
  document.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'k') { e.preventDefault(); dlg.open ? close() : open(); }
  });
  const k = IS_MAC ? '⌘ K' : 'Ctrl K';
  $('#car-switch-kbd').textContent = k;
  $('#sw-kbd-k').textContent = k;
  return { open, close };
})();

// ------------------------------------------------------------------ maps (MapLibre GL, loaded on first use)
let mlPromise = null;
function loadMapLibre() {
  if (!mlPromise) {
    mlPromise = import('./vendor/maplibre-gl/maplibre-gl.mjs').then((ml) => {
      ml.setWorkerUrl(new URL('./vendor/maplibre-gl/maplibre-gl-worker.mjs', import.meta.url).href);
      return ml;
    });
  }
  return mlPromise;
}
const OSM_ATTRIBUTION = '© <a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noopener">OpenStreetMap</a> contributors';
// thumbnails have room for the short form only
const OSM_ATTRIBUTION_SHORT = '© <a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noopener">OpenStreetMap</a>';
// Prototype limit: raster OpenStreetMap tiles are 256 px images, so they look soft on a 2x screen
// whatever the canvas does. The real server serves vector tiles, which are sharp at any pixel ratio.
function mapStyle(attribution = OSM_ATTRIBUTION) {
  return {
    version: 8,
    sources: {
      osm: { type: 'raster', tiles: ['https://tile.openstreetmap.org/{z}/{x}/{y}.png'], tileSize: 256, maxzoom: 19, attribution },
    },
    layers: [
      { id: 'bg', type: 'background', paint: { 'background-color': cssVar('--map-bg') } },
      {
        id: 'osm', type: 'raster', source: 'osm',
        paint: { 'raster-hue-rotate': 180, 'raster-saturation': -0.55, 'raster-contrast': 0.05, 'raster-brightness-min': 0.86, 'raster-brightness-max': 0.07, 'raster-fade-duration': 0 },
      },
      { id: 'tint', type: 'background', paint: { 'background-color': cssVar('--map-tint'), 'background-opacity': 0.22 } },
    ],
  };
}
// A map that stays sharp and in proportion:
// - MapLibre follows its container's size by itself (ResizeObserver), but not the device pixel ratio:
//   MapLibre 6.18 reads devicePixelRatio only in resize() (map.getPixelRatio() is setPixelRatio()'s
//   override, else devicePixelRatio), and nothing watches it. So on a display change the map is resized,
//   which gives the canvas the new pixel ratio. (setPixelRatio() would pin a ratio; we never pin one.)
// - Line widths and circle sizes are pixels in MapLibre: layers added with handle.addLayer(def, sized)
//   take them as functions of rpx(), and get them again when the root size changes.
// - The camera framing (handle.frame) uses padding in rem-scaled px or measured from the cards on top
//   of the map; it runs again after a resize or a display change, until the person moves the map.
async function createMap(container, { center, zoom = 15, bounds = null, interactive = true, attribution, ctx }) {
  const ml = await loadMapLibre();
  const map = new ml.Map({
    container, style: mapStyle(attribution), center, zoom, attributionControl: false, interactive,
    cooperativeGestures: false, dragRotate: false, pitchWithRotate: false, fadeDuration: 0,
    // Tiles load as <img> (allowed by the pack's img-src) instead of fetch (connect-src is 'self' only).
    refreshExpiredTiles: false,
    ...(bounds ? { bounds } : {}),
  });
  map.addControl(new ml.AttributionControl({ compact: false }), 'bottom-right');
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  map.touchZoomRotate?.disableRotation();
  container.dataset.mapReady = '0';
  const ready = new Promise((res) => map.once('load', res));
  map.on('idle', () => { container.dataset.mapReady = '1'; });
  map.on('error', (e) => { console.warn('map', e && e.error ? e.error.message : e); });

  const sized = []; // [layer id, paint property, () => value now]
  let framing = null, userMoved = false;
  map.on('movestart', (e) => { if (e.originalEvent instanceof Event) userMoved = true; });
  map.on('resize', () => { if (framing && !userMoved) framing(); });
  const offDisplay = display.onChange(() => {
    for (const [id, prop, value] of sized) if (map.getLayer(id)) map.setPaintProperty(id, prop, value());
    map.resize(); // new canvas size and pixel ratio; fires 'resize', which frames the view again
    map.triggerRepaint();
  });
  ctx.onCleanup(() => { offDisplay(); map.remove(); });
  return {
    map, ml, ready,
    addLayer(def, sizedPaint = {}) {
      const paint = { ...def.paint };
      for (const [prop, value] of Object.entries(sizedPaint)) { paint[prop] = value(); sized.push([def.id, prop, value]); }
      map.addLayer({ ...def, paint });
    },
    /** f() places the camera; it runs now and again after resizes until the person moves the map */
    frame(f) { framing = f; f(); },
  };
}
// camera padding that keeps a card on top of the map (top-left) clear, plus a margin in rem-scaled px
function padClear(mapEl, card, { top = 30, right = 30, bottom = 30, left = 30, beside = false } = {}) {
  const pad = { top: rpx(top), right: rpx(right), bottom: rpx(bottom), left: rpx(left) };
  if (card) {
    const m = mapEl.getBoundingClientRect(), c = card.getBoundingClientRect();
    if (beside) pad.left = Math.max(pad.left, c.right - m.left + rpx(left));
    else pad.top = Math.max(pad.top, c.bottom - m.top + rpx(top));
  }
  // MapLibre needs room left for the view itself
  const w = mapEl.clientWidth, h = mapEl.clientHeight;
  if (pad.left + pad.right > w * 0.8) pad.left = Math.max(0, w * 0.8 - pad.right);
  if (pad.top + pad.bottom > h * 0.8) pad.top = Math.max(0, h * 0.8 - pad.bottom);
  return pad;
}
const SVG_NS = 'http://www.w3.org/2000/svg';
// tone: live | warm (charging) | asleep | muted (stale or offline). heading: degrees, draws an arrow.
function addCarMarker(ml, map, lngLat, { tone = 'live', heading = null } = {}) {
  const el = document.createElement('div');
  el.className = `car-marker car-marker--${tone}`;
  el.setAttribute('aria-hidden', 'true');
  if (heading != null) {
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('class', 'car-marker-heading');
    const p = document.createElementNS(SVG_NS, 'path');
    p.setAttribute('d', 'M12 4 18.5 19 12 15.2 5.5 19z');
    svg.append(p);
    el.append(svg);
    el.style.setProperty('--rot', `${heading}deg`);
  }
  return new ml.Marker({ element: el }).setLngLat(lngLat).addTo(map);
}
function routePin(ml, map, lngLat, letter) {
  const el = document.createElement('div');
  el.className = `route-pin route-pin--${letter.toLowerCase()}`;
  el.textContent = letter;
  el.setAttribute('aria-hidden', 'true');
  return new ml.Marker({ element: el }).setLngLat(lngLat).addTo(map);
}
function addRouteLayers(m, coords, id = 'route', { thin = false } = {}) {
  m.map.addSource(id, { type: 'geojson', lineMetrics: true, data: { type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: coords } } });
  const line = { type: 'line', source: id, layout: { 'line-cap': 'round', 'line-join': 'round' } };
  m.addLayer({ ...line, id: `${id}-glow`, paint: { 'line-color': cssVar('--live'), 'line-opacity': 0.22 } },
    { 'line-width': () => rpx(thin ? 9 : 16), 'line-blur': () => rpx(thin ? 4 : 8) });
  m.addLayer({ ...line, id: `${id}-casing`, paint: { 'line-color': cssVar('--map-casing'), 'line-opacity': 0.85 } },
    { 'line-width': () => rpx(thin ? 5 : 8) });
  m.addLayer({ ...line, id: `${id}-line`, paint: { 'line-gradient': ['interpolate', ['linear'], ['line-progress'], 0, cssVar('--live-2'), 0.6, cssVar('--live'), 1, cssVar('--live-3')] } },
    { 'line-width': () => rpx(thin ? 2.6 : 4.2) });
}
const boundsOf = (coords) => {
  const lo = coords.map((c) => c[0]), la = coords.map((c) => c[1]);
  return [[Math.min(...lo), Math.min(...la)], [Math.max(...lo), Math.max(...la)]];
};
// a small map that only shows a trace: no controls, no gestures, the same dark style as the big maps
async function mountThumbMap(el, coords, ctx, { live = false } = {}) {
  if (!coords || coords.length < 2) return;
  try {
    const m = await createMap(el, { center: coords[0], bounds: boundsOf(coords), interactive: false, attribution: OSM_ATTRIBUTION_SHORT, ctx });
    await m.ready;
    if (!ctx.alive()) return;
    addRouteLayers(m, coords, 'thumb', { thin: true });
    m.map.addSource('thumb-ends', { type: 'geojson', data: { type: 'FeatureCollection', features: [
      { type: 'Feature', properties: { k: 'a' }, geometry: { type: 'Point', coordinates: coords[0] } },
      { type: 'Feature', properties: { k: 'b' }, geometry: { type: 'Point', coordinates: coords[coords.length - 1] } },
    ] } });
    m.addLayer({ id: 'thumb-ends', type: 'circle', source: 'thumb-ends', paint: {
      'circle-color': ['match', ['get', 'k'], 'a', cssVar('--bg-1'), cssVar('--live')],
      'circle-stroke-color': ['match', ['get', 'k'], 'a', cssVar('--live'), cssVar('--text')],
    } }, {
      'circle-radius': () => ['match', ['get', 'k'], 'a', rpx(4), rpx(live ? 5.5 : 4.5)],
      'circle-stroke-width': () => rpx(2),
    });
    const bounds = boundsOf(coords);
    m.frame(() => m.map.fitBounds(bounds, { padding: { top: rpx(22), bottom: rpx(30), left: rpx(22), right: rpx(22) }, duration: 0 }));
    el.classList.add('has-map');
  } catch (e) { console.warn(e); }
}

// ------------------------------------------------------------------ NOW
function gaugeSvg(soc, limit) {
  const cx = 150, cy = 150, r = 118, a0 = 135, sweep = 270;
  const pt = (deg, rr) => { const a = (deg * Math.PI) / 180; return [cx + rr * Math.cos(a), cy + rr * Math.sin(a)]; };
  const arc = (from, to, rr) => {
    const [x1, y1] = pt(from, rr), [x2, y2] = pt(to, rr);
    return `M${x1.toFixed(2)} ${y1.toFixed(2)} A${rr} ${rr} 0 ${to - from > 180 ? 1 : 0} 1 ${x2.toFixed(2)} ${y2.toFixed(2)}`;
  };
  const track = arc(a0, a0 + sweep, r);
  let ticks = '';
  for (let p = 0; p <= 100; p += 5) {
    const deg = a0 + (sweep * p) / 100;
    const major = p % 25 === 0;
    const [x1, y1] = pt(deg, r - (major ? 25 : 20)), [x2, y2] = pt(deg, r - 13);
    ticks += `<line class="gauge-tick${major ? ' gauge-tick--major' : ''}" x1="${x1.toFixed(1)}" y1="${y1.toFixed(1)}" x2="${x2.toFixed(1)}" y2="${y2.toFixed(1)}"/>`;
  }
  let lim = '', label = '';
  if (limit) {
    const deg = a0 + (sweep * limit) / 100;
    const [x1, y1] = pt(deg, r - 10), [x2, y2] = pt(deg, r + 10), [lx, ly] = pt(deg, r + 24);
    lim = `<line class="gauge-limit" x1="${x1.toFixed(1)}" y1="${y1.toFixed(1)}" x2="${x2.toFixed(1)}" y2="${y2.toFixed(1)}"/>`;
    // the number is HTML placed on the dial (in % of the 300-unit box), so it keeps the text-size floor
    label = `<span class="gauge-marks" aria-hidden="true"><span class="gauge-limit-label" data-x="${(lx / 3).toFixed(2)}" data-y="${(ly / 3).toFixed(2)}">${limit}</span></span>`;
  }
  return `<svg class="gauge" viewBox="0 0 300 300" aria-hidden="true" focusable="false">
    <path class="gauge-track" d="${track}"/>
    ${ticks}
    <path class="gauge-fill" d="${track}" pathLength="100" stroke-dasharray="${clamp(soc, 0, 100)} 100"/>
    ${lim}
  </svg>${label}`;
}
// Battery through today, from the drives and charges on the timeline (shown in the gauge panel when a
// large screen leaves it height to spare). SVG, so it is sharp at any pixel ratio.
function socTodayHtml(tl, socNow) {
  if (!tl) return '';
  const pts = [];
  for (const sg of tl.segments) {
    const d = sg.detail;
    if ((sg.kind === 'driving' || sg.kind === 'charging') && d && typeof d === 'object' && d.socStart != null) pts.push([sg.start, d.socStart], [sg.end, d.socEnd]);
  }
  if (!pts.length) return '';
  // the line stops where the received data stops (a car that went silent is not drawn up to now)
  const end = Math.max(pts[pts.length - 1][0], tl.receivedUntil || tl.now || tl.end), len = tl.end - tl.start;
  pts.unshift([tl.start, pts[0][1]]);
  pts.push([end, socNow ?? pts[pts.length - 1][1]]);
  const X = (t) => clamp(((t - tl.start) / len) * 1000, 0, 1000).toFixed(1), Y = (v) => (300 - clamp(v, 0, 100) * 3).toFixed(1);
  const line = pts.map(([t, v], i) => `${i ? 'L' : 'M'}${X(t)} ${Y(v)}`).join(' ');
  const pc = (t) => clamp(((t - tl.start) / len) * 100, 0, 100);
  const strip = tl.segments.filter((sg) => sg.kind === 'driving' || sg.kind === 'charging')
    .map((sg) => `<i data-state="${sg.kind}" data-x="${pc(sg.start).toFixed(2)}" data-w="${Math.max(0.4, pc(sg.end) - pc(sg.start)).toFixed(2)}"></i>`).join('');
  return `<div class="gauge-today" role="img" aria-label="Battery today: ${pts[0][1]}% at midnight, ${pts[pts.length - 1][1]}% now">
    <div class="gauge-today-head" aria-hidden="true"><span>Battery today</span><span><b>${pts[0][1]}%</b> → <b>${pts[pts.length - 1][1]}%</b></span></div>
    <div class="gauge-today-plot" aria-hidden="true"><span class="gauge-today-y" data-y="0">100%</span><span class="gauge-today-y" data-y="50">50%</span><svg viewBox="0 0 1000 300" preserveAspectRatio="none" focusable="false">
      <path class="gauge-today-area" d="${line} L${X(end)} 300 L0 300 Z"/><path class="gauge-today-line" d="${line}"/>
      ${tl.now ? `<line class="gauge-today-now" x1="${X(tl.now)}" y1="0" x2="${X(tl.now)}" y2="300"/>` : ''}</svg></div>
    <div class="gauge-today-strip" aria-hidden="true">${strip}</div>
    <div class="gauge-today-axis" aria-hidden="true"><span>12 AM</span><span>6 AM</span><span>12 PM</span><span>6 PM</span><span>12 AM</span></div>
  </div>`;
}
function tiresHtml(t) {
  const thr = t.placard - 0.15; // about 2 psi under the door placard
  const low = (v) => v < thr;
  const tire = (label, v, right) => `<div class="tire${right ? ' tire--r' : ''}${low(v) ? ' is-low' : ''}">
      <span class="tire-pos">${label}</span>
      <span class="tire-val">${withUnit(nf(U.press(v), U.pressD()), U.pressU())}</span>
      ${low(v) ? `<span class="tire-note">${icon('warn', 'ico--sm')} ${nf(U.press(t.placard - v), U.pressD())} ${U.pressU()} low</span>` : ''}
    </div>`;
  const w = (v) => (low(v) ? ' is-low' : '');
  return `<div class="tires">
    ${tire('Front left', t.fl, false)}
    <svg class="car-top" viewBox="0 0 64 118" aria-hidden="true" focusable="false">
      <rect class="wheel${w(t.fl)}" x="2" y="17" width="9" height="22" rx="3"/><rect class="wheel${w(t.fr)}" x="53" y="17" width="9" height="22" rx="3"/>
      <rect class="wheel${w(t.rl)}" x="2" y="79" width="9" height="22" rx="3"/><rect class="wheel${w(t.rr)}" x="53" y="79" width="9" height="22" rx="3"/>
      <rect class="body" x="9" y="3" width="46" height="112" rx="19"/>
      <path class="glass" d="M15.5 36 Q32 28 48.5 36 L45.5 52 Q32 48 18.5 52 Z"/>
      <path class="glass" d="M18.5 86 Q32 90 45.5 86 L47.5 98 Q32 103 16.5 98 Z"/>
    </svg>
    ${tire('Front right', t.fr, true)}
    ${tire('Rear left', t.rl, false)}
    ${tire('Rear right', t.rr, true)}
  </div>`;
}
// Temperatures on one plain 0–40 °C (32–104 °F) scale: a thin track, the shaded normal range and a tick
const TEMP_LO = 0, TEMP_HI = 40;
const TEMP_NORMAL = { Cabin: [18, 24], Battery: [15, 35] };
const tempPct = (c) => clamp(((c - TEMP_LO) / (TEMP_HI - TEMP_LO)) * 100, 0, 100);
function tempsHtml(t) {
  const rows = [['Outside', t.outsideC], ['Cabin', t.cabinC], ['Battery', t.batteryC]];
  return `<div class="temp-rows">${rows.map(([l, c]) => {
    const band = TEMP_NORMAL[l];
    return `<div class="temp-row"><span class="temp-label">${l}</span><span class="temp-val">${fTemp(c)}</span>
      <span class="temp-scale" aria-hidden="true">${band ? `<i class="temp-band" data-x="${tempPct(band[0]).toFixed(1)}" data-w="${(tempPct(band[1]) - tempPct(band[0])).toFixed(1)}"></i>` : ''}<i class="temp-tick" data-x="${tempPct(c).toFixed(1)}"></i></span></div>`;
  }).join('')}</div>
  <div class="temp-axis" aria-hidden="true"><span>${nf(U.temp(TEMP_LO))}${U.tempU()}</span><span class="temp-axis-key"><i></i>normal range</span><span>${nf(U.temp(TEMP_HI))}${U.tempU()}</span></div>`;
}
// fallback drawing when a map cannot load
function routeThumb(trace) {
  if (!trace || trace.length < 2) return '';
  const W = 168, H = 150, P = 20;
  const lat0 = trace[0][1] * Math.PI / 180;
  const xs = trace.map((p) => p[0] * Math.cos(lat0)), ys = trace.map((p) => p[1]);
  const minX = Math.min(...xs), maxX = Math.max(...xs), minY = Math.min(...ys), maxY = Math.max(...ys);
  const s = Math.min((W - 2 * P) / Math.max(1e-9, maxX - minX), (H - 2 * P) / Math.max(1e-9, maxY - minY));
  const ox = (W - (maxX - minX) * s) / 2, oy = (H - (maxY - minY) * s) / 2;
  const pts = trace.map((_, i) => [ox + (xs[i] - minX) * s, H - (oy + (ys[i] - minY) * s)]);
  const d = pts.map((p, i) => `${i ? 'L' : 'M'}${p[0].toFixed(1)} ${p[1].toFixed(1)}`).join(' ');
  const a = pts[0], b = pts[pts.length - 1];
  return `<svg class="thumb-fallback" viewBox="0 0 ${W} ${H}" aria-hidden="true" focusable="false"><path class="thumb-route-glow" d="${d}"/><path class="thumb-route" d="${d}"/><circle class="thumb-start" cx="${a[0].toFixed(1)}" cy="${a[1].toFixed(1)}" r="5"/><circle class="thumb-end" cx="${b[0].toFixed(1)}" cy="${b[1].toFixed(1)}" r="5.5"/></svg>`;
}
function curveThumb(curve) {
  if (!curve || curve.length < 2) return '';
  const W = 168, H = 150, P = 18;
  const max = Math.max(...curve) || 1;
  const pts = curve.map((v, i) => [P / 2 + (i / (curve.length - 1)) * (W - P), H - P - (v / max) * (H - 2 * P - 10)]);
  const d = pts.map((p, i) => `${i ? 'L' : 'M'}${p[0].toFixed(1)} ${p[1].toFixed(1)}`).join(' ');
  let grid = '';
  for (let y = 14; y < H; y += 22) grid += `<line class="thumb-grid" x1="0" y1="${y}" x2="${W}" y2="${y}"/>`;
  return `<svg viewBox="0 0 ${W} ${H}" aria-hidden="true" focusable="false">${grid}<path class="thumb-curve" d="${d} L${pts[pts.length - 1][0].toFixed(1)} ${H - P} L${pts[0][0].toFixed(1)} ${H - P} Z"/><path class="thumb-curve-line" d="${d}"/></svg>`;
}
const MAP_ZOOM = { highway: 13, street: 15, site: 15.6 };
const statesLegend = (keys) => `<div class="mini-legend" aria-hidden="true">${keys.map((k) => `<span><i data-state="${k}"></i>${k === 'pending' ? 'Not received yet' : STATES[k].label}</span>`).join('')}</div>`;

async function viewNow(ctx) {
  const [carId] = ctx.params;
  const { view } = ctx;
  recent.push(carId);
  const preview = ['parked', 'driving', 'charging'].includes(ctx.query.get('state')) ? ctx.query.get('state') : null;
  view.classList.add('view--full'); // on large screens the page fills the window (app.css "wide")
  const car0 = await carById(carId);
  if (!car0) throw Object.assign(new Error('No car with this address'), { status: 404 });
  setShell({ page: 'now', carId, crumbs: [{ label: 'All cars', href: '#/fleet' }, { label: car0.name, href: `#/cars/${carId}/now` }, { label: 'Now' }] });
  html(view, `
    <div class="page-head"><div><span class="skel skel--title"></span><p class="page-sub"><span class="skel skel--text skel--w320"></span></p></div></div>
    <div class="now-hero"><div class="panel skel--block"><span class="skel skel--block"></span></div><div class="panel map-panel"><span class="skel skel--block"></span></div></div>
    <div class="now-row">${'<div class="panel panel-pad"><span class="skel skel--text"></span><br><span class="skel skel--num"></span></div>'.repeat(4)}</div>`);
  const [now, tl] = await Promise.all([api.getNow(carId, { preview: preview || 'parked' }), api.getTimeline(carId, undefined, { preview: preview || 'parked' }).catch(() => null)]);
  if (!ctx.alive()) return;
  const car = now.car;
  const st = now.state;
  const isOwnerDemo = carId === 'model-y';
  const age = ageClass({ state: st, stale: now.stale, lastSeen: now.lastSeen });
  const stale = age === 'stale';
  const offline = st === 'unknown', asleep = st === 'asleep';
  const chg = now.charge || {};
  const charging = st === 'charging';
  const driving = st === 'driving';
  showCarInSidebar(car, { state: st, stale });
  carCache.set(car.id, car);

  // what is shown is the newest data; when that is old, say so and measure everything up to it
  const lastKnown = stale ? 'Last known: ' : '';
  const statusPills = [];
  const pillCls = (tone) => `pill pill--lg pill--${tone}${stale ? ' is-stale' : ''}`;
  if (driving) {
    statusPills.push(`<span class="${pillCls('live')}">${icon('steer')}${stale ? 'Last known: driving' : 'Driving'} · ${nf(U.speed(now.drive.speedKph))} ${U.speedU()}</span>`);
    if (!stale) statusPills.push(`<span class="pill pill--lg pill--plain">Gear ${esc(now.drive.gear)}</span>`);
  } else if (charging) {
    statusPills.push(`<span class="${pillCls('warm')}">${icon('bolt')}${stale ? 'Last known: charging' : 'Charging'} · ${nf(chg.powerKw, 1)} kW ${esc(chg.kind || '')}</span>`);
    if (!stale) statusPills.push(`<span class="pill pill--lg pill--plain">${icon('clock')}${dur(chg.minutesToLimit * 60)} to ${now.limit}%</span>`);
  } else {
    statusPills.push(chg.plugged ? `<span class="${pillCls('warm')}">${icon('plug')}${lastKnown}${stale ? 'plugged in' : 'Plugged in'}</span>` : `<span class="pill pill--lg pill--plain${stale ? ' is-stale' : ''}">${icon('plug')}${lastKnown}${stale ? 'unplugged' : 'Unplugged'}</span>`);
    if (chg.status === 'complete' && chg.plugged && !stale && !offline) statusPills.push(`<span class="pill pill--lg pill--live">${icon('check')}Charge complete</span>`);
  }
  const driveMs = driving ? Math.max(0, now.lastSeen - now.since) : 0;
  let gaugeStats;
  if (driving) {
    gaugeStats = [['Speed', fSpeed(now.drive.speedKph)], ['Power', withUnit(nf(now.drive.powerKw, 0), 'kW')], ['This drive', fDist(now.drive.tripKm, 1)]];
  } else if (charging) {
    gaugeStats = [['Power', withUnit(nf(chg.powerKw, 1), 'kW'), chg.volts ? `${chg.volts} V · ${chg.amps} A` : ''], ['Adding', withUnit('+' + nf(U.dist(chg.rateKph), 0), `${U.distU()}/h`)], ['Added', fKwh(chg.addedKwh), `since ${tm(chg.startedAt)}`]];
  } else {
    gaugeStats = [['Charge limit', withUnit(nf(now.limit), '%')], ['In the pack', fKwh((now.soc / 100) * now.packKwh, 0)], ['Full range', fDist(now.fullKm, 0)]];
  }
  const sinceLabel = stale || offline ? `Last data ${whenText(now.lastSeen)}`
    : driving ? `Driving since ${tm(now.since)}` : charging ? `Charging since ${tm(now.since)}`
      : `${STATES[st]?.label || 'Parked'} since ${whenText(now.since)}`;
  const placeIcon = driving ? 'route' : now.location.place === 'Home' ? 'home' : 'pin';
  const gaugeTone = stale ? 'stale' : offline ? 'offline' : charging ? 'charging' : now.soc < 20 ? 'low' : asleep ? 'asleep' : 'live';
  const daySegs = tl ? tl.segments : [];
  const dayLen = tl ? tl.end - tl.start : 1;
  const dayPct = (ms) => (tl ? clamp(((ms - tl.start) / dayLen) * 100, 0, 100) : 0);
  const ld = now.latestDrive, lc = now.latestCharge;
  const liveDrive = driving && !stale && now.drive && now.drive.id;
  const dimCls = stale || offline ? ' is-stale' : '';
  const banner = stale || offline ? `<div class="stale-banner${offline ? ' stale-banner--offline' : ''}" role="status">${icon(offline ? 'question' : 'clock')}
      <p><b>${offline ? 'Offline. ' : ''}No data for <span data-for-since="${now.lastSeen}">${forText(now.lastSeen)}</span>.</b> Showing the last known state from ${esc(whenText(now.lastSeen))}. It updates when the car's next log arrives.</p></div>` : '';

  html(view, `
    <div class="page-head now-head">
      <div>
        <span class="eyebrow">Now</span>
        <h1 class="page-title">${esc(car.name)} ${pill(st, { lg: true, stale })}</h1>
        <p class="page-sub"><span>${esc(car.model)} ${esc(car.trim)}${SEP}${car.year}</span><span class="sep"></span><span>Key ${keyChip(car.key)}</span><span class="sep"></span>
          ${ageHtml({ state: st, stale: now.stale, lastSeen: now.lastSeen }, { label: 'Last seen&nbsp;', title: 'Logs arrive about a minute after the car records them.', ts: 'lastSeen' })}</p>
      </div>
      ${isOwnerDemo ? `<div class="page-actions"><span class="preview-label" id="pv-label">Preview state</span><div class="seg preview-switch" role="group" aria-labelledby="pv-label">
        ${['parked', 'driving', 'charging'].map((s) => `<button type="button" class="seg-btn" data-preview="${s}" data-state="${s}" aria-pressed="${st === s}"><span class="dot"></span>${STATES[s].label}</button>`).join('')}
      </div></div>` : ''}
    </div>
    ${banner}

    <div class="now-hero${dimCls}">
      <section class="panel gauge-panel" aria-label="Battery">
        <div class="panel-head"><h2 class="panel-title">${icon('battery')}Battery</h2><span class="faint">${esc(sinceLabel)}</span></div>
        <div class="gauge-main">
          <div class="gauge-wrap gauge--${gaugeTone}" role="img" aria-label="${stale || offline ? 'Last known battery' : 'Battery'} ${now.soc} percent, ${tDist(now.rangeKm, 0)} range, charge limit ${now.limit} percent">
            ${gaugeSvg(now.soc, now.limit)}
            <div class="gauge-center"><div class="gauge-soc">${now.soc}<sup>%</sup></div><div class="gauge-range">${fDist(now.rangeKm, 0)}</div><div class="gauge-caption">${stale || offline ? 'last known range' : 'estimated range'}</div></div>
          </div>
          <div class="gauge-status">${statusPills.join('')}</div>
        </div>
        ${socTodayHtml(tl, now.soc)}
        <div class="gauge-stats">${gaugeStats.map(([l, v, s]) => `<div class="metric metric--sm"><span class="metric-label">${l}</span><span class="metric-value">${v}</span>${s ? `<span class="metric-sub">${esc(s)}</span>` : ''}</div>`).join('')}</div>
      </section>

      <section class="panel map-panel" aria-label="Location">
        <div class="map" id="now-map" role="img" aria-label="Map: ${esc(now.location.place)}"></div>
        <div class="map-overlay map-overlay--tl">
          <div class="glass-card place-card">
            <div class="place-card-top"><span class="place-icon${charging ? ' place-icon--warm' : stale || offline ? ' place-icon--muted' : ''}">${icon(placeIcon)}</span>
              <div><div class="place-name">${esc(now.location.place)}</div><div class="place-detail">${esc(now.location.detail)}</div></div></div>
            <dl class="place-card-rows">
              ${driving ? `<dt>Started</dt><dd>${tm(now.since)}${now.drive.from ? ` from ${esc(now.drive.from)}` : ''}</dd><dt>This drive</dt><dd>${tDist(now.drive.tripKm, 1)}${SEP}${dur(driveMs / 1000)}</dd>`
                : charging ? `<dt>Charging since</dt><dd>${tm(chg.startedAt)}</dd>${stale ? '' : `<dt>Reaches ${now.limit}%</dt><dd>about ${tm(Date.now() + chg.minutesToLimit * 60000)}</dd>`}`
                : `<dt>Here since</dt><dd>${esc(whenText(now.since))}</dd><dt>Charge port</dt><dd>${chg.plugged ? (chg.status === 'complete' ? 'Plugged in · complete' : 'Plugged in') : 'Unplugged'}</dd>`}
              <dt>Position</dt><dd>${ageHtml({ state: st, stale: now.stale, lastSeen: now.lastSeen }, { ts: 'lastSeen' })}</dd>
            </dl>
          </div>
        </div>
      </section>

      <section class="panel today-tl" aria-label="Today's timeline">
        <div class="panel-head"><h2 class="panel-title">${icon('timeline')}Today, newest first</h2><a class="panel-link" href="#/cars/${encodeURIComponent(carId)}/timeline">Timeline ${icon('chev-r', 'ico--sm')}</a></div>
        <div class="today-tl-body"><div class="tl-list today-tl-list">${tl ? timelineRows(tl).reverse().map((r) => timelineRowHtml(r, carId, { compact: true })).join('') : ''}</div></div>
      </section>
    </div>

    <div class="now-panels">
    <div class="now-row${dimCls}">
      <section class="panel panel-pad" aria-label="Temperatures">
        <div class="panel-head"><h2 class="panel-title">${icon('thermo')}Temperatures</h2></div>
        ${tempsHtml(now.temps)}
      </section>
      <section class="panel panel-pad" aria-label="Tire pressures">
        <div class="panel-head"><h2 class="panel-title">${icon('tire')}Tires</h2><span class="faint">placard ${nf(U.press(now.tires.placard), U.pressD())} ${U.pressU()}</span></div>
        ${tiresHtml(now.tires)}
      </section>
      <section class="panel panel-pad" aria-label="Vehicle">
        <div class="panel-head"><h2 class="panel-title">${icon('car', '')}Vehicle</h2></div>
        <dl class="kv">
          <div class="kv-row"><dt>${icon('battery')}12 V battery</dt><dd>${nf(now.lv.volts, 1)} V <span class="${now.lv.ok ? 'ok' : 'warn'}">· ${now.lv.ok ? 'healthy' : 'low'}</span></dd></div>
          <div class="kv-row"><dt>${icon('odo')}Odometer</dt><dd>${fDist(now.odometerKm, 0)}</dd></div>
          <div class="kv-row"><dt>${icon('plug')}Charge port</dt><dd>${chg.plugged ? 'Plugged in' : 'Unplugged'}</dd></div>
          <div class="kv-row" title="The newest log covers the car's data up to ${esc(ago(now.lastSeen))}."><dt>${icon('live')}Last log received</dt><dd><span data-ago="${now.receivedAt}" data-ts="receivedAt">${ago(now.receivedAt)}</span></dd></div>
        </dl>
      </section>
      <section class="panel panel-pad" aria-label="Today">
        <div class="panel-head"><h2 class="panel-title">${icon('calendar')}Today</h2><a class="panel-link" href="#/cars/${encodeURIComponent(carId)}/timeline">Timeline ${icon('chev-r', 'ico--sm')}</a></div>
        <div class="today-grid">
          <div class="metric metric--sm"><span class="metric-label">Drives</span><span class="metric-value">${nf(now.today.drives)}</span></div>
          <div class="metric metric--sm"><span class="metric-label">Distance</span><span class="metric-value">${fDist(now.today.distanceKm, 1)}</span></div>
          <div class="metric metric--sm"><span class="metric-label">Energy used</span><span class="metric-value">${fKwh(now.today.energyKwh, 1)}</span></div>
          <div class="metric metric--sm"><span class="metric-label">Charge added</span><span class="metric-value">${fKwh(now.today.chargeAddedKwh, 1)}</span></div>
        </div>
        <div class="daystrip-wrap">
          <div class="daystrip" role="img" aria-label="Today's states from midnight to now">${daySegs.map((s) => `<i data-state="${s.kind}" data-x="${dayPct(s.start).toFixed(2)}" data-w="${Math.max(0.4, dayPct(s.end) - dayPct(s.start)).toFixed(2)}"></i>`).join('')}${tl && tl.now && tl.receivedUntil < tl.now ? `<i data-state="pending" data-x="${dayPct(tl.receivedUntil).toFixed(2)}" data-w="${Math.max(0.3, dayPct(tl.now) - dayPct(tl.receivedUntil)).toFixed(2)}"></i>` : ''}</div>
          ${tl && tl.now ? `<i class="daystrip-now" data-x="${dayPct(tl.now).toFixed(2)}" title="Now"></i>` : ''}
        </div>
        <div class="daystrip-axis" aria-hidden="true"><span>12 AM</span><span>6 AM</span><span>12 PM</span><span>6 PM</span><span>12 AM</span></div>
        ${statesLegend(['driving', 'charging', 'parked', 'asleep'])}
      </section>
    </div>

    <div class="now-row now-row--2">
      ${liveDrive ? `<section class="panel recent-card recent-card--live" aria-label="Drive in progress">
        <div class="recent-thumb" id="thumb-drive">${routeThumb(now.drive.trace)}</div>
        <div class="recent-body">
          <div class="recent-foot"><span class="eyebrow eyebrow--live"><span class="live-dot" aria-hidden="true"></span>Drive in progress</span><a class="panel-link" href="#/cars/${encodeURIComponent(carId)}/drives/${encodeURIComponent(now.drive.id)}">Follow live ${icon('chev-r', 'ico--sm')}</a></div>
          <div class="recent-title">${esc(now.drive.from || 'Start')} ${icon('arrow-r')} ${esc(now.location.place)}</div>
          <div class="recent-when">${parts([`Started ${tm(now.since)}${now.drive.from ? ` from ${esc(now.drive.from)}` : ''}`, tDist(now.drive.tripKm, 1), dur(driveMs / 1000)])}</div>
          <div class="recent-stats">
            <div class="metric metric--sm"><span class="metric-label">Distance</span><span class="metric-value">${fDist(now.drive.tripKm, 1)}</span></div>
            <div class="metric metric--sm"><span class="metric-label">So far</span><span class="metric-value">${esc(dur(driveMs / 1000))}</span></div>
            <div class="metric metric--sm"><span class="metric-label">Energy</span><span class="metric-value">${fKwh(now.drive.energyKwh, 2)}</span></div>
          </div>
        </div>
      </section>` : ld ? `<section class="panel recent-card" aria-label="Latest drive">
        <div class="recent-thumb" id="thumb-drive">${routeThumb(ld.trace)}</div>
        <div class="recent-body">
          <div class="recent-foot"><span class="eyebrow">Latest drive</span><a class="panel-link" href="#/cars/${encodeURIComponent(carId)}/drives/${encodeURIComponent(ld.id)}">Open drive ${icon('chev-r', 'ico--sm')}</a></div>
          <div class="recent-title">${esc(ld.from.name)} ${icon('arrow-r')} ${esc(ld.to.name)}</div>
          <div class="recent-when">${parts([relDay(ld.start), `${tm(ld.start)} – ${tm(ld.end)}`, `${nf(ld.energyKwh, 2)} kWh`, `${ld.socStart}% → ${ld.socEnd}%`])}</div>
          <div class="recent-stats">
            <div class="metric metric--sm"><span class="metric-label">Distance</span><span class="metric-value">${fDist(ld.distanceKm, 1)}</span></div>
            <div class="metric metric--sm"><span class="metric-label">Duration</span><span class="metric-value">${esc(dur(ld.durationS))}</span></div>
            <div class="metric metric--sm"><span class="metric-label">Efficiency</span><span class="metric-value">${fEff(ld.effWhKm)}</span></div>
          </div>
        </div>
      </section>` : `<section class="panel"><div class="empty"><div class="empty-icon">${icon('route')}</div><h3>No drives yet</h3><p>Drives show up here a minute or so after the car parks.</p></div></section>`}
      ${lc ? `<section class="panel recent-card" aria-label="${lc.status === 'charging' ? 'Charging now' : 'Latest charge'}">
        <div class="recent-thumb recent-thumb--curve">${curveThumb(lc.curve)}</div>
        <div class="recent-body">
          <div class="recent-foot"><span class="eyebrow">${lc.status === 'charging' ? 'Charging now' : 'Latest charge'}</span><a class="panel-link" href="#/cars/${encodeURIComponent(carId)}/charges">Charges ${icon('chev-r', 'ico--sm')}</a></div>
          <div class="recent-title">${esc(lc.place.name)}${SEP}${esc(lc.kind)} ${nf(lc.maxKw, 0)} kW</div>
          <div class="recent-when">${parts([relDay(lc.start), `${tm(lc.start)} – ${lc.status === 'charging' ? 'now' : tm(lc.end)}`, `${nf(lc.usedKwh, 1)} kWh from the wall`])}</div>
          <div class="recent-stats">
            <div class="metric metric--sm"><span class="metric-label">Added</span><span class="metric-value">${fKwh(lc.addedKwh, 1)}</span></div>
            <div class="metric metric--sm"><span class="metric-label">Battery</span><span class="metric-value">${lc.socStart}–${lc.socEnd}<span class="unit">%</span></span></div>
            <div class="metric metric--sm"><span class="metric-label">${lc.status === 'charging' ? 'So far' : 'Duration'}</span><span class="metric-value">${esc(dur(lc.durationS))}</span></div>
          </div>
        </div>
      </section>` : `<section class="panel"><div class="empty"><div class="empty-icon">${icon('bolt')}</div><h3>No charges yet</h3></div></section>`}
    </div>
    </div>`);

  for (const b of $$('[data-preview]', view)) {
    b.addEventListener('click', () => { location.hash = `#/cars/${encodeURIComponent(carId)}/now${b.dataset.preview === 'parked' ? '' : '?state=' + b.dataset.preview}`; });
  }

  // refresh: same page while nothing important changed, otherwise draw it again
  ctx.onRefresh(async () => {
    const n = await api.getNow(carId, { preview: preview || 'parked' });
    if (!ctx.alive()) return;
    if (n.state !== now.state || n.stale !== now.stale || n.soc !== now.soc || n.location.place !== now.location.place) { route({ keepScroll: true }); return; }
    for (const [k, v] of [['lastSeen', n.lastSeen], ['receivedAt', n.receivedAt]]) {
      for (const el of $$(`[data-ts="${k}"]`, view)) { el.dataset.ago = v; el.textContent = ago(v); }
    }
    for (const el of $$('.age[data-age-for]', view)) refreshAge(el);
  });

  // the small map of the latest (or current) drive
  const thumbEl = $('#thumb-drive', view);
  if (thumbEl) mountThumbMap(thumbEl, liveDrive ? now.drive.trace : ld && ld.trace, ctx, { live: !!liveDrive });

  // the hero map
  const mapEl = $('#now-map', view);
  try {
    const center = [now.location.lon, now.location.lat];
    const zoom = MAP_ZOOM[now.location.kind] || 15.6;
    const m = await createMap(mapEl, { center, zoom, ctx });
    const { map, ml } = m;
    if (!ctx.alive()) return;
    await m.ready;
    if (!ctx.alive()) return;
    // keep the car clear of the place card: below it when a drive's trail is shown, beside it otherwise
    const card = $('.place-card', view);
    if (driving && now.location.trail) {
      const trail = boundsOf(now.location.trail);
      m.frame(() => map.fitBounds(trail, { padding: padClear(mapEl, card, { left: 50, top: 24, right: 80, bottom: 60 }), maxZoom: 16, duration: 0 }));
      addRouteLayers(m, now.location.trail, 'trail');
    } else {
      m.frame(() => map.jumpTo({ center, zoom, padding: { ...padClear(mapEl, card, { left: 4, top: 30, right: 0, bottom: 0, beside: true }), top: rpx(30) } }));
    }
    const tone = stale || offline ? 'muted' : charging ? 'warm' : asleep ? 'asleep' : 'live';
    addCarMarker(ml, map, center, { tone, heading: driving ? now.location.heading ?? null : null });
  } catch (e) {
    console.warn(e);
    html(mapEl, `<div class="map-fallback">Map unavailable</div>`);
  }
}

// ------------------------------------------------------------------ DRIVES
function rangeFromQuery(q) {
  const t = todayKey();
  const preset = q.get('range') || (q.get('from') ? 'custom' : '30');
  if (preset === 'custom') {
    let from = q.get('from') || addDays(t, -29), to = q.get('to') || t;
    if (from > to) [from, to] = [to, from];
    return { preset, from, to };
  }
  const n = { 7: 7, 30: 30, 90: 90 }[preset] || 30;
  return { preset: String(n), from: addDays(t, -(n - 1)), to: t };
}

async function viewDrives(ctx) {
  const [carId] = ctx.params;
  const { view } = ctx;
  recent.push(carId);
  const car = await carById(carId);
  if (!car) throw Object.assign(new Error('No car with this address'), { status: 404 });
  setShell({ page: 'drives', carId, crumbs: [{ label: 'All cars', href: '#/fleet' }, { label: car.name, href: `#/cars/${carId}/now` }, { label: 'Drives' }] });
  const rg = rangeFromQuery(ctx.query);
  const setRange = (p) => {
    const qs = new URLSearchParams(p).toString();
    location.hash = `#/cars/${encodeURIComponent(carId)}/drives${qs ? '?' + qs : ''}`;
  };
  // one grid on large screens: the header and the list on the left, the totals beside both (app.css "wide")
  html(view, `
    <div class="drives-page">
    <div class="page-head">
      <div><span class="eyebrow">${esc(car.name)}</span><h1 class="page-title">Drives</h1>
        <p class="page-sub"><span>${fmt.dayShort.format(dayNoon(rg.from))} – ${fmt.dayShort.format(dayNoon(rg.to))}</span><span class="sep"></span><span>times in ${esc(TZ.replace('_', ' '))} (${tzName()})</span></p></div>
      <div class="page-actions range-picker">
        <div class="seg" role="group" aria-label="Period">
          ${[['7', '7 days'], ['30', '30 days'], ['90', '90 days']].map(([v, l]) => `<button type="button" class="seg-btn" data-range="${v}" aria-pressed="${rg.preset === v}">${l}</button>`).join('')}
        </div>
        <label class="field"><span class="sr-only">From</span><input class="input" type="date" id="dr-from" value="${rg.from}" max="${todayKey()}"></label>
        <span class="range-dash" aria-hidden="true">–</span>
        <label class="field"><span class="sr-only">To</span><input class="input" type="date" id="dr-to" value="${rg.to}" max="${todayKey()}"></label>
      </div>
    </div>
    <section class="panel drives-summary" id="dr-sum" aria-label="Totals for the period">
      <div class="drives-metrics">${'<div class="metric"><span class="skel skel--text"></span><span class="skel skel--num"></span></div>'.repeat(5)}</div><div class="daily-chart"><span class="skel skel--block"></span></div>
    </section>
    <section class="panel drive-list-panel" aria-label="Drive list">
      <div class="drive-cols drive-th" aria-hidden="true"><span>Time</span><span>Route</span><span class="r">Distance</span><span class="r">Duration</span><span class="r drive-x">Avg speed</span><span class="r drive-x">Outside</span><span class="drive-th-eff" title="Bar: energy per distance. The tick on each bar marks the average for the period; teal bars are better than average.">Efficiency</span><span class="r drive-c-energy">Energy</span><span class="r">Battery</span><span></span></div>
      <div class="drive-list" id="dr-list" role="list">${'<div class="drive-cols drive-row"><span class="skel skel--text"></span><span class="skel skel--text"></span><span></span><span></span><span class="skel skel--text"></span></div>'.repeat(6)}</div>
      <div class="list-foot" id="dr-more" hidden><button class="btn" type="button">${icon('chev-ud', 'ico--sm')} Load older drives</button></div>
    </section>
    </div>`);
  for (const b of $$('[data-range]', view)) b.addEventListener('click', () => setRange({ range: b.dataset.range }));
  const onDate = () => setRange({ from: $('#dr-from').value, to: $('#dr-to').value });
  $('#dr-from').addEventListener('change', onDate);
  $('#dr-to').addEventListener('change', onDate);

  const first = await api.listDrives(carId, { from: rg.from, to: rg.to, limit: 20 });
  if (!ctx.alive()) return;
  const T = first.totals;
  html($('#dr-sum'), `
    <div class="drives-metrics">
    <div class="metric"><span class="metric-label">Drives</span><span class="metric-value">${nf(T.count)}</span></div>
    <div class="metric"><span class="metric-label">Distance</span><span class="metric-value">${fDist(T.distanceKm, 0)}</span></div>
    <div class="metric"><span class="metric-label">Driving time</span><span class="metric-value">${esc(dur(T.durationS))}</span></div>
    <div class="metric"><span class="metric-label">Energy</span><span class="metric-value">${fKwh(T.energyKwh, 1)}</span></div>
    <div class="metric"><span class="metric-label">Efficiency</span><span class="metric-value">${fEff(T.effWhKm)}</span><span class="metric-sub nowrap">average · tick on bars</span></div>
    </div>
    <div class="daily-chart"><div class="daily-chart-head"><span>Distance per day</span><span id="dc-read" aria-live="off"></span></div><div class="daily-chart-plot" id="dc-plot"></div></div>`);

  const listEl = $('#dr-list');
  let lastDay = null, cursor = first.nextCursor;
  const dayTotals = new Map(first.perDay.map((d) => [d.day, d]));
  const avgEff = T.effWhKm || 160;
  const effMax = avgEff * 1.6;
  const avgPct = ((avgEff / effMax) * 100).toFixed(1);
  const appendDrives = (items, replace) => {
    let out = '';
    for (const d of items) {
      const k = dayKey(d.start);
      if (k !== lastDay) {
        lastDay = k;
        const dt = dayTotals.get(k);
        out += `<div class="day-head" role="presentation"><h3>${relDay(d.start) === 'Today' || relDay(d.start) === 'Yesterday' ? `${relDay(d.start)}${SEP}` : ''}${fmt.dateLong.format(d.start)}</h3><span>${dt ? `${dt.count} ${dt.count === 1 ? 'drive' : 'drives'}${SEP}${tDist(dt.distanceKm, 1)}` : ''}</span></div>`;
      }
      const effPct = clamp((d.effWhKm / effMax) * 100, 4, 100);
      const over = d.effWhKm / avgEff - 1;
      out += `<div class="drive-cols drive-row" role="listitem">
        <a class="cover" href="#/cars/${encodeURIComponent(carId)}/drives/${encodeURIComponent(d.id)}" aria-label="${esc(`${d.from.name} to ${d.to.name}, ${relDay(d.start)} ${tm(d.start)}, ${tDist(d.distanceKm, 1)}, ${tEff(d.effWhKm)}`)}"></a>
        <span class="drive-time"><b>${tm(d.start)}</b><span>to ${tm(d.end)}</span></span>
        <span class="drive-route-ends"><b title="${esc(`${d.from.name} → ${d.to.name}`)}">${esc(d.from.name)}${icon('arrow-r')}${esc(d.to.name)}</b><span title="${esc(`${street(d.from.detail)} → ${street(d.to.detail)}`)}">${esc(street(d.from.detail))} → ${esc(street(d.to.detail))}</span></span>
        <span class="cell-r">${fDist(d.distanceKm, 1)}</span>
        <span class="cell-r">${esc(dur(d.durationS))}</span>
        <span class="cell-r drive-x">${d.durationS ? fSpeed(d.distanceKm / (d.durationS / 3600)) : '—'}</span>
        <span class="cell-r drive-x">${d.outsideC != null ? fTemp(d.outsideC) : '—'}</span>
        <span class="eff"><span class="eff-bar${d.effWhKm < avgEff ? ' is-better' : ''}" data-w="${effPct.toFixed(1)}"><i></i><b data-x="${avgPct}"></b></span><span class="eff-val"><span class="cell-r">${fEff(d.effWhKm)}</span>${over >= 0.1 ? `<span class="eff-delta">+${Math.round(over * 100)}%</span>` : ''}</span></span>
        <span class="cell-r drive-c-energy">${fKwh(d.energyKwh, 2)}</span>
        <span class="cell-r soc-delta">${d.socStart}%${icon('arrow-r')}${d.socEnd}%</span>
        <span class="row-chev" aria-hidden="true">${icon('chev-r')}</span>
      </div>`;
    }
    if (replace) html(listEl, out); else { const tmp = document.createElement('div'); html(tmp, out); listEl.append(...tmp.childNodes); }
  };
  if (!first.items.length) {
    html(listEl, `<div class="empty"><div class="empty-icon">${icon('calendar')}</div><h3>No drives in this period</h3><p>${esc(car.name)} did not drive between ${fmt.dayShort.format(dayNoon(rg.from))} and ${fmt.dayShort.format(dayNoon(rg.to))}. Pick a longer period above.</p><button class="btn" type="button" data-range-90>${icon('calendar')} Show 90 days</button></div>`);
    $('[data-range-90]', listEl)?.addEventListener('click', () => setRange({ range: '90' }));
  } else appendDrives(first.items, true);
  const more = $('#dr-more');
  more.hidden = !cursor;
  $('button', more).addEventListener('click', async (e) => {
    const b = e.currentTarget;
    b.disabled = true;
    const next = await api.listDrives(carId, { from: rg.from, to: rg.to, cursor, limit: 20 });
    if (!ctx.alive()) return;
    appendDrives(next.items, false);
    cursor = next.nextCursor;
    more.hidden = !cursor;
    b.disabled = false;
  });

  // distance per day (one series: no legend; the heading names it). Its box is sized in app.css.
  await loadUPlot();
  if (!ctx.alive()) return;
  const plot = $('#dc-plot');
  const xs = first.perDay.map((d) => dayNoon(d.day) / 1000);
  const ys = first.perDay.map((d) => U.dist(d.distanceKm));
  const read = $('#dc-read');
  const idle = () => { read.innerHTML = `busiest <b>${tDist(Math.max(...first.perDay.map((d) => d.distanceKm)), 1)}</b>`; };
  idle();
  const live = cssVar('--live');
  mountPlot(plot, (width, height) => ({
    width, height,
    padding: [rpx(8), rpx(16), 0, rpx(8)], legend: { show: false },
    cursor: { y: false, points: { show: false }, drag: { x: false, y: false } },
    scales: { x: { time: false, range: (_, a, b) => [a - 43200, b + 43200] }, y: { range: (_, a, b) => [0, Math.max(1, b * 1.12)] } },
    axes: [
      // about one date label per 7rem of width, newest day always labelled
      { show: true, size: rpx(18), gap: rpx(2), stroke: cssVar('--text-3'), font: chartFont(), grid: { show: false }, ticks: { show: false }, splits: (u) => { const n = xs.length; const step = Math.max(1, Math.ceil(n / Math.max(4, u.width / rpx(110)))); return xs.filter((_, i) => (n - 1 - i) % step === 0); }, values: (_, sp) => sp.map((v) => fmt.dayShort.format(v * 1000)) },
      { show: false },
    ],
    series: [{}, { stroke: live, fill: hexA(live, 0.75), width: 0, paths: uPlot.paths.bars({ size: [0.62, rpx(18), rpx(2)], radius: [0.3, 0], gap: 1 }), points: { show: false } }],
    hooks: {
      setCursor: [(uu) => {
        const i = uu.cursor.idx;
        if (i == null) { idle(); return; }
        const d = first.perDay[i];
        read.innerHTML = `${fmt.dayShort.format(dayNoon(d.day))}${SEP}<b>${tDist(d.distanceKm, 1)}</b>${SEP}${d.count} ${d.count === 1 ? 'drive' : 'drives'}`;
      }],
    },
  }), [xs, ys], ctx);
  plot.dataset.chartReady = '1';
}

// ------------------------------------------------------------------ DRIVE DETAIL
async function viewDrive(ctx) {
  const [carId, driveId] = ctx.params;
  const { view } = ctx;
  recent.push(carId);
  const car = await carById(carId);
  if (!car) throw Object.assign(new Error('No car with this address'), { status: 404 });
  view.classList.add('view--full'); // on large screens the page fills the window (app.css "wide")
  html(view, `<a class="back" href="#/cars/${encodeURIComponent(carId)}/drives">${icon('arrow-l', 'ico--sm')} Drives</a>
    <div class="page-head"><div><span class="skel skel--title"></span></div></div>
    <div class="drive-layout"><div class="panel map-panel"><span class="skel skel--block"></span></div><div class="panel stats-panel"><span class="skel skel--block"></span></div></div>`);
  const d = await api.getDrive(carId, driveId);
  if (!ctx.alive()) return;
  const live = !!d.inProgress;
  setShell({ page: 'drives', carId, crumbs: [{ label: 'All cars', href: '#/fleet' }, { label: car.name, href: `#/cars/${carId}/now` }, { label: 'Drives', href: `#/cars/${carId}/drives` }, { label: live ? 'Drive in progress' : `${relDay(d.start)}${SEP}${tm(d.start)}` }] });
  const S = d.stats, s = d.samples;
  const coords = s.lon.map((lon, i) => [lon, s.lat[i]]);
  const usedPct = Math.max(0.5, S.socStart - S.socEnd);
  html(view, `
    <a class="back" href="#/cars/${encodeURIComponent(carId)}/drives">${icon('arrow-l', 'ico--sm')} Drives</a>
    <div class="page-head">
      <div><span class="eyebrow">${live ? 'Drive in progress' : 'Drive'}${SEP}${esc(fmt.dateLong.format(d.start))}</span>
        <h1 class="page-title">${esc(d.from.name)} → ${esc(d.to.name)}${live ? ` <span class="pill pill--lg pill--live">${icon('live')}Live</span>` : ''}</h1>
        <p class="page-sub"><span>${tm(d.start)} – ${live ? 'now' : tm(d.end)} ${tzName()}</span><span class="sep"></span><span>${tDist(d.distanceKm, 1)}</span><span class="sep"></span><span>${esc(dur(d.durationS))}${live ? ' so far' : ''}</span><span class="sep"></span><span>${esc(car.name)}</span></p></div>
      <div class="page-actions">
        <a class="btn" href="#/cars/${encodeURIComponent(carId)}/timeline?day=${dayKey(d.start)}">${icon('timeline')} Day timeline</a>
        <a class="btn" href="#/cars/${encodeURIComponent(carId)}/signals?from=${d.start}&to=${d.end}">${icon('signals')} Raw signals</a>
      </div>
    </div>

    <div class="drive-layout">
      <section class="panel map-panel" aria-label="Route map">
        <div class="map" id="drive-map" role="img" aria-label="Map of the route from ${esc(d.from.name)} to ${esc(d.to.name)}"></div>
        <div class="map-overlay map-overlay--tl">
          <div class="glass-card route-legend">
            <div class="route-end"><span class="route-pin route-pin--a" aria-hidden="true">A</span><div><b>${esc(d.from.name)}</b><span>${esc(d.from.detail)}</span></div><time>${tm(d.start)}</time></div>
            <div class="route-line" aria-hidden="true"></div>
            <div class="route-end route-end--to"><span class="route-pin route-pin--b" aria-hidden="true">B</span><div><b>${esc(d.to.name)}</b><span>${esc(d.to.detail)}</span></div><time>${live ? 'now' : tm(d.end)}</time></div>
          </div>
        </div>
        <div class="map-overlay map-overlay--bl"><div class="glass-card hover-readout" id="map-readout" hidden aria-hidden="true"></div></div>
      </section>

      <section class="panel stats-panel" aria-label="Drive statistics">
        <div class="panel-head"><h2 class="panel-title">${icon('stats')}Drive stats</h2>${live ? '<span class="faint">so far</span>' : ''}</div>
        <div class="stats-body">
        <div class="stats-grid">
          <div class="metric"><span class="metric-label">Distance</span><span class="metric-value">${fDist(d.distanceKm, 2)}</span></div>
          <div class="metric"><span class="metric-label">Duration</span><span class="metric-value">${esc(dur(d.durationS))}</span></div>
          <div class="metric"><span class="metric-label">Max speed</span><span class="metric-value">${fSpeed(S.maxSpeedKph)}</span></div>
          <div class="metric"><span class="metric-label">Average speed</span><span class="metric-value">${fSpeed(S.avgSpeedKph)}</span></div>
          <div class="metric"><span class="metric-label">Energy used</span><span class="metric-value">${fKwh(S.energyKwh, 2)}</span><span class="metric-sub nowrap">${nf(S.regenKwh, 2)} kWh regen</span></div>
          <div class="metric"><span class="metric-label">Efficiency</span><span class="metric-value">${fEff(S.effWhKm)}</span></div>
          <div class="metric"><span class="metric-label">Climb</span><span class="metric-value">${withUnit('+' + nf(U.elev(S.ascentM)), U.elevU())}</span><span class="metric-sub nowrap">−${nf(U.elev(S.descentM))} ${U.elevU()} down</span></div>
          <div class="metric"><span class="metric-label">Peak power</span><span class="metric-value">${withUnit(nf(S.maxPowerKw), 'kW')}</span><span class="metric-sub nowrap">regen up to ${nf(S.maxRegenKw)} kW</span></div>
        </div>
        <div class="stats-foot">
        <div class="stats-divider"></div>
        <div class="battery-span">
          <div class="battery-span-end"><b>${S.socStart}%</b><span>${tDist(S.rangeStartKm, 0)}</span></div>
          <div class="soc-span" role="img" aria-label="Battery from ${S.socStart}% to ${S.socEnd}%"><i class="left" data-a="${S.socEnd}"></i><i class="used" data-a="${S.socEnd}" data-w="${usedPct}"></i></div>
          <div class="battery-span-end"><b>${S.socEnd}%</b><span>${tDist(S.rangeEndKm, 0)}</span></div>
        </div>
        <div class="stats-divider"></div>
        <div class="stats-grid">
          <div class="metric metric--sm"><span class="metric-label">Outside</span><span class="metric-value">${fTemp(S.outsideC)}</span></div>
          <div class="metric metric--sm"><span class="metric-label">Cabin</span><span class="metric-value">${fTemp(S.cabinC)}</span></div>
        </div>
        </div>
        </div>
      </section>

      <section class="panel charts-panel" aria-label="Speed, power, battery and elevation over time">
      <div class="charts-head">
        <h2 class="panel-title">${icon('signals')}Telemetry</h2>
        <div class="charts-hint"><span id="chart-time" class="chart-time" aria-live="off"></span><span>Drag across a chart to zoom · double-click to reset</span><button class="btn btn--sm" type="button" id="zoom-reset" hidden>${icon('reset', 'ico--sm')} Reset zoom</button></div>
      </div>
      <div id="charts"></div>
      </section>
    </div>`);

  // map first (it's the hero), charts right after
  const mapEl = $('#drive-map', view);
  let hover = () => {};
  let fitTo = () => {};
  const mapP = (async () => {
    const m = await createMap(mapEl, { center: coords[0], bounds: boundsOf(coords), ctx });
    const { map, ml } = m;
    await m.ready;
    if (!ctx.alive()) return;
    addRouteLayers(m, coords, 'route');
    // the hover marker: white with a teal ring and halo, unlike the A and B pins at the ends
    map.addSource('hover', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
    m.addLayer({ id: 'hover-halo', type: 'circle', source: 'hover', paint: { 'circle-color': cssVar('--live'), 'circle-opacity': 0.35, 'circle-blur': 0.35 } },
      { 'circle-radius': () => rpx(17) });
    m.addLayer({ id: 'hover-dot', type: 'circle', source: 'hover', paint: { 'circle-color': cssVar('--text'), 'circle-stroke-color': cssVar('--live') } },
      { 'circle-radius': () => rpx(8), 'circle-stroke-width': () => rpx(3) });
    routePin(ml, map, coords[0], 'A');
    if (live) addCarMarker(ml, map, coords[coords.length - 1], { tone: 'live' });
    else routePin(ml, map, coords[coords.length - 1], 'B');
    const src = map.getSource('hover');
    const readout = $('#map-readout', view);
    hover = (i) => {
      if (i == null) { src.setData({ type: 'FeatureCollection', features: [] }); readout.hidden = true; return; }
      src.setData({ type: 'Feature', properties: {}, geometry: { type: 'Point', coordinates: coords[i] } });
      readout.hidden = false;
      readout.innerHTML = `<span class="chart-time">${fmt.timeSec.format(s.t[i])}</span><span><b>${nf(U.speed(s.speedKph[i]))}</b> ${U.speedU()}</span><span><b>${nf(s.powerKw[i])}</b> kW</span><span><b>${nf(s.soc[i], 1)}</b>%</span>`;
    };
    // the route (or the part the charts are zoomed to) fits below the A/B card, also after a resize
    const legend = $('.route-legend', view);
    let range = [0, coords.length - 1];
    const fit = (duration) => {
      const sub = coords.slice(Math.max(0, range[0]), Math.min(coords.length, range[1] + 1));
      if (sub.length < 2) return;
      map.fitBounds(boundsOf(sub), { padding: padClear(mapEl, legend, { top: 20, bottom: 50, left: 60, right: 70 }), duration, maxZoom: 17 });
    };
    m.frame(() => fit(0));
    fitTo = (i0, i1) => { range = [i0, i1]; fit(500); };
  })().catch((e) => { console.warn(e); html(mapEl, '<div class="map-fallback">Map unavailable</div>'); });

  await loadUPlot();
  await document.fonts.ready;
  if (!ctx.alive()) return;
  mountDriveCharts($('#charts', view), d, ctx, { hover: (i) => hover(i), fitTo: (a, b) => fitTo(a, b) });
  await mapP;
}

// time-axis steps in seconds (uPlot time scales are in seconds here): uPlot's own steps from one minute up
const TIME_INCRS = [60, 300, 600, 900, 1800, 3600, 7200, 10800, 14400, 21600, 28800, 43200, 86400];
// Four charts on one time axis. Each fills a box that app.css sizes (taller on xwide screens, where the
// charts sit beside the map); mountPlot keeps each chart in step with its box, the root size and the
// pixel ratio. The zoom lives here, so a chart that is built again keeps it.
function mountDriveCharts(host, d, ctx, link) {
  const s = d.samples;
  const x = s.t.map((t) => t / 1000);
  const C = (n) => cssVar(n);
  const axisInk = C('--text-3'), grid = C('--chart-grid');
  const maxOf = (a) => Math.max(...a.filter((v) => v != null));
  const minOf = (a) => Math.min(...a.filter((v) => v != null));
  const sp = s.speedKph.map(U.speed), el = s.elevationM.map(U.elev);
  const defs = [
    { key: 'speed', name: 'Speed', color: C('--c-speed'), data: sp, unit: U.speedU(), digits: 0, area: true, range: [0, Math.max(10, maxOf(sp) * 1.15)],
      idle: `max <b>${nf(maxOf(sp))}</b> · avg <b>${nf(U.speed(d.stats.avgSpeedKph))}</b> ${U.speedU()}` },
    { key: 'power', name: 'Power', color: C('--c-power'), regen: C('--c-regen'), data: s.powerKw, unit: 'kW', digits: 0, split: true,
      range: [Math.min(-5, minOf(s.powerKw) * 1.25), Math.max(10, maxOf(s.powerKw) * 1.18)],
      idle: `peak <b>${nf(d.stats.maxPowerKw)}</b> kW · regen <b>${nf(d.stats.maxRegenKw)}</b> kW` },
    { key: 'soc', name: 'Battery', color: C('--c-soc'), data: s.soc, unit: '%', digits: 1,
      range: [Math.floor(minOf(s.soc) - 0.6), Math.ceil(maxOf(s.soc) + 0.6)], idle: `<b>${d.stats.socStart}%</b> → <b>${d.stats.socEnd}%</b>` },
    { key: 'elev', name: 'Elevation', color: C('--c-elev'), data: el, unit: U.elevU(), digits: 0, area: true, showX: true,
      range: [Math.min(0, minOf(el)), Math.max(10, maxOf(el) * 1.18)], idle: `+<b>${nf(U.elev(d.stats.ascentM))}</b> / −<b>${nf(U.elev(d.stats.descentM))}</b> ${U.elevU()}` },
  ];
  html(host, defs.map((df) => `<div class="chart-block" data-series="${df.key}">
      <div class="chart-label"><span class="swatch" aria-hidden="true"></span><span class="name">${df.name}<span class="chart-unit">${SEP}${esc(df.unit)}</span></span>
        ${df.split ? '<span class="key"><i data-series="regen"></i>regen (below 0)</span>' : ''}
        <span class="val" data-val="${df.key}">${df.idle}</span></div>
      <div class="chart" data-chart="${df.key}"></div></div>`).join(''));
  const vertGrad = (u, color, a0, a1) => {
    const { top, height } = u.bbox;
    const g = u.ctx.createLinearGradient(0, top, 0, top + height);
    g.addColorStop(0, hexA(color, a0)); g.addColorStop(1, hexA(color, a1));
    return g;
  };
  const splitGrad = (u, c1, c2, a) => {
    const { top, height } = u.bbox;
    const y0 = u.valToPos(0, 'y', true);
    const f = clamp((y0 - top) / height, 0, 1);
    const g = u.ctx.createLinearGradient(0, top, 0, top + height);
    g.addColorStop(0, hexA(c1, a)); g.addColorStop(f, hexA(c1, a)); g.addColorStop(f, hexA(c2, a)); g.addColorStop(1, hexA(c2, a));
    return g;
  };
  const plots = [];
  const charts = () => plots.map((p) => p.u).filter(Boolean);
  const xMin = x[0], xMax = x[x.length - 1];
  let zoom = [xMin, xMax];
  const resetBtn = $('#zoom-reset');
  const timeEl = $('#chart-time');
  const idxRange = (a, b) => { let i0 = 0, i1 = x.length - 1; while (i0 < x.length - 1 && x[i0] < a) i0++; while (i1 > 0 && x[i1] > b) i1--; return [i0, i1]; };
  const zoomTo = (a, b, fromUser = true) => {
    zoom = [a, b];
    for (const c of charts()) c.setScale('x', { min: a, max: b });
    const zoomed = a > xMin + 1 || b < xMax - 1;
    resetBtn.hidden = !zoomed;
    if (fromUser) { const [i0, i1] = idxRange(a, b); link.fitTo(zoomed ? i0 : 0, zoomed ? i1 : x.length - 1); }
  };
  resetBtn.addEventListener('click', () => zoomTo(xMin, xMax));
  const zeroInk = C('--chart-zero');
  const cursorInk = C('--bg-1');

  for (const df of defs) {
    const target = $(`[data-chart="${df.key}"]`, host);
    const valEl = $(`[data-val="${df.key}"]`, host);
    // sizes uPlot draws in pixels, from the 16 px design size (rpx), read again on every build
    const make = (width, height) => {
      const series = {
        label: df.name, width: rpx(2), points: { show: false },
        stroke: df.split ? (u) => splitGrad(u, df.color, df.regen, 1) : df.color,
        fill: df.split ? (u) => splitGrad(u, df.color, df.regen, 0.16) : df.area ? (u) => vertGrad(u, df.color, 0.26, 0.01) : undefined,
      };
      if (df.split) series.fillTo = () => 0;
      return {
        width, height,
        padding: [rpx(10), rpx(14), df.showX ? 0 : rpx(6), 0],
        legend: { show: false },
        tzDate: (ts) => uPlot.tzDate(new Date(ts * 1000), TZ),
        cursor: {
          sync: { key: 'drive-' + d.id, setSeries: false },
          drag: { x: true, y: false, setScale: false },
          points: { size: rpx(9), width: rpx(2), stroke: cursorInk, fill: df.color },
        },
        select: { show: true },
        scales: { x: { time: true }, y: { range: () => df.range } },
        axes: [
          {
            stroke: axisInk, font: chartFont(), grid: { stroke: grid, width: 1 }, ticks: { show: false }, gap: rpx(6), space: rpx(84),
            size: df.showX ? rpx(26) : rpx(2),
            // labels say h:mm, so steps are whole minutes: a short drive on a wide chart never repeats a label
            incrs: TIME_INCRS,
            values: (u, v) => (df.showX ? v.map((t) => fmt.time.format(t * 1000)) : v.map(() => '')),
          },
          {
            stroke: axisInk, font: chartFont(), grid: { stroke: grid, width: 1 }, ticks: { show: false }, gap: rpx(8), size: rpx(56), space: rpx(30),
            values: (u, v) => v.map((n) => nf(n, 0)),
            // whole percent steps for the battery: 65 / 66 / 67, never 64.5
            splits: df.key === 'soc' ? (u, ai, mn, mx) => { const st = Math.max(1, Math.ceil((mx - mn) / Math.max(3, Math.floor(u.height / rpx(36))))); const o = []; for (let v = Math.ceil(mn / st) * st; v <= mx; v += st) o.push(v); return o; } : undefined,
          },
        ],
        series: [{}, series],
        hooks: {
          draw: df.split ? [(u) => {
            // the canvas is in device pixels: a 1 CSS px dashed line, 4 px dashes at the design size
            const ratio = uPlot.pxRatio, lw = Math.max(1, Math.round(ratio));
            const y = Math.round(u.valToPos(0, 'y', true)) + (lw % 2 ? 0.5 : 0);
            const { ctx: c } = u;
            c.save(); c.strokeStyle = zeroInk; c.setLineDash([rpx(4) * ratio, rpx(4) * ratio]); c.lineWidth = lw;
            c.beginPath(); c.moveTo(u.bbox.left, y); c.lineTo(u.bbox.left + u.bbox.width, y); c.stroke(); c.restore();
          }] : [],
          setSelect: [(u) => {
            if (u.select.width < rpx(6)) return;
            const a = u.posToVal(u.select.left, 'x'), b = u.posToVal(u.select.left + u.select.width, 'x');
            u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
            zoomTo(a, b);
          }],
          setCursor: [(u) => {
            const i = u.cursor.idx;
            if (i == null || u.cursor.left < 0) {
              valEl.innerHTML = df.idle;
              if (df.key === 'speed') { timeEl.textContent = ''; link.hover(null); }
              return;
            }
            const v = df.data[i];
            valEl.innerHTML = df.split && v < 0 ? `<b class="regen">${nf(v)}</b> kW regen` : `<b>${nf(v, df.digits)}</b> ${esc(df.unit)}`;
            if (df.key === 'speed') { timeEl.textContent = fmt.timeSec.format(s.t[i]); link.hover(i); }
          }],
        },
      };
    };
    plots.push(mountPlot(target, make, [x, df.data], ctx, (u) => {
      u.over.addEventListener('dblclick', () => zoomTo(xMin, xMax));
      if (zoom[0] > xMin || zoom[1] < xMax) u.setScale('x', { min: zoom[0], max: zoom[1] });
    }));
  }
  host.dataset.chartReady = '1';
}

// ------------------------------------------------------------------ TIMELINE
// List rows, oldest first: one row per stay at a place (parked and asleep time together), per
// drive, per charge, per gap with no data, and what has not arrived yet.
function timelineRows(tl) {
  const rows = [];
  for (const sg of tl.segments) {
    if (sg.kind === 'parked' || sg.kind === 'asleep') {
      const ms = sg.end - sg.start;
      const prev = rows[rows.length - 1];
      if (prev && prev.type === 'stay' && prev.place.name === (sg.place || {}).name && prev.end >= sg.start) {
        prev.end = sg.end; prev[sg.kind] += ms; if (sg.note) prev.note = sg.note;
      } else {
        rows.push({ type: 'stay', start: sg.start, end: sg.end, place: sg.place || { name: 'Parked' }, parked: sg.kind === 'parked' ? ms : 0, asleep: sg.kind === 'asleep' ? ms : 0, note: sg.note || null });
      }
    } else rows.push({ ...sg, type: sg.kind });
  }
  if (tl.now && tl.receivedUntil < tl.now - 5000) rows.push({ type: 'pending', start: tl.receivedUntil, end: tl.now });
  return rows;
}
// compact: the short list beside the Now page's map (no chevron, no extra columns)
function timelineRowHtml(r, carId, { compact = false } = {}) {
  const ms = r.end - r.start;
  let state, title, detail, href = null, titleText;
  let battery = '', amount = ''; // the xwide list's extra columns
  if (r.type === 'stay') {
    state = r.asleep > r.parked ? 'asleep' : 'parked';
    title = `${esc(r.place.name)}${r.place.detail ? `<span class="tl-place">${SEP}${esc(street(r.place.detail))}</span>` : ''}`;
    titleText = `${r.place.name}${r.place.detail ? `${SEP}${street(r.place.detail)}` : ''}`;
    detail = r.asleep && r.parked ? `Asleep ${dur(r.asleep / 1000)}${SEP}awake ${dur(r.parked / 1000)}` : r.asleep ? 'Asleep' : `Awake ${dur(r.parked / 1000)}`;
    if (r.note) detail += `${SEP}${r.note}`;
  } else if (r.type === 'driving') {
    state = 'driving';
    title = esc(r.title);
    detail = typeof r.detail === 'string' ? `Driving${SEP}${r.detail}` : `Driving${SEP}${tDist(r.detail.distanceKm, 1)}${SEP}${tEff(r.detail.effWhKm)}${SEP}${r.detail.socStart}% → ${r.detail.socEnd}%`;
    if (typeof r.detail === 'object') { battery = `${r.detail.socStart}% → ${r.detail.socEnd}%`; amount = tDist(r.detail.distanceKm, 1); }
    href = `#/cars/${encodeURIComponent(carId)}/drives/${encodeURIComponent(r.driveId)}`;
  } else if (r.type === 'charging') {
    state = 'charging';
    title = esc(r.title);
    detail = typeof r.detail === 'string' ? r.detail : `${r.detail.kind} ${nf(r.detail.maxKw, 1)} kW${SEP}${nf(r.detail.addedKwh, 1)} kWh added${SEP}${r.detail.socStart}% → ${r.detail.socEnd}%`;
    if (typeof r.detail === 'object') { battery = `${r.detail.socStart}% → ${r.detail.socEnd}%`; amount = `+${nf(r.detail.addedKwh, 1)} kWh`; }
    href = `#/cars/${encodeURIComponent(carId)}/charges`;
  } else if (r.type === 'unknown') {
    state = 'unknown'; title = 'No data'; detail = 'No log arrived for this time. Not counted as sleep.';
  } else {
    state = 'pending'; title = 'Not received yet'; detail = 'Logs arrive about a minute after the car records them.';
  }
  const endText = r.type === 'pending' || r.inProgress ? 'now' : tm(r.end);
  const tag = href ? 'a' : 'div';
  titleText ??= r.title ?? title;
  return `<${tag} class="tl-item${href ? ' tl-item--link' : ''}" data-state="${state}"${href ? ` href="${href}"` : ''}>
      <span class="tl-time"><span>${tm(r.start)} –</span> <span>${endText}</span></span>
      <span class="tl-rail" aria-hidden="true"><i></i></span>
      <span class="tl-body"><span class="title" title="${esc(titleText)}">${title}</span>${compact
        ? `<span class="detail"><span class="tl-when"><span class="nowrap">${tm(r.start)} – ${endText}</span>${SEP}</span>${parts(detail.split(SEP).map(esc))}</span>`
        : `<span class="detail" title="${esc(detail)}">${esc(detail)}</span>`}</span>
      ${compact ? '' : `<span class="tl-x tl-num">${battery ? esc(battery) : '<span class="faint">—</span>'}</span><span class="tl-x tl-num">${amount ? esc(amount) : '<span class="faint">—</span>'}</span>`}
      <span class="tl-dur">${esc(dur(ms / 1000))}</span>
      ${compact ? '' : `<span class="row-chev" aria-hidden="true">${href ? icon('chev-r') : ''}</span>`}
    </${tag}>`;
}
async function viewTimeline(ctx) {
  const [carId] = ctx.params;
  const { view } = ctx;
  recent.push(carId);
  const car = await carById(carId);
  if (!car) throw Object.assign(new Error('No car with this address'), { status: 404 });
  setShell({ page: 'timeline', carId, crumbs: [{ label: 'All cars', href: '#/fleet' }, { label: car.name, href: `#/cars/${carId}/now` }, { label: 'Timeline' }] });
  const t = todayKey();
  let day = /^\d{4}-\d{2}-\d{2}$/.test(ctx.query.get('day') || '') ? ctx.query.get('day') : t;
  if (day > t) day = t;
  const go = (dd) => { location.hash = `#/cars/${encodeURIComponent(carId)}/timeline${dd === t ? '' : '?day=' + dd}`; };
  view.classList.add('view--full'); // on large screens the page fills the window (app.css "wide")
  html(view, `
    <div class="page-head">
      <div><span class="eyebrow">${esc(car.name)}</span><h1 class="page-title">Timeline</h1>
        <p class="page-sub"><span>${day === t ? `Today${SEP}` : ''}${esc(fmt.dateLong.format(dayNoon(day)))}</span><span class="sep"></span><span>states, drives, charges and gaps${SEP}${tzName()}</span></p></div>
      <div class="page-actions">
        <button class="btn btn--icon" type="button" id="tl-prev" aria-label="Previous day">${icon('chev-l')}</button>
        <label class="field"><span class="sr-only">Day</span><input class="input" type="date" id="tl-day" value="${day}" max="${t}"></label>
        <button class="btn btn--icon" type="button" id="tl-next" aria-label="Next day"${day >= t ? ' disabled' : ''}>${icon('chev-r')}</button>
        <button class="btn" type="button" id="tl-today"${day === t ? ' disabled' : ''}>Today</button>
      </div>
    </div>
    <section class="panel tl-panel" id="tl-band"><span class="skel skel--block"></span></section>
    <div class="tl-layout">
      <section class="panel tl-list-panel" id="tl-list"><div class="tl-list">${'<div class="tl-item"><span class="skel skel--text"></span><span></span><span class="skel skel--text"></span></div>'.repeat(5)}</div></section>
      <aside class="panel panel-pad tl-summary" id="tl-sum" aria-label="Day summary"></aside>
      <section class="panel tl-map-panel" aria-label="Map of the day's drives"><div class="map" id="tl-map" role="img" aria-label="Map of the day's drives"></div></section>
    </div>`);
  $('#tl-prev').addEventListener('click', () => go(addDays(day, -1)));
  $('#tl-next').addEventListener('click', () => go(addDays(day, 1)));
  $('#tl-today').addEventListener('click', () => go(t));
  $('#tl-day').addEventListener('change', (e) => e.target.value && go(e.target.value > t ? t : e.target.value));
  const tl = await api.getTimeline(carId, day);
  if (!ctx.alive()) return;
  const len = tl.end - tl.start;
  const pc = (ms) => clamp(((ms - tl.start) / len) * 100, 0, 100);
  const segs = tl.segments;
  const pending = tl.now ? `<i class="tl-seg" data-state="pending" data-x="${pc(tl.receivedUntil).toFixed(3)}" data-w="${Math.max(0.25, pc(tl.now) - pc(tl.receivedUntil)).toFixed(3)}" title="Not received yet: logs arrive about a minute late"></i><i class="tl-now" data-x="${pc(tl.now).toFixed(3)}" title="Now ${tm(tl.now)}"></i>` : '';
  const stateName = (k) => (k === 'unknown' ? 'No data' : (STATES[k] || {}).label || k);
  html($('#tl-band'), `
    <div class="tl-band" role="img" aria-label="States through the day">
      ${segs.map((sg) => `<i class="tl-seg" data-state="${sg.kind}" data-x="${pc(sg.start).toFixed(3)}" data-w="${(pc(sg.end) - pc(sg.start)).toFixed(3)}" title="${esc(`${stateName(sg.kind)} ${tm(sg.start)}–${tm(sg.end)}`)}"><span class="tl-seg-label" aria-hidden="true"><b>${esc(stateName(sg.kind))}</b><span>${esc(`${tm(sg.start)} – ${tm(sg.end)}`)}</span></span></i>`).join('')}
      ${pending}
    </div>
    <div class="tl-axis" aria-hidden="true">${[0, 3, 6, 9, 12, 15, 18, 21, 24].map((h) => `<span data-x="${((h / 24) * 100).toFixed(2)}">${h % 24 === 0 ? '12 AM' : h === 12 ? '12 PM' : h < 12 ? h + ' AM' : h - 12 + ' PM'}</span>`).join('')}</div>
    <div class="tl-legend">${['driving', 'charging', 'parked', 'asleep', 'unknown'].map((k) => `<span><i data-state="${k}"></i>${stateName(k)}</span>`).join('')}${tl.now ? '<span><i data-state="pending"></i>Not received yet</span><span><i class="tl-now-key"></i>Now</span>' : ''}</div>`);
  const rows = timelineRows(tl);
  html($('#tl-list'), rows.length ? `<div class="tl-list">${rows.some((r) => r.type === 'driving' || r.type === 'charging')
    ? '<div class="tl-item tl-head tl-x-head" aria-hidden="true"><span></span><span></span><span></span><span class="tl-x tl-num">Battery</span><span class="tl-x tl-num">Distance · energy</span><span class="tl-dur">Duration</span><span></span></div>' : ''}${rows.map((r) => timelineRowHtml(r, carId)).join('')}</div>`
    : `<div class="empty"><div class="empty-icon">${icon('timeline')}</div><h3>Nothing recorded</h3><p>No logs arrived for this day.</p></div>`);
  html($('#tl-sum'), daySummaryHtml(tl, stateName));
  mountDayMap($('.tl-map-panel', view), tl, carId, ctx);
}

// The day's drives on one map, beside the list on xwide screens. The map is made the first time its
// panel is shown (it is display: none on smaller layouts), so a window that grows into xwide gets it too.
function mountDayMap(panel, tl, carId, ctx) {
  const ids = tl.segments.filter((sg) => sg.kind === 'driving' && sg.driveId && !sg.inProgress).map((sg) => sg.driveId);
  const mapEl = $('#tl-map', panel);
  let started = false;
  const start = async () => {
    started = true;
    ro.disconnect();
    if (!ids.length) {
      html(panel, `<div class="empty"><div class="empty-icon">${icon('route')}</div><h3>No drives on this day</h3><p>Drives show up here a minute or so after the car parks.</p></div>`);
      return;
    }
    try {
      const drives = (await Promise.all(ids.map((id) => api.getDrive(carId, id).catch(() => null)))).filter(Boolean);
      if (!ctx.alive() || !drives.length) return;
      const traces = drives.map((d) => d.samples.lon.map((lon, i) => [lon, d.samples.lat[i]]));
      const all = traces.flat();
      const m = await createMap(mapEl, { center: all[0], bounds: boundsOf(all), ctx });
      await m.ready;
      if (!ctx.alive()) return;
      m.map.addSource('day', { type: 'geojson', data: { type: 'FeatureCollection', features: traces.map((c) => ({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: c } })) } });
      const line = { type: 'line', source: 'day', layout: { 'line-cap': 'round', 'line-join': 'round' } };
      m.addLayer({ ...line, id: 'day-glow', paint: { 'line-color': cssVar('--live'), 'line-opacity': 0.2 } }, { 'line-width': () => rpx(12), 'line-blur': () => rpx(6) });
      m.addLayer({ ...line, id: 'day-casing', paint: { 'line-color': cssVar('--map-casing'), 'line-opacity': 0.85 } }, { 'line-width': () => rpx(6.5) });
      m.addLayer({ ...line, id: 'day-line', paint: { 'line-color': cssVar('--live') } }, { 'line-width': () => rpx(3.2) });
      m.map.addSource('day-ends', { type: 'geojson', data: { type: 'FeatureCollection', features: traces.flatMap((c) => [c[0], c[c.length - 1]]).map((pt) => ({ type: 'Feature', properties: {}, geometry: { type: 'Point', coordinates: pt } })) } });
      m.addLayer({ id: 'day-ends', type: 'circle', source: 'day-ends', paint: { 'circle-color': cssVar('--bg-1'), 'circle-stroke-color': cssVar('--live') } },
        { 'circle-radius': () => rpx(4.5), 'circle-stroke-width': () => rpx(2.2) });
      const km = drives.reduce((a, d) => a + (d.distanceKm || 0), 0);
      const card = document.createElement('div');
      card.className = 'map-overlay map-overlay--tl';
      html(card, `<div class="glass-card day-map-card"><b>${drives.length} ${drives.length === 1 ? 'drive' : 'drives'}${SEP}${esc(tDist(km, 1))}</b><span>${esc(drives.map((d) => `${d.from.name} → ${d.to.name}`).filter((v, i, a) => a.indexOf(v) === i).slice(0, 3).join(', '))}</span></div>`);
      panel.append(card);
      const b = boundsOf(all);
      m.frame(() => m.map.fitBounds(b, { padding: padClear(mapEl, $('.day-map-card', card), { top: 24, bottom: 40, left: 40, right: 60 }), maxZoom: 16, duration: 0 }));
    } catch (e) {
      console.warn(e);
      html(mapEl, '<div class="map-fallback">Map unavailable</div>');
    }
  };
  const ro = new ResizeObserver(() => { if (!started && panel.clientWidth > 0) start(); });
  ro.observe(panel);
  ctx.onCleanup(() => ro.disconnect());
}

// The day in numbers, beside the list on xwide screens: time in each state, then drives and charges.
function daySummaryHtml(tl, stateName) {
  const span = (tl.now || tl.end) - tl.start;
  const time = { driving: 0, charging: 0, parked: 0, asleep: 0, unknown: 0 };
  let drives = 0, km = 0, kwhUsed = 0, charges = 0, added = 0;
  for (const sg of tl.segments) {
    time[sg.kind] = (time[sg.kind] || 0) + (sg.end - sg.start);
    if (sg.kind === 'driving') { drives++; if (sg.detail && typeof sg.detail === 'object') { km += sg.detail.distanceKm || 0; kwhUsed += ((sg.detail.distanceKm || 0) * (sg.detail.effWhKm || 0)) / 1000; } }
    if (sg.kind === 'charging') { charges++; if (sg.detail && typeof sg.detail === 'object') added += sg.detail.addedKwh || 0; }
  }
  const rows = ['driving', 'charging', 'parked', 'asleep', 'unknown'].map((k) => [k, stateName(k), time[k]]);
  if (tl.now && tl.receivedUntil < tl.now) rows.push(['pending', 'Not received yet', tl.now - tl.receivedUntil]);
  return `<div class="panel-head"><h2 class="panel-title">${icon('stats')}The day</h2><span class="faint">${tl.now ? 'so far' : '24 h'}</span></div>
    <div class="day-sum-rows">${rows.map(([k, label, ms]) => `<div class="day-sum-row" data-state="${k}"><i data-state="${k}" aria-hidden="true"></i><span>${esc(label)}</span><b>${ms ? esc(dur(ms / 1000)) : '—'}</b>
      <div class="bar" data-w="${span ? ((ms / span) * 100).toFixed(1) : 0}" aria-hidden="true"><i></i></div></div>`).join('')}</div>
    <div class="day-sum-totals">
      <div class="metric metric--sm"><span class="metric-label">Drives</span><span class="metric-value">${nf(drives)}</span></div>
      <div class="metric metric--sm"><span class="metric-label">Distance</span><span class="metric-value">${fDist(km, 1)}</span></div>
      <div class="metric metric--sm"><span class="metric-label">Energy used</span><span class="metric-value">${fKwh(kwhUsed, 1)}</span></div>
      <div class="metric metric--sm"><span class="metric-label">Charge added</span><span class="metric-value">${fKwh(added, 1)}</span><span class="metric-sub">${nf(charges)} ${charges === 1 ? 'charge' : 'charges'}</span></div>
    </div>`;
}

// ------------------------------------------------------------------ placeholder pages
const SOON = {
  charges: { icon: 'bolt', title: 'Charges', lead: 'Every charge with its curve, the energy that went in, what came from the wall, and what it cost.', items: ['A list of charges with place, AC or DC, start and end battery and energy added', 'A charge page with power, voltage, current and battery % on one zoomable time axis', 'Losses: energy from the charger compared with energy into the pack', 'Cost from your tariffs and places, filters by place, AC/DC and date'] },
  battery: { icon: 'battery', title: 'Battery', lead: 'How the pack is ageing, read from the car’s own CAN data rather than an estimate from the app.', items: ['Range at 100% over time and an estimated capacity from long charges', 'Brick voltage spread, highest and lowest brick, and pack temperatures', 'Degradation compared with the car’s age and mileage', '12 V battery history'] },
  stats: { icon: 'stats', title: 'Stats', lead: 'TeslaMate’s dashboards rebuilt as pages: efficiency, vampire drain, mileage, temperatures, tires, visited places.', items: ['Efficiency by speed and outside temperature', 'Vampire drain split by asleep, awake and unknown time', 'Mileage per week, month and year; visited places on a map', 'Tire pressure and temperature history'] },
  signals: { icon: 'signals', title: 'Signals', lead: 'Any decoded CAN signal over any time range, the same values as Sentry Manager’s decoded export, on screen.', items: ['Search across every signal the car’s DBC decodes, on each bus', 'Plot several signals on one shared time axis, zoom to the frame', 'Export the range as CSV', 'Opened from a drive or charge with its time range filled in'] },
  logs: { icon: 'logs', title: 'Logs', lead: 'Every log the CAN Bridge uploaded, kept exactly as it arrived, with its trust result.', items: ['Signature check and the car key it was signed with', 'Chain report: gaps, restarts and swapped logs', 'Format (candump or columns), size and upload time', 'Download the original file'] },
  settings: { icon: 'settings', title: 'Settings', lead: 'Everything about this Sentry Mate server, for admins.', items: ['Cars: upload token, address, certificate fingerprint and signing key per car', 'People, passkeys and which cars each account can see', 'Units, tariffs, geofences, maps and geocoding, MQTT', 'Interface packs, backups and rebuilding from the raw logs'] },
};
function soonHtml(k) {
  const p = SOON[k];
  return `<div class="soon">
    <section class="panel soon-copy">
      <div class="soon-icon">${icon(p.icon)}</div>
      <h2>${p.title} will live here</h2>
      <p>${esc(p.lead)}</p>
      <ul class="soon-list">${p.items.map((i) => `<li>${icon('check')}<span>${esc(i)}</span></li>`).join('')}</ul>
      <span class="soon-note">${icon('info', 'ico--sm')} Not built yet in this design preview.</span>
    </section>
    <section class="panel soon-preview" aria-hidden="true">
      <div class="ph-row"><div class="ph-tile"></div><div class="ph-tile"></div><div class="ph-tile"></div><div class="ph-tile"></div></div>
      <div class="ph-chart"><svg viewBox="0 0 600 200" preserveAspectRatio="none"><path class="fill" d="M0 160 C60 150 90 90 150 100 S240 150 300 120 S400 40 460 60 S560 110 600 90 L600 200 L0 200 Z"/><path d="M0 160 C60 150 90 90 150 100 S240 150 300 120 S400 40 460 60 S560 110 600 90"/></svg></div>
      <div class="ph-lines"><div class="ph-line"></div><div class="ph-line"></div><div class="ph-line"></div></div>
    </section>
  </div>`;
}
async function viewSoon(ctx) {
  const [carId, page] = ctx.params;
  const car = await carById(carId);
  if (!car) throw Object.assign(new Error('No car with this address'), { status: 404 });
  recent.push(carId);
  setShell({ page, carId, crumbs: [{ label: 'All cars', href: '#/fleet' }, { label: car.name, href: `#/cars/${carId}/now` }, { label: PAGE_TITLES[page] }] });
  html(ctx.view, `<div class="page-head"><div><span class="eyebrow">${esc(car.name)}</span><h1 class="page-title">${PAGE_TITLES[page]}</h1></div></div>${soonHtml(page)}`);
}
async function viewSettings(ctx) {
  setShell({ page: 'settings', crumbs: [{ label: 'Settings' }] });
  html(ctx.view, `<div class="page-head"><div><span class="eyebrow">Your account and the server</span><h1 class="page-title">Settings</h1></div></div>
    <div class="settings-stack">
      <section class="panel panel-pad" aria-labelledby="set-display">
        <div class="panel-head"><h2 class="panel-title" id="set-display">${icon('user')}Display · your account</h2><span class="faint">Follows you to every screen you sign in from</span></div>
        <div class="setting">
          <div><h3 id="set-size-label">Interface size</h3>
            <p>Makes text, controls, tables, charts and maps bigger or smaller together. Auto fits the screen: the standard size on most screens, larger on a very large one such as a 4K monitor at 100% scaling. The percentages scale Auto's size, so 100% is the same as Auto and 150% is half as big again on every screen. Browser zoom works on top.</p></div>
          <div class="setting-control">
            <div class="size-picker" id="set-size" role="radiogroup" aria-labelledby="set-size-label">${sizePickerHtml('ui-size')}</div>
            <p class="setting-note" id="set-size-note" aria-live="polite"></p>
          </div>
        </div>
      </section>
      ${soonHtml('settings')}
    </div>`);
  const note = $('#set-size-note', ctx.view);
  const showNote = () => { note.textContent = `${autoNote()}.`; };
  showNote();
  ctx.onCleanup(bindSizePicker($('#set-size', ctx.view), showNote));
  ctx.onCleanup(display.onChange(showNote));
}

// The account menu (the person at the bottom of the sidebar): a popover with the Interface size, so it
// is one click away on any page, and a link to Settings.
function setupAccountMenu() {
  const btn = $('#account-btn'), menu = $('#account-menu'), picker = $('#am-size'), note = $('#am-size-note');
  html(picker, sizePickerHtml('ui-size-menu'));
  // what Auto is on this screen, when it is not simply the standard size
  const showNote = () => { const pct = display.autoPercent(); note.textContent = pct === 100 ? '' : `Auto = ${pct}% here`; };
  showNote();
  bindSizePicker(picker, showNote);
  // above the button, inside the window; placed before it shows, so it never flashes elsewhere
  const place = () => {
    const r = btn.getBoundingClientRect();
    menu.style.left = `${Math.max(rpx(8), Math.min(r.left, innerWidth - rpx(288)))}px`;
    menu.style.bottom = `${Math.max(rpx(8), innerHeight - r.top + rpx(8))}px`;
  };
  menu.addEventListener('beforetoggle', (e) => { if (e.newState === 'open') { place(); showNote(); } });
  menu.addEventListener('toggle', (e) => btn.setAttribute('aria-expanded', String(e.newState === 'open')));
  const follow = () => { showNote(); if (menu.matches(':popover-open')) place(); };
  window.addEventListener('resize', follow);
  display.onChange(follow);
  $('#am-settings').addEventListener('click', () => menu.hidePopover());
}

// ------------------------------------------------------------------ start
setupUnits();
setupAccountMenu();
// the person's saved Interface size (boot.js already applied this browser's copy before the first paint)
api.getPrefs().then((p) => { if (p && p.interfaceSize !== display.size) display.apply(p.interfaceSize); }).catch(() => {});
startClock();
showNotices();
api.fleetSummary().then((s) => { if (!lastSummary) { lastSummary = s; summaryEtag = s.etag; } setNavCount(s.total); }).catch(() => {});
window.addEventListener('hashchange', () => { if ($('#switcher').open) switcher.close(); route(); });
if (!location.hash) history.replaceState(null, '', '#/fleet');
route();
