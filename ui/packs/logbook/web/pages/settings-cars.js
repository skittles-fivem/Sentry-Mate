// Sentry Mate, "Logbook" UI pack: Settings: Cars (/settings/cars and /settings/cars/{car}).
//
// Per-car settings, and how each car's CAN Bridge is tied to it. A column of every car the account sees
// (searched and paged by the server, with CSV) picks whose settings are shown. For a car: its name and
// what it reports about itself (GET / PATCH /api/v1/cars/{car}), how its uploads are doing in plain words
// with the next step (health, upload tokens, CAN Bridge keys), its tokens (made here, the secret shown
// once), the keys tied to it with a preferred one, the keys no car has yet, its time zone, its battery
// chemistry with the evidence, and free Supercharging as dated ranges (GET / PATCH .../settings).
// Every change saves at once, with the object's version, and a short note confirms it.
// Without a car: the car list, keys waiting for a car, and how a car appears (the empty state).

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { isValidZone, formatDate, formatDateTime, formatTime, dayKey, formatDayKey } from '../shared/time.js';
import { pageHead, section, empty, errorView, ago, chip, flag, button, icon, stateMark, note, sep } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { field, setFieldError, choice } from '../core/form.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { zoneLabel, zoneShort, count } from '../core/format.js';
import { remount } from '../core/router.js';

export const id = 'settings-cars';
export const css = true;
export const title = () => 'Settings: Cars';

const CHEM = { auto: 'Automatic', lfp: 'LFP', nca_nmc: 'NCA / NMC' };
const CHEM_LONG = { lfp: 'LFP (lithium iron phosphate)', nca_nmc: 'NCA / NMC (nickel based)' };
const SOURCE = { can: 'From the car (CAN)', vin: 'From the VIN', owner: 'Typed by an admin', teslamate: 'From a TeslaMate import' };
const CONFIG_ROWS = [
  ['trim', 'Trim'], ['drivetrain', 'Drivetrain'], ['performance_package', 'Performance package'], ['pack_kwh_code', 'Pack size code'],
  ['wheel', 'Wheels'], ['paint', 'Paint'], ['roof', 'Roof'], ['spoiler', 'Spoiler'], ['right_hand_drive', 'Right-hand drive'],
  ['map_region', 'Map region'], ['supercharging_access', 'Supercharging access'],
];
const yesNo = (v) => (v === true ? 'Yes' : v === false ? 'No' : v == null || v === '' ? '—' : String(v));
const settingsUrl = (carId) => state.buildUrl('settings-cars', carId ? { car: carId } : {});

/* ================================================================== parts */

/** One setting: the name and its explanation on the left, the control on the right. */
function setting(name, desc, control, { cover = null, cls = null } = {}) {
  return h('div', { class: ['sc-setting', cls], cover },
    h('div', { class: 'sc-setting-text' }, h('h3', { class: 'sc-setting-name' }, name), desc ? h('p', { class: 'sc-setting-desc' }, desc) : null),
    h('div', { class: 'sc-setting-control' }, control));
}

function crumbs(car) {
  const sepEl = () => h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/');
  return h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb', cover: 'web.settings.car.breadcrumb' },
    h('a', { href: '/' }, 'Fleet'), sepEl(),
    car ? h('a', { href: settingsUrl(null) }, 'Settings: Cars') : h('span', { 'aria-current': 'page' }, 'Settings: Cars'),
    car ? [sepEl(), h('span', { 'aria-current': 'page' }, car.name)] : null);
}

/** The steps by which a car appears (the empty state, and a reminder under the car list). */
function howCarsAppear() {
  return h('ol', { class: 'sc-steps' },
    h('li', null, h('strong', null, 'Make an upload token. '),
      'For the first car, run ', h('code', { class: 'mono' }, 'sentry-mate token new --car "Name"'),
      ' on the server; it prints the token and the three fields for the app. Later cars can also get one here.'),
    h('li', null, h('strong', null, 'Enter it in the CAN Bridge app '),
      'on the Screenmate One (or another logger such as a Raspberry Pi), in its second upload section, then press Apply & restart.'),
    h('li', null, h('strong', null, 'Wait for the first log. '),
      'The car appears here by itself, with its model and year from the VIN prefix. No Tesla account is needed. Name it and set its time zone.'),
    h('li', null, h('strong', null, 'A second logger in the same car '),
      '(a new Screenmate, a reinstall, a Pi) uploads with a new key. It waits under “Keys waiting for a car” until you tie it to the car.'));
}

/** The car list: every car the account sees, searched and paged by the server. */
function carRail(ctx, currentId) {
  const input = h('input', { class: 'field-input', type: 'search', id: 'sc-car-search', placeholder: 'Name, model or VIN prefix', autocomplete: 'off', spellcheck: 'false' });
  const table = dataTable(ctx, {
    path: 'cars', params: { sort: 'custom' }, limit: 25, itemName: ['car', 'cars'], label: 'Cars, in your order',
    columns: [{
      key: 'car', label: 'Car', th: true,
      cell: (c) => h('a', { class: ['sc-car-link', { 'is-current': c.id === currentId }], href: settingsUrl(c.id), 'aria-current': c.id === currentId ? 'page' : null },
        h('span', { class: 'cell-main' }, c.name || c.vin_prefix || c.id),
        h('span', { class: 'cell-sub' }, [c.model, c.year].filter(Boolean).join(' '), c.vin_prefix ? [sep(), h('span', { class: 'mono' }, c.vin_prefix)] : null),
        c.now ? h('span', { class: 'cell-sub' }, stateMark(c.now.state, { stale: c.now.stale })) : null),
    }],
    rowAttrs: (c) => ({ class: c.id === currentId ? 'is-current' : null }),
    empty: () => empty({ title: input.value.trim() ? `No car matches “${input.value.trim()}”.` : 'No car yet.', text: input.value.trim() ? 'Try part of the name, the model or the VIN prefix.' : null }),
  });
  let timer = 0;
  input.addEventListener('input', () => {
    clearTimeout(timer);
    timer = setTimeout(() => { const s = input.value.trim(); table.reload({ sort: 'custom', search: s || undefined }); }, 200);
  });
  ctx.onCleanup(() => clearTimeout(timer));
  const el = h('aside', { class: 'sc-rail', 'aria-label': 'Cars', cover: 'web.settings.car.car-switcher' },
    h('h2', { class: 'label' }, 'Cars', h('span', { class: 'label-note' }, 'pick one to change its settings')),
    h('div', { class: 'field field-search sc-search' }, h('label', { class: 'field-label', for: 'sc-car-search' }, 'Find a car'),
      h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), input)),
    table.el,
    // The same steps the empty state shows when no car is known yet.
    h('details', { class: 'sc-how', cover: 'web.settings.car.empty-state' },
      h('summary', null, 'How a new car appears'), howCarsAppear()));
  return { el, table };
}

/** Keys no car has yet, with a way to tie each to a car. */
function waitingTable(ctx, { car = null, cars = [], onChange }) {
  const assign = async (k, carId, carName) => {
    const ok = await confirmDialog({
      title: `Tie key ${k.key} to ${carName}?`,
      text: `Its ${count(k.logs_waiting || 0, 'waiting log')} and every later upload from it become ${carName}’s. If both loggers record at once, the preferred key wins; you can change which one below.`,
      yes: `Tie to ${carName}`,
    });
    if (!ok) return;
    try {
      await ctx.api.patch(api.apiPath`can-keys/${k.key}`, { car: carId });
      api.invalidate('');
      ctx.flash(`Key ${k.key} is now ${carName}’s.`);
      await onChange?.();
    } catch (err) { ctx.flash(`Not tied: ${err.message}`, { kind: 'error' }); }
  };
  return dataTable(ctx, {
    path: 'can-keys', params: { assigned: 'false' }, itemName: ['key waiting', 'keys waiting'], label: 'CAN Bridge keys waiting for a car',
    columns: [
      { key: 'key', label: 'Key', th: true, cell: (k) => [h('span', { class: 'cell-main mono' }, k.key), h('span', { class: 'cell-sub' }, k.device || 'Logger not named')], value: (k) => k.key },
      { key: 'first', label: 'First upload', cell: (k) => dateCell(k.first_seen_ms, ctx.tz), value: (k) => k.first_seen_ms },
      { key: 'last', label: 'Last upload', cell: (k) => dateCell(k.last_upload_ms, ctx.tz, true), value: (k) => k.last_upload_ms },
      { key: 'waiting', label: 'Logs kept', num: true, cell: (k) => count(k.logs_waiting || 0, 'log'), value: (k) => k.logs_waiting, hint: 'Logs it uploaded, kept until the key is tied to a car.' },
      { key: 'do', label: 'Tie to', cell: (k) => {
        if (car) return button(`Tie to ${car.name}`, { onclick: () => assign(k, car.id, car.name) });
        if (!cars.length) return '—';
        const sel = h('select', { class: 'field-select sc-assign-select', 'aria-label': `Car for key ${k.key}` }, cars.map((c) => h('option', { value: c.id }, c.name || c.id)));
        return h('div', { class: 'sc-assign' }, sel, button('Tie', { onclick: () => { const c = cars.find((x) => x.id === sel.value); assign(k, c.id, c.name || c.id); } }));
      } },
    ],
    empty: () => empty({ title: 'No key is waiting.', text: 'Every CAN Bridge that uploads belongs to a car.' }),
  });
}

function dateCell(ms, tz, withAgo = false) {
  if (ms == null) return h('span', { class: 'is-missing' }, 'never');
  return [h('span', { class: 'cell-main' }, formatDateTime(ms, tz, { weekday: false, now: api.now() })), withAgo ? h('span', { class: 'cell-sub' }, ago(ms)) : null];
}

/* ================================================================== the page */

export async function mount(ctx) {
  return ctx.car ? mountCar(ctx) : mountIndex(ctx);
}

/* ------------------------------------------------------------------ /settings/cars */

async function mountIndex(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const status = await ctx.api.get('setup/status');
  const head = pageHead({ eyebrow: crumbs(null), title: 'Settings: Cars', deck: ctx.info.purpose });

  if (!status.cars) {
    // The empty state: no car yet, and exactly why.
    const fixes = (status.fixes || []).filter((f) => f.cause !== 'import_running');
    const box = h('section', { class: 'empty sc-empty', cover: 'web.settings.car.empty-state' },
      h('p', { class: 'empty-title' }, 'No car yet.'),
      h('p', { class: 'empty-text' }, 'Sentry Mate needs no Tesla account. A car appears here when its CAN Bridge uploads its first log; you name it then.'),
      howCarsAppear(),
      fixes.length ? h('p', { class: 'note' }, `Now: ${fixes.map((f) => FIX_NOW[f.cause] || f.cause).join(' ')}`) : null);
    ctx.main.append(head, box);
    if (admin && status.keys_waiting) {
      const w = waitingTable(ctx, { cars: [] });
      ctx.main.append(section({ title: 'Keys waiting for a car', note: 'they become a car once one is known' }, w.el));
      await w.load();
    }
    ctx.ready();
    return {};
  }

  const rail = carRail(ctx, null);
  const mainCol = h('div', { class: 'sc-main' },
    h('div', { class: 'sc-choose' },
      h('p', { class: 'sc-choose-title' }, 'Choose a car to change its settings.'),
      h('p', { class: 'sc-choose-text' }, 'Its name, its time zone, its battery chemistry, free Supercharging, and how its CAN Bridge is tied to it. The car picker at the top of the page also works here.')));
  let waiting = null;
  if (admin) {
    const cars = await ctx.api.page('cars', { sort: 'custom' }, { limit: 100 }).then((p) => p.items || []).catch(() => []);
    waiting = waitingTable(ctx, { cars, onChange: () => waiting.reload() });
    mainCol.append(section({ title: 'Keys waiting for a car', note: 'a CAN Bridge uploading with a key no car has yet' }, waiting.el));
  }
  ctx.main.append(head, h('div', { class: 'sc-layout' }, rail.el, mainCol));
  await Promise.all([rail.table.load(), waiting ? waiting.load() : null]);
  ctx.ready();
  return {};
}

const FIX_NOW = {
  no_token: 'No upload token has been made yet.',
  token_unused: 'A token was made but no CAN Bridge has uploaded with it yet.',
  key_waiting: 'A CAN Bridge key is waiting for a car (below).',
  logs_waiting_trust: 'Some logs wait for a trust check.',
};

/* ------------------------------------------------------------------ /settings/cars/{car} */

async function mountCar(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  let car = ctx.car;
  const carPath = api.apiPath`cars/${car.id}`;
  const [settings0, battery] = await Promise.all([
    ctx.api.get(api.apiPath`cars/${car.id}/settings`),
    ctx.api.get(api.apiPath`cars/${car.id}/battery`).catch(() => null),
  ]);
  let settings = settings0;
  let carVersion = car.version;
  const ro = !admin;

  /** PATCH the car (name, time zone); a stale version is fetched fresh and tried once more. */
  async function patchCar(body) {
    try {
      return await ctx.api.patch(carPath, { version: carVersion, ...body });
    } catch (err) {
      if (err && err.code === 'stale_version') {
        const fresh = await ctx.api.get(carPath);
        carVersion = fresh.version;
        return ctx.api.patch(carPath, { version: carVersion, ...body });
      }
      throw err;
    }
  }
  /** PATCH the car's settings with its version, the same way. */
  async function patchSettings(body) {
    const path = api.apiPath`cars/${car.id}/settings`;
    try {
      settings = await ctx.api.patch(path, { version: settings.version, ...body });
    } catch (err) {
      if (err && err.code === 'stale_version') {
        const fresh = await ctx.api.get(path);
        settings = await ctx.api.patch(path, { version: fresh.version, ...body });
      } else throw err;
    }
    api.invalidate(api.apiPath`cars/${car.id}`);
    return settings;
  }
  /** After a change the frame shows (name, zone): the car is read again and the page drawn again. */
  function afterCarChange(text) {
    api.invalidate('');
    ctx.flash(text, { next: true });
    remount();
  }

  /* ---------- the car */
  const nameField = field({ label: 'Name', name: 'name', value: car.name, hint: 'As it shows on every page, in the car picker and on MQTT. 1 to 64 characters.', attrs: { maxlength: '64', required: true, disabled: ro, autocomplete: 'off' } });
  nameField.control.addEventListener('change', async () => {
    const v = nameField.control.value.trim();
    setFieldError(nameField, null);
    if (!v) { setFieldError(nameField, 'A car needs a name.'); return; }
    if (v === car.name) return;
    try { await patchCar({ name: v }); afterCarChange(`Saved: the car is now called ${v}.`); }
    catch (err) { setFieldError(nameField, (err.fields && err.fields.name) || err.message); }
  });
  nameField.control.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); nameField.control.blur(); } });

  const cfg = car.config || {};
  const configRows = CONFIG_ROWS.map(([k, label]) => ({ key: k, label, value: cfg[k] ? cfg[k].value : null, source: cfg[k] ? cfg[k].source : null }));
  const configTable = dataTable(ctx, {
    rows: configRows, itemName: ['detail', 'details'], label: 'What the car reports about itself',
    columns: [
      { key: 'label', label: 'Detail', th: true, cell: (r) => r.label, value: (r) => r.label },
      { key: 'value', label: 'Value', cell: (r) => (r.value == null ? h('span', { class: 'is-missing' }, 'not sent yet') : yesNo(r.value)), value: (r) => yesNo(r.value) },
      { key: 'source', label: 'From', cell: (r) => (r.source ? h('span', { class: 'cell-sub' }, SOURCE[r.source] || r.source) : '—'), value: (r) => (r.source ? SOURCE[r.source] || r.source : '') },
    ],
    csv: { filename: `${car.name || car.id}-car-details.csv` },
  });

  const facts = h('p', { class: 'sc-facts' },
    [car.model, car.year].filter(Boolean).join(' ') || 'Model not known yet', car.trim ? [sep(), car.trim] : null,
    car.vin_prefix ? [sep(), 'VIN ', h('span', { class: 'mono' }, car.vin_prefix), h('span', { class: 'sc-quiet' }, ' (prefix only: model and year come from it)')] : null);

  const carSection = section({ title: 'The car', note: ro ? 'read only for your account' : 'saved as you leave the field', id: 'sc-car' },
    setting('Name', 'Sentry Mate never asks Tesla for a name: the car is called what you type here.', nameField),
    setting('Model', null, facts),
    h('div', { class: 'sc-config' }, h('h3', { class: 'sc-sub' }, 'What the car reports about itself'),
      note('Read from the car’s own configuration message on the CAN bus, else worked out from the VIN prefix.'), configTable.el));

  /* ---------- uploads: outcome, tokens, keys */
  let uploads;
  let refreshUploads = async () => {};
  if (!admin) {
    uploads = section({ title: 'Uploads, tokens and keys', id: 'sc-up' },
      h('p', { class: 'note-serif', cover: 'faq.car-missing.outcomes' }, 'Only an admin sees upload tokens and CAN Bridge keys. How the uploads are doing is on Data health. '),
      h('a', { class: 'link-arrow', href: ctx.link('health') }, 'Data health', icon('arrowRight')));
  } else {
    const outcome = h('div', { class: 'sc-outcome', 'aria-live': 'polite', cover: 'faq.car-missing.outcomes' });
    const tokens = dataTable(ctx, {
      path: api.apiPath`cars/${car.id}/upload-tokens`, itemName: ['token', 'tokens'], label: 'Upload tokens of this car',
      columns: [
        { key: 'id', label: 'Token', th: true, cell: (t) => [h('span', { class: 'cell-main mono' }, t.id), h('span', { class: 'cell-sub' }, `made ${formatDate(t.created_ms, ctx.tz, { weekday: false, now: api.now() })}`)] },
        { key: 'status', label: 'State', cell: (t) => (t.revoked ? chip('Revoked') : t.key || t.last_upload_ms ? chip('In use', { kind: 'on' }) : chip('Not used yet', { kind: 'warn' })) },
        { key: 'key', label: 'Tied to key', cell: (t) => (t.key ? h('span', { class: 'mono' }, t.key) : h('span', { class: 'cell-sub' }, t.revoked ? '—' : 'ties itself on the first upload')) },
        { key: 'last', label: 'Last upload', cell: (t) => dateCell(t.last_upload_ms, ctx.tz, true) },
        { key: 'do', label: '', cell: (t) => (t.revoked ? '' : button('Revoke', { kind: 'quiet', cls: 'button-small', onclick: () => revoke(t) })) },
      ],
      empty: () => empty({ title: 'No upload token yet.', text: 'Make one with the button above and enter it in the CAN Bridge app.' }),
    });
    const keys = dataTable(ctx, {
      path: 'can-keys', params: { car: car.id }, itemName: ['key', 'keys'], label: `CAN Bridge keys tied to ${car.name}`,
      columns: [
        { key: 'key', label: 'Key', th: true, cell: (k) => [h('span', { class: 'cell-main mono' }, k.key), h('span', { class: 'cell-sub' }, k.device || 'Logger not named')] },
        { key: 'first', label: 'First upload', cell: (k) => dateCell(k.first_seen_ms, ctx.tz) },
        { key: 'last', label: 'Last upload', cell: (k) => dateCell(k.last_upload_ms, ctx.tz, true) },
        { key: 'waiting', label: 'Logs waiting', num: true, cell: (k) => count(k.logs_waiting || 0, 'log'), hint: 'Logs waiting for their signature check.' },
        { key: 'pref', label: 'Preferred', cell: (k) => (k.preferred ? flag('Preferred') : button('Make preferred', { cls: 'button-small', onclick: () => prefer(k) })) },
        { key: 'do', label: '', cell: (k) => button('Untie', { cls: 'button-small', onclick: () => untie(k) }) },
      ],
      empty: () => empty({ title: 'No CAN Bridge key is tied to this car yet.', text: 'A key ties itself when the bridge first uploads with one of this car’s tokens, or tie a waiting key below.' }),
    });
    const waiting = waitingTable(ctx, { car, onChange: () => refreshUploads() });
    const health = { value: null };

    async function revoke(t) {
      const ok = await confirmDialog({ title: `Revoke token ${t.id}?`, text: 'The CAN Bridge that uses it stops uploading at its next request. Logs it already sent stay. This cannot be undone: make a new token to upload again.', yes: 'Revoke the token', danger: true });
      if (!ok) return;
      try { await ctx.api.del(api.apiPath`cars/${car.id}/upload-tokens/${t.id}`); ctx.flash(`Token ${t.id} revoked.`); await refreshUploads(); }
      catch (err) { ctx.flash(`Not revoked: ${err.message}`, { kind: 'error' }); }
    }
    async function prefer(k) {
      try { await ctx.api.patch(api.apiPath`can-keys/${k.key}`, { preferred: true }); ctx.flash(`Key ${k.key} is now preferred for ${car.name}.`); await refreshUploads(); }
      catch (err) { ctx.flash(`Not saved: ${err.message}`, { kind: 'error' }); }
    }
    async function untie(k) {
      const ok = await confirmDialog({ title: `Untie key ${k.key} from ${car.name}?`, text: 'Its new uploads then wait under “Keys waiting for a car” until it is tied again. History already recorded stays.', yes: 'Untie the key', danger: true });
      if (!ok) return;
      try { await ctx.api.patch(api.apiPath`can-keys/${k.key}`, { car: null }); ctx.flash(`Key ${k.key} untied.`); await refreshUploads(); }
      catch (err) { ctx.flash(`Not saved: ${err.message}`, { kind: 'error' }); }
    }
    async function makeToken() {
      let made, net = null;
      try {
        [made, net] = await Promise.all([ctx.api.post(api.apiPath`cars/${car.id}/upload-tokens`, {}), ctx.api.get('network').catch(() => null)]);
      } catch (err) { ctx.flash(`No token made: ${err.message}`, { kind: 'error' }); return; }
      await refreshUploads();
      const up = net && (net.inbound || []).find((x) => x.purpose === 'upload');
      const address = `https://${location.hostname}${up && up.port ? `:${up.port}` : ''}/api/can/logs`;
      const secret = h('code', { class: 'mono sc-secret' }, made.secret || '');
      const copied = h('span', { class: 'sc-copied', 'aria-live': 'polite' });
      const copy = (text) => async () => {
        try { await navigator.clipboard.writeText(text); copied.textContent = 'Copied.'; } catch { copied.textContent = 'Copy did not work here: select the text and copy it.'; }
      };
      await openDialog({
        title: `Upload token for ${car.name}`,
        wide: true,
        body: h('div', { class: 'sc-token' },
          h('p', { class: 'dialog-text' }, 'This is the only time the token is shown. Put it into the CAN Bridge app now; if it is lost, revoke it and make a new one.'),
          h('dl', { class: 'sc-token-fields' },
            h('dt', null, 'Token'), h('dd', null, secret, button('Copy', { cls: 'button-small', iconName: 'check', onclick: copy(made.secret || '') })),
            h('dt', null, 'Upload address'), h('dd', null, h('code', { class: 'mono' }, address), button('Copy', { cls: 'button-small', iconName: 'check', onclick: copy(address) })),
            h('dt', null, 'Certificate SHA-256'), h('dd', null, 'Printed by ', h('code', { class: 'mono' }, 'sentry-mate fingerprint'), ' on the server.')),
          copied,
          h('p', { class: 'note' }, 'In the app, fill in its second upload section with these, then press Apply & restart. The token ties itself to the bridge’s key on the first upload.')),
        actions: [{ label: 'I have copied it', value: true, kind: 'primary' }],
      });
    }

    function drawOutcome() {
      const items = [];
      const toks = tokens.items || [];
      const active = toks.filter((t) => !t.revoked);
      const unused = active.filter((t) => !t.key && !t.last_upload_ms);
      const hl = health.value;
      const waitingKeys = waiting.items || [];
      const trustWaiting = (keys.items || []).reduce((n, k) => n + (k.logs_waiting || 0), 0);
      if (!active.length) items.push(['todo', 'No upload token is active.', 'This car’s CAN Bridge cannot upload. Make a token and enter it in the app.', button('Make an upload token', { kind: 'primary', onclick: makeToken })]);
      for (const t of unused) items.push(['todo', `Token ${t.id} has not been used yet.`, `Made ${formatDate(t.created_ms, ctx.tz, { weekday: false, now: api.now() })}. Enter it in the CAN Bridge app and press Apply & restart; the first upload ties it to the bridge’s key.`]);
      if (hl && (hl.reasons || []).includes('upload_silence')) items.push(['warn', hl.last_upload_ms ? `No upload since ${formatDateTime(hl.last_upload_ms, ctx.tz, { weekday: false, now: api.now() })}.` : 'No upload yet.', 'The bridge may be off, out of reach of the server, or its token revoked. Logs are kept in the car and catch up once it reaches the server again.']);
      if (hl && (hl.reasons || []).includes('trust_failed')) items.push(['warn', 'A log failed its signature check.', `${hl.last_log_trust && hl.last_log_trust.reason ? `Reason: ${hl.last_log_trust.reason}. ` : ''}Its time shows as unknown, never as asleep. Data health shows which log.`]);
      if (trustWaiting) items.push(['warn', `${count(trustWaiting, 'log')} wait for a trust check.`, 'They are stored but not decoded until their signature checks out.']);
      if (hl && (hl.reasons || []).includes('rebuild_pending')) items.push(['info', 'History is being rebuilt.', 'Drives, charges and states of the rebuilt days come back in a few seconds.']);
      if (waitingKeys.length) items.push(['todo', `${count(waitingKeys.length, 'key')} no car has ${waitingKeys.length === 1 ? 'is' : 'are'} uploading.`, `If one is a new logger in ${car.name} (a new Screenmate, a reinstall, a Raspberry Pi), tie it below. Its logs are kept until then.`]);
      if (!items.length || (hl && hl.healthy && items.every((x) => x[0] === 'info'))) {
        items.unshift(['ok', 'Uploading normally.', hl ? `Last upload ${hl.last_upload_ms ? formatDateTime(hl.last_upload_ms, ctx.tz, { weekday: false, now: api.now() }) : '—'}; last log ${hl.last_log_trust ? { verified: 'verified', unsigned: 'unsigned', failed: 'failed its check' }[hl.last_log_trust.result] || hl.last_log_trust.result : 'not checked yet'}.` : 'Every token is in use and no key is waiting.']);
      }
      replace(outcome,
        h('ul', { class: 'sc-outcome-list' }, items.map(([kind, t, text, action]) => h('li', { class: ['sc-out', `sc-out-${kind}`] },
          h('span', { class: 'sc-out-mark', 'aria-hidden': 'true' }, icon(kind === 'ok' ? 'check' : kind === 'info' ? 'info' : 'warning')),
          h('div', null, h('p', { class: 'sc-out-title' }, t), h('p', { class: 'sc-out-text' }, text), action ? h('p', { class: 'sc-out-action' }, action) : null)))),
        h('a', { class: 'link-arrow', href: ctx.link('health') }, 'Every upload, check and gap on Data health', icon('arrowRight')));
    }
    refreshUploads = async () => {
      api.invalidate('');
      const [hl] = await Promise.all([
        ctx.api.get(api.apiPath`cars/${car.id}/health`).catch(() => null),
        tokens.reload(), keys.reload(), waiting.reload(),
      ]);
      health.value = hl;
      if (ctx.alive()) drawOutcome();
    };

    uploads = section({ title: 'Uploads, tokens and keys', note: 'how the CAN Bridge is tied to this car', id: 'sc-up',
      actions: button('Make an upload token', { iconName: 'plug', onclick: makeToken }) },
    outcome,
    h('div', { class: 'sc-sub-block' }, h('h3', { class: 'sc-sub' }, 'Upload tokens'),
      note('A token lets one CAN Bridge upload this car’s logs. It is shown once, when it is made, and ties itself to the bridge’s key on the first upload. Revoke a token you no longer use.'),
      tokens.el),
    h('div', { class: 'sc-sub-block' }, h('h3', { class: 'sc-sub' }, 'CAN Bridge keys of this car'),
      note('Each logger signs its logs with its own key: a Screenmate, a reinstall and a Raspberry Pi in the same car are three keys. When two record at the same time, the preferred key’s logs are used and the other only fills its gaps.'),
      keys.el),
    h('div', { class: 'sc-sub-block' }, h('h3', { class: 'sc-sub' }, 'Keys waiting for a car'),
      note('A bridge that uploads with a key no car has yet. Its logs are kept until you tie it to a car.'),
      waiting.el));
    // Loaded after the page is put in place (below).
    uploads.load = async () => {
      const [hl] = await Promise.all([
        ctx.api.get(api.apiPath`cars/${car.id}/health`).catch(() => null),
        tokens.load(), keys.load(), waiting.load(),
      ]);
      health.value = hl;
      drawOutcome();
    };
  }

  /* ---------- time zone */
  const zones = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
  const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
  const tzField = field({ label: 'Time zone', name: 'time_zone', value: car.time_zone || '', hint: 'A name such as Europe/Amsterdam or America/Los_Angeles. Type to see the list.', attrs: { list: 'sc-zones', spellcheck: 'false', autocomplete: 'off', disabled: ro } });
  const zoneList = h('datalist', { id: 'sc-zones' }, zones.map((z) => h('option', { value: z })));
  const tzNow = h('p', { class: 'sc-setting-note' });
  const drawTzNow = () => {
    const tz = car.time_zone;
    tzNow.textContent = tz && isValidZone(tz) ? `Now in ${zoneLabel(tz)}: ${formatTime(api.now(), tz)} ${zoneShort(tz)}, ${formatDayKey(dayKey(api.now(), tz), { weekday: true })}.` : 'Not set: times show in this browser’s zone.';
  };
  drawTzNow();
  const saveZone = async (v) => {
    setFieldError(tzField, null);
    if (!v || !isValidZone(v)) { setFieldError(tzField, 'Not a known time zone. Pick one from the list, for example Europe/Amsterdam.'); return; }
    if (v === car.time_zone) return;
    try { await patchCar({ time_zone: v }); afterCarChange(`Saved: ${car.name} now uses ${v.replace(/_/g, ' ')}.`); }
    catch (err) { setFieldError(tzField, (err.fields && err.fields.time_zone) || err.message); }
  };
  tzField.control.addEventListener('change', () => saveZone(tzField.control.value.trim()));
  const useBrowser = browserZone && browserZone !== car.time_zone && !ro
    ? button(`Use this browser’s zone (${browserZone.replace(/_/g, ' ')})`, { cls: 'button-small', onclick: () => { tzField.control.value = browserZone; saveZone(browserZone); } }) : null;
  const tzSection = section({ title: 'Time zone', id: 'sc-tz', cover: 'docs.env.core.tz' },
    setting('The car’s time zone',
      'Days, weeks, months and the hours of time-of-use tariffs are counted in this zone, and every time of this car is shown in it. Times are stored in UTC, so a new zone regroups the history without changing it.',
      h('div', { class: 'sc-stack' }, tzField, zoneList, tzNow, useBrowser)));

  /* ---------- battery */
  const bc = settings.battery_chemistry || { setting: 'auto' };
  const chemChoice = choice({ label: 'Battery chemistry', labelHidden: true, name: 'chemistry', value: bc.setting || 'auto',
    options: [['auto', 'Automatic'], ['lfp', 'LFP'], ['nca_nmc', 'NCA / NMC']],
    onchange: async (v) => {
      try { await patchSettings({ battery_chemistry: { setting: v } }); ctx.flash(`Saved: battery chemistry ${v === 'auto' ? 'is worked out automatically' : `set to ${CHEM_LONG[v]}`}.`); drawChem(); }
      catch (err) { chemChoice.set(settings.battery_chemistry.setting); ctx.flash(`Not saved: ${err.message}`, { kind: 'error' }); }
    } });
  if (ro) chemChoice.querySelectorAll('button').forEach((b) => { b.disabled = true; });
  const chemNow = h('div', { class: 'sc-chem-now' });
  function drawChem() {
    const c = settings.battery_chemistry || {};
    const inUse = c.setting && c.setting !== 'auto' ? c.setting : c.detected;
    replace(chemNow,
      h('p', { class: 'sc-setting-value' }, 'In use: ', inUse ? CHEM_LONG[inUse] || inUse : 'not known yet',
        h('span', { class: 'sc-quiet' }, c.setting && c.setting !== 'auto' ? ' (set by an admin)' : c.detected ? ' (detected)' : '')),
      h('p', { class: 'sc-setting-note' },
        c.detected ? `Detected: ${CHEM_LONG[c.detected] || c.detected}. Evidence: ${c.evidence || 'not given'}` : 'Not detected yet: the pack’s voltages need a few charges to tell.',
        battery && battery.brick_spread_v != null ? ` Cell voltage spread now ${Math.round(battery.brick_spread_v * 1000)} mV.` : ''),
      c.setting !== 'auto' && c.detected && c.detected !== c.setting ? h('p', { class: 'sc-setting-note sc-warn' }, icon('warning'), ' The detected chemistry differs from the one set here.') : null);
  }
  drawChem();
  const batterySection = section({ title: 'Battery', id: 'sc-bat', cover: 'web.settings.car.lfp-battery' },
    setting('Chemistry',
      'Automatic works it out from the cell voltages and the pack size. LFP packs are meant to charge to 100 % (about once a week), nickel packs to about 80 % for every day; this sets the healthy band on the Battery page and its reminders. Recording is not affected.',
      h('div', { class: 'sc-stack' }, chemChoice, chemNow)),
    h('p', { class: 'sc-more' }, h('a', { class: 'link-arrow', href: ctx.link('battery') }, 'Battery health and charge levels', icon('arrowRight'))));

  /* ---------- charging: free Supercharging */
  const fsBox = h('div', { class: 'sc-fs' });
  let ranges = (settings.free_supercharging || []).map((r) => ({ ...r }));
  const access = cfg.supercharging_access;
  async function saveRanges(rowsEls) {
    const complete = ranges.every((r) => r.from_date);
    if (!complete) return;
    for (let i = 0; i < ranges.length; i += 1) {
      const r = ranges[i];
      if (r.to_date && r.to_date < r.from_date) { setFieldError(rowsEls[i].to, 'Must not be before the start.'); return; }
    }
    try {
      await patchSettings({ free_supercharging: ranges.map((r) => ({ from_date: r.from_date, to_date: r.to_date || null })) });
      ranges = (settings.free_supercharging || []).map((r) => ({ ...r }));
      ctx.flash(ranges.length ? `Saved: ${count(ranges.length, 'free Supercharging range')}.` : 'Saved: no free Supercharging.');
      drawRanges();
    } catch (err) {
      const f = err.fields || {};
      let placed = false;
      for (const [path, msg] of Object.entries(f)) {
        const m = /^free_supercharging\.(\d+)\.(from_date|to_date)$/.exec(path);
        if (m && rowsEls[Number(m[1])]) { setFieldError(rowsEls[Number(m[1])][m[2] === 'from_date' ? 'from' : 'to'], msg); placed = true; }
      }
      if (!placed) ctx.flash(`Not saved: ${err.message}`, { kind: 'error' });
    }
  }
  function drawRanges() {
    const els = [];
    const rows = ranges.map((r, i) => {
      const from = field({ label: 'From', name: `fs-from-${i}`, type: 'date', value: r.from_date || '', attrs: { required: true, disabled: ro } });
      const to = field({ label: 'Until (blank: no end)', name: `fs-to-${i}`, type: 'date', value: r.to_date || '', attrs: { disabled: ro } });
      els[i] = { from, to };
      from.control.addEventListener('change', () => { setFieldError(from, null); r.from_date = from.control.value || null; if (!r.from_date) setFieldError(from, 'Pick the first free day.'); saveRanges(els); });
      to.control.addEventListener('change', () => { setFieldError(to, null); r.to_date = to.control.value || null; saveRanges(els); });
      const status = h('p', { class: 'sc-fs-status' }, r.from_date
        ? `Supercharger sessions that start ${r.to_date ? `from ${formatDayKey(r.from_date, { weekday: false })} to ${formatDayKey(r.to_date, { weekday: false })}` : `on or after ${formatDayKey(r.from_date, { weekday: false })}`} cost nothing.`
        : 'Not saved yet: pick the first free day.');
      return h('li', { class: 'sc-fs-row' }, from, to,
        ro ? null : button('Remove', { cls: 'button-small', onclick: () => { ranges.splice(i, 1); drawRanges(); saveRanges([]); } }),
        status);
    });
    replace(fsBox,
      rows.length ? h('ol', { class: 'sc-fs-list' }, rows) : h('p', { class: 'note-serif' }, 'No free Supercharging: every Supercharger session is priced from the charger’s own billed energy or the place’s tariff.'),
      ro ? null : button('Add a range', { onclick: () => { ranges.push({ from_date: null, to_date: null }); drawRanges(); fsBox.querySelector('.sc-fs-row:last-child input')?.focus(); } }));
  }
  drawRanges();
  const chargingSection = section({ title: 'Charging', id: 'sc-chg', cover: 'web.settings.car.free-supercharging' },
    setting('Free Supercharging',
      'Dated ranges, in the car’s time zone. A Supercharger session that starts inside one gets a cost of zero, marked “free Supercharging”; sessions outside keep their price, so turning it off later does not rewrite history.',
      h('div', { class: 'sc-stack' },
        h('p', { class: 'sc-hint' }, icon('info', 'icon sc-hint-icon'), ' What the car says about Supercharging: ', access && access.value != null
          ? h('strong', null, yesNo(access.value)) : h('span', { class: 'is-missing' }, 'not sent yet'),
        access && access.source ? h('span', { class: 'sc-quiet' }, ` (${SOURCE[access.source] || access.source})`) : null,
        '. The car does not say since when, so the dates are yours.'),
        fsBox)),
    h('p', { class: 'sc-more' }, h('a', { class: 'link-arrow', href: ctx.link('charges') }, 'Charges and their cost', icon('arrowRight'))));

  /* ---------- put it together */
  const rail = carRail(ctx, car.id);
  const mainCol = h('div', { class: 'sc-main', cover: 'web.settings.car.autosave' },
    h('p', { class: 'sc-autosave' }, icon('check'), ro
      ? ' Your account can read these settings; an admin changes them.'
      : ' Every change saves at once, for everyone who sees this car; a short note at the top confirms it. There is no Save button.'),
    carSection, uploads, tzSection, batterySection, chargingSection);
  ctx.main.append(
    pageHead({ eyebrow: crumbs(car), title: car.name, deck: ctx.info.purpose,
      aside: h('p', { class: 'sc-as-of' }, car.last_activity_ms ? ['Last heard ', ago(car.last_activity_ms)] : 'Not heard from yet') }),
    h('div', { class: 'sc-layout' }, rail.el, mainCol));
  await Promise.all([rail.table.load(), configTable.load(), uploads.load ? uploads.load() : null]);
  ctx.ready();
  return {
    async update(c) { drawTzNow(); c.ready(); },
  };
}
