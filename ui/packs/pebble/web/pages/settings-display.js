// Sentry Mate, "Pebble" UI pack: Settings: Display and units (/settings/display).
//
// Your own choices first (GET / PATCH /api/v1/me/preferences): theme (Auto, Light, Dark), units, rated or ideal
// range, language, the main car (none: the car looked at last), interface size and the UI pack. Each is a
// rounded list row; a picker opens a sheet, and the change saves at once and shows on every page, with an example in the chosen unit next to it.
// Map style picks among the maps an admin offers (GET /api/v1/maps/config). Below a divider, what an admin sets
// for everyone (GET / PATCH /api/v1/settings): the default units and range. Where maps and addresses come from
// (with the outbound proxy and outside links) is in Settings: Maps and addresses, the release check in Settings:
// Server, the time zone for new cars in Settings: Cars; old links to those sections lead there (state.movedUrl).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { getLocale, isSupportedLocale } from '../shared/i18n.js';
import { format } from '../shared/units.js';
import { formatDateTime } from '../shared/time.js';
import * as state from '../shared/state.js';
import { tile, btn, chip, note, icon } from '../core/ui.js';
import { segmented, toggle } from '../core/form.js';
import { currentPrefs, onPrefs, savePref, isDark, onTheme } from '../core/prefs.js';
import { SIZES, SIZE_MIN, SIZE_MAX, sizeState, setSize, setDeviceSize, onSize, sizeNote, display } from '../core/display.js';
import { deviceSizeOptions, deviceSizeNote } from '../shared/devicesize.js';
import { currentAbout } from '../core/frame.js';
import { sectionHead } from '../core/bits.js';
import { group, pickRow, ctlRow, infoRow, notSaved } from '../core/settings.js';
import { inOrder } from '../core/settings.js';
import { packChoices } from '../shared/packs.js';
import { MAP_COLORS_CHOICES, MAP_COLORS_TEXT, mapStyles, pickStyle } from '../shared/maptiles.js';

export const id = 'settings-display';
export const shape = [[2, 'h3'], [2, 'h3'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Display and units';

const LOCALES = ['en-US', 'en-GB', 'en-CA', 'en-AU', 'de-DE', 'de-CH', 'fr-FR', 'fr-CA', 'nl-NL', 'nb-NO', 'sv-SE', 'da-DK',
  'fi-FI', 'es-ES', 'it-IT', 'pt-PT', 'pl-PL', 'cs-CZ', 'ja-JP', 'ko-KR', 'zh-CN'];
// used only when the server's list of packs can't be read
const PACKS = [['pebble', 'Pebble', 'Light, rounded tiles and a floating dock'], ['instrument', 'Instrument', 'A dark glass cluster with a side bar'], ['logbook', 'Logbook', 'Paper and ink, the day as a log'],
  ['horizon', 'Horizon', 'Neon panels over a dusk sky'], ['console', 'Console', 'A keyboard-first operator console']];
const DIST = [['km', 'Kilometres', 'km, km/h, Wh/km, metres'], ['mi', 'Miles', 'mi, mph, Wh/mi, feet']];
const TEMP = [['c', 'Celsius', '°C'], ['f', 'Fahrenheit', '°F']];
const PRESS = [['bar', 'bar'], ['psi', 'psi']];
const RANGE = [['rated', 'Rated', 'The official test figure of the market the car was sold in'], ['ideal', 'Ideal', 'The car’s own, usually longer figure']];
const WORDS = { distance: { km: 'kilometres', mi: 'miles' }, temperature: { c: '°C', f: '°F' }, pressure: { bar: 'bar', psi: 'psi' }, range: { rated: 'rated', ideal: 'ideal' } };

function langName(tag) {
  try { const n = new Intl.DisplayNames(['en'], { type: 'language' }).of(tag); return n && n !== tag ? n : null; } catch { return null; }
}
export async function mount(ctx) {
  // an old link to a section that moved (/settings/display#map-tiles): open it where it is now
  const moved = state.movedUrl(location.href);
  if (moved) { state.navigate(moved, { replace: true }); return {}; }
  inOrder(ctx);
  const admin = ctx.admin;
  let [srv, mapCfg, packList, cars] = await Promise.all([ctx.api.get('settings'), ctx.api.get('maps/config').catch(() => null),
    packChoices(ctx.api, { fallback: PACKS, current: (currentPrefs() || {}).ui_pack || null }),
    ctx.api.list('cars', { sort: 'custom' }).all({ max: 200 }).catch(() => [])]);
  if (!ctx.alive()) return {};
  const p0 = currentPrefs() || {};
  const save = (patch) => savePref(patch).catch((err) => { ctx.toast(notSaved(err), { kind: 'error' }); throw err; });
  const saved = (what) => ctx.toast(`${what} saved. Every page uses it now.`);

  /* ------------------------------------------------------------ look */
  const themeEx = h('p', { class: 'set-example', cover: 'shots.theme-note' });
  const theme = segmented({ label: null, name: 'Theme', value: p0.theme || 'device', options: [['device', 'Auto'], ['light', 'Light'], ['dark', 'Dark']],
    onchange: (v) => save({ theme: v }).then(() => saved('Theme')).catch(() => theme.set((currentPrefs() || {}).theme || 'device')) });
  const sizeValue = h('output', { class: 'size-value', 'aria-live': 'polite' });
  let sizeTimer = 0;
  ctx.onCleanup(() => clearTimeout(sizeTimer));
  const stepSize = (d) => {
    const now = sizeState().size;
    const next = d > 0 ? (SIZES.find((s) => s > now) ?? SIZE_MAX) : ([...SIZES].reverse().find((s) => s < now) ?? SIZE_MIN);
    setSize({ size: next });
    clearTimeout(sizeTimer);
    sizeTimer = setTimeout(() => save({ interface_size: sizeState().size }).catch(() => {}), 400);
    sync();
  };
  const smaller = btn('Smaller', { kind: 'soft', icon: 'minus', iconOnly: true, onclick: () => stepSize(-1) });
  const bigger = btn('Bigger', { kind: 'soft', icon: 'plus', iconOnly: true, onclick: () => stepSize(1) });
  const follow = toggle({ label: 'Follow screen size', sub: 'Big screens such as a 4K monitor get larger text by themselves.', checked: sizeState().follow,
    onchange: (on) => { setSize({ follow: on }); save({ follow_screen: on }).catch(() => {}); sync(); } });
  const sizeLine = h('span', { class: 'row-sub' });
  // kept in this browser only (shared/devicesize.js), never with the account
  const deviceSeg = segmented({ label: null, name: 'Interface size on this device', value: sizeState().device ? String(sizeState().device) : 'auto', cls: 'seg--device',
    options: deviceSizeOptions().map((o) => [o.value, o.value === 'auto' ? 'Auto' : o.label]), onchange: (v) => { setDeviceSize(v); sync(); } });
  const deviceLine = h('span', { class: 'row-sub' });
  ctx.onCleanup(onSize(() => sync()));
  const about = currentAbout();
  const packNow = (p0.ui_pack || (about && about.ui_pack && about.ui_pack.name ? about.ui_pack.name.toLowerCase() : 'pebble'));
  // every pack the server offers (GET /ui-packs), each with its short description
  const packOpts = packList.items.map((k) => [k.id, k.name, k.blurb || null]);
  if (!packOpts.some(([k]) => k === packNow)) packOpts.push([packNow, packNow, 'Installed on this server']);
  const packHere = (about && about.ui_pack && (about.ui_pack.id || (about.ui_pack.name || '').toLowerCase())) || 'pebble';
  const switchRow = h('div', { class: 'row row-static set-ex-row', hidden: packNow === packHere },
    h('span', { class: 'row-sub' }, 'The new pack is used the next time the app loads. '), btn('Switch now', { kind: 'soft', onclick: () => location.reload() }));
  const pack = pickRow({ name: 'UI pack', sub: 'The look of the whole app. Used the next time the app loads.', value: packNow, options: packOpts,
    sheetNote: 'Admins can install more packs. Your data and settings stay the same in every pack.',
    onpick: (v) => save({ ui_pack: v }).then(() => { switchRow.hidden = v === packHere; if (v !== packHere) ctx.toast('UI pack saved. Switch now, or it is used the next time the app loads.', { kind: 'info' }); }) });
  const mapColors = segmented({ label: null, name: 'Map colours', value: p0.map_colors || 'dimmed', options: MAP_COLORS_CHOICES,
    onchange: (v) => save({ map_colors: v }).then(() => saved('Map colours')).catch(() => mapColors.set((currentPrefs() || {}).map_colors || 'dimmed')) });
  // the person's map style: Map, Satellite, ... among the maps an admin lets people switch to
  const styles = mapStyles(mapCfg);
  const mapStyle = styles.length > 1 ? segmented({ label: null, name: 'Map style', value: (pickStyle(mapCfg, p0) || {}).id, options: styles.map((x) => [x.id, x.short || x.name]),
    onchange: (v) => save({ map_style: v }).then(() => saved('Map style')).catch(() => mapStyle.set((pickStyle(mapCfg, currentPrefs() || {}) || {}).id)) }) : null;
  const mapStyleRow = mapStyle
    ? ctlRow({ name: 'Map style', sub: 'The map you see on map pages, among the ones an admin offers. The switch on a map page changes it too.', control: mapStyle, stack: true })
    : infoRow({ name: 'Map style', sub: mapCfg && mapCfg.tiles && mapCfg.tiles.mode === 'online' ? 'This server offers one map, so there is nothing to pick.' : 'Maps come from this server’s offline map, so there is one map to show.' });
  mapStyleRow.id = 'map-style';
  const lookT = tile({ w: 2, title: 'Look', icon: 'sun', cover: ['shell.theme', 'web.settings.general.theme'] },
    group(null, [
      ctlRow({ name: 'Theme', sub: 'Auto follows your device, day and night.', control: theme, stack: true }),
      h('div', { class: 'row row-static set-ex-row' }, themeEx),
      ctlRow({ name: 'Map colours', sub: `${MAP_COLORS_TEXT} Satellite pictures are always shown as taken.`, control: mapColors, stack: true }),
      mapStyleRow,
    ]),
    group('Size', [
      ctlRow({ name: 'Interface size on this device', sub: 'This device only: kept in this browser, not with your account, so each computer can have its own size. Auto uses your account’s size below.', control: deviceSeg, stack: true }),
      h('div', { class: 'row row-static set-ex-row' }, deviceLine),
      ctlRow({ name: 'Interface size', sub: 'Your account’s size, on every device left at Auto.', control: h('div', { class: 'stepper', role: 'group', 'aria-label': 'Interface size' }, smaller, sizeValue, bigger) }),
      h('div', { class: 'row row-static set-ex-row' }, sizeLine),
      follow,
    ]),
    group('Pack', [pack, switchRow]));

  /* ------------------------------------------------------------ units */
  const u0 = p0.units || {};
  const exDist = h('span', { class: 'row-sub' });
  const exTemp = h('span', { class: 'row-sub' });
  const exPress = h('span', { class: 'row-sub' });
  const exRange = h('span', { class: 'row-sub' });
  const dist = pickRow({ name: 'Distance', value: u0.distance || 'km', options: DIST, cover: 'web.settings.general.unit-length', sheetNote: 'Also sets speeds, efficiency and short lengths.', onpick: (v) => save({ units: { distance: v } }).then(() => saved('Distance unit')) });
  const temp = pickRow({ name: 'Temperature', value: u0.temperature || 'c', options: TEMP, cover: 'web.settings.general.unit-temperature', onpick: (v) => save({ units: { temperature: v } }).then(() => saved('Temperature unit')) });
  const press = pickRow({ name: 'Tyre pressure', value: u0.pressure || 'bar', options: PRESS, cover: 'web.settings.general.unit-pressure', onpick: (v) => save({ units: { pressure: v } }).then(() => saved('Pressure unit')) });
  const range = pickRow({ name: 'Range', value: p0.range || 'rated', options: RANGE, cover: 'web.settings.general.preferred-range',
    sheetNote: 'The one you pick is used on every page, chart and download, and efficiency is worked out from it.',
    onpick: (v) => save({ range: v }).then(() => saved('Range')) });
  const unitsT = tile({ w: 2, title: 'Units and range', icon: 'gauge', cover: 'web.settings.general.autosave' },
    group(null, [dist, h('div', { class: 'row row-static set-ex-row' }, exDist), temp, h('div', { class: 'row row-static set-ex-row' }, exTemp),
      press, h('div', { class: 'row row-static set-ex-row' }, exPress), range, h('div', { class: 'row row-static set-ex-row' }, exRange)]),
    note('No Save button: each change is saved with your account at once and used on every device you sign in on. Data is kept in metric, so a unit change never changes it.'));

  /* ------------------------------------------------------------ language */
  const browserTags = [...new Set((navigator.languages || [navigator.language]).filter((t) => t && isSupportedLocale(t)))];
  const langOpts = () => {
    const pr = currentPrefs() || {};
    const tags = [...new Set([...browserTags, ...LOCALES, ...(pr.language && pr.language !== 'auto' ? [pr.language] : [])])];
    return [['auto', 'From your browser', browserTags[0] ? `Now ${langName(browserTags[0]) || browserTags[0]} (${browserTags[0]})` : 'The first language this browser can write'],
      ...tags.map((t) => [t, langName(t) || t, t])];
  };
  const lang = pickRow({ name: 'Dates and numbers', value: p0.language && p0.language !== 'auto' ? p0.language : 'auto', options: langOpts(), sheetTitle: 'Language',
    cover: ['shell.locale', 'web.settings.general.ui-language'],
    sheetNote: 'Words stay in English. This sets how dates, times and numbers are written, and the first day of the week.',
    onpick: (v) => save({ language: v }).then(() => saved('Language')) });
  const langInUse = h('span', { class: 'row-sub' });
  const langSample = h('span', { class: 'row-sub' });
  /* ------------------------------------------------------------ main car */
  // '' is no main car: pages without a car in their address show the car you looked at last.
  const carOpts = [['', 'None', 'The car you looked at last'],
    ...cars.map((c) => [c.id, c.name || c.id, [c.year, c.model].filter(Boolean).join(' ') || null, state.isSold(c)])];
  const mainCar = pickRow({ name: 'Main car', sub: 'Pages without a car in their address show this one.', value: p0.main_car || '', options: carOpts,
    sheetNote: 'With None, each page opens on the car you looked at last.',
    onpick: (v) => save({ main_car: v || null }).then(() => saved('Main car')) });
  const langT = tile({ w: 2, title: 'Language', icon: 'globe' },
    group(null, [lang,
      h('div', { class: 'row row-static set-ex-row', cover: 'web.shell.locale-negotiation' }, h('span', { class: 'row-text' }, langInUse, langSample))]),
    cars.length ? group('Car', [mainCar]) : null);

  /* ------------------------------------------------------------ keeping it in step */
  function sync() {
    const pr = currentPrefs() || {};
    const u = pr.units || {};
    theme.set(pr.theme || 'device');
    mapColors.set(pr.map_colors || 'dimmed');
    if (mapStyle) mapStyle.set((pickStyle(mapCfg, pr) || {}).id);
    const dark = isDark();
    themeEx.textContent = (pr.theme || 'device') === 'device'
      ? `Your device asks for ${dark ? 'dark' : 'light'} now, so the pages are ${dark ? 'dark' : 'light'}. It is set before a page draws, so it never flashes.`
      : `Always ${pr.theme === 'dark' ? 'dark' : 'light'}, whatever your device asks for.`;
    dist.set(u.distance || 'km'); temp.set(u.temperature || 'c'); press.set(u.pressure || 'bar'); range.set(pr.range || 'rated');
    const def = (kind, mine) => { const d = kind === 'range' ? srv.range : srv.units && srv.units[kind]; return d == null ? '' : mine === d ? ` Same as the server default.` : ` The server default is ${WORDS[kind][d]}.`; };
    exDist.textContent = `For example: ${format(42.2, 'distance')} at ${format(88, 'speed')}, a circle of ${format(150, 'length')}, ${format(152, 'efficiency')}.${def('distance', u.distance)}`;
    exTemp.textContent = `For example: outside ${format(21.5, 'temperature')}, cabin ${format(23, 'temperature')}.${def('temperature', u.temperature)}`;
    exPress.textContent = `For example: front left ${format(290, 'pressure')}.${def('pressure', u.pressure)}`;
    exRange.textContent = `Shown as “${(pr.range || 'rated') === 'ideal' ? 'Ideal range' : 'Rated range'}”, for example ${format(386, 'range')}.${def('range', pr.range)}`;
    lang.set(pr.language && pr.language !== 'auto' ? pr.language : 'auto');
    mainCar.set(pr.main_car || '');
    const loc = getLocale();
    const chosen = pr.language && pr.language !== 'auto';
    langInUse.textContent = `In use: ${langName(loc) || loc} (${loc}), ${chosen ? 'your choice' : 'the first of your browser’s languages this browser can write'}. If one cannot be used, the next in your browser’s list is taken, then English (US).`;
    let sample = '';
    try { sample = `${new Intl.NumberFormat(loc, { maximumFractionDigits: 1 }).format(12345.6)} · ${formatDateTime(api.now(), ctx.tz, { year: true })}`; } catch { sample = ''; }
    langSample.textContent = sample ? `Now: ${sample}` : '';
    const s = sizeState();
    sizeValue.textContent = `${s.size}%`;
    smaller.disabled = s.size <= SIZE_MIN;
    bigger.disabled = s.size >= SIZE_MAX;
    follow.set(s.follow);
    sizeLine.textContent = sizeNote();
    deviceSeg.set(s.device ? String(s.device) : 'auto');
    deviceLine.textContent = deviceSizeNote(s.device, { account: s.size });
  }
  ctx.onCleanup(onPrefs(sync));
  ctx.onCleanup(display.onChange(sync));
  ctx.onCleanup(onTheme(sync));

  /* ------------------------------------------------------------ for everyone */
  // a row that opens another settings page (at a section)
  const moreRow = (page, ic, name, sub, hash = '') => h('a', { class: 'row', href: `${state.buildUrl(page)}${hash}` },
    h('span', { class: 'row-ic', 'aria-hidden': 'true' }, icon(ic)),
    h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, name), h('span', { class: 'row-sub' }, sub)),
    h('span', { class: 'row-side' }, icon('chevronRight', 'row-go')));
  const divider = sectionHead(admin ? 'For everyone on this server' : 'Set by an admin for everyone',
    admin ? 'Your own choices above win over these for you. A default changes it for everyone who has not chosen their own.' : 'Your own choices above win over these. Only an admin can change what is below.',
    { cover: 'web.settings.general.divider' });
  const serverBox = h('div', { class: 'contents' });

  async function patch(body) {
    try {
      const next = await ctx.api.patch('settings', { version: srv.version, ...body });
      ctx.toast('Saved for everyone on this server.');
      if (ctx.alive()) drawServer(next);
      return next;
    } catch (err) {
      if (err && (err.code === 'stale_version' || err.status === 409)) {
        ctx.toast('Someone changed these settings meanwhile; they are shown fresh now. Make your change again.', { kind: 'info' });
        const fresh = await ctx.api.get('settings');
        if (ctx.alive()) drawServer(fresh);
        return null;
      }
      throw err;
    }
  }
  const quick = (body, undo) => patch(body).catch((err) => { undo(); ctx.toast(notSaved(err), { kind: 'error' }); });

  function drawServer(s) {
    srv = s;
    const off = !admin;
    const ro = off ? chip('Read only', { tone: 'muted', icon: 'lock' }) : null;

    const defs = [
      ['distance', 'Distance', DIST], ['temperature', 'Temperature', TEMP], ['pressure', 'Tyre pressure', PRESS],
    ].map(([k, name, opts]) => pickRow({ name, value: s.units[k], options: opts, disabled: off, onpick: (v) => quick({ units: { [k]: v } }, () => {}) }));
    const defRange = pickRow({ name: 'Range', value: s.range, options: RANGE, disabled: off, onpick: (v) => quick({ range: v }, () => {}) });
    const defaultsT = tile({ w: 2, title: 'Defaults for everyone', icon: 'person', side: ro },
      group(null, [...defs, defRange]),
      note('For people who have not chosen their own. The distance unit can start from the one a car reports from its own screen settings.'));

    // what an admin sets for everyone, elsewhere now
    const elsewhereT = tile({ w: 2, title: 'More for everyone', icon: 'server' },
      group(null, [
        moreRow('settings-maps', 'map', 'Maps and addresses', 'Map tiles, the offline map, address lookup and outside links.'),
        moreRow('settings-cars', 'car', 'Time zone for new cars', 'In Settings: Cars.', '#default-tz'),
        moreRow('settings-server', 'upload', 'Server updates', 'The release check, in Settings: Server.', '#release-check'),
      ]));
    serverBox.replaceChildren(defaultsT, elsewhereT);
  }
  drawServer(srv);
  sync();

  ctx.view.replaceChildren(lookT, unitsT, langT,
    tile({ w: 2, title: 'Where these live', icon: 'info' },
      group(null, [
        infoRow({ icon: 'person', name: 'Yours alone', sub: 'Theme, map colours and style, units, range, language, size and pack follow your account to every device.' }),
        infoRow({ icon: 'server', name: 'The server’s', sub: 'The defaults below are one setting for everyone.' }),
      ])),
    divider, serverBox);
  ctx.ready();
  return { update: (c) => { sync(); c.ready(); } };
}
