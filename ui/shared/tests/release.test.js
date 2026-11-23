// Tests for release.js: the build label, the release check's texts and the per-build dismissal.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as R from '../release.js';

const load = async () => R;
const NOW = Date.UTC(2026, 10, 22, 12);

test('build label: build number and date, the commit only in the title; no number says dev', async () => {
  const r = await load();
  const about = { version: '0.1.0', build: { version: '0.1.0', number: 41, short: '2f44a52', built_ms: Date.UTC(2026, 10, 22), dev: false } };
  assert.equal(r.buildLabel(about, NOW), '0.1.0 · build 41 · Nov 22');
  assert.equal(r.buildTitle(about), 'commit 2f44a52');
  assert.equal(r.buildLabel({ version: '0.1.0', build: { version: '0.1.0', dev: true, number: null, short: null } }, NOW), '0.1.0 · dev build');
  assert.equal(r.buildTitle({ build: { dev: true } }), '');
  // a commit but no number: still a dev build by its label
  assert.equal(r.buildLabel({ version: '0.1.0', build: { version: '0.1.0', number: null, short: '2f44a52', dev: false } }, NOW), '0.1.0 · dev build');
  assert.equal(r.buildDate(Date.UTC(2025, 0, 3), NOW), 'Jan 3, 2025');
});

test('release status texts', async () => {
  const r = await load();
  const newest = { short: '9c1e7d3', build: 42, date_ms: Date.UTC(2026, 10, 21), changes: 4, titles: ['a'], more: 2 };
  assert.equal(r.releaseStatus({ status: 'newer', newest }, NOW).text, 'A newer build is ready: build 42, Nov 21, 4 changes');
  assert.equal(r.newestTitle(newest), 'commit 9c1e7d3');
  assert.equal(r.newestText({ ...newest, build: null, changes: 1 }, NOW), 'Nov 21, 1 change');
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

test('server updates card: this build, the newest, last checked, in every state of the check', async () => {
  const r = await load();
  const about = { version: '0.1.0', build: { version: '0.1.0', number: 41, short: '2f44a52', built_ms: Date.UTC(2026, 10, 20), dev: false }, links: { source: null } };
  const row = (u, k) => u.rows.find((x) => x.key === k);
  const off = r.serverUpdates(about, { enabled: false, status: 'off', checked_ms: null }, NOW);
  assert.equal(off.on, false);
  assert.equal(row(off, 'this').text, '0.1.0 · build 41');
  assert.equal(row(off, 'this').title, 'commit 2f44a52');
  assert.equal(row(off, 'built').text, 'Nov 20');
  assert.equal(row(off, 'newest').tone, 'off');
  assert.equal(row(off, 'checked').text, '—');
  const newest = { short: '9c1e7d3', build: 42, date_ms: Date.UTC(2026, 10, 21), changes: 4 };
  const newer = r.serverUpdates(about, { enabled: true, status: 'newer', checked_ms: NOW - 2 * 3600e3, newest }, NOW);
  assert.deepEqual([row(newer, 'newest').text, row(newer, 'newest').badge, row(newer, 'newest').title], ['build 42, Nov 21, 4 changes', 'Newer', 'commit 9c1e7d3']);
  assert.equal(row(newer, 'checked').text, '2 h ago');
  const same = r.serverUpdates(about, { enabled: true, status: 'up_to_date', checked_ms: NOW, newest: { build: 41, date_ms: Date.UTC(2026, 10, 20), changes: 0 } }, NOW);
  assert.deepEqual([row(same, 'newest').text, row(same, 'newest').badge], ['build 41, Nov 20', 'Up to date']);
  assert.equal(row(same, 'checked').text, 'just now');
  const failed = r.serverUpdates(about, { enabled: true, status: 'error', checked_ms: NOW, error: { message: 'rate limited' } }, NOW);
  assert.deepEqual([row(failed, 'newest').text, failed.tone], ['Could not check: rate limited', 'error']);
  const never = r.serverUpdates(about, { enabled: true, status: 'never', checked_ms: null }, NOW);
  assert.equal(row(never, 'checked').text, 'Not yet');
  assert.equal(r.serverUpdates(about, { enabled: true, status: 'checking' }, NOW).busy, true);
  assert.equal(row(r.serverUpdates(about, null, NOW), 'newest').tone, 'error');
  assert.equal(row(r.serverUpdates({ version: '0.1.0', build: { dev: true } }, null, NOW), 'this').text, '0.1.0 · dev build');
  assert.equal(off.doc.href, null);
  assert.equal(r.updatingDoc({ links: { source: 'https://example.org/owner/app/' } }).href, 'https://example.org/owner/app/blob/main/docs/install.md#updating');
});
