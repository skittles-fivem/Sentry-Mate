// Maps on MapLibre GL (vendor/maplibre-gl, loaded on first use), styled like a night radar: the tiles dimmed
// and tinted indigo, routes as neon traces with a glow. Tiles come from GET /maps/config: Sentry Mate's own
// offline tiles, or the online source an admin turned on (only then does the browser ask an outside server),
// or none (the routes and markers on a plain background, with a note on how to turn tiles on).
// MapLibre reads the pixel ratio only in resize(), so the map is resized when the pixel ratio, the root size
// or the theme changes; line widths are functions of rpx() and set again; the camera framing runs again after
// a resize until the person moves the map. The attribution is escaped text.

import * as api from '../shared/api.js';
import { h, cssVar, esc } from '../shared/dom.js';
import { rpx, onRedraw, onTheme, getTheme } from './display.js';
import { getPrefs, onPrefs } from './prefs.js';
import { tileChoice, rasterPaint, dimmed, mapColors, tileSources, tileLayers, tileLayerIds, vectorGround, noTilesNote, watchTileErrors, onImagery, IMAGERY_HALO, mapStyleSwitch, swapTiles } from '../shared/maptiles.js';

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
/** The map settings (GET /maps/config) and the person's tiles (shared/maptiles.js tileChoice, with their map_style). */
export async function tileConfig() {
  let cfg = null;
  try { cfg = await api.get('maps/config', null, { maxAgeMs: 300000 }); } catch { cfg = null; }
  return { cfg, src: tileChoice(cfg, getPrefs()) };
}
/** The tiles to use: { kind: 'raster' | 'vector' | 'none', offline, template, attribution, maxZoom, overlays, imagery, styleId, why }. */
export const tileSource = async () => (await tileConfig()).src;

const num = (name, fallback) => { const v = parseFloat(cssVar(name)); return Number.isFinite(v) ? v : fallback; };
const isDark = () => getTheme().look !== 'light';
// in the dark theme the tiles are dimmed (never inverted) with the pack's violet tint over them; the light theme,
// "original colours" and satellite photos show the tile server's own colours
const tune = () => ({ saturation: num('--map-sat', -0.35), contrast: num('--map-contrast', 0.16), min: num('--map-bmin', 0.03), max: num('--map-bmax', 0.58) });
const tilePaint = () => { const dark = isDark(); return { dark, paint: rasterPaint({ dark, colors: mapColors(getPrefs()), tune: tune() }) }; };
const tintPaint = (src) => ({ 'background-color': cssVar('--map-tint'), 'background-opacity': dimmed(isDark(), mapColors(getPrefs()), src) && src.kind === 'raster' ? num('--map-tint-a', 0.12) : 0 });
function baseStyle(src) {
  const dark = isDark(), colors = mapColors(getPrefs());
  const layers = [{ id: 'bg', type: 'background', paint: { 'background-color': src.kind === 'vector' ? vectorGround(dark) : cssVar('--map-bg') } }];
  const sources = tileSources(src, { attribution: esc(src.attribution) });
  if (sources.tiles) {
    layers.push(...tileLayers(src, { dark, paint: rasterPaint({ dark, colors, tune: tune() }) }));
    layers.push({ id: 'tint', type: 'background', paint: tintPaint(src) });
  }
  return { version: 8, sources, layers };
}

/**
 * A map in el (sized by CSS): const m = await createMap(el, ctx, { bounds, center, zoom, interactive });
 * await m.ready; m.addLayer(def, { 'line-width': () => rpx(4) }); m.frame(() => m.map.fitBounds(...)).
 * Returns null when the page was left meanwhile; throws when MapLibre cannot start (no WebGL).
 */
export async function createMap(el, ctx, { center = [0, 0], zoom = 12, bounds = null, interactive = true, fullscreen = false, styleSwitch = interactive } = {}) {
  const [ml, { cfg, src: first }] = await Promise.all([loadMapLibre(), tileConfig()]);
  let src = first;
  watchTilePolicy(src.template && !src.offline ? src.template : null);
  if (!ctx.alive()) return null;
  const map = new ml.Map({
    container: el, style: baseStyle(src), center, zoom, attributionControl: false, interactive,
    dragRotate: false, pitchWithRotate: false, fadeDuration: 0, refreshExpiredTiles: false, maxZoom: Math.max(src.maxZoom, 18),
    ...(bounds ? { bounds, fitBoundsOptions: { padding: rpx(32) } } : {}),
  });
  if (src.template) map.addControl(new ml.AttributionControl({ compact: false }), 'bottom-right');
  // the Map / Satellite switch (only when the admin allows two styles or more), over the zoom buttons
  const sw = styleSwitch ? mapStyleSwitch(cfg, getPrefs(), {
    classes: { box: 'map-style maplibregl-ctrl', button: 'map-style-btn', active: 'is-on' },
    onChange: (next) => { const p = getPrefs(); if (p) p.map_style = next.styleId; setTiles(next); },
  }) : null;
  if (sw) map.addControl({ onAdd: () => sw, onRemove: () => sw.remove() }, 'top-right');
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  if (fullscreen) map.addControl(new ml.FullscreenControl({ container: el.parentElement || el }), 'top-right');
  map.touchZoomRotate?.disableRotation();
  el.dataset.tiles = src.kind;
  el.toggleAttribute('data-imagery', onImagery(src));
  if (src.kind === 'none') el.append(noTilesNote(src, { compact: el.clientHeight < 300, classes: { primary: 'btn btn--sm btn--go', secondary: 'btn btn--sm btn--ghost' } }));
  const errNote = (text) => h('p', { class: 'map-note map-note--error', role: 'status' }, text);
  let offErrors = watchTileErrors(map, el, src, errNote);
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
  const repaint = () => { for (const l of baseStyle(src).layers) for (const [k, v] of Object.entries(l.paint)) if (map.getLayer(l.id)) map.setPaintProperty(l.id, k, v); };
  // another style picked on the switch: new tiles under the page's own layers, the tint just over them, halos redone
  function setTiles(next) {
    if (!next || next.kind === 'none') return;
    watchTilePolicy(next.template && !next.offline ? next.template : null);
    offErrors();
    src = swapTiles(map, src, next, { ...tilePaint(), attribution: esc(next.attribution) });
    const own = (map.getStyle()?.layers || []).find((l) => !['bg', 'tint', ...tileLayerIds(src)].includes(l.id));
    if (map.getLayer('tint')) map.moveLayer('tint', own?.id);
    el.toggleAttribute('data-imagery', onImagery(src));
    offErrors = watchTileErrors(map, el, src, errNote);
    repaint();
    for (const f of recolour) f();
  }
  const offTheme = onTheme(() => {
    repaint();
    for (const f of recolour) f();
  });
  // the person's map colours (dimmed or original) change in place
  let colorsNow = mapColors(getPrefs());
  const offPrefs = onPrefs((p) => { if (mapColors(p) !== colorsNow) { colorsNow = mapColors(p); repaint(); } });
  const recolour = [];
  ctx.onCleanup(() => { offRedraw(); offTheme(); offPrefs(); offErrors(); map.remove(); });
  return {
    map, ml, ready,
    get tiles() { return src; },
    /** Whether the tiles are satellite photos now (draw the page's lines with a dark casing then). */
    imagery: () => onImagery(src),
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

/**
 * A dark casing under a line layer of source (shown only over satellite photos, where a neon line alone gets lost in
 * fields and roofs); width(): the line's width in design px. Add it before the line.
 */
export function addCasing(m, id, source, width, { filter = null } = {}) {
  const op = () => (m.imagery() ? 1 : 0);
  m.addLayer({ id, type: 'line', source, ...(filter ? { filter } : {}), layout: { 'line-cap': 'round', 'line-join': 'round' }, paint: { 'line-color': IMAGERY_HALO.casing, 'line-opacity': op() } },
    { 'line-width': () => rpx(width() + IMAGERY_HALO.casingWidth) });
  m.onRecolour(() => { if (m.map.getLayer(id)) m.map.setPaintProperty(id, 'line-opacity', op()); });
}

/** A neon trace: glow, then the line (with a dark casing over satellite photos). data: a LineString Feature or a FeatureCollection. */
export function addTrace(m, id, data, { color = '--mag', width = 3.5, glow = true } = {}) {
  m.map.addSource(id, { type: 'geojson', lineMetrics: true, data });
  const line = { type: 'line', source: id, layout: { 'line-cap': 'round', 'line-join': 'round' } };
  addCasing(m, `${id}-case`, id, () => width);
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
