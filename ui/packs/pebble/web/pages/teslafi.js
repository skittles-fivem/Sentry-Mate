// Sentry Mate, "Pebble" UI pack: TeslaFi, on Settings: Integrations (#teslafi).
//
// The TeslaFi input, for owners who pay for TeslaFi: this server can read TeslaFi's own polls of the car (about one
// a minute while it is awake) from TeslaFi's API with the owner's TeslaFi token, as a third live input beside SMO
// and the Tesla API. It is off until an admin adds a token; before that the server makes no connection to TeslaFi.
// It only reads (never a wake or a command), and no answer ever holds the token. Where SMO or the Tesla API cover
// the same time, theirs win, value by value; data from TeslaFi is labelled "Data from TeslaFi".
//
// Admins only, reading included. Tiles: the input's status (on or off, its host, TLS, requests today, the last
// error), its pace (Gentle / Normal / Often, PATCH integrations/teslafi), adding a token, and per account a tile with its status and actions
// (pause or resume, a new token, its time zone, rename, remove) and a tile of its cars, each matched to a car here,
// a new car or left out, with what the server does for it now. Every change asks for the admin's own password
// again (and the two-step code): askPassword(). Each account tile has the id teslafi-<account>.
//
// Data: GET/PATCH integrations/teslafi, GET/POST integrations/teslafi/accounts, GET/PATCH/DELETE
// integrations/teslafi/accounts/{id}, GET/PUT integrations/teslafi/accounts/{id}/cars, GET integrations/teslafi/status.

import { h } from '../shared/dom.js';
import * as state from '../shared/state.js';
import { formatTime, formatDateTime, formatDuration, isValidZone } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { tile, btn, chip, note, ago } from '../core/ui.js';
import { field, segmented } from '../core/form.js';
import { errorWords } from '../core/states.js';
import { facts } from '../core/bits.js';
import { srcPill } from '../core/source.js';
import { group, infoRow, pickRow, said, askPassword } from '../core/settings.js';

const enc = encodeURIComponent;
// the pace (PATCH integrations/teslafi { poll }): [value, word, its waits driving or charging / awake / asleep]
const PACE = [['gentle', 'Gentle', '2 min · 5 min · 20 min'], ['normal', 'Normal', '60 s · 3 min · 10 min'], ['often', 'Often', '15 s · 60 s · 3 min']];
const BASE = 'integrations/teslafi';
const HINT = 'TeslaFi changes ask for it again.';
const TESLAFI_SRC = { sources: [{ kind: 'teslafi_api', label: 'Data from TeslaFi' }] };

// account status: [chip word, tone, what it means]
export const TESLAFI_STATUS = {
  checking: ['Checking', 'muted', 'Just added: the server is asking TeslaFi for its car.'],
  ok: ['Reading', 'teal', 'TeslaFi takes the token; its matched car is read.'],
  paused: ['Paused', 'muted', 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile.'],
  token_refused: ['Token refused', 'sun', 'TeslaFi does not take this token any more (a wrong token, or the subscription ended), so nothing is asked of it. Replace the token, or remove the account.'],
  error: ['Error', 'bad', 'TeslaFi answers with errors. See the last error; the server tries again, waiting longer each time.'],
};
// what the server does for the account's car now: [words, why]
const MODE = {
  off: ['Not read', 'Not matched, left out, or the account is paused.'],
  asleep: ['Asleep', 'TeslaFi says the car sleeps: read at the asleep pace. These reads come from TeslaFi’s own database and never wake the car.'],
  idle: ['Parked', 'Parked and awake: read at the parked pace.'],
  charging: ['Charging', 'TeslaFi says it charges: read at the charging pace.'],
  driving: ['Driving', 'TeslaFi says it drives: read at the driving pace.'],
  backoff: ['Waiting after errors', 'TeslaFi answered with errors, asked to slow down, or the day’s requests are used up: nothing is asked until the next try.'],
};
const ERROR_WORDS = {
  teslafi_refused: 'TeslaFi did not take this token: check it on TeslaFi’s site (Settings, TeslaFi API), and that the subscription is still on. Nothing was kept.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was changed. Try again in a few minutes.',
  account_exists: 'This token’s car is already read through another TeslaFi account here (or the token is another car’s). Replace that account’s token instead, or remove it first.',
  car_taken: 'That car is already matched to another TeslaFi car. Leave it out there first, or pick another car.',
  no_such_teslafi_account: 'This TeslaFi account is not here any more: another admin may have removed it. Nothing was changed; the page now shows the accounts there are.',
  stale: 'Someone else changed this a moment ago, so nothing was saved. The page now shows their change: make yours again if it is still needed.',
};
const LAST_ERROR_WORDS = {
  token_refused: 'TeslaFi refused the token',
  teslafi_refused: 'TeslaFi refused the token',
  teslafi_unreachable: 'TeslaFi could not be reached',
  rate_limited: 'TeslaFi asked the server to slow down',
  daily_cap: 'the day’s 1,500 requests are used up',
  server_error: 'TeslaFi answered with a server error',
  timeout: 'TeslaFi took too long to answer',
};
const lastErrorText = (e) => (e ? (LAST_ERROR_WORDS[e.code] || e.message || e.code) : null);
const cap1 = (t) => (t ? t.charAt(0).toUpperCase() + t.slice(1) : t);

/** Our words on an error; the password sheet handles wrong_password and wrong_two_step itself. */
function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (ERROR_WORDS[e.code]) e.message = ERROR_WORDS[e.code];
  else if (e.status === 409) e.message = ERROR_WORDS.stale;
  if (e.code === 'teslafi_refused' || e.code === 'account_exists') e.fields = { ...(e.fields || {}), token: ERROR_WORDS[e.code] };
  if (e.fields) {
    const f = { ...e.fields };
    delete f.confirm_password; delete f.code;
    e.fields = Object.keys(f).length ? f : null;
  }
  return e;
}
const isStale = (e) => !!e && (e.code === 'no_such_teslafi_account' || e.code === 'stale_version' || e.code === 'version_conflict');
/** run() for askPassword: our words on an error; a stale page closes the sheet with { stale: words } instead. */
const guarded = (fn) => async (...a) => {
  try { return await fn(...a); } catch (e) { words(e); if (isStale(e)) return { stale: e.message }; throw e; }
};
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const mono = (t) => h('code', { class: 'set-mono' }, t);
const secs = (s) => (s == null ? '—' : s >= 120 && s % 60 === 0 ? `${formatNumber(s / 60)} min` : `${formatNumber(s)} s`);
const zones = () => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } };
const browserZone = () => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone; } catch { return null; } };
let listSeq = 0;
/** A time zone field with the zones this browser knows as suggestions. */
function zoneField(value, hint) {
  const id = `tf-zones-${++listSeq}`;
  const f = field({ label: 'TeslaFi’s time zone', name: 'time_zone', value: value || '', hint, attrs: { list: id, spellcheck: 'false', autocomplete: 'off', placeholder: browserZone() || 'America/Chicago' } });
  f.append(h('datalist', { id }, zones().map((z) => h('option', { value: z }))));
  return f;
}
const zoneError = (v) => (v && !isValidZone(v) ? 'Not a time zone this browser knows. Pick one from the list, for example America/Chicago.' : null);

export const teslafiAccountHref = (id) => `${state.buildUrl('settings-integrations')}#teslafi-${enc(id)}`;

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
    h('div', { class: 'tf-head-text' }, h('p', { class: 'note' }, 'For owners who pay for TeslaFi: this server can read TeslaFi’s own polls of the car with your TeslaFi token, as a third live input beside SMO and the Tesla API. It only reads; it never wakes the car or sends it a command. Where SMO or the Tesla API cover the same time, theirs win, value by value. Its data is marked:'),
      h('p', { class: 'src-line' }, srcPill(TESLAFI_SRC))));

  async function load() {
    if (!ctx.admin) {
      el.replaceChildren(head(), tile({ w: 2, title: 'TeslaFi', icon: 'key', side: chip('Admins only', { tone: 'muted' }) },
        said('info', 'Only an admin sees and changes the TeslaFi input: its token, its status and its car. Every page still says where its data came from, “Data from TeslaFi” included.')));
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
      el.replaceChildren(head(), tile({ w: 2, title: 'TeslaFi', icon: 'key', side: chip('Not read', { tone: 'bad' }) }, said('bad', 'The TeslaFi input could not be read: ', errorWords(e).text)));
      return;
    }
    if (!ctx.alive()) return;
    el.replaceChildren(head(), statusTile(), paceTile(), ...accounts.flatMap((a) => [accountTile(a), carsTile(a)]), addTile());
  }
  const reload = async (msg) => { if (msg) ctx.toast(msg); await load(); };
  const after = async (r, msg) => {
    if (!r) return false;
    if (r.stale) { ctx.toast(r.stale, { kind: 'info' }); await load(); return true; }
    await reload(typeof msg === 'function' ? msg(r) : msg);
    return true;
  };

  /* ---------- the input as a whole */
  function statusTile() {
    const cap = ((ov.polls && ov.polls.daily_cap) || 1500) * Math.max(1, ov.accounts || 0);
    const side = !ov.enabled ? chip('Off', { tone: 'muted', icon: 'minus' })
      : ov.cars ? chip(`Reading ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'}`, { tone: 'teal', icon: 'check' }) : chip('No car matched yet', { tone: 'sun', icon: 'warning' });
    const check = (level, name, sub, word) => h('li', { class: ['check', `is-${level}`] }, h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
      h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, name), sub ? h('span', { class: 'check-sub' }, sub) : null), h('span', { class: 'check-word' }, word));
    const le = ov.last_error;
    return tile({ w: 2, title: 'TeslaFi input', icon: 'key', side },
      !ov.enabled ? said('info', 'Off until a TeslaFi token is added. Until then the server makes no connection to TeslaFi at all.') : null,
      h('ul', { class: 'checks' },
        check(ov.enabled ? 'ok' : 'off', ov.enabled ? `${formatNumber(ov.accounts)} ${ov.accounts === 1 ? 'account' : 'accounts'}, ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'} read` : 'No TeslaFi token', ov.enabled ? 'Each matched car is read from TeslaFi as well.' : 'Add one below to read its car.', ov.enabled ? 'On' : 'Off'),
        check(ov.enabled ? 'ok' : 'off', 'Requests today', ov.enabled ? `Of at most ${formatNumber(cap)}: 1,500 a day per account, the checks of a new token included.` : 'None: there is no account.', formatNumber(ov.polls_today ?? 0)),
        check('ok', 'Connections set up', 'To TeslaFi, since the server started.', formatNumber(ov.clients_built ?? 0)),
        check(ov.tls === '1.2' ? 'warn' : 'ok', 'Encryption', ov.tls ? `TLS ${ov.tls}${ov.tls === '1.2' ? ': TeslaFi’s host offers no TLS 1.3' : ''}.` : 'No connection yet.', ov.tls ? `TLS ${ov.tls}` : '—'),
        check(le ? 'warn' : 'ok', 'Last error', le ? [cap1(lastErrorText(le)), le.at_ms ? `, ${formatDateTime(le.at_ms, ctx.tz, { now: ctx.api.now() })}` : ''] : 'None.', le ? 'Yes' : 'None')),
      group('Host it talks to', (ov.hosts || []).length ? ov.hosts.map((x) => infoRow({ icon: 'globe', name: mono(x), sub: 'Port 443, HTTPS only; the token in a header, never in an address; redirects never followed' }))
        : [infoRow({ icon: 'globe', name: 'None', sub: 'No host while there is no account. Once one is added: www.teslafi.com only, over HTTPS.' })]));
  }

  function paceTile() {
    const p = ov.polls || {};
    const seg = segmented({ name: 'How often TeslaFi is asked', value: ov.poll || 'normal', options: PACE.map(([v, w]) => [v, w]), onchange: (v) => setPace(v, seg) });
    return tile({ w: 2, title: 'How often TeslaFi is asked', icon: 'clock' },
      seg,
      facts(PACE.map(([v, w, t]) => [`${w}${v === (ov.poll || 'normal') ? ' (now)' : ''}`, t])),
      note('Each pace: driving or charging · parked and awake · asleep. Now, in detail:'),
      facts([
        ['Driving or charging', `every ${secs(p.driving_s)}`],
        ['Parked, awake', `every ${secs(p.parked_s)}`],
        ['Asleep', `every ${secs(p.asleep_s)}`],
        ['While SMO or the Tesla API sends', `every ${secs(p.other_live_s)} at most`],
        ['Between two requests', 'at least 10 s'],
        ['Requests a day', `at most ${formatNumber(p.daily_cap ?? 1500)} per account`],
        ['After an error', 'waits 1 min, doubling to 15 min'],
      ]),
      note('One pace for every TeslaFi account. Often reads about as fast as TeslaFi itself polls the car (up to 6,000 requests a day per account); reading faster would only read the same poll again. These reads come from TeslaFi’s database: they never wake the car and do not count against TeslaFi’s monthly command limit.'));
  }

  async function setPace(v, seg) {
    if (v === ov.poll) return;
    const w = PACE.find((x) => x[0] === v)[1];
    const r = await askPassword(ctx, {
      title: `Pace: ${w}`, yes: 'Change the pace', hint: HINT,
      text: 'How often the server reads each car from TeslaFi. The new pace takes effect at once.',
      run: guarded((cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v }))),
    });
    if (!(await after(r, `Pace saved: ${w.toLowerCase()}.`))) seg.set(ov.poll || 'normal');
  }

  /* ---------- adding an account */
  function addTile() {
    return tile({ w: 2, title: accounts.length ? 'Add another TeslaFi token' : 'Add a TeslaFi token', icon: 'plus' },
      h('ol', { class: 'set-list' },
        h('li', null, 'On TeslaFi’s site, open Settings, then TeslaFi API, and copy the token.'),
        h('li', null, 'Paste it here with your own Sentry Mate password. The server checks it with TeslaFi (one read, maybe a second 10 s later, so it can take a few seconds) before anything is kept.'),
        h('li', null, 'Match its car to a car here. Nothing is read until you do.')),
      h('div', { class: 'form-actions' }, btn('Add a TeslaFi token', { kind: 'primary', icon: 'key', onclick: () => addAccount() })),
      note('The token is sealed on the server and never shown again; no page and no answer holds it.'));
  }
  async function addAccount() {
    const label = field({ label: 'Name', name: 'label', value: 'TeslaFi', hint: 'Your own name for it, shown on this page.', attrs: { maxlength: '64', autocomplete: 'off', required: true } });
    const tok = field({ label: 'TeslaFi token', name: 'token', type: 'password', hint: 'From TeslaFi: Settings, TeslaFi API. Checked with TeslaFi before it is kept.', attrs: { spellcheck: 'false', autocomplete: 'off', required: true, class: 'input-mono' } });
    const zone = zoneField(browserZone(), 'The zone TeslaFi shows your times in (TeslaFi: Settings, Account). TeslaFi’s times carry no zone, so they are read in this one. Empty: the server’s default zone.');
    const r = await askPassword(ctx, {
      title: 'Add a TeslaFi token', yes: 'Check with TeslaFi and add', extra: [label, tok, zone], hint: HINT,
      text: 'Only paste the token of your own TeslaFi account. Anyone with it can read the car’s data at TeslaFi, so it never leaves this server and no page shows it.',
      run: async (cred, v) => {
        const bad = {};
        if (!String(v.label || '').trim()) bad.label = 'Give it a name.';
        if (!String(v.token || '').trim()) bad.token = 'Paste the token.';
        const tz = String(v.time_zone || '').trim();
        if (zoneError(tz)) bad.time_zone = zoneError(tz);
        if (Object.keys(bad).length) throw Object.assign(new Error('Not added yet.'), { fields: bad });
        try { return await ctx.api.post(`${BASE}/accounts`, withPw(cred, { label: v.label.trim(), token: v.token.trim(), time_zone: tz || null })); } catch (e) { throw words(e); }
      },
    });
    if (r) await reload(`${r.label || 'The account'} added and checked with TeslaFi. Match its car below to start reading it.`);
  }

  /* ---------- one account */
  function accountTile(a) {
    const st = TESLAFI_STATUS[a.status] || [a.status, 'muted', ''];
    const err = a.last_error;
    const md = MODE[a.mode] || [a.mode || 'Not known', ''];
    const patch = async ({ title, text = null, yes, body, extra = [], msg = 'Saved.', danger = false, check = null }) => {
      const r = await askPassword(ctx, {
        title, text, yes, extra, danger, hint: HINT,
        run: guarded(async (cred, v) => {
          const bad = check ? check(v) : null;
          if (bad) throw Object.assign(new Error('Not saved yet.'), { fields: bad });
          return ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: a.version, ...body(v) }));
        }),
      });
      await after(r, msg);
      return r;
    };
    const pause = btn(a.paused ? 'Resume' : 'Pause', { kind: a.paused ? 'primary' : 'soft', icon: a.paused ? 'refresh' : 'minus', onclick: () => patch({
      title: a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`, yes: a.paused ? 'Resume' : 'Pause',
      text: a.paused ? 'Its matched car is read from TeslaFi again from now on.' : 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile; the history already stored stays.',
      body: () => ({ paused: !a.paused }), msg: a.paused ? `${a.label} resumed.` : `${a.label} paused.` }) });
    const token = btn('Replace the token', { kind: a.status === 'token_refused' ? 'primary' : 'soft', icon: 'key', onclick: () => {
      const f = field({ label: 'New TeslaFi token', name: 'token', type: 'password', hint: 'Checked with TeslaFi first. If TeslaFi refuses it, nothing is changed and the old token stays.', attrs: { spellcheck: 'false', autocomplete: 'off', required: true, class: 'input-mono' } });
      return patch({ title: `New token for ${a.label}`, yes: 'Check with TeslaFi and replace', extra: [f],
        text: 'For a token TeslaFi stopped taking, or one you made again on TeslaFi’s site. The old token is deleted.',
        check: (v) => (!String(v.token || '').trim() ? { token: 'Paste the new token.' } : null),
        body: (v) => ({ token: String(v.token || '').trim() }), msg: 'Token replaced and checked with TeslaFi.' });
    } });
    const zoneBtn = btn('Time zone', { kind: 'soft', icon: 'clock', onclick: () => {
      const f = zoneField(a.time_zone, 'As TeslaFi shows it (TeslaFi: Settings, Account). TeslaFi’s times are read in it from now on. Empty: the server’s default zone.');
      return patch({ title: `Time zone of ${a.label}`, yes: 'Save the zone', extra: [f],
        check: (v) => { const z = String(v.time_zone || '').trim(); return zoneError(z) ? { time_zone: zoneError(z) } : null; },
        body: (v) => ({ time_zone: String(v.time_zone || '').trim() || null }), msg: 'Time zone saved.' });
    } });
    const rename = btn('Rename', { kind: 'soft', icon: 'edit', onclick: () => {
      const f = field({ label: 'Name', name: 'label', value: a.label, hint: 'Your own name for it.', attrs: { maxlength: '64', autocomplete: 'off', required: true } });
      return patch({ title: `Rename ${a.label}`, yes: 'Rename', extra: [f],
        check: (v) => (!String(v.label || '').trim() ? { label: 'Give it a name.' } : null),
        body: (v) => ({ label: String(v.label || '').trim() }), msg: 'Renamed.' });
    } });
    const remove = btn('Remove', { kind: 'danger', icon: 'trash', onclick: async () => {
      const r = await askPassword(ctx, {
        title: `Remove ${a.label}?`, danger: true, yes: 'Remove the account', hint: HINT,
        text: 'Reading stops at once and the sealed file with the token is deleted. The history already stored from it stays, marked “Data from TeslaFi”.',
        run: guarded(async (cred) => (await ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred))) ?? true),
      });
      await after(r, `${a.label} removed. Its history stays.`);
    } });
    const bad = a.status === 'token_refused' || a.status === 'error';
    return tile({ w: 2, title: a.label, icon: 'person', id: `teslafi-${a.id}`, cls: 'tesla-acc', side: chip(st[0], { tone: st[1] }) },
      st[2] ? said(bad ? 'warn' : a.status === 'ok' ? 'ok' : 'info', st[2]) : null,
      facts([
        ['Status', st[0]],
        ['What the server does', h('span', { title: md[1] || null }, a.paused ? 'Paused: nothing is asked' : md[0])],
        ['TeslaFi’s time zone', a.time_zone ? a.time_zone.replace(/_/g, ' ') : 'The server’s default'],
        ['Added', formatDateTime(a.added_ms, ctx.tz, { now: ctx.api.now() })],
        ['Last good answer', a.last_ok_ms ? ago(a.last_ok_ms) : 'None yet'],
        ['Last error', err ? h('span', { class: 'tesla-err' }, cap1(lastErrorText(err)), err.message && LAST_ERROR_WORDS[err.code] ? h('span', { class: 'tile-line' }, err.message) : null,
          err.at_ms ? h('span', { class: 'tile-line' }, formatDateTime(err.at_ms, ctx.tz, { now: ctx.api.now() })) : null) : 'None'],
        ['Requests today', `${formatNumber(a.polls_today ?? 0)} of 1,500`],
        ['Encryption', a.tls ? `TLS ${a.tls}` : 'No connection yet'],
        ['Host', h('span', { class: 'tesla-hosts' }, (a.hosts || []).map((x) => mono(x)))],
      ], { cls: 'tesla-facts' }),
      h('div', { class: 'form-actions tesla-acts' }, pause, token, zoneBtn, rename, remove));
  }

  /* ---------- an account's cars: match each, and what the server does for it now */
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
          : other ? `Picked for the TeslaFi car ${nameOf(other)} already` : 'Read into this car', state.isSold(x)];
      }),
      ['new', 'A new car', 'A new car is made for it here'], ['leave_out', 'Leave out', 'Never read']];
    const save = btn('Save the matches', { kind: 'primary', icon: 'check', onclick: () => saveMatches() });
    const pending = h('p', { class: 'note' });
    const problem = h('div', { class: 'tesla-dup', role: 'alert', hidden: true });
    const sync = () => {
      const n = changed().length;
      const dup = doubled();
      problem.replaceChildren(...(dup.length ? [said('warn', dup.map(([car, list]) => `${list.map(nameOf).join(' and ')} are both matched to ${carName(car)}.`).join(' '), ' One car here takes one TeslaFi car.')] : []));
      problem.hidden = !dup.length;
      save.hidden = !n;
      save.disabled = !!dup.length;
      pending.textContent = n ? `${n} ${n === 1 ? 'change' : 'changes'} not saved yet.` : '';
    };
    const statusOf = (c) => status.find((s) => s.account === a.id && s.vehicle === c.vehicle) || {};
    const modeText = (c) => {
      const sv = statusOf(c);
      const mode = sv.mode || (a.paused ? 'off' : a.mode);
      const m = MODE[mode] || [mode || 'Not known', ''];
      const bits = [m[0]];
      const next = sv.next_request_ms ?? null;
      if (mode === 'backoff' && next) bits.push(`next try ${formatTime(next, ctx.tz)} (in ${formatDuration(Math.max(0, next - ctx.api.now()))})`);
      else if (next && mode !== 'off') bits.push(`next read ${formatTime(next, ctx.tz)}`);
      if (sv.last_data_ms) bits.push(['TeslaFi’s newest poll ', ago(sv.last_data_ms)]);
      if (sv.last_error) bits.push(cap1(lastErrorText(sv.last_error)));
      return { mode, text: bits.filter(Boolean).map((b, i) => (i ? [', ', b] : b)), why: m[1] };
    };
    const rows = items.map((c) => {
      const sugg = c.match == null && c.suggested;
      const known = optsFor(c);
      if (!known.some(([v]) => v === picked.get(c.vehicle))) known.push([picked.get(c.vehicle), carName(picked.get(c.vehicle))]);
      const m = modeText(c);
      return h('div', { class: 'tesla-car' },
        pickRow({
          name: c.display_name || 'No name at TeslaFi',
          sub: [[c.year, c.model].filter(Boolean).join(' '), c.vin_prefix ? ` · ${c.vin_prefix}` : '', c.car_state ? ` · TeslaFi says ${c.car_state}` : '', c.current ? ' · polled now' : '', sugg && !takenBy.has(c.suggested) ? ` · Suggested: ${carName(c.suggested)}, same VIN prefix` : ''],
          value: picked.get(c.vehicle), options: known, sheetTitle: `Car for ${c.display_name || c.vehicle}`,
          sheetNote: 'A car gets TeslaFi’s data only once it is matched to a car here or to a new car. Saved with Save the matches.',
          onpick: (v) => { picked.set(c.vehicle, v); sync(); },
        }),
        h('p', { class: ['tesla-mode', { 'is-warn': m.mode === 'backoff' }], title: m.why || null },
          h('span', { class: ['src-dot', m.mode === 'off' ? 'tone-muted' : m.mode === 'backoff' ? 'tone-sun' : 'tone-coral'], 'aria-hidden': 'true' }),
          h('span', null, m.text)));
    });
    async function saveMatches() {
      const ch = changed();
      if (!ch.length || doubled().length) return;
      const r = await askPassword(ctx, {
        title: `Match the car of ${a.label}`, yes: 'Save the matches', hint: HINT,
        text: h('ul', { class: 'set-list' }, ch.map((c) => { const v = picked.get(c.vehicle); return h('li', null, h('b', null, nameOf(c)), ': ', v === 'new' ? 'a new car here, read from now on' : v === 'leave_out' ? 'left out, not read' : !v ? 'not decided, not read' : `${carName(v)}, read from now on`); })),
        run: guarded((cred) => ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, withPw(cred, { version: data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: picked.get(c.vehicle) || null })) }))),
      });
      await after(r, 'Matches saved.');
    }
    sync();
    return tile({ w: 2, title: `Car of ${a.label}`, icon: 'car', side: chip(`${formatNumber(items.length)} at TeslaFi`, { tone: 'muted' }) },
      items.length ? h('div', { class: 'rows' }, h('div', { class: 'rows-list' }, rows))
        : said('info', 'TeslaFi named no car yet. Its car shows here once TeslaFi’s latest poll names it.'),
      problem,
      note('Pick the car TeslaFi’s data goes to. A suggestion has the same VIN prefix as a car here; nothing is saved until you press Save the matches. A new VIN in TeslaFi’s polls is added here as a car not matched yet.'),
      h('div', { class: 'form-actions' }, save, pending));
  }

  return { el, load };
}

/** Facts rows for one car's TeslaFi input (Data health), or null when no TeslaFi car reads it. */
export function teslafiCarFacts(ctx, statusItems, carId) {
  const s = (statusItems || []).find((x) => x.car === carId);
  if (!s) return null;
  const m = MODE[s.mode] || [s.mode, ''];
  const now = ctx.api.now();
  return [
    ['What the server does', `${m[0]}${s.car_state ? ` (TeslaFi says ${s.car_state})` : ''}`],
    ['TeslaFi’s newest poll', s.last_data_ms ? formatDateTime(s.last_data_ms, ctx.tz, { now }) : 'None yet'],
    [s.mode === 'backoff' ? 'Next try' : 'Next read', s.next_request_ms ? formatTime(s.next_request_ms, ctx.tz) : 'Not planned'],
    ['TeslaFi requests today', formatNumber(s.polls_today ?? 0)],
    s.last_error ? ['TeslaFi error', cap1(lastErrorText(s.last_error))] : null,
    ['Account', h('a', { class: 'place-link', href: teslafiAccountHref(s.account) }, 'Open its TeslaFi account')],
  ].filter(Boolean);
}
