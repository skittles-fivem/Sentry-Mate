// Maps on MapLibre GL (vendor/maplibre-gl, loaded on first use), styled like a night radar: the tiles dimmed
// and tinted indigo, routes as neon traces with a glow. Tiles come from GET /maps/config: Sentry Mate's own
// offline tiles, or the online source an admin turned on (only then does the browser ask an outside server),
// or none (the routes and markers on a plain background, with a note on how to turn tiles on).
// MapLibre reads the pixel ratio only in resize(), so the map is resized when the pixel ratio, the root size
// or the theme changes; line widths are functions of rpx() and set again; the camera framing runs again after
// a resize until the person moves the map. The attribution is escaped text.

import * as api from '../shared/api.js';
import { h, cssVar, esc } from '../shared/dom.js';
import { rpx, onRedraw, onTheme } from './display.js';

let mlLoad = null;
/**
 * Load MapLibre once (an ES module, its worker and its stylesheet, all from this pack). Resolves once the
 * stylesheet is in too: a map built before it lays its canvas and controls out in the flow for a moment,
 * which makes the box around it grow and shrink back.
 */
export function loadMapLibre() {
  mlLoad ??= (() => {
    let css = Promise.resolve();
    if (!document.querySelector('link[data-maplibre]')) {
      const link = h('link', { rel: 'stylesheet', href: '/vendor/maplibre-gl/maplibre-gl.css', dataset: { maplibre: '' } });
      css = new Promise((r) => { link.addEventListener('load', r, { once: true }); link.addEventListener('error', r, { once: true }); });
      document.head.append(link);
    }
    const js = import('../vendor/maplibre-gl/maplibre-gl.mjs').then((ml) => {
      ml.setWorkerUrl(new URL('../vendor/maplibre-gl/maplibre-gl-worker.mjs', import.meta.url).href);
      return ml;
    });
    return Promise.all([js, css]).then(([ml]) => ml);
  })();
  return mlLoad;
}

/** The tiles to use: { kind: 'offline' | 'online' | 'none', template, attribution, maxZoom }. */
export async function tileSource() {
  let cfg = null;
  try { cfg = await api.get('maps/config', null, { maxAgeMs: 300000 }); } catch { cfg = null; }
  const t = cfg?.tiles || {};
  if (t.mode === 'offline' && t.offline_url) return { kind: 'offline', template: t.offline_url, attribution: t.attribution || '', maxZoom: t.max_zoom || 16 };
  if (t.mode === 'online' && t.online_template) return { kind: 'online', template: t.online_template, attribution: t.attribution || '', maxZoom: t.max_zoom || 19 };
  return { kind: 'none', template: null, attribution: '', maxZoom: 18 };
}

const num = (name, fallback) => { const v = parseFloat(cssVar(name)); return Number.isFinite(v) ? v : fallback; };
function baseStyle(src) {
  const layers = [{ id: 'bg', type: 'background', paint: { 'background-color': cssVar('--map-bg') } }];
  const sources = {};
  if (src.template) {
    sources.tiles = { type: 'raster', tiles: [src.template], tileSize: 256, maxzoom: src.maxZoom, attribution: esc(src.attribution) };
    layers.push({ id: 'tiles', type: 'raster', source: 'tiles', paint: {
      'raster-saturation': num('--map-sat', -0.6), 'raster-hue-rotate': num('--map-hue', 0), 'raster-contrast': num('--map-contrast', 0.1),
      'raster-brightness-min': num('--map-bmin', 0), 'raster-brightness-max': num('--map-bmax', 0.55), 'raster-fade-duration': 0,
    } });
    layers.push({ id: 'tint', type: 'background', paint: { 'background-color': cssVar('--map-tint'), 'background-opacity': num('--map-tint-a', 0.25) } });
  }
  return { version: 8, sources, layers };
}

/**
 * A map in el (sized by CSS): const m = await createMap(el, ctx, { bounds, center, zoom, interactive });
 * await m.ready; m.addLayer(def, { 'line-width': () => rpx(4) }); m.frame(() => m.map.fitBounds(...)).
 * Returns null when the page was left meanwhile; throws when MapLibre cannot start (no WebGL).
 */
export async function createMap(el, ctx, { center = [0, 0], zoom = 12, bounds = null, interactive = true, fullscreen = false } = {}) {
  const [ml, src] = await Promise.all([loadMapLibre(), tileSource()]);
  if (!ctx.alive()) return null;
  const map = new ml.Map({
    container: el, style: baseStyle(src), center, zoom, attributionControl: false, interactive,
    dragRotate: false, pitchWithRotate: false, fadeDuration: 0, refreshExpiredTiles: false, maxZoom: Math.max(src.maxZoom, 18),
    ...(bounds ? { bounds, fitBoundsOptions: { padding: rpx(32) } } : {}),
  });
  if (src.template) map.addControl(new ml.AttributionControl({ compact: false }), 'bottom-right');
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  if (fullscreen) map.addControl(new ml.FullscreenControl({ container: el.parentElement || el }), 'top-right');
  map.touchZoomRotate?.disableRotation();
  el.dataset.tiles = src.kind;
  if (src.kind === 'none') el.append(h('p', { class: 'map-note' }, 'No map tiles yet: an admin can turn on offline maps or an online map source in Display and units.'));
  const ready = new Promise((res) => map.once('load', res));
  map.on('error', (e) => console.warn('map', e?.error?.message || e));

  const sized = [];
  let framing = null, moved = false;
  map.on('movestart', (e) => { if (e.originalEvent instanceof Event) moved = true; });
  map.on('resize', () => { if (framing && !moved) framing(); });
  const offRedraw = onRedraw(() => {
    for (const [id, prop, f] of sized) if (map.getLayer(id)) map.setPaintProperty(id, prop, f());
    map.resize();
    map.triggerRepaint();
  });
  const offTheme = onTheme(() => {
    for (const l of baseStyle(src).layers) for (const [k, v] of Object.entries(l.paint)) if (map.getLayer(l.id)) map.setPaintProperty(l.id, k, v);
    for (const f of recolour) f();
  });
  const recolour = [];
  ctx.onCleanup(() => { offRedraw(); offTheme(); map.remove(); });
  return {
    map, ml, ready, tiles: src,
    addLayer(def, sizedPaint = {}) {
      const paint = { ...def.paint };
      for (const [prop, f] of Object.entries(sizedPaint)) { paint[prop] = f(); sized.push([def.id, prop, f]); }
      map.addLayer({ ...def, paint });
    },
    onRecolour(f) { recolour.push(f); },
    frame(f) { framing = f; f(); },
  };
}

/** What a map shows when it cannot be drawn. */
export const mapFallback = (text = 'The map could not be drawn in this browser.') => h('div', { class: 'map-fallback' }, text);

/** [[west, south], [east, north]] around [lon, lat] points. */
export function boundsOf(coords) {
  let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity;
  for (const [lo, la] of coords) { if (lo < w) w = lo; if (lo > e) e = lo; if (la < s) s = la; if (la > n) n = la; }
  if (w === e) { w -= 0.005; e += 0.005; }
  if (s === n) { s -= 0.005; n += 0.005; }
  return [[w, s], [e, n]];
}

/** A neon trace: glow, then the line. data: a LineString Feature or a FeatureCollection. */
export function addTrace(m, id, data, { color = '--mag', width = 3.5, glow = true } = {}) {
  m.map.addSource(id, { type: 'geojson', lineMetrics: true, data });
  const line = { type: 'line', source: id, layout: { 'line-cap': 'round', 'line-join': 'round' } };
  if (glow) m.addLayer({ ...line, id: `${id}-glow`, paint: { 'line-color': cssVar(color), 'line-opacity': 0.3 } }, { 'line-width': () => rpx(width * 4), 'line-blur': () => rpx(width * 2.5) });
  m.addLayer({ ...line, id: `${id}-line`, paint: { 'line-color': cssVar(color) } }, { 'line-width': () => rpx(width) });
  m.onRecolour(() => {
    if (m.map.getLayer(`${id}-glow`)) m.map.setPaintProperty(`${id}-glow`, 'line-color', cssVar(color));
    if (m.map.getLayer(`${id}-line`) && !m.map.getPaintProperty(`${id}-line`, 'line-gradient')) m.map.setPaintProperty(`${id}-line`, 'line-color', cssVar(color));
  });
}

/** A lettered pin (A, B) or a dot marker; tone: mag | cyan | org | grn. */
export function pin(m, lngLat, { text = '', tone = 'mag', title = null } = {}) {
  const el = h('div', { class: ['pin', `pin--${tone}`, !text && 'pin--dot'], title, 'aria-hidden': title ? null : 'true' }, text);
  return new m.ml.Marker({ element: el }).setLngLat(lngLat).addTo(m.map);
}

/** Camera padding (design px). */
export const pad = (n = 32) => ({ top: rpx(n), right: rpx(n), bottom: rpx(n), left: rpx(n) });
