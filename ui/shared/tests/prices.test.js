// Tests for prices.js: a place's dated prices, the one in force on a day, a typed price over one from invoices,
// the checks before saving and the body sent. Made-up prices only.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as P from '../prices.js';

const usd = (per_kwh, more = {}) => ({ currency: 'USD', per_kwh, per_minute: null, rates: [], session_fee: null, ...more });

test('the price of a day is the newest typed one that started by then, else one from invoices', () => {
  const list = [
    { from: '2026-05-01', source: 'typed', price: usd(0.3) },
    { from: '2026-01-10', source: 'invoice', price: usd(0.48), invoice: { sessions: 10 } },
    { from: '2026-03-01', source: 'typed', price: usd(0.25) },
  ];
  const on = (d) => P.priceOnDay(list, d)?.price.per_kwh ?? null;
  assert.equal(on('2026-01-09'), null);
  assert.equal(on('2026-02-01'), 0.48);
  assert.equal(on('2026-03-01'), 0.25);
  assert.equal(on('2026-12-31'), 0.3);
  assert.deepEqual(P.sortPrices(list).map((x) => x.from), ['2026-01-10', '2026-03-01', '2026-05-01']);
  assert.equal(P.invoiceShadowed(list, list[1]), false);
  assert.equal(P.invoiceShadowed([...list, { from: null, source: 'typed', price: usd(0.1) }], list[1]), true);
});

test('a place with only `price` has it from the start', () => {
  assert.deepEqual(P.pricesOf({ price: usd(0.2) }), [{ from: null, source: 'typed', price: usd(0.2) }]);
  assert.deepEqual(P.pricesOf({ price: null }), []);
  assert.equal(P.pricesOf({ prices: [{ from: '2026-02-01', price: usd(1) }, { from: null, price: usd(2) }] })[0].from, null);
});

test('entries are checked before saving, and only the typed ones are sent, with any kind of price kept', () => {
  assert.equal(P.isDay('2026-02-28'), true);
  assert.equal(P.isDay('2026-02-30'), false);
  const e = P.checkPrices([{ from: '2026-02-30', price: usd(0.1) }, { from: null, price: usd(200) }, { from: null, price: { currency: 'usd' } }]);
  assert.ok(e['prices.0.from'] && e['prices.1.price.per_kwh'] && e['prices.2.from'] && e['prices.2.price.currency'], JSON.stringify(e));
  // a price of a kind this editor does not show (steps, say) is a price
  assert.deepEqual(P.checkPrices([{ from: null, price: { currency: 'EUR', tiers: [{ up_to_kwh: 10, per_kwh: 0.2 }] } }]), {});
  const body = P.typedBody([
    { from: '2026-06-01', source: 'typed', price: usd(0.4) },
    { from: '2026-01-01', source: 'invoice', price: usd(0.5) },
    { from: null, price: usd(0.3) }]);
  assert.deepEqual(body, [{ from: null, price: usd(0.3) }, { from: '2026-06-01', price: usd(0.4) }]);
});

test('words, days and new places', () => {
  assert.equal(P.fromWord(null), 'From the start');
  assert.equal(P.fromWord('2026-03-01'), 'From 2026-03-01');
  assert.equal(P.dayIn(Date.UTC(2026, 0, 1, 3), 'America/Chicago'), '2025-12-31');
  assert.equal(P.dayIn(Date.UTC(2026, 0, 1, 3), 'UTC'), '2026-01-01');
  assert.equal(P.usualCurrency([{ price: usd(1) }, { prices: [{ from: null, price: { currency: 'EUR' } }, { from: '2026-01-01', price: { currency: 'EUR' } }] }]), 'EUR');
  assert.deepEqual(P.newPlaceBody({ name: '  Made-up Supercharger ', lat: 1, lon: 2 }), { name: 'Made-up Supercharger', kind: 'charger', lat: 1, lon: 2, radius_m: 100 });
  assert.match(P.priceLine(usd(0.31, { session_fee: 1, rates: [{}] })), /0\.31.*1 time-of-use rate.*a session/);
});
