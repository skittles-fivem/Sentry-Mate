// The full rebuild (docs/ui-api.md: /cars/{car}/full-rebuild, /system/full-rebuild), in the same words on every
// pack: what it does and does not touch, the button's state, the progress panel's fixed lines (phase, logs, days,
// bytes, rate, time left, last progress with the stall warning, errors) and the POST body with the password asked
// again. Pure functions: a page renders them in its own look, into a panel of fixed size (every line is always
// there, an empty one keeps its room). The status comes as `job` events of kind `full_rebuild` on the live stream
// (shared/live.js); a page reads it again every POLL_MS while one runs only while the stream is down.

/** How often a page reads the status again while a full rebuild runs and the live stream is down. */
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

/** Whether a full rebuild runs (the button is then "Cancel"); one that waits or is paused runs too. */
export const running = (s) => s?.state === 'running';

/** Whether a cancel was asked and the run has not ended yet: it is stopping, whatever else it was doing. */
export const cancelling = (s) => running(s) && !!s?.cancelling;

/** Whether the one under way waits (a new version's grace, put off until tonight) or is paused: no progress. */
export const held = (s) => running(s) && !!s?.hold;

/** When the one under way goes on (PATCH system/full-rebuild { when }): the buttons beside Cancel, in this order. */
export const WHEN = [
  { when: 'now', label: 'Start now', title: 'Start the full rebuild now' },
  { when: 'tonight', label: 'Tonight', title: 'Put the full rebuild off until 02:00 tonight (the server\'s time zone)' },
  { when: 'pause', label: 'Pause', title: 'Pause the full rebuild (its progress is kept) until you start it' },
];

/** Whether the button for `when` is shown: only while one runs, and not for what it does already. */
export function whenShown(s, when) {
  if (!running(s) || cancelling(s)) return false;
  const k = s.hold?.kind;
  if (when === 'now') return !!k;
  if (when === 'tonight') return k !== 'tonight';
  return k !== 'paused';
}

/** The PATCH body, and what the page says once it is done. */
export const whenBody = (when) => ({ when });
export const whenDone = (when) => ({
  now: 'Full rebuild started. Pages show the old history until the new one is ready.',
  tonight: 'Full rebuild put off until tonight.',
  pause: 'Full rebuild paused. Its progress is kept; start it again when you like.',
}[when] || '');

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

/** "in about 8 min", "in about 5 h" (for a wait). */
export function inText(ms, nowMs = Date.now()) {
  if (!Number.isFinite(ms)) return '';
  const left = ms - nowMs;
  if (left < 60000) return 'in under a minute';
  return `in ${durationText(left)}`;
}

/** What a hold says as the state: "Waits: starts in about 8 min", "Paused: starts when you start it". */
export function holdText(s, nowMs = Date.now()) {
  const h = s?.hold;
  if (!h) return '';
  if (h.kind === 'paused') return 'Paused: starts when you start it';
  if (h.kind === 'tonight') return `Put off until tonight: starts ${inText(h.until_ms, nowMs)}`;
  return `Waits: starts ${inText(h.until_ms, nowMs)}, unless you choose otherwise`;
}

/** Why it runs, in words (every cause, joined), and the first change explained (a link), or ''. */
export function whyText(s) {
  const c = Array.isArray(s?.causes) ? s.causes : [];
  return c.map((x) => x.text).filter(Boolean).join('; ');
}
export const whyLink = (s) => (Array.isArray(s?.causes) ? s.causes.find((x) => x.link)?.link || '' : '');
export const WHY_LINK = 'What changed';

/** "3 min ago", "just now" (for the last progress). */
export function agoText(ms, nowMs = Date.now()) {
  if (!Number.isFinite(ms)) return '';
  const s = Math.max(0, Math.round((nowMs - ms) / 1000));
  if (s < 10) return 'just now';
  if (s < 90) return `${s} s ago`;
  const m = Math.round(s / 60);
  if (m < 90) return `${m} min ago`;
  const hours = Math.round(m / 60);
  if (hours < 48) return `${hours} h ago`;
  return `${Math.round(hours / 24)} days ago`;
}

/**
 * A percentage 0..100 of the phase under way, or null: the logs decoded of all while decoding, then the days worked
 * out of all in the days step (the same counts the panel's lines show, so the bar always matches them); 100 while
 * switching and once done. The server's own `percent` when it sends one.
 */
export function percent(s) {
  if (!running(s)) return s?.state === 'done' ? 100 : null;
  if (Number.isFinite(s.percent)) return Math.max(0, Math.min(100, Math.floor(s.percent)));
  const of = (x) => (n(x?.total) > 0 ? Math.min(100, Math.floor((100 * n(x.done)) / n(x.total))) : 0);
  if (s.phase === 'decoding') return of(s.logs);
  if (s.phase === 'days') return of(s.days);
  return s.phase === 'switching' ? 100 : 0;
}

/**
 * The one line of the Rebuild row of Data health, from the same status as the panel (so the two never disagree):
 * { text, tone } for this car's full rebuild, or null when none runs (the row then says what the period rebuild
 * does). A paused or waiting run says so; the step is named only while a worker is on it.
 */
export function rowText(s, nowMs = Date.now(), carName = (id) => id) {
  if (!running(s)) return null;
  if (cancelling(s)) return { text: 'Cancelling the full rebuild', tone: 'warn' };
  if (held(s)) return { text: `Full rebuild: ${holdText(s, nowMs)}`, tone: 'warn' };
  const step = lines(s, nowMs, carName).find((l) => l.key === 'phase')?.text;
  return { text: step ? `Running: ${step}` : 'Running', tone: 'warn' };
}

/** "12 a second", "1.5 a second", "40 a minute" of a thing counted (logs, days). */
export function countRate(perS, one, many) {
  if (!Number.isFinite(perS) || perS <= 0) return '';
  if (perS >= 1) {
    const v = perS >= 10 ? Math.round(perS) : Math.round(perS * 10) / 10;
    return `${v.toLocaleString('en-US')} ${v === 1 ? one : many}/s`;
  }
  const m = Math.round(perS * 60);
  return `${m.toLocaleString('en-US')} ${m === 1 ? one : many}/min`;
}

/**
 * The progress panel's lines, always the same eight in the same order (`label`, `text`; an empty text keeps the
 * line's room): State, Step, Logs decoded, Days, Data, Speed and time left, Last progress, Why (what started it:
 * its `link` the change explained, or ''). `carName(id)` names a car. While it waits or is paused, State says until
 * when.
 */
export function lines(s, nowMs = Date.now(), carName = (id) => id) {
  const st = s?.state || 'idle';
  const r = running(s);
  const cars = n(s?.cars_total) > 1 ? ` (car ${Math.min(n(s.cars_done) + 1, n(s.cars_total))} of ${n(s.cars_total)}: ${carName(s.car)})` : '';
  let state = STATES[st] || st;
  if (st === 'done' && s.errors?.length) state = `Done, with ${plural(s.errors.length, 'problem', 'problems')}`;
  if (r && (s.stalled || stalledAt(s, nowMs))) state = 'Running, but no progress for a while';
  if (held(s)) state = holdText(s, nowMs);
  // (a run asked to stop is stopping: nothing else is said of it)
  if (cancelling(s)) state = 'Cancelling: the old history is kept';
  const logs = s?.logs || {};
  const days = s?.days || {};
  const bytes = s?.bytes || {};
  const showCounts = r || st === 'done' || st === 'failed' || st === 'cancelled';
  const idle = held(s) || cancelling(s);
  const speed = !r || idle ? '' : [
    Number.isFinite(s.rate_bytes_per_s) ? `${bytesText(s.rate_bytes_per_s)}/s` : '',
    s.phase === 'decoding' ? countRate(s.rate_logs_per_s, 'log', 'logs') : '',
    s.phase === 'days' ? countRate(s.rate_days_per_s, 'day', 'days') : '',
  ].filter(Boolean).join(' · ');
  const left = r && !idle && Number.isFinite(s.eta_ms) ? `${durationText(s.eta_ms)} left` : '';
  const measuring = r && !idle && (s.phase === 'decoding' || s.phase === 'days') && !speed && !left ? 'Working out the speed' : '';
  // the days are known from the start of the decoding (the car's days, counted again when their step starts), and
  // the data from the logs' sizes: both are shown as soon as a run is under way
  const count = (x, f = (v) => n(v).toLocaleString('en-US')) => (showCounts && n(x.total) ? `${f(x.done)} of ${f(x.total)}` : '');
  const daysText = count(days) || (r && s.phase !== 'switching' ? 'Counted once the logs are decoded' : '');
  const bytesLine = count(bytes, bytesText);
  const when = st === 'idle' ? '' : idle ? '' : r ? agoText(s.last_progress_ms, nowMs) : endedText(s, nowMs);
  return [
    { key: 'state', label: 'State', text: state },
    // (a run that waits or is paused does no step: State says so, and the step is not named as if it ran)
    { key: 'phase', label: 'Step', text: r && !idle ? `${PHASES[s.phase] || s.phase || ''}${cars}` : '' },
    { key: 'logs', label: 'Logs decoded', text: count(logs) },
    { key: 'days', label: 'Days', text: daysText },
    { key: 'bytes', label: 'Data', text: bytesLine },
    { key: 'rate', label: 'Speed', text: [speed, left].filter(Boolean).join(' · ') || measuring },
    { key: 'when', label: r ? 'Last progress' : 'Finished', text: when },
    { key: 'why', label: 'Why', text: st === 'idle' ? '' : whyText(s), link: st === 'idle' ? '' : whyLink(s) },
  ];
}

/** The recent rebuilds list: its title and how many rows it always has (an empty one keeps its room). */
export const HISTORY_TITLE = 'Recent rebuilds';
export const HISTORY_ROWS = 5;

const KINDS = {
  full: (who) => `Full rebuild of ${who}`,
  car: (who) => `${who}'s history made again`,
  positions: (who) => `${who}'s positions read again`,
  inputs: (who) => `${who}'s inputs joined again`,
  'positions+inputs': (who) => `${who}'s positions read and inputs joined again`,
};

/**
 * The last rebuilds of any kind, newest first, always HISTORY_ROWS lines (`text`, `title`; an empty text keeps the
 * line's room): "2 h ago · Full rebuild of every car · Server updated: positions are worked out … · took 25 min".
 */
export function historyLines(s, nowMs = Date.now(), carName = (id) => id) {
  const runs = Array.isArray(s?.history) ? s.history.slice(0, HISTORY_ROWS) : [];
  const out = runs.map((r) => {
    const who = r.scope === 'all' ? 'every car' : (r.cars || []).map(carName).join(', ') || 'a car';
    const what = (KINDS[r.kind] || KINDS.car)(who);
    const why = (r.causes || []).map((c) => c.text).filter(Boolean).join('; ');
    const took = Number.isFinite(r.took_ms) ? `took ${durationText(r.took_ms).replace(/^about /, '')}` : '';
    const end = r.state === 'failed' ? 'failed' : r.state === 'cancelled' ? 'cancelled' : '';
    const text = [agoText(r.finished_ms, nowMs), what, why, end || took].filter(Boolean).join(' · ');
    return { text, title: text };
  });
  if (!out.length && s) out.push({ text: 'None yet.', title: '' });
  while (out.length < HISTORY_ROWS) out.push({ text: '', title: '' });
  return out;
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
    case 'invalid': return 'That is not one of start now, tonight or pause.';
    case 'https_only': return 'Use HTTPS: the password is only sent over a secure connection.';
    default: return e?.message || 'The full rebuild could not be started.';
  }
}

/** Whether a notice is one of the full rebuild's (for the notices bar: it links to Settings: Server and access). */
export const isFullRebuildNotice = (n) => typeof n?.code === 'string' && n.code.startsWith('full_rebuild_');
export const NOTICE_LINK = '/settings/server#full-rebuild';
