// Display and units (/settings/display): how numbers, words and maps look. At the top a preview cluster shows
// the same four readings in the person's units as they change them. Their own choices (units, range, look,
// size, language, pack) save at once to /me/preferences and follow them to every screen; the Background is
// kept in this browser only. Below the "Server defaults" band: the defaults for everyone, and where addresses
// and map pictures come from (PATCH /settings with its version; admins change them, others read them).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { getUnits, format } from '../shared/units.js';
import { isSupportedLocale, onLocaleChange } from '../shared/i18n.js';
import { formatDate, isValidZone } from '../shared/time.js';
import { icon, hud, plate, tag, note, btn } from '../core/ui.js';
import { band } from '../core/kit.js';
import { field, showApiError } from '../core/form.js';
import { toast } from '../core/layer.js';
import { getPrefs, savePrefs, isAdmin, onPrefs } from '../core/prefs.js';
import { getInterface, onInterface } from '../core/display.js';
import { sizeChoice, followSwitch, lookChoice, skyChoice, sizeLine } from '../core/account.js';
import { getSky, onSky } from '../core/sky.js';
import { packChoices, packLabel } from '../shared/packs.js';
import { chan, hint, recLamp, presets, errText, code } from '../core/setting.js';
import { ago, num } from '../core/fmt.js';
import { MAP_COLORS_CHOICES, MAP_COLORS_TEXT, OFFLINE_ANCHOR, OFFLINE_STEPS, OFFLINE_NOTE, offlineSummary, offlineFileChoices, showOfflineHelp } from '../shared/maptiles.js';

export const id = 'settings-display';
export const css = true;

const LOCALES = [['auto', "This browser's language"], ['en-US', 'English (United States)'], ['en-GB', 'English (United Kingdom)'],
  ['de-DE', 'German number and date style'], ['fr-FR', 'French number and date style'], ['nl-NL', 'Dutch number and date style']]
  .filter(([v]) => v === 'auto' || isSupportedLocale(v));
// used only when the server's list of packs can't be read
const PACKS = [['instrument', 'Instrument'], ['logbook', 'Logbook'], ['horizon', 'Horizon'], ['console', 'Console'], ['pebble', 'Pebble']];
const LANG_RE = /^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$/;
const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
const BROWSER_ZONE = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
const WORD = { km: 'km', mi: 'miles', c: '°C', f: '°F', bar: 'bar', psi: 'psi', rated: 'rated', ideal: 'ideal' };

/**
 * Preset buttons for one choice that save at once: pick(value) returns false when the save failed, and the
 * saved value is pressed again. readOnly: shown, not changeable. Returns { el, set(v) }.
 */
function presetChoice(label, options, current, pick, { readOnly = false, cover } = {}) {
  let shown = String(current);
  const el = h('div', { class: 'seg', role: 'radiogroup', 'aria-label': label, cover, 'aria-disabled': readOnly ? 'true' : null });
  const set = (v) => { shown = String(v); for (const b of el.children) { const on = b.dataset.value === shown; b.setAttribute('aria-pressed', String(on)); b.setAttribute('aria-checked', String(on)); } };
  for (const [v, text] of options) {
    el.append(h('button', { class: 'seg-btn', type: 'button', role: 'radio', disabled: readOnly, dataset: { value: String(v) },
      'aria-pressed': String(String(v) === shown), 'aria-checked': String(String(v) === shown),
      onclick: async () => {
        const before = shown;
        if (String(v) === before) return;
        set(v);
        if ((await pick(v)) === false && shown === String(v)) set(before);
      } }, text));
  }
  return { el, set };
}

export async function mount(ctx) {
  const admin = isAdmin();
  let [about, server, geo, maps, packList] = await Promise.all([
    ctx.api.get('about', null, { maxAgeMs: 600000 }).catch(() => null),
    ctx.api.get('settings').catch(() => null),
    ctx.api.get('geocode/status').catch(() => null),
    ctx.api.get('maps/config').catch(() => null),
    packChoices(ctx.api, { fallback: PACKS, current: getPrefs()?.ui_pack || null }),
  ]);
  if (!ctx.alive()) return;
  const rec = recLamp({ cover: 'web.settings.general.autosave' });
  const mine = async (patch, what) => {
    try { await savePrefs(patch); rec.saved(what); return true; } catch (e) { rec.failed(what, e); return false; }
  };
  const p = getPrefs() || {};
  const u = getUnits();

  /* ---------- the preview cluster: four readings in the units chosen now */
  const preview = h('div', { class: 'sd-cluster', 'aria-live': 'polite' });
  const paintPreview = () => {
    const cell = (label, value, sub) => h('div', { class: 'sd-cell' }, h('span', { class: 'sd-cell-label' }, label), h('span', { class: 'sd-cell-value' }, value), h('span', { class: 'sd-cell-sub' }, sub));
    const n = getUnits();
    replace(preview,
      cell('Speed', format(100, 'speed'), 'the server sends 100 km/h'),
      cell('Range', format(380, 'distance'), `${n.range === 'ideal' ? 'ideal' : 'rated'} range shown`),
      cell('Outside', format(20, 'temperature'), 'sent as 20 °C'),
      cell('Tyre', format(290, 'pressure'), 'sent as 290 kPa'));
  };
  paintPreview();

  /* ---------- your units */
  const dist = presetChoice('Distance and speed', [['km', 'km · km/h'], ['mi', 'miles · mph']], u.distance, (v) => mine({ units: { distance: v } }, 'Distance unit'));
  const temp = presetChoice('Temperature', [['c', '°C'], ['f', '°F']], u.temperature, (v) => mine({ units: { temperature: v } }, 'Temperature unit'));
  const pres = presetChoice('Tyre pressure', [['bar', 'bar'], ['psi', 'psi']], u.pressure, (v) => mine({ units: { pressure: v } }, 'Pressure unit'));
  const rng = presetChoice('Range', [['rated', 'Rated'], ['ideal', 'Ideal']], u.range, (v) => mine({ range: v }, 'Range'));
  const defLine = (k) => h('p', { class: 'chan-hint', dataset: { def: k } });
  const defs = { distance: defLine('distance'), temperature: defLine('temperature'), pressure: defLine('pressure'), range: defLine('range') };
  const paintDefs = () => {
    for (const [k, el] of Object.entries(defs)) {
      const d = k === 'range' ? server?.range : server?.units?.[k];
      replace(el, d ? ['Server default: ', h('b', null, WORD[d] || d), '. Your own choice wins.'] : 'The server default could not be read.');
    }
  };
  paintDefs();

  const unitsPanel = hud({ title: 'Units and range', icon: 'gauge', id: 'sd-units', side: tag('Yours', 'cyan') },
    h('div', { class: 'sd-units' },
      h('div', { class: 'chans' },
        chan({ no: 1, title: 'Distance and speed', text: 'Drives, range, speed, efficiency and the radius of places.', cover: 'web.settings.general.unit-length' }, dist.el, defs.distance),
        chan({ no: 2, title: 'Temperature', text: 'Outside, cabin and battery.', cover: 'web.settings.general.unit-temperature' }, temp.el, defs.temperature),
        chan({ no: 3, title: 'Tyre pressure', text: 'The four tyres and the pressure the car asks for.', cover: 'web.settings.general.unit-pressure' }, pres.el, defs.pressure),
        chan({ no: 4, title: 'Range', text: 'Rated range follows the official test; ideal is what the car calls typical. Energy is measured from the pack either way: this only picks the range figure you see.', cover: 'web.settings.general.preferred-range' }, rng.el, defs.range)),
      h('div', { class: 'sd-side' }, h('p', { class: 'sd-side-head' }, 'Shown now as'), preview,
        note('The server keeps every value in metric. Changing units never changes your data, only how you read it.'))));

  /* ---------- your look */
  const look = lookChoice();
  const size = sizeChoice();
  const follow = followSwitch('sd-follow');
  const pct = h('input', { class: 'input sd-pct', id: 'sd-pct', type: 'number', min: '50', max: '200', step: '1', value: String(getInterface().size), 'aria-label': 'Interface size in percent' });
  const pctErr = h('p', { class: 'field-error', hidden: true });
  pct.addEventListener('change', async () => {
    const v = Math.round(Number(pct.value));
    if (!(v >= 50 && v <= 200)) { pctErr.hidden = false; replace(pctErr, icon('warn', 'i--sm'), h('span', null, 'A size from 50 to 200 %.')); return; }
    pctErr.hidden = true;
    await mine({ interface_size: v }, 'Interface size');
  });
  const sizeNote = h('p', { class: 'chan-hint' }, sizeLine());
  const browserLang = (navigator.languages && navigator.languages[0]) || navigator.language || 'unknown';
  const langSel = h('select', { class: 'input sd-select', 'aria-label': 'Language and number style', onchange: () => mine({ language: langSel.value }, 'Language') },
    LOCALES.map(([v, l]) => h('option', { value: v, selected: (p.language || 'auto') === v || (v === 'en-US' && p.language === 'en') }, v === 'auto' ? `${l} (${browserLang})` : l)));
  const langLine = h('p', { class: 'chan-hint' });
  const paintLang = () => replace(langLine, 'Written now as: ', h('b', null, format(1234.5, 'distance')), ', ', h('b', null, formatDate(ctx.api.now(), ctx.tz, { weekday: false, year: true })), '.');
  paintLang();
  const packNow = p.ui_pack || about?.ui_pack?.name?.toLowerCase() || 'horizon';
  const packHere = about?.ui_pack?.id || about?.ui_pack?.name?.toLowerCase() || 'horizon';
  const packSel = h('select', { class: 'input sd-select', 'aria-label': 'UI pack', onchange: async () => { if (await mine({ ui_pack: packSel.value }, 'UI pack')) paintSwitch(); } },
    packList.items.map((k) => h('option', { value: k.id, selected: packNow === k.id }, packLabel(k))));
  const switchBtn = btn('Switch now', () => location.reload(), { sm: true });
  const switchLine = hint('The new pack is used the next time the app loads.');
  const paintSwitch = () => { const other = packSel.value !== packHere; switchBtn.hidden = !other; switchLine.hidden = !other; };
  paintSwitch();

  const mapColors = presetChoice('Map colours', MAP_COLORS_CHOICES, getPrefs()?.map_colors || 'dimmed', (v) => mine({ map_colors: v }, 'Map colours'));
  const lookPanel = hud({ title: 'Look and language', icon: 'monitor', id: 'sd-look', side: tag('Follows you', 'cyan') },
    h('div', { class: 'chans' },
      chan({ no: 5, title: 'Look', text: 'Dusk is the dark look, Daybreak the light one; Device follows this device. Saved with your account, and set before the page is drawn, so there is no flash.', cover: ['shell.theme', 'web.settings.general.theme', 'shots.theme-note'] },
        look.el, hint('Every page, chart and map has both looks; pictures in the help may show either.')),
      chan({ title: 'Map colours', text: MAP_COLORS_TEXT }, mapColors.el),
      chan({ no: 6, title: 'Interface size', text: 'Text, controls, tables, charts and maps grow or shrink together. Follow screen size adds a step on a very large screen; browser zoom works on top of both.' },
        size.el,
        h('div', { class: 'row' }, h('label', { class: 'faint', for: 'sd-pct' }, 'Any size'), pct, h('span', { class: 'faint' }, '%')), pctErr,
        h('div', { class: 'tog' }, follow.el, h('label', { class: 'tog-label', for: 'sd-follow' }, 'Follow screen size')), sizeNote),
      chan({ no: 7, title: 'Language', text: 'The words are English for now; this sets how numbers and dates are written. "This browser\'s language" follows what your browser asks for, and the page tells the browser its language.', cover: ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation'] },
        langSel, langLine),
      chan({ no: 8, title: 'UI pack', text: `The design of the whole app. ${about?.ui_pack?.name ? `You are in ${about.ui_pack.name} ${about.ui_pack.version || ''}.` : ''} A new choice is used the next time the app loads.` },
        h('div', { class: 'row' }, packSel, switchBtn), switchLine)));

  /* ---------- the background (this browser only) */
  const sky = skyChoice();
  const skyNote = h('p', { class: 'chan-hint' });
  const paintSky = (k) => replace(skyNote, k.mode === 'still' ? 'Your device asks for less motion, so the grid stands still.'
    : k.choice === 'auto' ? `Auto is ${k.mode === 'calm' ? 'Calm' : k.mode === 'off' ? 'Off' : 'Full'} on this screen.` : '', ' Kept in this browser only; other screens keep their own.');
  paintSky(getSky());
  const skyPanel = hud({ title: 'Background', icon: 'horizon', id: 'sd-sky', side: tag('This browser', 'org') },
    h('div', { class: 'chans' },
      chan({ no: 9, title: 'Sky and grid', text: 'Full: the sun, the stars and the grid running to the horizon, at most 30 frames a second, stopped while the tab is hidden. Calm: the same picture standing still. Off: a plain dark sky. Auto picks Calm on phones and small screens.' },
        sky.el, skyNote)));

  ctx.onCleanup(onInterface((i) => { size.sync(i); follow.sync(i); pct.value = String(i.size); sizeNote.textContent = sizeLine(); }));
  ctx.onCleanup(onSky((k) => { sky.sync(k); paintSky(k); }));
  ctx.onCleanup(onPrefs((pp) => {
    const n = getUnits();
    dist.set(n.distance); temp.set(n.temperature); pres.set(n.pressure); rng.set(n.range);
    look.sync(pp);
    mapColors.set(pp?.map_colors || 'dimmed');
    langSel.value = pp?.language === 'en' ? 'en-US' : (pp?.language || 'auto');
    if (pp?.ui_pack) { packSel.value = pp.ui_pack; paintSwitch(); }
    paintPreview(); paintLang();
  }));
  ctx.onCleanup(onLocaleChange(() => { paintPreview(); paintLang(); }));

  /* ---------- server defaults, addresses and maps */
  const serverSlot = h('div', { class: 'stack' });
  function geoLine() {
    if (!geo) return hint('The address status could not be read.');
    const od = geo.offline_data || {};
    const bits = [od.present === false ? 'Offline place names: not installed.' : `Offline place names: ${od.name || 'bundled'}${od.places ? `, ${num(od.places)} places` : ''}${od.built_ms ? `, built ${ago(od.built_ms)}` : ''}.`,
      ` ${num(geo.cache_size ?? 0)} addresses already looked up and kept.`];
    if (geo.online?.last_error) bits.push(` Last online error: ${geo.online.last_error}.`);
    const r = geo.online?.retry_ms;
    if (r != null && Number.isFinite(Number(r))) {
      const at = Number(r) > 1e12 ? Number(r) : ctx.api.now() + Number(r);
      bits.push(at <= ctx.api.now() ? ' Next try: with the next address needed.' : ` Next try ${ago(at)}.`);
    }
    return h('p', { class: 'chan-hint' }, bits.join(''), ' ', h('a', { class: 'plc', href: state.buildUrl('health') }, 'Address status in Data health'));
  }
  function tileLine(t) {
    if (t.mode === 'online') return hint('Each viewer\'s browser asks the online source for map pictures; its credit shows on every map: ', h('b', null, t.attribution || '(none set)'), '.');
    return hint(maps?.tiles?.offline_data_present ? 'An offline map file is installed: maps need nothing from outside.' : 'No offline map file yet: maps draw routes and places on a plain background, with a note saying so. See Offline map file below.');
  }
  function paintServer() {
    if (!server) { replace(serverSlot, hud({ title: 'Server defaults', icon: 'server' }, note('The server settings could not be read. Your own choices above still work.'))); return; }
    const ro = !admin;
    const save = async (body, what, fields = []) => {
      try {
        server = await ctx.api.patch('settings', { version: server.version, ...body });
        rec.saved(what);
        for (const f of fields) f.setError(null);
        [geo, maps] = await Promise.all([ctx.api.get('geocode/status').catch(() => geo), ctx.api.get('maps/config').catch(() => maps)]);
        if (ctx.alive()) { paintServer(); paintDefs(); }
        return true;
      } catch (e) {
        if (e?.name === 'AbortError') return false;
        if (e.status === 409) {
          server = await ctx.api.get('settings').catch(() => server);
          if (ctx.alive()) { paintServer(); paintDefs(); }
          toast('Someone else changed the settings; this page shows theirs now.', { kind: 'info' });
          return false;
        }
        if (fields.length && e.fields) showApiError({ fields, setMessage: (t) => t && toast(t, { kind: 'error' }) }, e); else toast(`Could not save ${what.toLowerCase()}: ${errText(e)}`, { kind: 'error' });
        return false;
      }
    };
    const pc = (label, opts, cur, pick, cover) => presetChoice(label, opts, cur, pick, { readOnly: ro, cover }).el;
    const g = server.geocode || {}, t = server.tiles || {};
    const gUrl = field({ label: 'Address service (https)', name: 'sd-gurl', path: 'geocode.url', value: g.url || '', hint: 'Used only when lookup is online. Positions are rounded first; the service never gets a VIN or a car name.', attrs: { placeholder: 'https://', disabled: ro, spellcheck: 'false' } });
    const gRound = field({ label: 'Round positions to (m)', name: 'sd-grnd', path: 'geocode.rounding_m', type: 'number', value: g.rounding_m, hint: 'Metres, 0 to 1000, before an online lookup.', attrs: { min: '0', max: '1000', step: '1', disabled: ro } });
    const gLang = field({ label: 'Address language', name: 'sd-glang', path: 'geocode.language', value: g.language || 'en', hint: 'A code such as en, de, fr or pt-BR. A change looks every stored address up again.', attrs: { maxlength: '10', spellcheck: 'false', autocomplete: 'off', disabled: ro } });
    const tz = field({ label: 'Time zone for new cars', name: 'sd-tz', path: 'default_time_zone', value: server.default_time_zone || 'UTC', hint: 'A zone name such as America/Chicago. Type to search the list.', attrs: { list: 'sd-zones', disabled: ro, autocomplete: 'off', spellcheck: 'false' } });
    const tilePresets = maps?.tiles?.presets || [];
    const tilePreset = tilePresets.find((p) => p.url === t.online_url) || null;
    const tSource = tilePresets.length ? pc('Tile source', [...tilePresets.map((p) => [p.id, p.name]), ['custom', 'Custom address']], tilePreset ? tilePreset.id : 'custom', (v) => {
      const p = tilePresets.find((x) => x.id === v);
      if (p) save({ tiles: { online_url: p.url, attribution: p.attribution, max_zoom: p.max_zoom } }, 'Tile source', [tUrl, tAttr]);
      else tUrl.input.focus();
    }) : null;
    const tTerms = tilePreset ? hint(`${tilePreset.name}: free, no key. Its terms (${tilePreset.terms.replace(/^https:\/\//, '')}) ask for light use and its credit on every map.`) : null;
    const tUrl = field({ label: 'Online tile address (https)', name: 'sd-turl', path: 'tiles.online_url', value: t.online_url || '', hint: 'With {z}, {x} and {y} in it.', attrs: { placeholder: 'https://', disabled: ro, spellcheck: 'false' } });
    const tAttr = field({ label: 'Map credit', name: 'sd-tattr', path: 'tiles.attribution', value: t.attribution || '', hint: 'Shown in the corner of every map, as the source asks.', attrs: { disabled: ro } });
    const local = {
      'sd-grnd': (v) => (v === '' || !(Number(v) >= 0 && Number(v) <= 1000) ? 'A whole number of metres from 0 to 1000.' : null),
      'sd-glang': (v) => (!LANG_RE.test(v.trim()) ? 'Not a language code: two or three letters such as en or de, with a region if you like, such as pt-BR.' : null),
      'sd-tz': (v) => (!isValidZone(v.trim()) ? 'Not a time zone this browser knows: pick one from the list, such as America/Chicago.' : null),
      'sd-turl': (v) => (v && !/^https:\/\/.+/.test(v) ? 'An https:// address, or empty.' : null),
      'sd-gurl': (v) => (v && !/^https:\/\/.+/.test(v) ? 'An https:// address, or empty.' : null),
    };
    const words = { 'sd-tz': 'Time zone for new cars', 'sd-gurl': 'Address service', 'sd-grnd': 'Rounding', 'sd-glang': 'Address language', 'sd-turl': 'Tile address', 'sd-tattr': 'Map credit' };
    for (const [f, mk] of [[gUrl, (v) => ({ geocode: { url: v || null } })], [gRound, (v) => ({ geocode: { rounding_m: Math.round(Number(v)) } })],
      [gLang, (v) => ({ geocode: { language: v.trim() } })], [tz, (v) => ({ default_time_zone: v.trim() })], [tUrl, (v) => ({ tiles: { online_url: v || null } })], [tAttr, (v) => ({ tiles: { attribution: v } })]]) {
      if (ro) continue;
      f.input.addEventListener('change', () => {
        const bad = local[f.name]?.(f.input.value);
        if (bad) { f.setError(bad); return; }
        save(mk(f.input.value), words[f.name], [f]);
      });
    }
    replace(serverSlot,
      h('div', { cover: 'web.settings.general.divider', role: 'separator', 'aria-label': 'Server defaults' },
        band(2, 'Server defaults', h('span', { class: 'faint small' }, admin ? 'For everyone. A person\'s own choice above wins.' : 'Set by an admin, shown here to read. Your own choices win.'))),
      h('div', { class: 'cols' },
        hud({ title: 'Defaults for everyone', icon: 'server', id: 'sd-defaults', side: ro ? tag('Read only', 'org') : tag('Admin', 'mag') },
          h('div', { class: 'chans' },
            chan({ no: 10, title: 'Units', text: 'What an account sees until it picks its own. A change updates everyone who has not chosen.' },
              h('div', { class: 'row' },
                pc('Default distance', [['km', 'km'], ['mi', 'mi']], server.units?.distance, (v) => save({ units: { distance: v } }, 'Default distance unit')),
                pc('Default temperature', [['c', '°C'], ['f', '°F']], server.units?.temperature, (v) => save({ units: { temperature: v } }, 'Default temperature unit')),
                pc('Default pressure', [['bar', 'bar'], ['psi', 'psi']], server.units?.pressure, (v) => save({ units: { pressure: v } }, 'Default pressure unit')))),
            chan({ no: 11, title: 'Range', text: 'Rated or ideal, for accounts that have not chosen.' },
              pc('Default range', [['rated', 'Rated'], ['ideal', 'Ideal']], server.range, (v) => save({ range: v }, 'Default range')),
              hint('A new server starts from the distance unit set on the car\'s own screen, once its CAN Bridge has sent it.')),
            chan({ no: 12, title: 'Time zone for new cars', text: 'The zone a new car starts in: its days, weeks and prices by time of day. Each car keeps its own after that.' },
              tz.el, h('datalist', { id: 'sd-zones' }, ZONES.map((x) => h('option', { value: x }))),
              BROWSER_ZONE && BROWSER_ZONE !== server.default_time_zone && !ro ? [btn('Use this browser\'s zone', () => save({ default_time_zone: BROWSER_ZONE }, 'Time zone for new cars', [tz]), { icon: 'globe', sm: true, label: `Use this browser's zone, ${BROWSER_ZONE}` }), hint(`This browser is in ${BROWSER_ZONE}.`)] : null,
              hint('Each car\'s own zone: ', h('a', { class: 'plc', href: state.buildUrl('settings-cars') }, 'Settings: Cars'), '. The clock in the page header names the zone shown.')),
            chan({ no: 16, title: 'Outside links', text: 'Links that open another site in a new tab, only when someone clicks them.' },
              pc('Map links', [['false', 'No map links'], ['true', 'Map links']], String(!!server.outside_links?.maps), (v) => save({ outside_links: { maps: v === 'true' } }, 'Map links')),
              pc('Release notes', [['false', 'No release notes'], ['true', 'Release notes']], String(!!server.outside_links?.release_notes), (v) => save({ outside_links: { release_notes: v === 'true' } }, 'Release notes links')))),
          h('p', { class: 'note sd-metric' }, icon('info', 'i--sm'), h('span', null, 'Server values are metric (', code('100 km'), ', ', code('20 °C'), ', ', code('290 kPa'), '); every page converts them for each person.'))),
        hud({ title: 'Addresses and maps', icon: 'globe', id: 'sd-maps', side: tag('Private by default', 'grn') },
          h('div', { class: 'chans' },
            chan({ no: 13, title: 'Address lookup', text: 'Offline uses the place names that come with Sentry Mate: nothing leaves the server. Online asks the service you name (your own address server, or a proxy in front of one) with rounded positions, and keeps a cache.', cover: ['web.settings.general.address-lookup', 'docs.env.tesla-api.nominatim-proxy'] },
              pc('Address lookup', [['offline', 'Offline'], ['online', 'Online service']], g.mode, (v) => save({ geocode: { mode: v } }, 'Address lookup', [gUrl])), geoLine(), gUrl.el, gRound.el),
            chan({ no: 14, title: 'Address language', text: 'Street and place names in this language where the data has it; otherwise in the local language.', cover: ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid'] }, gLang.el),
            chan({ no: 15, title: 'Map tiles', text: 'Offline tiles come from Sentry Mate itself. An online source is asked by each viewer\'s browser, so it sees the map area and this server\'s address (as Referer, which OpenStreetMap needs; never a page or a car): turn it on only if you accept that.', cover: 'web.shell.map-tiles' },
              pc('Map tiles', [['offline', 'Offline'], ['online', 'Online source']], t.mode, (v) => save({ tiles: { mode: v } }, 'Map tiles', [tUrl])), tileLine(t), tSource, tTerms, tUrl.el, tAttr.el),
            chan({ title: 'Offline map file', id: OFFLINE_ANCHOR, text: 'A map of your own area kept on this server: maps then need nothing from outside, and no viewer\'s browser asks another site for tiles.' },
              hint(offlineSummary(maps?.tiles)),
              offlineFileChoices(maps?.tiles) ? pc('Offline map file', offlineFileChoices(maps?.tiles), t.offline_file || '', (v) => save({ tiles: { offline_file: v || null } }, 'Offline map file')) : null,
              h('ol', { class: 'chan-hint offline-steps' }, OFFLINE_STEPS.map((x) => h('li', null, x))), hint(OFFLINE_NOTE))))));
  }
  paintServer();

  replace(ctx.view, h('div', { class: 'page sd' },
    plate({ kicker: 'Set-up · display', title: 'Display and units', aim: ctx.info.aim, tools: rec.el }),
    h('section', { class: 'hud hud--plain jump', 'aria-label': 'Sections and saving' },
      presets([['sd-units', 'Units'], ['sd-look', 'Look'], ['sd-sky', 'Background'], ['sd-defaults', 'Defaults'], ['sd-maps', 'Maps']]),
      h('p', { class: 'note jump-note', cover: 'web.settings.general.autosave' }, icon('check', 'i--sm'),
        h('span', null, 'Every change saves at once; there is no Save button. The REC lamp lights and a short note confirms it.'))),
    band(1, 'Yours'),
    unitsPanel,
    lookPanel, skyPanel,
    serverSlot));
  showOfflineHelp(document.getElementById(OFFLINE_ANCHOR));
  ctx.ready();
}
