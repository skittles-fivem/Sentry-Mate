// Sentry Mate shared module: compaction.js (how a compaction pass is getting on, in words).
// Canonical source: ui/shared/compaction.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// compactionProgress(c, { now, bytes }) -> the words of the Storage panel's progress row, from the `compaction`
// object of GET /system/storage/detail (docs/ui-api.md). `now` is the server's clock (api.now()), `bytes(n)`
// the pack's byte format. Every pack shows the same facts in its own look, in a row of fixed size:
//
// {
//   running,          a pass is under way
//   step,             'CAN logs' | 'time series' | 'records' | null
//   stepOf,           'Step 1 of 3' | null
//   counting,         the step is still counting what it has to do (no bar yet: an indeterminate one)
//   pct,              0..100 of the step, null while counting
//   count,            '12 of 40 days' | 'Counting days…'
//   saved,            'Freed so far in this pass: 1.2 MB' | null
//   rate,             '3.5 days a minute' | null (not known before the first unit)
//   eta,              'About 4 min left in this step' | null
//   last,             'Last step 5 s ago' | null
//   stalled,          no unit finished for a while (the server says so)
//   stall,            'No progress for 3 min: the server may be busy, or the pass is stuck.' | null
//   errors,           '2 days could not be packed and were left as they were: <why>' | null
// }

import { formatDuration } from './time.js';
import { formatNumber } from './i18n.js';

export const COMPACT_STEPS = { logs: 'CAN logs', series: 'time series', records: 'records', imports: 'uploaded backups' };
const UNIT = { days: ['day', 'days'], records: ['record', 'records'], files: ['file', 'files'] };

const plural = (n, [one, other]) => (n === 1 ? one : other);

export function compactionProgress(c, { now = Date.now(), bytes = (n) => `${formatNumber(n)} B` } = {}) {
  const running = c?.status === 'running';
  const out = { running, step: null, stepOf: null, counting: false, pct: null, count: '', saved: null, rate: null, eta: null, last: null, stalled: false, stall: null, errors: null };
  if (!running) return out;
  const unit = UNIT[c.unit] || UNIT.days;
  out.step = COMPACT_STEPS[c.step] || c.step || null;
  out.stepOf = c.step_number && c.steps ? `Step ${c.step_number} of ${c.steps}` : null;
  const done = Math.max(0, c.done || 0), total = Math.max(0, c.total || 0);
  out.counting = c.phase === 'counting' || !c.step || (!total && c.phase !== 'packing');
  if (out.counting) {
    out.count = `Counting ${unit[1]}…`;
  } else {
    out.pct = total ? Math.min(100, Math.max(0, (done / total) * 100)) : 100;
    out.count = `${formatNumber(done)} of ${formatNumber(total)} ${plural(total, unit)}`;
  }
  if (c.pass_saved_bytes != null) out.saved = `Freed so far in this pass: ${bytes(c.pass_saved_bytes)}`;
  if (Number.isFinite(c.rate_per_s) && c.rate_per_s > 0) {
    const perMin = c.rate_per_s * 60;
    out.rate = perMin >= 1
      ? `${formatNumber(perMin, { maximumFractionDigits: perMin < 10 ? 1 : 0 })} ${plural(Math.round(perMin), unit)} a minute`
      : `${formatNumber(perMin * 60, { maximumFractionDigits: 1 })} ${plural(Math.round(perMin * 60), unit)} an hour`;
  }
  if (Number.isFinite(c.eta_ms) && !out.counting) out.eta = `About ${formatDuration(Math.max(1000, c.eta_ms))} left in this step`;
  if (Number.isFinite(c.progress_ms)) out.last = `Last step ${formatDuration(Math.max(0, now - c.progress_ms))} ago`;
  out.stalled = !!c.stalled;
  if (out.stalled) {
    const quiet = Number.isFinite(c.progress_ms) ? formatDuration(Math.max(0, now - c.progress_ms)) : null;
    out.stall = `No progress${quiet ? ` for ${quiet}` : ''}: the server may be busy, or the pass is stuck.`;
  }
  if (c.unit_errors) {
    const n = c.unit_errors;
    out.errors = `${formatNumber(n)} ${plural(n, unit)} could not be packed and ${n === 1 ? 'was' : 'were'} left as ${n === 1 ? 'it was' : 'they were'}${c.last_unit_error ? `: ${c.last_unit_error}` : '.'}`;
  }
  return out;
}
