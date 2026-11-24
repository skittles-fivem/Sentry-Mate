// Tests for time.js: zones and daylight saving, ISO weeks, buckets, formatting and period presets.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import * as T from '../time.js';
import { setLocale } from '../i18n.js';

const CHI = 'America/Chicago';
const BER = 'Europe/Berlin';
const iso = (ms) => new Date(ms).toISOString();
const sp = (s) => String(s).replace(/[\u00a0\u202f\u2009]/g, ' ');
const H = T.HOUR;
// the dev server's default "now": Monday 16 November 2026, 11:30 in Chicago
const NOW = Date.parse('2026-11-16T17:30:00Z');

beforeEach(() => setLocale('en-US'));

test('wall-clock parts and offsets in a zone', () => {
  const p = T.zonedParts(NOW, CHI);
  assert.deepEqual([p.year, p.month, p.day, p.hour, p.minute, p.weekday], [2026, 11, 16, 11, 30, 1]);
  assert.equal(T.zoneOffset(NOW, CHI), -6 * H);
  assert.equal(T.zoneOffset(Date.parse('2026-07-01T12:00:00Z'), CHI), -5 * H);
  assert.equal(T.zoneOffset(NOW, BER), 1 * H);
  assert.equal(T.zoneOffset(NOW, 'Asia/Kathmandu'), 5.75 * H);
  assert.equal(T.zoneOffset(NOW, 'UTC'), 0);
});

test('a day is 23 or 25 hours long on the days the clocks change', () => {
  const len = (key, tz) => (T.dayStart(T.addDays(key, 1), tz) - T.dayStart(key, tz)) / H;
  assert.equal(len('2026-03-08', CHI), 23); // clocks forward in the US
  assert.equal(len('2026-11-01', CHI), 25); // clocks back in the US
  assert.equal(len('2026-03-29', BER), 23); // in the EU
  assert.equal(len('2026-10-25', BER), 25);
  assert.equal(len('2026-11-16', CHI), 24);
  assert.equal(iso(T.dayStart('2026-03-08', CHI)), '2026-03-08T06:00:00.000Z');
  assert.equal(iso(T.dayStart('2026-03-09', CHI)), '2026-03-09T05:00:00.000Z');
  // a zone whose day starts at 1:00 on its change day (clocks go from 0:00 to 1:00)
  assert.equal(T.zonedParts(T.dayStart('2026-09-06', 'America/Santiago'), 'America/Santiago').hour, 1);
});

test('times that do not exist or happen twice', () => {
  // 2:30 on 8 March does not exist in Chicago: compatible moves forward to 3:30 CDT
  assert.equal(iso(T.zonedToMs({ year: 2026, month: 3, day: 8, hour: 2, minute: 30 }, CHI)), '2026-03-08T08:30:00.000Z');
  assert.equal(iso(T.zonedToMs({ year: 2026, month: 3, day: 8, hour: 2, minute: 30 }, CHI, { disambiguation: 'earlier' })), '2026-03-08T07:30:00.000Z');
  // 1:30 on 1 November happens twice: first CDT, then CST
  assert.equal(iso(T.zonedToMs({ year: 2026, month: 11, day: 1, hour: 1, minute: 30 }, CHI)), '2026-11-01T06:30:00.000Z');
  assert.equal(iso(T.zonedToMs({ year: 2026, month: 11, day: 1, hour: 1, minute: 30 }, CHI, { disambiguation: 'later' })), '2026-11-01T07:30:00.000Z');
  // and back again
  const p = T.zonedParts(Date.parse('2026-11-01T07:30:00Z'), CHI);
  assert.deepEqual([p.hour, p.minute, p.offset_ms], [1, 30, -6 * H]);
});

test('start of a day, week, month and year across a clock change', () => {
  const inDstDay = Date.parse('2026-11-01T20:00:00Z'); // Sunday 1 November, 14:00 CST
  assert.equal(iso(T.startOf(inDstDay, 'day', CHI)), '2026-11-01T05:00:00.000Z');
  assert.equal(iso(T.startOf(inDstDay, 'week', CHI)), '2026-10-26T05:00:00.000Z'); // Monday 26 October, CDT
  assert.equal(iso(T.startOf(inDstDay, 'month', CHI)), '2026-11-01T05:00:00.000Z');
  assert.equal(iso(T.startOf(NOW, 'year', CHI)), '2026-01-01T06:00:00.000Z');
  assert.equal(iso(T.startOf(NOW, 'hour', 'Asia/Kathmandu')), '2026-11-16T17:15:00.000Z');
  assert.equal(iso(T.endOf(inDstDay, 'day', CHI)), '2026-11-02T06:00:00.000Z');
  // a day later is the next midnight, not 24 hours later
  assert.equal(iso(T.addPeriod(T.dayStart('2026-03-07', CHI), 'day', 1, CHI)), '2026-03-08T06:00:00.000Z');
  assert.equal(iso(T.addPeriod(T.dayStart('2026-03-08', CHI), 'day', 1, CHI)), '2026-03-09T05:00:00.000Z');
  // a month after 31 January is the last day of February
  assert.equal(T.dayKey(T.addPeriod(T.dayStart('2026-01-31', CHI), 'month', 1, CHI), CHI), '2026-02-28');
  assert.equal(T.dayKey(T.addPeriod(T.dayStart('2026-03-31', CHI), 'month', -13, CHI), CHI), '2025-02-28');
});

test('ISO weeks, including the years with week 53', () => {
  const cases = [
    [[2026, 1, 1], { year: 2026, week: 1 }],
    [[2027, 1, 1], { year: 2026, week: 53 }],
    [[2026, 12, 31], { year: 2026, week: 53 }],
    [[2024, 12, 30], { year: 2025, week: 1 }],
    [[2021, 1, 3], { year: 2020, week: 53 }],
    [[2020, 12, 31], { year: 2020, week: 53 }],
    [[2026, 11, 16], { year: 2026, week: 47 }],
    [[2026, 3, 8], { year: 2026, week: 10 }],
  ];
  for (const [d, w] of cases) assert.deepEqual(T.isoWeek(...d), w, d.join('-'));
  assert.equal(T.isoWeeksInYear(2026), 53);
  assert.equal(T.isoWeeksInYear(2025), 52);
  // the same instant can be in different weeks in different zones
  const sundayEvening = Date.parse('2026-11-15T23:30:00Z');
  assert.equal(T.bucketKey(sundayEvening, 'week', CHI), '2026-W46');
  assert.equal(T.bucketKey(sundayEvening, 'week', BER), '2026-W47');
});

test('bucket keys and buckets follow the zone', () => {
  assert.equal(T.bucketKey(NOW, 'hour', CHI), '2026-11-16T11');
  assert.equal(T.bucketKey(NOW, 'day', CHI), '2026-11-16');
  assert.equal(T.bucketKey(NOW, 'month', CHI), '2026-11');
  assert.equal(T.bucketKey(NOW, 'year', CHI), '2026');
  const late = Date.parse('2026-11-17T04:00:00Z'); // still the 16th in Chicago, the 17th in Berlin
  assert.equal(T.bucketKey(late, 'day', CHI), '2026-11-16');
  assert.equal(T.bucketKey(late, 'day', BER), '2026-11-17');

  const weeks = T.buckets(T.dayStart('2026-10-26', CHI), T.dayStart('2026-11-09', CHI), 'week', CHI);
  assert.deepEqual(weeks.map((b) => [b.key, (b.end_ms - b.start_ms) / H]), [['2026-W44', 169], ['2026-W45', 168]]);
  const days = T.buckets(T.dayStart('2026-10-31', CHI), T.dayStart('2026-11-03', CHI), 'day', CHI);
  assert.deepEqual(days.map((b) => [b.key, (b.end_ms - b.start_ms) / H]), [['2026-10-31', 24], ['2026-11-01', 25], ['2026-11-02', 24]]);
  const months = T.buckets(Date.parse('2026-01-15T00:00:00Z'), Date.parse('2026-04-01T00:00:00Z'), 'month', CHI);
  assert.deepEqual(months.map((b) => b.key), ['2026-01', '2026-02', '2026-03']);
  assert.deepEqual(T.buckets(NOW, NOW, 'day', CHI), []);

  const items = [{ t: Date.parse('2026-11-16T05:30:00Z') }, { t: Date.parse('2026-11-16T06:30:00Z') }, { t: null }];
  const g = T.groupByBucket(items, (x) => x.t, 'day', CHI);
  assert.deepEqual([...g.keys()], ['2026-11-15', '2026-11-16']);
});

test('day keys', () => {
  assert.equal(T.addDays('2026-02-28', 1), '2026-03-01');
  assert.equal(T.addDays('2024-02-28', 1), '2024-02-29');
  assert.equal(T.addDays('2026-01-01', -1), '2025-12-31');
  assert.equal(T.daysBetween('2026-11-01', '2026-11-16'), 15);
  assert.throws(() => T.dayStart('16/11/2026', CHI), RangeError);
});

test('formatting dates, times, durations and how long ago', () => {
  assert.equal(sp(T.formatTime(NOW, CHI)), '11:30 AM');
  assert.equal(sp(T.formatTime(NOW, BER)), '6:30 PM');
  assert.equal(sp(T.formatDate(NOW, CHI, { now: NOW })), 'Mon, Nov 16');
  assert.equal(sp(T.formatDate(NOW, CHI)), 'Mon, Nov 16, 2026');
  assert.equal(sp(T.formatDate(Date.parse('2025-06-01T12:00:00Z'), CHI, { now: NOW })), 'Sun, Jun 1, 2025');
  assert.equal(sp(T.formatDateTime(NOW, CHI, { now: NOW })), 'Mon, Nov 16, 11:30 AM');
  assert.equal(sp(T.formatWhen(NOW - 3 * H, CHI, NOW)), 'Today, 8:30 AM');
  assert.equal(sp(T.formatWhen(NOW - 24 * H, CHI, NOW)), 'Yesterday, 11:30 AM');
  assert.equal(sp(T.formatWhen(NOW + 20 * H, CHI, NOW)), 'Tomorrow, 7:30 AM');
  assert.equal(sp(T.formatWhen(NOW - 5 * T.DAY, CHI, NOW)), 'Wed, Nov 11, 11:30 AM');
  assert.equal(sp(T.formatDayKey('2026-11-16')), 'Mon, Nov 16, 2026');
  assert.equal(sp(T.formatDateRange(T.dayStart('2026-11-01', CHI), T.dayStart('2026-11-17', CHI) - 1, CHI)), 'Nov 1 \u2013 16, 2026');
  assert.equal(T.zoneName(NOW, CHI), 'CST');
  assert.equal(T.zoneName(Date.parse('2026-07-01T12:00:00Z'), CHI), 'CDT');
  setLocale('en-GB');
  assert.equal(sp(T.formatTime(NOW, CHI)), '11:30');
  setLocale('en-US');

  assert.equal(sp(T.formatDuration(45000)), '45 s');
  assert.equal(sp(T.formatDuration(12 * 60000)), '12 min');
  assert.equal(sp(T.formatDuration(7500000)), '2 h 05 min');
  assert.equal(sp(T.formatDuration(2 * H)), '2 h');
  assert.equal(sp(T.formatDuration(3599000)), '1 h', '59 min 59 s rounds to an hour');
  assert.equal(sp(T.formatDuration(90061000)), '1 d 1 h');
  assert.equal(sp(T.formatDuration(725000, { seconds: true })), '12 min 05 s');
  assert.equal(sp(T.formatDuration(7500000, { style: 'long' })), '2 hours 5 minutes');
  assert.equal(sp(T.formatDuration(60000, { style: 'long' })), '1 minute');
  assert.equal(T.formatDuration(7513000, { style: 'clock' }), '2:05:13');
  assert.equal(T.formatDuration(65000, { style: 'clock' }), '1:05');
  assert.equal(sp(T.formatDuration(-90000)), '\u22122 min');
  assert.equal(T.formatDuration(null), '\u2014');

  assert.equal(T.formatRelative(NOW - 20000, NOW), 'just now');
  assert.equal(sp(T.formatRelative(NOW - 5 * 60000, NOW)), '5 min ago');
  assert.equal(sp(T.formatRelative(NOW - 125 * 60000, NOW)), '2 h 5 min ago');
  assert.equal(sp(T.formatRelative(NOW - 9 * H, NOW)), '9 h ago');
  assert.equal(sp(T.formatRelative(NOW - 4 * T.DAY, NOW)), '4 days ago');
  assert.equal(sp(T.formatRelative(NOW + 10 * 60000, NOW)), 'in 10 min');
  assert.equal(T.formatRelative(null, NOW), '\u2014');
});

test('ISO 8601 with the zone offset, for files', () => {
  assert.equal(T.toIsoZoned(NOW, CHI), '2026-11-16T11:30:00-06:00');
  assert.equal(T.toIsoZoned(Date.parse('2026-07-01T12:00:00.250Z'), CHI), '2026-07-01T07:00:00.250-05:00');
  assert.equal(T.toIsoZoned(NOW, 'Asia/Kathmandu'), '2026-11-16T23:15:00+05:45');
  assert.equal(T.toIsoZoned(null, CHI), '');
});

test('period presets resolve against now in the car zone', () => {
  const r = (p, extra) => T.resolvePeriod(p, { now: NOW, tz: CHI, ...extra });
  assert.deepEqual([r('24h').from_ms, r('24h').to_ms], [NOW - 24 * H, NOW]);
  assert.equal(iso(r('7d').from_ms), '2026-11-10T06:00:00.000Z'); // Tuesday 10 November, 00:00 CST
  assert.equal(iso(r({ preset: '30d' }).from_ms), '2026-10-18T05:00:00.000Z'); // 00:00 CDT, before the change
  assert.equal(iso(r('year').from_ms), '2026-01-01T06:00:00.000Z');
  assert.equal(r('all').from_ms, null);
  assert.equal(r('all', { first_ms: 1700000000000 }).from_ms, 1700000000000);
  assert.equal(r('7d').label, 'Last 7 days');
  assert.equal(r('year').label, 'This year');
  // the same preset in another zone starts at that zone's midnight
  assert.equal(iso(T.resolvePeriod('7d', { now: NOW, tz: BER }).from_ms), '2026-11-09T23:00:00.000Z');

  const c = r({ from: T.dayStart('2026-11-01', CHI), to: T.dayStart('2026-11-17', CHI) });
  assert.equal(c.preset, 'custom');
  assert.equal(sp(c.label), 'Nov 1 \u2013 16, 2026');
  const swapped = r({ from: NOW, to: NOW - H });
  assert.deepEqual([swapped.from_ms, swapped.to_ms], [NOW - H, NOW]);
  assert.equal(r({ from: NOW - H, to: null }).to_ms, NOW);
  assert.equal(r('weekly').preset, '30d', 'an unknown preset falls back');
  assert.equal(r(null, { fallback: '7d' }).preset, '7d');
  assert.equal(r({ from: 'yesterday' }).preset, '30d');
  assert.throws(() => T.resolvePeriod('7d', { tz: CHI }), TypeError);

  assert.deepEqual(T.periodParams(r('all')), { to_ms: NOW });
  assert.deepEqual(T.periodParams(r('24h')), { from_ms: NOW - 24 * H, to_ms: NOW });
});

test('moving a period back and forth keeps whole days across a clock change', () => {
  const tz = CHI;
  const nov = T.resolvePeriod({ from: T.dayStart('2026-11-01', tz), to: T.dayStart('2026-11-17', tz) }, { now: NOW, tz });
  const back = T.shiftPeriod(nov, -1, { tz, now: NOW });
  assert.equal(T.dayKey(back.from_ms, tz), '2026-10-16');
  assert.equal(iso(back.from_ms), '2026-10-16T05:00:00.000Z');
  assert.equal(back.to_ms, T.dayStart('2026-11-01', tz));
  assert.equal(sp(back.label), 'Oct 16 \u2013 31, 2026');
  const fwd = T.shiftPeriod(back, 1, { tz, now: NOW });
  assert.deepEqual([fwd.from_ms, fwd.to_ms], [nov.from_ms, nov.to_ms]);

  const week = T.resolvePeriod('7d', { now: NOW, tz });
  const prevWeek = T.shiftPeriod(week, -1, { tz, now: NOW });
  assert.deepEqual([T.dayKey(prevWeek.from_ms, tz), T.dayKey(prevWeek.to_ms - 1, tz)], ['2026-11-03', '2026-11-09']);

  const year = T.resolvePeriod('year', { now: NOW, tz });
  const lastYear = T.shiftPeriod(year, -1, { tz, now: NOW });
  assert.deepEqual([iso(lastYear.from_ms), iso(lastYear.to_ms)], ['2025-01-01T06:00:00.000Z', '2026-01-01T06:00:00.000Z']);

  const hours = T.resolvePeriod('24h', { now: NOW, tz });
  const prevDay = T.shiftPeriod(hours, -1, { tz, now: NOW });
  assert.deepEqual([prevDay.from_ms, prevDay.to_ms], [NOW - 48 * H, NOW - 24 * H]);

  assert.equal(T.shiftPeriod(T.resolvePeriod('all', { now: NOW, tz }), -1, { tz, now: NOW }), null);
});

test('whole-day periods for date inputs', () => {
  const tz = CHI;
  const d = T.dayRange('2026-11-16', '2026-11-01', tz);
  assert.deepEqual([iso(d.from), iso(d.to)], ['2026-11-01T05:00:00.000Z', '2026-11-17T06:00:00.000Z']);
  const r = T.resolvePeriod(d, { now: NOW, tz });
  assert.deepEqual(T.periodDays(r, tz), { from: '2026-11-01', to: '2026-11-16' });
  assert.deepEqual(T.periodDays(T.resolvePeriod('7d', { now: NOW, tz }), tz), { from: '2026-11-10', to: '2026-11-16' });
});

test('an unknown zone falls back to UTC instead of breaking the page', () => {
  const warn = console.warn;
  let warned = 0;
  console.warn = () => { warned++; };
  try {
    assert.equal(T.isValidZone('Mars/Olympus'), false);
    assert.equal(T.dayKey(NOW, 'Mars/Olympus'), '2026-11-16');
    assert.equal(T.zoneOffset(NOW, 'Mars/Olympus'), 0);
    assert.equal(warned, 1);
  } finally { console.warn = warn; }
});

test('where a car zone comes from, in words', () => {
  assert.equal(T.zoneSourceText('owner'), 'set for this car');
  assert.equal(T.zoneSourceText('position'), 'from the car’s position');
  assert.equal(T.zoneSourceText('default'), 'the server’s default');
  assert.equal(T.zoneSourceText(undefined), null);
});

test('hour buckets have one key each when the clocks go back, and the plain key on every other hour', () => {
  const from = Date.parse('2026-11-01T05:00:00Z'); // 00:00 in Chicago; 01:00 comes twice
  const b = T.buckets(from, from + 4 * H, 'hour', CHI);
  assert.deepEqual(b.map((x) => x.key), ['2026-11-01T00', '2026-11-01T01-05:00', '2026-11-01T01-06:00', '2026-11-01T02']);
  for (const x of b) {
    assert.equal(x.end_ms - x.start_ms, H);
    assert.equal(T.bucketKey(x.start_ms + 59 * T.MINUTE, 'hour', CHI), x.key);
  }
  const g = T.groupByBucket([from + 90 * T.MINUTE, from + 150 * T.MINUTE], (x) => x, 'hour', CHI);
  assert.equal(g.size, 2, 'the two 01:00 hours are two groups');
  // the hour the clocks skip in spring needs nothing extra
  const spring = Date.parse('2026-03-08T07:00:00Z');
  assert.deepEqual(T.buckets(spring, spring + 2 * H, 'hour', CHI).map((x) => x.key), ['2026-03-08T01', '2026-03-08T03']);
});

test('hour buckets stay on the wall-clock hour where the clocks change by half an hour', () => {
  const LH = 'Australia/Lord_Howe';
  const wall = (ms) => { const p = T.zonedParts(ms, LH); return `${p.hour}:${String(p.minute).padStart(2, '0')}`; };
  // spring: 02:00 becomes 02:30, so the hour from 02 is half as long
  const spring = T.buckets(Date.parse('2026-10-03T14:00:00Z'), Date.parse('2026-10-03T17:30:00Z'), 'hour', LH);
  assert.deepEqual(spring.map((x) => [x.key.slice(11), wall(x.start_ms), (x.end_ms - x.start_ms) / T.MINUTE]),
    [['00', '0:00', 60], ['01', '1:00', 60], ['02', '2:30', 30], ['03', '3:00', 60], ['04', '4:00', 60]]);
  // autumn: 02:00 becomes 01:30, so the hour from 01 is half as long again
  const autumn = T.buckets(Date.parse('2026-04-04T13:00:00Z'), Date.parse('2026-04-04T17:00:00Z'), 'hour', LH);
  assert.deepEqual(autumn.map((x) => [x.key.slice(11), wall(x.start_ms), (x.end_ms - x.start_ms) / T.MINUTE]),
    [['00', '0:00', 60], ['01', '1:00', 90], ['02', '2:00', 60], ['03', '3:00', 60]]);
  for (const list of [spring, autumn]) {
    for (let i = 1; i < list.length; i++) assert.equal(list[i].start_ms, list[i - 1].end_ms);
    for (const x of list) {
      assert.equal(T.startOf(x.end_ms - 1, 'hour', LH), x.start_ms);
      assert.equal(T.endOf(x.start_ms, 'hour', LH), x.end_ms);
      assert.equal(T.bucketKey(x.end_ms - 1, 'hour', LH), x.key);
    }
  }
});

test('a short duration has one numbering system where the language has digits of its own', () => {
  setLocale('ar-EG');
  const two = T.formatDuration(2 * H + 5 * T.MINUTE);
  const secs = T.formatDuration(12 * T.MINUTE + 5000, { seconds: true });
  setLocale('en-US');
  assert.equal(sp(two), '٢ h ٠٥ min');
  assert.equal(sp(secs), '١٢ min ٠٥ s');
  assert.equal(sp(T.formatDuration(2 * H + 5 * T.MINUTE)), '2 h 05 min');
});

test('a period that starts or ends before 1970, or after the year 2100, is no period: the API takes none', () => {
  assert.deepEqual(T.normalizePeriod({ from: 0, to: T.PERIOD_MAX_MS }), { from: 0, to: T.PERIOD_MAX_MS });
  assert.deepEqual(T.normalizePeriod({ from: NOW, to: null }), { from: NOW, to: null });
  assert.equal(T.normalizePeriod({ from: Date.parse('1900-01-01T00:00:00Z'), to: NOW }), null);
  assert.equal(T.normalizePeriod({ from: -1, to: null }), null);
  assert.equal(T.normalizePeriod({ from: null, to: -1 }), null);
  assert.equal(T.normalizePeriod({ from: NOW, to: T.PERIOD_MAX_MS + 1 }), null);
  assert.equal(T.PERIOD_MAX_MS, Date.parse('2100-01-01T00:00:00Z'));
  // and so a page asked for one shows its usual period
  assert.equal(T.resolvePeriod({ from: -2208960000000, to: NOW }, { now: NOW, tz: CHI }).preset, '30d');
});
