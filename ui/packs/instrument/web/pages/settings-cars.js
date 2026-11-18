// Settings: Cars (/settings/cars and /settings/cars/{car}): per-car settings, and how each car's CAN
// Bridge is tied to it. Without a car: every car the account sees (searched and paged by the server),
// keys waiting for a car, and how a car appears. With a car: its name, what it reports about itself,
// its upload tokens and CAN Bridge keys (with the outcome of its uploads), its time zone, battery
// chemistry, reference values and free Supercharging as dated ranges. Every change saves at once
// (PATCH with the object's version) and a short note confirms it.

import { h, replace } from '../shared/dom.js';
import * as state from '../shared/state.js';
import { invalidate } from '../shared/api.js';
import { isValidZone } from '../shared/time.js';
import { convert, toMetric, unitLabel, format } from '../shared/units.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, panelLink, kv, tag, pill, note } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { field, showApiError } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { openCarSwitcher, SWITCH_KEYS } from '../core/carswitch.js';
import { dateTime, date, time, ago, agoEl, tzName, carMeta, statePill } from '../core/fmt.js';

export const id = 'settings-cars';
export const css = true;
export const title = (ctx) => (ctx.car ? `Settings: ${ctx.car.name}` : 'Settings: Cars');

const enc = encodeURIComponent;
const SOURCE = { can: ['From CAN', 'live'], vin: ['From VIN', 'plain'], owner: ['Typed', 'warm'], teslamate: ['From TeslaMate', 'plain'] };
const CHEM = { auto: 'Automatic', lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA / NMC (nickel)' };
const CONFIG_ROWS = [
  ['trim', 'Trim'], ['drivetrain', 'Drivetrain'], ['performance_package', 'Performance package'], ['pack_kwh_code', 'Pack code'],
  ['wheel', 'Wheels'], ['paint', 'Paint'], ['roof', 'Roof'], ['spoiler', 'Spoiler'], ['right_hand_drive', 'Right-hand drive'],
  ['map_region', 'Map region'], ['supercharging_access', 'Supercharging access'],
];
const yesNo = (v) => (v === true ? 'Yes' : v === false ? 'No' : v);

// a row: the label and its explanation on the left, the control on the right (the app's .setting)
const setting = (titleText, text, control, cv = null) => h('div', { class: 'setting', cover: cv },
  h('div', null, h('h3', null, titleText), text ? h('p', null, text) : null), h('div', { class: 'setting-control' }, control));

// the sentence for an error that did not land under a field
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);

/** How a car appears: the steps, shown as the empty state and as a note beside the car picker. */
function howCarsAppear(admin) {
  return h('ol', { class: 'sc-steps' },
    h('li', null, h('b', null, 'Install the CAN Bridge app'), ' on the Screenmate One (or another logger, such as a Raspberry Pi) in the car.'),
    h('li', null, h('b', null, 'Enter an upload token'), admin ? ' made on this page. It is shown once, so copy it straight into the app.' : '. An admin makes one on this page.'),
    h('li', null, h('b', null, 'Wait for the first log.'), ' The car shows up here by itself, with its model and year from the VIN prefix. Name it, and set its time zone.'),
    h('li', null, h('b', null, 'A second logger'), ' in the same car (a new Screenmate, a reinstall, a Pi) brings a new key. It waits under Keys waiting until it is tied to the car.'));
}

/* ====================================================================== the page */

export async function mount(ctx) {
  if (!ctx.car) return mountIndex(ctx);
  return mountCar(ctx);
}

/* ---------------------------------------------------------------------- /settings/cars */

async function mountIndex(ctx) {
  const admin = ctx.admin;
  let status = null, waiting = null;
  try {
    [status, waiting] = await Promise.all([
      ctx.api.get('setup/status').catch(() => null),
      admin ? ctx.api.get('can-keys', { assigned: 'false', limit: 100 }) : Promise.resolve(null),
    ]);
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const noCars = status && status.cars === 0;
  const empty = h('section', { class: 'panel panel-pad sc-empty', cover: 'web.settings.car.empty-state', hidden: !noCars },
    emptyState({ icon: 'car', title: 'No cars yet', text: 'Sentry Mate needs no Tesla account: a car appears here when its CAN Bridge uploads its first log.' }),
    howCarsAppear(admin));

  const table = noCars ? null : dataTable(ctx, {
    title: 'Cars', icon: 'car', path: 'cars', params: { sort: 'custom' }, label: 'Cars and their settings', csvName: 'cars',
    note: 'In your own car order. Search finds a car by name, model or VIN prefix. Open a car to change its settings.',
    side: h('input', { class: 'input sc-search', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car',
      oninput: (e) => { clearTimeout(e.target._t); e.target._t = setTimeout(() => table.setParams({ search: e.target.value.trim() || null }), 200); } }),
    columns: [
      { key: 'name', label: 'Car', render: (c) => h('a', { class: 'link', href: state.buildUrl('settings-cars', { car: c.id }) }, c.name) },
      { key: 'model', label: 'Model and year', render: (c) => carMeta(c) || '—', csv: (c) => carMeta(c) },
      { key: 'vin_prefix', label: 'VIN prefix', render: (c) => h('span', { class: 'mono' }, c.vin_prefix || '—') },
      { key: 'time_zone', label: 'Time zone', render: (c) => c.time_zone || '—' },
      { key: 'state', label: 'Now', render: (c) => statePill(c.now?.state || 'unknown', { stale: c.now?.stale }), csv: (c) => c.now?.state || 'unknown' },
      { key: 'last', label: 'Last upload', render: (c) => (c.now?.received_ms ? agoEl(c.now.received_ms, c.time_zone) : 'Never'), csv: (c) => c.now?.received_ms ?? '' },
    ],
    rowHref: (c) => state.buildUrl('settings-cars', { car: c.id }),
    empty: () => emptyState({ icon: 'search', title: 'No car matches', text: 'Search looks at the name, the model and the VIN prefix. Clear the search to see every car.' }),
  });

  const waitPanel = admin ? waitingTable(ctx, waiting?.items || [], null, () => ctx.reload()) : null;

  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'settings-stack' },
      h('p', { class: 'note saving-note', cover: 'web.settings.car.autosave' }, icon('check', 'ico--sm'), ' Every change on these pages is saved at once; there is no Save button. A short note confirms it.'),
      empty,
      table ? table.el : null,
      waitPanel,
      noCars ? null : panel({ title: 'How a car appears', icon: 'info' }, howCarsAppear(admin))));
  if (table) await table.ready;
  if (!ctx.alive()) return;
  ctx.ready();
}

/* ---------------------------------------------------------------------- keys waiting (both routes) */

function waitingTable(ctx, rows, car, after) {
  const tz = ctx.tz;
  const assign = async (k, preferred) => {
    try {
      await ctx.api.patch(`can-keys/${enc(k.key)}`, preferred ? { car: car.id, preferred: true } : { car: car.id });
      flash(`Key ${k.key} is now tied to ${car.name}${preferred ? ' and preferred' : ''}. Its waiting logs are being read.`);
      after();
    } catch (e) { flash(`Could not assign the key: ${errText(e)}`, { kind: 'error' }); }
  };
  return dataTable(ctx, {
    title: 'Keys waiting for a car', icon: 'lock', rows, label: 'CAN Bridge keys waiting to be assigned', csvName: 'keys-waiting',
    note: car
      ? `A CAN Bridge signs every log with its own key. These keys uploaded logs but are not tied to any car yet; their logs wait, unread. Tie one to ${car.name} only if it records this car.`
      : 'A CAN Bridge signs every log with its own key. These keys uploaded logs but are not tied to any car yet; their logs wait, unread. Open the car it records to assign it.',
    sort: { key: 'first', dir: 'desc' },
    columns: [
      { key: 'key', label: 'Key', render: (k) => h('span', { class: 'mono' }, k.key), sort: true },
      { key: 'device', label: 'Device', render: (k) => k.device || 'Not reported', sort: true },
      { key: 'first', label: 'First upload', value: (k) => k.first_seen_ms, sort: true, render: (k) => dateTime(k.first_seen_ms, tz), csv: (k) => k.first_seen_ms },
      { key: 'last', label: 'Last upload', value: (k) => k.last_upload_ms, sort: true, render: (k) => (k.last_upload_ms ? agoEl(k.last_upload_ms, tz) : 'Never'), csv: (k) => k.last_upload_ms ?? '' },
      { key: 'logs_waiting', label: 'Logs waiting', num: true, sort: true, render: (k) => format(k.logs_waiting ?? 0, 'count') },
      { key: 'act', label: car ? 'Assign' : 'What to do', csv: false, render: (k) => (car
        ? h('div', { class: 'row sc-acts' },
          h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: () => assign(k, false) }, icon('plus', 'ico--sm'), `Assign to ${car.name}`),
          h('button', { class: 'btn btn--sm', type: 'button', onclick: () => assign(k, true) }, 'Assign and prefer'))
        : h('span', { class: 'faint' }, 'Open its car, then Assign')) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No keys waiting', text: 'Every CAN Bridge that uploaded is tied to a car. A key waits here when a new logger uploads with a key the server has not tied to a car.' }),
  }).el;
}

/* ---------------------------------------------------------------------- /settings/cars/{car} */

async function mountCar(ctx) {
  const admin = ctx.admin;
  const cid = ctx.car.id;
  const S = { car: null, settings: null, tokens: [], keys: [], waiting: [], health: null, battery: null, cars: null };

  async function load() {
    const [car, settings, tokens, keys, waiting, health, battery, cars] = await Promise.all([
      ctx.api.get(`cars/${enc(cid)}`),
      ctx.api.get(`cars/${enc(cid)}/settings`),
      admin ? ctx.api.get(`cars/${enc(cid)}/upload-tokens`, { limit: 100 }) : null,
      admin ? ctx.api.get('can-keys', { car: cid, limit: 100 }) : null,
      admin ? ctx.api.get('can-keys', { assigned: 'false', limit: 100 }) : null,
      ctx.api.get(`cars/${enc(cid)}/health`).catch(() => null),
      ctx.api.get(`cars/${enc(cid)}/battery`).catch(() => null),
      ctx.api.page('cars', { sort: 'custom', limit: 8 }).catch(() => null),
    ]);
    Object.assign(S, { car, settings, tokens: tokens?.items || [], keys: keys?.items || [], waiting: waiting?.items || [], health, battery, cars });
  }
  try { await load(); } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const car = () => S.car;
  const tz = () => S.car.time_zone || ctx.tz;
  const ro = !admin;
  const dis = (el) => { if (ro) el.setAttribute('disabled', ''); return el; };

  ctx.setCrumbs([{ label: 'Set-up' }, { label: 'Settings: Cars', href: state.buildUrl('settings-cars') }, { label: S.car.name }]);

  /* ---- saving: the car (name, time zone) and its settings, each with its version */
  async function patchCar(body, what, f = null) {
    try {
      S.car = await ctx.api.patch(`cars/${enc(cid)}`, { version: S.car.version, ...body });
      invalidate('cars');
      f?.setError(null);
      flash(`${what} saved.`);
      paintAll();
    } catch (e) {
      if (e.status === 409) { await refresh(); flash('Someone else changed this car; the page shows their change now. Try again.', { kind: 'info' }); return; }
      if (f && e.fields) showApiError({ fields: [f], setMessage: (t) => t && flash(t, { kind: 'error' }) }, e);
      else flash(`Could not save ${what.toLowerCase()}: ${errText(e)}`, { kind: 'error' });
    }
  }
  async function patchSettings(body, what, onFields = null) {
    try {
      S.settings = await ctx.api.patch(`cars/${enc(cid)}/settings`, { version: S.settings.version, ...body });
      flash(`${what} saved.`);
      paintAll();
      return true;
    } catch (e) {
      if (e.status === 409) { await refresh(); flash('Someone else changed these settings; the page shows theirs now. Try again.', { kind: 'info' }); return false; }
      if (onFields && e.fields) onFields(e.fields);
      else flash(`Could not save ${what.toLowerCase()}: ${errText(e)}`, { kind: 'error' });
      return false;
    }
  }
  async function refresh() {
    try { await load(); } catch (e) { flash(`Could not load the car again: ${errText(e)}`, { kind: 'error' }); return; }
    if (ctx.alive()) paintAll();
  }

  /* ---- slots, painted again after each change */
  const slots = { car: h('div'), uploads: h('div', { class: 'settings-stack' }), tz: h('div'), battery: h('div'), refs: h('div'), charging: h('div') };

  function paintAll() {
    if (!ctx.alive()) return;
    paintCar(); paintUploads(); paintTz(); paintBattery(); paintRefs(); paintCharging();
  }

  /* ---- Car and switcher */
  function paintCar() {
    const c = car();
    const list = S.cars?.items || [];
    const shown = list.some((x) => x.id === cid) ? list : [c, ...list.slice(0, 7)];
    const picker = h('div', { class: 'sc-picker', cover: 'web.settings.car.car-switcher' },
      h('div', { class: 'seg sc-cars', role: 'group', 'aria-label': 'Whose settings' },
        shown.map((x) => h('a', { class: 'seg-btn', href: state.buildUrl('settings-cars', { car: x.id }), 'aria-pressed': String(x.id === cid), 'aria-current': x.id === cid ? 'page' : null }, x.name))),
      h('div', { class: 'row' },
        h('button', { class: 'btn btn--sm', type: 'button', onclick: () => openCarSwitcher() }, icon('search', 'ico--sm'), S.cars?.next || (S.cars?.total ?? 0) > shown.length ? `Find another car (${S.cars?.total ?? 'more'})` : 'Find a car'),
        h('span', { class: 'faint' }, 'or press ', SWITCH_KEYS.map((k) => h('span', { class: 'kbd' }, k)), ' anywhere. The switcher keeps this page.')));

    const name = field({ label: 'Name', name: 'sc-name', path: 'name', value: c.name, hint: 'Shown in every list, the sidebar and the tab title. 1 to 64 characters.', attrs: { maxlength: '64', disabled: ro, autocomplete: 'off' } });
    name.input.addEventListener('change', () => {
      const v = name.input.value.trim();
      if (!v) { name.setError('A car needs a name.'); return; }
      if (v !== car().name) patchCar({ name: v }, 'Name', name);
    });

    const cfg = c.config || {};
    const rows = [
      ['Model and year', [carMeta(c) || '—', ' ', tag('From VIN', 'plain')]],
      ['VIN prefix', h('span', { class: 'mono' }, c.vin_prefix || '—')],
      ...CONFIG_ROWS.filter(([k]) => cfg[k]?.value != null).map(([k, label]) => [label, [String(yesNo(cfg[k].value)), ' ', SOURCE[cfg[k].source] ? tag(...SOURCE[cfg[k].source]) : null]]),
    ];
    const missing = CONFIG_ROWS.filter(([k]) => cfg[k]?.value == null).map(([, l]) => l.toLowerCase());

    replace(slots.car, panel({ title: 'Car', icon: 'car', side: h('span', { class: 'faint' }, `${carMeta(c)}${c.profile ? ` · CAN profile ${c.profile}` : ''}`) },
      setting('Whose settings', 'Pick the car to change. Each car has its own settings; nothing here changes another car.',
        [picker, h('p', { class: 'setting-note', cover: 'web.settings.car.empty-state' }, icon('info', 'ico--sm'), ' A new car appears here by itself once its CAN Bridge has uploaded its first log; no Tesla account is involved. ',
          h('a', { class: 'link', href: state.buildUrl('settings-cars') }, 'How a car appears'), '.')]),
      setting('Name', 'Sentry Mate never reads a name from Tesla: you name the car. Until then it shows its model and VIN prefix.', name.el),
      setting('What the car reports', `Read from the car's build message on the vehicle bus, with model and year from the VIN prefix. ${missing.length ? `Not reported: ${missing.join(', ')}.` : ''}`,
        kv(rows))));
  }

  /* ---- Uploads: outcome, tokens, keys, waiting keys */
  function outcome() {
    const hl = S.health, c = car();
    const last = hl?.last_upload_ms ?? c.now?.received_ms ?? null;
    const trust = hl?.last_log_trust?.result || null;
    const used = S.tokens.some((t) => t.last_upload_ms);
    const active = S.tokens.filter((t) => !t.revoked);
    const pref = S.keys.find((k) => k.preferred) || null;
    let tone = 'ok', ic = 'check', head, detail;
    if (!last) {
      tone = 'warn'; ic = 'warn';
      head = 'No log has arrived for this car yet.';
      detail = active.length ? `${active.length === 1 ? 'An upload token exists' : `${active.length} upload tokens exist`} but ${used ? 'none uploaded lately' : 'none has been used'}. Check that the CAN Bridge app has the token and can reach this server.` : 'Make an upload token below and enter it in the CAN Bridge app.';
    } else if (c.now?.stale) {
      tone = 'warn'; ic = 'warn';
      head = `Quiet: the last log arrived ${ago(last)} (${dateTime(last, tz())}).`;
      detail = trust === 'failed' ? `That log failed its check${hl.last_log_trust.reason ? ` (${hl.last_log_trust.reason.replaceAll('_', ' ')})` : ''}, so it was not used. ` : 'A car that is parked with its logger off sends nothing; a logger that lost its network catches up when it is back. ';
    } else {
      head = `Uploading: the last log arrived ${ago(last)}.`;
      detail = '';
    }
    if (trust && tone === 'ok') detail += trust === 'verified' ? 'Its signature was checked and matches a key tied to this car. ' : trust === 'unsigned' ? 'It carried no signature, so it is kept but marked unsigned. ' : 'It failed its check and was not used. ';
    if (pref) detail += `Preferred key: ${pref.key}${pref.device ? ` (${pref.device})` : ''}. `;
    if (S.waiting.length) detail += `${S.waiting.length === 1 ? 'One key is' : `${S.waiting.length} keys are`} waiting for a car below. `;
    if (hl?.logs_waiting) detail += `${hl.logs_waiting} logs wait to be checked. `;
    return h('div', { class: ['sc-outcome', `sc-outcome--${tone}`], role: 'status', cover: 'faq.car-missing.outcomes' },
      h('span', { class: 'sc-outcome-ic' }, icon(ic)),
      h('div', null, h('p', { class: 'sc-outcome-head' }, head), detail ? h('p', { class: 'sc-outcome-text' }, detail.trim()) : null),
      panelLink(state.buildUrl('health', { car: cid }), 'Upload status in Data health'));
  }

  async function makeToken() {
    let t;
    try { t = await ctx.api.post(`cars/${enc(cid)}/upload-tokens`, {}); } catch (e) { flash(`Could not make a token: ${errText(e)}`, { kind: 'error' }); return; }
    const secret = h('input', { class: 'input mono sc-secret', readonly: true, value: t.secret, 'aria-label': 'The new upload token', onfocus: (e) => e.target.select() });
    const copied = h('span', { class: 'faint', 'aria-live': 'polite' });
    await openDialog({
      title: `Upload token for ${car().name}`,
      body: [
        h('p', null, 'Enter this in the CAN Bridge app, under Sentry Mate upload. ', h('b', null, 'It is shown only now:'), ' the server keeps only a check value, so nobody can read it back later. If it is lost, make a new one and revoke this one.'),
        h('div', { class: 'row sc-secret-row' }, secret,
          h('button', { class: 'btn btn--sm', type: 'button', onclick: async () => {
            try { await navigator.clipboard.writeText(t.secret); copied.textContent = 'Copied.'; } catch { secret.select(); copied.textContent = 'Select and copy it by hand (Ctrl+C).'; }
          } }, icon('check', 'ico--sm'), 'Copy'), copied),
        h('p', { class: 'note' }, 'The first log sent with it ties the CAN Bridge key to this car.'),
      ],
      actions: [{ label: 'I have copied it', value: true, kind: 'primary' }],
    });
    S.tokens = (await ctx.api.get(`cars/${enc(cid)}/upload-tokens`, { limit: 100 }).catch(() => null))?.items || [t, ...S.tokens];
    if (ctx.alive()) paintUploads();
  }

  async function revokeToken(t) {
    if (!(await confirmDialog(`Revoke upload token ${t.id}? Logs sent with it are refused from now on. Logs already received stay.`, { title: 'Revoke the token?', ok: 'Revoke', danger: true }))) return;
    try {
      const r = await ctx.api.del(`cars/${enc(cid)}/upload-tokens/${enc(t.id)}`);
      S.tokens = S.tokens.map((x) => (x.id === t.id ? { ...x, ...r } : x));
      flash(`Token ${t.id} revoked.`);
      paintUploads();
    } catch (e) { flash(`Could not revoke the token: ${errText(e)}`, { kind: 'error' }); }
  }

  async function keyChange(k, body, done) {
    try {
      await ctx.api.patch(`can-keys/${enc(k.key)}`, body);
      flash(done);
      await refresh();
    } catch (e) { flash(`Could not change the key: ${errText(e)}`, { kind: 'error' }); }
  }

  function paintUploads() {
    const z = tz();
    if (ro) {
      replace(slots.uploads, panel({ title: 'Uploads, tokens and keys', icon: 'upload' }, outcome(),
        note('Upload tokens and CAN Bridge keys are shown to admins only, because a token lets a logger send data for this car.')));
      return;
    }
    const tokens = dataTable(ctx, {
      title: 'Upload tokens', icon: 'lock', rows: S.tokens, label: 'Upload tokens of this car', csvName: 'upload-tokens',
      note: 'A token lets a CAN Bridge upload logs for this car. Each is shown once, when made. Revoking one refuses its uploads from then on; past logs stay.',
      side: h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: makeToken }, icon('plus', 'ico--sm'), 'Make upload token'),
      sort: { key: 'created', dir: 'desc' },
      columns: [
        { key: 'id', label: 'Token', render: (t) => h('span', { class: 'mono' }, t.id), sort: true },
        { key: 'created', label: 'Made', value: (t) => t.created_ms, sort: true, render: (t) => dateTime(t.created_ms, z), csv: (t) => t.created_ms },
        { key: 'state', label: 'State', value: (t) => (t.revoked ? 2 : t.last_upload_ms ? 0 : 1), sort: true,
          render: (t) => (t.revoked ? pill('Revoked', { tone: 'plain' }) : t.last_upload_ms ? pill('In use', { tone: 'live' }) : pill('Not used yet', { tone: 'warm' })),
          csv: (t) => (t.revoked ? 'revoked' : t.last_upload_ms ? 'in use' : 'not used yet') },
        { key: 'key', label: 'Key it brought', render: (t) => (t.key ? h('span', { class: 'mono' }, t.key) : h('span', { class: 'faint' }, t.revoked ? '—' : 'None yet')) },
        { key: 'last', label: 'Last upload', value: (t) => t.last_upload_ms, sort: true, render: (t) => (t.last_upload_ms ? agoEl(t.last_upload_ms, z) : 'Never'), csv: (t) => t.last_upload_ms ?? '' },
        { key: 'act', label: '', csv: false, render: (t) => (t.revoked ? null : h('button', { class: 'btn btn--sm btn--danger', type: 'button', onclick: () => revokeToken(t) }, icon('x', 'ico--sm'), 'Revoke')) },
      ],
      empty: () => emptyState({ icon: 'lock', title: 'No upload tokens', text: 'Without a token no CAN Bridge can upload for this car. Make one and enter it in the CAN Bridge app.' }),
    });
    const keys = dataTable(ctx, {
      title: 'CAN Bridge keys', icon: 'chip', rows: S.keys, label: 'CAN Bridge keys tied to this car', csvName: 'can-keys',
      note: 'Each logger signs its logs with its own key: a Screenmate, a reinstall of the app or a Raspberry Pi each bring one. When two record the same time, the preferred key\'s log is used and the other is kept.',
      sort: { key: 'last', dir: 'desc' },
      columns: [
        { key: 'key', label: 'Key', render: (k) => h('span', { class: 'mono' }, k.key), sort: true },
        { key: 'device', label: 'Device', render: (k) => k.device || 'Not reported', sort: true },
        { key: 'first', label: 'First upload', value: (k) => k.first_seen_ms, sort: true, render: (k) => dateTime(k.first_seen_ms, z), csv: (k) => k.first_seen_ms },
        { key: 'last', label: 'Last upload', value: (k) => k.last_upload_ms, sort: true, render: (k) => (k.last_upload_ms ? agoEl(k.last_upload_ms, z) : 'Never'), csv: (k) => k.last_upload_ms ?? '' },
        { key: 'logs_waiting', label: 'Logs waiting', num: true, sort: true, render: (k) => format(k.logs_waiting ?? 0, 'count') },
        { key: 'preferred', label: 'Preferred', value: (k) => (k.preferred ? 0 : 1), sort: true, csv: (k) => (k.preferred ? 'yes' : 'no'),
          render: (k) => (k.preferred ? pill('Preferred', { tone: 'live', ic: 'check' })
            : h('button', { class: 'btn btn--sm', type: 'button', onclick: () => keyChange(k, { preferred: true }, `Key ${k.key} is now preferred for ${car().name}.`) }, 'Prefer this key')) },
        { key: 'act', label: '', csv: false, render: (k) => h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: async () => {
          if (await confirmDialog(`Untie key ${k.key} from ${car().name}? Its new logs will wait under Keys waiting until it is tied to a car again. Logs already read stay.`, { title: 'Untie the key?', ok: 'Untie', danger: true })) keyChange(k, { car: null }, `Key ${k.key} is no longer tied to ${car().name}.`);
        } }, 'Untie') },
      ],
      empty: () => emptyState({ icon: 'chip', title: 'No key tied to this car', text: 'The first log sent with this car\'s upload token ties its key here. A key waiting below can be assigned by hand.' }),
    });
    replace(slots.uploads,
      panel({ title: 'Uploads, tokens and keys', icon: 'upload', side: h('span', { class: 'faint' }, 'Admins only') }, outcome()),
      tokens.el, keys.el, waitingTable(ctx, S.waiting, car(), refresh));
  }

  /* ---- Time zone */
  const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
  function paintTz() {
    const z = car().time_zone || '';
    const zones = ZONES.includes(z) || !z ? ZONES : [z, ...ZONES];
    const listId = `sc-zones-${cid}`;
    const tzField = field({ label: 'Time zone', name: 'sc-tz', path: 'time_zone', value: z, hint: 'A zone name such as America/Chicago or Europe/Amsterdam. Type to search the list.', attrs: { list: listId, disabled: ro, autocomplete: 'off', spellcheck: 'false' } });
    const dl = h('datalist', { id: listId }, zones.map((x) => h('option', { value: x })));
    tzField.input.addEventListener('change', () => {
      const v = tzField.input.value.trim();
      if (!v || v === car().time_zone) return;
      if (!isValidZone(v)) { tzField.setError('Not a time zone this browser knows. Pick one from the list.'); return; }
      patchCar({ time_zone: v }, 'Time zone', tzField);
    });
    const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
    const now = ctx.api.now();
    replace(slots.tz, panel({ title: 'Time zone', icon: 'clock', cover: 'docs.env.core.tz' },
      setting('The car\'s time zone', 'Days, ISO weeks, months, statistics, charging heat maps and prices by time of day all use this zone, wherever the server runs. A change regroups the whole history at once; nothing is lost.',
        [tzField.el, dl,
          h('p', { class: 'setting-note' }, 'Now in this zone: ', h('b', null, time(now, z || ctx.tz)), `, ${date(now, z || ctx.tz)} (${tzName(z || ctx.tz)}).`),
          browserZone && browserZone !== z && !ro ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => patchCar({ time_zone: browserZone }, 'Time zone', tzField) }, icon('globe', 'ico--sm'), `Use this browser's zone (${browserZone})`) : null])));
  }

  /* ---- Battery chemistry */
  function paintBattery() {
    const bc = S.settings.battery_chemistry || { setting: 'auto' };
    const b = S.battery;
    const chosen = bc.setting;
    const seg = h('div', { class: 'seg', role: 'radiogroup', 'aria-label': 'Battery chemistry' },
      Object.entries(CHEM).map(([v, l]) => dis(h('button', { class: 'seg-btn', type: 'button', role: 'radio', 'aria-checked': String(v === chosen), 'aria-pressed': String(v === chosen),
        onclick: () => { if (v !== chosen) patchSettings({ battery_chemistry: { setting: v } }, 'Battery chemistry'); } }, l))));
    const detected = bc.detected ? CHEM[bc.detected] : null;
    const effective = chosen === 'auto' ? bc.detected : chosen;
    const evidence = h('div', { class: 'sc-evidence' },
      h('p', null, h('b', null, 'Detected: '), detected || 'not yet', detected ? '' : ' (a few charges and some driving are needed)', '.'),
      bc.evidence ? h('p', null, h('b', null, 'Evidence: '), bc.evidence) : null,
      b?.brick_spread_v != null ? h('p', null, h('b', null, 'Brick voltage spread now: '), format(b.brick_spread_v, 'voltage', { digits: 3 }), ' (lowest to highest cell group).') : null,
      b?.capacity?.full_pack_kwh != null ? h('p', null, h('b', null, 'Full pack: '), format(b.capacity.full_pack_kwh, 'energy'), ', from the battery management system.') : null,
      chosen !== 'auto' && bc.detected && bc.detected !== chosen ? h('p', { class: 'warn' }, icon('warn', 'ico--sm'), ` Your choice differs from what the car's data suggests (${detected}).`) : null);
    replace(slots.battery, panel({ title: 'Battery', icon: 'battery', cover: 'web.settings.car.lfp-battery' },
      setting('Chemistry', 'LFP packs like a full charge now and then, and their charge level reads flat in the middle; nickel packs (NCA or NMC) last longer kept lower. Battery and Charges use this for their charge-level advice. Automatic follows the evidence below.',
        [seg, h('p', { class: 'setting-note' }, 'In use now: ', h('b', null, effective ? CHEM[effective] : 'not known yet'), chosen === 'auto' ? ' (automatic).' : ' (set by hand).'), evidence])));
  }

  /* ---- Reference values (Battery, Efficiency and Software link here) */
  function paintRefs() {
    const s = S.settings;
    const disp = (v, kind) => (v == null ? '' : String(Math.round(convert(v, kind) * 10) / 10));
    const defs = [
      ['capacity_new_kwh', 'Capacity when new', 'kWh, 10 to 250. Leave empty to take it from the pack\'s first readings.', null, { min: '10', max: '250', step: '0.1' }],
      ['full_range_new_km', 'Full range when new', `${unitLabel('distance')}. Leave empty to take it from the first 30 days.`, 'distance', { min: '0', step: '1' }],
      ['warranty_threshold_pct', 'Warranty threshold', '% of the capacity when new that the battery warranty promises, 50 to 95.', null, { min: '50', max: '95', step: '1', required: true }],
      ['efficiency_wh_per_km_override', 'Consumption factor', `${unitLabel('efficiency')}. Leave empty to work it out from energy and range.`, 'efficiency', { min: '0', step: '1' }],
    ];
    const fields = defs.map(([k, label, hint, kind, attrs]) => {
      const f = field({ label, name: `sc-${k}`, path: k, type: 'number', value: kind ? disp(s[k], kind) : s[k] ?? '', hint, attrs: { ...attrs, disabled: ro } });
      f.input.addEventListener('change', () => {
        const raw = f.input.value.trim();
        if (raw === '' && k === 'warranty_threshold_pct') { f.setError('A number from 50 to 95.'); return; }
        const n = raw === '' ? null : Number(raw);
        if (n != null && !Number.isFinite(n)) { f.setError('A number.'); return; }
        const v = n == null ? null : kind ? Math.round(toMetric(n, kind) * 10) / 10 : n;
        patchSettings({ [k]: v }, label, (errs) => f.setError(errs[k] || Object.values(errs)[0]));
      });
      return f;
    });
    const sw = field({ label: 'Software version, typed', name: 'sc-sw', path: 'software_version_typed', value: s.software_version_typed || '', hint: 'Only for a car whose bus does not send it, such as 2026.38.4. Leave empty to use what the car reports.', attrs: { disabled: ro, autocomplete: 'off' } });
    sw.input.addEventListener('change', () => patchSettings({ software_version_typed: sw.input.value.trim() || null }, 'Software version', (errs) => sw.setError(errs.software_version_typed || Object.values(errs)[0])));
    replace(slots.refs, panel({ title: 'Reference values', icon: 'gauge', side: h('span', { class: 'faint' }, 'Optional; empty means worked out') },
      setting('Battery and range', 'Battery health compares today\'s capacity and range with these. Without them Sentry Mate uses the earliest readings it has, and says so on the Battery page.', [fields[0].el, fields[1].el, fields[2].el]),
      setting('Consumption and software', 'The consumption factor turns range into energy on Efficiency. A typed software version is marked as typed on Software.', [fields[3].el, sw.el])));
  }

  /* ---- Free Supercharging as dated ranges */
  function paintCharging() {
    const c = car();
    const access = c.config?.supercharging_access;
    let ranges = (S.settings.free_supercharging || []).map((r) => ({ ...r }));
    const list = h('div', { class: 'sc-ranges' });
    const msg = h('p', { class: 'form-error', role: 'alert', hidden: true });

    async function save(rows, what) {
      msg.hidden = true;
      const clean = rows.filter((r) => r.from_date).map((r) => ({ from_date: r.from_date, to_date: r.to_date || null }));
      await patchSettings({ free_supercharging: clean }, what, (errs) => {
        paintRows(rows);
        for (const [path, text] of Object.entries(errs)) {
          const m = /^free_supercharging\.(\d+)\.(from_date|to_date)$/.exec(path);
          const fe = m && list.querySelector(`[data-err="${m[1]}-${m[2]}"]`);
          if (fe) { fe.hidden = false; replace(fe, icon('warn'), text); } else { msg.hidden = false; replace(msg, icon('warn'), String(text)); }
        }
      });
    }

    function paintRows(rows) {
      ranges = rows;
      replace(list, rows.length ? rows.map((r, i) => {
        const from = dis(h('input', { class: 'input', type: 'date', value: r.from_date || '', 'aria-label': `Range ${i + 1}: first day`, required: true }));
        const to = dis(h('input', { class: 'input', type: 'date', value: r.to_date || '', 'aria-label': `Range ${i + 1}: last day (empty: still free)` }));
        const onChange = () => {
          const next = ranges.map((x, j) => (j === i ? { from_date: from.value || null, to_date: to.value || null } : x));
          if (!from.value) { ranges = next; const fe = list.querySelector(`[data-err="${i}-from_date"]`); fe.hidden = false; replace(fe, icon('warn'), 'Give the first free day.'); return; }
          save(next, 'Free Supercharging');
        };
        from.addEventListener('change', onChange); to.addEventListener('change', onChange);
        return h('div', { class: 'sc-range' },
          h('label', { class: 'sc-range-f' }, h('span', { class: 'form-label' }, 'From'), from, h('span', { class: 'form-error', dataset: { err: `${i}-from_date` }, hidden: true })),
          h('label', { class: 'sc-range-f' }, h('span', { class: 'form-label' }, 'To (included)'), to, h('span', { class: 'form-error', dataset: { err: `${i}-to_date` }, hidden: true })),
          h('span', { class: 'sc-range-say faint' }, r.from_date ? (r.to_date ? `Free for ${days(r.from_date, r.to_date)} days` : 'Free from then on, no end') : 'New range'),
          ro ? null : h('button', { class: 'btn btn--sm btn--ghost', type: 'button', 'aria-label': `Remove range ${i + 1}`, onclick: () => {
            const next = ranges.filter((_, j) => j !== i);
            if (r.from_date) save(next, 'Free Supercharging'); else paintRows(next);
          } }, icon('x', 'ico--sm'), 'Remove'));
      }) : h('p', { class: 'faint' }, 'No free ranges: every Supercharger session keeps the price it was billed or priced at.'));
    }
    paintRows(ranges);

    const hint = access?.value != null
      ? h('p', { class: 'sc-hint' }, icon('info', 'ico--sm'), ' The car reports Supercharging access: ', h('b', null, String(access.value)), SOURCE[access.source] ? [' ', tag(...SOURCE[access.source])] : null,
        '. That says what the car is entitled to now, not since when, so the dates are yours to give.')
      : h('p', { class: 'sc-hint' }, icon('info', 'ico--sm'), ' The car has not reported its Supercharging access yet. The dates are yours to give.');

    replace(slots.charging, panel({ title: 'Charging', icon: 'bolt', cover: 'web.settings.car.free-supercharging' },
      setting('Free Supercharging', 'Tesla Supercharger sessions of this car that start on a free day cost nothing; sessions before or after keep their own cost, so a range never rewrites the rest of the history. Days are the car\'s days, in its time zone.',
        [hint, list, msg, ro ? null : h('button', { class: 'btn btn--sm', type: 'button', onclick: () => paintRows([...ranges, { from_date: null, to_date: null }]) }, icon('plus', 'ico--sm'), 'Add a free range')])));
  }

  /* ---- the page */
  paintAll();
  const head = pageHead({
    eyebrow: S.car.name, title: ctx.info.title, purpose: ctx.info.purpose,
    back: { href: state.buildUrl('settings-cars'), label: 'All cars' },
    sub: [carMeta(S.car) || null, S.car.vin_prefix ? `VIN ${S.car.vin_prefix}…` : null, `Times in ${tzName(tz())}`],
    actions: [h('a', { class: 'btn btn--sm', href: state.buildUrl('car', { car: cid }) }, icon('gauge', 'ico--sm'), 'Car overview')],
  });
  head.querySelector('.back')?.setAttribute('data-cover', 'web.settings.car.breadcrumb');
  head.querySelector('.back')?.setAttribute('aria-label', 'Back to Settings: Cars, every car');

  replace(ctx.view, head,
    h('div', { class: 'settings-stack' },
      h('p', { class: 'note saving-note', cover: 'web.settings.car.autosave' }, icon('check', 'ico--sm'),
        ro ? ' These settings are changed by an admin; you can read them here.' : ' Every change here is saved at once; there is no Save button. A short note confirms it.'),
      slots.car, slots.uploads, h('div', { class: 'sc-two' }, slots.tz, slots.battery), slots.charging, slots.refs));
  ctx.events(['fleet'], (ev) => { if (ev.data?.car === cid && ev.data?.change === 'renamed' && ev.data.name !== S.car.name) refresh(); });
  ctx.ready();
}

// whole days from a to b, both included (dates as YYYY-MM-DD; calendar days, no time zone involved)
function days(a, b) {
  const [y1, m1, d1] = a.split('-').map(Number), [y2, m2, d2] = b.split('-').map(Number);
  return Math.round((Date.UTC(y2, m2 - 1, d2) - Date.UTC(y1, m1 - 1, d1)) / 86400000) + 1;
}
