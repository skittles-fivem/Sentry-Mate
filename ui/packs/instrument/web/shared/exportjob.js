// The export file's status (docs/ui-api.md: ExportFile, GET /system/exports/{export}) in the same words on every
// pack: which step it is at, a bar with its percentage, "7.7 GB of 18.4 GB", the speed, the time left, a warning when
// nothing moved for a minute, what went wrong and what to do, and once ready its size and SHA-256. Pure functions:
// each pack draws the model in its own look, into a panel of a fixed size (every slot is always there, empty or
// not, so the box never jumps as the export goes on).

import { format, DASH } from './units.js';

/** The steps of making an export, as the server numbers them (`progress.step`). */
export const EXPORT_STEPS = Object.freeze([
  { phase: 'counting', label: 'Counting', sub: 'what goes in' },
  { phase: 'waiting', label: 'Waiting', sub: 'for writes under way' },
  { phase: 'collecting', label: 'Snapshot', sub: 'uploads wait' },
]);

const PHASE_WORDS = {
  counting: 'Counting the files to carry',
  waiting: 'Waiting for the writes under way',
  collecting: 'Taking the snapshot (uploads wait)',
};

/** What to do for each failure code, when the server gave no words of its own. */
const TODO = {
  no_space: 'Free some space on the data disk, then make the export again.',
  no_hard_links: 'Use a move code instead, or keep the data folder on a disk that has hard links.',
  writes_busy: 'Something kept writing (an import, a big upload): try again in a few minutes.',
  server_stopping: 'The server is stopping: make the export once it runs again.',
  changed: 'Make the export again.',
  server_restarted: 'The server restarted while the file was being made: make the export again.',
  disk: 'Look at the server\'s log and its disk, then make the export again.',
  server_error: 'Look at the server\'s log, then make the export again.',
};

/** Bytes with one decimal from 1 GB up, so "7.7 GB of 18.4 GB" moves in steps one can see. */
export function exportBytes(n) {
  if (n == null || !Number.isFinite(n)) return DASH;
  return format(n, 'bytes', n >= 1e9 ? { digits: 1 } : {});
}

/** A length of time in words: "45 s", "3 min", "1 h 20 min". */
export function spanWords(s) {
  if (s == null || !Number.isFinite(s) || s < 0) return DASH;
  const n = Math.round(s);
  if (n < 60) return `${n} s`;
  if (n < 3600) {
    const m = Math.floor(n / 60), r = n % 60;
    return m < 5 && r >= 10 ? `${m} min ${r} s` : `${Math.round(n / 60)} min`;
  }
  const hh = Math.floor(n / 3600), mm = Math.round((n % 3600) / 60);
  return mm ? `${hh} h ${mm} min` : `${hh} h`;
}

/** "about 2 min left", "almost done" under a second, null when it can't be told. */
export function leftWords(etaS) {
  if (etaS == null || !Number.isFinite(etaS)) return null;
  return etaS < 1 ? 'almost done' : `about ${spanWords(etaS)} left`;
}

/** "150 MB/s", null when not known; "nothing moves" at 0. */
export function rateWords(perS) {
  if (perS == null || !Number.isFinite(perS)) return null;
  return perS <= 0 ? 'nothing moves' : `${exportBytes(perS)}/s`;
}

const pctText = (p) => `${format(Math.floor(p), 'number', { digits: 0 })} %`;
const clampPct = (p) => (p == null || !Number.isFinite(p) ? null : Math.max(0, Math.min(100, p)));

/** The three steps, each done, now or later. */
function stepsOf(step) {
  return EXPORT_STEPS.map((s, i) => ({ ...s, n: i + 1, state: step > i + 1 ? 'done' : step === i + 1 ? 'now' : 'later' }));
}

/** The progress lines of a `progress` (or `checksum`) object. */
function lines(p, { what = 'files' } = {}) {
  const known = p.bytes_total != null;
  const percent = known ? clampPct(p.percent ?? (p.bytes_total ? (100 * p.bytes_done) / p.bytes_total : 100)) : null;
  const amount = known ? `${exportBytes(p.bytes_done)} of ${exportBytes(p.bytes_total)}` : `${exportBytes(p.bytes_done)} so far`;
  const files = what === 'files' && p.files_done != null
    ? (p.files_total != null ? `${format(p.files_done, 'count')} of ${format(p.files_total, 'count')} files` : `${format(p.files_done, 'count')} files so far`)
    : '';
  const rate = rateWords(p.bytes_per_s);
  const left = p.stalled ? null : leftWords(p.eta_s);
  return {
    percent,
    percentText: percent == null ? '' : pctText(percent),
    amount,
    files,
    speed: [rate, left].filter(Boolean).join(' · ') || (known ? 'working out the speed…' : ''),
  };
}

/** The stalled warning: "No progress for 2 min." and what it may mean. */
export function stalledWords(quietMs) {
  return {
    head: `No progress for ${spanWords((quietMs || 0) / 1000)}`,
    text: 'The server may be busy with a large file (the database is copied whole). Wait a little, or cancel and make it again later; the server\'s log says more.',
  };
}

/**
 * Everything a pack draws for the export `e` (an ExportFile, or null for none yet):
 * { kind: 'none'|'busy'|'ready'|'failed'|'expired', tone: 'info'|'busy'|'warn'|'ok'|'error', head, sub, steps,
 *   percent (0..100 or null: not known yet), percentText, amount, files, speed, warn: { head, text } | null,
 *   problem: { code, message, todo } | null, sha256, checksum (words), canCancel, canDownload }.
 * Every text slot is a string ('' when empty), so a panel draws the same rows in every state.
 */
export function exportModel(e) {
  const base = {
    kind: 'none', tone: 'info', head: '', sub: '', steps: stepsOf(0), percent: null, percentText: '', amount: '',
    files: '', speed: '', warn: null, problem: null, sha256: null, checksum: '', canCancel: false, canDownload: false,
  };
  if (!e) return { ...base, head: 'No export file yet', sub: 'Make one: the server counts, waits a moment, then takes the snapshot.' };
  if (e.state === 'preparing') {
    const p = e.progress || { step: 1, bytes_done: 0, bytes_total: null, files_done: 0, files_total: null };
    const step = p.step || 1;
    const phase = p.phase || e.phase || 'counting';
    const l = lines(p);
    const warn = p.stalled ? stalledWords(p.quiet_ms) : null;
    return {
      ...base, ...l, kind: 'busy', tone: warn ? 'warn' : 'busy', warn,
      head: warn ? warn.head : 'Making the export file',
      sub: `Step ${step} of ${p.steps || 3}: ${PHASE_WORDS[phase] || phase}`,
      steps: stepsOf(step), canCancel: true,
      amount: phase === 'waiting' ? 'uploads under way finish first' : l.amount,
      files: phase === 'waiting' ? 'at most 30 s' : l.files,
      speed: phase === 'waiting' ? '' : l.speed,
      percentText: l.percentText || (phase === 'counting' ? 'counting…' : ''),
    };
  }
  if (e.state === 'failed') {
    const pr = e.problem || {};
    const code = pr.code || 'server_error';
    return {
      ...base, kind: 'failed', tone: 'error', head: 'The export file could not be made',
      sub: pr.message || e.error || 'The server gave no reason.', steps: stepsOf(0),
      problem: { code, message: pr.message || e.error || '', todo: pr.what_to_do || TODO[code] || TODO.server_error },
    };
  }
  if (e.state === 'expired') {
    return { ...base, kind: 'expired', head: 'The last export file was removed', sub: `${e.file_name || 'It'} was kept its time. Make a new one.` };
  }
  // ready
  const c = e.checksum;
  let checksum = '';
  if (e.sha256) {
    // known: the panel shows it in full
  } else if (c?.state === 'failed') checksum = `SHA-256: could not be worked out (${c.error || 'see the log'}); the file itself checks on the new server.`;
  else if (c) {
    const l = c.bytes_total != null ? lines(c, { what: 'bytes' }) : null;
    checksum = `SHA-256: reading the file once${l?.percentText ? `, ${l.percentText}` : ''}${l && leftWords(c.eta_s) ? `, ${leftWords(c.eta_s)}` : ''}… You can download now.`;
  }
  return {
    ...base, kind: 'ready', tone: 'ok', head: 'The export file is ready',
    sub: [exportBytes(e.size), e.files != null ? `${format(e.files, 'count')} files` : null].filter(Boolean).join(' · '),
    steps: stepsOf(4), percent: 100, percentText: '100 %', sha256: e.sha256 || null, checksum, canDownload: !!e.url,
  };
}

/** How long a page waits before it looks again: 1 s while it is made or its checksum is worked out, else never. */
export function exportPollMs(e) {
  if (!e) return null;
  if (e.state === 'preparing') return 1000;
  if (e.state === 'ready' && e.checksum?.state === 'working') return 2000;
  return null;
}
