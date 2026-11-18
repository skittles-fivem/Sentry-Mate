// Sentry Mate, "Logbook" UI pack: Settings: Display and units (/settings/display).
//
// Your own choices (GET / PATCH /api/v1/me/preferences): units, rated or ideal range, theme, language,
// interface size and following the screen size. Every change saves at once and applies on every page.
// Below the divider, the server's defaults and its address and map settings (GET / PATCH /api/v1/settings,
// admins only to change), with the address lookup's status (GET /api/v1/geocode/status).

import { h, replace } from '../shared/dom.js';
import { getLocale } from '../shared/i18n.js';
import { pageHead, errorView } from '../core/ui.js';
import { choice, switchField, field, form } from '../core/form.js';
import { currentPrefs, onPrefs } from '../core/prefs.js';
import { savePref } from '../core/frame.js';
import { SIZES, sizeState, setSize, sizeNote, display } from '../core/display.js';
import { currentAbout } from '../core/frame.js';

export const id = 'settings-display';
export const css = true;
export const title = () => 'Display and units';

function setting(name, desc, control, { cover = null, cls = null } = {}) {
  return h('div', { class: ['setting', cls], cover },
    h('div', { class: 'setting-text' }, h('h3', { class: 'setting-name' }, name), desc ? h('p', { class: 'setting-desc' }, desc) : null),
    h('div', { class: 'setting-control' }, control));
}

export async function mount(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const [server, geo] = await Promise.all([
    ctx.api.get('settings'),
    ctx.api.get('geocode/status').catch(() => null),
  ]);
  const p = currentPrefs() || {};
  const save = (patch) => savePref(patch, { quiet: false }).catch(() => {});

  // Your choices.
  const units = p.units || {};
  const dist = choice({ label: 'Distance', labelHidden: true, name: 'distance', value: units.distance || 'km', options: [['km', 'Kilometres'], ['mi', 'Miles']], onchange: (v) => save({ units: { distance: v } }) });
  const temp = choice({ label: 'Temperature', labelHidden: true, name: 'temperature', value: units.temperature || 'c', options: [['c', '°C'], ['f', '°F']], onchange: (v) => save({ units: { temperature: v } }) });
  const press = choice({ label: 'Tyre pressure', labelHidden: true, name: 'pressure', value: units.pressure || 'bar', options: [['bar', 'bar'], ['psi', 'psi']], onchange: (v) => save({ units: { pressure: v } }) });
  const range = choice({ label: 'Range', labelHidden: true, name: 'range', value: p.range || 'rated', options: [['rated', 'Rated'], ['ideal', 'Ideal']], onchange: (v) => save({ range: v }) });
  const theme = choice({ label: 'Theme', labelHidden: true, name: 'theme', value: p.theme || 'device', options: [['device', 'Device'], ['light', 'Light (paper)'], ['dark', 'Dark (night ink)']], onchange: (v) => save({ theme: v }) });
  const lang = h('select', { class: 'field-select', id: 'pref-language', 'aria-label': 'Interface language' },
    [['auto', 'From your browser'], ['en', 'English']].map(([v, t]) => h('option', { value: v, selected: (p.language || 'auto') === v }, t)));
  lang.addEventListener('change', () => save({ language: lang.value }));
  const langNote = h('p', { class: 'setting-note' });
  const size = choice({ label: 'Interface size', labelHidden: true, name: 'interface_size', value: sizeState().size, cls: 'segmented-sizes', options: SIZES.map((s) => [s, `${s}%`]),
    onchange: (v) => { setSize({ size: Number(v) }); save({ interface_size: Number(v) }); } });
  const follow = switchField({ label: 'Follow screen size', checked: sizeState().follow, onchange: (on) => { setSize({ follow: on }); save({ follow_screen: on }); } });
  const sizeNoteEl = h('p', { class: 'setting-note' });
  const sync = () => {
    const pr = currentPrefs() || {};
    dist.set(pr.units?.distance || 'km'); temp.set(pr.units?.temperature || 'c'); press.set(pr.units?.pressure || 'bar');
    range.set(pr.range || 'rated'); theme.set(pr.theme || 'device');
    lang.value = pr.language === 'en' ? 'en' : 'auto';
    langNote.textContent = `In use: ${getLocale()}${pr.language && pr.language !== 'auto' ? ' (your choice)' : ', chosen from your browser’s languages'}. Pages are in English; dates and numbers follow this.`;
    size.set(sizeState().size); follow.set(sizeState().follow);
    sizeNoteEl.textContent = sizeNote();
  };
  ctx.onCleanup(onPrefs(sync));
  ctx.onCleanup(display.onChange(sync));
  sync();
  const about = currentAbout();

  const mine = h('section', { class: 'settings', 'aria-labelledby': 'mine-title', cover: 'web.settings.general.autosave' },
    h('h2', { class: 'label settings-label', id: 'mine-title' }, 'Your choices', h('span', { class: 'label-note' }, 'saved at once with your account, on every device')),
    setting('Distance', 'Also sets speed and lengths (metres or feet).', dist, { cover: 'web.settings.general.unit-length' }),
    setting('Temperature', 'Outside, cabin and battery temperatures.', temp, { cover: 'web.settings.general.unit-temperature' }),
    setting('Tyre pressure', null, press, { cover: 'web.settings.general.unit-pressure' }),
    setting('Range', 'Rated follows the official test for the car’s market; ideal is the car’s own, longer figure. The one you pick shows everywhere.', range, { cover: 'web.settings.general.preferred-range' }),
    setting('Theme', 'Paper by day and night ink after dark, following your device, or always one of the two.', theme, { cover: ['shell.theme', 'shots.theme-note', 'web.settings.general.theme'] }),
    setting('Language', 'Dates, times and numbers follow this language.', h('div', null, lang, langNote), { cover: ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation'] }),
    setting('Interface size', 'Text, controls, tables, charts and maps grow or shrink together. With “Follow screen size” on, big screens such as 4K monitors get larger text by themselves. Browser zoom works on top.', h('div', { class: 'size-control' }, size, follow, sizeNoteEl), { cls: 'setting-size' }),
    setting('UI pack', 'The look of the whole app. Admins install other packs.', h('p', { class: 'setting-value' }, about && about.ui_pack ? `${about.ui_pack.name} ${about.ui_pack.version}` : 'Logbook')));

  // The divider: below it, settings that apply to the whole server.
  const divider = h('div', { class: 'settings-divider', role: 'separator', cover: 'web.settings.general.divider' },
    h('span', null, admin ? 'For everyone on this server' : 'Set by an admin for everyone'));

  const serverBox = h('div');
  function drawServer(s) {
    const disabled = !admin;
    const patch = async (body) => {
      try {
        const next = await ctx.api.patch('settings', { version: s.version, ...body });
        ctx.flash('Saved for everyone on this server.');
        drawServer(next);
      } catch (err) {
        if (err && err.code === 'stale_version') { ctx.flash('Someone changed these settings meanwhile; they are shown fresh now.', { kind: 'info' }); drawServer(await ctx.api.get('settings')); return; }
        throw err;
      }
    };
    const defUnits = choice({ label: 'Default units', labelHidden: true, name: 'def-units', value: s.units.distance === 'mi' ? 'mi' : 'km', options: [['km', 'km · °C · bar'], ['mi', 'mi · °F · psi']],
      onchange: (v) => patch({ units: v === 'mi' ? { distance: 'mi', temperature: 'f', pressure: 'psi' } : { distance: 'km', temperature: 'c', pressure: 'bar' } }).catch((e) => ctx.flash(e.message, { kind: 'error' })) });
    const lookup = choice({ label: 'Address lookup', labelHidden: true, name: 'geocode', value: s.geocode.mode, options: [['offline', 'Offline'], ['online', 'Online service']],
      onchange: (v) => patch({ geocode: { mode: v } }).catch((e) => ctx.flash(e.message, { kind: 'error' })) });
    const tiles = choice({ label: 'Map tiles', labelHidden: true, name: 'tiles', value: s.tiles.mode, options: [['offline', 'Offline'], ['online', 'Online tiles']],
      onchange: (v) => patch({ tiles: { mode: v } }).catch((e) => ctx.flash(e.message, { kind: 'error' })) });
    const mapsLink = switchField({ label: 'Allow links to an outside map', checked: !!s.outside_links.maps, onchange: (on) => patch({ outside_links: { maps: on } }).catch((e) => ctx.flash(e.message, { kind: 'error' })) });
    const notesLink = switchField({ label: 'Allow release-notes links', checked: !!s.outside_links.release_notes, onchange: (on) => patch({ outside_links: { release_notes: on } }).catch((e) => ctx.flash(e.message, { kind: 'error' })) });
    const release = switchField({ label: 'Check for new releases', checked: !!s.release_check.enabled, onchange: (on) => patch({ release_check: { enabled: on } }).catch((e) => ctx.flash(e.message, { kind: 'error' })) });
    const addr = form({
      fields: [
        field({ label: 'Address language', name: 'language', value: s.geocode.language || '', hint: 'A language code such as en, de or fr. Changes the street and place names of drives and charges.', attrs: { maxlength: '10', spellcheck: 'false', disabled } }),
        field({ label: 'Online service address', name: 'url', value: s.geocode.url || '', hint: 'Only used with the online service; point it at your own server or a proxy if you have one. Positions are rounded first.', attrs: { placeholder: 'https://…', disabled } }),
        field({ label: 'Round positions to', name: 'rounding_m', type: 'number', value: s.geocode.rounding_m ?? '', hint: 'Metres. Positions are rounded before they leave the server.', attrs: { min: '0', max: '1000', disabled } }),
      ],
      submitLabel: 'Save address settings',
      onsubmit: (v) => patch({ geocode: { language: v.language || null, url: v.url || null, rounding_m: v.rounding_m } }),
      cover: ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid', 'docs.env.tesla-api.nominatim-proxy'],
    });
    if (disabled) addr.querySelector('[type="submit"]').hidden = true;
    for (const el of [defUnits, lookup, tiles]) el.querySelectorAll('button').forEach((b) => { b.disabled = disabled; });
    for (const el of [mapsLink, notesLink, release]) el.input.disabled = disabled;
    const g = geo || {};
    replace(serverBox, h('section', { class: 'settings', 'aria-labelledby': 'server-title' },
      h('h2', { class: 'label settings-label', id: 'server-title' }, 'Server defaults', h('span', { class: 'label-note' }, admin ? 'admins only; your own choices above win' : 'read only for your account')),
      setting('Default units', 'For people who have not chosen their own.', defUnits),
      setting('Address lookup', `Offline uses the place names bundled with Sentry Mate (${s.geocode.dataset || 'built in'}); nothing leaves the server. Online asks the service below, with positions rounded.`,
        h('div', null, lookup, h('p', { class: 'setting-note' }, g.mode ? `Now: ${g.mode}${g.offline_data ? ', offline names present' : ''}${g.cache_size != null ? `, ${g.cache_size} addresses kept` : ''}${g.online && g.online.last_error ? `. Last error: ${g.online.last_error}` : ''}.` : '')),
        { cover: 'web.settings.general.address-lookup' }),
      setting('Address details', null, addr, { cls: 'setting-form' }),
      setting('Map tiles', `Offline tiles are served by Sentry Mate itself. Online tiles come from ${s.tiles.online_url ? new URL(s.tiles.online_url.replace(/\{[a-z]\}/g, '0')).host : 'the tile server'} (${s.tiles.attribution || 'credit shown on the map'}), up to zoom ${s.tiles.max_zoom}.`, tiles, { cover: 'web.shell.map-tiles' }),
      setting('Outside links', 'Off by default: nothing on a page sends you to another site unless an admin allows it.', h('div', { class: 'switches' }, mapsLink, notesLink)),
      setting('Release check', 'Asks for the newest version number once a day and shows a note in the footer. Off by default.', release)));
  }
  try { drawServer(server); } catch (err) { replace(serverBox, errorView(err)); }

  ctx.main.append(
    pageHead({ eyebrow: 'Settings', title: 'Display and units', deck: ctx.info.purpose }),
    mine, divider, serverBox);
  ctx.ready();
  // Its own controls change the preferences: they follow onPrefs above; the page is not drawn again.
  return { update: (c) => c.ready() };
}
