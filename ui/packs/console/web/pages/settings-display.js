// settings/display: how numbers, words and maps look, as 'key = value' lines.
// Yours (PATCH /me/preferences: saved and applied at once, they follow you to every browser): units, range,
// theme, UI pack, interface size, follow screen size, language. Below them the server's defaults and where
// addresses and map pictures come from (PATCH /settings with its version; admins change them, others read).

import { h, replace } from '../shared/dom.js';
import { getUnits, format } from '../shared/units.js';
import { isSupportedLocale, onLocaleChange } from '../shared/i18n.js';
import { formatDate, isValidZone } from '../shared/time.js';
import { head, paint, section, note, link } from '../core/ui.js';
import { editLines } from '../core/kvedit.js';
import { getPrefs, savePrefs, onPrefs } from '../core/prefs.js';
import { getInterface, applyInterface, getTheme, applyTheme, onChange } from '../core/display.js';
import { tag, ago } from '../core/fmt.js';
import { toast } from '../core/toast.js';

export const id = 'settings-display';
export const title = () => 'Settings: display';

const LOCALES = [['auto', 'auto'], ['en-US', 'en-US'], ['en-GB', 'en-GB'], ['de-DE', 'de-DE'], ['fr-FR', 'fr-FR'], ['nl-NL', 'nl-NL']].filter(([v]) => v === 'auto' || isSupportedLocale(v));
const PACKS = [['console', 'console'], ['instrument', 'instrument'], ['logbook', 'logbook']];
const LANG_RE = /^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$/;
const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
const BROWSER_ZONE = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
const https = (t) => (t.trim() === '' ? { value: null } : /^https:\/\/\S+$/.test(t.trim()) ? { value: t.trim() } : { error: 'an https:// address, or empty' });

export async function mount(ctx) {
  const admin = ctx.admin;
  let [about, server, geo, maps] = await Promise.all([
    ctx.api.get('about', null, { maxAgeMs: 600000 }).catch(() => null),
    ctx.api.get('settings').catch(() => null),
    ctx.api.get('geocode/status').catch(() => null),
    ctx.api.get('maps/config').catch(() => null),
  ]);
  if (!ctx.alive()) return;
  const p = getPrefs() || {};
  const u = getUnits();
  const pref = (patch) => savePrefs(patch);

  /* ---------------------------------------------------------------- yours */
  const example = h('p', { class: 'note' });
  const paintExample = () => replace(example, 'shown now as: ', format(100, 'distance'), ' · ', format(100, 'speed'), ' · ', format(20, 'temperature'), ' · ', format(290, 'pressure'),
    '. The server keeps metric values (100 km, 20 °C, 2.90 bar); only how you see them changes.');
  paintExample();
  const def = (k) => {
    const d = k === 'range' ? server?.range : server?.units?.[k];
    const words = { c: '°C', f: '°F', mi: 'miles' };
    return d ? ` Server default: ${words[d] || d}. Your choice wins.` : '';
  };
  const units = editLines([
    { key: 'units.distance', id: 'units', type: 'choice', value: u.distance, options: [['km', 'km'], ['mi', 'mi']], cover: 'web.settings.general.unit-length',
      help: `Kilometres or miles everywhere: drives, range, speed, efficiency, place radius.${def('distance')}`, save: (v) => pref({ units: { distance: v } }) },
    { key: 'units.temperature', type: 'choice', value: u.temperature, options: [['c', '°C'], ['f', '°F']], cover: 'web.settings.general.unit-temperature',
      help: `Outside, cabin and battery temperatures.${def('temperature')}`, save: (v) => pref({ units: { temperature: v } }) },
    { key: 'units.pressure', type: 'choice', value: u.pressure, options: [['bar', 'bar'], ['psi', 'psi']], cover: 'web.settings.general.unit-pressure',
      help: `The four tyres and the recommended pressure.${def('pressure')}`, save: (v) => pref({ units: { pressure: v } }) },
    { key: 'range', id: 'range', type: 'choice', value: u.range, options: [['rated', 'rated'], ['ideal', 'ideal']], cover: 'web.settings.general.preferred-range',
      help: `Rated follows the official test cycle; ideal is what the car calls typical. Consumption is measured at the pack either way.${def('range')}`, save: (v) => pref({ range: v }) },
  ]);

  const th = getTheme().choice;
  const langEx = h('span');
  const paintLang = () => replace(langEx, `example: ${format(1234.5, 'distance')}, ${formatDate(ctx.api.now(), ctx.tz, { weekday: false, year: true })}`);
  paintLang();
  const browserLang = (navigator.languages && navigator.languages[0]) || navigator.language || 'unknown';
  const shownPack = p.ui_pack || about?.ui_pack?.name?.toLowerCase() || 'console';
  const packs = PACKS.some(([v]) => v === shownPack) ? PACKS : [[shownPack, shownPack], ...PACKS];
  const look = editLines([
    { key: 'theme', id: 'theme', type: 'choice', value: th, options: [['device', 'auto'], ['dark', 'dark'], ['light', 'light']], cover: ['shell.theme', 'web.settings.general.theme', 'shots.theme-note'],
      help: 'Auto follows this device. Saved with your account and set before the page is drawn, so there is no flash. Every page, chart and map has a dark and a light look.',
      save: async (v) => { const before = getTheme().choice; applyTheme(v); try { await pref({ theme: v }); } catch (e) { applyTheme(before); throw e; } } },
    { key: 'ui_pack', id: 'pack', type: 'choice', value: shownPack, options: packs,
      help: `The design of the whole app.${about?.ui_pack?.name ? ` This page is the ${about.ui_pack.name} pack ${about.ui_pack.version || ''}.` : ''} Another pack is used the next time the app loads, where the server offers it.`,
      save: async (v) => { await pref({ ui_pack: v }); if (v !== 'console') toast('Saved. Reload the page to see the other pack.', { kind: 'info' }); } },
    { key: 'interface_size', id: 'size', type: 'number', value: getInterface().size, unit: '%', cls: 'ke--short', attrs: { min: '75', max: '200' },
      help: 'Text, tables, charts and maps bigger or smaller together, 75 to 200. Browser zoom works on top.',
      parse: (t) => { const n = Math.round(Number(t)); return n >= 75 && n <= 200 ? { value: n } : { error: 'a whole number from 75 to 200' }; },
      save: async (v) => { const before = getInterface(); applyInterface({ size: v }); try { await pref({ interface_size: v }); } catch (e) { applyInterface(before); throw e; } } },
    { key: 'follow_screen', type: 'bool', value: getInterface().follow,
      help: 'On: a very large screen (a native 4K monitor) gets its own step on top, so text keeps its size there.',
      save: async (v) => { const before = getInterface(); applyInterface({ follow: v }); try { await pref({ follow_screen: v }); } catch (e) { applyInterface(before); throw e; } } },
    { key: 'language', id: 'language', type: 'choice', value: p.language === 'en' ? 'en-US' : (p.language || 'auto'), options: LOCALES,
      cover: ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation'],
      help: [`The text is English for now; this sets how numbers and dates are written. Auto follows the language this browser asks for (${browserLang}), and the page tells the browser which language it is in. `, langEx],
      save: (v) => pref({ language: v }) },
  ]);
  ctx.onCleanup(onPrefs((pp) => {
    const n = getUnits();
    units.line('units.distance').set(n.distance); units.line('units.temperature').set(n.temperature);
    units.line('units.pressure').set(n.pressure); units.line('range').set(n.range);
    look.line('theme').set(pp?.theme || getTheme().choice);
    paintExample(); paintLang();
  }));
  ctx.onCleanup(onChange(() => { look.line('interface_size').set(getInterface().size); }));
  ctx.onCleanup(onLocaleChange(() => { paintExample(); paintLang(); }));

  /* ---------------------------------------------------------------- the server's defaults */
  const serverSlot = h('div');
  const patchServer = async (body) => {
    try {
      server = await ctx.api.patch('settings', { version: server.version, ...body });
    } catch (e) {
      if (e.status === 409) {
        server = await ctx.api.get('settings').catch(() => server);
        if (ctx.alive()) paintServer();
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
  function paintServer() {
    if (!server) { replace(serverSlot, section({ title: 'server defaults', cover: 'web.settings.general.divider' }, note('The server settings could not be read. Your own choices above still work.'))); return; }
    const ro = !admin;
    const g = server.geocode || {}, t = server.tiles || {};
    const off = maps?.tiles?.offline_data_present ?? maps?.geocoding?.offline_data_present;
    const presets = maps?.tiles?.presets || [];
    const preset = presets.find((p) => p.url === t.online_url) || null;
    const lines = editLines([
      { key: 'default.units.distance', type: 'choice', value: server.units?.distance, options: [['km', 'km'], ['mi', 'mi']], readOnly: ro,
        help: 'What an account sees until it picks its own. A fresh server starts from the unit set on the car\'s own screen.', save: (v) => patchServer({ units: { distance: v } }) },
      { key: 'default.units.temperature', type: 'choice', value: server.units?.temperature, options: [['c', '°C'], ['f', '°F']], readOnly: ro, save: (v) => patchServer({ units: { temperature: v } }) },
      { key: 'default.units.pressure', type: 'choice', value: server.units?.pressure, options: [['bar', 'bar'], ['psi', 'psi']], readOnly: ro, save: (v) => patchServer({ units: { pressure: v } }) },
      { key: 'default.range', type: 'choice', value: server.range, options: [['rated', 'rated'], ['ideal', 'ideal']], readOnly: ro, save: (v) => patchServer({ range: v }) },
      { key: 'default.time_zone', id: 'default-tz', type: 'text', value: server.default_time_zone || 'UTC', readOnly: ro, attrs: { list: 'sd-zones' },
        parse: (t) => (isValidZone(t.trim()) ? { value: t.trim() } : { error: 'not a time zone this browser knows: pick one from the list, such as America/Chicago' }),
        help: ['The zone a new car starts in. Each car keeps its own, changed in ', link(ctx.link('settings-cars'), 'settings/cars'), '; the page header\'s clock names the zone shown.'],
        after: BROWSER_ZONE && BROWSER_ZONE !== server.default_time_zone && !ro ? h('button', { class: 'btn', type: 'button', onclick: async () => {
          try { await patchServer({ default_time_zone: BROWSER_ZONE }); toast(`New cars start in ${BROWSER_ZONE}.`); paintServer(); } catch (e) { toast(e.message || 'not saved', { kind: 'error' }); }
        } }, `use ${BROWSER_ZONE}`) : null,
        save: (v) => patchServer({ default_time_zone: v }) },
    ]);
    const addr = editLines([
      { key: 'geocode.mode', type: 'choice', value: g.mode, options: [['offline', 'offline'], ['online', 'online']], readOnly: ro,
        cover: ['web.settings.general.address-lookup', 'docs.env.tesla-api.nominatim-proxy'],
        help: ['Offline uses the place names bundled with Sentry Mate: nothing leaves the server. Online asks the service you name (your own address server, or a proxy in front of one) with rounded positions, and keeps a cache. ', geoLine()],
        save: (v) => patchServer({ geocode: { mode: v } }) },
      { key: 'geocode.url', type: 'text', value: g.url || '', readOnly: ro, attrs: { placeholder: 'https://', inputmode: 'url' }, parse: https,
        help: 'Only used while lookup is online. The service never gets a VIN or a car name.', save: (v) => patchServer({ geocode: { url: v } }) },
      { key: 'geocode.rounding_m', type: 'number', value: g.rounding_m, unit: 'm', cls: 'ke--short', readOnly: ro, attrs: { min: '0', max: '1000' },
        parse: (x) => { const n = Number(x); return x.trim() !== '' && Number.isInteger(n) && n >= 0 && n <= 1000 ? { value: n } : { error: 'a whole number of metres from 0 to 1000' }; },
        help: 'Positions are rounded to this before an online lookup.', save: (v) => patchServer({ geocode: { rounding_m: v } }) },
      { key: 'geocode.language', type: 'text', value: g.language || 'en', cls: 'ke--short', readOnly: ro, attrs: { maxlength: '10' },
        cover: ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid'],
        parse: (x) => (LANG_RE.test(x.trim()) ? { value: x.trim() } : { error: 'not a language code: two or three letters such as en or de, with a region if you like: pt-BR' }),
        help: 'Street and place names in this language where the data has it, else in the local one. A change looks every stored address up again; a code the server refuses is said here.',
        save: (v) => patchServer({ geocode: { language: v } }) },
      { key: 'tiles.mode', type: 'choice', value: t.mode, options: [['offline', 'offline'], ['online', 'online']], readOnly: ro, cover: 'web.shell.map-tiles',
        help: [t.mode === 'online' ? `Maps load pictures from the online source; its credit shows on every map: ${t.attribution || '(none set)'}. ` : off ? 'Offline tiles are installed: maps need nothing from outside. ' : 'No offline tiles are installed: maps draw routes and places on a plain grey ground, with a note. ',
          'An online source is asked by each viewer\'s browser, so it sees the map area and this server\'s address (as Referer: OpenStreetMap refuses tiles without one), never a page or a car.'],
        save: (v) => patchServer({ tiles: { mode: v } }) },
      ...(presets.length ? [{ key: 'tiles.source', type: 'choice', value: preset ? preset.id : 'custom', readOnly: ro, options: [...presets.map((p) => [p.id, p.name]), ['custom', 'custom']],
        help: [preset ? `${preset.name}: free, no key; its terms (${preset.terms.replace(/^https:\/\//, '')}) ask for light use and the credit on every map. ` : 'Your own address below. ',
          'A source fills in the address, credit and zoom below.'],
        save: async (v) => {
          const p = presets.find((x) => x.id === v);
          if (!p) return;
          await patchServer({ tiles: { online_url: p.url, attribution: p.attribution, max_zoom: p.max_zoom } });
          setTimeout(() => { if (ctx.alive()) paintServer(); }, 0); // the address, credit and zoom lines show the source's
        } }] : []),
      { key: 'tiles.online_url', type: 'text', value: t.online_url || '', readOnly: ro, attrs: { placeholder: 'https://.../{z}/{x}/{y}.png', inputmode: 'url' },
        parse: (x) => (x.trim() === '' ? { value: null } : /^https:\/\/\S*\{z\}\S*\{x\}\S*\{y\}/.test(x.trim()) ? { value: x.trim() } : { error: 'an https:// address with {z}, {x} and {y} in it, or empty' }),
        save: (v) => patchServer({ tiles: { online_url: v } }) },
      { key: 'tiles.attribution', type: 'text', value: t.attribution || '', readOnly: ro, keepEmpty: true, help: 'Shown in the corner of every map, as the source asks.', save: (v) => patchServer({ tiles: { attribution: v ?? '' } }) },
      { key: 'outside_links.maps', type: 'bool', value: !!server.outside_links?.maps, readOnly: ro, help: 'A link from a place to an outside map, opened only when someone clicks it.', save: (v) => patchServer({ outside_links: { maps: v } }) },
      { key: 'outside_links.release_notes', type: 'bool', value: !!server.outside_links?.release_notes, readOnly: ro, save: (v) => patchServer({ outside_links: { release_notes: v } }) },
    ]);
    replace(serverSlot,
      section({ title: 'server defaults', cover: 'web.settings.general.divider', side: h('span', { class: 'muted' }, ro ? 'set by an admin; read only' : 'for everyone; a person\'s own choice wins') }, lines.el,
        h('datalist', { id: 'sd-zones' }, ZONES.map((x) => h('option', { value: x })))),
      section({ title: 'addresses and maps', side: h('span', { class: 'muted' }, 'private by default') }, addr.el));
  }
  paintServer();

  paint(ctx, head(ctx),
    h('p', { class: 'note', cover: 'web.settings.general.autosave' }, 'Each line saves on its own: Enter saves, Esc undoes, a key saves when pressed. There is no save button for the page. Your choices follow you to every browser you sign in on.'),
    section({ title: 'units', side: h('span', { class: 'muted' }, 'yours') }, units.el, example),
    section({ title: 'look and language', side: h('span', { class: 'muted' }, 'yours') }, look.el),
    serverSlot);
  ctx.ready();
}
