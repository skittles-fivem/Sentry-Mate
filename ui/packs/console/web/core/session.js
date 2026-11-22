// Who is signed in, and where an address may be shown:
//   1. the server has no account yet   -> /setup
//   2. nobody is signed in here         -> /sign-in?next=<the address asked for>
//   3. signed in                        -> the address; sign-in and setup send to /
// A 401 later (the session ended on the server, another tab signed out) goes to sign-in (core/app.js).

import * as api from '../shared/api.js';
import { loadSession, clearSession, getMe } from './prefs.js';

let known = null;      // { needsSetup, signedIn } once checked
let checking = null;
const subs = new Set();

/** f({ needsSetup, signedIn, me }) after the session changed. Returns off(). */
export function onSession(f) { subs.add(f); return () => subs.delete(f); }
function tell() { for (const f of [...subs]) { try { f({ ...known, me: getMe() }); } catch (e) { console.error(e); } } }

/** Ask the server: set-up needed? signed in? Throws an ApiError when it cannot say. */
export async function check() {
  checking ??= (async () => {
    const s = await api.get('setup');
    if (s.needs_setup) return (known = { needsSetup: true, signedIn: false });
    try {
      await loadSession();
      known = { needsSetup: false, signedIn: true };
    } catch (e) {
      if (e.status !== 401) throw e;
      known = { needsSetup: false, signedIn: false };
    }
    return known;
  })().finally(() => { checking = null; });
  const k = await checking;
  tell();
  return k;
}

export const status = () => known;

/** A safe place to go after signing in: a path of this app on this origin, never another site. */
export function safeNext(next) {
  if (typeof next !== 'string' || !next.startsWith('/')) return '/';
  let u;
  try { u = new URL(next, location.origin); } catch { return '/'; }
  if (u.origin !== location.origin) return '/';
  const p = u.pathname;
  if (p.startsWith('//') || p === '/sign-in' || p === '/setup' || p.startsWith('/api/')) return '/';
  return p + u.search + u.hash;
}

/** { ok: true } or { redirect: url } for a page id. */
export async function gate(page, here) {
  const k = known ?? (await check());
  if (k.needsSetup) return page === 'setup' ? { ok: true } : { redirect: '/setup' };
  if (!k.signedIn) {
    if (page === 'sign-in') return { ok: true };
    return { redirect: `/sign-in${here && here !== '/' ? `?next=${encodeURIComponent(here)}` : ''}` };
  }
  if (page === 'sign-in' || page === 'setup') return { redirect: '/' };
  return { ok: true };
}

/** After POST /session or /setup/fresh said "done". */
export async function signedIn() {
  await loadSession();
  known = { needsSetup: false, signedIn: true };
  api.invalidate();
  api.reconnectEvents();
  tell();
}

/** The session is gone (signed out here, or a 401). */
export function signedOut() {
  clearSession();
  api.invalidate();
  known = { needsSetup: false, signedIn: false };
  tell();
}

/** Sign out on the server, then here. */
export async function signOut() {
  try { await api.del('session'); } catch (e) { if (e.status !== 401) throw e; }
  signedOut();
}
