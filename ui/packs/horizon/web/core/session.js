// Who is here, and where the home address leads (the frame's home routing):
//   the server has no account yet  -> /setup (start fresh, move a server here, or restore from a file)
//   nobody is signed in here        -> /sign-in?next=<the page asked for>
//   signed in                       -> the page asked for; / is All cars
// admit(page) answers that for every page the router opens. A 401 later (the session ended on the
// server, another tab signed out) sends the person to sign-in and back to the page afterwards (core/main.js).

import * as api from '../shared/api.js';
import { loadAccount, forgetAccount, getMe } from './prefs.js';

let known = null;   // { needsSetup, signedIn } once asked
let asking = null;
const subs = new Set();

/** f({ needsSetup, signedIn, me }) after signing in or out. Returns off(). */
export function onSession(f) { subs.add(f); return () => subs.delete(f); }
const tell = () => { for (const f of [...subs]) { try { f({ ...known, me: getMe() }); } catch (e) { console.error(e); } } };

/** Ask the server whether set-up is needed and whether this browser is signed in. Throws an ApiError when it cannot say. */
export async function ask() {
  asking ??= (async () => {
    const s = await api.get('setup');
    if (s.needs_setup) return { needsSetup: true, signedIn: false };
    try { await loadAccount(); return { needsSetup: false, signedIn: true }; } catch (e) {
      if (e.status !== 401) throw e;
      return { needsSetup: false, signedIn: false };
    }
  })().finally(() => { asking = null; });
  known = await asking;
  tell();
  return known;
}

export const status = () => known;

/**
 * A safe page to go to after signing in: a path of this app, never another site. The address is read as the
 * browser reads it, and only the path, query and hash on this origin are kept.
 */
export function safeNext(next) {
  if (typeof next !== 'string' || !next.startsWith('/')) return '/';
  let u;
  try { u = new URL(next, location.origin); } catch { return '/'; }
  if (u.origin !== location.origin) return '/';
  const p = u.pathname;
  if (p.startsWith('//') || p === '/sign-in' || p === '/setup' || p.startsWith('/api/')) return '/';
  return p + u.search + u.hash;
}

/** Where a page may open: { ok: true } or { go: url }. */
export async function admit(page, here) {
  const k = known ?? (await ask());
  if (k.needsSetup) return page === 'setup' ? { ok: true } : { go: '/setup' };
  if (!k.signedIn) {
    if (page === 'sign-in') return { ok: true };
    return { go: here && here !== '/' ? `/sign-in?next=${encodeURIComponent(here)}` : '/sign-in' };
  }
  if (page === 'sign-in' || page === 'setup') return { go: '/' };
  return { ok: true };
}

/** After POST /session or /setup/fresh said "done": load the account and its preferences. */
export async function signedIn() {
  await loadAccount();
  known = { needsSetup: false, signedIn: true };
  api.invalidate();
  api.reconnectEvents();
  tell();
}

/** The session is gone (signed out here, or a 401). */
export function signedOut() {
  forgetAccount();
  api.invalidate();
  known = { needsSetup: false, signedIn: false };
  tell();
}

/** Sign out on the server, then here. */
export async function signOut() {
  try { await api.del('session'); } catch (e) { if (e.status !== 401) throw e; }
  signedOut();
}

/** Forget what is known, so the next page asks the server again (after a move or restore finished). */
export function reset() { known = null; }
