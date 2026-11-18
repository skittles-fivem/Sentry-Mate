// Sentry Mate, "Logbook" UI pack: maps on MapLibre.
//
// makeMap(ctx, el, { center, zoom, bounds }) draws a muted map in el. The tiles come from the server's map
// settings (GET /api/v1/maps/config): offline tiles served by Sentry Mate itself when they are installed
// (offline first), else the online tile server the owner turned on, else no tiles at all: the paper
// background with the routes and pins on it, and a note that says so. Nothing else is ever fetched from
// outside. MapLibre is loaded on first use (vendor/maplibre-gl), with its worker from the same folder.
//
// Sizes: the box is sized in rem by CSS and MapLibre follows it. Lines and padding are given in px at the
// standard size and scaled with the interface size; raster tiles are drawn larger on a large interface so
// their labels grow too. A new pixel ratio (another screen) makes MapLibre read it again. A new theme
// rebuilds the paint of the tiles and lines.

import { h, cssVar } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { display } from './display.js';
import { isDark, onTheme } from './prefs.js';

let lib = null;
/** Load MapLibre once (module, worker and style sheet). */
export async function loadMapLibre() {
  if (!lib) {
    lib = (async () => {
      if (!document.querySelector('link[data-maplibre]')) document.head.append(h('link', { rel: 'stylesheet', href: '/vendor/maplibre-gl/maplibre-gl.css', dataset: { maplibre: '' } }));
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

/** Where tiles come from: { url, attribution, maxZoom, offline } or null when there are none. */
export function tileSource(cfg) {
  const t = cfg && cfg.tiles;
  if (!t) return null;
  if (t.offline_url && t.offline_data_present !== false && t.mode === 'offline') return { url: t.offline_url, attribution: t.attribution || '', maxZoom: t.max_zoom || 16, offline: true };
  if (t.mode === 'online' && t.online_template) return { url: t.online_template, attribution: t.attribution || '', maxZoom: t.max_zoom || 19, offline: false };
  if (t.offline_url) return { url: t.offline_url, attribution: t.attribution || '', maxZoom: t.max_zoom || 16, offline: true };
  return null;
}

const tilePx = () => (display.scale >= 1.2 ? Math.round(256 * display.scale) : 256);
const rasterPaint = (dark) => (dark
  ? { 'raster-saturation': -1, 'raster-brightness-min': 0.78, 'raster-brightness-max': 0.07, 'raster-contrast': 0.08, 'raster-opacity': 0.92, 'raster-fade-duration': 0 }
  : { 'raster-saturation': -0.88, 'raster-contrast': -0.16, 'raster-brightness-min': 0.2, 'raster-opacity': 0.7, 'raster-fade-duration': 0 });

/** '37.7508° N, 122.4330° W'. */
export const coordsText = ([lon, lat]) => `${Math.abs(lat).toFixed(4)}° ${lat >= 0 ? 'N' : 'S'}, ${Math.abs(lon).toFixed(4)}° ${lon >= 0 ? 'E' : 'W'}`;

/** [[west, south], [east, north]] around [lon, lat] points. */
export function boundsOf(coords) {
  let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity;
  for (const [x, y] of coords) { w = Math.min(w, x); e = Math.max(e, x); s = Math.min(s, y); n = Math.max(n, y); }
  return [[w, s], [e, n]];
}

const ROUND = { 'line-cap': 'round', 'line-join': 'round' };

/**
 * A muted map in el: makeMap(ctx, el, { center: [lon, lat], zoom, bounds, padding, interactive, maxZoom }).
 * Resolves with { map, ready, addRoute(id, coords, { kind }), addPin([lon, lat], label, cls), frame(bounds) },
 * or null when the page was left meanwhile. ready resolves once the map has drawn (or after 15 s).
 * The box shimmers until then. Removed with the page.
 */
export async function makeMap(ctx, el, { center = null, zoom = 13, bounds = null, padding = 40, interactive = true, maxZoom = 16, fullscreen = false } = {}) {
  el.classList.add('map', 'is-loading');
  const [ml, cfg] = await Promise.all([loadMapLibre(), mapConfig().catch(() => null)]);
  if (ctx.signal.aborted) return null;
  const src = tileSource(cfg);
  const pad = () => Math.round(padding * display.scale);
  let tiles = tilePx();
  const source = () => ({ type: 'raster', tiles: [src.url], tileSize: tiles, maxzoom: src.maxZoom, attribution: src.attribution });
  const style = () => ({
    version: 8,
    sources: src ? { tiles: source() } : {},
    layers: [{ id: 'paper', type: 'background', paint: { 'background-color': cssVar('--map-paper') } },
      ...(src ? [{ id: 'tiles', type: 'raster', source: 'tiles', paint: rasterPaint(isDark()) }] : [])],
  });
  const map = new ml.Map({
    container: el, style: style(), center: center || [0, 0], zoom,
    bounds: bounds || undefined, fitBoundsOptions: { padding: pad(), maxZoom },
    attributionControl: { compact: false },
    refreshExpiredTiles: false, // tiles load as images, which the page's policy allows
    scrollZoom: false, dragRotate: false, pitchWithRotate: false, touchPitch: false,
    interactive, fadeDuration: 0, maxZoom: 18, maxCanvasSize: [8192, 8192],
  });
  map.touchZoomRotate.disableRotation();
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  if (fullscreen) map.addControl(new ml.FullscreenControl({ container: el.parentElement || el }), 'top-right');
  if (!src) el.append(h('p', { class: 'map-note', role: 'note' }, 'No map tiles: offline maps are not installed and online tiles are off.'));
  const lines = new Map(); // layer id -> { width, colour token }
  let framing = bounds;
  let moved = false;
  map.on('dragstart', () => { moved = true; });
  map.on('zoomstart', (e) => { if (e.originalEvent) moved = true; });
  const reframe = () => { if (framing && !moved) map.fitBounds(framing, { padding: pad(), maxZoom, animate: false }); };
  map.on('resize', reframe);
  const stopSize = display.onChange((change) => {
    if (change.dpr) map.resize();
    if (change.rem) {
      for (const [id, l] of lines) if (map.getLayer(id)) map.setPaintProperty(id, 'line-width', l.width * display.scale);
      if (src && tilePx() !== tiles && map.isStyleLoaded() && map.getSource('tiles')) {
        tiles = tilePx();
        const ids = map.getStyle().layers.map((l) => l.id);
        const next = ids[ids.indexOf('tiles') + 1];
        map.removeLayer('tiles'); map.removeSource('tiles');
        map.addSource('tiles', source());
        map.addLayer({ id: 'tiles', type: 'raster', source: 'tiles', paint: rasterPaint(isDark()) }, next);
      }
      reframe();
    }
  });
  const stopTheme = onTheme(() => {
    if (!map.isStyleLoaded()) return;
    map.setPaintProperty('paper', 'background-color', cssVar('--map-paper'));
    if (map.getLayer('tiles')) for (const [k, v] of Object.entries(rasterPaint(isDark()))) map.setPaintProperty('tiles', k, v);
    for (const [id, l] of lines) if (map.getLayer(id)) map.setPaintProperty(id, 'line-color', cssVar(l.colour));
  });
  let readyTimer = 0;
  ctx.onCleanup(() => { clearTimeout(readyTimer); stopSize(); stopTheme(); map.remove(); });
  let failed = 0;
  map.on('error', (e) => {
    if (!(e && (e.tile || e.sourceId === 'tiles'))) return;
    if (++failed === 1) el.append(h('p', { class: 'map-note', role: 'note' }, `Map tiles did not load; the map is at ${coordsText(map.getCenter().toArray())}.`));
  });
  const ready = new Promise((resolve) => {
    let done = false;
    const finish = () => { clearTimeout(readyTimer); if (!done) { done = true; el.classList.remove('is-loading'); resolve(); } };
    map.once('idle', finish);
    readyTimer = setTimeout(finish, 15000);
  });
  const loaded = new Promise((resolve) => { if (map.loaded()) resolve(); else map.once('load', resolve); });
  const pins = [];
  const placePins = () => { for (const p of pins) flipPin(map, p); };
  map.on('resize', placePins);
  map.on('moveend', placePins);

  function addLine(layer, width, colour) {
    lines.set(layer.id, { width, colour });
    map.addLayer({ ...layer, type: 'line', paint: { ...layer.paint, 'line-color': cssVar(colour), 'line-width': width * display.scale } });
  }
  return {
    map, ml, ready, loaded, source: src,
    /**
     * A line layer whose width (px at the standard size) follows the interface size and whose colour (a CSS
     * token such as '--accent') follows the theme: addLine({ id, source, filter }, 3, '--accent').
     */
    addLine(layer, width, colour = '--accent') { addLine({ layout: ROUND, paint: {}, ...layer }, width, colour); },
    /** A route line: kind 'route' (accent, with a paper casing), 'faint' or 'ahead' (dashed). */
    async addRoute(id, coords, { kind = 'route' } = {}) {
      await loaded;
      if (ctx.signal.aborted) return;
      map.addSource(id, { type: 'geojson', data: { type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: coords } } });
      if (kind === 'ahead') { addLine({ id: `${id}-line`, source: id, layout: ROUND, paint: { 'line-dasharray': [1.5, 2] } }, 2.2, '--ink-3'); return; }
      addLine({ id: `${id}-casing`, source: id, layout: ROUND, paint: { 'line-opacity': 0.9 } }, kind === 'faint' ? 5 : 8, '--paper');
      addLine({ id: `${id}-line`, source: id, layout: ROUND, paint: {} }, kind === 'faint' ? 2.4 : 3.6, kind === 'faint' ? '--ink-3' : '--accent');
    },
    /** A labelled pin at [lon, lat]; cls 'pin-start' | 'pin-end' | 'pin-here' | 'pin-charge'. */
    addPin(lngLat, label, cls = 'pin-here') {
      const pinEl = h('div', { class: ['pin', cls] }, h('span', { class: 'pin-dot' }), label ? h('span', { class: 'pin-label' }, label) : null);
      const marker = new ml.Marker({ element: pinEl, anchor: 'left' }).setLngLat(lngLat).addTo(map);
      const pin = { el: pinEl, lngLat, labelled: !!label };
      pins.push(pin);
      flipPin(map, pin);
      return marker;
    },
    /** Frame other bounds, now and after each resize (until the person moves the map). */
    frame(b) { framing = b; moved = false; reframe(); },
  };
}

/** Put a pin's label on the side with room (the marker is anchored by its left edge; CSS shifts it back). */
function flipPin(map, pin) {
  if (!pin.labelled) return;
  const x = map.project(pin.lngLat).x;
  const w = map.getContainer().clientWidth;
  const need = pin.el.offsetWidth + display.rem;
  pin.el.classList.toggle('pin-flip', x + need > w && x > w - x);
}
