// Tests for tyres.js: the tyres against the recommended pressures, mapped or not, and the line under them.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as T from '../tyres.js';
import { setUnits } from '../units.js';
import { setLocale } from '../i18n.js';

const sp = (s) => String(s).replace(/[   ]/g, ' ');
const W = (value) => ({ value, at_ms: 1, sources: [] });
const now = (o) => ({ tyres_kpa: [290, 288, 285, 287].map(W), tyres_slots_mapped: true, ...o });

// (set in each test: the other files' beforeEach hooks run for every test too)
const psi = () => {
  setLocale('en-US');
  setUnits({ distance: 'km', temperature: 'c', pressure: 'psi', range: 'rated' });
};

test('mapped: each tyre against its own axle', () => {
  const v = T.tyreView(now({ tyre_recommended_kpa: W(250), tyre_recommended_rear_kpa: W(320), tyre_recommended_source: 'can',
    tyres_kpa: [250, 250, 300, 280].map(W) }));
  assert.deepEqual(v.against, [250, 250, 320, 320]);
  assert.deepEqual(v.low, [false, false, false, true], '280 is under 90 % of 320');
  assert.equal(v.anyLow, true);
});

test('not mapped: every tyre against the lower axle, so no false alarm', () => {
  psi();
  const v = T.tyreView(now({ tyres_slots_mapped: false, tyre_recommended_kpa: W(250), tyre_recommended_rear_kpa: W(320),
    tyre_recommended_source: 'can', tyres_kpa: [250, 250, 300, 280].map(W) }));
  assert.deepEqual(v.against, [250, 250, 250, 250]);
  assert.deepEqual(v.low, [false, false, false, false]);
  assert.match(sp(T.recommendedLine({ ...now({}), tyres_slots_mapped: false, tyre_recommended_kpa: W(250), tyre_recommended_rear_kpa: W(320), tyre_recommended_source: 'can' })),
    /^Recommended 36 psi front, 46 psi rear \(from CAN\)\.$/);
  assert.match(sp(T.tyresTitle({ ...now({}), tyres_slots_mapped: false, tyre_recommended_kpa: W(250), tyre_recommended_rear_kpa: W(320), tyre_recommended_source: 'can' })),
    /flagged; a cold morning .* Which wheel each sensor sits on is not known yet, so each tyre is judged against the lower one\.$/);
});

test('the server’s word on low wins', () => {
  const v = T.tyreView(now({ tyre_recommended_kpa: W(290), tyre_low: W(false), tyres_kpa: [200, 290, 290, 290].map(W) }));
  assert.equal(v.low[0], true);
  assert.equal(v.anyLow, false);
});

test('the line says where the recommended pressure comes from', () => {
  psi();
  const line = (o) => sp(T.recommendedLine(now(o)));
  assert.equal(line({ tyre_recommended_kpa: W(290), tyre_recommended_rear_kpa: W(290), tyre_recommended_source: 'can' }), 'Recommended 42 psi (from CAN).');
  assert.equal(line({ tyre_recommended_kpa: { value: 290, at_ms: null, sources: [] }, tyre_recommended_source: 'placard', tyre_recommended_wheel_in: 20 }),
    'Recommended 42 psi (from the door placard for 20″ wheels).');
  assert.equal(line({ tyre_recommended_kpa: W(276), tyre_recommended_source: 'owner' }), 'Recommended 40 psi (typed).');
  assert.equal(line({}), 'No recommended pressure known.');
  assert.equal(sp(T.recommendedLine(now({ tyres_slots_mapped: false }))), 'No recommended pressure known.');
  assert.equal(T.tyresTitle(now({ tyres_slots_mapped: false })), 'Which wheel each sensor sits on is not known yet.');
  assert.equal(T.tyresTitle(now({})), '');
  assert.equal(T.recommendedLine({ tyres_kpa: [null, null, null, null], tyres_slots_mapped: false }), 'No recommended pressure known.');
});

test('a placard wheel size that is only a guess says so and where to set it', () => {
  psi();
  const P = { value: 290, at_ms: null, sources: [] };
  const placard = (o) => now({ tyres_slots_mapped: false, tyre_recommended_kpa: P, tyre_recommended_rear_kpa: P, tyre_recommended_source: 'placard',
    tyre_recommended_wheel_in: 21, ...o });
  assert.equal(sp(T.recommendedLine(placard({}))), 'Recommended 42 psi (from the door placard for 21″ wheels).');
  assert.deepEqual(T.wheelGuessLine(placard({ tyre_recommended_wheel_from: 'can', tyre_recommended_wheel_guess: true })),
    { said: 'Wheel size taken from the factory tyre.', ask: 'Not 21″?', action: 'Set the wheel size' });
  assert.equal(T.wheelGuessLine(placard({ tyre_recommended_wheel_from: 'vin', tyre_recommended_wheel_guess: true })).said, 'Wheel size guessed from the VIN.');
  assert.equal(T.wheelGuessLine(placard({ tyre_recommended_wheel_from: 'owner', tyre_recommended_wheel_guess: false })), null);
  assert.equal(T.wheelGuessLine(placard({})), null, 'an older server says nothing of a guess');
  // a size the server sends with a pressure that is not the placard's is not shown
  assert.equal(T.wheelGuessLine(now({ tyre_recommended_kpa: W(290), tyre_recommended_source: 'can', tyre_recommended_wheel_in: 21, tyre_recommended_wheel_guess: true })), null);
});
