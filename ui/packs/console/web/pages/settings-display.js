// settings/display: how numbers, words and maps look to you, as 'key = value' lines.
// Yours (PATCH /me/preferences: saved and applied at once, they follow you to every browser): units, range,
// theme, map colours, map style, UI pack, interface size, follow screen size, language, main car; the size on
// this device is kept in this browser. Below them the server's default units and range (PATCH /settings with
// its version; admins change them, others read). Where maps and addresses come from is in settings/maps, the
// release check in settings/server, the time zone for new cars in settings/cars; old links lead there (MOVED).

import { h, replace } from '../shared/dom.js';
import { getUnits, format } from '../shared/units.js';
import { isSupportedLocale, onLocaleChange } from '../shared/i18n.js';
import { formatDate } from '../shared/time.js';
import { head, paint, section, note, link } from '../core/ui.js';
import { editLines } from '../core/kvedit.js';
import { getPrefs, savePrefs, onPrefs } from '../core/prefs.js';
import { getInterface, applyInterface, applyDeviceSize, onInterface, getTheme, applyTheme, onChange } from '../core/display.js';
import { deviceSizeOptions, deviceSizeNote } from '../shared/devicesize.js';
import { tag } from '../core/fmt.js';
import { carOptions } from '../core/carpick.js';
import * as state from '../shared/state.js';
import { toast } from '../core/toast.js';
import { packChoices } from '../shared/packs.js';
import { MAP_COLORS_TEXT, mapStyles, pickStyle } from '../shared/maptiles.js';

export const id = 'settings-display';
export const title = () => 'Settings: display';

const LOCALES = [['auto', 'auto'], ['en-US', 'en-US'], ['en-GB', 'en-GB'], ['de-DE', 'de-DE'], ['fr-FR', 'fr-FR'], ['nl-NL', 'nl-NL']].filter(([v]) => v === 'auto' || isSupportedLocale(v));
// used only when the server's list of packs can't be read
const PACKS = [['console', 'console'], ['instrument', 'instrument'], ['logbook', 'logbook'], ['horizon', 'horizon'], ['pebble', 'pebble']];

/**
 * main_car: the car shown where an address names none, as a select: none (the car looked at last) or one of the
 * cars this account sees; a sold car is listed (under "sold cars") only while it is the one chosen. Saved at once.
 * Returns a line for editLines ({ el, spec, set }).
 */
function mainCarLine(cars, value, save) {
  const st = h('span', { class: 'ke-st', 'aria-live': 'polite' });
  const err = h('p', { class: 'ke-err', role: 'alert', hidden: true });
  const known = (v) => (v && cars.some((c) => c.id === v) ? v : '');
  let now = known(value);
  const sel = h('select', { class: 'in', id: 'main-car', 'aria-label': 'main_car', 'data-focus': '' });
  const fill = () => replace(sel, h('option', { value: '', selected: now === '' }, 'None (the car you looked at last)'),
    carOptions(cars.filter((c) => !state.isSold(c) || c.id === now), (c) => h('option', { value: c.id, selected: c.id === now }, c.name || c.id)));
  fill();
  sel.addEventListener('change', async () => {
    const before = now, v = sel.value;
    now = v; err.hidden = true; replace(st, h('span', { class: 'muted' }, 'saving'));
    try { await save(v || null); replace(st, tag('saved', 'ok')); fill(); } catch (e) { now = before; fill(); replace(st, tag('bad', 'bad')); err.hidden = false; replace(err, e?.message || 'not saved'); }
  });
  const el = h('div', { class: 'ke', id: 'main-car-line', dataset: { row: '' } },
    h('label', { class: 'ke-k', for: 'main-car' }, 'main_car'), h('span', { class: 'ke-eq', 'aria-hidden': 'true' }, '='),
    h('div', { class: 'ke-v' }, sel), h('span', { class: 'ke-side' }, st),
    h('p', { class: 'ke-help' }, 'The car pages open on when the address names no car, on every device. None: the car you looked at last.'), err);
  return { el, spec: { key: 'main_car' }, set: (v) => { const k = known(v); if (k !== now) { now = k; fill(); } } };
}

export async function mount(ctx) {
  // an old link to a section that moved (/settings/display#map-tiles): open it where it is now
  const moved = state.movedUrl(location.href);
  if (moved) { state.navigate(moved, { replace: true }); return; }
  const admin = ctx.admin;
  let [about, server, maps, packList, carList] = await Promise.all([
    ctx.api.get('about', null, { maxAgeMs: 600000 }).catch(() => null),
    ctx.api.get('settings').catch(() => null),
    ctx.api.get('maps/config').catch(() => null),
    packChoices(ctx.api, { fallback: PACKS, current: getPrefs()?.ui_pack || null }),
    ctx.api.get('cars', { sort: 'name', limit: 200 }, { maxAgeMs: 5000 }).then((r) => r.items || []).catch(() => []),
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
  // every pack the server offers (GET /ui-packs), as keys; each one's short description under them
  const packs = packList.items.map((k) => [k.id, k.id]);
  if (!packs.some(([v]) => v === shownPack)) packs.unshift([shownPack, shownPack]);
  const packHere = about?.ui_pack?.id || about?.ui_pack?.name?.toLowerCase() || 'console';
  const switchBtn = h('button', { class: 'btn', type: 'button', hidden: shownPack === packHere, onclick: () => location.reload() }, 'switch now');
  const packInfo = packList.items.filter((k) => k.blurb).map((k) => [h('br'), h('span', { class: 'muted' }, `${k.id}: ${k.blurb}`)]);
  const deviceNoteEl = h('span', { class: 'muted' });
  const paintDeviceNote = () => { deviceNoteEl.textContent = deviceSizeNote(getInterface().device, { account: getInterface().size }); };
  paintDeviceNote();
  const styles = mapStyles(maps);
  const look = editLines([
    { key: 'theme', id: 'theme', type: 'choice', value: th, options: [['device', 'auto'], ['dark', 'dark'], ['light', 'light']], cover: ['shell.theme', 'web.settings.general.theme', 'shots.theme-note'],
      help: 'Auto follows this device. Saved with your account and set before the page is drawn, so there is no flash. Every page, chart and map has a dark and a light look.',
      save: async (v) => { const before = getTheme().choice; applyTheme(v); try { await pref({ theme: v }); } catch (e) { applyTheme(before); throw e; } } },
    { key: 'map_colors', id: 'map-colors', type: 'choice', value: p.map_colors || 'dimmed', options: [['dimmed', 'dimmed'], ['original', 'original']],
      help: `${MAP_COLORS_TEXT} It doesn't apply to satellite pictures: those are always shown as taken.`, save: (v) => pref({ map_colors: v }) },
    // the map you see, among the ones an admin lets people switch to (settings/maps)
    styles.length > 1
      ? { key: 'map_style', id: 'map-style', type: 'choice', value: pickStyle(maps, p)?.id, options: styles.map((x) => [x.id, x.id]),
        help: ['The map on map pages, among the ones an admin offers here (', link(ctx.link('settings-maps'), 'settings/maps'), '). The switch on a map page changes it too.'],
        save: (v) => pref({ map_style: v }) }
      : { key: 'map_style', id: 'map-style', type: 'show', value: '', show: () => pickStyle(maps, p)?.id || (maps?.tiles?.mode === 'online' ? 'online' : 'offline'),
        help: maps?.tiles?.mode === 'online' ? 'This server offers one map, so there is nothing to pick.' : 'Maps come from this server\'s offline map, so there is one map to show.' },
    { key: 'ui_pack', id: 'pack', type: 'choice', value: shownPack, options: packs,
      help: [`The design of the whole app.${about?.ui_pack?.name ? ` This page is the ${about.ui_pack.name} pack ${about.ui_pack.version || ''}.` : ''} Another pack is used the next time the app loads, where the server offers it. `, switchBtn, packInfo],
      save: async (v) => { await pref({ ui_pack: v }); switchBtn.hidden = v === packHere; if (v !== packHere) toast('Saved. Used the next time the app loads; switch now reloads.', { kind: 'info' }); } },
    // kept in this browser only (shared/devicesize.js), never with the account
    { key: 'size_on_this_device', id: 'device-size', type: 'choice', value: getInterface().device ? String(getInterface().device) : 'auto',
      options: deviceSizeOptions().map((o) => [o.value, o.value === 'auto' ? 'auto' : o.label]), label: 'Interface size on this device',
      help: ['This device only: kept in this browser, not with your account, so each computer can have its own size. auto uses interface_size below; a fixed size draws everything at that percent of the standard size, with nothing added for large screens. ', deviceNoteEl],
      save: async (v) => { applyDeviceSize(v); } },
    { key: 'interface_size', id: 'size', type: 'number', value: getInterface().size, unit: '%', cls: 'ke--short', attrs: { min: '75', max: '200' },
      help: 'Your account\'s size, on every device left at auto. Text, tables, charts and maps bigger or smaller together, 75 to 200. Browser zoom works on top.',
      parse: (t) => { const n = Math.round(Number(t)); return n >= 75 && n <= 200 ? { value: n } : { error: 'a whole number from 75 to 200' }; },
      save: async (v) => { const before = getInterface(); applyInterface({ size: v }); try { await pref({ interface_size: v }); } catch (e) { applyInterface(before); throw e; } } },
    { key: 'follow_screen', type: 'bool', value: getInterface().follow,
      help: 'On: a very large screen (a native 4K monitor) gets its own step on top, so text keeps its size there.',
      save: async (v) => { const before = getInterface(); applyInterface({ follow: v }); try { await pref({ follow_screen: v }); } catch (e) { applyInterface(before); throw e; } } },
    { key: 'language', id: 'language', type: 'choice', value: p.language === 'en' ? 'en-US' : (p.language || 'auto'), options: LOCALES,
      cover: ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation'],
      help: [`The text is English for now; this sets how numbers and dates are written. Auto follows the language this browser asks for (${browserLang}), and the page tells the browser which language it is in. `, langEx],
      save: (v) => pref({ language: v }) },
    carList.length ? mainCarLine(carList, p.main_car || null, (v) => pref({ main_car: v })) : null,
  ]);
  ctx.onCleanup(onPrefs((pp) => {
    const n = getUnits();
    units.line('units.distance').set(n.distance); units.line('units.temperature').set(n.temperature);
    units.line('units.pressure').set(n.pressure); units.line('range').set(n.range);
    look.line('theme').set(pp?.theme || getTheme().choice);
    look.line('map_colors').set(pp?.map_colors || 'dimmed');
    if (styles.length > 1) look.line('map_style').set(pickStyle(maps, pp)?.id);
    look.line('main_car')?.set(pp?.main_car || null);
    paintExample(); paintLang();
  }));
  ctx.onCleanup(onChange(() => { look.line('interface_size').set(getInterface().size); }));
  ctx.onCleanup(onInterface((i) => { look.line('size_on_this_device').set(i.device ? String(i.device) : 'auto'); paintDeviceNote(); }));
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
  };
  function paintServer() {
    if (!server) { replace(serverSlot, section({ title: 'server defaults', cover: 'web.settings.general.divider' }, note('The server settings could not be read. Your own choices above still work.'))); return; }
    const ro = !admin;
    const lines = editLines([
      { key: 'default.units.distance', type: 'choice', value: server.units?.distance, options: [['km', 'km'], ['mi', 'mi']], readOnly: ro,
        help: 'What an account sees until it picks its own. A fresh server starts from the unit set on the car\'s own screen.', save: (v) => patchServer({ units: { distance: v } }) },
      { key: 'default.units.temperature', type: 'choice', value: server.units?.temperature, options: [['c', '°C'], ['f', '°F']], readOnly: ro, save: (v) => patchServer({ units: { temperature: v } }) },
      { key: 'default.units.pressure', type: 'choice', value: server.units?.pressure, options: [['bar', 'bar'], ['psi', 'psi']], readOnly: ro, save: (v) => patchServer({ units: { pressure: v } }) },
      { key: 'default.range', type: 'choice', value: server.range, options: [['rated', 'rated'], ['ideal', 'ideal']], readOnly: ro, save: (v) => patchServer({ range: v }) },
    ]);
    replace(serverSlot,
      section({ title: 'server defaults', cover: 'web.settings.general.divider', side: h('span', { class: 'muted' }, ro ? 'set by an admin; read only' : 'for everyone; a person\'s own choice wins') }, lines.el,
        note('Maps and addresses for everyone: ', link(ctx.link('settings-maps'), 'settings/maps'), '. The time zone for new cars: ', link(ctx.link('settings-cars'), 'settings/cars'), '. The release check: ', link(`${ctx.link('settings-server')}#release-check`, 'settings/server'), '.')));
  }
  paintServer();

  paint(ctx, head(ctx),
    h('p', { class: 'note', cover: 'web.settings.general.autosave' }, 'Each line saves on its own: Enter saves, Esc undoes, a key saves when pressed. There is no save button for the page. Your choices follow you to every browser you sign in on.'),
    section({ title: 'units', side: h('span', { class: 'muted' }, 'yours') }, units.el, example),
    section({ title: 'look and language', side: h('span', { class: 'muted' }, 'yours') }, look.el),
    serverSlot);
  ctx.ready();
}
