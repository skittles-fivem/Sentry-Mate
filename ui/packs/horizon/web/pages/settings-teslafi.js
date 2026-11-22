// Settings: Integrations, TeslaFi (#teslafi): the TeslaFi input, part of pages/settings-integrations.js.
// For owners who pay for TeslaFi: the server reads TeslaFi's own polls of the car from TeslaFi's API with the
// owner's TeslaFi token, as a third live input beside SMO and the Tesla API. Off until an admin adds a token:
// before that it makes no connection to TeslaFi at all. It only reads, never wakes a car, and no answer holds a token.
//
// Built like the Tesla account section (pages/settings-tesla.js), with its classes: a receiver face for the input
// (state, host, pace with its waits, requests today) beside a panel to add a token; then one HUD per account
// (#teslafi-<id>) with its status, pause or resume, a new token, name and time zone, removal, and its cars, each
// matched to a car here, a new car or left out.
//
// Admins only, reading included. Every change asks for the admin's own password again (and the two-step code):
// askPassword() from server-move.js, sent as confirm_password and code.
// API: GET/PATCH integrations/teslafi, GET/POST .../accounts, PATCH/DELETE .../accounts/{id},
// GET/PUT .../accounts/{id}/cars, GET .../status.

import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, btn, tag, kv, note, seg } from '../core/ui.js';
import { select } from '../core/kit.js';
import { field } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { toast } from '../core/layer.js';
import { dateTime, time, ago, dur, DASH } from '../core/fmt.js';
import { code, sig, hint } from '../core/setting.js';
import { POLL } from '../core/tesla.js';
import { askPassword } from './server-move.js';

const enc = encodeURIComponent;
const BASE = 'integrations/teslafi';
const PACE_WHY = {
  often: 'As fast as TeslaFi itself refreshes: finest drives and charges.',
  normal: 'The usual pace.',
  gentle: 'Fewest requests; coarser drives and charges.',
};
const WHERE = 'Find it on TeslaFi: Settings, TeslaFi API.';

/** An account's status: [word, signal tone, what it means]. */
const STATUS = {
  checking: ['Checking', 'live', 'The server is asking TeslaFi for this account\'s car for the first time.'],
  ok: ['Reading', 'ok', 'TeslaFi takes the token; the car matched below is read.'],
  paused: ['Paused', 'off', 'Paused here: no request goes to TeslaFi for this account until you resume it.'],
  token_refused: ['Token refused', 'bad', 'TeslaFi stopped taking the token (a wrong token, or the TeslaFi subscription ended). Nothing is asked until you give a new token.'],
  error: ['Errors', 'warn', 'TeslaFi answers this account with errors, so the server waits before it asks again.'],
};
/** What the server does for the account's car now. */
const MODE = {
  off: ['Off', 'off', 'Not read: the car is not matched, or the account is paused or its token refused.'],
  asleep: ['Asleep', 'off', 'TeslaFi says the car sleeps: read at the asleep pace. It is never woken.'],
  idle: ['Parked, awake', 'live', 'Read at the awake pace.'],
  charging: ['Charging', 'ok', 'Read at the charging pace.'],
  driving: ['Driving', 'ok', 'Read at the driving pace.'],
  backoff: ['Waiting after an error', 'warn', 'TeslaFi answered with an error, so the server waits before it asks again.'],
};
const ERRORS = {
  teslafi_refused: 'TeslaFi refused the token: a wrong token, or the TeslaFi subscription ended. Check it on TeslaFi (Settings, TeslaFi API). Nothing was changed.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was changed; try again in a few minutes.',
  account_exists: 'This token\'s car is already read through another TeslaFi account here.',
  car_taken: 'That car here is already matched to another TeslaFi car. Pick another, a new car, or leave this one out.',
  no_such_teslafi_account: 'This TeslaFi account is gone: someone removed it meanwhile. The page shows the accounts as they are now.',
  stale_version: 'Someone changed this meanwhile. The page shows their change now; try again.',
  throttled: 'Too many tries. Wait a minute, then try again.',
  not_allowed: 'Only an admin can change TeslaFi.',
};
const fiWords = (e) => ERRORS[e?.code] || e?.message || 'That did not work. Try again.';
const LAST_ERROR = { token_refused: 'TeslaFi refused the token', unreachable: 'TeslaFi could not be reached', rate_limited: 'TeslaFi asked the server to slow down' };
const lastErrorText = (le) => (le ? (le.message || LAST_ERROR[le.code] || le.code) : null);
const KEEP_409 = ['account_exists', 'car_taken'];

/** Our words on an error, for the password dialog (it shows the password and code errors itself). */
function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (e.status === 409 && !KEEP_409.includes(e.code)) e.code = 'stale_version';
  if (e.code !== 'wrong_password' && e.code !== 'wrong_two_step' && e.code !== 'invalid') e.message = fiWords(e);
  if (e.fields) {
    delete e.fields.confirm_password; delete e.fields.code;
    if (!Object.keys(e.fields).length) e.fields = null;
  }
  return e;
}
const isStale = (e) => e?.code === 'no_such_teslafi_account' || e?.code === 'stale_version';
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const secs = (s) => (s == null ? DASH : s >= 60 && s % 60 === 0 ? `${formatNumber(s / 60)} min` : `${formatNumber(s)} s`);
/** "driving or charging every 15 s, awake 1 min, asleep 3 min" */
export const paceText = (p = {}) => `driving or charging every ${secs(p.driving_s ?? p.charging_s)}, awake ${secs(p.parked_s)}, asleep ${secs(p.asleep_s)}`;

const tokenField = (label = 'TeslaFi token') => field({ label, name: 'token', type: 'password', required: true,
  hint: `${WHERE} Checked with TeslaFi before it is kept; sealed on the server and never shown again.`,
  attrs: { autocomplete: 'off', spellcheck: 'false', maxlength: '256' } });
const zoneField = (value) => field({ label: 'Time zone (optional)', name: 'time_zone', value: value || '',
  hint: 'The zone TeslaFi shows your times in, as on TeslaFi (for example America/Chicago). Empty: the server\'s own zone.',
  attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: 'America/Chicago', maxlength: '64' } });
const zoneOf = (v) => (String(v || '').trim() || null);

/**
 * The TeslaFi section. cars: this server's cars ({ id, name, time_zone }) for the match picker.
 * Returns { el, load() }; the page awaits load() before ready().
 */
export function teslafiSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const side = h('span', { class: 'tz-side' });
  const body = h('div', { class: 'stack' });
  const accSlot = h('div', { class: 'stack' });
  const el = h('section', { class: 'stack tz', id: 'teslafi', 'aria-label': 'TeslaFi' },
    hud({ title: 'TeslaFi', icon: 'antenna', tone: 'cyan', side },
      h('div', { class: 'stack' },
        h('p', { class: 'tz-intro' }, 'For owners who pay for TeslaFi: the server reads TeslaFi\'s own polls of your car with your TeslaFi token, as a third live input beside SMO and the Tesla API. It only reads, and never wakes the car. There is no connection to TeslaFi at all until a token is added. Where SMO or the Tesla API cover the same time, theirs win.'),
        body)),
    accSlot);

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
      replace(body, note('Only an admin sees and changes TeslaFi: its token, its car and how often TeslaFi is asked. Every page still says where its data came from.'));
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
      replace(accSlot);
      return;
    }
    if (!ctx.alive()) return;
    paint();
  }
  const reload = async (msg) => { if (msg) toast(msg); await load(); };

  const isMatched = (c) => c.match != null && c.match !== 'leave_out';
  const nOf = (n, one, many) => `${formatNumber(n)} ${n === 1 ? one : many}`;
  function reading() {
    const live = accounts.filter((a) => !a.paused && a.status !== 'token_refused');
    const n = live.reduce((k, a) => k + (carRows[a.id]?.items || a.cars || []).filter(isMatched).length, 0);
    return { live: live.length, cars: n, paused: accounts.filter((a) => a.paused).length, refused: accounts.filter((a) => !a.paused && a.status === 'token_refused').length };
  }
  function paint() {
    const rd = reading();
    replace(side, !accounts.length ? sig('off', 'Off until a token is added')
      : !rd.live ? (rd.refused ? sig('warn', 'Token refused: no car read') : sig('off', 'Paused: no car read'))
        : rd.cars ? sig('ok', `Reading ${nOf(rd.cars, 'car', 'cars')}`) : sig('warn', 'No car matched yet'));
    replace(body, accounts.length ? h('div', { class: 'tz-grid' }, statusPanel(), addPanel()) : h('div', { class: 'tz-grid' }, emptyPanel(), addPanel()));
    replace(accSlot, accounts.map(accountPanel));
  }

  function emptyPanel() {
    return emptyState({ icon: 'antenna', title: 'No TeslaFi token added',
      text: 'With a TeslaFi token added, the server reads the polls TeslaFi already makes of your car; until then it makes no connection to TeslaFi at all.' });
  }

  /* ---------- the input as a whole */
  function statusPanel() {
    const p = ov.polls || {};
    const poll = ov.poll || 'normal';
    const cap = (p.daily_cap || 0) * Math.max(1, ov.accounts || 0);
    const rd = reading();
    const input = !rd.live ? [rd.refused ? 'Stopped' : 'Paused', [rd.paused ? `${nOf(rd.paused, 'account', 'accounts')} paused` : '', rd.refused ? `${nOf(rd.refused, 'token', 'tokens')} refused` : ''].filter(Boolean).join(', ') + ': no car read']
      : ['On', `${nOf(rd.live, 'account', 'accounts')}, ${nOf(rd.cars, 'car', 'cars')} read${rd.paused ? `; ${formatNumber(rd.paused)} paused` : ''}`];
    const pace = seg(POLL.map(([v, l]) => [v, l]), poll, (v) => setPace(v, pace), { label: 'How often TeslaFi is asked' });
    return h('div', { class: 'tz-face' },
      h('div', { class: 'tz-read' },
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Input'), h('span', { class: 'tz-v' }, input[0]), h('span', { class: 'tz-how' }, input[1])),
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Requests today'), h('span', { class: 'tz-v' }, formatNumber(ov.polls_today ?? 0)),
          h('span', { class: 'tz-how' }, cap ? `of at most ${formatNumber(cap)}` : DASH)),
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'TLS'), h('span', { class: 'tz-v' }, ov.tls || DASH), h('span', { class: 'tz-how' }, 'to TeslaFi\'s host'))),
      ov.last_error
        ? h('p', { class: 'tz-err' }, icon('warn', 'i--sm'), h('span', null, 'Last error: ', h('b', null, lastErrorText(ov.last_error)), ov.last_error.at_ms ? `, ${dateTime(ov.last_error.at_ms, ctx.tz)} (${ago(ov.last_error.at_ms)})` : '', '.'))
        : h('p', { class: 'tz-ok' }, icon('check', 'i--sm'), h('span', null, 'No error.')),
      h('div', { class: 'tz-block' }, h('p', { class: 'tz-k' }, 'TeslaFi host it talks to'),
        h('ul', { class: 'tz-hosts' }, (ov.hosts?.length ? ov.hosts : ['www.teslafi.com']).map((x) => h('li', null, code(x))))),
      h('div', { class: 'tz-block' },
        h('div', { class: 'tz-pace-head' }, h('p', { class: 'tz-k' }, 'How often TeslaFi is asked'), pace.el),
        h('dl', { class: 'tz-pace' },
          [['A driving car', p.driving_s], ['A charging car', p.charging_s], ['A parked, awake car', p.parked_s], ['A car asleep', p.asleep_s],
            ['Any car while SMO or the Tesla API is live', p.other_live_s]].map(([l, s]) => h('div', null, h('dt', null, l), h('dd', null, `every ${secs(s)}`)))),
        h('p', { class: 'tz-dim' }, `${PACE_WHY[poll] || ''} Now: ${paceText(p)}. At every pace TeslaFi's Retry-After and its errors are respected, and the reads come from TeslaFi's own database: they never wake the car.${p.daily_cap ? ` At most ${formatNumber(p.daily_cap)} requests a day per account.` : ''}`)));
  }
  async function setPace(v, pace) {
    const cur = ov.poll || 'normal';
    if (v === cur) return;
    const r = await ask({
      title: `Pace: ${POLL.find((x) => x[0] === v)[1]}`,
      intro: h('p', null, 'How often the server reads TeslaFi. ', PACE_WHY[v], ' TeslaFi\'s Retry-After and errors are always respected.'),
      submit: 'Change the pace',
      run: async (cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v })),
    });
    if (r) { if (r.polls) ov = r; await reload(`How often TeslaFi is asked: saved (${paceText((r && r.polls) || ov.polls)}).`); }
    else pace.set(cur);
  }

  /* ---------- adding a token */
  function addPanel() {
    return h('div', { class: 'tz-add' },
      h('p', { class: 'tz-add-head' }, icon('plus', 'i--sm'), h('span', null, accounts.length ? 'Add another TeslaFi token' : 'Add a TeslaFi token')),
      h('div', { class: 'tz-way' },
        h('p', { class: 'card-title' }, 'Your TeslaFi API token'),
        h('p', { class: 'card-text' }, 'On TeslaFi, open Settings, then TeslaFi API, and copy the token shown there. The server checks it with TeslaFi (this can take about 10 s) before anything is kept, and never shows it again.'),
        h('div', { class: 'row' }, btn('Add a token', () => addToken(), { kind: 'go', icon: 'key' }))),
      hint('A new account\'s car is not read until you match it to a car here or to a new car.'));
  }
  async function addToken() {
    const label = field({ label: 'Name for this account', name: 'label', required: true, value: 'TeslaFi', hint: 'Your own name for it, shown on this page.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const tok = tokenField();
    const zone = zoneField(null);
    const r = await ask({
      title: 'Add a TeslaFi token', submit: 'Check with TeslaFi and add', extra: [label, tok, zone],
      intro: h('p', null, 'Only add a token of your own TeslaFi account. Anyone with it can read your car\'s data on TeslaFi, so it never leaves this server and no page shows it.'),
      run: async (cred, v) => {
        const body = { label: v.label.trim(), token: v.token.trim() };
        if (zoneOf(v.time_zone)) body.time_zone = zoneOf(v.time_zone);
        return ctx.api.post(`${BASE}/accounts`, withPw(cred, body));
      },
    });
    if (r) await reload(`${r.label} added. Match its car below to start reading it.`);
  }

  /* ---------- one account */
  function accountPanel(a) {
    const st = STATUS[a.status] || [a.status || 'Not known', 'off', ''];
    const err = a.last_error;
    const sv = status.find((s) => s.account === a.id) || {};
    const change = (title, intro, submit, bodyOf, { extra = [], msg = 'Saved.', danger = false } = {}) => ask({
      title, intro: intro ? h('p', null, intro) : null, submit, extra, danger,
      run: async (cred, v) => ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: a.version, ...bodyOf(v) })),
    }).then((r) => (r ? reload(msg) : null));

    const pauseBtn = btn(a.paused ? 'Resume' : 'Pause', () => change(a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`,
      a.paused ? 'Its matched car is read again from now on.' : 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile; the history already stored stays.',
      a.paused ? 'Resume' : 'Pause', () => ({ paused: !a.paused }), { msg: a.paused ? `${a.label} resumed.` : `${a.label} paused.` }),
    { sm: true, icon: a.paused ? 'live' : 'moon' });
    const tokenBtn = btn('Replace token', () => change(`New token for ${a.label}`, 'For a token TeslaFi stopped taking, or one you renewed on TeslaFi. If TeslaFi refuses it, nothing is changed.',
      'Check with TeslaFi and replace', (v) => ({ token: v.token.trim() }), { extra: [tokenField('New TeslaFi token')], msg: 'Token replaced and checked with TeslaFi.' }),
    { sm: true, icon: 'key', kind: a.status === 'token_refused' ? 'go' : null });
    const editBtn = btn('Name and time zone', () => {
      const lf = field({ label: 'Name', name: 'label', required: true, value: a.label, attrs: { maxlength: '64', autocomplete: 'off' } });
      return change(`Name and time zone of ${a.label}`, null, 'Save', (v) => ({ label: v.label.trim(), time_zone: zoneOf(v.time_zone) }), { extra: [lf, zoneField(a.time_zone)], msg: 'Saved.' });
    }, { sm: true, icon: 'sliders', kind: 'ghost' });
    const removeBtn = btn('Remove', async () => {
      const r = await ask({
        title: `Remove ${a.label}?`, danger: true, submit: 'Remove the account',
        intro: h('p', null, 'Reading stops at once and the sealed file with its token is deleted. The history already stored from it stays, marked as data from TeslaFi.'),
        run: async (cred) => ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred)),
      });
      if (r) await reload(`${a.label} removed. Its history stays.`);
    }, { sm: true, icon: 'x', kind: 'ghost' });

    const mode = MODE[sv.mode || a.mode] || [sv.mode || a.mode || 'Not known', 'off', ''];
    const carState = sv.car_state || (a.cars || []).find((c) => c.current)?.car_state;
    const errText = err ? [lastErrorText(err), err.at_ms ? ` · ${ago(err.at_ms)}` : ''] : null;
    return hud({ title: a.label, icon: 'key', id: `teslafi-${a.id}`, cls: 'tz-acc', label: `TeslaFi account ${a.label}`, side: [sig(st[1], st[0])] },
      h('div', { class: 'stack' },
        st[2] ? h('p', { class: ['tz-why', (a.status === 'token_refused' || a.status === 'error') && 'warn'] }, st[2]) : null,
        h('div', { class: 'tz-acc-grid' },
          kv([
            ['Added', dateTime(a.added_ms, ctx.tz)],
            ['Last OK', a.last_ok_ms ? h('span', { title: dateTime(a.last_ok_ms, ctx.tz) }, ago(a.last_ok_ms)) : h('span', { class: 'faint' }, 'none yet')],
            ['Last error', errText ? h('span', { class: 'warn', title: err.message || null }, errText) : h('span', { class: 'faint' }, 'none')],
            ['Polls today', `${formatNumber(sv.polls_today ?? a.polls_today ?? 0)}${ov.polls?.daily_cap ? ` of at most ${formatNumber(ov.polls.daily_cap)}` : ''}`],
            ['Doing now', h('span', { class: 'tz-inline', title: mode[2] }, sig(mode[1], mode[0]), carState ? h('span', { class: 'faint' }, `TeslaFi says ${carState}`) : null)],
            ['Next read', sv.next_request_ms ? `${time(sv.next_request_ms, ctx.tz)} (in ${dur(Math.max(0, sv.next_request_ms - ctx.api.now()))})` : h('span', { class: 'faint' }, DASH)],
            ['Time zone', a.time_zone ? code(a.time_zone) : h('span', { class: 'faint' }, 'the server\'s own')],
          ]),
          h('div', { class: 'stack' },
            h('div', { class: 'row' }, pauseBtn, tokenBtn, editBtn, removeBtn),
            sv.last_data_ms ? h('p', { class: 'tz-dim' }, `TeslaFi's newest poll handed on ${ago(sv.last_data_ms)}.`) : null)),
        carsTable(a)));
  }

  /* ---------- an account's cars: match each one */
  function carsTable(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    if (!items.length) return emptyState({ icon: 'car', title: 'No car yet', text: 'TeslaFi has named no car for this token yet. It shows here after TeslaFi\'s next poll of it.' });
    const takenBy = new Map();
    for (const o of accounts) if (o.id !== a.id) for (const c of carRows[o.id]?.items || o.cars || []) if (isMatched(c) && c.match !== 'new') takenBy.set(c.match, o.label);
    const free = cars.filter((c) => !takenBy.has(c.id)), taken = cars.filter((c) => takenBy.has(c.id));
    const opts = [['', 'Not decided (not read)'], ...free.map((c) => [c.id, c.name]), ['new', 'A new car'], ['leave_out', 'Leave out (not read)'],
      ...(taken.length ? [{ group: 'Matched on another TeslaFi account', items: taken.map((c) => [c.id, `${c.name} (${takenBy.get(c.id)})`]) }] : [])];
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
      const pre = c.match ?? c.suggested ?? '';
      const known = opts.some((o) => (o.group ? o.items.some(([v]) => v === pre) : o[0] === pre)) ? opts : [...opts, [pre, carName(pre)]];
      const sel = select(known, pre, sync, { label: `Car here for ${c.display_name || c.vehicle}`, cls: 'tz-sel' });
      sels.set(c.vehicle, sel);
      return h('li', { class: 'tz-car' },
        h('div', { class: 'tz-car-id' }, h('p', { class: 'tz-car-name' }, icon('car', 'i--sm'), h('b', null, c.display_name || 'No name at TeslaFi')),
          h('p', { class: 'tz-dim' }, [c.year, c.model].filter(Boolean).join(' ') || 'Model not known', ' · ', h('span', { class: 'mono' }, c.vin_prefix || DASH))),
        h('div', { class: 'tz-car-match' }, h('span', { class: 'tz-k' }, 'Car here'), sel,
          c.match == null && c.suggested ? h('span', { class: 'tz-sugg' }, tag('Suggested', 'grn'), ' same VIN start as ', carName(c.suggested), '; saved only with Save matches') : null),
        h('div', { class: 'tz-car-mode' }, h('span', { class: 'tz-k' }, 'TeslaFi says'),
          c.current ? sig('live', c.car_state || 'Not known') : h('span', { class: 'tz-dim' }, 'Not the car TeslaFi polls now')));
    });
    sync();
    return h('div', { class: 'tz-cars' },
      h('div', { class: 'tz-cars-head' }, h('p', { class: 'tz-k' }, `Its cars (${items.length})`), save),
      h('ul', { class: 'tz-car-list' }, rows),
      hint('A car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN start as a car here; nothing changes until you press Save matches.'));
  }

  return { el, load };
}
