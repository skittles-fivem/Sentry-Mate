// Sentry Mate, "Pebble" UI pack: Settings: Cars (/settings/cars and /settings/cars/{car}).
//
// Per-car settings, and how each car's CAN Bridge is tied to it, as rounded tiles of grouped rows. For a car: its
// name and what it reports about itself with where each detail comes from (GET / PATCH /api/v1/cars/{car}), how
// its uploads are doing with the next step (health, upload tokens, CAN Bridge keys), its tokens (made here, the
// secret shown once in a sheet), the keys tied to it with a preferred one, keys no car has yet, its time zone, its
// battery chemistry with the evidence, free Supercharging as dated ranges, and its data sources (#data-sources: the
// live inputs in priority order, dragged or moved with arrow buttons, and gap filling from imported history)
// (GET / PATCH .../settings). Every
// change saves at once with the object's version and a toast confirms it. Without a car: every car, the keys
// waiting for a car, tokens for a new car, the time zone a new car starts in (#default-tz: the server's
// default_time_zone, PATCH /api/v1/settings with its version; an admin's, others read), and how a car appears.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { convert, toMetric, unitLabel } from '../shared/units.js';
import { isValidZone, formatDate, formatDateTime, formatTime, formatDayKey, zoneSourceText } from '../shared/time.js';
import { tile, btn, chip, note, icon, stateTag, ago } from '../core/ui.js';
import { srcPill, srcText } from '../core/source.js';
import { emptyTile } from '../core/states.js';
import { openSheet, confirmSheet, sheetRow } from '../core/sheet.js';
import { field, segmented, toggle, setFieldError } from '../core/form.js';
import { remount } from '../core/router.js';
import { checkRow, moreButton } from '../core/bits.js';
import { group, infoRow, said, notSaved } from '../core/settings.js';
import { inOrder } from '../core/settings.js';
import { copyButton } from './move-parts.js';

export const id = 'settings-cars';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h3'], [2, 'h3'], [2, 'h2'], [2, 'h2']];
export const title = (ctx) => (ctx.car ? `${ctx.car.name}: settings` : 'Cars');

const CHEM = { auto: 'Automatic', lfp: 'LFP (lithium iron phosphate)', nca_nmc: 'NCA / NMC (nickel based)' };
const SOURCE = { can: 'From the car', vin: 'From the VIN', owner: 'Typed by an admin', teslamate: 'Imported', tesla_api: 'Tesla API' };
const SOURCE_TONE = { can: 'teal', vin: 'sky', owner: 'coral', teslamate: 'sun', tesla_api: 'sky' };
const CONFIG = [['trim', 'Trim'], ['drivetrain', 'Drivetrain'], ['performance_package', 'Performance package'], ['pack_kwh_code', 'Pack size code'],
  ['wheel', 'Wheels'], ['paint', 'Paint'], ['roof', 'Roof'], ['spoiler', 'Spoiler'], ['right_hand_drive', 'Right-hand drive'], ['map_region', 'Map region'], ['supercharging_access', 'Supercharging']];
const FIX = { no_token: 'No upload token has been made yet.', token_unused: 'A token was made but no CAN Bridge has uploaded with it yet.', key_waiting: 'A CAN Bridge key waits for a car (below).', logs_waiting_trust: 'Some logs wait for a trust check.' };
const yesNo = (v) => (v === true ? 'Yes' : v === false ? 'No' : v == null || v === '' ? '—' : String(v));
const settingsUrl = (car) => state.buildUrl('settings-cars', car ? { car } : {});
/** The four live inputs, highest priority first, when the server sends no order of its own. */
const SRC_ORDER = ['smo', 'tesla_api', 'teslafi_api', 'teslamate_local'];
const SRC_NAME = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'local TeslaMate' };
const cap = (t) => String(t).charAt(0).toUpperCase() + String(t).slice(1);
const onOff = (v) => (v === true ? 'on' : v === false ? 'off' : 'not known');
const sameList = (a, b) => Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((x, i) => x === b[i]);
const day = (ms, tz) => (ms == null ? 'never' : formatDate(ms, tz, { now: api.now() }));

function crumb(car) {
  return h('nav', { class: 'crumb', 'aria-label': 'Breadcrumb', cover: 'web.settings.car.breadcrumb' },
    h('a', { href: '/', class: 'place-link' }, 'Fleet'), icon('chevronRight'),
    car ? h('a', { href: settingsUrl(null), class: 'place-link' }, 'Cars') : h('span', { 'aria-current': 'page' }, 'Cars'),
    car ? [icon('chevronRight'), h('span', { 'aria-current': 'page' }, car.name)] : null);
}

/** The steps by which a car appears. */
const howSteps = () => h('ol', { class: 'set-list' },
  h('li', null, h('b', null, 'Make an upload token. '), 'An admin presses “Make an upload token”. It shows the three fields for the app, ready to copy.'),
  h('li', null, h('b', null, 'Enter it in the CAN Bridge app '), 'on the Screenmate One (or another logger, such as a Raspberry Pi), then press Apply & restart.'),
  h('li', null, h('b', null, 'Wait for the first log. '), 'The car appears by itself, with its model and year from the VIN prefix. No Tesla account is needed.'),
  h('li', null, h('b', null, 'A second logger in the same car '), 'uploads with a new key. It waits under “Keys waiting for a car” until you tie it to the car.'));

/** The three fields of the CAN Bridge app for a token just made, each with Copy. Shown this once. */
function tokenSheet(made, forWhat) {
  const app = made.app || {};
  const row = (name, sub, value) => h('div', { class: 'row row-static set-ctl is-stack' },
    h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, name), h('span', { class: 'row-sub' }, sub)),
    value ? [h('code', { class: 'set-mono' }, value), copyButton(() => value)] : h('span', { class: 'row-sub' }, 'The server did not send it: see Settings, Server.'));
  return openSheet({
    title: `Upload token for ${forWhat}`,
    body: [
      h('p', { class: 'sheet-text' }, 'This is the only time the token is shown. Put these three into the CAN Bridge app now; a lost token is revoked and made again.'),
      group(null, [
        row('Server address', 'Where the bridge sends its logs.', app.address || null),
        row('Token', 'The secret that lets this bridge upload.', app.token || made.secret || null),
        row('Certificate SHA-256', 'So the bridge knows it talks to this server.', app.certificate_sha256 || null),
      ]),
      h('ol', { class: 'set-list set-sheet-form' },
        h('li', null, 'On the Screenmate One, open the CAN Bridge app.'),
        h('li', null, 'In its “Second Sentry Six Manager” section, fill in the three fields.'),
        h('li', null, 'Press Apply & restart. The token ties itself to the bridge’s key on the first upload.')),
    ],
    actions: [{ label: 'I have copied it', value: true, kind: 'primary' }],
  });
}

/** Find any car by name, model or VIN prefix in a sheet. Resolves with the car, or null. */
function pickCar(ctx, titleText) {
  let chosen = null;
  return openSheet({
    title: titleText,
    body: (close) => {
      const input = h('input', { class: 'input', type: 'search', placeholder: 'Name, model or VIN prefix', 'aria-label': 'Find the car', autocomplete: 'off', 'data-autofocus': '' });
      const list = h('div', { class: 'rows-list', 'aria-live': 'polite' });
      let seq = 0, timer = 0;
      const find = async () => {
        const n = ++seq;
        try {
          const pg = await ctx.api.page('cars', { sort: 'custom', search: input.value.trim() || undefined }, { limit: 20 });
          if (n !== seq) return;
          list.replaceChildren(...((pg.items || []).length ? pg.items.map((c) => sheetRow({ icon: 'car', name: c.name || c.id, sub: [c.model, c.year, c.vin_prefix].filter(Boolean).join(' · '), onclick: () => { chosen = c; close('picked'); } }))
            : [h('p', { class: 'list-empty' }, 'No car matches. Try part of the name, the model or the VIN prefix.')]));
        } catch (err) { if (n === seq) list.replaceChildren(h('p', { class: 'list-empty' }, `The cars could not be loaded: ${err.message}`)); }
      };
      input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(find, 200); });
      find();
      return [h('label', { class: 'search' }, icon('search'), input), h('div', { class: 'rows' }, list)];
    },
  }).then((v) => (v === 'picked' ? chosen : null));
}

/** Rows of keys no car has yet, each with a way to tie it to a car. */
async function waitingRows(ctx, { car = null, onChange }) {
  const keys = await ctx.api.list('can-keys', { assigned: 'false' }).all({ max: 200 });
  const tie = async (k, c) => {
    const ok = await confirmSheet({ title: `Tie key ${k.key} to ${c.name}?`, text: `Its ${k.logs_waiting || 0} waiting logs and every later upload from it become ${c.name}’s.`, yes: `Tie to ${c.name}` });
    if (!ok) return;
    try { await ctx.api.patch(api.apiPath`can-keys/${k.key}`, { car: c.id }); api.invalidate(''); ctx.toast(`Key ${k.key} is now ${c.name}’s.`); await onChange?.(); } catch (err) { ctx.toast(notSaved(err), { kind: 'error' }); }
  };
  return {
    count: keys.length,
    rows: keys.map((k) => infoRow({ icon: 'key', tone: 'sun', name: k.device || 'Logger not named', sub: `${k.key} · first ${day(k.first_seen_ms, ctx.tz)} · ${k.logs_waiting || 0} logs kept`,
      side: ctx.admin ? btn(car ? `Tie to ${car.name}` : 'Choose the car', { kind: 'soft', icon: car ? 'check' : 'search', cls: 'btn-sm', onclick: async () => {
        const c = car || await pickCar(ctx, `Tie key ${k.key} to a car`);
        if (c) tie(k, c);
      } }) : null })),
  };
}

/** Tokens for a car not seen yet (admins). */
async function newCarTile(ctx, redraw) {
  const toks = await ctx.api.list('upload-tokens', {}).all({ max: 100 });
  const make = async () => {
    let made;
    try { made = await ctx.api.post('upload-tokens', {}); } catch (err) { ctx.toast(`No token made: ${err.message}`, { kind: 'error' }); return; }
    await tokenSheet(made, 'a new car');
    redraw();
  };
  const revoke = async (t) => {
    if (!await confirmSheet({ title: `Revoke token ${t.id}?`, text: 'A CAN Bridge that uses it can no longer upload.', yes: 'Revoke', danger: true })) return;
    try { await ctx.api.del(api.apiPath`upload-tokens/${t.id}`); ctx.toast(`Token ${t.id} revoked.`); } catch (err) { ctx.toast(notSaved(err), { kind: 'error' }); }
    redraw();
  };
  return tile({ w: 2, title: 'Tokens for a new car', icon: 'plug', side: btn('Make a token', { kind: 'primary', icon: 'plus', cls: 'btn-sm', onclick: make }) },
    note('For a car Sentry Mate has not seen yet. Its first log makes the car here; the token then moves to that car.'),
    toks.length ? group(null, toks.map((t) => infoRow({ icon: 'key', name: t.id, sub: `Made ${day(t.created_ms, ctx.tz)}`, side: t.revoked ? chip('Revoked', { tone: 'muted' }) : [chip('Waiting for the first upload', { tone: 'sun' }), btn(`Revoke ${t.id}`, { kind: 'ghost', icon: 'close', iconOnly: true, cls: 'btn-sm', onclick: () => revoke(t) })] })))
      : said('info', 'No token waits for a new car.'));
}

/** Every car the account sees, searched by the server. */
function carsTile(ctx, current) {
  const list = h('div', { class: 'rows-list' });
  const input = h('input', { class: 'input', type: 'search', placeholder: 'Name, model or VIN prefix', 'aria-label': 'Find a car', autocomplete: 'off' });
  let pager = null, timer = 0;
  // sold cars wait behind 'Sold cars (n)' (open when the car shown is sold); a search shows them, marked Sold
  let showSold = !!(current && current.sold);
  const rowOf = (c) => sheetRow({ href: settingsUrl(c.id), icon: 'car', current: current && c.id === current.id, name: c.name || c.id,
    sub: [c.model, c.year, c.vin_prefix].filter(Boolean).join(' · ') || 'Model not known yet', side: c.sold ? chip('Sold', { tone: 'muted' }) : c.now ? stateTag(c.now.state, { stale: c.now.stale }) : null });
  const soldBtn = btn('Sold cars', { kind: 'ghost', icon: 'chevronDown', cls: 'btn-sm', onclick: () => { showSold = !showSold; draw(); } });
  const draw = () => {
    const q = input.value.trim();
    const { active, sold } = q ? { active: pager.items, sold: [] } : state.splitSold(pager.items);
    const rows = [...active, ...(showSold ? sold : [])].map(rowOf);
    list.replaceChildren(...(rows.length || sold.length ? rows : [h('p', { class: 'list-empty' }, q ? `No car matches “${q}”.` : 'No car yet.')]));
    soldBtn.hidden = !sold.length;
    soldBtn.querySelector('.btn-text').textContent = `${showSold ? 'Hide sold cars' : 'Sold cars'} (${sold.length})`;
    soldBtn.querySelector('.ic')?.replaceWith(icon(showSold ? 'chevronUp' : 'chevronDown'));
    soldBtn.setAttribute('aria-expanded', String(showSold));
    more.querySelector('button').hidden = pager.done;
  };
  const more = moreButton(async () => { await pager.more(); draw(); return !pager.done; }, { text: 'More cars' });
  const load = async () => {
    pager = ctx.api.pager('cars', { sort: 'custom', search: input.value.trim() || undefined }, { limit: 12 });
    await pager.more();
    draw();
  };
  input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(load, 200); });
  ctx.onCleanup(() => clearTimeout(timer));
  const el = tile({ w: 2, title: 'Every car', icon: 'fleet', cover: 'web.settings.car.car-switcher' },
    h('label', { class: 'search' }, icon('search'), input), h('div', { class: 'rows' }, list), h('div', { class: 'form-actions' }, soldBtn), more);
  return { el, load };
}

export async function mount(ctx) {
  inOrder(ctx);
  return ctx.car ? mountCar(ctx) : mountIndex(ctx);
}

/* ------------------------------------------------------------------ /settings/cars */

/**
 * The zone a new car starts in (#default-tz): the server's default_time_zone. An admin changes it (PATCH /settings
 * with its version); others read it. Each car keeps its own zone after that. -> { el, ready }
 */
function defaultZoneTile(ctx) {
  const off = !ctx.admin;
  const zones = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
  const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
  const el = tile({ w: 2, title: 'Time zone for new cars', icon: 'clock', id: 'default-tz', side: off ? chip('Read only', { tone: 'muted', icon: 'lock' }) : null });
  const body = h('div', { class: 'contents' });
  el.append(body);
  let srv = null;
  function draw() {
    if (!srv) { body.replaceChildren(note('The server settings could not be read.')); return; }
    const z = srv.default_time_zone || 'UTC';
    const f = field({ label: 'Time zone for new cars', name: 'default_time_zone', value: z, hint: 'A name such as America/Chicago. Type to see the list.', attrs: { list: 'sc-default-zones', spellcheck: 'false', autocomplete: 'off', disabled: off } });
    const save = async (v) => {
      setFieldError(f, null);
      if (!v || !isValidZone(v)) { setFieldError(f, 'Not a known time zone. Pick one from the list, for example America/Chicago.'); return; }
      if (v === srv.default_time_zone) return;
      try {
        srv = await ctx.api.patch('settings', { version: srv.version, default_time_zone: v });
        ctx.toast(`New cars start in ${v.replace(/_/g, ' ')}. Saved for everyone on this server.`);
      } catch (err) {
        if (err && (err.status === 409 || err.code === 'stale_version')) {
          srv = await ctx.api.get('settings').catch(() => srv);
          ctx.toast('Someone changed these settings meanwhile; they are shown fresh now. Make your change again.', { kind: 'info' });
        } else { setFieldError(f, (err.fields && err.fields.default_time_zone) || notSaved(err)); return; }
      }
      if (ctx.alive()) draw();
    };
    f.control.addEventListener('change', () => save(f.control.value.trim()));
    body.replaceChildren(f, h('datalist', { id: 'sc-default-zones' }, zones.map((x) => h('option', { value: x }))),
      browserZone && browserZone !== z && !off ? h('div', { class: 'form-actions' }, btn(`Use ${browserZone.replace(/_/g, ' ')}`, { kind: 'soft', icon: 'pin', onclick: () => { f.control.value = browserZone; save(browserZone); } })) : null,
      note('The zone a new car starts in: its days, weeks and prices by time of day. Each car keeps its own after that: open a car to change it.'));
  }
  const ready = ctx.api.get('settings').then((x) => { srv = x; }).catch(() => null).then(() => { if (ctx.alive()) draw(); });
  return { el, ready };
}

async function mountIndex(ctx) {
  const zone = defaultZoneTile(ctx);
  const status = await ctx.api.get('setup/status');
  if (!ctx.alive()) return {};
  const bar = h('div', { class: 'page-bar w-full' }, crumb(null));
  const how = tile({ w: 2, title: 'How a new car appears', icon: 'spark', cover: 'web.settings.car.empty-state' }, howSteps());
  const tiles = [bar];
  if (!status.cars) {
    const fixes = (status.fixes || []).filter((f) => FIX[f.cause]);
    tiles.push(emptyTile({ icon: 'car', title: 'No car yet', text: 'Sentry Mate needs no Tesla account. A car appears when its CAN Bridge uploads its first log.', cover: 'web.settings.car.empty-state' }),
      how, fixes.length ? tile({ w: 2, title: 'Now', icon: 'info' }, group(null, fixes.map((f) => infoRow({ icon: 'warning', tone: 'sun', name: FIX[f.cause] })))) : null);
  } else {
    const cars = carsTile(ctx, null);
    await cars.load();
    tiles.push(cars.el, tile({ w: 2, title: 'Choose a car', icon: 'car', cover: 'web.settings.car.autosave' },
      h('p', { class: 'lede' }, 'Pick a car to change its settings.'),
      note('Its name, time zone, battery chemistry, free Supercharging, and how its CAN Bridge is tied to it. Every change saves at once.')), how);
  }
  if (ctx.admin) {
    tiles.push(await newCarTile(ctx, () => remount()));
    const w = await waitingRows(ctx, { onChange: () => remount() });
    tiles.push(tile({ w: 2, title: 'Keys waiting for a car', icon: 'key' }, w.count ? group(null, w.rows) : said('ok', 'No key is waiting: every CAN Bridge that uploads belongs to a car.')));
  }
  tiles.push(zone.el);
  await zone.ready;
  if (!ctx.alive()) return {};
  ctx.view.replaceChildren(...tiles.filter(Boolean));
  ctx.ready();
  return {};
}

/* ------------------------------------------------------------------ /settings/cars/{car} */

async function mountCar(ctx) {
  const admin = ctx.admin;
  const ro = !admin;
  const car = ctx.car;
  const carPath = api.apiPath`cars/${car.id}`;
  const setPath = api.apiPath`cars/${car.id}/settings`;
  let settings = await ctx.api.get(setPath);
  if (!ctx.alive()) return {};
  let carVersion = car.version;

  /** Save the car (name, zone, sold) with its version; someone else's change in between shows theirs. */
  async function patchCar(body, what) {
    try {
      await ctx.api.patch(carPath, { version: carVersion, ...body });
      api.invalidate('');
      ctx.toast(`Saved: ${what}.`, { next: true });
      remount();
    } catch (err) {
      if (err && err.code === 'stale_version') { api.invalidate(''); ctx.toast('Someone else changed this car at the same time; the page shows their change now. Make yours again.', { kind: 'info', next: true }); remount(); return; }
      throw err;
    }
  }
  async function patchSettings(body) {
    try { settings = await ctx.api.patch(setPath, { version: settings.version, ...body }); } catch (err) {
      if (!err || err.code !== 'stale_version') throw err;
      api.invalidate(setPath);
      settings = await ctx.api.get(setPath);
      throw Object.assign(new Error('Someone else changed this at the same time; their change is shown now.'), { code: 'conflict' });
    }
    api.invalidate(carPath);
    return settings;
  }

  /* ---------- the car */
  const nameF = field({ label: 'Name', name: 'name', value: car.name, hint: 'As it shows on every page and on MQTT. Sentry Mate never asks Tesla for it.', attrs: { maxlength: '64', autocomplete: 'off', disabled: ro } });
  nameF.control.addEventListener('change', async () => {
    const v = nameF.control.value.trim();
    setFieldError(nameF, null);
    if (!v) { setFieldError(nameF, 'A car needs a name.'); return; }
    if (v === car.name) return;
    try { await patchCar({ name: v }, `the car is now called ${v}`); } catch (err) { setFieldError(nameF, (err.fields && err.fields.name) || err.message); }
  });
  nameF.control.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); nameF.control.blur(); } });
  const cfg = car.config || {};
  // the recommended tyre pressures, typed (config.tyre_pressure_front_kpa / _rear_kpa, kPa)
  const pu = unitLabel('pressure');
  const r2 = (x) => Math.round(x * 100) / 100;
  const pLo = Math.ceil(convert(100, 'pressure') * 100) / 100, pHi = Math.floor(convert(600, 'pressure') * 100) / 100;
  const tyreF = (k, label, hint) => {
    const cur = cfg[k] ? cfg[k].value : null;
    const f = field({ label: `${label} (${pu})`, name: k, type: 'number', value: cur == null ? '' : String(r2(convert(cur, 'pressure'))), hint: `${pLo} to ${pHi} ${pu}. ${hint}`,
      attrs: { min: String(pLo), max: String(pHi), step: pu === 'psi' ? '1' : '0.05', disabled: ro } });
    f.control.addEventListener('change', async () => {
      const raw = f.control.value.trim();
      const n = raw === '' ? null : Number(raw);
      setFieldError(f, null);
      if (n != null && (!Number.isFinite(n) || n < pLo || n > pHi)) { setFieldError(f, `A number from ${pLo} to ${pHi} ${pu}, or empty.`); return; }
      try { await patchCar({ config: { [k]: n == null ? null : Math.round(toMetric(n, 'pressure') * 10) / 10 } }, n == null ? `${label.toLowerCase()} is no longer typed` : `${label.toLowerCase()} ${n} ${pu}`); }
      catch (err) { setFieldError(f, (err.fields && err.fields[`config.${k}`]) || notSaved(err)); }
    });
    return f;
  };
  const carT = tile({ w: 2, title: 'The car', icon: 'car', side: chip(ro ? 'Read only' : 'Saves as you go', { tone: ro ? 'muted' : 'teal', icon: ro ? 'lock' : 'check', cover: 'web.settings.car.autosave' }) },
    nameF,
    group('What it reports about itself', [
      infoRow({ name: 'Model', sub: car.vin_prefix ? `VIN prefix ${car.vin_prefix}: only the prefix is kept` : null, side: h('span', { class: 'set-value' }, [car.model, car.year].filter(Boolean).join(' ') || 'Not known yet') }),
      ...CONFIG.map(([k, label]) => {
        const c = cfg[k];
        return infoRow({ name: label, sub: c && c.value != null ? null : 'Not sent yet', side: h('span', { class: 'set-line' },
          h('span', { class: 'set-value' }, c && c.value != null ? yesNo(c.value) : '—'), c && c.value != null ? (c.source === 'vin' ? chip(SOURCE.vin, { tone: SOURCE_TONE.vin }) : srcPill(c, { short: true, typed: 'Typed by an admin' })) : null) });
      }),
    ], { note: 'Read from the car’s own configuration message (SMO) or from the Tesla API, else worked out from the VIN prefix or typed by an admin. The label says where each one came from.' }),
    tyreF('tyre_pressure_front_kpa', 'Recommended tyre pressure, front', 'The cold pressure on the door placard. Empty: what the car sends, else the placard’s for its wheel size.'),
    tyreF('tyre_pressure_rear_kpa', 'Recommended tyre pressure, rear', 'Empty: the front’s when that is typed.'),
    note('A typed pressure beats the one the car sends. A tyre more than 10 % under it is flagged on the car page.'));

  /* ---------- sold */
  // PATCH cars/{car} { version, sold, sold_date }: a sold car keeps its history; sold: false forgets the date.
  const soldWords = car.sold ? (car.sold_date ? `Sold on ${formatDayKey(car.sold_date, { weekday: false })}` : 'Sold, no date given') : 'Not sold';
  const soldF = field({ label: car.sold ? 'Sold on' : 'Sold on (optional)', name: 'sold_date', type: 'date', value: car.sold_date || '', hint: car.sold ? 'Change it, or empty it to keep no date.' : 'Leave it empty when the date is not known.' });
  const saveSold = async (sold, date) => {
    setFieldError(soldF, null);
    try {
      await patchCar(sold ? { sold: true, sold_date: date || null } : { sold: false }, sold ? (date ? `${car.name} was sold on ${date}` : `${car.name} is marked sold`) : `${car.name} is not sold`);
    } catch (err) { setFieldError(soldF, (err.fields && err.fields.sold_date) || notSaved(err)); }
  };
  if (car.sold) soldF.control.addEventListener('change', () => { if ((soldF.control.value || null) !== (car.sold_date || null)) saveSold(true, soldF.control.value); });
  const soldT = tile({ w: 2, title: 'Sold', icon: 'flag', side: car.sold ? chip('Sold', { tone: 'muted' }) : null },
    group(null, [infoRow({ icon: car.sold ? 'check' : 'car', tone: car.sold ? 'sun' : 'teal', name: soldWords,
      sub: car.sold ? 'Its history stays and its pages stay open. It is left out of the fleet and raises nothing that needs a look.' : 'Still in the fleet.' })]),
    admin ? soldF : null,
    admin ? h('div', { class: 'form-actions' }, car.sold
      ? btn('Mark as not sold', { kind: 'soft', icon: 'reset', onclick: () => saveSold(false) })
      : btn('Mark as sold', { kind: 'primary', icon: 'check', onclick: async () => {
        if (!await confirmSheet({ title: `Mark ${car.name} as sold?`, text: 'It keeps all its history and stays browsable. The fleet leaves it out, and the Tesla API and TeslaFi store nothing more for it. You can undo this.', yes: 'Mark as sold' })) return;
        saveSold(true, soldF.control.value);
      } })) : note('Only an admin can mark a car as sold.'));

  /* ---------- uploads */
  let upT;
  if (!admin) {
    upT = tile({ w: 2, title: 'Uploads', icon: 'upload', cover: 'faq.car-missing.outcomes' },
      note('Only an admin sees upload tokens and CAN Bridge keys. How this car’s uploads are doing is on Health.'),
      h('a', { class: 'tile-link', href: ctx.link('health') }, 'Health', icon('chevronRight')));
  } else {
    const [hl, toks, keys, waiting] = await Promise.all([
      ctx.api.get(api.apiPath`cars/${car.id}/health`).catch(() => null),
      ctx.api.list(api.apiPath`cars/${car.id}/upload-tokens`, {}).all({ max: 100 }),
      ctx.api.list('can-keys', { car: car.id }).all({ max: 100 }),
      waitingRows(ctx, { car, onChange: () => remount() }),
    ]);
    if (!ctx.alive()) return {};
    const reasons = (hl && hl.reasons) || [];
    const active = toks.filter((t) => !t.revoked);
    const when = (ms) => formatDateTime(ms, ctx.tz, { now: api.now() });
    const makeToken = async () => {
      let made;
      try { made = await ctx.api.post(api.apiPath`cars/${car.id}/upload-tokens`, {}); } catch (err) { ctx.toast(`No token made: ${err.message}`, { kind: 'error' }); return; }
      await tokenSheet(made, car.name);
      remount();
    };
    const act = (fn, okText) => async () => { try { await fn(); ctx.toast(okText); remount(); } catch (err) { ctx.toast(notSaved(err), { kind: 'error' }); } };
    const checks = [
      !hl ? checkRow({ name: 'Uploads', sub: 'Could not be read just now; Health shows every upload.', word: 'Not known', level: 'off' })
        : reasons.includes('upload_silence') ? checkRow({ name: 'Uploads', sub: hl.last_upload_ms ? `Nothing since ${when(hl.last_upload_ms)}. The bridge may be off or out of reach; logs wait in the car.` : 'This car has not uploaded yet.', word: 'Silent', level: 'bad' })
          : checkRow({ name: 'Uploads', sub: `Last upload ${hl.last_upload_ms ? when(hl.last_upload_ms) : '—'}.`, word: 'Arriving', level: 'ok' }),
      hl ? checkRow({ name: 'Last log check', sub: reasons.includes('trust_failed') ? 'A log failed its signature check; its time shows as unknown, never as asleep.' : 'Signature and hash chain.', word: reasons.includes('trust_failed') ? 'Failed' : (hl.last_log_trust && hl.last_log_trust.result) === 'verified' ? 'Verified' : 'Pending', level: reasons.includes('trust_failed') ? 'bad' : 'ok' }) : null,
      checkRow({ name: 'Upload token', sub: active.length ? `${active.length} active.` : 'None active: the bridge cannot upload. Make one below.', word: active.length ? 'Ready' : 'Missing', level: active.length ? 'ok' : 'bad' }),
      ...active.filter((t) => !t.key && !t.last_upload_ms).map((t) => checkRow({ name: `Token ${t.id}`, sub: `Made ${day(t.created_ms, ctx.tz)}. Enter it in the CAN Bridge app; the first upload ties it.`, word: 'Not used', level: 'warn' })),
      waiting.count ? checkRow({ name: 'Keys no car has', sub: 'A logger the server does not know yet. If it is in this car, tie it below.', word: String(waiting.count), level: 'warn' }) : null,
      reasons.includes('rebuild_pending') ? checkRow({ name: 'History', sub: 'Being rebuilt; back in a few seconds.', word: 'Rebuilding', level: 'warn' }) : null,
    ];
    upT = tile({ w: 2, title: 'Uploads, tokens and keys', icon: 'upload', cover: 'faq.car-missing.outcomes', side: btn('Make a token', { kind: 'primary', icon: 'plus', cls: 'btn-sm', onclick: makeToken }) },
      h('ul', { class: 'checks' }, checks.filter(Boolean)),
      group('Upload tokens', toks.length ? toks.map((t) => infoRow({ icon: 'key', name: t.id, sub: `Made ${day(t.created_ms, ctx.tz)}${t.key ? ` · tied to ${t.key}` : ''}${t.last_upload_ms ? ` · last upload ${day(t.last_upload_ms, ctx.tz)}` : ''}`,
        side: t.revoked ? chip('Revoked', { tone: 'muted' }) : [chip(t.key || t.last_upload_ms ? 'In use' : 'Not used yet', { tone: t.key || t.last_upload_ms ? 'teal' : 'sun' }),
          btn(`Revoke ${t.id}`, { kind: 'ghost', icon: 'close', iconOnly: true, cls: 'btn-sm', onclick: async () => {
            if (!await confirmSheet({ title: `Revoke token ${t.id}?`, text: 'The CAN Bridge that uses it stops uploading at its next request. Logs it sent stay.', yes: 'Revoke', danger: true })) return;
            act(() => ctx.api.del(api.apiPath`cars/${car.id}/upload-tokens/${t.id}`), `Token ${t.id} revoked.`)();
          } })] })) : [infoRow({ name: 'No upload token yet', sub: 'Make one and enter it in the CAN Bridge app.' })],
      { note: 'A token lets one CAN Bridge upload. It is shown once, and ties itself to the bridge’s key on the first upload.' }),
      group('CAN Bridge keys', keys.length ? keys.map((k) => infoRow({ icon: 'chip', tone: k.preferred ? 'teal' : null, name: k.device || 'Logger not named', sub: `${k.key} · last upload ${day(k.last_upload_ms, ctx.tz)}${k.logs_waiting ? ` · ${k.logs_waiting} logs wait` : ''}`,
        side: [k.preferred ? chip('Preferred', { tone: 'teal' }) : btn('Prefer', { kind: 'soft', cls: 'btn-sm', onclick: act(() => ctx.api.patch(api.apiPath`can-keys/${k.key}`, { preferred: true }), `Key ${k.key} is preferred now.`) }),
          btn(`Untie ${k.key}`, { kind: 'ghost', icon: 'close', iconOnly: true, cls: 'btn-sm', onclick: async () => {
            if (!await confirmSheet({ title: `Untie key ${k.key}?`, text: 'Its new uploads wait for a car until it is tied again. History stays.', yes: 'Untie', danger: true })) return;
            act(() => ctx.api.patch(api.apiPath`can-keys/${k.key}`, { car: null }), `Key ${k.key} untied.`)();
          } })] })) : [infoRow({ name: 'No key tied yet', sub: 'A key ties itself on the first upload with one of this car’s tokens.' })],
      { note: 'Each logger signs with its own key. When two record at once, the preferred key wins and the other fills its gaps.' }),
      group('Keys waiting for a car', waiting.count ? waiting.rows : [infoRow({ name: 'No key is waiting' })]));
  }

  /* ---------- time zone */
  const zones = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
  const browserZone = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } })();
  const tzF = field({ label: 'Time zone', name: 'time_zone', value: car.time_zone || '', hint: 'A name such as Europe/Amsterdam. Type to see the list.', attrs: { list: 'sc-zones', spellcheck: 'false', autocomplete: 'off', disabled: ro } });
  const tzNow = h('p', { class: 'note' });
  const drawNow = () => {
    const z = car.time_zone;
    const src = zoneSourceText(car.time_zone_source);
    const from = src ? `${z.replace(/_/g, ' ')} (${src})${car.time_zone_source === 'owner' ? '' : ': type or pick a zone to set it for this car'}. ` : '';
    tzNow.textContent = z && isValidZone(z) ? `${from}Now there: ${formatTime(api.now(), z)}, ${formatDate(api.now(), z, { now: api.now() })}.` : 'Not set: times show in this browser’s zone.';
  };
  drawNow();
  const saveZone = async (v) => {
    setFieldError(tzF, null);
    if (!v || !isValidZone(v)) { setFieldError(tzF, 'Not a known time zone. Pick one from the list, for example Europe/Amsterdam.'); return; }
    if (v === car.time_zone && car.time_zone_source === 'owner') return;
    try { await patchCar({ time_zone: v }, `${car.name} now uses ${v.replace(/_/g, ' ')}`); } catch (err) { setFieldError(tzF, (err.fields && err.fields.time_zone) || err.message); }
  };
  tzF.control.addEventListener('change', () => saveZone(tzF.control.value.trim()));
  const tzT = tile({ w: 2, title: 'Time zone', icon: 'clock', cover: 'docs.env.core.tz' },
    tzF, h('datalist', { id: 'sc-zones' }, zones.map((z) => h('option', { value: z }))), tzNow,
    (car.time_zone_source === 'owner' || (browserZone && browserZone !== car.time_zone)) && !ro ? h('div', { class: 'form-actions' },
      car.time_zone_source === 'owner' ? btn('From the car’s position', { kind: 'soft', icon: 'pin', onclick: async () => {
        setFieldError(tzF, null);
        try { await patchCar({ time_zone: null }, `${car.name} takes its time zone from its position`); } catch (err) { setFieldError(tzF, (err.fields && err.fields.time_zone) || err.message); }
      } }) : null,
      browserZone && browserZone !== car.time_zone ? btn(`Use ${browserZone.replace(/_/g, ' ')}`, { kind: 'soft', icon: 'pin', onclick: () => { tzF.control.value = browserZone; saveZone(browserZone); } }) : null) : null,
    note('Days, weeks and months, and the hours of time-of-use prices, are counted in this zone. Times are stored in UTC, so a new zone regroups the history without changing it.'));

  /* ---------- battery */
  const chemNow = h('div', { class: 'contents' });
  const drawChem = () => {
    const c = settings.battery_chemistry || {};
    const inUse = c.setting && c.setting !== 'auto' ? c.setting : c.detected;
    chemNow.replaceChildren(group(null, [
      infoRow({ icon: 'battery', tone: 'teal', name: 'In use', sub: c.setting && c.setting !== 'auto' ? 'Set by an admin' : c.detected ? 'Detected' : 'The pack’s voltages need a few charges to tell', side: h('span', { class: 'set-value' }, inUse ? CHEM[inUse] : 'Not known yet') }),
      infoRow({ icon: 'search', name: 'Evidence', sub: c.evidence || 'Not given yet' }),
      c.setting && c.setting !== 'auto' && c.detected && c.detected !== c.setting ? infoRow({ icon: 'warning', tone: 'sun', name: 'The detected chemistry differs from the one set here.' }) : null,
    ]));
  };
  drawChem();
  const chem = segmented({ name: 'Battery chemistry', value: (settings.battery_chemistry || {}).setting || 'auto', options: [['auto', 'Automatic'], ['lfp', 'LFP'], ['nca_nmc', 'NCA / NMC']],
    onchange: async (v) => {
      try { await patchSettings({ battery_chemistry: { setting: v } }); ctx.toast(v === 'auto' ? 'Saved: the chemistry is worked out by itself.' : `Saved: ${CHEM[v]}.`); } catch (err) { ctx.toast(err.code === 'conflict' ? err.message : notSaved(err), { kind: 'error' }); }
      chem.set((settings.battery_chemistry || {}).setting || 'auto');
      drawChem();
    } });
  if (ro) chem.querySelectorAll('button').forEach((b) => { b.disabled = true; });
  const batT = tile({ w: 2, title: 'Battery', icon: 'battery', cover: 'web.settings.car.lfp-battery' },
    chem, chemNow,
    note('LFP packs are meant to charge to 100 % about once a week, nickel packs to about 80 % for every day; this sets the healthy band on the Battery page. Recording is not affected.'),
    h('a', { class: 'tile-link', href: ctx.link('battery') }, 'Battery', icon('chevronRight')));

  /* ---------- free Supercharging */
  const fsBox = h('div', { class: 'contents' });
  const fsNote = h('p', { class: 'form-error', role: 'status', hidden: true });
  let ranges = (settings.free_supercharging || []).map((r) => ({ from_date: r.from_date, to_date: r.to_date || null }));
  const rangeWords = (r) => (r.to_date ? `${formatDayKey(r.from_date, { weekday: false })} to ${formatDayKey(r.to_date, { weekday: false })}` : `from ${formatDayKey(r.from_date, { weekday: false })} on`);
  async function saveRanges() {
    fsNote.hidden = true;
    const bad = ranges.findIndex((r) => r.from_date && r.to_date && r.to_date < r.from_date);
    if (bad >= 0) { fsNote.textContent = 'Not saved: a range ends before it starts. Fix it, then it saves.'; fsNote.hidden = false; return; }
    const keep = ranges.filter((r) => r.from_date);
    try {
      await patchSettings({ free_supercharging: keep.map((r) => ({ from_date: r.from_date, to_date: r.to_date || null })) });
      ctx.toast(keep.length ? `Saved: ${keep.length} free Supercharging ${keep.length === 1 ? 'range' : 'ranges'}.` : 'Saved: no free Supercharging.');
      ranges = [...(settings.free_supercharging || []).map((r) => ({ ...r })), ...ranges.filter((r) => !r.from_date)];
    } catch (err) {
      if (err.code === 'conflict') ranges = (settings.free_supercharging || []).map((r) => ({ ...r }));
      fsNote.textContent = err.code === 'conflict' ? err.message : notSaved(err); fsNote.hidden = false;
    }
    drawRanges();
  }
  function drawRanges() {
    const rows = ranges.map((r, i) => {
      const from = field({ label: 'From', name: `fs-from-${i}`, type: 'date', value: r.from_date || '', attrs: { disabled: ro } });
      const to = field({ label: 'Until (empty: no end)', name: `fs-to-${i}`, type: 'date', value: r.to_date || '', attrs: { disabled: ro } });
      from.control.addEventListener('change', () => { r.from_date = from.control.value || null; saveRanges(); });
      to.control.addEventListener('change', () => { r.to_date = to.control.value || null; saveRanges(); });
      return h('div', { class: 'row row-static set-ctl is-stack' },
        h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, r.from_date ? `Free ${rangeWords(r)}` : 'New range: pick the first free day'),
          h('span', { class: 'row-sub' }, r.from_date ? 'Supercharger sessions that start in it cost nothing.' : 'It is saved once it has a start date.')),
        h('div', { class: 'two' }, from, to),
        ro ? null : h('div', { class: 'form-actions' }, btn('Remove', { kind: 'ghost', icon: 'trash', cls: 'btn-sm', onclick: () => { const was = !!r.from_date; ranges.splice(i, 1); drawRanges(); if (was) saveRanges(); } })));
    });
    fsBox.replaceChildren(
      group(null, rows.length ? rows : [infoRow({ name: 'No free Supercharging', sub: 'Every Supercharger session is priced from the billed energy or the place’s price.' })]),
      fsNote,
      ro ? null : h('div', { class: 'form-actions' }, btn('Add a range', { kind: 'soft', icon: 'plus', onclick: () => { ranges.push({ from_date: null, to_date: null }); drawRanges(); } })));
  }
  drawRanges();
  const access = cfg.supercharging_access;
  const fsT = tile({ w: 2, title: 'Free Supercharging', icon: 'bolt', cover: 'web.settings.car.free-supercharging' },
    said('info', 'The car says Supercharging is: ', h('b', null, access && access.value != null ? yesNo(access.value) : 'not sent yet'), access && access.value != null ? ` (${access.source === 'vin' ? SOURCE.vin.toLowerCase() : (srcText(access) || SOURCE[access.source] || 'typed by an admin').replace(/^D/, 'd')})` : '', '. It does not say since when, so the dates are yours.'),
    fsBox,
    note('Dated ranges in the car’s time zone. Sessions outside them keep their price, so turning this off later does not rewrite history.'));

  /* ---------- data sources */
  // GET .../settings: source_order (null = the default), source_order_default, inputs [{ kind, label, on, last_ms }] in
  // that order, imports [{ site, label, from_ms, to_ms }], import_gap_fill (null = follow the server),
  // import_gap_fill_on, import_gap_fill_default. PATCH takes source_order (all four once, or null) and import_gap_fill.
  const dsBox = h('div', { class: 'contents' });
  const dsNote = h('p', { class: 'form-error', role: 'status', hidden: true });
  const known = new Map();
  let pending = null; // the order shown while a save is on its way
  let busy = false;
  const defaultOrder = () => (Array.isArray(settings.source_order_default) && settings.source_order_default.length ? settings.source_order_default : SRC_ORDER);
  const savedOrder = () => (Array.isArray(settings.source_order) && settings.source_order.length ? settings.source_order : defaultOrder());
  const inputOf = (k) => known.get(k) || { kind: k, label: SRC_NAME[k] || k, on: false, last_ms: null };
  const nameOf = (k) => cap(inputOf(k).label || SRC_NAME[k] || k);
  const orderWords = (o) => o.map((k) => inputOf(k).label || SRC_NAME[k] || k).join(', ');
  async function saveDs(body, okText, before) {
    if (busy) return;
    dsNote.hidden = true;
    busy = true;
    drawDs();
    try {
      await patchSettings(body);
      ctx.toast(okText);
    } catch (err) {
      before?.();
      dsNote.textContent = err.code === 'conflict' ? err.message : notSaved(err);
      dsNote.hidden = false;
    }
    busy = false;
    pending = null;
    drawDs();
  }
  const saveOrder = (next) => {
    if (busy) return;
    if (sameList(next, savedOrder())) { pending = null; drawDs(); return; }
    pending = next;
    saveDs({ source_order: next }, `Saved: ${orderWords(next)}, highest first.`);
  };
  /** Drag by the grip (mouse, pen or touch): the row moves past a neighbour once the pointer is beyond its middle. */
  function dragFrom(e, rowEl) {
    if (busy || e.button > 0) return;
    e.preventDefault();
    const list = rowEl.parentElement;
    const pid = e.pointerId;
    rowEl.classList.add('is-dragging');
    const rowsNow = () => [...list.querySelectorAll('.ds-row')];
    const move = (ev) => {
      const rows = rowsNow(), i = rows.indexOf(rowEl), prev = rows[i - 1], next = rows[i + 1];
      const mid = (r) => { const b = r.getBoundingClientRect(); return b.top + b.height / 2; };
      if (ev.pointerId !== pid) return;
      // the neighbour moves, not the dragged row, so the row under the pointer stays in the page
      if (prev && ev.clientY < mid(prev)) list.insertBefore(prev, rowEl.nextSibling);
      else if (next && ev.clientY > mid(next)) list.insertBefore(next, rowEl);
    };
    const end = (ev) => {
      if (ev.pointerId !== pid) return;
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', end);
      window.removeEventListener('pointercancel', end);
      rowEl.classList.remove('is-dragging');
      const next = rowsNow().map((r) => r.dataset.kind);
      if (ev.type === 'pointercancel') { drawDs(); return; }
      saveOrder(next);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', end);
    window.addEventListener('pointercancel', end);
  }
  const stepBy = (order, k, by) => {
    const i = order.indexOf(k), j = i + by;
    if (i < 0 || j < 0 || j >= order.length) return order;
    const next = [...order];
    [next[i], next[j]] = [next[j], next[i]];
    return next;
  };
  function drawDs() {
    for (const i of settings.inputs || []) if (i && i.kind) known.set(i.kind, i);
    const order = pending || savedOrder();
    // keep the focus on the same control across a redraw (an arrow at the end of the list: the other arrow)
    const a = document.activeElement;
    const had = a && dsBox.contains(a) ? a.dataset.focus || null : null;
    const rows = order.map((k, i) => {
      const inp = inputOf(k);
      const name = nameOf(k);
      const last = inp.last_ms != null ? ['last data ', ago(inp.last_ms)] : 'no data yet';
      const said = inp.label || SRC_NAME[k] || k;
      const rowEl = h('div', { class: ['row', 'row-static', 'ds-row', { 'is-off': !inp.on }], role: 'listitem', dataset: { kind: k } },
        admin ? h('span', { class: ['row-ic', 'ds-grip', inp.on ? 'tone-teal' : null], title: `Drag ${said} up or down`, 'aria-hidden': 'true', onpointerdown: (e) => dragFrom(e, rowEl) }, icon('grip'))
          : h('span', { class: ['row-ic', 'ds-rank', inp.on ? 'tone-teal' : null], 'aria-hidden': 'true' }, String(i + 1)),
        h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, h('span', { class: 'sr-only' }, `${i + 1}. `), name),
          h('span', { class: 'row-sub' }, h('b', { class: inp.on ? 'ds-on' : 'ds-off' }, inp.on ? 'On' : 'Off'), ' · ', last)),
        admin ? h('span', { class: 'row-side ds-moves' },
          btn(`Move ${said} up`, { kind: 'ghost', icon: 'chevronUp', iconOnly: true, cls: 'btn-sm', disabled: i === 0, attrs: { 'data-focus': `${k}:up` }, onclick: () => saveOrder(stepBy(order, k, -1)) }),
          btn(`Move ${said} down`, { kind: 'ghost', icon: 'chevronDown', iconOnly: true, cls: 'btn-sm', disabled: i === order.length - 1, attrs: { 'data-focus': `${k}:down` }, onclick: () => saveOrder(stepBy(order, k, 1)) })) : null);
      return rowEl;
    });
    const imports = Array.isArray(settings.imports) ? settings.imports : [];
    const span = (im) => (im.from_ms != null && im.to_ms != null ? ` (${day(im.from_ms, ctx.tz)} to ${day(im.to_ms, ctx.tz)})` : '');
    const impWords = imports.map((im) => `${im.label || 'Imported'}${span(im)}`).join(', ');
    const lastRow = infoRow({ icon: 'lock', name: 'Imported history', cls: 'ds-fixed', sub: imports.length ? impWords : 'None yet' });
    lastRow.setAttribute('role', 'listitem');
    const isDefault = sameList(order, defaultOrder());
    // gap filling: null follows the server's setting
    const gfSet = settings.import_gap_fill ?? null;
    const gfDefault = settings.import_gap_fill_default ?? null;
    const gfOn = settings.import_gap_fill_on ?? gfSet ?? gfDefault;
    const gap = toggle({
      label: 'Fill gaps from imported history',
      sub: imports.length ? `Uses ${impWords}. ${gfSet == null ? `Follows the server default: ${onOff(gfDefault)}.` : `Set for this car. Server default: ${onOff(gfDefault)}.`}` : 'No imported history',
      checked: imports.length ? !!gfOn : false,
      disabled: ro || !imports.length,
      onchange: (on) => { if (busy) { gap.set(gfOn); return; } saveDs({ import_gap_fill: on }, on ? 'Saved: gaps are filled from imported history.' : 'Saved: gaps are not filled from imported history.', () => gap.set(gfOn)); },
    });
    gap.input.dataset.focus = 'gap';
    const follow = admin && imports.length && gfSet != null
      ? btn(`Follow the server default (${onOff(gfDefault)})`, { kind: 'soft', icon: 'reset', cls: 'btn-sm',
        onclick: () => saveDs({ import_gap_fill: null }, `Saved: gap filling follows the server default (${onOff(gfDefault)}).`) }) : null;
    dsBox.replaceChildren(...[
      h('section', { class: 'rows set-group' },
        h('h3', { class: 'rows-name' }, 'Live inputs, highest first'),
        h('div', { class: 'rows-list ds-list', role: 'list', 'aria-label': 'Live inputs, highest priority first' }, rows, lastRow),
        h('p', { class: 'set-note' }, admin ? 'Drag a row by its grip, or use the arrows. Imported history always comes after the live inputs.' : 'Imported history always comes after the live inputs. Only an admin can change the order.')),
      admin && !isDefault ? h('div', { class: 'form-actions' }, btn('Reset to default order', { kind: 'soft', icon: 'reset', cls: 'btn-sm',
        onclick: () => { if (busy) return; pending = defaultOrder(); saveDs({ source_order: null }, `Saved: the default order, ${orderWords(defaultOrder())}.`); } })) : null,
      group('Gaps', [gap], { note: 'Where no live input has data, the imported history fills the hole. Where a live input has data, it is shown.' }),
      follow ? h('div', { class: 'form-actions' }, follow) : null].filter(Boolean));
    if (had) {
      const same = [...dsBox.querySelectorAll('[data-focus]')].filter((el) => !el.disabled);
      const kind = had.split(':')[0];
      (same.find((el) => el.dataset.focus === had) || same.find((el) => el.dataset.focus.startsWith(`${kind}:`)))?.focus();
    }
  }
  drawDs();
  const dsT = tile({ w: 2, id: 'data-sources', title: 'Data sources', icon: 'pulse', side: ro ? chip('Read only', { tone: 'muted', icon: 'lock' }) : null },
    note('When two sources cover the same time, the higher one wins, value by value.'),
    dsBox, dsNote,
    admin ? h('a', { class: 'tile-link ds-link', href: `${state.buildUrl('import')}#${encodeURIComponent(`overlaps-${car.id}`)}` }, 'Choose per span which source is shown', icon('chevronRight'))
      : note('Which source is shown per span, where an import overlaps live history, is chosen by an admin on the Import page.'));

  /* ---------- put together */
  const cars = carsTile(ctx, car);
  await cars.load();
  if (!ctx.alive()) return {};
  const bar = h('div', { class: 'page-bar w-full' }, crumb(car),
    h('span', { class: 'page-bar-end note' }, car.last_activity_ms ? ['Last heard ', ago(car.last_activity_ms)] : 'Not heard from yet'));
  const how = tile({ w: 2, title: 'How a new car appears', icon: 'spark', cover: 'web.settings.car.empty-state' }, howSteps());
  ctx.view.replaceChildren(bar, carT, soldT, upT, dsT, tzT, batT, fsT, cars.el, how);
  ctx.ready();
  return { update(c) { drawNow(); c.ready(); } };
}
