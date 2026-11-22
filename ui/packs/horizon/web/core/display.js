// Display: the Interface size, Follow screen size, the theme, and the pixel ratio canvases and maps draw at.
//
// boot.js (window.hzBoot) puts this browser's copy into effect before the first paint; this module owns the
// values afterwards. The root font size is the only size that changes (app.css "root size"); every other
// length is in rem. Canvases and maps draw in device pixels, so they listen with onRedraw() and draw again
// when the pixel ratio changes (another monitor, browser zoom), the Interface size changes, the root size
// changes for another reason (Auto on another screen) or the theme changes.

const boot = window.hzBoot || {
  LOW: 75, HIGH: 200, SIZE_KEY: 'sentrymate.interface', THEME_KEY: 'sentrymate.theme', SKY_KEY: 'sentrymate.horizon.sky',
  autoFactor: () => 1, applySize: (s) => ({ size: 100, follow: true, auto: 1, ...s }), clampSize: (v) => v,
  themeFor: () => 'dark', applyTheme: () => 'dark', skyChoice: (v) => v || 'auto', savedSize: () => null, savedSky: () => 'auto',
};

/** The Interface sizes the pickers offer (percent). Any whole number from 75 to 200 is valid. */
export const SIZES = Object.freeze([75, 90, 100, 110, 125, 150, 175, 200]);

const html = document.documentElement;
const keep = (key, value) => { try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* storage blocked */ } };
const lists = { redraw: new Set(), size: new Set(), theme: new Set() };
const tell = (set, v) => { for (const f of [...set]) { try { f(v); } catch (e) { console.error(e); } } };

let size = boot.applySize(boot.savedSize() || {});      // { size, follow, auto }
let themeChoice = html.dataset.themeChoice || 'device';
const measure = () => ({ rootPx: parseFloat(getComputedStyle(html).fontSize) || 16, dpr: window.devicePixelRatio || 1 });
let now = measure();
let pending = new Set();
let raf = 0;
let screenKey = `${screen.width}x${screen.height}`;

function recheck(cause) {
  const m = measure();
  if (Math.abs(m.rootPx - now.rootPx) < 0.005 && m.dpr === now.dpr) return;
  if (m.dpr !== now.dpr) { pending.add('dpr'); watchRatio(m.dpr); }
  pending.add(cause);
  now = m;
  cancelAnimationFrame(raf);
  raf = requestAnimationFrame(() => {
    const ev = { ...now, dprChanged: pending.has('dpr'), sizeChanged: pending.has('size') };
    pending = new Set();
    tell(lists.redraw, ev);
  });
}

// the window moved to another screen: Auto follows the screen
function screenMoved() {
  const k = `${screen.width}x${screen.height}`;
  if (k === screenKey) return;
  screenKey = k;
  size = boot.applySize(size);
  tell(lists.size, { ...size });
}

let ratioQuery = null;
let poll = 0;
function onRatio() { clearInterval(poll); screenMoved(); recheck('dpr'); }
function watchRatio(dpr) {
  ratioQuery?.removeEventListener('change', onRatio);
  ratioQuery = matchMedia(`(resolution: ${dpr}dppx)`);
  ratioQuery.addEventListener('change', onRatio);
}
watchRatio(now.dpr);
// device emulation changes the ratio without an event: look twice a second until the query has fired once
poll = setInterval(() => { if ((window.devicePixelRatio || 1) !== now.dpr) recheck('dpr'); }, 500);
const onWindow = () => { screenMoved(); recheck('root'); };
window.addEventListener('resize', onWindow);
screen.addEventListener?.('change', onWindow);
document.addEventListener('visibilitychange', () => { if (!document.hidden) onWindow(); });
matchMedia('(prefers-color-scheme: light)').addEventListener?.('change', () => { if (themeChoice === 'device') applyTheme('device', { save: false }); });

/** { size: 75..200, follow, auto } now. */
export const getInterface = () => ({ ...size });

/** Apply { size, follow } now (the rest stays) and keep a copy for the next first paint. Saving is core/prefs.js's job. */
export function applyInterface(patch = {}) {
  const before = size;
  size = boot.applySize({ size: patch.size ?? size.size, follow: patch.follow ?? size.follow });
  keep(boot.SIZE_KEY, { size: size.size, follow: size.follow });
  recheck('size');
  if (before.size !== size.size || before.follow !== size.follow || before.auto !== size.auto) tell(lists.size, { ...size });
  return { ...size };
}

/** Auto on this screen in percent (100 on most screens, 145 on a 4K one). */
export const autoPercent = () => Math.round(boot.autoFactor() * 100);
/** The size the person sees: Auto x Interface size, in percent of the standard size. */
export const shownPercent = () => Math.round(size.auto * size.size);

/** { choice: 'light' | 'dark' | 'device', look: 'light' | 'dark' } */
export const getTheme = () => ({ choice: themeChoice, look: html.dataset.theme || 'dark' });

/** Apply a theme choice now and keep a copy for the next first paint. */
export function applyTheme(choice, { save = true } = {}) {
  themeChoice = choice === 'light' || choice === 'dark' ? choice : 'device';
  const before = html.dataset.theme;
  const look = boot.applyTheme(themeChoice);
  if (save) keep(boot.THEME_KEY, themeChoice);
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', look === 'light' ? '#fbf7ff' : '#0a051e');
  if (before !== look || save) tell(lists.theme, getTheme());
  return look;
}

/** px at the 16 px design size -> CSS px now, for the few sizes JS hands to canvases and maps. */
export const rpx = (n) => (n * now.rootPx) / 16;
export const rootPx = () => now.rootPx;
export const dpr = () => now.dpr;

/** f({ rootPx, dpr, dprChanged, sizeChanged }) when canvases must draw again. Returns off(). */
export function onRedraw(f) { lists.redraw.add(f); return () => lists.redraw.delete(f); }
/** f({ size, follow, auto }) after the Interface size or Follow screen size changed. Returns off(). */
export function onInterface(f) { lists.size.add(f); return () => lists.size.delete(f); }
/** f({ choice, look }) after the theme changed. Returns off(). */
export function onTheme(f) { lists.theme.add(f); return () => lists.theme.delete(f); }
/** The boot module's helpers (keys, clamps) for core/sky.js. */
export const bootInfo = boot;
