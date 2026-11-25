import { test } from 'node:test';
import assert from 'node:assert/strict';
import './fake-browser.js';
import { rowModel, changeModel, fitsPath, applyPath, changesPath } from '../gpsfits.js';
import { fitModel } from '../gpsfit.js';
import { rowText, lines, whenShown, cancelling } from '../fullrebuild.js';

const when = (ms) => `t${ms}`;
const done = (over) => ({ id: '7', state: 'done', days: 30, percent: 100, started_ms: 10, logs: 412, pairs: 18240, layout: 'lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,0)', median_m: 5, p95_m: 14, is_dbc: false, enough: true, weak: false, good: true, applied: false, applied_ms: null, applied_by: null, ...over });

test('a past fit row says when, the counts, the best layout with its errors, good or weak, and whether applied', () => {
  const r = rowModel(done(), when);
  assert.deepEqual([r.when, r.counts, r.verdict, r.tone], ['t10', '30 days · 412 logs · 18,240 pairs', 'Good', 'ok']);
  assert.equal(r.err, '5 m median · 14 m 95th pct');
  assert.deepEqual([r.canApply, r.canDelete, r.canOpen], [true, true, true]);
  assert.equal(rowModel(done({ weak: true }), when).verdict, 'Weak');
  assert.equal(rowModel(done({ good: false }), when).verdict, 'Far off');
  assert.equal(rowModel(done({ enough: false, layout: null }), when).verdict, 'Too few pairs');
  assert.equal(rowModel(done({ layout: null }), when).verdict, 'No layout found');
  assert.equal(rowModel(done({ state: 'failed', layout: null }), when).verdict, 'Failed');
  const a = rowModel(done({ applied: true, applied_ms: 99, applied_by: 'admin' }), when);
  assert.deepEqual([a.applied, a.canApply], ['Applied by admin t99', false]);
  const run = rowModel({ id: '8', state: 'running', percent: 3, days: 30, started_ms: 20 }, when);
  assert.deepEqual([run.verdict, run.counts, run.canApply, run.canDelete, run.canOpen], ['Running (3 %)', '', false, false, false]);
  assert.equal(rowModel(done({ p95_m: null }), when).err, '5 m median · over 10 km 95th pct');
});

test('the layouts used over time say when, by whom and the layout before', () => {
  const c = changeModel({ at_ms: 5, by: 'admin', layout: null, before: 'lat 0|27@1- (2e-6,0); lon 28|28@1- (1e-6,0)', fit: null }, when);
  assert.deepEqual([c.when, c.by, c.now, c.before, c.fromFit], ['t5', 'admin', 'standard (DBC)', 'before: lat 0|27@1- (2e-6,0); lon 28|28@1- (1e-6,0)', false]);
  assert.equal(changeModel({ at_ms: 5, layout: 'x', before: null, fit: '3' }, when).before, 'before: standard (DBC)');
  assert.deepEqual([fitsPath('c1'), applyPath('c1', '7'), changesPath('c1')], ['cars/c1/gps-layout/fits', 'cars/c1/gps-layout/fits/7/apply', 'cars/c1/gps-layout/changes']);
});

test('the fit shows a time left only once the server has a speed', () => {
  const s = { state: 'running', phase: 'logs', done: 272, total: 22402, percent: 1, eta_ms: 4_740_000, server_busy: null };
  assert.match(fitModel(s).eta, /1 h 19 min left/);
  assert.equal(fitModel({ ...s, eta_ms: null }).eta, '');
  assert.equal(fitModel({ ...s, phase: 'positions', eta_ms: null }).eta, '');
});

const paused = { state: 'running', phase: 'decoding', cars_total: 1, cars_done: 0, car: 'c1', this_car: true, hold: { kind: 'paused', until_ms: null }, logs: { done: 5, total: 10 }, rate_logs_per_s: null, eta_ms: null, last_progress_ms: 1, cancelling: false };

test('a paused full rebuild is one state in the row and in the lines', () => {
  assert.equal(rowText(paused, 1000).text, 'Full rebuild paused: starts when you start it');
  const l = Object.fromEntries(lines(paused, 1000).map((x) => [x.key, x.text]));
  assert.equal(l.state, 'Paused: starts when you start it');
  assert.equal(l.phase, '');
  assert.equal(l.rate, '');
  assert.equal(rowText({ ...paused, hold: null }, 1000).text, 'Running: Decoding the stored logs');
  assert.equal(rowText({ state: 'done' }), null);
});

test('a cancel asked for is said everywhere, and nothing else is offered', () => {
  const c = { ...paused, hold: null, cancelling: true };
  assert.equal(cancelling(c), true);
  assert.equal(rowText(c, 1000).text, 'Cancelling the full rebuild');
  assert.equal(lines(c, 1000)[0].text, 'Cancelling: the old history is kept');
  assert.equal(lines(c, 1000)[1].text, '');
  for (const w of ['now', 'tonight', 'pause']) assert.equal(whenShown(c, w), false);
});
