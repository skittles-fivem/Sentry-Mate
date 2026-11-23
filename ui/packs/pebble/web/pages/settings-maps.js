// Sentry Mate, "Pebble" UI pack: Settings: Maps and addresses (/settings/maps).
//
// Where the maps and the addresses of every account come from, set once for the whole server by an admin
// (GET / PATCH /api/v1/settings with its version): map tiles (offline, or an online preset or address with its
// credit), place names and roads over photos, the maps people may switch to, the offline map file, address
// lookup with its status (GET /api/v1/geocode/status), rounding and language, the outbound proxy those online
// look-ups use, and links to other sites. A watch-only account reads them here. Each person's own map colours
// and map style stay in Settings: Display and units.

import { h } from '../shared/dom.js';
import { isSupportedLocale } from '../shared/i18n.js';
import { format, getUnits, FT_PER_M } from '../shared/units.js';
import * as state from '../shared/state.js';
import { tile, chip, note, icon } from '../core/ui.js';
import { segmented, toggle, field, form, setFieldError } from '../core/form.js';
import { onPrefs } from '../core/prefs.js';
import { facts } from '../core/bits.js';
import { group, pickRow, ctlRow, infoRow, notSaved, inOrder } from '../core/settings.js';
import { OFFLINE_ANCHOR, OFFLINE_STEPS, OFFLINE_NOTE, LABELS_TEXT, STYLES_TEXT, offlineSummary, offlineFileChoices, showOfflineHelp, presetHosts } from '../shared/maptiles.js';

export const id = 'settings-maps';
export const shape = [[2, 'h2'], [2, 'h2'], [2, 'h1']];
export const title = () => 'Maps and addresses';

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
  let [srv, geo, mapCfg] = await Promise.all([ctx.api.get('settings'), ctx.api.get('geocode/status').catch(() => null), ctx.api.get('maps/config').catch(() => null)]);
  if (!ctx.alive()) return {};
  // satellite first, then the rest as the server lists them
  const presets = () => [...((mapCfg && mapCfg.tiles && mapCfg.tiles.presets) || [])].sort((a, b) => (b.kind === 'satellite') - (a.kind === 'satellite'));
  const regionNote = (p) => (p.region ? ` ${p.region === 'us' ? 'United States only' : `${p.region} only`}: elsewhere it shows nothing.` : '');
  const presetLine = (p) => `From ${presetHosts(p).join(' and ') || 'the tile server'}, down to zoom ${p.max_zoom}.${regionNote(p)}`;
  const box = h('div', { class: 'contents' });
  const feet = () => getUnits().distance === 'mi';
  let drawnFeet = feet();
  ctx.onCleanup(onPrefs(() => { if (feet() !== drawnFeet && ctx.alive()) draw(srv); }));

  async function patch(body) {
    try {
      const next = await ctx.api.patch('settings', { version: srv.version, ...body });
      if (body.geocode || body.outbound) geo = await ctx.api.get('geocode/status').catch(() => geo);
      if (body.tiles) mapCfg = await ctx.api.get('maps/config').catch(() => mapCfg);
      ctx.toast('Saved for everyone on this server.');
      if (ctx.alive()) draw(next);
      return next;
    } catch (err) {
      if (err && (err.code === 'stale_version' || err.status === 409)) {
        ctx.toast('Someone changed these settings meanwhile; they are shown fresh now. Make your change again.', { kind: 'info' });
        const fresh = await ctx.api.get('settings');
        if (ctx.alive()) draw(fresh);
        return null;
      }
      throw err;
    }
  }
  const quick = (body, undo) => patch(body).catch((err) => { undo(); ctx.toast(notSaved(err), { kind: 'error' }); });

  function draw(s) {
    srv = s;
    drawnFeet = feet();
    const off = !admin;
    const ro = off ? chip('Read only', { tone: 'muted', icon: 'lock' }) : null;
    const tilePresets = presets();

    /* ------------------------------------------------------------ map tiles */
    const tilesSeg = segmented({ name: 'Map tiles', value: s.tiles.mode, options: [['offline', 'Offline'], ['online', 'Online']],
      onchange: (v) => {
        if (v === 'online' && !tilePresets.length && !s.tiles.online_url) { tilesSeg.set(s.tiles.mode); setFieldError(tileUrl, 'Save an https:// tile address with {z}, {x} and {y} first, then choose Online.'); tileUrl.control.focus(); return; }
        // online without an address of its own: the satellite preset (the default)
        quick({ tiles: v === 'online' && chosenId !== 'custom' ? { mode: v, preset: chosenId } : { mode: v } }, () => tilesSeg.set(s.tiles.mode));
      } });
    tilesSeg.querySelectorAll('button').forEach((b) => { b.disabled = off; });
    const mt = (mapCfg && mapCfg.tiles) || {};
    const chosenId = mt.preset || s.tiles.preset || (tilePresets.find((x) => x.url === s.tiles.online_url) || {}).id || (s.tiles.online_url ? 'custom' : 'satellite');
    const tilePreset = tilePresets.find((x) => x.id === chosenId) || null;
    const tileSource = tilePresets.length ? pickRow({ name: 'Online map', sheetTitle: 'Online map',
      sub: tilePreset ? `${tilePreset.name}. ${presetLine(tilePreset)} Free, no key; its terms (${String(tilePreset.terms || '').replace(/^https:\/\//, '')}) ask for light use and its credit on every map.` : 'Your own tile address below. A preset fills in its address, credit and zoom.',
      value: tilePreset ? tilePreset.id : 'custom', disabled: off,
      options: [...tilePresets.map((x) => [x.id, x.name, presetLine(x)]), ['custom', 'Custom address', 'Your own https:// tile address, typed below']],
      sheetNote: 'Each viewer’s browser loads the map pictures from the hosts named, which see the viewer’s IP address, this server’s address and the map area shown.',
      onpick: (v) => {
        // a custom address is typed and saved in the form below (saved as preset "custom")
        if (v === 'custom') { tileSource.set(tilePreset ? tilePreset.id : 'custom'); tileUrl.control.focus(); setFieldError(tileUrl, 'Type your own https:// tile address here, then save it.'); return; }
        return quick({ tiles: { preset: v } }, () => tileSource.set(tilePreset ? tilePreset.id : 'custom'));
      } }) : null;
    // place names and roads over the photos, for a preset that has them
    const labelsRow = tilePreset && (tilePreset.overlays || []).length ? toggle({ label: 'Place names and roads', sub: LABELS_TEXT, checked: mt.labels !== false && s.tiles.labels !== false, disabled: off,
      onchange: (on) => quick({ tiles: { labels: on } }, () => labelsRow.set(!on)) }) : null;
    // the styles viewers may switch between (Map / Satellite)
    const allowed = new Set(mt.allowed_styles || s.tiles.styles || ['satellite', 'osm']);
    const styleRows = tilePresets.map((x) => {
      const t = toggle({ label: x.name, sub: `${x.kind === 'satellite' ? 'Satellite pictures' : 'A drawn map'}. ${presetLine(x)}`, checked: allowed.has(x.id) || x.id === chosenId, disabled: off || x.id === chosenId,
        onchange: (on) => {
          const next = tilePresets.map((y) => y.id).filter((pid) => (pid === x.id ? on : allowed.has(pid)));
          quick({ tiles: { styles: next } }, () => t.set(!on));
        } });
      return t;
    });
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
        try { await patch({ tiles: { online_url: url || null, attribution: String(v.attribution || '').trim(), max_zoom: v.max_zoom, ...(url && !tilePresets.some((x) => x.url === url) ? { preset: 'custom' } : {}) } }); } catch (err) { throw strip(err, /^tiles\./); }
      },
    });
    if (off) tileForm.submitButton.hidden = true;
    // the offline map file: which is in use, (with several) which to use, and how to make one
    const fileOpts = offlineFileChoices(mapCfg && mapCfg.tiles);
    const fileSeg = fileOpts ? segmented({ name: 'Offline map file', value: s.tiles.offline_file || '', options: fileOpts,
      onchange: (v) => quick({ tiles: { offline_file: v || null } }, () => fileSeg.set(s.tiles.offline_file || '')) }) : null;
    if (fileSeg) fileSeg.querySelectorAll('button').forEach((b) => { b.disabled = off; });
    const offlineGroup = group('Offline map file', [
      fileSeg ? ctlRow({ name: 'In use', sub: offlineSummary(mapCfg && mapCfg.tiles), control: fileSeg, stack: true })
        : h('div', { class: 'row row-static' }, h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, 'In use'), h('span', { class: 'row-sub' }, offlineSummary(mapCfg && mapCfg.tiles)))),
      h('div', { class: 'row row-static' }, h('span', { class: 'row-text' }, h('span', { class: 'row-sub' }, 'A map of your own area kept on this server: maps then need nothing from outside, and no viewer’s browser asks another site for tiles.'),
        h('ol', { class: 'row-sub offline-steps' }, OFFLINE_STEPS.map((x) => h('li', null, x))), h('span', { class: 'row-sub' }, OFFLINE_NOTE))),
    ], { id: OFFLINE_ANCHOR });
    const tilesRow = ctlRow({ name: 'Map tiles', sub: s.tiles.mode === 'online'
      ? `Online, from ${(mt.hosts && mt.hosts.length ? mt.hosts : [hostOf(s.tiles.online_url) || 'the tile server below']).join(', ')}. Each viewer’s browser asks them for the map pictures, sending this server’s address as Referer (tile servers ask for it; never a page or a car).`
      : 'Offline: maps are drawn from the offline map file on this server; without one, maps show routes and pins on a plain ground, with a note.', control: tilesSeg, stack: true, cover: 'web.shell.map-tiles' });
    tilesRow.id = 'map-tiles';
    if (labelsRow) labelsRow.id = 'place-names';
    const mapsT = tile({ w: 2, title: 'Maps', icon: 'map', side: ro },
      group(null, [tilesRow, tileSource, labelsRow]),
      tileForm,
      tilePresets.length ? group('Maps people can switch to', styleRows, { note: `${STYLES_TEXT} The default is always on.`, id: 'map-styles' }) : null,
      offlineGroup);

    /* ------------------------------------------------------------ addresses */
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
    const langF = field({ label: 'Address language', name: 'language', value: s.geocode.language || '', hint: 'A code such as en, de or fr. Street and place names are written in it where the data has it.', attrs: { maxlength: '10', spellcheck: 'false', autocomplete: 'off', disabled: off, list: 'sm-langs' } });
    langF.id = 'address-language';
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
      extra: h('datalist', { id: 'sm-langs' }, ['en', 'de', 'fr', 'nl', 'nb', 'sv', 'da', 'fi', 'es', 'it', 'pt', 'pl', 'cs', 'ja', 'ko', 'zh'].map((c) => h('option', { value: c }, langName(c) || c))),
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
    const lookupRow = ctlRow({ name: 'Address lookup', sub: `Offline uses the place names on this server (${s.geocode.dataset || 'built in'}); nothing leaves it. Online asks the service below, rounded, and keeps each answer.`, control: lookup, stack: true,
      cover: ['web.settings.general.address-lookup', 'docs.env.tesla-api.nominatim-proxy'] });
    lookupRow.id = 'address-lookup';
    const addrT = tile({ w: 2, title: 'Addresses', icon: 'pin', side: ro || chip('Private by default', { tone: 'muted' }) },
      group(null, [lookupRow]), geoFacts, addrForm);

    /* ------------------------------------------------------------ the proxy those look-ups use */
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
    const proxyT = tile({ w: 2, title: 'Outbound proxy', icon: 'globe', side: ro },
      note(s.outbound && s.outbound.proxy ? `Now: through ${hostOf(s.outbound.proxy) || 'the proxy'}. Online address look-ups and map tiles the server downloads go through it.` : 'Now: none. The server connects directly, and only when online look-ups or tile downloads are on.'),
      proxyForm);

    /* ------------------------------------------------------------ outside links */
    const mapLinks = toggle({ label: 'Links to an outside map', sub: 'Open a spot in another map site, only when clicked.', checked: !!s.outside_links.maps, disabled: off, onchange: (on) => quick({ outside_links: { maps: on } }, () => mapLinks.set(s.outside_links.maps)) });
    const notes = toggle({ label: 'Release-notes links', sub: 'Link a car software version to its notes on another site.', checked: !!s.outside_links.release_notes, disabled: off, onchange: (on) => quick({ outside_links: { release_notes: on } }, () => notes.set(s.outside_links.release_notes)) });
    const linksT = tile({ w: 2, title: 'Outside links', icon: 'external', side: ro },
      group(null, [mapLinks, notes], { id: 'outside-links' }),
      note('Off by default: nothing on a page sends you to another site unless an admin allows it.'));

    box.replaceChildren(mapsT, addrT, linksT, proxyT);
  }
  draw(srv);

  ctx.view.replaceChildren(
    tile({ w: 2, title: 'For everyone', icon: 'info' },
      note('Where maps and addresses come from, for everyone on this server.'),
      group(null, [
        infoRow({ icon: admin ? 'server' : 'lock', name: admin ? 'One setting for everyone' : 'Set by an admin', sub: admin ? 'Each change is saved at once, for every account.' : 'Shown here to read; only an admin can change them.' }),
        h('a', { class: 'row', href: state.buildUrl('settings-display') },
          h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, 'Your own map colours and map style'), h('span', { class: 'row-sub' }, 'Settings: Display and units')),
          h('span', { class: 'row-side' }, icon('chevronRight', 'row-go'))),
      ])),
    box);
  showOfflineHelp(document.getElementById(OFFLINE_ANCHOR));
  ctx.ready();
  return {};
}
