// Sentry Mate · "Console" UI pack · the entry module (index.html loads it after boot.js).
// Builds the frame once, starts the keys, then lets the router mount pages.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { initFrame } from './frame.js';
import { startRouter } from './router.js';
import { initKeys } from './keys.js';
import * as session from './session.js';
import { toast } from './toast.js';

// a session that ends while a page is open: sign in again, then come back to this page
function sessionEnded() {
  if (!session.status()?.signedIn) return;
  session.signedOut();
  toast('Your session ended. Sign in again to go on.', { kind: 'info', next: true });
  state.navigate(`/sign-in?next=${encodeURIComponent(location.pathname + location.search)}`, { replace: true });
}
api.on('unauthorized', ({ path } = {}) => {
  const p = String(path || '').replace(/^\/?(api\/v1\/)?/, '');
  if (p.startsWith('session') || p.startsWith('setup')) return;   // a wrong password is not a lost session
  sessionEnded();
});
api.on('events', (info) => { if (info?.state === 'signed_out') sessionEnded(); });

initFrame();
initKeys();
startRouter();
