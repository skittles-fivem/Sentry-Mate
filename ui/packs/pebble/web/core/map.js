// Sentry Mate, "Pebble" UI pack: map tiles on MapLibre.
//
// mapTile(ctx, box, opts) draws a soft map inside a rounded box: the tiles from the server's map settings
// (GET /api/v1/maps/config: offline tiles first when installed, else the online tile server the owner turned on,
// else none, only the routes and pins on a plain ground), routes in the pack's series colours and round pins.
// Light and dark have their own look (the tiles are calmed down, never tinted neon). Nothing else is fetched
// from outside. MapLibre loads on first use from vendor/maplibre-gl, its worker from the same folder.
//
// Line widths and padding are given in px at the standard size and follow the Interface size; a new theme
// repaints the ground, the tiles and the lines.

import { h, cssVar } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { display } from './display.js';
import { isDark, onTheme } from './prefs.js';

let lib = null;
/** Load MapLibre once (module, worker and its style sheet). */
export function loadMapLibre() {
  if (!lib) {
    lib = (async () => {
      if (!document.querySelector('link[data-maplibre]')) {
        const link = h('link', { rel: 'stylesheet', href: '/vendor/maplibre-gl/maplibre-gl.css', dataset: { maplibre: '' } });
        const done = new Promise((r) => { link.addEventListener('load', r, { once: true }); link.addEventListener('error', r, { once: true }); });
        document.head.append(link);
        await done;
      }
      const ml = await import('/vendor/maplibre-gl/maplibre-gl.mjs');
      ml.setWorkerUrl(new URL('/vendor/maplibre-gl/maplibre-gl-worker.mjs', location.origin).href);
      return ml;
    })();
    lib.catch(() => { lib = null; });
  }
  return lib;
}

/** The server's map settings, kept for 5 minutes. */
export const mapConfig = () => api.get('maps/config', null, { maxAgeMs: 300000 });

// A page keeps the Content-Security-Policy and Referrer-Policy it was loaded with. After an admin turns online tiles
// on or picks another tile source, an open page still blocks the new tile server (and sends it no Referer), so its
// maps would stay blank until a reload: the first tile the page's policy blocks loads the page again, once, so it
// gets the server's current policy. Not again for the same server within two minutes (then the server leaves it out).
let tileOrigin = null, policyWatched = false;
export function watchTilePolicy(template) {
  try { tileOrigin = template ? new URL(template.replace(/\{[^}]*\}/g, '0')).origin : null; } catch { tileOrigin = null; }
  if (!tileOrigin || policyWatched) return;
  policyWatched = true;
  document.addEventListener('securitypolicyviolation', (e) => {
    if (!tileOrigin || !String(e.blockedURI || '').startsWith(tileOrigin)) return;
    try {
      const last = JSON.parse(sessionStorage.getItem('sm-tile-policy-reload') || 'null');
      if (last && last.origin === tileOrigin && Date.now() - last.at < 120000) return;
      sessionStorage.setItem('sm-tile-policy-reload', JSON.stringify({ origin: tileOrigin, at: Date.now() }));
    } catch { return; } // no session storage: nothing would stop a reload loop
    location.reload();
  });
}
/** Where the tiles come from: { url, attribution, maxZoom, offline }, or null when there are none. */
export function tileSource(cfg) {
  const t = cfg && cfg.tiles;
  if (!t) return null;
  if (t.offline_url && t.offline_data_present !== false && t.mode === 'offline') return { url: t.offline_url, attribution: t.attribution || '', maxZoom: t.max_zoom || 16, offline: true };
  if (t.mode === 'online' && t.online_template) return { url: t.online_template, attribution: t.attribution || '', maxZoom: t.max_zoom || 19, offline: false };
  if (t.offline_url) return { url: t.offline_url, attribution: t.attribution || '', maxZoom: t.max_zoom || 16, offline: true };
  return null;
}

const paint = (dark) => (dark
  ? { 'raster-saturation': -0.85, 'raster-brightness-min': 0.62, 'raster-brightness-max': 0.12, 'raster-contrast': 0.05, 'raster-opacity': 0.95, 'raster-fade-duration': 0 }
  : { 'raster-saturation': -0.55, 'raster-contrast': -0.1, 'raster-brightness-min': 0.12, 'raster-opacity': 0.9, 'raster-fade-duration': 0 });
const tilePx = () => (display.scale >= 1.2 ? Math.round(256 * display.scale) : 256);

/** [[west, south], [east, north]] around [lon, lat] points (null without points). */
export function boundsOf(coords) {
  let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity;
  for (const c of coords) {
    if (!c || !Number.isFinite(c[0]) || !Number.isFinite(c[1])) continue;
    w = Math.min(w, c[0]); e = Math.max(e, c[0]); s = Math.min(s, c[1]); n = Math.max(n, c[1]);
  }
  if (!Number.isFinite(w)) return null;
  if (e - w < 0.002) { w -= 0.002; e += 0.002; }
  if (n - s < 0.002) { s -= 0.002; n += 0.002; }
  return [[w, s], [e, n]];
}

/** '37.7508° N, 122.4330° W' */
export const coordsText = (lon, lat) => `${Math.abs(lat).toFixed(4)}° ${lat >= 0 ? 'N' : 'S'}, ${Math.abs(lon).toFixed(4)}° ${lon >= 0 ? 'E' : 'W'}`;

const ROUND = { 'line-cap': 'round', 'line-join': 'round' };

/**
 * A map in box: mapTile(ctx, box, { center: [lon, lat], zoom, bounds, padding, interactive, fullscreen, maxZoom }).
 * Resolves with { map, ready, route(id, coords, { tone, dashed }), routes(id, lines, tone), pin(lngLat, { tone, label,
 * onclick }), circle(id, lngLat, radius_m), frame(bounds), noTiles } or null when the page was left meanwhile.
 * ready resolves once the map has drawn (or after 12 s), so a page can say it is ready after its map.
 */
export async function mapTile(ctx, box, { center = null, zoom = 13, bounds = null, padding = 36, interactive = true, fullscreen = false, maxZoom = 16 } = {}) {
  box.classList.add('map', 'is-loading');
  let ml, cfg;
  try { [ml, cfg] = await Promise.all([loadMapLibre(), mapConfig().catch(() => null)]); } catch (err) {
    box.classList.remove('is-loading');
    box.append(h('p', { class: 'map-note' }, 'The map could not start in this browser.'));
    return { map: null, ready: Promise.resolve(), route() {}, routes() {}, pin() {}, circle() {}, frame() {}, noTiles: true, failed: err };
  }
  if (ctx.signal.aborted) return null;
  const src = tileSource(cfg);
  watchTilePolicy(src && !src.offline ? src.url : null);
  const pad = () => Math.round(padding * display.scale);
  let tiles = tilePx();
  const source = () => ({ type: 'raster', tiles: [src.url], tileSize: tiles, maxzoom: src.maxZoom, attribution: src.attribution });
  const map = new ml.Map({
    container: box,
    style: {
      version: 8,
      sources: src ? { tiles: source() } : {},
      layers: [{ id: 'ground', type: 'background', paint: { 'background-color': cssVar('--map-ground') } },
        ...(src ? [{ id: 'tiles', type: 'raster', source: 'tiles', paint: paint(isDark()) }] : [])],
    },
    center: center || [0, 0], zoom, bounds: bounds || undefined, fitBoundsOptions: { padding: pad(), maxZoom },
    attributionControl: { compact: false }, refreshExpiredTiles: false, // the credit stays in view, as the tile policies ask
    scrollZoom: false, dragRotate: false, pitchWithRotate: false, touchPitch: false, cooperativeGestures: false,
    interactive, fadeDuration: 0, maxZoom: 18, maxCanvasSize: [8192, 8192],
  });
  map.touchZoomRotate.disableRotation();
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  if (fullscreen) map.addControl(new ml.FullscreenControl({ container: box.closest('.tile') || box }), 'top-right');
  if (!src) box.append(h('p', { class: 'map-note' }, 'No map tiles: offline maps are not installed and online tiles are off.'));
  const lines = new Map(); // layer id -> { width, token }
  let framing = bounds;
  let moved = false;
  map.on('dragstart', () => { moved = true; });
  map.on('zoomstart', (e) => { if (e.originalEvent) moved = true; });
  const reframe = () => { if (framing && !moved) map.fitBounds(framing, { padding: pad(), maxZoom, animate: false }); };
  map.on('resize', reframe);
  const stopSize = display.onChange((change) => {
    if (change.dpr) map.resize();
    if (!change.rem) return;
    for (const [id, l] of lines) if (map.getLayer(id)) map.setPaintProperty(id, 'line-width', l.width * display.scale);
    if (src && tilePx() !== tiles && map.isStyleLoaded() && map.getSource('tiles')) {
      tiles = tilePx();
      const ids = map.getStyle().layers.map((l) => l.id);
      const next = ids[ids.indexOf('tiles') + 1];
      map.removeLayer('tiles'); map.removeSource('tiles');
      map.addSource('tiles', source());
      map.addLayer({ id: 'tiles', type: 'raster', source: 'tiles', paint: paint(isDark()) }, next);
    }
    reframe();
  });
  const stopTheme = onTheme(() => {
    if (!map.isStyleLoaded()) return;
    map.setPaintProperty('ground', 'background-color', cssVar('--map-ground'));
    if (map.getLayer('tiles')) for (const [k, v] of Object.entries(paint(isDark()))) map.setPaintProperty('tiles', k, v);
    for (const [id, l] of lines) if (map.getLayer(id)) map.setPaintProperty(id, l.prop || 'line-color', cssVar(l.token));
  });
  let timer = 0;
  ctx.onCleanup(() => { clearTimeout(timer); stopSize(); stopTheme(); map.remove(); });
  let failedTiles = 0;
  map.on('error', (e) => {
    if (!(e && (e.tile || e.sourceId === 'tiles'))) return;
    if (++failedTiles === 1) box.append(h('p', { class: 'map-note' }, 'Map tiles did not load; the routes and pins still show.'));
  });
  const ready = new Promise((resolve) => {
    let done = false;
    const finish = () => { clearTimeout(timer); if (!done) { done = true; box.classList.remove('is-loading'); resolve(); } };
    map.once('idle', finish);
    timer = setTimeout(finish, 12000);
  });
  const loaded = new Promise((resolve) => { if (map.loaded()) resolve(); else map.once('load', resolve); });
  function line(layer, width, token) {
    lines.set(layer.id, { width, token });
    map.addLayer({ ...layer, type: 'line', layout: ROUND, paint: { ...(layer.paint || {}), 'line-color': cssVar(token), 'line-width': width * display.scale } });
  }
  const fc = (list) => ({ type: 'FeatureCollection', features: list.filter((c) => c && c.length > 1).map((c) => ({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: c } })) });
  return {
    map, ml, ready, loaded, noTiles: !src,
    /** One route line with a light casing: tone 'c1' (coral), 'c2' (sky, dashed when asked) or 'c3' (teal). */
    async route(id, coords, { tone = 'c1', dashed = false, width = 4 } = {}) {
      await loaded;
      if (ctx.signal.aborted) return;
      map.addSource(id, { type: 'geojson', data: fc([coords]) });
      line({ id: `${id}-case`, source: id }, width + 3, '--map-case');
      line({ id: `${id}-line`, source: id, paint: dashed ? { 'line-dasharray': [1.6, 1.8] } : {} }, width, `--${tone}`);
    },
    /** Many lines as one layer (every drive of a year): lighter and thinner. */
    async routes(id, list, tone = 'c1') {
      await loaded;
      if (ctx.signal.aborted) return;
      if (map.getSource(id)) { map.getSource(id).setData(fc(list)); return; }
      map.addSource(id, { type: 'geojson', data: fc(list) });
      line({ id: `${id}-line`, source: id, paint: { 'line-opacity': 0.75 } }, 2.5, `--${tone}`);
    },
    /** A place's circle (radius in metres) as a soft disc, drawn as a polygon. */
    async circle(id, [lon, lat], radius, tone = 'c3') {
      await loaded;
      if (ctx.signal.aborted) return;
      const pts = [];
      for (let i = 0; i <= 48; i++) {
        const a = (i / 48) * 2 * Math.PI;
        pts.push([lon + (radius / (111320 * Math.cos((lat * Math.PI) / 180))) * Math.cos(a), lat + (radius / 110540) * Math.sin(a)]);
      }
      const data = { type: 'Feature', properties: {}, geometry: { type: 'Polygon', coordinates: [pts] } };
      if (map.getSource(id)) { map.getSource(id).setData(data); return; }
      map.addSource(id, { type: 'geojson', data });
      lines.set(`${id}-fill`, { width: 0, token: `--${tone}`, prop: 'fill-color' });
      map.addLayer({ id: `${id}-fill`, type: 'fill', source: id, paint: { 'fill-color': cssVar(`--${tone}`), 'fill-opacity': 0.18 } });
      line({ id: `${id}-edge`, source: id }, 2, `--${tone}`);
    },
    /** A round pin at [lon, lat]: tone 'coral' | 'teal' | 'sky' | 'sun' | 'muted', with a label beside it. */
    pin(lngLat, { tone = 'coral', label = null, title = null, onclick = null, cover = null } = {}) {
      const el = h(onclick ? 'button' : 'div', { class: ['pin', `tone-${tone}`], type: onclick ? 'button' : null, title, 'aria-label': onclick ? (title || label) : null, onclick, cover },
        h('span', { class: 'pin-dot', 'aria-hidden': 'true' }), label ? h('span', { class: 'pin-label' }, label) : null);
      return new ml.Marker({ element: el, anchor: 'center' }).setLngLat(lngLat).addTo(map);
    },
    /** Frame other bounds, now and after each resize (until the person moves the map). */
    frame(b) { framing = b; moved = false; reframe(); },
  };
}
