// Car settings (/settings/cars and /settings/cars/{car}). Without a car: the garage list of every car the account
// sees (searched and paged by the server), upload tokens for a car that is not here yet, CAN Bridge keys waiting
// for a car, and how a car appears. With a car: a row of car presets to switch, its name and what it reports
// about itself (each fact with its source), the uploads as a row of warning lights with its tokens and keys, its
// time zone on a clock face, the battery chemistry, free Supercharging as dated ranges, and reference values.
// Every change saves at once (PATCH with the object's version); the REC lamp and a toast confirm it.

import * as state from '../shared/state.js';
import { h, replace, setTitle as docTitle } from '../shared/dom.js';
import { invalidate } from '../shared/api.js';
import { isValidZone } from '../shared/time.js';
import { convert, toMetric, unitLabel, format } from '../shared/units.js';
import { icon, hud, plate, btn, linkBtn, tag, kv, note, callout } from '../core/ui.js';
import { search, plural } from '../core/kit.js';
import { tracks } from '../core/tracks.js';
import { field } from '../core/form.js';
import { emptyState } from '../core/states.js';
import { openDialog, confirmDialog, toast } from '../core/layer.js';
import { openCarPicker } from '../core/pickers.js';
import { dateTime, date, time, ago, agoEl, zone, carLine, stateWord, DASH } from '../core/fmt.js';
import { chan, hint, recLamp, errText, code, lcd, sig, copyBtn, presets } from '../core/setting.js';

export const id = 'settings-cars';
export const css = true;
export const title = (ctx) => (ctx.car ? 'Car settings' : 'Car settings: every car');

const enc = encodeURIComponent;
const SOURCE = { can: ['From CAN', 'cyan'], vin: ['From VIN', null], owner: ['Typed', 'org'], teslamate: ['Imported', 'org'] };
const CHEM = [['auto', 'Automatic'], ['lfp', 'LFP'], ['nca_nmc', 'NCA / NMC']];
const CHEM_WORD = { auto: 'Automatic', lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA / NMC (nickel)' };
const FACTS = [
  ['trim', 'Trim'], ['drivetrain', 'Drivetrain'], ['performance_package', 'Performance package'], ['pack_kwh_code', 'Pack code'],
  ['wheel', 'Wheels'], ['paint', 'Paint'], ['roof', 'Roof'], ['spoiler', 'Spoiler'], ['right_hand_drive', 'Right-hand drive'],
  ['map_region', 'Map region'], ['supercharging_access', 'Supercharging access'],
];
const yesNo = (v) => (v === true ? 'Yes' : v === false ? 'No' : String(v));
const srcTag = (s) => (SOURCE[s] ? tag(SOURCE[s][0], SOURCE[s][1]) : null);
const one = (v) => String(Math.round(v * 10) / 10);

/** How a car appears, as four lit steps. */
function howCarsAppear(admin) {
  return h('ol', { class: 'sc-how' },
    h('li', null, h('span', { class: 'sc-how-n' }, '1'), h('div', null, h('b', null, 'Install the CAN Bridge app'), ' on the Screenmate One (or another logger, such as a Raspberry Pi) in the car.')),
    h('li', null, h('span', { class: 'sc-how-n' }, '2'), h('div', null, admin ? [h('b', null, 'Make an upload token'), ' under Upload tokens for a new car and type it into the app. It is shown once.'] : [h('b', null, 'Enter an upload token'), ' in the app. An admin makes one on this page.'])),
    h('li', null, h('span', { class: 'sc-how-n' }, '3'), h('div', null, h('b', null, 'Wait for the first log.'), ' The car shows up here by itself, with its model and year from the VIN prefix. Name it and set its time zone.')),
    h('li', null, h('span', { class: 'sc-how-n' }, '4'), h('div', null, h('b', null, 'A second logger'), ' in the same car brings its own key. It waits under Keys waiting until it is tied to the car.')));
}

/** The token, shown once: the three fields of the CAN Bridge app, each with Copy. */
function showSecret(t, heading, after) {
  const app = t.app || { address: null, token: t.secret, certificate_sha256: null };
  const field3 = (label, value, why, once = false) => h('div', { class: 'sc-secret' },
    h('div', { class: 'card-head' }, h('p', { class: 'card-title' }, label), once ? tag('Shown once', 'org') : null),
    value ? [lcd(value), copyBtn(value, `Copy ${label.toLowerCase()}`)] : h('p', { class: 'faint small' }, 'Not known to this server: an admin sees it under Server and access.'),
    h('p', { class: 'chan-hint' }, why));
  return new Promise((resolve) => {
    const d = openDialog({
      title: heading, cls: 'dlg--wide',
      body: [
        h('p', null, 'In the CAN Bridge app, open ', h('b', null, 'Second Sentry Six Manager'), ' and fill in these three fields. ', h('b', null, 'The token is shown only now:'), ' the server keeps only a check value. If it is lost, make a new one and revoke this one.'),
        field3('Server address', app.address, 'Where the app sends its logs: this server\'s upload port, over HTTPS.'),
        field3('Token', app.token || t.secret, 'Lets the app upload. Keep it like a password.', true),
        field3('Certificate SHA-256', app.certificate_sha256, 'The app trusts this server by this fingerprint. It stays the same across restarts, updates and moves.'),
        note(after),
      ],
      actions: [btn('I have copied them', () => d.close(), { kind: 'go', icon: 'check' })],
      onClose: () => resolve(),
    });
  });
}

const tokenSig = (t) => (t.revoked ? sig('off', 'Revoked') : t.last_upload_ms ? sig('ok', 'In use') : sig('warn', 'Not used yet'));
const tokenWord = (t) => (t.revoked ? 'revoked' : t.last_upload_ms ? 'in use' : 'not used yet');

export async function mount(ctx) {
  return ctx.car ? mountCar(ctx) : mountGarage(ctx);
}

/* ====================================================================== every car */

async function mountGarage(ctx) {
  const admin = ctx.admin;
  let setup, waiting, fresh;
  try {
    [setup, waiting, fresh] = await Promise.all([
      ctx.api.get('setup/status').catch(() => null),
      admin ? ctx.api.get('can-keys', { assigned: 'false', limit: 100 }) : null,
      admin ? ctx.api.get('upload-tokens', { limit: 100 }) : null,
    ]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const noCars = setup?.cars === 0;
  const rec = recLamp({ cover: 'web.settings.car.autosave' });

  let findText = '';
  const list = noCars ? null : tracks(ctx, {
    title: 'Every car', icon: 'car', path: 'cars', params: { sort: 'custom' }, label: 'Cars and their settings', csvName: 'cars', csv: { all: true },
    note: 'In your own car order. Open a car to change its settings.',
    side: search('', (v) => { findText = v; list.reload({ sort: 'custom', search: v || null }); }, { placeholder: 'Find a car', label: 'Find a car by name, model or VIN start', ctx }),
    rowHref: (c) => state.buildUrl('settings-cars', { car: c.id }),
    columns: [
      { key: 'name', label: 'Car', lead: true, render: (c) => h('a', { class: 'cell-main', href: state.buildUrl('settings-cars', { car: c.id }) }, c.name), csv: (c) => c.name },
      { key: 'model', label: 'Model and year', render: (c) => carLine(c) || DASH, csv: (c) => carLine(c) },
      { key: 'vin', label: 'VIN start', render: (c) => (c.vin_prefix ? code(c.vin_prefix) : DASH), csv: (c) => c.vin_prefix || '' },
      { key: 'tz', label: 'Time zone', render: (c) => c.time_zone || DASH, csv: (c) => c.time_zone || '' },
      { key: 'now', label: 'Now', render: (c) => sig(c.now?.state === 'unknown' || !c.now?.state ? 'off' : c.now?.stale ? 'warn' : 'ok', c.now?.stale && c.now?.state && c.now.state !== 'unknown' ? `Last known: ${stateWord(c.now.state).toLowerCase()}` : stateWord(c.now?.state)), csv: (c) => c.now?.state || 'unknown' },
      { key: 'last', label: 'Last upload', num: true, render: (c) => (c.now?.received_ms ? agoEl(c.now.received_ms) : h('span', { class: 'faint' }, 'Never')), csv: (c) => (c.now?.received_ms ? new Date(c.now.received_ms).toISOString() : '') },
    ],
    empty: () => emptyState({ icon: 'search', title: findText ? 'No car matches' : 'No car yet', text: findText ? 'Search looks at the name, the model and the VIN start. Clear it to see every car.' : 'A car appears when its CAN Bridge uploads its first log.' }),
  });

  /* tokens for a car that is not here yet */
  let tokens = fresh?.items || [];
  const tokenList = admin ? tracks(ctx, {
    title: 'Upload tokens for a new car', icon: 'key', tone: 'orange', rows: tokens, label: 'Upload tokens for a car that has not uploaded yet', csvName: 'new-car-tokens', cover: 'web.settings.car.empty-state',
    note: noCars ? 'Step 2: make a token here and type it into the CAN Bridge app. The car appears with its first log.'
      : 'For a car that is not here yet: its first log makes the car. For a car already here, make the token on its own page instead.',
    side: btn('Make upload token', async () => {
      let t;
      try { t = await ctx.api.post('upload-tokens', {}); } catch (e) { toast(`Could not make a token: ${errText(e)}`, { kind: 'error' }); return; }
      await showSecret(t, 'Upload token for a new car', 'The first log sent with it makes the car here, named after its model until you name it. The token then moves to that car\'s own page.');
      tokens = (await ctx.api.get('upload-tokens', { limit: 100 }).catch(() => null))?.items || [t, ...tokens];
      if (ctx.alive()) tokenList.setRows(tokens);
    }, { kind: 'go', icon: 'plus', sm: true }),
    sort: { key: 'made', dir: 'desc' },
    columns: [
      { key: 'id', label: 'Token', lead: true, render: (x) => code(x.id), value: (x) => x.id, sort: true },
      { key: 'made', label: 'Made', value: (x) => x.created_ms, sort: true, render: (x) => dateTime(x.created_ms, ctx.tz), csv: (x) => new Date(x.created_ms).toISOString() },
      { key: 'state', label: 'State', value: (x) => (x.revoked ? 1 : 0), sort: true, render: tokenSig, csv: tokenWord },
      { key: 'act', label: '', cls: 'is-act', csv: false, render: (x) => (x.revoked ? null : btn('Revoke', async () => {
        if (!(await confirmDialog(`Revoke upload token ${x.id}? A logger that has it can no longer bring in a car with it.`, { title: 'Revoke the token?', ok: 'Revoke', danger: true }))) return;
        try {
          const r = await ctx.api.del(`upload-tokens/${enc(x.id)}`);
          tokens = tokens.map((y) => (y.id === x.id ? { ...y, ...r, revoked: true } : y));
          tokenList.setRows(tokens);
          rec.light('Token revoked');
          toast(`Token ${x.id} revoked.`);
        } catch (e) { toast(`Could not revoke it: ${errText(e)}`, { kind: 'error' }); }
      }, { kind: 'danger', icon: 'x', sm: true })) },
    ],
    empty: () => emptyState({ icon: 'key', title: 'No token waits for a new car', text: noCars ? 'Press Make upload token to bring the first car in.' : 'Press Make upload token when a new car joins.' }),
  }) : null;

  const waitList = admin ? waitingKeys(ctx, waiting?.items || [], null, () => ctx.reload(), noCars) : null;

  const empty = noCars ? hud({ title: 'No cars yet', icon: 'car', tone: 'orange', cover: 'web.settings.car.empty-state' },
    emptyState({ icon: 'car', title: 'The garage is empty', text: 'Sentry Mate needs no Tesla account: a car appears here when its CAN Bridge uploads its first log.' }),
    howCarsAppear(admin)) : null;

  replace(ctx.view, h('div', { class: 'page sc' },
    plate({ kicker: 'Set-up · cars', title: 'Car settings', aim: ctx.info.aim, tools: rec.el }),
    h('section', { class: 'hud hud--plain jump', 'aria-label': 'Saving' },
      h('p', { class: 'note jump-note', cover: 'web.settings.car.autosave' }, icon('check', 'i--sm'),
        h('span', null, admin ? 'Every change on these pages saves at once; there is no Save button. The REC lamp lights and a short note confirms it.' : 'Car settings are changed by an admin; you can read them here.'))),
    empty, noCars ? tokenList?.el : null,
    list?.el, noCars ? null : tokenList?.el, waitList,
    noCars ? null : hud({ title: 'How a car appears', icon: 'info' }, howCarsAppear(admin))));
  if (list) await list.ready;
  if (ctx.alive()) ctx.ready();
}

/* ---------------------------------------------------------------------- keys waiting (both routes) */

function waitingKeys(ctx, rows, car, after, noCars = false) {
  const assign = async (k, preferred) => {
    try {
      await ctx.api.patch(`can-keys/${enc(k.key)}`, preferred ? { car: car.id, preferred: true } : { car: car.id });
      toast(`Key ${k.key} is tied to ${car.name}${preferred ? ' and preferred' : ''}. Its waiting logs are being read.`);
      after();
    } catch (e) { toast(`Could not tie the key: ${errText(e)}`, { kind: 'error' }); }
  };
  return tracks(ctx, {
    title: 'Keys waiting for a car', icon: 'key', rows, label: 'CAN Bridge keys waiting to be tied to a car', csvName: 'keys-waiting',
    note: car ? `Each CAN Bridge signs its logs with its own key. These uploaded logs but are tied to no car; their logs wait, unread. Tie one to ${car.name} only if it records this car.`
      : noCars ? 'These keys uploaded logs but are tied to no car; their logs wait, unread. Once the first car appears, open it and tie the key there.'
        : 'These keys uploaded logs but are tied to no car; their logs wait, unread. Open the car a key records to tie it.',
    sort: { key: 'first', dir: 'desc' },
    columns: [
      { key: 'key', label: 'Key', lead: true, render: (k) => code(k.key), value: (k) => k.key, sort: true },
      { key: 'device', label: 'Device', render: (k) => k.device || h('span', { class: 'faint' }, 'Not reported'), sort: true },
      { key: 'first', label: 'First upload', value: (k) => k.first_seen_ms, sort: true, render: (k) => dateTime(k.first_seen_ms, ctx.tz), csv: (k) => new Date(k.first_seen_ms).toISOString() },
      { key: 'last', label: 'Last upload', value: (k) => k.last_upload_ms, sort: true, render: (k) => (k.last_upload_ms ? agoEl(k.last_upload_ms) : 'Never') },
      { key: 'waiting', label: 'Logs waiting', num: true, value: (k) => k.logs_waiting ?? 0, sort: true, render: (k) => format(k.logs_waiting ?? 0, 'count') },
      { key: 'act', label: car ? 'Tie it' : 'What to do', cls: 'is-act', csv: false, render: (k) => (car
        ? h('div', { class: 'row' }, btn(`Tie to ${car.name}`, () => assign(k, false), { kind: 'go', icon: 'plus', sm: true }), btn('Tie and prefer', () => assign(k, true), { sm: true }))
        : h('span', { class: 'faint small' }, noCars ? 'No car to tie it to yet' : 'Open its car, then tie it')) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No key waits', text: 'Every CAN Bridge that uploaded is tied to a car.' }),
  }).el;
}

/* ====================================================================== one car */

async function mountCar(ctx) {
  const admin = ctx.admin;
  const ro = !admin;
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
  try { await load(); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const rec = recLamp({ cover: 'web.settings.car.autosave' });
  const tz = () => S.car.time_zone || ctx.tz;
  const off = (el) => { if (ro) el.setAttribute('disabled', ''); return el; };

  /* saves of one object go one after the other, so each sends the version the one before got back */
  const queue = { car: Promise.resolve(), settings: Promise.resolve() };
  const busy = { car: 0, settings: 0 };
  const serial = (kind, fn) => { busy[kind]++; const run = queue[kind].then(fn).finally(() => { busy[kind]--; }); queue[kind] = run.catch(() => {}); return run; };
  const settle = (kind) => { if (busy[kind] <= 1) paintAll(); };
  async function refresh() {
    try { await load(); } catch (e) { toast(`Could not load the car again: ${errText(e)}`, { kind: 'error' }); return; }
    if (ctx.alive()) paintAll();
  }
  const patchCar = (body, what, f = null) => serial('car', async () => {
    try {
      S.car = await ctx.api.patch(`cars/${enc(cid)}`, { version: S.car.version, ...body });
      invalidate('cars');
      if (ctx.car) Object.assign(ctx.car, { name: S.car.name, time_zone: S.car.time_zone });
      ctx.carChanged(S.car);
      f?.setError(null);
      rec.saved(what);
      settle('car');
    } catch (e) {
      if (e.status === 409) { await refresh(); toast('Someone else changed this car; the page shows their change now. Try again.', { kind: 'info' }); return; }
      if (f && e.fields) f.setError(Object.values(e.fields).join(' ')); else rec.failed(what, e);
    }
  });
  const patchSettings = (body, what, onFields = null) => serial('settings', async () => {
    try {
      S.settings = await ctx.api.patch(`cars/${enc(cid)}/settings`, { version: S.settings.version, ...body });
      rec.saved(what);
      settle('settings');
      return true;
    } catch (e) {
      if (e.status === 409) { await refresh(); toast('Someone else changed these settings; the page shows theirs now. Try again.', { kind: 'info' }); return false; }
      if (onFields && e.fields) onFields(e.fields); else rec.failed(what, e);
      return false;
    }
  });

  const slots = { head: h('div'), car: h('div'), uploads: h('div', { class: 'stack' }), tz: h('div'), battery: h('div'), charging: h('div'), refs: h('div') };
  function paintAll() { if (!ctx.alive()) return; paintHead(); paintCar(); paintUploads(); paintTz(); paintBattery(); paintCharging(); paintRefs(); }

  /* ---- the plate, with the trail back to every car */
  function paintHead() {
    const c = S.car;
    const trail = h('nav', { class: 'sc-trail', 'aria-label': 'Where this page is', cover: 'web.settings.car.breadcrumb' },
      h('span', null, 'Set-up'), icon('chev-r', 'i--sm'),
      h('a', { href: state.buildUrl('settings-cars'), 'aria-label': 'Car settings, every car' }, 'Car settings'), icon('chev-r', 'i--sm'),
      h('span', { 'aria-current': 'page' }, c.name));
    replace(slots.head, plate({ kicker: trail, title: c.name, aim: [carLine(c), c.vin_prefix ? `VIN ${c.vin_prefix}…` : null, `times in ${zone(tz())}`].filter(Boolean).join(' · '),
      tools: [rec.el, linkBtn('Car overview', state.buildUrl('car', { car: cid }), { icon: 'gauge', sm: true }), linkBtn('Every car', state.buildUrl('settings-cars'), { icon: 'arrow-l', sm: true, kind: 'ghost' })] }));
    docTitle('Car settings', c.name);
  }

  /* ---- the car: presets to switch, its name, what it reports */
  function paintCar() {
    const c = S.car;
    const listed = (S.cars?.items || []).map((x) => (x.id === cid ? { ...x, name: c.name } : x));
    const shown = listed.some((x) => x.id === cid) ? listed : [c, ...listed.slice(0, 7)];
    const more = S.cars?.next || (S.cars?.total ?? 0) > shown.length;
    const switcher = h('div', { class: 'sc-switch', cover: 'web.settings.car.car-switcher' },
      h('div', { class: 'sc-presets', role: 'group', 'aria-label': 'Whose settings' },
        shown.map((x, i) => h('a', { class: ['sc-preset', x.id === cid && 'is-on'], href: state.buildUrl('settings-cars', { car: x.id }), 'aria-current': x.id === cid ? 'page' : null },
          h('span', { class: 'sc-preset-n' }, String(i + 1)), h('span', { class: 'ell' }, x.name)))),
      h('div', { class: 'row' }, btn(more ? `Find another car (${S.cars?.total ?? 'more'})` : 'Find a car', () => openCarPicker(), { icon: 'search', sm: true }),
        h('span', { class: 'faint small' }, 'or press ', h('span', { class: 'kbd' }, 'Ctrl'), ' ', h('span', { class: 'kbd' }, 'K'), ' anywhere. The picker keeps this page.')));

    const name = field({ label: 'Name', name: 'sc-name', path: 'name', value: c.name, hint: 'Shown in every list, on the radio and in the tab title. 1 to 64 characters.', attrs: { maxlength: '64', disabled: ro, autocomplete: 'off' } });
    name.input.addEventListener('change', () => {
      const v = name.input.value.trim();
      if (!v) { name.setError('A car needs a name.'); return; }
      if (v !== S.car.name) patchCar({ name: v }, 'Name', name);
    });
    const cfg = c.config || {};
    const facts = [
      ['Model and year', h('span', { class: 'tagline' }, carLine(c) || DASH, tag('From VIN'))],
      ['VIN start', c.vin_prefix ? code(c.vin_prefix) : DASH],
      ...FACTS.filter(([k]) => cfg[k]?.value != null).map(([k, l]) => [l, h('span', { class: 'tagline' }, yesNo(cfg[k].value), srcTag(cfg[k].source))]),
    ];
    const missing = FACTS.filter(([k]) => cfg[k]?.value == null).map(([, l]) => l.toLowerCase());
    replace(slots.car, hud({ title: 'The car', icon: 'car', id: 'sc-car', side: c.profile ? tag(`CAN profile ${c.profile}`, 'cyan') : null },
      h('div', { class: 'chans' },
        chan({ no: 1, title: 'Whose settings', text: 'Each car has its own settings; nothing here changes another car.' }, switcher,
          h('p', { class: 'chan-hint', cover: 'web.settings.car.empty-state' }, 'A new car appears by itself once its CAN Bridge has uploaded its first log; no Tesla account is involved. ',
            h('a', { class: 'plc', href: state.buildUrl('settings-cars') }, 'How a car appears'), '.')),
        chan({ no: 2, title: 'Name', text: 'Sentry Mate never reads a name from Tesla: you name the car. Until then it shows its model and VIN start.' }, name.el),
        chan({ no: 3, title: 'What the car reports', text: `From the car's build message on the vehicle bus, with model and year from the VIN start.${missing.length ? ` Not reported yet: ${missing.join(', ')}.` : ''}` }, kv(facts)))));
  }

  /* ---- uploads: warning lights, tokens, keys */
  function lights() {
    const hl = S.health, c = S.car;
    const last = hl?.last_upload_ms ?? c.now?.received_ms ?? null;
    const trust = hl?.last_log_trust?.result || null;
    const active = S.tokens.filter((t) => !t.revoked);
    const pref = S.keys.find((k) => k.preferred) || null;
    const lamp = (tone, label, value) => h('li', { class: ['sc-light', `sc-light--${tone}`] }, h('span', { class: 'sc-light-led', 'aria-hidden': 'true' }), h('span', { class: 'sc-light-l' }, label), h('span', { class: 'sc-light-v' }, value));
    let tone = 'ok', head, text = '';
    if (!last) { tone = 'warn'; head = 'No log has arrived for this car yet.'; text = active.length ? `${plural(active.length, 'upload token')} exist, none used lately. Check that the CAN Bridge app has the token and can reach this server.` : 'Make an upload token below and type it into the CAN Bridge app.'; }
    else if (c.now?.stale) { tone = 'warn'; head = `Quiet: the last log came ${ago(last)} (${dateTime(last, tz())}).`; text = trust === 'failed' ? 'That log failed its check, so it was not used.' : 'A parked car with its logger off sends nothing; a logger that lost its network catches up when it is back.'; }
    else { head = `Uploading: the last log came ${ago(last)}.`; }
    if (trust && tone === 'ok') text = trust === 'verified' ? 'Its signature matches a key tied to this car.' : trust === 'unsigned' ? 'It carried no signature, so it is kept but marked unsigned.' : 'It failed its check and was not used.';
    return h('div', { class: 'sc-lights-wrap', cover: 'faq.car-missing.outcomes' },
      h('ul', { class: 'sc-lights', 'aria-label': 'Upload lights' },
        lamp(last ? (c.now?.stale ? 'warn' : 'ok') : 'bad', 'Last log', last ? ago(last) : 'never'),
        lamp(trust === 'verified' ? 'ok' : trust === 'failed' ? 'bad' : trust ? 'warn' : 'off', 'Signature', trust || 'not yet'),
        lamp(active.length ? 'ok' : 'warn', 'Tokens', String(active.length)),
        lamp(pref ? 'ok' : S.keys.length ? 'warn' : 'off', 'Keys tied', pref ? `${S.keys.length} · 1 preferred` : String(S.keys.length)),
        lamp(S.waiting.length ? 'warn' : 'off', 'Keys waiting', String(S.waiting.length)),
        lamp(hl?.logs_waiting ? 'warn' : 'off', 'Logs to check', String(hl?.logs_waiting ?? 0))),
      callout(tone === 'ok' ? 'ok' : 'warn', head, text, ' ', h('a', { class: 'plc', href: state.buildUrl('health', { car: cid }) }, 'Upload status in Data health')));
  }
  async function makeToken() {
    let t;
    try { t = await ctx.api.post(`cars/${enc(cid)}/upload-tokens`, {}); } catch (e) { toast(`Could not make a token: ${errText(e)}`, { kind: 'error' }); return; }
    await showSecret(t, `Upload token for ${S.car.name}`, 'The first log sent with it ties the CAN Bridge key to this car.');
    S.tokens = (await ctx.api.get(`cars/${enc(cid)}/upload-tokens`, { limit: 100 }).catch(() => null))?.items || [t, ...S.tokens];
    if (ctx.alive()) paintUploads();
  }
  async function revoke(t) {
    if (!(await confirmDialog(`Revoke upload token ${t.id}? Logs sent with it are refused from now on; logs already here stay.`, { title: 'Revoke the token?', ok: 'Revoke', danger: true }))) return;
    try {
      const r = await ctx.api.del(`cars/${enc(cid)}/upload-tokens/${enc(t.id)}`);
      S.tokens = S.tokens.map((x) => (x.id === t.id ? { ...x, ...r, revoked: true } : x));
      rec.light('Token revoked'); toast(`Token ${t.id} revoked.`);
      paintUploads();
    } catch (e) { toast(`Could not revoke it: ${errText(e)}`, { kind: 'error' }); }
  }
  async function keyChange(k, body, done) {
    try { await ctx.api.patch(`can-keys/${enc(k.key)}`, body); rec.light('Key saved'); toast(done); await refresh(); } catch (e) { toast(`Could not change the key: ${errText(e)}`, { kind: 'error' }); }
  }
  function paintUploads() {
    const z = tz();
    const top = hud({ title: 'Uploads', icon: 'upload', id: 'sc-uploads', side: tag(ro ? 'Read only' : 'Admins', ro ? 'org' : 'mag') }, lights(),
      ro ? note('Upload tokens and CAN Bridge keys are shown to admins only: a token lets a logger send data for this car.') : null);
    if (ro) { replace(slots.uploads, top); return; }
    const tokenList = tracks(ctx, {
      title: 'Upload tokens', icon: 'key', rows: S.tokens, label: 'Upload tokens of this car', csvName: 'upload-tokens',
      note: 'A token lets a CAN Bridge upload for this car. It is shown once, when made. Revoking one refuses its uploads from then on; past logs stay.',
      side: btn('Make upload token', makeToken, { kind: 'go', icon: 'plus', sm: true }),
      sort: { key: 'made', dir: 'desc' },
      columns: [
        { key: 'id', label: 'Token', lead: true, render: (t) => code(t.id), value: (t) => t.id, sort: true },
        { key: 'made', label: 'Made', value: (t) => t.created_ms, sort: true, render: (t) => dateTime(t.created_ms, z), csv: (t) => new Date(t.created_ms).toISOString() },
        { key: 'state', label: 'State', value: (t) => (t.revoked ? 2 : t.last_upload_ms ? 0 : 1), sort: true, render: tokenSig, csv: tokenWord },
        { key: 'key', label: 'Key it brought', render: (t) => (t.key ? code(t.key) : h('span', { class: 'faint' }, t.revoked ? DASH : 'None yet')), csv: (t) => t.key || '' },
        { key: 'last', label: 'Last upload', num: true, value: (t) => t.last_upload_ms, sort: true, render: (t) => (t.last_upload_ms ? agoEl(t.last_upload_ms) : 'Never') },
        { key: 'act', label: '', cls: 'is-act', csv: false, render: (t) => (t.revoked ? null : btn('Revoke', () => revoke(t), { kind: 'danger', icon: 'x', sm: true })) },
      ],
      empty: () => emptyState({ icon: 'key', title: 'No upload tokens', text: 'Without a token no CAN Bridge can upload for this car. Make one and type it into the app.' }),
    });
    const keyList = tracks(ctx, {
      title: 'CAN Bridge keys', icon: 'chip', rows: S.keys, label: 'CAN Bridge keys tied to this car', csvName: 'can-keys',
      note: 'Each logger signs its logs with its own key: a Screenmate, a reinstall of the app or a Raspberry Pi each bring one. When two record the same time, the preferred key\'s log is used and the other is kept.',
      sort: { key: 'last', dir: 'desc' },
      columns: [
        { key: 'key', label: 'Key', lead: true, render: (k) => code(k.key), value: (k) => k.key, sort: true },
        { key: 'device', label: 'Device', render: (k) => k.device || h('span', { class: 'faint' }, 'Not reported'), sort: true },
        { key: 'first', label: 'First upload', value: (k) => k.first_seen_ms, sort: true, render: (k) => dateTime(k.first_seen_ms, z), csv: (k) => new Date(k.first_seen_ms).toISOString() },
        { key: 'last', label: 'Last upload', value: (k) => k.last_upload_ms, sort: true, render: (k) => (k.last_upload_ms ? agoEl(k.last_upload_ms) : 'Never') },
        { key: 'waiting', label: 'Logs waiting', num: true, value: (k) => k.logs_waiting ?? 0, sort: true, render: (k) => format(k.logs_waiting ?? 0, 'count') },
        { key: 'pref', label: 'Preferred', value: (k) => (k.preferred ? 0 : 1), sort: true, csv: (k) => (k.preferred ? 'yes' : 'no'),
          render: (k) => (k.preferred ? sig('ok', 'Preferred') : btn('Prefer this key', () => keyChange(k, { preferred: true }, `Key ${k.key} is now preferred for ${S.car.name}.`), { sm: true })) },
        { key: 'act', label: '', cls: 'is-act', csv: false, render: (k) => btn('Untie', async () => {
          if (await confirmDialog(`Untie key ${k.key} from ${S.car.name}? Its new logs wait under Keys waiting until it is tied again. Logs already read stay.`, { title: 'Untie the key?', ok: 'Untie', danger: true })) keyChange(k, { car: null }, `Key ${k.key} is no longer tied to ${S.car.name}.`);
        }, { kind: 'ghost', sm: true }) },
      ],
      empty: () => emptyState({ icon: 'chip', title: 'No key tied to this car', text: 'The first log sent with this car\'s upload token ties its key here. A waiting key can be tied by hand.' }),
    });
    replace(slots.uploads, top, tokenList.el, keyList.el, waitingKeys(ctx, S.waiting, S.car, refresh));
  }

  /* ---- time zone, on a clock face */
  const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
  function paintTz() {
    const z = S.car.time_zone || '';
    const listId = `sc-zones-${cid}`;
    const f = field({ label: 'Time zone', name: 'sc-tz', path: 'time_zone', value: z, hint: 'A zone name such as America/Chicago or Europe/Amsterdam. Type to search the list.', attrs: { list: listId, disabled: ro, autocomplete: 'off', spellcheck: 'false' } });
    f.input.addEventListener('change', () => {
      const v = f.input.value.trim();
      if (!v || v === S.car.time_zone) return;
      if (!isValidZone(v)) { f.setError('Not a time zone this browser knows. Pick one from the list.'); return; }
      patchCar({ time_zone: v }, 'Time zone', f);
    });
    const here = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
    const now = ctx.api.now();
    const zz = z || ctx.tz;
    replace(slots.tz, hud({ title: 'Time zone', icon: 'clock', id: 'sc-tz', cover: 'docs.env.core.tz' },
      h('div', { class: 'sc-clock', role: 'img', 'aria-label': `Now in ${zz}: ${time(now, zz)}` },
        h('span', { class: 'sc-clock-t' }, time(now, zz)), h('span', { class: 'sc-clock-d' }, `${date(now, zz)} · ${zone(zz)}`)),
      h('div', { class: 'chans' },
        chan({ no: 4, title: 'The car\'s zone', text: 'Days, weeks, months, statistics, charging grids and prices by time of day use it, wherever the server runs. A change regroups the whole history at once; nothing is lost.' },
          f.el, h('datalist', { id: listId }, (ZONES.includes(z) || !z ? ZONES : [z, ...ZONES]).map((x) => h('option', { value: x }))),
          here && here !== z && !ro ? btn(`Use this browser's zone (${here})`, () => patchCar({ time_zone: here }, 'Time zone', f), { icon: 'globe', sm: true }) : null))));
  }

  /* ---- battery chemistry */
  function paintBattery() {
    const bc = S.settings.battery_chemistry || { setting: 'auto' };
    const b = S.battery;
    const chosen = bc.setting;
    const seg = h('div', { class: 'seg', role: 'radiogroup', 'aria-label': 'Battery chemistry' },
      CHEM.map(([v, l]) => off(h('button', { class: 'seg-btn', type: 'button', role: 'radio', 'aria-checked': String(v === chosen), 'aria-pressed': String(v === chosen),
        onclick: () => { if (v !== chosen) patchSettings({ battery_chemistry: { setting: v } }, 'Battery chemistry'); } }, l))));
    const effective = chosen === 'auto' ? bc.detected : chosen;
    replace(slots.battery, hud({ title: 'Battery', icon: 'battery', id: 'sc-battery', cover: 'web.settings.car.lfp-battery' },
      h('div', { class: 'chans' },
        chan({ no: 5, title: 'Chemistry', text: 'LFP packs like a full charge now and then and read flat in the middle; nickel packs (NCA or NMC) last longer kept lower. Battery and Charges use this for their charge-level advice. Automatic follows the evidence.' },
          seg,
          h('p', { class: 'chan-hint' }, 'In use now: ', h('b', null, effective ? CHEM_WORD[effective] : 'not known yet'), chosen === 'auto' ? ' (automatic).' : ' (set by hand).'),
          kv([
            ['Detected', bc.detected ? CHEM_WORD[bc.detected] : 'not yet: a few charges and some driving are needed'],
            bc.evidence ? ['Evidence', bc.evidence] : null,
            b?.brick_spread_v != null ? ['Cell spread now', format(b.brick_spread_v, 'voltage', { digits: 3 })] : null,
            b?.capacity?.full_pack_kwh != null ? ['Full pack', format(b.capacity.full_pack_kwh, 'energy')] : null,
          ]),
          chosen !== 'auto' && bc.detected && bc.detected !== chosen ? h('p', { class: 'warn small' }, `Your choice differs from what the car's data suggests (${CHEM_WORD[bc.detected]}).`) : null))));
  }

  /* ---- free Supercharging as dated ranges */
  function paintCharging() {
    const access = S.car.config?.supercharging_access;
    let ranges = (S.settings.free_supercharging || []).map((r) => ({ ...r }));
    const list = h('div', { class: 'sc-ranges' });
    const msg = h('p', { class: 'field-error', role: 'alert', hidden: true });
    const say = (el, text) => { el.hidden = !text; if (text) replace(el, icon('warn', 'i--sm'), h('span', null, text)); };
    function problems(rows) {
      const errs = [];
      rows.forEach((r, i) => {
        if (!r.from_date && !r.to_date) return;
        if (!r.from_date) errs.push([i, 'from_date', 'Give the first free day.']);
        else if (r.to_date && r.to_date < r.from_date) errs.push([i, 'to_date', 'The last day is before the first day.']);
      });
      if (errs.length) return { errs, overlap: null };
      const order = rows.map((r, i) => ({ ...r, i })).filter((r) => r.from_date).sort((a, b) => (a.from_date < b.from_date ? -1 : 1));
      for (let j = 1; j < order.length; j++) {
        const a = order[j - 1], b = order[j];
        if (!a.to_date || b.from_date <= a.to_date) {
          const [x, y] = [a.i, b.i].sort((p, q) => p - q);
          return { errs: [], overlap: `Ranges ${x + 1} and ${y + 1} share days: ${date(dayMs(b.from_date), 'UTC', { year: true })} is in both${a.to_date ? '' : ` (range ${a.i + 1} has no end)`}. A day can be in one range only, so this was not saved.` };
        }
      }
      return null;
    }
    async function save(rows) {
      say(msg, null);
      for (const e of list.querySelectorAll('[data-err]')) e.hidden = true;
      const bad = problems(rows);
      if (bad) {
        for (const [i, w, t] of bad.errs) { const e = list.querySelector(`[data-err="${i}-${w}"]`); if (e) say(e, t); }
        if (bad.overlap) say(msg, bad.overlap);
        return;
      }
      await patchSettings({ free_supercharging: rows.filter((r) => r.from_date).map((r) => ({ from_date: r.from_date, to_date: r.to_date || null })) }, 'Free Supercharging', (fields) => {
        paintRows(rows);
        for (const [path, text] of Object.entries(fields)) {
          const m = /^free_supercharging\.(\d+)\.(from_date|to_date)$/.exec(path);
          const e = m && list.querySelector(`[data-err="${m[1]}-${m[2]}"]`);
          if (e) say(e, /before/i.test(text) ? 'The last day is before the first day.' : m[2] === 'from_date' ? 'Give the first free day as a date.' : 'Give the last free day as a date, or leave it empty.');
          else say(msg, String(text));
        }
      });
    }
    const length = (r) => (!r.from_date ? 'New range' : r.to_date && r.to_date < r.from_date ? 'Last day before the first' : r.to_date ? `Free for ${plural(days(r.from_date, r.to_date), 'day')}` : 'Free from then on, no end');
    function paintRows(rows) {
      ranges = rows;
      replace(list, rows.length ? rows.map((r, i) => {
        const from = off(h('input', { class: 'input', type: 'date', value: r.from_date || '', 'aria-label': `Range ${i + 1}: first day`, required: true }));
        const to = off(h('input', { class: 'input', type: 'date', value: r.to_date || '', 'aria-label': `Range ${i + 1}: last day (empty: still free)` }));
        const len = h('span', { class: 'sc-range-len' }, length(r));
        const changed = () => {
          const row = { from_date: from.value || null, to_date: to.value || null };
          ranges = ranges.map((x, j) => (j === i ? row : x));
          len.textContent = length(row);
          save(ranges);
        };
        from.addEventListener('change', changed); to.addEventListener('change', changed);
        return h('div', { class: 'sc-range' },
          h('span', { class: 'chan-no' }, `R${i + 1}`),
          h('label', { class: 'field' }, h('span', { class: 'field-label' }, 'From'), from, h('p', { class: 'field-error', dataset: { err: `${i}-from_date` }, hidden: true })),
          h('label', { class: 'field' }, h('span', { class: 'field-label' }, 'To (included)'), to, h('p', { class: 'field-error', dataset: { err: `${i}-to_date` }, hidden: true })),
          len,
          ro ? null : btn('Remove', () => { const next = ranges.filter((_, j) => j !== i); const had = ranges[i]?.from_date; paintRows(next); if (had) save(next); }, { kind: 'ghost', icon: 'x', sm: true, label: `Remove range ${i + 1}` }));
      }) : h('p', { class: 'faint small' }, 'No free ranges: every Supercharger session keeps the price it was billed or priced at.'));
    }
    paintRows(ranges);
    replace(slots.charging, hud({ title: 'Charging', icon: 'bolt', id: 'sc-charging', cover: 'web.settings.car.free-supercharging' },
      h('div', { class: 'chans' },
        chan({ no: 6, title: 'Free Supercharging', text: 'Supercharger sessions of this car that start on a free day cost nothing; sessions before or after keep their own cost, so a range never rewrites the rest of the history. Days are the car\'s days, in its time zone.', wide: true },
          hint(access?.value != null ? ['The car reports Supercharging access: ', h('b', null, String(access.value)), ' ', srcTag(access.source), '. That says what the car may do now, not since when: the dates are yours to give.'] : 'The car has not reported its Supercharging access yet. The dates are yours to give.'),
          list, msg, ro ? null : h('div', { class: 'row' }, btn('Add a free range', () => paintRows([...ranges, { from_date: null, to_date: null }]), { icon: 'plus', sm: true }))))));
  }

  /* ---- reference values */
  function paintRefs() {
    const s = S.settings;
    const disp = (v, kind) => (v == null ? '' : one(convert(v, kind)));
    const lim = (a, b, kind) => (kind ? [Math.ceil(convert(a, kind) * 10) / 10, Math.floor(convert(b, kind) * 10) / 10] : [a, b]);
    const defs = [
      ['capacity_new_kwh', 'Capacity when new', 'Empty: taken from the pack\'s first readings.', null, 'kWh', [10, 250], '0.1'],
      ['full_range_new_km', 'Full range when new', 'Empty: taken from the first 30 days.', 'distance', unitLabel('distance'), [100, 1500], '0.1'],
      ['warranty_threshold_pct', 'Warranty threshold', 'The share of the capacity when new that the battery warranty promises.', null, '%', [50, 95], '1'],
      ['efficiency_wh_per_km_override', 'Consumption factor', 'Empty: worked out from energy and range.', 'efficiency', unitLabel('efficiency'), [60, 500], '0.1'],
    ];
    const fields = defs.map(([k, label, why, kind, unit, [a, b], step]) => {
      const [lo, hi] = lim(a, b, kind);
      const range = `${one(lo)} to ${one(hi)} ${unit}`;
      const required = k === 'warranty_threshold_pct';
      const f = field({ label: `${label} (${unit})`, name: `sc-${k}`, path: k, type: 'number', value: kind ? disp(s[k], kind) : s[k] ?? '', hint: `${range}. ${why}`, attrs: { min: String(lo), max: String(hi), step, required, disabled: ro } });
      const sayRange = required ? `A number from ${range}.` : `A number from ${range}, or empty.`;
      f.input.addEventListener('change', () => {
        const raw = f.input.value.trim();
        if (raw === '' && required) { f.setError(sayRange); return; }
        const n = raw === '' ? null : Number(raw);
        if (n != null && (!Number.isFinite(n) || n < lo || n > hi)) { f.setError(sayRange); return; }
        f.setError(null);
        const v = n == null ? null : kind ? Math.round(toMetric(n, kind) * 10) / 10 : n;
        patchSettings({ [k]: v }, label, (errs) => { const t = errs[k] || Object.values(errs)[0]; f.setError(/between|at least|at most|range/i.test(t) ? sayRange : t); });
      });
      return f;
    });
    const sw = field({ label: 'Software version, typed', name: 'sc-sw', path: 'software_version_typed', value: s.software_version_typed || '', hint: 'Only for a car whose bus does not send it, such as 2026.38.4. Empty: what the car reports.', attrs: { disabled: ro, autocomplete: 'off', spellcheck: 'false' } });
    sw.input.addEventListener('change', () => patchSettings({ software_version_typed: sw.input.value.trim() || null }, 'Software version', (errs) => sw.setError(errs.software_version_typed || Object.values(errs)[0])));
    replace(slots.refs, hud({ title: 'Reference values', icon: 'sliders', id: 'sc-refs', side: tag('Optional') },
      h('div', { class: 'chans' },
        chan({ no: 7, title: 'Battery and range', text: 'Battery health compares today\'s capacity and range with these. Without them Sentry Mate uses its earliest readings, and says so on Battery.' },
          h('div', { class: 'sc-fields' }, fields[0].el, fields[1].el, fields[2].el)),
        chan({ no: 8, title: 'Consumption and software', text: 'The consumption factor turns range into energy on Efficiency. A typed version is marked as typed on Software.' },
          h('div', { class: 'sc-fields' }, fields[3].el, sw.el)))));
  }

  paintAll();
  replace(ctx.view, h('div', { class: 'page sc' },
    slots.head,
    h('section', { class: 'hud hud--plain jump', 'aria-label': 'Sections and saving' },
      presets([['sc-car', 'Car'], ['sc-uploads', 'Uploads'], ['sc-tz', 'Time zone'], ['sc-battery', 'Battery'], ['sc-charging', 'Charging'], ['sc-refs', 'Reference']]),
      h('p', { class: 'note jump-note', cover: 'web.settings.car.autosave' }, icon('check', 'i--sm'),
        h('span', null, ro ? 'These settings are changed by an admin; you can read them here.' : 'Every change saves at once; there is no Save button. The REC lamp lights and a short note confirms it.'))),
    slots.car, slots.uploads,
    h('div', { class: 'cols' }, slots.tz, slots.battery),
    slots.charging, slots.refs));
  ctx.events(['fleet'], (ev) => { if (ev.data?.car === cid && ev.data?.change === 'renamed' && ev.data.name !== S.car.name && !busy.car) refresh(); });
  ctx.ready();
}

// a calendar day (YYYY-MM-DD) as the ms of its midnight in UTC
function dayMs(d) { const [y, m, dd] = d.split('-').map(Number); return Date.UTC(y, m - 1, dd); }
// whole days from a to b, both counted
function days(a, b) { return Math.round((dayMs(b) - dayMs(a)) / 86400000) + 1; }
