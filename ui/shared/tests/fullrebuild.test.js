// Tests for fullrebuild.js: the panel's fixed lines, the state words and tones, the POST body and error words.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as F from '../fullrebuild.js';

const NOW = Date.UTC(2026, 10, 22, 12);

const running = {
  state: 'running', id: 3, scope: 'all', cars: ['c1', 'c2'], car: 'c2', cars_done: 1, cars_total: 2, reason: 'owner',
  phase: 'decoding', logs: { done: 1200, total: 4800 }, days: { done: 40, total: 40 },
  bytes: { done: 250e6, total: 1e9, copied: 30e6 }, rate_bytes_per_s: 2.5e6, eta_ms: 300000,
  started_ms: NOW - 600000, finished_ms: null, last_progress_ms: NOW - 3000, stalled: false, errors: [], error: null,
};

test('the panel always has the same seven lines', () => {
  const idle = F.lines({ state: 'idle' }, NOW);
  const run = F.lines(running, NOW, (id) => (id === 'c2' ? 'Comet' : id));
  assert.deepEqual(idle.map((l) => l.key), run.map((l) => l.key));
  assert.equal(idle.length, 7);
  assert.equal(idle[0].text, 'Never run');
  assert.ok(idle.slice(1).every((l) => l.text === ''), 'an idle panel keeps the lines, empty');
  const by = Object.fromEntries(run.map((l) => [l.key, l.text]));
  assert.equal(by.state, 'Running');
  assert.equal(by.phase, 'Decoding the stored logs (car 2 of 2: Comet)');
  assert.equal(by.logs, '1,200 of 4,800');
  assert.equal(by.bytes, '250 MB of 1.0 GB');
  assert.equal(by.rate, '2.5 MB/s · about 5 min left');
  assert.equal(by.when, 'just now');
  assert.equal(F.percent(running), 25);
  assert.equal(F.tone(running, NOW), 'wait');
});

test('a stalled run, a failed one and one done with problems say so', () => {
  const stalled = { ...running, last_progress_ms: NOW - 5 * 60000 };
  assert.equal(F.tone(stalled, NOW), 'warn');
  assert.equal(F.lines(stalled, NOW)[0].text, 'Running, but no progress for a while');
  assert.equal(F.lines(stalled, NOW)[6].text, '5 min ago');
  const failed = { ...running, state: 'failed', finished_ms: NOW - 60000, error: 'disk full', errors: [{ car: 'c2', log: null, message: 'disk full' }] };
  assert.equal(F.tone(failed, NOW), 'error');
  assert.equal(F.lines(failed, NOW)[0].text, 'Failed: the old history was kept');
  assert.equal(F.lines(failed, NOW)[1].text, '', 'no step once it ended');
  assert.deepEqual(F.errorLines(failed, 3, () => 'Comet'), ['Comet: disk full']);
  const done = { ...running, state: 'done', finished_ms: NOW - 3600e3, errors: [{ car: 'c1', message: 'a' }, { car: 'c1', message: 'b' }] };
  assert.equal(F.lines(done, NOW)[0].text, 'Done, with 2 problems');
  assert.match(F.lines(done, NOW)[6].text, /^60 min ago, took /);
  assert.equal(F.tone(done, NOW), 'warn');
  assert.deepEqual(F.errorLines(done, 1), ['1 more problems (oldest not shown).', 'c1: b']);
});

test('words: bytes, durations, the body and the errors', () => {
  assert.equal(F.bytesText(512), '512 B');
  assert.equal(F.bytesText(12_300), '12.3 kB');
  assert.equal(F.bytesText(340e6), '340 MB');
  assert.equal(F.durationText(30000), 'under a minute');
  assert.equal(F.durationText(200 * 60000), 'about 3 h 20 min');
  assert.deepEqual(F.body('pw'), { confirm_password: 'pw' });
  assert.deepEqual(F.body('pw', '123456'), { confirm_password: 'pw', code: '123456' });
  assert.equal(F.errorText({ code: 'wrong_password' }), 'That password is not right.');
  assert.equal(F.errorText({ code: 'full_rebuild_running' }), 'A full rebuild is running already.');
  assert.equal(F.errorText({ message: 'x' }), 'x');
  assert.ok(F.isFullRebuildNotice({ code: 'full_rebuild_offered' }));
  assert.ok(!F.isFullRebuildNotice({ code: 'release_available' }));
  assert.equal(F.staleText({ car_stale: true }).startsWith('This car'), true);
  assert.equal(F.staleText({ car_stale: false }), '');
  assert.equal(F.EXPLAIN.length, 2);
});
