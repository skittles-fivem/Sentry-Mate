// Settings: Integrations, the Tesla account section (#tesla): the Tesla API input. This server can sign in to
// a Tesla account and read its cars from Tesla's Owner API, as a second live input beside SMO. It is off until
// an admin adds an account; before that the server makes no connection to Tesla at all. It only reads: it never
// wakes a car or sends it a command, and no answer ever holds a Tesla token.
//
// Admins only, reading included. Every change except starting a sign-in asks for the admin's own password again
// (and the two-step code when the admin has two-step sign-in on): askPassword() from settings-server-move.js,
// sent as confirm_password and code.
//
// API: GET/PATCH integrations/tesla, GET/POST integrations/tesla/accounts, POST integrations/tesla/sign-in,
// POST integrations/tesla/sign-in/{sign_in}, PATCH/DELETE integrations/tesla/accounts/{id},
// GET/PUT integrations/tesla/accounts/{id}/cars, GET integrations/tesla/status.

import { h, replace } from '../shared/dom.js';
import { buildUrl } from '../shared/state.js';
import { icon } from '../core/icons.js';
import { soldParts, soldOptions } from '../core/carswitch.js';
import { kv, tag, pill, note, metric, figures } from '../core/page.js';
import { field } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { dateTime, ago, num, dur, time, tzName } from '../core/fmt.js';
import { askPassword } from './settings-server-move.js';

const enc = encodeURIComponent;
const BASE = 'integrations/tesla';
// Account times (added, last error, last good answer) are shown on this computer's clock, with the zone
// named: an account belongs to no car, so the last car's zone would make the same time read differently.
const LOCAL = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
const at = (ms) => `${dateTime(ms, LOCAL)} ${tzName(LOCAL)}`;
// Sign-ins started here and not finished: the server has no call that ends one, and at most five are open,
// so a closed dialog's sign-in is used again by the next press (kept while it has two minutes left).
const openSignIns = new Map();

/* ---------------------------------------------------------------------- words */

export const ACCOUNT_STATUS = {
  checking: ['Checking', 'plain', 'Just added: the server is asking Tesla for its cars.'],
  ok: ['Reading', 'live', 'Tesla takes its sign-in; its matched cars are read.'],
  paused: ['Paused', 'plain', 'Its cars are not read until you resume it. Nothing is asked of Tesla meanwhile.'],
  sign_in_needed: ['Sign in again', 'warm', 'Tesla stopped taking its sign-in, so its cars are not read. Give it a new refresh token (Replace token), or remove it and sign in at Tesla again. Nothing is asked of Tesla meanwhile.'],
  error: ['Not read', 'warm', 'Tesla refuses it, or answers with errors. See the last error.'],
};

export const CAR_MODE = {
  off: ['Not read', 'Not matched, left out, or its account is paused.'],
  watching_list: ['Watching the car list', 'Asleep or offline: only the list of cars is read, so the car is never woken.'],
  online_idle: ['Awake, idle', 'Awake with nothing going on: read at the parked pace.'],
  letting_sleep: ['Letting it sleep', 'Awake and quiet for 10 minutes: left alone for 21 minutes so it can fall asleep.'],
  charging: ['Charging', 'Read at the charging pace.'],
  driving: ['Driving', 'Read at the driving pace.'],
  backoff: ['Waiting after errors', 'Tesla answered with errors, or the day\'s 3,000 requests are used up: nothing is asked until the next try. After a server error it waits longer each time, up to 15 minutes; after a refusal (403) an hour.'],
};

const CAR_STATE = { online: 'Online', asleep: 'Asleep', offline: 'Offline', unknown: 'Not known' };
const LOCATION = [
  ['driving_or_charging', 'While driving or charging', 'The usual: the position is asked for only while the car drives or charges.'],
  ['always', 'Always', 'Also while parked and awake. The car shows its location-shared icon whenever the position is asked for.'],
  ['never', 'Never', 'No position from the Tesla API at all: drives from it have no route and charges no place.'],
];
const PACE = [['gentle', 'Gentle'], ['normal', 'Normal'], ['often', 'Often']];
const REGION = [['global', 'Global (tesla.com)'], ['china', 'China (tesla.cn)']];

// error codes, in words (the API's message is kept when there is no better one)
const ERROR_WORDS = {
  tesla_refused: 'Tesla said no to this sign-in or token: it may have run out, been used already, or the Tesla password changed since. Sign in at Tesla again, or paste a new refresh token.',
  tesla_unreachable: 'Tesla could not be reached, or answered with an error. Nothing was kept. Try again in a few minutes.',
  account_exists: 'The cars of this Tesla account are already read through another Tesla account here. Replace that account\'s token instead, or remove it first.',
  car_taken: 'One of these cars is already matched to a vehicle of another Tesla account here. Leave it out there first, or pick another car.',
  too_many_sign_ins: 'Five sign-ins are open already. Finish one, or wait up to ten minutes for them to run out.',
  no_such_sign_in: 'This sign-in ran out (it lasts ten minutes) or was used already. Close this and start a new one.',
  no_such_tesla_account: 'This Tesla account is not here any more: another admin may have removed it. Nothing was changed; the page now shows the accounts there are.',
  version_conflict: 'Someone else changed this a moment ago, so nothing was saved. The page now shows their change: make yours again if it is still needed.',
};
// last_error codes on an account or the whole input
const LAST_ERROR_WORDS = {
  sign_in_needed: 'Tesla stopped taking the sign-in',
  tesla_refused: 'Tesla refused the token',
  tesla_unreachable: 'Tesla could not be reached',
  endpoint_gone: 'Tesla closed the address the server reads; the server stops asking it',
  rate_limited: 'Tesla asked the server to slow down',
  daily_cap: 'The day\'s 3,000 requests are used up',
  forbidden: 'Tesla refused the request (403)',
  server_error: 'Tesla answered with a server error',
  timeout: 'Tesla took too long to answer',
};
export const lastErrorText = (e) => (e ? (LAST_ERROR_WORDS[e.code] || e.message || e.code) : null);

/** The password dialog's own codes, and our words for Tesla's. Mutates and returns the error. */
function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (e.status === 409 && !ERROR_WORDS[e.code]) e.message = ERROR_WORDS.version_conflict;
  else if (ERROR_WORDS[e.code]) e.message = ERROR_WORDS[e.code];
  else if (e.code === 'invalid' && e.fields?.pasted_url) { /* the field shows the server's words */ }
  // the password goes in the dialog's own field
  if (e.fields) { delete e.fields.confirm_password; delete e.fields.code; if (!Object.keys(e.fields).length) e.fields = null; }
  return e;
}
// the account changed or went away since the page read it: the dialog closes and the page reads it again
const isStale = (e) => e && (e.code === 'no_such_tesla_account' || (e.status === 409 && !ERROR_WORDS[e.code]) || e.code === 'version_conflict');
/** run() for askPassword: our words for the error; a stale page closes the dialog with { stale: words }. */
const guarded = (fn) => async (...a) => {
  try { return await fn(...a); } catch (e) { words(e); if (isStale(e)) return { stale: e.message }; throw e; }
};
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const code = (t) => h('code', { class: 'si-code' }, t);
const secs = (s) => (s == null ? '—' : s >= 120 && s % 60 === 0 ? `${num(s / 60)} min` : `${num(s)} s`);

/* ---------------------------------------------------------------------- the section */

/**
 * The Tesla account section. ctx: the page's; cars: Sentry Mate's cars ({ id, name, ... }) for the match picker.
 * Returns { el, load() } (load reads everything and paints; the page awaits it before ready()).
 */
export function teslaSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const body = h('div', { class: 'ta-body' });
  const headPill = h('span', { class: 'ta-head-pill' });
  const el = h('section', { class: 'si-section ta', id: 'tesla', 'aria-label': 'Tesla account' },
    h('div', { class: 'si-section-head' }, h('h2', { class: 'si-h2' }, icon('car'), 'Tesla account'), headPill),
    h('p', { class: 'note' }, 'A second live input beside SMO: this server can sign in to a Tesla account and read its cars from Tesla\'s Owner API (the one Tesla\'s own app uses). It only reads: it never wakes a car or sends it a command. Off until an admin adds an account; before that the server makes no connection to Tesla at all. Pages then say for every row whether its data came from SMO, the Tesla API, or both.'),
    body);

  let ov = null, accounts = [], status = [], carRows = {};
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  async function load() {
    if (!admin) {
      replace(headPill, pill('Admins only'));
      replace(body, h('div', { class: 'panel panel-pad' }, note(icon('lock', 'ico--sm'), ' Only an admin sees and changes the Tesla account: its status, its cars and how often Tesla is asked. Every page still says where its data came from (SMO, the Tesla API or an import).')));
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
      replace(body, h('div', { class: 'panel panel-pad' }, h('p', { class: 'warn' }, icon('warn', 'ico--sm'), ' The Tesla account could not be read: ', errorWords(e).text)));
      return;
    }
    if (!ctx.alive()) return;
    paint();
  }
  const reload = async (msg) => { if (msg) flash(msg); await load(); };
  /** After a dialog: nothing on cancel; the page read again with the right words when it was stale. */
  const after = async (r, msg) => {
    if (!r) return;
    if (r.stale) { flash(r.stale, { kind: 'info' }); await load(); return; }
    await reload(typeof msg === 'function' ? msg(r) : msg);
  };

  // matched: cars matched to a car here or a new one; read: those the server reads now (not paused, mode not off)
  function carCounts() {
    let matched = 0, read = 0;
    for (const a of accounts) {
      for (const c of carRows[a.id]?.items || []) {
        if (c.match == null || c.match === 'leave_out') continue;
        matched++;
        if (!a.paused && c.mode && c.mode !== 'off') read++;
      }
    }
    return { matched, read };
  }
  const cars_ = (n) => `${num(n)} ${n === 1 ? 'car' : 'cars'}`;

  function paint() {
    const { matched, read } = carCounts();
    replace(headPill, !ov.enabled ? pill('Off until an account is added', { lg: true })
      : read ? pill(`Reading ${cars_(read)}`, { tone: 'live', lg: true })
        : matched && accounts.every((a) => a.paused) ? pill('Paused: no car is read', { lg: true })
          : matched ? pill('No car is read now', { tone: 'warm', lg: true }) : pill('No car matched yet', { tone: 'warm', lg: true }));
    replace(body,
      h('div', { class: 'ta-grid' }, statusPanel(), addPanel()),
      accounts.length ? accounts.map(accountPanel) : null);
  }

  /* ---------- the input as a whole: state, hosts, pace */
  function statusPanel() {
    const p = ov.polls || {};
    const { matched, read } = carCounts();
    const paused = accounts.filter((a) => a.paused).length;
    const cap = (p.daily_cap || 3000) * Math.max(1, ov.accounts || 0);
    const paceSeg = h('div', { class: 'seg', role: 'group', 'aria-label': 'How often Tesla is asked' },
      PACE.map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(ov.poll === v), onclick: () => setPace(v) }, l)));
    return h('div', { class: 'panel panel-pad ta-status' },
      h('div', { class: 'panel-head' }, h('h3', { class: 'panel-title' }, icon('pulse'), 'Status')),
      figures([
        metric('State', ov.enabled ? 'On' : 'Off', null, { size: 'sm', explain: ov.enabled ? `${num(ov.accounts)} ${ov.accounts === 1 ? 'account' : 'accounts'}${paused ? ` (${num(paused)} paused)` : ''}, ${read ? `${cars_(read)} read` : 'no car read now'}${matched > read ? `, ${num(matched)} matched` : ''}.` : 'Off until an account is added: no connection to Tesla is made.' }),
        metric('Requests today', ov.polls_today, 'number', { size: 'sm', digits: 0, explain: ov.enabled ? `Of at most ${num(cap)} (3,000 a day per account).` : 'None: there is no account.' }),
        metric('Connections set up', ov.clients_built, 'number', { size: 'sm', digits: 0, explain: 'To Tesla, since the server started.' }),
      ], 'si-figs'),
      ov.last_error ? h('p', { class: 'si-last-error warn' }, icon('warn', 'ico--sm'), ' Last error: ', h('b', null, lastErrorText(ov.last_error)),
        ov.last_error.at_ms ? `, ${at(ov.last_error.at_ms)} (${ago(ov.last_error.at_ms)})` : '', '.') : h('p', { class: 'si-last-error faint' }, icon('check', 'ico--sm'), ' No error.'),
      h('div', { class: 'ta-hosts' }, h('span', { class: 'metric-label' }, 'Tesla hosts it talks to'),
        ov.hosts?.length ? h('ul', { class: 'ta-host-list' }, ov.hosts.map((x) => h('li', null, code(x))))
          : h('p', { class: 'faint' }, 'None while there is no account. Once one is added: Tesla\'s sign-in host, its Owner API and (outside China) its live stream, all over HTTPS.')),
      h('div', { class: 'ta-pace' },
        h('div', { class: 'ta-pace-head' }, h('span', { class: 'metric-label' }, 'How often Tesla is asked'), paceSeg),
        h('dl', { class: 'ta-pace-list' },
          [['A parked, awake car', p.parked_s], ['A charging car', p.charging_s], ['A driving car', p.driving_s], ['A driving car with Tesla\'s live stream', p.driving_streaming_s],
            ['The list of cars (asleep cars)', p.list_s], ['Any car while its SMO is live', p.smo_live_s]].map(([l, s]) => h('div', null, h('dt', null, l), h('dd', null, `every ${secs(s)}`)))),
        h('p', { class: 'setting-note' }, 'In every pace an asleep car is never woken (only the list of cars is read), and an awake car with nothing going on is left alone so it can fall asleep. At most one request a second goes to Tesla per account.')));
  }
  async function setPace(v) {
    if (v === ov.poll) return;
    const r = await askPassword(ctx, {
      title: `Pace: ${PACE.find((x) => x[0] === v)[1]}`,
      intro: h('p', { class: 'note' }, 'How often the server reads each car from the Tesla API. A faster pace gives finer drives and charges from the API, and uses more of the 3,000 requests a day.'),
      submit: 'Change the pace',
      run: guarded((cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v }))),
    });
    await after(r, 'How often Tesla is asked: saved.');
  }

  /* ---------- adding an account */
  function addPanel() {
    const regionSel = h('select', { class: 'select', 'aria-label': 'Tesla region' }, REGION.map(([v, l]) => h('option', { value: v }, l)));
    const voidBox = h('input', { type: 'checkbox', id: 'ta-void' });
    return h('div', { class: 'panel panel-pad ta-add' },
      h('div', { class: 'panel-head' }, h('h3', { class: 'panel-title' }, icon('plus'), accounts.length ? 'Add another account' : 'Add a Tesla account')),
      h('label', { class: 'ta-field' }, h('span', { class: 'metric-label' }, 'Region'), regionSel),
      h('div', { class: 'ta-ways' },
        h('div', { class: 'si-rule' },
          h('h3', null, 'Sign in at Tesla'),
          h('p', null, 'Opens Tesla\'s own sign-in page in a new tab. Tesla\'s page asks for the Tesla password, the two-step code and any captcha: this server never sees them. Then you paste the address Tesla ends on.'),
          h('label', { class: 'check ta-void', for: 'ta-void' }, voidBox, h('span', null, 'End on Tesla\'s ', code('/void/callback'), ' page instead (for a browser that refuses ', code('tesla://'), ' addresses)')),
          h('button', { class: 'btn btn--primary', type: 'button', onclick: (e) => signIn(e.currentTarget, regionSel.value, voidBox.checked ? 'void' : 'app') }, icon('ext', 'ico--sm'), 'Sign in at Tesla')),
        h('div', { class: 'si-rule' },
          h('h3', null, 'Paste a refresh token'),
          h('p', null, 'A Tesla refresh token from another tool. The server checks it with Tesla (one token refresh, one car list) before anything is kept, and never shows it again.'),
          h('button', { class: 'btn', type: 'button', onclick: () => addByToken(regionSel.value) }, icon('lock', 'ico--sm'), 'Paste a refresh token'))),
      h('p', { class: 'setting-note' }, 'A new account\'s cars are not read until you match each one to a car here, to a new car, or leave it out.'));
  }

  async function signIn(btn, region, redirect) {
    const key = `${region}|${redirect}`;
    let s = openSignIns.get(key);
    if (s && s.expires_ms - ctx.api.now() < 120_000) { openSignIns.delete(key); s = null; }
    if (!s) {
      btn.disabled = true;
      try { s = await ctx.api.post(`${BASE}/sign-in`, { region, redirect }); } catch (e) {
        if (e?.name !== 'AbortError') flash(`Could not start the sign-in: ${words(e).message}`, { kind: 'error' });
        return;
      } finally { btn.disabled = false; }
      openSignIns.set(key, s);
    }
    if (!ctx.alive()) return;
    const endsOn = redirect === 'void' ? `https://auth.tesla.${region === 'china' ? 'cn' : 'com'}/void/callback?code=…` : 'tesla://auth/callback?code=…';
    const label = field({ label: 'Name for this account', name: 'label', required: true, value: 'Tesla account', hint: 'Your own name for it, shown on this page. Not the account\'s e-mail.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const pasted = field({ label: 'The address Tesla ended on', name: 'pasted_url', path: 'pasted_url', type: 'textarea', required: true,
      hint: `The whole address from the address bar, starting ${endsOn.split('?')[0]}`, attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3', placeholder: endsOn } });
    const openBtn = h('a', { class: 'btn btn--primary', href: s.url, target: '_blank', rel: 'noopener noreferrer external' }, icon('ext', 'ico--sm'), 'Open Tesla\'s sign-in page');
    const intro = h('div', { class: 'ta-steps' },
      h('ol', { class: 'ta-step-list' },
        h('li', null, h('b', null, 'Open Tesla\'s page'), ' and sign in there. Tesla\'s own page asks for your Tesla password and code; this server never sees them. ', openBtn),
        h('li', null, h('b', null, 'The last page will not load.'), ' That is expected: Tesla ends on an address that starts ', code(endsOn.split('?')[0]), ', which a browser cannot open. The browser may show an error, or seem to do nothing; the address is still in its address bar.'),
        h('li', null, h('b', null, 'Copy that whole address'), ' from the address bar and paste it below, with your own Sentry Mate password.')),
      h('p', { class: 'note ta-expiry' }, icon('clock', 'ico--sm'), ` This sign-in lasts ten minutes, until ${time(s.expires_ms, LOCAL)} on this computer's clock, and works once.`));
    const r = await askPassword(ctx, {
      title: 'Sign in at Tesla', intro, submit: 'Add the account', extra: [label, pasted],
      run: async (cred, v) => {
        try {
          const a = await ctx.api.post(`${BASE}/sign-in/${enc(s.sign_in)}`, withPw(cred, { pasted_url: v.pasted_url.trim(), label: v.label.trim() }));
          openSignIns.delete(key);
          return a;
        } catch (e) { if (e?.code === 'no_such_sign_in') openSignIns.delete(key); throw words(e); }
      },
    });
    await after(r, `${r?.label} added. Match its cars below to start reading them.`);
  }

  async function addByToken(region) {
    const label = field({ label: 'Name for this account', name: 'label', required: true, value: 'Tesla account', hint: 'Your own name for it. Not the account\'s e-mail.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const reg = field({ label: 'Region', name: 'region', type: 'select', value: region, options: REGION });
    const tok = field({ label: 'Refresh token', name: 'refresh_token', path: 'refresh_token', type: 'textarea', required: true, hint: 'Checked with Tesla before it is kept. It is sealed on the server and never shown again.', attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3' } });
    const r = await askPassword(ctx, {
      title: 'Add a Tesla account by refresh token', submit: 'Check with Tesla and add', extra: [label, reg, tok],
      intro: h('p', { class: 'note' }, 'Only paste a token of your own Tesla account. Anyone with it can read the account\'s cars, so it never leaves this server and no page shows it.'),
      run: guarded((cred, v) => ctx.api.post(`${BASE}/accounts`, withPw(cred, { label: v.label.trim(), region: v.region, refresh_token: v.refresh_token.trim() }))),
    });
    await after(r, `${r?.label} added. Match its cars below to start reading them.`);
  }

  /* ---------- one account */
  function accountPanel(a) {
    const st = ACCOUNT_STATUS[a.status] || [a.status, 'plain', ''];
    const err = a.last_error;
    const patch = (title, intro, submit, bodyOf, extra = [], msg = 'Saved.', danger = false) => async () => {
      const r = await askPassword(ctx, {
        title, intro: intro ? h('p', { class: 'note' }, intro) : null, submit, extra, danger,
        run: guarded((cred, v) => ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: a.version, ...bodyOf(v) }))),
      });
      await after(r, msg);
    };
    const pauseBtn = h('button', { class: 'btn btn--sm', type: 'button', onclick: patch(a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`,
      a.paused ? 'Its matched cars are read again from now on.' : 'Its cars are not read until you resume it. Nothing is asked of Tesla meanwhile; the history already stored stays.',
      a.paused ? 'Resume' : 'Pause', () => ({ paused: !a.paused }), [], a.paused ? `${a.label} resumed.` : `${a.label} paused.`) }, icon(a.paused ? 'live' : 'moon', 'ico--sm'), a.paused ? 'Resume' : 'Pause');
    const renameBtn = h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => {
      const f = field({ label: 'Name', name: 'label', required: true, value: a.label, attrs: { maxlength: '64', autocomplete: 'off' } });
      return patch(`Rename ${a.label}`, null, 'Rename', (v) => ({ label: v.label.trim() }), [f], 'Renamed.')();
    } }, icon('settings', 'ico--sm'), 'Rename');
    const tokenBtn = h('button', { class: ['btn btn--sm', a.status === 'sign_in_needed' ? 'btn--primary' : 'btn--ghost'], type: 'button', onclick: () => {
      const f = field({ label: 'New refresh token', name: 'refresh_token', path: 'refresh_token', type: 'textarea', required: true, hint: 'Checked with Tesla before it replaces the old one.', attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3' } });
      return patch(`New token for ${a.label}`, 'For an account Tesla stopped taking (sign in again), or a token you renewed elsewhere. The old token is deleted.', 'Check with Tesla and replace', (v) => ({ refresh_token: v.refresh_token.trim() }), [f], 'Token replaced and checked with Tesla.')();
    } }, icon('lock', 'ico--sm'), 'Replace token');
    const removeBtn = h('button', { class: 'btn btn--sm btn--ghost ta-remove', type: 'button', onclick: async () => {
      const r = await askPassword(ctx, {
        title: `Remove ${a.label}?`, danger: true, submit: 'Remove the account',
        intro: h('p', { class: 'note' }, 'Reading stops at once and the sealed file with its tokens is deleted. The history already stored from it stays, marked as data from the Tesla API.'),
        run: guarded((cred) => ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred))),
      });
      await after(r, `${a.label} removed. Its history stays.`);
    } }, icon('x', 'ico--sm'), 'Remove');

    const locSeg = h('div', { class: 'seg', role: 'group', 'aria-label': `When ${a.label}'s car positions are asked for` },
      LOCATION.map(([v, l, why]) => h('button', { class: 'seg-btn', type: 'button', title: why, 'aria-pressed': String(a.location === v),
        onclick: () => (a.location === v ? null : patch(`Position: ${l.toLowerCase()}?`, why, 'Save', () => ({ location: v }), [], 'When the position is asked for: saved.')()) }, l)));

    return h('div', { class: 'panel panel-pad ta-account', id: `tesla-${a.id}` },
      h('div', { class: 'ta-acc-head' },
        h('div', { class: 'ta-acc-title' }, h('h3', null, a.label), pill(st[0], { tone: st[1] }), h('span', { class: 'faint' }, a.region === 'china' ? 'China' : 'Global')),
        h('div', { class: 'row ta-acc-acts' }, pauseBtn, tokenBtn, renameBtn, removeBtn)),
      st[2] ? h('p', { class: ['setting-note', a.status === 'sign_in_needed' || a.status === 'error' ? 'warn' : null] }, st[2]) : null,
      kv([
        ['Added', at(a.added_ms), { icon: 'calendar' }],
        ['Last good answer', a.last_ok_ms ? h('span', { title: at(a.last_ok_ms) }, ago(a.last_ok_ms)) : h('span', { class: 'faint' }, 'none yet'), { icon: 'check' }],
        ['Last error', err ? h('span', { class: 'warn' }, lastErrorText(err), err.message && LAST_ERROR_WORDS[err.code] ? h('span', { class: 'faint' }, ` (${err.message})`) : null, err.at_ms ? h('span', { title: at(err.at_ms) }, `, ${ago(err.at_ms)}`) : '') : h('span', { class: 'faint' }, 'none'), { icon: 'warn' }],
        ['Requests today', `${num(a.polls_today ?? 0)} of 3,000`, { icon: 'pulse' }],
        ['Hosts', h('span', { class: 'ta-inline-hosts' }, (a.hosts || []).map((x) => code(x))), { icon: 'globe' }],
      ]),
      h('div', { class: 'ta-loc' },
        h('div', { class: 'ta-pace-head' }, h('span', { class: 'metric-label' }, 'Ask for the cars\' position'), locSeg),
        h('p', { class: 'setting-note' }, icon('info', 'ico--sm'), ' While the position is asked for, the car shows an icon on its screen that its location is shared. ', LOCATION.find((x) => x[0] === a.location)?.[2] || '')),
      carsTable(a));
  }

  /* ---------- an account's cars: match each one, and what the server does for it now */
  function carsTable(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    if (!items.length) return emptyState({ icon: 'car', title: 'No cars on this account', text: 'Tesla lists no car for this account. A car added to it at Tesla shows here within a few minutes.' });
    // cars here another Tesla account already reads: the server refuses them (car_taken), so they are not offered
    const takenBy = new Map();
    for (const o of accounts) {
      if (o.id === a.id) continue;
      for (const c of carRows[o.id]?.items || []) if (c.match && c.match !== 'new' && c.match !== 'leave_out') takenBy.set(c.match, o.label);
    }
    const soldIds = new Set(cars.filter((x) => x.sold).map((x) => x.id));
    const opts = [['', 'Not decided (not read)'], ...cars.map((c) => [c.id, takenBy.has(c.id) ? `${c.name} (read through ${takenBy.get(c.id)})` : c.name, takenBy.has(c.id)]), ['new', 'A new car'], ['leave_out', 'Leave out (not read)']];
    const sels = new Map();
    const saveBtn = h('button', { class: 'btn btn--primary btn--sm', type: 'button', hidden: true }, icon('check', 'ico--sm'), 'Save matches');
    const problem = h('p', { class: 'warn ta-match-warn', role: 'alert', hidden: true });
    const changed = () => items.filter((c) => (sels.get(c.vehicle).value || null) !== (c.match ?? null));
    // one car here takes one Tesla vehicle: two vehicles of this account on one car would both feed it
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
      replace(problem, dup.length ? [icon('warn', 'ico--sm'), ' ', dup.map(([car, list]) => `${list.map((c) => c.display_name || c.vehicle).join(' and ')} are both matched to ${carName(car)}.`).join(' '), ' One car here takes one Tesla car: pick another car, A new car, or Leave out.'] : null);
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
      // a suggestion another account already reads is not taken over: the picker starts at Not decided
      const start = c.match == null && takenBy.has(pre) ? '' : pre;
      const sel = h('select', { class: 'select', 'aria-label': `Sentry Mate car for ${c.display_name || c.vehicle}` },
        (() => { // sold cars last, in a "Sold cars" group, unless matched or suggested
          const mk = ([v, l, taken]) => h('option', { value: v, selected: v === start, disabled: taken && v !== c.match ? true : null }, l);
          const { main, sold } = soldParts(known, [start, c.match, pre], { id: ([v]) => v, isSold: ([v]) => soldIds.has(v) });
          return [main.map(mk), soldOptions(sold, mk)];
        })());
      sel.addEventListener('change', sync);
      sels.set(c.vehicle, sel);
      const mode = CAR_MODE[c.mode] || [c.mode, ''];
      const next = sv.next_poll_ms ?? null;
      const tz = cars.find((x) => x.id === c.match)?.time_zone || ctx.tz;
      return h('tr', null,
        h('td', null, h('span', { class: 'cell-text' }, h('b', null, c.display_name || 'No name at Tesla'), h('span', { class: 'sub' }, [c.year, c.model].filter(Boolean).join(' '), ' · ', h('span', { class: 'mono' }, c.vin_prefix || '—')))),
        h('td', null, h('div', { class: 'ta-match' }, sel, c.match == null && c.suggested && !takenBy.has(c.suggested) ? tag('Suggested: same VIN prefix', 'live') : null)),
        h('td', null, h('span', { class: 'ta-cell-label' }, 'Tesla says: '), CAR_STATE[c.state] || c.state || '—'),
        h('td', null, h('span', { class: 'cell-text', title: mode[1] }, h('span', null, mode[0], sv.streaming ? ' · live stream' : ''),
          c.mode === 'backoff' && next ? h('span', { class: 'sub warn' }, `Next try ${time(next, tz)} (in ${dur(Math.max(0, next - ctx.api.now()))})`)
            : next && c.mode !== 'off' ? h('span', { class: 'sub' }, `Next read ${time(next, tz)}`) : h('span', { class: 'sub' }, mode[1]))),
        h('td', { class: 'num' }, sv.last_data_ms ? h('span', { title: dateTime(sv.last_data_ms, tz) }, h('span', { class: 'ta-cell-label' }, 'Last data '), ago(sv.last_data_ms)) : h('span', null, h('span', { class: 'ta-cell-label' }, 'Last data '), h('span', { class: 'faint' }, 'none yet'))));
    });
    // a waiting suggestion is already picked: it can be saved as it is
    sync();
    return h('div', { class: 'ta-cars' },
      h('div', { class: 'ta-cars-head' }, h('span', { class: 'metric-label' }, `Its cars (${items.length})`), saveBtn),
      problem,
      h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl ta-tbl' },
        h('thead', null, h('tr', null, ['Car at Tesla', 'Sentry Mate car', 'Tesla says', 'What the server does', 'Last data'].map((t, i) => h('th', { class: i === 4 ? 'num' : null, scope: 'col' }, t)))),
        h('tbody', null, rows))),
      h('p', { class: 'setting-note' }, 'A car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN prefix as a car here; it is not saved until you press Save matches.'));
  }

  return { el, load };
}

/** Status rows for Data health: what the server does for one car (null when no Tesla vehicle reads it). */
export function teslaCarStatus(ctx, statusItems, carId) {
  const sv = (statusItems || []).filter((s) => s.car === carId);
  if (!sv.length) return null;
  return sv.map((s) => {
    const mode = CAR_MODE[s.mode] || [s.mode, ''];
    return [
      ['What the server does', h('span', null, h('b', null, mode[0]), s.streaming ? ' · live stream' : '', h('span', { class: 'faint' }, ` (Tesla says ${(CAR_STATE[s.car_state] || s.car_state || 'not known').toLowerCase()})`)), { icon: 'globe', title: mode[1] }],
      ['Last Tesla data', s.last_data_ms ? h('span', { title: dateTime(s.last_data_ms, ctx.tz) }, ago(s.last_data_ms)) : h('span', { class: 'faint' }, 'none yet'), { icon: 'clock' }],
      [s.mode === 'backoff' ? 'Next try' : 'Next read', s.next_poll_ms ? `${time(s.next_poll_ms, ctx.tz)}${s.mode === 'backoff' ? ' (waiting after errors)' : ''}` : h('span', { class: 'faint' }, 'not planned'), { icon: 'calendar' }],
      ['Tesla requests today', num(s.polls_today ?? 0), { icon: 'pulse' }],
      s.last_error ? ['Tesla error', h('span', { class: 'warn' }, lastErrorText(s.last_error)), { icon: 'warn' }] : null,
    ];
  })[0];
}

export const teslaAccountHref = (id) => `${buildUrl('settings-integrations')}#tesla-${id}`;
