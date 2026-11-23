// settings/integrations, the teslafi block (#teslafi): the TeslaFi input. For owners who pay for TeslaFi, this server
// can read TeslaFi's own polls of the car (about one a minute while it is awake) from TeslaFi's API with the owner's
// TeslaFi token, as a third live input beside SMO and the Tesla API. Off until an admin adds a token; before that the
// server makes no connection to TeslaFi at all. It only reads: never a wake or a command, and no answer holds the token.
//
// Admins only, reading included. Every change but saving the car match asks for the admin's own password again (and
// the two-step code when the account has it): askPassword() from core/secret.js, sent as confirm_password and code.
// The car match takes the admin's session alone (it stores and shows no token). Each account block has the id
// teslafi-<account> so a notice can link to it. Built like pages/tesla.js.
//
// API: GET integrations/teslafi, GET/POST integrations/teslafi/accounts, GET/PATCH/DELETE .../accounts/{id},
// GET/PUT .../accounts/{id}/cars, GET integrations/teslafi/status, PATCH integrations/teslafi { version, poll } (the pace).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { isValidZone } from '../shared/time.js';
import { section, kv, btn, note } from '../core/ui.js';
import { table } from '../core/table.js';
import { pairOptions } from '../core/carpick.js';
import { field } from '../core/form.js';
import { errorWords } from '../core/states.js';
import { tag, dateTime, time, ago, DASH } from '../core/fmt.js';
import { toast } from '../core/toast.js';
import { askPassword } from '../core/secret.js';

const enc = encodeURIComponent;
const BASE = 'integrations/teslafi';

// account status: [tag word, tone, what it means]
const ACCOUNT_STATUS = {
  checking: ['checking', 'muted', 'Just added: the server is asking TeslaFi for its car.'],
  ok: ['reading', 'ok', 'TeslaFi takes the token; its matched car is read.'],
  paused: ['paused', 'muted', 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile.'],
  token_refused: ['token refused', 'warn', 'TeslaFi refused the token (a wrong token, or the TeslaFi subscription ended), so nothing more is asked. Replace the token.'],
  error: ['error', 'bad', 'TeslaFi answers with errors. See the last error.'],
};
// what the server does for the car TeslaFi polls: [words, why]
const MODE = {
  off: ['not read', 'Not matched, left out, paused, or the token is refused.'],
  asleep: ['asleep', 'TeslaFi says the car sleeps: read at the asleep pace.'],
  idle: ['parked', 'Parked and awake: read at the awake pace.'],
  charging: ['charging', 'Read at the charging pace.'],
  driving: ['driving', 'Read at the driving pace.'],
  backoff: ['waiting after errors', 'TeslaFi answered with errors, or the day\'s requests are used up: it waits 60 s, doubling to 15 minutes (a 429 waits what TeslaFi asks).'],
};
// the pace (one for every token): [value, label, its waits in words]
const PACE = [
  ['gentle', 'gentle', 'driving or charging every 2 min, awake 5 min, asleep 20 min'],
  ['normal', 'normal', 'driving or charging every 60 s, awake 3 min, asleep 10 min'],
  ['often', 'often', 'driving or charging every 15 s, awake 60 s, asleep 3 min'],
];
const ERROR_WORDS = {
  teslafi_refused: 'TeslaFi refused this token: a wrong token, or the TeslaFi subscription ended. Copy the token again from TeslaFi\'s API page (Settings, API Token). Nothing was changed.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was changed. Try again in a few minutes.',
  account_exists: 'This token\'s car is already read through another TeslaFi account here. Replace that account\'s token instead, or remove it first.',
  car_taken: 'That car is already matched to the car of another TeslaFi account here. Leave it out there first, or pick another car.',
  no_such_teslafi_account: 'This TeslaFi account is not here any more: another admin may have removed it. The page shows the accounts there are now.',
  version_conflict: 'Someone else changed this TeslaFi account a moment ago. The page shows their change now; make yours again.',
};
const LAST_ERROR_WORDS = {
  token_refused: 'TeslaFi refused the token', teslafi_refused: 'TeslaFi refused the token', teslafi_unreachable: 'TeslaFi could not be reached',
  rate_limited: 'TeslaFi asked the server to slow down', daily_cap: 'the day\'s 1,500 requests are used up', server_error: 'TeslaFi answered with a server error', timeout: 'TeslaFi took too long to answer',
};
const lastErrorText = (e) => (e ? (LAST_ERROR_WORDS[e.code] || e.message || e.code) : null);

function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (ERROR_WORDS[e.code]) e.message = ERROR_WORDS[e.code];
  else if (e.status === 409) e.message = ERROR_WORDS.version_conflict;
  if (e.fields) { delete e.fields.confirm_password; delete e.fields.code; if (!Object.keys(e.fields).length) e.fields = null; }
  return e;
}
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const code = (t) => h('span', { class: 'prompt' }, t);
const secs = (s) => (s == null ? DASH : s >= 120 && s % 60 === 0 ? `${formatNumber(s / 60)} min` : `${formatNumber(s)} s`);
const zoneHint = 'The zone TeslaFi shows this account\'s times in (TeslaFi: Settings > Account). Empty: the server\'s default zone.';
const zoneField = (value) => field({ label: 'time zone', name: 'time_zone', path: 'time_zone', value: value || '', hint: zoneHint, attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: 'America/Chicago' } });
// a zone as typed: null for empty; an Error for one this browser does not know (the server is the judge of the rest)
const zoneOf = (v) => { const t = String(v || '').trim(); if (!t) return null; if (!isValidZone(t)) { const e = new Error('Not a time zone this page knows: type an IANA name such as America/Chicago.'); e.fields = { time_zone: e.message }; throw e; } return t; };

export const teslafiAccountHref = (id) => `${state.buildUrl('settings-integrations')}#teslafi-${enc(id)}`;

/** The teslafi block. cars: Sentry Mate's cars ({ id, name, time_zone }). Returns { el, side, load() }. */
export function teslafiSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const side = h('span');
  const body = h('div', { 'aria-live': 'polite' });
  const el = section({ title: 'teslafi', id: 'teslafi', side },
    note('A third live input beside SMO and the Tesla API, for owners who pay for TeslaFi: this server reads TeslaFi\'s own polls of the car from TeslaFi\'s API with your TeslaFi token. It only reads: never a wake or a command, so the car is not kept awake and TeslaFi\'s monthly command limit is not used. Off until an admin adds a token. Where SMO or the Tesla API saw the same time, theirs win; the rest says data from TeslaFi.'),
    body);

  let ov = null, accounts = [], status = [], carRows = {};
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;
  const tables = [];

  async function load() {
    if (!admin) {
      replace(side, tag('admins only', 'muted'));
      replace(body, note(tag('admin', 'muted'), ' Only an admin sees and changes the TeslaFi token: its status, its car and how often TeslaFi is asked. Every page still says where its data came from, ', h('span', { class: 'src' }, h('span', { class: 'src-b' }, '[', h('span', { class: 'src-k src-k--teslafi_api' }, 'tfi'), ']')), ' for data from TeslaFi.'));
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
      replace(body, h('p', { class: 'note tone--bad' }, 'TeslaFi\'s settings could not be read: ', errorWords(e).text));
      return;
    }
    if (!ctx.alive()) return;
    paint();
    await Promise.all(tables.map((t) => t.ready)).catch(() => {});
  }
  let pending = null;
  const refresh = () => (pending = load().catch(() => {}));
  const reload = async (msg) => { if (msg) toast(msg); await refresh(); };

  // a change repaints the block: the focus goes back to the button with the same data-fk, else to a fallback
  const fk = (el, key) => { el.dataset.fk = key; return el; };
  const shown = (el) => el && el.isConnected && !el.hidden && !el.disabled && (el.checkVisibility ? el.checkVisibility() : true);
  function refocus(keys) {
    const now = document.activeElement;
    if (now && now !== document.body && now.isConnected) return;
    for (const k of keys) {
      const x = body.querySelector(`[data-fk="${CSS.escape(k)}"]`);
      if (shown(x)) { x.focus({ preventScroll: true }); return; }
    }
  }
  const keep = (keys, fn) => async (...args) => {
    try { return await fn(...args); } finally { await pending; if (ctx.alive()) refocus([...keys, 'teslafi-top']); }
  };
  const stale = (e) => { if (e && (e.code === 'version_conflict' || e.code === 'no_such_teslafi_account' || (e.status === 409 && e.code !== 'account_exists' && e.code !== 'car_taken'))) refresh(); return e; };
  const fail = (e) => stale(words(e));

  function paint() {
    tables.length = 0;
    replace(side, !ov.enabled ? tag('off until a token is added', 'muted')
      : ov.cars ? tag(`reading ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'}`, 'ok') : tag('no car matched yet', 'warn'));
    replace(body, statusBlock(), addBlock(), accounts.map(accountBlock));
  }

  function statusBlock() {
    const p = ov.polls || {};
    const daily = p.daily_cap || 1500;
    const cap = daily * Math.max(1, ov.accounts || 0);
    const poll = ov.poll || 'normal';
    const pace = h('div', { class: 'keys', role: 'group', 'aria-label': 'How often TeslaFi is asked' },
      PACE.map(([v, l, why]) => h('button', { class: 'key', type: 'button', title: why, 'aria-pressed': String(poll === v), dataset: { fk: `fi-pace:${v}` }, onclick: keep([`fi-pace:${v}`], () => setPace(v)) }, l)));
    return h('div', null,
      h('p', { class: 'sub-title', tabindex: '-1', dataset: { fk: 'teslafi-top' } }, 'status'),
      kv([
        ['state', ov.enabled ? [tag('on', 'ok'), ` ${formatNumber(ov.accounts)} ${ov.accounts === 1 ? 'token' : 'tokens'}, ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'} read`]
          : [tag('off', 'muted'), ' off until a token is added: no connection to TeslaFi is made']],
        ['requests today', [formatNumber(ov.polls_today ?? 0), h('span', { class: 'muted' }, ov.enabled ? ` of at most ${formatNumber(cap)} (${formatNumber(daily)} a day per token)` : ' none: there is no token')]],
        ['connections', [formatNumber(ov.clients_built ?? 0), h('span', { class: 'muted' }, ` set up to TeslaFi since the server started${ov.tls ? `, TLS ${ov.tls}` : ''}`)]],
        ['last error', ov.last_error ? [lastErrorText(ov.last_error), ov.last_error.at_ms ? h('span', { class: 'muted' }, ` ${dateTime(ov.last_error.at_ms, ctx.tz)} (${ago(ov.last_error.at_ms)})`) : null] : h('span', { class: 'muted' }, 'none'), { tone: ov.last_error ? 'warn' : null }],
        ['teslafi host', ov.hosts?.length ? h('span', { class: 'tesla-hosts' }, ov.hosts.map((x) => code(x))) : h('span', { class: 'muted' }, 'none while there is no token. Once one is added: www.teslafi.com, over https only, the token in a header.')],
      ], { cols: 1 }),
      h('p', { class: 'sub-title' }, 'how often teslafi is asked'),
      pace,
      h('ul', { class: 'ln-list' }, PACE.map(([v, l, why]) => h('li', null, tag(l, poll === v ? 'ok' : 'muted'), h('span', { class: 'ln-list-t' }, why)))),
      kv([
        ['driving', `every ${secs(p.driving_s)}`],
        ['charging', `every ${secs(p.charging_s)}`],
        ['parked, awake', `every ${secs(p.parked_s)}`],
        ['asleep', `every ${secs(p.asleep_s)}`],
        ['SMO or API live', [`every ${secs(p.other_live_s)}`, h('span', { class: 'muted' }, ' at most, while another input sends')]],
        ['per day', [formatNumber(p.daily_cap ?? 1500), h('span', { class: 'muted' }, ' requests a token, then none until midnight UTC')]],
      ]),
      note('Often reads about as fast as TeslaFi itself refreshes (up to 6,000 requests a day per token); normal and gentle stay within 1,500. In every pace at most one request goes out every 10 s per token, TeslaFi\'s own waits (Retry-After) and the back-off after errors are kept, and the car is never woken: these reads come from TeslaFi\'s own database.'));
  }
  async function setPace(v) {
    if (v === (ov.poll || 'normal')) return;
    const why = PACE.find((x) => x[0] === v)?.[2] || '';
    const r = await askPassword(ctx, {
      title: `teslafi pace: ${v}`, submit: 'change the pace',
      intro: note(`How often the server reads TeslaFi's polls of every car: ${why}. A faster pace gives finer drives and charges from TeslaFi, and uses more requests a day.`),
      run: async (cred) => { try { return await ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v })); } catch (e) { throw fail(e); } },
    });
    if (r) await reload(`TeslaFi pace saved: ${v}.`);
  }

  function addBlock() {
    return h('div', null,
      h('p', { class: 'sub-title' }, accounts.length ? 'add another teslafi token' : 'add a teslafi token'),
      h('p', { class: 'note' }, 'In TeslaFi open Settings, API Token, and copy the token. The server checks it with TeslaFi (one read; when no car is named, one more 10 s later) before anything is kept, seals it, and never shows it again.'),
      h('div', { class: 'acts' }, fk(btn('add a teslafi token', keep(['addtoken'], addToken), { kind: 'primary' }), 'addtoken')),
      note('The car of a new token is not read until you match it to a car here, to a new car, or leave it out.'));
  }

  async function addToken() {
    const label = field({ label: 'name', name: 'label', required: true, value: 'TeslaFi', hint: 'Your own name for it, shown on this page.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const tok = field({ label: 'token', name: 'token', path: 'token', type: 'password', required: true, hint: 'TeslaFi\'s API token. Checked with TeslaFi before it is kept; sealed on the server and never shown again.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
    const zone = zoneField('');
    const r = await askPassword(ctx, {
      title: 'add a teslafi token', submit: 'check with teslafi and add', extra: [label, tok, zone],
      intro: note('Only add a token of your own TeslaFi account. Anyone with it can read the car\'s history at TeslaFi, so it never leaves this server and no page shows it. The check can take about 10 s.'),
      run: async (cred, v) => { try { return await ctx.api.post(`${BASE}/accounts`, withPw(cred, { label: v.label.trim(), token: v.token.trim(), time_zone: zoneOf(v.time_zone) })); } catch (e) { throw fail(e); } },
    });
    if (r) await reload(`${r.label || 'The token'} added. Match its car below to start reading it.`);
  }

  function accountBlock(a) {
    const st = ACCOUNT_STATUS[a.status] || [a.status, 'muted', ''];
    const err = a.last_error;
    const cur = () => accounts.find((x) => x.id === a.id) || a;
    const K = (k) => `tfa:${a.id}:${k}`;
    const back = [`tfa:${a.id}`];
    const patch = (title, intro, submit, bodyOf, extra = [], msg = 'Saved.') => async () => {
      const r = await askPassword(ctx, {
        title, intro: intro ? note(intro) : null, submit, extra,
        run: async (cred, v) => { try { return await ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: cur().version, ...bodyOf(v) })); } catch (e) { throw fail(e); } },
      });
      if (r) await reload(msg);
    };
    const pause = btn(a.paused ? 'resume' : 'pause', keep([K('pause'), ...back], patch(a.paused ? `resume ${a.label}?` : `pause ${a.label}?`,
      a.paused ? 'Its car is read again from now on.' : 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile; the history already stored stays.',
      a.paused ? 'resume' : 'pause', () => ({ paused: !a.paused }), [], a.paused ? `${a.label} resumed.` : `${a.label} paused.`)), { kind: a.paused ? 'primary' : '' });
    const token = btn('replace token', keep([K('token'), ...back], () => {
      const f = field({ label: 'token', name: 'token', path: 'token', type: 'password', required: true, hint: 'Checked with TeslaFi before it replaces the old one. If TeslaFi refuses it, nothing is changed.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
      return patch(`new token for ${a.label}`, 'For a token TeslaFi refused, or one you made new at TeslaFi. The old token is deleted.', 'check with teslafi and replace', (v) => ({ token: v.token.trim() }), [f], 'Token replaced and checked with TeslaFi.')();
    }), { kind: a.status === 'token_refused' ? 'primary' : '' });
    const settings = btn('name and zone', keep([K('edit'), ...back], () => {
      const l = field({ label: 'name', name: 'label', required: true, value: a.label, attrs: { maxlength: '64', autocomplete: 'off' } });
      const z = zoneField(a.time_zone);
      return patch(`${a.label}: name and time zone`, 'From a change of zone on, TeslaFi\'s times are read in the new zone at once.', 'save', (v) => ({ label: v.label.trim(), time_zone: zoneOf(v.time_zone) }), [l, z], 'Saved.')();
    }), { kind: 'quiet' });
    const remove = btn('remove', keep([K('remove'), ...back], async () => {
      const r = await askPassword(ctx, {
        title: `remove ${a.label}?`, danger: true, submit: 'remove the token',
        intro: note('Reading stops at once and the sealed file with the token is deleted. The history already stored from it stays, marked as data from TeslaFi.'),
        run: async (cred) => { try { return await ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred)); } catch (e) { throw fail(e); } },
      });
      if (r) await reload(`${a.label} removed. Its history stays.`);
    }), { kind: 'danger' });
    fk(pause, K('pause')); fk(token, K('token')); fk(settings, K('edit')); fk(remove, K('remove'));
    const m = MODE[a.mode] || [a.mode || DASH, ''];
    return h('div', { class: 'tesla-acc', id: `teslafi-${a.id}` },
      h('div', { class: 'tesla-acc-head' }, h('h3', { tabindex: '-1', dataset: { fk: `tfa:${a.id}` } }, a.label), tag(st[0], st[1])),
      st[2] ? h('p', { class: ['note', a.status === 'token_refused' || a.status === 'error' ? 'tone--warn' : null] }, st[2]) : null,
      kv([
        ['status', [tag(st[0], st[1]), a.running ? h('span', { class: 'muted' }, ' running') : null]],
        ['now', h('span', { title: m[1] }, m[0])],
        ['added', dateTime(a.added_ms, ctx.tz)],
        ['last ok', a.last_ok_ms ? h('span', { title: dateTime(a.last_ok_ms, ctx.tz) }, ago(a.last_ok_ms)) : h('span', { class: 'muted' }, 'none yet')],
        ['last error', err ? [lastErrorText(err), err.message && LAST_ERROR_WORDS[err.code] ? h('span', { class: 'muted' }, ` (${err.message})`) : null, err.at_ms ? h('span', { class: 'muted' }, `, ${ago(err.at_ms)}`) : null] : h('span', { class: 'muted' }, 'none'), { tone: err ? 'warn' : null }],
        ['requests today', `${formatNumber(a.polls_today ?? 0)} of ${formatNumber(ov.polls?.daily_cap || 1500)}`],
        ['time zone', a.time_zone ? a.time_zone : h('span', { class: 'muted' }, 'the server\'s default')],
        ['connection', a.tls ? `https, TLS ${a.tls}` : h('span', { class: 'muted' }, 'not yet')],
      ]),
      h('div', { class: 'acts tesla-acts' }, pause, token, settings, remove),
      carsBlock(a));
  }

  function carsBlock(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    const opts = [['', 'not decided (not read)'], ...cars.map((c) => [c.id, c.name || c.id]), ['new', 'a new car'], ['leave_out', 'leave out (not read)']];
    const picked = new Map(items.map((c) => [c.vehicle, c.match ?? c.suggested ?? '']));
    const K = (k) => `tfa:${a.id}:${k}`;
    const save = fk(btn('save match', keep([K('save'), K('cars'), `tfa:${a.id}`], () => saveMatches()), { kind: 'primary' }), K('save'));
    const changed = () => items.filter((c) => (picked.get(c.vehicle) || null) !== (c.match ?? null));
    const sync = () => { save.hidden = !changed().length; };
    const select = (c) => {
      const pre = picked.get(c.vehicle);
      const known = opts.some(([v]) => v === pre) ? opts : [...opts, [pre, carName(pre)]];
      const sel = h('select', { class: 'in in--cell tesla-sel tf-sel', 'aria-label': `Sentry Mate car for the TeslaFi car ${c.display_name || c.vehicle}` }, pairOptions(known, cars, pre));
      sel.addEventListener('change', () => { picked.set(c.vehicle, sel.value); sync(); });
      return sel;
    };
    async function saveMatches() {
      const ch = changed();
      if (!ch.length) return;
      // the admin's session alone: no password asked again (a match stores and shows no token)
      save.disabled = true;
      let r = null;
      try { r = await ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, { version: carRows[a.id]?.version ?? data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: picked.get(c.vehicle) || null })) }); } catch (e) { if (e?.name !== 'AbortError') toast(errorWords(fail(e)).text, { kind: 'error' }); } finally { save.disabled = false; sync(); }
      if (r) await reload('Match saved.');
    }
    const statusOf = (c) => status.find((s) => s.account === a.id && s.vehicle === c.vehicle) || {};
    const zoneOfCar = (c) => cars.find((x) => x.id === c.match)?.time_zone || ctx.tz;
    const t = table(ctx, {
      bare: true, rows: items, csv: false, label: `Cars of the TeslaFi token ${a.label}`,
      columns: [
        { key: 'car', label: 'car at teslafi', ph: 'main', cls: 'tf-car-col', render: (c) => [h('b', null, c.display_name || 'no name at TeslaFi'), h('span', { class: 'muted' }, ` ${[c.year, c.model].filter(Boolean).join(' ')} ${c.vin_prefix || ''}`)] },
        { key: 'match', label: 'sentry mate car', ph: 'sub', render: (c) => [select(c), c.match == null && c.suggested ? [' ', tag('suggested: same vin prefix', 'accent')] : null] },
        { key: 'state', label: 'teslafi says', ph: 'sub', render: (c) => [h('span', { class: 'ph-k' }, 'teslafi says '), c.car_state || DASH, c.current ? h('span', { class: 'muted', title: 'the car TeslaFi polls now' }, ', polled now') : null] },
        { key: 'next', label: 'next read', ph: 'sub', grow: true, render: (c) => { const sv = statusOf(c); return [h('span', { class: 'ph-k' }, 'next read '), sv.next_request_ms ? time(sv.next_request_ms, zoneOfCar(c)) : h('span', { class: 'muted' }, 'not planned'), sv.last_error ? h('span', { class: 'tone--warn' }, ` ${lastErrorText(sv.last_error)}`) : null]; } },
        { key: 'last', label: 'last data', num: true, ph: 'num', render: (c) => { const sv = statusOf(c); return sv.last_data_ms ? h('span', { title: dateTime(sv.last_data_ms, zoneOfCar(c)) }, ago(sv.last_data_ms)) : h('span', { class: 'muted' }, DASH); } },
      ],
      empty: () => h('div', { class: 'empty' }, h('p', { class: 'empty-line' }, '-- no car yet --'), h('p', { class: 'empty-hint' }, 'TeslaFi has not named a car for this token yet. It shows here after TeslaFi\'s next poll of the car.')),
    });
    tables.push(t);
    sync();
    return h('div', null, h('div', { class: 'tesla-cars-head' }, h('p', { class: 'sub-title', tabindex: '-1', dataset: { fk: K('cars') } }, `its car${items.length === 1 ? '' : 's'} (${items.length})`), save), t.el,
      note('A car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN prefix as a car here; it is not saved until you press save match. A car is matched to one TeslaFi car at most.'));
  }

  return { el, side, load };
}
