// Sentry Mate · Horizon UI pack · the entry module (index.html loads it after boot.js).
// Starts the background, builds the frame once, then lets the router open pages.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { startSky } from './sky.js';
import { initFrame } from './frame.js';
import { startRouter } from './router.js';
import * as session from './session.js';
import { toast } from './layer.js';

// a session that ends while a page is open (signed out in another tab, removed by an admin, expired):
// sign in again and come back to this page
function sessionEnded() {
  if (!session.status()?.signedIn) return;
  session.signedOut();
  toast('Your session ended. Sign in again to go on.', { kind: 'info', next: true });
  state.navigate(`/sign-in?next=${encodeURIComponent(location.pathname + location.search)}`, { replace: true });
}
// A 401 on some other request is not always a lost session: a wrong password or two-step code typed into a
// dialog that asks for them again (Tesla account, move, recovery key) is a 401 too (wrong_code). So ask the
// server once whether this browser is still signed in; only a 401 on that question ends the session here.
let checking = null;
api.on('unauthorized', ({ path } = {}) => {
  const p = String(path || '').replace(/^\/?(api\/v1\/)?/, '').replace(/[?#].*$/, '');
  if (p.startsWith('session') || p.startsWith('setup')) return; // a wrong password is not a lost session
  if (p === 'me') { sessionEnded(); return; }
  if (!session.status()?.signedIn) return;
  checking ??= api.get('me', null, { maxAgeMs: 0 }).catch(() => null).finally(() => { checking = null; });
});
api.on('events', (info) => { if (info?.state === 'signed_out') sessionEnded(); });

startSky();
initFrame();
startRouter();
