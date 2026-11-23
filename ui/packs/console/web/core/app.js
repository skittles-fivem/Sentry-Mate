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
api.on('unauthorized', ({ method, path } = {}) => {
  const p = String(path || '').replace(/^\/?(api\/v1\/)?/, '');
  if (p.startsWith('session') || p.startsWith('setup')) return;   // a wrong password is not a lost session
  // a change that asks for the password and two-step code again answers 401 wrong_code while the session is
  // still there: ask the server first (a 401 on that read comes back here as a GET and ends the session)
  if (method && method !== 'GET' && method !== 'HEAD') { api.get('me', null, { maxAgeMs: 0 }).catch(() => {}); return; }
  sessionEnded();
});
api.on('events', (info) => { if (info?.state === 'signed_out') sessionEnded(); });

// every face of the pack's fonts, loading from the start: a weight first used deep in a page (a bold figure, a
// sans heading) is in before that page says it is ready, so text never re-flows under the reader when it swaps in
if (document.fonts?.load) {
  for (const f of ['400 1em "JetBrains Mono"', '600 1em "JetBrains Mono"', '400 1em "IBM Plex Sans"', '600 1em "IBM Plex Sans"']) document.fonts.load(f).catch(() => {});
}

initFrame();
initKeys();
startRouter();
