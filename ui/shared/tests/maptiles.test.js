// Tests for maptiles.js: which tiles a map draws, the dark paint (dimmed, never inverted), the tile error causes and
// the offline map summary.
import { test } from 'node:test';
import assert from 'node:assert/strict';

globalThis.location ??= { origin: 'https://sentry.example:10101', hash: '' };
const mt = await import('../maptiles.js');

const presets = [{ id: 'osm', url: 'https://tile.openstreetmap.org/{z}/{x}/{y}.png' }];

test('which tiles a map draws', () => {
  const none = mt.tileChoice({ tiles: { mode: 'offline', offline_url: null, offline_present: false, presets } });
  assert.equal(none.kind, 'none');
  assert.match(none.why, /no offline map file/);
  assert.equal(mt.tileChoice(null).kind, 'none');
  assert.match(mt.tileChoice({ tiles: { mode: 'offline', offline_error: 'x.pmtiles: damaged' } }).why, /can.t be read \(x\.pmtiles: damaged\)/);
  const off = mt.tileChoice({ tiles: { mode: 'offline', offline_url: '/api/v1/maps/tiles/{z}/{x}/{y}', offline_present: true, offline_kind: 'vector', offline_max_zoom: 15, attribution: 'OSM' } });
  assert.deepEqual([off.kind, off.offline, off.template, off.maxZoom], ['vector', true, 'https://sentry.example:10101/api/v1/maps/tiles/{z}/{x}/{y}', 15]);
  const on = mt.tileChoice({ tiles: { mode: 'online', online_template: presets[0].url, max_zoom: 19, offline_url: null } });
  assert.deepEqual([on.kind, on.offline, on.template], ['raster', false, presets[0].url]);
});

test('dark maps are dimmed, never inverted; light and original colours are left alone', () => {
  const dim = mt.rasterPaint({ dark: true });
  assert.ok(dim['raster-brightness-min'] < dim['raster-brightness-max'], 'min below max: not an inversion');
  assert.ok(dim['raster-brightness-max'] < 1 && dim['raster-brightness-max'] > 0.4);
  const plain = { 'raster-saturation': 0, 'raster-contrast': 0, 'raster-brightness-min': 0, 'raster-brightness-max': 1, 'raster-hue-rotate': 0, 'raster-opacity': 1, 'raster-fade-duration': 0 };
  assert.deepEqual(mt.rasterPaint({ dark: false }), plain);
  assert.deepEqual(mt.rasterPaint({ dark: true, colors: 'original' }), plain);
  assert.deepEqual(Object.keys(dim).sort(), Object.keys(plain).sort(), 'the same keys, so a change sets them all again');
  assert.equal(mt.mapColors({ map_colors: 'original' }), 'original');
  assert.equal(mt.mapColors(null), 'dimmed');
});

test('a tile error says its cause', () => {
  const osm = { offline: false, template: presets[0].url };
  assert.match(mt.tileErrorText({ status: 403 }, osm), /tile\.openstreetmap\.org refused them \(403\)/);
  assert.match(mt.tileErrorText({ status: 429 }, osm), /too many requests/);
  assert.match(mt.tileErrorText({}, osm), /could not reach tile\.openstreetmap\.org/);
  assert.match(mt.tileErrorText({ status: 503 }, { offline: true, template: '/x' }), /offline map file can.t be read/);
});

test('the offline map summary and file choices', () => {
  assert.match(mt.offlineSummary({ offline_present: false }), /No offline map file/);
  const t = { mode: 'offline', offline_present: true, offline_file: 'a.pmtiles', offline_kind: 'raster', offline_max_zoom: 14,
    offline_files: [{ name: 'a.pmtiles', bytes: 48_200_000 }, { name: 'b.pmtiles', bytes: 1000 }] };
  assert.equal(mt.offlineSummary(t), 'Offline map file: a.pmtiles (48 MB), a raster map, down to zoom 14.');
  assert.deepEqual(mt.offlineFileChoices(t).map(([v]) => v), ['', 'a.pmtiles', 'b.pmtiles']);
  assert.equal(mt.offlineFileChoices({ offline_files: [{ name: 'a.pmtiles', bytes: 1 }] }), null);
  assert.equal(mt.bytesText(1500), '1.5 KB');
});

test('a map opens no deeper than its tiles are sharp, also after a switch', () => {
  const sat = { kind: 'raster', template: 'https://a.example/{z}/{x}/{y}', maxZoom: 19, overlays: [] };
  const us = { ...sat, maxZoom: 16 };
  assert.equal(mt.sharpZoom(sat), 18);
  assert.equal(mt.sharpZoom(us), 15);
  assert.equal(mt.sharpZoom(us, 512), 16);
  assert.equal(mt.sharpZoom({ kind: 'raster', maxZoom: 14 }), 13);
  assert.equal(mt.sharpZoom({ kind: 'vector', maxZoom: 15 }), 22);
  assert.equal(mt.sharpZoom(null), 22);
  assert.ok(mt.SPOT_ZOOM >= 14 && mt.SPOT_ZOOM <= mt.sharpZoom(sat));
  // a fake map: the switch to tiles that end sooner zooms out to them; a switch back keeps the zoom
  const fake = (zoom) => ({ zoom, layers: [], sources: {}, getLayer: () => null, getSource: () => null, removeLayer() {}, removeSource() {},
    getStyle() { return { layers: this.layers }; }, addSource(id, d) { this.sources[id] = d; }, addLayer(l) { this.layers.push(l); },
    getZoom() { return this.zoom; }, jumpTo(o) { if (o.zoom != null) this.zoom = o.zoom; } });
  const m = fake(17);
  mt.swapTiles(m, sat, us);
  assert.equal(m.zoom, 15);
  mt.swapTiles(m, us, sat);
  assert.equal(m.zoom, 15);
  const same = fake(17);
  mt.swapTiles(same, us, us);
  assert.equal(same.zoom, 17, 'redrawing the same tiles moves nothing');
});
