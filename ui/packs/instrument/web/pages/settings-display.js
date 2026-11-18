// Settings: Display and units (/settings/display): how numbers, words and maps look. The person's own
// choices (PATCH /api/v1/me/preferences) save at once and apply at once; below them, the server's
// defaults and the address and map sources, which an admin changes (PATCH /api/v1/settings).

import { h, replace } from '../shared/dom.js';
import { getUnits, format } from '../shared/units.js';
import { isSupportedLocale } from '../shared/i18n.js';
import { buildUrl } from '../shared/state.js';
import { icon } from '../core/icons.js';
import { pageHead, panel } from '../core/page.js';
import { field, showApiError } from '../core/form.js';
import { getPrefs, savePrefs, isAdmin, onPrefs } from '../core/prefs.js';
import { getInterface, onInterface } from '../core/display.js';
import { sizePicker, followSwitch, sizeNote, themeButtons } from '../core/account.js';
import { flash } from '../core/flash.js';
import { ago } from '../core/fmt.js';

export const id = 'settings-display';
export const css = true;

const LOCALES = [['auto', "This browser's language"], ['en-US', 'English (United States)'], ['en-GB', 'English (United Kingdom)'], ['de-DE', 'German number and date style'], ['fr-FR', 'French number and date style'], ['nl-NL', 'Dutch number and date style']].filter(([v]) => v === 'auto' || isSupportedLocale(v));

// A language code as address data names it: en, de, pt-BR, zh-Hant.
const LANG_RE = /^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$/;

const UNIT_WORDS = { km: 'km', mi: 'miles', c: '°C', f: '°F', bar: 'bar', psi: 'psi', rated: 'rated', ideal: 'ideal' };

// a row: the label and its explanation on the left, the control on the right
const setting = (title, text, control, cover = null) => h('div', { class: 'setting', cover }, h('div', null, h('h3', null, title), h('p', null, text)), h('div', { class: 'setting-control' }, control));

// buttons for one choice; onPick(value) saves it. seg.set(value) shows another value (a change elsewhere).
function choice(name, options, current, onPick) {
  const mark = (v) => { for (const b of seg.children) { const on = b.dataset.v === String(v); b.setAttribute('aria-pressed', String(on)); b.setAttribute('aria-checked', String(on)); } };
  const seg = h('div', { class: 'seg', role: 'radiogroup', 'aria-label': name },
    options.map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', role: 'radio', 'aria-checked': String(String(v) === String(current)), 'aria-pressed': String(String(v) === String(current)), dataset: { v: String(v) },
      onclick: async () => { mark(v); await onPick(v); } }, l)));
  seg.set = mark;
  return seg;
}

async function savePref(patch, what) {
  try { await savePrefs(patch); flash(`${what} saved.`); return true; } catch (e) { flash(`Could not save ${what.toLowerCase()}: ${e.message}`, { kind: 'error' }); return false; }
}

export async function mount(ctx) {
  const admin = isAdmin();
  let about = null, server = null, geo = null, maps = null;
  [about, server, geo, maps] = await Promise.all([
    ctx.api.get('about', null, { maxAgeMs: 600000 }).catch(() => null),
    ctx.api.get('settings').catch(() => null),
    ctx.api.get('geocode/status').catch(() => null),
    ctx.api.get('maps/config').catch(() => null),
  ]);
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
  ctx.onCleanup(onInterface((i) => { size.sync(i); follow.sync(i); pctIn.value = String(i.size); noteEl.textContent = sizeNote(); }));
  const theme = themeButtons();
  ctx.onCleanup(onPrefs((pp) => theme.sync(pp)));
  const browserLang = (navigator.languages && navigator.languages[0]) || navigator.language || 'unknown';
  const lang = h('select', { class: 'select', 'aria-label': 'Language and number style', onchange: (e) => savePref({ language: e.target.value }, 'Language') },
    LOCALES.map(([v, l]) => h('option', { value: v, selected: (p.language || 'auto') === v || (v === 'en-US' && p.language === 'en') }, v === 'auto' ? `${l} (${browserLang})` : l)));
  const packs = [['instrument', 'Instrument'], ['logbook', 'Logbook']];
  const packSel = h('select', { class: 'select', 'aria-label': 'UI pack', onchange: (e) => savePref({ ui_pack: e.target.value }, 'UI pack') },
    packs.map(([v, l]) => h('option', { value: v, selected: (p.ui_pack || 'instrument') === v }, l)));
  const look = panel({ title: 'Look and language', icon: 'monitor', side: h('span', { class: 'faint' }, 'Yours; follows you to every screen') },
    setting('Theme', 'Light, dark, or follow this device. It is saved with your account, so every browser you sign in on uses it, and it is set before the page is drawn, so there is no flash.',
      [theme.el, h('p', { class: 'setting-note' }, 'Every page, chart and map has both a light and a dark look; screenshots in the help show either.')], ['shell.theme', 'web.settings.general.theme', 'shots.theme-note']),
    setting('Interface size', 'Makes text, controls, tables, charts and maps bigger or smaller together. With Follow screen size on, a very large screen (a native 4K monitor) adds its own step on top. Browser zoom works on top of both.',
      [size.el, h('div', { class: 'row' }, h('label', { class: 'faint', for: 'set-size-pct' }, 'Any size:'), pctIn, h('span', { class: 'faint' }, '%')), sizeErr,
        h('div', { class: 'switch-row switch-row--inline' }, follow.el, h('label', { for: 'set-follow' }, 'Follow screen size')), noteEl]),
    setting('Language', 'The text is in English for now. This sets how numbers and dates are written. "This browser\'s language" follows the language your browser asks for; the app tells the browser which language the page is in.',
      [lang, h('p', { class: 'setting-note' }, 'Example: ', h('b', null, format(1234.5, 'distance')), ', ', h('b', null, new Intl.DateTimeFormat(document.documentElement.lang || undefined, { dateStyle: 'medium', timeZone: ctx.tz }).format(ctx.api.now())), '.')],
      ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation']),
    setting('UI pack', `The design of the whole app. ${about?.ui_pack?.name ? `This page is the ${about.ui_pack.name} pack ${about.ui_pack.version || ''}.` : ''} Your choice is used the next time the app loads where the server offers that pack.`, packSel));

  /* ---------- server defaults, addresses and maps */
  const serverPanel = h('div', { class: 'settings-server' });
  function geoStatus() {
    if (!geo) return h('p', { class: 'setting-note' }, 'Address status could not be read.');
    const od = geo.offline_data || {};
    const bits = [];
    bits.push(od.present === false ? 'Offline place names: not installed.' : `Offline place names: ${od.name || 'bundled'}${od.places ? `, ${od.places.toLocaleString()} places` : ''}${od.built_ms ? `, built ${ago(od.built_ms)}` : ''}.`);
    bits.push(` Cache: ${(geo.cache_size ?? 0).toLocaleString()} addresses already looked up.`);
    if (geo.online?.last_error) bits.push(` Last online error: ${geo.online.last_error}.`);
    return h('p', { class: 'setting-note' }, icon(geo.online?.last_error ? 'warn' : 'check', 'ico--sm'), ' ', bits.join(''), ' ', h('a', { href: buildUrl('health'), class: 'panel-link' }, 'Address status in Data health ›'));
  }
  function tileStatus(t) {
    const off = maps?.tiles?.offline_data_present;
    if (t.mode === 'online') return h('p', { class: 'setting-note' }, icon('globe', 'ico--sm'), ' Maps load pictures from the online source; its credit shows on every map: ', h('b', null, t.attribution || '(none set)'), '.');
    return h('p', { class: 'setting-note' }, icon(off ? 'check' : 'info', 'ico--sm'), off ? ' Offline tiles are installed: maps need nothing from outside.' : ' No offline tiles are installed yet: maps draw routes and places on a plain background, with a note saying so.');
  }
  function paintServer() {
    if (!server) { replace(serverPanel, panel({ title: 'Server defaults', icon: 'server' }, h('p', { class: 'note' }, 'The server settings could not be loaded. Your own choices above still work.'))); return; }
    const ro = !admin;
    const patch = async (body, what, fields = []) => {
      try {
        server = await ctx.api.patch('settings', { version: server.version, ...body });
        flash(`${what} saved.`);
        for (const f of fields) f.setError(null);
        [geo, maps] = await Promise.all([ctx.api.get('geocode/status').catch(() => geo), ctx.api.get('maps/config').catch(() => maps)]);
        if (!ctx.alive()) return;
        paintServer(); paintDefaults();
      } catch (e) {
        if (e?.name === 'AbortError') return;
        if (e.status === 409) { server = await ctx.api.get('settings'); paintServer(); paintDefaults(); flash('Someone else changed the settings; this page shows theirs now.', { kind: 'info' }); return; }
        if (fields.length) showApiError({ fields, setMessage: (t) => t && flash(t, { kind: 'error' }) }, e); else flash(`Could not save: ${e.message}`, { kind: 'error' });
      }
    };
    const disabled = (el) => { if (ro) el.setAttribute('disabled', ''); return el; };
    const g = server.geocode || {}, t = server.tiles || {};
    const gUrl = field({ label: 'Address service (https)', name: 'gurl', path: 'geocode.url', value: g.url || '', hint: 'Only used when lookup is online. Positions are rounded first (below); the service never gets a VIN or a car name.', attrs: { placeholder: 'https://', disabled: ro } });
    const gRound = field({ label: 'Round positions to (m)', name: 'grnd', path: 'geocode.rounding_m', type: 'number', value: g.rounding_m, hint: 'metres, from 0 to 1000, before an online lookup', attrs: { min: '0', max: '1000', step: '1', disabled: ro } });
    const gLang = field({ label: 'Address language', name: 'glang', path: 'geocode.language', value: g.language || 'en', hint: 'A code such as en, de, fr or pt-BR. Changing it looks every stored address up again.', attrs: { maxlength: '10', spellcheck: 'false', autocomplete: 'off', disabled: ro } });
    const tUrl = field({ label: 'Online tile address (https)', name: 'turl', path: 'tiles.online_url', value: t.online_url || '', hint: 'With {z}, {x} and {y} in it.', attrs: { placeholder: 'https://', disabled: ro } });
    const tAttr = field({ label: 'Map credit', name: 'tattr', path: 'tiles.attribution', value: t.attribution || '', hint: 'Shown in the corner of every map, as the source asks.', attrs: { disabled: ro } });
    // checks here first (worded plainly), then the server's own checks under the same field
    const local = {
      grnd: (v) => (v === '' || !(Number(v) >= 0 && Number(v) <= 1000) ? 'A whole number of metres from 0 to 1000.' : null),
      glang: (v) => (!LANG_RE.test(v.trim()) ? 'Not a language code. Use two or three letters, such as en or de, optionally with a region: pt-BR.' : null),
      turl: (v) => (v && !/^https:\/\/.+/.test(v) ? 'An https:// address, or empty.' : null),
      gurl: (v) => (v && !/^https:\/\/.+/.test(v) ? 'An https:// address, or empty.' : null),
    };
    for (const [f, mk] of [[gUrl, (v) => ({ geocode: { url: v || null } })], [gRound, (v) => ({ geocode: { rounding_m: Math.round(Number(v)) } })], [gLang, (v) => ({ geocode: { language: v.trim() } })], [tUrl, (v) => ({ tiles: { online_url: v || null } })], [tAttr, (v) => ({ tiles: { attribution: v } })]]) {
      f.input.addEventListener('change', () => {
        const bad = local[f.name]?.(f.input.value);
        if (bad) { f.setError(bad); return; }
        patch(mk(f.input.value), f.el.querySelector('label').textContent, [f]);
      });
    }
    replace(serverPanel,
      h('div', { class: 'settings-divider', role: 'separator', cover: 'web.settings.general.divider' }, h('span', null, 'Server defaults'), h('span', { class: 'faint' }, admin ? 'For everyone; you change them as an admin. A person’s own choice above wins over these.' : 'Set by an admin; shown here to read. Your own choices above win over these.')),
      h('div', { class: 'settings-pair' },
        panel({ title: 'Defaults for everyone', icon: 'server', side: ro ? h('span', { class: 'faint' }, 'Read only') : null },
          setting('Units', 'What an account sees until it picks its own units. Changing these updates everyone who has not chosen.',
            h('div', { class: 'row' },
              disabled(choice('Default distance', [['km', 'km'], ['mi', 'mi']], server.units?.distance, (v) => patch({ units: { distance: v } }, 'Default distance unit'))),
              disabled(choice('Default temperature', [['c', '°C'], ['f', '°F']], server.units?.temperature, (v) => patch({ units: { temperature: v } }, 'Default temperature unit'))),
              disabled(choice('Default pressure', [['bar', 'bar'], ['psi', 'psi']], server.units?.pressure, (v) => patch({ units: { pressure: v } }, 'Default pressure unit'))))),
          setting('Range', 'Rated or ideal, for accounts that have not chosen.', disabled(choice('Default range', [['rated', 'Rated'], ['ideal', 'Ideal']], server.range, (v) => patch({ range: v }, 'Default range')))),
          h('p', { class: 'setting-note' }, 'A fresh server starts from the distance unit set on the car’s own screen, once its CAN Bridge has sent it.')),
        panel({ title: 'Addresses and maps', icon: 'globe', side: h('span', { class: 'faint' }, 'Private by default') },
          setting('Address lookup', 'Offline uses the place names bundled with Sentry Mate: nothing leaves the server. Online asks the service you name (for example your own address server or a proxy in front of one), with rounded positions, and keeps a cache.',
            [disabled(choice('Address lookup', [['offline', 'Offline'], ['online', 'Online service']], g.mode, (v) => patch({ geocode: { mode: v } }, 'Address lookup', [gUrl]))),
              geoStatus(), gUrl.el, gRound.el], ['web.settings.general.address-lookup', 'docs.env.tesla-api.nominatim-proxy']),
          setting('Address language', 'Street and place names are written in this language where the data has it; otherwise in the local language.', gLang.el, ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid']),
          setting('Map tiles', 'Offline tiles come from Sentry Mate itself. An online source is asked by each viewer’s browser for the map pictures, so it sees the map area; only turn it on if you accept that.',
            [disabled(choice('Map tiles', [['offline', 'Offline'], ['online', 'Online source']], t.mode, (v) => patch({ tiles: { mode: v } }, 'Map tiles', [tUrl]))), tileStatus(t), tUrl.el, tAttr.el], 'web.shell.map-tiles'),
          setting('Outside links', 'Links that open another site in a new tab, only when the viewer clicks them.',
            h('div', { class: 'row' },
              disabled(choice('Map links', [['false', 'No map links'], ['true', 'Map links']], String(!!server.outside_links?.maps), (v) => patch({ outside_links: { maps: v === 'true' } }, 'Map links'))),
              disabled(choice('Release notes', [['false', 'No release notes'], ['true', 'Release notes']], String(!!server.outside_links?.release_notes), (v) => patch({ outside_links: { release_notes: v === 'true' } }, 'Release-notes links'))))))));
  }
  paintServer();

  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'settings-stack' },
      h('p', { class: 'note saving-note', cover: 'web.settings.general.autosave' }, icon('check', 'ico--sm'), ' Every change here is saved at once; there is no Save button. A short note confirms it, and your choices follow you to every browser you sign in on.'),
      h('div', { class: 'settings-pair' }, units, look), serverPanel));
  ctx.ready();
}
