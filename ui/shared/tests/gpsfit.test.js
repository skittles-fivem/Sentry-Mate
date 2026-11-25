import { test } from 'node:test';
import assert from 'node:assert/strict';
import './fake-browser.js';
import { fitModel, applyBody, metres, gpsFit, fitPath, layoutPath, layoutModel, hasCanData, STANDARD, BUTTON, AGAIN, RUNNING } from '../gpsfit.js';

// made-up answers of GET /cars/{car}/gps-layout/fit (layouts, counts and metres only)
const LAYOUT = 'lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)';
const result = (over) => ({
  car: 'c1', days: 30, logs: 10, logs_skipped: 0, days_read: 3, days_skipped: 0, pairs: 3000,
  sources: [{ input: 'teslamate-local', positions: 600 }, { input: 'tesla-api', positions: 0 }],
  enough: true, spread_ns_m: 240, spread_ew_m: 180, weak: false,
  lat: { dbc: { layout: '0|28@1- (1e-6,0)', median_m: 1, p95_m: 2 }, best: { layout: '0|28@1- (1e-6,0)', median_m: 1, p95_m: 2 }, runner_up: null },
  lon: { dbc: { layout: '28|28@1- (1e-6,0)', median_m: null, p95_m: null }, best: { layout: '28|28@1- (1e-6,16.777216)', median_m: 1, p95_m: 3 }, runner_up: null },
  layout: LAYOUT, median_m: 2, p95_m: 4, is_dbc: false, good: true, ...over,
});
const done = (over, r = {}) => ({ car: 'c1', state: 'done', id: '1', days: 30, phase: null, done: 0, total: 0, percent: 100,
  started_ms: 0, finished_ms: 1, error: null, result: result(r), current: null, applied: false, server_busy: null, ...over });

test('metres: whole metres, kilometres, and only "over 10 km" past that', () => {
  assert.equal(metres(4.4), '4 m');
  assert.equal(metres(1400), '1.4 km');
  assert.equal(metres(null), 'over 10 km');
});

test('none run yet: the button and two empty rows', () => {
  const m = fitModel({ state: 'idle' });
  assert.equal(m.button, BUTTON);
  assert.equal(m.rows.length, 2);
  assert.equal(m.canApply, false);
  assert.equal(fitModel(null).state, 'idle');
});

test('running: the step with its count, the percentage and what else the server does', () => {
  const m = fitModel({ state: 'running', phase: 'logs', done: 120, total: 400, percent: 48, server_busy: 'rebuild 34 %' });
  assert.equal(m.button, RUNNING);
  assert.equal(m.buttonDisabled, true);
  assert.equal(m.step, 'Reading the logs’ GPS frames: 120 of 400 logs');
  assert.equal(m.pct, 48);
  assert.equal(m.notes, 'The server is also busy: rebuild 34 %.');
  assert.equal(fitModel({ state: 'running', phase: 'queued', percent: 0 }).step, 'Waiting for another car’s fit');
});

test('waiting for a rebuild: its step and count kept, and why it waits', () => {
  const why = 'the full rebuild goes first (both read the same logs); this fit goes on from here once it ends';
  const m = fitModel({ state: 'running', phase: 'logs', done: 272, total: 22402, percent: 30, server_busy: 'rebuild 2 %', waiting: why });
  assert.equal(m.running, true);
  assert.equal(m.step, 'Reading the logs’ GPS frames: 272 of 22,402 logs');
  assert.equal(m.headline, 'Waiting (30 %)');
  assert.equal(m.notes, `Waiting: ${why}.`);
  assert.equal(fitModel({ state: 'running', phase: 'logs', done: 1, total: 2, percent: 60, waiting: null }).headline, 'Finding the layout (60 %)');
});

test('done: the best layouts with their errors, the pairs, and Apply', () => {
  const m = fitModel(done());
  assert.equal(m.button, AGAIN);
  assert.equal(m.tone, 'ok');
  assert.match(m.headline, /^Found a layout that fits: 2 m median, 4 m at the 95th percentile/);
  assert.deepEqual(m.rows[1], { label: 'Longitude', layout: '28|28@1- (1e-6,16.777216)', median: '1 m', p95: '3 m' });
  assert.equal(m.pairs, '3,000 frames from 10 logs paired with positions (local TeslaMate 600) over 30 days');
  assert.equal(m.canApply, true);
  assert.deepEqual(applyBody(done()), { layout: LAYOUT });
  // applied already: no Apply
  const a = fitModel(done({ applied: true, current: LAYOUT }));
  assert.equal(a.canApply, false);
  assert.equal(a.applied, true);
});

test('what was left out, a weak or far-off fit, the DBC’s own layout, too few pairs and a failure are said', () => {
  assert.match(fitModel(done({}, { days_skipped: 2, logs_skipped: 1 })).notes, /2 days of positions left out.*1 log left out/);
  const far = fitModel(done({}, { good: false, p95_m: 5200, weak: true }));
  assert.equal(far.tone, 'warn');
  assert.match(far.headline, /far off: .* 5.2 km at the 95th percentile/);
  assert.match(far.notes, /moved little/);
  const dbc = fitModel(done({}, { is_dbc: true, layout: 'lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,0)' }));
  assert.match(dbc.headline, /DBC’s own layout fits/);
  assert.deepEqual(applyBody(done({}, { is_dbc: true })), { layout: null });
  const few = fitModel(done({}, { enough: false, pairs: 5 }));
  assert.match(few.headline, /Too few pairs/);
  assert.equal(few.canApply, false);
  const failed = fitModel({ state: 'failed', error: 'listing the logs: gone', percent: 100 });
  assert.equal(failed.tone, 'error');
  assert.match(failed.headline, /listing the logs: gone/);
});

test('the controller starts, reads again while running and applies with the PUT', async () => {
  const calls = [];
  let reads = 0;
  const api = {
    get: async (p) => { calls.push(['GET', p]); reads += 1; return reads < 2 ? { state: 'running', percent: 50, phase: 'positions', done: 1, total: 2 } : done({ applied: calls.some((c) => c[0] === 'PUT') }); },
    post: async (p, b) => { calls.push(['POST', p, b]); return { state: 'running', percent: 0, phase: 'queued' }; },
    put: async (p, b) => { calls.push(['PUT', p, b]); return { layout: b.layout, job: '7' }; },
  };
  const seen = [];
  const c = gpsFit(api, 'c1', { onChange: (s) => seen.push(s.state) });
  await c.start();
  // the polls run on a timer: wait for the fit to end
  for (let i = 0; i < 100 && c.status().state !== 'done'; i++) await new Promise((r) => setTimeout(r, 50));
  c.stop();
  assert.equal(c.status().state, 'done');
  assert.deepEqual(calls[0], ['POST', fitPath('c1'), { days: 30 }]);
  await c.apply();
  assert.deepEqual(calls.find((x) => x[0] === 'PUT'), ['PUT', layoutPath('c1'), { layout: LAYOUT }]);
  assert.equal(c.status().applied, true);
  assert.ok(seen.includes('running') && seen.includes('done'));
});

test('the GPS layout line: standard or the car’s own with when it was set, and "…" until read', () => {
  const when = (ms) => `at ${ms}`;
  assert.deepEqual(layoutModel(null, when), { custom: false, value: '…', when: '', title: '' });
  const std = layoutModel({ car: 'c1', layout: null, set_ms: null }, when);
  assert.equal(std.value, STANDARD);
  assert.equal(std.custom, false);
  assert.equal(std.when, '');
  assert.equal(layoutModel({ layout: null, set_ms: 5 }, when).when, 'since at 5');
  const own = layoutModel({ layout: LAYOUT, set_ms: 9 }, when);
  assert.equal(own.custom, true);
  assert.equal(own.value, `custom: ${LAYOUT}`);
  assert.equal(own.when, 'set at 9');
  assert.match(own.title, /its own layout/);
});

test('the section is for cars with SMO (CAN) data: an SMO source or a CAN Bridge key', () => {
  assert.equal(hasCanData({ sources: [{ kind: 'smo' }] }, null), true);
  assert.equal(hasCanData({ sources: [{ kind: 'tesla_api' }] }, { keys: [] }), false);
  assert.equal(hasCanData({ sources: [] }, { keys: [{ key: 'k1' }] }), true);
  assert.equal(hasCanData(null, null), false);
});
