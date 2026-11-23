// Tests for state.js: every route round trip, period in the query, carried links, the store, navigation.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';

import { historyLog, define, htmlEl } from './fake-browser.js';
import * as S from '../state.js';

beforeEach(() => {
  globalThis.localStorage.clear();
  globalThis.sessionStorage.clear();
  historyLog.length = 0;
  S.store.set({ page: null, car: null, drive: null, charge: null, period: null, periodFromUrl: false, query: {}, path: '/' });
});

const BIG = '9007199254740993'; // past 2^53: must stay a string, every digit
const ODD = 'car one/2?x';

test('every page id builds a URL that parses back to the same page and ids', () => {
  const params = { car: 'c1', drive: BIG, charge: '18446744073709551615' };
  for (const r of S.ROUTES) {
    const args = {};
    for (const k of r.keys) args[k] = params[k];
    const href = S.buildUrl(r.page, args);
    const back = S.parseUrl(href);
    assert.equal(back.page, r.page, href);
    for (const k of ['car', 'drive', 'charge']) assert.equal(back[k], args[k] ?? null, `${href} ${k}`);
  }
  assert.ok(S.PAGE_IDS.includes('settings-integrations'));
  assert.equal(S.PAGE_IDS.length, 24, '22 pages plus sign-in and set-up');
});

test('ids with any characters are encoded in the path and decoded back', () => {
  const href = S.buildUrl('drive', { car: ODD, drive: BIG });
  assert.equal(href, `/cars/car%20one%2F2%3Fx/drives/${BIG}`);
  assert.deepEqual([S.parseUrl(href).car, S.parseUrl(href).drive], [ODD, BIG]);
  assert.equal(S.parseUrl('/cars/%E0%A4%A/drives').page, 'not-found', 'bad percent-encoding is not a page');
});

test('the period travels in the query, in either shape', () => {
  const a = S.buildUrl('drives', { car: 'c1', period: { preset: '7d' } });
  assert.equal(a, '/cars/c1/drives?preset=7d');
  assert.deepEqual(S.parseUrl(a).period, { preset: '7d' });
  const b = S.buildUrl('timeline', { car: 'c1', period: { from: 1762000000000, to: 1763300000000 }, query: { kinds: ['drive', 'charge'], q: '' } });
  assert.equal(b, '/cars/c1/timeline?from=1762000000000&to=1763300000000&kinds=drive%2Ccharge');
  const pb = S.parseUrl(b);
  assert.deepEqual(pb.period, { from: 1762000000000, to: 1763300000000 });
  assert.deepEqual(pb.query, { kinds: 'drive,charge' });
  assert.deepEqual(S.parseUrl('/cars/c1/drives?from=200&to=100').period, { from: 100, to: 200 }, 'swapped');
  assert.deepEqual(S.parseUrl('/cars/c1/drives?from=100').period, { from: 100, to: null }, 'open end: until now');
  assert.equal(S.parseUrl('/cars/c1/drives?preset=fortnight').period, null);
  assert.equal(S.parseUrl('/cars/c1/drives?from=yesterday').period, null);
  assert.deepEqual(S.parseUrl('/cars/c1/drives?preset=7d&from=1&to=2').period, { from: 1, to: 2 }, 'explicit dates win');
  assert.deepEqual(S.parseUrl('https://sm.example/cars/c1/battery?preset=year#top'), {
    page: 'battery', car: 'c1', drive: null, charge: null, period: { preset: 'year' }, query: {}, path: '/cars/c1/battery',
  });
  assert.deepEqual(S.periodQuery({ preset: 'custom', from: 1.4, to: 2.6 }), { from: '1', to: '3' });
});

test('pages with and without a car', () => {
  assert.equal(S.buildUrl('charges', { car: 'c1' }), '/cars/c1/charges');
  assert.equal(S.buildUrl('charges', { query: { cars: 'all' } }), '/charges?cars=all');
  assert.deepEqual(S.parseUrl('/charges?cars=all'), { page: 'charges', car: null, drive: null, charge: null, period: null, query: { cars: 'all' }, path: '/charges' });
  assert.equal(S.buildUrl('statistics'), '/statistics');
  assert.equal(S.buildUrl('health', { car: 'c1' }), '/cars/c1/health');
  assert.equal(S.buildUrl('health'), '/system');
  assert.equal(S.buildUrl('fleet', { car: 'c1' }), '/');
  assert.equal(S.parseUrl('/').page, 'fleet');
  assert.equal(S.parseUrl('/cars/c1/').page, 'car');
  assert.equal(S.parseUrl('/nowhere').page, 'not-found');
  assert.throws(() => S.buildUrl('drive', { car: 'c1' }), RangeError);
  assert.throws(() => S.buildUrl('dashboard'), RangeError);
});

test('the store tells subscribers about real changes only', () => {
  const st = S.createStore({ a: 1, p: { preset: '7d' } });
  const seen = [];
  const off = st.subscribe((s, prev) => seen.push([prev.a, s.a]));
  st.set({ a: 1, p: { preset: '7d' } });
  st.set({ a: 2 });
  st.set((s) => ({ a: s.a + 1 }));
  off();
  st.set({ a: 9 });
  assert.deepEqual(seen, [[1, 2], [2, 3]]);
  assert.ok(Object.isFrozen(st.get()));
});

test('navigate changes the address, removes data-ready, and keeps the period for the tab', () => {
  htmlEl().dataset.ready = 'drives';
  const s = S.navigate('/cars/c7/drives?preset=7d&min_distance_km=10');
  assert.equal(htmlEl().dataset.ready, undefined);
  assert.deepEqual(historyLog, [['push', '/cars/c7/drives?preset=7d&min_distance_km=10']]);
  assert.deepEqual([s.page, s.car, s.period, s.periodFromUrl, s.query], ['drives', 'c7', { preset: '7d' }, true, { min_distance_km: '10' }]);
  assert.equal(S.lastCar(), 'c7');
  // a menu link without a period: the tab's last choice still applies
  const t = S.navigate('/cars/c7/timeline', { replace: true });
  assert.deepEqual([t.page, t.period, t.periodFromUrl], ['timeline', { preset: '7d' }, false]);
  assert.deepEqual(historyLog[1], ['replace', '/cars/c7/timeline']);
  assert.deepEqual(S.tabPeriod(), { preset: '7d' });
});

test('links carry the car and the chosen period; the car switcher keeps the page', () => {
  S.navigate('/cars/c1/timeline?preset=30d&kinds=drive');
  assert.equal(S.linkTo('drives'), '/cars/c1/drives?preset=30d');
  assert.equal(S.linkTo('places'), '/places', 'no period on a page without one');
  assert.equal(S.linkTo('settings-cars'), '/settings/cars/c1');
  assert.equal(S.linkTo('charges', { car: null }), '/charges?preset=30d');
  assert.equal(S.linkTo('drive', { drive: BIG }), `/cars/c1/drives/${BIG}?preset=30d`);
  assert.equal(S.linkTo('battery', { period: null }), '/cars/c1/battery');
  assert.equal(S.linkForCar('c2'), '/cars/c2/timeline?preset=30d&kinds=drive');
  S.navigate(`/cars/c1/drives/${BIG}?preset=30d`);
  assert.equal(S.linkForCar('c2'), '/cars/c2/drives?preset=30d', 'a drive belongs to one car: go to the list');
  S.navigate('/places');
  assert.equal(S.linkForCar('c2'), '/cars/c2?preset=30d', 'a page without a car goes to the overview, the tab period kept');
  assert.equal(S.linkTo('car'), '/cars/c1?preset=30d', 'the last car is used when the page has none');
  S.forgetCar('c1');
  S.store.set({ car: null });
  assert.equal(S.linkTo('drives'), '/', 'no car known: Fleet');
});

test('setPeriod and setQuery write the address without a navigation', () => {
  S.navigate('/cars/c1/drives?min_distance_km=5');
  htmlEl().dataset.ready = 'drives';
  historyLog.length = 0;
  S.setPeriod({ from: 1000, to: 2000 });
  assert.deepEqual(historyLog.pop(), ['replace', '/cars/c1/drives?from=1000&to=2000&min_distance_km=5']);
  assert.equal(htmlEl().dataset.ready, 'drives');
  S.setQuery({ min_distance_km: null, q: 'coffee' }, { push: true });
  assert.deepEqual(historyLog.pop(), ['push', '/cars/c1/drives?from=1000&to=2000&q=coffee']);
  S.setPeriod('year');
  assert.deepEqual(historyLog.pop(), ['replace', '/cars/c1/drives?preset=year&q=coffee']);
  S.setPeriod(null);
  assert.deepEqual(historyLog.pop(), ['replace', '/cars/c1/drives?q=coffee']);
  assert.equal(S.current().period, null);
  assert.equal(S.tabPeriod(), null);
  assert.throws(() => S.setPeriod('fortnight'), RangeError);
});

test('storage that throws is only a lost convenience', () => {
  const broken = { getItem() { throw new Error('blocked'); }, setItem() { throw new Error('blocked'); }, removeItem() { throw new Error('blocked'); } };
  const keep = [globalThis.localStorage, globalThis.sessionStorage];
  define('localStorage', broken);
  define('sessionStorage', broken);
  try {
    const s = S.navigate('/cars/c3/battery?preset=all');
    assert.deepEqual([s.car, s.period], ['c3', { preset: 'all' }]);
    assert.equal(S.lastCar(), null);
    assert.equal(S.tabPeriod(), null);
  } finally {
    define('localStorage', keep[0]);
    define('sessionStorage', keep[1]);
  }
});

test('splitSold keeps the order and puts sold cars apart', async () => {
  const { splitSold, isSold } = await import('../state.js');
  const r = splitSold([{ id: 'c1' }, { id: 'c0', sold: true }, { id: 'c2', sold: false }]);
  assert.deepEqual(r.active.map(c => c.id), ['c1', 'c2']);
  assert.deepEqual(r.sold.map(c => c.id), ['c0']);
  assert.equal(isSold(null), false);
});

test('preferredCar: main car, else last car (not sold), else pinned, else the first not sold', () => {
  const cars = [{ id: 'c1', sold: true }, { id: 'c2' }, { id: 'c3' }, { id: 'c4', sold: true }];
  const P = (p) => ({ main_car: null, last_car: null, pinned_cars: [], car_order: [], ...p });
  assert.equal(S.preferredCar(cars, P({ main_car: 'c3', last_car: 'c2' })), 'c3', 'the main car wins');
  assert.equal(S.preferredCar(cars, P({ main_car: 'c4' })), 'c4', 'a main car is kept even when sold');
  assert.equal(S.preferredCar(cars, P({ main_car: 'c9', last_car: 'c3' })), 'c3', 'a main car not seen is passed over');
  assert.equal(S.preferredCar(cars, P({ last_car: 'c1', pinned_cars: ['c4', 'c3'] })), 'c3', 'a sold last car and sold pins are passed over');
  assert.equal(S.preferredCar(cars, P({ last_car: 'c9', car_order: ['c1', 'c3'] })), 'c3', 'then the car order');
  assert.equal(S.preferredCar(cars, P({})), 'c2', 'else the first not sold, never the imported old car');
  assert.equal(S.preferredCar([{ id: 'c1', sold: true }], P({})), 'c1', 'only sold cars: the first');
  assert.equal(S.preferredCar([], P({ main_car: 'c1' })), null);
});

test('rememberCar saves last_car once after quick picks, never a sold car picked implicitly', async () => {
  const sent = [];
  S.configureCarMemory({ delayMs: 10, save: async (id) => { sent.push(id); return { last_car: id, main_car: null, pinned_cars: [], car_order: [] }; } });
  S.setCarPrefs({ main_car: null, last_car: 'c1', pinned_cars: [], car_order: [] });
  S.rememberCar('c1');
  S.rememberCar('c2');
  S.rememberCar('c3');
  S.rememberCar('c4', { sold: true, explicit: false });
  await new Promise((r) => setTimeout(r, 40));
  assert.deepEqual(sent, ['c3'], 'debounced: the newest pick only');
  assert.equal(S.carPrefs().last_car, 'c3');
  assert.equal(S.lastCar(), 'c3');
  S.rememberCar('c3');
  S.rememberCar('c4', { sold: true });
  await S.flushCar();
  assert.deepEqual(sent, ['c3', 'c4'], 'the same car again is not sent; a sold car opened explicitly is');
  // another device chose: the server's word replaces the local copy; a main car wins over it
  S.setCarPrefs({ main_car: 'c2', last_car: 'c1', pinned_cars: [], car_order: [] });
  assert.equal(S.lastCar(), 'c2');
  S.setCarPrefs({ main_car: null, last_car: 'c1', pinned_cars: [], car_order: [] });
  assert.equal(S.lastCar(), 'c1');
  S.configureCarMemory({ save: async () => { throw new Error('offline'); } });
  S.rememberCar('c5');
  await S.flushCar();
  assert.equal(S.lastCar(), 'c5', 'a failed save keeps the local copy');
});

test('a URL that only moves the #hash on the page shown is no new page: data-ready and the store stay', () => {
  S.navigate('/settings/integrations');
  htmlEl().dataset.ready = 'settings-integrations';
  const before = S.current();
  const seen = [];
  const off = S.subscribe((s) => seen.push(s.page));
  assert.equal(S.sameRoute('/settings/integrations#mqtt'), true);
  assert.equal(S.sameRoute('/settings/integrations?tab=setup#mqtt'), false, 'another query is another view');
  assert.equal(S.sameRoute('/settings/cars#data-sources'), false);
  const s = S.navigate('/settings/integrations#teslamate');
  off();
  assert.equal(s, before);
  assert.deepEqual(seen, []);
  assert.equal(htmlEl().dataset.ready, 'settings-integrations');
  assert.deepEqual(historyLog.at(-1), ['push', '/settings/integrations#teslamate']);
  // another page still re-routes
  S.navigate('/settings/cars#data-sources');
  assert.equal(htmlEl().dataset.ready, undefined);
  assert.equal(S.current().page, 'settings-cars');
});
