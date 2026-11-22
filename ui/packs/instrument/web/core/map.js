// Maps on MapLibre GL (vendor/maplibre-gl, loaded on first use), with tiles chosen offline first
// (shared/maptiles.js): GET /api/v1/maps/config -> the offline map file Sentry Mate serves (raster, or a
// Protomaps vector file drawn with a plain style); the online source the owner turned on (only then does
// the browser ask an outside server); otherwise no tiles: the map shows the track and markers on a plain
// background under a "No map tiles yet" note (for an admin, a one-click OpenStreetMap switch). Tiles that
// fail show a short note with the cause. In the dark theme raster tiles are dimmed (never inverted, so
// streets, labels and water stay readable) unless the person chose original colours.
// Maps stay sharp and in proportion: MapLibre reads the pixel ratio only in resize(), so the map is
// resized when the pixel ratio or the root size changes; line widths and circle sizes added with
// handle.addLayer(def, sized) are functions of rpx() and are set again; the camera framing (handle.frame)
// runs again after a resize until the person moves the map. The attribution is text (escaped).

import * as api from '../shared/api.js';
import { h, cssVar, esc } from '../shared/dom.js';
import { rpx, onChange, onTheme, getTheme } from './display.js';
import { getPrefs, onPrefs } from './prefs.js';
import { tileChoice, rasterPaint, dimmed, mapColors, tileSourceDef, tileLayers, vectorGround, noTilesNote, watchTileErrors } from '../shared/maptiles.js';

let mlLoad = null;
/** Load MapLibre once (an ES module and its worker, both from this pack). */
export function loadMapLibre() {
  mlLoad ??= import('../vendor/maplibre-gl/maplibre-gl.mjs').then((ml) => {
    ml.setWorkerUrl(new URL('../vendor/maplibre-gl/maplibre-gl-worker.mjs', import.meta.url).href);
    return ml;
  });
  return mlLoad;
}

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
/** The tile set to use (shared/maptiles.js tileChoice): { kind: 'raster' | 'vector' | 'none', offline, template, attribution, maxZoom, why }. */
export async function tileSource() {
  let cfg = null;
  try { cfg = await api.get('maps/config', null, { maxAgeMs: 300000 }); } catch { cfg = null; }
  return tileChoice(cfg);
}

const isDark = () => getTheme().effective === 'dark';

function style(src) {
  const n = (name, fallback) => { const v = parseFloat(cssVar(name)); return Number.isFinite(v) ? v : fallback; };
  const dark = isDark(), colors = mapColors(getPrefs());
  const ground = src.kind === 'vector' ? vectorGround(dark) : cssVar('--map-bg');
  const layers = [{ id: 'bg', type: 'background', paint: { 'background-color': ground } }];
  const sources = {};
  const def = tileSourceDef(src, { attribution: esc(src.attribution) });
  if (def) {
    sources.tiles = def;
    const tune = { saturation: n('--map-dim-saturation', -0.25), contrast: n('--map-dim-contrast', 0.12), min: n('--map-dim-min', 0.04), max: n('--map-dim-max', 0.66) };
    layers.push(...tileLayers(src, { dark, paint: rasterPaint({ dark, colors, tune }) }));
    // the pack's blue tint, only over the dimmed look (original colours stay as the tile server drew them)
    layers.push({ id: 'tint', type: 'background', paint: { 'background-color': cssVar('--map-tint'), 'background-opacity': dimmed(dark, colors) && src.kind === 'raster' ? n('--map-tint-opacity', 0.1) : 0 } });
  }
  return { version: 8, sources, layers };
}

/**
 * A map in container (sized by CSS):
 *   const m = await createMap(el, ctx, { center: [lon, lat], zoom: 15, bounds, interactive: true });
 *   await m.ready; m.addLayer({ id, type: 'line', source, paint }, { 'line-width': () => rpx(4) });
 *   m.frame(() => m.map.fitBounds(b, { padding: rpx(30), duration: 0 }));
 * Returns { map, ml, ready, addLayer, frame, tiles } or null when the page was left meanwhile.
 * Throws when MapLibre cannot start (no WebGL): show mapFallback() then.
 */
export async function createMap(container, ctx, { center = [0, 0], zoom = 14, bounds = null, interactive = true } = {}) {
  const [ml, src] = await Promise.all([loadMapLibre(), tileSource()]);
  watchTilePolicy(src.template && !src.offline ? src.template : null);
  if (!ctx.alive()) return null;
  const map = new ml.Map({
    container, style: style(src), center, zoom, attributionControl: false, interactive,
    cooperativeGestures: false, dragRotate: false, pitchWithRotate: false, fadeDuration: 0, refreshExpiredTiles: false,
    maxZoom: Math.max(src.maxZoom, 18),
    ...(bounds ? { bounds } : {}),
  });
  if (src.template) map.addControl(new ml.AttributionControl({ compact: false }), 'bottom-right');
  const offErrors = watchTileErrors(map, container, src, (text) => h('p', { class: 'map-note map-note--error', role: 'status' }, text));
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  map.touchZoomRotate?.disableRotation();
  container.dataset.tiles = src.kind;
  if (src.kind === 'none') container.append(noTilesNote(src, { compact: container.clientHeight < 300, classes: { primary: 'btn btn--sm btn--primary', secondary: 'btn btn--sm btn--ghost' } }));
  const ready = new Promise((res) => map.once('load', res));
  map.on('error', (e) => { console.warn('map', e && e.error ? e.error.message : e); });

  const sized = [];
  let framing = null, userMoved = false;
  map.on('movestart', (e) => { if (e.originalEvent instanceof Event) userMoved = true; });
  map.on('resize', () => { if (framing && !userMoved) framing(); });
  const off = onChange(() => {
    for (const [id, prop, value] of sized) if (map.getLayer(id)) map.setPaintProperty(id, prop, value());
    map.resize();
    map.triggerRepaint();
  });
  // a theme change recolours the base layers in place (setStyle would drop the page's own layers)
  const repaint = () => {
    for (const l of style(src).layers) for (const [k, v] of Object.entries(l.paint)) if (map.getLayer(l.id)) map.setPaintProperty(l.id, k, v);
  };
  const offTheme = onTheme(repaint);
  // the person's map colours (dimmed or original) change in place too
  let colorsNow = mapColors(getPrefs());
  const offPrefs = onPrefs((p) => { if (mapColors(p) !== colorsNow) { colorsNow = mapColors(p); repaint(); } });
  ctx.onCleanup(() => { off(); offTheme(); offPrefs(); offErrors(); map.remove(); });
  return {
    map, ml, ready, tiles: src,
    addLayer(def, sizedPaint = {}) {
      const paint = { ...def.paint };
      for (const [prop, value] of Object.entries(sizedPaint)) { paint[prop] = value(); sized.push([def.id, prop, value]); }
      map.addLayer({ ...def, paint });
    },
    /** f() places the camera; it runs now and again after resizes until the person moves the map. */
    frame(f) { framing = f; f(); },
  };
}

/** What a map shows when it cannot be drawn (no WebGL, a library that did not load). */
export const mapFallback = (text = 'The map could not be drawn in this browser.') => h('div', { class: 'map-fallback' }, text);

/** [[west, south], [east, north]] around [lon, lat] points. */
export function boundsOf(coords) {
  let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity;
  for (const [lo, la] of coords) { if (lo < w) w = lo; if (lo > e) e = lo; if (la < s) s = la; if (la > n) n = la; }
  return [[w, s], [e, n]];
}

/** Camera padding that keeps a card on top of the map clear, plus a margin (design px). */
export function padClear(mapEl, card, { top = 30, right = 30, bottom = 30, left = 30, beside = false } = {}) {
  const pad = { top: rpx(top), right: rpx(right), bottom: rpx(bottom), left: rpx(left) };
  if (card) {
    const m = mapEl.getBoundingClientRect(), c = card.getBoundingClientRect();
    if (beside) pad.left = Math.max(pad.left, c.right - m.left + rpx(left));
    else pad.top = Math.max(pad.top, c.bottom - m.top + rpx(top));
  }
  const w = mapEl.clientWidth, hh = mapEl.clientHeight;
  if (pad.left + pad.right > w * 0.8) pad.left = Math.max(0, w * 0.8 - pad.right);
  if (pad.top + pad.bottom > hh * 0.8) pad.top = Math.max(0, hh * 0.8 - pad.bottom);
  return pad;
}

/** The car's marker. tone: live | warm (charging) | asleep | muted (stale, unknown). heading in degrees. */
export function carMarker(m, lngLat, { tone = 'live', heading = null } = {}) {
  const el = h('div', { class: ['car-marker', `car-marker--${tone}`], 'aria-hidden': 'true' });
  if (heading != null) {
    const ns = 'http://www.w3.org/2000/svg';
    const s = document.createElementNS(ns, 'svg');
    s.setAttribute('viewBox', '0 0 24 24');
    s.setAttribute('class', 'car-marker-heading');
    const p = document.createElementNS(ns, 'path');
    p.setAttribute('d', 'M12 4 18.5 19 12 15.2 5.5 19z');
    s.append(p);
    el.append(s);
    el.style.setProperty('--rot', `${heading}deg`);
  }
  return new m.ml.Marker({ element: el }).setLngLat(lngLat).addTo(m.map);
}

/** An A or B pin at a route's start or end. */
export function routePin(m, lngLat, letter) {
  const el = h('div', { class: ['route-pin', `route-pin--${letter.toLowerCase()}`], 'aria-hidden': 'true' }, letter);
  return new m.ml.Marker({ element: el }).setLngLat(lngLat).addTo(m.map);
}

/** A route line (glow, casing, a gradient along it). coords: [[lon, lat], ...]. */
export function addRoute(m, coords, id = 'route', { thin = false } = {}) {
  m.map.addSource(id, { type: 'geojson', lineMetrics: true, data: { type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: coords } } });
  const line = { type: 'line', source: id, layout: { 'line-cap': 'round', 'line-join': 'round' } };
  m.addLayer({ ...line, id: `${id}-glow`, paint: { 'line-color': cssVar('--live'), 'line-opacity': 0.22 } },
    { 'line-width': () => rpx(thin ? 9 : 16), 'line-blur': () => rpx(thin ? 4 : 8) });
  m.addLayer({ ...line, id: `${id}-casing`, paint: { 'line-color': cssVar('--map-casing'), 'line-opacity': 0.85 } },
    { 'line-width': () => rpx(thin ? 5 : 8) });
  m.addLayer({ ...line, id: `${id}-line`, paint: { 'line-gradient': ['interpolate', ['linear'], ['line-progress'], 0, cssVar('--live-2'), 0.6, cssVar('--live'), 1, cssVar('--live-3')] } },
    { 'line-width': () => rpx(thin ? 2.6 : 4.2) });
}
