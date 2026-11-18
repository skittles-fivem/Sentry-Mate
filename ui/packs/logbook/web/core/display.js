// Sentry Mate, "Logbook" UI pack: how big things are drawn.
//
// The size of everything is one number, the root font size, set in app.css from two CSS custom
// properties on <html>:
//
//   html font-size = the browser's standard size (100%, 16 px unless the person changed it)
//                    x --auto (Auto: follows the screen) x --ui-size (Interface size / 100)
//
// Auto is worked out from the screen's size in CSS pixels (screen.width x screen.height): 1 up to a
// 1440 x 900 screen, rising in a straight line to 1.45 at 3840 x 2160, whichever of width and height
// is nearer its start (an ultra-wide 3440 x 1440 gets the growth its height allows). The person's
// "Follow screen size" switch (on by default) turns Auto off (1). Interface size is 75 to 200 %
// (default 100). Both are stored with the person's preferences on the server (interface_size,
// follow_screen); this browser keeps a copy so boot.js can size the very first paint. boot.js has the
// same formula: change both together.
//
// Canvases (uPlot charts) and maps draw in device pixels, so they need to hear when the root size or
// the pixel ratio changes: display.onChange(fn). Two standard signals, no polling: a 1rem square
// watched by a ResizeObserver in device pixels, and a resolution media query.

const root = document.documentElement;

export const SIZES = Object.freeze([75, 90, 100, 110, 125, 150, 175, 200]);
export const SIZE_MIN = 75;
export const SIZE_MAX = 200;
const CACHE_KEY = 'logbook:size';

/** Auto for a screen of w x h CSS px: 1 up to 1440 x 900, 1.45 at 3840 x 2160 and above. */
export function autoFactor(w = window.screen && window.screen.width, h = window.screen && window.screen.height) {
  if (!(w > 0) || !(h > 0)) return 1;
  const fw = (w - 1440) / (3840 - 1440);
  const fh = (h - 900) / (2160 - 900);
  const f = Math.max(0, Math.min(1, Math.min(fw, fh)));
  return Math.round((1 + 0.45 * f) * 1000) / 1000;
}

function readCache() {
  try { const v = JSON.parse(localStorage.getItem(CACHE_KEY) || 'null'); return v && typeof v === 'object' ? v : null; } catch { return null; }
}
function writeCache() {
  try { localStorage.setItem(CACHE_KEY, JSON.stringify({ size, follow })); } catch { /* storage blocked: fine */ }
}

const clampSize = (v) => (Number.isFinite(+v) ? Math.max(SIZE_MIN, Math.min(SIZE_MAX, Math.round(+v))) : 100);

const cached = readCache();
let size = clampSize(cached?.size ?? 100);
let follow = cached?.follow !== false;
const sizeListeners = new Set();

/** Put the current Auto and Interface size on <html> (CSSOM, which the page's CSP allows). */
export function applySize() {
  const auto = follow ? autoFactor() : 1;
  root.style.setProperty('--auto', String(auto));
  root.style.setProperty('--ui-size', String(size / 100));
  root.dataset.sized = '';
  writeCache();
  for (const f of [...sizeListeners]) { try { f({ size, follow, auto }); } catch (e) { console.error(e); } }
  check();
}

/** { size, follow, auto }: the person's choice and the Auto factor of this screen. */
export const sizeState = () => ({ size, follow, auto: follow ? autoFactor() : 1 });
/** Set the Interface size (percent) and Follow screen size; values that are not given stay. */
export function setSize({ size: s = size, follow: f = follow } = {}) {
  const ns = clampSize(s), nf = f !== false;
  if (ns === size && nf === follow) return false;
  size = ns; follow = nf;
  applySize();
  return true;
}
/** fn({ size, follow, auto }) after the size choice or Auto changed. Returns a remover. */
export function onSize(fn) { sizeListeners.add(fn); return () => sizeListeners.delete(fn); }

/* ------------------------------------------------------------------ rem and pixel ratio */

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
let mq = null;
function onRatio() { applySize(); } // another screen: Auto may change too
function watchRatio() {
  if (mq) mq.removeEventListener('change', onRatio);
  mq = matchMedia(`(resolution: ${cur.dpr}dppx)`);
  mq.addEventListener('change', onRatio);
}

let probe = null, standardProbe = null;
/** Start watching (called once by start.js, after <body> exists). */
export function startDisplay() {
  probe = document.createElement('div');
  probe.className = 'rem-probe';
  probe.setAttribute('aria-hidden', 'true');
  standardProbe = document.createElement('div');
  standardProbe.className = 'standard-probe';
  standardProbe.setAttribute('aria-hidden', 'true');
  document.body.append(probe, standardProbe);
  const ro = new ResizeObserver(check);
  try { ro.observe(probe, { box: 'device-pixel-content-box' }); } catch { ro.observe(probe); }
  watchRatio();
  // A move to another screen: a resize, a new pixel ratio (above) or the screen's own change event.
  let last = `${screen.width}x${screen.height}`;
  const screenCheck = () => { const now = `${screen.width}x${screen.height}`; if (now !== last) { last = now; applySize(); } else check(); };
  addEventListener('resize', screenCheck);
  if (window.screen && typeof window.screen.addEventListener === 'function') window.screen.addEventListener('change', screenCheck);
  applySize();
}

export const display = {
  /** The root font size in CSS px. */
  get rem() { return cur.rem; },
  get dpr() { return cur.dpr; },
  /** The browser's standard size in CSS px (16 unless the person changed it): what 100% means. */
  get standard() { return standardProbe ? parseFloat(getComputedStyle(standardProbe).fontSize) || 16 : 16; },
  /** For drawings designed at the standard 16 px root (chart fonts, line widths): multiply their px by this. */
  get scale() { return cur.rem / 16; },
  /** fn({ rem, dpr }) with what changed. Returns a function that stops it. */
  onChange(fn) { listeners.add(fn); return () => listeners.delete(fn); },
  /** Look now and tell the listeners if anything changed. */
  refresh: check,
};

/** "Text on this screen: 145% of standard (Auto 145% x size 100%)." */
export function sizeNote() {
  const pct = Math.round((display.rem / display.standard) * 100);
  const s = sizeState();
  const auto = Math.round(s.auto * 100);
  return follow
    ? `Text on this screen: ${pct}% of standard (Auto ${auto}% for this screen × size ${size}%).`
    : `Text on this screen: ${pct}% of standard (size ${size}%; not following the screen).`;
}

/**
 * Keep the reader's place when the size changes: whatever is at the middle of the window stays at the
 * same height on screen, so going from 100% to 200% leaves the row you were reading in view.
 */
export function keepReadingPlace(mainEl) {
  let anchor = null, frame = 0;
  function remember() {
    frame = 0;
    check();
    anchor = null;
    if (window.scrollY < 2) return;
    const hit = document.elementFromPoint(innerWidth / 2, innerHeight / 2);
    const el = hit && hit.closest('tr, li, article, section, header, figure, p, dl, h1, h2');
    if (el && mainEl.contains(el)) { const b = el.getBoundingClientRect(); anchor = { el, at: (b.top + b.height / 2) / innerHeight }; }
  }
  addEventListener('scroll', () => { if (!frame) frame = requestAnimationFrame(remember); }, { passive: true });
  display.onChange((change) => {
    if (!change.rem || !anchor || !anchor.el.isConnected) return;
    const box = anchor.el.getBoundingClientRect();
    const want = Math.max(0, Math.min(anchor.at * innerHeight - box.height / 2, innerHeight - Math.min(box.height, innerHeight)));
    window.scrollBy(0, box.top - want);
    remember();
  });
}
