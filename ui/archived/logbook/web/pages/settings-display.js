// Sentry Mate, "Logbook" UI pack: Settings: Display and units (/settings/display).
//
// Your own choices (GET / PATCH /api/v1/me/preferences): units, rated or ideal range, theme, language,
// interface size and following the screen size. Every change saves at once and applies on every page, with
// an example in the chosen unit or language next to the control.
// The map style picks among the maps an admin lets people switch to (GET /api/v1/maps/config).
// Below the divider, the server's default units and range (GET / PATCH /api/v1/settings, admins only to
// change). Where maps and addresses come from is in Settings: Maps and addresses, the release check in
// Settings: Server and access, the time zone for new cars in Settings: Cars; old links to those sections
// (/settings/display#offline-map) open them where they are now (movedUrl in shared/state.js).

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { getLocale, isSupportedLocale } from '../shared/i18n.js';
import { format } from '../shared/units.js';
import { formatDateTime } from '../shared/time.js';
import { movedUrl, navigate } from '../shared/state.js';
import { pageHead, errorView, pageLink, flag, button } from '../core/ui.js';
import { choice, switchField, carOptions } from '../core/form.js';
import { MAP_COLORS_CHOICES, MAP_COLORS_TEXT, mapStyles, pickStyle } from '../shared/maptiles.js';
import { currentPrefs, onPrefs } from '../core/prefs.js';
import { savePref, currentAbout } from '../core/frame.js';
import { SIZES, sizeState, setSize, setDeviceSize, onSize, sizeNote, display } from '../core/display.js';
import { deviceSizeOptions, deviceSizeNote } from '../shared/devicesize.js';
import { packChoices, packLabel } from '../shared/packs.js';

export const id = 'settings-display';
export const css = true;
export const title = () => 'Display and units';

// Locales offered for dates and numbers, besides the browser's own languages. Names come from the browser.
// used only when the server's list of packs can't be read
const PACKS = [['logbook', 'Logbook'], ['instrument', 'Instrument'], ['horizon', 'Horizon'], ['console', 'Console'], ['pebble', 'Pebble']];
const LOCALES = ['en-US', 'en-GB', 'en-CA', 'en-AU', 'de-DE', 'de-CH', 'fr-FR', 'fr-CA', 'nl-NL', 'nb-NO', 'sv-SE', 'da-DK',
  'fi-FI', 'es-ES', 'it-IT', 'pt-PT', 'pl-PL', 'cs-CZ', 'ja-JP', 'ko-KR', 'zh-CN'];

function langName(tag) {
  try {
    const n = new Intl.DisplayNames(['en'], { type: 'language' }).of(tag);
    return n && n !== tag ? n : null;
  } catch { return null; }
}

function setting(name, desc, control, { cover = null, cls = null, extra = null, id = null } = {}) {
  return h('div', { class: ['setting', cls], cover, id },
    h('div', { class: 'setting-text' },
      h('h3', { class: 'setting-name' }, name),
      desc ? h('p', { class: 'setting-desc' }, desc) : null,
      extra),
    h('div', { class: 'setting-control' }, control));
}

const UNIT_WORDS = {
  distance: { km: 'kilometres', mi: 'miles' },
  temperature: { c: '°C', f: '°F' },
  pressure: { bar: 'bar', psi: 'psi' },
  range: { rated: 'rated', ideal: 'ideal' },
};

export async function mount(ctx) {
  // an old link to a section that moved (/settings/display#offline-map): open it where it is now
  const moved = movedUrl(location.href);
  if (moved) { navigate(moved, { replace: true }); return {}; }
  const admin = ctx.me && ctx.me.role === 'admin';
  const [server, maps, packList, carList] = await Promise.all([
    ctx.api.get('settings'),
    ctx.api.get('maps/config').catch(() => null),
    packChoices(ctx.api, { fallback: PACKS, current: (currentPrefs() || {}).ui_pack || null }),
    ctx.api.list('cars', { sort: 'name' }, { limit: 200 }).all({ max: 5000 }).catch((e) => { if (api.isAbort(e)) throw e; return null; }),
  ]);
  let srv = server;
  const p = currentPrefs() || {};
  const save = (patch) => savePref(patch, { quiet: false }).catch(() => {});

  // ------------------------------------------------------------ your choices
  const units = p.units || {};
  const dist = choice({ label: 'Distance', labelHidden: true, name: 'distance', value: units.distance || 'km', options: [['km', 'Kilometres'], ['mi', 'Miles']], onchange: (v) => save({ units: { distance: v } }) });
  const temp = choice({ label: 'Temperature', labelHidden: true, name: 'temperature', value: units.temperature || 'c', options: [['c', 'Celsius (°C)'], ['f', 'Fahrenheit (°F)']], onchange: (v) => save({ units: { temperature: v } }) });
  const press = choice({ label: 'Tyre pressure', labelHidden: true, name: 'pressure', value: units.pressure || 'bar', options: [['bar', 'bar'], ['psi', 'psi']], onchange: (v) => save({ units: { pressure: v } }) });
  const range = choice({ label: 'Range', labelHidden: true, name: 'range', value: p.range || 'rated', options: [['rated', 'Rated'], ['ideal', 'Ideal']], onchange: (v) => save({ range: v }) });
  const theme = choice({ label: 'Theme', labelHidden: true, name: 'theme', value: p.theme || 'device', options: [['device', 'Follow device'], ['light', 'Light (paper)'], ['dark', 'Dark (night ink)']], onchange: (v) => save({ theme: v }) });
  const mapColors = choice({ label: 'Map colours', labelHidden: true, name: 'map-colors', value: p.map_colors || 'dimmed', options: MAP_COLORS_CHOICES, onchange: (v) => save({ map_colors: v }) });
  // the person's map style (map_style): Map, Satellite, … among the maps an admin lets people switch to
  const styles = mapStyles(maps);
  const mapStyle = styles.length > 1 ? choice({ label: 'Map style', labelHidden: true, name: 'map-style', value: (pickStyle(maps, p) || {}).id, options: styles.map((x) => [x.id, x.short || x.name]), onchange: (v) => save({ map_style: v }) }) : null;
  const mapStyleNote = h('p', { class: 'setting-note' }, styles.length > 1 ? 'The switch on a map page changes it too.'
    : maps && maps.tiles && maps.tiles.mode === 'online' ? 'This server offers one map, so there is nothing to pick.' : 'Maps come from this server’s offline map, so there is one map to show.');

  const example = () => h('p', { class: 'setting-example' });
  const distEx = example();
  const tempEx = example();
  const pressEx = example();
  const rangeEx = example();
  const themeEx = example();
  const defNote = () => h('p', { class: 'setting-default' });
  const distDef = defNote();
  const tempDef = defNote();
  const pressDef = defNote();
  const rangeDef = defNote();

  const browserTags = [...new Set((navigator.languages || [navigator.language]).filter((t) => t && isSupportedLocale(t)))];
  const langOptions = () => {
    const pr = currentPrefs() || {};
    const tags = [...new Set([...browserTags, ...LOCALES, ...(pr.language && pr.language !== 'auto' ? [pr.language] : [])])];
    return [['auto', `From your browser${browserTags[0] ? ` (now ${browserTags[0]})` : ''}`],
      ...tags.map((t) => [t, `${langName(t) || t} · ${t}`])];
  };
  const lang = h('select', { class: 'field-select', id: 'pref-language', 'aria-label': 'Language for dates and numbers', 'aria-describedby': 'pref-language-note' });
  const fillLang = () => {
    const pr = currentPrefs() || {};
    const want = pr.language && pr.language !== 'auto' ? pr.language : 'auto';
    replace(lang, langOptions().map(([v, t]) => h('option', { value: v, selected: v === want }, t)));
    lang.value = want;
  };
  lang.addEventListener('change', () => save({ language: lang.value }));
  const langNote = h('p', { class: 'setting-note', id: 'pref-language-note' });
  const langEx = example();

  const size = choice({ label: 'Interface size', labelHidden: true, name: 'interface_size', value: sizeState().size, cls: 'segmented-sizes', options: SIZES.map((s) => [s, `${s}%`]),
    onchange: (v) => { setSize({ size: Number(v) }); save({ interface_size: Number(v) }); } });
  const follow = switchField({ label: 'Follow screen size', checked: sizeState().follow, onchange: (on) => { setSize({ follow: on }); save({ follow_screen: on }); } });
  const sizeNoteEl = h('p', { class: 'setting-note' });
  // kept in this browser only (shared/devicesize.js), never with the account
  const device = choice({ label: 'Interface size on this device', labelHidden: true, name: 'device_size_page', value: sizeState().device ? String(sizeState().device) : 'auto', cls: 'segmented-sizes',
    options: deviceSizeOptions().map((o) => [o.value, o.value === 'auto' ? 'Automatic' : o.label.replace(/\u00a0/g, '')]), onchange: (v) => setDeviceSize(v) });
  const deviceNoteEl = h('p', { class: 'setting-note' });

  const defaultLine = (el, kind, mine) => {
    const d = kind === 'range' ? srv.range : srv.units?.[kind];
    const words = UNIT_WORDS[kind];
    el.textContent = d == null ? '' : (mine === d
      ? `Same as the server default (${words[d]}).`
      : `Your choice; the server default is ${words[d]}.`);
  };

  const sync = () => {
    const pr = currentPrefs() || {};
    const u = pr.units || {};
    dist.set(u.distance || 'km'); temp.set(u.temperature || 'c'); press.set(u.pressure || 'bar');
    range.set(pr.range || 'rated'); theme.set(pr.theme || 'device'); mapColors.set(pr.map_colors || 'dimmed');
    if (mapStyle) mapStyle.set((pickStyle(maps, pr) || {}).id);
    distEx.textContent = `For example: a drive of ${format(42.2, 'distance')} at ${format(88, 'speed')}, a place radius of ${format(150, 'length')}, ${format(152, 'efficiency')}.`;
    tempEx.textContent = `For example: outside ${format(21.5, 'temperature')}, cabin ${format(23, 'temperature')}, battery ${format(31, 'temperature')}.`;
    pressEx.textContent = `For example: front left ${format(290, 'pressure')}, the same pressure in the other unit is ${format(290, 'pressure', { units: { pressure: (u.pressure || 'bar') === 'bar' ? 'psi' : 'bar' } })}.`;
    rangeEx.textContent = `Shown as “${(pr.range || 'rated') === 'ideal' ? 'Ideal range' : 'Rated range'}”, for example ${format(386, 'range')}.`;
    const dark = document.documentElement.dataset.theme ? document.documentElement.dataset.theme === 'dark' : matchMedia('(prefers-color-scheme: dark)').matches;
    themeEx.textContent = (pr.theme || 'device') === 'device'
      ? `Your device asks for ${dark ? 'dark' : 'light'} now, so this page is ${dark ? 'night ink' : 'paper'}.`
      : `Always ${pr.theme === 'dark' ? 'night ink' : 'paper'}, whatever your device asks for.`;
    defaultLine(distDef, 'distance', u.distance); defaultLine(tempDef, 'temperature', u.temperature);
    defaultLine(pressDef, 'pressure', u.pressure); defaultLine(rangeDef, 'range', pr.range);
    if (document.activeElement !== lang) fillLang();
    if (document.activeElement !== mainSel) fillMain();
    const loc = getLocale();
    const chosen = pr.language && pr.language !== 'auto';
    langNote.textContent = `In use: ${langName(loc) || loc} (${loc}), ${chosen ? 'your choice' : 'the first of your browser’s languages this browser can write'}. If a language cannot be used, the next one in your browser’s list is taken, then English (US).`;
    let sample = '';
    try {
      sample = `${new Intl.NumberFormat(loc, { maximumFractionDigits: 1 }).format(12345.6)} · ${formatDateTime(api.now(), ctx.tz, { year: true })}`;
    } catch { sample = ''; }
    langEx.textContent = sample ? `Numbers and dates now look like this: ${sample}. Words stay in English.` : '';
    size.set(sizeState().size); follow.set(sizeState().follow);
    sizeNoteEl.textContent = sizeNote();
    device.set(sizeState().device ? String(sizeState().device) : 'auto');
    deviceNoteEl.textContent = deviceSizeNote(sizeState().device, { account: sizeState().size });
  };
  ctx.onCleanup(onPrefs(sync));
  ctx.onCleanup(display.onChange(sync));
  ctx.onCleanup(onSize(sync));
  const mq = matchMedia('(prefers-color-scheme: dark)');
  mq.addEventListener('change', sync);
  ctx.onCleanup(() => mq.removeEventListener('change', sync));
  fillLang();

  // The main car: the car every page without one in its address shows (main_car), else the car looked at last.
  const cars = carList || [];
  const mainSel = h('select', { class: 'field-select', id: 'pref-main-car', 'aria-label': 'Main car', 'aria-describedby': 'pref-main-car-note', disabled: !carList });
  const mainNote = h('p', { class: 'setting-note', id: 'pref-main-car-note' });
  const fillMain = () => {
    const want = (currentPrefs() || {}).main_car || '';
    const known = !want || cars.some((c) => c.id === want);
    replace(mainSel, h('option', { value: '' }, 'None (the car you looked at last)'),
      carOptions(cars, { option: (c) => h('option', { value: c.id, selected: c.id === want }, c.name || c.id) }),
      known ? null : h('option', { value: want, selected: true }, want));
    mainSel.value = want;
    const car = cars.find((c) => c.id === want);
    mainNote.textContent = !carList ? 'The list of cars did not load. Reload the page to choose a main car.'
      : !cars.length ? 'No car on your account yet.'
        : want ? `Pages without a car of their own show ${car ? car.name : want}.`
          : 'Pages without a car of their own show the car you looked at last.';
  };
  mainSel.addEventListener('change', async () => {
    const before = (currentPrefs() || {}).main_car || null;
    try { await savePref({ main_car: mainSel.value || null }, { quiet: false }); } catch { mainSel.value = before || ''; }
    fillMain();
  });
  fillMain();

  const about = currentAbout();
  // every pack the server offers (GET /ui-packs); the one running now is the served pack
  const packHere = (about && about.ui_pack && (about.ui_pack.id || (about.ui_pack.name || '').toLowerCase())) || 'logbook';
  const packNow = p.ui_pack || packHere;
  const packSel = h('select', { class: 'field-select', id: 'pref-pack', 'aria-label': 'UI pack', 'aria-describedby': 'pref-pack-note' },
    packList.items.map((k) => h('option', { value: k.id, selected: k.id === packNow }, packLabel(k))));
  if (!packList.items.some((k) => k.id === packNow)) packSel.prepend(h('option', { value: packNow, selected: true }, packNow));
  const switchBtn = button('Switch now', { onclick: () => location.reload() });
  const packNote = h('p', { class: 'setting-note', id: 'pref-pack-note' });
  const paintPack = () => {
    const other = packSel.value !== packHere;
    switchBtn.hidden = !other;
    packNote.textContent = other ? 'The new pack is used the next time the app loads, or now with Switch now.'
      : `In use: ${about && about.ui_pack ? `${about.ui_pack.name} ${about.ui_pack.version || ''}`.trim() : 'Logbook'}. Your choice is used the next time the app loads.`;
  };
  packSel.addEventListener('change', async () => {
    const before = (currentPrefs() || {}).ui_pack || packHere;
    try { await savePref({ ui_pack: packSel.value }, { quiet: false }); } catch { packSel.value = before; }
    paintPack();
  });
  paintPack();
  const savedNote = h('span', { class: 'label-note' }, 'no save button: each change is saved at once with your account and used on every device you sign in on');

  const mine = h('div', { class: 'settings-mine' },
    h('section', { class: 'settings', 'aria-labelledby': 'units-title', cover: 'web.settings.general.autosave' },
      h('h2', { class: 'label settings-label', id: 'units-title' }, 'Units and range', savedNote),
      setting('Distance', 'Also sets speeds, efficiency and short lengths (metres or feet).', dist, { cover: 'web.settings.general.unit-length', extra: [distEx, distDef] }),
      setting('Temperature', 'Outside, cabin and battery temperatures.', temp, { cover: 'web.settings.general.unit-temperature', extra: [tempEx, tempDef] }),
      setting('Tyre pressure', 'The four tyre pressures on the car and drive pages.', press, { cover: 'web.settings.general.unit-pressure', extra: [pressEx, pressDef] }),
      setting('Range', 'Two range figures come from the car. Rated follows the official test of the market the car was sold in, so the same car shows another number in another country; ideal is the car’s own, usually longer figure. The one you pick is used on every page, chart and download, and efficiency is worked out from it.', range, { cover: 'web.settings.general.preferred-range', extra: [rangeEx, rangeDef] })),
    h('section', { class: 'settings', 'aria-labelledby': 'look-title' },
      h('h2', { class: 'label settings-label', id: 'look-title' }, 'Look and language', h('span', { class: 'label-note' }, 'yours alone, not this browser’s or the server’s')),
      setting('Theme', 'Paper by day and night ink after dark, following your device, or always one of the two. It is put in place before the page draws, so it never flashes the other one.', theme, { cover: ['shell.theme', 'shots.theme-note', 'web.settings.general.theme'], extra: themeEx }),
      setting('Map colours', `${MAP_COLORS_TEXT} Satellite pictures are never dimmed, so this does not apply to them.`, mapColors),
      setting('Map style', 'The map you see on map pages, among the ones an admin offers on this server.', h('div', { class: 'lang-control' }, mapStyle, mapStyleNote), { id: 'map-style' }),
      setting('Language', 'How dates, times and numbers are written (for example 1,234.5 or 1.234,5, and the first day of the week).', h('div', { class: 'lang-control' }, lang, langNote),
        { cover: ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation'], extra: langEx }),
      setting('Interface size on this device', 'This device only: kept in this browser, not with your account, so a laptop with large display scaling and a desktop monitor can each have their own size. Automatic uses your account’s Interface size below; a fixed size draws everything at that percent of the standard size, with nothing added for big screens.', h('div', { class: 'size-control' }, device, deviceNoteEl), { cls: 'setting-size' }),
      setting('Interface size', 'Your account’s size, on every device left at Automatic. Text, controls, tables, charts and maps grow or shrink together. With “Follow screen size” on, big screens such as 4K monitors get larger text by themselves. Browser zoom works on top.', h('div', { class: 'size-control' }, size, follow, sizeNoteEl), { cls: 'setting-size' }),
      setting('UI pack', 'The look of the whole app. Admins install other packs; your data and settings stay the same in every pack.',
        h('div', { class: 'lang-control' }, h('div', { class: 'pack-control' }, packSel, switchBtn), packNote)),
      setting('Main car', 'The car shown first on the pages that do not name one, such as the car overview opened from the menu. With none chosen, the car you looked at last is shown.',
        h('div', { class: 'lang-control' }, mainSel, mainNote))));

  // ------------------------------------------------------------ the divider
  const divider = h('div', { class: 'settings-divider', role: 'separator', cover: 'web.settings.general.divider' },
    h('span', null, admin ? 'Below: for everyone on this server' : 'Below: set by an admin for everyone'));
  const dividerNote = h('p', { class: 'settings-divider-note' }, admin
    ? 'Your own choices above win over these for you. Changing a default changes it for everyone who has not made their own choice.'
    : 'Your own choices above win over these. Only an admin can change what is below.');

  // ------------------------------------------------------------ the server's defaults
  // Where maps and addresses come from is in Settings: Maps and addresses; the release check in Settings:
  // Server and access; the time zone for new cars in Settings: Cars.
  const serverBox = h('div', { class: 'settings-server' });

  /** "Not saved: …" with the server's own reasons, not only its summary. */
  const notSaved = (err) => {
    const why = err && err.fields ? Object.values(err.fields).join(' ') : (err && err.message) || String(err);
    return `Not saved: ${why}`;
  };

  function drawServer(s) {
    srv = s;
    const disabled = !admin;
    const patch = async (body) => {
      try {
        const next = await ctx.api.patch('settings', { version: srv.version, ...body });
        ctx.flash('Saved for everyone on this server.');
        if (ctx.alive()) { drawServer(next); sync(); }
        return next;
      } catch (err) {
        if (err && (err.code === 'stale_version' || err.status === 409)) {
          ctx.flash('Someone changed these settings meanwhile; they are shown fresh now. Make your change again.', { kind: 'info' });
          const fresh = await ctx.api.get('settings');
          if (ctx.alive()) { drawServer(fresh); sync(); }
          return null;
        }
        throw err;
      }
    };
    // A default saves at once. When it is refused, only that control goes back.
    const quick = (body, undo) => patch(body).catch((err) => {
      undo();
      ctx.flash(notSaved(err), { kind: 'error' });
    });

    const defDist = choice({ label: 'Default distance', labelHidden: true, name: 'def-distance', value: s.units.distance, options: [['km', 'km'], ['mi', 'mi']], onchange: (v) => quick({ units: { distance: v } }, () => defDist.set(s.units.distance)) });
    const defTemp = choice({ label: 'Default temperature', labelHidden: true, name: 'def-temperature', value: s.units.temperature, options: [['c', '°C'], ['f', '°F']], onchange: (v) => quick({ units: { temperature: v } }, () => defTemp.set(s.units.temperature)) });
    const defPress = choice({ label: 'Default pressure', labelHidden: true, name: 'def-pressure', value: s.units.pressure, options: [['bar', 'bar'], ['psi', 'psi']], onchange: (v) => quick({ units: { pressure: v } }, () => defPress.set(s.units.pressure)) });
    const defRange = choice({ label: 'Default range', labelHidden: true, name: 'def-range', value: s.range, options: [['rated', 'Rated'], ['ideal', 'Ideal']], onchange: (v) => quick({ range: v }, () => defRange.set(s.range)) });
    const defaults = h('div', { class: 'defaults-grid' },
      h('span', { class: 'defaults-name' }, 'Distance'), defDist,
      h('span', { class: 'defaults-name' }, 'Temperature'), defTemp,
      h('span', { class: 'defaults-name' }, 'Pressure'), defPress,
      h('span', { class: 'defaults-name' }, 'Range'), defRange);
    for (const el of [defDist, defTemp, defPress, defRange]) el.querySelectorAll('button').forEach((b) => { b.disabled = disabled; });

    replace(serverBox,
      h('section', { class: 'settings', 'aria-labelledby': 'defaults-title' },
        h('h2', { class: 'label settings-label', id: 'defaults-title' }, 'Defaults for everyone', h('span', { class: 'label-note' }, admin ? 'admins only; saved at once' : 'read only for your account'), disabled ? flag('Read only') : null),
        setting('Units and range', 'For people who have not chosen their own. The distance unit can start from the one a car reports from its own screen settings.', defaults),
        h('p', { class: 'settings-elsewhere' }, 'Also for everyone, on their own pages: where maps and addresses come from in ',
          pageLink('Settings: Maps and addresses', 'settings-maps', {}, { arrow: false, cls: null }), ', the release check in ',
          h('a', { href: `${ctx.link('settings-server')}#release-check` }, 'Settings: Server and access'), ', and the time zone for new cars in ',
          h('a', { href: `${ctx.link('settings-cars')}#default-tz` }, 'Settings: Cars'), '.')));
  }
  try { drawServer(server); } catch (err) { replace(serverBox, errorView(err)); }
  sync();

  ctx.main.append(
    pageHead({ eyebrow: 'Settings', title: 'Display and units', deck: ctx.info.purpose }),
    mine, divider, dividerNote, serverBox);
  ctx.ready();
  // Its own controls change the preferences: they follow onPrefs above; the page is not drawn again.
  return { update: (c) => { const to = movedUrl(location.href); if (to) { navigate(to, { replace: true }); return; } sync(); c.ready(); } };
}
