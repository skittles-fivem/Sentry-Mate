// The full rebuild (docs/ui-api.md: /cars/{car}/full-rebuild, /system/full-rebuild), in the same words on every
// pack: what it does and does not touch, the button's state, the progress panel's fixed lines (phase, logs, days,
// bytes, rate, time left, last progress with the stall warning, errors) and the POST body with the password asked
// again. Pure functions: a page renders them in its own look, into a panel of fixed size (every line is always
// there, an empty one keeps its room), and reads the status again every POLL_MS while one runs.

/** How often a page reads the status again while a full rebuild runs (and once more after it ends). */
export const POLL_MS = 2000;
/** How often while none runs (a page left open sees one an admin started elsewhere). */
export const IDLE_POLL_MS = 30000;

/** The button's words. */
export const BUTTON = 'Full rebuild';
export const BUTTON_ALL = 'Full rebuild of every car';
export const CANCEL = 'Cancel full rebuild';

/** The short explanation shown beside the button (two short paragraphs). */
export const EXPLAIN = [
  'Throws away everything the server worked out (drives, charges, states, visits, positions, battery and efficiency figures, statistics, kept page data) and makes it again from the stored CAN logs and the Tesla API, TeslaFi and TeslaMate data the server keeps. For when the history is wrong and a newer server decodes it better.',
  'It never changes the stored logs, their signatures, the API data or imports, nor what you entered: charge costs, places and prices, typed versions, car settings, overlap choices, sold status and source order stay. Pages keep showing the old history until the new one is ready; if it fails, the old one stays.',
];

/** The password prompt's words. */
export const CONFIRM_TITLE = 'Full rebuild';
export const confirmText = (carName) => (carName
  ? `Rebuild all of ${carName}'s history from the stored data? This can take a long time. Type your password to go on.`
  : 'Rebuild the history of every car from the stored data? This can take a long time. Type your password to go on.');

const PHASES = {
  queued: 'Waiting to start',
  copying: 'Copying the raw data beside the history',
  decoding: 'Decoding the stored logs',
  days: 'Working out the battery figures per day',
  switching: 'Switching to the new history',
};

const STATES = {
  idle: 'Never run',
  running: 'Running',
  done: 'Done',
  failed: 'Failed: the old history was kept',
  cancelled: 'Cancelled: the old history was kept',
};

/** The tone of a state: 'off' (none ran), 'wait' (running), 'ok', 'warn' (cancelled or stalled), 'error'. */
export function tone(s, nowMs = Date.now()) {
  switch (s?.state) {
    case 'running': return s.stalled || stalledAt(s, nowMs) ? 'warn' : 'wait';
    case 'done': return s.errors?.length ? 'warn' : 'ok';
    case 'failed': return 'error';
    case 'cancelled': return 'warn';
    default: return 'off';
  }
}

/** Whether a full rebuild runs (the button is then "Cancel"). */
export const running = (s) => s?.state === 'running';

/** No progress for two minutes while running (the server says so too; this follows the page's clock between reads). */
export function stalledAt(s, nowMs = Date.now()) {
  return running(s) && Number.isFinite(s.last_progress_ms) && nowMs - s.last_progress_ms > 120000;
}

const n = (v) => (Number.isFinite(v) ? v : 0);
const plural = (k, one, many) => `${k.toLocaleString('en-US')} ${k === 1 ? one : many}`;

/** "1.2 GB", "340 MB", "12 kB", "512 B". */
export function bytesText(b) {
  const v = n(b);
  if (v < 1000) return `${v} B`;
  const units = ['kB', 'MB', 'GB', 'TB'];
  let x = v;
  let i = -1;
  while (x >= 1000 && i < units.length - 1) { x /= 1000; i += 1; }
  return `${x >= 100 ? Math.round(x) : x.toFixed(1)} ${units[i]}`;
}

/** "about 3 h 20 min", "about 12 min", "under a minute". */
export function durationText(ms) {
  if (!Number.isFinite(ms)) return '';
  if (ms < 60000) return 'under a minute';
  const min = Math.round(ms / 60000);
  if (min < 60) return `about ${min} min`;
  const h = Math.floor(min / 60);
  const m = min % 60;
  return m ? `about ${h} h ${m} min` : `about ${h} h`;
}

/** "3 min ago", "just now" (for the last progress). */
export function agoText(ms, nowMs = Date.now()) {
  if (!Number.isFinite(ms)) return '';
  const s = Math.max(0, Math.round((nowMs - ms) / 1000));
  if (s < 10) return 'just now';
  if (s < 90) return `${s} s ago`;
  const m = Math.round(s / 60);
  if (m < 90) return `${m} min ago`;
  return `${Math.round(m / 60)} h ago`;
}

/** A percentage 0..100 of the phase under way (logs by bytes when known, days), or null. */
export function percent(s) {
  if (!running(s)) return s?.state === 'done' ? 100 : null;
  if (s.phase === 'decoding') {
    const b = s.bytes || {};
    if (n(b.total) > 0) return Math.min(100, Math.floor((100 * n(b.done)) / n(b.total)));
    const l = s.logs || {};
    return n(l.total) > 0 ? Math.min(100, Math.floor((100 * n(l.done)) / n(l.total))) : 0;
  }
  if (s.phase === 'days') {
    const d = s.days || {};
    return n(d.total) > 0 ? Math.min(100, Math.floor((100 * n(d.done)) / n(d.total))) : 0;
  }
  return s.phase === 'switching' ? 100 : 0;
}

/**
 * The progress panel's lines, always the same seven in the same order (`label`, `text`; an empty text keeps the
 * line's room): State, Step, Logs decoded, Days, Data, Speed and time left, Last progress. `carName(id)` names a car.
 */
export function lines(s, nowMs = Date.now(), carName = (id) => id) {
  const st = s?.state || 'idle';
  const r = running(s);
  const cars = n(s?.cars_total) > 1 ? ` (car ${Math.min(n(s.cars_done) + 1, n(s.cars_total))} of ${n(s.cars_total)}: ${carName(s.car)})` : '';
  let state = STATES[st] || st;
  if (st === 'done' && s.errors?.length) state = `Done, with ${plural(s.errors.length, 'problem', 'problems')}`;
  if (r && (s.stalled || stalledAt(s, nowMs))) state = 'Running, but no progress for a while';
  const logs = s?.logs || {};
  const days = s?.days || {};
  const bytes = s?.bytes || {};
  const showCounts = r || st === 'done' || st === 'failed' || st === 'cancelled';
  const speed = r && Number.isFinite(s.rate_bytes_per_s) ? `${bytesText(s.rate_bytes_per_s)}/s` : '';
  const left = r && Number.isFinite(s.eta_ms) ? `${durationText(s.eta_ms)} left` : '';
  const when = st === 'idle' ? '' : r ? agoText(s.last_progress_ms, nowMs) : endedText(s, nowMs);
  return [
    { key: 'state', label: 'State', text: state },
    { key: 'phase', label: 'Step', text: r ? `${PHASES[s.phase] || s.phase || ''}${cars}` : '' },
    { key: 'logs', label: 'Logs decoded', text: showCounts && n(logs.total) ? `${n(logs.done).toLocaleString('en-US')} of ${n(logs.total).toLocaleString('en-US')}` : '' },
    { key: 'days', label: 'Days', text: showCounts && n(days.total) ? `${n(days.done).toLocaleString('en-US')} of ${n(days.total).toLocaleString('en-US')}` : '' },
    { key: 'bytes', label: 'Data', text: showCounts && n(bytes.total) ? `${bytesText(bytes.done)} of ${bytesText(bytes.total)}` : '' },
    { key: 'rate', label: 'Speed', text: [speed, left].filter(Boolean).join(' · ') },
    { key: 'when', label: r ? 'Last progress' : 'Finished', text: when },
  ];
}

function endedText(s, nowMs) {
  if (!Number.isFinite(s?.finished_ms)) return '';
  const took = Number.isFinite(s.started_ms) ? `, took ${durationText(s.finished_ms - s.started_ms).replace(/^about /, '')}` : '';
  return `${agoText(s.finished_ms, nowMs)}${took}`;
}

/** The errors, newest last, as readable lines ("Car c1: The log ... could not be decoded: ..."); at most `max`. */
export function errorLines(s, max = 3, carName = (id) => id) {
  const errs = Array.isArray(s?.errors) ? s.errors : [];
  const out = errs.slice(-max).map((e) => (e.car && n(s.cars_total) > 1 ? `${carName(e.car)}: ${e.message}` : e.message));
  if (errs.length > max) out.unshift(`${errs.length - max} more problems (oldest not shown).`);
  return out;
}

/** The progress bar's fill, 0..1 (0 when none runs). */
export const fraction = (s) => (percent(s) ?? 0) / 100;

/** A line about this car's history and this server's decoding, or '' (for Data health). */
export function staleText(s) {
  if (s?.car_stale) return 'This car\'s history was made with an older way of decoding the logs: a full rebuild makes it again with this server\'s.';
  return '';
}

/** The POST body: the password asked again (and the two-step code when one is given). */
export function body(password, code) {
  const b = { confirm_password: password };
  if (code) b.code = code;
  return b;
}

/** An error of the POST in words: a wrong password, one running already, else the server's message. */
export function errorText(e) {
  switch (e?.code) {
    case 'wrong_password': return 'That password is not right.';
    case 'password_needed': return 'Type your password to go on.';
    case 'wrong_code': return 'That two-step code is not right.';
    case 'full_rebuild_running': return 'A full rebuild is running already.';
    case 'full_rebuild_not_running': return 'No full rebuild is running.';
    case 'https_only': return 'Use HTTPS: the password is only sent over a secure connection.';
    default: return e?.message || 'The full rebuild could not be started.';
  }
}

/** Whether a notice is one of the full rebuild's (for the notices bar: it links to Settings: Server and access). */
export const isFullRebuildNotice = (n) => typeof n?.code === 'string' && n.code.startsWith('full_rebuild_');
export const NOTICE_LINK = '/settings/server#full-rebuild';
