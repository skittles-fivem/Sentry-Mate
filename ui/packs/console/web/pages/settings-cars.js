// settings/cars and settings/cars/{car}: per-car keys as 'key = value' lines, and how each car's CAN Bridge
// is tied to it.
// Without a car: every car the account sees, upload tokens for a car not here yet, keys waiting for a car,
// and how a car appears. With a car: name and what the car reports (each value with its source), upload
// tokens and CAN Bridge keys with the outcome of the uploads, time zone, battery chemistry, reference values
// and free Supercharging as dated ranges. Every line saves on its own (PATCH with the object's version).
// Data sources: the car's live inputs in priority order (drag, or up / down), saved as source_order, and whether
// imported history fills the gaps (import_gap_fill: true / false / null = the server's setting).
// API: GET/PATCH cars/{car}, cars/{car}/settings; cars/{car}/upload-tokens, upload-tokens, can-keys (admin).

import * as state from '../shared/state.js';
import { h, replace, setVars } from '../shared/dom.js';
import { invalidate } from '../shared/api.js';
import { isValidZone, dayKey, zoneSourceText } from '../shared/time.js';
import { convert, toMetric, unitLabel, format } from '../shared/units.js';
import { head, paint, section, note, link, btn, kv } from '../core/ui.js';
import { table } from '../core/table.js';
import { editLines, saveError } from '../core/kvedit.js';
import { empty } from '../core/states.js';
import { tag, stateWord, dateTime, date, time, ago, DASH } from '../core/fmt.js';
import { toast } from '../core/toast.js';
import { confirmDialog } from '../core/dialog.js';
import { copyBtn, showOnce } from '../core/secret.js';
import { openCarSwitcher } from '../core/carswitch.js';
import { src } from '../core/source.js';
import { deviceName } from '../shared/devices.js';
import { deviceText, renameBtn } from './device-name.js';

export const id = 'settings-cars';
export const css = true;
export const title = (ctx) => (ctx.car ? `Settings: ${ctx.car.name || 'car'}` : 'Settings: cars');

const enc = encodeURIComponent;
const SOURCE = { can: ['can', 'accent'], vin: ['vin', 'muted'], owner: ['typed', 'warn'], teslamate: ['imported', 'muted'], tesla_api: ['tesla api', 'muted'] };
const CHEM = [['auto', 'auto'], ['lfp', 'lfp'], ['nca_nmc', 'nca/nmc']];
const CHEM_WORDS = { lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA / NMC (nickel)' };
const CONFIG = [['trim', 'trim'], ['drivetrain', 'drivetrain'], ['performance_package', 'performance'], ['pack_kwh_code', 'pack code'], ['wheel', 'wheels'],
  ['paint', 'paint'], ['roof', 'roof'], ['spoiler', 'spoiler'], ['right_hand_drive', 'right-hand drive'], ['map_region', 'map region'], ['supercharging_access', 'supercharging']];
const LIVE = ['smo', 'tesla_api', 'teslafi_api', 'teslamate_local'];
const LIVE_LABEL = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'local TeslaMate' };
const sameOrder = (a, b) => Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((x, i) => x === b[i]);
const yesNo = (v) => (v === true ? 'yes' : v === false ? 'no' : String(v));
const oldTag = (s) => (SOURCE[s] ? tag(SOURCE[s][0], SOURCE[s][1]) : null);
// a config value { value, source, sources }: where it came from; empty sources: an admin typed it
const srcTag = (v) => (Array.isArray(v?.sources) ? src(v, { typed: 'typed by an admin' }) : oldTag(v?.source));
const meta = (c) => [c.model, c.year].filter(Boolean).join(' ');
const errMsg = (e) => saveError(e);
const soldTag = (c) => h('span', { title: c.sold_date ? `Sold on ${c.sold_date}` : 'Sold' }, tag('sold', 'muted'));

function howCarsAppear(admin) {
  return h('ol', { class: 'howto' }, [
    'Install the CAN Bridge app on the Screenmate One in the car (or another logger, such as a Raspberry Pi).',
    admin ? 'Make an upload token here and type it into the app. It is shown once.' : 'Type an upload token into the app. An admin makes one on this page.',
    'Wait for the first log: the car appears by itself, with its model and year from the VIN prefix. Name it and set its time zone.',
    'A second logger in the same car brings a new key. It waits under keys waiting until it is tied to the car.',
  ].map((t, i) => h('li', null, h('span', { class: 'howto-n' }, `${i + 1}.`), h('span', null, t))));
}

/** The token, once: the three fields of the CAN Bridge app's "Second Sentry Six Manager" section. */
function showToken(t, heading, after) {
  const app = t.app || { address: null, token: t.secret, certificate_sha256: null };
  const line = (k, v, why) => h('div', { class: 'hc' },
    h('p', null, h('span', { class: 'muted' }, `${k}: `), v ? h('span', { class: 'break' }, v) : h('span', { class: 'muted' }, 'not known to this server')),
    v ? h('div', { class: 'acts' }, copyBtn(v, `copy ${k}`)) : null, h('p', { class: 'fld-hint' }, why));
  return showOnce({
    title: heading, okLabel: 'I have copied them',
    body: h('div', { class: 'hc' },
      h('p', null, 'In the CAN Bridge app open ', h('b', null, 'Second Sentry Six Manager'), ' and fill in these three fields. ', tag('shown once', 'warn'), ' the server keeps only a check value.'),
      h('div', { class: 'box hc' },
        line('address', app.address, 'Where the app sends its logs: this server\'s upload port, over HTTPS.'),
        line('token', app.token || t.secret, 'Lets the app upload. Keep it like a password.'),
        line('certificate sha-256', app.certificate_sha256, 'The app trusts this server by it. It stays the same across restarts, updates and moves.')),
      note(after)),
  });
}

const tokenState = (t) => (t.revoked ? tag('revoked', 'muted') : t.last_upload_ms ? tag('in use', 'ok') : tag('not used yet', 'warn'));

export async function mount(ctx) { return ctx.car ? mountCar(ctx) : mountIndex(ctx); }

/* ---------------------------------------------------------------- settings/cars */
async function mountIndex(ctx) {
  const admin = ctx.admin;
  const [status, waiting, fresh] = await Promise.all([
    ctx.api.get('setup/status').catch(() => null),
    admin ? ctx.api.get('can-keys', { assigned: 'false', limit: 100 }).catch(() => null) : null,
    admin ? ctx.api.get('upload-tokens', { limit: 100 }).catch(() => null) : null,
  ]);
  if (!ctx.alive()) return;
  const noCars = status?.cars === 0;
  const tz = ctx.tz;
  const cars = noCars ? null : table(ctx, {
    title: 'cars', path: 'cars', params: { sort: 'custom' }, csvName: 'cars', label: 'Cars and their settings',
    rowHref: (c) => state.buildUrl('settings-cars', { car: c.id }),
    columns: [
      { key: 'name', label: 'car', ph: 'main', render: (c) => [link(state.buildUrl('settings-cars', { car: c.id }), c.name || c.id), state.isSold(c) ? [' ', soldTag(c)] : null] },
      { key: 'model', label: 'model', ph: 'sub', render: (c) => meta(c) || null },
      { key: 'vin_prefix', label: 'vin prefix', render: (c) => c.vin_prefix || null },
      { key: 'time_zone', label: 'time zone', ph: 'sub', render: (c) => c.time_zone || null },
      { key: 'state', label: 'now', ph: 'num', render: (c) => stateWord(c.now?.state, { stale: c.now?.stale }) },
      { key: 'last', label: 'last upload', grow: true, render: (c) => (c.now?.received_ms ? ago(c.now.received_ms) : 'never') },
    ],
    empty: () => empty('no car matches'),
  });
  const tokens = admin ? newCarTokens(ctx, fresh?.items || [], noCars) : null;
  const keys = admin ? waitingTable(ctx, waiting?.items || [], null, () => ctx.reload(), tz) : null;
  paint(ctx, head(ctx, { title: 'Settings: cars' }),
    h('p', { class: 'note', cover: 'web.settings.car.autosave' }, admin ? 'Open a car to change its keys: each line saves with Enter, Esc undoes.' : 'Car settings are changed by an admin; you can read them here.'),
    noCars ? section({ title: 'no cars yet', cover: 'web.settings.car.empty-state' }, empty('no cars yet', 'Sentry Mate needs no Tesla account: a car appears when its CAN Bridge uploads its first log.'), howCarsAppear(admin)) : null,
    cars?.el, tokens?.el, keys?.el,
    noCars ? null : section({ title: 'how a car appears', cover: 'web.settings.car.empty-state' }, howCarsAppear(admin)));
  await Promise.all([cars?.ready, tokens?.ready, keys?.ready].filter(Boolean));
  if (ctx.alive()) ctx.ready();
}

function newCarTokens(ctx, rows, noCars) {
  let list = rows;
  const t = table(ctx, {
    title: 'tokens for a new car', id: 'tokens', rows: list, csv: false, label: 'Upload tokens for a car that has not uploaded yet',
    note: noCars ? 'Make a token and type it into the CAN Bridge app: the first log sent with it makes the car here.' : 'For a car not here yet. For a car already here, make the token on its own page so the logs go to that car.',
    side: btn('make token', () => make(), { kind: 'primary' }),
    sort: { key: 'created', dir: 'desc' },
    columns: [
      { key: 'id', label: 'token', ph: 'main' },
      { key: 'created', label: 'made', ph: 'sub', value: (x) => x.created_ms, sort: true, render: (x) => dateTime(x.created_ms, ctx.tz) },
      { key: 'state', label: 'state', ph: 'num', grow: true, render: tokenState },
      { key: 'act', label: '', render: (x) => (x.revoked ? false : btn('revoke', () => revoke(x), { kind: 'danger' })), ph: 'num' },
    ],
    empty: () => empty('no token waits for a new car', 'Press make token when a new car joins.'),
  });
  async function reload(fallback) { list = (await ctx.api.get('upload-tokens', { limit: 100 }).catch(() => null))?.items || fallback; if (ctx.alive()) t.setRows(list); }
  async function make() {
    let tok;
    try { tok = await ctx.api.post('upload-tokens', {}); } catch (e) { toast(`Could not make a token: ${errMsg(e)}`, { kind: 'error' }); return; }
    await showToken(tok, 'upload token for a new car', 'The first log sent with it makes the car here, named after its model until you name it. The token then moves to that car\'s page.');
    await reload([tok, ...list]);
  }
  async function revoke(x) {
    if (!(await confirmDialog(`Revoke token ${x.id}? A logger that has it can no longer bring a car in with it.`, { title: 'revoke the token?', ok: 'revoke', danger: true }))) return;
    try { await ctx.api.del(`upload-tokens/${enc(x.id)}`); toast(`Token ${x.id} revoked.`); await reload(list); } catch (e) { toast(`Could not revoke: ${errMsg(e)}`, { kind: 'error' }); }
  }
  return t;
}

function waitingTable(ctx, rows, car, after, tz) {
  const assign = async (k, preferred) => {
    try {
      await ctx.api.patch(`can-keys/${enc(k.key)}`, preferred ? { car: car.id, preferred: true } : { car: car.id });
      toast(`Key ${k.key} is tied to ${car.name}${preferred ? ' and preferred' : ''}. Its waiting logs are being read.`);
      after();
    } catch (e) { toast(`Could not assign the key: ${errMsg(e)}`, { kind: 'error' }); }
  };
  return table(ctx, {
    title: 'keys waiting for a car', rows, csv: false, label: 'CAN Bridge keys waiting to be tied to a car',
    note: car ? `These keys uploaded logs but are tied to no car; their logs wait, unread. Tie one to ${car.name} only if it records this car.`
      : 'These keys uploaded logs but are tied to no car; their logs wait, unread. Open the car a key records to tie it there.',
    sort: { key: 'first', dir: 'desc' },
    columns: [
      { key: 'key', label: 'key', ph: 'main' },
      { key: 'device', label: 'device', ph: 'sub', render: (k) => [deviceText(k), ctx.admin ? [' ', renameBtn(ctx, k, () => after())] : null] },
      { key: 'first', label: 'first upload', sort: true, value: (k) => k.first_seen_ms, render: (k) => dateTime(k.first_seen_ms, tz) },
      { key: 'last', label: 'last upload', ph: 'sub', sort: true, value: (k) => k.last_upload_ms, render: (k) => (k.last_upload_ms ? ago(k.last_upload_ms) : 'never') },
      { key: 'logs_waiting', label: 'logs', num: true, ph: 'num', render: (k) => format(k.logs_waiting ?? 0, 'count') },
      { key: 'act', label: '', grow: true, render: (k) => (car ? h('div', { class: 'acts' }, btn(`tie to ${car.name}`, () => assign(k, false), { kind: 'primary' }), btn('tie and prefer', () => assign(k, true)))
        : h('span', { class: 'muted' }, 'open its car to tie it')) },
    ],
    empty: () => empty('no keys waiting', 'Every CAN Bridge that uploaded is tied to a car.'),
  });
}

/* ---------------------------------------------------------------- settings/cars/{car} */
async function mountCar(ctx) {
  const admin = ctx.admin;
  const ro = !admin;
  const cid = ctx.car.id;
  const S = {};
  // first: the page's first load reuses the car the router read a moment ago; later loads ask again
  async function load(first = false) {
    const [car, settings, tokens, keys, waiting, health, battery, cars] = await Promise.all([
      ctx.api.get(`cars/${enc(cid)}`, null, first ? { maxAgeMs: 5000 } : undefined),
      ctx.api.get(`cars/${enc(cid)}/settings`),
      admin ? ctx.api.get(`cars/${enc(cid)}/upload-tokens`, { limit: 100 }).catch(() => null) : null,
      admin ? ctx.api.get('can-keys', { car: cid, limit: 100 }).catch(() => null) : null,
      admin ? ctx.api.get('can-keys', { assigned: 'false', limit: 100 }).catch(() => null) : null,
      ctx.api.get(`cars/${enc(cid)}/health`).catch(() => null),
      ctx.api.get(`cars/${enc(cid)}/battery`).catch(() => null),
      ctx.api.page('cars', { sort: 'custom' }, { limit: 8 }).catch(() => null),
    ]);
    Object.assign(S, { car, settings, tokens: tokens?.items || [], keys: keys?.items || [], waiting: waiting?.items || [], health, battery, cars: cars?.items || [], more: !!cars?.next });
  }
  await load(true);
  if (!ctx.alive()) return;
  const tz = () => S.car.time_zone || ctx.tz;

  // saves of one object go one after the other, each with the version the one before got back
  const queue = { car: Promise.resolve(), settings: Promise.resolve() };
  const serial = (k, fn) => { const run = queue[k].then(fn); queue[k] = run.catch(() => {}); return run; };
  const stale = async () => { await load().catch(() => {}); if (ctx.alive()) repaint(); return new Error('Someone else changed this first; the page shows their change now. Try again.'); };
  const patchCar = (body) => serial('car', async () => {
    try { S.car = await ctx.api.patch(`cars/${enc(cid)}`, { version: S.car.version, ...body }); } catch (e) { if (e.status === 409) throw await stale(); throw e; }
    invalidate('cars');
    Object.assign(ctx.car, { name: S.car.name, time_zone: S.car.time_zone, time_zone_source: S.car.time_zone_source });
  });
  const patchSettings = (body) => serial('settings', async () => {
    try { S.settings = await ctx.api.patch(`cars/${enc(cid)}/settings`, { version: S.settings.version, ...body }); } catch (e) { if (e.status === 409) throw await stale(); throw e; }
  });

  const slots = { top: h('div'), car: h('div'), sources: h('div'), sold: h('div'), uploads: h('div'), tz: h('div'), battery: h('div'), refs: h('div'), free: h('div') };
  function repaint() { paintTop(); paintCar(); paintSources(); paintSold(); paintUploads(); paintTz(); paintBattery(); paintRefs(); paintFree(); }

  /* ---- where this is, and the other cars */
  function paintTop() {
    const c = S.car;
    // sold cars leave the picker; the open one stays, sold or not
    const others = S.cars.filter((x) => x.id === cid || !state.isSold(x));
    const list = others.some((x) => x.id === cid) ? others.map((x) => (x.id === cid ? { ...x, name: c.name } : x)) : [c, ...others.slice(0, 7)];
    replace(slots.top,
      h('p', { class: 'trail', cover: 'web.settings.car.breadcrumb' }, link(state.buildUrl('settings-cars'), 'settings/cars'), ' / ', h('span', null, c.name || cid), state.isSold(c) ? [' ', soldTag(c)] : null,
        h('span', { class: 'muted' }, `  ${[meta(c), c.vin_prefix ? `vin ${c.vin_prefix}` : null, `times in ${tz()}`].filter(Boolean).join(' · ')}`)),
      h('div', { class: 'carline', cover: 'web.settings.car.car-switcher', role: 'group', 'aria-label': 'Whose settings' },
        h('span', { class: 'muted' }, 'car:'),
        list.map((x) => h('a', { class: 'key', href: state.buildUrl('settings-cars', { car: x.id }), 'aria-pressed': String(x.id === cid), 'aria-current': x.id === cid ? 'page' : null }, x.name || x.id, x.id === cid && state.isSold(c) ? ' (sold)' : '')),
        S.more ? btn('find another car', () => openCarSwitcher()) : null));
  }

  /* ---- name and what the car reports */
  function paintCar() {
    const c = S.car, cfg = c.config || {};
    const lines = editLines([
      { key: 'name', id: 'name', type: 'text', value: c.name || '', readOnly: ro, attrs: { maxlength: '64' },
        help: 'Sentry Mate never reads a name from Tesla: you name the car. Shown in every list and the tab title.',
        parse: (t) => (t.trim() ? { value: t.trim() } : { error: 'a car needs a name, 1 to 64 characters' }),
        save: async (v) => { await patchCar({ name: v }); paintTop(); } },
    ]);
    const missing = CONFIG.filter(([k]) => cfg[k]?.value == null).map(([, l]) => l);
    replace(slots.car, section({ title: 'car', side: h('span', { class: 'muted' }, c.profile ? `can profile ${c.profile}` : '') },
      lines.el,
      kv([
        ['model', [meta(c) || DASH, ' ', tag('vin', 'muted')]],
        ['vin prefix', c.vin_prefix || DASH],
        ...CONFIG.filter(([k]) => cfg[k]?.value != null).map(([k, l]) => [l, [yesNo(cfg[k].value), ' ', srcTag(cfg[k])]]),
      ]),
      h('p', { class: 'note' }, 'Read from the car\'s build message on its bus, or from the Tesla API; model and year from the VIN prefix. Each value says where it came from: ',
        src({ sources: [{ kind: 'smo', label: 'Data from SMO' }] }), ' SMO (the car\'s CAN Bridge), ', src({ sources: [{ kind: 'tesla_api', label: 'Data from the Tesla API' }] }), ' the Tesla API, ',
        src({ sources: [{ kind: 'import', label: 'Imported' }] }), ' an import, ', src({ sources: [] }, { typed: 'typed by an admin' }), ' typed by an admin, ', tag('vin', 'muted'), ' the VIN prefix.',
        missing.length ? ` Not reported: ${missing.join(', ')}.` : ''),
      h('p', { class: 'note', cover: 'web.settings.car.empty-state' }, 'A new car appears by itself once its CAN Bridge has uploaded its first log, or when a vehicle of a Tesla account is matched to a new car under ',
        link(`${state.buildUrl('settings-integrations')}#tesla`, 'settings/integrations'), '. ', link(state.buildUrl('settings-cars'), 'how a car appears'), '.')));
  }

  /* ---- data sources: the live inputs in priority order, and gap filling from imported history */
  let sourcesBusy = false;
  function paintSources(problem = null) {
    const s = S.settings;
    if (!Array.isArray(s.source_order)) { replace(slots.sources); return; }   // an older server: no order to show
    const order = s.source_order.filter((k) => LIVE.includes(k));
    const def = Array.isArray(s.source_order_default) ? s.source_order_default : LIVE;
    const byKind = new Map((Array.isArray(s.inputs) ? s.inputs : []).map((i) => [i.kind, i]));
    const nameOf = (k) => byKind.get(k)?.label || LIVE_LABEL[k] || k;
    const msg = h('p', { class: 'ke-msg', role: 'alert', hidden: true });
    const say = (t) => { msg.hidden = !t; replace(msg, t || ''); };
    const list = h('ol', { class: 'ds-list', 'aria-label': 'Live inputs, highest priority first' });

    async function save(next, done, focus = null) {
      if (sourcesBusy || sameOrder(next, order)) return;
      sourcesBusy = true;
      say(null);
      rows(next);
      let problem = null;
      try {
        await patchSettings({ source_order: next });
        toast(done);
      } catch (e) { problem = saveError(e, 'source_order'); }
      sourcesBusy = false;
      if (!ctx.alive()) return;
      paintSources(problem);
      if (focus) slots.sources.querySelector(`[data-ds="${focus}"]`)?.focus({ preventScroll: true });
    }
    const move = (k, d) => {
      const i = order.indexOf(k), j = i + d;
      if (i < 0 || j < 0 || j >= order.length) return;
      const next = [...order];
      [next[i], next[j]] = [next[j], next[i]];
      const f = `${k}:${d < 0 && j > 0 ? 'up' : d > 0 && j < order.length - 1 ? 'dn' : 'grip'}`;
      save(next, `${nameOf(k)} moved ${d < 0 ? 'up' : 'down'}: now ${j + 1} of ${order.length}.`, f);
    };

    // drag by the grip (mouse, pen and touch: pointer events, so a finger can drag too); the arrows on the grip move it too
    function grip(k) {
      const g = h('button', { class: 'btn btn--quiet ds-grip', type: 'button', dataset: { ds: `${k}:grip` }, title: 'Drag to change the order (or use the arrow keys)',
        'aria-label': `Drag ${nameOf(k)} to change its place; arrow keys move it`, 'aria-keyshortcuts': 'ArrowUp ArrowDown', disabled: sourcesBusy }, '::');
      g.addEventListener('keydown', (e) => {
        if (e.key === 'ArrowUp' || e.key === 'ArrowDown') { e.preventDefault(); e.stopPropagation(); move(k, e.key === 'ArrowUp' ? -1 : 1); }
      });
      g.addEventListener('pointerdown', (e) => {
        if (e.button !== 0 || sourcesBusy) return;
        e.preventDefault();
        // the rows stay where they are in the page (moving the dragged one would end the drag); only their shown
        // place changes (--ds-at, the flex order), and the order is read from it when the pointer is let go
        const li = g.closest('li');
        const items = [...list.querySelectorAll('li[data-kind]')];
        let at = items.indexOf(li);
        const place = (ks) => { ks.forEach((x, i) => { setVars(x, { '--ds-at': String(i) }); x.querySelector('.ds-n').textContent = `${i + 1}.`; }); };
        const shown = () => [...items].sort((a, b) => Number(a.style.getPropertyValue('--ds-at')) - Number(b.style.getPropertyValue('--ds-at')));
        place(items);
        g.setPointerCapture(e.pointerId);
        li.classList.add('is-drag');
        const onMove = (ev) => {
          const others = shown().filter((x) => x !== li);
          let to = others.findIndex((x) => { const r = x.getBoundingClientRect(); return ev.clientY < r.top + r.height / 2; });
          if (to < 0) to = others.length;
          if (to === at) return;
          at = to;
          others.splice(to, 0, li);
          place(others);
        };
        const end = () => {
          g.removeEventListener('pointermove', onMove); g.removeEventListener('pointerup', end); g.removeEventListener('pointercancel', end);
          li.classList.remove('is-drag');
          const next = shown().map((x) => x.dataset.kind);
          if (sameOrder(next, order)) { rows(order); return; }
          save(next, `Source order saved: ${next.map(nameOf).join(', ')}.`, `${k}:grip`);
        };
        g.addEventListener('pointermove', onMove); g.addEventListener('pointerup', end); g.addEventListener('pointercancel', end);
      });
      return g;
    }
    function rows(ks = order) {
      replace(list, ks.map((k, i) => {
        const inp = byKind.get(k);
        const on = !!inp?.on;
        return h('li', { class: ['ds-row', !on && 'ds-row--off'], dataset: { kind: k, row: '' } },
          h('span', { class: 'ds-n', 'aria-hidden': 'true' }, `${i + 1}.`),
          h('span', { class: 'ds-name' }, h('span', { class: 'src-b' }, '[', h('span', { class: ['src-k', `src-k--${k}`] }, { smo: 'smo', tesla_api: 'api', teslafi_api: 'tfi', teslamate_local: 'tml' }[k] || k), ']'), ` ${nameOf(k)}`),
          h('span', { class: 'ds-on' }, on ? tag('on', 'ok') : tag('off', 'muted')),
          h('span', { class: 'ds-last muted' }, inp?.last_ms != null ? `newest ${ago(inp.last_ms)}` : on ? 'nothing yet' : 'not set up here'),
          ro ? null : h('span', { class: 'ds-acts' },
            h('button', { class: 'btn btn--quiet', type: 'button', dataset: { ds: `${k}:up` }, disabled: i === 0 || sourcesBusy, 'aria-label': `Move ${nameOf(k)} up`, onclick: () => move(k, -1) }, 'up'),
            h('button', { class: 'btn btn--quiet', type: 'button', dataset: { ds: `${k}:dn` }, disabled: i === ks.length - 1 || sourcesBusy, 'aria-label': `Move ${nameOf(k)} down`, onclick: () => move(k, 1) }, 'dn'),
            grip(k)));
      }), h('li', { class: 'ds-row ds-row--imp' },
        h('span', { class: 'ds-n', 'aria-hidden': 'true' }, `${ks.length + 1}.`),
        h('span', { class: 'ds-name' }, h('span', { class: 'src-b' }, '[', h('span', { class: 'src-k src-k--import' }, 'imp'), ']'), ' imported history'),
        h('span', { class: 'ds-last muted' }, 'always after the live inputs')));
    }
    rows();
    say(problem);

    // gap filling: on, off or the server's setting (null)
    const imports = Array.isArray(s.imports) ? s.imports : [];
    const srvOn = s.import_gap_fill_default === true ? 'on' : s.import_gap_fill_default === false ? 'off' : 'not known';
    const gfValue = s.import_gap_fill === true ? 'on' : s.import_gap_fill === false ? 'off' : 'server';
    const z = tz();
    const impText = imports.length ? imports.map((x) => `${x.label}${x.from_ms != null && x.to_ms != null ? ` (${date(x.from_ms, z)} to ${date(x.to_ms, z)})` : ''}`).join('; ') : null;
    const gap = editLines([
      { key: 'import_gap_fill', id: 'gap-fill', type: 'choice', value: gfValue, label: 'Fill gaps from imported history', readOnly: ro,
        options: [['server', `server default: ${srvOn}`], ['on', 'on'], ['off', 'off']],
        disabledOption: () => !imports.length,
        show: (v) => (v === 'server' ? `server default: ${srvOn}` : v),
        help: [h('b', null, 'Fill gaps from imported history'), ': where no live input recorded, the import fills in. ',
          imports.length ? ['The imports it uses: ', impText, '. '] : h('span', { class: 'tone--warn' }, 'No imported history for this car. '),
          imports.length ? `Now ${s.import_gap_fill_on ? 'on' : 'off'}${s.import_gap_fill == null ? ', as the server sets it' : ''}.` : 'Import a TeslaMate or TeslaFi history first.'],
        save: async (v) => {
          await patchSettings({ import_gap_fill: v === 'on' ? true : v === 'off' ? false : null });
          toast(v === 'server' ? `Gap filling follows the server: ${srvOn}.` : `Gap filling from imported history: ${v}.`);
          if (ctx.alive()) queueMicrotask(paintSources);
        } },
    ]);

    const differs = !sameOrder(order, def);
    replace(slots.sources, section({ title: 'data sources', id: 'data-sources',
      side: ro ? h('span', { class: 'muted' }, 'set by an admin') : differs ? btn('reset to default order', () => save([...def], `Source order back to the default: ${def.map(nameOf).join(', ')}.`)) : h('span', { class: 'muted' }, 'default order') },
      h('p', { class: 'note ds-why' }, 'When two sources cover the same time, the higher one wins, value by value. Imported history always comes after the live inputs.'),
      list, msg,
      gap.el,
      h('p', { class: 'note' }, ro
        ? [link(state.buildUrl('timeline', { car: cid }), 'Choose per span which source is shown', { class: 'lnk lnk--tap' }), ': the timeline marks where imported and live history overlap; an admin chooses in import.']
        : [link(`${state.buildUrl('import')}#overlaps`, 'Choose per span which source is shown', { class: 'lnk lnk--tap' }), ' where imported and live history overlap.'])));
  }

  /* ---- sold: the car left the fleet; its history stays */
  function paintSold() {
    const c = S.car;
    const sold = state.isSold(c);
    const msg = h('p', { class: 'ke-msg', role: 'alert', hidden: true });
    const say = (t) => { msg.hidden = !t; replace(msg, t || ''); };
    const day = h('input', { class: 'in in--date', type: 'date', value: c.sold_date || '', disabled: ro, 'aria-label': 'Sold on (optional)' });
    async function save(body, done) {
      say(null);
      try { await patchCar(body); toast(done); } catch (e) { if (ctx.alive()) { paintSold(); paintTop(); } say(errMsg(e)); return; }
      if (ctx.alive()) { paintTop(); paintSold(); }
    }
    if (sold && !ro) day.addEventListener('change', () => save({ sold_date: day.value || null }, day.value ? `Sold date saved: ${day.value}.` : 'Sold date cleared.'));
    const stateLine = sold ? [tag('sold', 'muted'), ' ', h('b', null, c.sold_date ? `Sold on ${c.sold_date}` : 'Sold'), c.sold_date ? null : h('span', { class: 'muted' }, ' (no date given)')]
      : [tag('active', 'ok'), ' in the fleet: live state, attention and the Tesla API are on'];
    const controls = ro ? null : h('div', { class: 'ke-list' }, h('div', { class: 'ke' },
      h('span', { class: 'ke-k' }, 'sold_date'), h('span', { class: 'ke-eq' }, '='),
      h('div', { class: 'ke-v range-line' }, day, h('span', { class: 'muted' }, sold ? 'change saves' : 'optional')),
      h('span', { class: 'ke-side' }, sold
        ? btn('mark as not sold', () => save({ sold: false }, `${c.name || 'The car'} is not sold: back in the fleet.`))
        : btn('mark as sold', () => save(day.value ? { sold: true, sold_date: day.value } : { sold: true }, `${c.name || 'The car'} is marked sold${day.value ? ` on ${day.value}` : ''}.`), { kind: 'primary' }))));
    replace(slots.sold, section({ title: 'sold', id: 'sold', side: ro ? h('span', { class: 'muted' }, 'set by an admin') : null },
      h('p', { class: 'note', role: 'status' }, stateLine),
      controls, msg,
      note('A sold car keeps all its history and every page of it. It leaves the fleet list, the totals and the car pickers (shown under sold cars), raises no attention, and the Tesla API and TeslaFi store nothing more for it.')));
  }

  /* ---- uploads: what happened, tokens, keys, keys waiting */
  function outcome() {
    const hl = S.health, c = S.car;
    const last = hl?.last_upload_ms ?? c.now?.received_ms ?? null;
    const trust = hl?.last_log_trust?.result || null;
    const active = S.tokens.filter((t) => !t.revoked);
    const pref = S.keys.find((k) => k.preferred);
    let t, text;
    if (!last) { t = tag('todo', 'warn'); text = active.length ? 'No log has arrived for this car yet. A token exists: check the CAN Bridge app has it and can reach this server.' : 'No log has arrived yet. Make an upload token below and type it into the CAN Bridge app.'; }
    else if (c.now?.stale) { t = tag('quiet', 'warn'); text = `The last log arrived ${ago(last)} (${dateTime(last, tz())}). A parked car with its logger off sends nothing; a logger without network catches up later.`; }
    else { t = tag('ok', 'ok'); text = `Uploading: the last log arrived ${ago(last)}.`; }
    const more = [
      trust === 'verified' ? 'Its signature matches a key tied to this car.' : trust === 'unsigned' ? 'It carried no signature: kept, marked unsigned.' : trust === 'failed' ? 'It failed its check and was not used.' : null,
      pref ? `Preferred key: ${pref.key} (${deviceName(pref).text}).` : null,
      S.waiting.length ? `${S.waiting.length} key${S.waiting.length === 1 ? '' : 's'} waiting for a car below.` : null,
      hl?.logs_waiting ? `${hl.logs_waiting} logs wait to be checked.` : null,
    ].filter(Boolean).join(' ');
    return h('ul', { class: 'ln-list', cover: 'faq.car-missing.outcomes', role: 'status' },
      h('li', null, t, h('span', { class: 'ln-list-t' }, text, more ? h('span', { class: 'muted' }, more) : null, link(state.buildUrl('health', { car: cid }), 'upload status in health'))));
  }
  async function makeToken() {
    let t;
    try { t = await ctx.api.post(`cars/${enc(cid)}/upload-tokens`, {}); } catch (e) { toast(`Could not make a token: ${errMsg(e)}`, { kind: 'error' }); return; }
    await showToken(t, `upload token for ${S.car.name}`, 'The first log sent with it ties the CAN Bridge key to this car.');
    S.tokens = (await ctx.api.get(`cars/${enc(cid)}/upload-tokens`, { limit: 100 }).catch(() => null))?.items || [t, ...S.tokens];
    if (ctx.alive()) paintUploads();
  }
  async function revokeToken(t) {
    if (!(await confirmDialog(`Revoke token ${t.id}? Logs sent with it are refused from now on; logs already here stay.`, { title: 'revoke the token?', ok: 'revoke', danger: true }))) return;
    try { const r = await ctx.api.del(`cars/${enc(cid)}/upload-tokens/${enc(t.id)}`); S.tokens = S.tokens.map((x) => (x.id === t.id ? { ...x, ...r } : x)); toast(`Token ${t.id} revoked.`); paintUploads(); } catch (e) { toast(`Could not revoke: ${errMsg(e)}`, { kind: 'error' }); }
  }
  async function keyChange(k, body, done) {
    try { await ctx.api.patch(`can-keys/${enc(k.key)}`, body); toast(done); await load(); if (ctx.alive()) paintUploads(); } catch (e) { toast(`Could not change the key: ${errMsg(e)}`, { kind: 'error' }); }
  }
  let uploadTables = [];
  function paintUploads() {
    const z = tz();
    if (ro) {
      replace(slots.uploads, section({ title: 'uploads', id: 'tokens' }, outcome(), note('Upload tokens and CAN Bridge keys are shown to admins only: a token lets a logger send data for this car.')));
      uploadTables = [];
      return;
    }
    const tokens = table(ctx, {
      title: 'upload tokens', id: 'tokens', rows: S.tokens, csv: false, label: 'Upload tokens of this car',
      note: 'A token lets a CAN Bridge upload logs for this car. Each is shown once, when made. Revoking one refuses its uploads from then on; past logs stay.',
      side: btn('make token', makeToken, { kind: 'primary' }),
      sort: { key: 'created', dir: 'desc' },
      columns: [
        { key: 'id', label: 'token', ph: 'main' },
        { key: 'created', label: 'made', sort: true, ph: 'sub', value: (t) => t.created_ms, render: (t) => dateTime(t.created_ms, z) },
        { key: 'state', label: 'state', ph: 'num', render: tokenState },
        { key: 'key', label: 'key it brought', ph: 'sub', render: (t) => t.key || h('span', { class: 'muted' }, t.revoked ? DASH : 'none yet') },
        { key: 'last', label: 'last upload', sort: true, value: (t) => t.last_upload_ms, render: (t) => (t.last_upload_ms ? ago(t.last_upload_ms) : 'never') },
        { key: 'act', label: '', grow: true, render: (t) => (t.revoked ? false : btn('revoke', () => revokeToken(t), { kind: 'danger' })) },
      ],
      empty: () => empty('no upload tokens', 'Without a token no CAN Bridge can upload for this car.'),
    });
    const keys = table(ctx, {
      title: 'can bridge keys', rows: S.keys, csv: false, label: 'CAN Bridge keys tied to this car',
      note: 'Each logger signs its logs with its own key. When two record the same time, the preferred key\'s log is used and the other is kept.',
      sort: { key: 'last', dir: 'desc' },
      columns: [
        { key: 'key', label: 'key', ph: 'main' },
        { key: 'device', label: 'device', ph: 'sub', render: (k) => [deviceText(k), ' ', renameBtn(ctx, k, (row) => { S.keys = S.keys.map((x) => (x.key === k.key ? { ...x, ...row } : x)); if (ctx.alive()) paintUploads(); })] },
        { key: 'first', label: 'first upload', sort: true, value: (k) => k.first_seen_ms, render: (k) => dateTime(k.first_seen_ms, z) },
        { key: 'last', label: 'last upload', sort: true, ph: 'sub', value: (k) => k.last_upload_ms, render: (k) => (k.last_upload_ms ? ago(k.last_upload_ms) : 'never') },
        { key: 'logs_waiting', label: 'logs waiting', num: true, render: (k) => format(k.logs_waiting ?? 0, 'count') },
        { key: 'preferred', label: 'preferred', ph: 'num', render: (k) => (k.preferred ? tag('preferred', 'ok') : btn('prefer', () => keyChange(k, { preferred: true }, `Key ${k.key} is preferred now.`))) },
        { key: 'act', label: '', grow: true, render: (k) => btn('untie', async () => {
          if (await confirmDialog(`Untie key ${k.key} from ${S.car.name}? Its new logs wait under keys waiting until it is tied again. Logs already read stay.`, { title: 'untie the key?', ok: 'untie', danger: true })) keyChange(k, { car: null }, `Key ${k.key} is untied.`);
        }, { kind: 'quiet' }) },
      ],
      empty: () => empty('no key tied to this car', 'The first log sent with this car\'s token ties its key here.'),
    });
    const waiting = waitingTable(ctx, S.waiting, S.car, async () => { await load(); if (ctx.alive()) paintUploads(); }, z);
    uploadTables = [tokens, keys, waiting];
    replace(slots.uploads, section({ title: 'uploads', side: h('span', { class: 'muted' }, 'admins only') }, outcome()), tokens.el, keys.el, waiting.el);
  }

  /* ---- time zone */
  const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
  function paintTz() {
    const z = S.car.time_zone || '';
    const src = S.car.time_zone_source;
    const srcText = zoneSourceText(src);
    const listId = `sc-zones-${cid}`;
    const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
    const now = ctx.api.now();
    const lines = editLines([
      { key: 'time_zone', id: 'time-zone', type: 'text', value: z, readOnly: ro, cover: 'docs.env.core.tz', attrs: { list: listId },
        parse: (t) => (isValidZone(t.trim()) ? { value: t.trim() } : { error: 'not a time zone this browser knows: pick one from the list, such as Europe/Amsterdam' }),
        help: [srcText ? `${z} (${srcText})${src === 'owner' ? '' : ': type or pick a zone to set it for this car'}. ` : '', `Days, ISO weeks, months, statistics and prices by time of day use this zone, wherever the server runs. A change regroups the whole history at once; nothing is lost. Now there: ${time(now, z || ctx.tz)}, ${date(now, z || ctx.tz)}.`],
        after: [src === 'owner' && !ro ? btn('automatic', async () => {
          try { await patchCar({ time_zone: null }); toast('Time zone: worked out from the car’s position again.'); repaint(); } catch (e) { toast(errMsg(e), { kind: 'error' }); }
        }) : null, browserZone && browserZone !== z && !ro ? btn(`use ${browserZone}`, async () => {
          try { await patchCar({ time_zone: browserZone }); toast(`Time zone saved: ${browserZone}.`); repaint(); } catch (e) { toast(errMsg(e), { kind: 'error' }); }
        }) : null].filter(Boolean),
        save: async (v) => { await patchCar({ time_zone: v }); paintTop(); } },
    ]);
    replace(slots.tz, section({ title: 'time zone' }, lines.el, h('datalist', { id: listId }, ZONES.map((x) => h('option', { value: x })))));
  }

  /* ---- battery chemistry */
  function paintBattery() {
    const bc = S.settings.battery_chemistry || { setting: 'auto' };
    const b = S.battery;
    const eff = bc.setting === 'auto' ? bc.detected : bc.setting;
    const lines = editLines([
      { key: 'battery_chemistry', type: 'choice', value: bc.setting, options: CHEM, readOnly: ro, cover: 'web.settings.car.lfp-battery',
        help: 'LFP packs like a full charge now and then and read flat in the middle; nickel packs (NCA, NMC) last longer kept lower. Battery and charges use this for their advice. Auto follows the evidence.',
        save: async (v) => { await patchSettings({ battery_chemistry: { setting: v } }); paintBattery(); } },
    ]);
    replace(slots.battery, section({ title: 'battery' }, lines.el, kv([
      ['in use', eff ? `${CHEM_WORDS[eff]}${bc.setting === 'auto' ? ' (auto)' : ' (set by hand)'}` : 'not known yet'],
      ['detected', bc.detected ? CHEM_WORDS[bc.detected] : 'not yet: a few charges and some driving are needed'],
      bc.evidence ? ['evidence', bc.evidence] : null,
      b?.brick_spread_v != null ? ['cell spread now', format(b.brick_spread_v, 'voltage', { digits: 3 })] : null,
      b?.capacity?.full_pack_kwh != null ? ['full pack', format(b.capacity.full_pack_kwh, 'energy')] : null,
      bc.setting !== 'auto' && bc.detected && bc.detected !== bc.setting ? ['note', `the car's data suggests ${CHEM_WORDS[bc.detected]}`, { tone: 'warn' }] : null,
    ])));
  }

  /* ---- reference values */
  function paintRefs() {
    const s = S.settings;
    const shown = (v, kind) => (v == null ? '' : String(Math.round(convert(v, kind) * 10) / 10));
    // what the car's build gives when nothing is typed (shown as the empty input's placeholder)
    const DEF = { capacity_new_kwh: 'capacity_new_kwh_default', full_range_new_km: 'full_range_new_km_default', efficiency_wh_per_km_override: 'efficiency_wh_per_km_default' };
    const ph = (k, kind) => { const d = s[DEF[k]]; return d == null ? undefined : { placeholder: kind ? shown(d, kind) : String(d) }; };
    const num = (k, label, kind, unit, lo, hi, help, required = false) => {
      const l = kind ? Math.ceil(convert(lo, kind) * 10) / 10 : lo, u = kind ? Math.floor(convert(hi, kind) * 10) / 10 : hi;
      return { key: k, type: 'number', value: kind ? shown(s[k], kind) : s[k], toText: (v) => (v == null ? '' : String(v)), unit, cls: 'ke--short', readOnly: ro, attrs: ph(k, kind),
        help: `${help}${ph(k, kind) ? ` The car's build gives ${ph(k, kind).placeholder} ${unit}.` : ''} ${l} to ${u} ${unit}${required ? '' : ', or empty'}.`,
        parse: (t) => {
          if (t.trim() === '') return required ? { error: `a number from ${l} to ${u}` } : { value: null };
          const n = Number(t);
          return Number.isFinite(n) && n >= l && n <= u ? { value: n } : { error: `a number from ${l} to ${u}${required ? '' : ', or empty'}` };
        },
        save: async (v) => { await patchSettings({ [k]: v == null ? null : kind ? Math.round(toMetric(v, kind) * 10) / 10 : v }); return undefined; },
        label };
    };
    const pk = S.battery?.pack;
    const detectedPack = () => (!s.pack_replaced_date && pk?.source === 'detected' && pk.replaced_ms != null ? `Detected from the pack's readings: ${dayKey(pk.replaced_ms, tz())}. ` : '');
    const lines = editLines([
      num('capacity_new_kwh', 'capacity when new', null, 'kWh', 10, 250, 'Empty: taken from the pack\'s first readings.'),
      num('full_range_new_km', 'full range when new', 'distance', unitLabel('distance'), 100, 1500, 'Empty: taken from the first 30 days.'),
      num('warranty_threshold_pct', 'warranty threshold', null, '%', 50, 95, 'The share of the capacity when new the warranty promises.', true),
      { key: 'pack_replaced_date', type: 'date', value: s.pack_replaced_date || '', readOnly: ro, label: 'pack replaced on',
        help: `${detectedPack()}The day a new pack went in: from then on the battery figures cover the new pack. Empty: not replaced${detectedPack() ? ' (or as detected)' : ''}.`,
        save: (v) => patchSettings({ pack_replaced_date: v || null }) },
      num('pack_new_kwh', 'new pack capacity when new', null, 'kWh', 10, 250, 'The usable capacity of the replacement pack when new.'),
      num('efficiency_wh_per_km_override', 'consumption factor', 'efficiency', unitLabel('efficiency'), 60, 500, 'Turns range into energy on efficiency. Empty: worked out from energy and range.'),
      { key: 'software_version_typed', type: 'text', value: s.software_version_typed || '', readOnly: ro, attrs: { placeholder: '2026.38.4' },
        help: 'Only for a car whose bus does not send it. Marked as typed on software. Empty: what the car reports.',
        save: (v) => patchSettings({ software_version_typed: v }) },
    ]);
    replace(slots.refs, section({ title: 'reference values', side: h('span', { class: 'muted' }, 'empty means worked out') }, lines.el,
      note('Battery health compares today\'s capacity and range with these; without them it uses the earliest readings and says so.')),
    tyreSection());
  }

  /* ---- the recommended tyre pressures, typed (config.tyre_pressure_front_kpa / _rear_kpa, kPa) */
  function tyreSection() {
    const cfg = S.car.config || {};
    const unit = unitLabel('pressure');
    const r2 = (x) => Math.round(x * 100) / 100;
    const lo = Math.ceil(convert(100, 'pressure') * 100) / 100, hi = Math.floor(convert(600, 'pressure') * 100) / 100;
    const line = (k, label, help) => {
      const cur = cfg[k]?.value;
      return { key: k, type: 'number', value: cur == null ? '' : String(r2(convert(cur, 'pressure'))), toText: (v) => (v == null ? '' : String(v)), unit, cls: 'ke--short', readOnly: ro,
        help: `${help} ${lo} to ${hi} ${unit}, or empty.`,
        parse: (t) => {
          if (t.trim() === '') return { value: null };
          const n = Number(t);
          return Number.isFinite(n) && n >= lo && n <= hi ? { value: n } : { error: `a number from ${lo} to ${hi}, or empty` };
        },
        save: async (v) => { await patchCar({ config: { [k]: v == null ? null : Math.round(toMetric(v, 'pressure') * 10) / 10 } }); return undefined; },
        label };
    };
    const lines = editLines([
      line('tyre_pressure_front_kpa', 'recommended, front', 'The cold pressure on the door placard. Empty: what the car sends, else the placard\'s for its wheel size.'),
      line('tyre_pressure_rear_kpa', 'recommended, rear', 'Empty: the front\'s when that is typed.'),
    ]);
    return section({ title: 'tyre pressure', side: h('span', { class: 'muted' }, 'typed beats the car') }, lines.el,
      note('A tyre more than 10 % under its recommended pressure is flagged on the car page.'));
  }

  /* ---- free Supercharging as dated ranges */
  function paintFree() {
    let ranges = (S.settings.free_supercharging || []).map((r) => ({ ...r }));
    const access = S.car.config?.supercharging_access;
    const box = h('div', { class: 'ke-list' });
    const msg = h('p', { class: 'ke-msg', role: 'alert', hidden: true });
    const say = (t) => { msg.hidden = !t; replace(msg, t || ''); };
    const days = (a, b) => { const [y1, m1, d1] = a.split('-').map(Number), [y2, m2, d2] = b.split('-').map(Number); return Math.round((Date.UTC(y2, m2 - 1, d2) - Date.UTC(y1, m1 - 1, d1)) / 86400000) + 1; };
    function problem(rows) {
      for (const [i, r] of rows.entries()) {
        if (!r.from_date && r.to_date) return `range ${i + 1}: give the first free day`;
        if (r.from_date && r.to_date && r.to_date < r.from_date) return `range ${i + 1}: the last day is before the first day`;
      }
      const o = rows.map((r, i) => ({ ...r, i })).filter((r) => r.from_date).sort((a, b) => (a.from_date < b.from_date ? -1 : 1));
      for (let j = 1; j < o.length; j++) if (!o[j - 1].to_date || o[j].from_date <= o[j - 1].to_date) return `ranges ${o[j - 1].i + 1} and ${o[j].i + 1} share days; a day can be in one range only`;
      return null;
    }
    async function save(rows) {
      const bad = problem(rows);
      if (bad) { say(bad); return; }
      say(null);
      try {
        await patchSettings({ free_supercharging: rows.filter((r) => r.from_date).map((r) => ({ from_date: r.from_date, to_date: r.to_date || null })) });
        toast('Free Supercharging saved.');
      } catch (e) { say(errMsg(e)); }
    }
    function rows() {
      replace(box, ranges.length ? ranges.map((r, i) => {
        const from = h('input', { class: 'in in--date', type: 'date', value: r.from_date || '', disabled: ro, 'aria-label': `range ${i + 1}: first day` });
        const to = h('input', { class: 'in in--date', type: 'date', value: r.to_date || '', disabled: ro, 'aria-label': `range ${i + 1}: last day, empty for no end` });
        const len = h('span', { class: 'muted' }, !r.from_date ? 'new range' : r.to_date ? (r.to_date < r.from_date ? 'last day before first' : `${days(r.from_date, r.to_date)} days`) : 'no end');
        const change = () => { ranges[i] = { from_date: from.value || null, to_date: to.value || null }; save(ranges); rows(); };
        from.addEventListener('change', change); to.addEventListener('change', change);
        return h('div', { class: 'ke', dataset: { row: '' } }, h('span', { class: 'ke-k' }, `free[${i + 1}]`), h('span', { class: 'ke-eq' }, '='),
          h('div', { class: 'ke-v range-line' }, from, h('span', { class: 'muted' }, '..'), to, len),
          h('span', { class: 'ke-side' }, ro ? null : btn('remove', () => { const had = ranges[i]?.from_date; ranges = ranges.filter((_, j) => j !== i); rows(); if (had) save(ranges); }, { kind: 'quiet' })));
      }) : empty('no free ranges', 'Every Supercharger session keeps the price it was billed or priced at.'));
    }
    rows();
    replace(slots.free, section({ title: 'free supercharging', cover: 'web.settings.car.free-supercharging', side: ro ? null : btn('add a range', () => { ranges = [...ranges, { from_date: null, to_date: null }]; rows(); }) },
      h('p', { class: 'note' }, 'Supercharger sessions that start on a free day cost nothing; sessions outside keep their own cost. Days are the car\'s days, in its time zone. ',
        access?.value != null ? ['The car reports supercharging access: ', String(access.value), ' ', srcTag(access), '. That says what it may do now, not since when.'] : 'The car has not reported its supercharging access yet.'),
      box, msg));
  }

  repaint();
  paint(ctx, head(ctx, { title: 'Settings: car' }),
    slots.top,
    h('p', { class: 'note', cover: 'web.settings.car.autosave' }, ro ? 'These settings are changed by an admin; you can read them here.' : 'Each line saves on its own: Enter saves, Esc undoes. There is no save button for the page.'),
    slots.car, slots.sources, slots.sold, slots.tz, slots.battery, slots.refs, slots.free, slots.uploads);
  ctx.events(['fleet'], (ev) => { if (ev.data?.car === cid && ((ev.data?.change === 'renamed' && ev.data.name !== S.car.name) || ev.data?.change === 'sold')) load().then(() => { if (ctx.alive()) repaint(); }).catch(() => {}); });
  await Promise.all(uploadTables.map((t) => t.ready));
  if (ctx.alive()) ctx.ready();
}
