// Maps on MapLibre GL (vendor/maplibre-gl, loaded on first use), in a flat grey style.
//
// Tiles: GET /api/v1/maps/config. 'offline' tiles come from Sentry Mate itself; 'online' tiles from the
// source the owner turned on (only then does the browser ask an outside server); otherwise the map has no
// tiles: a plain land colour, the track and the points, and a line that says how to turn tiles on.
// The raster is made grey (saturation -1) and, in the dark theme, turned dark (brightness min above max
// flips it), so every map looks the same flat grey whatever the source. Colours are tokens (--map-*).
// Line widths and point sizes are design px through rpx(); they are set again when the root size or the
// pixel ratio changes, and the camera framing runs again after a resize until the person moves the map.

import * as api from '../shared/api.js';
import { h, cssVar, esc } from '../shared/dom.js';
import { rpx, onChange, onTheme } from './display.js';

let mlLoad = null;
/** Load MapLibre once (an ES module and its worker, both from this pack). */
export function loadMapLibre() {
  mlLoad ??= import('../vendor/maplibre-gl/maplibre-gl.mjs').then((ml) => {
    ml.setWorkerUrl(new URL('../vendor/maplibre-gl/maplibre-gl-worker.mjs', import.meta.url).href);
    return ml;
  }).catch((e) => { mlLoad = null; throw e; });
  return mlLoad;
}

/** { kind: 'offline' | 'online' | 'none', template, attribution, maxZoom } */
export async function tileSource() {
  let cfg = null;
  try { cfg = await api.get('maps/config', null, { maxAgeMs: 300000 }); } catch { cfg = null; }
  const t = cfg?.tiles || {};
  if (t.mode === 'offline' && t.offline_url) return { kind: 'offline', template: t.offline_url, attribution: t.attribution || '', maxZoom: t.max_zoom || 16 };
  if (t.mode === 'online' && t.online_template) return { kind: 'online', template: t.online_template, attribution: t.attribution || '', maxZoom: t.max_zoom || 19 };
  return { kind: 'none', template: null, attribution: '', maxZoom: 18 };
}

const num = (name, fallback) => { const v = parseFloat(cssVar(name)); return Number.isFinite(v) ? v : fallback; };

function baseLayers(src) {
  const layers = [{ id: 'land', type: 'background', paint: { 'background-color': cssVar('--map-land') } }];
  if (src.template) {
    layers.push({
      id: 'tiles', type: 'raster', source: 'tiles',
      paint: {
        'raster-saturation': -1, 'raster-contrast': num('--map-contrast', 0),
        'raster-brightness-min': num('--map-bright-min', 0), 'raster-brightness-max': num('--map-bright-max', 1),
        'raster-opacity': num('--map-opacity', 1), 'raster-fade-duration': 0,
      },
    });
  }
  return layers;
}

function style(src) {
  const sources = {};
  if (src.template) sources.tiles = { type: 'raster', tiles: [src.template], tileSize: 256, maxzoom: src.maxZoom, attribution: esc(src.attribution) };
  return { version: 8, sources, layers: baseLayers(src) };
}

/**
 * A map in el (sized by CSS):
 *   const m = await createMap(el, ctx, { center, zoom, interactive });
 *   await m.ready; m.line('route', coords); m.points('stops', features); m.fit(coords);
 * Returns null when the page was left meanwhile. Throws when MapLibre cannot start (no WebGL): show
 * mapFallback() then.
 */
export async function createMap(el, ctx, { center = [0, 0], zoom = 13, interactive = true } = {}) {
  const [ml, src] = await Promise.all([loadMapLibre(), tileSource()]);
  if (!ctx.alive()) return null;
  el.classList.add('map');
  const map = new ml.Map({
    container: el, style: style(src), center, zoom, attributionControl: false, interactive,
    dragRotate: false, pitchWithRotate: false, fadeDuration: 0, refreshExpiredTiles: false,
    maxZoom: Math.max(src.maxZoom, 18),
  });
  map.touchZoomRotate?.disableRotation();
  if (src.template) map.addControl(new ml.AttributionControl({ compact: false }), 'bottom-right');
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  el.dataset.tiles = src.kind;
  if (src.kind === 'none') el.append(h('p', { class: 'map-note' }, '-- no map tiles: an admin can turn them on in settings / display --'));
  const ready = new Promise((res) => map.once('load', res));
  map.on('error', (e) => console.warn('map', e?.error?.message || e));

  const sized = [];
  let framing = null, moved = false;
  map.on('movestart', (e) => { if (e.originalEvent instanceof Event) moved = true; });
  map.on('resize', () => { if (framing && !moved) framing(); });
  const off = onChange(() => {
    for (const [id, prop, f] of sized) if (map.getLayer(id)) map.setPaintProperty(id, prop, f());
    map.resize();
  });
  const offTheme = onTheme(() => {
    for (const l of baseLayers(src)) for (const [k, v] of Object.entries(l.paint)) if (map.getLayer(l.id)) map.setPaintProperty(l.id, k, v);
    for (const [id, prop, tok] of colours) if (map.getLayer(id)) map.setPaintProperty(id, prop, cssVar(tok));
  });
  const colours = [];   // [layer, paint property, token]
  const ro = new ResizeObserver(() => map.resize());
  ro.observe(el);
  ctx.onCleanup(() => { off(); offTheme(); ro.disconnect(); map.remove(); });

  const layer = (def, size = {}, tokens = {}) => {
    const paint = { ...def.paint };
    for (const [p, f] of Object.entries(size)) { paint[p] = f(); sized.push([def.id, p, f]); }
    for (const [p, tok] of Object.entries(tokens)) { paint[p] = cssVar(tok); colours.push([def.id, p, tok]); }
    map.addLayer({ ...def, paint });
  };

  return {
    map, ml, ready, tiles: src,
    /** A line through [[lon, lat], ...] (or several: [[[lon, lat], ...], ...]) in a token's colour (the accent by default). */
    line(id, coords, { token = '--accent', width = 3, dash = null } = {}) {
      const multi = Array.isArray(coords[0]?.[0]);
      map.addSource(id, { type: 'geojson', data: { type: 'Feature', properties: {}, geometry: { type: multi ? 'MultiLineString' : 'LineString', coordinates: coords } } });
      layer({ id: `${id}-case`, type: 'line', source: id, layout: { 'line-cap': 'butt', 'line-join': 'miter' }, paint: {} },
        { 'line-width': () => rpx(width + 2) }, { 'line-color': '--map-land' });
      layer({ id, type: 'line', source: id, layout: { 'line-cap': 'butt', 'line-join': 'miter' }, paint: dash ? { 'line-dasharray': dash } : {} },
        { 'line-width': () => rpx(width) }, { 'line-color': token });
    },
    /** Square points: features [{ lon, lat, props }]; size in design px. */
    points(id, list, { token = '--accent', size = 7, ring = '--map-land', sizeBy = null } = {}) {
      map.addSource(id, { type: 'geojson', data: { type: 'FeatureCollection', features: list.map((p) => ({ type: 'Feature', properties: p.props || {}, geometry: { type: 'Point', coordinates: [p.lon, p.lat] } })) } });
      const r = sizeBy ? ['interpolate', ['linear'], ['get', sizeBy.prop], sizeBy.min, rpx(size / 2), sizeBy.max, rpx(size)] : null;
      layer({ id, type: 'circle', source: id, paint: r ? { 'circle-radius': r } : {} },
        { ...(r ? {} : { 'circle-radius': () => rpx(size / 2) }), 'circle-stroke-width': () => rpx(1.5) },
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
      layer({ id: `${id}-edge`, type: 'line', source: id, paint: {} }, { 'line-width': () => rpx(1.5) }, { 'line-color': token });
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
