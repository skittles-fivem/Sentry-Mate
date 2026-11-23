// Tests for devices.js: what a CAN Bridge key is called, and the name rule.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as D from '../devices.js';
import { setLocale } from '../i18n.js';

test('a chosen name wins, trimmed', () => {
  setLocale('en-US');
  assert.deepEqual(D.deviceName({ key: 'cbk_8d41c0a2', device: '  Screenmate in the car ', device_label: 'x' }),
    { text: 'Screenmate in the car', named: true });
});

test('unnamed: the server label, else the automatic one', () => {
  setLocale('en-US');
  assert.deepEqual(D.deviceName({ key: 'cbk_8d41c0a2', device: null, device_label: 'CAN Bridge key 8d41c0' }),
    { text: 'CAN Bridge key 8d41c0', named: false });
  assert.deepEqual(D.deviceName({ key: 'cbk_8d41c0a2' }), { text: 'CAN Bridge key 8d41c0', named: false });
  assert.deepEqual(D.deviceName({ key: 'abc', device: '   ' }), { text: 'CAN Bridge key abc', named: false });
  assert.equal(D.autoDeviceLabel(''), 'CAN Bridge key');
  assert.equal(D.shortKeyId('0123456789ab'), '012345');
});

test('name rule: at most 60 characters, empty clears', () => {
  setLocale('en-US');
  assert.equal(D.checkDeviceName(''), null);
  assert.equal(D.checkDeviceName('a'.repeat(60)), null);
  assert.equal(D.checkDeviceName(` ${'a'.repeat(60)} `), null);
  assert.match(D.checkDeviceName('a'.repeat(61)), /60/);
});

test('renameKey sends the trimmed name, or null to clear', async () => {
  const sent = [];
  const client = { patch: async (path, body) => { sent.push([path, body]); return { key: 'k/1', device: body.name }; } };
  await D.renameKey(client, 'k/1', '  Pi logger ');
  await D.renameKey(client, 'k/1', '   ');
  assert.deepEqual(sent, [['can-keys/k%2F1', { name: 'Pi logger' }], ['can-keys/k%2F1', { name: null }]]);
});
