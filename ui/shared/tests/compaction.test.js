import { test } from 'node:test';
import assert from 'node:assert/strict';
import './fake-browser.js';
import { compactionProgress } from '../compaction.js';

const NOW = Date.parse('2026-11-16T17:30:00Z');
const run = (over) => ({ status: 'running', automatic: false, step: 'logs', step_number: 1, steps: 3, phase: 'packing', unit: 'days',
  done: 12, total: 40, pass_saved_bytes: 1_200_000, rate_per_s: 0.05, eta_ms: 560_000, progress_ms: NOW - 5000, stalled: false,
  unit_errors: 0, last_unit_error: null, ...over });
const bytes = (n) => `${n} B`;

test('a pass that is not running says nothing of progress', () => {
  const p = compactionProgress({ status: 'idle' }, { now: NOW });
  assert.equal(p.running, false);
  assert.equal(p.pct, null);
  assert.equal(compactionProgress(null).running, false);
});

test('a running step: its number, units done of total, what it freed, rate, estimate and last progress', () => {
  const p = compactionProgress(run(), { now: NOW, bytes });
  assert.equal(p.step, 'CAN logs');
  assert.equal(p.stepOf, 'Step 1 of 3');
  assert.equal(p.counting, false);
  assert.equal(p.pct, 30);
  assert.equal(p.count, '12 of 40 days');
  assert.equal(p.saved, 'Freed so far in this pass: 1200000 B');
  assert.equal(p.rate, '3 days a minute');
  assert.match(p.eta, /^About 9 min left in this step$/);
  assert.match(p.last, /^Last step 5 s ago$/);
  assert.equal(p.stalled, false);
  assert.equal(p.errors, null);
});

test('counting: no bar and no estimate yet', () => {
  const p = compactionProgress(run({ phase: 'counting', done: 0, total: 0, rate_per_s: null, eta_ms: null }), { now: NOW });
  assert.equal(p.counting, true);
  assert.equal(p.pct, null);
  assert.equal(p.count, 'Counting days…');
  assert.equal(p.eta, null);
  assert.equal(p.rate, null);
});

test('records, a slow rate, a stall and units left as they were', () => {
  const p = compactionProgress(run({ step: 'records', unit: 'records', step_number: 3, done: 1, total: 1, rate_per_s: 0.001,
    stalled: true, progress_ms: NOW - 180_000, unit_errors: 2, last_unit_error: 'a pack is damaged' }), { now: NOW });
  assert.equal(p.count, '1 of 1 record');
  assert.equal(p.rate, '3.6 records an hour');
  assert.match(p.stall, /^No progress for 3 min/);
  assert.equal(p.errors, '2 records could not be packed and were left as they were: a pack is damaged');
});
