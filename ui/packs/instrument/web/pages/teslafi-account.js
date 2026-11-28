// Settings: Integrations, the TeslaFi section (#teslafi): the TeslaFi input. For owners who pay for TeslaFi: this
// server reads TeslaFi's own polls of the car with the owner's TeslaFi token, as a third live input beside SMO and
// the Tesla API. Off until an admin adds a token; before that the server makes no connection to TeslaFi at all. It
// only reads (never a wake or a command), and no answer ever holds the token.
//
// Admins only, reading included. Every change but saving the car match asks for the admin's own password again (and
// the two-step code), like the Tesla account section: askPassword() from settings-server-move.js, sent as
// confirm_password and code. The car match takes the admin's session alone (it stores and shows no token).
//
// API: GET integrations/teslafi, GET/POST integrations/teslafi/accounts, PATCH/DELETE
// integrations/teslafi/accounts/{id}, GET/PUT integrations/teslafi/accounts/{id}/cars, GET/POST
// integrations/teslafi/accounts/{id}/redate (data stored while the zone was wrong, dated again), GET
// integrations/teslafi/status.

import { h, replace } from '../shared/dom.js';
import { icon } from '../core/icons.js';
import { soldParts, soldOptions } from '../core/carswitch.js';
import { kv, tag, pill, note, metric, figures } from '../core/page.js';
import { field } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { dateTime, ago, num, dur, time, tzName } from '../core/fmt.js';
import { askPassword } from './settings-server-move.js';
import { invoicesBlock, loadUnmatched } from './invoices-block.js';

const enc = encodeURIComponent;
const BASE = 'integrations/teslafi';
const LOCAL = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
const at = (ms) => `${dateTime(ms, LOCAL)} ${tzName(LOCAL)}`;
const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();

const STATUS = {
  checking: ['Checking', 'plain', 'Just added: the server is asking TeslaFi for its car.'],
  ok: ['Reading', 'live', 'TeslaFi takes the token; its matched car is read.'],
  paused: ['Paused', 'plain', 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile.'],
  token_refused: ['Token refused', 'warm', 'TeslaFi does not take the token any more (a wrong token, or the TeslaFi subscription ended), so nothing is asked until you give a new one with Replace token.'],
  error: ['Not read', 'warm', 'TeslaFi answers with errors. See the last error; the server tries again by itself, waiting longer each time.'],
};
const MODE = {
  off: ['Not read', 'Not matched, left out, paused, or its token is refused.'],
  asleep: ['Asleep', 'TeslaFi says the car sleeps: read at the asleep pace (TeslaFi\'s own database; the car is never woken).'],
  idle: ['Parked', 'Read at the awake pace.'],
  charging: ['Charging', 'Read at the driving or charging pace.'],
  driving: ['Driving', 'Read at the driving or charging pace.'],
  backoff: ['Waiting after errors', 'TeslaFi answered with errors, or asked the server to wait (429): nothing is asked until the next try.'],
};
// the pace choice (one setting for every TeslaFi account); the waits of the chosen one come back in the overview's `polls`
const PACE = [
  ['gentle', 'Gentle', ['2 min', '5 min', '20 min'], 'Fewer requests: for a car that needs no fine detail from TeslaFi.'],
  ['normal', 'Normal', ['60 s', '3 min', '10 min'], 'The default: about as often as TeslaFi itself polls an awake car.'],
  ['often', 'Often', ['15 s', '60 s', '3 min'], 'As fast as TeslaFi refreshes: finer drives and charges, with more requests to TeslaFi (it may limit them: the server then waits as it asks).'],
];
const paceLine = ([d, a, s]) => `driving or charging every ${d}, awake every ${a}, asleep every ${s}`;
const ERROR_WORDS = {
  teslafi_refused: 'TeslaFi did not take this token: check it on TeslaFi (Settings, TeslaFi API), and that the TeslaFi subscription is active. Nothing was changed.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was changed. Try again in a few minutes.',
  account_exists: 'This token\'s car is already read through another TeslaFi account here. Replace that account\'s token instead, or remove it first.',
  car_taken: 'That car is already matched to the car of another TeslaFi account here. Leave it out there first, or pick another car.',
  no_such_teslafi_account: 'This TeslaFi account is not here any more: another admin may have removed it. The page now shows the accounts there are.',
  version_conflict: 'Someone else changed this a moment ago, so nothing was saved. The page now shows their change: make yours again if it is still needed.',
};
const LAST_ERROR = {
  token_refused: 'TeslaFi refused the token',
  teslafi_unreachable: 'TeslaFi could not be reached',
  rate_limited: 'TeslaFi asked the server to slow down',
  server_error: 'TeslaFi answered with a server error',
  timeout: 'TeslaFi took too long to answer',
};
export const limitedText = (until, tz) => `TeslaFi is limiting requests, next try at ${dateTime(until, tz)}.`;
const limitedNote = (until, tz, cls = 'setting-note') => h('p', { class: [cls, 'warn', 'ta-limited'], role: 'status' }, icon('warn', 'ico--sm'),
  h('span', null, h('b', null, limitedText(until, tz)), ' This server sets no daily limit of its own: TeslaFi does the limiting, and the server waits as long as TeslaFi asks (at least a minute).'));
const lastErrorText = (e) => (e ? (LAST_ERROR[e.code] || e.message || e.code) : null);

function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (e.code === 'stale_version' || (e.status === 409 && !ERROR_WORDS[e.code])) e.message = ERROR_WORDS.version_conflict;
  else if (ERROR_WORDS[e.code]) e.message = ERROR_WORDS[e.code];
  // a refused or unreachable token: the message goes under the token field
  if (e.code === 'teslafi_refused' || e.code === 'teslafi_unreachable' || e.code === 'account_exists') e.fields = { token: e.message };
  if (e.fields) { delete e.fields.confirm_password; delete e.fields.code; if (!Object.keys(e.fields).length) e.fields = null; }
  return e;
}
const isStale = (e) => e && (e.code === 'no_such_teslafi_account' || e.code === 'stale_version' || (e.status === 409 && !ERROR_WORDS[e.code]));
const guarded = (fn) => async (...a) => {
  try { return await fn(...a); } catch (e) { words(e); if (isStale(e)) return { stale: e.message }; throw e; }
};
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const code = (t) => h('code', { class: 'si-code' }, t);
const secs = (s) => (s == null ? '—' : s >= 120 && s % 60 === 0 ? `${num(s / 60)} min` : `${num(s)} s`);
const tokenField = (label, hint) => field({ label, name: 'token', path: 'token', type: 'password', required: true, hint,
  attrs: { spellcheck: 'false', autocomplete: 'off', maxlength: '256' } });
const zoneField = (value) => field({ label: 'TeslaFi\'s time zone', name: 'time_zone', path: 'time_zone', value: value || '',
  hint: 'The zone TeslaFi shows its times in (TeslaFi, Settings, Account), such as America/Chicago. Empty: the server\'s own zone.',
  attrs: { list: 'tf-zones', autocomplete: 'off', spellcheck: 'false', placeholder: 'America/Chicago' } });
/** A time as a datetime-local input's value, in the browser's zone (`2026-11-26T13:58`). */
const localInput = (ms) => {
  if (ms == null) return '';
  const d = new Date(ms - new Date(ms).getTimezoneOffset() * 60000);
  return d.toISOString().slice(0, 16);
};
const zoneList = () => h('datalist', { id: 'tf-zones' }, ZONES.map((z) => h('option', { value: z })));
const zoneOk = (z) => !z || !ZONES.length || ZONES.includes(z) || z === 'UTC';

/** The TeslaFi section. Returns { el, load() } (load reads and paints; the page awaits it before ready()). */
export function teslafiSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const body = h('div', { class: 'ta-body' });
  const headPill = h('span', { class: 'ta-head-pill' });
  const el = h('section', { class: 'si-section ta tf', id: 'teslafi', 'aria-label': 'TeslaFi' },
    h('div', { class: 'si-section-head' }, h('h2', { class: 'si-h2' }, icon('globe'), 'TeslaFi'), headPill),
    h('p', { class: 'note' }, 'For owners who pay for TeslaFi: a third live input beside SMO and the Tesla API. The server reads TeslaFi\'s own polls of the car (about one a minute while it is awake) with your TeslaFi token. It only reads: it never wakes the car or sends a command, and its reads do not count against TeslaFi\'s monthly command limit. Off until an admin adds a token. Where SMO or the Tesla API cover the same time, theirs win, value by value; rows from it say "Data from TeslaFi".'),
    body);

  let ov = null, accounts = [], status = [], carRows = {}, unmatched = [];
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  async function load() {
    if (!admin) {
      replace(headPill, pill('Admins only'));
      replace(body, h('div', { class: 'panel panel-pad' }, note(icon('lock', 'ico--sm'), ' Only an admin sees and changes TeslaFi: its token, status and car. Every page still says where its data came from, "Data from TeslaFi" included.')), h('div', { hidden: true, cover: 'inv.settings.teslafi' }));
      return;
    }
    try {
      [ov, accounts, status] = await Promise.all([
        ctx.api.get(BASE),
        ctx.api.list(`${BASE}/accounts`).all().catch(() => []),
        ctx.api.list(`${BASE}/status`).all().catch(() => []),
        loadUnmatched(ctx, 'teslafi'),
      ]).then(([a, b, c, d]) => { unmatched = d; return [a, b, c]; });
      const rows = await Promise.all(accounts.map((a) => ctx.api.get(`${BASE}/accounts/${enc(a.id)}/cars`).catch(() => null)));
      carRows = Object.fromEntries(accounts.map((a, i) => [a.id, rows[i] || { items: a.cars || [], version: a.version }]));
    } catch (e) {
      if (e?.name === 'AbortError') return;
      replace(headPill, pill('Not read', { tone: 'warm' }));
      replace(body, h('div', { class: 'panel panel-pad' }, h('p', { class: 'warn' }, icon('warn', 'ico--sm'), ' TeslaFi\'s settings could not be read: ', errorWords(e).text)));
      return;
    }
    if (!ctx.alive()) return;
    paint();
  }
  const after = async (r, msg) => {
    if (!r) return;
    if (r.stale) { flash(r.stale, { kind: 'info' }); await load(); return; }
    flash(typeof msg === 'function' ? msg(r) : msg);
    await load();
  };

  function counts() {
    let matched = 0, read = 0;
    for (const a of accounts) {
      for (const c of carRows[a.id]?.items || []) {
        if (c.match == null || c.match === 'leave_out') continue;
        matched++;
        if (!a.paused && a.mode && a.mode !== 'off') read++;
      }
    }
    return { matched, read };
  }

  function paint() {
    const { matched, read } = counts();
    replace(headPill, !ov.enabled ? pill('Off until a token is added', { lg: true })
      : accounts.some((a) => a.status === 'token_refused') ? pill('Token refused', { tone: 'warm', lg: true })
        : read ? pill(`Reading ${read === 1 ? 'one car' : `${num(read)} cars`}`, { tone: 'live', lg: true })
          : matched && accounts.every((a) => a.paused) ? pill('Paused', { lg: true }) : pill(matched ? 'No car is read now' : 'No car matched yet', { tone: 'warm', lg: true }));
    replace(body, h('div', { class: 'ta-grid' }, statusPanel(), addPanel()),
      invoicesBlock(ctx, { source: 'teslafi', ov, base: BASE, unmatched, guarded, withPw, after }), accounts.map(accountPanel));
  }

  function statusPanel() {
    const p = ov.polls || {};
    const poll = ov.poll || 'normal';
    const paceSeg = h('div', { class: 'seg', role: 'group', 'aria-label': 'How often TeslaFi is asked' },
      PACE.map(([v, l, w]) => h('button', { class: 'seg-btn', type: 'button', title: `${l}: ${paceLine(w)}`, 'aria-pressed': String(poll === v), onclick: () => setPace(v) }, l)));
    return h('div', { class: 'panel panel-pad ta-status' },
      h('div', { class: 'panel-head' }, h('h3', { class: 'panel-title' }, icon('pulse'), 'Status')),
      ov.rate_limited_until_ms ? limitedNote(ov.rate_limited_until_ms, LOCAL) : null,
      figures([
        metric('State', ov.enabled ? 'On' : 'Off', null, { size: 'sm', explain: ov.enabled ? `${num(ov.accounts)} ${ov.accounts === 1 ? 'account' : 'accounts'}, ${num(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'} matched.` : 'Off until a token is added: no connection to TeslaFi is made.' }),
        metric('Requests today', ov.polls_today, 'number', { size: 'sm', digits: 0, explain: ov.enabled ? 'Since midnight UTC, for information: there is no cap on this side.' : 'None: there is no account.' }),
        metric('Connection', ov.tls ? `TLS ${ov.tls}` : '—', null, { size: 'sm', explain: ov.tls ? 'To www.teslafi.com on 443; the token goes in a header, never in an address.' : 'None yet.' }),
      ], 'si-figs'),
      ov.last_error ? h('p', { class: 'si-last-error warn' }, icon('warn', 'ico--sm'), ' Last error: ', h('b', null, lastErrorText(ov.last_error)),
        ov.last_error.at_ms ? `, ${at(ov.last_error.at_ms)} (${ago(ov.last_error.at_ms)})` : '', '.') : h('p', { class: 'si-last-error faint' }, icon('check', 'ico--sm'), ' No error.'),
      h('div', { class: 'ta-hosts' }, h('span', { class: 'metric-label' }, 'Host it talks to'),
        ov.hosts?.length ? h('ul', { class: 'ta-host-list' }, ov.hosts.map((x) => h('li', null, code(x))))
          : h('p', { class: 'faint' }, 'None while there is no token. Once one is added: www.teslafi.com only, over HTTPS.')),
      h('div', { class: 'ta-pace' },
        h('div', { class: 'ta-pace-head' }, h('span', { class: 'metric-label' }, 'How often TeslaFi is asked'), paceSeg),
        h('ul', { class: 'tf-pace-opts' }, PACE.map(([v, l, w]) => h('li', { class: poll === v ? 'is-on' : null },
          h('b', null, l), poll === v ? ' (now)' : '', `: ${paceLine(w)}.`))),
        h('dl', { class: 'ta-pace-list' },
          [['A driving car', p.driving_s], ['A charging car', p.charging_s], ['A parked car', p.parked_s], ['An asleep car', p.asleep_s]]
            .map(([l, s]) => h('div', null, h('dt', null, l), h('dd', null, l.endsWith('live') ? `at most every ${secs(s)}` : `every ${secs(s)}`)))),
        h('p', { class: 'setting-note' }, `${PACE.find((x) => x[0] === poll)?.[3] || ''} Whatever the pace: at most one request every 10 s per account, with no daily cap on this side (TeslaFi does the limiting: its requests to slow down are always obeyed and shown here), and after an error it waits 60 s, doubling up to 15 minutes. These reads come from TeslaFi's own database: they never wake the car.`)));
  }
  async function setPace(v) {
    if (v === (ov.poll || 'normal')) return;
    const pc = PACE.find((x) => x[0] === v);
    const r = await askPassword(ctx, {
      title: `Pace: ${pc[1]}`,
      intro: h('p', { class: 'note' }, `How often the server reads TeslaFi's polls, for every TeslaFi account: ${paceLine(pc[2])}. TeslaFi has new data only as often as it polls the car itself, so a faster pace mostly shortens the wait for a new poll.`),
      submit: 'Change the pace',
      run: guarded((cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v }))),
    });
    await after(r, `How often TeslaFi is asked: ${pc[1]}, saved.`);
  }

  function addPanel() {
    return h('div', { class: 'panel panel-pad ta-add' },
      h('div', { class: 'panel-head' }, h('h3', { class: 'panel-title' }, icon('plus'), accounts.length ? 'Add another TeslaFi token' : 'Add a TeslaFi token')),
      h('ol', { class: 'ta-step-list' },
        h('li', null, 'In TeslaFi, open Settings, then ', h('b', null, 'TeslaFi API'), ', and copy the API token there.'),
        h('li', null, 'Add it here with your own Sentry Mate password. The server checks it with TeslaFi (one read; when TeslaFi names no car yet, one more 10 s later) before anything is kept.'),
        h('li', null, 'Match its car to a car here. Nothing is read before that.')),
      h('button', { class: 'btn btn--primary', type: 'button', onclick: addToken }, icon('lock', 'ico--sm'), 'Add a TeslaFi token'),
      h('p', { class: 'setting-note' }, 'The token is sealed on the server and never shown again, not even to an admin.'));
  }

  async function addToken() {
    const label = field({ label: 'Name for it', name: 'label', required: true, value: 'TeslaFi', hint: 'Your own name for it, shown on this page.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const tok = tokenField('TeslaFi API token', 'From TeslaFi, Settings, TeslaFi API. Checked with TeslaFi before it is kept.');
    const zone = zoneField('');
    const r = await askPassword(ctx, {
      title: 'Add a TeslaFi token', submit: 'Check with TeslaFi and add', extra: [label, tok, zone],
      intro: h('div', null, h('p', { class: 'note' }, 'Anyone with this token can read your car\'s TeslaFi data, so it never leaves this server and no page shows it. Checking it can take about 10 seconds.'), zoneList()),
      run: guarded(async (cred, v) => {
        const z = v.time_zone.trim();
        if (!zoneOk(z)) { const e = new Error(); e.fields = { time_zone: `${z} is not a time zone. Pick one from the list, such as America/Chicago.` }; throw e; }
        return ctx.api.post(`${BASE}/accounts`, withPw(cred, { label: v.label.trim(), token: v.token.trim(), ...(z ? { time_zone: z } : {}) }));
      }),
    });
    await after(r, (a) => `${a.label} added and checked with TeslaFi. Match its car below to start reading it.`);
  }

  function accountPanel(a) {
    const unmatched = a.status === 'ok' && !(carRows[a.id]?.items || a.cars || []).some((c) => c.match && c.match !== 'leave_out');
    const st = unmatched ? ['Car not matched', 'warm', 'TeslaFi takes the token, but nothing is read until its car is matched below.'] : STATUS[a.status] || [a.status, 'plain', ''];
    const mode = MODE[a.mode] || [a.mode, ''];
    const err = a.last_error;
    const patch = (title, intro, submit, bodyOf, extra = [], msg = 'Saved.') => async () => {
      const r = await askPassword(ctx, {
        title, intro: intro ? h('div', { class: 'tf-intro' }, h('p', { class: 'note' }, intro), zoneList()) : zoneList(), submit, extra,
        run: guarded((cred, v) => {
          const b = bodyOf(v);
          if ('time_zone' in b && !zoneOk(b.time_zone)) { const e = new Error(); e.fields = { time_zone: `${b.time_zone} is not a time zone. Pick one from the list.` }; throw e; }
          return ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: a.version, ...b }));
        }),
      });
      await after(r, msg);
    };
    const pauseBtn = h('button', { class: 'btn btn--sm', type: 'button', onclick: patch(a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`,
      a.paused ? 'Its matched car is read again from now on.' : 'Its car is not read until you resume it. Nothing is asked of TeslaFi meanwhile; the history already stored stays.',
      a.paused ? 'Resume' : 'Pause', () => ({ paused: !a.paused }), [], a.paused ? `${a.label} resumed.` : `${a.label} paused.`) }, icon(a.paused ? 'live' : 'moon', 'ico--sm'), a.paused ? 'Resume' : 'Pause');
    const tokenBtn = h('button', { class: ['btn btn--sm', a.status === 'token_refused' ? 'btn--primary' : 'btn--ghost'], type: 'button', onclick: () => {
      const f = tokenField('New TeslaFi API token', 'Checked with TeslaFi first. When TeslaFi does not take it, nothing changes and the old token stays.');
      return patch(`New token for ${a.label}`, 'For a token TeslaFi stopped taking, or one you made again on TeslaFi. The old token is deleted once the new one is checked.', 'Check with TeslaFi and replace', (v) => ({ token: v.token.trim() }), [f], 'Token replaced and checked with TeslaFi.')();
    } }, icon('lock', 'ico--sm'), 'Replace token');
    const editBtn = h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => {
      const n = field({ label: 'Name', name: 'label', required: true, value: a.label, attrs: { maxlength: '64', autocomplete: 'off' } });
      const z = zoneField(a.time_zone);
      return patch(`Change ${a.label}`, 'After a change of the time zone TeslaFi\'s times are read in the new zone at once.', 'Save', (v) => ({ label: v.label.trim(), time_zone: v.time_zone.trim() || null }), [n, z], 'Saved.')();
    } }, icon('settings', 'ico--sm'), 'Name and zone');
    // data stored while the zone was wrong: dated again, once (the moment and the old zone from the audit log)
    const redateBtn = h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: async () => {
      let d = {};
      try { d = await ctx.api.get(`${BASE}/accounts/${enc(a.id)}/redate`); } catch (e) { flash(errorWords(e).text, { kind: 'error' }); return; }
      const zb = field({ label: 'TeslaFi\'s times were read in', name: 'zone_before', path: 'zone_before', value: d.zone_before || '',
        hint: `The zone TeslaFi's times were read in before${d.zone_before ? '' : ' (the audit log does not say it)'}; if the account had no zone, the server's (UTC in the image).`,
        attrs: { list: 'tf-zones', autocomplete: 'off', spellcheck: 'false' } });
      const until = field({ label: 'For the data stored before', name: 'before', path: 'before_ms', type: 'datetime-local', required: true, value: localInput(d.before_ms),
        hint: `When the zone was set right (your time, ${tzName(LOCAL)}). ${d.before_ms ? 'From the audit log.' : 'The audit log has no change of the zone: give the moment.'} When TeslaFi's zone is east of the zone its times were read in (Europe/Berlin read as UTC), the last hours before the moment were stored after it: give a moment that much later.` });
      const r = await askPassword(ctx, {
        title: `Date ${a.label}'s stored data again`,
        intro: h('div', { class: 'tf-intro' }, h('p', { class: 'note' }, `TeslaFi writes its times without a zone. The data stored before the moment below is dated again as read in ${d.zone || 'the zone set now'}, each time by its own offset; the drives and charges made from it move with it (their purposes, notes and costs stay on them), and TeslaFi's charge costs are read again. Asking the same again moves nothing.`), zoneList()),
        submit: 'Date it again', extra: [zb, until],
        run: guarded((cred, v) => {
          const z = v.zone_before.trim();
          if (!z) { const e = new Error(); e.fields = { zone_before: 'Give the zone TeslaFi\'s times were read in before.' }; throw e; }
          if (!zoneOk(z)) { const e = new Error(); e.fields = { zone_before: `${z} is not a time zone. Pick one from the list, such as UTC.` }; throw e; }
          // the moment as the server gave it, to the millisecond, unless it was changed here
          const t = d.before_ms != null && v.before === localInput(d.before_ms) ? d.before_ms : new Date(v.before).getTime();
          if (!Number.isFinite(t)) { const e = new Error(); e.fields = { before_ms: 'Give the moment the zone was set right.' }; throw e; }
          return ctx.api.post(`${BASE}/accounts/${enc(a.id)}/redate`, withPw(cred, { zone_before: z, before_ms: t }));
        }),
      });
      await after(r, (x) => {
        const sum = (k) => (x.cars || []).reduce((n, c) => n + (c[k] || 0), 0);
        const failed = (x.errors || []).map((e) => ` Car ${e.car}: ${e.message}`).join('');
        if (!(x.cars || []).length && !(x.errors || []).length) return 'Nothing moved: no car of this account is matched.';
        if ((x.cars || []).every((c) => c.already)) return `That was done before: nothing moved.${failed}`;
        if (['samples', 'spans', 'drives', 'charges'].every((k) => sum(k) === 0)) return `Nothing moved: no data stored before the moment was read in that zone.${failed}`;
        return `Dated again: ${num(sum('samples'))} samples, ${num(sum('drives'))} drives and ${num(sum('charges'))} charges. What was built from them is being built again.${failed}`;
      });
    } }, icon('clock', 'ico--sm'), 'Re-date stored data');
    const removeBtn = h('button', { class: 'btn btn--sm btn--ghost ta-remove', type: 'button', onclick: async () => {
      const r = await askPassword(ctx, {
        title: `Remove ${a.label}?`, danger: true, submit: 'Remove',
        intro: h('p', { class: 'note' }, 'Reading stops at once and the sealed file with the token is deleted. The history already stored from it stays, marked as data from TeslaFi.'),
        run: guarded((cred) => ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred))),
      });
      await after(r, `${a.label} removed. Its history stays.`);
    } }, icon('x', 'ico--sm'), 'Remove');
    return h('div', { class: 'panel panel-pad ta-account', id: `teslafi-${a.id}` },
      h('div', { class: 'ta-acc-head' },
        h('div', { class: 'ta-acc-title' }, h('h3', null, a.label), pill(st[0], { tone: st[1] }), a.tls ? h('span', { class: 'faint' }, `TLS ${a.tls}`) : null),
        h('div', { class: 'row ta-acc-acts' }, pauseBtn, tokenBtn, editBtn, redateBtn, removeBtn)),
      st[2] ? h('p', { class: ['setting-note', (a.status === 'token_refused' || a.status === 'error') && 'warn'] }, st[2]) : null,
      a.rate_limited_until_ms ? limitedNote(a.rate_limited_until_ms, LOCAL) : null,
      kv([
        ['Added', at(a.added_ms), { icon: 'calendar' }],
        ['What the server does', h('span', { title: mode[1] }, mode[0]), { icon: 'pulse' }],
        ['Last good answer', a.last_ok_ms ? h('span', { title: at(a.last_ok_ms) }, ago(a.last_ok_ms)) : h('span', { class: 'faint' }, 'none yet'), { icon: 'check' }],
        ['Last error', err ? h('span', { class: 'warn' }, lastErrorText(err), err.at_ms ? h('span', { title: at(err.at_ms) }, `, ${ago(err.at_ms)}`) : '') : h('span', { class: 'faint' }, 'none'), { icon: 'warn' }],
        ['Requests today', num(a.polls_today ?? 0), { icon: 'pulse', title: 'Since midnight UTC, for information: there is no cap on this side.' }],
        ['TeslaFi\'s time zone', a.time_zone ? a.time_zone.replace(/_/g, ' ') : h('span', { class: 'faint' }, 'the server\'s own'), { icon: 'clock' }],
        ['Token', h('span', { class: 'faint' }, 'sealed on the server, never shown'), { icon: 'lock' }],
      ]),
      carsTable(a));
  }

  function carsTable(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    if (!items.length) return emptyState({ icon: 'car', title: 'No car yet', text: 'TeslaFi has not named a car for this token yet. It shows here once TeslaFi\'s polls name one.' });
    const takenBy = new Map();
    for (const o of accounts) {
      if (o.id === a.id) continue;
      for (const c of carRows[o.id]?.items || []) if (c.match && c.match !== 'new' && c.match !== 'leave_out') takenBy.set(c.match, o.label);
    }
    const soldIds = new Set(cars.filter((x) => x.sold).map((x) => x.id));
    const opts = [['', 'Not decided (not read)'], ...cars.map((c) => [c.id, takenBy.has(c.id) ? `${c.name} (matched in ${takenBy.get(c.id)})` : c.name, takenBy.has(c.id)]), ['new', 'A new car'], ['leave_out', 'Leave out (not read)']];
    const sels = new Map();
    const saveBtn = h('button', { class: 'btn btn--primary btn--sm', type: 'button', hidden: true }, icon('check', 'ico--sm'), 'Save matches');
    const changed = () => items.filter((c) => (sels.get(c.vehicle).value || null) !== (c.match ?? null));
    saveBtn.addEventListener('click', async () => {
      const ch = changed();
      if (!ch.length) return;
      // the admin's session alone: no password asked again (a match stores and shows no token)
      saveBtn.disabled = true;
      let r = null;
      try { r = await guarded(() => ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, { version: data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: sels.get(c.vehicle).value || null })) }))(); } catch (e) { if (e?.name !== 'AbortError') flash(errorWords(e).text, { kind: 'error' }); } finally { saveBtn.disabled = false; }
      await after(r, 'Match saved.');
    });
    const sv = status.find((s) => s.account === a.id) || {};
    const rows = items.map((c) => {
      const pre = c.match ?? c.suggested ?? '';
      const known = opts.some(([v]) => v === pre) ? opts : [...opts, [pre, carName(pre)]];
      const start = c.match == null && takenBy.has(pre) ? '' : pre;
      const sel = h('select', { class: 'select', 'aria-label': `Sentry Mate car for ${c.display_name || c.vehicle}` },
        (() => { // sold cars last, in a "Sold cars" group, unless matched or suggested
          const mk = ([v, l, taken]) => h('option', { value: v, selected: v === start, disabled: taken && v !== c.match ? true : null }, l);
          const { main, sold } = soldParts(known, [start, c.match, pre], { id: ([v]) => v, isSold: ([v]) => soldIds.has(v) });
          return [main.map(mk), soldOptions(sold, mk)];
        })());
      sel.addEventListener('change', () => { saveBtn.hidden = !changed().length; });
      sels.set(c.vehicle, sel);
      const tz = cars.find((x) => x.id === c.match)?.time_zone || ctx.tz;
      const next = c.current ? sv.next_request_ms : null;
      return h('tr', null,
        h('td', null, h('span', { class: 'cell-text' }, h('b', null, c.display_name || 'No name at TeslaFi'), h('span', { class: 'sub' }, [c.year, c.model].filter(Boolean).join(' '), ' · ', h('span', { class: 'mono' }, c.vin_prefix || '—')))),
        h('td', null, h('div', { class: 'ta-match' }, sel, c.match == null && c.suggested && !takenBy.has(c.suggested) ? tag('Suggested: same VIN prefix', 'live') : null)),
        h('td', null, h('span', { class: 'ta-cell-label' }, 'TeslaFi says: '), c.current ? c.car_state || '—' : h('span', { class: 'faint' }, 'not polled now')),
        h('td', null, h('span', { class: 'cell-text' }, h('span', null, c.current ? (MODE[sv.mode] || MODE[a.mode] || ['—'])[0] : 'Not read'),
          next ? h('span', { class: ['sub', sv.mode === 'backoff' && 'warn'] }, `${sv.mode === 'backoff' ? 'Next try' : 'Next read'} ${time(next, tz)} (in ${dur(Math.max(0, next - ctx.api.now()))})`) : h('span', { class: 'sub' }, c.current ? 'none planned' : 'another car on the account'))),
        h('td', { class: 'num' }, c.current && sv.last_data_ms ? h('span', { title: dateTime(sv.last_data_ms, tz) }, h('span', { class: 'ta-cell-label' }, 'Last data '), ago(sv.last_data_ms)) : h('span', null, h('span', { class: 'ta-cell-label' }, 'Last data '), h('span', { class: 'faint' }, 'none yet'))));
    });
    // a waiting suggestion is already picked: it can be saved as it is
    saveBtn.hidden = !changed().length;
    return h('div', { class: 'ta-cars' },
      h('div', { class: 'ta-cars-head' }, h('span', { class: 'metric-label' }, items.length === 1 ? 'Its car' : `Its cars (${items.length})`), saveBtn),
      h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl ta-tbl' },
        h('thead', null, h('tr', null, ['Car at TeslaFi', 'Sentry Mate car', 'TeslaFi says', 'What the server does', 'Last data'].map((t, i) => h('th', { class: i === 4 ? 'num' : null, scope: 'col' }, t)))),
        h('tbody', null, rows))),
      h('p', { class: 'setting-note' }, 'TeslaFi\'s data goes to a car only once it is matched to a car here or to a new car. A suggestion has the same VIN prefix as a car here; it is not saved until you press Save matches. A new car on the TeslaFi account is added here unmatched, and its polls never go to the car matched before.'));
  }

  return { el, load };
}
