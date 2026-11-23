// Settings: Maps and addresses (/settings/maps): where the maps and the addresses of every account come
// from, set once for the whole server by an admin (PATCH /api/v1/settings): map tiles (offline, or an online
// preset or address with its credit), place names and roads over photos, the maps people may switch to, the
// offline map file, address lookup with its rounding and language, and links to other sites. A watch-only
// account reads them here. Each person's own map colours and map style stay in Settings: Display and units.

import { h, replace } from '../shared/dom.js';
import { buildUrl } from '../shared/state.js';
import { icon } from '../core/icons.js';
import { pageHead, panel } from '../core/page.js';
import { field, showApiError } from '../core/form.js';
import { isAdmin } from '../core/prefs.js';
import { flash } from '../core/flash.js';
import { ago } from '../core/fmt.js';
import { LABELS_TEXT, STYLES_TEXT, presetHosts, OFFLINE_ANCHOR, OFFLINE_STEPS, OFFLINE_NOTE, offlineSummary, offlineFileChoices, showOfflineHelp } from '../shared/maptiles.js';

export const id = 'settings-maps';
export const css = true;

// A language code as address data names it: en, de, pt-BR, zh-Hant.
const LANG_RE = /^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$/;
// A web address that may break after each slash on a narrow screen (a <wbr> after it).
const breakable = (s) => s.split('/').flatMap((part, i, all) => (i < all.length - 1 ? [`${part}/`, h('wbr')] : [part]));

// a row: the label and its explanation on the left, the control on the right
const setting = (title, text, control, cover = null, rowId = null) => h('div', { class: 'setting', cover, id: rowId }, h('div', null, h('h3', null, title), h('p', null, text)), h('div', { class: 'setting-control' }, control));

// buttons for one choice; onPick(value) saves it and returns false when the save failed, which puts the
// stored value back. readOnly: the buttons only show the value (disabled).
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
  return seg;
}

export async function mount(ctx) {
  const admin = isAdmin();
  let server = null, geo = null, maps = null;
  try {
    [server, geo, maps] = await Promise.all([
      ctx.api.get('settings'),
      ctx.api.get('geocode/status').catch(() => null),
      ctx.api.get('maps/config').catch(() => null),
    ]);
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const body = h('div', { class: 'settings-pair' });
  function geoStatus() {
    if (!geo) return h('p', { class: 'setting-note' }, 'Address status could not be read.');
    const od = geo.offline_data || {};
    const bits = [];
    bits.push(od.present === false ? 'Offline place names: not installed.' : `Offline place names: ${od.name || 'bundled'}${od.places ? `, ${od.places.toLocaleString()} places` : ''}${od.built_ms ? `, built ${ago(od.built_ms)}` : ''}.`);
    bits.push(` Cache: ${(geo.cache_size ?? 0).toLocaleString()} addresses already looked up.`);
    if (geo.online?.last_error) bits.push(` Last online error: ${geo.online.last_error}.`);
    const retry = geo.online?.retry_ms;
    if (retry != null && Number.isFinite(Number(retry))) {
      // a time (ms since 1970) or, when small, a wait from now
      const at = Number(retry) > 1e12 ? Number(retry) : ctx.api.now() + Number(retry);
      bits.push(at <= ctx.api.now() ? ' Next try: at the next address needed.' : ` Next try ${ago(at)}.`);
    }
    return h('p', { class: 'setting-note' }, icon(geo.online?.last_error ? 'warn' : 'check', 'ico--sm'), ' ', bits.join(''), ' ', h('a', { href: buildUrl('health'), class: 'panel-link' }, 'Address status in Data health ›'));
  }
  function tileStatus(t) {
    const off = maps?.tiles?.offline_data_present;
    if (t.mode === 'online') return h('p', { class: 'setting-note' }, icon('globe', 'ico--sm'), ' Maps load pictures from the online source; its credit shows on every map: ', h('b', null, t.attribution || '(none set)'), '.');
    return h('p', { class: 'setting-note' }, icon(off ? 'check' : 'info', 'ico--sm'), off ? ' An offline map file is installed: maps need nothing from outside.' : ' No offline map file yet: maps draw routes and places on a plain background, with a note saying so. See Offline map file below.');
  }

  function paint() {
    const ro = !admin;
    const patch = async (patchBody, what, fields = []) => {
      try {
        server = await ctx.api.patch('settings', { version: server.version, ...patchBody });
        flash(`${what} saved.`);
        for (const f of fields) f.setError(null);
        [geo, maps] = await Promise.all([ctx.api.get('geocode/status').catch(() => geo), ctx.api.get('maps/config').catch(() => maps)]);
        if (ctx.alive()) paint();
        return true;
      } catch (e) {
        if (e?.name === 'AbortError') return false;
        if (e.status === 409) { server = await ctx.api.get('settings').catch(() => server); if (ctx.alive()) paint(); flash('Someone else changed the settings; this page shows theirs now.', { kind: 'info' }); return false; }
        if (fields.length) showApiError({ fields, setMessage: (t) => t && flash(t, { kind: 'error' }) }, e); else flash(`Could not save: ${e.message}`, { kind: 'error' });
        return false;
      }
    };
    // a server choice: read-only (buttons disabled) for anyone but an admin
    const sChoice = (name, options, current, onPick) => choice(name, options, current, onPick, { readOnly: ro });
    const g = server.geocode || {}, t = server.tiles || {};
    const gUrl = field({ label: 'Address service (https)', name: 'gurl', path: 'geocode.url', value: g.url || '', hint: 'Only used when lookup is online. Positions are rounded first (below); the service never gets a VIN or a car name.', attrs: { placeholder: 'https://', disabled: ro } });
    const gRound = field({ label: 'Round positions to (m)', name: 'grnd', path: 'geocode.rounding_m', type: 'number', value: g.rounding_m, hint: 'metres, from 0 to 1000, before an online lookup', attrs: { min: '0', max: '1000', step: '1', disabled: ro } });
    const gLang = field({ label: 'Address language', name: 'glang', path: 'geocode.language', value: g.language || 'en', hint: 'A code such as en, de, fr or pt-BR. Changing it looks every stored address up again.', attrs: { maxlength: '10', spellcheck: 'false', autocomplete: 'off', disabled: ro } });
    // the online presets, satellite first (the default), then the admin's own address
    const mt = maps?.tiles || {};
    const tilePresets = [...(mt.presets || [])].sort((a, b) => (b.id === 'satellite') - (a.id === 'satellite'));
    const presetId = mt.preset || t.preset || tilePresets.find((p) => p.url === t.online_url)?.id || (t.online_url ? 'custom' : 'satellite');
    const tilePreset = tilePresets.find((p) => p.id === presetId) || null;
    const region = (p) => (p.region ? ` · ${p.region === 'US' ? 'United States only' : `${p.region} only`}` : '');
    const presetMeta = (p) => `${presetHosts(p).join(', ')} · up to zoom ${p.max_zoom}${region(p)}`;
    const presetOpt = (p) => h('span', { class: 'tp-opt' }, h('b', null, p.name), h('span', { class: 'tp-meta' }, presetMeta(p)));
    const tSource = tilePresets.length ? sChoice('Online map', [...tilePresets.map((p) => [p.id, presetOpt(p)]), ['custom', h('span', { class: 'tp-opt' }, h('b', null, 'Custom address'), h('span', { class: 'tp-meta' }, 'Your own tile server, below'))]], presetId, (v) => {
      if (v !== 'custom') return patch({ tiles: { mode: t.mode, preset: v } }, 'Online map', [tUrl, tAttr]);
      if (!tUrl.input.value) { tUrl.input.focus(); return true; }
      return patch({ tiles: { mode: t.mode, preset: 'custom' } }, 'Online map', [tUrl, tAttr]);
    }) : null;
    tSource?.classList.add('seg--stack');
    const tTerms = tilePreset ? h('p', { class: 'setting-note' }, icon('info', 'ico--sm'), ` ${tilePreset.name}: free, no key. Its terms (`, ...breakable(String(tilePreset.terms || '').replace(/^https:\/\//, '')), ') ask for light use and its credit on every map.', tilePreset.region === 'US' ? ' It has pictures of the United States only: elsewhere the map stays blank.' : '') : null;
    // names and roads over the photos (only presets that have them)
    const tLabels = tilePreset?.overlays?.length ? setting('Place names and roads', LABELS_TEXT,
      sChoice('Place names and roads', [['true', 'On'], ['false', 'Off']], String(mt.labels ?? t.labels ?? true), (v) => patch({ tiles: { labels: v === 'true' } }, 'Place names and roads')), null, 'place-names') : null;
    // the maps viewers may switch to on a map page (Map / Satellite)
    const allowed = new Set(mt.allowed_styles || t.styles || []);
    const tStyles = tilePresets.length ? setting('Maps people can switch to', STYLES_TEXT,
      h('div', { class: 'tp-styles', role: 'group', 'aria-label': 'Maps people can switch to' }, tilePresets.map((p) => {
        const idText = `sm-style-${p.id}`;
        const input = h('input', { type: 'checkbox', id: idText, checked: allowed.has(p.id), disabled: ro, onchange: async () => {
          const next = tilePresets.filter((x) => (x.id === p.id ? input.checked : allowed.has(x.id))).map((x) => x.id);
          if (!(await patch({ tiles: { styles: next } }, 'Maps people can switch to'))) input.checked = !input.checked;
        } });
        return h('div', { class: 'switch-row switch-row--inline tp-style' }, h('span', { class: 'switch' }, input, h('i', { 'aria-hidden': 'true' })), h('label', { for: idText }, p.short || p.name));
      })), null, 'map-styles') : null;
    const tUrl = field({ label: 'Online tile address (https)', name: 'turl', path: 'tiles.online_url', value: t.online_url || '', hint: 'With {z}, {x} and {y} in it.', attrs: { placeholder: 'https://', disabled: ro } });
    // the offline map file: how to make one, which is in use, and (with several) which to use
    const tFiles = offlineFileChoices(maps?.tiles);
    const offlineRow = setting('Offline map file', 'A map of your own area kept on this server: maps then need nothing from outside, and no viewer’s browser asks another site for tiles.',
      [h('p', { class: 'setting-note' }, icon(maps?.tiles?.offline_present ? 'check' : 'info', 'ico--sm'), ' ', offlineSummary(maps?.tiles)),
        tFiles ? sChoice('Offline map file', tFiles, t.offline_file || '', (v) => patch({ tiles: { offline_file: v || null } }, 'Offline map file')) : null,
        h('ol', { class: 'setting-note offline-steps' }, OFFLINE_STEPS.map((x) => h('li', null, x))),
        h('p', { class: 'setting-note' }, OFFLINE_NOTE)], null, OFFLINE_ANCHOR);
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
    const roSide = ro ? h('span', { class: 'faint' }, 'Read only') : null;
    replace(body,
      panel({ title: 'Maps', icon: 'globe', side: roSide },
        setting('Map tiles', 'Offline tiles come from Sentry Mate itself. An online map (satellite pictures by default) is asked by each viewer’s browser for the map pictures, so it sees the map area and this server’s address (as Referer, which some tile servers such as OpenStreetMap need; never a page or a car); only turn it on if you accept that.',
          [sChoice('Map tiles', [['offline', 'Offline'], ['online', 'Online source']], t.mode, (v) => patch({ tiles: { mode: v } }, 'Map tiles', [tUrl])), tileStatus(t), tSource, tTerms, tUrl.el, tAttr.el], 'web.shell.map-tiles', 'map-tiles'),
        tLabels, tStyles, offlineRow),
      h('div', { class: 'settings-col' },
        panel({ title: 'Addresses', icon: 'pin', side: roSide || h('span', { class: 'faint' }, 'Private by default') },
          setting('Address lookup', 'Offline uses the place names bundled with Sentry Mate: nothing leaves the server. Online asks the service you name (for example your own address server or a proxy in front of one), with rounded positions, and keeps a cache.',
            [sChoice('Address lookup', [['offline', 'Offline'], ['online', 'Online service']], g.mode, (v) => patch({ geocode: { mode: v } }, 'Address lookup', [gUrl])),
              geoStatus(), gUrl.el, gRound.el], ['web.settings.general.address-lookup', 'docs.env.tesla-api.nominatim-proxy'], 'address-lookup'),
          setting('Address language', 'Street and place names are written in this language where the data has it; otherwise in the local language.', gLang.el, ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid'], 'address-language')),
        panel({ title: 'Outside links', icon: 'share', side: roSide },
          setting('Links to other sites', 'Links that open another site in a new tab, only when the viewer clicks them.',
            h('div', { class: 'row' },
              sChoice('Map links', [['false', 'No map links'], ['true', 'Map links']], String(!!server.outside_links?.maps), (v) => patch({ outside_links: { maps: v === 'true' } }, 'Map links')),
              sChoice('Release notes', [['false', 'No release notes'], ['true', 'Release notes']], String(!!server.outside_links?.release_notes), (v) => patch({ outside_links: { release_notes: v === 'true' } }, 'Release-notes links'))), null, 'outside-links'))));
  }
  paint();

  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'settings-stack' },
      h('p', { class: 'note saving-note' }, icon(admin ? 'check' : 'lock', 'ico--sm'),
        admin ? ' For everyone on this server. Every change is saved at once; a short note confirms it.' : ' Set by an admin for everyone; shown here to read.',
        ' Your own map colours and map style: ', h('a', { href: buildUrl('settings-display'), class: 'panel-link' }, 'Settings: Display and units ›')),
      body));
  showOfflineHelp(document.getElementById(OFFLINE_ANCHOR));
  ctx.ready();
}
