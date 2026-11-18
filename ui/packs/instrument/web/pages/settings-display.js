// Settings: Display and units (/settings/display): how numbers, words and maps look. The person's own
// choices (PATCH /api/v1/me/preferences) save at once and apply at once; below them, the server's
// defaults and the address and map sources, which an admin changes (PATCH /api/v1/settings).

import { h, replace } from '../shared/dom.js';
import { getUnits, format } from '../shared/units.js';
import { isSupportedLocale } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, panel } from '../core/page.js';
import { field, showApiError } from '../core/form.js';
import { getPrefs, savePrefs, isAdmin, onPrefs } from '../core/prefs.js';
import { getInterface, onInterface } from '../core/display.js';
import { sizePicker, followSwitch, sizeNote, themeButtons } from '../core/account.js';
import { flash } from '../core/flash.js';

export const id = 'settings-display';
export const css = true;

const LOCALES = [['auto', "This browser's language"], ['en-US', 'English (United States)'], ['en-GB', 'English (United Kingdom)'], ['de-DE', 'German number and date style'], ['fr-FR', 'French number and date style'], ['nl-NL', 'Dutch number and date style']].filter(([v]) => v === 'auto' || isSupportedLocale(v));

// a row: the label and its explanation on the left, the control on the right
const setting = (title, text, control, cover = null) => h('div', { class: 'setting', cover }, h('div', null, h('h3', null, title), h('p', null, text)), h('div', { class: 'setting-control' }, control));

// buttons for one choice; onPick(value) saves it
function choice(name, options, current, onPick) {
  const seg = h('div', { class: 'seg', role: 'radiogroup', 'aria-label': name },
    options.map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', role: 'radio', 'aria-checked': String(v === current), 'aria-pressed': String(v === current), dataset: { v: String(v) },
      onclick: async () => { for (const b of seg.children) { b.setAttribute('aria-pressed', String(b.dataset.v === String(v))); b.setAttribute('aria-checked', String(b.dataset.v === String(v))); } await onPick(v); } }, l)));
  return seg;
}

async function savePref(patch, what) {
  try { await savePrefs(patch); flash(`${what} saved.`); } catch (e) { flash(`Could not save ${what.toLowerCase()}: ${e.message}`, { kind: 'error' }); }
}

export async function mount(ctx) {
  const admin = isAdmin();
  let about = null, server = null, geo = null;
  [about, server, geo] = await Promise.all([
    ctx.api.get('about', null, { maxAgeMs: 600000 }).catch(() => null),
    ctx.api.get('settings').catch(() => null),
    ctx.api.get('geocode/status').catch(() => null),
  ]);
  if (!ctx.alive()) return;
  const p = getPrefs() || {};
  const u = getUnits();

  /* ---------- your units */
  const units = panel({ title: 'Units and range', icon: 'odo', side: h('span', { class: 'faint' }, 'Yours; saved at once') },
    setting('Distance and speed', 'Kilometres or miles, everywhere: drives, range, speed, place radius.', choice('Distance', [['km', 'km and km/h'], ['mi', 'miles and mph']], u.distance, (v) => savePref({ units: { distance: v } }, 'Distance unit')), 'web.settings.general.unit-length'),
    setting('Temperature', 'Outside, cabin and battery temperatures.', choice('Temperature', [['c', '°C'], ['f', '°F']], u.temperature, (v) => savePref({ units: { temperature: v } }, 'Temperature unit')), 'web.settings.general.unit-temperature'),
    setting('Tyre pressure', 'All four tyres and the recommended pressure.', choice('Pressure', [['bar', 'bar'], ['psi', 'psi']], u.pressure, (v) => savePref({ units: { pressure: v } }, 'Pressure unit')), 'web.settings.general.unit-pressure'),
    setting('Range', 'Rated range follows the official test cycle; ideal range is what the car calls typical range. Consumption is measured either way, so this only changes which range figure you see.',
      choice('Range', [['rated', 'Rated'], ['ideal', 'Ideal']], u.range, (v) => savePref({ range: v }, 'Range')), 'web.settings.general.preferred-range'),
    h('p', { class: 'note' }, 'Example now: ', h('b', null, format(100, 'distance')), ', ', h('b', null, format(20, 'temperature')), ', ', h('b', null, format(290, 'pressure')), '. The server sends metric values; this page only changes how you see them.'));

  /* ---------- your look */
  const size = sizePicker('set-size');
  const follow = followSwitch('set-follow');
  const pctIn = h('input', { class: 'input input--sm', type: 'number', min: '75', max: '200', step: '1', value: String(getInterface().size), 'aria-label': 'Interface size in percent' });
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
  const lang = h('select', { class: 'select', 'aria-label': 'Language and number style', onchange: (e) => savePref({ language: e.target.value === 'auto' ? 'auto' : e.target.value }, 'Language') },
    LOCALES.map(([v, l]) => h('option', { value: v, selected: (p.language || 'auto') === v || (v === 'en-US' && p.language === 'en') }, l)));
  const packs = [['instrument', 'Instrument'], ['logbook', 'Logbook']];
  const packSel = h('select', { class: 'select', 'aria-label': 'UI pack', onchange: (e) => savePref({ ui_pack: e.target.value }, 'UI pack') },
    packs.map(([v, l]) => h('option', { value: v, selected: (p.ui_pack || 'instrument') === v }, l)));
  const look = panel({ title: 'Look and language', icon: 'monitor', side: h('span', { class: 'faint' }, 'Yours; follows you to every screen') },
    setting('Theme', 'Light, dark, or follow this device. Set before the page is drawn, so there is no flash.', theme.el, ['shell.theme', 'web.settings.general.theme', 'shots.theme-note']),
    setting('Interface size', 'Makes text, controls, tables, charts and maps bigger or smaller together. With Follow screen size on, a very large screen (a native 4K monitor) adds its own step on top. Browser zoom works on top of both.',
      [size.el, h('div', { class: 'row' }, h('label', { class: 'faint', for: 'set-size-pct' }, 'Any size:'), pctIn, h('span', { class: 'faint' }, '%')), sizeErr,
        h('div', { class: 'switch-row switch-row--inline' }, follow.el, h('label', { for: 'set-follow' }, 'Follow screen size')), noteEl]),
    setting('Language', 'The text is in English for now. This sets how numbers and dates are written; the page tells the browser its language.', lang, ['shell.locale', 'web.settings.general.ui-language', 'web.shell.locale-negotiation']),
    setting('UI pack', `The design of the whole app. ${about?.ui_pack?.name ? `This page is the ${about.ui_pack.name} pack ${about.ui_pack.version || ''}.` : ''} Your choice is used the next time the app loads where the server offers that pack.`, packSel));
  pctIn.id = 'set-size-pct';

  /* ---------- server defaults, addresses and maps */
  const serverPanel = h('div', { class: 'settings-stack' });
  function paintServer() {
    if (!server) { replace(serverPanel, panel({ title: 'Server defaults', icon: 'server' }, h('p', { class: 'note' }, 'The server settings could not be loaded.'))); return; }
    const ro = !admin;
    const patch = async (body, what, fields = []) => {
      try {
        server = await ctx.api.patch('settings', { version: server.version, ...body });
        flash(`${what} saved.`);
        for (const f of fields) f.setError(null);
        paintServer();
      } catch (e) {
        if (e.status === 409) { server = await ctx.api.get('settings'); paintServer(); flash('Someone else changed the settings; this page shows theirs now.', { kind: 'info' }); return; }
        if (fields.length) showApiError({ fields, setMessage: (t) => t && flash(t, { kind: 'error' }) }, e); else flash(`Could not save: ${e.message}`, { kind: 'error' });
      }
    };
    const disabled = (el) => { if (ro) el.setAttribute('disabled', ''); return el; };
    const g = server.geocode || {}, t = server.tiles || {};
    const gUrl = field({ label: 'Address service (https)', name: 'gurl', path: 'geocode.url', value: g.url || '', hint: 'Only used when lookup is online. Positions are rounded first (below).', attrs: { placeholder: 'https://', disabled: ro } });
    const gRound = field({ label: 'Round positions to', name: 'grnd', path: 'geocode.rounding_m', type: 'number', value: g.rounding_m, hint: 'metres, before an online lookup', attrs: { min: '0', max: '1000', disabled: ro } });
    const gLang = field({ label: 'Address language', name: 'glang', path: 'geocode.language', value: g.language || 'en', hint: 'A code such as en, de or fr. Changing it looks every stored address up again.', attrs: { maxlength: '10', disabled: ro } });
    const tUrl = field({ label: 'Online tile address (https)', name: 'turl', path: 'tiles.online_url', value: t.online_url || '', hint: 'With {z}, {x} and {y}.', attrs: { disabled: ro } });
    const tAttr = field({ label: 'Map credit', name: 'tattr', path: 'tiles.attribution', value: t.attribution || '', attrs: { disabled: ro } });
    for (const [f, mk] of [[gUrl, (v) => ({ geocode: { url: v || null } })], [gRound, (v) => ({ geocode: { rounding_m: Number(v) } })], [gLang, (v) => ({ geocode: { language: v.trim() } })], [tUrl, (v) => ({ tiles: { online_url: v || null } })], [tAttr, (v) => ({ tiles: { attribution: v } })]]) {
      f.input.addEventListener('change', () => patch(mk(f.input.value), f.el.querySelector('label').textContent, [f]));
    }
    replace(serverPanel,
      h('div', { class: 'settings-divider', role: 'separator', cover: 'web.settings.general.divider' }, h('span', null, 'Server defaults'), h('span', { class: 'faint' }, admin ? 'For everyone; an admin changes them. A person’s own choice above wins.' : 'Set by an admin; shown here to read.')),
      panel({ title: 'Defaults for everyone', icon: 'server' },
        setting('Units', 'What a new account sees until it picks its own units.',
          h('div', { class: 'row' },
            disabled(choice('Default distance', [['km', 'km'], ['mi', 'mi']], server.units?.distance, (v) => patch({ units: { distance: v } }, 'Default distance unit'))),
            disabled(choice('Default temperature', [['c', '°C'], ['f', '°F']], server.units?.temperature, (v) => patch({ units: { temperature: v } }, 'Default temperature unit'))),
            disabled(choice('Default pressure', [['bar', 'bar'], ['psi', 'psi']], server.units?.pressure, (v) => patch({ units: { pressure: v } }, 'Default pressure unit'))))),
        setting('Range', 'Rated or ideal, for accounts that have not chosen.', disabled(choice('Default range', [['rated', 'Rated'], ['ideal', 'Ideal']], server.range, (v) => patch({ range: v }, 'Default range'))))),
      panel({ title: 'Addresses and maps', icon: 'globe', side: h('span', { class: 'faint' }, 'Private by default') },
        setting('Address lookup', 'Offline uses the place names bundled with Sentry Mate: nothing leaves the server. Online asks the service you name, with rounded positions, and keeps a cache.',
          [disabled(choice('Address lookup', [['offline', 'Offline'], ['online', 'Online service']], g.mode, (v) => patch({ geocode: { mode: v } }, 'Address lookup', [gUrl]))),
            h('p', { class: 'setting-note' }, geo ? `Offline data: ${geo.offline_data?.name || '—'}${geo.offline_data?.places ? `, ${geo.offline_data.places.toLocaleString()} places` : ''}. Cache: ${geo.cache_size ?? 0} addresses.${geo.online?.last_error ? ` Last online error: ${geo.online.last_error}.` : ''}` : 'Status not known.'),
            gUrl.el, gRound.el], ['web.settings.general.address-lookup', 'docs.env.tesla-api.nominatim-proxy']),
        setting('Address language', 'Street and place names are written in this language where the data has it.', gLang.el, ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid']),
        setting('Map tiles', 'Offline tiles come from Sentry Mate itself. An online source is asked by each viewer’s browser for the map pictures, so it sees the map area; only turn it on if you accept that.',
          [disabled(choice('Map tiles', [['offline', 'Offline'], ['online', 'Online source']], t.mode, (v) => patch({ tiles: { mode: v } }, 'Map tiles', [tUrl]))), tUrl.el, tAttr.el], 'web.shell.map-tiles'),
        setting('Outside links', 'Links that open another site in a new tab, only when the viewer clicks them.',
          h('div', { class: 'row' },
            disabled(choice('Map links', [[false, 'No map links'], [true, 'Map links']], !!server.outside_links?.maps, (v) => patch({ outside_links: { maps: v === true || v === 'true' } }, 'Map links'))),
            disabled(choice('Release notes', [[false, 'No release notes'], [true, 'Release notes']], !!server.outside_links?.release_notes, (v) => patch({ outside_links: { release_notes: v === true || v === 'true' } }, 'Release-notes links')))))));
  }
  paintServer();

  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'settings-stack' },
      h('p', { class: 'note saving-note', cover: 'web.settings.general.autosave' }, icon('check', 'ico--sm'), ' Every change here is saved at once; there is no Save button. A short note confirms it.'),
      units, look, serverPanel));
  ctx.ready();
}
