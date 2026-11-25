// The export file's status words (exportjob.js): steps, bar, amounts, speed, time left, stalled, failed, ready.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { exportModel, exportPollMs, spanWords, leftWords, rateWords, exportBytes } from '../exportjob.js';

const GB = 1e9;
const preparing = (progress) => ({ id: 'ex1', state: 'preparing', phase: progress.phase, progress, checksum: null, sha256: null, problem: null });

test('the words of a length of time, a speed and an amount', () => {
  assert.equal(spanWords(45), '45 s');
  assert.equal(spanWords(130), '2 min 10 s');
  assert.equal(spanWords(600), '10 min');
  assert.equal(spanWords(4800), '1 h 20 min');
  assert.equal(leftWords(0.4), 'almost done');
  assert.equal(leftWords(90), 'about 1 min 30 s left');
  assert.equal(leftWords(null), null);
  assert.equal(rateWords(0), 'nothing moves');
  assert.equal(rateWords(153_600_000), '154\u00a0MB/s');
  assert.equal(exportBytes(7.74 * GB), '7.7 GB');
});

test('none yet, and every text slot is a string', () => {
  const m = exportModel(null);
  assert.equal(m.kind, 'none');
  for (const k of ['head', 'sub', 'percentText', 'amount', 'files', 'speed', 'checksum']) assert.equal(typeof m[k], 'string', k);
  assert.equal(exportPollMs(null), null);
});

test('counting: no total yet, so no percentage', () => {
  const m = exportModel(preparing({ phase: 'counting', step: 1, steps: 3, files_done: 1200, files_total: null, bytes_done: 2 * GB, bytes_total: null, percent: null, bytes_per_s: null, eta_s: null, quiet_ms: 0, stalled: false }));
  assert.equal(m.kind, 'busy');
  assert.equal(m.percent, null);
  assert.equal(m.percentText, 'counting…');
  assert.equal(m.sub, 'Step 1 of 3: Counting the files to carry');
  assert.match(m.amount, /2\.0.GB so far/);
  assert.match(m.files, /1,200 files so far/);
  assert.deepEqual(m.steps.map((s) => s.state), ['now', 'later', 'later']);
  assert.ok(m.canCancel && !m.canDownload);
});

test('collecting: a bar, x of y GB, the speed and the time left', () => {
  const e = preparing({ phase: 'collecting', step: 3, steps: 3, files_done: 21_974, files_total: 52_318, bytes_done: 7.74 * GB, bytes_total: 18.43 * GB, percent: 42.0, bytes_per_s: 153_000_000, eta_s: 70, quiet_ms: 400, stalled: false });
  const m = exportModel(e);
  assert.equal(m.percent, 42);
  assert.equal(m.percentText, '42 %');
  assert.equal(m.amount, '7.7 GB of 18.4 GB');
  assert.equal(m.files, '21,974 of 52,318 files');
  assert.equal(m.speed, '153 MB/s · about 1 min 10 s left');
  assert.equal(m.warn, null);
  assert.deepEqual(m.steps.map((s) => s.state), ['done', 'done', 'now']);
  assert.equal(exportPollMs(e), 1000);
});

test('stalled: a warning instead of the time left', () => {
  const m = exportModel(preparing({ phase: 'collecting', step: 3, steps: 3, files_done: 9, files_total: 20, bytes_done: 4 * GB, bytes_total: 10 * GB, percent: 40, bytes_per_s: 0, eta_s: null, quiet_ms: 125_000, stalled: true }));
  assert.equal(m.tone, 'warn');
  assert.equal(m.warn.head, 'No progress for 2 min');
  assert.equal(m.head, m.warn.head);
  assert.equal(m.speed, 'nothing moves');
});

test('failed: what went wrong and what to do', () => {
  const m = exportModel({ id: 'ex1', state: 'failed', phase: 'failed', error: 'not enough space', problem: { code: 'no_space', message: 'not enough space', what_to_do: 'Free some space.' } });
  assert.equal(m.kind, 'failed');
  assert.deepEqual(m.problem, { code: 'no_space', message: 'not enough space', todo: 'Free some space.' });
  // an older server: only `error`
  const old = exportModel({ id: 'ex1', state: 'failed', error: 'a disk error' });
  assert.equal(old.problem.code, 'server_error');
  assert.match(old.problem.todo, /log/);
});

test('failed: a restart that stopped the making says so, with the step to take', () => {
  const m = exportModel({ id: 'ex1', state: 'failed', phase: 'failed', error: 'The server restarted while the export file was being made.', problem: { code: 'server_restarted', message: 'The server restarted while the export file was being made.' } });
  assert.equal(m.kind, 'failed');
  assert.equal(m.problem.code, 'server_restarted');
  assert.match(m.problem.todo, /make the export again/);
});

test('ready: the size, then the SHA-256 once it is worked out', () => {
  const e = { id: 'ex1', state: 'ready', phase: 'ready', size: 4_318_761, files: 412, url: '/f', sha256: null, checksum: { state: 'working', bytes_done: 2e6, bytes_total: 4e6, percent: 50, bytes_per_s: 2e6, eta_s: 1, stalled: false } };
  let m = exportModel(e);
  assert.equal(m.kind, 'ready');
  assert.equal(m.sub, '4.3 MB · 412 files');
  assert.match(m.checksum, /50 %/);
  assert.ok(m.canDownload && !m.canCancel);
  assert.equal(exportPollMs(e), 2000);
  m = exportModel({ ...e, sha256: 'ab'.repeat(32), checksum: { state: 'done' } });
  assert.equal(m.sha256, 'ab'.repeat(32));
  assert.equal(m.checksum, '');
  assert.equal(exportPollMs({ ...e, checksum: { state: 'done' } }), null);
});
