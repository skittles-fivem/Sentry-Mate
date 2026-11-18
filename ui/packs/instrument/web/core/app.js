// Sentry Mate · "Instrument" UI pack · the entry module (index.html loads it after boot.js).
// Builds the frame once, then lets the router mount pages. Everything else is in core/ and pages/.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { initFrame } from './frame.js';
import { startRouter } from './router.js';
import * as session from './session.js';
import { flash } from './flash.js';

// a session that ends while a page is open (signed out in another tab, removed by an admin, expired):
// go to sign-in and come back to this page afterwards
function sessionEnded() {
  const s = session.status();
  if (!s?.signedIn) return;
  session.signedOut();
  flash('Your session ended. Sign in again to go on.', { kind: 'info', next: true });
  const here = location.pathname + location.search;
  state.navigate(`/sign-in?next=${encodeURIComponent(here)}`, { replace: true });
}
api.on('unauthorized', ({ path } = {}) => {
  const p = String(path || '').replace(/^\/?(api\/v1\/)?/, '');
  if (p.startsWith('session') || p.startsWith('setup')) return; // a wrong password is not a lost session
  sessionEnded();
});
api.on('events', (info) => { if (info?.state === 'signed_out') sessionEnded(); });

initFrame();
startRouter();
