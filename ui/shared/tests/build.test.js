// Tests for build.js: which build the server runs, and loading a page's files across an update.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { define } from './fake-browser.js';
import * as api from '../api.js';
import * as shared from '../build.js';

// a module of its own (its own "first build seen"), so the order of the test files does not matter
const fresh = async (tag) => import(`../build.js?${tag}`);

// a stand-in address for one test (taken away again after it: other files read globalThis.location)
let cleanup = null;
function fakeLocation(href, t) {
  if (t && !cleanup) { const before = Object.getOwnPropertyDescriptor(globalThis, 'location'); cleanup = () => { if (before) Object.defineProperty(globalThis, 'location', before); else delete globalThis.location; cleanup = null; }; t.after(cleanup); }
  const loc = { href, replaced: [], replace(u) { this.replaced.push(u); } };
  define('location', loc);
  return loc;
}
const flush = () => new Promise((r) => setTimeout(r, 5));

test('the first build seen is the loaded one; another one later means the server was updated', async () => {
  const b = await fresh('first');
  assert.equal(b.buildChanged(), false);
  b.observeBuild(null);
  b.observeBuild('');
  assert.equal(b.loadedBuild(), null);
  b.observeBuild('aaaa1111aaaa1111');
  b.observeBuild(' aaaa1111aaaa1111 ');
  assert.equal(b.buildChanged(), false);
  b.observeBuild('bbbb2222bbbb2222');
  assert.equal(b.buildChanged(), true);
  assert.deepEqual([b.loadedBuild(), b.serverBuild()], ['aaaa1111aaaa1111', 'bbbb2222bbbb2222']);
});

test('the API client hands each answer\'s build header over', async () => {
  const fetch = async () => new Response('{}', { status: 200, headers: { 'Content-Type': 'application/json', [shared.BUILD_HEADER]: 'cccc3333cccc3333' } });
  await api.createClient({ fetch }).get('cars');
  assert.equal(shared.serverBuild(), 'cccc3333cccc3333');
});

test('checkBuild asks /health without the cache and never rejects', async () => {
  const b = await fresh('check');
  const asked = [];
  await b.checkBuild({ fetch: async (u, o) => { asked.push([u, o.cache]); return new Response(null, { headers: { [b.BUILD_HEADER]: 'dddd' } }); } });
  assert.deepEqual(asked, [['/health', 'no-store']]);
  assert.equal(b.serverBuild(), 'dddd');
  assert.equal(await b.checkBuild({ fetch: async () => { throw new TypeError('Failed to fetch'); } }), 'dddd');
});

test('after an update the next page loads its address in full, not as a module', async (t) => {
  const b = await fresh('changed');
  const loc = fakeLocation('https://sm.example/cars/c1/health?x=1#keys', t);
  b.observeBuild('old');
  b.observeBuild('new');
  let imported = false;
  const p = b.loadPage(async () => { imported = true; return {}; });
  let settled = false;
  p.then(() => { settled = true; }, () => { settled = true; });
  await flush();
  assert.equal(imported, false);
  assert.equal(settled, false, 'the tab is going away: the router waits');
  assert.equal(loc.replaced.length, 1);
  const u = new URL(loc.replaced[0]);
  assert.equal(u.pathname, '/cars/c1/health');
  assert.equal(u.searchParams.get('x'), '1');
  assert.ok(u.searchParams.get(b.FRESH_PARAM), 'past the cache');
  assert.equal(u.hash, '#keys', 'the same place');
});

test('a page file that fails is tried once with a full load; a second failure is page_failed, not "no answer"', async (t) => {
  const b = await fresh('retry');
  sessionStorage.clear();
  const loc = fakeLocation('https://sm.example/cars/c1/health', t);
  const fail = () => Promise.reject(new TypeError('Failed to fetch dynamically imported module'));
  const up = { fetch: async () => ({ ok: true, headers: { get: () => null } }) };   // the server answers /health
  b.loadPage(fail, up);
  await flush();
  assert.equal(loc.replaced.length, 1, 'one more try, in full');
  // the page after that full load fails again
  fakeLocation('https://sm.example/cars/c1/health');
  await assert.rejects(b.loadPage(fail, up), (e) => e.code === 'page_failed' && e.status === 0 && b.isPageFailed(e)
    && /updated or a file could not load/.test(e.message) && e.cause instanceof TypeError);
  assert.equal(sessionStorage.getItem('sentrymate.page-retry'), null, 'Reload tries again from the start');
  // another page that fails gets its own try
  const loc2 = fakeLocation('https://sm.example/cars/c1/drives');
  b.loadPage(fail, up);
  await flush();
  assert.equal(loc2.replaced.length, 1);
  // and one that loads forgets the try
  const mod = { mount() {} };
  assert.equal(await b.loadPage(async () => mod), mod);
  assert.equal(sessionStorage.getItem('sentrymate.page-retry'), null);
});

test('a page file that fails while the server gives no answer is "no answer from the server": the tab stays, and tries afresh later', async (t) => {
  const b = await fresh('away');
  sessionStorage.clear();
  const loc = fakeLocation('https://sm.example/cars/c1/health', t);
  const fail = () => Promise.reject(new TypeError('Failed to fetch dynamically imported module'));
  const asked = [];
  const away = async (url, init) => { asked.push([url, init?.cache]); throw new TypeError('Failed to fetch'); };
  // the server is away (a restart, the network gone): a full load would put the browser's own error page there
  await assert.rejects(b.loadPage(fail, { fetch: away }), (e) => e.status === 0 && e.code === 'network' && !b.isPageFailed(e)
    && /server/i.test(e.message) && e.cause instanceof TypeError);
  assert.deepEqual(asked, [['/health', 'no-store']]);
  assert.equal(loc.replaced.length, 0, 'no full load while the server does not answer');
  assert.equal(sessionStorage.getItem('sentrymate.page-retry'), null, 'and it does not count as the one try');
  // one that never answers is given up on, too
  await assert.rejects(b.loadPage(fail, { fetch: () => new Promise(() => {}), awayAfterMs: 20 }), (e) => e.code === 'network');
  assert.equal(loc.replaced.length, 0);
  // the server is back and the file still fails: now the one full load
  b.loadPage(fail, { fetch: async () => ({ ok: true, headers: { get: () => null } }) });
  await flush();
  assert.equal(loc.replaced.length, 1);
  // an answer that is an error is an answer: the server is there
  sessionStorage.clear();
  const loc2 = fakeLocation('https://sm.example/cars/c1/drives');
  b.loadPage(fail, { fetch: async () => ({ ok: false, status: 503, headers: { get: () => null } }) });
  await flush();
  assert.equal(loc2.replaced.length, 1);
});

test('the words of a page that could not load', () => {
  assert.equal(shared.PAGE_FAILED.code, 'page_failed');
  assert.equal(shared.PAGE_FAILED.title, 'Sentry Mate was updated or a file could not load');
  assert.equal(shared.PAGE_FAILED.action, 'Reload');
  assert.equal(shared.pageFailedError().status, 0);
});
