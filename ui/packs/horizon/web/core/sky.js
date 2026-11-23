// The background: a dusk sky with stars, a striped setting sun with its glow, wireframe mountains, haze,
// faint scanlines and a neon grid floor that runs toward the horizon.
//
// Cost: everything but the floor is static (CSS and one SVG; the glow and the haze only fade with opacity).
// The floor is ONE canvas covering only the part below the horizon, drawn at most 30 times a second (12 in
// Calm), at devicePixelRatio capped at 1.5, with a single stroke per frame. It stops while the tab is hidden
// and is a still picture under prefers-reduced-motion.
//
// Background setting, kept per browser (localStorage), not on the server: Auto (Full on computers and
// tablets, Calm on phones), Full, Calm, Off (a plain dusk gradient).

import { svg, h, replace } from '../shared/dom.js';
import { onTheme } from './display.js';

export const CHOICES = Object.freeze([['auto', 'Auto'], ['full', 'Full'], ['calm', 'Calm'], ['off', 'Off']]);
const html = document.documentElement;
// boot.js (window.uiRoot) read this browser's choice before the first paint
const bootInfo = { SKY_KEY: window.uiRoot?.SKY_KEY || 'sentrymate.horizon.sky', skyChoice: window.uiRoot?.skyChoice || ((v) => (v === 'full' || v === 'calm' || v === 'off' ? v : 'auto')) };
const still = matchMedia('(prefers-reduced-motion: reduce)');
const S = { choice: bootInfo.skyChoice(html.dataset.skyChoice), mode: 'full', canvas: null, ctx: null, w: 0, h: 0, dpr: 1, phase: 0, raf: 0, last: 0, drawn: 0, frames: 0, holds: new Set() };
const subs = new Set();

function phone() { return matchMedia('(pointer: coarse)').matches && Math.min(screen.width, screen.height) < 600; }
function modeFor(choice) {
  if (choice === 'off') return 'off';
  if (still.matches) return 'still';
  if (choice === 'calm' || choice === 'full') return choice;
  return phone() ? 'calm' : 'full';
}

// a seeded random, so the mountains are the same on every load
function seeded(seed) { let s = seed >>> 0; return () => { s = (s * 1664525 + 1013904223) >>> 0; return s / 4294967296; }; }

function mountains() {
  const W = 1600, H = 220, r = seeded(20261120);
  const ridge = (x0, x1, peak) => {
    const pts = [[x0, H]];
    for (let i = 1; i < 10; i++) {
      const x = x0 + ((x1 - x0) * i) / 10 + (r() - 0.5) * 28;
      const middle = 1 - Math.abs(i / 10 - 0.5) * 1.7;
      pts.push([x, H - peak * (0.35 + 0.65 * middle) * (0.55 + r() * 0.45)]);
    }
    pts.push([x1, H]);
    return pts;
  };
  const pts = (list) => list.map(([x, y]) => `${Math.round(x)},${Math.round(y)}`).join(' ');
  const wires = (list) => {
    let d = '';
    for (let i = 1; i < list.length - 1; i++) {
      const [x, y] = list[i];
      d += `M${Math.round(x)} ${Math.round(y)}L${Math.round(x + (r() - 0.5) * 60)} ${H}`;
      if (i < list.length - 2) {
        const [nx, ny] = list[i + 1];
        d += `M${Math.round(x)} ${Math.round(y)}L${Math.round((x + nx) / 2)} ${Math.round(Math.max(y, ny) + (H - Math.max(y, ny)) * 0.45)}L${Math.round(nx)} ${Math.round(ny)}`;
      }
    }
    return d;
  };
  const range = (list, cls) => [
    svg('polygon', { class: `m-fill ${cls}`, points: pts(list) }),
    svg('path', { class: `m-wire ${cls}`, d: wires(list) }),
    svg('polyline', { class: `m-ridge ${cls}`, points: pts(list) }),
  ];
  return svg('svg', { class: 'sky-mtn', viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: 'xMidYMax slice', 'aria-hidden': 'true', focusable: 'false' },
    range(ridge(380, 1220, 90), 'far'), range(ridge(-40, 640, 200), 'near'), range(ridge(960, 1640, 170), 'near'));
}

function build() {
  const el = document.getElementById('sky');
  if (!el) return;
  S.canvas = h('canvas');
  replace(el, h('div', { class: 'sky-stars' }), h('div', { class: 'sky-glow' }), h('div', { class: 'sky-sun' }), mountains(),
    h('div', { class: 'sky-haze' }), h('div', { class: 'sky-floor' }, S.canvas), h('div', { class: 'sky-scan' }));
  S.ctx = S.canvas.getContext('2d', { alpha: true });
}

function fit() {
  const c = S.canvas;
  if (!c) return;
  const r = c.getBoundingClientRect();
  S.dpr = Math.min(1.5, window.devicePixelRatio || 1);
  const w = Math.max(1, Math.round(r.width * S.dpr)), hh = Math.max(1, Math.round(r.height * S.dpr));
  if (w !== c.width || hh !== c.height) { c.width = w; c.height = hh; }
  S.w = w; S.h = hh;
  paint();
}

function paint() {
  const { ctx, w, h: H } = S;
  if (!ctx || !w || !H) return;
  ctx.clearRect(0, 0, w, H);
  if (S.mode === 'off') return;
  const cx = w / 2;
  const unit = Math.max(w, H * 2.2) / 9;
  ctx.beginPath();
  // rows: y = H / z for z from the near edge (1) toward the horizon
  const ph = S.phase % 1;
  for (let k = 1; k < 64; k++) {
    const z = k - ph;
    if (z < 0.98) continue;
    const y = H / z;
    if (y < 1.5) break;
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
  }
  // columns, meeting at the vanishing point
  const cols = Math.ceil(w / 2 / unit) + 8;
  for (let i = -cols; i <= cols; i++) {
    const xb = cx + i * unit * 1.6;
    ctx.moveTo(cx + (xb - cx) * 0.02, 0);
    ctx.lineTo(xb, H);
  }
  ctx.strokeStyle = html.dataset.theme === 'light' ? 'rgba(194, 19, 125, 0.55)' : '#ff62cc';
  ctx.lineWidth = 1.5 * S.dpr;
  ctx.stroke();
}

const moving = () => (S.mode === 'full' || S.mode === 'calm') && !document.hidden && S.holds.size === 0;
function tick(t) {
  S.raf = 0;
  if (!moving()) return;
  const fps = S.mode === 'calm' ? 12 : 30;
  if (t - S.drawn >= 1000 / fps - 2) {
    const dt = S.last ? Math.min(0.1, (t - S.last) / 1000) : 0;
    S.last = t; S.drawn = t;
    S.phase += dt * (S.mode === 'calm' ? 0.18 : 0.55);
    S.frames++;
    paint();
  }
  S.raf = requestAnimationFrame(tick);
}
function run() {
  if (moving()) { if (!S.raf) { S.last = 0; S.raf = requestAnimationFrame(tick); } }
  else if (S.raf) { cancelAnimationFrame(S.raf); S.raf = 0; }
  html.classList.toggle('sky-moving', moving());
}

function apply() {
  S.mode = modeFor(S.choice);
  html.dataset.sky = S.mode;
  html.dataset.skyChoice = S.choice;
  fit();
  run();
  for (const f of [...subs]) { try { f(getSky()); } catch (e) { console.error(e); } }
}

/** { choice: auto | full | calm | off, mode: full | calm | still | off, frames } */
export const getSky = () => ({ choice: S.choice, mode: S.mode, frames: S.frames });

/** Choose the Background for this browser. */
export function setSky(choice) {
  S.choice = bootInfo.skyChoice(choice);
  try {
    if (S.choice === 'auto') localStorage.removeItem(bootInfo.SKY_KEY); else localStorage.setItem(bootInfo.SKY_KEY, JSON.stringify(S.choice));
  } catch { /* private window: this page only */ }
  apply();
}

/** Stop the floor while `reason` holds (a video, a heavy chart), and let it go on after. */
export function holdSky(reason, on) { if (on) S.holds.add(reason); else S.holds.delete(reason); run(); }

/** f(getSky()) after the Background changed. Returns off(). */
export function onSky(f) { subs.add(f); return () => subs.delete(f); }

/** Build the background once. */
export function startSky() {
  build();
  apply();
  let t = 0;
  window.addEventListener('resize', () => { clearTimeout(t); t = setTimeout(fit, 120); });
  document.addEventListener('visibilitychange', run);
  still.addEventListener?.('change', apply);
  onTheme(paint);
}
