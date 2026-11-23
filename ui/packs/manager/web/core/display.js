// Display: Interface size, Follow screen size, theme, and the pixel ratio for canvases and maps.
//
// The root font size is set in one place (app.css "root size"): browser default x Auto x Interface size.
// boot.js (window.uiRoot) applies the saved copy before the first paint; this module owns the values
// afterwards. Every other size is in rem, so layout follows by itself. Canvases (uPlot) and maps
// (MapLibre) draw in pixels, so they subscribe to onChange() and redraw when:
// - the pixel ratio changed: the window moved to another monitor, or browser zoom. Browsers report this
//   with the "(resolution: ...)" media query; DevTools device emulation changes devicePixelRatio without
//   an event, so until the browser has fired that query once, a check every half second stands in;
// - the Interface size or Follow screen size changed (account menu, Settings, another device);
// - the root size changed for another reason (the window moved to a screen with another Auto).
// Window resizes alone do not change the root size: Auto follows the screen, not the window.

const ui = window.uiRoot || {
  MIN: 75, MAX: 200, KEY: 'sentrymate.interface', THEME_KEY: 'sentrymate.theme',
  autoFactor: () => 1, apply: (s) => ({ size: 100, follow: true, auto: 1, ...s }), clampSize: (v) => v,
  effectiveTheme: () => 'dark', applyTheme: () => 'dark', saved: () => null,
};

/** The Interface sizes offered in the pickers (percent). Any whole number from 75 to 200 is valid. */
export const SIZES = Object.freeze([75, 90, 100, 110, 125, 150, 175, 200]);

function store(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* storage blocked: fine */ }
}

// uPlot keeps the pixel ratio in a variable of its own and follows its own "(resolution)" media query.
// When this module notices a change that query did not report, it replays a 'change' event on uPlot's
// query, found by watching matchMedia only while uPlot's code runs.
export const uplotRatio = (() => {
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

const root = document.documentElement;
const subs = new Set();
const ifaceSubs = new Set();
const themeSubs = new Set();
const read = () => ({ rootPx: parseFloat(getComputedStyle(root).fontSize) || 16, dpr: window.devicePixelRatio || 1 });

let iface = ui.apply(ui.saved() || {}); // { size, follow, auto }
let themeChoice = root.dataset.themeChoice || 'device';
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
    for (const f of [...subs]) { try { f(ev); } catch (e) { console.error(e); } }
  });
}
// the window may be on another screen now: Auto follows the screen
function screenCheck() {
  const k = `${screen.width}x${screen.height}`;
  if (k === screenKey) return;
  screenKey = k;
  iface = ui.apply(iface);
  for (const f of [...ifaceSubs]) { try { f({ ...iface }); } catch (e) { console.error(e); } }
}
let silentCheck = 0;
function onDpr() { clearInterval(silentCheck); screenCheck(); check('dpr'); }
function watchDpr(dpr) {
  mq?.removeEventListener('change', onDpr);
  mq = window.matchMedia(`(resolution: ${dpr}dppx)`);
  mq.addEventListener('change', onDpr);
}
watchDpr(state.dpr);
silentCheck = setInterval(() => { if ((window.devicePixelRatio || 1) !== state.dpr) check('dpr'); }, 500);
const onScreen = () => { screenCheck(); check('root'); };
window.addEventListener('resize', onScreen);
screen.addEventListener?.('change', onScreen);
document.addEventListener('visibilitychange', () => { if (!document.hidden) onScreen(); });

// the device theme can change while the page is open (a laptop at sunset)
const darkMq = window.matchMedia('(prefers-color-scheme: light)');
darkMq.addEventListener?.('change', () => { if (themeChoice === 'device') applyTheme('device', { save: false }); });

/** The Interface size now: { size (75..200), follow (bool), auto (the factor in use) }. */
export const getInterface = () => ({ ...iface });

/**
 * Apply an Interface size and/or Follow screen size now ({ size, follow }); the rest stays. Keeps a copy
 * in this browser for the next first paint. Saving to the server is the caller's job (core/prefs.js).
 */
export function applyInterface(patch = {}) {
  const next = { size: patch.size ?? iface.size, follow: patch.follow ?? iface.follow };
  const before = iface;
  iface = ui.apply(next);
  store(ui.KEY, { size: iface.size, follow: iface.follow });
  check('size');
  if (before.size !== iface.size || before.follow !== iface.follow || before.auto !== iface.auto) {
    for (const f of [...ifaceSubs]) { try { f({ ...iface }); } catch (e) { console.error(e); } }
  }
  return { ...iface };
}

/** What Auto is on this screen, in percent of the standard size (100 on most screens, 145 on 4K). */
export const autoPercent = () => Math.round(ui.autoFactor() * 100);

/** The size the person sees, in percent of the standard size: Auto x Interface size. */
export const effectivePercent = () => Math.round(iface.auto * iface.size);

/** The theme choice ('light' | 'dark' | 'device') and the one drawn now ('light' | 'dark'). */
export const getTheme = () => ({ choice: themeChoice, effective: root.dataset.theme || 'dark' });

/** Apply a theme choice now; keeps a copy in this browser for the next first paint. */
export function applyTheme(choice, { save = true } = {}) {
  themeChoice = choice === 'light' || choice === 'dark' ? choice : 'device';
  const before = root.dataset.theme;
  const eff = ui.applyTheme(themeChoice);
  if (save) store(ui.THEME_KEY, themeChoice);
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute('content', eff === 'light' ? '#F4F7FB' : '#070B15');
  if (before !== eff || save) for (const f of [...themeSubs]) { try { f(getTheme()); } catch (e) { console.error(e); } }
  return eff;
}

/** px at the 16 px design size -> CSS px now. For the few sizes JS hands to canvases and maps. */
export const rpx = (n) => (n * state.rootPx) / 16;
export const rootPx = () => state.rootPx;
export const dpr = () => state.dpr;

/** f({ rootPx, dpr, dprChanged, sizeChanged }) after the root size or the pixel ratio changed. Returns off(). */
export function onChange(f) { subs.add(f); return () => subs.delete(f); }
/** f({ size, follow, auto }) after the Interface size or Follow screen size changed. Returns off(). */
export function onInterface(f) { ifaceSubs.add(f); return () => ifaceSubs.delete(f); }
/** f({ choice, effective }) after the theme changed. Returns off(). Charts and maps redraw with new colours. */
export function onTheme(f) { themeSubs.add(f); return () => themeSubs.delete(f); }
