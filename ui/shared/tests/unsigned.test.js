// Tests for unsigned.js: the kinds in plain words, a log used on the owner's word, the counts, the confirm text and
// the request body.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as U from '../unsigned.js';

const summary = {
  can_use: 11, can_use_before_first_signed: 9, in_use: 0, never: 2,
  kinds: { no_integrity: 7, no_sidecar: 1, signed_false: 2, could_not_sign: 1 },
};

test('each kind reads in plain words, a log the bridge could not sign with its reason', () => {
  assert.equal(U.kindText({ unsigned_kind: 'no_integrity' }), 'made before the CAN Bridge app signed logs');
  assert.equal(U.kindText({ unsigned_kind: 'no_sidecar' }), 'no sidecar came with it');
  assert.equal(U.kindText({ unsigned_kind: 'signed_false' }), 'the app sent it marked as not signed');
  assert.equal(U.kindText({ unsigned_kind: 'could_not_sign', reason: 'the CAN Bridge could not sign it (keystore was busy)' }),
    'the app tried to sign it and could not: keystore was busy');
  assert.match(U.kindText({ unsigned_kind: 'unknown' }), /older version/);
  assert.equal(U.kindText({ unsigned_kind: null, result: 'failed' }), '');
  assert.equal(U.statusText({ unsigned_kind: 'no_integrity', override_use: true }), 'unsigned, used by the owner');
  assert.equal(U.statusText({ unsigned_kind: 'no_integrity', result: 'failed' }), 'unsigned, not used');
});

test('the counts and the confirm text say how many of which kinds, and that bad ones are never included', () => {
  assert.equal(U.kindsText(summary.kinds),
    '7 made before the CAN Bridge app signed logs, 1 with no sidecar, 2 marked as not signed by the app, 1 the app could not sign');
  assert.match(U.summaryText(summary), /^11 logs failed their check only for having no signature and are not used/);
  assert.match(U.summaryText(summary), /2 logs failed for something else .* never used/);
  assert.equal(U.summaryText({ can_use: 0, in_use: 0 }), '');
  const all = U.confirmText(summary, 'Comet');
  assert.match(all[0], /^Use 11 logs of Comet .*\(7 made before/);
  assert.match(all[1], /never included \(2 logs of this car\)/);
  assert.match(all[2], /upload token/);
  assert.match(U.confirmText(summary, 'Comet', true)[0], /^Use 9 logs of Comet .*all from before its first signed log/);
  const c = U.choice(summary);
  assert.equal(c.label, 'Only the 9 logs from before this car’s first signed log');
  assert.equal(c.value, true);
  assert.equal(U.choice({ ...summary, can_use_before_first_signed: 0 }), null);
});

test('the body asks the password again, all of them or only those before the first signed log', () => {
  assert.deepEqual(U.body({ password: 'pw' }), { all: true, confirm_password: 'pw' });
  assert.deepEqual(U.body({ password: 'pw', code: '123456', onlyBefore: true }),
    { all: true, before_first_signed: true, confirm_password: 'pw', code: '123456' });
  assert.deepEqual(U.body({ password: 'pw', names: ['a.log.xz'], onlyBefore: true }), { names: ['a.log.xz'], confirm_password: 'pw' });
  assert.match(U.doneText({ changed: 9 }, true), /^Using 9 logs/);
  assert.match(U.doneText({ changed: 0 }, false), /none were in use/);
});
