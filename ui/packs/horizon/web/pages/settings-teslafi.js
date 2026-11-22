// Settings: Integrations, TeslaFi (#teslafi): the TeslaFi input, part of pages/settings-integrations.js.
// For owners who pay for TeslaFi: the server reads TeslaFi's own polls of the car (about one a minute while it is
// awake) with the owner's TeslaFi token, as a third live input beside SMO and the Tesla API. Off until an admin
// adds a token: before that the server makes no connection to TeslaFi at all. It only reads (TeslaFi's latest
// poll, and the last poll of the car awake), never wakes the car, and no answer ever holds the token. Where SMO or
// the Tesla API cover the same time, theirs win.
//
// Built like the Tesla account section (pages/settings-tesla.js, its tz-* look): the input's state, the host,
// its pace and requests today on the left, adding a token on the right; then one HUD per account (#teslafi-<id>):
// status, TeslaFi's time zone, last good answer and last error, pause or resume, a new token, rename, remove, and
// the car TeslaFi polls matched to a car here, a new car or left out.
//
// Admins only, reading included. Every change asks for the admin's own password again (and the two-step code).
// API: GET/PATCH integrations/teslafi (PATCH { version, poll }: the pace), GET/POST .../accounts, PATCH/DELETE .../accounts/{id},
// GET/PUT .../accounts/{id}/cars, GET .../status.

import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, btn, tag, kv, note, seg } from '../core/ui.js';
import { select, carOptions } from '../core/kit.js';
import { field } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { toast } from '../core/layer.js';
import { dateTime, time, ago, dur, DASH } from '../core/fmt.js';
import { code, sig, hint } from '../core/setting.js';
import { askPassword } from './server-move.js';

const enc = encodeURIComponent;
const BASE = 'integrations/teslafi';
const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();

/** An account's status: [word, signal tone, what it means]. */
const STATUS = {
  checking: ['Checking', 'live', 'The server is asking TeslaFi for this token\'s car for the first time.'],
  ok: ['Reading', 'ok', 'TeslaFi takes the token; the car matched below is read.'],
  paused: ['Paused', 'off', 'Paused here: nothing is asked of TeslaFi for this account until you resume it.'],
  token_refused: ['Token refused', 'bad', 'TeslaFi no longer takes this token (it was changed or removed there, or the TeslaFi subscription ended). Nothing is asked until you give a new token.'],
  error: ['TeslaFi answers with errors', 'warn', 'TeslaFi answers this account with errors, so the server waits longer between tries.'],
};
/** The pace (`poll`): [value, word, driving or charging, awake, asleep, why]. As docs/ui-api.md, "Pace". */
const PACES = [
  ['gentle', 'Gentle', '2 min', '5 min', '20 min', 'Fewest requests; coarser drives and charges.'],
  ['normal', 'Normal', '60 s', '3 min', '10 min', 'The usual pace: about one read a minute while the car is busy.'],
  ['often', 'Often', '15 s', '60 s', '3 min', 'About as fast as TeslaFi itself refreshes: finest drives and charges, more of the day\'s requests.'],
];
const paceOf = (v) => PACES.find((x) => x[0] === v) || PACES[1];
/** What the server does for the car now. */
const MODE = {
  off: ['Off', 'off', 'Not read: the car is left out or not matched yet, or the account is paused or its token refused.'],
  asleep: ['Asleep', 'off', 'TeslaFi says the car sleeps: read at the asleep pace, never woken.'],
  idle: ['Parked', 'live', 'Parked and awake: read at the awake pace.'],
  charging: ['Charging', 'ok', 'Read at the driving and charging pace.'],
  driving: ['Driving', 'ok', 'Read at the driving and charging pace.'],
  backoff: ['Waiting after an error', 'warn', 'TeslaFi answered with an error, so the server waits before it asks again (60 s, doubling to 15 minutes).'],
};
const ERRORS = {
  teslafi_refused: 'TeslaFi does not take this token: check it on TeslaFi\'s API page (Settings, TeslaFi API). A wrong token, or the TeslaFi subscription ended. Nothing was changed.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was changed; try again in a few minutes.',
  account_exists: 'That token\'s car is already read through another TeslaFi account here.',
  car_taken: 'That car here is already matched to another TeslaFi car. Pick another, a new car, or leave this one out.',
  no_such_teslafi_account: 'This TeslaFi account is gone: someone removed it meanwhile. The page shows the accounts as they are now.',
  no_such_vehicle: 'That car is no longer on this TeslaFi account.',
  stale_version: 'Someone changed this account meanwhile. The page shows their change now; try again.',
  throttled: 'Too many tries. Wait a minute, then try again.',
  not_allowed: 'Only an admin can change TeslaFi.',
};
const LAST_ERROR = { token_refused: 'TeslaFi refused the token', teslafi_refused: 'TeslaFi refused the token', teslafi_unreachable: 'TeslaFi could not be reached', rate_limited: 'TeslaFi asked the server to slow down', http_error: 'TeslaFi answered with an error' };
const lastErrorText = (le) => (le ? `${LAST_ERROR[le.code] || le.code}${le.message ? `: ${le.message}` : ''}` : null);

function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (e.code !== 'wrong_password' && e.code !== 'wrong_two_step' && e.code !== 'invalid' && e.code !== 'bad_input') e.message = ERRORS[e.code] || e.message || 'That did not work. Try again.';
  if (e.status === 409 && !['account_exists', 'car_taken'].includes(e.code)) e.message = ERRORS.stale_version;
  if (e.fields) {
    delete e.fields.confirm_password; delete e.fields.code;
    if (!Object.keys(e.fields).length) e.fields = null;
  }
  return e;
}
const isStale = (e) => e?.code === 'no_such_teslafi_account' || (e?.status === 409 && !['account_exists', 'car_taken'].includes(e.code));
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const secs = (s) => (s == null ? DASH : s >= 120 && s % 60 === 0 ? `${formatNumber(s / 60)} min` : `${formatNumber(s)} s`);
const nOf = (n, one, many) => `${formatNumber(n)} ${n === 1 ? one : many}`;
const zoneHint = 'The zone TeslaFi shows its times in (TeslaFi: Settings, Account): an IANA name such as America/Chicago. Empty: the server\'s default zone.';
const tokenAttrs = { spellcheck: 'false', autocomplete: 'off', autocapitalize: 'off', class: 'input mono' };

/**
 * The TeslaFi section. cars: this server's cars ({ id, name, time_zone }) for the match picker.
 * Returns { el, load() }; the page awaits load() before ready().
 */
export function teslafiSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const side = h('span', { class: 'tz-side' });
  const body = h('div', { class: 'stack' });
  const el = h('section', { class: 'stack tz', id: 'teslafi', 'aria-label': 'TeslaFi' },
    hud({ title: 'TeslaFi', icon: 'antenna', tone: 'cyan', side },
      h('div', { class: 'stack' },
        h('p', { class: 'tz-intro' }, 'A third live input, for owners who pay for TeslaFi: the server reads TeslaFi\'s own polls of the car (at the pace chosen below) with your TeslaFi token. It only reads, never wakes the car or sends it a command, and no page shows the token again. Off until an admin adds a token; until then the server makes no connection to TeslaFi at all. Where SMO or the Tesla API cover the same time, theirs win; pages label the rest "Data from TeslaFi".'),
        body)));
  const accSlot = h('div', { class: 'stack' });
  el.append(accSlot);

  let ov = null, accounts = [], status = [], carRows = {};
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  async function ask(opts) {
    let stale = false;
    const r = await askPassword(ctx, { ...opts, run: async (cred, v) => {
      try { return await opts.run(cred, v); } catch (e) {
        const w = words(e);
        if (isStale(w)) stale = true;
        throw w;
      }
    } });
    if (!r && stale && ctx.alive()) await load();
    return r;
  }

  async function load() {
    if (!admin) {
      replace(side, tag('Admins only', 'org'));
      replace(body, note('Only an admin sees and changes TeslaFi: its token, status and car. Every page still says where its data came from, "Data from TeslaFi" included.'));
      replace(accSlot);
      return;
    }
    try {
      [ov, accounts, status] = await Promise.all([
        ctx.api.get(BASE),
        ctx.api.list(`${BASE}/accounts`).all(),
        ctx.api.list(`${BASE}/status`).all().catch(() => []),
      ]);
      const rows = await Promise.all(accounts.map((a) => ctx.api.get(`${BASE}/accounts/${enc(a.id)}/cars`).catch(() => null)));
      carRows = Object.fromEntries(accounts.map((a, i) => [a.id, rows[i] || { items: a.cars || [], version: a.version }]));
    } catch (e) {
      if (e?.name === 'AbortError') throw e;
      replace(side, sig('warn', 'Not read'));
      replace(body, h('p', { class: 'warn' }, 'TeslaFi could not be read: ', errorWords(e).text));
      return;
    }
    if (!ctx.alive()) return;
    paint();
  }
  const reload = async (msg) => { if (msg) toast(msg); await load(); };

  const isMatched = (c) => c.match != null && c.match !== 'leave_out';
  function reading() {
    const live = accounts.filter((a) => !a.paused && a.status !== 'token_refused');
    const n = live.reduce((k, a) => k + (carRows[a.id]?.items || a.cars || []).filter(isMatched).length, 0);
    return { live: live.length, cars: n, paused: accounts.filter((a) => a.paused).length, refused: accounts.filter((a) => !a.paused && a.status === 'token_refused').length };
  }
  function paint() {
    const rd = reading();
    replace(side, !ov.enabled ? sig('off', 'Off until a token is added')
      : !rd.live ? (rd.refused ? sig('warn', 'Token refused: no car read') : sig('off', 'Paused: no car read'))
        : rd.cars ? sig('ok', `Reading ${nOf(rd.cars, 'car', 'cars')}`) : sig('warn', 'No car matched yet'));
    replace(body, h('div', { class: 'tz-grid' }, statusPanel(), addPanel()));
    replace(accSlot, accounts.map(accountPanel));
  }

  /* ---------- the input as a whole: state, host, pace, requests, last error */
  function statusPanel() {
    const p = ov.polls || {};
    const rd = reading();
    const cap = (p.daily_cap || 0) * Math.max(1, ov.accounts || 0);
    const poll = ov.poll || 'normal';
    const pace = seg(PACES.map(([v, l]) => [v, l]), poll, (v) => setPace(v, pace), { label: 'How often TeslaFi is asked' });
    const input = !ov.enabled ? ['Off', 'until a token is added']
      : !rd.live ? [rd.refused ? 'Stopped' : 'Paused', `${[rd.paused ? `${nOf(rd.paused, 'account', 'accounts')} paused` : '', rd.refused ? `${nOf(rd.refused, 'token', 'tokens')} refused` : ''].filter(Boolean).join(', ')}: no car read`]
        : ['On', `${nOf(rd.live, 'account', 'accounts')}, ${nOf(rd.cars, 'car', 'cars')} read`];
    return h('div', { class: 'tz-face' },
      h('div', { class: 'tz-read' },
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Input'), h('span', { class: 'tz-v' }, input[0]), h('span', { class: 'tz-how' }, input[1])),
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Requests today'), h('span', { class: 'tz-v' }, formatNumber(ov.polls_today ?? 0)),
          h('span', { class: 'tz-how' }, ov.enabled && cap ? `of at most ${formatNumber(cap)}` : 'none without a token')),
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Connection'), h('span', { class: 'tz-v' }, ov.tls ? `TLS ${ov.tls}` : DASH),
          h('span', { class: 'tz-how' }, ov.tls === '1.2' ? 'TeslaFi offers no TLS 1.3' : ov.tls ? 'to TeslaFi, port 443' : 'none made yet'))),
      ov.last_error
        ? h('p', { class: 'tz-err' }, icon('warn', 'i--sm'), h('span', null, 'Last error: ', h('b', null, lastErrorText(ov.last_error)), ov.last_error.at_ms ? `, ${dateTime(ov.last_error.at_ms, ctx.tz)} (${ago(ov.last_error.at_ms)})` : '', '.'))
        : h('p', { class: 'tz-ok' }, icon('check', 'i--sm'), h('span', null, ov.enabled ? 'No error.' : 'Nothing asked of TeslaFi yet.')),
      h('div', { class: 'tz-block' }, h('p', { class: 'tz-k' }, 'Host it talks to'),
        ov.hosts?.length ? h('ul', { class: 'tz-hosts' }, ov.hosts.map((x) => h('li', null, code(x))))
          : h('p', { class: 'tz-dim' }, 'None while there is no token. Once one is added: www.teslafi.com only, over HTTPS, the token in a header (never in an address).')),
      h('div', { class: 'tz-block' },
        h('div', { class: 'tz-pace-head' }, h('p', { class: 'tz-k' }, 'How often TeslaFi is asked'), pace.el),
        h('ul', { class: 'tz-paces' }, PACES.map(([v, l, busy, awake, asleep]) => h('li', { class: ['tz-pc', v === poll && 'is-on'] },
          h('b', null, l), h('span', { class: 'mono' }, `${busy} driving or charging · ${awake} awake · ${asleep} asleep`)))),
        h('dl', { class: 'tz-pace' },
          [['A driving car', p.driving_s], ['A charging car', p.charging_s], ['A parked, awake car', p.parked_s], ['A sleeping car', p.asleep_s], ['While SMO or the Tesla API is live', p.other_live_s]]
            .map(([l, s]) => h('div', null, h('dt', null, l), h('dd', null, l.startsWith('While') ? `at most every ${secs(s)}` : `every ${secs(s)}`)))),
        h('p', { class: 'tz-dim' }, `${paceOf(poll)[5]} TeslaFi only has new data as often as it polls the car itself, and these reads never wake the car. Whatever the pace: at most one request every 10 s${p.daily_cap ? ` and ${formatNumber(p.daily_cap)} a day per account (then nothing until midnight UTC)` : ''}, checks of a new token included. Errors wait 60 s, doubling to 15 minutes; TeslaFi asking to slow down waits what it asks.`)));
  }
  async function setPace(v, pace) {
    const cur = ov.poll || 'normal';
    if (v === cur) return;
    const pc = paceOf(v);
    const r = await ask({
      title: `Pace: ${pc[1]}`,
      intro: h('p', null, `How often the server reads TeslaFi, for every TeslaFi account: driving or charging every ${pc[2]}, awake every ${pc[3]}, asleep every ${pc[4]}. `, pc[5]),
      submit: 'Change the pace',
      run: async (cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v })),
    });
    if (r) await reload(`How often TeslaFi is asked: ${pc[1]}, saved.`);
    else if (ctx.alive()) pace.set(ov.poll || 'normal');
  }

  /* ---------- adding a token */
  function zoneField(value) {
    const f = field({ label: 'TeslaFi\'s time zone', name: 'time_zone', value: value ?? '', hint: zoneHint, attrs: { list: 'tf-zones', autocomplete: 'off', spellcheck: 'false', class: 'input mono' } });
    f.el.append(h('datalist', { id: 'tf-zones' }, ZONES.map((z) => h('option', { value: z }))));
    return f;
  }
  function addPanel() {
    return h('div', { class: 'tz-add' },
      h('p', { class: 'tz-add-head' }, icon('plus', 'i--sm'), h('span', null, accounts.length ? 'Add another TeslaFi account' : 'Add a TeslaFi token')),
      h('div', { class: 'tz-way' },
        h('p', { class: 'card-title' }, 'Paste your TeslaFi token'),
        h('p', { class: 'card-text' }, 'In TeslaFi: Settings, TeslaFi API: turn the API on and copy its token. The server checks it with TeslaFi (one read, and when the latest poll names no car one more 10 s later) before anything is kept, so adding can take about 10 seconds.'),
        h('div', { class: 'row' }, btn('Add a TeslaFi token', () => addToken(), { kind: 'go', icon: 'key' }))),
      hint('Its car is not read until you match it to a car here or to a new car.'));
  }
  async function addToken() {
    const label = field({ label: 'Name for this account', name: 'label', required: true, value: 'TeslaFi', hint: 'Your own name for it, shown on this page.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const tok = field({ label: 'TeslaFi token', name: 'token', required: true, hint: 'Checked with TeslaFi before it is kept. Sealed on the server, never shown again.', attrs: { ...tokenAttrs, autocomplete: 'off', maxlength: '256' } });
    const zone = zoneField(ctx.tz);
    const r = await ask({
      title: 'Add a TeslaFi token', submit: 'Check with TeslaFi and add', extra: [label, tok, zone],
      intro: h('p', null, 'Only paste a token of your own TeslaFi account. Anyone with it can read the car\'s data, so it never leaves this server and no page shows it.'),
      run: async (cred, v) => ctx.api.post(`${BASE}/accounts`, withPw(cred, { label: v.label.trim(), token: v.token.trim(), time_zone: v.time_zone.trim() || null })),
    });
    if (r) await reload(`${r.label} added. Match its car below to start reading it.`);
  }

  /* ---------- one account */
  function accountPanel(a) {
    const st = STATUS[a.status] || [a.status || 'Not known', 'off', ''];
    const err = a.last_error;
    const change = (title, intro, submit, bodyOf, { extra = [], msg = 'Saved.', danger = false } = {}) => ask({
      title, intro: intro ? h('p', null, intro) : null, submit, extra, danger,
      run: async (cred, v) => ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: a.version, ...bodyOf(v) })),
    }).then((r) => (r ? reload(msg) : null));

    const pauseBtn = btn(a.paused ? 'Resume' : 'Pause', () => change(a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`,
      a.paused ? 'Its matched car is read again from now on.' : 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile; the history already stored stays.',
      a.paused ? 'Resume' : 'Pause', () => ({ paused: !a.paused }), { msg: a.paused ? `${a.label} resumed.` : `${a.label} paused.` }),
    { sm: true, icon: a.paused ? 'live' : 'moon' });
    const tokenBtn = btn('Replace token', () => {
      const f = field({ label: 'New TeslaFi token', name: 'token', required: true, hint: 'Checked with TeslaFi before it replaces the old one. If TeslaFi refuses it, nothing changes.', attrs: { ...tokenAttrs, maxlength: '256' } });
      return change(`New token for ${a.label}`, 'For a token TeslaFi stopped taking, or one you made again in TeslaFi.', 'Check with TeslaFi and replace', (v) => ({ token: v.token.trim() }), { extra: [f], msg: 'Token replaced and checked with TeslaFi.' });
    }, { sm: true, icon: 'key', kind: a.status === 'token_refused' ? 'go' : null });
    const zoneBtn = btn('Time zone', () => {
      const f = zoneField(a.time_zone);
      return change(`TeslaFi's time zone for ${a.label}`, 'TeslaFi writes its times without a zone; from now on they are read in this one.', 'Save', (v) => ({ time_zone: v.time_zone.trim() || null }), { extra: [f], msg: 'Time zone saved.' });
    }, { sm: true, icon: 'clock', kind: 'ghost' });
    const renameBtn = btn('Rename', () => {
      const f = field({ label: 'Name', name: 'label', required: true, value: a.label, attrs: { maxlength: '64', autocomplete: 'off' } });
      return change(`Rename ${a.label}`, null, 'Rename', (v) => ({ label: v.label.trim() }), { extra: [f], msg: 'Renamed.' });
    }, { sm: true, icon: 'sliders', kind: 'ghost' });
    const removeBtn = btn('Remove', async () => {
      const r = await ask({
        title: `Remove ${a.label}?`, danger: true, submit: 'Remove the account',
        intro: h('p', null, 'Reading stops at once and the sealed file with the token is deleted. The history already stored from it stays, marked as data from TeslaFi.'),
        run: async (cred) => ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred)),
      });
      if (r) await reload(`${a.label} removed. Its history stays.`);
    }, { sm: true, icon: 'x', kind: 'ghost' });

    const errLine = err ? [lastErrorText(err), err.at_ms ? ` · ${ago(err.at_ms)}` : ''] : null;
    return hud({ title: a.label, icon: 'key', id: `teslafi-${a.id}`, cls: 'tz-acc', label: `TeslaFi account ${a.label}`, side: [sig(st[1], st[0]), a.tls ? tag(`TLS ${a.tls}`, 'cyan') : null] },
      h('div', { class: 'stack' },
        st[2] ? h('p', { class: ['tz-why', (a.status === 'token_refused' || a.status === 'error') && 'warn'] }, st[2]) : null,
        h('div', { class: 'tz-acc-grid' },
          kv([
            ['Added', dateTime(a.added_ms, ctx.tz)],
            ['Time zone', a.time_zone ? code(a.time_zone) : h('span', { class: 'faint' }, 'the server\'s default')],
            ['Last good answer', a.last_ok_ms ? h('span', { title: dateTime(a.last_ok_ms, ctx.tz) }, ago(a.last_ok_ms)) : h('span', { class: 'faint' }, 'none yet')],
            ['Last error', errLine ? h('span', { class: 'warn' }, errLine) : h('span', { class: 'faint' }, 'none')],
            ['Requests today', `${formatNumber(a.polls_today ?? 0)}${ov.polls?.daily_cap ? ` of ${formatNumber(ov.polls.daily_cap)}` : ''}`],
            ['Doing now', sig((MODE[a.mode] || MODE.off)[1], (MODE[a.mode] || [a.mode || 'Not known'])[0])],
          ]),
          h('div', { class: 'stack' },
            h('div', { class: 'row' }, pauseBtn, tokenBtn, zoneBtn, renameBtn, removeBtn),
            h('p', { class: 'tz-dim' }, 'Every change asks for your password again. A new token is checked with TeslaFi first; if TeslaFi refuses it, nothing in the change is kept.'))),
        carsTable(a)));
  }

  /* ---------- the car TeslaFi polls: matched to a car here */
  function carsTable(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    if (!items.length) return emptyState({ icon: 'car', title: 'No car yet', text: 'TeslaFi\'s latest poll named no car yet. Once a poll names it (the car awake), it shows here to be matched.' });
    const takenBy = new Map();
    for (const o of accounts) if (o.id !== a.id) for (const c of carRows[o.id]?.items || o.cars || []) if (isMatched(c) && c.match !== 'new') takenBy.set(c.match, o.label);
    const free = cars.filter((c) => !takenBy.has(c.id)), taken = cars.filter((c) => takenBy.has(c.id));
    // sold cars wait under Sold cars (a sold car already matched stays up top)
    const freeOpts = carOptions(free, items.map((c) => c.match));
    const opts = [['', 'Not decided (not read)'], ...freeOpts.filter((o) => !o.group), ['new', 'A new car'], ['leave_out', 'Leave out (not read)'], ...freeOpts.filter((o) => o.group),
      ...(taken.length ? [{ group: 'Matched on another account', items: taken.map((c) => [c.id, `${c.name} (${takenBy.get(c.id)})`]) }] : [])];
    const sels = new Map();
    const save = btn('Save matches', () => saveMatches(), { kind: 'go', sm: true, icon: 'check' });
    const changed = () => items.filter((c) => (sels.get(c.vehicle).value || null) !== (c.match ?? null));
    const sync = () => { save.hidden = !changed().length; };
    async function saveMatches() {
      const ch = changed();
      if (!ch.length) return;
      const r = await ask({
        title: `Match the car of ${a.label}`, submit: 'Save matches',
        intro: h('ul', { class: 'tz-list' }, ch.map((c) => { const v = sels.get(c.vehicle).value; return h('li', null, h('b', null, c.display_name || c.vehicle), ': ', v === 'new' ? 'a new car here, read from now on' : v === 'leave_out' ? 'left out, not read' : !v ? 'not decided, not read' : `${carName(v)}, read from now on`); })),
        run: async (cred) => ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, withPw(cred, { version: data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: sels.get(c.vehicle).value || null })) })),
      });
      if (r) await reload('Match saved.');
    }
    const rows = items.map((c) => {
      const sv = status.find((s) => s.account === a.id && s.vehicle === c.vehicle) || {};
      const pre = c.match ?? c.suggested ?? '';
      const known = opts.some((o) => (o.group ? o.items.some(([v]) => v === pre) : o[0] === pre)) ? opts : [...opts, [pre, carName(pre)]];
      const sel = select(known, pre, sync, { label: `Car here for ${c.display_name || c.vehicle}`, cls: 'tz-sel' });
      sels.set(c.vehicle, sel);
      const mode = MODE[sv.mode] || MODE.off;
      const tz = cars.find((x) => x.id === c.match)?.time_zone || ctx.tz;
      const next = sv.next_request_ms ?? null;
      return h('li', { class: 'tz-car' },
        h('div', { class: 'tz-car-id' }, h('p', { class: 'tz-car-name' }, icon('car', 'i--sm'), h('b', null, c.display_name || 'No name at TeslaFi'), c.current ? tag('Polled now', 'cyan') : null),
          h('p', { class: 'tz-dim' }, [c.year, c.model].filter(Boolean).join(' ') || 'Model not known', ' · ', h('span', { class: 'mono' }, c.vin_prefix || DASH), c.car_state ? ` · TeslaFi says ${String(c.car_state).toLowerCase()}` : '')),
        h('div', { class: 'tz-car-match' }, h('span', { class: 'tz-k' }, 'Car here'), sel,
          c.match == null && c.suggested ? h('span', { class: 'tz-sugg' }, tag('Suggested', 'grn'), ' same VIN start as ', carName(c.suggested), '; saved only with Save matches') : null),
        h('div', { class: 'tz-car-mode', title: mode[2] }, h('span', { class: 'tz-k' }, 'Doing now'), sig(mode[1], mode[0]),
          sv.mode === 'backoff' && next ? h('span', { class: 'tz-dim warn' }, `Next try ${time(next, tz)} (in ${dur(Math.max(0, next - ctx.api.now()))})`)
            : next && sv.mode && sv.mode !== 'off' ? h('span', { class: 'tz-dim' }, `Next read ${time(next, tz)}`) : h('span', { class: 'tz-dim' }, mode[2]),
          sv.last_data_ms ? h('span', { class: 'tz-dim' }, `Newest poll ${ago(sv.last_data_ms)}`) : null,
          sv.last_error ? h('span', { class: 'tz-dim warn' }, lastErrorText(sv.last_error)) : null));
    });
    sync();
    return h('div', { class: 'tz-cars' },
      h('div', { class: 'tz-cars-head' }, h('p', { class: 'tz-k' }, `Its car${items.length === 1 ? '' : 's'} (${items.length})`), save),
      h('ul', { class: 'tz-car-list' }, rows),
      hint('A car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN start as a car here; nothing changes until you press Save matches.'));
  }

  return { el, load };
}
