// settings/maps: where maps and addresses come from, for everyone on this server, as 'key = value' lines.
// Map tiles (offline, or an online preset or address with its credit), place names and roads over photos, the
// maps people may switch to, the offline map file, address lookup with its rounding and language, and links to
// outside sites (PATCH /settings with its version; admins change them, others read). Each person's own map
// colours and map style stay in settings/display.

import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { head, paint, section, note, link } from '../core/ui.js';
import { editLines } from '../core/kvedit.js';
import { tag, ago } from '../core/fmt.js';
import { LABELS_TEXT, STYLES_TEXT, presetHosts, OFFLINE_ANCHOR, OFFLINE_STEPS, OFFLINE_NOTE, offlineSummary, offlineFileChoices, showOfflineHelp } from '../shared/maptiles.js';

export const id = 'settings-maps';
export const title = () => 'Settings: maps and addresses';

const LANG_RE = /^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$/;
const https = (t) => (t.trim() === '' ? { value: null } : /^https:\/\/\S+$/.test(t.trim()) ? { value: t.trim() } : { error: 'an https:// address, or empty' });

/**
 * tiles.styles: the presets viewers may switch to on a map page, as keys that each turn on and off (saved at once).
 * Returns a line for editLines ({ el, spec }).
 */
function stylesLine(presets, allowed, ro, save) {
  let now = [...allowed];
  const st = h('span', { class: 'ke-st', 'aria-live': 'polite' });
  const err = h('p', { class: 'ke-err', role: 'alert', hidden: true });
  const mark = () => { for (const b of keys.children) b.setAttribute('aria-pressed', String(now.includes(b.dataset.v))); };
  const keys = h('div', { class: 'keys ke-keys', role: 'group', 'aria-label': 'tiles.styles' }, presets.map((p) => h('button', {
    class: 'key', type: 'button', dataset: { v: p.id }, disabled: ro, title: p.name,
    onclick: async () => {
      const before = now;
      now = now.includes(p.id) ? now.filter((x) => x !== p.id) : presets.map((x) => x.id).filter((x) => x === p.id || now.includes(x));
      mark(); err.hidden = true; replace(st, h('span', { class: 'muted' }, 'saving'));
      try { await save(now); replace(st, tag('saved', 'ok')); } catch (e) { now = before; mark(); replace(st, tag('bad', 'bad')); err.hidden = false; replace(err, e?.message || 'not saved'); }
    },
  }, p.id)));
  mark();
  const el = h('div', { class: ['ke', ro && 'ke--ro'], id: 'map-styles', dataset: { row: '' } },
    h('span', { class: 'ke-k' }, 'tiles.styles'), h('span', { class: 'ke-eq', 'aria-hidden': 'true' }, '='),
    h('div', { class: 'ke-v' }, keys), h('span', { class: 'ke-side' }, st),
    h('p', { class: 'ke-help' }, `${STYLES_TEXT} With fewer than two (counting the one chosen above), maps show no switch.`), err);
  return { el, spec: { key: 'tiles.styles' } };
}

export async function mount(ctx) {
  const admin = ctx.admin;
  let server = null, geo = null, maps = null;
  try {
    [server, geo, maps] = await Promise.all([
      ctx.api.get('settings'),
      ctx.api.get('geocode/status').catch(() => null),
      ctx.api.get('maps/config').catch(() => null),
    ]);
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const slot = h('div');
  const patchServer = async (body) => {
    try {
      server = await ctx.api.patch('settings', { version: server.version, ...body });
    } catch (e) {
      if (e.status === 409) {
        server = await ctx.api.get('settings').catch(() => server);
        if (ctx.alive()) paintAll();
        throw Object.assign(new Error('Someone else changed the settings first; the page shows theirs now. Try again.'), { status: 409 });
      }
      throw e;
    }
    [geo, maps] = await Promise.all([ctx.api.get('geocode/status').catch(() => geo), ctx.api.get('maps/config').catch(() => maps)]);
  };
  function geoLine() {
    if (!geo) return 'address status could not be read';
    const od = geo.offline_data || {};
    const bits = [od.present === false ? 'offline place names: not installed' : `offline place names: ${od.name || 'bundled'}${od.places ? `, ${format(od.places, 'count')} places` : ''}${od.built_ms ? `, built ${ago(od.built_ms)}` : ''}`,
      `cache: ${format(geo.cache_size ?? 0, 'count')} addresses looked up`];
    if (geo.online?.last_error) bits.push(`last online error: ${geo.online.last_error}`);
    return [geo.online?.last_error ? tag('warn', 'warn') : tag('ok', 'ok'), ' ', bits.join(' · '), ' · ', link(ctx.link('health'), 'address status in health')];
  }

  function paintAll() {
    const ro = !admin;
    const g = server.geocode || {}, t = server.tiles || {};
    const off = maps?.tiles?.offline_data_present ?? maps?.geocoding?.offline_data_present;
    // the online presets, the default street map first; the chosen one (or custom) from maps/config, else from the address
    const presets = [...(maps?.tiles?.presets || [])].sort((a, b) => (b.id === 'osm') - (a.id === 'osm'));
    const chosenId = maps?.tiles?.preset || t.preset || presets.find((p) => p.url === t.online_url)?.id || 'custom';
    const preset = presets.find((p) => p.id === chosenId) || null;
    const labelsOn = maps?.tiles?.labels ?? t.labels ?? true;
    const allowed = maps?.tiles?.allowed_styles || t.styles || [];
    // after a tiles change: maps/config again (styles, hosts, the chosen preset), then the lines
    const tilesSaved = () => { ctx.api.invalidate?.('maps/'); setTimeout(async () => { maps = await ctx.api.get('maps/config', null, { maxAgeMs: 0 }).catch(() => maps); if (ctx.alive()) paintAll(); }, 0); };
    const presetInfo = (p) => h('li', { dataset: { preset: p.id } }, h('span', { class: p.id === chosenId ? 'tone--accent' : null }, p.id), ` ${p.name}: `,
      presetHosts(p).join(', ') || '-', `, zoom to ${p.max_zoom}`, p.region ? h('span', { class: 'tone--warn' }, ` (${p.region === 'US' ? 'United States only: elsewhere it is blank' : `${p.region} only`})`) : null);
    const tiles = editLines([
      { key: 'tiles.mode', id: 'map-tiles', type: 'choice', value: t.mode, options: [['offline', 'offline'], ['online', 'online']], readOnly: ro, cover: 'web.shell.map-tiles',
        help: [t.mode === 'online' ? `Maps load pictures from the online source; its credit shows on every map: ${maps?.tiles?.attribution || t.attribution || '(none set)'}. ` : off ? 'An offline map file is installed: maps need nothing from outside. ' : 'No offline map file yet: maps draw routes and places on a plain grey ground, with a note (see tiles.offline_file). ',
          'An online source (the OpenStreetMap street map by default) is asked by each viewer\'s browser, so it sees the map area and this server\'s address (as Referer: some tile servers refuse tiles without one), never a page or a car.'],
        save: async (v) => { await patchServer({ tiles: { mode: v } }); tilesSaved(); } },
      ...(presets.length ? [{ key: 'tiles.preset', id: 'tiles-preset', type: 'choice', value: preset ? preset.id : 'custom', readOnly: ro, options: [...presets.map((p) => [p.id, p.id]), ['custom', 'custom']],
        show: (v) => (presets.find((p) => p.id === v)?.name || 'custom'),
        help: [preset ? `${preset.name}: free, no key; its terms (${String(preset.terms || '').replace(/^https:\/\//, '')}) ask for light use and the credit on every map. ` : 'Your own address below (custom). ',
          'A preset fills in the address, credit and zoom below. Each viewer\'s browser loads the pictures from:',
          h('ul', { class: 'preset-list' }, presets.map(presetInfo))],
        save: async (v) => { await patchServer({ tiles: { mode: t.mode, preset: v } }); tilesSaved(); } }] : []),
      { key: 'tiles.online_url', type: 'text', value: (preset ? preset.url : t.online_url) || '', readOnly: ro, attrs: { placeholder: 'https://.../{z}/{x}/{y}.png', inputmode: 'url' },
        parse: (x) => (x.trim() === '' ? { value: null } : /^https:\/\/\S*\{z\}\S*\{x\}\S*\{y\}/.test(x.trim()) ? { value: x.trim() } : { error: 'an https:// address with {z}, {x} and {y} in it, or empty' }),
        save: async (v) => { await patchServer({ tiles: { online_url: v } }); tilesSaved(); } },
      { key: 'tiles.attribution', type: 'text', value: (preset ? preset.attribution : t.attribution) || '', readOnly: ro, keepEmpty: true, help: 'Shown in the corner of every map, as the source asks.', save: (v) => patchServer({ tiles: { attribution: v ?? '' } }) },
      ...(preset?.overlays?.length ? [{ key: 'tiles.labels', id: 'place-names', type: 'bool', value: !!labelsOn, readOnly: ro,
        help: LABELS_TEXT, save: async (v) => { await patchServer({ tiles: { labels: v } }); tilesSaved(); } }] : []),
      ...(presets.length ? [stylesLine(presets, allowed, ro, async (list) => { await patchServer({ tiles: { styles: list } }); tilesSaved(); })] : []),
      (() => {
        // the offline map file: which is in use, (with several) which to use, and how to make one
        const files = offlineFileChoices(maps?.tiles);
        const help = [offlineSummary(maps?.tiles), h('ol', { class: 'offline-steps' }, OFFLINE_STEPS.map((x) => h('li', null, x))), OFFLINE_NOTE];
        return files
          ? { key: 'tiles.offline_file', id: OFFLINE_ANCHOR, type: 'choice', value: t.offline_file || '', options: files.map(([v]) => [v, v || 'largest']), readOnly: ro, help,
            save: async (v) => { await patchServer({ tiles: { offline_file: v || null } }); setTimeout(() => { if (ctx.alive()) paintAll(); }, 0); } }
          : { key: 'tiles.offline_file', id: OFFLINE_ANCHOR, type: 'show', show: () => maps?.tiles?.offline_file || 'none', value: '', help };
      })(),
    ]);
    const addr = editLines([
      { key: 'geocode.mode', id: 'address-lookup', type: 'choice', value: g.mode, options: [['offline', 'offline'], ['online', 'online']], readOnly: ro,
        cover: ['web.settings.general.address-lookup', 'docs.env.tesla-api.nominatim-proxy'],
        help: ['Offline uses the place names bundled with Sentry Mate: nothing leaves the server. Online asks the service you name (your own address server, or a proxy in front of one) with rounded positions, and keeps a cache. ', geoLine()],
        save: (v) => patchServer({ geocode: { mode: v } }) },
      { key: 'geocode.url', type: 'text', value: g.url || '', readOnly: ro, attrs: { placeholder: 'https://', inputmode: 'url' }, parse: https,
        help: 'Only used while lookup is online. The service never gets a VIN or a car name.', save: (v) => patchServer({ geocode: { url: v } }) },
      { key: 'geocode.rounding_m', type: 'number', value: g.rounding_m, unit: 'm', cls: 'ke--short', readOnly: ro, attrs: { min: '0', max: '1000' },
        parse: (x) => { const n = Number(x); return x.trim() !== '' && Number.isInteger(n) && n >= 0 && n <= 1000 ? { value: n } : { error: 'a whole number of metres from 0 to 1000' }; },
        help: 'Positions are rounded to this before an online lookup.', save: (v) => patchServer({ geocode: { rounding_m: v } }) },
      { key: 'geocode.language', id: 'address-language', type: 'text', value: g.language || 'en', cls: 'ke--short', readOnly: ro, attrs: { maxlength: '10' },
        cover: ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid'],
        parse: (x) => (LANG_RE.test(x.trim()) ? { value: x.trim() } : { error: 'not a language code: two or three letters such as en or de, with a region if you like: pt-BR' }),
        help: 'Street and place names in this language where the data has it, else in the local one. A change looks every stored address up again; a code the server refuses is said here.',
        save: (v) => patchServer({ geocode: { language: v } }) },
    ]);
    const out = editLines([
      { key: 'outside_links.maps', id: 'outside-links', type: 'bool', value: !!server.outside_links?.maps, readOnly: ro, help: 'A link from a place to an outside map, opened only when someone clicks it.', save: (v) => patchServer({ outside_links: { maps: v } }) },
      { key: 'outside_links.release_notes', type: 'bool', value: !!server.outside_links?.release_notes, readOnly: ro, help: 'A link from a car software version to its release notes on an outside site.', save: (v) => patchServer({ outside_links: { release_notes: v } }) },
    ]);
    const side = (text) => h('span', { class: 'muted' }, ro ? 'set by an admin; read only' : text);
    replace(slot,
      section({ title: 'maps', side: side('for everyone') }, tiles.el),
      section({ title: 'addresses', side: side('private by default') }, addr.el),
      section({ title: 'outside links', side: side('off by default') }, out.el));
  }
  paintAll();

  paint(ctx, head(ctx),
    h('p', { class: 'note' }, 'Where maps and addresses come from, for everyone on this server. ',
      admin ? 'Each line saves on its own: Enter saves, Esc undoes, a key saves when pressed. ' : 'Set by an admin; shown here to read. ',
      'Your own map colours and map style: ', link(ctx.link('settings-display'), 'settings/display'), '.'),
    slot);
  showOfflineHelp(document.getElementById(OFFLINE_ANCHOR));
  ctx.ready();
}
