// Tests for teslafi-data.js: when the car page says TeslaFi has had no new data, when it says its times are read in
// the wrong zone, and when it says nothing (current data, a sleeping car).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as T from '../teslafi-data.js';

const NOW = 1_800_000_000_000;
const MIN = 60_000, HOUR = 3_600_000;
const input = (last, received) => [{ kind: 'smo', last_ms: NOW - 5000 }, { kind: 'teslafi_api', label: 'TeslaFi', last_ms: last, received_ms: received }];

test('spans read in words', () => {
  assert.equal(T.spanText(45 * MIN), '45 min');
  assert.equal(T.spanText(6 * HOUR), '6 h');
  assert.equal(T.spanText(2 * HOUR + 30 * MIN), '2 h 30 min');
  assert.equal(T.spanText(13 * HOUR + 20 * MIN), '13 h');
  assert.equal(T.spanText(72 * HOUR), '3 days');
});

test('data that came 6 h ago and nothing since says TeslaFi has had no new data, unless the car sleeps', () => {
  const n = T.dataNote(input(NOW - 6 * HOUR, NOW - 6 * HOUR + MIN), 'parked', NOW);
  assert.equal(n.kind, 'no_data');
  assert.equal(n.ms, 6 * HOUR - MIN);
  assert.match(n.text, /TeslaFi has had no new data from the car for 5 h 59 min/);
  assert.match(n.text, /problem is on TeslaFi's side/);
  assert.equal(T.dataNote(input(NOW - 6 * HOUR, NOW - 6 * HOUR + MIN), 'asleep', NOW), null, 'asleep is silent as expected');
  // under two hours: nothing yet
  assert.equal(T.dataNote(input(NOW - 100 * MIN, NOW - 100 * MIN), 'parked', NOW), null);
  // no `received_ms` (an older server): TeslaFi's own time stands for it
  assert.equal(T.dataNote(input(NOW - 3 * HOUR, undefined), 'driving', NOW)?.kind, 'no_data');
});

test('data that is still coming but stamped hours off says the zone is wrong', () => {
  const behind = T.dataNote(input(NOW - MIN - 6 * HOUR, NOW - MIN), 'parked', NOW);
  assert.equal(behind.kind, 'time_offset');
  assert.equal(behind.ms, 6 * HOUR);
  assert.match(behind.text, /stamped 6 h before the moment this server read it/);
  assert.match(behind.text, /time zone/);
  const ahead = T.dataNote(input(NOW - MIN + 5 * HOUR, NOW - MIN), 'asleep', NOW);
  assert.equal(ahead.kind, 'time_offset', 'a sleeping car with data still coming is judged the same');
  assert.match(ahead.text, /5 h after/);
  // minutes apart is TeslaFi's delay
  assert.equal(T.dataNote(input(NOW - 21 * MIN, NOW - MIN), 'parked', NOW), null);
});

test('current data, no TeslaFi, or no data yet say nothing', () => {
  assert.equal(T.dataNote(input(NOW - MIN, NOW - MIN), 'driving', NOW), null);
  assert.equal(T.dataNote([{ kind: 'smo', last_ms: NOW }], 'driving', NOW), null);
  assert.equal(T.dataNote(input(null, null), 'parked', NOW), null);
  assert.equal(T.dataNote(undefined, 'parked', NOW), null);
});
