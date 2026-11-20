// The signed-in account and its preferences (/api/v1/me, /api/v1/me/preferences).
//
// loadAccount() reads both; usePrefs() puts the units, range, theme, language and Interface size into effect
// (shared/units.js, shared/i18n.js, core/display.js). savePrefs(patch) uses the change at once, then saves it;
// a `prefs` event (another device, a new server default) is used the same way through usePrefs().

import * as api from '../shared/api.js';
import { setUnits } from '../shared/units.js';
import { setLocale } from '../shared/i18n.js';
import { applyInterface, applyTheme } from './display.js';

let me = null;
let prefs = null;
const subs = new Set();

/** The account ({ id, name, role: 'admin' | 'watch', cars, two_step_required? }) or null. */
export const getMe = () => me;
/** True for an admin (may change settings and car data). */
export const isAdmin = () => me?.role === 'admin';
/** The preferences now, or null before they loaded. */
export const getPrefs = () => prefs;
/** f(prefs) after the preferences changed. Returns off(). */
export function onPrefs(f) { subs.add(f); return () => subs.delete(f); }

/** Put preferences into effect. */
export function usePrefs(p) {
  if (!p) return;
  prefs = p;
  setUnits(p);
  setLocale(p.language || 'auto');
  if (p.theme) applyTheme(p.theme);
  if (p.interface_size != null || p.follow_screen != null) applyInterface({ size: p.interface_size ?? undefined, follow: p.follow_screen ?? undefined });
  for (const f of [...subs]) { try { f(prefs); } catch (e) { console.error(e); } }
}

/** Read the account and its preferences: { me, prefs }, or the ApiError (401 when nobody is signed in). */
export async function loadAccount() {
  const [m, p] = await Promise.all([api.get('me'), api.get('me/preferences', null, { maxAgeMs: 0 })]);
  me = m;
  usePrefs(p);
  return { me, prefs };
}

/** Forget the account (signed out, or a 401). */
export function forgetAccount() { me = null; }

/** Change preferences: used at once, then saved. On failure the saved ones come back and the error is thrown. */
export async function savePrefs(patch) {
  const before = prefs;
  usePrefs({ ...(prefs || {}), ...patch, units: { ...(prefs?.units || {}), ...(patch.units || {}) } });
  try {
    const saved = await api.patch('me/preferences', patch);
    usePrefs(saved);
    return saved;
  } catch (e) {
    if (before) usePrefs(before);
    throw e;
  }
}
