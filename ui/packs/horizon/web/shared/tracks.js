// Tracks on maps: the fixes a map frames. A stray position fix (a GPS jump to another state and back) would make a
// map fit a whole country around a short drive, so the bounds leave out every fix that implies a jump faster than
// any car drives (over 400 km/h from its neighbours). The line itself is still drawn as recorded.
//   dropJumps(coords, t_ms)          -> the fixes that are possible, in order
//   trackBounds(lines, points)       -> [[west, south], [east, north]] around the possible fixes of each line
//                                       ({ coords, t_ms }) and the points (stops, markers), or null
//   lineOf(d)                        -> { coords, t_ms } of a /tracks drive ({ lat, lon, t_ms }) or a GeoJSON
//                                       LineString feature (properties.t_ms)

/** The fastest a fix may move from its neighbours, km/h. */
export const MAX_JUMP_KPH = 400;

const R = 6371008.8;
const rad = (d) => (d * Math.PI) / 180;
/** Metres between two [lon, lat] points (haversine). */
export function distanceM(a, b) {
  const dLat = rad(b[1] - a[1]), dLon = rad(b[0] - a[0]);
  const s = Math.sin(dLat / 2) ** 2 + Math.cos(rad(a[1])) * Math.cos(rad(b[1])) * Math.sin(dLon / 2) ** 2;
  return 2 * R * Math.asin(Math.min(1, Math.sqrt(s)));
}

const ok = (p) => Array.isArray(p) && Number.isFinite(p[0]) && Number.isFinite(p[1]);

/**
 * The fixes of a line (coords [[lon, lat, ...]], t_ms parallel) without those that imply an impossible jump: a fix
 * is left out when it is too fast from both its neighbours (a spike), or, at an end of the line, from its one
 * neighbour while that neighbour is not itself too fast from the next. Without times (t_ms missing or not parallel)
 * the line is kept as it is. Two fixes less than a second apart count as a second apart.
 */
export function dropJumps(coords, t_ms, { maxKph = MAX_JUMP_KPH } = {}) {
  const pts = [];
  for (let i = 0; i < (coords || []).length; i++) if (ok(coords[i])) pts.push([coords[i], t_ms?.[i]]);
  if (!Array.isArray(t_ms) || t_ms.length !== coords.length || pts.length < 2) return pts.map(([c]) => c);
  const maxMs = maxKph / 3.6; // metres per second
  const fast = (a, b) => {
    if (!a || !b) return false;
    const dt = Number.isFinite(a[1]) && Number.isFinite(b[1]) ? Math.max(1, Math.abs(b[1] - a[1]) / 1000) : null;
    return dt != null && distanceM(a[0], b[0]) / dt > maxMs;
  };
  const kept = [];
  for (let i = 0; i < pts.length; i++) {
    const prev = kept[kept.length - 1], next = pts[i + 1];
    let drop;
    if (prev && next) drop = fast(prev, pts[i]) && fast(pts[i], next);
    // an end of the line: only when its neighbour agrees with the next one (two lone fixes are both kept)
    else if (next) drop = !!pts[i + 2] && fast(pts[i], next) && !fast(next, pts[i + 2]);
    else drop = kept.length > 1 && fast(prev, pts[i]) && !fast(kept[kept.length - 2], prev);
    if (!drop) kept.push(pts[i]);
  }
  return kept.map(([c]) => c);
}

/** { coords, t_ms } of a /tracks drive ({ lat: [], lon: [], t_ms: [] }) or a GeoJSON LineString feature. */
export function lineOf(d) {
  if (d?.geometry) return { coords: d.geometry.coordinates || [], t_ms: d.properties?.t_ms || null };
  const lat = d?.lat || [], lon = d?.lon || [];
  return { coords: lat.map((la, i) => [lon[i], la]), t_ms: d?.t_ms || null };
}

/**
 * [[west, south], [east, north]] around the possible fixes of the lines ({ coords, t_ms }, or /tracks drives and
 * GeoJSON features) and the points ([lon, lat]: stops, markers, kept as they are); null when there is nothing.
 */
export function trackBounds(lines = [], points = []) {
  let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity;
  const add = (p) => { if (!ok(p)) return; if (p[0] < w) w = p[0]; if (p[0] > e) e = p[0]; if (p[1] < s) s = p[1]; if (p[1] > n) n = p[1]; };
  for (const l of lines || []) {
    const { coords, t_ms } = l?.coords ? l : lineOf(l);
    for (const p of dropJumps(coords, t_ms)) add(p);
  }
  for (const p of points || []) add(p);
  return w === Infinity ? null : [[w, s], [e, n]];
}
