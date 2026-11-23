// Sentry Mate, "Logbook" UI pack: Settings: Cars (/settings/cars and /settings/cars/{car}).
//
// Per-car settings, and how each car's CAN Bridge is tied to it. A column of every car the account sees
// (searched and paged by the server, with CSV) picks whose settings are shown. For a car: its name and
// what it reports about itself (GET / PATCH /api/v1/cars/{car}), how its uploads are doing in plain words
// with the next step (health, upload tokens, CAN Bridge keys), its tokens (made here, the secret shown
// once), the keys tied to it with a preferred one, the keys no car has yet, its time zone, its battery
// chemistry with the evidence, and free Supercharging as dated ranges (GET / PATCH .../settings). An admin
// marks a car sold, with an optional date, or not sold again (PATCH sold, sold_date).
// Every change saves at once, with the object's version, and a short note confirms it.
// Without a car: the car list, keys waiting for a car, and how a car appears (the empty state).

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { convert, toMetric, unitLabel } from '../shared/units.js';
import { isValidZone, formatDate, formatDateTime, formatTime, dayKey, formatDayKey, zoneSourceText } from '../shared/time.js';
import { pageHead, section, empty, errorView, ago, chip, flag, button, icon, stateMark, note, sep } from '../core/ui.js';
import { labelOf } from '../core/sources.js';
import { dataTable } from '../core/table.js';
import { field, setFieldError, choice, switchField } from '../core/form.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { zoneLabel, zoneShort, count } from '../core/format.js';
import { remount } from '../core/router.js';
import { deviceName } from '../shared/devices.js';
import { deviceText, renameButton } from './device-name.js';

export const id = 'settings-cars';
export const css = true;
export const title = () => 'Settings: Cars';

const CHEM = { auto: 'Automatic', lfp: 'LFP', nca_nmc: 'NCA / NMC' };
const CHEM_LONG = { lfp: 'LFP (lithium iron phosphate)', nca_nmc: 'NCA / NMC (nickel based)' };
const SOURCE = { can: 'From SMO (the car’s CAN bus)', vin: 'From the VIN', owner: 'Typed by an admin', teslamate: 'From a TeslaMate import', tesla_api: 'From the Tesla API' };
/** A config value's origin: its sources in words (none: an admin typed it), else the old source word. */
const fromWords = (x) => (!x ? null : Array.isArray(x.sources) ? (x.sources.length ? labelOf(x.sources) : (x.source === 'vin' ? SOURCE.vin : SOURCE.owner)) : SOURCE[x.source] || x.source || null);
const CONFIG_ROWS = [
  ['trim', 'Trim'], ['drivetrain', 'Drivetrain'], ['performance_package', 'Performance package'], ['pack_kwh_code', 'Pack size code'],
  ['wheel', 'Wheels'], ['paint', 'Paint'], ['roof', 'Roof'], ['spoiler', 'Spoiler'], ['right_hand_drive', 'Right-hand drive'],
  ['map_region', 'Map region'], ['supercharging_access', 'Supercharging access'],
];
const TRUST_REASONS = {
  signature_mismatch: 'its signature does not match the bridge’s key',
  chain_break: 'it does not follow the log before it in the chain',
  unknown_key: 'it was signed with a key the server does not know',
  bad_format: 'the file could not be read',
};
const TRUST_WORDS = { verified: 'was verified', unsigned: 'came without a signature', failed: 'failed its check' };
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
      'An admin presses “Make an upload token” here (for a car not seen yet: under “Upload tokens for a new car”). It shows the three fields for the app, ready to copy: the server address, the token and the certificate SHA-256.'),
    h('li', null, h('strong', null, 'Enter it in the CAN Bridge app '),
      'on the Screenmate One (or another logger such as a Raspberry Pi), in its second upload section, then press Apply & restart.'),
    h('li', null, h('strong', null, 'Wait for the first log. '),
      'The car appears here by itself, with its model and year from the VIN prefix. No Tesla account is needed. Name it and set its time zone.'),
    h('li', null, h('strong', null, 'A second logger in the same car '),
      '(a new Screenmate, a reinstall, a Pi) uploads with a new key. It waits under “Keys waiting for a car” until you tie it to the car.'));
}

/**
 * The three fields of the CAN Bridge app's "Second Sentry Six Manager" section for a token just made
 * (POST .../upload-tokens answers them in `app`), each with a Copy button. The token is shown this once.
 */
async function tokenDialog(ctx, made, forWhat) {
  const app = made.app || {};
  const copied = h('span', { class: 'sc-copied', 'aria-live': 'polite' });
  const copy = (what, text) => async () => {
    try { await navigator.clipboard.writeText(text); copied.textContent = `${what} copied.`; } catch { copied.textContent = 'Copy did not work here: select the text and copy it.'; }
  };
  const row = (name, hint, value, cls = 'mono') => [
    h('dt', null, name),
    h('dd', null, value ? [h('code', { class: cls }, value), button('Copy', { cls: 'button-small', onclick: copy(name, value) })] : h('span', { class: 'cell-sub' }, 'The server did not send it: open Settings, Server and access.'),
      hint ? h('span', { class: 'sc-token-hint' }, hint) : null)];
  await openDialog({
    title: `Upload token for ${forWhat}`,
    wide: true,
    body: h('div', { class: 'sc-token' },
      h('p', { class: 'dialog-text' }, 'This is the only time the token is shown. Put these three into the CAN Bridge app now; if the token is lost, revoke it and make a new one.'),
      h('dl', { class: 'sc-token-fields' },
        row('Server address', 'Where the bridge sends its logs (the upload port).', app.address || null),
        row('Token', 'The secret that lets this bridge upload.', app.token || made.secret || null, 'mono sc-secret'),
        row('Certificate SHA-256', 'So the bridge knows it talks to this server and no other.', app.certificate_sha256 || null)),
      copied,
      h('ol', { class: 'sc-token-steps' },
        h('li', null, 'On the Screenmate One (or the other logger), open the CAN Bridge app.'),
        h('li', null, 'In its “Second Sentry Six Manager” section, fill in the three fields above.'),
        h('li', null, 'Press Apply & restart. The token ties itself to the bridge’s key on the first upload.'))),
    actions: [{ label: 'I have copied it', value: true, kind: 'primary' }],
  });
}

/** Upload tokens for a car the server has not seen yet (GET / POST /upload-tokens): admins only. */
function newCarTokens(ctx) {
  const table = dataTable(ctx, {
    path: 'upload-tokens', itemName: ['token', 'tokens'], label: 'Upload tokens for a new car',
    columns: [
      { key: 'id', label: 'Token', th: true, cell: (t) => [h('span', { class: 'cell-main mono' }, t.id), h('span', { class: 'cell-sub' }, `made ${formatDate(t.created_ms, ctx.tz, { weekday: false, now: api.now() })}`)] },
      { key: 'status', label: 'State', cell: (t) => (t.revoked ? chip('Revoked') : chip('Waiting for the first upload', { kind: 'warn' })) },
      { key: 'do', label: '', cell: (t) => (t.revoked ? '' : button('Revoke', { kind: 'quiet', cls: 'button-small', onclick: () => revoke(t) })) },
    ],
    empty: () => empty({ title: 'No token waits for a new car.', text: 'Make one with the button above, then enter it in the CAN Bridge app.' }),
  });
  async function make() {
    let made;
    try { made = await ctx.api.post('upload-tokens', {}); } catch (err) { ctx.flash(`No token made: ${err.message}`, { kind: 'error' }); return; }
    await table.reload({});
    await tokenDialog(ctx, made, 'a new car');
  }
  async function revoke(t) {
    const ok = await confirmDialog({ title: `Revoke token ${t.id}?`, text: 'A CAN Bridge that uses it can no longer upload. This cannot be undone: make a new token to upload again.', yes: 'Revoke the token', danger: true });
    if (!ok) return;
    try { await ctx.api.del(api.apiPath`upload-tokens/${t.id}`); ctx.flash(`Token ${t.id} revoked.`); } catch (err) { ctx.flash(`Not revoked: ${err.message}`, { kind: 'error' }); }
    await table.reload({});
  }
  const el = section({ title: 'Upload tokens for a new car', note: 'the first log with one makes the car', id: 'sc-new-tokens',
    actions: button('Make an upload token', { kind: 'primary', iconName: 'plug', onclick: make }) },
  note('For a car Sentry Mate has not seen yet. The first log sent with the token makes the car here, named after its model until you name it; the token then moves to that car’s page.'),
  table.el);
  return { el, load: () => table.load(), make };
}

/** The car list: every car the account sees, searched and paged by the server. */
function carRail(ctx, current) {
  const currentId = current ? current.id : null;
  // The car being edited, pinned above the list while it is not among the rows loaded (a long list, a search).
  const editing = h('div', { class: 'sc-editing', hidden: true });
  const drawEditing = () => {
    const shown = (table.items || []).some((c) => c.id === currentId);
    editing.hidden = !current || shown;
    if (!current || shown) { replace(editing); return; }
    replace(editing,
      h('p', { class: 'sc-editing-label' }, 'Editing'),
      h('a', { class: ['sc-car-link', 'is-current'], href: settingsUrl(current.id), 'aria-current': 'page' },
        h('span', { class: 'cell-main' }, current.name || current.vin_prefix || current.id),
        h('span', { class: 'cell-sub' }, [current.model, current.year].filter(Boolean).join(' '), current.vin_prefix ? [sep(), h('span', { class: 'mono' }, current.vin_prefix)] : null)),
      h('p', { class: 'sc-editing-note' }, input.value.trim() ? 'Not among the cars that match the search.' : 'Further down the list; the list below starts at the top.'));
  };
  const input = h('input', { class: 'field-input', type: 'search', id: 'sc-car-search', placeholder: 'Name, model or VIN prefix', autocomplete: 'off', spellcheck: 'false' });
  const table = dataTable(ctx, {
    path: 'cars', params: { sort: 'custom' }, limit: 25, itemName: ['car', 'cars'], label: 'Cars, in your order',
    columns: [{
      key: 'car', label: 'Car', th: true,
      cell: (c) => h('a', { class: ['sc-car-link', { 'is-current': c.id === currentId }], href: settingsUrl(c.id), 'aria-current': c.id === currentId ? 'page' : null },
        h('span', { class: 'cell-main' }, c.name || c.vin_prefix || c.id),
        h('span', { class: 'cell-sub' }, [c.model, c.year].filter(Boolean).join(' '), c.vin_prefix ? [sep(), h('span', { class: 'mono' }, c.vin_prefix)] : null),
        state.isSold(c) ? h('span', { class: 'cell-sub' }, flag('Sold'), c.sold_date ? ` ${formatDayKey(c.sold_date, { weekday: false })}` : null)
          : c.now ? h('span', { class: 'cell-sub' }, stateMark(c.now.state, { stale: c.now.stale })) : null),
    }],
    rowAttrs: (c) => ({ class: [{ 'is-current': c.id === currentId, 'is-sold': state.isSold(c) }] }),
    onRows: () => drawEditing(),
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
    editing,
    table.el,
    // The same steps the empty state shows when no car is known yet.
    h('details', { class: 'sc-how', cover: 'web.settings.car.empty-state' },
      h('summary', null, 'How a new car appears'), howCarsAppear()));
  return { el, table };
}

/**
 * Pick any car, however many there are: a search over every car the account sees (cars?search=),
 * the first matches listed. Resolves with the chosen car, or null.
 */
function pickCar(ctx, { title, text }) {
  const input = h('input', { class: 'field-input', type: 'search', id: 'sc-pick-search', placeholder: 'Name, model or VIN prefix', autocomplete: 'off', spellcheck: 'false' });
  const list = h('ul', { class: 'sc-pick-list', 'aria-live': 'polite' });
  const status = h('p', { class: 'sc-pick-status' });
  let found = [];
  let seq = 0;
  let timer = 0;
  let chosen = null;
  return openDialog({
    title,
    initialFocus: '#sc-pick-search',
    body: (close) => {
      const search = async () => {
        const n = ++seq;
        const s = input.value.trim();
        status.textContent = 'Looking…';
        try {
          const page = await ctx.api.page('cars', { sort: 'custom', search: s || undefined }, { limit: 20 });
          if (n !== seq) return;
          found = page.items || [];
          replace(list, found.map((c) => h('li', null, h('button', { type: 'button', class: 'sc-pick-car', onclick: () => { chosen = c; close('picked'); } },
            h('span', { class: 'cell-main' }, c.name || c.vin_prefix || c.id),
            h('span', { class: 'cell-sub' }, [c.model, c.year].filter(Boolean).join(' ') || 'Model not known yet', c.vin_prefix ? [sep(), h('span', { class: 'mono' }, c.vin_prefix)] : null)))));
          status.textContent = !found.length ? (s ? `No car matches “${s}”. Try part of the name, the model or the VIN prefix.` : 'No car yet.')
            : page.next ? `The first ${found.length} ${s ? 'matches' : 'cars'}; type to narrow the list.` : `${count(found.length, s ? 'match' : 'car')}.`;
        } catch (err) {
          if (n === seq) status.textContent = `The cars could not be loaded: ${err.message}`;
        }
      };
      input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(search, 200); });
      search();
      return h('div', { class: 'sc-pick' },
        text ? h('p', { class: 'dialog-text' }, text) : null,
        h('div', { class: 'field field-search' }, h('label', { class: 'field-label', for: 'sc-pick-search' }, 'Find the car'),
          h('div', { class: 'field-control' }, icon('search', 'icon field-icon'), input)),
        status, list);
    },
    actions: [{ label: 'Cancel', value: null, kind: 'quiet' }],
    signal: ctx.signal,
  }).then((v) => { clearTimeout(timer); return v === 'picked' ? chosen : null; });
}

/** Keys no car has yet, with a way to tie each to a car. */
function waitingTable(ctx, { car = null, onChange }) {
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
      { key: 'key', label: 'Key', th: true, cell: (k) => [h('span', { class: 'cell-main mono' }, k.key), deviceText(k, 'cell-sub')], value: (k) => k.key },
      { key: 'first', label: 'First upload', cell: (k) => dateCell(k.first_seen_ms, ctx.tz), value: (k) => k.first_seen_ms },
      { key: 'last', label: 'Last upload', cell: (k) => dateCell(k.last_upload_ms, ctx.tz, true), value: (k) => k.last_upload_ms },
      { key: 'waiting', label: 'Logs kept', num: true, cell: (k) => count(k.logs_waiting || 0, 'log'), value: (k) => k.logs_waiting, hint: 'Logs it uploaded, kept until the key is tied to a car.' },
      { key: 'do', label: 'Tie to', cell: (k) => {
        const rename = renameButton(ctx, k, () => onChange?.());
        if (car) return h('span', { class: 'dev-acts' }, button(`Tie to ${car.name}`, { onclick: () => assign(k, car.id, car.name) }), rename);
        return h('span', { class: 'dev-acts' }, button('Choose the car…', { iconName: 'search', cls: 'button-small', onclick: async () => {
          const c = await pickCar(ctx, { title: `Tie key ${k.key} to a car`, text: `Find the car this logger is in (${deviceName(k).text}). Every car is searched, however many there are.` });
          if (c) await assign(k, c.id, c.name || c.id);
        } }), rename);
      } },
    ],
    empty: () => empty({ title: 'No key is waiting.', text: 'Every CAN Bridge that uploads belongs to a car.' }),
  });
}

/* ------------------------------------------------------------------ data sources */

const SOURCE_KINDS = ['smo', 'tesla_api', 'teslafi_api', 'teslamate_local'];
const SOURCE_NAME = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'local TeslaMate' };

/**
 * Data sources (GET / PATCH .../settings source_order, import_gap_fill): the car's four live inputs in their
 * priority order, each with whether it feeds the car and its newest data; an admin reorders them by dragging
 * the grip or with the arrow buttons, and each move saves at once. Below: gap filling from imported history,
 * the car's own choice or the server's, and the way to the per-span overlap choices (Import).
 */
function dataSources(ctx, { car, ro, getSettings, patchSettings }) {
  const orderOf = (s) => {
    if (Array.isArray(s.source_order) && s.source_order.length) return [...s.source_order];
    const fromInputs = (s.inputs || []).map((i) => i.kind).filter((k) => SOURCE_KINDS.includes(k));
    return fromInputs.length ? [...fromInputs, ...SOURCE_KINDS.filter((k) => !fromInputs.includes(k))] : [...SOURCE_KINDS];
  };
  const defaultOf = (s) => (Array.isArray(s.source_order_default) && s.source_order_default.length ? s.source_order_default : SOURCE_KINDS);
  const inputOf = (s, kind) => (s.inputs || []).find((i) => i.kind === kind) || { kind, label: SOURCE_NAME[kind] || kind, on: false, last_ms: null };
  const nameOf = (s, kind) => inputOf(s, kind).label || SOURCE_NAME[kind] || kind;
  const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

  let shown = orderOf(getSettings()); // the order on screen (ahead of the server while a move saves)
  let busy = false;
  const list = h('ol', { class: 'sc-src-list', 'aria-label': 'Live inputs, highest priority first' });
  const problem = h('p', { class: 'sc-fs-note sc-fs-note-conflict', role: 'status', hidden: true });
  const resetBox = h('p', { class: 'sc-src-reset' });
  const showProblem = (text) => { problem.hidden = !text; replace(problem, text ? [icon('warning'), ' ', text] : null); };

  function moveButton(kind, dir, i) {
    const name = nameOf(getSettings(), kind);
    const label = `Move ${name} ${dir}`;
    const off = dir === 'up' ? i === 0 : i === shown.length - 1;
    return h('button', { type: 'button', class: ['button', 'button-quiet', 'sc-src-btn', `sc-src-${dir}`], 'aria-label': label, title: label, disabled: off,
      onclick: () => { if (!busy) move(kind, dir === 'up' ? i - 1 : i + 1, dir); } },
    icon('chevronDown', dir === 'up' ? 'icon sc-flip' : 'icon'));
  }

  function draw(focus = null) {
    const s = getSettings();
    replace(list, shown.map((kind, i) => {
      const inp = inputOf(s, kind);
      const li = h('li', { class: ['sc-src', { 'is-off': !inp.on, 'is-ro': ro }], dataset: { kind } },
        h('span', { class: 'sc-src-rank', 'aria-hidden': 'true' }, String(i + 1)),
        ro ? null : h('span', { class: 'sc-src-grip', 'aria-hidden': 'true', title: 'Drag to change the order', onpointerdown: (e) => startDrag(e, li) }, icon('menu')),
        h('div', { class: 'sc-src-text' },
          h('p', { class: 'sc-src-name' }, h('span', { class: 'src-dots', 'aria-hidden': 'true' }, h('span', { class: ['src-dot', `src-dot-${kind}`] })), nameOf(s, kind),
            h('span', { class: 'sr-only' }, `, priority ${i + 1} of ${shown.length}`)),
          h('p', { class: 'sc-src-status' }, inp.on ? chip('On', { kind: 'on' }) : chip('Off'), ' ',
            h('span', { class: 'sc-src-age' }, inp.last_ms ? ['Newest data ', ago(inp.last_ms)] : 'No data yet'))),
        ro ? null : h('div', { class: 'sc-src-move' }, moveButton(kind, 'up', i), moveButton(kind, 'down', i)));
      return li;
    }));
    const def = defaultOf(s);
    replace(resetBox, ro || same(shown, def) ? null : [
      button('Reset to default order', { cls: 'button-small', iconName: 'reset', onclick: () => { if (!busy) save([...def], { reset: true }); } }),
      h('span', { class: 'sc-quiet' }, ` ${def.map((k) => nameOf(s, k)).join(', ')}`)]);
    resetBox.hidden = !resetBox.firstChild;
    if (focus) {
      const li = list.querySelector(`li[data-kind="${focus.kind}"]`);
      const b = li && (li.querySelector(`.sc-src-${focus.dir}:not(:disabled)`) || li.querySelector('.sc-src-btn:not(:disabled)'));
      b?.focus();
    }
  }

  async function save(next, { reset = false, focus = null } = {}) {
    const before = shown;
    if (same(next, before)) return;
    shown = next;
    busy = true;
    showProblem(null);
    draw(focus);
    try {
      const s = await patchSettings({ source_order: reset ? null : next });
      shown = orderOf(s);
      const names = shown.map((k) => nameOf(s, k));
      ctx.flash(reset ? `Saved: the default order again (${names.join(', ')}).` : `Saved: ${names[0]} first, then ${names.slice(1).join(', ')}.`);
    } catch (err) {
      shown = orderOf(getSettings());
      if (err && err.code === 'conflict') {
        ctx.flash(`Not saved: someone else changed the order of ${car.name}’s sources at the same time. That is what is shown now; change it again if yours is still needed.`, { kind: 'error' });
      } else {
        const msg = (err && err.fields && err.fields.source_order) || (err && err.message) || 'The server refused the order.';
        showProblem(`Not saved: ${msg}`);
        ctx.flash(`Not saved: ${msg}`, { kind: 'error' });
      }
    } finally {
      busy = false;
      draw(focus);
      drawGap();
    }
  }
  function move(kind, to, dir) {
    const next = shown.filter((k) => k !== kind);
    next.splice(to, 0, kind);
    save(next, { focus: { kind, dir } });
  }

  /* Dragging by the grip: pointer events, so a mouse, a pen and a finger all work; the row moves as it goes. */
  function startDrag(e, li) {
    if (busy || (e.pointerType === 'mouse' && e.button !== 0)) return;
    e.preventDefault();
    // Followed on the document: moving the row in the list would drop a pointer capture on its grip.
    const id = e.pointerId;
    li.classList.add('is-dragging');
    const onMove = (ev) => {
      if (ev.pointerId !== id) return;
      ev.preventDefault();
      const others = [...list.children].filter((x) => x !== li);
      const before = others.find((x) => { const r = x.getBoundingClientRect(); return ev.clientY < r.top + r.height / 2; }) || null;
      if (before !== li.nextElementSibling) list.insertBefore(li, before);
    };
    const end = (ev) => {
      if (ev.pointerId !== id) return;
      stop();
      li.classList.remove('is-dragging');
      const next = [...list.children].map((x) => x.dataset.kind);
      if (ev.type === 'pointercancel' || same(next, shown)) { draw(); return; }
      save(next);
    };
    const stop = () => {
      document.removeEventListener('pointermove', onMove);
      document.removeEventListener('pointerup', end);
      document.removeEventListener('pointercancel', end);
    };
    document.addEventListener('pointermove', onMove, { passive: false });
    document.addEventListener('pointerup', end);
    document.addEventListener('pointercancel', end);
    ctx.onCleanup(stop);
  }

  /* Gap filling: the car's own choice (true / false), or null to follow the server's setting. */
  const gapSwitch = switchField({ label: 'Fill gaps from imported history', checked: false, onchange: (on) => saveGap(on) });
  const gapUses = h('p', { class: 'sc-setting-note' });
  const gapFollow = h('p', { class: 'sc-setting-note sc-src-follow' });
  const onOff = (v) => (v ? 'on' : 'off');
  function drawGap() {
    const s = getSettings();
    const imports = Array.isArray(s.imports) ? s.imports : [];
    const def = s.import_gap_fill_default;
    gapSwitch.set(imports.length ? !!s.import_gap_fill_on : false);
    gapSwitch.input.disabled = ro || !imports.length || busy;
    gapSwitch.classList.toggle('is-disabled', !imports.length);
    const span = (i) => (i.from_ms != null && i.to_ms != null ? ` (${formatDate(i.from_ms, ctx.tz, { weekday: false, now: api.now() })} to ${formatDate(i.to_ms, ctx.tz, { weekday: false, now: api.now() })})` : '');
    replace(gapUses, imports.length ? ['Uses: ', imports.map((i, n) => [n ? '; ' : null, h('strong', null, i.label || i.site), span(i)]), '.'] : 'No imported history: there is nothing to fill gaps from.');
    const own = s.import_gap_fill === true || s.import_gap_fill === false;
    replace(gapFollow, own
      ? [`Set for this car${def == null ? '' : `. Server default: ${onOff(def)}`}. `, ro ? null : button('Follow the server default', { cls: 'button-small', onclick: () => saveGap(null) })]
      : def == null ? 'Follows the server default.' : `Follows the server default: ${onOff(def)}.`);
  }
  async function saveGap(v) {
    if (busy) return;
    busy = true;
    try {
      const s = await patchSettings({ import_gap_fill: v });
      ctx.flash(v == null ? `Saved: ${car.name} follows the server’s gap filling (${onOff(s.import_gap_fill_on)}).` : `Saved: gaps ${v ? 'are filled' : 'are not filled'} from imported history for ${car.name}.`);
    } catch (err) {
      if (err && err.code === 'conflict') ctx.flash(`Not saved: someone else changed gap filling for ${car.name} at the same time. That is what is shown now.`, { kind: 'error' });
      else ctx.flash(`Not saved: ${(err && err.fields && err.fields.import_gap_fill) || (err && err.message)}`, { kind: 'error' });
    } finally {
      busy = false;
      shown = orderOf(getSettings());
      drawGap();
      draw();
    }
  }

  draw();
  drawGap();
  const overlaps = `${state.buildUrl('import', {})}#im-overlaps`;
  return section({ title: 'Data sources', note: ro ? 'read only for your account' : 'which input wins', id: 'data-sources' },
    setting('Priority of the live inputs',
      'When two sources cover the same time, the higher one wins, value by value. Imported history always comes after the live inputs.',
      h('div', { class: 'sc-stack sc-src-order' }, list, problem, resetBox, ro ? h('p', { class: 'sc-setting-note' }, 'An admin changes the order.') : null)),
    setting('Gap filling',
      'Where no live input has a value for a time, the imported history shown for that span fills it in: positions, charge levels, odometer and temperatures. Distance, energy and costs are never filled.',
      h('div', { class: 'sc-stack sc-src-gap' }, gapSwitch, gapUses, gapFollow)),
    ro ? null : h('p', { class: 'sc-more' }, h('a', { class: 'link-arrow', href: overlaps }, 'Choose per span which source is shown', icon('arrowRight'))));
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
      h('p', { class: 'empty-text' }, 'A car appears here when its SMO (CAN Bridge) uploads its first log, or once a Tesla account is added in Settings: Integrations and its car matched; you name it then.'),
      howCarsAppear(),
      fixes.length ? h('p', { class: 'note' }, `Now: ${fixes.map((f) => FIX_NOW[f.cause] || f.cause).join(' ')}`) : null);
    ctx.main.append(head, box);
    if (admin) {
      const nt = newCarTokens(ctx);
      ctx.main.append(nt.el);
      await nt.load();
    }
    if (admin && status.keys_waiting) {
      const w = waitingTable(ctx, {});
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
  let fresh = null;
  if (admin) {
    fresh = newCarTokens(ctx);
    waiting = waitingTable(ctx, { onChange: () => waiting.reload() });
    mainCol.append(fresh.el, section({ title: 'Keys waiting for a car', note: 'a CAN Bridge uploading with a key no car has yet' }, waiting.el));
  }
  ctx.main.append(head, h('div', { class: 'sc-layout' }, rail.el, mainCol));
  await Promise.all([rail.table.load(), waiting ? waiting.load() : null, fresh ? fresh.load() : null]);
  ctx.ready();
  return {};
}

const FIX_NOW = {
  no_token: 'No upload token has been made yet.',
  token_unused: 'A token was made but no CAN Bridge has uploaded with it yet.',
  key_waiting: 'A CAN Bridge key is waiting for a car (below).',
  logs_waiting_trust: 'Some logs wait for a trust check.',
  gps_inputs_disagree: 'One input’s GPS positions are out of step with the others’; its positions are not used there.',
  logs_missing: 'Some logs are missing on the server; the car was asked to send them again.',
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

  /*
   * Saving with the object's version. When someone else saved in between (409 stale_version), the fresh
   * object is read. The change is sent again only when every field it touches is still what this page
   * showed (someone changed another field); otherwise nothing is sent and a ConflictError carries the
   * fresh object, so the page shows the other change instead of writing over it.
   */
  class ConflictError extends Error {
    constructor(fresh) { super('Someone else changed this at the same time; your change was not saved.'); this.code = 'conflict'; this.fresh = fresh; }
  }
  const same = (a, b) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
  const fsList = (l) => (l || []).map((r) => ({ from_date: r.from_date || null, to_date: r.to_date || null }));
  /** PATCH the car (name, time zone). */
  async function patchCar(body) {
    try {
      return await ctx.api.patch(carPath, { version: carVersion, ...body });
    } catch (err) {
      if (!err || err.code !== 'stale_version') throw err;
      api.invalidate(carPath);
      const fresh = await ctx.api.get(carPath);
      if (Object.keys(body).some((k) => !same(fresh[k], car[k]))) throw new ConflictError(fresh);
      carVersion = fresh.version;
      return ctx.api.patch(carPath, { version: carVersion, ...body });
    }
  }
  /** A car field someone else changed: the page is drawn again from the car as it is now. */
  function carConflict(err, what) {
    if (!err || err.code !== 'conflict') return false;
    api.invalidate('');
    ctx.flash(`Not saved: someone else changed the ${what} at the same time. The page now shows their change; make yours again if it is still needed.`, { kind: 'error', next: true });
    remount();
    return true;
  }
  /** PATCH the car's settings. */
  async function patchSettings(body) {
    const path = api.apiPath`cars/${car.id}/settings`;
    const base = settings; // what this change was made against (another save may finish meanwhile)
    try {
      settings = await ctx.api.patch(path, { version: base.version, ...body });
    } catch (err) {
      if (!err || err.code !== 'stale_version') throw err;
      api.invalidate(path);
      const fresh = await ctx.api.get(path);
      const changed = Object.keys(body).some((k) => (k === 'free_supercharging'
        ? !same(fsList(fresh[k]), fsList(base[k]))
        : k === 'battery_chemistry' ? !same(fresh[k] && fresh[k].setting, base[k] && base[k].setting) : !same(fresh[k], base[k])));
      if (changed) { settings = fresh; throw new ConflictError(fresh); }
      settings = await ctx.api.patch(path, { version: fresh.version, ...body });
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
    catch (err) { if (carConflict(err, 'name')) return; setFieldError(nameField, (err.fields && err.fields.name) || err.message); }
  });
  nameField.control.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); nameField.control.blur(); } });

  const cfg = car.config || {};
  const configRows = CONFIG_ROWS.map(([k, label]) => ({ key: k, label, value: cfg[k] ? cfg[k].value : null, source: fromWords(cfg[k]) }));
  const configTable = dataTable(ctx, {
    rows: configRows, itemName: ['detail', 'details'], label: 'What the car reports about itself',
    columns: [
      { key: 'label', label: 'Detail', th: true, cell: (r) => r.label, value: (r) => r.label },
      { key: 'value', label: 'Value', cell: (r) => (r.value == null ? h('span', { class: 'is-missing' }, 'not sent yet') : yesNo(r.value)), value: (r) => yesNo(r.value) },
      { key: 'source', label: 'From', cell: (r) => (r.source ? h('span', { class: 'cell-sub' }, r.source) : '—'), value: (r) => r.source || '' },
    ],
    csv: { filename: `${car.name || car.id}-car-details.csv` },
  });

  const facts = h('p', { class: 'sc-facts' },
    [car.model, car.year].filter(Boolean).join(' ') || 'Model not known yet', car.trim ? [sep(), car.trim] : null,
    car.vin_prefix ? [sep(), 'VIN ', h('span', { class: 'mono' }, car.vin_prefix), h('span', { class: 'sc-quiet' }, ' (prefix only: model and year come from it)')] : null);

  /* ---------- the recommended tyre pressures, typed (config.tyre_pressure_front_kpa / _rear_kpa, kPa) */
  const pu = unitLabel('pressure');
  const r2 = (x) => Math.round(x * 100) / 100;
  const pLo = Math.ceil(convert(100, 'pressure') * 100) / 100, pHi = Math.floor(convert(600, 'pressure') * 100) / 100;
  const tyreField = (k, label, hint) => {
    const cur = cfg[k] ? cfg[k].value : null;
    const f = field({ label: `${label} (${pu})`, name: k, type: 'number', value: cur == null ? '' : String(r2(convert(cur, 'pressure'))), hint: `${pLo} to ${pHi} ${pu}. ${hint}`,
      attrs: { min: String(pLo), max: String(pHi), step: pu === 'psi' ? '1' : '0.05', disabled: ro } });
    f.control.addEventListener('change', async () => {
      const raw = f.control.value.trim();
      const n = raw === '' ? null : Number(raw);
      setFieldError(f, null);
      if (n != null && (!Number.isFinite(n) || n < pLo || n > pHi)) { setFieldError(f, `A number from ${pLo} to ${pHi} ${pu}, or empty.`); return; }
      try {
        await patchCar({ config: { [k]: n == null ? null : Math.round(toMetric(n, 'pressure') * 10) / 10 } });
        afterCarChange(n == null ? `Saved: ${label.toLowerCase()} is no longer typed.` : `Saved: ${label.toLowerCase()} ${n} ${pu}.`);
      } catch (err) { if (carConflict(err, 'tyre pressure')) return; setFieldError(f, (err.fields && err.fields[`config.${k}`]) || err.message); }
    });
    return f;
  };
  const tyreControl = h('div', { class: 'sc-stack' },
    tyreField('tyre_pressure_front_kpa', 'Front', 'The cold pressure on the door placard. Empty: what the car sends, else the placard’s for its wheel size.'),
    tyreField('tyre_pressure_rear_kpa', 'Rear', 'Empty: the front’s when that is typed.'));

  /* ---------- sold */
  const sold = state.isSold(car);
  const soldWords = sold ? (car.sold_date ? `Sold on ${formatDayKey(car.sold_date, { weekday: false })}` : 'Sold (date not given)') : 'Not sold';
  const soldDate = field({ label: sold ? 'Sold on' : 'Sold on (optional)', name: 'sold_date', type: 'date', value: car.sold_date || '', attrs: { disabled: ro, max: dayKey(api.now(), car.time_zone || undefined) } });
  /** PATCH sold / sold_date; 409 and 422 as for the name. */
  async function saveSold(body, text) {
    setFieldError(soldDate, null);
    try { await patchCar(body); afterCarChange(text); }
    catch (err) { if (carConflict(err, 'sold state')) return; setFieldError(soldDate, (err.fields && (err.fields.sold_date || err.fields.sold)) || err.message); }
  }
  const dateOrNull = () => soldDate.control.value || null;
  if (sold && !ro) {
    soldDate.control.addEventListener('change', () => {
      const d = dateOrNull();
      if (d === (car.sold_date || null)) return;
      saveSold({ sold_date: d }, d ? `Saved: ${car.name} was sold on ${formatDayKey(d, { weekday: false })}.` : `Saved: ${car.name} is sold, with no date.`);
    });
  }
  const markSold = ro ? null : sold
    ? button('Mark as not sold', { onclick: () => saveSold({ sold: false, sold_date: null }, `Saved: ${car.name} is not sold; it is listed and watched again.`) })
    : button('Mark as sold', { onclick: () => { const d = dateOrNull(); saveSold({ sold: true, sold_date: d }, `Saved: ${car.name} is marked sold${d ? ` on ${formatDayKey(d, { weekday: false })}` : ''}.`); } });
  const soldControl = h('div', { class: 'sc-stack sc-sold' },
    h('p', { class: ['sc-setting-value', { 'sc-sold-on': sold }] }, sold ? flag('Sold') : null, sold ? ' ' : null, soldWords),
    ro ? null : soldDate,
    markSold ? h('p', null, markSold) : null,
    h('p', { class: 'sc-setting-note' }, sold
      ? 'Its history stays, browsable as before. It is out of the fleet totals and the car picker’s first list, raises nothing to look at, and the Tesla API and TeslaFi store nothing more for it.'
      : ro ? 'An admin marks a car sold.' : 'Pick the date if you know it, then press Mark as sold. Its history stays; it leaves the fleet totals and raises nothing to look at.'));

  const carSection = section({ title: 'The car', note: ro ? 'read only for your account' : 'saved as you leave the field', id: 'sc-car' },
    setting('Name', 'Sentry Mate never asks Tesla for a name: the car is called what you type here.', nameField),
    setting('Sold', 'A car you no longer have. Mark it sold to keep its history without listing it with the cars you drive.', soldControl, { cls: 'sc-setting-sold' }),
    setting('Model', null, facts),
    setting('Recommended tyre pressure', 'A typed pressure beats the one the car sends. The car page flags a tyre more than 10 % under it.', tyreControl),
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
        { key: 'key', label: 'Key', th: true, cell: (k) => [h('span', { class: 'cell-main mono' }, k.key), deviceText(k, 'cell-sub')] },
        { key: 'first', label: 'First upload', cell: (k) => dateCell(k.first_seen_ms, ctx.tz) },
        { key: 'last', label: 'Last upload', cell: (k) => dateCell(k.last_upload_ms, ctx.tz, true) },
        { key: 'waiting', label: 'Logs waiting', num: true, cell: (k) => count(k.logs_waiting || 0, 'log'), hint: 'Logs waiting for their signature check.' },
        { key: 'pref', label: 'Preferred', cell: (k) => (k.preferred ? flag('Preferred') : button('Make preferred', { cls: 'button-small', onclick: () => prefer(k) })) },
        { key: 'do', label: '', cell: (k) => h('span', { class: 'dev-acts' }, renameButton(ctx, k, () => keys.reload()), button('Untie', { cls: 'button-small', onclick: () => untie(k) })) },
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
      let made;
      try {
        made = await ctx.api.post(api.apiPath`cars/${car.id}/upload-tokens`, {});
      } catch (err) { ctx.flash(`No token made: ${err.message}`, { kind: 'error' }); return; }
      await refreshUploads();
      await tokenDialog(ctx, made, car.name);
    }

    function drawOutcome() {
      const items = [];
      const toks = tokens.items || [];
      const active = toks.filter((t) => !t.revoked);
      const unused = active.filter((t) => !t.key && !t.last_upload_ms);
      const hl = health.value;
      const waitingKeys = waiting.items || [];
      const trustWaiting = (keys.items || []).reduce((n, k) => n + (k.logs_waiting || 0), 0);
      const fmt = (ms) => formatDateTime(ms, ctx.tz, { weekday: false, now: api.now() });
      const reasons = hl ? hl.reasons || [] : [];
      const trust = hl && hl.last_log_trust;
      // First, always: how this car's own uploads are doing.
      if (!hl) items.push(['info', 'How this car’s uploads are doing could not be read just now.', 'Data health shows every upload and check; try again there.']);
      else if (reasons.includes('upload_silence')) items.push(['warn', hl.last_upload_ms ? `This car has not uploaded since ${fmt(hl.last_upload_ms)}.` : 'This car has not uploaded yet.', 'The bridge may be off, out of reach of the server, or its token revoked. Logs are kept in the car and catch up once it reaches the server again.']);
      if (hl && reasons.includes('trust_failed')) items.push(['warn', reasons.includes('upload_silence') ? 'The last log of this car failed its signature check.' : 'This car’s uploads arrive, but a log failed its signature check.', `${trust && trust.reason ? `The reason: ${TRUST_REASONS[trust.reason] || 'a check the server could not name'}. ` : ''}${trust && trust.log_start_ms ? `The log starts ${fmt(trust.log_start_ms)}. ` : ''}Its time shows as unknown, never as asleep. Data health shows which log.`]);
      if (hl && !reasons.includes('upload_silence') && !reasons.includes('trust_failed') && (hl.healthy || !reasons.length)) items.push(['ok', 'This car is uploading normally.', `Last upload ${hl.last_upload_ms ? fmt(hl.last_upload_ms) : '—'}; the last log ${trust ? TRUST_WORDS[trust.result] || 'was checked' : 'is not checked yet'}.`]);
      else if (hl && !reasons.includes('upload_silence') && !reasons.includes('trust_failed')) items.push(['info', 'This car’s uploads arrive, with something still pending.', `Last upload ${hl.last_upload_ms ? fmt(hl.last_upload_ms) : '—'}. What is pending is listed below.`]);
      // Then what needs doing.
      if (!active.length) items.push(['todo', 'No upload token is active.', 'This car’s CAN Bridge cannot upload. Make a token and enter it in the app.', button('Make an upload token', { kind: 'primary', onclick: makeToken })]);
      for (const t of unused) items.push(['todo', `Token ${t.id} has not been used yet.`, `Made ${formatDate(t.created_ms, ctx.tz, { weekday: false, now: api.now() })}. Enter it in the CAN Bridge app and press Apply & restart; the first upload ties it to the bridge’s key.`]);
      if (trustWaiting) items.push(['warn', `${count(trustWaiting, 'log')} wait for a trust check.`, 'They are stored but not decoded until their signature checks out.']);
      if (reasons.includes('rebuild_pending')) items.push(['info', 'History is being rebuilt.', 'Drives, charges and states of the rebuilt days come back in a few seconds.']);
      if (waitingKeys.length) items.push(['todo', `${count(waitingKeys.length, 'key')} that no car has ${waitingKeys.length === 1 ? 'is' : 'are'} uploading.`, `Not about ${car.name}’s own uploads: a logger the server does not know yet. If one is a new logger in ${car.name} (a new Screenmate, a reinstall, a Raspberry Pi), tie it below. Its logs are kept until then.`]);
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

  /* ---------- data sources: the live inputs' priority, and gap filling from an import */
  const sourcesSection = dataSources(ctx, { car, ro, getSettings: () => settings, patchSettings });

  /* ---------- time zone */
  const zones =(() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
  const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
  const tzField = field({ label: 'Time zone', name: 'time_zone', value: car.time_zone || '', hint: 'A name such as Europe/Amsterdam or America/Los_Angeles. Type to see the list.', attrs: { list: 'sc-zones', spellcheck: 'false', autocomplete: 'off', disabled: ro } });
  const zoneList = h('datalist', { id: 'sc-zones' }, zones.map((z) => h('option', { value: z })));
  const tzNow = h('p', { class: 'sc-setting-note' });
  const drawTzNow = () => {
    const tz = car.time_zone;
    const src = zoneSourceText(car.time_zone_source);
    tzNow.textContent = tz && isValidZone(tz) ? `${src ? `${zoneLabel(tz)} (${src})${car.time_zone_source === 'owner' ? '' : ': type or pick a zone to set it for this car'}. ` : ''}Now in ${zoneLabel(tz)}: ${formatTime(api.now(), tz)} ${zoneShort(tz)}, ${formatDayKey(dayKey(api.now(), tz), { weekday: true })}.` : 'Not set: times show in this browser’s zone.';
  };
  drawTzNow();
  const saveZone = async (v) => {
    setFieldError(tzField, null);
    if (!v || !isValidZone(v)) { setFieldError(tzField, 'Not a known time zone. Pick one from the list, for example Europe/Amsterdam.'); return; }
    if (v === car.time_zone && car.time_zone_source === 'owner') return;
    try { await patchCar({ time_zone: v }); afterCarChange(`Saved: ${car.name} now uses ${v.replace(/_/g, ' ')}.`); }
    catch (err) { if (carConflict(err, 'time zone')) return; setFieldError(tzField, (err.fields && err.fields.time_zone) || err.message); }
  };
  tzField.control.addEventListener('change', () => saveZone(tzField.control.value.trim()));
  const useBrowser = browserZone && browserZone !== car.time_zone && !ro
    ? button(`Use this browser’s zone (${browserZone.replace(/_/g, ' ')})`, { cls: 'button-small', onclick: () => { tzField.control.value = browserZone; saveZone(browserZone); } }) : null;
  const useAuto = car.time_zone_source === 'owner' && !ro
    ? button('Work it out from the car’s position', { cls: 'button-small', onclick: async () => {
      setFieldError(tzField, null);
      try { await patchCar({ time_zone: null }); afterCarChange(`Saved: ${car.name} now takes its time zone from its position.`); }
      catch (err) { if (carConflict(err, 'time zone')) return; setFieldError(tzField, (err.fields && err.fields.time_zone) || err.message); }
    } }) : null;
  const tzSection = section({ title: 'Time zone', id: 'sc-tz', cover: 'docs.env.core.tz' },
    setting('The car’s time zone',
      'Days, weeks, months and the hours of time-of-use tariffs are counted in this zone, and every time of this car is shown in it. Times are stored in UTC, so a new zone regroups the history without changing it.',
      h('div', { class: 'sc-stack' }, tzField, zoneList, tzNow, useAuto, useBrowser)));

  /* ---------- battery */
  const bc = settings.battery_chemistry || { setting: 'auto' };
  const chemChoice = choice({ label: 'Battery chemistry', labelHidden: true, name: 'chemistry', value: bc.setting || 'auto',
    options: [['auto', 'Automatic'], ['lfp', 'LFP'], ['nca_nmc', 'NCA / NMC']],
    onchange: async (v) => {
      try { await patchSettings({ battery_chemistry: { setting: v } }); ctx.flash(`Saved: battery chemistry ${v === 'auto' ? 'is worked out automatically' : `set to ${CHEM_LONG[v]}`}.`); drawChem(); syncRanges(); }
      catch (err) {
        const now = (settings.battery_chemistry || {}).setting || 'auto';
        chemChoice.set(now);
        if (err && err.code === 'conflict') {
          drawChem(); syncRanges();
          ctx.flash(`Not saved: someone else set the battery chemistry to ${CHEM[now] || now} at the same time. That is what is shown now; choose again if yours is still needed.`, { kind: 'error' });
        } else ctx.flash(`Not saved: ${err.message}`, { kind: 'error' });
      }
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
  // Rows as shown. A row added here and not given a start date yet (fresh) is left out of what is saved;
  // a saved row whose dates are wrong holds the whole list back (sending the rest would delete it), and
  // says so next to it. A removal is saved at once unless such a row holds it back.
  const fromSaved = () => (settings.free_supercharging || []).map((r) => ({ from_date: r.from_date || null, to_date: r.to_date || null, fresh: false }));
  let ranges = fromSaved();
  let fsEls = [];
  let fsNote = null; // [kind, text] under the list
  const access = cfg.supercharging_access;
  const rowProblem = (r) => {
    if (!r.from_date) return r.fresh ? null : 'This range needs a start date; until it has one the list is not saved.';
    if (r.to_date && r.to_date < r.from_date) return 'The end is before the start; until that is fixed the list is not saved.';
    return null;
  };
  async function saveRanges({ removed = false } = {}) {
    const blocked = ranges.map(rowProblem);
    if (blocked.some(Boolean)) {
      ranges.forEach((r, i) => { if (blocked[i] && fsEls[i]) setFieldError(r.from_date ? fsEls[i].to : fsEls[i].from, blocked[i]); });
      fsNote = ['held', removed ? 'The removed range is not saved yet: fix the marked range first, then it is saved with it.' : 'Not saved yet: fix the marked range first.'];
      drawFsNote();
      return;
    }
    const keep = ranges.filter((r) => r.from_date);
    const pending = ranges.filter((r) => !r.from_date); // new rows without a start: kept on screen, not sent
    try {
      await patchSettings({ free_supercharging: keep.map((r) => ({ from_date: r.from_date, to_date: r.to_date || null })) });
      ranges = [...fromSaved(), ...pending];
      chemChoice.set((settings.battery_chemistry || {}).setting || 'auto'); drawChem();
      fsNote = pending.length ? ['held', keep.length ? `Saved the ${count(keep.length, 'range')} that ${keep.length === 1 ? 'has' : 'have'} dates. The new range is saved once it has a start date.` : 'Saved: no dated range is left. The new range is saved once it has a start date.'] : null;
      ctx.flash(keep.length ? `Saved: ${count(keep.length, 'free Supercharging range')}.` : 'Saved: no free Supercharging.');
      drawRanges();
    } catch (err) {
      if (err && err.code === 'conflict') {
        const mine = keep;
        ranges = fromSaved();
        fsNote = ['conflict', `Not saved: someone else changed these ranges at the same time. The list now shows theirs (${count(ranges.length, 'range')}). You tried to save ${mine.length ? mine.map((r) => rangeWords(r)).join('; ') : 'no range'}. Make your change again if it is still needed.`];
        drawRanges();
        ctx.flash('Not saved: someone else changed the free Supercharging ranges at the same time.', { kind: 'error' });
        return;
      }
      const f = err.fields || {};
      let placed = false;
      const sentIdx = ranges.map((r, i) => (r.from_date ? i : -1)).filter((i) => i >= 0);
      for (const [path, msg] of Object.entries(f)) {
        const m = /^free_supercharging\.(\d+)\.(from_date|to_date)$/.exec(path);
        const i = m ? sentIdx[Number(m[1])] : undefined;
        if (i != null && fsEls[i]) { setFieldError(fsEls[i][m[2] === 'from_date' ? 'from' : 'to'], msg); placed = true; }
      }
      fsNote = ['held', removed ? 'The removed range is not saved: the server refused the list (see the marked range or the note at the top).' : 'Not saved: the server refused the list.'];
      drawFsNote();
      if (!placed) ctx.flash(`Not saved: ${err.message}`, { kind: 'error' });
    }
  }
  /** After another setting was saved: if the server's ranges are not the ones shown, show the server's. */
  function syncRanges() {
    const shown = ranges.filter((r) => !r.fresh);
    if (same(fsList(settings.free_supercharging), fsList(shown))) return;
    ranges = [...fromSaved(), ...ranges.filter((r) => r.fresh && !r.from_date)];
    fsNote = ['conflict', `Someone else changed these ranges in the meantime; the list now shows theirs (${count(ranges.filter((r) => !r.fresh).length, 'range')}).`];
    drawRanges();
  }
  const rangeWords = (r) => (r.to_date ? `${formatDayKey(r.from_date, { weekday: false })} to ${formatDayKey(r.to_date, { weekday: false })}` : `from ${formatDayKey(r.from_date, { weekday: false })} on`);
  const fsNoteEl = h('p', { class: 'sc-fs-note', role: 'status', hidden: true });
  function drawFsNote() {
    fsNoteEl.hidden = !fsNote;
    fsNoteEl.className = fsNote ? `sc-fs-note sc-fs-note-${fsNote[0]}` : 'sc-fs-note';
    replace(fsNoteEl, fsNote ? [icon('warning'), ' ', fsNote[1]] : null);
  }
  function drawRanges() {
    fsEls = [];
    const rows = ranges.map((r, i) => {
      const from = field({ label: 'From', name: `fs-from-${i}`, type: 'date', value: r.from_date || '', attrs: { required: true, disabled: ro } });
      const to = field({ label: 'Until (blank: no end)', name: `fs-to-${i}`, type: 'date', value: r.to_date || '', attrs: { disabled: ro } });
      fsEls[i] = { from, to };
      const changed = () => { setFieldError(from, null); setFieldError(to, null); fsNote = null; drawFsNote(); saveRanges(); };
      from.control.addEventListener('change', () => { r.from_date = from.control.value || null; changed(); });
      to.control.addEventListener('change', () => { r.to_date = to.control.value || null; changed(); });
      const status = h('p', { class: 'sc-fs-status' }, r.from_date
        ? `Supercharger sessions that start ${r.to_date ? `from ${formatDayKey(r.from_date, { weekday: false })} to ${formatDayKey(r.to_date, { weekday: false })}` : `on or after ${formatDayKey(r.from_date, { weekday: false })}`} cost nothing.`
        : 'Not saved yet: pick the first free day. The other ranges are saved without it.');
      return h('li', { class: 'sc-fs-row' }, from, to,
        ro ? null : button('Remove', { cls: 'button-small', onclick: () => {
          const wasSaved = !ranges[i].fresh;
          ranges.splice(i, 1);
          fsNote = null;
          drawRanges();
          if (wasSaved) saveRanges({ removed: true });
        } }),
        status);
    });
    replace(fsBox,
      rows.length ? h('ol', { class: 'sc-fs-list' }, rows) : h('p', { class: 'note-serif' }, 'No free Supercharging: every Supercharger session is priced from the charger’s own billed energy or the place’s tariff.'),
      fsNoteEl,
      ro ? null : button('Add a range', { onclick: () => { ranges.push({ from_date: null, to_date: null, fresh: true }); drawRanges(); fsBox.querySelector('.sc-fs-row:last-child input')?.focus(); } }));
    drawFsNote();
    // Rows that still hold the list back keep their message after a redraw.
    ranges.forEach((r, i) => { const p = rowProblem(r); if (p && fsNote && fsNote[0] === 'held') setFieldError(r.from_date ? fsEls[i].to : fsEls[i].from, p); });
  }
  drawRanges();
  const chargingSection = section({ title: 'Charging', id: 'sc-chg', cover: 'web.settings.car.free-supercharging' },
    setting('Free Supercharging',
      'Dated ranges, in the car’s time zone. A Supercharger session that starts inside one gets a cost of zero, marked “free Supercharging”; sessions outside keep their price, so turning it off later does not rewrite history.',
      h('div', { class: 'sc-stack' },
        h('p', { class: 'sc-hint' }, icon('info', 'icon sc-hint-icon'), ' What the car says about Supercharging: ', access && access.value != null
          ? h('strong', null, yesNo(access.value)) : h('span', { class: 'is-missing' }, 'not sent yet'),
        fromWords(access) ? h('span', { class: 'sc-quiet' }, ` (${fromWords(access)})`) : null,
        '. The car does not say since when, so the dates are yours.'),
        fsBox)),
    h('p', { class: 'sc-more' }, h('a', { class: 'link-arrow', href: ctx.link('charges') }, 'Charges and their cost', icon('arrowRight'))));

  /* ---------- put it together */
  const rail = carRail(ctx, car);
  const mainCol = h('div', { class: 'sc-main', cover: 'web.settings.car.autosave' },
    h('p', { class: 'sc-autosave' }, icon('check'), ro
      ? ' Your account can read these settings; an admin changes them.'
      : ' Every change saves at once, for everyone who sees this car; a short note at the top confirms it. There is no Save button.'),
    carSection, uploads, sourcesSection, tzSection, batterySection, chargingSection);
  ctx.main.append(
    pageHead({ eyebrow: crumbs(car), title: sold ? [car.name, ' ', flag('Sold')] : car.name, deck: ctx.info.purpose,
      aside: h('p', { class: 'sc-as-of' }, car.last_activity_ms ? ['Last heard ', ago(car.last_activity_ms)] : 'Not heard from yet') }),
    h('div', { class: ['sc-layout', 'sc-layout-car'] }, rail.el, mainCol));
  await Promise.all([rail.table.load(), configTable.load(), uploads.load ? uploads.load() : null]);
  ctx.ready();
  return {
    async update(c) { drawTzNow(); c.ready(); },
  };
}
