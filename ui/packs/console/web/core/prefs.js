// The signed-in account and its preferences (/api/v1/me, /api/v1/me/preferences).
//
// loadSession() reads both; applyPrefs() puts units, language, theme and Interface size into effect.
// savePrefs(patch) applies at once, then saves; a `prefs` event from another device applies too.

import * as api from '../shared/api.js';
import { setUnits } from '../shared/units.js';
import { setLocale } from '../shared/i18n.js';
import { setCarPrefs } from '../shared/state.js';
import { applyInterface, applyTheme } from './display.js';

let me = null;
let prefs = null;
const subs = new Set();

/** { id, name, role: 'admin' | 'watch', cars } or null. */
export const getMe = () => me;
export const isAdmin = () => me?.role === 'admin';
export const getPrefs = () => prefs;
/** f(prefs) after the preferences changed. Returns off(). */
export function onPrefs(f) { subs.add(f); return () => subs.delete(f); }

export function applyPrefs(p) {
  if (!p) return;
  prefs = p;
  setCarPrefs(p);
  setUnits(p);
  setLocale(p.language || 'auto');
  if (p.theme) applyTheme(p.theme);
  if (p.interface_size != null || p.follow_screen != null) {
    applyInterface({ size: p.interface_size ?? undefined, follow: p.follow_screen ?? undefined });
  }
  for (const f of [...subs]) { try { f(prefs); } catch (e) { console.error(e); } }
}

/** Read the account and its preferences; rejects with the ApiError (401 when nobody is signed in). */
export async function loadSession() {
  const [m, p] = await Promise.all([api.get('me'), api.get('me/preferences', null, { maxAgeMs: 0 })]);
  me = m;
  applyPrefs(p);
  return { me, prefs };
}

export function clearSession() { me = null; }

/** Change preferences: applied at once, then saved. On failure the saved ones come back and the error is thrown. */
export async function savePrefs(patch) {
  const before = prefs;
  applyPrefs({ ...(prefs || {}), ...patch, units: { ...(prefs?.units || {}), ...(patch.units || {}) } });
  try {
    const saved = await api.patch('me/preferences', patch);
    applyPrefs(saved);
    return saved;
  } catch (e) {
    if (before) applyPrefs(before);
    throw e;
  }
}
