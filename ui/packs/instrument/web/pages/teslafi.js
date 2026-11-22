// Settings: Integrations, the TeslaFi section (#teslafi): the TeslaFi input, for owners who pay for TeslaFi.
// This server reads TeslaFi's own polls of the car from TeslaFi's API with the owner's TeslaFi token, as a third
// live input beside SMO and the Tesla API. It is off until an admin adds a token; before that the server makes no
// connection to TeslaFi at all. It only reads: never a wake or a command, and no answer ever holds the token.
//
// Built like the Tesla account section (tesla-account.js): admins only, reading included; every change asks for
// the admin's own password again (and the two-step code) with askPassword(), sent as confirm_password and code.
//
// API: GET/PATCH integrations/teslafi, GET/POST integrations/teslafi/accounts,
// PATCH/DELETE integrations/teslafi/accounts/{id}, GET/PUT integrations/teslafi/accounts/{id}/cars,
// GET integrations/teslafi/status.

import { h, replace } from '../shared/dom.js';
import { icon } from '../core/icons.js';
import { kv, tag, pill, note, metric, figures } from '../core/page.js';
import { field } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { dateTime, ago, num, dur, time, tzName } from '../core/fmt.js';
import { askPassword } from './settings-server-move.js';

const enc = encodeURIComponent;
const BASE = 'integrations/teslafi';
// account times on this computer's clock, with the zone named (as the Tesla account section does)
const LOCAL = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
const at = (ms) => `${dateTime(ms, LOCAL)} ${tzName(LOCAL)}`;

/* ---------------------------------------------------------------------- words */

const ACCOUNT_STATUS = {
  checking: ['Checking', 'plain', 'Just added: the server is reading TeslaFi\'s latest poll.'],
  ok: ['Reading', 'live', 'TeslaFi takes the token; its matched car is read.'],
  paused: ['Paused', 'plain', 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile.'],
  token_refused: ['Token refused', 'warm', 'TeslaFi refused the token: a wrong token, or the TeslaFi subscription ended. Nothing is asked of TeslaFi until you give it a new token (Replace token).'],
  error: ['Not read', 'warm', 'TeslaFi answers with errors, or cannot be reached. See the last error; the server tries again by itself.'],
};

const CAR_MODE = {
  off: ['Not read', 'Not matched, left out, paused, or the token is refused.'],
  asleep: ['Asleep', 'TeslaFi says the car sleeps: read at the asleep pace.'],
  idle: ['Awake, parked', 'Read at the awake pace.'],
  charging: ['Charging', 'Read at the charging pace.'],
  driving: ['Driving', 'Read at the driving pace.'],
  backoff: ['Waiting after errors', 'TeslaFi answered with errors, asked to wait (Retry-After), or the day\'s requests are used up: nothing is asked until the next try.'],
};

// the pace choice; the waits each gives are what GET integrations/teslafi answers in `polls`
const PACE = [
  ['gentle', 'Gentle', 'Driving or charging every 2 min, awake every 5 min, asleep every 20 min.'],
  ['normal', 'Normal', 'Driving or charging every 60 s, awake every 3 min, asleep every 10 min.'],
  ['often', 'Often', 'As fast as TeslaFi itself refreshes: driving or charging every 15 s, awake every 60 s, asleep every 3 min.'],
];

const ERROR_WORDS = {
  teslafi_refused: 'TeslaFi did not take this token: check it on TeslaFi (Settings, TeslaFi API), and that the TeslaFi subscription is active. Nothing was kept.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was kept. Try again in a few minutes.',
  account_exists: 'This token\'s car is already read through another TeslaFi account here. Replace that account\'s token instead, or remove it first.',
  car_taken: 'That car is already matched to a car of another TeslaFi account here. Leave it out there first, or pick another car.',
  no_such_teslafi_account: 'This TeslaFi account is not here any more: another admin may have removed it. Nothing was changed; the page now shows the accounts there are.',
  version_conflict: 'Someone else changed this a moment ago, so nothing was saved. The page now shows their change: make yours again if it is still needed.',
};
const LAST_ERROR_WORDS = {
  token_refused: 'TeslaFi refused the token',
  teslafi_unreachable: 'TeslaFi could not be reached',
  rate_limited: 'TeslaFi asked the server to wait',
  daily_cap: 'The day\'s requests are used up',
  server_error: 'TeslaFi answered with a server error',
  timeout: 'TeslaFi took too long to answer',
};
const lastErrorText = (e) => (e ? (LAST_ERROR_WORDS[e.code] || e.message || e.code) : null);

function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (e.status === 409 && !ERROR_WORDS[e.code]) e.message = ERROR_WORDS.version_conflict;
  else if (ERROR_WORDS[e.code]) e.message = ERROR_WORDS[e.code];
  if (e.fields) { delete e.fields.confirm_password; delete e.fields.code; if (!Object.keys(e.fields).length) e.fields = null; }
  return e;
}
const isStale = (e) => e && (e.code === 'no_such_teslafi_account' || (e.status === 409 && !ERROR_WORDS[e.code]) || e.code === 'version_conflict');
const guarded = (fn) => async (...a) => {
  try { return await fn(...a); } catch (e) { words(e); if (isStale(e)) return { stale: e.message }; throw e; }
};
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const code = (t) => h('code', { class: 'si-code' }, t);
const secs = (s) => (s == null ? '—' : s >= 60 && s % 60 === 0 ? `${num(s / 60)} min` : `${num(s)} s`);
/** "driving or charging every 15 s, awake 1 min, asleep 3 min" from the overview's polls. */
export const paceWords = (p = {}) => {
  const live = p.driving_s === p.charging_s ? `driving or charging every ${secs(p.driving_s)}` : `driving every ${secs(p.driving_s)}, charging ${secs(p.charging_s)}`;
  return `${live}, awake ${secs(p.parked_s)}, asleep ${secs(p.asleep_s)}`;
};

let zoneOpts = null;
function zones() {
  if (!zoneOpts) {
    let list = [];
    try { list = Intl.supportedValuesOf('timeZone'); } catch { list = [LOCAL]; }
    zoneOpts = [['', 'The server\'s default zone'], ...list.map((z) => [z, z.replace(/_/g, ' ')])];
  }
  return zoneOpts;
}
const zoneField = (value) => field({ label: 'Time zone (optional)', name: 'time_zone', path: 'time_zone', type: 'select', value: value || '', options: zones(),
  hint: 'The zone TeslaFi shows this account\'s times in (TeslaFi, Settings, Time Zone). Leave it at the default if unsure.' });

/* ---------------------------------------------------------------------- the section */

/**
 * The TeslaFi section. ctx: the page's; cars: Sentry Mate's cars ({ id, name, ... }) for the match picker.
 * Returns { el, load() }.
 */
export function teslafiSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const body = h('div', { class: 'ta-body' });
  const headPill = h('span', { class: 'ta-head-pill' });
  const el = h('section', { class: 'si-section ta', id: 'teslafi', 'aria-label': 'TeslaFi' },
    h('div', { class: 'si-section-head' }, h('h2', { class: 'si-h2' }, icon('share'), 'TeslaFi'), headPill),
    h('p', { class: 'note' }, 'For owners who pay for TeslaFi: this server reads TeslaFi\'s own polls of the car with your TeslaFi token, as a third live input beside SMO and the Tesla API. It only reads: it never wakes the car or sends a command, and these reads do not count against TeslaFi\'s command limit. Where SMO or the Tesla API cover the same time, theirs win.'),
    body);

  let ov = null, accounts = [], status = [], carRows = {};
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  async function load() {
    if (!admin) {
      replace(headPill, pill('Admins only'));
      replace(body, h('div', { class: 'panel panel-pad' }, note(icon('lock', 'ico--sm'), ' Only an admin sees and changes the TeslaFi accounts: their status, their cars and how often TeslaFi is asked.')));
      return;
    }
    try {
      [ov, accounts, status] = await Promise.all([
        ctx.api.get(BASE),
        ctx.api.list(`${BASE}/accounts`).all().catch(() => []),
        ctx.api.list(`${BASE}/status`).all().catch(() => []),
      ]);
      const rows = await Promise.all(accounts.map((a) => ctx.api.get(`${BASE}/accounts/${enc(a.id)}/cars`).catch(() => null)));
      carRows = Object.fromEntries(accounts.map((a, i) => [a.id, rows[i] || { items: a.cars || [], version: a.version }]));
    } catch (e) {
      if (e?.name === 'AbortError') return;
      replace(headPill, pill('Not read', { tone: 'warm' }));
      replace(body, h('div', { class: 'panel panel-pad' }, h('p', { class: 'warn' }, icon('warn', 'ico--sm'), ' TeslaFi could not be read: ', errorWords(e).text)));
      return;
    }
    if (!ctx.alive()) return;
    paint();
  }
  const after = async (r, msg) => {
    if (!r) return;
    if (r.stale) { flash(r.stale, { kind: 'info' }); await load(); return; }
    if (msg) flash(typeof msg === 'function' ? msg(r) : msg);
    await load();
  };

  const isMatched = (c) => c.match != null && c.match !== 'leave_out';
  function carCounts() {
    let matched = 0, read = 0;
    for (const a of accounts) {
      const n = (carRows[a.id]?.items || []).filter(isMatched).length;
      matched += n;
      if (n && a.status === 'ok' && a.mode && a.mode !== 'off') read += n;
    }
    return { matched, read };
  }
  const cars_ = (n) => `${num(n)} ${n === 1 ? 'car' : 'cars'}`;

  function paint() {
    const { matched, read } = carCounts();
    replace(headPill, !ov.enabled ? pill('Off until a token is added', { lg: true })
      : read ? pill(`Reading ${cars_(read)}`, { tone: 'live', lg: true })
        : matched && accounts.every((a) => a.paused) ? pill('Paused: no car is read', { lg: true })
          : matched ? pill('No car is read now', { tone: 'warm', lg: true }) : pill('No car matched yet', { tone: 'warm', lg: true }));
    if (!accounts.length) {
      replace(body, h('div', { class: 'ta-grid' },
        h('div', { class: 'panel panel-pad ta-status' },
          emptyState({ icon: 'share', title: 'No TeslaFi token added', text: 'With a TeslaFi token, this server reads TeslaFi\'s own polls of your car; until one is added it makes no connection to TeslaFi at all.' })),
        addPanel()));
      return;
    }
    replace(body, h('div', { class: 'ta-grid' }, statusPanel(), addPanel()), accounts.map(accountPanel));
  }

  /* ---------- the input as a whole: state, hosts, pace */
  function statusPanel() {
    const p = ov.polls || {};
    const poll = ov.poll || 'normal';
    const { matched, read } = carCounts();
    const paused = accounts.filter((a) => a.paused).length;
    const cap = (p.daily_cap || 1500) * Math.max(1, ov.accounts || 0);
    const paceSeg = h('div', { class: 'seg', role: 'group', 'aria-label': 'How often TeslaFi is asked' },
      PACE.map(([v, l, why]) => h('button', { class: 'seg-btn', type: 'button', title: why, 'aria-pressed': String(poll === v), onclick: () => setPace(v) }, l)));
    return h('div', { class: 'panel panel-pad ta-status' },
      h('div', { class: 'panel-head' }, h('h3', { class: 'panel-title' }, icon('pulse'), 'Status')),
      figures([
        metric('State', ov.enabled ? 'On' : 'Off', null, { size: 'sm', explain: `${num(ov.accounts)} ${ov.accounts === 1 ? 'account' : 'accounts'}${paused ? ` (${num(paused)} paused)` : ''}, ${read ? `${cars_(read)} read` : 'no car read now'}${matched > read ? `, ${num(matched)} matched` : ''}.` }),
        metric('Requests today', ov.polls_today, 'number', { size: 'sm', digits: 0, explain: `Of at most ${num(cap)} (${num(p.daily_cap || 1500)} a day per account).` }),
        metric('Connections set up', ov.clients_built, 'number', { size: 'sm', digits: 0, explain: 'To TeslaFi, since the server started.' }),
      ], 'si-figs'),
      ov.last_error ? h('p', { class: 'si-last-error warn' }, icon('warn', 'ico--sm'), ' Last error: ', h('b', null, lastErrorText(ov.last_error)),
        ov.last_error.at_ms ? `, ${at(ov.last_error.at_ms)} (${ago(ov.last_error.at_ms)})` : '', '.') : h('p', { class: 'si-last-error faint' }, icon('check', 'ico--sm'), ' No error.'),
      h('div', { class: 'ta-hosts' }, h('span', { class: 'metric-label' }, 'TeslaFi host it talks to'),
        h('ul', { class: 'ta-host-list' }, (ov.hosts || []).map((x) => h('li', null, code(x)))),
        ov.tls ? h('p', { class: 'faint' }, `Over TLS ${ov.tls}, the token in a header, never in an address.`) : null),
      h('div', { class: 'ta-pace' },
        h('div', { class: 'ta-pace-head' }, h('span', { class: 'metric-label' }, 'How often TeslaFi is asked'), paceSeg),
        h('p', { class: 'ta-pace-now' }, 'Now: ', h('b', null, paceWords(p)), '.'),
        h('dl', { class: 'ta-pace-list' },
          [['A driving car', p.driving_s], ['A charging car', p.charging_s], ['A parked, awake car', p.parked_s], ['An asleep car', p.asleep_s], ['Any car while SMO or the Tesla API is live', p.other_live_s]]
            .map(([l, s]) => h('div', null, h('dt', null, l), h('dd', null, `every ${secs(s)}`)))),
        h('p', { class: 'setting-note' }, `${PACE.find((x) => x[0] === poll)?.[2] || ''} In every pace TeslaFi's Retry-After and its errors are always respected, and at most ${num(p.daily_cap || 1500)} requests a day go to TeslaFi per account.`)));
  }
  async function setPace(v) {
    if (v === (ov.poll || 'normal')) return;
    const pc = PACE.find((x) => x[0] === v);
    const r = await askPassword(ctx, {
      title: `Pace: ${pc[1]}`,
      intro: h('p', { class: 'note' }, `How often the server reads TeslaFi's polls. ${pc[2]} TeslaFi refreshes its own data about once a minute while the car is awake, so a faster pace mostly shortens the wait for a new poll.`),
      submit: 'Change the pace',
      run: guarded((cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v }))),
    });
    await after(r, 'How often TeslaFi is asked: saved.');
  }

  /* ---------- adding a token */
  function addPanel() {
    return h('div', { class: 'panel panel-pad ta-add' },
      h('div', { class: 'panel-head' }, h('h3', { class: 'panel-title' }, icon('plus'), accounts.length ? 'Add another TeslaFi token' : 'Add a TeslaFi token')),
      h('ol', { class: 'ta-step-list' },
        h('li', null, 'On TeslaFi, open ', h('b', null, 'Settings'), ', then ', h('b', null, 'TeslaFi API'), ', and copy your token.'),
        h('li', null, 'Paste it here with a name, and the time zone TeslaFi shows your times in.'),
        h('li', null, 'Match its car to a car here: until then nothing is read.')),
      h('button', { class: 'btn btn--primary tf-add-btn', type: 'button', onclick: addToken }, icon('lock', 'ico--sm'), 'Add a TeslaFi token'),
      h('p', { class: 'setting-note' }, 'The server checks the token with TeslaFi (this can take about 10 s) before anything is kept, seals it, and never shows it again.'));
  }

  async function addToken() {
    const label = field({ label: 'Name for this account', name: 'label', required: true, value: 'TeslaFi', hint: 'Your own name for it, shown on this page.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const tok = field({ label: 'TeslaFi token', name: 'token', path: 'token', type: 'password', required: true, hint: 'From TeslaFi, Settings, TeslaFi API. Checked with TeslaFi before it is kept.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
    const zone = zoneField(null);
    const r = await askPassword(ctx, {
      title: 'Add a TeslaFi token', submit: 'Check with TeslaFi and add', extra: [label, tok, zone],
      intro: h('p', { class: 'note' }, 'Only paste a token of your own TeslaFi account. Anyone with it can read the account\'s car, so it never leaves this server and no page shows it.'),
      run: guarded((cred, v) => ctx.api.post(`${BASE}/accounts`, withPw(cred, { label: v.label.trim(), token: v.token.trim(), ...(v.time_zone ? { time_zone: v.time_zone } : {}) }))),
    });
    await after(r, (x) => `${x.label} added. Match its car below to start reading it.`);
  }

  /* ---------- one account */
  function accountPanel(a) {
    const st = ACCOUNT_STATUS[a.status] || [a.status, 'plain', ''];
    const err = a.last_error;
    const sv0 = status.find((s) => s.account === a.id) || {};
    const mode = CAR_MODE[sv0.mode || a.mode] || [sv0.mode || a.mode || '—', ''];
    const patch = (title, intro, submit, bodyOf, extra = [], msg = 'Saved.', danger = false) => async () => {
      const r = await askPassword(ctx, {
        title, intro: intro ? h('p', { class: 'note' }, intro) : null, submit, extra, danger,
        run: guarded((cred, v) => ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: a.version, ...bodyOf(v) }))),
      });
      await after(r, msg);
    };
    const pauseBtn = h('button', { class: 'btn btn--sm', type: 'button', onclick: patch(a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`,
      a.paused ? 'Its matched car is read again from now on.' : 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile; the history already stored stays.',
      a.paused ? 'Resume' : 'Pause', () => ({ paused: !a.paused }), [], a.paused ? `${a.label} resumed.` : `${a.label} paused.`) }, icon(a.paused ? 'live' : 'moon', 'ico--sm'), a.paused ? 'Resume' : 'Pause');
    const editBtn = h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => {
      const f = field({ label: 'Name', name: 'label', required: true, value: a.label, attrs: { maxlength: '64', autocomplete: 'off' } });
      const z = zoneField(a.time_zone);
      return patch(`Change ${a.label}`, 'After a change of time zone TeslaFi\'s times are read in the new zone at once.', 'Save', (v) => ({ label: v.label.trim(), time_zone: v.time_zone || null }), [f, z], 'Saved.')();
    } }, icon('settings', 'ico--sm'), 'Name and zone');
    const tokenBtn = h('button', { class: ['btn btn--sm', a.status === 'token_refused' ? 'btn--primary' : 'btn--ghost'], type: 'button', onclick: () => {
      const f = field({ label: 'New TeslaFi token', name: 'token', path: 'token', type: 'password', required: true, hint: 'From TeslaFi, Settings, TeslaFi API. Checked with TeslaFi before it replaces the old one.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
      return patch(`New token for ${a.label}`, 'For a token TeslaFi refused, or one you made new on TeslaFi. The old token is deleted. If TeslaFi refuses the new one, nothing changes.', 'Check with TeslaFi and replace', (v) => ({ token: v.token.trim() }), [f], 'Token replaced and checked with TeslaFi.')();
    } }, icon('lock', 'ico--sm'), 'Replace token');
    const removeBtn = h('button', { class: 'btn btn--sm btn--ghost ta-remove', type: 'button', onclick: async () => {
      const r = await askPassword(ctx, {
        title: `Remove ${a.label}?`, danger: true, submit: 'Remove the account',
        intro: h('p', { class: 'note' }, 'Reading stops at once and the sealed file with its token is deleted. The history already stored from it stays, marked as data from TeslaFi.'),
        run: guarded((cred) => ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred))),
      });
      await after(r, `${a.label} removed. Its history stays.`);
    } }, icon('x', 'ico--sm'), 'Remove');

    return h('div', { class: 'panel panel-pad ta-account', id: `teslafi-${a.id}` },
      h('div', { class: 'ta-acc-head' },
        h('div', { class: 'ta-acc-title' }, h('h3', null, a.label), pill(st[0], { tone: st[1] }), h('span', { class: 'faint' }, a.time_zone ? a.time_zone.replace(/_/g, ' ') : 'Server\'s default zone')),
        h('div', { class: 'row ta-acc-acts' }, pauseBtn, tokenBtn, editBtn, removeBtn)),
      st[2] ? h('p', { class: ['setting-note', a.status === 'token_refused' || a.status === 'error' ? 'warn' : null] }, st[2]) : null,
      kv([
        ['What the server does', h('span', { title: mode[1] }, h('b', null, mode[0]), sv0.car_state ? h('span', { class: 'faint' }, ` (TeslaFi says ${sv0.car_state.toLowerCase()})`) : null), { icon: 'globe' }],
        ['Added', at(a.added_ms), { icon: 'calendar' }],
        ['Last good answer', a.last_ok_ms ? h('span', { title: at(a.last_ok_ms) }, ago(a.last_ok_ms)) : h('span', { class: 'faint' }, 'none yet'), { icon: 'check' }],
        ['Last error', err ? h('span', { class: 'warn' }, lastErrorText(err), err.message && LAST_ERROR_WORDS[err.code] ? h('span', { class: 'faint' }, ` (${err.message})`) : null, err.at_ms ? h('span', { title: at(err.at_ms) }, `, ${ago(err.at_ms)}`) : '') : h('span', { class: 'faint' }, 'none'), { icon: 'warn' }],
        ['Requests today', `${num(a.polls_today ?? 0)} of ${num(ov.polls?.daily_cap || 1500)}`, { icon: 'pulse' }],
        ['Host', h('span', { class: 'ta-inline-hosts' }, (a.hosts || []).map((x) => code(x)), a.tls ? h('span', { class: 'faint' }, ` TLS ${a.tls}`) : null), { icon: 'globe' }],
      ]),
      carsTable(a));
  }

  /* ---------- an account's cars: match each one */
  function carsTable(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    if (!items.length) return emptyState({ icon: 'car', title: 'No car yet', text: 'TeslaFi\'s polls name no car yet. It shows here once TeslaFi polls it.' });
    const takenBy = new Map();
    for (const o of accounts) {
      if (o.id === a.id) continue;
      for (const c of carRows[o.id]?.items || []) if (c.match && c.match !== 'new' && c.match !== 'leave_out') takenBy.set(c.match, o.label);
    }
    const opts = [['', 'Not decided (not read)'], ...cars.map((c) => [c.id, takenBy.has(c.id) ? `${c.name} (read through ${takenBy.get(c.id)})` : c.name, takenBy.has(c.id)]), ['new', 'A new car'], ['leave_out', 'Leave out (not read)']];
    const sels = new Map();
    const saveBtn = h('button', { class: 'btn btn--primary btn--sm', type: 'button', hidden: true }, icon('check', 'ico--sm'), 'Save matches');
    const problem = h('p', { class: 'warn ta-match-warn', role: 'alert', hidden: true });
    const changed = () => items.filter((c) => (sels.get(c.vehicle).value || null) !== (c.match ?? null));
    const doubled = () => {
      const seen = new Map();
      for (const c of items) {
        const v = sels.get(c.vehicle).value;
        if (!v || v === 'new' || v === 'leave_out') continue;
        seen.set(v, [...(seen.get(v) || []), c]);
      }
      return [...seen].filter(([, list]) => list.length > 1);
    };
    const sync = () => {
      const dup = doubled();
      const bad = new Set(dup.flatMap(([, list]) => list.map((c) => c.vehicle)));
      for (const [veh, sel] of sels) { sel.classList.toggle('is-invalid', bad.has(veh)); sel.setAttribute('aria-invalid', String(bad.has(veh))); }
      replace(problem, dup.length ? [icon('warn', 'ico--sm'), ' ', dup.map(([car, list]) => `${list.map((c) => c.display_name || c.vehicle).join(' and ')} are both matched to ${carName(car)}.`).join(' '), ' One car here takes one TeslaFi car: pick another car, A new car, or Leave out.'] : null);
      problem.hidden = !dup.length;
      saveBtn.hidden = !changed().length;
      saveBtn.disabled = !!dup.length;
    };
    saveBtn.addEventListener('click', async () => {
      const ch = changed();
      if (!ch.length || doubled().length) return;
      const r = await askPassword(ctx, {
        title: `Match the cars of ${a.label}`, submit: 'Save matches',
        intro: h('ul', { class: 'si-list' }, ch.map((c) => { const v = sels.get(c.vehicle).value; return h('li', null, h('b', null, c.display_name || c.vehicle), ': ', v === 'new' ? 'a new car here, read from now on' : v === 'leave_out' ? 'left out, not read' : !v ? 'not decided, not read' : `${carName(v)}, read from now on`); })),
        run: guarded((cred) => ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, withPw(cred, { version: data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: sels.get(c.vehicle).value || null })) }))),
      });
      await after(r, 'Matches saved.');
    });
    const rows = items.map((c) => {
      const sv = status.find((s) => s.account === a.id && s.vehicle === c.vehicle) || {};
      const pre = c.match ?? c.suggested ?? '';
      const known = opts.some(([v]) => v === pre) ? opts : [...opts, [pre, carName(pre)]];
      const start = c.match == null && takenBy.has(pre) ? '' : pre;
      const sel = h('select', { class: 'select', 'aria-label': `Sentry Mate car for ${c.display_name || c.vehicle}` },
        known.map(([v, l, taken]) => h('option', { value: v, selected: v === start, disabled: taken && v !== c.match ? true : null }, l)));
      sel.addEventListener('change', sync);
      sels.set(c.vehicle, sel);
      const m = c.current && sv.mode ? (CAR_MODE[sv.mode] || [sv.mode, '']) : isMatched(c) ? ['Not polled now', 'TeslaFi polls another car of this account now.'] : CAR_MODE.off;
      const next = c.current ? sv.next_request_ms ?? null : null;
      const tz = cars.find((x) => x.id === c.match)?.time_zone || ctx.tz;
      return h('tr', null,
        h('td', null, h('span', { class: 'cell-text' }, h('b', null, c.display_name || 'No name at TeslaFi'), h('span', { class: 'sub' }, [c.year, c.model].filter(Boolean).join(' '), ' · ', h('span', { class: 'mono' }, c.vin_prefix || '—')))),
        h('td', null, h('div', { class: 'ta-match' }, sel, c.match == null && c.suggested && !takenBy.has(c.suggested) ? tag('Suggested: same VIN prefix', 'live') : null)),
        h('td', null, h('span', { class: 'ta-cell-label' }, 'TeslaFi says: '), c.car_state || '—'),
        h('td', null, h('span', { class: 'cell-text', title: m[1] }, h('span', null, m[0]),
          sv.mode === 'backoff' && next ? h('span', { class: 'sub warn' }, `Next try ${time(next, tz)} (in ${dur(Math.max(0, next - ctx.api.now()))})`)
            : next && sv.mode !== 'off' ? h('span', { class: 'sub' }, `Next read ${time(next, tz)}`) : h('span', { class: 'sub' }, m[1]))),
        h('td', { class: 'num' }, c.current && sv.last_data_ms ? h('span', { title: dateTime(sv.last_data_ms, tz) }, h('span', { class: 'ta-cell-label' }, 'Last data '), ago(sv.last_data_ms)) : h('span', null, h('span', { class: 'ta-cell-label' }, 'Last data '), h('span', { class: 'faint' }, 'none yet'))));
    });
    return h('div', { class: 'ta-cars' },
      h('div', { class: 'ta-cars-head' }, h('span', { class: 'metric-label' }, `Its cars (${items.length})`), saveBtn),
      problem,
      h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl ta-tbl' },
        h('thead', null, h('tr', null, ['Car at TeslaFi', 'Sentry Mate car', 'TeslaFi says', 'What the server does', 'Last data'].map((t, i) => h('th', { class: i === 4 ? 'num' : null, scope: 'col' }, t)))),
        h('tbody', null, rows))),
      h('p', { class: 'setting-note' }, 'A car gets TeslaFi\'s data only once it is matched to a car here or to a new car. A suggestion has the same VIN prefix as a car here; it is not saved until you press Save matches.'));
  }

  return { el, load };
}
