// Tests for units.js: conversions, precision per kind, blanks, money, field kinds, rated or ideal range.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import * as U from '../units.js';
import { setLocale } from '../i18n.js';

// Text with plain spaces, so the expectations are readable (the module joins number and unit with a
// no-break space, and Intl may use narrow ones).
const sp = (s) => String(s).replace(/[\u00a0\u202f\u2009]/g, ' ');
const f = (...a) => sp(U.format(...a));

beforeEach(() => {
  setLocale('en-US');
  U.setUnits({ distance: 'km', temperature: 'c', pressure: 'bar', range: 'rated' });
  U.setMissingText(null);
});

test('metric values keep their unit, with the precision of the kind', () => {
  assert.equal(f(12.34, 'distance'), '12.3 km');
  assert.equal(f(123.4, 'distance'), '123 km');
  assert.equal(f(48211.7, 'odometer'), '48,212 km');
  assert.equal(f(312.6, 'range'), '313 km');
  assert.equal(f(87.4, 'speed'), '87 km/h');
  assert.equal(f(21.25, 'temperature'), '21.3 °C');
  assert.equal(f(290, 'pressure'), '2.90 bar');
  assert.equal(f(0.35, 'energy'), '0.35 kWh');
  assert.equal(f(45.25, 'energy'), '45.3 kWh');
  assert.equal(f(12345.6, 'energy'), '12,346 kWh');
  assert.equal(f(7.24, 'power'), '7.2 kW');
  assert.equal(f(151.6, 'power'), '152 kW');
  assert.equal(f(38, 'power_w'), '38 W');
  assert.equal(f(161.4, 'efficiency'), '161 Wh/km');
  assert.equal(f(80, 'percent'), '80%');
  assert.equal(f(12.64, 'voltage'), '12.6 V');
  assert.equal(f(398.2, 'voltage'), '398 V');
  assert.equal(f(16, 'current'), '16 A');
  assert.equal(f(270, 'degrees'), '270°');
  assert.equal(f(1240, 'length'), '1,240 m');
  assert.equal(f(7500000, 'duration'), '2 h 05 min');
  assert.equal(f(1234567890, 'bytes'), '1.2 GB');
  assert.equal(f(345000000, 'bytes'), '345 MB');
  assert.equal(f(512, 'bytes'), '512 B');
  assert.equal(f(1234567, 'count'), '1,234,567');
});

test('imperial units convert from the metric API values', () => {
  U.setUnits({ units: { distance: 'mi', temperature: 'f', pressure: 'psi' }, range: 'ideal' });
  assert.equal(f(16.0934, 'distance'), '10.0 mi');
  assert.equal(f(160.934, 'distance'), '100 mi', 'rounds up past 100: no decimal');
  assert.equal(f(99.9, 'speed'), '62 mph');
  assert.equal(f(160.0, 'distance'), '99.4 mi', 'under 100 keeps its decimal');
  assert.equal(f(100, 'speed'), '62 mph');
  assert.equal(f(0, 'temperature'), '32.0 °F');
  assert.equal(f(-40, 'temperature'), '\u221240.0 °F');
  assert.equal(f(10, 'temperature_delta'), '18.0 °F');
  assert.equal(f(290, 'pressure'), '42 psi');
  assert.equal(f(150, 'efficiency'), '241 Wh/mi');
  assert.equal(f(100, 'length'), '328 ft');
  assert.equal(f(80, 'range_rate'), '50 mi/h');
  assert.equal(U.unitLabel('distance'), 'mi');
  assert.equal(U.unitLabel('speed'), 'mph');
  assert.equal(U.unitLabel('pressure'), 'psi');
  // per-call override
  assert.equal(f(16.0934, 'distance', { units: { distance: 'km' } }), '16.1 km');
});

test('toMetric turns a typed value back into the API unit', () => {
  U.setUnits({ distance: 'mi', temperature: 'f', pressure: 'psi' });
  assert.ok(Math.abs(U.toMetric(10, 'distance') - 16.09344) < 1e-9);
  assert.ok(Math.abs(U.toMetric(32, 'temperature')) < 1e-9);
  assert.ok(Math.abs(U.toMetric(42, 'pressure') - 289.58) < 0.01);
  assert.ok(Math.abs(U.toMetric(328.084, 'length') - 100) < 1e-3);
  assert.equal(U.toMetric('', 'distance'), null);
  assert.equal(U.toMetric('abc', 'distance'), null);
  for (const kind of ['distance', 'speed', 'temperature', 'pressure', 'efficiency', 'length', 'money_per_distance']) {
    const back = U.convert(U.toMetric(123.45, kind), kind);
    assert.ok(Math.abs(back - 123.45) < 1e-9, kind);
  }
});

test('null and other non-numbers are an em dash, with the reason when one is known', () => {
  assert.equal(U.format(null, 'distance'), '\u2014');
  assert.equal(U.format(undefined, 'temperature'), '\u2014');
  assert.equal(U.format(NaN, 'energy'), '\u2014');
  assert.equal(U.format('12', 'energy'), '\u2014');
  const p = U.parts(null, 'temperature', { why: 'No reading while the car was asleep' });
  assert.equal(p.missing, true);
  assert.equal(p.text, '\u2014');
  assert.equal(p.why, 'No reading while the car was asleep');
  assert.equal(U.parts(null, 'temperature').why, null);
  U.setMissingText((kind) => (kind === 'pressure' ? 'Tyre sensors not paired yet' : null));
  assert.equal(U.parts(null, 'pressure').why, 'Tyre sensors not paired yet');
  assert.equal(U.parts(null, 'pressure', { why: 'Own reason' }).why, 'Own reason');
  assert.equal(U.parts(null, 'energy').why, null);
});

test('signs, digits, compact numbers and no minus zero', () => {
  assert.equal(f(-0.01, 'distance'), '0.0 km');
  assert.equal(f(5, 'percent', { sign: true }), '+5%');
  assert.equal(f(-5, 'percent', { sign: true }), '\u22125%');
  assert.equal(f(0, 'percent', { sign: true }), '0%');
  assert.equal(f(12.3456, 'energy', { digits: 3 }), '12.346 kWh');
  assert.equal(f(2500000, 'count', { compact: true }), '2.5M');
  assert.equal(f(99999, 'count', { compact: true }), '99,999');
  assert.equal(f(12.3, 'distance', { unit: false }), '12.3');
  const p = U.parts(12.34, 'distance');
  assert.deepEqual([p.number, p.unit, p.sep], ['12.3', 'km', '\u00a0']);
});

test('money keeps its currency, and currencies are never added together', () => {
  assert.equal(f(4.2, 'money', { currency: 'EUR' }), '€4.20');
  assert.equal(f({ amount: 12, currency: 'USD' }, 'money'), '$12.00');
  assert.equal(f({ amount: 0.315, currency: 'EUR' }, 'price_energy'), '€0.315/kWh');
  assert.equal(f({ amount: 0.3, currency: 'EUR' }, 'price_energy'), '€0.30/kWh');
  assert.equal(f({ amount: 0.4, currency: 'GBP' }, 'price_time'), '£0.40/min');
  assert.equal(f(4.2, 'money_per_distance', { currency: 'EUR' }), '€4.20 per 100 km');
  U.setUnits({ distance: 'mi' });
  assert.equal(f(4.2, 'money_per_distance', { currency: 'EUR' }), '€6.76 per 100 mi');
  assert.equal(f(3, 'money'), '3.00');
  assert.equal(f(3, 'money', { currency: 'XQQ' }), 'XQQ 3.00');
  assert.equal(f(3, 'money', { currency: 'EURO' }), '3.00 EURO');
  assert.equal(f({ amount: null, currency: 'EUR' }, 'money'), '\u2014');
  assert.equal(sp(U.formatMoneyList([{ currency: 'EUR', amount: 12.4 }, { currency: 'USD', amount: 3.1 }])), '€12.40 + $3.10');
  assert.equal(sp(U.formatMoneyList({ EUR: 1 })), '€1.00');
  assert.equal(U.formatMoneyList([]), '\u2014');
});

test('the locale decides separators and the space before %', () => {
  setLocale('de-DE');
  assert.equal(f(234.5, 'energy'), '234,5 kWh');
  assert.equal(f(1234.5, 'energy', { digits: 1 }), '1.234,5 kWh');
  assert.equal(f(45, 'percent'), '45 %');
  assert.equal(f(4.2, 'money', { currency: 'EUR' }), '4,20 €');
  setLocale('en-US');
  assert.equal(f(1234.5, 'energy', { digits: 1 }), '1,234.5 kWh');
});

test('the kind of an API field comes from its unit suffix', () => {
  const cases = {
    distance_km: 'distance', start_odometer_km: 'odometer', odometer_km: 'odometer', end_range_km: 'range', range_km: 'range',
    speed_max_kph: 'speed', outside_avg_c: 'temperature', tyre_fl_kpa: 'pressure', energy_added_kwh: 'energy', power_max_kw: 'power',
    avg_power_w: 'power_w', wh_per_km: 'efficiency', net_wh_per_km: 'efficiency', cost_per_100km: 'money_per_distance',
    soc_pct: 'percent', lv_v: 'voltage', pilot_a: 'current', heading_deg: 'degrees', accuracy_m: 'length', climb_m: 'length',
    duration_ms: 'duration', charging_ms: 'duration', awake_vehicle_bus_only_ms: 'duration', since_previous_ms: 'duration',
    start_ms: null, at_ms: null, last_upload_ms: null, name: null,
  };
  for (const [field, kind] of Object.entries(cases)) assert.equal(U.kindOf(field), kind, field);
  assert.equal(sp(U.formatField({ distance_km: 12.34 }, 'distance_km')), '12.3 km');
  assert.equal(U.formatField(null, 'distance_km'), '\u2014');
});

test('settings: only allowed values, listeners told once per change', () => {
  const seen = [];
  const off = U.onUnitsChange((u) => seen.push(u.distance));
  U.setUnits({ distance: 'mi' });
  U.setUnits({ distance: 'mi' });
  U.setUnits({ distance: 'furlong', temperature: 'k' });
  assert.deepEqual(seen, ['mi']);
  assert.deepEqual(U.getUnits(), { distance: 'mi', temperature: 'c', pressure: 'bar', range: 'rated' });
  off();
  U.setUnits({ distance: 'km' });
  assert.deepEqual(seen, ['mi']);
  assert.throws(() => U.format(1, 'furlongs'), RangeError);
});

test('rated or ideal range', () => {
  const now = { range_km: 300, ideal_range_km: 320 };
  assert.equal(U.rangeValue(now), 300);
  assert.equal(U.rangeLabel(), 'Rated range');
  U.setUnits({ range: 'ideal' });
  assert.equal(U.rangeValue(now), 320);
  assert.equal(U.rangeField('start_range_km'), 'start_ideal_range_km');
  assert.equal(U.rangeValue({ start_range_km: 250 }, 'start_range_km'), 250);
  assert.equal(U.rangeValue({ ideal_range_km: null, range_km: 1 }), null);
  assert.equal(U.rangeLabel(), 'Ideal range');
});
