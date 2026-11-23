// Sentry Mate shared module: offline.js (the service worker the server serves at /sw.js).
// Canonical source: ui/shared/offline.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// registerServiceWorker() registers /sw.js (scope /) once the page has loaded and the browser is idle, so a
// return visit draws from the kept page and files before the network answers (the worker's rules are in
// crates/web/src/sw.js). Only over HTTPS: the dev server is plain HTTP and has no /sw.js. It never throws and
// logs nothing. clearServiceWorker() drops everything the worker keeps (call it on sign-out).

export const SCRIPT = '/sw.js';
const PREFIX = 'sm-';
const REPLY_MS = 1000;

function usable(g) {
  const nav = g.navigator;
  return !!(nav && nav.serviceWorker && g.isSecureContext && g.location && g.location.protocol === 'https:');
}

function idle(g, fn) {
  if (typeof g.requestIdleCallback === 'function') g.requestIdleCallback(fn, { timeout: 5000 });
  else g.setTimeout(fn, 1000);
}

/** Registers the worker after the page's first draw; resolves to the registration, or null. */
export function registerServiceWorker(g = globalThis) {
  if (!usable(g)) return Promise.resolve(null);
  return new Promise((resolve) => {
    const go = () => idle(g, () => {
      g.navigator.serviceWorker.register(SCRIPT, { scope: '/' }).then((reg) => {
        // the page and the files it names, kept now (this load did not go through the worker yet)
        g.navigator.serviceWorker.ready.then((r) => {
          if (r.active) r.active.postMessage({ type: 'sentry-mate:warm' });
        }, () => {});
        resolve(reg);
      }, () => resolve(null));
    });
    if (g.document && g.document.readyState !== 'complete') g.addEventListener('load', go, { once: true });
    else go();
  });
}

function ask(worker, g, msg) {
  if (typeof g.MessageChannel !== 'function') {
    worker.postMessage(msg);
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    const ch = new g.MessageChannel();
    const done = () => { g.clearTimeout(t); ch.port1.close(); resolve(); };
    const t = g.setTimeout(done, REPLY_MS);
    ch.port1.onmessage = done;
    worker.postMessage(msg, [ch.port2]);
  });
}

/** Drops what the worker keeps (its memory and its caches); never throws. */
export async function clearServiceWorker(g = globalThis) {
  const tasks = [];
  const sw = g.navigator && g.navigator.serviceWorker;
  if (sw && sw.controller) {
    try { tasks.push(ask(sw.controller, g, { type: 'sentry-mate:clear' })); } catch { /* the caches below */ }
  }
  const store = g.caches;
  if (store && typeof store.keys === 'function') {
    tasks.push(store.keys().then((names) => Promise.all(names.filter((n) => n.startsWith(PREFIX)).map((n) => store.delete(n)))));
  }
  await Promise.all(tasks.map((p) => Promise.resolve(p).catch(() => {})));
}
