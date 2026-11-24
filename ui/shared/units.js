// Sentry Mate shared module: units.js (the person's units, numbers with their unit).
// Canonical source: ui/shared/units.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// The API always sends metric values named by their unit (_km, _kph, _c, _kpa, _kwh, _kw, _wh_per_km,
// _pct, _ms, ...). setUnits() takes the person's preferences (distance km|mi, temperature c|f, pressure
// bar|psi, range rated|ideal) and format(value, kind) turns an API value into text with its unit, in
// the person's units and locale, with a precision chosen per kind. null (unknown) becomes an em dash;
// parts() also gives the reason it is blank (opts.why, or the setMissingText() hook) so a page can show it.

import { formatNumber, numberParts, t } from './i18n.js';
import { formatDuration } from './time.js';

export const NBSP = '\u00a0';
export const DASH = '\u2014';
export const KM_PER_MI = 1.609344;
export const FT_PER_M = 1 / 0.3048;
export const PSI_PER_KPA = 0.14503773773;

export const DEFAULT_UNITS = Object.freeze({ distance: 'km', temperature: 'c', pressure: 'bar', range: 'rated' });
const ALLOWED = { distance: ['km', 'mi'], temperature: ['c', 'f'], pressure: ['bar', 'psi'], range: ['rated', 'ideal'] };

let units = { ...DEFAULT_UNITS };
const listeners = new Set();

/**
 * Use these units from now on. Accepts the /api/v1/me/preferences object ({ units: { distance,
 * temperature, pressure }, range }) or a flat { distance, temperature, pressure, range }. Values that are
 * missing or not allowed keep their current setting. Returns the units in use.
 */
export function setUnits(prefs = {}) {
  const flat = { ...(prefs.units || {}), ...prefs };
  const next = { ...units };
  for (const k of Object.keys(ALLOWED)) if (ALLOWED[k].includes(flat[k])) next[k] = flat[k];
  const changed = Object.keys(next).some((k) => next[k] !== units[k]);
  units = next;
  if (changed) for (const f of [...listeners]) { try { f({ ...units }); } catch (e) { console.error(e); } }
  return { ...units };
}
/** The units in use: { distance, temperature, pressure, range }. */
export const getUnits = () => ({ ...units });
/** f(units) after the units changed; returns an unsubscribe function. */
export function onUnitsChange(f) {
  listeners.add(f);
  return () => listeners.delete(f);
}

/* ------------------------------------------------------------------ kinds */

const mi = (u) => u.distance === 'mi';
const fahr = (u) => u.temperature === 'f';
const psi = (u) => u.pressure === 'psi';
const byMagnitude = (steps) => (v) => { const a = Math.abs(v); for (const [lim, d] of steps) if (a < lim) return d; return 0; };

// Each kind: the API unit it takes, how to convert to the person's unit and back, the unit symbol and
// the default number of decimals (from the converted value). Symbols are not translated.
const KINDS = {
  distance: { from: 'km', to: (v, u) => (mi(u) ? v / KM_PER_MI : v), back: (v, u) => (mi(u) ? v * KM_PER_MI : v), unit: (u) => (mi(u) ? 'mi' : 'km'), digits: byMagnitude([[100, 1]]) },
  odometer: { from: 'km', to: (v, u) => (mi(u) ? v / KM_PER_MI : v), back: (v, u) => (mi(u) ? v * KM_PER_MI : v), unit: (u) => (mi(u) ? 'mi' : 'km'), digits: () => 0 },
  range: { from: 'km', to: (v, u) => (mi(u) ? v / KM_PER_MI : v), back: (v, u) => (mi(u) ? v * KM_PER_MI : v), unit: (u) => (mi(u) ? 'mi' : 'km'), digits: () => 0 },
  length: { from: 'm', to: (v, u) => (mi(u) ? v * FT_PER_M : v), back: (v, u) => (mi(u) ? v / FT_PER_M : v), unit: (u) => (mi(u) ? 'ft' : 'm'), digits: () => 0 },
  speed: { from: 'kph', to: (v, u) => (mi(u) ? v / KM_PER_MI : v), back: (v, u) => (mi(u) ? v * KM_PER_MI : v), unit: (u) => (mi(u) ? 'mph' : 'km/h'), digits: () => 0 },
  range_rate: { from: 'km per hour', to: (v, u) => (mi(u) ? v / KM_PER_MI : v), back: (v, u) => (mi(u) ? v * KM_PER_MI : v), unit: (u) => (mi(u) ? 'mi/h' : 'km/h'), digits: () => 0 },
  temperature: { from: 'c', to: (v, u) => (fahr(u) ? (v * 9) / 5 + 32 : v), back: (v, u) => (fahr(u) ? ((v - 32) * 5) / 9 : v), unit: (u) => (fahr(u) ? '°F' : '°C'), digits: () => 1 },
  temperature_delta: { from: 'c', to: (v, u) => (fahr(u) ? (v * 9) / 5 : v), back: (v, u) => (fahr(u) ? (v * 5) / 9 : v), unit: (u) => (fahr(u) ? '°F' : '°C'), digits: () => 1 },
  pressure: { from: 'kpa', to: (v, u) => (psi(u) ? v * PSI_PER_KPA : v / 100), back: (v, u) => (psi(u) ? v / PSI_PER_KPA : v * 100), unit: (u) => (psi(u) ? 'psi' : 'bar'), digits: (v, u) => (psi(u) ? 0 : 2) },
  energy: { from: 'kwh', unit: () => 'kWh', digits: byMagnitude([[1, 2], [1000, 1]]) },
  power: { from: 'kw', unit: () => 'kW', digits: byMagnitude([[10, 1]]) },
  power_w: { from: 'w', unit: () => 'W', digits: () => 0 },
  efficiency: { from: 'wh_per_km', to: (v, u) => (mi(u) ? v * KM_PER_MI : v), back: (v, u) => (mi(u) ? v / KM_PER_MI : v), unit: (u) => (mi(u) ? 'Wh/mi' : 'Wh/km'), digits: () => 0 },
  percent: { from: 'pct', unit: () => '%', digits: () => 0 },
  voltage: { from: 'v', unit: () => 'V', digits: byMagnitude([[100, 1]]) },
  current: { from: 'a', unit: () => 'A', digits: byMagnitude([[10, 1]]) },
  degrees: { from: 'deg', unit: () => '°', digits: () => 0 },
  count: { from: '', unit: () => '', digits: () => 0 },
  number: { from: '', unit: () => '', digits: byMagnitude([[10, 2], [100, 1]]) },
  // special formatting below
  duration: { from: 'ms', unit: () => '' },
  bytes: { from: 'bytes', unit: () => 'B' },
  money: { from: 'amount', unit: () => '' },
  price_energy: { from: 'amount per kWh', unit: () => '/kWh' },
  price_time: { from: 'amount per minute', unit: () => '/min' },
  money_per_distance: { from: 'amount per 100 km', to: (v, u) => (mi(u) ? v * KM_PER_MI : v), back: (v, u) => (mi(u) ? v / KM_PER_MI : v), unit: (u) => (mi(u) ? 'per 100 mi' : 'per 100 km') },
};
/** Every kind format() knows. */
export const KIND_NAMES = Object.freeze(Object.keys(KINDS));

function kindOrThrow(kind) {
  const k = KINDS[kind];
  if (!k) throw new RangeError(`units.js: unknown kind ${JSON.stringify(kind)}`);
  return k;
}
const cur = (opts) => (opts && opts.units ? { ...units, ...opts.units } : units);

/** An API (metric) value in the person's unit, unrounded; null stays null. */
export function convert(value, kind, opts) {
  const k = kindOrThrow(kind);
  if (value == null || typeof value !== 'number' || !Number.isFinite(value)) return null;
  return k.to ? k.to(value, cur(opts)) : value;
}
/** A value typed in the person's unit back to the API's metric unit (for filters and forms). */
export function toMetric(value, kind, opts) {
  const k = kindOrThrow(kind);
  if (value == null || value === '' || !Number.isFinite(+value)) return null;
  return k.back ? k.back(+value, cur(opts)) : +value;
}
/** The unit symbol for a kind in the person's units: 'mi', 'km/h', '°F', 'bar', 'Wh/mi', '%'. */
export function unitLabel(kind, opts) {
  return kindOrThrow(kind).unit(cur(opts));
}
/** The default number of decimals for a value of this kind (already converted). */
export function digitsFor(kind, displayValue, opts) {
  const k = kindOrThrow(kind);
  return k.digits ? autoDigits(k, displayValue ?? 0, cur(opts)) : 0;
}

/* ------------------------------------------------------------------ the kind of an API field */

const SUFFIXES = [
  ['_wh_per_km', 'efficiency'], ['_per_100km', 'money_per_distance'], ['_odometer_km', 'odometer'], ['odometer_km', 'odometer'],
  ['range_km', 'range'], ['_km', 'distance'], ['_kph', 'speed'], ['_kpa', 'pressure'], ['_kwh', 'energy'], ['_kw', 'power'],
  ['_w', 'power_w'], ['_pct', 'percent'], ['_c', 'temperature'], ['_v', 'voltage'], ['_a', 'current'], ['_deg', 'degrees'],
  ['_m', 'length'], ['_bytes', 'bytes'], ['bytes', 'bytes'],
];
/**
 * The kind for an API field name, from its unit suffix: 'distance_km' -> 'distance', 'end_range_km' ->
 * 'range', 'outside_avg_c' -> 'temperature'. Fields ending in _ms are times or durations, so they give
 * 'duration' only when the name says so (duration_ms, charging_ms, ...); otherwise null.
 */
const DURATION_MS = /(^|_)(duration|charging|plugged|asleep|awake|only|unknown|gap|since_previous|dwell|uptime|read|retry)_ms$/;
export function kindOf(field) {
  const f = String(field);
  if (f.endsWith('_ms')) return DURATION_MS.test(f) ? 'duration' : null;
  for (const [suffix, kind] of SUFFIXES) if (f.endsWith(suffix) || f === suffix.replace(/^_/, '')) return kind;
  return null;
}

/* ------------------------------------------------------------------ the explanation hook for blanks */

let missingText = () => null;
/**
 * The hook that explains a blank figure: fn(kind, opts) -> text or null. A page can still pass its own
 * reason as opts.why ('No reading while the car was asleep'), which always wins.
 */
export function setMissingText(fn) {
  missingText = typeof fn === 'function' ? fn : () => null;
}

/* ------------------------------------------------------------------ formatting */

function signOpt(sign) {
  if (sign === true || sign === 'exceptZero') return 'exceptZero';
  if (sign === 'always') return 'always';
  return 'negative';
}

function numberText(v, digits, opts) {
  const o = { minimumFractionDigits: digits, maximumFractionDigits: digits, signDisplay: signOpt(opts.sign) };
  if (opts.compact && Math.abs(v) >= 100000) Object.assign(o, { notation: 'compact', minimumFractionDigits: 0, maximumFractionDigits: 1 });
  if (opts.grouping === false) o.useGrouping = false;
  return formatNumber(v, o);
}

const BYTE_UNITS = ['B', 'kB', 'MB', 'GB', 'TB', 'PB'];
function moneyOf(value, opts) {
  if (value && typeof value === 'object') return { amount: value.amount ?? value.value ?? null, currency: value.currency ?? opts.currency ?? null };
  return { amount: value, currency: opts.currency ?? null };
}
function currencyText(amount, currency, { min = null, max = null, sign } = {}) {
  const o = { signDisplay: signOpt(sign) };
  if (currency) {
    o.style = 'currency';
    o.currency = currency;
    o.currencyDisplay = 'symbol'; // the locale's own symbol: CA$ and US$ differ where $ alone would not say which
  } else {
    o.minimumFractionDigits = 2;
  }
  if (min != null) o.minimumFractionDigits = min;
  if (max != null) o.maximumFractionDigits = Math.max(max, o.minimumFractionDigits ?? 0);
  try {
    return formatNumber(amount, o);
  } catch {
    // a currency code the browser does not know: the number and the code
    return `${formatNumber(amount, { minimumFractionDigits: 2, maximumFractionDigits: 2, signDisplay: o.signDisplay })}${NBSP}${currency}`;
  }
}

// The kind's decimals for this value; a value that rounds up past a step uses the step's decimals
// (99.97 mi is '100 mi', not '100.0 mi').
function autoDigits(k, v, u) {
  const d = k.digits(Math.abs(v), u);
  const r = Math.round(Math.abs(v) * 10 ** d) / 10 ** d;
  return Math.min(d, k.digits(r, u));
}

function missing(kind, opts, unit) {
  const why = opts.why ?? missingText(kind, opts) ?? null;
  return { value: null, number: DASH, unit, sep: '', text: DASH, missing: true, why };
}

/**
 * The pieces of a formatted value: { value (converted number or null), number (text), unit, sep (the
 * space between them), text (number + sep + unit), missing, why }. opts:
 *   digits    decimals instead of the kind's default
 *   sign      true: '+5' for positive values (for changes); 'always' also '+0'
 *   compact   true: 1.2M for values of 100,000 and more
 *   currency  for money kinds when the value is a plain number
 *   units     override the person's units for this call ({ distance: 'km' })
 *   why       the reason, when the value is null
 */
export function parts(value, kind, opts = {}) {
  const k = kindOrThrow(kind);
  const u = cur(opts);

  if (kind === 'money' || kind === 'price_energy' || kind === 'price_time' || kind === 'money_per_distance') {
    const { amount, currency } = moneyOf(value, opts);
    const unit = kind === 'money' ? '' : k.unit(u);
    if (amount == null || !Number.isFinite(amount)) return missing(kind, opts, unit);
    const conv = k.to ? k.to(amount, u) : amount;
    const per = kind === 'price_energy' || kind === 'price_time';
    const number = currencyText(conv, currency, { min: opts.digits ?? null, max: opts.digits ?? (per ? 3 : null), sign: opts.sign });
    const sep = kind === 'money_per_distance' ? ' ' : '';
    return { value: conv, number, unit: t(unit), sep, text: number + sep + t(unit), missing: false, why: null, currency };
  }

  if (value == null || typeof value !== 'number' || !Number.isFinite(value)) return missing(kind, opts, k.unit(u));

  if (kind === 'duration') {
    const text = formatDuration(value, { style: opts.style ?? 'short', seconds: opts.seconds ?? 'auto' });
    return { value, number: text, unit: '', sep: '', text, missing: false, why: null };
  }

  if (kind === 'bytes') {
    // the unit and the decimals are chosen from the rounded number: 999,999 is '1.0 MB' and 9,960 is '10 kB'
    let v = Math.abs(value), i = 0, d;
    for (;;) {
      d = opts.digits ?? (i === 0 || Number(v.toFixed(1)) >= 10 ? 0 : 1);
      if (Number(v.toFixed(d)) < 1000 || i === BYTE_UNITS.length - 1) break;
      v /= 1000; i++;
    }
    const number = numberText(Math.sign(value) * v, d, opts);
    return { value, number, unit: BYTE_UNITS[i], sep: NBSP, text: number + NBSP + BYTE_UNITS[i], missing: false, why: null };
  }

  const v = k.to ? k.to(value, u) : value;
  const d = opts.digits ?? autoDigits(k, v, u);
  const unit = k.unit(u);

  if (kind === 'percent') {
    // the locale decides the space before % ('45%' in English, '45 %' in German)
    const ps = numberParts(v / 100, { style: 'percent', minimumFractionDigits: d, maximumFractionDigits: d, signDisplay: signOpt(opts.sign) });
    const number = ps.filter((p) => p.type !== 'percentSign' && p.type !== 'literal').map((p) => p.value).join('');
    const text = ps.map((p) => (p.type === 'literal' ? NBSP : p.value)).join('');
    const sep = ps.some((p) => p.type === 'literal') ? NBSP : '';
    return { value: v, number, unit: '%', sep, text, missing: false, why: null };
  }

  const number = numberText(v, d, opts);
  const sep = !unit ? '' : kind === 'degrees' ? '' : NBSP;
  return { value: v, number, unit, sep, text: number + sep + unit, missing: false, why: null };
}

/**
 * An API value as text with its unit, in the person's units: format(12.3, 'distance') -> '7.6 mi'.
 * null -> '—'. Kinds: distance, odometer, range (km); length (m: elevation, accuracy, radius); speed
 * (km/h); range_rate (range added per hour, km/h); temperature, temperature_delta (°C); pressure
 * (kPa); energy (kWh); power (kW); power_w (W); efficiency (Wh/km); percent; voltage; current; degrees;
 * count; number; duration (ms); bytes; money ({ amount, currency } or a number with opts.currency);
 * price_energy (per kWh); price_time (per minute); money_per_distance (per 100 km).
 * opts.unit: false gives the number without its unit (for table cells whose header names the unit).
 */
export function format(value, kind, opts = {}) {
  const p = parts(value, kind, opts);
  return opts.unit === false ? p.number : p.text;
}

/** format() for the field of an object, with the kind worked out from the field name. */
export function formatField(obj, field, opts = {}) {
  const kind = opts.kind ?? kindOf(field) ?? 'number';
  return format(obj ? obj[field] : null, kind, opts);
}

/** Money: formatMoney(12.4, 'EUR') -> '€12.40'. A missing currency gives the bare amount. */
export const formatMoney = (amount, currency, opts = {}) => format(amount, 'money', { ...opts, currency });

/**
 * Amounts in several currencies, never added together: '€12.40 + $3.10'. Takes [{ currency, amount }]
 * or { EUR: 12.4, USD: 3.1 }. Nothing to show -> '—'.
 */
export function formatMoneyList(list, opts = {}) {
  const items = Array.isArray(list) ? list : list && typeof list === 'object' ? Object.entries(list).map(([currency, amount]) => ({ currency, amount })) : [];
  const shown = items.filter((x) => x && x.amount != null && Number.isFinite(x.amount)).map((x) => formatMoney(x.amount, x.currency, opts));
  return shown.length ? shown.join(' + ') : DASH;
}

/* ------------------------------------------------------------------ rated or ideal range */

/** 'Rated range' or 'Ideal range', from the person's choice. */
export const rangeLabel = () => (units.range === 'ideal' ? t('Ideal range') : t('Rated range'));

/**
 * The field to read for a range in the person's choice: 'range_km' -> 'ideal_range_km' when ideal is
 * chosen; 'start_range_km' -> 'start_ideal_range_km'.
 */
export function rangeField(field = 'range_km') {
  return units.range === 'ideal' ? String(field).replace(/range_km$/, 'ideal_range_km') : field;
}

/**
 * The range value from an API object in the person's choice of rated or ideal. When the object has no
 * ideal twin of the field, the field itself is used (the server then already follows the preference).
 */
export function rangeValue(obj, field = 'range_km') {
  if (!obj) return null;
  const f = rangeField(field);
  return f in obj ? obj[f] : obj[field] ?? null;
}
