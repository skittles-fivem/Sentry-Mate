// Sentry Mate · "Instrument" UI pack · the entry module (index.html loads it after boot.js).
// Builds the frame once, then lets the router mount pages. Everything else is in core/ and pages/.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { initFrame } from './frame.js';
import { watchTables } from './snap.js';
import { markScrolling } from '../shared/dom.js';
import { setLazy } from '../shared/lazy.js';
import { startRouter, reload, firstReady } from './router.js';
import { getMe } from './prefs.js';
import * as session from './session.js';
import { registerServiceWorker } from '../shared/offline.js';
import { flash } from './flash.js';

// a session that ends while a page is open (signed out in another tab, removed by an admin, expired):
// go to sign-in and come back to this page afterwards
function sessionEnded() {
  const s = session.status();
  if (!s?.signedIn) return;
  session.signedOut();
  flash('Your session ended. Sign in again to go on.', { kind: 'info', next: true });
  const here = location.pathname + location.search + location.hash;
  state.navigate(`/sign-in?next=${encodeURIComponent(here)}`, { replace: true });
}
api.on('unauthorized', ({ method, path } = {}) => {
  const p = String(path || '').replace(/^\/?(api\/v1\/)?/, '');
  if (p.startsWith('session') || p.startsWith('setup')) return; // a wrong password is not a lost session
  // A change that asks for the password and two-step code again answers 401 wrong_code for a missing or
  // wrong code, with the session still there: ask the server before going to sign-in.
  if (method && method !== 'GET' && method !== 'HEAD') {
    api.get('me', null, { maxAgeMs: 0 }).then(() => {}, (e) => { if (e?.status === 401) sessionEnded(); });
    return;
  }
  sessionEnded();
});
// The rule asks every account for two-step sign-in and this one has none: the server answers every call but its own
// account's with 403 two_step_setup. Ask who this is again; if it may only set two-step sign-in up, build the page again
// (the router shows the set-up page, at any address).
let askingMe = false;
api.on('two_step_setup', async () => {
  if (askingMe || !session.status()?.signedIn || getMe()?.two_step_setup_only) return;
  askingMe = true;
  try { if (await session.refreshMe()) reload(); } catch { /* the next call asks again */ } finally { askingMe = false; }
});
api.on('events', (info) => { if (info?.state === 'signed_out') sessionEnded(); });

// charts and maps below the fold are built when they come near (shared/lazy.js, off by default for now): their boxes
// keep their height from the start, and the jump check (ui/tools/jumps.js) passes with it on
setLazy(true);
initFrame();
watchTables();
markScrolling(); // data-scrolling: app.css pauses the endless pulses while the page scrolls
// every face of the pack's fonts, loading from the start: a weight first used deep in a page (a bold figure, a
// mono value) is in before that page says it is ready, so text never re-flows under the reader when it swaps in
if (document.fonts?.load) {
  for (const f of ['400 1em "SM Inter"', '500 1em "SM Inter"', '600 1em "SM Inter"', '300 1em "SM Inter Display"',
    '500 1em "SM Inter Display"', '600 1em "SM Inter Display"', '400 1em "SM JetBrains Mono"']) document.fonts.load(f).catch(() => {});
}

startRouter();
// once the first page is ready (at most FIRST_PAGE_MS): the next load draws from what the service worker keeps.
// Not before: on a first visit the worker fetches the files it keeps, and on a slow line that took the first page's
// bandwidth
const FIRST_PAGE_MS = 10000;
Promise.race([firstReady, new Promise((r) => { setTimeout(r, FIRST_PAGE_MS); })]).then(() => registerServiceWorker());
