// Map tiles for every pack, so each map knows the same things whatever it looks like:
//   tileChoice(cfg, prefs)     which tiles to draw, from GET /maps/config: the offline map file (raster or vector, served
//                              by Sentry Mate), the online style (the admin's, or the person's map_style among the
//                              styles the admin allows: Map / Satellite), or none (with why);
//   rasterPaint(o)             how raster tiles look: the tile server's own colours in the light theme or when the person
//                              chose "original colours" (me/preferences map_colors), dimmed in the dark theme otherwise;
//                              never inverted, so streets, labels and water stay readable; satellite photos are never
//                              dimmed (choice.imagery), so they stay readable in the dark theme;
//   tileSources(choice, o)     the style's sources: 'tiles', and 'tiles-overlay-N' for the names and roads over photos;
//   mapStyleSwitch(cfg, ...)   the Map / Satellite switch a map page shows (only styles the admin allows; remembered per
//                              person as me/preferences map_style), and swapTiles(map, ...) to redraw with the new one;
//   vectorLayers(dark)         a plain style for an offline Protomaps file (land, water, parks, roads, buildings; no
//                              labels: the file has no fonts);
//   noTilesNote(choice, o)     the note a map shows instead of a silent empty background: "No map tiles yet", and for
//                              an admin a one-click OpenStreetMap switch (after a confirm that says what the tile
//                              server sees) and a link to adding an offline map file; for others who to ask;
//   watchTileErrors(map, o)    a short message with the cause when tiles fail (403, 429, blocked, ...), not a grid of
//                              empty squares.
// Nothing here reaches outside the server by itself: online tiles are asked for only once an admin turned them on.

import * as api from './api.js';
import { h, replace } from './dom.js';

/** Where Settings: Display and units explains adding an offline map file. */
export const OFFLINE_HELP = '/settings/maps#offline-map';

/** The dimmed dark rendering (a pack may tune it): darker, a little less colour, a little more contrast. */
export const DIM = { saturation: -0.25, contrast: 0.12, min: 0.04, max: 0.66 };

function why(cfg) {
  const t = cfg?.tiles;
  if (!t) return 'The map settings could not be read.';
  if (t.offline_error) return `The offline map file can't be read (${t.offline_error}).`;
  if (t.offline_present && t.offline_kind && t.offline_kind !== 'vector' && t.offline_kind !== 'raster') return 'The offline map file has tiles this app can’t draw.';
  if (t.mode === 'online') return 'The online map has no tile address set.';
  return 'There is no offline map file on this server, and the online map is off.';
}

/** The online styles a viewer may switch between (the admin's chosen one first); none while tiles are offline. */
export const mapStyles = (cfg) => (cfg?.tiles?.mode === 'online' ? (cfg.tiles.styles || []).filter((s) => s?.template) : []);

/** The person's map style (me/preferences map_style): 'default' (the admin's) or a preset id. */
export const mapStyle = (prefs) => (typeof prefs?.map_style === 'string' ? prefs.map_style : 'default');

/** The style to draw for this person: theirs when the admin allows it, else the admin's. */
export function pickStyle(cfg, prefs) {
  const styles = mapStyles(cfg);
  const want = mapStyle(prefs);
  return styles.find((s) => s.id === want) || styles[0] || null;
}

/**
 * The tiles to draw: { kind: 'raster' | 'vector' | 'none', offline, template, attribution, minZoom, maxZoom, overlays,
 * imagery, styleId, why }. Offline tiles come from this server (template on this origin); online ones from the style
 * the person picked among those an admin allows (prefs.map_style), else the admin's.
 */
export function tileChoice(cfg, prefs = null) {
  const t = cfg?.tiles || {};
  const attribution = t.attribution || '';
  if (t.mode !== 'online' && t.offline_url && (t.offline_kind === 'raster' || t.offline_kind === 'vector')) {
    return { kind: t.offline_kind, offline: true, template: location.origin + t.offline_url, attribution,
      minZoom: t.offline_min_zoom || 0, maxZoom: t.offline_max_zoom || 14, overlays: [], imagery: false, styleId: null, why: null };
  }
  const s = pickStyle(cfg, prefs);
  if (s) return { kind: 'raster', offline: false, template: s.template, attribution: s.attribution || attribution, minZoom: 0, maxZoom: s.max_zoom || 19, overlays: s.overlays || [], imagery: !!s.imagery, styleId: s.id, why: null };
  if (t.mode === 'online' && t.online_template) return { kind: 'raster', offline: false, template: t.online_template, attribution, minZoom: 0, maxZoom: t.max_zoom || 19, overlays: t.overlays || [], imagery: !!t.imagery, styleId: t.preset || null, why: null };
  return { kind: 'none', offline: false, template: null, attribution: '', minZoom: 0, maxZoom: 18, overlays: [], imagery: false, styleId: null, why: why(cfg) };
}

/** The person's map colours: 'dimmed' (default) or 'original'. */
export const mapColors = (prefs) => (prefs?.map_colors === 'original' ? 'original' : 'dimmed');

/**
 * Raster paint for the theme. Every key is always set, so a theme or colours change can set them all again in place.
 * tune: { saturation, contrast, min, max, opacity } for the dimmed look (DIM by default).
 * imagery: satellite photos, never dimmed (they would turn to mud in the dark theme).
 */
export function rasterPaint({ dark = false, colors = 'dimmed', tune = null, imagery = false } = {}) {
  const dim = dark && colors !== 'original' && !imagery;
  const d = { ...DIM, opacity: 1, ...(tune || {}) };
  return {
    'raster-saturation': dim ? d.saturation : 0,
    'raster-contrast': dim ? d.contrast : 0,
    'raster-brightness-min': dim ? d.min : 0,
    'raster-brightness-max': dim ? d.max : 1,
    'raster-hue-rotate': 0,
    'raster-opacity': dim ? d.opacity : 1,
    'raster-fade-duration': 0,
  };
}

/** Whether the map is drawn in its dimmed look (for a pack's tint over it): never over satellite photos. */
export const dimmed = (dark, colors, choice = null) => !!dark && colors !== 'original' && !choice?.imagery;

/** Whether the map under the page's own lines and markers is photos (draw them with a halo / outline then). */
export const onImagery = (choice) => !!choice?.imagery;

/**
 * The halo for lines and markers over satellite photos: a dark edge under a light line reads on fields, forest,
 * water and roofs alike. { casing, casingWidth (px added to the line's width), stroke, glow }.
 */
export const IMAGERY_HALO = { casing: 'rgba(10, 14, 20, 0.85)', casingWidth: 3, stroke: '#ffffff', glow: 'rgba(0, 0, 0, 0.55)' };

const VECTOR = {
  light: { earth: '#f2efe9', water: '#aad3df', park: '#cdebc0', wood: '#add19e', sand: '#f5e9c6', building: '#d9d0c9', minor: '#ffffff', major: '#fcd6a4', highway: '#f8b694', casing: '#c9c2b8', path: '#a59f97', rail: '#a8a8a8', boundary: '#9e9cab' },
  dark: { earth: '#262a30', water: '#1b3447', park: '#23382a', wood: '#22362a', sand: '#3a3628', building: '#363b43', minor: '#4f565f', major: '#8a7350', highway: '#a06e58', casing: '#1a1d21', path: '#59606a', rail: '#5c6169', boundary: '#6f6c80' },
};

/** Layers for an offline Protomaps (basemap schema) vector file in the source 'tiles', drawn under the page's own. */
export function vectorLayers(dark) {
  const c = dark ? VECTOR.dark : VECTOR.light;
  const kind = (...k) => ['in', ['get', 'kind'], ['literal', k]];
  const w = (a, b) => ['interpolate', ['exponential', 1.6], ['zoom'], 8, a, 18, b];
  const line = (id, layer, filter, color, width, extra = {}) => ({ id, type: 'line', source: 'tiles', 'source-layer': layer, filter, layout: { 'line-cap': 'round', 'line-join': 'round' }, paint: { 'line-color': color, 'line-width': width, ...extra } });
  return [
    { id: 'tiles-earth', type: 'fill', source: 'tiles', 'source-layer': 'earth', paint: { 'fill-color': c.earth } },
    { id: 'tiles-wood', type: 'fill', source: 'tiles', 'source-layer': 'landcover', filter: kind('forest', 'wood', 'scrub'), paint: { 'fill-color': c.wood, 'fill-opacity': 0.6 } },
    { id: 'tiles-park', type: 'fill', source: 'tiles', 'source-layer': 'landuse', filter: kind('park', 'nature_reserve', 'garden', 'grass', 'forest', 'wood', 'cemetery', 'golf_course', 'playground', 'pitch'), paint: { 'fill-color': c.park } },
    { id: 'tiles-sand', type: 'fill', source: 'tiles', 'source-layer': 'landuse', filter: kind('beach', 'sand'), paint: { 'fill-color': c.sand } },
    { id: 'tiles-water', type: 'fill', source: 'tiles', 'source-layer': 'water', paint: { 'fill-color': c.water } },
    { id: 'tiles-building', type: 'fill', source: 'tiles', 'source-layer': 'buildings', minzoom: 14, paint: { 'fill-color': c.building, 'fill-opacity': 0.9 } },
    line('tiles-path', 'roads', kind('path'), c.path, w(0.4, 1.6), { 'line-dasharray': [2, 1.5] }),
    line('tiles-rail', 'roads', kind('rail'), c.rail, w(0.4, 1.4)),
    line('tiles-casing', 'roads', kind('major_road', 'highway'), c.casing, w(1.6, 16)),
    line('tiles-minor', 'roads', kind('minor_road', 'other'), c.minor, w(0.4, 9)),
    line('tiles-major', 'roads', kind('major_road', 'medium_road'), c.major, w(0.9, 13)),
    line('tiles-highway', 'roads', kind('highway'), c.highway, w(1.2, 15)),
    line('tiles-boundary', 'boundaries', ['<=', ['to-number', ['get', 'kind_detail'], 4], 4], c.boundary, 1, { 'line-dasharray': [3, 2] }),
  ];
}

/** The background colour under an offline vector map. */
export const vectorGround = (dark) => (dark ? VECTOR.dark.earth : VECTOR.light.earth);

/**
 * The style's source for the tiles (raster or vector), or null for none.
 * rasterSize: the raster tile size to draw at (256, or larger on a big interface).
 */
export function tileSourceDef(choice, { rasterSize = 256, attribution = choice?.attribution } = {}) {
  if (!choice || choice.kind === 'none') return null;
  if (choice.kind === 'vector') return { type: 'vector', tiles: [choice.template], minzoom: choice.minZoom, maxzoom: choice.maxZoom, attribution };
  return { type: 'raster', tiles: [choice.template], tileSize: rasterSize, minzoom: choice.minZoom || 0, maxzoom: choice.maxZoom, attribution };
}

/** The overlay source ids (names and roads over photos), in drawing order. */
export const overlayIds = (choice) => (choice?.kind === 'raster' ? (choice.overlays || []).map((_, i) => `tiles-overlay-${i}`) : []);

/** Every source the tiles need: { tiles, 'tiles-overlay-0', ... } (empty for none). Add each with map.addSource. */
export function tileSources(choice, opts = {}) {
  const base = tileSourceDef(choice, opts);
  if (!base) return {};
  const out = { tiles: base };
  overlayIds(choice).forEach((id, i) => {
    out[id] = { type: 'raster', tiles: [choice.overlays[i]], tileSize: opts.rasterSize || 256, minzoom: 0, maxzoom: choice.maxZoom };
  });
  return out;
}

/**
 * The tile layers for the choice (one raster layer and its overlays, or the vector layers), with the raster paint
 * given. Satellite photos (choice.imagery) always get the plain paint: they are never dimmed.
 */
export function tileLayers(choice, { dark = false, paint = null } = {}) {
  if (!choice || choice.kind === 'none') return [];
  if (choice.kind === 'vector') return vectorLayers(dark);
  const base = choice.imagery ? rasterPaint({ imagery: true }) : (paint || rasterPaint({ dark }));
  return [{ id: 'tiles', type: 'raster', source: 'tiles', paint: base },
    ...overlayIds(choice).map((id) => ({ id, type: 'raster', source: id, paint: rasterPaint({ imagery: true }) }))];
}

/** Sets the tile layers' colours again in place (a theme or map colours change). */
export function repaintTiles(map, choice, { dark = false, paint = null } = {}) {
  for (const l of tileLayers(choice, { dark, paint })) {
    if (!map.getLayer(l.id)) continue;
    for (const [k, v] of Object.entries(l.paint)) map.setPaintProperty(l.id, k, v);
  }
}

/** The tile layers' ids (to put the page's own layers above them). */
export const tileLayerIds = (choice) => tileLayers(choice).map((l) => l.id);

/**
 * Redraws a map with another tile choice (the person switched Map / Satellite): the old tile layers and sources go,
 * the new ones go under the page's own layers. Returns the new choice.
 */
export function swapTiles(map, from, to, { rasterSize = 256, dark = false, paint = null, attribution } = {}) {
  for (const id of tileLayerIds(from)) if (map.getLayer(id)) map.removeLayer(id);
  for (const id of ['tiles', ...overlayIds(from)]) if (map.getSource(id)) map.removeSource(id);
  const layers = map.getStyle()?.layers || [];
  const before = layers.find((l) => l.type !== 'background')?.id;
  for (const [id, def] of Object.entries(tileSources(to, { rasterSize, attribution }))) map.addSource(id, def);
  for (const l of tileLayers(to, { dark, paint })) map.addLayer(l, before);
  return to;
}

/** Saves the person's map style (me/preferences map_style); resolves with the saved preferences. */
export async function setMapStyle(id) {
  const p = await api.patch('me/preferences', { map_style: id });
  api.invalidate('me/preferences');
  return p;
}

/**
 * The Map / Satellite switch for a map page, or null when the admin allows only one style (or tiles are offline).
 * classes: { box, button, active } for the pack's look. onChange(choice, style) after the person picked (the pick is
 * saved for them); the page redraws its tiles with swapTiles.
 */
export function mapStyleSwitch(cfg, prefs, { classes = {}, onChange = () => {}, label = 'Map style' } = {}) {
  const styles = mapStyles(cfg);
  if (styles.length < 2) return null;
  const c = { box: 'map-style', button: 'map-style-btn', active: 'is-active', ...classes };
  let current = pickStyle(cfg, prefs)?.id;
  const buttons = styles.map((s) => h('button', {
    class: c.button, type: 'button', title: s.name, 'aria-pressed': String(s.id === current), dataset: { mapStyle: s.id },
    onclick: async () => {
      if (s.id === current) return;
      current = s.id;
      for (const b of buttons) {
        const on = b.dataset.mapStyle === current;
        b.setAttribute('aria-pressed', String(on));
        b.classList.toggle(c.active, on);
      }
      const next = tileChoice(cfg, { map_style: s.id });
      onChange(next, s);
      try { await setMapStyle(s.id); } catch { /* shown as picked for this page; saved next time */ }
    },
  }, s.short || s.name));
  for (const b of buttons) b.classList.toggle(c.active, b.dataset.mapStyle === current);
  return h('div', { class: c.box, role: 'group', 'aria-label': label, dataset: { mapStyles: String(styles.length) } }, ...buttons);
}

function host(template) {
  try { return new URL(String(template).replace(/\{[^}]*\}/g, '0')).host; } catch { return 'the tile server'; }
}

/** Why tiles failed, in a short sentence, from MapLibre's error. */
export function tileErrorText(err, choice) {
  const status = err?.status;
  const where = choice?.offline ? 'this server' : host(choice?.template);
  if (choice?.offline) {
    if (status === 503) return 'Map tiles did not load: the offline map file can’t be read.';
    if (status === 401) return 'Map tiles did not load: you were signed out.';
    if (status === 404) return 'Map tiles did not load: the offline map is no longer on this server.';
  }
  if (status === 403) return `Map tiles did not load: ${where} refused them (403). Tile servers block requests without this site’s address (Referer), or from an address that asks too much.`;
  if (status === 429) return `Map tiles did not load: ${where} says too many requests (429). Try again later.`;
  if (status === 401) return `Map tiles did not load: ${where} needs a key (401).`;
  if (status === 404) return `Map tiles did not load: ${where} has no such tiles (404). Check the tile address in Settings: Display and units.`;
  if (status >= 500) return `Map tiles did not load: ${where} has a problem (${status}). Try again later.`;
  return `Map tiles did not load: this browser could not reach ${where} (no connection, a blocker, or the page needs a reload after a settings change).`;
}

/**
 * Shows a note in container when tiles fail more than they load: note(text) builds the pack's element; it is put in
 * container once and its text updated. Returns a function that stops watching.
 */
export function watchTileErrors(map, container, choice, note) {
  if (!choice || choice.kind === 'none') return () => {};
  let ok = 0, bad = 0, el = null, probed = null, stopped = false;
  const show = (err) => {
    if (stopped || bad <= ok) return;
    const text = tileErrorText(err, choice);
    if (!el) { el = note(text); container.append(el); } else if (el.textContent !== text) el.textContent = text;
    el.dataset.tileError = String(err?.status ?? 'unreachable');
  };
  const onData = (e) => { if (e.sourceId === 'tiles' && e.tile && e.dataType === 'source') { ok++; if (el && ok > bad * 2) { el.remove(); el = null; bad = 0; } } };
  const onError = (e) => {
    const isTile = e?.sourceId === 'tiles' || (e?.error?.url && choice.template && String(e.error.url).startsWith(choice.template.split('{')[0]));
    if (!isTile) return;
    bad++;
    if (bad <= ok) return;
    if (e.error?.status || probed) { show(e.error?.status ? e.error : probed); return; }
    // tiles drawn as images say nothing about why they failed: ask for the same tile once to learn the status (the
    // same server, the same tile number; nothing new leaves)
    const c = e.tile?.tileID?.canonical;
    if (!c) { show(e.error); return; }
    probed = { status: undefined };
    const url = choice.template.replace('{z}', c.z).replace('{x}', c.x).replace('{y}', c.y);
    fetch(url, { credentials: choice.offline ? 'same-origin' : 'omit', cache: 'no-store' })
      .then((r) => { probed = { status: r.ok ? undefined : r.status }; }, () => { probed = { status: undefined }; })
      .then(() => show(probed));
  };
  map.on('data', onData);
  map.on('error', onError);
  return () => { stopped = true; map.off('data', onData); map.off('error', onError); el?.remove(); };
}

let meLoad = null;

const role = () => { meLoad ??= api.get('me', null, { maxAgeMs: 60000 }).then((m) => m?.role || null, () => { meLoad = null; return null; }); return meLoad; };

/** The preset a no-tiles note offers: the server's default (satellite), else OpenStreetMap. */
const offeredPreset = (cfg) => {
  const ps = cfg?.tiles?.presets || [];
  return ps.find((x) => x.id === 'satellite') || ps.find((x) => x.id === 'osm') || null;
};

/** Every host a preset's pictures come from (its base and its overlays), each once. */
export function presetHosts(p) {
  const out = [];
  for (const u of [p?.url || p?.template, ...(p?.overlays || [])]) {
    const x = u ? host(u) : null;
    if (x && x !== 'the tile server' && !out.includes(x)) out.push(x);
  }
  return out;
}

/** Turns the default online preset on (satellite; Settings: Display and units does the same); resolves when saved. */
export async function useOnlineMap() {
  const [cfg, server] = await Promise.all([api.get('maps/config'), api.get('settings')]);
  const p = offeredPreset(cfg);
  if (!p) throw new Error('This server offers no online map preset.');
  await api.patch('settings', { version: server.version, tiles: { mode: 'online', preset: p.id } });
  api.invalidate('maps/');
}

/**
 * The note a map with no tiles shows over its plain background (routes and markers still draw).
 * classes: { box, title, text, actions, primary, secondary, error } for the pack's own look.
 * onDone(): after the online map was turned on (default: load the page again, for the server's new policy).
 */
export function noTilesNote(choice, { classes = {}, compact = false, onDone = () => location.reload() } = {}) {
  const c = { box: 'map-empty', title: 'map-empty-title', text: 'map-empty-text', actions: 'map-empty-actions', primary: 'map-empty-btn', secondary: 'map-empty-btn', error: 'map-empty-error', ...classes };
  const text = h('p', { class: c.text }, choice?.why || 'There is no map to draw under the routes yet.');
  const actions = h('div', { class: c.actions });
  const err = h('p', { class: c.error, role: 'alert', hidden: true });
  const box = h('div', { class: [c.box, compact ? `${c.box}--compact` : null], role: 'note', dataset: { mapEmpty: 'none' } },
    h('p', { class: c.title }, 'No map tiles yet'), text, actions, err);
  const plain = () => {
    replace(actions,
      h('button', { class: c.primary, type: 'button', onclick: ask }, 'Use online map (satellite)'),
      h('a', { class: c.secondary, href: OFFLINE_HELP }, 'Add an offline map file'));
  };
  const ask = async () => {
    err.hidden = true;
    const cfg = await api.get('maps/config', null, { maxAgeMs: 60000 }).catch(() => null);
    box.dataset.mapEmpty = 'confirm';
    const hosts = presetHosts(offeredPreset(cfg) || { url: 'https://server.arcgisonline.com/' }).join(' and ');
    replace(text, `Turn on the satellite map for everyone? Each viewer’s browser then loads the map pictures from ${hosts}, which sees the viewer’s IP address, this server’s address and the map area shown (never a car or a page). You can turn it off or pick another map in Settings: Display and units.`);
    const yes = h('button', { class: c.primary, type: 'button', onclick: async () => {
      yes.disabled = true;
      try { await useOnlineMap(); replace(text, 'The online map is on. Loading it…'); actions.replaceChildren(); onDone(); } catch (e) { yes.disabled = false; err.hidden = false; err.textContent = `Could not turn the online map on: ${e?.message || e}`; }
    } }, 'Turn on');
    replace(actions, yes, h('button', { class: c.secondary, type: 'button', onclick: () => { box.dataset.mapEmpty = 'none'; replace(text, choice?.why || ''); plain(); } }, 'Cancel'));
  };
  role().then((r) => {
    if (r === 'admin') plain();
    else { actions.remove(); text.append(' Ask an admin to turn on the map.'); }
  });
  return box;
}

/* ---------- for Settings: Display and units (the map rows every pack shows) */

/** The map colours choice, its words and what it does. */
export const MAP_COLORS_CHOICES = [['dimmed', 'Dimmed'], ['original', 'Original colours']];
export const MAP_COLORS_TEXT = 'How maps look in the dark theme. Dimmed makes the map darker so it sits with the dark pages, with streets, labels and water still clear; original colours shows the map as the tile server draws it. The light theme always shows the original colours.';

/** The anchor id of the offline map row (OFFLINE_HELP links to it). */
export const OFFLINE_ANCHOR = 'offline-map';

/** How to make an offline map file for an area, as steps (plain text; nothing here is a link). */
export const OFFLINE_STEPS = [
  'On your own computer, get the free pmtiles tool from Protomaps (github.com/protomaps/go-pmtiles).',
  'Cut your area out of a Protomaps world map: pmtiles extract https://build.protomaps.com/20261101.pmtiles my-area.pmtiles --bbox=WEST,SOUTH,EAST,NORTH --maxzoom=15 (a date from build.protomaps.com; a city is tens of MB, a country a few GB).',
  'Copy my-area.pmtiles into the maps folder of Sentry Mate’s data folder (data/maps/). It is used at once; with several files, the largest, or the one chosen here.',
];
export const OFFLINE_NOTE = 'Sentry Mate never downloads a map by itself, and with an offline file no tile request leaves the server. A Protomaps (vector) file draws land, water, parks, roads and buildings, without names; a raster .pmtiles file shows its pictures as made.';

/** A size in bytes, short (48 MB). */
export function bytesText(n) {
  if (!Number.isFinite(n)) return '';
  const u = ['bytes', 'KB', 'MB', 'GB', 'TB'];
  let i = 0, v = n;
  while (v >= 1000 && i < u.length - 1) { v /= 1000; i++; }
  return `${i ? v.toFixed(v < 10 ? 1 : 0) : v} ${u[i]}`;
}

/** One sentence on the offline map file, from /maps/config's tiles. */
export function offlineSummary(t) {
  if (!t) return 'The map settings could not be read.';
  if (t.offline_error) return `The offline map file can’t be read: ${t.offline_error}.`;
  if (!t.offline_present) return 'No offline map file on this server yet: with offline tiles, maps draw routes and places on a plain background, with a note saying so.';
  const f = (t.offline_files || []).find((x) => x.name === t.offline_file);
  const kind = t.offline_kind === 'vector' ? 'a vector map (no names)' : 'a raster map';
  const zoom = t.offline_max_zoom != null ? `, down to zoom ${t.offline_max_zoom}` : '';
  return `Offline map file: ${t.offline_file}${f ? ` (${bytesText(f.bytes)})` : ''}, ${kind}${zoom}.${t.mode === 'online' ? ' Not used while tiles are online.' : ''}`;
}

/** The choices for the offline file (when there is more than one): the largest, or a file by name. */
export function offlineFileChoices(t) {
  const files = t?.offline_files || [];
  if (files.length < 2) return null;
  return [['', 'The largest'], ...files.map((f) => [f.name, `${f.name} (${bytesText(f.bytes)})`])];
}

/** Scrolls to the offline map row when the page was opened from a map's "Add an offline map file". */
export function showOfflineHelp(el) {
  if (location.hash !== `#${OFFLINE_ANCHOR}` || !el) return;
  requestAnimationFrame(() => el.scrollIntoView({ block: 'start' }));
}

/* ---------- the online map rows (Settings: Display and units) */

/** What the place names and roads switch does. */
export const LABELS_TEXT = 'Draws town names, borders and roads over the satellite pictures (from the same server), so places can be found.';
/** What the viewers' styles row does. */
export const STYLES_TEXT = 'The maps people may switch to on a map page (Map / Satellite). Each person’s pick is remembered for them; the one chosen above is the default.';
/** Words for a preset in a list: its name, and its region when not worldwide. */
export const presetLabel = (p) => (p.region ? `${p.name}` : p.name);
