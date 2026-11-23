// Sentry Mate, "Pebble" UI pack: map tiles on MapLibre.
//
// mapTile(ctx, box, opts) draws a soft map inside a rounded box: the tiles from the server's map settings
// (GET /api/v1/maps/config, read by shared/maptiles.js: the offline map file Sentry Mate serves, raster or a
// Protomaps vector file in a plain style, else the online tile server the owner turned on, else none: only the
// routes and pins on a plain ground under a "No map tiles yet" note, with a one-click satellite map switch for
// an admin), routes in the pack's series colours and round pins. Tiles that fail show their cause. The light
// theme shows the tile server's own colours; the dark one dims them (never inverted, so streets, labels and
// water stay readable) unless the person chose original colours. Satellite photos are never dimmed, and over
// them the lines get a dark casing and the pins a dark edge. When the admin allows more than one style, a small
// Map / Satellite switch sits on the map (the pick is the person's map_style). Nothing else is fetched from outside.
// MapLibre loads on first use from vendor/maplibre-gl, its worker from the same folder.
//
// Line widths and padding are given in px at the standard size and follow the Interface size; a new theme
// repaints the ground, the tiles and the lines.

import { h, cssVar } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { display } from './display.js';
import { isDark, onTheme, currentPrefs, onPrefs } from './prefs.js';
import { tileChoice, rasterPaint, mapColors, tileSources, tileLayers, repaintTiles, vectorGround, noTilesNote, watchTileErrors, onImagery, IMAGERY_HALO, mapStyleSwitch, swapTiles } from '../shared/maptiles.js';

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
/** Where the tiles come from (shared/maptiles.js): { kind, template, attribution, maxZoom, offline, why }; kind 'none' when there are none. */
export const tileSource = (cfg, prefs = currentPrefs()) => tileChoice(cfg, prefs);

// the dimmed dark look (calm, a little colour kept, never inverted)
const DIM = { saturation: -0.4, contrast: 0.14, min: 0.04, max: 0.58 };
const paintNow = (choice = null) => rasterPaint({ dark: isDark(), colors: mapColors(currentPrefs()), tune: DIM, imagery: onImagery(choice) });
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
export async function mapTile(ctx, box, { center = null, zoom = 13, bounds = null, padding = 36, interactive = true, fullscreen = false, maxZoom = 16, styleSwitch = true } = {}) {
  box.classList.add('map', 'is-loading');
  let ml, cfg;
  try { [ml, cfg] = await Promise.all([loadMapLibre(), mapConfig().catch(() => null)]); } catch (err) {
    box.classList.remove('is-loading');
    box.append(h('p', { class: 'map-note' }, 'The map could not start in this browser.'));
    return { map: null, ready: Promise.resolve(), route() {}, routes() {}, pin() {}, circle() {}, frame() {}, noTiles: true, failed: err };
  }
  if (ctx.signal.aborted) return null;
  const choice = tileSource(cfg);
  let src = choice.kind === 'none' ? null : choice;
  watchTilePolicy(src && !src.offline ? src.template : null);
  box.toggleAttribute('data-imagery', onImagery(src));
  const pad = () => Math.round(padding * display.scale);
  let tiles = tilePx();
  const ground = () => (src?.kind === 'vector' ? vectorGround(isDark()) : cssVar('--map-ground'));
  const map = new ml.Map({
    container: box,
    style: {
      version: 8,
      sources: tileSources(src, { rasterSize: tiles }),
      layers: [{ id: 'ground', type: 'background', paint: { 'background-color': ground() } },
        ...tileLayers(src, { dark: isDark(), paint: paintNow(src) })],
    },
    center: center || [0, 0], zoom, bounds: bounds || undefined, fitBoundsOptions: { padding: pad(), maxZoom },
    attributionControl: { compact: false }, refreshExpiredTiles: false, // the credit stays in view, as the tile policies ask
    scrollZoom: false, dragRotate: false, pitchWithRotate: false, touchPitch: false, cooperativeGestures: false,
    interactive, fadeDuration: 0, maxZoom: 18, maxCanvasSize: [8192, 8192],
  });
  map.touchZoomRotate.disableRotation();
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  if (fullscreen) map.addControl(new ml.FullscreenControl({ container: box.closest('.tile') || box }), 'top-right');
  if (!src) box.append(noTilesNote(choice, { compact: box.clientHeight < 300, classes: { primary: 'btn btn-primary', secondary: 'btn btn-soft' } }));
  const lines = new Map(); // layer id -> { width, token, prop, halo }
  // over satellite photos the page's casings turn dark (IMAGERY_HALO) and the halo-only casings show
  const colorOf = (token) => (token === '--map-case' && onImagery(src) ? IMAGERY_HALO.casing : cssVar(token));
  const restyle = () => {
    for (const [id, l] of lines) {
      if (!map.getLayer(id)) continue;
      map.setPaintProperty(id, l.prop || 'line-color', colorOf(l.token));
      if (l.halo) map.setPaintProperty(id, 'line-opacity', onImagery(src) ? 1 : 0);
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
    if (!change.rem) return;
    for (const [id, l] of lines) if (map.getLayer(id)) map.setPaintProperty(id, 'line-width', l.width * display.scale);
    if (src?.kind === 'raster' && tilePx() !== tiles && map.isStyleLoaded() && map.getSource('tiles')) {
      tiles = tilePx();
      swapTiles(map, src, src, { rasterSize: tiles, dark: isDark(), paint: paintNow(src) });
    }
    reframe();
  });
  const stopTheme = onTheme(() => {
    if (!map.isStyleLoaded()) return;
    map.setPaintProperty('ground', 'background-color', ground());
    if (src) repaintTiles(map, src, { dark: isDark(), paint: paintNow(src) });
    restyle();
  });
  // the person's map colours (dimmed or original) change in place
  let colorsNow = mapColors(currentPrefs());
  const stopPrefs = onPrefs((p) => {
    if (mapColors(p) === colorsNow) return;
    colorsNow = mapColors(p);
    if (src && map.isStyleLoaded()) repaintTiles(map, src, { dark: isDark(), paint: paintNow(src) });
  });
  const errorNote = (text) => h('p', { class: 'map-note map-note-error', role: 'status' }, text);
  let stopErrors = watchTileErrors(map, box, src, errorNote);
  let timer = 0;
  ctx.onCleanup(() => { clearTimeout(timer); stopSize(); stopTheme(); stopPrefs(); stopErrors(); map.remove(); });
  const ready = new Promise((resolve) => {
    let done = false;
    const finish = () => { clearTimeout(timer); if (!done) { done = true; box.classList.remove('is-loading'); resolve(); } };
    map.once('idle', finish);
    timer = setTimeout(finish, 12000);
  });
  const loaded = new Promise((resolve) => { if (map.loaded()) resolve(); else map.once('load', resolve); });
  // Map / Satellite: only when the admin allows two styles or more (else null); redraws the tiles in place
  const switcher = src && styleSwitch ? mapStyleSwitch(cfg, currentPrefs(), {
    classes: { box: 'map-style', button: 'map-style-btn', active: 'is-on' },
    onChange: (next) => loaded.then(() => {
      if (ctx.signal.aborted || next.kind === 'none') return;
      stopErrors();
      src = swapTiles(map, src, next, { rasterSize: tiles, dark: isDark(), paint: paintNow(next) });
      box.toggleAttribute('data-imagery', onImagery(src));
      watchTilePolicy(src.offline ? null : src.template);
      restyle();
      stopErrors = watchTileErrors(map, box, src, errorNote);
    }),
  }) : null;
  if (switcher) { box.append(switcher); box.setAttribute('data-styles', ''); }
  function line(layer, width, token, { halo = false } = {}) {
    lines.set(layer.id, { width, token, halo });
    map.addLayer({ ...layer, type: 'line', layout: ROUND, paint: { ...(layer.paint || {}), 'line-color': colorOf(token), 'line-width': width * display.scale, ...(halo ? { 'line-opacity': onImagery(src) ? 1 : 0 } : {}) } });
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
      line({ id: `${id}-case`, source: id }, 2.5 + IMAGERY_HALO.casingWidth, '--map-case', { halo: true });
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
      line({ id: `${id}-case`, source: id }, 2 + IMAGERY_HALO.casingWidth, '--map-case', { halo: true });
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
