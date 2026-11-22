// Sentry Mate, "Pebble" UI pack: TeslaFi, on Settings: Integrations (#teslafi).
//
// The TeslaFi input, for owners who pay for TeslaFi: this server reads TeslaFi's own polls of the car from TeslaFi's
// API with the owner's TeslaFi token, as a third live input beside SMO and the Tesla API. It is off until an admin
// adds a token; before that the server makes no connection to TeslaFi at all. It only reads TeslaFi's database: it
// never wakes a car or sends it a command, and no answer ever holds the token.
//
// Admins only, reading included. Built like the Tesla account (pages/tesla.js): the input's status, how often
// TeslaFi is read (gentle, normal, often, with the waits each gives), adding a token, and per account a tile with its
// status and actions (pause or resume, a new token, time zone, rename, remove) and a tile of its cars, each matched
// to a car here, a new car or left out. Every change asks for the admin's own password again (and the two-step
// code): askPassword(). Each account tile has the id teslafi-<account>, so a notice can open it.
//
// Data: GET/PATCH integrations/teslafi, GET/POST integrations/teslafi/accounts,
// PATCH/DELETE integrations/teslafi/accounts/{id}, GET/PUT integrations/teslafi/accounts/{id}/cars,
// GET integrations/teslafi/status.

import { h } from '../shared/dom.js';
import * as state from '../shared/state.js';
import { formatTime, formatDateTime, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { tile, btn, chip, note, ago } from '../core/ui.js';
import { field, segmented } from '../core/form.js';
import { errorWords } from '../core/states.js';
import { facts } from '../core/bits.js';
import { group, infoRow, pickRow, said, askPassword } from '../core/settings.js';

const enc = encodeURIComponent;
const BASE = 'integrations/teslafi';

/* ---------------------------------------------------------------------- words */

// account status: [chip word, tone, what it means]
const ACCOUNT_STATUS = {
  checking: ['Checking', 'muted', 'Just added: the server is asking TeslaFi for its car.'],
  ok: ['Reading', 'teal', 'TeslaFi takes its token; its matched car is read.'],
  paused: ['Paused', 'muted', 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile.'],
  token_refused: ['Token refused', 'sun', 'TeslaFi stopped taking its token (a wrong token, or the TeslaFi subscription ended), so nothing is asked until you give a new one. Replace its token.'],
  error: ['Error', 'bad', 'TeslaFi answers with errors. The server waits longer after each one; see the last error.'],
};
// what the server does for the account now: [words, why]
const MODE = {
  off: ['Not read', 'No car matched, or the account is paused or its token refused.'],
  asleep: ['Asleep', 'TeslaFi says the car sleeps: read at the asleep pace.'],
  idle: ['Parked, awake', 'Read at the awake pace.'],
  charging: ['Charging', 'Read at the charging pace.'],
  driving: ['Driving', 'Read at the driving pace.'],
  backoff: ['Waiting after errors', 'TeslaFi answered with errors, asked to wait, or the day’s requests are used up: nothing is asked until the next try.'],
};
const PACE = [['gentle', 'Gentle'], ['normal', 'Normal'], ['often', 'Often']];
const PACE_TEXT = {
  often: 'As fast as TeslaFi itself refreshes its data: 15 s driving or charging, 1 min awake, 3 min asleep.',
  normal: '1 min driving or charging, 3 min awake, 10 min asleep.',
  gentle: '2 min driving or charging, 5 min awake, 20 min asleep.',
};

// error codes of a change, in words
const ERROR_WORDS = {
  teslafi_refused: 'TeslaFi did not take this token: check it on TeslaFi (Settings, TeslaFi API), and that the TeslaFi subscription is active. Nothing was kept.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was kept. Try again in a few minutes.',
  account_exists: 'The car of this token is already read through another TeslaFi account here. Replace that account’s token instead, or remove it first.',
  car_taken: 'That car is already matched to the car of another TeslaFi account here. Leave it out there first, or pick another car.',
  no_such_teslafi_account: 'This TeslaFi account is not here any more: another admin may have removed it. Nothing was changed; the page now shows the accounts there are.',
  version_conflict: 'Someone else changed this a moment ago, so nothing was saved. The page now shows their change: make yours again if it is still needed.',
};
// last_error codes of an account or the whole input
const LAST_ERROR_WORDS = {
  token_refused: 'TeslaFi refused the token',
  teslafi_refused: 'TeslaFi refused the token',
  teslafi_unreachable: 'TeslaFi could not be reached',
  unreachable: 'TeslaFi could not be reached',
  rate_limited: 'TeslaFi asked the server to wait',
  daily_cap: 'the day’s requests are used up',
  server_error: 'TeslaFi answered with a server error',
  timeout: 'TeslaFi took too long to answer',
};
const lastErrorText = (e) => (e ? (LAST_ERROR_WORDS[e.code] || e.message || e.code) : null);
const cap1 = (t) => (t ? t.charAt(0).toUpperCase() + t.slice(1) : t);

/** Our words on an error of a change; the password sheet handles wrong_password and wrong_two_step itself. */
function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (ERROR_WORDS[e.code]) e.message = ERROR_WORDS[e.code];
  else if (e.status === 409) e.message = ERROR_WORDS.version_conflict;
  if (e.code === 'teslafi_refused') e.fields = { token: 'TeslaFi did not take this token. Copy it again from TeslaFi, Settings, TeslaFi API.' };
  else if (e.fields) {
    const f = { ...e.fields };
    delete f.confirm_password; delete f.code;
    e.fields = Object.keys(f).length ? f : null;
  }
  return e;
}
const isStale = (e) => !!e && (e.code === 'no_such_teslafi_account' || e.code === 'version_conflict' || e.code === 'stale_version' || (e.status === 409 && !ERROR_WORDS[e.code]));
/** run() for askPassword: our words on an error; a stale page closes the sheet with { stale: words } instead. */
const guarded = (fn) => async (...a) => {
  try { return await fn(...a); } catch (e) { words(e); if (isStale(e)) return { stale: e.message }; throw e; }
};
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const mono = (t) => h('code', { class: 'set-mono' }, t);
const wait = (s) => (s == null ? '—' : s >= 60 && s % 60 === 0 ? `${formatNumber(s / 60)} min` : `${formatNumber(s)} s`);
const HINT = 'TeslaFi changes ask for it again.';
const TOKEN_HINT = 'On TeslaFi: Settings, TeslaFi API. Checked with TeslaFi before it is kept, then sealed on the server and never shown again.';

/** The time zones offered: the browser's list, the server's default first. */
function zoneOptions(current) {
  let list = [];
  try { list = Intl.supportedValuesOf('timeZone'); } catch { list = []; }
  if (current && !list.includes(current)) list = [current, ...list];
  return [['', 'The server’s default zone'], ...list.map((z) => [z, z.replace(/_/g, ' ')])];
}
const zoneField = (value) => field({ label: 'Time zone (optional)', name: 'time_zone', type: 'select', value: value || '', options: zoneOptions(value),
  hint: 'The zone TeslaFi shows this account’s times in (TeslaFi, Settings, Account). Leave it at the default when it is the same as this server’s.' });

export const teslafiAccountHref = (id) => `${state.buildUrl('settings-integrations')}#teslafi${id ? `-${enc(id)}` : ''}`;

/* ---------------------------------------------------------------------- the tiles */

/**
 * The TeslaFi tiles. cars: Sentry Mate's cars ({ id, name }) for the match picker.
 * Returns { el, load() }: el is a display: contents box for the grid; load reads everything and paints.
 */
export function teslafiTiles(ctx, { cars = [] } = {}) {
  const el = h('div', { class: 'contents' });
  let ov = null, accounts = [], status = [], carRows = {};
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  const head = () => h('div', { class: 'section-head w-full', id: 'teslafi' },
    h('h2', { class: 'section-title' }, 'TeslaFi'),
    h('p', { class: 'note' }, 'For owners who pay for TeslaFi: this server can read TeslaFi’s own polls of the car with your TeslaFi token, as a third live input beside SMO and the Tesla API. It only reads TeslaFi’s data: it never wakes a car or sends it a command. Where SMO or the Tesla API cover the same time, theirs win.'));

  async function load() {
    if (!ctx.admin) {
      el.replaceChildren(head(), tile({ w: 2, title: 'TeslaFi', icon: 'key', side: chip('Admins only', { tone: 'muted' }) },
        said('info', 'Only an admin sees and changes the TeslaFi input: its token, its car and how often TeslaFi is read. Every page still says where its data came from.')));
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
      el.replaceChildren(head(), tile({ w: 2, title: 'TeslaFi', icon: 'key', side: chip('Not read', { tone: 'bad' }) }, said('bad', 'TeslaFi’s settings could not be read: ', errorWords(e).text)));
      return;
    }
    if (!ctx.alive()) return;
    el.replaceChildren(head(), ...(accounts.length ? [statusTile(), paceTile(), ...accounts.flatMap((a) => [accountTile(a), carsTile(a)])] : [emptyTile()]), addTile());
  }
  const reload = async (msg) => { if (msg) ctx.toast(msg); await load(); };
  const after = async (r, msg) => {
    if (!r) return false;
    if (r.stale) { ctx.toast(r.stale, { kind: 'info' }); await load(); return true; }
    await reload(typeof msg === 'function' ? msg(r) : msg);
    return true;
  };

  /* ---------- no account yet */
  function emptyTile() {
    return tile({ w: 2, title: 'TeslaFi input', icon: 'key', side: chip('Off', { tone: 'muted', icon: 'minus' }) },
      said('info', 'With a TeslaFi token added, this server reads the polls TeslaFi already makes of your car; until one is added it makes no connection to TeslaFi at all.'));
  }

  /* ---------- the input as a whole */
  function statusTile() {
    const cap = ((ov.polls && ov.polls.daily_cap) || 1500) * Math.max(1, ov.accounts || 0);
    const side = !ov.enabled ? chip('Off', { tone: 'muted', icon: 'minus' })
      : ov.cars ? chip(`Reading ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'}`, { tone: 'teal', icon: 'check' }) : chip('No car matched yet', { tone: 'sun', icon: 'warning' });
    const check = (level, name, sub, word) => h('li', { class: ['check', `is-${level}`] }, h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
      h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, name), sub ? h('span', { class: 'check-sub' }, sub) : null), h('span', { class: 'check-word' }, word));
    const le = ov.last_error;
    return tile({ w: 2, title: 'TeslaFi input', icon: 'key', side },
      h('ul', { class: 'checks' },
        check(ov.enabled ? 'ok' : 'off', `${formatNumber(ov.accounts)} ${ov.accounts === 1 ? 'account' : 'accounts'}, ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'} read`, 'Each matched car is read from TeslaFi as well as from SMO.', ov.enabled ? 'On' : 'Off'),
        check('ok', 'Requests today', `Of at most ${formatNumber(cap)}: ${formatNumber((ov.polls && ov.polls.daily_cap) || 1500)} a day per account, then nothing until midnight UTC.`, formatNumber(ov.polls_today ?? 0)),
        check('ok', 'Connections set up', `To TeslaFi, since the server started${ov.tls ? `, over TLS ${ov.tls}` : ''}.`, formatNumber(ov.clients_built ?? 0)),
        check(le ? 'warn' : 'ok', 'Last error', le ? [cap1(lastErrorText(le)), le.at_ms ? `, ${formatDateTime(le.at_ms, ctx.tz, { now: ctx.api.now() })}` : ''] : 'None.', le ? 'Yes' : 'None')),
      group('TeslaFi host it talks to', (ov.hosts && ov.hosts.length ? ov.hosts : ['www.teslafi.com']).map((x) => infoRow({ icon: 'globe', name: mono(x), sub: 'Over HTTPS only, the token in a header, never in an address' }))));
  }

  function paceTile() {
    const p = ov.polls || {};
    const cur = ov.poll || 'normal';
    const seg = segmented({ name: 'How often TeslaFi is read', value: cur, options: PACE, onchange: (v) => setPace(v, seg) });
    return tile({ w: 2, title: 'How often TeslaFi is read', icon: 'clock' },
      seg,
      h('p', { class: 'set-a' }, `Now: driving or charging every ${wait(p.driving_s)}, awake ${wait(p.parked_s)}, asleep ${wait(p.asleep_s)}.`),
      facts([
        ['Driving', `every ${wait(p.driving_s)}`],
        ['Charging', `every ${wait(p.charging_s)}`],
        ['Parked, awake', `every ${wait(p.parked_s)}`],
        ['Asleep', `every ${wait(p.asleep_s)}`],
        ['While SMO or the Tesla API sends', `every ${wait(p.other_live_s)} at most`],
        ['Requests a day', `at most ${formatNumber(p.daily_cap ?? 1500)} per account`],
      ]),
      h('ul', { class: 'set-list' }, PACE.map(([k, w]) => h('li', null, h('b', null, w), ': ', PACE_TEXT[k]))),
      note('At every pace TeslaFi’s own waits (Retry-After) and errors are respected. These reads come from TeslaFi’s database: they never wake the car and do not count against TeslaFi’s monthly command limit.'));
  }
  async function setPace(v, seg) {
    const cur = ov.poll || 'normal';
    if (v === cur) return;
    const w = PACE.find((x) => x[0] === v)[1];
    const r = await askPassword(ctx, {
      title: `Pace: ${w}`, yes: 'Change the pace', hint: HINT,
      text: `How often the server reads TeslaFi. ${PACE_TEXT[v]}`,
      run: guarded((cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v }))),
    });
    if (!(await after(r, `Pace saved: ${w.toLowerCase()}.`))) seg.set(cur);
  }

  /* ---------- adding a token */
  function addTile() {
    return tile({ w: 2, title: accounts.length ? 'Add another TeslaFi token' : 'Add a TeslaFi token', icon: 'plus' },
      h('p', { class: 'set-a' }, 'Your TeslaFi API token is on TeslaFi: Settings, TeslaFi API. The server checks it with TeslaFi (one read, which can take about 10 s) before anything is kept, and never shows it again.'),
      h('div', { class: 'form-actions' }, btn('Add a token', { kind: 'primary', icon: 'key', onclick: () => addToken() })),
      note('The car of a new token is not read until you match it to a car here, to a new car, or leave it out.'));
  }

  async function addToken() {
    const label = field({ label: 'Name', name: 'label', value: 'TeslaFi', hint: 'Your own name for it, shown on this page.', attrs: { maxlength: '64', autocomplete: 'off', required: true } });
    const tok = field({ label: 'TeslaFi API token', name: 'token', type: 'password', hint: TOKEN_HINT, attrs: { spellcheck: 'false', autocomplete: 'off', required: true } });
    const zone = zoneField(null);
    const r = await askPassword(ctx, {
      title: 'Add a TeslaFi token', yes: 'Check with TeslaFi and add', extra: [label, tok, zone], hint: HINT,
      text: 'Only add a token of your own TeslaFi account. Anyone with it can read your car’s data at TeslaFi, so it never leaves this server and no page shows it.',
      run: async (cred, v) => {
        const bad = {};
        if (!String(v.label || '').trim()) bad.label = 'Give it a name.';
        if (!String(v.token || '').trim()) bad.token = 'Paste the token from TeslaFi, Settings, TeslaFi API.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Not added yet.'), { fields: bad });
        const body = { label: v.label.trim(), token: v.token.trim() };
        if (v.time_zone) body.time_zone = v.time_zone;
        try { return await ctx.api.post(`${BASE}/accounts`, withPw(cred, body)); } catch (e) { throw words(e); }
      },
    });
    await after(r, (x) => `${x.label || 'The account'} added. Match its car below to start reading it.`);
  }

  /* ---------- one account */
  function accountTile(a) {
    const st = ACCOUNT_STATUS[a.status] || [a.status, 'muted', ''];
    const err = a.last_error;
    const sv = status.find((s) => s.account === a.id) || {};
    const mode = sv.mode || a.mode;
    const m = MODE[mode] || [mode || 'Not known', ''];
    const now = ctx.api.now();
    const patch = async ({ title, text = null, yes, body, extra = [], msg = 'Saved.' }) => {
      const r = await askPassword(ctx, {
        title, text, yes, extra, hint: HINT,
        run: guarded((cred, v) => ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: a.version, ...body(v) }))),
      });
      await after(r, msg);
      return r;
    };
    const pause = btn(a.paused ? 'Resume' : 'Pause', { kind: a.paused ? 'primary' : 'soft', icon: a.paused ? 'refresh' : 'minus', onclick: () => patch({
      title: a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`, yes: a.paused ? 'Resume' : 'Pause',
      text: a.paused ? 'Its matched car is read from TeslaFi again from now on.' : 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile; the history already stored stays.',
      body: () => ({ paused: !a.paused }), msg: a.paused ? `${a.label} resumed.` : `${a.label} paused.` }) });
    const token = btn('Replace the token', { kind: a.status === 'token_refused' ? 'primary' : 'soft', icon: 'key', onclick: () => {
      const f = field({ label: 'New TeslaFi API token', name: 'token', type: 'password', hint: TOKEN_HINT, attrs: { spellcheck: 'false', autocomplete: 'off', required: true } });
      return patch({ title: `New token for ${a.label}`, yes: 'Check with TeslaFi and replace', extra: [f],
        text: 'For a token TeslaFi stopped taking, or one you made anew at TeslaFi. The old token is deleted. If TeslaFi refuses the new one, nothing changes.',
        body: (v) => {
          const t = String(v.token || '').trim();
          if (!t) throw Object.assign(new Error('Paste the new token first.'), { fields: { token: 'Paste the token from TeslaFi, Settings, TeslaFi API.' } });
          return { token: t };
        }, msg: 'Token replaced and checked with TeslaFi.' });
    } });
    const zone = btn('Time zone', { kind: 'soft', icon: 'clock', onclick: () => patch({ title: `Time zone of ${a.label}`, yes: 'Save', extra: [zoneField(a.time_zone)],
      body: (v) => ({ time_zone: v.time_zone || null }), msg: 'Time zone saved.' }) });
    const rename = btn('Rename', { kind: 'soft', icon: 'edit', onclick: () => {
      const f = field({ label: 'Name', name: 'label', value: a.label, hint: 'Your own name for it, shown on this page.', attrs: { maxlength: '64', autocomplete: 'off', required: true } });
      return patch({ title: `Rename ${a.label}`, yes: 'Rename', extra: [f], body: (v) => ({ label: String(v.label || '').trim() }), msg: 'Renamed.' });
    } });
    const remove = btn('Remove', { kind: 'danger', icon: 'trash', onclick: async () => {
      const r = await askPassword(ctx, {
        title: `Remove ${a.label}?`, danger: true, yes: 'Remove the token', hint: HINT,
        text: 'Reading stops at once and the sealed file with its token is deleted. The history already stored from it stays, marked as data from TeslaFi.',
        run: guarded(async (cred) => (await ctx.api.del(`${BASE}/accounts/${enc(a.id)}`, { headers: { 'X-Sentry-Mate-Password': cred.password, ...(cred.two_step ? { 'X-Sentry-Mate-Code': cred.two_step } : {}) } })) ?? true),
      });
      await after(r, `${a.label} removed. Its history stays.`);
    } });
    const sevBad = a.status === 'token_refused' || a.status === 'error';
    const next = sv.next_request_ms ?? null;
    const doing = [m[0], sv.car_state || a.cars?.find((c) => c.current)?.car_state ? ` (TeslaFi says ${sv.car_state || a.cars.find((c) => c.current).car_state})` : '',
      next && mode !== 'off' ? `, ${mode === 'backoff' ? 'next try' : 'next read'} ${formatTime(next, ctx.tz)}${mode === 'backoff' ? ` (in ${formatDuration(Math.max(0, next - now))})` : ''}` : ''];
    return tile({ w: 2, title: a.label, icon: 'person', id: `teslafi-${a.id}`, cls: 'tesla-acc', side: chip(st[0], { tone: st[1] }) },
      st[2] ? said(sevBad ? 'warn' : a.status === 'ok' ? 'ok' : 'info', st[2]) : null,
      facts([
        ['Status', st[0]],
        ['What the server does', h('span', { title: m[1] || null }, doing)],
        ['Added', formatDateTime(a.added_ms, ctx.tz, { now })],
        ['Last good answer', a.last_ok_ms ? ago(a.last_ok_ms) : 'None yet'],
        ['Newest TeslaFi poll', sv.last_data_ms ? ago(sv.last_data_ms) : 'None yet'],
        ['Last error', err ? h('span', { class: 'tesla-err' }, cap1(lastErrorText(err)), err.message && LAST_ERROR_WORDS[err.code] ? h('span', { class: 'tile-line' }, err.message) : null,
          err.at_ms ? h('span', { class: 'tile-line' }, formatDateTime(err.at_ms, ctx.tz, { now })) : null) : 'None'],
        ['Requests today', `${formatNumber(a.polls_today ?? sv.polls_today ?? 0)} of ${formatNumber((ov.polls && ov.polls.daily_cap) || 1500)}`],
        ['Time zone', a.time_zone ? a.time_zone.replace(/_/g, ' ') : 'The server’s default'],
        ['TeslaFi host', h('span', { class: 'tesla-hosts' }, (a.hosts || []).map((x) => mono(x)), a.tls ? ` TLS ${a.tls}` : '')],
      ], { cls: 'tesla-facts' }),
      h('div', { class: 'form-actions tesla-acts' }, pause, token, zone, rename, remove));
  }

  /* ---------- an account's cars: match each */
  function carsTile(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    const takenBy = new Map();
    for (const o of accounts) {
      if (o.id === a.id) continue;
      for (const c of (carRows[o.id] && carRows[o.id].items) || []) if (c.match && c.match !== 'new' && c.match !== 'leave_out') takenBy.set(c.match, o.label);
    }
    const isCar = (v) => !!v && v !== 'new' && v !== 'leave_out';
    const nameOf = (c) => c.display_name || c.vehicle;
    const picked = new Map(items.map((c) => [c.vehicle, c.match ?? (takenBy.has(c.suggested) ? '' : c.suggested) ?? '']));
    const changed = () => items.filter((c) => (picked.get(c.vehicle) || null) !== (c.match ?? null));
    const doubled = () => {
      const by = new Map();
      for (const c of items) { const v = picked.get(c.vehicle); if (isCar(v)) by.set(v, [...(by.get(v) || []), c]); }
      return [...by].filter(([, list]) => list.length > 1);
    };
    const optsFor = (c) => [['', 'Not decided', 'Not read until you choose'],
      ...cars.map((x) => {
        const other = items.find((o) => o.vehicle !== c.vehicle && picked.get(o.vehicle) === x.id);
        return [x.id, x.name || x.id, takenBy.has(x.id) ? `Read through ${takenBy.get(x.id)} already: leave it out there first`
          : other ? `Picked for the TeslaFi car ${nameOf(other)} already: one car here takes one TeslaFi car` : 'Read into this car'];
      }),
      ['new', 'A new car', 'A new car is made for it here'], ['leave_out', 'Leave out', 'Never read']];
    const optLists = new Map();
    const save = btn('Save the matches', { kind: 'primary', icon: 'check', onclick: () => saveMatches() });
    const pending = h('p', { class: 'note' });
    const problem = h('div', { class: 'tesla-dup', role: 'alert', hidden: true });
    const sync = () => {
      const n = changed().length;
      const dup = doubled();
      for (const c of items) { const list = optLists.get(c.vehicle); if (list) list.splice(0, list.length, ...optsFor(c)); }
      problem.replaceChildren(...(dup.length ? [said('warn', dup.map(([car, list]) => `${list.map(nameOf).join(' and ')} are both matched to ${carName(car)}.`).join(' '), ' One car here takes one TeslaFi car: pick another car, A new car, or Leave out.')] : []));
      problem.hidden = !dup.length;
      save.hidden = !n;
      save.disabled = !!dup.length;
      pending.textContent = n ? `${n} ${n === 1 ? 'change' : 'changes'} not saved yet.` : '';
    };
    const rows = items.map((c) => {
      const sugg = c.match == null && c.suggested;
      const known = optsFor(c);
      if (!known.some(([v]) => v === picked.get(c.vehicle))) known.push([picked.get(c.vehicle), carName(picked.get(c.vehicle))]);
      optLists.set(c.vehicle, known);
      return h('div', { class: 'tesla-car' },
        pickRow({
          name: c.display_name || 'No name at TeslaFi',
          sub: [[c.year, c.model].filter(Boolean).join(' '), c.vin_prefix ? ` · ${c.vin_prefix}` : '', c.current ? ` · polled by TeslaFi now${c.car_state ? `, ${c.car_state}` : ''}` : '', sugg && !takenBy.has(c.suggested) ? ` · Suggested: ${carName(c.suggested)}, same VIN prefix` : ''],
          value: picked.get(c.vehicle), options: known, sheetTitle: `Car for ${c.display_name || c.vehicle}`,
          sheetNote: 'A car is read only once it is matched to a car here or to a new car. Saved with Save the matches.',
          onpick: (v) => { picked.set(c.vehicle, v); sync(); },
        }));
    });
    async function saveMatches() {
      const ch = changed();
      if (!ch.length || doubled().length) return;
      const r = await askPassword(ctx, {
        title: `Match the cars of ${a.label}`, yes: 'Save the matches', hint: HINT,
        text: h('ul', { class: 'set-list' }, ch.map((c) => { const v = picked.get(c.vehicle); return h('li', null, h('b', null, c.display_name || c.vehicle), ': ', v === 'new' ? 'a new car here, read from now on' : v === 'leave_out' ? 'left out, not read' : !v ? 'not decided, not read' : `${carName(v)}, read from now on`); })),
        run: guarded((cred) => ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, withPw(cred, { version: data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: picked.get(c.vehicle) || null })) }))),
      });
      await after(r, 'Matches saved.');
    }
    sync();
    return tile({ w: 2, title: `Cars of ${a.label}`, icon: 'car', side: chip(`${formatNumber(items.length)} at TeslaFi`, { tone: 'muted' }) },
      items.length ? h('div', { class: 'rows' }, h('div', { class: 'rows-list' }, rows))
        : said('info', 'TeslaFi has named no car for this token yet. It shows here once TeslaFi has polled it.'),
      problem,
      note('Pick the car TeslaFi’s data is read into. A suggestion has the same VIN prefix as a car here; it is not saved until you press Save the matches.'),
      h('div', { class: 'form-actions' }, save, pending));
  }

  return { el, load };
}
