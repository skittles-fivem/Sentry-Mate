// Sentry Mate, "Pebble" UI pack: how big things are drawn, and the bento grid's measures.
//
// Size. Everything is sized from one number, the root font size (app.css):
//   html font-size = the browser's standard size (100%) x --auto (follows the screen) x --ui-size
// Auto is 1 up to a 1440 x 810 screen, rising in a straight line to 1.45 at 3840 x 2160 and capped there (the
// long and the short side both count; Firefox reports the screen scaled by its zoom, so there it is 1). "Follow
// screen size" off makes Auto 1. Interface size is 50 to 200 %. boot.js has the same formula for the first
// paint: change both together. Both choices live in the person's preferences (interface_size, follow_screen);
// this browser keeps a copy only for boot.js.
//
// Device pixels. --dp on <html> is one device pixel in CSS px. app.css rounds lengths to it, so tile edges,
// borders and lines sit on whole device pixels at every browser zoom (50 to 200 %) and on 2x and 3x screens.
//
// The grid. layout() works out the bento grid for the window in whole device pixels and puts it on <html>:
//   --cols (2 to 8), --col (one column), --gap, --gutter, --grid-w (the grid's width), --grid-x (its left edge)
//   data-form: 'phone' (under 600 CSS px wide: icons-only dock, 2 columns), 'compact' (icons-only dock) or 'wide'.
// Columns are 17.5rem to 22.5rem wide (280 to 360 px at the standard size), 4 at 1280, 6 at 1920 and 8 at most:
// a big screen shows more tiles, not bigger ones, and the grid sits in the middle with equal margins.

const root = document.documentElement;

export const SIZES = Object.freeze([50, 75, 90, 100, 110, 125, 150, 175, 200]);
export const SIZE_MIN = 50;
export const SIZE_MAX = 200;
const CACHE_KEY = 'pebble:size';
const GECKO_ZOOMED_SCREEN = /\bFirefox\//.test(navigator.userAgent || '');

/** Auto for a screen of w x h CSS px: 1 up to 1440 x 810, 1.45 at 3840 x 2160 and above (either way up). */
export function autoFactor(w = window.screen && window.screen.width, h = window.screen && window.screen.height) {
  if (GECKO_ZOOMED_SCREEN) return 1;
  if (!(w > 0) || !(h > 0)) return 1;
  const long = Math.max(w, h), short = Math.min(w, h);
  const f = Math.max(0, Math.min(1, Math.min((long - 1440) / 2400, (short - 810) / 1350)));
  return Math.round((1 + 0.45 * f) * 1000) / 1000;
}

function readCache() {
  try { const v = JSON.parse(localStorage.getItem(CACHE_KEY) || 'null'); return v && typeof v === 'object' ? v : null; } catch { return null; }
}
const clampSize = (v) => (Number.isFinite(+v) && v !== null && v !== '' ? Math.max(SIZE_MIN, Math.min(SIZE_MAX, Math.round(+v))) : 100);
const cached = readCache();
let size = clampSize(cached?.size ?? 100);
let follow = cached?.follow !== false;
const sizeListeners = new Set();

/** { size, follow, auto }: the person's choice and Auto for this screen. */
export const sizeState = () => ({ size, follow, auto: follow ? autoFactor() : 1 });

function applySize() {
  root.style.setProperty('--auto', String(follow ? autoFactor() : 1));
  root.style.setProperty('--ui-size', String(size / 100));
  try { localStorage.setItem(CACHE_KEY, JSON.stringify({ size, follow })); } catch { /* storage blocked: fine */ }
  for (const f of [...sizeListeners]) { try { f(sizeState()); } catch (e) { console.error(e); } }
  check();
}

/** Set the Interface size (percent) and Follow screen size; values not given stay. Returns true on a change. */
export function setSize({ size: s = size, follow: f = follow } = {}) {
  const ns = clampSize(s), nf = f !== false;
  if (ns === size && nf === follow) return false;
  size = ns; follow = nf;
  applySize();
  return true;
}
/** fn({ size, follow, auto }) after the size choice or Auto changed. Returns a remover. */
export function onSize(fn) { sizeListeners.add(fn); return () => sizeListeners.delete(fn); }

/* ------------------------------------------------------------------ device pixels and the grid */

const dprNow = () => window.devicePixelRatio || 1;
/** Round a CSS px length to whole device pixels. */
export const snap = (v, dpr = dprNow()) => Math.round(v * dpr) / dpr;
const floorDev = (v, dpr = dprNow()) => Math.floor(v * dpr + 1e-6) / dpr;

const remNow = () => parseFloat(getComputedStyle(root).fontSize) || 16;
let cur = { rem: 0, dpr: 0, width: 0 };
let grid = null;
const listeners = new Set();
const gridListeners = new Set();

/** The grid for a window w CSS px wide, a root size rem and a pixel ratio dpr (pure: tests and pages use it). */
export function gridFor(w, rem, dpr) {
  const phone = w < 600;
  const gap = Math.max(1, snap(rem, dpr));
  const gutter = phone ? gap : Math.max(gap, snap(2 * rem, dpr));
  const avail = Math.max(0, w - 2 * gutter);
  const min = 17.5 * rem, max = 22.5 * rem;
  let cols = phone ? 2 : Math.floor((avail + gap) / (min + gap));
  cols = Math.max(2, Math.min(8, cols));
  const col = Math.max(0, floorDev(Math.min(max, (avail - (cols - 1) * gap) / cols), dpr));
  const width = cols * col + (cols - 1) * gap;
  const x = snap((w - width) / 2, dpr);
  // labels on every dock item only where they fit: from 62rem of window
  const form = phone ? 'phone' : w < 62 * rem ? 'compact' : 'wide';
  return { cols, col, gap, gutter, width, x, form, phone, rem, dpr };
}

/** The box the page scrolls in (it ends above the dock's zone). */
export const scroller = () => document.getElementById('scroller') || document.scrollingElement || root;
/** Scroll the page back to the top. */
export function scrollToTop() { scroller().scrollTo(0, 0); }
const widthNow = () => scroller().clientWidth || root.clientWidth || window.innerWidth;

function layout() {
  const w = widthNow();
  const g = gridFor(w, cur.rem, cur.dpr);
  const same = grid && ['cols', 'col', 'gap', 'gutter', 'width', 'x', 'form'].every((k) => grid[k] === g[k]);
  grid = g;
  if (same) return;
  root.style.setProperty('--cols', String(g.cols));
  root.style.setProperty('--col', `${g.col}px`);
  root.style.setProperty('--gap', `${g.gap}px`);
  root.style.setProperty('--gutter', `${g.gutter}px`);
  root.style.setProperty('--grid-w', `${g.width}px`);
  root.style.setProperty('--grid-x', `${g.x}px`);
  root.dataset.cols = String(g.cols);
  root.dataset.form = g.form;
  for (const f of [...gridListeners]) { try { f(g); } catch (e) { console.error(e); } }
}

function check() {
  const next = { rem: remNow(), dpr: dprNow(), width: widthNow() };
  const change = { rem: next.rem !== cur.rem, dpr: next.dpr !== cur.dpr, width: next.width !== cur.width };
  if (!change.rem && !change.dpr && !change.width) return;
  cur = next;
  if (change.dpr) { root.style.setProperty('--dp', `${1 / cur.dpr}px`); watchRatio(); }
  layout();
  if (change.rem || change.dpr) for (const fn of [...listeners]) { try { fn(change); } catch (err) { console.error(err); } }
}

let mq = null;
function watchRatio() {
  if (mq) mq.removeEventListener('change', onRatio);
  mq = matchMedia(`(resolution: ${cur.dpr}dppx)`);
  mq.addEventListener('change', onRatio);
}
function onRatio() { applySize(); } // another screen or a new zoom: Auto may change too

/** Start watching (start.js calls it once, after <body> exists). */
export function startDisplay() {
  const probe = document.createElement('div');
  probe.className = 'rem-probe';
  probe.setAttribute('aria-hidden', 'true');
  document.body.append(probe);
  const ro = new ResizeObserver(check);
  try { ro.observe(probe, { box: 'device-pixel-content-box' }); } catch { ro.observe(probe); }
  new ResizeObserver(check).observe(root);
  if (document.getElementById('scroller')) new ResizeObserver(check).observe(document.getElementById('scroller'));
  let last = `${screen.width}x${screen.height}`;
  const screenCheck = () => { const now = `${screen.width}x${screen.height}`; if (now !== last) { last = now; applySize(); } else check(); };
  addEventListener('resize', screenCheck);
  if (window.screen && typeof window.screen.addEventListener === 'function') window.screen.addEventListener('change', screenCheck);
  applySize();
  check();
}

export const display = {
  /** The root font size in CSS px. */
  get rem() { return cur.rem || remNow(); },
  get dpr() { return cur.dpr || dprNow(); },
  /** For drawings designed at the standard 16 px root (chart fonts, line widths): multiply their px by this. */
  get scale() { return (cur.rem || remNow()) / 16; },
  /** The grid now: { cols, col, gap, gutter, width, x, form, phone }. */
  get grid() { if (!grid) { cur = { rem: remNow(), dpr: dprNow(), width: widthNow() }; layout(); } return grid; },
  /** fn({ rem, dpr }) when the root size or the pixel ratio changed (charts redraw). Returns a remover. */
  onChange(fn) { listeners.add(fn); return () => listeners.delete(fn); },
  /** fn(grid) when the grid changed (columns, widths, the dock's form). Returns a remover. */
  onGrid(fn) { gridListeners.add(fn); return () => gridListeners.delete(fn); },
  refresh: check,
};

/** "Text here is 145% of standard (Auto 145% for this screen x size 100%)." */
export function sizeNote() {
  const pct = Math.round((display.rem / 16) * 100);
  const s = sizeState();
  return follow
    ? `Text here is ${pct}% of standard (Auto ${Math.round(s.auto * 100)}% for this screen × size ${size}%).`
    : `Text here is ${pct}% of standard (size ${size}%; not following the screen).`;
}
