// Sentry Mate, "Logbook" UI pack: maps on MapLibre.
//
// makeMap(ctx, el, { center, zoom, bounds }) draws a muted map in el. The tiles come from the server's map
// settings (GET /api/v1/maps/config, read by shared/maptiles.js): the offline map file Sentry Mate serves
// (raster, or a Protomaps vector file in a plain style), else the online tile server the owner turned on,
// else no tiles at all: the paper background with the routes and pins on it, under a "No map tiles yet"
// note (for an admin, a one-click switch to the satellite map). Tiles that fail show their cause. Nothing else is
// ever fetched from outside. MapLibre is loaded on first use (vendor/maplibre-gl), with its worker from the
// same folder. In the dark theme raster tiles are dimmed (not inverted) unless the person chose original
// colours; the light theme shows the tile server's own colours.
//
// Sizes: the box is sized in rem by CSS and MapLibre follows it. Lines and padding are given in px at the
// standard size and scaled with the interface size; raster tiles are drawn larger on a large interface so
// their labels grow too. A new pixel ratio (another screen) makes MapLibre read it again. A new theme
// rebuilds the paint of the tiles and lines.
//
// Satellite pictures (choice.imagery) are never dimmed; over them the routes, outlines and pins get a dark
// edge (IMAGERY_HALO) so they read on fields, water and roofs alike. An interactive map shows a small
// Map / Satellite switch when the admin allows more than one style (remembered per person).

import { h, cssVar, styleSheet } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { display } from './display.js';
import { isDark, onTheme, currentPrefs, onPrefs } from './prefs.js';
import { tileChoice, rasterPaint, mapColors, tileSources, tileLayers, repaintTiles, swapTiles, sharpZoom, vectorGround, noTilesNote, watchTileErrors, onImagery, mapStyleSwitch, IMAGERY_HALO } from '../shared/maptiles.js';

let lib = null;
/** Load MapLibre once (module, worker and style sheet, all relative to this file); resolves once the style sheet applies. */
export function loadMapLibre() {
  lib ??= Promise.all([
    import('../vendor/maplibre-gl/maplibre-gl.mjs'),
    styleSheet(new URL('../vendor/maplibre-gl/maplibre-gl.css', import.meta.url).href),
  ]).then(([ml]) => {
    ml.setWorkerUrl(new URL('../vendor/maplibre-gl/maplibre-gl-worker.mjs', import.meta.url).href);
    return ml;
  }).catch((e) => { lib = null; throw e; });
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
/** Where tiles come from (shared/maptiles.js): { kind, template, attribution, maxZoom, offline, why }; kind 'none' when there are none. */
export const tileSource = (cfg, prefs = currentPrefs()) => tileChoice(cfg, prefs);

const tilePx = () => (display.scale >= 1.2 ? Math.round(256 * display.scale) : 256);
// the dimmed dark look, tuned for the paper colours (never inverted: streets, labels and water stay readable)
const DIM = { saturation: -0.45, contrast: 0.16, min: 0.03, max: 0.56 };
const paintFor = (choice = null) => rasterPaint({ dark: isDark(), colors: mapColors(currentPrefs()), tune: DIM, imagery: onImagery(choice) });

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
export async function makeMap(ctx, el, { center = null, zoom = 13, bounds = null, padding = 40, interactive = true, maxZoom = 16, fullscreen = false, styleSwitch = interactive } = {}) {
  el.classList.add('map', 'is-loading');
  const [ml, cfg] = await Promise.all([loadMapLibre(), mapConfig().catch(() => null)]);
  if (ctx.signal.aborted) return null;
  const choice = tileSource(cfg);
  let src = choice.kind === 'none' ? null : choice;
  watchTilePolicy(src && !src.offline ? src.template : null);
  el.classList.toggle('is-imagery', onImagery(src));
  const pad = () => Math.round(padding * display.scale);
  let tiles = tilePx();
  // never opens deeper than the tiles are sharp (the satellite photos end sooner than the map)
  zoom = Math.min(zoom, sharpZoom(src, tiles));
  maxZoom = Math.min(maxZoom, sharpZoom(src, tiles));
  const paper = () => (src?.kind === 'vector' ? vectorGround(isDark()) : cssVar('--map-paper'));
  const style = () => ({
    version: 8,
    sources: tileSources(src, { rasterSize: tiles }),
    layers: [{ id: 'paper', type: 'background', paint: { 'background-color': paper() } },
      ...tileLayers(src, { dark: isDark(), paint: paintFor(src) })],
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
  if (!src) el.append(noTilesNote(choice, { compact: el.clientHeight < 300, classes: { primary: 'button button-small', secondary: 'button button-quiet button-small' } }));
  const lines = new Map(); // layer id -> { width, colour token, halo (the dark edge under it over photos) }
  const tileWatchers = new Set(); // page callbacks for a new tile choice (Map / Satellite)
  // a line's colour: over satellite pictures the paper casings turn dark, and the halos show
  const lineColour = (l) => (l.halo ? IMAGERY_HALO.casing : onImagery(src) && l.colour === '--paper' ? IMAGERY_HALO.casing : cssVar(l.colour));
  const lineWidth = (l) => (l.halo ? l.width + IMAGERY_HALO.casingWidth : l.width) * display.scale;
  const restyleLines = () => {
    for (const [id, l] of lines) {
      if (!map.getLayer(id)) continue;
      map.setPaintProperty(id, 'line-color', lineColour(l));
      if (l.halo) map.setPaintProperty(id, 'line-opacity', onImagery(src) ? 0.85 : 0);
    }
  };
  let framing = bounds;
  let moved = false;
  map.on('dragstart', () => { moved = true; });
  map.on('zoomstart', (e) => { if (e.originalEvent) moved = true; });
  const reframe = () => { if (framing && !moved) map.fitBounds(framing, { padding: pad(), maxZoom, animate: false }); };
  map.on('resize', reframe);
  const stopSize = display.onChange((change) => {
    if (change.dpr) map.resize();
    if (change.rem) {
      for (const [id, l] of lines) if (map.getLayer(id)) map.setPaintProperty(id, 'line-width', lineWidth(l));
      if (src?.kind === 'raster' && tilePx() !== tiles && map.isStyleLoaded() && map.getSource('tiles')) {
        tiles = tilePx();
        swapTiles(map, src, src, { rasterSize: tiles, dark: isDark(), paint: paintFor(src) });
      }
      reframe();
    }
  });
  const stopTheme = onTheme(() => {
    if (!map.isStyleLoaded()) return;
    map.setPaintProperty('paper', 'background-color', paper());
    if (src) repaintTiles(map, src, { dark: isDark(), paint: paintFor(src) });
    restyleLines();
  });
  // the person's map colours (dimmed or original) change in place
  let colorsNow = mapColors(currentPrefs());
  const stopPrefs = onPrefs((p) => {
    if (mapColors(p) === colorsNow) return;
    colorsNow = mapColors(p);
    if (src && map.isStyleLoaded()) repaintTiles(map, src, { dark: isDark(), paint: paintFor(src) });
  });
  const errorNote = (text) => h('p', { class: 'map-note map-note-error', role: 'status' }, text);
  let stopErrors = watchTileErrors(map, el, src, errorNote);
  // Map / Satellite: redraws the tiles under the page's own layers, then the halos and the pins' edge
  const styleBox = src && styleSwitch ? mapStyleSwitch(cfg, currentPrefs(), {
    classes: { box: 'map-style', button: 'map-style-btn', active: 'is-active' },
    onChange: (next) => {
      if (!next || next.kind === 'none' || !map.isStyleLoaded()) return;
      stopErrors();
      src = swapTiles(map, src, next, { rasterSize: tiles, dark: isDark(), paint: paintFor(next) });
      watchTilePolicy(src.offline ? null : src.template);
      el.classList.toggle('is-imagery', onImagery(src));
      restyleLines();
      stopErrors = watchTileErrors(map, el, src, errorNote);
      for (const fn of tileWatchers) fn(src);
    },
  }) : null;
  if (styleBox) el.append(styleBox);
  let readyTimer = 0;
  ctx.onCleanup(() => { clearTimeout(readyTimer); stopSize(); stopTheme(); stopPrefs(); stopErrors(); map.remove(); });
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
    // a line of its own colour gets a dark halo under it, seen only over satellite pictures (casings turn dark instead)
    if (colour !== '--paper') {
      const halo = { width, colour, halo: true };
      const { 'line-dasharray': _dash, ...rest } = layer.paint || {};
      lines.set(`${layer.id}-halo`, halo);
      map.addLayer({ ...layer, id: `${layer.id}-halo`, type: 'line', paint: { ...rest, 'line-color': lineColour(halo), 'line-width': lineWidth(halo), 'line-opacity': onImagery(src) ? 0.85 : 0, 'line-blur': 1 } });
    }
    const l = { width, colour };
    lines.set(layer.id, l);
    map.addLayer({ ...layer, type: 'line', paint: { ...layer.paint, 'line-color': lineColour(l), 'line-width': lineWidth(l) } });
  }
  return {
    map, ml, ready, loaded,
    /** The tiles drawn now (changes with the Map / Satellite switch). */
    get source() { return src; },
    /** Whether the map under the page's layers is satellite pictures now. */
    imagery: () => onImagery(src),
    /** fn(choice) after the person switched Map / Satellite; returns a function that stops it. */
    onTiles(fn) { tileWatchers.add(fn); return () => tileWatchers.delete(fn); },
    /** Shows or hides a line made with addLine, and its halo. */
    setVisible(id, on) { for (const x of [`${id}-halo`, id]) if (map.getLayer(x)) map.setLayoutProperty(x, 'visibility', on ? 'visible' : 'none'); },
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
