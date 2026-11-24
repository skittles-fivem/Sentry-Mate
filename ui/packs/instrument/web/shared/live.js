// Sentry Mate shared module: live.js (figures that change while a page is open).
// Canonical source: ui/shared/live.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// The server sends what changes on the live stream (docs/ui-api.md, "Events"):
//   job          { kind, id, status }   a job's progress, `status` exactly as its status endpoint gives it:
//                                       full_rebuild, compaction, export, missing_scan, import, invoices (admins)
//   health       { car, health }        a car's /cars/{car}/health
//   connections  the /integrations/overview body (admins)
//   storage      the /system/storage body plus `compaction` (admins)
//   rows         { car, kinds, from_ms, to_ms }: lists, totals and statistics of the car changed
//
// follow() keeps one figure up to date: it reads the status endpoint once at the start and once after every
// reconnect of the stream (what changed while it was down), then takes each matching event as it comes. Only while
// the stream is down (connecting, reconnecting, signed out, no EventSource) does it read the endpoint every
// pollMs, the fallback; never while the stream is open. A page paints what it is given into the elements it built
// once, changing only the text that changed (setText, setNumber), so nothing on the page moves.
//
//   const f = follow(ctx.api, { type: 'job', match: jobOf('full_rebuild'), pick: (d) => d.status,
//     load: () => ctx.api.get('system/full-rebuild'), onValue: paint,
//     pollMs: (s) => (s?.state === 'running' ? 2000 : 30000) });
//   ...  f.refresh() after a change of the page's own (a button), f.stop() is done by the page's scope.

/** The live figures' event names (besides the older now, fleet, rows, prefs, notice). */
export const LIVE_TYPES = Object.freeze(['job', 'health', 'connections', 'storage']);

/** How often a figure is read while the stream is down, by default. */
export const FALLBACK_POLL_MS = 5000;

/** A matcher of `job` events of a kind (and of one id of it when given). */
export const jobOf = (kind, id = null) => (d) => !!d && d.kind === kind && (id == null || d.id === id);

/** A matcher of `health` and `rows` events of a car. */
export const carOf = (car) => (d) => !!d && d.car === car;

/**
 * Follows one live figure. api: a page scope or client with get-like `load` and `events(onEvent, { types, signal,
 * onStatus })`. Options:
 *   type     the event's name ('job', 'health', 'connections', 'storage', 'rows', ...)
 *   match    (data) -> whether this event is the figure's (default: every one)
 *   pick     (data) -> the value given to onValue (default: the data)
 *   load     () -> Promise of the value, read from its endpoint (optional: an event-only figure)
 *   onValue  (value, how) -> paints it; how is 'load' or 'event'
 *   onError  (err) -> a read failed (the last value stays)
 *   overtaken (read, eventValue) -> the value to show when an event came in while a read was under way (default: the
 *            read is dropped, the event is the newer word); for a figure whose events carry less than its endpoint
 *   pollMs   ms, or (lastValue) -> ms | null (null: no fallback read for now), while the stream is down
 *   onLive   (live: boolean) -> the stream went up or down (a page can show it)
 *   initial  the value as the page has just read it (with load): no read at the start, only the later ones
 *   signal   stops following when aborted (a page scope's own is used by default)
 *   timers   { setTimeout, clearTimeout } (tests)
 * Returns { refresh(), stop(), live, value }.
 */
export function follow(api, opts = {}) {
  const { type, match = () => true, pick = (d) => d, load = null, onValue = () => {}, onError = null, onLive = null } = opts;
  const setT = opts.timers?.setTimeout || globalThis.setTimeout.bind(globalThis);
  const clearT = opts.timers?.clearTimeout || globalThis.clearTimeout.bind(globalThis);
  const signal = opts.signal || api.signal || null;
  const pollOf = typeof opts.pollMs === 'function' ? opts.pollMs : () => (opts.pollMs ?? FALLBACK_POLL_MS);
  // a value the page has just read stands for the first read (the stream already open at the start reads nothing)
  let given = load != null && 'initial' in opts;
  let value = given ? opts.initial : undefined;
  let live = false;
  let stopped = false;
  let timer = null;
  let reading = null;
  let again = false;
  let applied = 0; // events applied so far: a read that began before one is older than it

  const give = (v, how) => {
    value = v;
    try { onValue(v, how); } catch (e) { console.error(e); }
  };
  function schedule() {
    if (timer) { clearT(timer); timer = null; }
    if (stopped || live || !load) return;
    const ms = pollOf(value);
    if (ms == null || !Number.isFinite(ms)) return;
    timer = setT(() => { timer = null; read(); }, Math.max(250, ms));
  }
  async function read() {
    if (stopped || !load) return;
    if (reading) { again = true; return reading; }
    reading = (async () => {
      try {
        const began = applied;
        const v = await load();
        if (!stopped) {
          if (began === applied) give(v, 'load');
          // an event came in while the read was under way: it is the newer word on what it carries, but it may carry
          // less than the endpoint (a storage detail has its categories only there): with `overtaken` the read is
          // kept, with the event's value put over it
          else if (opts.overtaken) give(opts.overtaken(v, value), 'load');
        }
      } catch (e) {
        if (!stopped && e?.name !== 'AbortError') onError?.(e);
      }
    })();
    try { await reading; } finally { reading = null; }
    if (again && !stopped) { again = false; return read(); }
    schedule();
  }
  function status(info) {
    const up = info?.state === 'open';
    // still down (connecting, then reconnecting, again and again): a fallback read already planned keeps its time
    if (up === live) { if (!up && !timer) schedule(); return; }
    live = up;
    onLive?.(up);
    // back up: what changed while it was down, read once; down: the fallback reads start
    if (up) { if (timer) { clearT(timer); timer = null; } if (!given) read(); } else schedule();
  }
  const unsubscribe = api.events((ev) => {
    if (stopped || ev?.type !== type || !match(ev.data)) return;
    applied++;
    give(pick(ev.data), 'event');
    schedule();
  }, { types: [type], signal, onStatus: status });
  function stop() {
    if (stopped) return;
    stopped = true;
    if (timer) { clearT(timer); timer = null; }
    try { unsubscribe?.(); } catch { /* gone */ }
  }
  signal?.addEventListener?.('abort', stop, { once: true });
  // (the stream already open answered at once above, and that started the first read)
  if (given) { given = false; schedule(); } else if (!reading) read();
  return {
    refresh: read,
    stop,
    get live() { return live; },
    get value() { return value; },
  };
}

/**
 * Calls run() once a burst of `rows` events of a car (and of one of `kinds`, when given) has passed: `waitMs`
 * after the last one. For lists, totals and statistics, which read their rows again (in place) when they change.
 * Returns stop().
 */
export function onRows(api, { car = null, kinds = null, waitMs = 1500, run, signal = null, timers = null }) {
  const setT = timers?.setTimeout || globalThis.setTimeout.bind(globalThis);
  const clearT = timers?.clearTimeout || globalThis.clearTimeout.bind(globalThis);
  let t = null;
  const stopEvents = api.events((ev) => {
    const d = ev?.data;
    if (ev?.type !== 'rows' || !d) return;
    if (car && d.car !== car) return;
    if (kinds && !(d.kinds || []).some((k) => kinds.includes(k))) return;
    if (t) clearT(t);
    t = setT(() => { t = null; run(d); }, waitMs);
  }, { types: ['rows'], signal: signal || api.signal || null });
  return () => { if (t) clearT(t); t = null; stopEvents?.(); };
}

/** Sets an element's text only when it changed (no style or layout work for a figure that stayed). */
export function setText(el, text) {
  if (!el) return false;
  const t = text == null ? '' : String(text);
  if (el.textContent === t) return false;
  el.textContent = t;
  return true;
}

/** Sets an attribute only when it changed (null removes it). */
export function setAttr(el, name, value) {
  if (!el) return false;
  const v = value == null ? null : String(value);
  if (el.getAttribute(name) === v) return false;
  if (v == null) el.removeAttribute(name);
  else el.setAttribute(name, v);
  return true;
}

/**
 * Paints `values` ({ key: text }) into the elements under `root` marked `data-live="key"`, changing only the ones
 * whose text changed. Returns how many changed.
 */
export function patchFigures(root, values) {
  if (!root) return 0;
  let n = 0;
  for (const el of root.querySelectorAll('[data-live]')) {
    const k = el.getAttribute('data-live');
    if (Object.prototype.hasOwnProperty.call(values, k) && setText(el, values[k])) n++;
  }
  return n;
}

/** The words of the live stream's state for a "Live" dot: { tone: 'live' | 'wait' | 'off', text }. */
export function liveWords(info) {
  switch (info?.state) {
    case 'open': return { tone: 'live', text: 'Live' };
    case 'connecting': return { tone: 'wait', text: 'Connecting' };
    case 'reconnecting': return { tone: 'wait', text: 'Reconnecting: figures are read every few seconds' };
    case 'signed_out': return { tone: 'off', text: 'Signed out' };
    case 'unsupported': return { tone: 'off', text: 'Not live: figures are read every few seconds' };
    default: return { tone: 'off', text: 'Not live' };
  }
}
