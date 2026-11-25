// Maps on MapLibre GL (vendor/maplibre-gl, loaded on first use), with tiles chosen offline first
// (shared/maptiles.js): GET /api/v1/maps/config -> the offline map file Sentry Mate serves (raster, or a
// Protomaps vector file drawn with a plain style); the online source the owner turned on (only then does
// the browser ask an outside server); otherwise no tiles: the map shows the track and markers on a plain
// background under a "No map tiles yet" note (for an admin, a one-click online map switch). Tiles that
// fail show a short note with the cause. In the dark theme raster tiles are dimmed (never inverted, so
// streets, labels and water stay readable) unless the person chose original colours; satellite photos are
// never dimmed, and the page's lines, outlines and markers get a dark halo over them. An interactive map
// shows the Map / Satellite switch (under the zoom buttons) when the admin allows more than one style.
// Maps stay sharp and in proportion: MapLibre reads the pixel ratio only in resize(), so the map is
// resized when the pixel ratio or the root size changes; line widths and circle sizes added with
// handle.addLayer(def, sized) are functions of rpx() and are set again; the camera framing (handle.frame)
// runs again after a resize until the person moves the map. The attribution is text, shown from the start.

import * as api from '../shared/api.js';
import { h, cssVar, esc, styleSheet } from '../shared/dom.js';
import { whenNear } from '../shared/lazy.js';
import { rpx, onChange, onTheme, getTheme } from './display.js';
import { flash } from './flash.js';
import { getPrefs, onPrefs } from './prefs.js';
import { tileChoice, rasterPaint, dimmed, mapColors, tileSources, tileLayers, vectorGround, noTilesNote, watchTileErrors, onImagery, IMAGERY_HALO, mapStyleSwitch, swapTiles, sharpZoom } from '../shared/maptiles.js';

let mlLoad = null;
const mlSheet = () => styleSheet(new URL('../vendor/maplibre-gl/maplibre-gl.css', import.meta.url).href, { first: true });
/**
 * Load MapLibre once (an ES module, its worker and its style sheet, all from this pack). Resolves only once the
 * style sheet applies, so no map is drawn without it (its controls would jump into place). Pages without a map
 * never load any of it.
 */
export function loadMapLibre() {
  mlLoad ??= Promise.all([
    import('../vendor/maplibre-gl/maplibre-gl.mjs'),
    mlSheet(),
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
/** GET /maps/config (cached a few minutes), or null when it can't be read. */
async function mapsConfig() {
  try { return await api.get('maps/config', null, { maxAgeMs: 300000 }); } catch { return null; }
}
/** The tile set to use (shared/maptiles.js tileChoice, with the person's map style): { kind, offline, template, attribution, maxZoom, overlays, imagery, styleId, why }. */
export async function tileSource() {
  return tileChoice(await mapsConfig(), getPrefs());
}

const isDark = () => getTheme().effective === 'dark';

const cssNum = (name, fallback) => { const v = parseFloat(cssVar(name)); return Number.isFinite(v) ? v : fallback; };
/** The raster paint for the theme and the person's map colours (satellite photos get the plain paint in tileLayers). */
const tilePaint = (dark, colors) => rasterPaint({ dark, colors, tune: { saturation: cssNum('--map-dim-saturation', -0.25), contrast: cssNum('--map-dim-contrast', 0.12), min: cssNum('--map-dim-min', 0.04), max: cssNum('--map-dim-max', 0.66) } });
/** The pack's blue tint, only over the dimmed look (original colours and satellite photos stay as drawn). */
const tintPaint = (src, dark, colors) => ({ 'background-color': cssVar('--map-tint'), 'background-opacity': dimmed(dark, colors, src) && src.kind === 'raster' ? cssNum('--map-tint-opacity', 0.1) : 0 });

function style(src) {
  const dark = isDark(), colors = mapColors(getPrefs());
  const ground = src.kind === 'vector' ? vectorGround(dark) : cssVar('--map-bg');
  const layers = [{ id: 'bg', type: 'background', paint: { 'background-color': ground } }];
  const sources = tileSources(src, { attribution: esc(src.attribution) });
  if (sources.tiles) {
    // the base tiles, then the names and roads over photos (tiles-overlay-N), under the page's own layers
    layers.push(...tileLayers(src, { dark, paint: tilePaint(dark, colors) }));
    layers.push({ id: 'tint', type: 'background', paint: tintPaint(src, dark, colors) });
  }
  return { version: 8, sources, layers };
}

// Resolves once the page says it is ready (<html data-ready>): a map's setup (the library, its worker, WebGL, the
// style) is the largest piece of work on a page, so afterReady keeps it out of the page's first drawing.
function whenReady(signal) {
  const root = document.documentElement;
  if (root.dataset.ready || typeof MutationObserver !== 'function') return Promise.resolve();
  return new Promise((resolve) => {
    const mo = new MutationObserver(() => { if (root.dataset.ready) { mo.disconnect(); resolve(); } });
    mo.observe(root, { attributes: true, attributeFilter: ['data-ready'] });
    signal?.addEventListener?.('abort', () => { mo.disconnect(); resolve(); }, { once: true });
  });
}

/**
 * A map in container (sized by CSS):
 *   const m = await createMap(el, ctx, { center: [lon, lat], zoom: 15, bounds, interactive: true });
 *   await m.ready; m.addLayer({ id, type: 'line', source, paint }, { 'line-width': () => rpx(4) });
 *   m.frame(() => m.map.fitBounds(b, { padding: rpx(30), duration: 0 }));
 * Returns { map, ml, ready, addLayer, frame, tiles } or null when the page was left meanwhile.
 * With lazy: true a map below the fold is made (and MapLibre loaded) only when it comes within a screen of view
 * (shared/lazy.js; with afterReady too, only after the page is ready and once it is in view): until then this
 * waits, so a page awaits it for ready only when nearNow(el) says it is near.
 * With onShell, the map's foot (its credit and style switch, the page's own) is laid out before MapLibre has arrived
 * and onShell() is called then: a page can be ready with its overlays where they stay, and the library (over a
 * megabyte) draws the map into its reserved place after.
 * Throws when MapLibre cannot start (no WebGL): show mapFallback() then.
 */
export async function createMap(container, ctx, { center = [0, 0], zoom = 14, bounds = null, interactive = true, lazy = false, afterReady = false, onShell = null } = {}) {
  if (lazy) {
    // afterReady: not before the page is drawn and ready, and then once the map is in view (a map's setup is the
    // largest piece of work on a page: it must not hold up the rest)
    if (afterReady) { await whenReady(ctx.signal); if (!ctx.alive()) return null; }
    await new Promise((resolve) => ctx.onCleanup(whenNear(container, resolve, { margin: afterReady ? 0 : 1 })));
    if (!ctx.alive()) return null;
  }
  const mlIn = loadMapLibre();
  mlIn.catch(() => {}); // awaited below; a page left before then does not see its failure
  const [cfg] = await Promise.all([mapsConfig(), onShell ? mlSheet() : null]);
  let src = tileChoice(cfg, getPrefs());
  watchTilePolicy(src.template && !src.offline ? src.template : null);
  if (!ctx.alive()) return null;
  // the map (made below) and the moment it has loaded; the style switch is made before both
  let map = null, loaded;
  const ready = new Promise((res) => { loaded = res; });
  // the credit is the page's own text, there from the start: MapLibre's control fills in only once the tiles have
  // loaded, after the page is ready, and the overlays above it would then move
  const creditText = h('div', { class: 'maplibregl-ctrl-attrib-inner' });
  const credit = h('div', { class: 'maplibregl-ctrl maplibregl-ctrl-attrib' }, creditText);
  const setCredit = (s) => { creditText.textContent = s.attribution || ''; credit.classList.toggle('maplibregl-attrib-empty', !s.attribution); };
  setCredit(src);
  const styleSwitch = interactive ? mapStyleSwitch(cfg, getPrefs(), {
    classes: { box: 'map-style maplibregl-ctrl', button: 'map-style-btn', active: 'is-active' },
    label: 'Map style',
    // after 'load', not while the style is "loaded": map.loaded() and isStyleLoaded() read false while any tiles or
    // sources are still loading, long after 'load' fired (which never fires again), and a pick then was lost
    onChange: (next) => { ready.then(() => { if (ctx.alive()) swap(next); }); },
    // the pick shows on this page; one that could not be saved is gone on the next, so say it
    onError: (err, style) => { if (!api.isAbort(err) && ctx.alive()) flash(`Could not save the map style: ${err?.message || 'no answer from the server'} ${style.name} shows on this page only.`, { kind: 'error' }); },
  }) : null;
  // the overlays' room for the foot: --map-foot-br above it, and on a narrow map (or a phone) --map-foot under the
  // page's bottom-left overlay, as the credit and the switch then take the whole bottom
  const footRoom = (foot) => {
    if (foot) container.parentElement?.style.setProperty('--map-foot-br', `${Math.ceil(foot.offsetHeight)}px`);
    const wide = !!document.documentElement.dataset.phone || (foot && foot.offsetWidth > container.clientWidth * 0.5);
    const h2 = foot && wide ? Math.ceil(foot.offsetHeight) : 0;
    container.parentElement?.style.setProperty('--map-foot', `${h2}px`);
    return h2;
  };
  let shell = null;
  if (onShell) {
    // the foot as MapLibre will lay it out (a bottom control goes in before the ones already there), until it does
    container.classList.add('maplibregl-map');
    shell = h('div', { class: 'maplibregl-ctrl-bottom-right' }, styleSwitch, src.template ? credit : null);
    container.append(shell);
    footRoom(shell);
    onShell();
  }
  let ml;
  try { ml = await mlIn; } finally { shell?.remove(); }
  if (!ctx.alive()) return null;
  map = new ml.Map({
    // never opens deeper than the tiles are sharp (the satellite photos end sooner than the map)
    container, style: style(src), center, zoom: Math.min(zoom, sharpZoom(src)), attributionControl: false, interactive,
    cooperativeGestures: false, dragRotate: false, pitchWithRotate: false, fadeDuration: 0, refreshExpiredTiles: false,
    maxZoom: Math.max(src.maxZoom, 18),
    ...(bounds ? { bounds } : {}),
  });
  if (src.template) map.addControl({ onAdd: () => credit, onRemove: () => credit.remove() }, 'bottom-right');
  const errorNote = (text) => h('p', { class: 'map-note map-note--error', role: 'status' }, text);
  let offErrors = watchTileErrors(map, container, src, errorNote);
  if (interactive) map.addControl(new ml.NavigationControl({ showCompass: false }), 'top-right');
  map.touchZoomRotate?.disableRotation();
  container.dataset.tiles = src.kind;
  container.dataset.imagery = String(onImagery(src));
  if (src.kind === 'none') container.append(noTilesNote(src, { compact: container.clientHeight < 300, classes: { primary: 'btn btn--sm btn--primary', secondary: 'btn btn--sm btn--ghost' } }));
  map.once('load', loaded);
  map.on('error', (e) => { console.warn('map', e && e.error ? e.error.message : e); });

  const sized = [];
  // [layer, paint property, value off photos, value over photos]: the halo the page's own layers get over satellite
  const halo = [];
  const paintHalo = () => {
    const on = onImagery(src);
    for (const [id, prop, plain, photo] of halo) if (map.getLayer(id)) map.setPaintProperty(id, prop, on ? photo() : plain());
  };
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
  // the Map / Satellite switch (only with two or more styles the admin allows): redraws the tiles in place
  const swap = (next) => {
    const dark = isDark();
    src = swapTiles(map, src, next, { dark, paint: tilePaint(dark, mapColors(getPrefs())), attribution: esc(next.attribution) });
    // the tint goes back on top of the new tiles, under the page's own layers
    if (map.getLayer('tint')) map.moveLayer('tint', map.getStyle().layers.find((l) => l.id !== 'tint' && l.id !== 'bg' && !l.id.startsWith('tiles'))?.id);
    repaint();
    paintHalo();
    map.setMaxZoom(Math.max(src.maxZoom, 18));
    container.dataset.imagery = String(onImagery(src));
    handle.tiles = src;
    setCredit(src);
    offErrors(); offErrors = watchTileErrors(map, container, src, errorNote);
    watchTilePolicy(src.template && !src.offline ? src.template : null);
  };
  // bottom right, above the credit: the page's own cards sit at the top
  if (styleSwitch) map.addControl({ onAdd: () => styleSwitch, onRemove: () => styleSwitch.remove() }, 'bottom-right');
  // on a narrow map the credit (and the switch) take the whole bottom: the page's bottom-left overlay moves above
  // them (--map-foot on the map's panel) and the camera keeps routes and markers clear of them
  const foot = container.querySelector('.maplibregl-ctrl-bottom-right');
  let footNow = 0;
  const measureFoot = () => {
    // a bottom-right overlay always sits above the credit and the switch; on a phone the credit and the switch always
    // take the bottom (the credit's text arrives after the map does, and the overlays must not jump up when it does)
    const h2 = footRoom(foot);
    if (h2 === footNow) return;
    footNow = h2;
    // with the page's bottom overlays now above the credit, keep their height clear too
    const over = h2 ? Math.max(0, ...[...(container.parentElement?.querySelectorAll(':scope > .map-overlay--bl, :scope > .map-overlay--br') || [])].map((el) => el.offsetHeight)) : 0;
    map.setPadding({ top: 0, left: 0, right: 0, bottom: over ? h2 + over : 0 });
    if (framing && !userMoved) framing();
  };
  if (foot && typeof ResizeObserver === 'function') {
    const ro = new ResizeObserver(() => measureFoot());
    ro.observe(foot); ro.observe(container);
    ctx.onCleanup(() => ro.disconnect());
  }
  measureFoot(); // at once, so the overlays start where they stay
  const offTheme = onTheme(repaint);
  // the person's map colours (dimmed or original) change in place too
  let colorsNow = mapColors(getPrefs());
  const offPrefs = onPrefs((p) => { if (mapColors(p) !== colorsNow) { colorsNow = mapColors(p); repaint(); } });
  ctx.onCleanup(() => { off(); offTheme(); offPrefs(); offErrors(); map.remove(); });
  const handle = {
    map, ml, ready, tiles: src,
    /**
     * Adds a page layer; sizedPaint values are functions of rpx(), set again on a resize. Over satellite photos a
     * line gets a dark halo under it (its own casing layer, id-casing, turns dark instead when there is one) and a
     * circle's stroke turns dark, so routes, outlines and dots stay clear on fields, forest and roofs alike.
     */
    addLayer(def, sizedPaint = {}) {
      const paint = { ...def.paint };
      for (const [prop, value] of Object.entries(sizedPaint)) { paint[prop] = value(); sized.push([def.id, prop, value]); }
      const id = def.id;
      if (def.type === 'line' && !/-(glow|casing|halo)$/.test(id)) {
        const base = id.replace(/-line$/, '');
        if (!map.getLayer(`${base}-casing`)) {
          const w = sizedPaint['line-width'] || (() => (typeof paint['line-width'] === 'number' ? paint['line-width'] : 1));
          const hw = () => w() + rpx(IMAGERY_HALO.casingWidth);
          sized.push([`${id}-halo`, 'line-width', hw]);
          halo.push([`${id}-halo`, 'line-opacity', () => 0, () => 0.85]);
          map.addLayer({ id: `${id}-halo`, type: 'line', source: def.source, ...(def.filter ? { filter: def.filter } : {}), layout: { 'line-cap': 'round', 'line-join': 'round', ...(def.layout || {}) },
            paint: { 'line-color': IMAGERY_HALO.casing, 'line-width': hw(), 'line-opacity': onImagery(src) ? 0.85 : 0 } });
        }
      }
      if (def.type === 'line' && /-casing$/.test(id) && paint['line-color'] != null) {
        const plain = paint['line-color'], op = paint['line-opacity'] ?? 1;
        halo.push([id, 'line-color', () => plain, () => IMAGERY_HALO.casing], [id, 'line-opacity', () => op, () => 0.9]);
      }
      if (def.type === 'circle' && paint['circle-stroke-color'] != null) {
        const plain = paint['circle-stroke-color'];
        halo.push([id, 'circle-stroke-color', () => plain, () => IMAGERY_HALO.casing]);
      }
      map.addLayer({ ...def, paint });
      paintHalo();
    },
    /** f() places the camera; it runs now and again after resizes until the person moves the map. */
    frame(f) { framing = f; f(); },
  };
  return handle;
}

/** What a map shows when it cannot be drawn (no WebGL, a library that did not load). */
export const mapFallback = (text = 'The map could not be drawn in this browser.') => h('div', { class: 'map-fallback' }, text);

/** [[west, south], [east, north]] around [lon, lat] points. */
export function boundsOf(coords) {
  let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity;
  for (const [lo, la] of coords) { if (lo < w) w = lo; if (lo > e) e = lo; if (la < s) s = la; if (la > n) n = la; }
  return [[w, s], [e, n]];
}

/** Camera padding that keeps a card on top of the map clear, plus a margin (design px). A card that does not lie on
 *  the map (on a phone it is stacked above or below it) takes no room from it. */
export function padClear(mapEl, card, { top = 30, right = 30, bottom = 30, left = 30, beside = false } = {}) {
  const pad = { top: rpx(top), right: rpx(right), bottom: rpx(bottom), left: rpx(left) };
  if (card) {
    const m = mapEl.getBoundingClientRect(), c = card.getBoundingClientRect();
    const onMap = c.width > 0 && c.height > 0 && c.left < m.right && c.right > m.left && c.top < m.bottom && c.bottom > m.top;
    if (!onMap) { /* beside the map, not on it */ } else if (beside) pad.left = Math.max(pad.left, c.right - m.left + rpx(left));
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
