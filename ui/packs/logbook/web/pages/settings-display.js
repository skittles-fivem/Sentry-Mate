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
import { format, getUnits, FT_PER_M } from '../shared/units.js';
import { formatDateTime, isValidZone } from '../shared/time.js';
import { pageHead, errorView, pageLink, flag, button } from '../core/ui.js';
import { choice, switchField, field, form, setFieldError } from '../core/form.js';
import { MAP_COLORS_CHOICES, MAP_COLORS_TEXT, LABELS_TEXT, STYLES_TEXT, presetHosts, OFFLINE_ANCHOR, OFFLINE_STEPS, OFFLINE_NOTE, offlineSummary, offlineFileChoices, showOfflineHelp } from '../shared/maptiles.js';
import { currentPrefs, onPrefs } from '../core/prefs.js';
import { savePref, currentAbout } from '../core/frame.js';
import { SIZES, sizeState, setSize, sizeNote, display } from '../core/display.js';
import { packChoices, packLabel } from '../shared/packs.js';

export const id = 'settings-display';
export const css = true;
export const title = () => 'Display and units';

// Locales offered for dates and numbers, besides the browser's own languages. Names come from the browser.
// used only when the server's list of packs can't be read
const PACKS = [['logbook', 'Logbook'], ['instrument', 'Instrument'], ['horizon', 'Horizon'], ['console', 'Console'], ['pebble', 'Pebble']];
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
  const [server, geoFirst, mapsFirst, packList] = await Promise.all([
    ctx.api.get('settings'),
    ctx.api.get('geocode/status').catch(() => null),
    ctx.api.get('maps/config').catch(() => null),
    packChoices(ctx.api, { fallback: PACKS, current: (currentPrefs() || {}).ui_pack || null }),
  ]);
  let maps = mapsFirst;
  const tilePresets = (maps && maps.tiles && maps.tiles.presets) || [];
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
  const mapColors = choice({ label: 'Map colours', labelHidden: true, name: 'map-colors', value: p.map_colors || 'dimmed', options: MAP_COLORS_CHOICES, onchange: (v) => save({ map_colors: v }) });

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
    range.set(pr.range || 'rated'); theme.set(pr.theme || 'device'); mapColors.set(pr.map_colors || 'dimmed');
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
    if (srv && feet() !== drawnFeet) redraw(srv);
  };
  ctx.onCleanup(onPrefs(sync));
  ctx.onCleanup(display.onChange(sync));
  const mq = matchMedia('(prefers-color-scheme: dark)');
  mq.addEventListener('change', sync);
  ctx.onCleanup(() => mq.removeEventListener('change', sync));
  fillLang();

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
      setting('Language', 'How dates, times and numbers are written (for example 1,234.5 or 1.234,5, and the first day of the week).', h('div', { class: 'lang-control' }, lang, langNote),
        { cover: ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation'], extra: langEx }),
      setting('Interface size', 'Text, controls, tables, charts and maps grow or shrink together. With “Follow screen size” on, big screens such as 4K monitors get larger text by themselves. Browser zoom works on top.', h('div', { class: 'size-control' }, size, follow, sizeNoteEl), { cls: 'setting-size' }),
      setting('UI pack', 'The look of the whole app. Admins install other packs; your data and settings stay the same in every pack.',
        h('div', { class: 'lang-control' }, h('div', { class: 'pack-control' }, packSel, switchBtn), packNote))));

  // ------------------------------------------------------------ the divider
  const divider = h('div', { class: 'settings-divider', role: 'separator', cover: 'web.settings.general.divider' },
    h('span', null, admin ? 'Below: for everyone on this server' : 'Below: set by an admin for everyone'));
  const dividerNote = h('p', { class: 'settings-divider-note' }, admin
    ? 'Your own choices above win over these for you. Changing a default changes it for everyone who has not made their own choice.'
    : 'Your own choices above win over these. Only an admin can change what is below.');

  // ------------------------------------------------------------ server settings
  const serverBox = h('div', { class: 'settings-server' });

  const proxyHost = (u) => { try { const x = new URL(u); return `${x.protocol.replace(':', '')} ${x.host}`; } catch { return 'the proxy'; } };
  const healthLink = () => {
    // Admins see the address look-ups under Server health there; the anchor takes them to it.
    const a = pageLink('Address status in Data health and storage', 'health');
    a.setAttribute('href', `${a.getAttribute('href')}#hl-srv`);
    return a;
  };

  async function refreshGeo() {
    try { geo = await ctx.api.get('geocode/status'); } catch { /* keep the last one */ }
  }

  // Rounding is typed in the person's own length unit: feet when distances are in miles, else metres.
  const feet = () => getUnits().distance === 'mi';
  const roundShown = (m) => (m == null ? '' : feet() ? Math.round(m * FT_PER_M) : m);
  let drawnFeet = feet();

  // What the person typed in the server forms (and where the focus was) outlives a redraw: a toggle, a
  // default or a fresh copy after someone else's change never empties a half-filled form.
  function keepTyped() {
    const typed = new Map();
    for (const c of serverBox.querySelectorAll('form input[name]')) {
      if (c.dataset.saved != null && c.value !== c.dataset.saved) typed.set(c.closest('form').dataset.form + '/' + c.name, c.value);
    }
    const all = [...serverBox.querySelectorAll('button, input, select, textarea, a[href]')];
    return { typed, focus: all.indexOf(document.activeElement), feet: drawnFeet };
  }
  function putTyped(kept) {
    for (const c of serverBox.querySelectorAll('form input[name]')) {
      const k = c.closest('form').dataset.form + '/' + c.name;
      if (!kept.typed.has(k)) continue;
      if (c.name === 'rounding' && kept.feet !== drawnFeet) continue; // typed in the other unit
      c.value = kept.typed.get(k);
      c.dispatchEvent(new Event('input'));
    }
    if (kept.focus >= 0) [...serverBox.querySelectorAll('button, input, select, textarea, a[href]')][kept.focus]?.focus();
  }
  const redraw = (s) => { const kept = keepTyped(); drawServer(s); putTyped(kept); };

  /** "Not saved: …" with the server's own reasons, not only its summary. */
  const notSaved = (err) => {
    const why = err && err.fields ? Object.values(err.fields).join(' ') : (err && err.message) || String(err);
    return `Not saved: ${why}`;
  };

  function drawServer(s) {
    srv = s;
    drawnFeet = feet();
    const disabled = !admin;
    const patch = async (body) => {
      try {
        const next = await ctx.api.patch('settings', { version: srv.version, ...body });
        ctx.flash('Saved for everyone on this server.');
        if (body.geocode || body.outbound) await refreshGeo();
        if (body.tiles) maps = await ctx.api.get('maps/config').catch(() => maps);
        if (ctx.alive()) { redraw(next); sync(); }
        return next;
      } catch (err) {
        if (err && err.code === 'stale_version') {
          ctx.flash('Someone changed these settings meanwhile; they are shown fresh now. Make your change again.', { kind: 'info' });
          const fresh = await ctx.api.get('settings');
          if (ctx.alive()) { redraw(fresh); sync(); }
          return null;
        }
        throw err;
      }
    };
    // A toggle or default saves at once. When it is refused, only that control goes back; nothing else
    // on the page is drawn again, so what is typed in the forms stays.
    const quick = (body, undo) => patch(body).catch((err) => {
      undo();
      ctx.flash(notSaved(err), { kind: 'error' });
    });

    // Defaults
    const defDist = choice({ label: 'Default distance', labelHidden: true, name: 'def-distance', value: s.units.distance, options: [['km', 'km'], ['mi', 'mi']], onchange: (v) => quick({ units: { distance: v } }, () => defDist.set(s.units.distance)) });
    const defTemp = choice({ label: 'Default temperature', labelHidden: true, name: 'def-temperature', value: s.units.temperature, options: [['c', '°C'], ['f', '°F']], onchange: (v) => quick({ units: { temperature: v } }, () => defTemp.set(s.units.temperature)) });
    const defPress = choice({ label: 'Default pressure', labelHidden: true, name: 'def-pressure', value: s.units.pressure, options: [['bar', 'bar'], ['psi', 'psi']], onchange: (v) => quick({ units: { pressure: v } }, () => defPress.set(s.units.pressure)) });
    const defRange = choice({ label: 'Default range', labelHidden: true, name: 'def-range', value: s.range, options: [['rated', 'Rated'], ['ideal', 'Ideal']], onchange: (v) => quick({ range: v }, () => defRange.set(s.range)) });
    const defaults = h('div', { class: 'defaults-grid' },
      h('span', { class: 'defaults-name' }, 'Distance'), defDist,
      h('span', { class: 'defaults-name' }, 'Temperature'), defTemp,
      h('span', { class: 'defaults-name' }, 'Pressure'), defPress,
      h('span', { class: 'defaults-name' }, 'Range'), defRange);

    // The time zone new cars start in
    const zones = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
    const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
    const tzField = field({ label: 'Time zone for new cars', name: 'default_time_zone', value: s.default_time_zone || 'UTC', hint: 'A name such as America/Chicago. Type to see the list.', attrs: { list: 'sd-zones', spellcheck: 'false', autocomplete: 'off', disabled } });
    const saveZone = async (v) => {
      setFieldError(tzField, null);
      if (!v || !isValidZone(v)) { setFieldError(tzField, 'Not a known time zone. Pick one from the list, for example America/Chicago.'); return; }
      if (v === s.default_time_zone) return;
      try { await patch({ default_time_zone: v }); } catch (err) { setFieldError(tzField, (err.fields && err.fields.default_time_zone) || notSaved(err)); }
    };
    tzField.control.addEventListener('change', () => saveZone(tzField.control.value.trim()));
    const zoneControl = h('div', { class: 'tiles-control' }, tzField, h('datalist', { id: 'sd-zones' }, zones.map((z) => h('option', { value: z }))),
      browserZone && browserZone !== s.default_time_zone && !disabled
        ? button(`Use this browser’s zone (${browserZone.replace(/_/g, ' ')})`, { cls: 'button-small', onclick: () => { tzField.control.value = browserZone; saveZone(browserZone); } }) : null,
      h('p', { class: 'setting-note' }, 'Each car keeps its own zone: ', pageLink('Settings: Cars', 'settings-cars'), '. The clock in the masthead names the zone shown.'));

    // Address lookup
    const lookup = choice({ label: 'Address lookup', labelHidden: true, name: 'geocode', value: s.geocode.mode, options: [['offline', 'Offline (on this server)'], ['online', 'Online service']],
      onchange: (v) => {
        const body = { mode: v };
        if (v === 'online') {
          // Online needs the service's address. A valid one typed but not saved yet is sent along; with none,
          // the page says so under the address field instead of asking the server.
          const typed = String(urlField.control.value || '').trim();
          if (typed && typed !== (s.geocode.url || '')) {
            if (!/^https:\/\/[^\s/]+/.test(typed)) return refuse(lookup, s.geocode.mode, urlField, 'Use an https:// address here; then choose Online service.');
            body.url = typed;
          } else if (!s.geocode.url) {
            return refuse(lookup, s.geocode.mode, urlField, 'Type the https:// address of the address service here first, then choose Online service again.');
          }
        }
        quick({ geocode: body }, () => lookup.set(s.geocode.mode));
      } });
    function refuse(ctl, old, fieldEl, text) {
      ctl.set(old);
      setFieldError(fieldEl, text);
      fieldEl.control.focus();
      ctx.flash(`Not changed: ${text}`, { kind: 'error' });
    }
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
      statusRows.push(['Rounding', `${format(s.geocode.rounding_m, 'length')}${drawnFeet ? ` (${format(s.geocode.rounding_m, 'count')} m)` : ''} before a position leaves the server`]);
      if (s.outbound?.proxy) statusRows.push(['Through', `the outbound proxy ${proxyHost(s.outbound.proxy)}`]);
      const on = g.online || {};
      if (s.geocode.mode === 'online' || on.last_error) {
        statusRows.push(['Online service', on.last_error ? `last error: ${on.last_error}${on.retry_ms ? `; tries again ${formatDateTime(on.retry_ms, ctx.tz)}` : ''}` : 'no error']);
      }
    }
    const geoStatus = h('div', { class: 'geo-status' },
      g.mode
        ? h('dl', { class: 'geo-status-list' }, statusRows.map(([k, v]) => [h('dt', null, k), h('dd', { class: { 'geo-bad': /^last error/.test(v) } }, v)]))
        : h('p', { class: 'setting-note' }, 'The address lookup did not answer, so its status is not known.'),
      admin ? healthLink() : null);

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
      hint: 'Only used with the online service: the https:// address of a reverse-lookup service or of your own copy of one. Positions are rounded first; nothing is sent while lookup is offline.',
      attrs: { placeholder: 'https://…', spellcheck: 'false', autocomplete: 'off', disabled } });
    urlField.control.addEventListener('input', () => setFieldError(urlField, null));
    const maxRound = drawnFeet ? Math.round(1000 * FT_PER_M) : 1000;
    const roundField = field({ label: `Round positions to (${drawnFeet ? 'feet' : 'metres'})`, name: 'rounding', type: 'number', value: roundShown(s.geocode.rounding_m),
      hint: `0 to ${format(maxRound, 'count')} ${drawnFeet ? 'ft' : 'm'}${drawnFeet ? ' (kept in metres on the server, so it may move by a foot)' : ''}. A larger number tells the service less about where the car was, with less exact addresses.`,
      attrs: { min: '0', max: String(maxRound), step: '1', disabled } });
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
        if (v.rounding == null || !Number.isInteger(v.rounding) || v.rounding < 0 || v.rounding > maxRound) bad.rounding = `A whole number of ${drawnFeet ? 'feet' : 'metres'} from 0 to ${format(maxRound, 'count')}.`;
        if (Object.keys(bad).length) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: bad });
        const roundingM = drawnFeet ? Math.min(1000, Math.round(v.rounding / FT_PER_M)) : v.rounding;
        try {
          await patch({ geocode: { language: r.code, url: url || null, rounding_m: roundingM } });
        } catch (err) {
          // Field paths come as geocode.language: put them under their fields.
          if (err && err.fields) {
            err.fields = Object.fromEntries(Object.entries(err.fields).map(([k, m]) => [k.replace(/^geocode\./, '').replace(/^rounding_m$/, 'rounding'), m]));
          } else if (err && !err.message?.startsWith('Not saved')) {
            err.message = `Not saved: ${err.message || 'the server did not answer'}. The addresses stay as they were; try again.`;
          }
          throw err;
        }
      },
      cover: ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid'],
    });
    addr.dataset.form = 'addr';
    if (disabled) addr.querySelector('[type="submit"]').hidden = true;

    // Outbound proxy: one for everything the server fetches itself.
    const proxyField = field({ label: 'Proxy address', name: 'proxy', value: s.outbound?.proxy || '',
      hint: 'For example http://proxy.lan:3128 or socks5://proxy.lan:1080, with no name or password in it. Leave it empty to connect directly.',
      attrs: { placeholder: 'http://…', spellcheck: 'false', autocomplete: 'off', disabled } });
    const proxyForm = form({
      fields: [proxyField],
      submitLabel: 'Save proxy',
      onsubmit: async (v) => {
        const url = String(v.proxy || '').trim();
        if (url && !/^(https?|socks5):\/\/[A-Za-z0-9.-]+(:\d{1,5})?\/?$/.test(url)) {
          throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: { proxy: 'An address such as http://proxy.lan:3128: http, https or socks5, a host name and a port, nothing else.' } });
        }
        try {
          await patch({ outbound: { proxy: url || null } });
        } catch (err) {
          if (err && err.fields) err.fields = Object.fromEntries(Object.entries(err.fields).map(([k, m]) => [k.replace(/^outbound\./, ''), m]));
          throw err;
        }
      },
    });
    proxyForm.dataset.form = 'proxy';
    if (disabled) proxyForm.querySelector('[type="submit"]').hidden = true;
    const proxyNow = h('p', { class: 'setting-note' }, s.outbound?.proxy
      ? `Now: through ${proxyHost(s.outbound.proxy)}. Online address look-ups and map tiles the server downloads go through it; nothing else leaves the server.`
      : 'Now: none. The server connects directly, and only when online look-ups or tile downloads are on.');

    // Map tiles
    const tiles = choice({ label: 'Map tiles', labelHidden: true, name: 'tiles', value: s.tiles.mode, options: [['offline', 'Offline (on this server)'], ['online', 'Online tile server']],
      onchange: (v) => {
        if (v === 'online' && !s.tiles.online_url && !tilePresets.length) return refuse(tiles, s.tiles.mode, tileUrlField, 'Save an https:// tile address with {z}, {x} and {y} here first; then choose Online tile server.');
        quick({ tiles: { mode: v } }, () => tiles.set(s.tiles.mode));
      } });
    // the online preset in use (maps/config says which, the default satellite one when none was picked yet)
    const mt = (maps && maps.tiles) || {};
    const presetId = mt.preset || s.tiles.preset || (tilePresets.find((x) => x.url === s.tiles.online_url) || {}).id || (s.tiles.online_url ? 'custom' : (tilePresets[0] || {}).id || 'custom');
    const tilePreset = tilePresets.find((x) => x.id === presetId) || null;
    let tileHost = null;
    try { tileHost = s.tiles.online_url ? new URL(s.tiles.online_url.replace(/\{[a-z]\}/g, '0')).host : null; } catch { tileHost = null; }
    const nowHosts = (mt.hosts && mt.hosts.length ? mt.hosts : tilePreset ? presetHosts(tilePreset) : tileHost ? [tileHost] : []).join(', ');
    const tileNow = h('p', { class: 'setting-note' }, s.tiles.mode === 'online'
      ? `Now: online, ${tilePreset ? tilePreset.name : 'a custom address'}, from ${nowHosts || 'the tile server below'}. Each viewer’s browser asks it for the map pictures, so it sees which area is shown and this server’s address (as Referer, which some tile servers need; never a page or a car).`
      : 'Now: offline. Maps are drawn from the offline map file on this server; without one, maps show the routes and pins on plain paper, with a note saying so.');
    // the offline map file: which is in use, (with several) which to use, and how to make one
    const tFiles = offlineFileChoices(maps && maps.tiles);
    const offlineFile = tFiles ? choice({ label: 'Offline map file', labelHidden: true, name: 'offline-file', value: s.tiles.offline_file || '', options: tFiles,
      onchange: (v) => quick({ tiles: { offline_file: v || null } }, () => offlineFile.set(s.tiles.offline_file || '')) }) : null;
    if (offlineFile) offlineFile.querySelectorAll('button').forEach((b) => { b.disabled = disabled; });
    const offlineRow = setting('Offline map file', 'A map of your own area kept on this server: maps then need nothing from outside, and no viewer’s browser asks another site for tiles.',
      h('div', { class: 'tiles-control' }, h('p', { class: 'setting-note' }, offlineSummary(maps && maps.tiles)), offlineFile,
        h('ol', { class: 'setting-note offline-steps' }, OFFLINE_STEPS.map((x) => h('li', null, x))), h('p', { class: 'setting-note' }, OFFLINE_NOTE)));
    offlineRow.id = OFFLINE_ANCHOR;
    // the presets (satellite first), each with where its pictures come from, how deep it zooms and where it covers
    const presetNote = (x) => [presetHosts(x).join(', '), `zoom up to ${x.max_zoom}`, x.region ? `${x.region} only: elsewhere the map is blank` : null].filter(Boolean).join(' · ');
    const sortedPresets = [...tilePresets].sort((a, b) => (a.kind === 'satellite' ? 0 : 1) - (b.kind === 'satellite' ? 0 : 1));
    const radios = [];
    const pickPreset = (v) => {
      for (const r of radios) r.checked = r.value === v;
      if (v === 'custom') {
        if (!s.tiles.online_url) { tileUrlField.control.focus(); return; }
        quick({ tiles: { mode: s.tiles.mode, preset: 'custom' } }, () => pickPreset.reset());
        return;
      }
      quick({ tiles: { mode: s.tiles.mode, preset: v } }, () => pickPreset.reset());
    };
    pickPreset.reset = () => { for (const r of radios) r.checked = r.value === presetId; };
    const presetRow = (id, name, note) => {
      const input = h('input', { type: 'radio', name: 'tile-preset', value: id, checked: id === presetId, disabled, class: 'preset-radio', onchange: () => pickPreset(id) });
      radios.push(input);
      return h('label', { class: 'preset-option', dataset: { preset: id } }, input,
        h('span', { class: 'preset-text' }, h('span', { class: 'preset-name' }, name), note ? h('span', { class: 'preset-note' }, note) : null));
    };
    const tileSource = tilePresets.length ? h('div', { class: 'preset-list', role: 'radiogroup', 'aria-label': 'Online map' },
      sortedPresets.map((x) => presetRow(x.id, x.name, presetNote(x))),
      presetRow('custom', 'Custom address', s.tiles.online_url && presetId === 'custom' ? `${tileHost || 'your tile server'} · zoom up to ${s.tiles.max_zoom}` : 'Your own tile server, typed below')) : null;
    const tileTerms = tilePreset && tilePreset.terms ? h('p', { class: 'setting-note' }, `${tilePreset.name}: free, no key. Its terms (${String(tilePreset.terms).replace(/^https:\/\//, '')}) ask for light use and its credit on every map.`) : null;
    // names and roads over the photos (presets that have them)
    const labelsSwitch = tilePreset && (tilePreset.overlays || []).length
      ? switchField({ label: 'Place names and roads', checked: !!(mt.labels ?? s.tiles.labels), onchange: (on) => quick({ tiles: { labels: on } }, () => labelsSwitch.set(mt.labels ?? s.tiles.labels)) }) : null;
    if (labelsSwitch) labelsSwitch.input.disabled = disabled;
    // the maps viewers may switch to (the chosen one is always offered)
    const allowed = new Set(mt.allowed_styles || s.tiles.styles || []);
    const styleBoxes = sortedPresets.map((x) => {
      const on = x.id === presetId || allowed.has(x.id);
      const input = h('input', { type: 'checkbox', value: x.id, checked: on, disabled: disabled || x.id === presetId, class: 'preset-check',
        onchange: () => {
          const list = styleBoxes.filter((b) => b.checked && b.value !== presetId).map((b) => b.value);
          quick({ tiles: { styles: list } }, () => { for (const b of styleBoxes) b.checked = b.value === presetId || allowed.has(b.value); });
        } });
      input.label = h('label', { class: 'preset-option' }, input, h('span', { class: 'preset-text' }, h('span', { class: 'preset-name' }, x.short || x.name),
        h('span', { class: 'preset-note' }, x.id === presetId ? `${x.name}, the default above` : x.name)));
      return input;
    });
    const stylesList = sortedPresets.length > 1 ? h('div', { class: 'preset-list', role: 'group', 'aria-label': 'Maps people can switch to' }, styleBoxes.map((b) => b.label)) : null;
    const tileUrlField = field({ label: 'Online tile address', name: 'online_url', value: s.tiles.online_url || '', hint: 'An https:// address with {z}, {x} and {y} in it.', attrs: { spellcheck: 'false', autocomplete: 'off', placeholder: 'https://…/{z}/{x}/{y}.png', disabled } });
    tileUrlField.control.addEventListener('input', () => setFieldError(tileUrlField, null));
    const tileForm = form({
      fields: [
        tileUrlField,
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
          await patch({ tiles: { online_url: url || null, attribution: String(v.attribution || '').trim(), max_zoom: v.max_zoom, preset: 'custom' } });
        } catch (err) {
          if (err && err.fields) err.fields = Object.fromEntries(Object.entries(err.fields).map(([k, m]) => [k.replace(/^tiles\./, ''), m]));
          throw err;
        }
      },
    });
    tileForm.dataset.form = 'tiles';
    if (disabled) tileForm.querySelector('[type="submit"]').hidden = true;
    // What each form field holds on the server now: a value that differs was typed and is kept on a redraw.
    for (const c of [...addr.elements, ...tileForm.elements, ...proxyForm.elements]) if (c.name && c.tagName === 'INPUT') c.dataset.saved = c.value;

    const mapsLink = switchField({ label: 'Allow links to an outside map', checked: !!s.outside_links.maps, onchange: (on) => quick({ outside_links: { maps: on } }, () => mapsLink.set(s.outside_links.maps)) });
    const notesLink = switchField({ label: 'Allow release-notes links', checked: !!s.outside_links.release_notes, onchange: (on) => quick({ outside_links: { release_notes: on } }, () => notesLink.set(s.outside_links.release_notes)) });
    const release = switchField({ label: 'Check for new releases', checked: !!s.release_check.enabled, onchange: (on) => quick({ release_check: { enabled: on } }, () => release.set(s.release_check.enabled)) });
    for (const el of [defDist, defTemp, defPress, defRange, lookup, tiles]) el.querySelectorAll('button').forEach((b) => { b.disabled = disabled; });
    for (const el of [mapsLink, notesLink, release]) el.input.disabled = disabled;

    // A new flag for each header: one node appended twice would only show in the second.
    const readOnly = () => (disabled ? flag('Read only') : null);
    replace(serverBox,
      h('section', { class: 'settings', 'aria-labelledby': 'defaults-title' },
        h('h2', { class: 'label settings-label', id: 'defaults-title' }, 'Defaults for everyone', h('span', { class: 'label-note' }, admin ? 'admins only; saved at once' : 'read only for your account'), readOnly()),
        setting('Units and range', 'For people who have not chosen their own. The distance unit can start from the one a car reports from its own screen settings.', defaults),
        setting('Time zone for new cars', 'The zone a new car starts in: its days, weeks and the hours of time-of-use prices.', zoneControl)),
      h('section', { class: 'settings', 'aria-labelledby': 'maps-title' },
        h('h2', { class: 'label settings-label', id: 'maps-title' }, 'Addresses and maps', h('span', { class: 'label-note' }, 'private by default: nothing leaves this server unless an admin turns it on'), readOnly()),
        setting('Address lookup', `How a recorded position becomes a street address. Offline uses the place names bundled with Sentry Mate (${s.geocode.dataset || 'built in'}); nothing leaves the server. Online asks the service below, with positions rounded, and keeps each answer so the same spot is asked once.`,
          lookup, { cover: 'web.settings.general.address-lookup', extra: geoStatus }),
        setting('Address details', 'Language, online service and rounding.', addr, { cls: 'setting-form' }),
        setting('Map tiles', `The pictures under every map. Credit: ${s.tiles.attribution || 'shown on the map'}; zoom up to ${s.tiles.max_zoom}.`, h('div', { class: 'tiles-control' }, tiles, tileNow), { cover: 'web.shell.map-tiles' }),
        offlineRow,
        setting('Online map', 'Only used with online tiles. Satellite is the default; a preset fills in its address, credit and zoom, or type your own address below.', h('div', { class: 'tiles-control' }, tileSource, tileTerms, tileForm), { cls: 'setting-form' }),
        labelsSwitch ? setting('Place names and roads', LABELS_TEXT, labelsSwitch) : null,
        stylesList ? setting('Maps people can switch to', STYLES_TEXT, stylesList, { cls: 'setting-form' }) : null,
        setting('Outbound proxy', 'If this server may only reach the internet through a proxy: online address look-ups and map tiles the server downloads go through it. Viewers’ browsers loading online tiles use their own connection.',
          h('div', { class: 'proxy-control' }, proxyForm, proxyNow), { cls: 'setting-form', cover: 'docs.env.tesla-api.nominatim-proxy' }),
        setting('Outside links', 'Off by default: nothing on a page sends you to another site unless an admin allows it. Links open in a new tab, only when clicked.', h('div', { class: 'switches' }, mapsLink, notesLink)),
        setting('Release check', 'Asks for the newest version number once a day and shows a note in the footer. Off by default.', release)));
  }
  try { drawServer(server); } catch (err) { replace(serverBox, errorView(err)); }
  sync();

  ctx.main.append(
    pageHead({ eyebrow: 'Settings', title: 'Display and units', deck: ctx.info.purpose }),
    mine, divider, dividerNote, serverBox);
  showOfflineHelp(document.getElementById(OFFLINE_ANCHOR));
  ctx.ready();
  // Its own controls change the preferences: they follow onPrefs above; the page is not drawn again.
  return { update: (c) => { sync(); c.ready(); } };
}
