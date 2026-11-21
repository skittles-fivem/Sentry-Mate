// Sentry Mate, "Pebble" UI pack: Settings: Display and units (/settings/display).
//
// Your own choices first (GET / PATCH /api/v1/me/preferences): theme (Auto, Light, Dark), units, rated or ideal
// range, language, interface size and the UI pack. Each is a rounded list row; a picker opens a sheet, and the
// change saves at once and shows on every page, with an example in the chosen unit next to it.
// Below a divider, what an admin sets for everyone (GET / PATCH /api/v1/settings): default units, address lookup
// and its status (GET /api/v1/geocode/status), map tiles, the outbound proxy, outside links and the release check.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { getLocale, isSupportedLocale } from '../shared/i18n.js';
import { format, getUnits, FT_PER_M } from '../shared/units.js';
import { formatDateTime } from '../shared/time.js';
import { tile, btn, chip, note } from '../core/ui.js';
import { segmented, toggle, field, form, setFieldError } from '../core/form.js';
import { currentPrefs, onPrefs, savePref, isDark, onTheme } from '../core/prefs.js';
import { SIZES, SIZE_MIN, SIZE_MAX, sizeState, setSize, sizeNote, display } from '../core/display.js';
import { currentAbout } from '../core/frame.js';
import { sectionHead, facts } from '../core/bits.js';
import { group, pickRow, ctlRow, infoRow, notSaved } from '../core/settings.js';
import { inOrder } from '../core/settings.js';

export const id = 'settings-display';
export const shape = [[2, 'h3'], [2, 'h3'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Display and units';

const LOCALES = ['en-US', 'en-GB', 'en-CA', 'en-AU', 'de-DE', 'de-CH', 'fr-FR', 'fr-CA', 'nl-NL', 'nb-NO', 'sv-SE', 'da-DK',
  'fi-FI', 'es-ES', 'it-IT', 'pt-PT', 'pl-PL', 'cs-CZ', 'ja-JP', 'ko-KR', 'zh-CN'];
const PACKS = [['pebble', 'Pebble', 'Light, rounded tiles and a floating dock'], ['instrument', 'Instrument', 'A dark glass cluster with a side bar'], ['logbook', 'Logbook', 'Paper and ink, the day as a log']];
const DIST = [['km', 'Kilometres', 'km, km/h, Wh/km, metres'], ['mi', 'Miles', 'mi, mph, Wh/mi, feet']];
const TEMP = [['c', 'Celsius', '°C'], ['f', 'Fahrenheit', '°F']];
const PRESS = [['bar', 'bar'], ['psi', 'psi']];
const RANGE = [['rated', 'Rated', 'The official test figure of the market the car was sold in'], ['ideal', 'Ideal', 'The car’s own, usually longer figure']];
const WORDS = { distance: { km: 'kilometres', mi: 'miles' }, temperature: { c: '°C', f: '°F' }, pressure: { bar: 'bar', psi: 'psi' }, range: { rated: 'rated', ideal: 'ideal' } };

function langName(tag) {
  try { const n = new Intl.DisplayNames(['en'], { type: 'language' }).of(tag); return n && n !== tag ? n : null; } catch { return null; }
}
/** An address language as typed: a canonical code, or what is wrong in plain words. */
function checkLang(raw) {
  const v = String(raw || '').trim();
  if (!v) return { error: 'Type a language code such as en, de or fr.' };
  if (!/^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$/.test(v) || !isSupportedLocale(v)) return { error: `“${v}” is not a language code. Use a code such as en, de or fr.` };
  return { code: Intl.getCanonicalLocales(v)[0] };
}
const hostOf = (u) => { try { return new URL(String(u).replace(/\{[a-z]\}/g, '0')).host; } catch { return null; } };
/** Field paths come as geocode.language: put them under the form's own field names. */
const strip = (err, prefix, rename = {}) => {
  if (err && err.fields) err.fields = Object.fromEntries(Object.entries(err.fields).map(([k, m]) => { const n = k.replace(prefix, ''); return [rename[n] || n, m]; }));
  return err;
};

export async function mount(ctx) {
  inOrder(ctx);
  const admin = ctx.admin;
  let [srv, geo] = await Promise.all([ctx.api.get('settings'), ctx.api.get('geocode/status').catch(() => null)]);
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
  const about = currentAbout();
  const packNow = (p0.ui_pack || (about && about.ui_pack && about.ui_pack.name ? about.ui_pack.name.toLowerCase() : 'pebble'));
  const packOpts = PACKS.some(([k]) => k === packNow) ? PACKS : [...PACKS, [packNow, packNow, 'Installed on this server']];
  const pack = pickRow({ name: 'UI pack', sub: 'The look of the whole app. Used the next time the app loads.', value: packNow, options: packOpts,
    sheetNote: 'Admins can install more packs. Your data and settings stay the same in every pack.',
    onpick: (v) => save({ ui_pack: v }).then(() => ctx.toast('UI pack saved. Reload the page to see it.', { kind: 'info' })) });
  const lookT = tile({ w: 2, title: 'Look', icon: 'sun', cover: ['shell.theme', 'web.settings.general.theme'] },
    group(null, [
      ctlRow({ name: 'Theme', sub: 'Auto follows your device, day and night.', control: theme, stack: true }),
      h('div', { class: 'row row-static set-ex-row' }, themeEx),
    ]),
    group('Size', [
      ctlRow({ name: 'Interface size', sub: null, control: h('div', { class: 'stepper', role: 'group', 'aria-label': 'Interface size' }, smaller, sizeValue, bigger) }),
      h('div', { class: 'row row-static set-ex-row' }, sizeLine),
      follow,
    ]),
    group('Pack', [pack]));

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
  const langT = tile({ w: 2, title: 'Language', icon: 'globe' },
    group(null, [lang,
      h('div', { class: 'row row-static set-ex-row', cover: 'web.shell.locale-negotiation' }, h('span', { class: 'row-text' }, langInUse, langSample))]));

  /* ------------------------------------------------------------ keeping it in step */
  function sync() {
    const pr = currentPrefs() || {};
    const u = pr.units || {};
    theme.set(pr.theme || 'device');
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
    if (feet() !== drawnFeet) drawServer(srv);
  }
  ctx.onCleanup(onPrefs(sync));
  ctx.onCleanup(display.onChange(sync));
  ctx.onCleanup(onTheme(sync));

  /* ------------------------------------------------------------ for everyone */
  const divider = sectionHead(admin ? 'For everyone on this server' : 'Set by an admin for everyone',
    admin ? 'Your own choices above win over these for you. A default changes it for everyone who has not chosen their own.' : 'Your own choices above win over these. Only an admin can change what is below.',
    { cover: 'web.settings.general.divider' });
  const serverBox = h('div', { class: 'contents' });
  const feet = () => getUnits().distance === 'mi';
  let drawnFeet = feet();

  async function patch(body) {
    try {
      const next = await ctx.api.patch('settings', { version: srv.version, ...body });
      if (body.geocode || body.outbound) geo = await ctx.api.get('geocode/status').catch(() => geo);
      ctx.toast('Saved for everyone on this server.');
      if (ctx.alive()) drawServer(next);
      return next;
    } catch (err) {
      if (err && err.code === 'stale_version') {
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
    drawnFeet = feet();
    const off = !admin;
    const ro = off ? chip('Read only', { tone: 'muted', icon: 'lock' }) : null;

    const defs = [
      ['distance', 'Distance', DIST], ['temperature', 'Temperature', TEMP], ['pressure', 'Tyre pressure', PRESS],
    ].map(([k, name, opts]) => pickRow({ name, value: s.units[k], options: opts, disabled: off, onpick: (v) => quick({ units: { [k]: v } }, () => {}) }));
    const defRange = pickRow({ name: 'Range', value: s.range, options: RANGE, disabled: off, onpick: (v) => quick({ range: v }, () => {}) });
    const defaultsT = tile({ w: 2, title: 'Defaults for everyone', icon: 'person', side: ro },
      group(null, [...defs, defRange]),
      note('For people who have not chosen their own. The distance unit can start from the one a car reports from its own screen settings.'));

    // address lookup
    const g = geo || {};
    const od = g.offline_data;
    const lookup = segmented({ name: 'Address lookup', value: s.geocode.mode, options: [['offline', 'Offline'], ['online', 'Online']],
      onchange: (v) => {
        const body = { mode: v };
        if (v === 'online') {
          const typed = String(urlF.control.value || '').trim();
          if (typed && typed !== (s.geocode.url || '')) {
            if (!/^https:\/\/[^\s/]+/.test(typed)) { lookup.set(s.geocode.mode); setFieldError(urlF, 'Use an https:// address here, then choose Online.'); urlF.control.focus(); return; }
            body.url = typed;
          } else if (!s.geocode.url) { lookup.set(s.geocode.mode); setFieldError(urlF, 'Type the https:// address of the address service here first, then choose Online again.'); urlF.control.focus(); return; }
        }
        quick({ geocode: body }, () => lookup.set(s.geocode.mode));
      } });
    lookup.querySelectorAll('button').forEach((b) => { b.disabled = off; });
    const geoFacts = facts([
      ['In use', g.mode ? (g.mode === 'online' ? 'The online service below' : 'Offline place names') : 'Not known: no answer'],
      ['Offline names', od && typeof od === 'object' ? (od.present ? `${od.name || 'present'}${od.places != null ? `, ${format(od.places, 'count')} places` : ''}` : 'not installed') : od ? 'present' : null],
      ['Kept addresses', g.cache_size != null ? `${format(g.cache_size, 'count')} looked up before` : null],
      ['Rounding', `${format(s.geocode.rounding_m, 'length')} before a position leaves the server`],
      ['Online service', g.online && g.online.last_error ? `last error: ${g.online.last_error}` : s.geocode.mode === 'online' ? 'no error' : null],
    ]);
    const langF = field({ label: 'Address language', name: 'language', value: s.geocode.language || '', hint: 'A code such as en, de or fr. Street and place names are written in it where the data has it.', attrs: { maxlength: '10', spellcheck: 'false', autocomplete: 'off', disabled: off, list: 'sd-langs' } });
    const langNm = h('p', { class: 'hint' });
    const showName = () => { const r = checkLang(langF.control.value); langNm.textContent = r.code ? `${langName(r.code) || r.code}` : ''; };
    langF.control.addEventListener('input', () => { showName(); setFieldError(langF, null); });
    langF.append(langNm);
    showName();
    const urlF = field({ label: 'Online service address', name: 'url', value: s.geocode.url || '', hint: 'Only used with Online. Positions are rounded first; nothing is sent while lookup is offline.', attrs: { placeholder: 'https://…', spellcheck: 'false', autocomplete: 'off', disabled: off } });
    urlF.control.addEventListener('input', () => setFieldError(urlF, null));
    const maxRound = drawnFeet ? Math.round(1000 * FT_PER_M) : 1000;
    const roundF = field({ label: `Round positions to (${drawnFeet ? 'feet' : 'metres'})`, name: 'rounding', type: 'number', value: s.geocode.rounding_m == null ? '' : drawnFeet ? Math.round(s.geocode.rounding_m * FT_PER_M) : s.geocode.rounding_m,
      hint: `0 to ${format(maxRound, 'count')}. Larger tells the service less about where the car was.`, attrs: { min: '0', max: String(maxRound), step: '1', disabled: off } });
    const addrForm = form({
      fields: [langF, urlF, roundF], submitLabel: 'Save address settings',
      extra: h('datalist', { id: 'sd-langs' }, ['en', 'de', 'fr', 'nl', 'nb', 'sv', 'da', 'fi', 'es', 'it', 'pt', 'pl', 'cs', 'ja', 'ko', 'zh'].map((c) => h('option', { value: c }, langName(c) || c))),
      cover: ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid'],
      onsubmit: async (v) => {
        const r = checkLang(v.language);
        const bad = {};
        if (r.error) bad.language = r.error;
        const url = String(v.url || '').trim();
        if (url && !/^https:\/\/[^\s/]+/.test(url)) bad.url = 'Use an https:// address, or leave it empty.';
        if (v.rounding == null || !Number.isInteger(v.rounding) || v.rounding < 0 || v.rounding > maxRound) bad.rounding = `A whole number from 0 to ${format(maxRound, 'count')}.`;
        if (Object.keys(bad).length) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: bad });
        try { await patch({ geocode: { language: r.code, url: url || null, rounding_m: drawnFeet ? Math.min(1000, Math.round(v.rounding / FT_PER_M)) : v.rounding } }); } catch (err) { throw strip(err, /^geocode\./, { rounding_m: 'rounding' }); }
      },
    });
    if (off) addrForm.submitButton.hidden = true;
    const addrT = tile({ w: 2, title: 'Addresses', icon: 'pin', side: ro, cover: 'web.settings.general.address-lookup' },
      group(null, [ctlRow({ name: 'Address lookup', sub: `Offline uses the place names on this server (${s.geocode.dataset || 'built in'}); nothing leaves it. Online asks the service below, rounded, and keeps each answer.`, control: lookup, stack: true })]),
      geoFacts, addrForm);

    // map tiles
    const tilesSeg = segmented({ name: 'Map tiles', value: s.tiles.mode, options: [['offline', 'Offline'], ['online', 'Online']],
      onchange: (v) => {
        if (v === 'online' && !s.tiles.online_url) { tilesSeg.set(s.tiles.mode); setFieldError(tileUrl, 'Save an https:// tile address with {z}, {x} and {y} first, then choose Online.'); tileUrl.control.focus(); return; }
        quick({ tiles: { mode: v } }, () => tilesSeg.set(s.tiles.mode));
      } });
    tilesSeg.querySelectorAll('button').forEach((b) => { b.disabled = off; });
    const tileUrl = field({ label: 'Online tile address', name: 'online_url', value: s.tiles.online_url || '', hint: 'An https:// address with {z}, {x} and {y} in it.', attrs: { spellcheck: 'false', autocomplete: 'off', placeholder: 'https://…/{z}/{x}/{y}.png', disabled: off } });
    tileUrl.control.addEventListener('input', () => setFieldError(tileUrl, null));
    const tileForm = form({
      fields: [tileUrl,
        field({ label: 'Credit shown on the map', name: 'attribution', value: s.tiles.attribution || '', hint: 'Who made the map pictures, as their licence asks.', attrs: { disabled: off } }),
        field({ label: 'Deepest zoom', name: 'max_zoom', type: 'number', value: s.tiles.max_zoom ?? '', hint: '1 to 22.', attrs: { min: '1', max: '22', step: '1', disabled: off } })],
      submitLabel: 'Save tile settings',
      onsubmit: async (v) => {
        const bad = {};
        const url = String(v.online_url || '').trim();
        if (url && !(/^https:\/\//.test(url) && ['{z}', '{x}', '{y}'].every((x) => url.includes(x)))) bad.online_url = 'An https:// address with {z}, {x} and {y} in it.';
        if (v.max_zoom == null || !Number.isInteger(v.max_zoom) || v.max_zoom < 1 || v.max_zoom > 22) bad.max_zoom = 'A whole number from 1 to 22.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: bad });
        try { await patch({ tiles: { online_url: url || null, attribution: String(v.attribution || '').trim(), max_zoom: v.max_zoom } }); } catch (err) { throw strip(err, /^tiles\./); }
      },
    });
    if (off) tileForm.submitButton.hidden = true;
    const mapsT = tile({ w: 2, title: 'Maps', icon: 'map', side: ro, cover: 'web.shell.map-tiles' },
      group(null, [ctlRow({ name: 'Map tiles', sub: s.tiles.mode === 'online'
        ? `Online, from ${hostOf(s.tiles.online_url) || 'the tile server below'}. Each viewer’s browser asks it for the map pictures.`
        : 'Offline: maps are drawn by this server; with no offline tiles, maps show routes and pins on a plain ground.', control: tilesSeg, stack: true })]),
      tileForm);

    // proxy
    const proxyF = field({ label: 'Proxy address', name: 'proxy', value: (s.outbound && s.outbound.proxy) || '', hint: 'For example http://proxy.lan:3128 or socks5://proxy.lan:1080, with no name or password in it. Empty: connect directly.', attrs: { placeholder: 'http://…', spellcheck: 'false', autocomplete: 'off', disabled: off } });
    const proxyForm = form({
      fields: [proxyF], submitLabel: 'Save proxy',
      onsubmit: async (v) => {
        const url = String(v.proxy || '').trim();
        if (url && !/^(https?|socks5):\/\/[A-Za-z0-9.-]+(:\d{1,5})?\/?$/.test(url)) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: { proxy: 'http, https or socks5, a host name and a port, nothing else.' } });
        try { await patch({ outbound: { proxy: url || null } }); } catch (err) { throw strip(err, /^outbound\./); }
      },
    });
    if (off) proxyForm.submitButton.hidden = true;
    const proxyT = tile({ w: 2, title: 'Outbound proxy', icon: 'globe', side: ro, cover: 'docs.env.tesla-api.nominatim-proxy' },
      note(s.outbound && s.outbound.proxy ? `Now: through ${hostOf(s.outbound.proxy) || 'the proxy'}. Online address look-ups and map tiles the server downloads go through it.` : 'Now: none. The server connects directly, and only when online look-ups or tile downloads are on.'),
      proxyForm);

    // outside links and the release check
    const maps = toggle({ label: 'Links to an outside map', sub: 'Open a spot in another map site, only when clicked.', checked: !!s.outside_links.maps, disabled: off, onchange: (on) => quick({ outside_links: { maps: on } }, () => maps.set(s.outside_links.maps)) });
    const notes = toggle({ label: 'Release-notes links', sub: 'Link a new version to its notes on another site.', checked: !!s.outside_links.release_notes, disabled: off, onchange: (on) => quick({ outside_links: { release_notes: on } }, () => notes.set(s.outside_links.release_notes)) });
    const rel = toggle({ label: 'Check for new releases', sub: 'Asks once a day for the newest version number. Off by default.', checked: !!s.release_check.enabled, disabled: off, onchange: (on) => quick({ release_check: { enabled: on } }, () => rel.set(s.release_check.enabled)) });
    const linksT = tile({ w: 2, title: 'Outside links', icon: 'external', side: ro },
      group(null, [maps, notes]), group('Release check', [rel]),
      note('Off by default: nothing on a page sends you to another site unless an admin allows it.'));

    serverBox.replaceChildren(defaultsT, addrT, mapsT, proxyT, linksT);
  }
  drawServer(srv);
  sync();

  ctx.view.replaceChildren(lookT, unitsT, langT,
    tile({ w: 2, title: 'Where these live', icon: 'info' },
      group(null, [
        infoRow({ icon: 'person', name: 'Yours alone', sub: 'Theme, units, range, language, size and pack follow your account to every device.' }),
        infoRow({ icon: 'server', name: 'The server’s', sub: 'Defaults, addresses and maps below are one setting for everyone.' }),
      ])),
    divider, serverBox);
  ctx.ready();
  return { update: (c) => { sync(); c.ready(); } };
}
