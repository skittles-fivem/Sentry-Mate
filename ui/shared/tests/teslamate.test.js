// Tests for teslamate.js: the drafts and the server's rules (docs/ui-api.md, "Integrations: Local TeslaMate").
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { tmDraft, tmCheck, tmPutBody, tmMatchChanges, tmPreselect } from '../teslamate.js';

const mqtt = (v) => tmDraft('mqtt', { host: '192.0.2.10', port: '1883', tls: false, prefix: 'teslamate', ...v });

test('a password over plain TCP needs the tick; none needs nothing', () => {
  assert.deepEqual(tmCheck('mqtt', mqtt({}), null), {});
  assert.ok(tmCheck('mqtt', mqtt({ password: 'x' }), null)['mqtt.password']);
  assert.deepEqual(tmCheck('mqtt', mqtt({ password: 'x', allow_unencrypted: true }), null), {});
  assert.deepEqual(tmCheck('mqtt', mqtt({ password: 'x', tls: true, port: 8883 }), null), {});
  // a kept password counts too
  const saved = { host: '192.0.2.10', port: 1883, tls: false, password_set: true };
  assert.ok(tmCheck('mqtt', mqtt({}), saved)['mqtt.password']);
  assert.deepEqual(tmCheck('mqtt', mqtt({ allow_unencrypted: true }), saved), {});
  assert.deepEqual(tmCheck('mqtt', mqtt({ password_remove: true }), saved), {});
});

test('a kept password goes only to its host; the pin, the prefix and the host are checked', () => {
  const saved = { host: '192.0.2.10', port: 1883, tls: false, password_set: true };
  assert.match(tmCheck('mqtt', mqtt({ host: '192.0.2.11', allow_unencrypted: true }), saved)['mqtt.password'], /type it again/);
  assert.equal(mqtt({ pin_sha256: 'ab'.repeat(32) }).pin_sha256, null); // no pin without TLS
  for (const p of ['tm/+', '#', '$SYS', 'a//b']) assert.ok(tmCheck('mqtt', mqtt({ prefix: p }), null)['mqtt.prefix'], p);
  assert.ok(tmCheck('mqtt', mqtt({ host: 'mqtt://192.0.2.10' }), null)['mqtt.host']);
  const db = tmDraft('database', { enabled: true, host: '', port: '5432', database: 'teslamate', interval_s: '30' });
  const bad = tmCheck('database', db, null);
  assert.ok(bad['database.host'] && bad['database.interval_s']);
});

test('the PUT body and the match changes', () => {
  const t = { enabled: true, version: 4, mqtt: { host: '192.0.2.10', port: 1883, password_set: true }, database: { enabled: false } };
  const b = tmPutBody(t, 'mqtt', mqtt({}));
  assert.equal(b.version, 4);
  assert.ok(!('password' in b.database) && !('password_set' in b.database));
  assert.equal(tmPutBody({ enabled: false, version: 0 }, 'mqtt', mqtt({})).version, 0);
  const items = [{ teslamate_car: '1', match: null, suggested: 'c2' }, { teslamate_car: '2', match: 'leave_out' }];
  const chosen = new Map(items.map((c) => [c.teslamate_car, tmPreselect(c)]));
  assert.deepEqual(tmMatchChanges(items, chosen), [{ teslamate_car: '1', match: 'c2' }]);
});

test('the local TeslaMate notices link to its section of Settings: Integrations', async () => {
  const { TM_NOTICE_CODES, isTmNotice, tmNoticeFix, TM_NOTICE_ACTION } = await import('../teslamate.js');
  assert.equal(TM_NOTICE_CODES.length, 5);
  for (const code of TM_NOTICE_CODES) assert.ok(isTmNotice({ code }), code);
  assert.ok(!isTmNotice({ code: 'teslafi_account_error' }) && !isTmNotice(null));
  assert.equal(tmNoticeFix({ code: 'teslamate_quiet', fix_link: '/settings/integrations#teslamate' }), '/settings/integrations#teslamate');
  assert.equal(tmNoticeFix({ code: 'teslamate_quiet', fix_link: null }, '/x/settings/integrations?car=c1'), '/x/settings/integrations?car=c1#teslamate');
  assert.equal(tmNoticeFix({ code: 'teslamate_auth_failed', fix_link: '/settings/integrations#other' }), '/settings/integrations#teslamate');
  assert.match(TM_NOTICE_ACTION, /TeslaMate/);
});
