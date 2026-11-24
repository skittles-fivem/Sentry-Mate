// Settings: Cars (/settings/cars and /settings/cars/{car}): per-car settings, and how each car's CAN
// Bridge is tied to it. Without a car: every car the account sees (searched and paged by the server),
// keys waiting for a car, and how a car appears. With a car: its name, what it reports about itself,
// its upload tokens and CAN Bridge keys (with the outcome of its uploads; an admin names a key, can-key-name.js), its time zone, battery
// chemistry, reference values, free Supercharging as dated ranges, and its data sources (#data-sources: the
// live inputs' priority order, gap filling from an import, a link to the overlaps). Every change saves at once
// (PATCH with the object's version) and a short note confirms it. The index also holds the time zone a new car
// starts in (#default-tz: the server's default_time_zone).

import { h, replace, keepFocus, setTitle as docTitle } from '../shared/dom.js';
import * as state from '../shared/state.js';
import { invalidate } from '../shared/api.js';
import { isValidZone, dayKey, zoneSourceText } from '../shared/time.js';
import { convert, toMetric, unitLabel, format } from '../shared/units.js';
const num = (v) => String(Math.round(v * 10) / 10);
import { icon } from '../core/icons.js';
import { pageHead, panel, panelLink, kv, tag, pill, note } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { field as baseField, showApiError, once } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { openCarSwitcher, SWITCH_KEYS } from '../core/carswitch.js';
import { carChanged } from '../core/frame.js';
import { dateTime, date, time, ago, agoEl, tzName, carMeta, statePill } from '../core/fmt.js';
import { deviceText, deviceWords, renameKeyButton } from './can-key-name.js';
import { deviceName } from '../shared/devices.js';

export const id = 'settings-cars';
export const css = true;
export const title = (ctx) => (ctx.car ? 'Car settings' : 'Settings: Cars');

const enc = encodeURIComponent;
const KEEP = Symbol('keep');   // returned by an onFields that shows the server's words on a list it keeps as typed

// the fields of the car page as drawn now, by their API path: after a save that failed the page is drawn again
// from what is saved, and the message a field shows (a 422) is put on the field that takes the place of it
let fieldsNow = new Map();
function field(opts) {
  const f = baseField(opts);
  fieldsNow.set(f.path, f);
  return f;
}
const SOURCE = { can: ['From SMO', 'live'], vin: ['From VIN', 'plain'], owner: ['Typed', 'warm'], teslamate: ['From TeslaMate', 'plain'], tesla_api: ['From the Tesla API', 'plain'] };
const CHEM = { auto: 'Automatic', lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA / NMC (nickel)' };
const CONFIG_ROWS = [
  ['trim', 'Trim'], ['drivetrain', 'Drivetrain'], ['performance_package', 'Performance package'], ['pack_kwh_code', 'Pack code'],
  ['wheel', 'Wheels'], ['paint', 'Paint'], ['roof', 'Roof'], ['spoiler', 'Spoiler'], ['right_hand_drive', 'Right-hand drive'],
  ['map_region', 'Map region'], ['supercharging_access', 'Supercharging access'],
];
// the four live inputs, highest priority first, as the server orders them when nothing is chosen
const SOURCE_ORDER = ['smo', 'tesla_api', 'teslafi_api', 'teslamate_local'];
const INPUT_LABEL = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'local TeslaMate' };
const yesNo = (v) => (v === true ? 'Yes' : v === false ? 'No' : v);

// a row: the label and its explanation on the left, the control on the right (the app's .setting)
const setting = (titleText, text, control, cv = null) => h('div', { class: 'setting', cover: cv },
  h('div', null, h('h3', null, titleText), text ? h('p', null, text) : null), h('div', { class: 'setting-control' }, control));

// the currency codes this browser knows (ISO 4217), or null where it cannot say: then any three letters go to the server
const KNOWN_CURRENCIES = (() => { try { return new Set(Intl.supportedValuesOf('currency')); } catch { return null; } })();
const soldText = (c) => (c.sold_date ? `Sold on ${c.sold_date}` : 'Sold (date not given)');

// the sentence for an error that did not land under a field
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);

/** How a car appears: the steps, shown as the empty state and as a note beside the car picker. */
function howCarsAppear(admin) {
  return h('ol', { class: 'sc-steps' },
    h('li', null, h('b', null, 'Install the CAN Bridge app'), ' on the Screenmate One (or another logger, such as a Raspberry Pi) in the car.'),
    admin
      ? h('li', null, h('b', null, 'Make an upload token'), ' with ', h('b', null, 'Make upload token'), ' under Upload tokens for a new car, and enter it in the app. It is shown once, so copy it straight in.')
      : h('li', null, h('b', null, 'Enter an upload token'), ' in the app. An admin makes one on this page.'),
    h('li', null, h('b', null, 'Wait for the first log.'), ' The car shows up here by itself, with its model and year from the VIN prefix. Name it, and set its time zone.'),
    h('li', null, h('b', null, 'Or a Tesla account'), ' (optional): added under Settings: Integrations, its cars are read from the Tesla API once each is matched to a car here or to a new car.'),
    h('li', null, h('b', null, 'A second logger'), ' in the same car (a new Screenmate, a reinstall, a Pi) brings a new key. It waits under Keys waiting until it is tied to the car.'));
}

/**
 * The dialog that shows a new token, once: the three fields of the CAN Bridge app's "Second Sentry Six Manager"
 * section (server address, token, certificate SHA-256), each with its own Copy button.
 */
async function showSecret(t, titleText, after) {
  const app = t.app || { address: null, token: t.secret, certificate_sha256: null };
  const row = (label, value, explain, secret = false) => {
    const input = h('input', { class: 'input mono sc-secret', readonly: true, value: value || '', 'aria-label': label, onfocus: (e) => e.target.select() });
    const said = h('span', { class: 'faint sc-copied', 'aria-live': 'polite' });
    return h('div', { class: ['sc-app-row', secret && 'sc-app-row--secret'] },
      h('p', { class: 'sc-app-label' }, label, secret ? tag('shown once', 'warm') : null),
      value ? h('div', { class: 'row sc-secret-row' }, input,
        h('button', { class: 'btn btn--sm', type: 'button', onclick: async () => {
          try { await navigator.clipboard.writeText(value); said.textContent = 'Copied.'; } catch { input.select(); said.textContent = 'Select and copy it by hand (Ctrl+C).'; }
        } }, icon('check', 'ico--sm'), 'Copy'), said) : h('p', { class: 'faint' }, 'Not known to this server: an admin sees it under Settings: Server and access.'),
      h('p', { class: 'form-hint' }, explain));
  };
  await openDialog({
    title: titleText, wide: true,
    body: [
      h('p', null, 'In the CAN Bridge app, open ', h('b', null, 'Second Sentry Six Manager'), ' and fill in these three fields. ', h('b', null, 'The token is shown only now:'), ' the server keeps only a check value, so nobody can read it back later. If it is lost, make a new one and revoke this one.'),
      h('div', { class: 'sc-app' },
        row('Server address', app.address, 'Where the app sends its logs: this server\'s upload port, over HTTPS.'),
        row('Token', app.token || t.secret, 'Lets the app upload. Keep it like a password.', true),
        row('Certificate SHA-256', app.certificate_sha256, 'The app trusts this server by this fingerprint, so it needs no certificate authority. It stays the same across restarts, updates and moves.')),
      h('p', { class: 'note' }, after),
    ],
    actions: [{ label: 'I have copied them', value: true, kind: 'primary' }],
  });
}

const tokenState = (t) => (t.revoked ? pill('Revoked', { tone: 'plain' }) : t.last_upload_ms ? pill('In use', { tone: 'live' }) : pill('Not used yet', { tone: 'warm' }));
const tokenStateText = (t) => (t.revoked ? 'revoked' : t.last_upload_ms ? 'in use' : 'not used yet');

/* ====================================================================== the page */

export async function mount(ctx) {
  if (!ctx.car) return mountIndex(ctx);
  return mountCar(ctx);
}

/* ---------------------------------------------------------------------- /settings/cars */

async function mountIndex(ctx) {
  const admin = ctx.admin;
  let status = null, waiting = null, fresh = null;
  try {
    [status, waiting, fresh] = await Promise.all([
      ctx.api.get('setup/status').catch(() => null),
      admin ? ctx.api.get('can-keys', { assigned: 'false', limit: 100 }) : Promise.resolve(null),
      admin ? ctx.api.get('upload-tokens', { limit: 100 }) : Promise.resolve(null),
    ]);
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const noCars = status && status.cars === 0;
  const empty = h('section', { class: 'panel panel-pad sc-empty', cover: 'web.settings.car.empty-state', hidden: !noCars },
    emptyState({ icon: 'car', title: 'No cars yet', text: 'A car appears here when its CAN Bridge (SMO) uploads its first log. No Tesla account is needed; adding one (Settings: Integrations) is an optional second input, and its cars can be matched to new cars here.' }),
    howCarsAppear(admin));

  const table = noCars ? null : dataTable(ctx, {
    title: 'Cars', icon: 'car', path: 'cars', params: { sort: 'custom' }, label: 'Cars and their settings', csvName: 'cars',
    note: 'In your own car order. Search finds a car by name, model or VIN prefix. Open a car to change its settings.',
    side: h('input', { class: 'input sc-search', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car',
      oninput: (e) => { clearTimeout(e.target._t); e.target._t = setTimeout(() => table.setParams({ search: e.target.value.trim() || null }), 200); } }),
    columns: [
      { key: 'name', label: 'Car', render: (c) => [h('a', { class: 'link', href: state.buildUrl('settings-cars', { car: c.id }) }, c.name), state.isSold(c) ? [' ', tag('Sold', 'plain')] : null], csv: (c) => c.name },
      { key: 'model', label: 'Model and year', render: (c) => carMeta(c) || '—', csv: (c) => carMeta(c) },
      { key: 'vin_prefix', label: 'VIN prefix', render: (c) => h('span', { class: 'mono' }, c.vin_prefix || '—') },
      { key: 'time_zone', label: 'Time zone', render: (c) => c.time_zone || '—' },
      { key: 'state', label: 'Now', render: (c) => statePill(c.now?.state || 'unknown', { stale: c.now?.stale }), csv: (c) => c.now?.state || 'unknown' },
      { key: 'last', label: 'Last upload', render: (c) => (c.now?.received_ms ? agoEl(c.now.received_ms, c.time_zone) : 'Never'), csv: (c) => c.now?.received_ms ?? '' },
    ],
    rowHref: (c) => state.buildUrl('settings-cars', { car: c.id }),
    empty: () => emptyState({ icon: 'search', title: 'No car matches', text: 'Search looks at the name, the model and the VIN prefix. Clear the search to see every car.' }),
  });

  const waitPanel = admin ? waitingTable(ctx, waiting?.items || [], null, () => ctx.reload(), noCars) : null;
  const newTokens = admin ? newCarTokens(ctx, fresh?.items || [], noCars) : null;
  const zone = defaultZonePanel(ctx, admin);

  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'settings-stack' },
      h('p', { class: 'note saving-note', cover: 'web.settings.car.autosave' }, icon('check', 'ico--sm'),
        admin ? ' Every change on these pages is saved at once; there is no Save button. A short note confirms it.' : ' Car settings are changed by an admin; you can read them here.'),
      empty,
      noCars ? newTokens : null,
      table ? table.el : null,
      noCars ? null : newTokens,
      waitPanel,
      zone.el,
      noCars ? null : panel({ title: 'How a car appears', icon: 'info' }, howCarsAppear(admin))));
  await zone.ready;
  if (table) await table.ready;
  if (!ctx.alive()) return;
  ctx.ready();
}

/* ---------------------------------------------------------------------- time zone for new cars (index) */

/**
 * The zone a new car starts in (#default-tz; the server's default_time_zone, PATCH /settings with its version).
 * An admin changes it; others read it. Each car keeps its own zone after that.
 */
function defaultZonePanel(ctx, admin) {
  const el = panel({ title: 'Time zone for new cars', icon: 'clock', id: 'default-tz', side: admin ? null : h('span', { class: 'faint' }, 'Read only') });
  let server = null;
  const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
  const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
  async function save(zone, f) {
    try {
      server = await ctx.api.patch('settings', { version: server.version, default_time_zone: zone });
      flash('Time zone for new cars saved.');
    } catch (e) {
      if (e?.name === 'AbortError') return;
      if (e.status === 409) { server = await ctx.api.get('settings').catch(() => server); flash('Someone else changed the settings; this page shows theirs now.', { kind: 'info' }); }
      else if (e.fields) { f.reset(); f.setError(Object.values(e.fields).join(' ')); return; }
      else flash(`Could not save the time zone for new cars: ${errText(e)}`, { kind: 'error' });
    }
    if (ctx.alive()) keepFocus(el, paint);
  }
  function paint() {
    if (!server) { replace(el, el.firstChild, h('p', { class: 'note' }, 'The server settings could not be read.')); return; }
    const z = server.default_time_zone || 'UTC';
    const f = field({ label: 'Time zone for new cars', name: 'sc-default-tz', path: 'default_time_zone', value: z, hint: 'A zone name such as America/Chicago. Type to search the list.', attrs: { list: 'sc-default-zones', disabled: !admin, autocomplete: 'off', spellcheck: 'false' } });
    f.input.addEventListener('change', () => {
      const v = f.input.value.trim();
      if (!isValidZone(v)) { f.setError('Not a time zone this browser knows: pick one from the list, such as America/Chicago.'); return; }
      save(v, f);
    });
    const row = setting('The zone a new car starts in', 'Its days, weeks and prices by time of day, until the car\'s position or someone sets its own. Each car keeps its own zone after that: open a car above to change it.',
      [f.el, h('datalist', { id: 'sc-default-zones' }, ZONES.map((x) => h('option', { value: x }))),
        browserZone && browserZone !== z && admin ? [h('button', { class: 'btn btn--sm', type: 'button', 'aria-label': `Use this browser's zone, ${browserZone}`, onclick: () => save(browserZone, f) }, icon('globe', 'ico--sm'), 'Use this browser’s zone'),
          h('p', { class: 'setting-note' }, `This browser is in ${browserZone}.`)] : null]);
    const head = el.firstChild;
    replace(el, head, row);
  }
  const ready = ctx.api.get('settings').then((x) => { server = x; }).catch(() => null).then(() => { if (ctx.alive()) paint(); });
  return { el, ready };
}

/* ---------------------------------------------------------------------- upload tokens for a new car (index) */

function newCarTokens(ctx, rows, noCars) {
  const tz = ctx.tz;
  let list = rows;
  async function reloadList(fallback) {
    list = (await ctx.api.get('upload-tokens', { limit: 100 }).catch(() => null))?.items || fallback;
    if (ctx.alive()) t.setRows(list);
  }
  async function make() {
    let tok;
    try { tok = await ctx.api.post('upload-tokens', {}); } catch (e) { flash(`Could not make a token: ${errText(e)}`, { kind: 'error' }); return; }
    await showSecret(tok, 'Upload token for a new car', 'The first log sent with it makes the car here, named after its model and VIN prefix until you name it. The token then moves to that car\'s own page.');
    await reloadList([tok, ...list]);
  }
  async function revoke(x) {
    if (!(await confirmDialog(`Revoke upload token ${x.id}? A logger that has it can no longer bring in a car with it.`, { title: 'Revoke the token?', ok: 'Revoke', danger: true }))) return;
    try {
      const r = await ctx.api.del(`upload-tokens/${enc(x.id)}`);
      flash(`Token ${x.id} revoked.`);
      await reloadList(list.map((y) => (y.id === x.id ? { ...y, ...r } : y)));
    } catch (e) { flash(`Could not revoke the token: ${errText(e)}`, { kind: 'error' }); }
  }
  const t = dataTable(ctx, {
    title: 'Upload tokens for a new car', icon: 'lock', rows: list, label: 'Upload tokens for a car that has not uploaded yet', csvName: 'new-car-tokens', cover: 'web.settings.car.empty-state',
    note: noCars
      ? 'Step 2 above: make a token here and enter it in the CAN Bridge app. The car appears on this page with its first log.'
      : 'For a car that is not here yet: its first log makes the car. For a car already here, make the token on its own page instead, so the logs go to that car.',
    side: h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: once(make) }, icon('plus', 'ico--sm'), 'Make upload token'),
    sort: { key: 'created', dir: 'desc' },
    columns: [
      { key: 'id', label: 'Token', render: (x) => h('span', { class: 'mono' }, x.id), sort: true },
      { key: 'created', label: 'Made', value: (x) => x.created_ms, sort: true, render: (x) => dateTime(x.created_ms, tz), csv: (x) => x.created_ms },
      { key: 'state', label: 'State', value: (x) => (x.revoked ? 1 : 0), sort: true, render: tokenState, csv: tokenStateText },
      { key: 'act', label: '', csv: false, render: (x) => (x.revoked ? null : h('button', { class: 'btn btn--sm btn--danger', type: 'button', onclick: once(() => revoke(x)) }, icon('x', 'ico--sm'), 'Revoke')) },
    ],
    empty: () => emptyState({ icon: 'lock', title: 'No token for a new car', text: noCars ? 'Press Make upload token to get the first car in.' : 'None is waiting for a car. Press Make upload token when a new car joins.' }),
  });
  return t.el;
}

/* ---------------------------------------------------------------------- keys waiting (both routes) */

function waitingTable(ctx, rows, car, after, noCars = false) {
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
      : noCars
        ? 'A CAN Bridge signs every log with its own key. These keys uploaded logs but are not tied to any car yet; their logs wait, unread. There is no car to tie them to until the first car appears; then open it and assign the key there.'
        : 'A CAN Bridge signs every log with its own key. These keys uploaded logs but are not tied to any car yet; their logs wait, unread. Open the car it records to assign it.',
    sort: { key: 'first', dir: 'desc' },
    columns: [
      { key: 'key', label: 'Key', render: (k) => h('span', { class: 'mono' }, k.key), sort: true },
      { key: 'device', label: 'Device', value: deviceWords, csv: deviceWords, sort: true,
        render: (k) => h('span', { class: 'row sc-dev' }, deviceText(k), renameKeyButton(ctx, k, ctx.admin, after)) },
      { key: 'first', label: 'First upload', value: (k) => k.first_seen_ms, sort: true, render: (k) => dateTime(k.first_seen_ms, tz), csv: (k) => k.first_seen_ms },
      { key: 'last', label: 'Last upload', value: (k) => k.last_upload_ms, sort: true, render: (k) => (k.last_upload_ms ? agoEl(k.last_upload_ms, tz) : 'Never'), csv: (k) => k.last_upload_ms ?? '' },
      { key: 'logs_waiting', label: 'Logs waiting', num: true, sort: true, render: (k) => format(k.logs_waiting ?? 0, 'count') },
      { key: 'act', label: car ? 'Assign' : 'What to do', csv: false, render: (k) => (car
        ? h('div', { class: 'row sc-acts' },
          h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: once(() => assign(k, false)) }, icon('plus', 'ico--sm'), `Assign to ${car.name}`),
          h('button', { class: 'btn btn--sm', type: 'button', onclick: once(() => assign(k, true)) }, 'Assign and prefer'))
        : h('span', { class: 'faint' }, noCars ? 'No car to tie it to yet. Once a car appears, open it and Assign' : 'Open its car, then Assign')) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No keys waiting', text: 'Every CAN Bridge that uploaded is tied to a car. A key waits here when a new logger uploads with a key the server has not tied to a car.' }),
  }).el;
}

/* ---------------------------------------------------------------------- /settings/cars/{car} */

async function mountCar(ctx) {
  const admin = ctx.admin;
  const cid = ctx.car.id;
  const S = { car: null, settings: null, tokens: [], keys: [], waiting: [], health: null, battery: null, cars: null, currency: null };

  // first: the page's first load reuses the car the router read a moment ago; later loads ask again
  async function load(first = false) {
    const [car, settings, tokens, keys, waiting, health, battery, cars] = await Promise.all([
      ctx.api.get(`cars/${enc(cid)}`, null, first ? { maxAgeMs: 5000 } : undefined),
      ctx.api.get(`cars/${enc(cid)}/settings`),
      admin ? ctx.api.get(`cars/${enc(cid)}/upload-tokens`, { limit: 100 }) : null,
      admin ? ctx.api.get('can-keys', { car: cid, limit: 100 }) : null,
      admin ? ctx.api.get('can-keys', { assigned: 'false', limit: 100 }) : null,
      ctx.api.get(`cars/${enc(cid)}/health`).catch(() => null),
      ctx.api.get(`cars/${enc(cid)}/battery`).catch(() => null),
      ctx.api.page('cars', { sort: 'custom', limit: 8 }).catch(() => null),
    ]);
    Object.assign(S, { car, settings, tokens: tokens?.items || [], keys: keys?.items || [], waiting: waiting?.items || [], health, battery, cars });
    // the currency for an empty default price: the one the car's charges cost most in
    if (!S.currency) S.currency = settings.default_price_per_kwh?.currency || topCurrency(await ctx.api.get(`cars/${enc(cid)}/charges`, { limit: 1 }).catch(() => null));
  }
  try { await load(true); } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const car = () => S.car;
  const tz = () => S.car.time_zone || ctx.tz;
  const ro = !admin;
  const dis = (el) => { if (ro) el.setAttribute('disabled', ''); return el; };

  /* ---- saving: the car (name, time zone) and its settings, each with its version. Saves of one object go
     one at a time, so each sends the version the one before it got back (two quick changes never clash). */
  const queue = { car: Promise.resolve(), settings: Promise.resolve() };
  const inFlight = { car: 0, settings: 0 };
  function serial(kind, fn) {
    inFlight[kind]++;
    const run = queue[kind].then(fn).finally(() => { inFlight[kind]--; });
    queue[kind] = run.catch(() => {});
    return run;
  }
  // repaint once the last waiting save of an object is answered, so a repaint never drops a change still on its way
  const settle = (kind) => { if (inFlight[kind] <= 1) paintAll(); };

  function patchCar(body, what, f = null) {
    return serial('car', async () => {
      try {
        S.car = await ctx.api.patch(`cars/${enc(cid)}`, { version: S.car.version, ...body });
        invalidate('cars');
        if (ctx.car) Object.assign(ctx.car, { name: S.car.name, time_zone: S.car.time_zone, time_zone_source: S.car.time_zone_source });
        carChanged(S.car);
        f?.setError(null);
        flash(`${what} saved.`);
        settle('car');
      } catch (e) {
        if (e.status === 409) { await refresh(); flash('Someone else changed this car; the page shows their change now. Try again.', { kind: 'info' }); return; }
        failed('car', () => {
          if (f && e.fields) showApiError({ fields: [f], setMessage: (t) => t && flash(t, { kind: 'error' }) }, e);
          else flash(`Could not save ${what.toLowerCase()}: ${errText(e)}`, { kind: 'error' });
        });
      }
    });
  }
  function patchSettings(body, what, onFields = null) {
    return serial('settings', async () => {
      try {
        S.settings = await ctx.api.patch(`cars/${enc(cid)}/settings`, { version: S.settings.version, ...body });
        flash(`${what} saved.`);
        settle('settings');
        return true;
      } catch (e) {
        if (e.status === 409) { await refresh(); flash('Someone else changed these settings; the page shows theirs now. Try again.', { kind: 'info' }); return false; }
        let keep = false;
        failed('settings', () => {
          if (onFields && e.fields) keep = onFields(e.fields) === KEEP;
          else flash(`Could not save ${what.toLowerCase()}: ${errText(e)}`, { kind: 'error' });
        }, () => keep);
        return false;
      }
    });
  }
  async function refresh() {
    try { await load(); } catch (e) { flash(`Could not load the car again: ${errText(e)}`, { kind: 'error' }); return; }
    if (ctx.alive()) paintAll();
  }

  /* ---- slots, painted again after each change */
  const slots = { car: h('div'), uploads: h('div', { class: 'settings-stack' }), tz: h('div'), battery: h('div'), refs: h('div'), charging: h('div'), sources: h('div') };

  function paintAll() {
    if (!ctx.alive()) return;
    fieldsNow = new Map();
    // the keyboard focus stays where it was (a field tabbed out of, a button) when its panel is drawn again
    const root = slots.car.parentElement?.parentElement || ctx.view;
    const was = document.activeElement;
    const sig = was && was !== root && root.contains(was) ? signature(was) : null;
    const slot = sig && Object.values(slots).find((x) => x.contains(was));
    keepFocus(root, () => {
      paintHead(); paintCar(); paintUploads(); paintSources(); paintTz(); paintBattery(); paintRefs(); paintCharging();
      // the same control by its name or its words (a panel that gained or lost a line moved it); if it is gone, the
      // first control of its panel
      if (sig && !was.isConnected && (!document.activeElement || document.activeElement === document.body)) {
        const to = [...root.querySelectorAll(FOCUSABLE)].find((x) => !x.disabled && signature(x) === sig) || (slot && slot.querySelector(FOCUSABLE));
        to?.focus({ preventScroll: true });
      }
    });
  }
  const FOCUSABLE = 'a[href], button, input, select, textarea, [tabindex]';
  const signature = (el) => (el.name ? `${el.localName}[${el.name}]` : `${el.localName} ${el.getAttribute('aria-label') || el.textContent.trim()}`);

  // After a save that failed: the page is drawn again from what is saved, so no field keeps a value that is not.
  // `show()` puts the server's words on the fields; the ones it set are shown again on the fields drawn anew.
  // A later save waiting will draw the page when it is answered; keep() true leaves the page as it is (a list the
  // person is still editing, its errors shown on its rows).
  function failed(kind, show, keep = () => false) {
    const said = () => new Map([...fieldsNow].map(([p, f]) => [p, f.error()]).filter(([, t]) => t));
    const before = said();
    show();
    if (inFlight[kind] > 1 || keep()) return;
    const carry = [...said()].filter(([p, t]) => before.get(p) !== t);
    paintAll();
    for (const [p, t] of carry) fieldsNow.get(p)?.setError(t);
    const first = carry.map(([p]) => fieldsNow.get(p)).find((f) => f && !f.input.disabled);
    first?.input.focus();
  }

  /* ---- the head, the breadcrumb and the tab title: they carry the name and the time zone */
  let head = null;
  function paintHead() {
    const next = pageHead({
      eyebrow: S.car.name, title: ctx.info.title, purpose: ctx.info.purpose,
      back: { href: state.buildUrl('settings-cars'), label: 'All cars' },
      sub: [S.car.sold ? soldText(S.car) : null, carMeta(S.car) || null, S.car.vin_prefix ? `VIN ${S.car.vin_prefix}…` : null, `Times in ${tzName(tz())}`],
      actions: [h('a', { class: 'btn btn--sm', href: state.buildUrl('car', { car: cid }) }, icon('gauge', 'ico--sm'), 'Car overview')],
    });
    next.querySelector('.back')?.setAttribute('data-cover', 'web.settings.car.breadcrumb');
    next.querySelector('.back')?.setAttribute('aria-label', 'Back to Settings: Cars, every car');
    if (head?.isConnected) head.replaceWith(next);
    head = next;
    ctx.setCrumbs([{ label: 'Set-up' }, { label: 'Settings: Cars', href: state.buildUrl('settings-cars') }, { label: S.car.name }]);
    docTitle('Car settings', S.car.name);
  }

  /* ---- Car and switcher */
  function paintCar() {
    const c = car();
    const list = (S.cars?.items || []).map((x) => (x.id === cid ? { ...x, name: c.name } : x)); // a rename shows here at once
    const unsold = list.filter((x) => x.id === cid || !state.isSold(x)); // sold cars only through the switcher
    const shown = unsold.some((x) => x.id === cid) ? unsold : [c, ...unsold.slice(0, 7)];
    const picker = h('div', { class: 'sc-picker', cover: 'web.settings.car.car-switcher' },
      h('div', { class: 'seg sc-cars', role: 'group', 'aria-label': 'Whose settings' },
        shown.map((x) => h('a', { class: 'seg-btn', href: state.buildUrl('settings-cars', { car: x.id }), 'aria-pressed': String(x.id === cid), 'aria-current': x.id === cid ? 'page' : null }, h('span', { class: 'name-cut' }, x.name)))),
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
        [picker, h('p', { class: 'setting-note', cover: 'web.settings.car.empty-state' }, icon('info', 'ico--sm'), ' A new car appears here by itself once its CAN Bridge has uploaded its first log, or when a Tesla account\'s car is matched to a new car. ',
          h('a', { class: 'link', href: state.buildUrl('settings-cars') }, 'How a car appears'), '.')]),
      setting('Name', 'Sentry Mate never reads a name from Tesla: you name the car. Until then it shows its model and VIN prefix.', name.el),
      setting('Sold', 'A sold car keeps all its history, browsable as before. It leaves the fleet cards and car pickers (behind "Sold cars"), raises no attention items, and no input reads it any more.', soldControl(c)),
      setting('What the car reports', `Read from the car's build message on the vehicle bus (SMO), with model and year from the VIN prefix; a value only the Tesla API gave says so. ${missing.length ? `Not reported: ${missing.join(', ')}.` : ''}`,
        kv(rows))));
  }

  /* ---- Sold: the state, and for admins mark sold (optional date) or not sold */
  function soldControl(c) {
    const state_ = h('p', { class: ['sc-sold-state', c.sold && 'is-sold'] }, icon(c.sold ? 'flag' : 'car', 'ico--sm'), ' ', c.sold ? h('b', null, soldText(c)) : 'Not sold. The car shows everywhere.');
    if (ro) return state_;
    const today = new Date().toISOString().slice(0, 10);
    const day = field({ label: c.sold ? 'Sold date' : 'Sold date (optional)', name: 'sc-sold-date', type: 'date', path: 'sold_date', value: c.sold_date || '', hint: c.sold ? 'Change it, or clear it if not known.' : 'Leave empty if not known.', attrs: { max: today } });
    // the picker stops at today; a date typed past it is said under the field and not sent
    const late = () => {
      if (!day.input.value || day.input.value <= today) return false;
      day.setError('A day up to today: a car is not sold on a day still to come.');
      day.input.focus();
      return true;
    };
    if (c.sold) {
      day.input.addEventListener('change', () => {
        const v = day.input.value || null;
        if (late()) return;
        if (v !== (car().sold_date || null)) patchCar({ sold: true, sold_date: v }, 'Sold date', day);
      });
      return h('div', { class: 'sc-sold' }, state_, day.el,
        h('div', { class: 'row' }, h('button', { class: 'btn btn--sm', type: 'button', onclick: once(() => patchCar({ sold: false, sold_date: null }, 'Not sold')) }, icon('reset', 'ico--sm'), 'Mark as not sold')));
    }
    return h('div', { class: 'sc-sold' }, state_, day.el,
      h('div', { class: 'row' }, h('button', { class: 'btn btn--sm', type: 'button', onclick: once(async () => { if (!late()) await patchCar({ sold: true, sold_date: day.input.value || null }, 'Sold', day); }) }, icon('flag', 'ico--sm'), 'Mark as sold')));
  }

  /* ---- Uploads: outcome, tokens, keys, waiting keys */
  function outcome() {
    const hl = S.health, c = car();
    const last = hl?.newest_log_ms ?? hl?.last_upload_ms ?? c.now?.received_ms ?? null;
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
    if (pref) detail += `Preferred key: ${pref.key}${deviceName(pref).named ? ` (${deviceName(pref).text})` : ''}. `;
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
    await showSecret(t, `Upload token for ${car().name}`, 'The first log sent with it ties the CAN Bridge key to this car.');
    S.tokens = (await ctx.api.get(`cars/${enc(cid)}/upload-tokens`, { limit: 100 }).catch(() => null))?.items || [t, ...S.tokens];
    // the panel is drawn again with the new token: the button that made it keeps the focus the dialog gave back
    if (ctx.alive()) keepFocus(slots.uploads, paintUploads);
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
      side: h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: once(makeToken) }, icon('plus', 'ico--sm'), 'Make upload token'),
      sort: { key: 'created', dir: 'desc' },
      columns: [
        { key: 'id', label: 'Token', render: (t) => h('span', { class: 'mono' }, t.id), sort: true },
        { key: 'created', label: 'Made', value: (t) => t.created_ms, sort: true, render: (t) => dateTime(t.created_ms, z), csv: (t) => t.created_ms },
        { key: 'state', label: 'State', value: (t) => (t.revoked ? 2 : t.last_upload_ms ? 0 : 1), sort: true,
          render: tokenState, csv: tokenStateText },
        { key: 'key', label: 'Key it brought', render: (t) => (t.key ? [h('span', { class: 'mono' }, t.key), ' ', deviceText(t)] : h('span', { class: 'faint' }, t.revoked ? '—' : 'None yet')) },
        { key: 'last', label: 'Last upload', value: (t) => t.last_upload_ms, sort: true, render: (t) => (t.last_upload_ms ? agoEl(t.last_upload_ms, z) : 'Never'), csv: (t) => t.last_upload_ms ?? '' },
        { key: 'act', label: '', csv: false, render: (t) => (t.revoked ? null : h('button', { class: 'btn btn--sm btn--danger', type: 'button', onclick: once(() => revokeToken(t)) }, icon('x', 'ico--sm'), 'Revoke')) },
      ],
      empty: () => emptyState({ icon: 'lock', title: 'No upload tokens', text: 'Without a token no CAN Bridge can upload for this car. Make one and enter it in the CAN Bridge app.' }),
    });
    const keys = dataTable(ctx, {
      title: 'CAN Bridge keys', icon: 'chip', rows: S.keys, label: 'CAN Bridge keys tied to this car', csvName: 'can-keys',
      note: 'Each logger signs its logs with its own key: a Screenmate, a reinstall of the app or a Raspberry Pi each bring one. When two record the same time, the preferred key\'s log is used and the other is kept.',
      sort: { key: 'last', dir: 'desc' },
      columns: [
        { key: 'key', label: 'Key', render: (k) => h('span', { class: 'mono' }, k.key), sort: true },
        { key: 'device', label: 'Device', value: deviceWords, csv: deviceWords, sort: true,
          render: (k) => h('span', { class: 'row sc-dev' }, deviceText(k), renameKeyButton(ctx, k, admin, () => refresh())) },
        { key: 'first', label: 'First upload', value: (k) => k.first_seen_ms, sort: true, render: (k) => dateTime(k.first_seen_ms, z), csv: (k) => k.first_seen_ms },
        { key: 'last', label: 'Last upload', value: (k) => k.last_upload_ms, sort: true, render: (k) => (k.last_upload_ms ? agoEl(k.last_upload_ms, z) : 'Never'), csv: (k) => k.last_upload_ms ?? '' },
        { key: 'logs_waiting', label: 'Logs waiting', num: true, sort: true, render: (k) => format(k.logs_waiting ?? 0, 'count') },
        { key: 'preferred', label: 'Preferred', value: (k) => (k.preferred ? 0 : 1), sort: true, csv: (k) => (k.preferred ? 'yes' : 'no'),
          render: (k) => (k.preferred ? pill('Preferred', { tone: 'live', ic: 'check' })
            : h('button', { class: 'btn btn--sm', type: 'button', onclick: once(() => keyChange(k, { preferred: true }, `Key ${k.key} is now preferred for ${car().name}.`)) }, 'Prefer this key')) },
        { key: 'act', label: '', csv: false, render: (k) => h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: once(async () => {
          if (await confirmDialog(`Untie key ${k.key} from ${car().name}? Its new logs will wait under Keys waiting until it is tied to a car again. Logs already read stay.`, { title: 'Untie the key?', ok: 'Untie', danger: true })) await keyChange(k, { car: null }, `Key ${k.key} is no longer tied to ${car().name}.`);
        }) }, 'Untie') },
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
    const src = car().time_zone_source;
    const zones = ZONES.includes(z) || !z ? ZONES : [z, ...ZONES];
    const listId = `sc-zones-${cid}`;
    const tzField = field({ label: 'Time zone', name: 'sc-tz', path: 'time_zone', value: z, hint: 'A zone name such as America/Chicago or Europe/Amsterdam. Type to search the list.', attrs: { list: listId, disabled: ro, autocomplete: 'off', spellcheck: 'false' } });
    const dl = h('datalist', { id: listId }, zones.map((x) => h('option', { value: x })));
    tzField.input.addEventListener('change', () => {
      const v = tzField.input.value.trim();
      if (!v || (v === car().time_zone && src === 'owner')) return;
      if (!isValidZone(v)) { tzField.setError('Not a time zone this browser knows. Pick one from the list.'); return; }
      patchCar({ time_zone: v }, 'Time zone', tzField);
    });
    const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
    const now = ctx.api.now();
    replace(slots.tz, panel({ title: 'Time zone', icon: 'clock', cover: 'docs.env.core.tz' },
      setting('The car\'s time zone', 'Days, ISO weeks, months, statistics, charging heat maps and prices by time of day all use this zone, wherever the server runs. A change regroups the whole history at once; nothing is lost.',
        [tzField.el, dl,
          zoneSourceText(src) ? h('p', { class: 'setting-note sc-tz-src' }, h('b', null, z), ` (${zoneSourceText(src)})`,
            src === 'owner' ? '.' : '. Type or pick a zone to set it for this car.') : null,
          src === 'owner' && !ro ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => patchCar({ time_zone: null }, 'Time zone', tzField) }, icon('pin', 'ico--sm'), 'Work it out from the car’s position') : null,
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
    // the limits the server takes (metric), shown and checked in the person's unit
    const lim = (a, b, kind) => (kind ? [Math.ceil(convert(a, kind) * 10) / 10, Math.floor(convert(b, kind) * 10) / 10] : [a, b]);
    const DEF = { capacity_new_kwh: 'capacity_new_kwh_default', full_range_new_km: 'full_range_new_km_default', efficiency_wh_per_km_override: 'efficiency_wh_per_km_default' };
    const defs = [
      ['capacity_new_kwh', 'Capacity when new', 'Leave empty to take it from the pack\'s first readings.', null, 'kWh', [10, 250], '0.1'],
      ['full_range_new_km', 'Full range when new', 'Leave empty to take it from the first 30 days.', 'distance', unitLabel('distance'), [100, 1500], '0.1'],
      ['warranty_threshold_pct', 'Warranty threshold', 'The share of the capacity when new that the battery warranty promises.', null, '%', [50, 95], '1'],
      ['efficiency_wh_per_km_override', 'Consumption factor', 'Leave empty to work it out from energy and range.', 'efficiency', unitLabel('efficiency'), [60, 500], '0.1'],
      ['pack_new_kwh', 'New pack capacity when new', 'The usable capacity of the replacement pack when new.', null, 'kWh', [10, 250], '0.1'],
    ];
    const fields = defs.map(([k, label, hint, kind, u, [a, b], step]) => {
      const [lo, hi] = lim(a, b, kind);
      // what the car's build gives when nothing is typed (the empty input's placeholder)
      const d = s[DEF[k]], dv = d == null ? undefined : kind ? String(Math.round(convert(d, kind) * 10) / 10) : String(d);
      const range = `${num(lo)} to ${num(hi)} ${u}`;
      const required = k === 'warranty_threshold_pct';
      const f = field({ label: `${label} (${u})`, name: `sc-${k}`, path: k, type: 'number', value: kind ? disp(s[k], kind) : s[k] ?? '',
        hint: `${range}. ${hint}${dv ? ` The car's build gives ${dv} ${u}.` : ''}`,
        attrs: { min: String(lo), max: String(hi), step, required, disabled: ro, placeholder: dv } });
      const say = required ? `A number from ${range}.` : `A number from ${range}, or empty.`;
      f.input.addEventListener('change', () => {
        // (a number input gives "" for text it cannot read, "1e" or "-.": that is not "empty")
        if (f.input.validity.badInput) { f.setError(say); return; }
        const raw = f.input.value.trim();
        if (raw === '' && required) { f.setError(say); return; }
        const n = raw === '' ? null : Number(raw);
        if (n != null && (!Number.isFinite(n) || n < lo || n > hi)) { f.setError(say); return; }
        f.setError(null);
        const v = n == null ? null : kind ? Math.round(toMetric(n, kind) * 10) / 10 : n;
        // the server's limits are in km and Wh/km: a range error is said again in the field's own unit
        patchSettings({ [k]: v }, label, (errs) => { const t = errs[k] || Object.values(errs)[0]; f.setError(/between|at least|at most|range/i.test(t) ? say : t); });
      });
      return f;
    });
    const pk = S.battery?.pack;
    const detected = !s.pack_replaced_date && pk?.source === 'detected' && pk.replaced_ms != null ? dayKey(pk.replaced_ms, tz()) : null;
    const swap = field({ label: 'Pack replaced on', name: 'sc-pack-date', type: 'date', path: 'pack_replaced_date', value: s.pack_replaced_date || '',
      hint: `${detected ? `Detected from the pack's readings: ${detected}. ` : ''}The day a new pack went in: from then on the battery figures cover the new pack. Leave empty if it was never replaced.`, attrs: { disabled: ro, placeholder: detected || undefined } });
    swap.input.addEventListener('change', () => patchSettings({ pack_replaced_date: swap.input.value || null }, 'Pack replaced on', (errs) => swap.setError(errs.pack_replaced_date || Object.values(errs)[0])));
    const sw = field({ label: 'Software version, typed', name: 'sc-sw', path: 'software_version_typed', value: s.software_version_typed || '', hint: 'Only for a car whose bus does not send it, such as 2026.38.4. Leave empty to use what the car reports.', attrs: { disabled: ro, autocomplete: 'off' } });
    sw.input.addEventListener('change', () => patchSettings({ software_version_typed: sw.input.value.trim() || null }, 'Software version', (errs) => sw.setError(errs.software_version_typed || Object.values(errs)[0])));
    replace(slots.refs, panel({ title: 'Reference values', icon: 'gauge', side: h('span', { class: 'faint' }, 'Optional; empty means worked out') },
      setting('Battery and range', 'Battery health compares today\'s capacity and range with these. Without them Sentry Mate uses the earliest readings it has, and says so on the Battery page.', [fields[0].el, fields[1].el, fields[2].el, swap.el, fields[4].el]),
      setting('Consumption and software', 'The consumption factor turns range into energy on Efficiency. A typed software version is marked as typed on Software.', [fields[3].el, sw.el]),
      tyreSetting()));
  }

  /* ---- the recommended tyre pressures, typed (config.tyre_pressure_front_kpa / _rear_kpa, kPa) */
  function tyreSetting() {
    const cfg = car().config || {};
    const u = unitLabel('pressure');
    const r2 = (x) => Math.round(x * 100) / 100;
    const lo = Math.ceil(convert(100, 'pressure') * 100) / 100, hi = Math.floor(convert(600, 'pressure') * 100) / 100;
    const say = `A number from ${lo} to ${hi} ${u}, or empty.`;
    const one = (k, label, hint) => {
      const cur = cfg[k]?.value;
      const f = field({ label: `${label} (${u})`, name: `sc-${k}`, path: `config.${k}`, type: 'number', value: cur == null ? '' : String(r2(convert(cur, 'pressure'))),
        hint: `${lo} to ${hi} ${u}. ${hint}`, attrs: { min: String(lo), max: String(hi), step: u === 'psi' ? '1' : '0.05', disabled: ro } });
      f.input.addEventListener('change', () => {
        if (f.input.validity.badInput) { f.setError(say); return; }
        const raw = f.input.value.trim();
        const n = raw === '' ? null : Number(raw);
        if (n != null && (!Number.isFinite(n) || n < lo || n > hi)) { f.setError(say); return; }
        f.setError(null);
        patchCar({ config: { [k]: n == null ? null : Math.round(toMetric(n, 'pressure') * 10) / 10 } }, label, f);
      });
      return f;
    };
    return setting('Tyre pressure', 'The cold pressures on the door placard. A typed pressure beats the one the car sends and the placard\'s for its wheel size; a tyre more than 10 % under it is flagged on the car page.',
      [one('tyre_pressure_front_kpa', 'Recommended pressure, front', 'Leave empty to use what the car sends.').el,
        one('tyre_pressure_rear_kpa', 'Recommended pressure, rear', 'Leave empty to use the front\'s when that is typed.').el]);
  }

  /* ---- Free Supercharging as dated ranges */
  function paintCharging() {
    const c = car();
    const access = c.config?.supercharging_access;
    let ranges = (S.settings.free_supercharging || []).map((r) => ({ ...r }));
    const list = h('div', { class: 'sc-ranges' });
    const msg = h('p', { class: 'form-error', role: 'alert', hidden: true });

    const showErr = (i, which, text) => {
      const fe = list.querySelector(`[data-err="${i}-${which}"]`);
      if (fe) { fe.hidden = false; replace(fe, icon('warn'), text); }
    };
    const clearErrs = () => {
      msg.hidden = true;
      for (const fe of list.querySelectorAll('[data-err]')) fe.hidden = true;
    };
    // the server's words name its fields; the page says the same in its own words
    const ownWords = (which, text) => (/before/i.test(text) ? 'The last day is before the first day.'
      : which === 'from_date' ? 'Give the first free day as a date.' : 'Give the last free day as a date, or leave it empty for no end.');

    // what is wrong with the ranges before they are sent: a missing first day, a last day before the first, or
    // two ranges that share a day (a day can be free in one range only)
    function problems(rows) {
      const out = [];
      rows.forEach((r, i) => {
        if (!r.from_date && !r.to_date) return; // a new, empty row: nothing to say yet
        if (!r.from_date) out.push([i, 'from_date', 'Give the first free day.']);
        else if (r.to_date && r.to_date < r.from_date) out.push([i, 'to_date', 'The last day is before the first day.']);
      });
      if (out.length) return { fields: out, overlap: null };
      const order = rows.map((r, i) => ({ ...r, i })).filter((r) => r.from_date).sort((a, b) => (a.from_date < b.from_date ? -1 : a.from_date > b.from_date ? 1 : 0));
      for (let j = 1; j < order.length; j++) {
        const a = order[j - 1], b = order[j];
        if (!a.to_date || b.from_date <= a.to_date) {
          const [x, y] = [a.i, b.i].sort((p, q) => p - q);
          return { fields: [], overlap: `Ranges ${x + 1} and ${y + 1} share days: ${date(dayMs(b.from_date), 'UTC')} falls in both${a.to_date ? '' : ` (range ${a.i + 1} has no end)`}. A day can be in one range only, so this was not saved. Change or remove one of them.` };
        }
      }
      return null;
    }

    async function save(rows, what) {
      clearErrs();
      const bad = problems(rows);
      if (bad) {
        for (const [i, which, text] of bad.fields) showErr(i, which, text);
        if (bad.overlap) { msg.hidden = false; replace(msg, icon('warn'), bad.overlap); }
        return;
      }
      const clean = rows.filter((r) => r.from_date).map((r) => ({ from_date: r.from_date, to_date: r.to_date || null }));
      await patchSettings({ free_supercharging: clean }, what, (errs) => {
        paintRows(rows);
        for (const [path, text] of Object.entries(errs)) {
          const m = /^free_supercharging\.(\d+)\.(from_date|to_date)$/.exec(path);
          if (m && list.querySelector(`[data-err="${m[1]}-${m[2]}"]`)) showErr(m[1], m[2], ownWords(m[2], String(text)));
          else { msg.hidden = false; replace(msg, icon('warn'), /from_date|to_date/.test(String(text)) ? 'A range could not be saved: check that each has a first day, and that its last day is not before it.' : String(text)); }
        }
        return KEEP;
      });
    }

    // the line beside a range: how long it is, or what is wrong with it
    const sayRange = (r) => (!r.from_date ? 'New range'
      : r.to_date && r.to_date < r.from_date ? 'Last day is before the first day'
        : r.to_date ? `Free for ${days(r.from_date, r.to_date)} day${days(r.from_date, r.to_date) === 1 ? '' : 's'}` : 'Free from then on, no end');

    function paintRows(rows) {
      ranges = rows;
      replace(list, rows.length ? rows.map((r, i) => {
        const from = dis(h('input', { class: 'input', type: 'date', value: r.from_date || '', 'aria-label': `Range ${i + 1}: first day`, required: true }));
        const to = dis(h('input', { class: 'input', type: 'date', value: r.to_date || '', 'aria-label': `Range ${i + 1}: last day (empty: still free)` }));
        const say = h('span', { class: 'sc-range-say faint' });
        const showSay = (x) => { say.textContent = sayRange(x); say.classList.toggle('is-bad', !!(x.from_date && x.to_date && x.to_date < x.from_date)); };
        showSay(r);
        const onChange = () => {
          // the list follows every edit at once, so a second quick change builds on the first
          const row = { from_date: from.value || null, to_date: to.value || null };
          ranges = ranges.map((x, j) => (j === i ? row : x));
          showSay(row);
          save(ranges, 'Free Supercharging');
        };
        from.addEventListener('change', onChange); to.addEventListener('change', onChange);
        return h('div', { class: 'sc-range' },
          h('label', { class: 'sc-range-f' }, h('span', { class: 'form-label' }, 'From'), from, h('span', { class: 'form-error', dataset: { err: `${i}-from_date` }, hidden: true })),
          h('label', { class: 'sc-range-f' }, h('span', { class: 'form-label' }, 'To (included)'), to, h('span', { class: 'form-error', dataset: { err: `${i}-to_date` }, hidden: true })),
          say,
          ro ? null : h('button', { class: 'btn btn--sm btn--ghost', type: 'button', 'aria-label': `Remove range ${i + 1}`, onclick: () => {
            const next = ranges.filter((_, j) => j !== i);
            const had = ranges[i]?.from_date;
            paintRows(next);
            if (had) save(next, 'Free Supercharging');
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
        [hint, list, msg, ro ? null : h('button', { class: 'btn btn--sm', type: 'button', onclick: () => paintRows([...ranges, { from_date: null, to_date: null }]) }, icon('plus', 'ico--sm'), 'Add a free range')]),
      priceSetting()));
  }

  /* ---- the default price per kWh ({ value, currency } or null): Statistics estimates unpriced charges at it */
  function priceSetting() {
    const p = S.settings.default_price_per_kwh;
    const what = 'Default price per kWh';
    const val = field({ label: 'Price per kWh', name: 'sc-dp-value', path: 'default_price_per_kwh.value', type: 'number', value: p ? String(p.value) : '',
      attrs: { min: '0', max: '10', step: 'any', inputmode: 'decimal', placeholder: '0.15', disabled: ro } });
    const cur = field({ label: 'Currency', name: 'sc-dp-currency', path: 'default_price_per_kwh.currency', value: p?.currency || S.currency || '',
      attrs: { maxlength: '3', autocomplete: 'off', spellcheck: 'false', placeholder: 'USD', disabled: ro } });
    const onErrs = (errs) => {
      val.setError(errs['default_price_per_kwh.value'] || null);
      cur.setError(errs['default_price_per_kwh.currency'] || errs.default_price_per_kwh || null);
    };
    const save = () => {
      const raw = val.input.value.trim(), n = Number(raw), c = cur.input.value.trim().toUpperCase();
      const bad = {};
      if (raw === '' || !Number.isFinite(n) || n < 0 || n > 10) bad['default_price_per_kwh.value'] = 'A price from 0 to 10.';
      if (!/^[A-Z]{3}$/.test(c)) bad['default_price_per_kwh.currency'] = 'Three letters, such as USD or EUR.';
      else if (KNOWN_CURRENCIES && !KNOWN_CURRENCIES.has(c)) bad['default_price_per_kwh.currency'] = `${c} is not a currency. Its three letters, such as USD or EUR.`;
      onErrs(bad);
      if (!Object.keys(bad).length) patchSettings({ default_price_per_kwh: { value: n, currency: c } }, what, onErrs);
    };
    for (const f of [val, cur]) f.input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); save(); } });
    return setting(what, 'Charges without a price are estimated at it on Statistics, shown apart as estimated.',
      [h('div', { class: 'sc-price' }, val.el, cur.el),
        ro ? null : h('div', { class: 'sc-price-acts' },
          h('button', { class: 'btn btn--sm', type: 'button', onclick: save }, 'Save'),
          p ? h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => patchSettings({ default_price_per_kwh: null }, what, onErrs) }, 'Clear') : null)]);
  }

  /* ---- Data sources: the live inputs in their priority order, gap filling from an import, and the overlaps */
  let focusAfter = null; // after a move by button, the same button keeps the focus once the list is drawn again
  let pendingOrder = null; // the order on its way to the server, shown until the answer is in
  const said = h('p', { class: 'sr-only', 'aria-live': 'polite' });
  function paintSources() {
    const s = S.settings;
    const def = Array.isArray(s.source_order_default) && s.source_order_default.length ? s.source_order_default : SOURCE_ORDER;
    const saved = Array.isArray(s.source_order) && s.source_order.length ? s.source_order : def;
    const order = pendingOrder || saved;
    const byKind = Object.fromEntries((s.inputs || []).map((i) => [i.kind, i]));
    const has = (k) => (car().sources || []).some((x) => x.kind === k);
    const info = (k) => byKind[k] || { kind: k, label: INPUT_LABEL[k] || k, on: has(k), last_ms: null };
    const label = (k) => info(k).label || INPUT_LABEL[k] || k;
    async function saveOrder(next, what) {
      if (next.every((k, i) => k === order[i])) return;
      pendingOrder = next;
      paintSources();
      let bad = null;
      await patchSettings({ source_order: next.every((k, i) => k === def[i]) ? null : next }, what,
        (errs) => { bad = String(errs.source_order || Object.values(errs)[0]); });
      pendingOrder = null;
      paintSources();
      focusAfter = null;
      const box = slots.sources.querySelector('.sc-order-msg');
      if (bad && box) { box.hidden = false; replace(box, icon('warn'), ' ', bad); }
    }
    const move = (k, dir) => {
      const i = order.indexOf(k), j = i + dir;
      if (i < 0 || j < 0 || j >= order.length) return;
      const next = [...order];
      [next[i], next[j]] = [next[j], next[i]];
      focusAfter = { kind: k, dir };
      said.textContent = `${label(k)} is now ${j + 1} of ${order.length}.`;
      saveOrder(next, 'Source order');
    };

    // drag by the grip (mouse, pen or touch): the row follows the pointer and the others make room as it passes them
    function drag(e, li) {
      if (ro || (e.pointerType === 'mouse' && e.button !== 0)) return;
      e.preventDefault();
      const list = li.parentElement, grip = e.currentTarget;
      let startY = e.clientY;
      grip.setPointerCapture?.(e.pointerId);
      li.classList.add('is-dragging');
      const onMove = (ev) => {
        const dy = ev.clientY - startY;
        li.style.setProperty('--dy', `${dy}px`);
        const top = li.offsetTop, mid = top + dy + li.offsetHeight / 2;
        const prev = li.previousElementSibling, next = li.nextElementSibling;
        let moved = false;
        if (prev && mid < prev.offsetTop + prev.offsetHeight / 2) { li.after(prev); moved = true; } // the dragged row itself stays put in the DOM, so it keeps the pointer
        else if (next && mid > next.offsetTop + next.offsetHeight / 2) { list.insertBefore(next, li); moved = true; }
        if (moved) { startY += li.offsetTop - top; li.style.setProperty('--dy', `${ev.clientY - startY}px`); }
      };
      const end = (ev) => {
        window.removeEventListener('pointermove', onMove);
        window.removeEventListener('pointerup', end);
        window.removeEventListener('pointercancel', end);
        li.classList.remove('is-dragging');
        li.style.removeProperty('--dy');
        const next = [...list.children].map((x) => x.dataset.kind);
        if (ev.type === 'pointercancel' || next.every((k, i) => k === order[i])) { paintSources(); return; }
        said.textContent = `${label(li.dataset.kind)} is now ${next.indexOf(li.dataset.kind) + 1} of ${next.length}.`;
        saveOrder(next, 'Source order');
      };
      window.addEventListener('pointermove', onMove);
      window.addEventListener('pointerup', end);
      window.addEventListener('pointercancel', end);
    }

    const rows = order.map((k, i) => {
      const x = info(k);
      const li = h('li', { class: ['sc-src', !x.on && 'is-off', ro && 'is-ro'], dataset: { kind: k } },
        h('span', { class: 'sc-src-rank', 'aria-hidden': 'true' }, String(i + 1)),
        ro ? null : h('span', { class: 'sc-grip', title: `Drag to move ${label(k)}`, 'aria-hidden': 'true', onpointerdown: (e) => drag(e, li) }, icon('chev-ud', 'ico--sm')),
        h('div', { class: 'sc-src-main' },
          h('span', { class: 'sc-src-name' }, h('i', { class: `src-m src-m--${k}`, 'aria-hidden': 'true' }), h('b', null, label(k))),
          x.on ? pill('On', { tone: 'live' }) : pill('Off', { tone: 'plain' }),
          h('span', { class: 'sc-src-last faint' }, x.last_ms != null ? ['Last data ', agoEl(x.last_ms, tz())] : x.on ? 'No data yet' : 'Not feeding this car')),
        ro ? null : h('div', { class: 'sc-src-acts' },
          h('button', { class: 'btn btn--sm btn--icon sc-up', type: 'button', 'aria-label': `Move ${label(k)} up`, title: `Move ${label(k)} up`, disabled: i === 0, dataset: { move: `${k}:-1` }, onclick: () => move(k, -1) }, icon('chev-d', 'ico--sm')),
          h('button', { class: 'btn btn--sm btn--icon', type: 'button', 'aria-label': `Move ${label(k)} down`, title: `Move ${label(k)} down`, disabled: i === order.length - 1, dataset: { move: `${k}:1` }, onclick: () => move(k, 1) }, icon('chev-d', 'ico--sm'))));
      return li;
    });
    const isDefault = order.every((k, i) => k === def[i]);

    // gap filling: the car's own choice (true or false), or null to follow the server's setting
    const g = s.import_gap_fill ?? null;
    const serverOn = s.import_gap_fill_default ?? null;
    const on = s.import_gap_fill_on ?? (g ?? serverOn ?? false);
    const imports = Array.isArray(s.imports) ? s.imports : null;
    const none = imports != null && !imports.length;
    const onOff = (v) => (v == null ? 'not known' : v ? 'on' : 'off');
    const gapInput = h('input', { type: 'checkbox', id: `sc-gapfill-${cid}`, role: 'switch', checked: !none && !!on, disabled: ro || none,
      onchange: async () => {
        const want = gapInput.checked;
        if (!(await patchSettings({ import_gap_fill: want }, 'Gap filling'))) gapInput.checked = !!on;
      } });
    const used = (imports || []).map((m) => h('li', null, h('i', { class: 'src-m src-m--import', 'aria-hidden': 'true' }), h('b', null, m.label || m.site),
      m.from_ms != null ? h('span', { class: 'faint' }, ` ${date(m.from_ms, tz())} to ${m.to_ms != null ? date(m.to_ms, tz()) : 'now'}`) : null));
    const overlapHref = admin ? `${state.buildUrl('import')}#overlaps` : state.buildUrl('timeline', { car: cid });

    replace(slots.sources, panel({ title: 'Data sources', icon: 'signals', id: 'data-sources', side: h('span', { class: 'faint' }, ro ? 'Changed by an admin' : 'Drag or use the arrows') },
      setting('Priority of the live inputs', 'When two sources cover the same time, the higher one wins, value by value. Imported history always comes after the live inputs.',
        [h('ol', { class: 'sc-order', 'aria-label': `Live inputs of ${car().name}, highest priority first` }, rows), said,
          h('p', { class: 'form-error sc-order-msg', role: 'alert', hidden: true }),
          h('div', { class: 'row sc-order-foot' },
            h('span', { class: 'setting-note' }, isDefault ? 'This is the default order.' : `Default: ${def.map(label).join(', ')}.`),
            !ro && !isDefault ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => { said.textContent = 'Back to the default order.'; saveOrder([...def], 'Default order'); } }, icon('reset', 'ico--sm'), 'Reset to default order') : null)]),
      setting('Gap filling', 'Where no live input has a value at a time, the imported history shown for that span fills it in, such as a position, a charge level or a temperature. Distance, energy and costs are never filled.',
        [h('div', { class: 'switch-row switch-row--inline sc-gap' }, h('span', { class: 'switch' }, gapInput, h('i', { 'aria-hidden': 'true' })),
          h('label', { for: `sc-gapfill-${cid}` }, 'Fill gaps from imported history')),
        none ? h('p', { class: 'setting-note' }, icon('info', 'ico--sm'), ' No imported history: there is nothing to fill gaps from.', admin ? [' ', h('a', { class: 'link', href: state.buildUrl('import') }, 'Import a backup'), '.'] : null)
          : used.length ? h('ul', { class: 'sc-imports', 'aria-label': 'Imported history it uses' }, used) : null,
        h('p', { class: 'setting-note' }, g == null ? ['Follows the server: ', h('b', null, `Server default: ${onOff(serverOn)}`), '.'] : [`Set for this car (${onOff(g)}). `, h('b', null, `Server default: ${onOff(serverOn)}`), '.']),
        g != null && !ro ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => patchSettings({ import_gap_fill: null }, 'Gap filling') }, icon('reset', 'ico--sm'), `Follow the server default (${onOff(serverOn)})`) : null]),
      setting('Overlapping spans', 'Where an import and a live input both recorded the same time, one side is shown for that span and the other is kept.',
        panelLink(overlapHref, admin ? 'Choose per span which source is shown' : 'See per span which source is shown, on the Timeline'))));

    if (focusAfter) {
      const { kind, dir } = focusAfter;
      const b = slots.sources.querySelector(`[data-move="${kind}:${dir}"]`);
      (b && !b.disabled ? b : slots.sources.querySelector(`[data-move="${kind}:${-dir}"]`))?.focus();
    }
  }

  /* ---- the page */
  paintAll();
  replace(ctx.view, head,
    h('div', { class: 'settings-stack' },
      h('p', { class: 'note saving-note', cover: 'web.settings.car.autosave' }, icon('check', 'ico--sm'),
        ro ? ' These settings are changed by an admin; you can read them here.' : ' Every change here is saved at once; there is no Save button. A short note confirms it.'),
      slots.car, slots.uploads, slots.sources, h('div', { class: 'sc-two' }, slots.tz, slots.battery), slots.charging, slots.refs));
  ctx.events(['fleet'], (ev) => { if (ev.data?.car === cid && ev.data?.change === 'renamed' && ev.data.name !== S.car.name && !inFlight.car) refresh(); });
  ctx.ready();
}

// a calendar day (YYYY-MM-DD) as the ms of its midnight in UTC, to write it with the date formatter in UTC
// the currency a car's charges cost most in (the Charges list's totals), or null
function topCurrency(list) {
  const by = list?.totals?.cost_by_currency || [];
  return by.length ? by.reduce((a, b) => (b.amount > a.amount ? b : a)).currency : null;
}

function dayMs(d) {
  const [y, m, dd] = d.split('-').map(Number);
  return Date.UTC(y, m - 1, dd);
}

// whole days from a to b, both included (dates as YYYY-MM-DD; calendar days, no time zone involved)
function days(a, b) {
  const [y1, m1, d1] = a.split('-').map(Number), [y2, m2, d2] = b.split('-').map(Number);
  return Math.round((Date.UTC(y2, m2 - 1, d2) - Date.UTC(y1, m1 - 1, d1)) / 86400000) + 1;
}
