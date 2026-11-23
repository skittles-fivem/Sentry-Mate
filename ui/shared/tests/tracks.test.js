// Tests for tracks.js: the bounds a map fits leave out stray fixes (impossible jumps), never real driving.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dropJumps, trackBounds, lineOf, distanceM, MAX_JUMP_KPH } from '../tracks.js';

// a made-up drive: one fix a second, about 25 m apart (90 km/h), heading east
const drive = (n, lon0 = -100, lat = 40) => Array.from({ length: n }, (_, i) => [lon0 + i * 0.0003, lat]);
const secs = (n, t0 = 1_700_000_000_000) => Array.from({ length: n }, (_, i) => t0 + i * 1000);

test('distance', () => {
  assert.ok(Math.abs(distanceM([0, 0], [0, 1]) - 111195) < 10);
  assert.equal(distanceM([5, 5], [5, 5]), 0);
  assert.equal(MAX_JUMP_KPH, 400);
});

test('a stray fix in the middle or at either end is left out', () => {
  const c = drive(10), t = secs(10);
  assert.deepEqual(dropJumps(c, t), c, 'real driving stays');
  const mid = c.map((p) => p.slice()); mid[5] = [-120, 45];
  assert.deepEqual(dropJumps(mid, t), c.filter((_, i) => i !== 5));
  const first = c.map((p) => p.slice()); first[0] = [-80, 30];
  assert.deepEqual(dropJumps(first, t), c.slice(1));
  const last = c.map((p) => p.slice()); last[9] = [-80, 30];
  assert.deepEqual(dropJumps(last, t), c.slice(0, 9));
  // a fast but possible stretch (300 km/h) stays
  const quick = Array.from({ length: 5 }, (_, i) => [i * 0.00098, 0]);
  assert.deepEqual(dropJumps(quick, secs(5)).length, 5);
  // two lone fixes, or no times: nothing to judge by, both kept
  assert.equal(dropJumps([[0, 0], [10, 10]], secs(2)).length, 2);
  assert.equal(dropJumps(mid, null).length, 10);
  // a gap in time makes a long way possible (the car was driven while no fix came)
  assert.equal(dropJumps([[0, 0], [0, 1], [0, 1.0002]], [0, 3600000, 3601000]).length, 3);
  // holes in the line are skipped
  assert.equal(dropJumps([[0, 0], [null, null], [0, 0.0002]], secs(3)).length, 2);
});

test('the bounds a map fits', () => {
  const c = drive(10), t = secs(10);
  c[4] = [-120, 45];
  const b = trackBounds([{ coords: c, t_ms: t }]);
  assert.ok(b[0][0] > -100.01 && b[1][0] < -99.99 && b[0][1] === 40 && b[1][1] === 40, JSON.stringify(b));
  // /tracks drives and GeoJSON features, plus stops kept as they are
  const d = { lat: c.map((p) => p[1]), lon: c.map((p) => p[0]), t_ms: t };
  assert.deepEqual(trackBounds([d]), b);
  const f = { type: 'Feature', properties: { t_ms: t }, geometry: { type: 'LineString', coordinates: c } };
  assert.deepEqual(trackBounds([f]), b);
  assert.deepEqual(lineOf(d).coords, c);
  assert.deepEqual(trackBounds([f], [[-99, 41]])[1], [-99, 41]);
  assert.equal(trackBounds([], []), null);
});
