// Tests for csv.js: escaping, formula guard, columns with units, columnar data, file names.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import * as C from '../csv.js';
import { setUnits } from '../units.js';
import { setLocale } from '../i18n.js';

beforeEach(() => {
  setLocale('en-US');
  setUnits({ distance: 'km', temperature: 'c', pressure: 'bar' });
});

test('fields are quoted only when they need it, quotes doubled', () => {
  assert.equal(C.csvCell('plain'), 'plain');
  assert.equal(C.csvCell('a,b'), '"a,b"');
  assert.equal(C.csvCell('say "hi"'), '"say ""hi"""');
  assert.equal(C.csvCell('two\nlines'), '"two\nlines"');
  assert.equal(C.csvCell('cr\rhere'), '"cr\rhere"');
  assert.equal(C.csvCell(' leading'), '" leading"');
  assert.equal(C.csvCell('trailing '), '"trailing "');
  assert.equal(C.csvCell('Café Zürich'), 'Café Zürich');
  assert.equal(C.csvCell(''), '');
});

test('values: numbers plain with a dot, blanks empty, dates ISO, objects JSON', () => {
  assert.equal(C.csvCell(1234.5), '1234.5');
  assert.equal(C.csvCell(-3), '-3');
  assert.equal(C.csvCell(0), '0');
  assert.equal(C.csvCell(null), '');
  assert.equal(C.csvCell(undefined), '');
  assert.equal(C.csvCell(NaN), '');
  assert.equal(C.csvCell(Infinity), '');
  assert.equal(C.csvCell(true), 'true');
  assert.equal(C.csvCell(false), 'false');
  assert.equal(C.csvCell(new Date(Date.UTC(2026, 10, 16, 17, 30))), '2026-11-16T17:30:00.000Z');
  assert.equal(C.csvCell({ a: 1, b: 'x' }), '"{""a"":1,""b"":""x""}"');
  assert.equal(C.csvCell('9007199254740993'), '9007199254740993', 'big ids stay text, all digits kept');
});

test('text a spreadsheet would run as a formula gets an apostrophe; numbers in text do not', () => {
  assert.equal(C.csvCell('=SUM(A1:A9)'), "'=SUM(A1:A9)");
  assert.equal(C.csvCell('+1 555 0100'), "'+1 555 0100");
  assert.equal(C.csvCell('-cmd'), "'-cmd");
  assert.equal(C.csvCell('@home'), "'@home");
  assert.equal(C.csvCell('\tTab'), "'\tTab");
  assert.equal(C.csvCell('=HYPERLINK("x","y")'), '"\'=HYPERLINK(""x"",""y"")"');
  assert.equal(C.csvCell('-12.5'), '-12.5');
  assert.equal(C.csvCell('+3'), '+3');
  assert.equal(C.csvCell('1e-5'), '1e-5');
});

test('rows to CSV: header, CRLF, BOM, objects or arrays', () => {
  const rows = [{ name: 'Home, garage', kwh: 12.5 }, { name: 'Work "B"', kwh: null }];
  assert.equal(C.toCsv(rows), 'name,kwh\r\n"Home, garage",12.5\r\n"Work ""B""",\r\n');
  assert.equal(C.toCsv(rows, ['kwh']), 'kwh\r\n12.5\r\n\r\n');
  assert.equal(C.toCsv(rows, [{ label: 'Place', key: 'name' }, { label: 'Twice', value: (r) => (r.kwh == null ? null : r.kwh * 2) }], { eol: '\n' }),
    'Place,Twice\n"Home, garage",25\n"Work ""B""",\n');
  assert.equal(C.toCsv([[1, 'a'], [2, 'b,c']], ['n', 's']), 'n,s\r\n1,a\r\n2,"b,c"\r\n');
  assert.equal(C.toCsv(rows, null, { header: false }), '"Home, garage",12.5\r\n"Work ""B""",\r\n');
  assert.ok(C.toCsv(rows, null, { bom: true }).startsWith('\ufeffname,kwh'));
  assert.equal(C.toCsv([]), '');
});

test('unit columns convert and name the unit; time columns are ISO in the zone', () => {
  const rows = [{ t_ms: Date.parse('2026-11-16T17:30:00Z'), distance_km: 16.09344, outside_c: 20, soc_pct: 81 }];
  const cols = [
    C.timeColumn('Time', 't_ms', 'America/Chicago'),
    C.column('Distance', 'distance_km', 'distance'),
    C.column('Outside', 'outside_c', 'temperature'),
    C.column('Battery', 'soc_pct', 'percent'),
    C.column('Note', () => 'n/a'),
  ];
  assert.equal(C.toCsv(rows, cols), 'Time,Distance (km),Outside (°C),Battery (%),Note\r\n2026-11-16T11:30:00-06:00,16.0934,20,81,n/a\r\n');
  setUnits({ distance: 'mi', temperature: 'f' });
  const cols2 = [C.column('Distance', 'distance_km', 'distance'), C.column('Outside', 'outside_c', 'temperature')];
  assert.equal(C.toCsv(rows, cols2), 'Distance (mi),Outside (°F)\r\n10,68\r\n');
  assert.equal(C.toCsv([{ distance_km: null }], [C.column('Distance', 'distance_km', 'distance')]), 'Distance (mi)\r\n\r\n');
});

test('columnar series data becomes rows', () => {
  const data = { t_ms: [1000, 2000, 3000], values: { soc_pct: [80, null, 79], speed_kph: [0, 12] }, bucket_ms: 1000 };
  assert.deepEqual(C.rowsFromColumns(data, { state: ['parked', 'driving', 'driving'] }), [
    { t_ms: 1000, soc_pct: 80, speed_kph: 0, state: 'parked' },
    { t_ms: 2000, soc_pct: null, speed_kph: 12, state: 'driving' },
    { t_ms: 3000, soc_pct: 79, speed_kph: null, state: 'driving' },
  ]);
  assert.deepEqual(C.rowsFromColumns(null), []);
});

test('file names are safe and readable', () => {
  assert.equal(C.csvFilename('Model Y', 'drives', '2026-11-01', 'to', '2026-11-16'), 'model-y-drives-2026-11-01-to-2026-11-16.csv');
  assert.equal(C.csvFilename('Zürich / Genève', '../etc'), 'zurich-geneve-..-etc.csv');
  assert.equal(C.csvFilename('', null), 'data.csv');
  assert.ok(C.csvFilename('x'.repeat(500)).length <= 124);
  // without a document, downloadCsv only builds the text
  assert.equal(C.downloadCsv('a.csv', [{ a: 1 }]), '\ufeffa\r\n1\r\n');
});

test('a time column gives an empty cell for a time that is not a number', () => {
  const col = C.timeColumn('t', 't', 'UTC');
  assert.equal(C.toCsv([{ t: NaN }, { t: 'x' }, { t: Infinity }, { t: null }, { t: 0 }], [col], { header: false, eol: '\n' }), '\n\n\n\n1970-01-01T00:00:00+00:00\n');
});
