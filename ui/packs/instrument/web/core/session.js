// Who is signed in, and where the home address leads (the frame's home routing):
//   1. the server has no account yet          -> /setup (create the first admin with the set-up code)
//   2. nobody is signed in in this browser     -> /sign-in?next=<the page asked for>
//   3. signed in                               -> the page asked for; / is Fleet
// A signed-in account that the rule for every account has not met yet (GET /me: two_step_setup_only) gets the
// two-step set-up page at every address.
// gate(page) answers this for every navigation. A 401 from any call later (the session ended on the
// server, another tab signed out) sends the person to sign-in, keeping the page they were on.

import * as api from '../shared/api.js';
import { clearServiceWorker } from '../shared/offline.js';
import { loadSession, clearSession, getMe } from './prefs.js';

let known = null;        // null until checked: { needsSetup, signedIn }
let checking = null;
const subs = new Set();

/** f({ needsSetup, signedIn, me }) after the session changed (signed in, signed out). Returns off(). */
export function onSession(f) { subs.add(f); return () => subs.delete(f); }
function tell() { for (const f of [...subs]) { try { f({ ...known, me: getMe() }); } catch (e) { console.error(e); } } }

/** Ask the server: is set-up needed, and is this browser signed in? Throws an ApiError when it cannot say. */
export async function check() {
  checking ??= (async () => {
    // the account and its preferences are asked for with set-up, not after it: a round trip less before every first
    // page (a server that still needs set-up has no account, so they only answer 401, which is no lost session here)
    const session = loadSession().then(() => null, (e) => e);
    const s = await api.get('setup');
    if (s.needs_setup) { known = { needsSetup: true, signedIn: false }; return known; }
    const e = await session;
    if (e && e.status !== 401) throw e;
    known = { needsSetup: false, signedIn: !e };
    return known;
  })().finally(() => { checking = null; });
  const k = await checking;
  tell();
  return k;
}

export const status = () => known;

/**
 * A safe place to go after signing in: a path inside this app, never another site. The address is read the
 * way the browser reads it ('/\\host' and '/<tab>/host' are other sites to a browser), and only its path,
 * query and hash on this origin are kept.
 */
export function safeNext(next) {
  if (typeof next !== 'string' || !next.startsWith('/')) return '/';
  let u;
  try { u = new URL(next, location.origin); } catch { return '/'; }
  if (u.origin !== location.origin) return '/';
  const path = u.pathname;
  if (path.startsWith('//') || path === '/sign-in' || path === '/setup' || path.startsWith('/api/')) return '/';
  return path + u.search + u.hash;
}

/**
 * Where a page may be shown: { ok: true } or { redirect: url }. Sign-in and set-up are only for people
 * who need them; every other page needs a session.
 */
export async function gate(page, here) {
  const k = known ?? (await check());
  if (k.needsSetup) return page === 'setup' ? { ok: true } : { redirect: '/setup' };
  if (!k.signedIn) {
    if (page === 'sign-in') return { ok: true };
    const next = here && here !== '/' ? `?next=${encodeURIComponent(here)}` : '';
    return { redirect: `/sign-in${next}` };
  }
  if (page === 'sign-in' || page === 'setup') return { redirect: '/' };
  // the rule asks every account for two-step sign-in and this one has none: the server answers every call but its own
  // account's with 403 two_step_setup, so all there is to show is the page that sets it up (pages/two-step-setup.js)
  if (getMe()?.two_step_setup_only) return page === 'two-step-setup' ? { ok: true } : { wall: true };
  if (page === 'two-step-setup') return { redirect: '/' };
  return { ok: true };
}

/**
 * The server answered 403 two_step_setup: ask again who this is, and say whether the account may now only set
 * two-step sign-in up (then the router shows that page, whatever address it was on).
 */
export async function refreshMe() {
  api.invalidate();
  try { await loadSession(); } catch (e) { if (e.status !== 401 && e.status !== 403) throw e; }
  return !!getMe()?.two_step_setup_only;
}

/** After POST /session or POST /setup answered "done": load the account and its preferences. */
export async function signedIn() {
  await loadSession();
  known = { needsSetup: false, signedIn: true };
  api.invalidate();
  api.reconnectEvents();
  tell();
}

/** The session is gone (signed out here, or a 401 from the server). */
export function signedOut() {
  clearSession();
  api.invalidate();
  known = { needsSetup: false, signedIn: false };
  tell();
}

/** Sign out on the server, then here. */
export async function signOut() {
  try { await api.del('session'); } catch (e) { if (e.status !== 401) throw e; }
  await clearServiceWorker();
  signedOut();
}
