// Tests for release.js: the build label, the release check's texts and the per-build dismissal.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as R from '../release.js';

const load = async () => R;
const NOW = Date.UTC(2026, 10, 22, 12);

test('build label: build, short commit and date; a local build says dev', async () => {
  const r = await load();
  const about = { version: '0.1.0', build: { version: '0.1.0', short: '2f44a52', built_ms: Date.UTC(2026, 10, 22), dev: false } };
  assert.equal(r.buildLabel(about, NOW), '0.1.0 · build 2f44a52 · Nov 22');
  assert.equal(r.buildLabel({ version: '0.1.0', build: { version: '0.1.0', dev: true, short: null } }, NOW), '0.1.0 · dev build');
  assert.equal(r.buildDate(Date.UTC(2025, 0, 3), NOW), 'Jan 3, 2025');
});

test('release status texts', async () => {
  const r = await load();
  const newest = { short: '9c1e7d3', date_ms: Date.UTC(2026, 10, 21), changes: 4, titles: ['a'], more: 2 };
  assert.equal(r.releaseStatus({ status: 'newer', newest }, NOW).text, 'A newer build is ready: 9c1e7d3, Nov 21, 4 changes');
  assert.equal(r.releaseStatus({ status: 'up_to_date' }, NOW).text, 'Up to date');
  assert.equal(r.releaseStatus({ status: 'error', error: { message: 'rate limited' } }, NOW).text, 'Could not check: rate limited');
  assert.equal(r.releaseStatus({ status: 'off' }, NOW).tone, 'off');
  assert.deepEqual(r.changeLines(newest), ['a', 'and 2 more changes']);
  assert.equal(r.checkedAgo({ checked_ms: NOW - 2 * 3600e3 }, NOW), 'checked 2 h ago');
  assert.equal(r.changesText(1), '1 change');
});

test('dismissal is per commit and survives blocked storage', async () => {
  const r = await load();
  const had = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
  const store = new Map();
  globalThis.localStorage = { getItem: (k) => store.get(k) ?? null, setItem: (k, v) => store.set(k, v) };
  const n = { detail: { commit: 'abc' } };
  assert.equal(r.releaseDismissed(n), false);
  r.dismissRelease(n);
  assert.equal(r.releaseDismissed(n), true);
  assert.equal(r.releaseDismissed({ detail: { commit: 'def' } }), false);
  globalThis.localStorage = { getItem() { throw new Error('blocked'); }, setItem() { throw new Error('blocked'); } };
  assert.equal(r.releaseDismissed(n), false);
  r.dismissRelease(n);
  if (had) Object.defineProperty(globalThis, 'localStorage', had); else delete globalThis.localStorage;
});
