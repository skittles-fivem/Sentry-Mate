// Sentry Mate, "Logbook" UI pack: the entry point (index.html loads this module).
//
// Home routing, in this order, on every load:
//   1. no account exists yet (GET /api/v1/setup says needs_setup): the set-up screen;
//   2. not signed in (GET /api/v1/me answers 401): the sign-in screen, which comes back here afterwards;
//   3. otherwise the address asked for, and Fleet for "/".
// Then the person's preferences are applied (units, theme, language, interface size), the frame is
// built, and the router takes over. A 401 later (the session ended) goes back to the sign-in screen.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { startDisplay, keepReadingPlace } from './display.js';
import { loadPrefs } from './prefs.js';
import { mountFrame } from './frame.js';
import { watchTables } from './snap.js';
import { startRouter, AUTH_SCREENS } from './router.js';

const here = () => location.pathname + location.search;
const pageNow = () => state.parseUrl(location.href).page;

async function start() {
  startDisplay();
  watchTables();
  keepReadingPlace(document.getElementById('main'));
  let setup = { needs_setup: false };
  try { setup = await api.get('setup'); } catch { /* an older server: carry on */ }
  if (setup.needs_setup) {
    if (pageNow() !== 'setup') history.replaceState(null, '', '/setup');
    return startRouter({});
  }
  let me = null;
  try { me = await api.get('me'); } catch (err) {
    if (!(err && err.status === 401)) throw err;
  }
  if (!me) {
    if (!AUTH_SCREENS.includes(pageNow())) history.replaceState(null, '', `/sign-in?next=${encodeURIComponent(here())}`);
    return startRouter({});
  }
  const [about, cars] = await Promise.all([
    api.get('about', null, { maxAgeMs: 600000 }).catch(() => null),
    api.page('cars', null, { limit: 1 }).catch(() => null),
    loadPrefs().catch(() => null),
  ]);
  const frame = mountFrame({ me, about });
  api.on('unauthorized', () => {
    if (AUTH_SCREENS.includes(pageNow())) return;
    location.assign(`/sign-in?next=${encodeURIComponent(here())}`);
  });
  startRouter({ frame, me, carsCount: cars && typeof cars.total === 'number' ? cars.total : null });
}

start().catch((err) => {
  console.error(err);
  const m = document.getElementById('main');
  if (m) {
    m.textContent = 'Sentry Mate could not start: ' + (err && err.message ? err.message : String(err)) + '. Reload the page to try again.';
  }
  document.documentElement.dataset.ready = 'error:start';
});
