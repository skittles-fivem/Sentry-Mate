// Sentry Mate UI pack: made-up data shaped like the real JSON API (/api/v1, docs/ui-api.md).
//
// app.js imports only this module, so moving to the real server means replacing this one file
// with one that calls fetch('/api/v1/...') and returns the same shapes.
//
// Units in the API are metric and SI-like (km, km/h, °C, bar, kW, kWh, Wh/km); times are epoch
// milliseconds. The UI converts for display. Nothing here holds "all cars" in a way the real
// server would: lists are paged and totals come from a summary call.
//
// Any number of cars: the car list is paged by offset inside a snapshot. The first page returns
// an `asOf` token and later pages pass it back, so every page comes from one ordering even while
// cars keep reporting (sorting by Last seen would otherwise skip or repeat rows between pages).
// Rows keep the snapshot's order but always carry current values. The fleet summary answers
// `ifNoneMatch` with { notModified: true } when nothing changed, like an HTTP 304.
//
// Test with more cars: add ?cars=200000 to the page address (before the #), up to 1,000,000.
// ?latency=0 removes the simulated network delay.

export const API_VERSION = 1;
export const TIME_ZONE = 'America/Chicago';

const T0 = Date.now();
const params = (() => { try { return new URLSearchParams(location.search); } catch { return new URLSearchParams(); } })();
const CAR_COUNT = Math.max(1, Math.min(1000000, parseInt(params.get('cars'), 10) || 240));
const LATENCY = params.has('latency') ? Math.max(0, +params.get('latency') || 0) : 140;

// ---------------------------------------------------------------- deterministic randomness
function hashStr(s) {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); }
  return h >>> 0;
}
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6D2B79F5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const pick = (r, arr) => arr[Math.floor(r() * arr.length)];
const between = (r, a, b) => a + (b - a) * r();
const round = (v, d = 1) => Math.round(v * 10 ** d) / 10 ** d;
const sleep = (ms) => new Promise((res) => setTimeout(res, ms));
const latency = (r = Math.random) => sleep(LATENCY ? LATENCY * (0.6 + r() * 0.8) : 0);
const coll = new Intl.Collator('en', { numeric: true });

// ---------------------------------------------------------------- time in the car's zone
const zoneFmt = new Intl.DateTimeFormat('en-US', {
  timeZone: TIME_ZONE, year: 'numeric', month: '2-digit', day: '2-digit',
  hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23',
});
function zparts(ms) {
  const o = {};
  for (const p of zoneFmt.formatToParts(ms)) o[p.type] = p.value;
  return { y: +o.year, m: +o.month, d: +o.day, h: +o.hour % 24, mi: +o.minute, s: +o.second };
}
const pad2 = (n) => String(n).padStart(2, '0');
export function dayOf(ms) { const p = zparts(ms); return `${p.y}-${pad2(p.m)}-${pad2(p.d)}`; }
export function zonedTime(day, h = 0, mi = 0, s = 0) {
  const [y, m, d] = day.split('-').map(Number);
  const want = Date.UTC(y, m - 1, d, h, mi, s);
  let t = want;
  for (let k = 0; k < 3; k++) {
    const p = zparts(t);
    t += want - Date.UTC(p.y, p.m - 1, p.d, p.h, p.mi, p.s);
  }
  return t;
}
export function addDays(day, n) {
  const [y, m, d] = day.split('-').map(Number);
  return new Date(Date.UTC(y, m - 1, d + n)).toISOString().slice(0, 10);
}
const dayIndex = (day) => { const [y, m, d] = day.split('-').map(Number); return Math.round(Date.UTC(y, m - 1, d) / 86400000); };
const weekday = (day) => { const [y, m, d] = day.split('-').map(Number); return new Date(Date.UTC(y, m - 1, d)).getUTCDay(); };
const TODAY = dayOf(T0);

// ---------------------------------------------------------------- the owner's corridor (made up)
// A drive route through public San Francisco streets: Ferry Building, The Embarcadero, Bay St,
// Laguna St, Marina Blvd, Mason St (Crissy Field), up to the Golden Gate Bridge Welcome Center.
// [lat, lon, elevation m]
const ROUTE = [
  [37.79537, -122.39403, 3], [37.7972, -122.39531, 3], [37.79893, -122.39697, 3], [37.80088, -122.39892, 3],
  [37.80199, -122.40014, 3], [37.80304, -122.40132, 4], [37.80399, -122.40229, 4], [37.80483, -122.40314, 4],
  [37.80558, -122.40411, 5], [37.80619, -122.40508, 5], [37.80629, -122.40546, 5], [37.80612, -122.40765, 7],
  [37.8059, -122.41001, 9], [37.80565, -122.41259, 11], [37.80533, -122.41508, 13], [37.80471, -122.42001, 14],
  [37.80416, -122.42452, 12], [37.80365, -122.42881, 9], [37.80331, -122.4317, 7], [37.80419, -122.43198, 6],
  [37.80507, -122.43211, 5], [37.80541, -122.4331, 4], [37.80575, -122.43439, 3], [37.80614, -122.43578, 3],
  [37.80627, -122.43696, 3], [37.80627, -122.43825, 3], [37.80599, -122.44104, 3], [37.80565, -122.44372, 3],
  [37.80534, -122.44555, 3], [37.805, -122.44791, 4], [37.80461, -122.45091, 4], [37.80424, -122.45413, 4],
  [37.80397, -122.45627, 4], [37.80365, -122.45842, 4], [37.80332, -122.46057, 5], [37.80304, -122.46271, 5],
  [37.80314, -122.46443, 6], [37.80339, -122.46614, 8], [37.80365, -122.46743, 14], [37.80427, -122.4685, 22],
  [37.80487, -122.46958, 30], [37.80534, -122.47044, 38], [37.80592, -122.47151, 46], [37.80656, -122.4728, 53],
  [37.80707, -122.47387, 60], [37.8075, -122.47483, 66],
];
// cruise speed (m/s) by route index range
const CRUISE = (i) => (i <= 10 ? 14.8 : i <= 18 ? 12.8 : i <= 20 ? 8.5 : i <= 28 ? 15.2 : i <= 36 ? 12 : 12.5);
const PLACES = {
  work: { id: 'work', name: 'Work', detail: 'Ferry Building, The Embarcadero', idx: 0 },
  northbeach: { id: 'northbeach', name: 'North Beach', detail: 'Bay St at Columbus Ave', idx: 13 },
  fortmason: { id: 'fortmason', name: 'Fort Mason', detail: 'Laguna St, Marina', idx: 20 },
  home: { id: 'home', name: 'Home', detail: 'Marina Blvd, San Francisco', idx: 25 },
  crissy: { id: 'crissy', name: 'Crissy Field', detail: 'Mason St, Presidio', idx: 34 },
  ggb: { id: 'ggb', name: 'GG Bridge Welcome Center', detail: 'Lincoln Blvd, Presidio', idx: 45 },
};
const HOME = { name: PLACES.home.name, detail: PLACES.home.detail };

function haversine(a, b) {
  const R = 6371000, toR = Math.PI / 180;
  const dLat = (b[0] - a[0]) * toR, dLon = (b[1] - a[1]) * toR;
  const s = Math.sin(dLat / 2) ** 2 + Math.cos(a[0] * toR) * Math.cos(b[0] * toR) * Math.sin(dLon / 2) ** 2;
  return 2 * R * Math.asin(Math.sqrt(s));
}
function routeSlice(fromIdx, toIdx) {
  const pts = fromIdx <= toIdx ? ROUTE.slice(fromIdx, toIdx + 1) : ROUTE.slice(toIdx, fromIdx + 1).reverse();
  const idx = fromIdx <= toIdx ? pts.map((_, k) => fromIdx + k) : pts.map((_, k) => fromIdx - k);
  const cum = [0];
  for (let k = 1; k < pts.length; k++) cum.push(cum[k - 1] + haversine(pts[k - 1], pts[k]));
  return { pts, idx, cum, length: cum[cum.length - 1] };
}
function alongRoute(slice, s) {
  const { pts, cum } = slice;
  if (s <= 0) return { lat: pts[0][0], lon: pts[0][1], ele: pts[0][2], seg: 0 };
  let k = 1;
  while (k < cum.length - 1 && cum[k] < s) k++;
  const f = Math.min(1, (s - cum[k - 1]) / Math.max(1e-6, cum[k] - cum[k - 1]));
  const a = pts[k - 1], b = pts[k];
  return { lat: a[0] + (b[0] - a[0]) * f, lon: a[1] + (b[1] - a[1]) * f, ele: a[2] + (b[2] - a[2]) * f, seg: k - 1 };
}
/** the first `s` metres of a slice (a drive that is still going) */
function truncSlice(sl, s) {
  let k = 1;
  while (k < sl.cum.length - 1 && sl.cum[k] < s) k++;
  const p = alongRoute(sl, s);
  return {
    pts: sl.pts.slice(0, k).concat([[p.lat, p.lon, p.ele]]),
    idx: sl.idx.slice(0, k).concat([sl.idx[Math.min(k, sl.idx.length - 1)]]),
    cum: sl.cum.slice(0, k).concat([s]),
    length: s,
  };
}

// ---------------------------------------------------------------- cars
const MODELS = [
  { model: 'Model Y', trim: 'Long Range AWD', fullKm: 580, y0: 2021, y1: 2025, w: 30 },
  { model: 'Model 3', trim: 'Long Range AWD', fullKm: 552, y0: 2019, y1: 2025, w: 20 },
  { model: 'Model 3', trim: 'RWD', fullKm: 438, y0: 2021, y1: 2025, w: 15 },
  { model: 'Model Y', trim: 'RWD', fullKm: 515, y0: 2023, y1: 2025, w: 13 },
  { model: 'Model S', trim: 'Long Range', fullKm: 647, y0: 2018, y1: 2024, w: 7 },
  { model: 'Model X', trim: 'Long Range', fullKm: 539, y0: 2018, y1: 2024, w: 7 },
  { model: 'Cybertruck', trim: 'AWD', fullKm: 523, y0: 2024, y1: 2025, w: 8 },
];
const MODEL_W = MODELS.reduce((s, m) => s + m.w, 0);
const PREFIXES = ['Courier', 'Shuttle', 'Field', 'Pool', 'Service', 'Exec', 'Patrol', 'Route'];
const GEO = {
  'Depot 1 · Mission Bay': [37.7706, -122.3915], 'Depot 2 · Hayward': [37.655, -122.118],
  'Depot 3 · Oakland Port': [37.798, -122.306], 'Depot 4 · South San Francisco': [37.6545, -122.399],
  'SFO long-term lot': [37.628, -122.401], 'Customer site · Palo Alto': [37.443, -122.161],
  'Union Square garage': [37.788, -122.4075], 'Salesforce Transit Center': [37.7895, -122.396],
  'Customer site · Redwood City': [37.486, -122.228], 'Moscone Center loading': [37.784, -122.401],
  'Customer site · Berkeley': [37.87, -122.27], 'Driver home · Noe Valley': [37.751, -122.433],
  'Supercharger · Daly City': [37.705, -122.469], 'Supercharger · Mission Bay': [37.768, -122.393],
  'Supercharger · Burlingame': [37.59, -122.346], 'Supercharger · Emeryville': [37.839, -122.293],
  'Supercharger · Fremont': [37.548, -121.988], 'Driver home charger · Bernal Heights': [37.742, -122.416],
  'US-101 N near Brisbane': [37.689, -122.393], 'I-280 S at Daly City': [37.696, -122.47],
  'The Embarcadero at Pier 27': [37.805, -122.403], 'Bay Bridge westbound': [37.804, -122.35],
  'Geary Blvd at Masonic Ave': [37.782, -122.447], 'I-880 N near Hayward': [37.645, -122.112],
  'CA-1 near Pacifica': [37.62, -122.488], 'El Camino Real, San Mateo': [37.563, -122.323],
  'Van Ness Ave at Lombard St': [37.8, -122.424], 'I-580 E near Oakland': [37.815, -122.25],
  '19th Ave at Lincoln Way': [37.765, -122.477], 'Third St at Mission Bay': [37.77, -122.389],
};
// direction of travel for the road places (degrees, 0 = north)
const HEADINGS = {
  'US-101 N near Brisbane': 345, 'I-280 S at Daly City': 165, 'Bay Bridge westbound': 255, 'I-880 N near Hayward': 320,
  'CA-1 near Pacifica': 175, 'I-580 E near Oakland': 95, 'The Embarcadero at Pier 27': 125, 'Van Ness Ave at Lombard St': 180,
  'Geary Blvd at Masonic Ave': 270, '19th Ave at Lincoln Way': 0, 'Third St at Mission Bay': 180, 'El Camino Real, San Mateo': 320,
};
const PLACE_POOL = {
  driving: ['US-101 N near Brisbane', 'I-280 S at Daly City', 'The Embarcadero at Pier 27', 'Bay Bridge westbound',
    'Geary Blvd at Masonic Ave', 'I-880 N near Hayward', 'CA-1 near Pacifica', 'El Camino Real, San Mateo',
    'Van Ness Ave at Lombard St', 'I-580 E near Oakland', '19th Ave at Lincoln Way', 'Third St at Mission Bay'],
  charging: ['Supercharger · Daly City', 'Depot 2 · Hayward', 'Supercharger · Mission Bay', 'Depot 1 · Mission Bay',
    'Supercharger · Burlingame', 'Depot 3 · Oakland Port', 'Supercharger · Emeryville', 'Supercharger · Fremont',
    'Driver home charger · Bernal Heights'],
  parked: ['Depot 1 · Mission Bay', 'Depot 2 · Hayward', 'Depot 3 · Oakland Port', 'SFO long-term lot',
    'Customer site · Palo Alto', 'Union Square garage', 'Salesforce Transit Center', 'Customer site · Redwood City',
    'Moscone Center loading', 'Customer site · Berkeley', 'Driver home · Noe Valley', 'Depot 4 · South San Francisco'],
};
PLACE_POOL.asleep = PLACE_POOL.parked;
PLACE_POOL.unknown = PLACE_POOL.parked;
/** highway: a road or bridge (the map zooms out); street: an address or crossing; site: a named place */
function placeKind(name) {
  if (/^(US-|I-|CA-)|Bridge/.test(name)) return 'highway';
  if (/ at |, /.test(name) && !name.includes('·')) return 'street';
  return 'site';
}

const MIN = 60000, HOUR = 3600000, DAY = 86400000;
const carCache = new Map();

function makeKey(i) {
  const h = hashStr('vehicle-key-' + i).toString(16).padStart(8, '0').toUpperCase();
  return `${h.slice(0, 4)} ${h.slice(4, 8)}`;
}

function ownerCars(i) {
  if (i === 0) {
    const t = dayData('model-y', TODAY);
    const ld = latestDrives('model-y', 1)[0];
    return {
      id: 'model-y', index: 0, name: 'Model Y', key: '7C1E 04AF', model: 'Model Y', trim: 'Long Range AWD', year: 2025,
      fullKm: 580, state: 'parked', stale: false, soc: 71, rangeKm: 0.71 * 580, place: 'Home',
      lagMs: 64 * 1000, lastSeenAt: null, since: ld ? ld.end : T0 - 3 * HOUR,
      todayKm: round(t.drives.reduce((s, d) => s + d.distanceKm, 0), 1), plugged: true, chargeStatus: 'complete', tireWarning: null,
    };
  }
  return {
    id: 'model-3', index: 1, name: 'Model 3', key: '2B90 E5D3', model: 'Model 3', trim: 'Long Range AWD', year: 2021,
    fullKm: 552, state: 'asleep', stale: false, soc: 58, rangeKm: 0.58 * 552, place: 'Home',
    lagMs: 0, lastSeenAt: T0 - 3 * HOUR - 12 * MIN, since: T0 - 3 * HOUR - 12 * MIN, todayKm: 0, plugged: false, tireWarning: null,
  };
}

function carAt(i) {
  let car = carCache.get(i);
  if (car) return car;
  if (i < 2) car = ownerCars(i);
  else {
    const r = rng(hashStr('car-' + i));
    let w = r() * MODEL_W, M = MODELS[0];
    for (const m of MODELS) { if ((w -= m.w) <= 0) { M = m; break; } }
    const year = M.y0 + Math.floor(r() * (M.y1 - M.y0 + 1));
    const x = r();
    const state = x < 0.17 ? 'driving' : x < 0.26 ? 'charging' : x < 0.56 ? 'parked' : x < 0.93 ? 'asleep' : 'unknown';
    const stale = (state === 'driving' || state === 'charging' || state === 'parked') && r() < 0.07;
    // Cars that are awake keep reporting (their last report is always about a minute old);
    // stale, asleep and offline cars stopped at a fixed time and get older.
    let lastSeenAt = null, lagMs = 0;
    if (stale) lastSeenAt = T0 - between(r, 12, 200) * MIN;
    else if (state === 'asleep') lastSeenAt = T0 - between(r, 25, 30 * 60) * MIN;
    else if (state === 'unknown') lastSeenAt = T0 - between(r, 26, 9 * 24) * HOUR;
    else lagMs = between(r, 52, 140) * 1000;
    const seen = lastSeenAt ?? T0 - lagMs;
    let soc = Math.round(state === 'charging' ? between(r, 18, 79) : state === 'driving' ? between(r, 24, 92) : between(r, 22, 90));
    if ((state === 'parked' || state === 'asleep') && r() < 0.05) soc = Math.round(between(r, 6, 19));
    const todayKm = state === 'driving' ? between(r, 15, 230) : state === 'charging' ? between(r, 30, 260)
      : state === 'parked' ? (r() < 0.4 ? 0 : between(r, 4, 120)) : state === 'asleep' ? (r() < 0.6 ? 0 : between(r, 2, 60)) : 0;
    const place = pick(r, PLACE_POOL[state]);
    car = {
      id: 'c' + String(i).padStart(4, '0'), index: i,
      name: `${PREFIXES[hashStr('p' + i) % PREFIXES.length]} ${String(i).padStart(3, '0')}`,
      key: makeKey(i), model: M.model, trim: M.trim, year, fullKm: M.fullKm, state, stale, soc,
      rangeKm: (soc / 100) * M.fullKm * between(r, 0.94, 1.0), place, lagMs, lastSeenAt,
      since: state === 'asleep' || state === 'unknown' ? seen : seen - between(r, 3, 240) * MIN,
      todayKm: round(todayKm, 1),
      plugged: state === 'charging' || (state !== 'driving' && r() < 0.25),
      tireWarning: null,
    };
    if (state === 'driving') car.speedKph = Math.round(between(r, 0, 112));
    if (state === 'charging') car.chargeKw = round(place.startsWith('Supercharger') ? between(r, 38, 190) : between(r, 6.8, 11.4), 1);
    if (state !== 'unknown' && r() < 0.04) car.tireWarning = pick(r, ['fl', 'fr', 'rl', 'rr']);
    if (state === 'driving') car.heading = HEADINGS[place] ?? Math.round(r() * 359);
  }
  carCache.set(i, car);
  return car;
}
const lastSeenOf = (car) => (car.lagMs ? Date.now() - car.lagMs : car.lastSeenAt);

function summary(car) {
  const { index, fullKm, lagMs, lastSeenAt, ...pub } = car;
  return { ...pub, lastSeen: lastSeenOf(car), rangeKm: round(car.rangeKm, 1) };
}
function carIndexById(id) {
  if (id === 'model-y') return 0;
  if (id === 'model-3') return 1;
  const m = /^c(\d{4,7})$/.exec(id || '');
  if (!m) return -1;
  const i = +m[1];
  return i >= 2 && i < CAR_COUNT ? i : -1;
}

// ---------------------------------------------------------------- sorting and search (built once per list)
// state groups as the fleet board counts them: a stale car is counted as stale, not by its last known state
const STATE_ORDER = { driving: 0, charging: 1, parked: 2, asleep: 3, unknown: 4 };
const groupOf = (c) => (c.stale ? 5 : STATE_ORDER[c.state]);
const NAME_PREFIXES = [...PREFIXES, 'Model', 'Model Y'].sort(coll.compare);
const PLACE_RANK = new Map([...new Set([...Object.keys(GEO), 'Home'])].sort(coll.compare).map((p, k) => [p, k]));
const nameKeys = new Map();
function nameKey(i) {
  let k = nameKeys.get(i);
  if (k !== undefined) return k;
  // the same order as a numeric collation of the names ("Pool 9" before "Pool 10"), without comparing strings
  const name = carAt(i).name;
  const m = /^(.*\S)\s+(\d+)$/.exec(name);
  const r = NAME_PREFIXES.indexOf(m ? m[1] : name);
  k = (r < 0 ? NAME_PREFIXES.length : r) * 1e7 + (m ? +m[2] : 0);
  nameKeys.set(i, k);
  return k;
}
const SORT_KEYS = {
  name: null,
  state: groupOf,
  soc: (c) => c.soc,
  lastSeen: (c) => -lastSeenOf(c),
  todayKm: (c) => -c.todayKm,
  place: (c) => PLACE_RANK.get(c.place) ?? 1e6,
};
function inFilter(c, state) {
  if (!state) return true;
  if (state === 'stale') return c.stale;
  if (state === 'driving' || state === 'charging' || state === 'parked') return c.state === state && !c.stale;
  return c.state === state;
}
const norm = (s) => String(s || '').toLowerCase().replace(/[^a-z0-9]+/g, '');
const hayCache = new Map();
function matchesWords(i, c, words, wn) {
  let h = hayCache.get(i);
  if (!h) {
    h = { hay: `${c.name} ${c.model} ${c.trim} ${c.year} ${c.place}`.toLowerCase(), key: norm(c.key), name: norm(c.name) };
    hayCache.set(i, h);
  }
  return words.every((w, k) => h.hay.includes(w) || (wn[k] && (h.key.includes(wn[k]) || h.name.includes(wn[k]))));
}
function buildOrder(state, query, sort, dir) {
  const words = String(query || '').toLowerCase().split(/\s+/).filter(Boolean);
  const wn = words.map(norm);
  const keyFn = SORT_KEYS[sort];
  const ids = [];
  for (let i = 0; i < CAR_COUNT; i++) {
    const c = carAt(i);
    if (!inFilter(c, state)) continue;
    if (words.length && !matchesWords(i, c, words, wn)) continue;
    ids.push(i);
  }
  const n = ids.length;
  const k1 = new Float64Array(n), k2 = new Float64Array(n), perm = new Uint32Array(n);
  for (let j = 0; j < n; j++) { k1[j] = keyFn ? keyFn(carAt(ids[j])) : 0; k2[j] = nameKey(ids[j]); perm[j] = j; }
  perm.sort((a, b) => k1[a] - k1[b] || k2[a] - k2[b]);
  const out = new Int32Array(n);
  for (let j = 0; j < n; j++) out[j] = ids[perm[dir === 'desc' ? n - 1 - j : j]];
  return out;
}
// snapshots: one ordering per list, reused for 60 s by new readers and kept for as long as a reader pages through it
const SNAP_TTL = 60000, SNAP_MAX = 24;
const snaps = new Map();
const snapByKey = new Map();
let snapSeq = 0;
function getSnapshot(key, asOf, build) {
  let s = asOf ? snaps.get(asOf) : null;
  if (s && s.key !== key) s = null;
  if (!s && !asOf) {
    const c = snaps.get(snapByKey.get(key));
    if (c && Date.now() - c.at < SNAP_TTL) s = c;
  }
  if (!s) {
    s = { id: `${(++snapSeq).toString(36)}.${Date.now().toString(36)}`, key, ids: build(), at: Date.now() };
    snapByKey.set(key, s.id);
  }
  snaps.delete(s.id);
  snaps.set(s.id, s);
  while (snaps.size > SNAP_MAX) snaps.delete(snaps.keys().next().value);
  return s;
}

const encodeCursor = (o) => btoa(JSON.stringify(o)).replace(/=+$/, '');
const decodeCursor = (c) => { try { return JSON.parse(atob(c)); } catch { return null; } };

/**
 * GET /api/v1/cars?query=&state=&sort=&dir=&offset=&limit=&asOf=   (or ?cursor=)
 * state: driving | charging | parked (cars with current data in that state) | asleep | unknown (offline) | stale
 * offset lets a virtual list jump straight to any row; asOf (returned by the first page) keeps
 * every page in the same order; cursor continues from the previous page.
 * -> { items: CarSummary[], total, offset, asOf, nextCursor }
 */
export async function listCars({ query = '', state = '', sort = 'name', dir = 'asc', cursor = null, offset = 0, limit = 50, asOf = null } = {}) {
  await latency();
  const c = cursor ? decodeCursor(cursor) : null;
  if (c && c.a) asOf = c.a;
  const start = c ? c.o : Math.max(0, offset | 0);
  limit = Math.max(1, Math.min(500, limit | 0));
  if (!Object.prototype.hasOwnProperty.call(SORT_KEYS, sort)) sort = 'name';
  dir = dir === 'desc' ? 'desc' : 'asc';
  const key = `${state}|${String(query).trim().toLowerCase()}|${sort}|${dir}`;
  const snap = getSnapshot(key, asOf, () => buildOrder(state, query, sort, dir));
  const items = [];
  for (let j = start; j < Math.min(start + limit, snap.ids.length); j++) items.push(summary(carAt(snap.ids[j])));
  const next = start + limit < snap.ids.length ? encodeCursor({ o: start + limit, a: snap.id }) : null;
  return { items, total: snap.ids.length, offset: start, asOf: snap.id, nextCursor: next };
}

/** GET /api/v1/cars/:id -> CarSummary | null */
export async function getCar(carId) {
  await latency();
  const i = carIndexById(carId);
  return i < 0 ? null : summary(carAt(i));
}

let fleetCache = null;
/**
 * GET /api/v1/fleet/summary   (If-None-Match: <etag>)
 * states counts only cars whose data is current; stale cars are counted once, under stale, so
 * driving + charging + parked + asleep + unknown + stale = total. avgSoc covers the cars seen in
 * the last 24 hours (avgSocCars of them).
 * -> { total, states, stale, todayKm, chargingKw, avgSoc, avgSocCars, updatedAt, dataDelaySec, etag }
 *    or { notModified: true, etag, updatedAt } when ifNoneMatch matches
 */
export async function fleetSummary({ ifNoneMatch = null } = {}) {
  await latency();
  if (!fleetCache) {
    const states = { driving: 0, charging: 0, parked: 0, asleep: 0, unknown: 0 };
    let stale = 0, todayKm = 0, chargingKw = 0, socSum = 0, socN = 0;
    const now = Date.now();
    for (let i = 0; i < CAR_COUNT; i++) {
      const c = carAt(i);
      if (c.stale) stale++; else states[c.state]++;
      todayKm += c.todayKm;
      if (now - lastSeenOf(c) < DAY) { socSum += c.soc; socN++; }
      if (c.state === 'charging' && !c.stale) chargingKw += c.chargeKw || 0;
    }
    fleetCache = {
      total: CAR_COUNT, states, stale, todayKm: round(todayKm, 0), chargingKw: round(chargingKw, 0),
      avgSoc: socN ? round(socSum / socN, 0) : null, avgSocCars: socN, dataDelaySec: 60,
    };
    fleetCache.etag = `W/"${hashStr(JSON.stringify(fleetCache)).toString(36)}"`;
  }
  if (ifNoneMatch && ifNoneMatch === fleetCache.etag) return { notModified: true, etag: fleetCache.etag, updatedAt: Date.now() };
  return { ...fleetCache, states: { ...fleetCache.states }, updatedAt: Date.now() };
}

// severity: higher needs someone sooner
function attentionOf(c) {
  const ls = lastSeenOf(c), silent = Date.now() - ls;
  const active = c.state === 'driving' || c.state === 'charging';
  if (c.soc < 10 && c.state !== 'charging') return { kind: 'battery', severity: 6, soc: c.soc, score: 100 - c.soc };
  if (c.stale && active) return { kind: 'stale', severity: 5, since: ls, score: silent };
  if (c.state === 'unknown' && silent > DAY) return { kind: 'offline', severity: 4, since: ls, score: silent };
  if (c.tireWarning) return { kind: 'tire', severity: 3, tire: c.tireWarning, score: 0 };
  if (c.soc < 20 && c.state !== 'charging') return { kind: 'battery', severity: 2, soc: c.soc, score: 100 - c.soc };
  if (c.stale) return { kind: 'stale', severity: 1, since: ls, score: silent };
  return null;
}
let attnCache = null;
/**
 * GET /api/v1/fleet/attention?limit=
 * Cars someone should look at, worst first: data stopped mid-drive or mid-charge, offline for a
 * day or more, a tire warning, a low battery.
 * -> { items: [CarSummary & { attention: { kind, severity, soc?, since?, tire? } }], total }
 */
export async function needsAttention({ limit = 5 } = {}) {
  await latency();
  if (!attnCache || Date.now() - attnCache.at > 30000) {
    const list = [];
    for (let i = 0; i < CAR_COUNT; i++) {
      const a = attentionOf(carAt(i));
      if (a) list.push([i, a]);
    }
    list.sort((x, y) => y[1].severity - x[1].severity || y[1].score - x[1].score);
    attnCache = { list, at: Date.now() };
  }
  const items = attnCache.list.slice(0, Math.max(1, limit | 0)).map(([i, a]) => {
    const { score, ...attention } = a;
    return { ...summary(carAt(i)), attention };
  });
  return { items, total: attnCache.list.length };
}

// ---------------------------------------------------------------- the person's preferences
// The real server keeps these per person, so they follow them to every screen they sign in from.
// Here they live in this browser's localStorage (wrapped: storage can be blocked or full).
// interfaceSize: 'auto' or a percentage. The pack multiplies its Auto root size by it (Auto is the
// standard size on most screens and larger on very large ones), so 100 is the same size as 'auto'.
export const INTERFACE_SIZES = ['auto', 75, 90, 100, 110, 125, 150, 175, 200];
const PREFS_KEY = 'sentrymate.mock.prefs';
const DEFAULT_PREFS = { interfaceSize: 'auto' };
function readPrefs() {
  try {
    const p = JSON.parse(localStorage.getItem(PREFS_KEY) || 'null');
    return { ...DEFAULT_PREFS, ...(p && typeof p === 'object' ? p : {}) };
  } catch { return { ...DEFAULT_PREFS }; }
}
function cleanPrefs(p) {
  return { interfaceSize: INTERFACE_SIZES.includes(p.interfaceSize) ? p.interfaceSize : 'auto' };
}
/** GET /api/v1/me/prefs -> { interfaceSize } */
export async function getPrefs() {
  await latency();
  return cleanPrefs(readPrefs());
}
/**
 * PATCH /api/v1/me/prefs  { interfaceSize }
 * -> the saved prefs. An unknown value is a 400 on the real server; here it falls back to 'auto'.
 */
export async function setPrefs(patch = {}) {
  await latency();
  if ('interfaceSize' in patch && !INTERFACE_SIZES.includes(patch.interfaceSize)) throw new ApiError(400, 'Unknown interface size');
  const next = cleanPrefs({ ...readPrefs(), ...patch });
  try { localStorage.setItem(PREFS_KEY, JSON.stringify(next)); } catch { /* storage blocked: kept for this page only */ }
  return next;
}

/** GET /api/v1/notices -> server notices every pack should show (none right now) */
export async function getNotices() {
  await latency();
  return [];
}

// ---------------------------------------------------------------- drives and charges
const PACK_KWH = 78;
const ANCHOR_DAYS = 120; // the battery is followed day by day from here; older days start fresh
const TRIPS_WEEKEND = [['home', 'ggb'], ['ggb', 'home'], ['home', 'crissy'], ['crissy', 'home'], ['home', 'northbeach'],
  ['northbeach', 'home'], ['home', 'fortmason'], ['fortmason', 'crissy'], ['work', 'ggb'], ['home', 'work']];
const chargeLimit = (carId) => (carId === 'model-y' ? 71 : 80);

function planDay(carId, day) {
  const r = rng(hashStr(carId + day));
  const wd = weekday(day);
  const plan = [];
  if (day === addDays(TODAY, -1) && carId === 'model-y') {
    plan.push(['home', 'work', 10 * 60 + 20], ['work', 'ggb', 14 * 60 + 14], ['ggb', 'home', 15 * 60 + 41], ['home', 'northbeach', 18 * 60 + 52], ['northbeach', 'home', 20 * 60 + 31]);
  } else if (r() < 0.08) {
    // no driving that day
  } else if (wd >= 1 && wd <= 5) {
    plan.push(['home', 'work', 7 * 60 + 50 + Math.floor(r() * 40)]);
    plan.push(['work', 'home', 17 * 60 + 5 + Math.floor(r() * 70)]);
    if (r() < 0.45) {
      const dest = pick(r, ['northbeach', 'fortmason', 'crissy']);
      const t = 19 * 60 + Math.floor(r() * 50);
      plan.push(['home', dest, t], [dest, 'home', t + 45 + Math.floor(r() * 50)]);
    }
  } else {
    const n = 1 + Math.floor(r() * 2);
    let t = 9 * 60 + 40 + Math.floor(r() * 120);
    let at = 'home';
    for (let k = 0; k < n; k++) {
      const choices = TRIPS_WEEKEND.filter(([a]) => a === at);
      const [a, b] = pick(r, choices.length ? choices : TRIPS_WEEKEND);
      plan.push([a, b, t]);
      at = b;
      t += 60 + Math.floor(r() * 150);
    }
    if (at !== 'home') plan.push([at, 'home', t]);
  }
  return plan;
}

/** -> [drive, battery % at the end before rounding] */
function makeDrive(carId, day, k, from, to, startMin, socStart) {
  const r = rng(hashStr(`${carId}|${day}|${k}`));
  const A = PLACES[from], B = PLACES[to];
  const slice = routeSlice(A.idx, B.idx);
  const distanceKm = slice.length / 1000 * between(r, 1.01, 1.04);
  const avgKph = between(r, 25, 33);
  const durationS = Math.round(distanceKm / avgKph * 3600 + between(r, 30, 120));
  const start = zonedTime(day, Math.floor(startMin / 60), startMin % 60, Math.floor(r() * 60));
  const outsideC = round(between(r, 10, 19), 1);
  const effWhKm = between(r, 138, 205) + (outsideC < 12 ? 12 : 0);
  const energyKwh = distanceKm * effWhKm / 1000;
  const socEnd = socStart - energyKwh / PACK_KWH * 100;
  return [{
    id: `${day}-${k}`, carId, start, end: start + durationS * 1000, durationS,
    from: { name: A.name, detail: A.detail }, to: { name: B.name, detail: B.detail },
    fromId: from, toId: to,
    distanceKm: round(distanceKm, 2), energyKwh: round(energyKwh, 2), effWhKm: round(effWhKm, 1),
    socStart: Math.round(socStart), socEnd: Math.round(socEnd), outsideC,
    maxSpeedKph: null,
  }, socEnd];
}
function moveDrive(d, start) { d.start = start; d.end = start + d.durationS * 1000; }

function shouldCharge(carId, day, soc, limit) {
  if (day === TODAY || soc >= limit - 2) return false;
  if (carId === 'model-y') {
    // the owner charged four nights ago and not since: today's morning charge follows three days of driving
    const back = dayIndex(TODAY) - dayIndex(day);
    if (back >= 1 && back <= 3) return false;
    if (back === 4) return true;
  }
  return soc < 45 || (hashStr('ch' + carId + day) % 3 === 0 && soc < limit - 6);
}

const dayCache = new Map();
/**
 * All drives and charges of one local day for one car (oldest first), with the battery at the
 * start and end of the day. Each day starts where the day before ended, less about 1% overnight.
 */
function dayData(carId, day) {
  const ck = carId + '|' + day;
  if (dayCache.has(ck)) return dayCache.get(ck);
  const r = rng(hashStr('soc' + carId + day));
  const socStart = day < addDays(TODAY, -ANCHOR_DAYS)
    ? between(r, 62, 80)
    : dayData(carId, addDays(day, -1)).socEnd - between(r, 0.6, 1.4);
  let out;
  if (carId === 'model-y' && day === TODAY) out = ownerTodayData(socStart);
  else {
    let soc = socStart;
    const drives = [];
    const charges = [];
    let prevEnd = -Infinity;
    planDay(carId, day).forEach(([a, b, t], k) => {
      const [d, end] = makeDrive(carId, day, k + 1, a, b, t, soc);
      if (d.start < prevEnd + 5 * MIN) return; // a planned drive may not start before the last one ended
      drives.push(d);
      soc = end - between(r, 0, 0.3);
      prevEnd = d.end;
    });
    // only what already happened: nothing after now, and nothing after the car's current state
    // began (a car parked, driving, asleep or offline "since 8:05" finished its last drive before that)
    const ci = carIndexById(carId);
    const cutoff = Math.min(T0 - 2 * MIN, ci >= 1 ? carAt(ci).since - MIN : Infinity);
    for (let k = drives.length - 1; k >= 0; k--) if (drives[k].end > cutoff) drives.splice(k, 1);
    const limit = chargeLimit(carId);
    if (shouldCharge(carId, day, soc, limit)) {
      const lastEnd = drives.length ? drives[drives.length - 1].end : zonedTime(day, 18, 0);
      const start = Math.max(lastEnd + 14 * MIN, zonedTime(day, 21, 5));
      const ch = makeCharge(carId, day, start, Math.round(soc), limit);
      if (ch.end <= cutoff) { charges.push(ch); soc = limit; }
    }
    out = { drives, charges, socEnd: soc };
  }
  out.socStart = socStart;
  dayCache.set(ck, out);
  return out;
}

function makeCharge(carId, day, start, socStart, socEnd, opts = {}) {
  const kw = opts.kw || 11.2;
  const addedKwh = (socEnd - socStart) / 100 * PACK_KWH;
  const durationS = Math.round(addedKwh / (kw * 0.93) * 3600);
  const end = start + durationS * 1000;
  const n = 48;
  const curve = Array.from({ length: n }, (_, i) => {
    const f = i / (n - 1);
    const ramp = Math.min(1, f * 14);
    const taper = f > 0.93 ? Math.max(0.05, 1 - (f - 0.93) * 12) : 1;
    return round(kw * ramp * taper * (0.994 + 0.006 * Math.sin(i * 1.3)), 2);
  });
  return {
    id: `${day}-c1`, carId, start, end, durationS, place: { ...HOME },
    kind: 'AC', maxKw: kw, addedKwh: round(addedKwh, 1), usedKwh: round(addedKwh / 0.91, 1),
    socStart, socEnd, curve, status: opts.status || 'complete',
  };
}

/**
 * The owner's Model Y today, built backwards from this morning's charge (it finished 38 minutes
 * ago): Home → Crissy Field, about 45 minutes there, back Home, plugged in 12 minutes later.
 * The battery carries on from yesterday. If the morning is too young for that, today has only the charge.
 */
function ownerTodayData(startSoc) {
  const dayStart = zonedTime(TODAY, 0, 0);
  const chargeEnd = T0 - 38 * MIN;
  const build = (withDrives) => {
    let soc = startSoc;
    const drives = [];
    if (withDrives) {
      const [d1, e1] = makeDrive('model-y', TODAY, 1, 'home', 'crissy', 0, soc);
      const [d2, e2] = makeDrive('model-y', TODAY, 2, 'crissy', 'home', 0, e1 - 0.1);
      drives.push(d1, d2);
      soc = e2;
    }
    const charge = makeCharge('model-y', TODAY, 0, Math.round(soc), 71);
    charge.end = chargeEnd;
    charge.start = chargeEnd - charge.durationS * 1000;
    if (withDrives) {
      const [d1, d2] = drives;
      moveDrive(d2, charge.start - 12 * MIN - d2.durationS * 1000);
      moveDrive(d1, d2.start - 47 * MIN - d1.durationS * 1000);
    }
    return { drives, charges: [charge], socEnd: 71 };
  };
  const full = build(true);
  return full.drives[0].start >= dayStart + 30 * MIN ? full : build(false);
}

function dayRange(from, to) {
  const days = [];
  for (let d = to; d >= from && days.length < 800; d = addDays(d, -1)) days.push(d);
  return days;
}

/**
 * GET /api/v1/cars/:id/drives?from=YYYY-MM-DD&to=YYYY-MM-DD&cursor=&limit=
 * Days are local days in the server's time zone; newest drive first.
 * -> { items, nextCursor, totals: {count, distanceKm, durationS, energyKwh, effWhKm}, perDay: [{day, distanceKm, count}] }
 */
export async function listDrives(carId, { from, to, cursor = null, limit = 30 } = {}) {
  await latency();
  if (carIndexById(carId) < 0) throw new ApiError(404, 'No such car');
  to = to && to < TODAY ? to : TODAY;
  from = from || addDays(to, -29);
  if (from > to) [from, to] = [to, from];
  const all = [];
  const perDay = [];
  for (const day of dayRange(from, to)) {
    const { drives } = dayData(carId, day);
    perDay.push({ day, distanceKm: round(drives.reduce((s, d) => s + d.distanceKm, 0), 2), count: drives.length });
    for (let k = drives.length - 1; k >= 0; k--) all.push(drives[k]);
  }
  perDay.reverse();
  const c = cursor ? decodeCursor(cursor) : null;
  const start = c ? c.o : 0;
  const items = all.slice(start, start + limit);
  const totals = all.reduce((t, d) => {
    t.count++; t.distanceKm += d.distanceKm; t.durationS += d.durationS; t.energyKwh += d.energyKwh; return t;
  }, { count: 0, distanceKm: 0, durationS: 0, energyKwh: 0 });
  totals.effWhKm = totals.distanceKm ? round(totals.energyKwh * 1000 / totals.distanceKm, 1) : null;
  totals.distanceKm = round(totals.distanceKm, 1); totals.energyKwh = round(totals.energyKwh, 1);
  return { items, nextCursor: start + limit < all.length ? encodeCursor({ o: start + limit }) : null, totals, perDay, from, to };
}

export class ApiError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}

/**
 * GET /api/v1/cars/:id/drives/:driveId
 * -> drive summary + stats + samples (columns, same length): t (ms), lat, lon, speedKph, powerKw, soc, elevationM
 * A drive that is still going has inProgress: true and samples up to the newest data.
 */
export async function getDrive(carId, driveId) {
  await latency();
  if (carId === 'model-y' && driveId === `${TODAY}-live`) return simulateDrive(liveDrive());
  const m = /^(\d{4}-\d{2}-\d{2})-(\d+)$/.exec(driveId || '');
  if (!m || carIndexById(carId) < 0) throw new ApiError(404, 'No such drive');
  const d = dayData(carId, m[1]).drives.find((x) => x.id === driveId);
  if (!d) throw new ApiError(404, 'No such drive');
  return simulateDrive(d);
}

// the Now page's "driving" preview: left Home a few minutes after the charge, heading east on Bay St
function liveDrive() {
  const sl = routeSlice(PLACES.home.idx, 0);
  const sAt = sl.cum[9] + 40;
  const start = T0 - 6 * MIN - 20 * 1000, end = T0 - 64 * 1000;
  const distanceKm = sAt / 1000;
  return {
    id: `${TODAY}-live`, carId: 'model-y', start, end, durationS: Math.round((end - start) / 1000), inProgress: true,
    from: { ...HOME }, to: { name: 'Bay St at Van Ness Ave', detail: 'Still driving, heading east' },
    fromId: 'home', toId: null, distanceKm: round(distanceKm, 2), energyKwh: 0.25, effWhKm: round(250 / distanceKm, 1),
    socStart: 70.6, socEnd: 70, outsideC: 14.4, maxSpeedKph: null, slice: truncSlice(sl, sAt),
  };
}

function simulateDrive(drive) {
  const { slice: given, ...d } = drive;
  const slice = given || routeSlice(PLACES[d.fromId].idx, PLACES[d.toId].idx);
  const r = rng(hashStr('sim' + d.id + d.carId));
  // traffic lights and stops along the way (distance in m, dwell in s)
  const stops = [];
  for (let s = 260 + r() * 300; s < slice.length - 200; s += 380 + r() * 700) {
    if (r() < 0.55) stops.push({ s, dwell: 8 + r() * 34 });
  }
  stops.push({ s: d.inProgress ? slice.length + 1e7 : slice.length, dwell: 0 });
  const mass = 2050, g = 9.81;
  let s = 0, v = 0, t = 0, stopK = 0, dwell = 0;
  const sim = [];
  let prevEle = alongRoute(slice, 0).ele;
  while (s < slice.length - 0.5 && t < 7200) {
    const here = alongRoute(slice, s);
    const cruise = CRUISE(slice.idx[Math.min(here.seg, slice.idx.length - 1)]) * (0.92 + 0.1 * Math.sin(t / 37) + 0.06 * Math.sin(t / 11));
    const next = stops[stopK];
    const toStop = next.s - s;
    const v0 = v;
    if (dwell > 0) { v = 0; dwell -= 1; }
    else if (toStop <= (v * v) / (2 * 1.9) + 3) {
      if (toStop <= 1.5) { s = next.s; v = 0; dwell = next.dwell; stopK++; }
      else { const need = (v * v) / (2 * toStop); v = Math.max(Math.min(v - Math.min(3, Math.max(need, 0.3)), toStop), 1.0); }
    } else {
      v = Math.max(0, v + (cruise > v ? Math.min(1.7, cruise - v) : Math.max(-1.5, cruise - v)));
    }
    const a = v - v0;
    s = Math.min(slice.length, s + v);
    const ele = alongRoute(slice, s).ele;
    const grade = v > 0.1 ? (ele - prevEle) / Math.max(v, 0.1) : 0;
    prevEle = ele;
    const wheel = mass * a * v + 0.5 * 1.2 * 0.58 * v ** 3 + 0.0095 * mass * g * v + mass * g * grade * v;
    const batt = (wheel >= 0 ? wheel / 0.88 : Math.max(wheel * 0.72, -68000)) / 1000 + 0.55;
    sim.push({ t, s, v, p: batt, ele });
    t += 1;
    if (stopK >= stops.length) break;
  }
  // scale time to the drive's duration and power to its energy
  const k = d.durationS / Math.max(1, sim.length);
  const n = Math.max(60, Math.min(320, Math.round(d.durationS / 8)));
  const pos = sim.reduce((e, x) => e + Math.max(0, x.p), 0) / 3600;
  const neg = sim.reduce((e, x) => e + Math.min(0, x.p), 0) / 3600;
  const pScale = (d.energyKwh / k - neg) / Math.max(0.001, pos);
  const out = { t: [], lat: [], lon: [], speedKph: [], powerKw: [], soc: [], elevationM: [] };
  let eUsed = 0;
  let maxV = 0, regen = 0, ascent = 0, descent = 0, lastEle = null, maxP = 0, minP = 0;
  for (let i = 0; i < n; i++) {
    const a = Math.floor(i / n * sim.length), b = Math.max(a + 1, Math.floor((i + 1) / n * sim.length));
    const chunk = sim.slice(a, Math.min(b, sim.length));
    const mid = chunk[Math.floor(chunk.length / 2)] || sim[sim.length - 1];
    const vAvg = chunk.reduce((x, c) => x + c.v, 0) / chunk.length;
    const pAvg = chunk.reduce((x, c) => x + (c.p > 0 ? c.p * pScale : c.p), 0) / chunk.length;
    const p = alongRoute(slice, i === n - 1 ? slice.length : mid.s);
    eUsed += pAvg * (chunk.length * k) / 3600;
    if (pAvg < 0) regen += -pAvg * (chunk.length * k) / 3600;
    out.t.push(Math.round(d.start + a * k * 1000));
    out.lat.push(round(p.lat, 6)); out.lon.push(round(p.lon, 6));
    out.speedKph.push(round(vAvg * 3.6 / k, 1));
    out.powerKw.push(round(pAvg, 1));
    out.soc.push(round(d.socStart - eUsed / PACK_KWH * 100, 2));
    out.elevationM.push(round(p.ele, 1));
    maxV = Math.max(maxV, vAvg * 3.6 / k); maxP = Math.max(maxP, pAvg); minP = Math.min(minP, pAvg);
    if (lastEle != null) { const de = p.ele - lastEle; if (de > 0) ascent += de; else descent -= de; }
    lastEle = p.ele;
  }
  const car = carAt(carIndexById(d.carId));
  const kmPerSoc = car.fullKm / 100;
  const socEnd = out.soc[out.soc.length - 1];
  return {
    ...d,
    socStart: Math.round(d.socStart),
    socEnd: Math.round(socEnd),
    stats: {
      maxSpeedKph: round(maxV, 0), avgSpeedKph: round(d.distanceKm / (d.durationS / 3600), 1),
      energyKwh: d.energyKwh, regenKwh: round(regen, 2), effWhKm: d.effWhKm,
      maxPowerKw: round(maxP, 0), maxRegenKw: round(-minP, 0),
      outsideC: d.outsideC, cabinC: round(20.5 + (d.outsideC - 14) * 0.08, 1), batteryC: round(23 + d.distanceKm * 0.25, 1),
      socStart: Math.round(d.socStart), socEnd: Math.round(socEnd),
      rangeStartKm: round(d.socStart * kmPerSoc, 0), rangeEndKm: round(socEnd * kmPerSoc, 0),
      ascentM: round(ascent, 0), descentM: round(descent, 0),
    },
    samples: out,
  };
}

// ---------------------------------------------------------------- now
/**
 * GET /api/v1/cars/:id/now -> the car's newest values with how old they are.
 * (Prototype only: opts.preview = 'parked' | 'driving' | 'charging' picks a made-up scenario for
 * the owner's Model Y. The real endpoint has no such option; the Now page also gets live updates
 * from the server-sent event stream.)
 */
export async function getNow(carId, opts = {}) {
  await latency();
  const i = carIndexById(carId);
  if (i < 0) throw new ApiError(404, 'No such car');
  const car = carAt(i);
  if (i === 0) return ownerNow(opts.preview || 'parked');
  return fleetNow(car);
}

function latestDrives(carId, n = 1) {
  const out = [];
  for (let d = TODAY, k = 0; out.length < n && k < 30; d = addDays(d, -1), k++) {
    const ds = dayData(carId, d).drives;
    for (let j = ds.length - 1; j >= 0 && out.length < n; j--) out.push(ds[j]);
  }
  return out;
}
function latestCharge(carId) {
  for (let d = TODAY, k = 0; k < 30; d = addDays(d, -1), k++) {
    const cs = dayData(carId, d).charges;
    if (cs.length) return cs[cs.length - 1];
  }
  return null;
}
function trace(d) {
  const sl = routeSlice(PLACES[d.fromId].idx, PLACES[d.toId].idx);
  return sl.pts.map((p) => [p[1], p[0]]);
}
function todaySummary(carId) {
  const { drives, charges } = dayData(carId, TODAY);
  return {
    drives: drives.length,
    distanceKm: round(drives.reduce((s, d) => s + d.distanceKm, 0), 2),
    energyKwh: round(drives.reduce((s, d) => s + d.energyKwh, 0), 2),
    chargeAddedKwh: round(charges.reduce((s, c) => s + c.addedKwh, 0), 1),
    charges: charges.length,
  };
}
// the Now page's "charging" preview: plugged in 34 minutes ago at the battery today's charge started from
function chargingPreview() {
  const S = dayData('model-y', TODAY).charges[0].socStart;
  const kw = 11.2;
  const addedKwh = round(kw * 0.93 * (34 / 60) * 0.97, 1);
  const soc = Math.round(S + (addedKwh / PACK_KWH) * 100);
  const minutesToLimit = Math.max(1, Math.round(((71 - soc) / 100) * PACK_KWH / (kw * 0.93) * 60));
  return { startedAt: T0 - 34 * MIN, S, soc, kw, addedKwh, minutesToLimit };
}

function ownerNow(preview) {
  const car = carAt(0);
  const pub = summary(car);
  const home = ROUTE[PLACES.home.idx];
  const charge = dayData('model-y', TODAY).charges[0];
  const ld = latestDrives('model-y', 1)[0];
  const lc = latestCharge('model-y');
  const base = {
    car: pub, stale: false, lastSeen: pub.lastSeen, receivedAt: pub.lastSeen + 58 * 1000,
    odometerKm: 29695.9, lv: { volts: 13.6, ok: true },
    tires: { fl: 2.896, fr: 2.896, rl: 2.827, rr: 2.689, placard: 2.896 },
    limit: 71, fullKm: car.fullKm, packKwh: PACK_KWH,
    today: todaySummary('model-y'),
    latestDrive: ld ? { ...ld, trace: trace(ld) } : null,
    latestCharge: lc,
  };
  if (preview === 'driving') {
    const live = liveDrive();
    const trail = live.slice.pts.map((q) => [q[1], q[0]]);
    const at = trail[trail.length - 1];
    return {
      ...base, state: 'driving', since: live.start,
      soc: 70, rangeKm: 0.70 * car.fullKm,
      temps: { outsideC: 14.4, cabinC: 21.1, batteryC: 26.7 },
      location: { place: 'Bay St at Van Ness Ave', detail: 'San Francisco · heading east', kind: 'street', lat: at[1], lon: at[0], heading: 88, trail },
      drive: { id: live.id, speedKph: 45, powerKw: 16.4, gear: 'D', tripKm: live.distanceKm, energyKwh: live.energyKwh, from: 'Home', start: live.start, trace: trail },
      charge: { plugged: false, status: 'disconnected' },
      today: {
        ...base.today, drives: base.today.drives + 1,
        distanceKm: round(base.today.distanceKm + live.distanceKm, 2), energyKwh: round(base.today.energyKwh + live.energyKwh, 2),
      },
    };
  }
  if (preview === 'charging') {
    const cp = chargingPreview();
    const n = 30;
    const curve = Array.from({ length: n }, (_, i) => round(cp.kw * Math.min(1, (i / (n - 1)) * 18) * (0.994 + 0.006 * Math.sin(i * 1.3)), 2));
    const nowCharge = {
      id: `${TODAY}-c0`, carId: 'model-y', start: cp.startedAt, end: pub.lastSeen, durationS: 34 * 60,
      place: { ...HOME }, kind: 'AC', maxKw: cp.kw, addedKwh: cp.addedKwh, usedKwh: round(cp.addedKwh / 0.91, 1),
      socStart: cp.S, socEnd: cp.soc, curve, status: 'charging',
    };
    return {
      ...base, state: 'charging', since: cp.startedAt,
      soc: cp.soc, rangeKm: (cp.soc / 100) * car.fullKm,
      temps: { outsideC: 13.3, cabinC: 15.6, batteryC: 30.0 },
      location: { place: 'Home', detail: PLACES.home.detail, kind: 'site', lat: home[0], lon: home[1] },
      charge: {
        plugged: true, status: 'charging', kind: 'AC', powerKw: cp.kw, volts: 240, amps: 47,
        addedKwh: cp.addedKwh, rateKph: 54, minutesToLimit: cp.minutesToLimit, startedAt: cp.startedAt, startSoc: cp.S,
      },
      today: { ...base.today, chargeAddedKwh: cp.addedKwh, charges: 1 },
      latestCharge: nowCharge,
    };
  }
  return {
    ...base, state: 'parked', since: ld ? ld.end : car.since,
    soc: 71, rangeKm: car.rangeKm,
    temps: { outsideC: 14.4, cabinC: 17.8, batteryC: 22.2 },
    location: { place: 'Home', detail: PLACES.home.detail, kind: 'site', lat: home[0], lon: home[1] },
    charge: { plugged: true, status: 'complete', kind: 'AC', powerKw: 0, completedAt: charge ? charge.end : car.since },
  };
}

function fleetNow(car) {
  const r = rng(hashStr('now' + car.id));
  const pub = summary(car);
  const atHome = car.place === 'Home';
  const geo = atHome ? ROUTE[PLACES.home.idx] : GEO[car.place] || [37.77, -122.42];
  const ld = latestDrives(car.id, 1)[0];
  const tires = { fl: round(between(r, 2.75, 2.95), 3), fr: round(between(r, 2.75, 2.95), 3), rl: round(between(r, 2.72, 2.93), 3), rr: round(between(r, 2.72, 2.93), 3), placard: 2.896 };
  if (car.tireWarning) tires[car.tireWarning] = round(between(r, 2.3, 2.55), 3);
  return {
    car: pub, state: car.state, stale: car.stale, since: car.since, lastSeen: pub.lastSeen,
    receivedAt: pub.lastSeen + 60 * 1000,
    soc: car.soc, rangeKm: car.rangeKm, limit: 80, fullKm: car.fullKm, packKwh: PACK_KWH,
    temps: { outsideC: round(between(r, 9, 19), 1), cabinC: round(between(r, 14, 23), 1), batteryC: round(between(r, 16, 31), 1) },
    tires,
    lv: { volts: round(between(r, 12.4, 13.8), 1), ok: true },
    odometerKm: round(between(r, 8000, 160000), 1),
    location: { place: car.place, detail: atHome ? PLACES.home.detail : 'San Francisco Bay Area', kind: atHome ? 'site' : placeKind(car.place), lat: geo[0], lon: geo[1], heading: car.state === 'driving' ? car.heading : null },
    charge: car.state === 'charging'
      ? { plugged: true, status: 'charging', kind: car.chargeKw > 20 ? 'DC' : 'AC', powerKw: car.chargeKw, addedKwh: round(between(r, 2, 30), 1), rateKph: round(car.chargeKw * 5.6, 0), minutesToLimit: Math.round(between(r, 12, 90)), startedAt: car.since }
      : { plugged: car.plugged, status: car.plugged ? 'complete' : 'disconnected' },
    drive: car.state === 'driving' ? { speedKph: car.speedKph, powerKw: round(car.speedKph * 0.32, 1), gear: 'D', tripKm: round(between(r, 2, 40), 1), from: null, start: car.since } : null,
    today: { drives: car.todayKm ? 1 + Math.floor(r() * 5) : 0, distanceKm: car.todayKm, energyKwh: round(car.todayKm * 0.16, 1), chargeAddedKwh: car.state === 'charging' ? round(between(r, 3, 30), 1) : 0, charges: car.state === 'charging' ? 1 : 0 },
    latestDrive: ld ? { ...ld, trace: trace(ld) } : null,
    latestCharge: latestCharge(car.id),
  };
}

// ---------------------------------------------------------------- timeline
/**
 * GET /api/v1/cars/:id/timeline?day=YYYY-MM-DD
 * -> { day, start, end, now, receivedUntil, segments: [{kind, start, end, ...}] }
 * kind: asleep | parked (with place: {name, detail}, note?) | driving (title, detail: drive, driveId)
 *     | charging (title, detail: charge, place) | unknown (no log arrived for that time; never counted as sleep)
 * receivedUntil: data for the day has arrived up to here; after it, until now, is still on its way.
 */
export async function getTimeline(carId, day = TODAY, opts = {}) {
  await latency();
  const ci = carIndexById(carId);
  if (ci < 0) throw new ApiError(404, 'No such car');
  const car = carAt(ci);
  const start = zonedTime(day, 0, 0), end = zonedTime(addDays(day, 1), 0, 0);
  const isToday = day === TODAY;
  const nowMs = Date.now();
  const lastData = car.stale || car.state === 'unknown' ? lastSeenOf(car) : nowMs - 64 * 1000;
  const until = isToday ? Math.max(start, Math.min(end, lastData)) : end;
  const { drives, charges } = dayData(carId, day);
  let events = [
    ...drives.map((d) => ({ kind: 'driving', start: d.start, end: d.end, title: `${d.from.name} → ${d.to.name}`, detail: d, driveId: d.id, from: d.from, to: d.to })),
    ...charges.map((c) => ({ kind: 'charging', start: c.start, end: c.end, title: `Charging at ${c.place.name}`, detail: c, place: c.place })),
  ].sort((a, b) => a.start - b.start);
  // prototype only: the Now page's preview states end the day differently
  let tail = null;
  if (isToday && carId === 'model-y' && (opts.preview === 'charging' || opts.preview === 'driving')) {
    const cp = opts.preview === 'charging' ? chargingPreview() : null;
    const from = cp ? cp.startedAt : liveDrive().start;
    events = events.filter((e) => e.end <= from - 5 * MIN && !(cp && e.kind === 'charging'));
    tail = cp
      ? { kind: 'charging', start: from, end: until, title: 'Charging at Home', detail: `AC ${cp.kw} kW · ${cp.S}% → ${cp.soc}% so far`, place: { ...HOME }, inProgress: true }
      : { kind: 'driving', start: from, end: until, title: 'Home → Bay St at Van Ness Ave', detail: 'Drive in progress, heading east on Bay St', driveId: `${TODAY}-live`, inProgress: true, from: { ...HOME } };
  }
  const segs = [];
  const gapSeed = hashStr('gap' + carId + day);
  const gap = carId === 'model-y' && isToday ? { start: zonedTime(day, 2, 41), end: zonedTime(day, 3, 26) }
    : gapSeed % 4 === 0 ? { start: zonedTime(day, 2, 40 + (gapSeed % 15)), end: zonedTime(day, 3, 25) } : null;
  const add = (kind, s, e, place, note) => {
    if (e <= s) return;
    if (gap && kind === 'asleep' && gap.start < e && gap.end > s) {
      add(kind, s, Math.max(s, gap.start), place, note);
      segs.push({ kind: 'unknown', start: Math.max(s, gap.start), end: Math.min(e, gap.end) });
      add(kind, Math.min(e, gap.end), e, place, note);
      return;
    }
    segs.push({ kind, start: s, end: e, place, ...(note ? { note } : {}) });
  };
  // after activity the car stays awake for 15 min, then sleeps; it wakes 10 min before the next one
  const fill = (a, b, place, wakeAtEnd) => {
    if (b <= a) return;
    const wake = wakeAtEnd ? Math.max(a, b - 10 * MIN) : b;
    const sleepAt = a === start ? a : Math.min(wake, a + 15 * MIN);
    add('parked', a, sleepAt, place); add('asleep', sleepAt, wake, place); add('parked', wake, b, place);
  };
  let t = start;
  const first = tail && !events.length ? tail : events[0];
  let here = first ? (first.from || first.place || HOME) : HOME;
  for (const ev of events) {
    if (ev.start >= until) break;
    const evStart = Math.max(ev.start, start);
    fill(t, evStart, here, true);
    const { from, to, ...seg } = ev;
    segs.push({ ...seg, start: evStart, end: Math.min(ev.end, until) });
    t = Math.min(ev.end, until);
    here = to || ev.place || here;
  }
  if (tail) {
    fill(t, tail.start, here, true);
    const { from, ...seg } = tail;
    segs.push(seg);
    t = until;
  }
  if (t < until) {
    if (isToday && carId === 'model-y') segs.push({ kind: 'parked', start: t, end: until, place: here, note: 'plugged in, charge complete' });
    else fill(t, until, here, false);
  }
  const merged = [];
  for (const sg of segs) {
    if (sg.end <= sg.start) continue;
    const prev = merged[merged.length - 1];
    if (prev && prev.kind === sg.kind && (sg.kind === 'parked' || sg.kind === 'asleep') && prev.end >= sg.start && prev.place?.name === sg.place?.name) prev.end = sg.end;
    else merged.push({ ...sg });
  }
  return { day, start, end, now: isToday ? nowMs : null, receivedUntil: until, segments: merged };
}

export const today = () => TODAY;
