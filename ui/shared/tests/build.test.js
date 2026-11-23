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
  b.loadPage(fail);
  await flush();
  assert.equal(loc.replaced.length, 1, 'one more try, in full');
  // the page after that full load fails again
  fakeLocation('https://sm.example/cars/c1/health');
  await assert.rejects(b.loadPage(fail), (e) => e.code === 'page_failed' && e.status === 0 && b.isPageFailed(e)
    && /updated or a file could not load/.test(e.message) && e.cause instanceof TypeError);
  assert.equal(sessionStorage.getItem('sentrymate.page-retry'), null, 'Reload tries again from the start');
  // another page that fails gets its own try
  const loc2 = fakeLocation('https://sm.example/cars/c1/drives');
  b.loadPage(fail);
  await flush();
  assert.equal(loc2.replaced.length, 1);
  // and one that loads forgets the try
  const mod = { mount() {} };
  assert.equal(await b.loadPage(async () => mod), mod);
  assert.equal(sessionStorage.getItem('sentrymate.page-retry'), null);
});

test('the words of a page that could not load', () => {
  assert.equal(shared.PAGE_FAILED.code, 'page_failed');
  assert.equal(shared.PAGE_FAILED.title, 'Sentry Mate was updated or a file could not load');
  assert.equal(shared.PAGE_FAILED.action, 'Reload');
  assert.equal(shared.pageFailedError().status, 0);
});
