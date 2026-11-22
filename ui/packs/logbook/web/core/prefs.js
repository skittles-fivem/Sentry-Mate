// Sentry Mate, "Logbook" UI pack: the person's preferences (GET / PATCH /api/v1/me/preferences).
//
// applyPrefs() puts them to work: units and rated or ideal range (shared/units.js), the language and
// <html lang> (shared/i18n.js), the theme (data-theme on <html>), and the interface size (display.js).
// savePrefs(patch) saves at once and applies the answer. onPrefs(fn) hears every change, also those
// pushed by the server (a `prefs` event, frame.js), with the set of keys that changed. onTheme(fn) hears
// when the page is drawn dark or light from then on (charts and maps redraw their colours).

import * as api from '../shared/api.js';
import { setUnits } from '../shared/units.js';
import { setLocale } from '../shared/i18n.js';
import { cover } from '../shared/dom.js';
import { setSize } from './display.js';

const root = document.documentElement;
const THEME_KEY = 'logbook:theme';
let prefs = null;
const listeners = new Set();

/** The preferences in use (null before the first load). */
export const currentPrefs = () => prefs;

const themeListeners = new Set();
let drawnDark = null;
function themeChanged() {
  const dark = isDark();
  if (dark === drawnDark) return;
  drawnDark = dark;
  for (const f of [...themeListeners]) { try { f(dark); } catch (e) { console.error(e); } }
}
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', themeChanged);

/** Light, dark or the device's setting: data-theme on <html>, and a copy for boot.js. */
export function applyTheme(theme) {
  if (theme === 'light' || theme === 'dark') root.dataset.theme = theme; else delete root.dataset.theme;
  try { localStorage.setItem(THEME_KEY, JSON.stringify(theme || 'device')); } catch { /* blocked: fine */ }
  themeChanged();
}
/** fn(dark) when the page starts to be drawn dark or light (a new theme, or the device switched). Returns a remover. */
export function onTheme(fn) { themeListeners.add(fn); return () => themeListeners.delete(fn); }
/** True when the page is drawn dark now (maps and charts read it when they draw). */
export function isDark() {
  const t = root.dataset.theme;
  if (t === 'dark') return true;
  if (t === 'light') return false;
  return matchMedia('(prefers-color-scheme: dark)').matches;
}

const KEYS = ['units', 'range', 'theme', 'language', 'interface_size', 'follow_screen', 'ui_pack', 'car_order', 'pinned_cars'];
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/** Use these preferences now. Returns the keys that changed. */
export function applyPrefs(p) {
  if (!p || typeof p !== 'object') return new Set();
  const before = prefs;
  prefs = { ...p };
  const changed = new Set(KEYS.filter((k) => !before || !same(before[k], p[k])));
  setUnits(p);
  // The language: 'auto' (or none) negotiates from the browser's languages; <html lang> follows.
  setLocale(p.language && p.language !== 'auto' ? p.language : 'auto');
  cover(root, 'web.shell.document-language');
  applyTheme(p.theme);
  setSize({ size: p.interface_size ?? 100, follow: p.follow_screen !== false });
  if (changed.size) for (const f of [...listeners]) { try { f(prefs, changed); } catch (e) { console.error(e); } }
  return changed;
}

/** Load the person's preferences from the server and apply them. */
export async function loadPrefs() {
  const p = await api.get('me/preferences');
  applyPrefs(p);
  return p;
}

/** Save a change at once (PATCH) and apply what the server answers. Throws ApiError. */
export async function savePrefs(patch) {
  const p = await api.patch('me/preferences', patch);
  applyPrefs(p);
  return p;
}

/** fn(prefs, changedKeys) after the preferences changed. Returns a remover. */
export function onPrefs(fn) { listeners.add(fn); return () => listeners.delete(fn); }
