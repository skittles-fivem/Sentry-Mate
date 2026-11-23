// Tests for rehome.js: an import's moved rows and the rows that wait for a car, in words.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as R from '../rehome.js';

const job = {
  moves: [
    { kind: 'rehomed', to_car: 'c2', to_name: 'Maple', rows: 1000 },
    { kind: 'rehomed', to_car: 'c2', to_name: 'Maple', rows: 840 },
    { kind: 'rehomed', to_car: null, hold: 'h1', rows: 620 },
  ],
  needs_car: [{ hold: 'h1', status: 'waiting', rows: 620 }, { hold: 'h2', status: 'ignored', rows: 40 }, { hold: 'h3', status: 'assigned', rows: 9 }],
};

test('one line per import: moved to a car, waiting, left aside', () => {
  assert.equal(R.moveSummary(job), '1840 rows moved to Maple; 620 rows need a car; 40 rows left aside');
  assert.equal(R.moveSummary({ moves: [], needs_car: [] }), '');
  assert.equal(R.moveSummary(null), '');
  assert.equal(R.moveSummary({ moves: [{ to_car: 'c5', rows: 3 }] }, (c) => `car ${c}`), '3 rows moved to car c5');
});

test('the lots still without a car, waiting first', () => {
  assert.deepEqual(R.waiting(job.needs_car).map((h) => h.hold), ['h1', 'h2']);
  assert.deepEqual(R.waiting(undefined), []);
});

test('why a lot waits, and what a choice did', () => {
  assert.match(R.holdWhy({ why: 'odometer' }), /odometer/);
  assert.match(R.holdWhy({ why: 'no_vin' }), /without a VIN/);
  assert.equal(R.holdSaid({ move: { rows: 5, to_name: 'Maple' } }), '5 rows moved to Maple; it is being rebuilt.');
  assert.match(R.holdSaid({ hold: { status: 'ignored' }, move: null }), /Left aside/);
  assert.match(R.holdSaid({ hold: { status: 'waiting' }, move: null }), /wait/);
});
