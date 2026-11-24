// Tests for storage.js: the Storage panel's groups, their shares and the categories' short names.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as S from '../storage.js';
import { setLocale } from '../i18n.js';

const cat = (key, group, disk_bytes, label = `${key} (long label)`) => ({ key, group, label, files: 2, bytes: disk_bytes, disk_bytes });

test('short names from the key, the label when the key is not known', () => {
  assert.equal(S.shortName(cat('series.teslamate.snapshots', 'series', 1)), 'TeslaMate import: day series');
  assert.equal(S.shortName(cat('series.teslamate.journals', 'series', 1)), 'TeslaMate import: not yet folded');
  assert.equal(S.shortName(cat('curves.can.packs', 'series', 1)), 'CAN logs: charge curves, packed');
  assert.equal(S.shortName(cat('logs.raw', 'logs', 1)), 'Signed originals');
  assert.equal(S.shortName(cat('kv.weather', 'kv', 1, 'Kept answers and records: weather')), 'Kept answers and records: weather');
  assert.equal(S.shortName(cat('new.thing', 'server', 1, 'A new thing')), 'A new thing');
  assert.equal(S.shortName(cat('series.solar.snapshots', 'series', 1)), 'solar: day series');
});

test('groups largest first, each row largest first, shares of the whole', () => {
  const g = S.storageGroups([cat('server.audit', 'server', 10), cat('logs.raw', 'logs', 600), cat('logs.packs', 'logs', 200),
    cat('series.can.snapshots', 'series', 190), cat('odd.one', 'weird', 0)]);
  assert.deepEqual(g.map((x) => x.group), ['logs', 'series', 'server', 'weird']);
  assert.equal(g[0].name, 'CAN logs (signed originals)');
  assert.equal(g[0].disk_bytes, 800);
  assert.equal(g[0].files, 4);
  assert.deepEqual(g[0].rows.map((r) => r.key), ['logs.raw', 'logs.packs']);
  assert.equal(g[0].share, 0.8);
  assert.equal(g[3].name, 'weird', 'a group this page does not know keeps its word');
  assert.equal(S.groupWord('logs'), 'CAN logs');
  assert.deepEqual(S.storageGroups([]), []);
  assert.deepEqual(S.storageGroups(null), []);
});

test('shares as percent, a sliver as under one', () => {
  setLocale('en-US');
  const sp = (s) => s.replace(/[  ]/g, ' ');
  assert.equal(sp(S.shareText(0.4249)), '42%');
  assert.equal(sp(S.shareText(0.004)), '<1%');
  assert.equal(sp(S.shareText(0)), '0%');
});

test('names that are properties of every object are not names of ours', () => {
  assert.equal(S.shortName({ key: 'constructor', label: 'Mine' }), 'Mine');
  assert.equal(S.shortName({ key: 'series.constructor.snapshots' }), 'constructor: day series');
  assert.equal(S.groupWord('constructor'), 'constructor');
});
