// Sentry Mate, "Logbook" UI pack: Settings: Display and units (/settings/display).
//
// Your own choices (GET / PATCH /api/v1/me/preferences): units, rated or ideal range, theme, language,
// interface size and following the screen size. Every change saves at once and applies on every page, with
// an example in the chosen unit or language next to the control.
// Below the divider, the server's defaults and its address and map settings (GET / PATCH /api/v1/settings,
// admins only to change), with the address lookup's status (GET /api/v1/geocode/status).

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { getLocale, isSupportedLocale } from '../shared/i18n.js';
import { format } from '../shared/units.js';
import { formatDateTime } from '../shared/time.js';
import { pageHead, errorView, pageLink, flag } from '../core/ui.js';
import { choice, switchField, field, form, setFieldError } from '../core/form.js';
import { currentPrefs, onPrefs } from '../core/prefs.js';
import { savePref, currentAbout } from '../core/frame.js';
import { SIZES, sizeState, setSize, sizeNote, display } from '../core/display.js';

export const id = 'settings-display';
export const css = true;
export const title = () => 'Display and units';

// Locales offered for dates and numbers, besides the browser's own languages. Names come from the browser.
const LOCALES = ['en-US', 'en-GB', 'en-CA', 'en-AU', 'de-DE', 'de-CH', 'fr-FR', 'fr-CA', 'nl-NL', 'nb-NO', 'sv-SE', 'da-DK',
  'fi-FI', 'es-ES', 'it-IT', 'pt-PT', 'pl-PL', 'cs-CZ', 'ja-JP', 'ko-KR', 'zh-CN'];
// Address languages suggested in the address language field (any language code is allowed).
const ADDRESS_LANGS = ['en', 'de', 'fr', 'nl', 'nb', 'sv', 'da', 'fi', 'es', 'it', 'pt', 'pl', 'cs', 'ja', 'ko', 'zh'];

function langName(tag) {
  try {
    const n = new Intl.DisplayNames(['en'], { type: 'language' }).of(tag);
    return n && n !== tag ? n : null;
  } catch { return null; }
}

/** The address language as typed: a canonical code, or an error in plain words. */
function checkLangCode(raw) {
  const v = String(raw || '').trim();
  if (!v) return { error: 'Type a language code such as en, de or fr.' };
  if (!/^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$/.test(v) || !isSupportedLocale(v)) {
    return { error: `“${v}” is not a language code. Use a code such as en, de or fr.` };
  }
  return { code: Intl.getCanonicalLocales(v)[0] };
}

function setting(name, desc, control, { cover = null, cls = null, extra = null } = {}) {
  return h('div', { class: ['setting', cls], cover },
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
  const admin = ctx.me && ctx.me.role === 'admin';
  const [server, geoFirst] = await Promise.all([
    ctx.api.get('settings'),
    ctx.api.get('geocode/status').catch(() => null),
  ]);
  let srv = server;
  let geo = geoFirst;
  const p = currentPrefs() || {};
  const save = (patch) => savePref(patch, { quiet: false }).catch(() => {});

  // ------------------------------------------------------------ your choices
  const units = p.units || {};
  const dist = choice({ label: 'Distance', labelHidden: true, name: 'distance', value: units.distance || 'km', options: [['km', 'Kilometres'], ['mi', 'Miles']], onchange: (v) => save({ units: { distance: v } }) });
  const temp = choice({ label: 'Temperature', labelHidden: true, name: 'temperature', value: units.temperature || 'c', options: [['c', 'Celsius (°C)'], ['f', 'Fahrenheit (°F)']], onchange: (v) => save({ units: { temperature: v } }) });
  const press = choice({ label: 'Tyre pressure', labelHidden: true, name: 'pressure', value: units.pressure || 'bar', options: [['bar', 'bar'], ['psi', 'psi']], onchange: (v) => save({ units: { pressure: v } }) });
  const range = choice({ label: 'Range', labelHidden: true, name: 'range', value: p.range || 'rated', options: [['rated', 'Rated'], ['ideal', 'Ideal']], onchange: (v) => save({ range: v }) });
  const theme = choice({ label: 'Theme', labelHidden: true, name: 'theme', value: p.theme || 'device', options: [['device', 'Follow device'], ['light', 'Light (paper)'], ['dark', 'Dark (night ink)']], onchange: (v) => save({ theme: v }) });

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
    range.set(pr.range || 'rated'); theme.set(pr.theme || 'device');
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
  };
  ctx.onCleanup(onPrefs(sync));
  ctx.onCleanup(display.onChange(sync));
  const mq = matchMedia('(prefers-color-scheme: dark)');
  mq.addEventListener('change', sync);
  ctx.onCleanup(() => mq.removeEventListener('change', sync));
  fillLang();

  const about = currentAbout();
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
      setting('Language', 'How dates, times and numbers are written (for example 1,234.5 or 1.234,5, and the first day of the week).', h('div', { class: 'lang-control' }, lang, langNote),
        { cover: ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation'], extra: langEx }),
      setting('Interface size', 'Text, controls, tables, charts and maps grow or shrink together. With “Follow screen size” on, big screens such as 4K monitors get larger text by themselves. Browser zoom works on top.', h('div', { class: 'size-control' }, size, follow, sizeNoteEl), { cls: 'setting-size' }),
      setting('UI pack', 'The look of the whole app. Admins install other packs.', h('p', { class: 'setting-value' }, about && about.ui_pack ? `${about.ui_pack.name} ${about.ui_pack.version}` : 'Logbook'))));

  // ------------------------------------------------------------ the divider
  const divider = h('div', { class: 'settings-divider', role: 'separator', cover: 'web.settings.general.divider' },
    h('span', null, admin ? 'Below: for everyone on this server' : 'Below: set by an admin for everyone'));
  const dividerNote = h('p', { class: 'settings-divider-note' }, admin
    ? 'Your own choices above win over these for you. Changing a default changes it for everyone who has not made their own choice.'
    : 'Your own choices above win over these. Only an admin can change what is below.');

  // ------------------------------------------------------------ server settings
  const serverBox = h('div', { class: 'settings-server' });

  async function refreshGeo() {
    try { geo = await ctx.api.get('geocode/status'); } catch { /* keep the last one */ }
  }

  function drawServer(s) {
    srv = s;
    const disabled = !admin;
    const failed = (err) => {
      ctx.flash(`Not saved: ${err.message || err}`, { kind: 'error' });
      drawServer(srv);
    };
    const patch = async (body) => {
      try {
        const next = await ctx.api.patch('settings', { version: srv.version, ...body });
        ctx.flash('Saved for everyone on this server.');
        if (body.geocode) await refreshGeo();
        if (ctx.alive()) { drawServer(next); sync(); }
        return next;
      } catch (err) {
        if (err && err.code === 'stale_version') {
          ctx.flash('Someone changed these settings meanwhile; they are shown fresh now. Make your change again.', { kind: 'info' });
          const fresh = await ctx.api.get('settings');
          if (ctx.alive()) drawServer(fresh);
          return null;
        }
        throw err;
      }
    };
    const quick = (body) => patch(body).catch(failed);

    // Defaults
    const defDist = choice({ label: 'Default distance', labelHidden: true, name: 'def-distance', value: s.units.distance, options: [['km', 'km'], ['mi', 'mi']], onchange: (v) => quick({ units: { distance: v } }) });
    const defTemp = choice({ label: 'Default temperature', labelHidden: true, name: 'def-temperature', value: s.units.temperature, options: [['c', '°C'], ['f', '°F']], onchange: (v) => quick({ units: { temperature: v } }) });
    const defPress = choice({ label: 'Default pressure', labelHidden: true, name: 'def-pressure', value: s.units.pressure, options: [['bar', 'bar'], ['psi', 'psi']], onchange: (v) => quick({ units: { pressure: v } }) });
    const defRange = choice({ label: 'Default range', labelHidden: true, name: 'def-range', value: s.range, options: [['rated', 'Rated'], ['ideal', 'Ideal']], onchange: (v) => quick({ range: v }) });
    const defaults = h('div', { class: 'defaults-grid' },
      h('span', { class: 'defaults-name' }, 'Distance'), defDist,
      h('span', { class: 'defaults-name' }, 'Temperature'), defTemp,
      h('span', { class: 'defaults-name' }, 'Pressure'), defPress,
      h('span', { class: 'defaults-name' }, 'Range'), defRange);

    // Address lookup
    const lookup = choice({ label: 'Address lookup', labelHidden: true, name: 'geocode', value: s.geocode.mode, options: [['offline', 'Offline (on this server)'], ['online', 'Online service']],
      onchange: (v) => quick({ geocode: { mode: v } }) });
    const g = geo || {};
    const od = g.offline_data;
    const statusRows = [];
    if (g.mode) {
      statusRows.push(['In use', g.mode === 'online' ? 'the online service below' : 'offline place names']);
      if (od && typeof od === 'object') {
        statusRows.push(['Offline names', od.present ? `${od.name || 'present'}${od.places != null ? `, ${format(od.places, 'count')} places` : ''}${od.built_ms ? `, built ${formatDateTime(od.built_ms, ctx.tz, { year: true })}` : ''}` : 'not installed']);
      } else if (od != null) {
        statusRows.push(['Offline names', od ? 'present' : 'not installed']);
      }
      if (g.cache_size != null) statusRows.push(['Kept addresses', `${format(g.cache_size, 'count')} looked up before, answered without asking again`]);
      statusRows.push(['Rounding', `${format(s.geocode.rounding_m, 'length')} before a position leaves the server`]);
      const on = g.online || {};
      if (s.geocode.mode === 'online' || on.last_error) {
        statusRows.push(['Online service', on.last_error ? `last error: ${on.last_error}${on.retry_ms ? `; tries again ${formatDateTime(on.retry_ms, ctx.tz)}` : ''}` : 'no error']);
      }
    }
    const geoStatus = h('div', { class: 'geo-status' },
      g.mode
        ? h('dl', { class: 'geo-status-list' }, statusRows.map(([k, v]) => [h('dt', null, k), h('dd', { class: { 'geo-bad': /^last error/.test(v) } }, v)]))
        : h('p', { class: 'setting-note' }, 'The address lookup did not answer, so its status is not known.'),
      pageLink('Address status in Data health and storage', 'health'));

    // Address details
    const datalistId = 'addr-langs';
    const langField = field({ label: 'Address language', name: 'language', value: s.geocode.language || '',
      hint: 'A language code such as en, de or fr. Street and place names of drives, charges and places are written in it where the data has it, otherwise in the local language. Addresses already looked up are looked up again.',
      attrs: { maxlength: '10', spellcheck: 'false', autocomplete: 'off', list: datalistId, disabled, required: true } });
    const langNameEl = h('p', { class: 'field-hint lang-name' });
    const showLangName = () => {
      const r = checkLangCode(langField.control.value);
      langNameEl.textContent = r.code ? `${langName(r.code) || r.code}` : '';
    };
    langField.control.addEventListener('input', () => { showLangName(); setFieldError(langField, null); });
    langField.querySelector('.field-control').append(langNameEl);
    showLangName();
    const urlField = field({ label: 'Online service address', name: 'url', value: s.geocode.url || '',
      hint: 'Only used with the online service: an https:// address of a reverse-lookup service, your own copy of one, or a proxy in front of it. Positions are rounded first; nothing is sent while lookup is offline.',
      attrs: { placeholder: 'https://…', spellcheck: 'false', autocomplete: 'off', disabled } });
    const roundField = field({ label: 'Round positions to (metres)', name: 'rounding_m', type: 'number', value: s.geocode.rounding_m ?? '',
      hint: '0 to 1000 m. A larger number tells the service less about where the car was, with less exact addresses.', attrs: { min: '0', max: '1000', step: '1', disabled } });
    const addr = form({
      fields: [langField, urlField, roundField],
      extra: h('datalist', { id: datalistId }, ADDRESS_LANGS.map((c) => h('option', { value: c }, langName(c) || c))),
      submitLabel: 'Save address settings',
      onsubmit: async (v) => {
        const r = checkLangCode(v.language);
        const bad = {};
        if (r.error) bad.language = r.error;
        const url = String(v.url || '').trim();
        if (url && !/^https:\/\/[^\s/]+/.test(url)) bad.url = 'Use an https:// address, or leave it empty.';
        if (v.rounding_m == null || !Number.isInteger(v.rounding_m) || v.rounding_m < 0 || v.rounding_m > 1000) bad.rounding_m = 'A whole number of metres from 0 to 1000.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: bad });
        try {
          await patch({ geocode: { language: r.code, url: url || null, rounding_m: v.rounding_m } });
        } catch (err) {
          // Field paths come as geocode.language: put them under their fields.
          if (err && err.fields) {
            err.fields = Object.fromEntries(Object.entries(err.fields).map(([k, m]) => [k.replace(/^geocode\./, ''), m]));
          } else if (err && !err.message?.startsWith('Not saved')) {
            err.message = `Not saved: ${err.message || 'the server did not answer'}. The addresses stay as they were; try again.`;
          }
          throw err;
        }
      },
      cover: ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid', 'docs.env.tesla-api.nominatim-proxy'],
    });
    if (disabled) addr.querySelector('[type="submit"]').hidden = true;

    // Map tiles
    const tiles = choice({ label: 'Map tiles', labelHidden: true, name: 'tiles', value: s.tiles.mode, options: [['offline', 'Offline (on this server)'], ['online', 'Online tile server']],
      onchange: (v) => quick({ tiles: { mode: v } }) });
    let tileHost = null;
    try { tileHost = s.tiles.online_url ? new URL(s.tiles.online_url.replace(/\{[a-z]\}/g, '0')).host : null; } catch { tileHost = null; }
    const tileNow = h('p', { class: 'setting-note' }, s.tiles.mode === 'online'
      ? `Now: online, from ${tileHost || 'the tile server below'}. Each viewer’s browser asks it for the map pictures, so it sees which area is shown.`
      : 'Now: offline. Maps are drawn by this server; where no offline tiles are installed, maps show the routes and pins on plain paper.');
    const tileForm = form({
      fields: [
        field({ label: 'Online tile address', name: 'online_url', value: s.tiles.online_url || '', hint: 'An https:// address with {z}, {x} and {y} in it.', attrs: { spellcheck: 'false', autocomplete: 'off', placeholder: 'https://…/{z}/{x}/{y}.png', disabled } }),
        field({ label: 'Credit shown on the map', name: 'attribution', value: s.tiles.attribution || '', hint: 'Who made the map pictures, as their licence asks.', attrs: { disabled } }),
        field({ label: 'Deepest zoom', name: 'max_zoom', type: 'number', value: s.tiles.max_zoom ?? '', hint: '1 to 22; the tile server’s own limit.', attrs: { min: '1', max: '22', step: '1', disabled } }),
      ],
      submitLabel: 'Save tile settings',
      onsubmit: async (v) => {
        const bad = {};
        const url = String(v.online_url || '').trim();
        if (url && !(/^https:\/\//.test(url) && ['{z}', '{x}', '{y}'].every((x) => url.includes(x)))) bad.online_url = 'An https:// address with {z}, {x} and {y} in it.';
        if (v.max_zoom == null || !Number.isInteger(v.max_zoom) || v.max_zoom < 1 || v.max_zoom > 22) bad.max_zoom = 'A whole number from 1 to 22.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: bad });
        try {
          await patch({ tiles: { online_url: url || null, attribution: String(v.attribution || '').trim(), max_zoom: v.max_zoom } });
        } catch (err) {
          if (err && err.fields) err.fields = Object.fromEntries(Object.entries(err.fields).map(([k, m]) => [k.replace(/^tiles\./, ''), m]));
          throw err;
        }
      },
    });
    if (disabled) tileForm.querySelector('[type="submit"]').hidden = true;

    const mapsLink = switchField({ label: 'Allow links to an outside map', checked: !!s.outside_links.maps, onchange: (on) => quick({ outside_links: { maps: on } }) });
    const notesLink = switchField({ label: 'Allow release-notes links', checked: !!s.outside_links.release_notes, onchange: (on) => quick({ outside_links: { release_notes: on } }) });
    const release = switchField({ label: 'Check for new releases', checked: !!s.release_check.enabled, onchange: (on) => quick({ release_check: { enabled: on } }) });
    for (const el of [defDist, defTemp, defPress, defRange, lookup, tiles]) el.querySelectorAll('button').forEach((b) => { b.disabled = disabled; });
    for (const el of [mapsLink, notesLink, release]) el.input.disabled = disabled;

    const readOnly = disabled ? flag('Read only') : null;
    replace(serverBox,
      h('section', { class: 'settings', 'aria-labelledby': 'defaults-title' },
        h('h2', { class: 'label settings-label', id: 'defaults-title' }, 'Defaults for everyone', h('span', { class: 'label-note' }, admin ? 'admins only; saved at once' : 'read only for your account'), readOnly),
        setting('Units and range', 'For people who have not chosen their own. The distance unit can start from the one a car reports from its own screen settings.', defaults)),
      h('section', { class: 'settings', 'aria-labelledby': 'maps-title' },
        h('h2', { class: 'label settings-label', id: 'maps-title' }, 'Addresses and maps', h('span', { class: 'label-note' }, 'private by default: nothing leaves this server unless an admin turns it on'), readOnly),
        setting('Address lookup', `How a recorded position becomes a street address. Offline uses the place names bundled with Sentry Mate (${s.geocode.dataset || 'built in'}); nothing leaves the server. Online asks the service below, with positions rounded, and keeps each answer so the same spot is asked once.`,
          lookup, { cover: 'web.settings.general.address-lookup', extra: geoStatus }),
        setting('Address details', 'Language, online service and rounding.', addr, { cls: 'setting-form' }),
        setting('Map tiles', `The pictures under every map. Credit: ${s.tiles.attribution || 'shown on the map'}; zoom up to ${s.tiles.max_zoom}.`, h('div', { class: 'tiles-control' }, tiles, tileNow), { cover: 'web.shell.map-tiles' }),
        setting('Online tile server', 'Only used with online tiles.', tileForm, { cls: 'setting-form' }),
        setting('Outside links', 'Off by default: nothing on a page sends you to another site unless an admin allows it. Links open in a new tab, only when clicked.', h('div', { class: 'switches' }, mapsLink, notesLink)),
        setting('Release check', 'Asks for the newest version number once a day and shows a note in the footer. Off by default.', release)));
  }
  try { drawServer(server); } catch (err) { replace(serverBox, errorView(err)); }
  sync();

  ctx.main.append(
    pageHead({ eyebrow: 'Settings', title: 'Display and units', deck: ctx.info.purpose }),
    mine, divider, dividerNote, serverBox);
  ctx.ready();
  // Its own controls change the preferences: they follow onPrefs above; the page is not drawn again.
  return { update: (c) => { sync(); c.ready(); } };
}
