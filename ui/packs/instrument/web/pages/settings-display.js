// Settings: Display and units (/settings/display): how numbers, words and maps look to each person. Their own
// choices (PATCH /api/v1/me/preferences) save at once and apply at once, and Interface size on this device is
// kept in this browser; below them, the server's default units, which an admin changes (PATCH /api/v1/settings).
// Where maps and addresses come from is in Settings: Maps and addresses; the release check in Settings: Server
// and access; the time zone for new cars in Settings: Cars. Old links to those sections lead there (MOVED).

import { h, replace } from '../shared/dom.js';
import { getUnits, format } from '../shared/units.js';
import { isSupportedLocale, onLocaleChange } from '../shared/i18n.js';
import { formatDate } from '../shared/time.js';
import { buildUrl, movedUrl, navigate } from '../shared/state.js';
import { soldParts, soldOptions } from '../core/carswitch.js';
import { packChoices, packLabel } from '../shared/packs.js';
import { icon } from '../core/icons.js';
import { pageHead, panel } from '../core/page.js';
import { getPrefs, savePrefs, isAdmin, onPrefs } from '../core/prefs.js';
import { getInterface, onInterface } from '../core/display.js';
import { sizePicker, followSwitch, sizeNote, devicePicker, deviceNote, themeButtons } from '../core/account.js';
import { flash } from '../core/flash.js';
import { MAP_COLORS_CHOICES, MAP_COLORS_TEXT, mapStyles, pickStyle } from '../shared/maptiles.js';

export const id = 'settings-display';
export const css = true;

// used only when the server's list of packs can't be read
const PACKS = [['instrument', 'Instrument']];
const LOCALES = [['auto', "This browser's language"], ['en-US', 'English (United States)'], ['en-GB', 'English (United Kingdom)'], ['de-DE', 'German number and date style'], ['fr-FR', 'French number and date style'], ['nl-NL', 'Dutch number and date style']].filter(([v]) => v === 'auto' || isSupportedLocale(v));


const UNIT_WORDS = { km: 'km', mi: 'miles', c: '°C', f: '°F', bar: 'bar', psi: 'psi', rated: 'rated', ideal: 'ideal' };

// a row: the label and its explanation on the left, the control on the right
const setting = (title, text, control, cover = null) => h('div', { class: 'setting', cover }, h('div', null, h('h3', null, title), h('p', null, text)), h('div', { class: 'setting-control' }, control));

// buttons for one choice; onPick(value) saves it and returns false when the save failed, which puts
// the stored value back. seg.set(value) shows another value (a change elsewhere). readOnly: the
// buttons only show the value (disabled, so neither a click nor the keyboard changes them).
function choice(name, options, current, onPick, { readOnly = false } = {}) {
  let shown = String(current);
  const mark = (v) => { for (const b of seg.children) { const on = b.dataset.v === String(v); b.setAttribute('aria-pressed', String(on)); b.setAttribute('aria-checked', String(on)); } shown = String(v); };
  const seg = h('div', { class: 'seg', role: 'radiogroup', 'aria-label': name },
    options.map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', role: 'radio', 'aria-checked': String(String(v) === String(current)), 'aria-pressed': String(String(v) === String(current)), dataset: { v: String(v) }, disabled: readOnly,
      onclick: readOnly ? null : async () => {
        const before = shown;
        if (String(v) === before) return;
        mark(v);
        if ((await onPick(v)) === false && shown === String(v)) mark(before);
      } }, l)));
  if (readOnly) seg.setAttribute('aria-disabled', 'true');
  seg.set = mark;
  return seg;
}

async function savePref(patch, what) {
  try { await savePrefs(patch); flash(`${what} saved.`); return true; } catch (e) { flash(`Could not save ${what.toLowerCase()}: ${e.message}`, { kind: 'error' }); return false; }
}

// The first reads; prefetch() warms them while the session is checked, and when a link here is under the pointer
// (not the map settings: a page with a map takes a warmed answer as fresh, and this page is where they change)
const firstReads = (ctx, { maps = true } = {}) => [
  ctx.api.get('about', null, { maxAgeMs: 600000 }),
  ctx.api.get('settings'),
  maps ? ctx.api.get('maps/config') : null,
  packChoices(ctx.api, { fallback: PACKS, current: (ctx.prefs || getPrefs())?.ui_pack || 'instrument' }),
  ctx.api.list('cars', { sort: 'custom', limit: 500 }).all({ max: 5000 }),
];
export function prefetch(ctx) { return Promise.all(firstReads(ctx, { maps: false }).map((p) => p?.catch(() => null))); }

export async function mount(ctx) {
  // an old link to a section that moved (/settings/display#map-tiles): open it where it is now
  const moved = movedUrl(location.href);
  if (moved) { navigate(moved, { replace: true }); return; }
  const admin = isAdmin();
  let about = null, server = null, maps = null, packList = null, cars = null;
  [about, server, maps, packList, cars] = await Promise.all(firstReads(ctx).map((p) => p.catch(() => null)));
  if (!ctx.alive()) return;
  const p = getPrefs() || {};
  const u = getUnits();

  /* ---------- your units */
  const segDist = choice('Distance', [['km', 'km and km/h'], ['mi', 'miles and mph']], u.distance, (v) => savePref({ units: { distance: v } }, 'Distance unit'));
  const segTemp = choice('Temperature', [['c', '°C'], ['f', '°F']], u.temperature, (v) => savePref({ units: { temperature: v } }, 'Temperature unit'));
  const segPres = choice('Pressure', [['bar', 'bar'], ['psi', 'psi']], u.pressure, (v) => savePref({ units: { pressure: v } }, 'Pressure unit'));
  const segRange = choice('Range', [['rated', 'Rated'], ['ideal', 'Ideal']], u.range, (v) => savePref({ range: v }, 'Range'));
  // the server default under each choice, so a person sees what they override
  const defNote = () => h('p', { class: 'setting-note' });
  const defs = { distance: defNote(), temperature: defNote(), pressure: defNote(), range: defNote() };
  const paintDefaults = () => {
    for (const k of Object.keys(defs)) {
      const d = k === 'range' ? server?.range : server?.units?.[k];
      replace(defs[k], d ? ['Server default: ', h('b', null, UNIT_WORDS[d] || d), '. Your choice wins.'] : 'Server default not known.');
    }
  };
  const example = h('p', { class: 'setting-note example-line' });
  const paintExample = () => replace(example, icon('info', 'ico--sm'), ' Shown now as: ',
    h('b', null, format(100, 'distance')), ' · ', h('b', null, format(100, 'speed')), ' · ', h('b', null, format(20, 'temperature')), ' · ', h('b', null, format(290, 'pressure')),
    '. The server always sends metric values (100 km, 20 °C, 2.90 bar); only how you see them changes.');
  paintDefaults(); paintExample();
  ctx.onCleanup(onPrefs(() => {
    const n = getUnits();
    segDist.set(n.distance); segTemp.set(n.temperature); segPres.set(n.pressure); segRange.set(n.range);
    paintExample();
  }));

  const units = panel({ title: 'Units and range', icon: 'odo', side: h('span', { class: 'faint' }, 'Yours; saved at once') },
    setting('Distance and speed', 'Kilometres or miles, everywhere: drives, range, speed, efficiency, place radius.', [segDist, defs.distance], 'web.settings.general.unit-length'),
    setting('Temperature', 'Outside, cabin and battery temperatures.', [segTemp, defs.temperature], 'web.settings.general.unit-temperature'),
    setting('Tyre pressure', 'All four tyres and the recommended pressure.', [segPres, defs.pressure], 'web.settings.general.unit-pressure'),
    setting('Range', 'Rated range follows the official test cycle; ideal range is what the car calls typical range. Consumption is measured from the pack either way, so this only changes which range figure you see.',
      [segRange, defs.range], 'web.settings.general.preferred-range'),
    example);

  /* ---------- your look */
  const dev = devicePicker('set-device');
  const devNoteEl = h('p', { class: 'setting-note', id: 'set-device-note' }, deviceNote());
  const size = sizePicker('set-size');
  const follow = followSwitch('set-follow');
  const pctIn = h('input', { class: 'input input--sm', id: 'set-size-pct', type: 'number', min: '75', max: '200', step: '1', value: String(getInterface().size), 'aria-label': 'Interface size in percent' });
  const sizeErr = h('p', { class: 'form-error', hidden: true });
  pctIn.addEventListener('change', async () => {
    const v = Math.round(Number(pctIn.value));
    if (!(v >= 75 && v <= 200)) { sizeErr.hidden = false; replace(sizeErr, icon('warn'), 'A size from 75 to 200 %.'); return; }
    sizeErr.hidden = true;
    await savePref({ interface_size: v }, 'Interface size');
  });
  const noteEl = h('p', { class: 'setting-note' }, sizeNote());
  ctx.onCleanup(onInterface((i) => { dev.sync(i); devNoteEl.textContent = deviceNote(); size.sync(i); follow.sync(i); pctIn.value = String(i.size); noteEl.textContent = sizeNote(); }));
  const theme = themeButtons();
  const segMapColors = choice('Map colours', MAP_COLORS_CHOICES, p.map_colors || 'dimmed', (v) => savePref({ map_colors: v }, 'Map colours'));
  // the person's map style: Map, Satellite, ... among the maps an admin lets people switch to
  const styles = mapStyles(maps);
  const segMapStyle = styles.length > 1 ? choice('Map style', styles.map((x) => [x.id, x.short || x.name]), pickStyle(maps, p)?.id, (v) => savePref({ map_style: v }, 'Map style')) : null;
  const mapStyleNote = h('p', { class: 'setting-note' }, styles.length > 1 ? 'The switch on a map page changes it too.'
    : maps?.tiles?.mode === 'online' ? 'This server offers one map, so there is nothing to pick.' : 'Maps come from this server’s offline map, so there is one map to show.');
  ctx.onCleanup(onPrefs((pp) => {
    theme.sync(pp);
    segMapColors.set(pp?.map_colors || 'dimmed');
    segMapStyle?.set(pickStyle(maps, pp)?.id);
    lang.value = pp?.language === 'en' ? 'en-US' : (pp?.language || 'auto');
    packSel.value = pp?.ui_pack || 'instrument';
    paintMainCar(pp?.main_car ?? null);
    paintSwitch();
    paintLangExample();
  }));
  ctx.onCleanup(onLocaleChange(() => { paintExample(); paintLangExample(); }));
  const browserLang = (navigator.languages && navigator.languages[0]) || navigator.language || 'unknown';
  const lang = h('select', { class: 'select', 'aria-label': 'Language and number style', onchange: (e) => savePref({ language: e.target.value }, 'Language') },
    LOCALES.map(([v, l]) => h('option', { value: v, selected: (p.language || 'auto') === v || (v === 'en-US' && p.language === 'en') }, v === 'auto' ? `${l} (${browserLang})` : l)));
  // every pack the server offers (GET /ui-packs); the one running now is the served pack
  const packHere = about?.ui_pack?.id || 'instrument';
  const packSel = h('select', { class: 'select', id: 'set-pack', 'aria-label': 'UI pack', onchange: async (e) => { if (await savePref({ ui_pack: e.target.value }, 'UI pack')) paintSwitch(); } },
    packList.items.map((k) => h('option', { value: k.id, selected: (p.ui_pack || 'instrument') === k.id }, packLabel(k))));
  const switchBtn = h('button', { class: 'btn btn--sm', type: 'button', hidden: true, onclick: () => location.reload() }, 'Switch now');
  const switchNote = h('p', { class: 'setting-note', hidden: true }, 'The new pack is used the next time the app loads.');
  function paintSwitch() { const other = packSel.value !== packHere; switchBtn.hidden = !other; switchNote.hidden = !other; }
  paintSwitch();
  // Main car: the car pages without one in the address show (Fleet's sidebar, Travel's days, links); none
  // means the car looked at last. Sold cars wait in their own group unless one is the Main car.
  const mainCarSel = h('select', { class: 'select', id: 'set-main-car', 'aria-label': 'Main car', disabled: !cars,
    onchange: (e) => savePref({ main_car: e.target.value || null }, 'Main car') });
  function paintMainCar(chosen) {
    const opt = (c) => h('option', { value: c.id, selected: c.id === chosen }, c.name || c.id);
    const { main, sold } = soldParts(cars || [], chosen ? [chosen] : []);
    const missing = chosen && !(cars || []).some((c) => c.id === chosen);
    replace(mainCarSel,
      h('option', { value: '', selected: !chosen }, 'None (the car you looked at last)'),
      main.map(opt), soldOptions(sold, opt),
      missing ? h('option', { value: chosen, selected: true }, chosen) : null);
  }
  paintMainCar(p.main_car ?? null);
  // written with the app's own formatters, so it shows what the saved language really does
  const langExample = h('p', { class: 'setting-note' });
  const paintLangExample = () => replace(langExample, 'Example: ', h('b', null, format(1234.5, 'distance')), ', ', h('b', null, formatDate(ctx.api.now(), ctx.tz, { weekday: false, year: true })), '.');
  paintLangExample();
  const look = panel({ title: 'Look and language', icon: 'monitor', side: h('span', { class: 'faint' }, 'Yours; follows you to every screen') },
    setting('Theme', 'Light, dark, or follow this device. It is saved with your account, so every browser you sign in on uses it, and it is set before the page is drawn, so there is no flash.',
      [theme.el, h('p', { class: 'setting-note' }, 'Every page, chart and map has both a light and a dark look; screenshots in the help show either.')], ['shell.theme', 'web.settings.general.theme', 'shots.theme-note']),
    setting('Map colours', `${MAP_COLORS_TEXT} It does not apply to satellite pictures: photos always show as taken, never dimmed.`, segMapColors),
    setting('Map style', 'The map you see on map pages, among the ones an admin offers on this server.', [segMapStyle, mapStyleNote], null),
    setting('Interface size on this device', 'Only this browser on this device: kept here, not with your account, so a laptop with large display scaling and a desktop monitor can each have their own size. Automatic uses your account\'s Interface size below. A fixed size draws everything at that percent of the standard size, with nothing added for large screens.',
      [dev.el, devNoteEl]),
    setting('Interface size', 'Your account\'s size, on every device left at Automatic. Makes text, controls, tables, charts and maps bigger or smaller together. With Follow screen size on, a very large screen (a native 4K monitor) adds its own step on top. Browser zoom works on top of both.',
      [size.el, h('div', { class: 'row' }, h('label', { class: 'faint', for: 'set-size-pct' }, 'Any size:'), pctIn, h('span', { class: 'faint' }, '%')), sizeErr,
        h('div', { class: 'switch-row switch-row--inline' }, follow.el, h('label', { for: 'set-follow' }, 'Follow screen size')), noteEl]),
    setting('Language', 'The text is in English for now. This sets how numbers and dates are written. "This browser\'s language" follows the language your browser asks for; the app tells the browser which language the page is in.',
      [lang, langExample],
      ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation']),
    setting('UI pack', `The design of the whole app. ${about?.ui_pack?.name ? `This page is the ${about.ui_pack.name} pack ${about.ui_pack.version || ''}.` : ''} Your choice is used the next time the app loads where the server offers that pack.`,
      [h('div', { class: 'row pack-row' }, packSel, switchBtn), switchNote]),
    setting('Main car', 'The car shown on pages without one in their address: the sidebar on All cars, the days on Travel, and links to car pages. With none, the car you looked at last is shown.',
      [mainCarSel, cars ? null : h('p', { class: 'setting-note' }, 'The cars could not be read.')]));

  /* ---------- the server's defaults, which a person's own choice above wins over */
  const serverPanel = h('div', { class: 'settings-server' });
  function paintServer() {
    if (!server) { replace(serverPanel, panel({ title: 'Server defaults', icon: 'server' }, h('p', { class: 'note' }, 'The server settings could not be loaded. Your own choices above still work.'))); return; }
    const ro = !admin;
    const patch = async (body, what) => {
      try {
        server = await ctx.api.patch('settings', { version: server.version, ...body });
        flash(`${what} saved.`);
        if (!ctx.alive()) return true;
        paintServer(); paintDefaults();
        return true;
      } catch (e) {
        if (e?.name === 'AbortError') return false;
        if (e.status === 409) { server = await ctx.api.get('settings').catch(() => server); if (ctx.alive()) { paintServer(); paintDefaults(); } flash('Someone else changed the settings; this page shows theirs now.', { kind: 'info' }); return false; }
        flash(`Could not save: ${e.message}`, { kind: 'error' });
        return false;
      }
    };
    // a server choice: read-only (buttons disabled) for anyone but an admin
    const sChoice = (name, options, current, onPick) => choice(name, options, current, onPick, { readOnly: ro });
    replace(serverPanel,
      h('div', { class: 'settings-divider', role: 'separator', cover: 'web.settings.general.divider' }, h('span', null, 'Server defaults'), h('span', { class: 'faint' }, admin ? 'For everyone; you change them as an admin. A person’s own choice above wins over these.' : 'Set by an admin; shown here to read. Your own choices above win over these.')),
      panel({ title: 'Defaults for everyone', icon: 'server', side: ro ? h('span', { class: 'faint' }, 'Read only') : null },
        setting('Units', 'What an account sees until it picks its own units. Changing these updates everyone who has not chosen.',
          h('div', { class: 'row' },
            sChoice('Default distance', [['km', 'km'], ['mi', 'mi']], server.units?.distance, (v) => patch({ units: { distance: v } }, 'Default distance unit')),
            sChoice('Default temperature', [['c', '°C'], ['f', '°F']], server.units?.temperature, (v) => patch({ units: { temperature: v } }, 'Default temperature unit')),
            sChoice('Default pressure', [['bar', 'bar'], ['psi', 'psi']], server.units?.pressure, (v) => patch({ units: { pressure: v } }, 'Default pressure unit')))),
        setting('Range', 'Rated or ideal, for accounts that have not chosen.', sChoice('Default range', [['rated', 'Rated'], ['ideal', 'Ideal']], server.range, (v) => patch({ range: v }, 'Default range'))),
        h('p', { class: 'setting-note' }, 'A fresh server starts from the distance unit set on the car’s own screen, once its CAN Bridge has sent it.')));
  }
  paintServer();

  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'settings-stack' },
      h('p', { class: 'note saving-note', cover: 'web.settings.general.autosave' }, icon('check', 'ico--sm'), ' Every change here is saved at once; there is no Save button. A short note confirms it, and your choices follow you to every browser you sign in on.'),
      h('div', { class: 'settings-pair' }, units, look), serverPanel));
  ctx.ready();
}
