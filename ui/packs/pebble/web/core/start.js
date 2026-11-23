// Sentry Mate, "Pebble" UI pack: the entry point (index.html loads this module).
//
// Home routing, in this order, on every load:
//   1. no account exists yet (GET /api/v1/setup says needs_setup): the set-up screen;
//   2. not signed in (GET /api/v1/me answers 401): the sign-in screen, which comes back here afterwards;
//   3. otherwise the address asked for, and Fleet for "/".
// Then the person's preferences are applied (units, theme, language, interface size), the frame is built and
// the router takes over. A 401 later (the session ended) goes back to the sign-in screen.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { startDisplay } from './display.js';
import { loadPrefs } from './prefs.js';
import { mountFrame } from './frame.js';
import { startRouter, AUTH_SCREENS } from './router.js';
import { registerServiceWorker } from '../shared/offline.js';
import { toast } from './toast.js';

const here = () => location.pathname + location.search;
const pageNow = () => state.parseUrl(location.href).page;

async function start() {
  startDisplay();
  let setup = { needs_setup: false };
  try { setup = await api.get('setup'); } catch { /* an older server: carry on */ }
  if (setup.needs_setup) {
    if (pageNow() !== 'setup') history.replaceState(null, '', '/setup');
    return startRouter({});
  }
  let me = null;
  try { me = await api.get('me', null, { keep: true }); } catch (err) { // kept: the map's tile settings read it too
    if (!(err && err.status === 401)) throw err;
  }
  if (!me) {
    if (!AUTH_SCREENS.includes(pageNow())) history.replaceState(null, '', `/sign-in?next=${encodeURIComponent(here())}`);
    return startRouter({});
  }
  // the notices come with the rest, so they are in place before the page and do not push it down later
  const [about, cars, , notices] = await Promise.all([
    api.get('about', null, { maxAgeMs: 600000 }).catch(() => null),
    // the count of the car pill leaves out sold cars (fleet/summary counts the unsold ones)
    api.get('fleet/summary', null, { keep: true }).then((x) => (x && typeof x.cars === 'number' ? { total: x.cars } : api.page('cars', null, { limit: 1 })), () => api.page('cars', null, { limit: 1 })).catch(() => null),
    loadPrefs().catch(() => null),
    api.get('notices', null, { keep: true }).then((r) => r.items || [], () => null),
  ]);
  const carsCount = cars && typeof cars.total === 'number' ? cars.total : null;
  const frame = mountFrame({ me, about, carsCount, notices });
  const ended = () => {
    if (AUTH_SCREENS.includes(pageNow())) return;
    toast('Your session ended. Sign in again to go on.', { kind: 'info', next: true });
    location.assign(`/sign-in?next=${encodeURIComponent(here())}`);
  };
  api.on('unauthorized', ({ method } = {}) => {
    if (AUTH_SCREENS.includes(pageNow())) return;
    // A change that asks for the password and two-step code again can answer 401 for a missing or wrong code
    // while the session is still there: ask the server before going to the sign-in screen.
    if (method && method !== 'GET' && method !== 'HEAD') {
      // (a 401 on this GET comes back here and goes to the sign-in screen)
      api.get('me', null, { maxAgeMs: 0 }).catch(() => {});
      return;
    }
    ended();
  });
  startRouter({ frame, me, carsCount });
}

start().catch((err) => {
  console.error(err);
  const m = document.getElementById('main');
  if (m) m.textContent = `Sentry Mate could not start: ${err && err.message ? err.message : String(err)}. Reload the page to try again.`;
  document.documentElement.dataset.ready = 'error:start';
});
// after the first draw: the next load draws from what the service worker keeps
registerServiceWorker();
