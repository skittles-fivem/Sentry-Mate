// Sentry Mate, "Logbook" UI pack: Settings: Maps and addresses (/settings/maps).
//
// Where the maps and the addresses of every account come from, set once for the whole server by an admin
// (GET / PATCH /api/v1/settings, with its version): the map tiles (offline, or an online preset or address
// with its credit), the offline map file, place names and roads over the photos, the maps people may switch
// to, the address lookup (with its status from GET /api/v1/geocode/status, the online service and the
// rounding), the address language, the outbound proxy and links to other sites. A watch-only account reads
// them here, with every control disabled. Each person's own map colours and map style stay in Settings:
// Display and units.

import { h, replace } from '../shared/dom.js';
import { isSupportedLocale } from '../shared/i18n.js';
import { format, getUnits, FT_PER_M } from '../shared/units.js';
import { formatDateTime } from '../shared/time.js';
import { pageHead, errorView, pageLink, flag } from '../core/ui.js';
import { choice, switchField, field, form, setFieldError } from '../core/form.js';
import { onPrefs } from '../core/prefs.js';
import { LABELS_TEXT, STYLES_TEXT, presetHosts, OFFLINE_ANCHOR, OFFLINE_STEPS, OFFLINE_NOTE, offlineSummary, offlineFileChoices, showOfflineHelp } from '../shared/maptiles.js';

export const id = 'settings-maps';
export const css = true;
export const title = () => 'Settings: Maps and addresses';

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

/** One setting: the name and its explanation on the left, the control on the right. */
function setting(name, desc, control, { cover = null, cls = null, extra = null, id = null } = {}) {
  return h('div', { class: ['setting', cls], cover, id },
    h('div', { class: 'setting-text' },
      h('h3', { class: 'setting-name' }, name),
      desc ? h('p', { class: 'setting-desc' }, desc) : null,
      extra),
    h('div', { class: 'setting-control' }, control));
}

export async function mount(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const [server, geoFirst, mapsFirst] = await Promise.all([
    ctx.api.get('settings'),
    ctx.api.get('geocode/status').catch(() => null),
    ctx.api.get('maps/config').catch(() => null),
  ]);
  let srv = server;
  let geo = geoFirst;
  let maps = mapsFirst;

  const box = h('div', { class: 'settings-server' });

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

  // What the person typed in the forms (and where the focus was) outlives a redraw: a toggle or a fresh
  // copy after someone else's change never empties a half-filled form.
  function keepTyped() {
    const typed = new Map();
    for (const c of box.querySelectorAll('form input[name]')) {
      if (c.dataset.saved != null && c.value !== c.dataset.saved) typed.set(c.closest('form').dataset.form + '/' + c.name, c.value);
    }
    const all = [...box.querySelectorAll('button, input, select, textarea, a[href]')];
    return { typed, focus: all.indexOf(document.activeElement), feet: drawnFeet };
  }
  function putTyped(kept) {
    for (const c of box.querySelectorAll('form input[name]')) {
      const k = c.closest('form').dataset.form + '/' + c.name;
      if (!kept.typed.has(k)) continue;
      if (c.name === 'rounding' && kept.feet !== drawnFeet) continue; // typed in the other unit
      c.value = kept.typed.get(k);
      c.dispatchEvent(new Event('input'));
    }
    if (kept.focus >= 0) [...box.querySelectorAll('button, input, select, textarea, a[href]')][kept.focus]?.focus();
  }
  const redraw = (s) => { const kept = keepTyped(); draw(s); putTyped(kept); };

  /** "Not saved: …" with the server's own reasons, not only its summary. */
  const notSaved = (err) => {
    const why = err && err.fields ? Object.values(err.fields).join(' ') : (err && err.message) || String(err);
    return `Not saved: ${why}`;
  };

  function draw(s) {
    srv = s;
    drawnFeet = feet();
    const disabled = !admin;
    const tilePresets = (maps && maps.tiles && maps.tiles.presets) || [];
    const patch = async (body) => {
      try {
        const next = await ctx.api.patch('settings', { version: srv.version, ...body });
        ctx.flash('Saved for everyone on this server.');
        if (body.geocode || body.outbound) await refreshGeo();
        if (body.tiles) maps = await ctx.api.get('maps/config').catch(() => maps);
        if (ctx.alive()) redraw(next);
        return next;
      } catch (err) {
        if (err && (err.code === 'stale_version' || err.status === 409)) {
          ctx.flash('Someone changed these settings meanwhile; they are shown fresh now. Make your change again.', { kind: 'info' });
          const fresh = await ctx.api.get('settings');
          if (ctx.alive()) redraw(fresh);
          return null;
        }
        throw err;
      }
    };
    // A toggle saves at once. When it is refused, only that control goes back; nothing else on the page
    // is drawn again, so what is typed in the forms stays.
    const quick = (body, undo) => patch(body).catch((err) => {
      undo();
      ctx.flash(notSaved(err), { kind: 'error' });
    });
    function refuse(ctl, old, fieldEl, text) {
      ctl.set(old);
      setFieldError(fieldEl, text);
      fieldEl.control.focus();
      ctx.flash(`Not changed: ${text}`, { kind: 'error' });
    }

    /* ---------- map tiles */
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
    const offlineRow = setting('Offline map file', 'A map of your own area kept on this server: maps then need nothing from outside, and no viewer’s browser asks another site for tiles.',
      h('div', { class: 'tiles-control' }, h('p', { class: 'setting-note' }, offlineSummary(maps && maps.tiles)), offlineFile,
        h('ol', { class: 'setting-note offline-steps' }, OFFLINE_STEPS.map((x) => h('li', null, x))), h('p', { class: 'setting-note' }, OFFLINE_NOTE)),
      { cls: 'setting-form', id: OFFLINE_ANCHOR });

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
    const presetRow = (pid, name, noteText) => {
      const input = h('input', { type: 'radio', name: 'tile-preset', value: pid, checked: pid === presetId, disabled, class: 'preset-radio', onchange: () => pickPreset(pid) });
      radios.push(input);
      return h('label', { class: 'preset-option', dataset: { preset: pid } }, input,
        h('span', { class: 'preset-text' }, h('span', { class: 'preset-name' }, name), noteText ? h('span', { class: 'preset-note' }, noteText) : null));
    };
    const tileSource = tilePresets.length ? h('div', { class: 'preset-list', role: 'radiogroup', 'aria-label': 'Online map' },
      sortedPresets.map((x) => presetRow(x.id, x.name, presetNote(x))),
      presetRow('custom', 'Custom address', s.tiles.online_url && presetId === 'custom' ? `${tileHost || 'your tile server'} · zoom up to ${s.tiles.max_zoom}` : 'Your own tile server, typed below')) : null;
    const tileTerms = tilePreset && tilePreset.terms ? h('p', { class: 'setting-note' }, `${tilePreset.name}: free, no key. Its terms (${String(tilePreset.terms).replace(/^https:\/\//, '')}) ask for light use and its credit on every map.`) : null;
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

    // names and roads over the photos (presets that have them)
    const labelsSwitch = tilePreset && (tilePreset.overlays || []).length
      ? switchField({ label: 'Place names and roads', checked: !!(mt.labels ?? s.tiles.labels), onchange: (on) => quick({ tiles: { labels: on } }, () => labelsSwitch.set(mt.labels ?? s.tiles.labels)) }) : null;
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
        h('span', { class: 'preset-note' }, x.id === presetId ? `${x.name}, the online map above` : x.name)));
      return input;
    });
    const stylesList = sortedPresets.length > 1 ? h('div', { class: 'preset-list', role: 'group', 'aria-label': 'Maps people can switch to' }, styleBoxes.map((b) => b.label)) : null;

    /* ---------- address lookup */
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

    // the online service and the rounding
    const urlField = field({ label: 'Online service address', name: 'url', value: s.geocode.url || '',
      hint: 'Only used with the online service: the https:// address of a reverse-lookup service or of your own copy of one. Positions are rounded first; nothing is sent while lookup is offline.',
      attrs: { placeholder: 'https://…', spellcheck: 'false', autocomplete: 'off', disabled } });
    urlField.control.addEventListener('input', () => setFieldError(urlField, null));
    const maxRound = drawnFeet ? Math.round(1000 * FT_PER_M) : 1000;
    const roundField = field({ label: `Round positions to (${drawnFeet ? 'feet' : 'metres'})`, name: 'rounding', type: 'number', value: roundShown(s.geocode.rounding_m),
      hint: `0 to ${format(maxRound, 'count')} ${drawnFeet ? 'ft' : 'm'}${drawnFeet ? ' (kept in metres on the server, so it may move by a foot)' : ''}. A larger number tells the service less about where the car was, with less exact addresses.`,
      attrs: { min: '0', max: String(maxRound), step: '1', disabled } });
    const serviceForm = form({
      fields: [urlField, roundField],
      submitLabel: 'Save the address service',
      onsubmit: async (v) => {
        const bad = {};
        const url = String(v.url || '').trim();
        if (url && !/^https:\/\/[^\s/]+/.test(url)) bad.url = 'Use an https:// address, or leave it empty.';
        if (v.rounding == null || !Number.isInteger(v.rounding) || v.rounding < 0 || v.rounding > maxRound) bad.rounding = `A whole number of ${drawnFeet ? 'feet' : 'metres'} from 0 to ${format(maxRound, 'count')}.`;
        if (Object.keys(bad).length) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: bad });
        const roundingM = drawnFeet ? Math.min(1000, Math.round(v.rounding / FT_PER_M)) : v.rounding;
        try {
          await patch({ geocode: { url: url || null, rounding_m: roundingM } });
        } catch (err) {
          // Field paths come as geocode.url: put them under their fields.
          if (err && err.fields) {
            err.fields = Object.fromEntries(Object.entries(err.fields).map(([k, m]) => [k.replace(/^geocode\./, '').replace(/^rounding_m$/, 'rounding'), m]));
          } else if (err && !err.message?.startsWith('Not saved')) {
            err.message = `Not saved: ${err.message || 'the server did not answer'}. The addresses stay as they were; try again.`;
          }
          throw err;
        }
      },
    });
    serviceForm.dataset.form = 'service';

    /* ---------- address language */
    const datalistId = 'addr-langs';
    const langField = field({ label: 'Address language', name: 'language', value: s.geocode.language || '',
      hint: 'A language code such as en, de or fr. Addresses already looked up are looked up again.',
      attrs: { maxlength: '10', spellcheck: 'false', autocomplete: 'off', list: datalistId, disabled, required: true } });
    const langNameEl = h('p', { class: 'field-hint lang-name' });
    const showLangName = () => {
      const r = checkLangCode(langField.control.value);
      langNameEl.textContent = r.code ? `${langName(r.code) || r.code}` : '';
    };
    langField.control.addEventListener('input', () => { showLangName(); setFieldError(langField, null); });
    langField.querySelector('.field-control').append(langNameEl);
    showLangName();
    const langForm = form({
      fields: [langField],
      extra: h('datalist', { id: datalistId }, ADDRESS_LANGS.map((c) => h('option', { value: c }, langName(c) || c))),
      submitLabel: 'Save the address language',
      onsubmit: async (v) => {
        const r = checkLangCode(v.language);
        if (r.error) throw Object.assign(new Error('Not saved: check the fields marked.'), { fields: { language: r.error } });
        try {
          await patch({ geocode: { language: r.code } });
        } catch (err) {
          if (err && err.fields) err.fields = Object.fromEntries(Object.entries(err.fields).map(([k, m]) => [k.replace(/^geocode\./, ''), m]));
          else if (err && !err.message?.startsWith('Not saved')) err.message = `Not saved: ${err.message || 'the server did not answer'}. The addresses stay as they were; try again.`;
          throw err;
        }
      },
    });
    langForm.dataset.form = 'lang';

    /* ---------- outbound proxy: one for everything the server fetches itself */
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
    const proxyNow = h('p', { class: 'setting-note' }, s.outbound?.proxy
      ? `Now: through ${proxyHost(s.outbound.proxy)}. Online address look-ups and map tiles the server downloads go through it; nothing else leaves the server.`
      : 'Now: none. The server connects directly, and only when online look-ups or tile downloads are on.');

    /* ---------- outside links */
    const mapsLink = switchField({ label: 'Allow links to an outside map', checked: !!s.outside_links.maps, onchange: (on) => quick({ outside_links: { maps: on } }, () => mapsLink.set(s.outside_links.maps)) });
    const notesLink = switchField({ label: 'Allow release-notes links', checked: !!s.outside_links.release_notes, onchange: (on) => quick({ outside_links: { release_notes: on } }, () => notesLink.set(s.outside_links.release_notes)) });

    // What each form field holds on the server now: a value that differs was typed and is kept on a redraw.
    const forms = [tileForm, serviceForm, langForm, proxyForm];
    for (const f of forms) for (const c of f.elements) if (c.name && c.tagName === 'INPUT') c.dataset.saved = c.value;
    // Read only for anyone but an admin: every control disabled, no Save buttons.
    if (disabled) {
      for (const f of forms) f.querySelector('[type="submit"]').hidden = true;
      for (const el of [tiles, lookup, offlineFile].filter(Boolean)) el.querySelectorAll('button').forEach((b) => { b.disabled = true; });
      for (const el of [labelsSwitch, mapsLink, notesLink].filter(Boolean)) el.input.disabled = true;
    }

    // A new flag for each header: one node appended twice would only show in the second.
    const readOnly = () => (disabled ? flag('Read only') : null);
    replace(box,
      h('section', { class: 'settings', 'aria-labelledby': 'maps-title' },
        h('h2', { class: 'label settings-label', id: 'maps-title' }, 'Maps', h('span', { class: 'label-note' }, 'the pictures under every map, for every account'), readOnly()),
        setting('Map tiles', `The pictures under every map. Credit: ${s.tiles.attribution || 'shown on the map'}; zoom up to ${s.tiles.max_zoom}.`, h('div', { class: 'tiles-control' }, tiles, tileNow), { cover: 'web.shell.map-tiles', id: 'map-tiles' }),
        offlineRow,
        setting('Online map', 'Only used with online tiles. Satellite is the default; a preset fills in its address, credit and zoom, or type your own address below.', h('div', { class: 'tiles-control' }, tileSource, tileTerms, tileForm), { cls: 'setting-form', id: 'online-map' }),
        labelsSwitch ? setting('Place names and roads', LABELS_TEXT, labelsSwitch, { id: 'place-names' }) : null,
        stylesList ? setting('Maps people can switch to', STYLES_TEXT, stylesList, { cls: 'setting-form', id: 'map-styles' }) : null),
      h('section', { class: 'settings', 'aria-labelledby': 'addresses-title' },
        h('h2', { class: 'label settings-label', id: 'addresses-title' }, 'Addresses', h('span', { class: 'label-note' }, 'private by default: nothing leaves this server unless an admin turns it on'), readOnly()),
        setting('Address lookup', `How a recorded position becomes a street address. Offline uses the place names bundled with Sentry Mate (${s.geocode.dataset || 'built in'}); nothing leaves the server. Online asks the service below, with positions rounded, and keeps each answer so the same spot is asked once.`,
          h('div', { class: 'tiles-control' }, lookup, serviceForm), { cls: 'setting-form', cover: ['web.settings.general.address-lookup', 'docs.env.tesla-api.nominatim-proxy'], extra: geoStatus, id: 'address-lookup' }),
        setting('Address language', 'Street and place names of drives, charges and places are written in this language where the data has it, otherwise in the local language.', langForm,
          { cls: 'setting-form', cover: ['web.settings.general.address-language', 'web.settings.general.address-language-error', 'web.settings.general.address-language-invalid'], id: 'address-language' }),
        setting('Outbound proxy', 'If this server may only reach the internet through a proxy: online address look-ups and map tiles the server downloads go through it. Viewers’ browsers loading online tiles use their own connection.',
          h('div', { class: 'proxy-control' }, proxyForm, proxyNow), { cls: 'setting-form', id: 'outbound-proxy' })),
      h('section', { class: 'settings', 'aria-labelledby': 'outside-title' },
        h('h2', { class: 'label settings-label', id: 'outside-title' }, 'Links to other sites', h('span', { class: 'label-note' }, 'off by default'), readOnly()),
        setting('Outside links', 'Nothing on a page sends you to another site unless an admin allows it. Links open in a new tab, only when clicked.', h('div', { class: 'switches' }, mapsLink, notesLink), { id: 'outside-links' })));
  }
  try { draw(server); } catch (err) { replace(box, errorView(err)); }
  // The rounding is typed in the person's own length unit: a new distance unit draws the forms again.
  ctx.onCleanup(onPrefs(() => { if (srv && feet() !== drawnFeet) redraw(srv); }));

  const lede = h('p', { class: 'settings-lede' }, admin
    ? 'For everyone on this server. A switch or choice saves at once; a form saves with its button.'
    : 'Set by an admin for everyone; shown here to read.',
  ' Your own map colours and map style are in ', h('a', { href: ctx.link('settings-display') }, 'Settings: Display and units'), '.');

  ctx.main.append(
    pageHead({ eyebrow: 'Settings', title: 'Maps and addresses', deck: ctx.info.purpose }),
    lede, box);
  showOfflineHelp(document.getElementById(OFFLINE_ANCHOR));
  ctx.ready();
  return {};
}
