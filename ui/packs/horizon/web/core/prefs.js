// The signed-in person and their preferences (/api/v1/me, /api/v1/me/preferences).
//
// loadSession() reads both once at start; applyPrefs() puts units, range, theme, language and the
// Interface size into effect (shared/units.js, shared/i18n.js, core/display.js). savePrefs(patch) applies at
// once, then saves; a `prefs` event from another device or a server default changing applies too.

import * as api from '../shared/api.js';
import { setUnits } from '../shared/units.js';
import { setLocale } from '../shared/i18n.js';
import { setCarPrefs } from '../shared/state.js';
import { applyInterface, applyTheme } from './display.js';

let me = null;
let prefs = null;
const subs = new Set();

/** The signed-in account ({ id, name, role: 'admin' | 'watch', cars }) or null. */
export const getMe = () => me;
/** True when the account is an admin (may change settings and car data). */
export const isAdmin = () => me?.role === 'admin';
/** The preferences now (the /me/preferences object) or null before they loaded. */
export const getPrefs = () => prefs;

/** f(prefs) after the preferences changed (here, on another device, or a server default). Returns off(). */
export function onPrefs(f) { subs.add(f); return () => subs.delete(f); }

/** Put preferences into effect: units, range, theme, language, Interface size and Follow screen size. */
export function applyPrefs(p) {
  if (!p) return;
  prefs = p;
  setCarPrefs(p); // main_car / last_car: the car pages without a car in the address show
  setUnits(p);
  setLocale(p.language || 'auto'); // also sets <html lang>: en-GB, or the language the text is really in
  if (p.theme) applyTheme(p.theme);
  if (p.interface_size != null || p.follow_screen != null) {
    applyInterface({ size: p.interface_size ?? undefined, follow: p.follow_screen ?? undefined });
  }
  for (const f of [...subs]) { try { f(prefs); } catch (e) { console.error(e); } }
}

/**
 * Read the account and its preferences. Resolves { me, prefs }; rejects with the ApiError (401 when
 * nobody is signed in).
 */
export async function loadSession() {
  const [m, p] = await Promise.all([api.get('me'), api.get('me/preferences', null, { maxAgeMs: 0 })]);
  me = m;
  applyPrefs(p);
  return { me, prefs };
}

/** Forget the session (after signing out or a 401). */
export function clearSession() { me = null; }

/**
 * Change preferences: applied at once, then saved with PATCH /me/preferences. On failure the saved
 * preferences come back and the error is thrown (the caller shows it).
 */
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
