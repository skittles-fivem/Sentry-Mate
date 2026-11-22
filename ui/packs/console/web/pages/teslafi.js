// settings/integrations, the teslafi block (#teslafi): the TeslaFi input, for owners who pay for TeslaFi. This
// server reads TeslaFi's own polls of the car from TeslaFi's API with the owner's TeslaFi token, as a third live
// input beside SMO and the Tesla API. It is off until an admin adds a token; before that the server makes no
// connection to TeslaFi at all. It only reads: never a wake or a command, and no answer ever holds the token.
//
// Admins only, reading included. Every change asks for the admin's own password again (and the two-step code when
// the account has it): askPassword() from core/secret.js, sent as confirm_password and code. Built like the tesla
// account block (pages/tesla.js); each account block has the id teslafi-<account>.
//
// API: GET/PATCH integrations/teslafi, GET/POST integrations/teslafi/accounts,
// PATCH/DELETE integrations/teslafi/accounts/{id}, GET/PUT integrations/teslafi/accounts/{id}/cars,
// GET integrations/teslafi/status.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { section, kv, btn, note } from '../core/ui.js';
import { table } from '../core/table.js';
import { field } from '../core/form.js';
import { errorWords } from '../core/states.js';
import { tag, dateTime, time, ago, dur, DASH } from '../core/fmt.js';
import { toast } from '../core/toast.js';
import { askPassword } from '../core/secret.js';

const enc = encodeURIComponent;
const BASE = 'integrations/teslafi';

/* ---------------------------------------------------------------------- words */

// account status: [tag word, tone, what it means]
const ACCOUNT_STATUS = {
  checking: ['checking', 'muted', 'Just added: the server is reading TeslaFi\'s latest poll.'],
  ok: ['reading', 'ok', 'TeslaFi takes its token; its matched car is read.'],
  paused: ['paused', 'muted', 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile.'],
  token_refused: ['token refused', 'warn', 'TeslaFi does not take its token any more (a wrong token, or the TeslaFi subscription ended), so its car is not read. Replace its token. Nothing is asked of TeslaFi meanwhile.'],
  error: ['error', 'bad', 'TeslaFi answers with errors, or cannot be reached. See the last error.'],
};

// what the server does for an account's car: [words, why]
const CAR_MODE = {
  off: ['not read', 'Not matched, left out, paused, or its token is refused.'],
  asleep: ['asleep', 'TeslaFi says the car sleeps: read at the asleep pace.'],
  idle: ['awake, parked', 'Awake and parked: read at the awake pace.'],
  charging: ['charging', 'Read at the charging pace.'],
  driving: ['driving', 'Read at the driving pace.'],
  backoff: ['waiting after errors', 'TeslaFi answered with errors, asked the server to wait, or the day\'s 1,500 requests are used up: nothing is asked until the next try. Errors wait 60 s, doubling up to 15 minutes; TeslaFi\'s own wait (Retry-After) is always kept.'],
};
const PACE = [
  ['gentle', 'gentle', 'driving or charging every 2 min, awake 5 min, asleep 20 min'],
  ['normal', 'normal', 'driving or charging every 1 min, awake 3 min, asleep 10 min'],
  ['often', 'often', 'as fast as TeslaFi itself refreshes: driving or charging every 15 s, awake 1 min, asleep 3 min'],
];

// error codes of a change, in words
const ERROR_WORDS = {
  teslafi_refused: 'TeslaFi did not take this token: check it on TeslaFi (Settings, TeslaFi API), and that the TeslaFi subscription is active. Nothing was kept.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was kept. Try again in a few minutes.',
  account_exists: 'The car of this token is already read through another TeslaFi account here. Replace that account\'s token instead, or remove it first.',
  car_taken: 'This car is already matched to a car of another TeslaFi account here. Leave it out there first, or pick another car.',
  no_such_teslafi_account: 'This TeslaFi account is not here any more: another admin may have removed it. The page shows the accounts there are now.',
  version_conflict: 'Someone else changed the TeslaFi input a moment ago. The page shows their change now; make yours again.',
};
// last_error codes of an account or the whole input
const LAST_ERROR_WORDS = {
  token_refused: 'TeslaFi refused the token',
  teslafi_refused: 'TeslaFi refused the token',
  teslafi_unreachable: 'TeslaFi could not be reached',
  rate_limited: 'TeslaFi asked the server to wait',
  daily_cap: 'the day\'s 1,500 requests are used up',
  server_error: 'TeslaFi answered with a server error',
  timeout: 'TeslaFi took too long to answer',
};
const lastErrorText = (e) => (e ? (LAST_ERROR_WORDS[e.code] || e.message || e.code) : null);

/** Our words on an error of a change. Mutates and returns it; the password dialog handles its own codes. */
function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (ERROR_WORDS[e.code]) e.message = ERROR_WORDS[e.code];
  else if (e.status === 409) e.message = ERROR_WORDS.version_conflict;
  if (e.fields) {
    delete e.fields.confirm_password; delete e.fields.code;
    if (!Object.keys(e.fields).length) e.fields = null;
  }
  return e;
}
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const code = (t) => h('span', { class: 'prompt' }, t);
const wait = (s) => (s == null ? DASH : s >= 60 && s % 60 === 0 ? `${formatNumber(s / 60)} min` : `${formatNumber(s)} s`);
const lines = (rows) => h('ul', { class: 'ln-list' }, rows.filter(Boolean).map(([t, text]) => h('li', null, t, h('span', { class: 'ln-list-t' }, text))));

// the time zones this browser knows, for the time zone picker ('' is the server's default)
let ZONES = null;
const zones = () => (ZONES ??= (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })());
function zoneField(value, hint) {
  const list = zones();
  const opts = [['', 'the server\'s default'], ...(value && !list.includes(value) ? [[value, value]] : []), ...list.map((z) => [z, z])];
  return field({ label: 'time zone', name: 'time_zone', type: 'select', value: value || '', options: opts, hint });
}

export const teslafiHref = () => `${state.buildUrl('settings-integrations')}#teslafi`;

/* ---------------------------------------------------------------------- the block */

/**
 * The teslafi block. cars: Sentry Mate's cars ({ id, name, time_zone }) for the match picker.
 * Returns { el, side, load() }: load reads everything and paints (the page awaits it before ready()).
 */
export function teslafiSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const side = h('span');
  const body = h('div', { 'aria-live': 'polite' });
  const el = section({ title: 'teslafi', id: 'teslafi', side },
    note('For owners who pay for TeslaFi: this server reads TeslaFi\'s own polls of the car (about one a minute while it is awake) with your TeslaFi token, as a third live input beside SMO and the Tesla API. It only reads: never a wake or a command, and the reads do not count against TeslaFi\'s command limit. No connection to TeslaFi is made until a token is added. Where SMO or the Tesla API cover the same time, theirs win.'),
    body);

  let ov = null, accounts = [], status = [], carRows = {};
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;
  const tables = [];

  async function load() {
    if (!admin) {
      replace(side, tag('admins only', 'muted'));
      replace(body, note(tag('admin', 'muted'), ' Only an admin sees and changes the TeslaFi input: its token, its car and how often TeslaFi is asked. Every page still says where its data came from.'));
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
      replace(side, tag('not read', 'bad'));
      replace(body, h('p', { class: 'note tone--bad' }, 'The TeslaFi input could not be read: ', errorWords(e).text));
      return;
    }
    if (!ctx.alive()) return;
    paint();
    await Promise.all(tables.map((t) => t.ready)).catch(() => {});
  }
  let pending = null;
  const refresh = () => (pending = load().catch(() => {}));
  const reload = async (msg) => { if (msg) toast(msg); await refresh(); };

  // a change repaints the block: the focus goes back to the button with the same data-fk, else a fallback
  const fk = (el, key) => { el.dataset.fk = key; return el; };
  const shown = (el) => el && el.isConnected && !el.hidden && !el.disabled && (el.checkVisibility ? el.checkVisibility() : true);
  function refocus(keys) {
    const now = document.activeElement;
    if (now && now !== document.body && now.isConnected) return;
    for (const k of keys) {
      const el = body.querySelector(`[data-fk="${CSS.escape(k)}"]`);
      if (shown(el)) { el.focus({ preventScroll: true }); return; }
    }
  }
  const keep = (keys, fn) => async (...args) => {
    try { return await fn(...args); } finally { await pending; if (ctx.alive()) refocus([...keys, 'teslafi-top']); }
  };
  const stale = (e) => { if (e && (e.code === 'version_conflict' || e.code === 'no_such_teslafi_account' || (e.status === 409 && !ERROR_WORDS[e.code]))) refresh(); return e; };
  const fail = (e) => stale(words(e));

  function paint() {
    tables.length = 0;
    replace(side, !ov.enabled ? tag('off until a token is added', 'muted')
      : ov.cars ? tag(`reading ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'}`, 'ok') : tag('no car matched yet', 'warn'));
    if (!accounts.length) {
      replace(body, h('div', { class: 'empty' }, h('p', { class: 'empty-line' }, '-- no teslafi token --'),
        h('p', { class: 'empty-hint' }, 'Add your TeslaFi token and the server reads TeslaFi\'s own polls of your car with it. Until then no connection to TeslaFi is made.')), addBlock());
      return;
    }
    replace(body, statusBlock(), accounts.map(accountBlock), addBlock());
  }

  /* ---------- the input as a whole, and its pace */
  function statusBlock() {
    const p = ov.polls || {};
    const poll = ov.poll || 'normal';
    const cap = (p.daily_cap || 1500) * Math.max(1, ov.accounts || 0);
    const pace = h('div', { class: 'keys', role: 'group', 'aria-label': 'How often TeslaFi is asked' },
      PACE.map(([v, l, why]) => h('button', { class: 'key', type: 'button', title: why, 'aria-pressed': String(poll === v), dataset: { fk: `fi-pace:${v}` }, onclick: keep([`fi-pace:${v}`], () => setPace(v)) }, l)));
    return h('div', null,
      h('p', { class: 'sub-title', tabindex: '-1', dataset: { fk: 'teslafi-top' } }, 'status'),
      kv([
        ['state', [tag('on', 'ok'), ` ${formatNumber(ov.accounts)} ${ov.accounts === 1 ? 'token' : 'tokens'}, ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'} read`]],
        ['requests today', [formatNumber(ov.polls_today ?? 0), h('span', { class: 'muted' }, ` of at most ${formatNumber(cap)} (1,500 a day per token)`)]],
        ['last error', ov.last_error ? [lastErrorText(ov.last_error), ov.last_error.at_ms ? h('span', { class: 'muted' }, ` ${dateTime(ov.last_error.at_ms, ctx.tz)} (${ago(ov.last_error.at_ms)})`) : null] : h('span', { class: 'muted' }, 'none'), { tone: ov.last_error ? 'warn' : null }],
        ['teslafi host', ov.hosts?.length ? h('span', { class: 'tesla-hosts' }, ov.hosts.map((x) => code(x))) : DASH],
        ['tls', ov.tls ? `tls ${ov.tls}` : DASH],
      ], { cols: 1 }),
      h('p', { class: 'sub-title' }, 'how often teslafi is asked'),
      pace,
      kv([
        ['driving', `every ${wait(p.driving_s)}`],
        ['charging', `every ${wait(p.charging_s)}`],
        ['parked, awake', `every ${wait(p.parked_s)}`],
        ['asleep', `every ${wait(p.asleep_s)}`],
        ['SMO or tesla api live', [`every ${wait(p.other_live_s)}`, h('span', { class: 'muted' }, ' at most, while another input sends the car live')]],
      ]),
      note('Often is as fast as TeslaFi itself refreshes. In every pace TeslaFi\'s own waits (Retry-After) and errors are kept, at most one request goes out every 10 s per token, and the car is never woken: these reads come from TeslaFi\'s database.'));
  }
  async function setPace(v) {
    if (v === (ov.poll || 'normal')) return;
    const why = PACE.find((x) => x[0] === v)?.[2] || '';
    const r = await askPassword(ctx, {
      title: `teslafi pace: ${v}`, submit: 'change the pace',
      intro: note(`How often the server reads TeslaFi's polls: ${why}. A faster pace gives finer drives and charges, and uses more of the 1,500 requests a day.`),
      run: async (cred) => { try { return await ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v })); } catch (e) { throw fail(e); } },
    });
    if (r) await reload(`TeslaFi pace saved: ${v}.`);
  }

  /* ---------- adding a token */
  function addBlock() {
    return h('div', null,
      h('p', { class: 'sub-title' }, accounts.length ? 'add another teslafi token' : 'add a teslafi token'),
      lines([
        [tag('token', 'accent'), [h('b', null, 'Paste your TeslaFi token. '), 'Find it on TeslaFi: Settings, TeslaFi API. The server checks it with TeslaFi (one read, about 10 s) before anything is kept, seals it, and never shows it again.',
          h('div', { class: 'acts' }, fk(btn('add a teslafi token', keep(['fi-add'], () => addToken()), { kind: 'primary' }), 'fi-add'))]],
      ]),
      note('Its car is not read until you match it to a car here, to a new car, or leave it out.'));
  }

  async function addToken() {
    const label = field({ label: 'name', name: 'label', required: true, value: 'TeslaFi', hint: 'Your own name for it, shown on this page.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const tok = field({ label: 'token', name: 'token', path: 'token', type: 'password', required: true, hint: 'From TeslaFi: Settings, TeslaFi API. Checked with TeslaFi before it is kept.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
    const zone = zoneField('', 'The zone TeslaFi shows your times in (TeslaFi, Settings). Leave it at the server\'s default if they are the same.');
    const r = await askPassword(ctx, {
      title: 'add a teslafi token', submit: 'check with teslafi and add', extra: [label, tok, zone],
      intro: note('Only paste a token of your own TeslaFi account. It never leaves this server except to TeslaFi, and no page shows it.'),
      run: async (cred, v) => {
        const b = { label: v.label.trim(), token: v.token.trim(), ...(v.time_zone ? { time_zone: v.time_zone } : {}) };
        try { return await ctx.api.post(`${BASE}/accounts`, withPw(cred, b)); } catch (e) { throw fail(e); }
      },
    });
    if (r) await reload(`${r.label || 'The TeslaFi token'} added. Match its car below to start reading it.`);
  }

  /* ---------- one account */
  function accountBlock(a) {
    const st = ACCOUNT_STATUS[a.status] || [a.status, 'muted', ''];
    const err = a.last_error;
    const cur = () => accounts.find((x) => x.id === a.id) || a;
    const sv = status.find((s) => s.account === a.id) || {};
    const K = (k) => `fi:${a.id}:${k}`;
    const back = [`fi:${a.id}`];
    const patch = (title, intro, submit, bodyOf, extra = [], msg = 'Saved.', danger = false) => async () => {
      const r = await askPassword(ctx, {
        title, intro: intro ? note(intro) : null, submit, extra, danger,
        run: async (cred, v) => { try { return await ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: cur().version, ...bodyOf(v) })); } catch (e) { throw fail(e); } },
      });
      if (r) await reload(msg);
    };
    const pause = btn(a.paused ? 'resume' : 'pause', keep([K('pause'), ...back], patch(a.paused ? `resume ${a.label}?` : `pause ${a.label}?`,
      a.paused ? 'Its matched car is read from TeslaFi again from now on.' : 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile; the history already stored stays.',
      a.paused ? 'resume' : 'pause', () => ({ paused: !a.paused }), [], a.paused ? `${a.label} resumed.` : `${a.label} paused.`)), { kind: a.paused ? 'primary' : '' });
    const token = btn('replace token', keep([K('token'), ...back], () => {
      const f = field({ label: 'token', name: 'token', path: 'token', type: 'password', required: true, hint: 'From TeslaFi: Settings, TeslaFi API. Checked with TeslaFi before it replaces the old one.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
      return patch(`new token for ${a.label}`, 'For a token TeslaFi refused, or one you renewed on TeslaFi. If TeslaFi does not take the new one, nothing is changed. The old token is deleted.', 'check with teslafi and replace', (v) => ({ token: v.token.trim() }), [f], 'Token replaced and checked with TeslaFi.')();
    }), { kind: a.status === 'token_refused' ? 'primary' : '' });
    const zone = btn('time zone', keep([K('zone'), ...back], () => {
      const f = zoneField(a.time_zone, 'The zone TeslaFi shows this account\'s times in.');
      return patch(`time zone of ${a.label}`, 'TeslaFi\'s times are read in the new zone from now on; no poll is dropped for it.', 'save', (v) => ({ time_zone: v.time_zone || null }), [f], 'Time zone saved.')();
    }), { kind: 'quiet' });
    const rename = btn('rename', keep([K('rename'), ...back], () => {
      const f = field({ label: 'name', name: 'label', required: true, value: a.label, attrs: { maxlength: '64', autocomplete: 'off' } });
      return patch(`rename ${a.label}`, null, 'rename', (v) => ({ label: v.label.trim() }), [f], 'Renamed.')();
    }), { kind: 'quiet' });
    const remove = btn('remove', keep([K('remove'), ...back], async () => {
      const r = await askPassword(ctx, {
        title: `remove ${a.label}?`, danger: true, submit: 'remove the token',
        intro: note('Reading stops at once and the sealed file with its token is deleted. The history already stored from it stays, marked as data from TeslaFi.'),
        run: async (cred) => { try { return await ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred)); } catch (e) { throw fail(e); } },
      });
      if (r) await reload(`${a.label} removed. Its history stays.`);
    }), { kind: 'danger' });
    fk(pause, K('pause')); fk(token, K('token')); fk(zone, K('zone')); fk(rename, K('rename')); fk(remove, K('remove'));
    const m = CAR_MODE[sv.mode || a.mode] || [sv.mode || a.mode || DASH, ''];
    const carState = sv.car_state ?? a.cars?.find((c) => c.current)?.car_state;

    return h('div', { class: 'tesla-acc', id: `teslafi-${a.id}` },
      h('div', { class: 'tesla-acc-head' }, h('h3', { tabindex: '-1', dataset: { fk: `fi:${a.id}` } }, a.label), tag(st[0], st[1])),
      st[2] ? h('p', { class: ['note', a.status === 'token_refused' || a.status === 'error' ? 'tone--warn' : null] }, st[2]) : null,
      kv([
        ['status', [tag(st[0], st[1]), a.running ? h('span', { class: 'muted' }, ' running') : null]],
        ['now', [h('span', { title: m[1] }, m[0]), h('span', { class: 'muted' }, ` (TeslaFi says ${carState || 'not known'})`)]],
        ['last ok', a.last_ok_ms ? h('span', { title: dateTime(a.last_ok_ms, ctx.tz) }, ago(a.last_ok_ms)) : h('span', { class: 'muted' }, 'none yet')],
        ['last error', err ? [lastErrorText(err), err.message && LAST_ERROR_WORDS[err.code] ? h('span', { class: 'muted' }, ` (${err.message})`) : null, err.at_ms ? h('span', { class: 'muted' }, `, ${ago(err.at_ms)}`) : null] : h('span', { class: 'muted' }, 'none'), { tone: err ? 'warn' : null }],
        ['requests today', `${formatNumber(a.polls_today ?? 0)} of 1,500`],
        [sv.mode === 'backoff' ? 'next try' : 'next read', sv.next_request_ms ? [time(sv.next_request_ms, ctx.tz), h('span', { class: 'muted' }, ` (in ${dur(Math.max(0, sv.next_request_ms - ctx.api.now()))})`)] : h('span', { class: 'muted' }, 'not planned'), { tone: sv.mode === 'backoff' ? 'warn' : null }],
        ['last teslafi data', sv.last_data_ms ? h('span', { title: dateTime(sv.last_data_ms, ctx.tz) }, ago(sv.last_data_ms)) : h('span', { class: 'muted' }, 'none yet')],
        ['time zone', a.time_zone || h('span', { class: 'muted' }, 'the server\'s default')],
        ['added', dateTime(a.added_ms, ctx.tz)],
        ['host', h('span', { class: 'tesla-hosts' }, (a.hosts || []).map((x) => code(x)), a.tls ? h('span', { class: 'muted' }, `tls ${a.tls}`) : null)],
      ]),
      h('div', { class: 'acts tesla-acts' }, pause, token, zone, rename, remove),
      carsBlock(a));
  }

  /* ---------- an account's cars: match each */
  function carsBlock(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    const opts = [['', 'not decided (not read)'], ...cars.map((c) => [c.id, c.name || c.id]), ['new', 'a new car'], ['leave_out', 'leave out (not read)']];
    const picked = new Map(items.map((c) => [c.vehicle, c.match ?? c.suggested ?? '']));
    const K = (k) => `fi:${a.id}:${k}`;
    const save = fk(btn('save matches', keep([K('save'), K('cars'), `fi:${a.id}`], () => saveMatches()), { kind: 'primary' }), K('save'));
    save.hidden = true;
    const changed = () => items.filter((c) => (picked.get(c.vehicle) || null) !== (c.match ?? null));
    const vName = (c) => c.display_name || c.vehicle;
    const dupes = () => {
      const by = new Map();
      for (const c of items) { const v = picked.get(c.vehicle); if (v && v !== 'new' && v !== 'leave_out') by.set(v, [...(by.get(v) || []), c]); }
      return [...by].filter(([, l]) => l.length > 1);
    };
    const dupeMsg = h('p', { class: 'note tone--bad tesla-dupe', role: 'alert', hidden: true });
    const sync = () => {
      const d = dupes();
      save.hidden = !changed().length;
      save.disabled = !!d.length;
      dupeMsg.hidden = !d.length;
      replace(dupeMsg, d.length ? [tag('two on one', 'bad'), ` ${d.map(([car, l]) => `${l.map(vName).join(' and ')} are both matched to ${carName(car)}.`).join(' ')} One Sentry Mate car takes one TeslaFi car: pick another car, a new car or leave out for one of them.`] : '');
    };
    const select = (c) => {
      const pre = picked.get(c.vehicle);
      const known = opts.some(([v]) => v === pre) ? opts : [...opts, [pre, carName(pre)]];
      const sel = h('select', { class: 'in in--cell tesla-sel', 'aria-label': `Sentry Mate car for the TeslaFi car ${vName(c)}` }, known.map(([v, l]) => h('option', { value: v, selected: v === pre }, l)));
      sel.addEventListener('change', () => { picked.set(c.vehicle, sel.value); sync(); });
      return sel;
    };
    async function saveMatches() {
      const ch = changed();
      if (!ch.length || dupes().length) { sync(); return; }
      const r = await askPassword(ctx, {
        title: `match the car of ${a.label}`, submit: 'save matches',
        intro: lines(ch.map((c) => { const v = picked.get(c.vehicle); return [tag(v === 'leave_out' || !v ? 'off' : 'read', v === 'leave_out' || !v ? 'muted' : 'ok'), [h('b', null, vName(c)), ': ', v === 'new' ? 'a new car here, read from now on' : v === 'leave_out' ? 'left out, not read' : !v ? 'not decided, not read' : `${carName(v)}, read from now on`]]; })),
        run: async (cred) => { try { return await ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, withPw(cred, { version: carRows[a.id]?.version ?? data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: picked.get(c.vehicle) || null })) })); } catch (e) { throw fail(e); } },
      });
      if (r) await reload('Matches saved.');
    }
    const t = table(ctx, {
      bare: true, rows: items, csv: false, label: `Cars of the TeslaFi account ${a.label}`,
      columns: [
        { key: 'car', label: 'car at teslafi', ph: 'main', cls: 'tesla-car-col', render: (c) => [h('b', null, c.display_name || 'no name at TeslaFi'), h('span', { class: 'muted' }, ` ${[c.year, c.model].filter(Boolean).join(' ')} ${c.vin_prefix || ''}`)] },
        { key: 'match', label: 'sentry mate car', ph: 'sub', render: (c) => [select(c), c.match == null && c.suggested ? [' ', tag('suggested: same vin prefix', 'accent')] : null] },
        { key: 'state', label: 'teslafi says', grow: true, ph: 'sub', render: (c) => [h('span', { class: 'ph-k' }, 'teslafi says '), c.current ? (c.car_state || DASH) : h('span', { class: 'muted' }, 'not polled now')] },
      ],
      empty: () => h('div', { class: 'empty' }, h('p', { class: 'empty-line' }, '-- no car yet --'), h('p', { class: 'empty-hint' }, 'TeslaFi\'s polls named no car yet. It shows here once TeslaFi polls it.')),
    });
    tables.push(t);
    sync();
    return h('div', null, h('div', { class: 'tesla-cars-head' }, h('p', { class: 'sub-title', tabindex: '-1', dataset: { fk: K('cars') } }, `its cars (${items.length})`), save), dupeMsg, t.el, note('A car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN prefix as a car here; it is not saved until you press save matches.'));
  }

  return { el, side, load };
}
