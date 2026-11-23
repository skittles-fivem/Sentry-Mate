// Tests for audit.js: every code the server writes (audit-codes.json) has a label and a group, every item
// kind reads as words, and unknown codes and kinds still show.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import * as A from '../audit.js';

const CODES = JSON.parse(readFileSync(new URL('../audit-codes.json', import.meta.url), 'utf8'));
const GROUP_IDS = A.AUDIT_GROUPS.map((g) => g.id);

// one sample target per item kind (contract: `<kind>:<id>`)
const SAMPLE = {
  car: 'car:c1', drive: 'drive:c1:d9', charge: 'charge:c1:ch3', update: 'update:c1:u2', trip: 'trip:t1', place: 'place:p1',
  import: 'import:i1', 'import-hold': 'import-hold:h1', export: 'export:e1', move: 'move:m1', account: 'account:a1', session: 'session:s1', key: 'key:c1:k1',
  token: 'token:c1:tk1', server: 'server:release', settings: 'settings:mqtt', tesla: 'tesla:ta1', teslafi: 'teslafi:tf1',
  teslamate: 'teslamate:local', 'ui-pack': 'ui-pack:horizon',
};
const CTX = {
  carName: (id) => ({ c1: 'BingBong' })[id],
  accountName: (id) => ({ a1: 'ada' })[id],
  trip: (id) => (id === 't1' ? { name: 'Alps', car: 'c1', from_ms: 1, to_ms: 2 } : undefined),
  packName: (id) => ({ horizon: 'Horizon' })[id],
};
const readable = (text, target) => {
  assert.equal(typeof text, 'string');
  assert.ok(text.length > 0);
  assert.ok(!/another item/i.test(text), `${target}: ${text}`);
  assert.ok(!text.includes(target), `${target} shows its raw key: ${text}`);
};

test('the code list is sorted, unique and uses known groups', () => {
  const codes = CODES.map((c) => c.code);
  assert.deepEqual(codes, [...codes].sort());
  assert.equal(new Set(codes).size, codes.length);
  for (const c of CODES) {
    assert.ok(GROUP_IDS.includes(c.group), `${c.code}: group ${c.group}`);
    assert.ok(c.item === null || Object.hasOwn(SAMPLE, c.item), `${c.code}: item kind ${c.item} has no sample`);
  }
});

test('every code has a label and the group the list gives it', () => {
  for (const { code, group } of CODES) {
    assert.ok(Object.hasOwn(A.AUDIT_ACTIONS, code), `${code} has no label`);
    assert.ok(!A.actionLabel(code).startsWith('Other'), code);
    assert.equal(A.actionGroup(code), group, code);
  }
  // and audit.js names no code the server does not write
  for (const code of Object.keys(A.AUDIT_ACTIONS)) assert.ok(CODES.some((c) => c.code === code), `${code} is not in audit-codes.json`);
});

test('labels in plain words', () => {
  assert.equal(A.actionLabel('sign-in'), 'Signed in');
  assert.equal(A.actionLabel('sign-in-failed'), 'Wrong password');
  assert.equal(A.actionLabel('sign-out'), 'Signed out');
  assert.equal(A.actionLabel('preferences-changed'), 'Changed preferences');
  assert.equal(A.actionLabel('ui-pack-chosen'), 'Chose a design');
  assert.equal(A.actionLabel('teslamate-local-car-matched'), 'Matched a TeslaMate car');
  assert.equal(A.actionLabel('settings-changed'), 'Changed the server settings');
  assert.equal(A.actionLabel('charge-cost-entered'), 'Entered a charge cost');
  assert.equal(A.actionLabel('release-checked'), 'Checked for a newer build');
  assert.equal(A.actionLabel('change'), 'Changed something');
  for (const label of Object.values(A.AUDIT_ACTIONS)) assert.ok(!/[a-z]-[a-z]+-/.test(label) && /^[A-Z]/.test(label), label);
});

test('groups', () => {
  assert.deepEqual(GROUP_IDS, ['sign-ins', 'accounts', 'settings', 'cars', 'integrations', 'imports']);
  assert.equal(A.groupLabel('sign-ins'), 'Sign-ins');
  assert.equal(A.groupLabel('imports'), 'Imports and moves');
  assert.equal(A.groupLabel('nope'), 'Other');
});

test('unknown code still shows', () => {
  assert.equal(A.actionLabel('laser-fired'), 'Other: laser fired');
  assert.equal(A.actionGroup('laser-fired'), 'other');
  assert.equal(A.actionLabel(''), 'Other');
  assert.equal(A.actionLabel(undefined), 'Other');
});

test('every item kind reads as words, with and without context', () => {
  for (const [kind, target] of Object.entries(SAMPLE)) {
    for (const ctx of [CTX, {}]) {
      const it = A.auditItem({ target, car: target.split(':').length > 2 ? 'c1' : null }, ctx);
      assert.ok(it, kind);
      readable(it.text, target);
      assert.ok(it.route === null || (typeof it.route[0] === 'string' && typeof it.route[1] === 'object'), kind);
      assert.ok(it.hash === null || it.hash.startsWith('#'), kind);
    }
  }
});

test('items name the thing and link to it', () => {
  assert.deepEqual(A.auditItem({ target: 'teslamate:local' }, CTX), { text: 'Local TeslaMate', route: ['settings-integrations', {}], hash: '#teslamate', faint: false });
  assert.equal(A.auditItem({ target: 'settings:teslamate_local' }, CTX).text, 'Local TeslaMate');
  assert.deepEqual(A.auditItem({ target: 'ui-pack:horizon' }, CTX), { text: 'The Horizon design', route: ['settings-display', {}], hash: null, faint: false });
  assert.equal(A.auditItem({ target: 'ui-pack:pebble' }, {}).text, 'The Pebble design');
  assert.deepEqual(A.auditItem({ target: 'server:release' }, CTX).route, ['settings-server', {}]);
  assert.equal(A.auditItem({ target: 'server:release' }, CTX).text, 'Software release');
  assert.deepEqual(A.auditItem({ target: 'drive:c1:d9', car: 'c1' }, CTX), { text: 'A drive of BingBong', route: ['drive', { car: 'c1', drive: 'd9' }], hash: null, faint: false });
  assert.equal(A.auditItem({ target: 'tesla:ta1' }, CTX).hash, '#tesla-ta1');
  assert.equal(A.auditItem({ target: 'teslafi:tf1:v2' }, CTX).hash, '#teslafi-tf1');
  assert.equal(A.auditItem({ target: 'account:a1' }, CTX).text, 'Account ada');
  assert.equal(A.auditItem({ target: 'trip:t1' }, CTX).text, 'Trip Alps');
  // a car match: target the integration's item, car the matched car
  assert.equal(A.auditItem({ target: 'teslamate:local', car: 'c1' }, CTX).text, 'Local TeslaMate');
  // a car no longer here
  const gone = A.auditItem({ target: 'car:zz' }, CTX);
  assert.equal(gone.faint, true);
  assert.equal(gone.route, null);
});

test('no target, or an unknown kind', () => {
  assert.equal(A.auditItem({ target: null }, CTX), null);
  assert.equal(A.auditItem({ target: '' }, CTX), null);
  assert.deepEqual(A.auditItem({ target: 'gizmo:42' }, CTX), { text: 'Something else', route: null, hash: null, faint: true });
  assert.equal(A.auditItem({ target: 'gizmo' }, CTX).text, 'Something else');
  // a throwing context does not break a row
  assert.equal(A.auditItem({ target: 'car:c1' }, { carName: () => { throw new Error('x'); } }).text, 'A car no longer here');
});
