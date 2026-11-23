// Display: Interface size, Follow screen size, theme, and the root size canvases and maps need.
//
// app.css works the root size out from --ui-size and --ui-follow (set by boot.js and by this module) and the
// window width; every other length is a whole number of px derived from it. This module keeps the
// person's choice, saves a copy for the next first paint, and tells charts and maps when they must redraw:
// the root size changed (a resize, the Interface size), the pixel ratio changed (browser zoom, another
// screen), or the theme changed.

const ui = window.consoleRoot || {
  MIN: 75, MAX: 200, KEY: 'sentrymate.console.interface', THEME_KEY: 'sentrymate.console.theme',
  apply: (s) => ({ size: 100, follow: true, ...s }), clampSize: (v) => v,
  effectiveTheme: () => 'dark', applyTheme: () => 'dark', saved: () => null,
};

/** The Interface sizes the account menu steps through (percent). Any whole number from 75 to 200 is valid. */
export const SIZES = Object.freeze([75, 90, 100, 110, 125, 150, 175, 200]);

const root = document.documentElement;
const store = (key, value) => { try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* blocked: fine */ } };
const read = () => ({ rootPx: parseFloat(getComputedStyle(root).fontSize) || 16, dpr: window.devicePixelRatio || 1 });

const subs = new Set();
const themeSubs = new Set();
let iface = ui.apply(ui.saved() || {});
let themeChoice = root.dataset.themeChoice || 'device';
let state = read();
let frame = 0;
let mq = null;

function check() {
  const now = read();
  if (now.rootPx === state.rootPx && now.dpr === state.dpr) return;
  const dprChanged = now.dpr !== state.dpr;
  state = now;
  if (dprChanged) watchDpr(now.dpr);
  cancelAnimationFrame(frame);
  frame = requestAnimationFrame(() => {
    for (const f of [...subs]) { try { f({ ...state, dprChanged }); } catch (e) { console.error(e); } }
  });
}
function watchDpr(dpr) {
  mq?.removeEventListener('change', check);
  mq = window.matchMedia(`(resolution: ${dpr}dppx)`);
  mq.addEventListener('change', check);
}
watchDpr(state.dpr);
window.addEventListener('resize', check);

const lightMq = window.matchMedia('(prefers-color-scheme: light)');
lightMq.addEventListener?.('change', () => { if (themeChoice === 'device') applyTheme('device', { save: false }); });

/** { size: 75..200, follow: bool } now. */
export const getInterface = () => ({ ...iface });

/** Apply { size, follow } now (the rest stays) and keep a copy for the next first paint. Saving is core/prefs.js's job. */
export function applyInterface(patch = {}) {
  iface = ui.apply({ size: patch.size ?? iface.size, follow: patch.follow ?? iface.follow });
  store(ui.KEY, iface);
  check();
  return { ...iface };
}

/** The next or previous size of SIZES from the size now (dir 1 or -1). */
export function stepSize(dir) {
  const s = iface.size;
  const list = dir > 0 ? SIZES.filter((x) => x > s) : SIZES.filter((x) => x < s).reverse();
  return list[0] ?? s;
}

/** { choice: 'light' | 'dark' | 'device', effective: 'light' | 'dark' } */
export const getTheme = () => ({ choice: themeChoice, effective: root.dataset.theme || 'dark' });

/** Apply a theme choice now; keeps a copy for the next first paint. */
export function applyTheme(choice, { save = true } = {}) {
  themeChoice = choice === 'light' || choice === 'dark' ? choice : 'device';
  const eff = ui.applyTheme(themeChoice);
  if (save) store(ui.THEME_KEY, themeChoice);
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', eff === 'light' ? '#F7F7F5' : '#0E1116');
  for (const f of [...themeSubs]) { try { f(getTheme()); } catch (e) { console.error(e); } }
  return eff;
}

/** The root size in CSS px (a whole number), and the grid unit (half of it, rounded). */
export const rootPx = () => state.rootPx;
export const unit = () => Math.round(state.rootPx / 2);
/** px at the 16 px design size -> whole CSS px now. For the few sizes JS hands to canvases and maps. */
export const rpx = (n) => Math.max(1, Math.round((n * state.rootPx) / 16));
export const dpr = () => state.dpr;

/** f({ rootPx, dpr, dprChanged }) after the root size or the pixel ratio changed. Returns off(). */
export function onChange(f) { subs.add(f); return () => subs.delete(f); }
/** f({ choice, effective }) after the theme changed. Returns off(). */
export function onTheme(f) { themeSubs.add(f); return () => themeSubs.delete(f); }
