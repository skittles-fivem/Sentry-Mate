// Maps on MapLibre GL (vendor/maplibre-gl, loaded on first use), in a flat grey style.
//
// Tiles: GET /api/v1/maps/config, read by shared/maptiles.js. The offline map file comes from Sentry Mate
// itself (raster, or a Protomaps vector file in a plain style); online tiles from the source the owner
// turned on (only then does the browser ask an outside server); otherwise the map has no tiles: a plain land
// colour, the track and the points, under a "No map tiles yet" note (for an admin, a one-click online map
// switch). Tiles that fail show their cause. In the dark theme the raster is dimmed and mostly grey (never
// inverted, so streets, labels and water stay readable) unless the person chose original colours; satellite
// pictures are never dimmed, and the lines, points and pins over them get a dark casing (IMAGERY_HALO); the light
// theme shows the tile server's own colours. Colours are tokens (--map-*). When the admin allows more than one
// online style, each map has a small Map / Satellite switch (top left) that redraws its tiles in place.
// Line widths and point sizes are design px through rpx(); they are set again when the root size or the
// pixel ratio changes, and the camera framing runs again after a resize until the person moves the map.

import * as api from '../shared/api.js';
import { h, cssVar, esc, styleSheet } from '../shared/dom.js';
import { rpx, onChange, onTheme, getTheme } from './display.js';
import { whenNear } from '../shared/lazy.js';
import { getPrefs, onPrefs, applyPrefs } from './prefs.js';
import { tileChoice, rasterPaint, mapColors, tileSources, tileLayers, vectorGround, noTilesNote, watchTileErrors, onImagery, IMAGERY_HALO, mapStyleSwitch, swapTiles, sharpZoom } from '../shared/maptiles.js';

let mlLoad = null;
/**
 * Load MapLibre once (an ES module, its worker and its style sheet, all from this pack). Resolves only once the
 * style sheet applies, so no map is drawn without it (its controls would jump into place). Pages without a map
 * never load any of it.
 */
export function loadMapLibre() {
  mlLoad ??= Promise.all([
    import('../vendor/maplibre-gl/maplibre-gl.mjs'),
    styleSheet(new URL('../vendor/maplibre-gl/maplibre-gl.css', import.meta.url).href, { first: true }),
  ]).then(([ml]) => {
    ml.setWorkerUrl(new URL('../vendor/maplibre-gl/maplibre-gl-worker.mjs', import.meta.url).href);
    return ml;
  }).catch((e) => { mlLoad = null; throw e; });
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
async function mapsConfig() {
  try { return await api.get('maps/config', null, { maxAgeMs: 300000 }); } catch { return null; }
}
/** shared/maptiles.js tileChoice (with the person's map style): { kind, offline, template, attribution, maxZoom, overlays, imagery, why } */
export async function tileSource() {
  return tileChoice(await mapsConfig(), getPrefs());
}

const num = (name, fallback) => { const v = parseFloat(cssVar(name)); return Number.isFinite(v) ? v : fallback; };
const isDark = () => getTheme().effective !== 'light';

const tilePaint = (src, dark) => {
  const tune = { saturation: num('--map-dim-saturation', -0.7), contrast: num('--map-dim-contrast', 0.18), min: num('--map-dim-min', 0.03), max: num('--map-dim-max', 0.55) };
  return rasterPaint({ dark, colors: mapColors(getPrefs()), tune, imagery: src.imagery });
};

function baseLayers(src) {
  const dark = isDark();
  const layers = [{ id: 'land', type: 'background', paint: { 'background-color': src.kind === 'vector' ? vectorGround(dark) : cssVar('--map-land') } }];
  layers.push(...tileLayers(src, { dark, paint: tilePaint(src, dark) }));
  return layers;
}

function style(src) {
  return { version: 8, sources: tileSources(src, { attribution: esc(src.attribution) }), layers: baseLayers(src) };
}

/**
 * A map in el (sized by CSS):
 *   const m = await createMap(el, ctx, { center, zoom, interactive });
 *   await m.ready; m.line('route', coords); m.points('stops', features); m.fit(coords);
 * Returns null when the page was left meanwhile. Throws when MapLibre cannot start (no WebGL): show
 * mapFallback() then.
 */
export async function createMap(el, ctx, { center = [0, 0], zoom = 13, interactive = true, styleSwitch = true, lazy = false } = {}) {
  // lazy: a map below the fold is made (and MapLibre loaded) when it comes within a screen of view (shared/lazy.js);
  // a page waits for it before ready only when nearNow(el) says it is near
  if (lazy) {
    await new Promise((resolve) => ctx.onCleanup(whenNear(el, resolve)));
    if (!ctx.alive()) return null;
  }
  const [ml, cfg] = await Promise.all([loadMapLibre(), mapsConfig()]);
  let src = tileChoice(cfg, getPrefs());
  watchTilePolicy(src.template && !src.offline ? src.template : null);
  if (!ctx.alive()) return null;
  el.classList.add('map');
  const map = new ml.Map({
    // never opens deeper than the tiles are sharp (the satellite photos end sooner than the map)
    container: el, style: style(src), center, zoom: Math.min(zoom, sharpZoom(src)), attributionControl: false, interactive,
    dragRotate: false, pitchWithRotate: false, fadeDuration: 0, refreshExpiredTiles: false,
    maxZoom: Math.max(src.maxZoom, 18),
  });
  map.touchZoomRotate?.disableRotation();
  if (src.template) map.addControl(new ml.AttributionControl({ compact: false }), 'bottom-right');
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  const mark = () => { el.dataset.tiles = src.kind; el.classList.toggle('map--imagery', onImagery(src)); };
  mark();
  if (src.kind === 'none') el.append(noTilesNote(src, { compact: el.clientHeight < 300, classes: { primary: 'btn btn--primary', secondary: 'btn btn--line' } }));
  const errNote = (text) => h('p', { class: 'map-note map-note--error', role: 'status' }, `-- ${text} --`);
  let offErrors = watchTileErrors(map, el, src, errNote);
  const ready = new Promise((res) => map.once('load', res));
  map.on('error', (e) => console.warn('map', e?.error?.message || e));

  // over satellite pictures the page's casings turn dark and a little wider (IMAGERY_HALO)
  const img = () => onImagery(src);
  const tokenColour = (tok) => (tok === '--map-land' && img() ? IMAGERY_HALO.casing : cssVar(tok));
  const sized = [];
  let framing = null, moved = false;
  map.on('movestart', (e) => { if (e.originalEvent instanceof Event) moved = true; });
  map.on('resize', () => { if (framing && !moved) framing(); });
  const off = onChange(() => { resize(); map.resize(); });
  const repaint = () => { for (const l of baseLayers(src)) for (const [k, v] of Object.entries(l.paint)) if (map.getLayer(l.id)) map.setPaintProperty(l.id, k, v); };
  const recolour = () => { for (const [id, prop, tok] of colours) if (map.getLayer(id)) map.setPaintProperty(id, prop, tokenColour(tok)); };
  const resize = () => { for (const [id, prop, f] of sized) if (map.getLayer(id)) map.setPaintProperty(id, prop, f()); };
  const offTheme = onTheme(() => { repaint(); recolour(); });
  // the Map / Satellite switch (only when the admin allows two or more styles)
  const sw = styleSwitch ? mapStyleSwitch(cfg, getPrefs(), {
    classes: { box: 'map-style', button: 'map-style-btn', active: 'is-on' },
    // after 'load', not while the style is "loaded": isStyleLoaded() reads false while any tiles or sources are
    // still loading, long after 'load' fired (which never fires again), and a pick then was lost
    onChange: (next, s) => { ready.then(() => { if (ctx.alive()) apply(next, s); }); },
  }) : null;
  const apply = (next, s) => {
    const dark = isDark();
    src = swapTiles(map, src, next, { dark, paint: tilePaint(next, dark), attribution: esc(next.attribution) });
    if (map.getLayer('land')) map.setPaintProperty('land', 'background-color', cssVar('--map-land'));
    watchTilePolicy(src.template && !src.offline ? src.template : null);
    offErrors(); offErrors = watchTileErrors(map, el, src, errNote);
    map.setMaxZoom(Math.max(src.maxZoom, 18));
    mark(); recolour(); resize();
    const p = getPrefs();
    if (p) applyPrefs({ ...p, map_style: s.id });
  };
  if (sw) el.append(sw);
  // the person's map colours (dimmed or original) change in place
  let colorsNow = mapColors(getPrefs());
  const offPrefs = onPrefs((p) => { if (mapColors(p) !== colorsNow) { colorsNow = mapColors(p); repaint(); } });
  const colours = [];   // [layer, paint property, token]
  const ro = new ResizeObserver(() => map.resize());
  ro.observe(el);
  ctx.onCleanup(() => { off(); offTheme(); offPrefs(); offErrors(); ro.disconnect(); map.remove(); });

  const layer = (def, size = {}, tokens = {}) => {
    const paint = { ...def.paint };
    for (const [p, f] of Object.entries(size)) { paint[p] = f(); sized.push([def.id, p, f]); }
    for (const [p, tok] of Object.entries(tokens)) { paint[p] = tokenColour(tok); colours.push([def.id, p, tok]); }
    map.addLayer({ ...def, paint });
  };

  return {
    map, ml, ready, tiles: src,
    /** A line through [[lon, lat], ...] (or several: [[[lon, lat], ...], ...]) in a token's colour (the accent by default). */
    line(id, coords, { token = '--accent', width = 3, dash = null } = {}) {
      const multi = Array.isArray(coords[0]?.[0]);
      map.addSource(id, { type: 'geojson', data: { type: 'Feature', properties: {}, geometry: { type: multi ? 'MultiLineString' : 'LineString', coordinates: coords } } });
      layer({ id: `${id}-case`, type: 'line', source: id, layout: { 'line-cap': 'butt', 'line-join': 'miter' }, paint: {} },
        { 'line-width': () => rpx(width + (img() ? IMAGERY_HALO.casingWidth + 1 : 2)) }, { 'line-color': '--map-land' });
      layer({ id, type: 'line', source: id, layout: { 'line-cap': 'butt', 'line-join': 'miter' }, paint: dash ? { 'line-dasharray': dash } : {} },
        { 'line-width': () => rpx(width) }, { 'line-color': token });
    },
    /** Square points: features [{ lon, lat, props }]; size in design px. */
    points(id, list, { token = '--accent', size = 7, ring = '--map-land', sizeBy = null } = {}) {
      map.addSource(id, { type: 'geojson', data: { type: 'FeatureCollection', features: list.map((p) => ({ type: 'Feature', properties: p.props || {}, geometry: { type: 'Point', coordinates: [p.lon, p.lat] } })) } });
      const r = sizeBy ? ['interpolate', ['linear'], ['get', sizeBy.prop], sizeBy.min, rpx(size / 2), sizeBy.max, rpx(size)] : null;
      layer({ id, type: 'circle', source: id, paint: r ? { 'circle-radius': r } : {} },
        { ...(r ? {} : { 'circle-radius': () => rpx(size / 2) }), 'circle-stroke-width': () => rpx(img() ? 2 : 1.5) },
        { 'circle-color': token, 'circle-stroke-color': ring });
    },
    /** A circle of r metres (a flat fill and a 1.5 px edge); returns set(lon, lat, r) to move it. */
    circle(id, lon, lat, r, { token = '--accent' } = {}) {
      const data = (lo, la, rr) => {
        const pts = [];
        const dLat = rr / 111320, dLon = rr / (111320 * Math.cos((la * Math.PI) / 180));
        for (let i = 0; i <= 64; i++) { const a = (i / 64) * 2 * Math.PI; pts.push([lo + dLon * Math.cos(a), la + dLat * Math.sin(a)]); }
        return { type: 'Feature', properties: {}, geometry: { type: 'Polygon', coordinates: [pts] } };
      };
      map.addSource(id, { type: 'geojson', data: data(lon, lat, r) });
      layer({ id: `${id}-fill`, type: 'fill', source: id, paint: { 'fill-opacity': 0.14 } }, {}, { 'fill-color': token });
      // a dark casing under the edge, drawn only over satellite pictures
      layer({ id: `${id}-case`, type: 'line', source: id, paint: {} }, { 'line-width': () => (img() ? rpx(1.5 + IMAGERY_HALO.casingWidth) : 0) }, { 'line-color': '--map-land' });
      layer({ id: `${id}-edge`, type: 'line', source: id, paint: {} }, { 'line-width': () => rpx(img() ? 2 : 1.5) }, { 'line-color': token });
      return (lo, la, rr) => map.getSource(id)?.setData(data(lo, la, rr));
    },
    /** A marker element at [lon, lat] (a square in CSS). */
    marker(lngLat, cls = 'map-pin') {
      return new ml.Marker({ element: h('div', { class: cls, 'aria-hidden': 'true' }) }).setLngLat(lngLat).addTo(map);
    },
    /** f() places the camera now and after resizes, until the person moves the map. */
    frame(f) { framing = f; f(); },
    /** Fit the camera around [[lon, lat], ...]. */
    fit(coords, { pad = 24, maxZoom = 15 } = {}) {
      const pts = coords.filter((c) => c && Number.isFinite(c[0]) && Number.isFinite(c[1]));
      if (!pts.length) return;
      this.frame(() => {
        if (pts.length === 1) { map.jumpTo({ center: pts[0], zoom: Math.min(maxZoom, 14) }); return; }
        const b = boundsOf(pts);
        const p = Math.min(rpx(pad), Math.floor(Math.min(el.clientWidth, el.clientHeight) / 4));
        map.fitBounds(b, { padding: p, duration: 0, maxZoom });
      });
    },
  };
}

/** What a map box shows when it cannot be drawn. */
export const mapFallback = (text = 'the map could not be drawn in this browser') => h('p', { class: 'map-note map-note--fail' }, `-- ${text} --`);

/** [[west, south], [east, north]] around [lon, lat] points. */
export function boundsOf(coords) {
  let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity;
  for (const [lo, la] of coords) { if (lo < w) w = lo; if (lo > e) e = lo; if (la < s) s = la; if (la > n) n = la; }
  return [[w, s], [e, n]];
}

/** Draw a map into el, or the fallback line; returns the handle or null. */
export async function safeMap(el, ctx, opts) {
  try {
    const m = await createMap(el, ctx, opts);
    if (m) await Promise.race([m.ready, new Promise((r) => setTimeout(r, 4000))]);
    return m;
  } catch (e) {
    console.warn(e);
    el.replaceChildren(mapFallback());
    return null;
  }
}
