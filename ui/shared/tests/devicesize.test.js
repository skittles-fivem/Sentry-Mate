// Tests for devicesize.js: the Interface size kept in this browser only.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import * as D from '../devicesize.js';

const sp = (s) => String(s).replace(/ /g, ' ');
const memory = () => {
  const m = new Map();
  return { getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, String(v)), removeItem: (k) => m.delete(k), m };
};
const blocked = { getItem() { throw new Error('SecurityError'); }, setItem() { throw new Error('SecurityError'); }, removeItem() { throw new Error('SecurityError'); } };

test('only the offered steps are kept; anything else is Automatic', () => {
  assert.deepEqual([...D.DEVICE_SIZES], [80, 90, 100, 110, 125]);
  for (const v of [80, '90', 100.2, 110, 125]) assert.ok(D.normalDeviceSize(v) != null, String(v));
  for (const v of [null, undefined, '', 'auto', 75, 150, 'big', NaN, Infinity]) assert.equal(D.normalDeviceSize(v), null, String(v));
});

test('write then read; Automatic removes the key', () => {
  const s = memory();
  assert.equal(D.readDeviceSize(s), null, 'nothing kept: Automatic');
  assert.equal(D.writeDeviceSize(110, s), 110);
  assert.equal(s.m.get(D.DEVICE_SIZE_KEY), '110');
  assert.equal(D.readDeviceSize(s), 110);
  assert.equal(D.writeDeviceSize('auto', s), null);
  assert.equal(s.m.has(D.DEVICE_SIZE_KEY), false);
  s.setItem(D.DEVICE_SIZE_KEY, '{not json');
  assert.equal(D.readDeviceSize(s), null, 'a broken value is Automatic');
  s.setItem(D.DEVICE_SIZE_KEY, '300');
  assert.equal(D.readDeviceSize(s), null, 'a value out of the steps is Automatic');
});

test('blocked or missing storage: no throw, Automatic, the picked size still returned', () => {
  assert.equal(D.readDeviceSize(blocked), null);
  assert.equal(D.writeDeviceSize(90, blocked), 90);
  assert.equal(D.readDeviceSize(null), null);
  assert.equal(D.writeDeviceSize(90, null), 90);
});

test('labels and notes say it is this device only', () => {
  assert.equal(D.deviceSizeLabel(null), 'Automatic');
  assert.equal(sp(D.deviceSizeLabel(125)), '125 %');
  assert.deepEqual(D.deviceSizeOptions().map((o) => o.value), ['auto', '80', '90', '100', '110', '125']);
  assert.match(sp(D.deviceSizeNote(90)), /90 % of the standard size.*Only this browser on this device/);
  assert.match(sp(D.deviceSizeNote(null, { account: 100, shown: 121 })), /account’s Interface size \(100 %, 121 % on this screen\)/);
  assert.match(sp(D.deviceSizeNote(null, { account: 100, shown: 100 })), /\(100 %\)\./);
});

test('another tab: the storage event for the key calls back', () => {
  const handlers = [];
  const target = { addEventListener: (t, f) => handlers.push(f), removeEventListener: (t, f) => handlers.splice(handlers.indexOf(f), 1) };
  const got = [];
  const off = D.onDeviceSizeStorage((v) => got.push(v), target);
  handlers[0]({ key: 'other' });
  handlers[0]({ key: D.DEVICE_SIZE_KEY });
  assert.equal(got.length, 1);
  off();
  assert.equal(handlers.length, 0);
});

test('every pack reads the same key in boot.js before the first paint', () => {
  const packs = join(dirname(fileURLToPath(import.meta.url)), '..', '..', 'packs');
  for (const p of readdirSync(packs)) {
    let src;
    try { src = readFileSync(join(packs, p, 'web', 'boot.js'), 'utf8'); } catch { continue; }
    assert.ok(src.includes(`'${D.DEVICE_SIZE_KEY}'`), `${p}/web/boot.js reads ${D.DEVICE_SIZE_KEY}`);
    for (const n of D.DEVICE_SIZES) assert.ok(src.includes(String(n)), `${p}/web/boot.js knows ${n}`);
  }
});
