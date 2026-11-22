// Tests for packs.js: the pack picker's list from GET /ui-packs, its fallback, and the short descriptions.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { packBlurb, packChoices, packLabel } from '../packs.js';

const FALLBACK = [['instrument', 'Instrument'], ['logbook', 'Logbook', 'Paper and ink']];
const client = (answer) => ({ get: async (path) => { assert.equal(path, 'ui-packs'); if (answer instanceof Error) throw answer; return answer; } });

test('the first phrase of a description', () => {
  assert.equal(packBlurb('Soft, friendly and touch first: rounded tiles in a bento grid. Runs on /api/v1.'), 'Soft, friendly and touch first');
  assert.equal(packBlurb('A calm, editorial light-first UI: the day told as a journal.'), 'A calm, editorial light-first UI');
  assert.equal(packBlurb('Just one sentence.'), 'Just one sentence');
  assert.equal(packBlurb(''), '');
  assert.equal(packBlurb(null), '');
  const long = packBlurb('word '.repeat(40), 30);
  assert.ok(long.length <= 30 && long.endsWith('…'));
});

test('every pack the server lists, unsupported ones left out', async () => {
  const r = await packChoices(client({ items: [
    { id: 'instrument', name: 'Instrument', description: 'Dark-first: glass.', version: '0.2.0', api_supported: true },
    { id: 'pebble', name: 'Pebble', description: 'Soft: tiles.', version: '0.1.0', api_supported: true },
    { id: 'old', name: 'Old', description: '', api_supported: false },
  ] }), { fallback: FALLBACK, current: 'pebble' });
  assert.equal(r.fromServer, true);
  assert.deepEqual(r.items.map((p) => p.id), ['instrument', 'pebble']);
  assert.equal(r.items[1].blurb, 'Soft');
  assert.equal(packLabel(r.items[1]), 'Pebble: Soft');
  assert.equal(packLabel({ name: 'Old', blurb: '' }), 'Old');
});

test('the current pack stays in the list even when it is not offered', async () => {
  const r = await packChoices(client({ items: [{ id: 'old', name: 'Old', api_supported: false }, { id: 'a', name: 'A' }] }), { current: 'old' });
  assert.deepEqual(r.items.map((p) => p.id), ['old', 'a']);
  const s = await packChoices(client({ items: [{ id: 'a', name: 'A' }] }), { current: 'gone' });
  assert.deepEqual(s.items.map((p) => p.id), ['a', 'gone']);
});

test('the fallback list when the request fails or answers nothing', async () => {
  for (const answer of [new Error('offline'), null, { items: [] }, { nope: 1 }]) {
    const r = await packChoices(client(answer), { fallback: FALLBACK, current: 'instrument' });
    assert.equal(r.fromServer, false);
    assert.deepEqual(r.items.map((p) => [p.id, p.name, p.blurb]), [['instrument', 'Instrument', ''], ['logbook', 'Logbook', 'Paper and ink']]);
  }
});
