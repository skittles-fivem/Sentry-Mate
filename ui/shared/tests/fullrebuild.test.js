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

test('the panel always has the same eight lines', () => {
  const idle = F.lines({ state: 'idle' }, NOW);
  const run = F.lines(running, NOW, (id) => (id === 'c2' ? 'Comet' : id));
  assert.deepEqual(idle.map((l) => l.key), run.map((l) => l.key));
  assert.equal(idle.length, 8);
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

test('a run says why it runs, and one that waits says until when', () => {
  const causes = [
    { code: 'server_update', text: 'Server updated: positions are worked out differently', link: 'https://example.invalid/docs/rebuilds.md#rules-2' },
    { code: 'owner', text: 'Asked for by admin', link: null },
  ];
  const why = F.lines({ ...running, causes }, NOW).find((l) => l.key === 'why');
  assert.equal(why.text, 'Server updated: positions are worked out differently; Asked for by admin');
  assert.equal(why.link, 'https://example.invalid/docs/rebuilds.md#rules-2');
  assert.equal(F.lines({ state: 'idle', causes }, NOW).find((l) => l.key === 'why').text, '');

  const grace = { ...running, phase: 'queued', hold: { kind: 'grace', until_ms: NOW + 8 * 60000 } };
  assert.ok(F.held(grace) && !F.held(running));
  assert.equal(F.lines(grace, NOW)[0].text, 'Waits: starts in about 8 min, unless you choose otherwise');
  assert.equal(F.lines(grace, NOW)[6].text, '', 'no last progress while it waits');
  assert.equal(F.lines({ ...grace, hold: { kind: 'paused', until_ms: null } }, NOW)[0].text, 'Paused: starts when you start it');
  assert.equal(F.lines({ ...grace, hold: { kind: 'tonight', until_ms: NOW + 5 * 3600000 } }, NOW)[0].text, 'Put off until tonight: starts in about 5 h');
  assert.deepEqual(F.WHEN.filter((w) => F.whenShown(grace, w.when)).map((w) => w.when), ['now', 'tonight', 'pause']);
  assert.deepEqual(F.WHEN.filter((w) => F.whenShown(running, w.when)).map((w) => w.when), ['tonight', 'pause']);
  assert.deepEqual(F.WHEN.filter((w) => F.whenShown({ state: 'done' }, w.when)), []);
  assert.deepEqual(F.whenBody('tonight'), { when: 'tonight' });
});

test('the recent rebuilds keep their rows, newest first, with cause and time taken', () => {
  const history = [
    { kind: 'positions', scope: 'car', cars: ['c1'], causes: [{ code: 'gps_layout', text: 'The car\'s GPS layout changed: its positions are read again' }], state: 'done', started_ms: NOW - 3 * 60000, finished_ms: NOW - 2 * 60000, took_ms: 60000, logs: 12 },
    { kind: 'full', scope: 'all', cars: ['c1', 'c2'], causes: [{ code: 'server_update', text: 'Server updated: positions are worked out differently' }], state: 'cancelled', started_ms: NOW - 7200000, finished_ms: NOW - 3600000, took_ms: 3600000, logs: 900 },
  ];
  const l = F.historyLines({ state: 'idle', history }, NOW, (id) => (id === 'c1' ? 'Comet' : id));
  assert.equal(l.length, F.HISTORY_ROWS);
  assert.equal(l[0].text, '2 min ago · Comet\'s positions read again · The car\'s GPS layout changed: its positions are read again · took 1 min');
  assert.equal(l[1].text, '60 min ago · Full rebuild of every car · Server updated: positions are worked out differently · cancelled');
  assert.ok(l.slice(2).every((x) => x.text === ''));
  assert.equal(F.historyLines({ state: 'idle', history: [] }, NOW)[0].text, 'None yet.');
  assert.equal(F.historyLines(null, NOW)[0].text, '');
});

test('the rebuild speed line: logs a second now and the stage that takes most of the time', () => {
  assert.equal(F.speedText({ logs_per_s: 1.1, stage: 'windows', stage_share_pct: 72, over_ms: 30000 }),
    '1.1 logs/s · most of the time: rebuilding the timeline around them (72 %)');
  assert.equal(F.speedText({ logs_per_s: 0.5, stage: 'now', stage_share_pct: 40.4 }), '30 logs/min · most of the time: the newest values (40 %)');
  assert.equal(F.speedText({ logs_per_s: 156, stage: 'nope', stage_share_pct: 50 }), '156 logs/s');
  assert.equal(F.speedText(null), '');
  assert.equal(F.speedText({ logs_per_s: null }), '');
});
