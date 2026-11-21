// Settings: Integrations, the Tesla account (#tesla): the Tesla API input, part of pages/settings-integrations.js.
// The server can sign in to a Tesla account and read its cars from Tesla's Owner API, as a second live input
// beside SMO. It is off until an admin adds an account: before that it makes no connection to Tesla at all. It
// only reads: it never wakes a car or sends it a command, and no answer ever holds a Tesla token.
//
// The section is a receiver panel: the input's state, the Tesla hosts it talks to, the pace with its seconds and
// the requests today on the left; adding an account (sign in at Tesla, or paste a refresh token) on the right.
// Then one HUD per account (#tesla-<id>, where notices and the audit log link): its status light, last good
// answer and last error, pause or resume, when the car's position is asked for, a new token, removal, and its
// cars, each matched to a car here, a new car or left out, with what the server does for it now.
//
// Admins only, reading included. Every change except starting a sign-in asks for the admin's own password again
// (and the two-step code): askPassword() from server-move.js, sent as confirm_password and code.
// API: GET/PATCH integrations/tesla, GET/POST .../accounts, POST .../sign-in, POST .../sign-in/{sign_in},
// PATCH/DELETE .../accounts/{id}, GET/PUT .../accounts/{id}/cars, GET .../status.

import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, btn, tag, kv, note, seg } from '../core/ui.js';
import { select } from '../core/kit.js';
import { field } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { toast } from '../core/layer.js';
import { dateTime, time, ago, dur, DASH } from '../core/fmt.js';
import { code, sig, hint } from '../core/setting.js';
import { ACCOUNT_STATUS, CAR_MODE, CAR_STATE, LOCATION, POLL, teslaWords, lastErrorText, requestsText } from '../core/tesla.js';
import { askPassword } from './server-move.js';

const enc = encodeURIComponent;
const BASE = 'integrations/tesla';
const REGION = [['global', 'Global (tesla.com)'], ['china', 'China (tesla.cn)']];
const LOCATION_WHY = {
  driving_or_charging: 'The usual: the position is asked for only while the car drives or charges, so drives get a route and charges a place.',
  always: 'Also while it is parked and awake.',
  never: 'Never: drives from the Tesla API alone get no route, and charges no place.',
};
const PACE_WHY = { often: 'Finer drives and charges from the Tesla API; uses more of the day\'s requests.', normal: 'The usual pace.', gentle: 'Fewest requests; coarser drives and charges.' };

/** Our words on an error, for the password dialog (it shows the password and code errors itself). keepPasted: the
 * pasted address is Tesla's callback, so the server's own words on it (another sign-in, no code) stay. */
function words(e, { keepPasted = false } = {}) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (e.code !== 'wrong_password' && e.code !== 'wrong_two_step' && e.code !== 'invalid') e.message = teslaWords(e);
  if (e.status === 409 && e.code !== 'account_exists' && e.code !== 'car_taken' && e.code !== 'too_many_sign_ins') e.message = teslaWords({ code: 'stale_version' });
  if (e.fields) {
    delete e.fields.confirm_password; delete e.fields.code;
    if (e.fields.pasted_url && !keepPasted) e.fields.pasted_url = 'That is not the address Tesla\'s page ended on. Paste the whole address from the address bar: it starts tesla://auth/callback (or …/void/callback) and holds code= and state=.';
    if (!Object.keys(e.fields).length) e.fields = null;
  }
  return e;
}
// a change the server refused because the page shows an old state: the page reads it again once the dialog closes
const isStale = (e) => e?.code === 'no_such_tesla_account' || e?.code === 'stale_version'
  || (e?.status === 409 && !['account_exists', 'car_taken', 'too_many_sign_ins'].includes(e.code));
const isCallback = (pasted, endsOn) => String(pasted || '').trim().split(/[?#]/)[0].replace(/\/+$/, '').toLowerCase() === endsOn.toLowerCase();

// open sign-ins at Tesla, kept in this tab (a reload included) so pressing Sign in again goes on with the same one
// instead of opening another: the server allows only five at a time, each for ten minutes
const SIGN_INS = 'hz.tesla.sign-ins';
function openSignIns(now) {
  let all = {};
  try { all = JSON.parse(sessionStorage.getItem(SIGN_INS) || '{}') || {}; } catch { /* none kept */ }
  return Object.fromEntries(Object.entries(all).filter(([, x]) => x && x.sign_in && x.url && x.expires_ms - 30_000 > now));
}
function keepSignIn(key, s, now) {
  const all = openSignIns(now);
  if (s) all[key] = { sign_in: s.sign_in, url: s.url, expires_ms: s.expires_ms }; else delete all[key];
  try { sessionStorage.setItem(SIGN_INS, JSON.stringify(all)); } catch { /* kept for this press only */ }
}

// a host name breaks only between its labels, and never inside the last two (teslamotors.com stays whole)
function hostChip(x) {
  const parts = String(x).split('.');
  const lead = parts.length > 2 ? parts.slice(0, -2) : [];
  const tail = parts.slice(lead.length).join('.');
  return h('code', { class: 'cd tz-host' }, ...lead.flatMap((p) => [`${p}.`, h('wbr')]), tail);
}
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const secs = (s) => (s == null ? DASH : s >= 120 && s % 60 === 0 ? `${formatNumber(s / 60)} min` : `${formatNumber(s)} s`);

/**
 * The Tesla account section. cars: this server's cars ({ id, name, time_zone }) for the match picker.
 * Returns { el, load() }; the page awaits load() before ready().
 */
export function teslaSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const side = h('span', { class: 'tz-side' });
  const body = h('div', { class: 'stack' });
  const el = h('section', { class: 'stack tz', id: 'tesla', 'aria-label': 'Tesla account' },
    hud({ title: 'Tesla account', icon: 'antenna', tone: 'cyan', side },
      h('div', { class: 'stack' },
        h('p', { class: 'tz-intro' }, 'A second live input beside SMO: the server signs in to a Tesla account and reads its cars from Tesla\'s Owner API (the one Tesla\'s own app uses). It only reads, and never wakes a car or sends it a command. Off until an admin adds an account; until then the server makes no connection to Tesla at all. Every page then says where its data came from: SMO, the Tesla API, or both.'),
        body)));

  let ov = null, accounts = [], status = [], carRows = {};
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  // askPassword, and when the server said the page is out of date (someone changed or removed the account
  // meanwhile), the page reads everything again once the dialog closes, as the dialog's words promise
  async function ask(opts, wordsOpts) {
    let stale = false;
    const r = await askPassword(ctx, { ...opts, run: async (cred, v) => {
      try { return await opts.run(cred, v); } catch (e) {
        const w = words(e, typeof wordsOpts === 'function' ? wordsOpts(v) : wordsOpts);
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
      replace(body, note('Only an admin sees and changes the Tesla account: its status, its cars and how often Tesla is asked. Every page still says where its data came from (SMO, the Tesla API or an import).'));
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
      replace(body, h('p', { class: 'warn' }, 'The Tesla account could not be read: ', errorWords(e).text));
      return;
    }
    if (!ctx.alive()) return;
    paint();
  }
  const reload = async (msg) => { if (msg) toast(msg); await load(); };

  // what the input does now, from the accounts themselves: a paused account, or one Tesla stopped taking, reads no car
  const isMatched = (c) => c.match != null && c.match !== 'leave_out';
  function reading() {
    const live = accounts.filter((a) => !a.paused && a.status !== 'sign_in_needed');
    const cars = live.reduce((n, a) => n + (carRows[a.id]?.items || a.cars || []).filter(isMatched).length, 0);
    const paused = accounts.filter((a) => a.paused).length;
    const needSignIn = accounts.filter((a) => !a.paused && a.status === 'sign_in_needed').length;
    return { live: live.length, cars, paused, needSignIn };
  }
  const nOf = (n, one, many) => `${formatNumber(n)} ${n === 1 ? one : many}`;
  function paint() {
    const rd = reading();
    replace(side, !ov.enabled ? sig('off', 'Off until an account is added')
      : !rd.live ? (rd.needSignIn ? sig('warn', 'Sign-in needed: no car read') : sig('off', 'Paused: no car read'))
        : rd.cars ? sig('ok', `Reading ${nOf(rd.cars, 'car', 'cars')}`) : sig('warn', 'No car matched yet'));
    replace(body, h('div', { class: 'tz-grid' }, statusPanel(), addPanel()));
    replace(accSlot, accounts.map(accountPanel));
  }
  const accSlot = h('div', { class: 'stack' });
  el.append(accSlot);

  /* ---------- the input as a whole */
  function statusPanel() {
    const p = ov.polls || {};
    const cap = (p.daily_cap || 0) * Math.max(1, ov.accounts || 0);
    const rd = reading();
    const input = !ov.enabled ? ['Off', 'until an account is added']
      : !rd.live ? [rd.needSignIn ? 'Stopped' : 'Paused', [rd.paused ? `${nOf(rd.paused, 'account', 'accounts')} paused` : '', rd.needSignIn ? `${nOf(rd.needSignIn, 'account needs', 'accounts need')} a new sign-in` : ''].filter(Boolean).join(', ') + ': no car read']
        : ['On', `${nOf(rd.live, 'account', 'accounts')}, ${nOf(rd.cars, 'car', 'cars')} read${rd.paused ? `; ${formatNumber(rd.paused)} paused` : ''}${rd.needSignIn ? `; ${formatNumber(rd.needSignIn)} needs a sign-in` : ''}`];
    const pace = seg(POLL.map(([v, l]) => [v, l]), ov.poll, (v) => setPace(v, pace), { label: 'How often Tesla is asked' });
    return h('div', { class: 'tz-face' },
      h('div', { class: 'tz-read' },
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Input'), h('span', { class: 'tz-v' }, input[0]),
          h('span', { class: 'tz-how' }, input[1])),
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Requests today'), h('span', { class: 'tz-v' }, formatNumber(ov.polls_today ?? 0)),
          h('span', { class: 'tz-how' }, ov.enabled && cap ? `of at most ${formatNumber(cap)}` : 'none without an account')),
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Connections'), h('span', { class: 'tz-v' }, formatNumber(ov.clients_built ?? 0)),
          h('span', { class: 'tz-how' }, 'set up since the server started'))),
      ov.last_error
        ? h('p', { class: 'tz-err' }, icon('warn', 'i--sm'), h('span', null, 'Last error: ', h('b', null, lastErrorText(ov.last_error)), ov.last_error.at_ms ? `, ${dateTime(ov.last_error.at_ms, ctx.tz)} (${ago(ov.last_error.at_ms)})` : '', '.'))
        : h('p', { class: 'tz-ok' }, icon('check', 'i--sm'), h('span', null, ov.enabled ? 'No error.' : 'Nothing asked of Tesla yet.')),
      h('div', { class: 'tz-block' }, h('p', { class: 'tz-k' }, 'Tesla hosts it talks to'),
        ov.hosts?.length ? h('ul', { class: 'tz-hosts' }, ov.hosts.map((x) => h('li', null, code(x))))
          : h('p', { class: 'tz-dim' }, 'None while there is no account. Once one is added: Tesla\'s sign-in host, its Owner API and (outside China) its live stream, all over HTTPS.')),
      h('div', { class: 'tz-block' },
        h('div', { class: 'tz-pace-head' }, h('p', { class: 'tz-k' }, 'How often Tesla is asked'), pace.el),
        h('dl', { class: 'tz-pace' },
          [['A parked, awake car', p.parked_s], ['A charging car', p.charging_s], ['A driving car', p.driving_s], ['Driving, with Tesla\'s live stream', p.driving_streaming_s],
            ['The car list (cars asleep)', p.list_s], ['Any car while SMO is live', p.smo_live_s]].map(([l, s]) => h('div', null, h('dt', null, l), h('dd', null, `every ${secs(s)}`)))),
        h('p', { class: 'tz-dim' }, `${PACE_WHY[ov.poll] || ''} At every pace a car asleep is never woken (only the car list is read), and an awake car with nothing going on is left alone so it can fall asleep.${p.daily_cap ? ` At most ${formatNumber(p.daily_cap)} requests a day per account.` : ''}`)));
  }
  async function setPace(v, pace) {
    if (v === ov.poll) return;
    const r = await ask({
      title: `Pace: ${POLL.find((x) => x[0] === v)[1]}`,
      intro: h('p', null, 'How often the server reads each car from the Tesla API. ', PACE_WHY[v]),
      submit: 'Change the pace',
      run: async (cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v })),
    });
    if (r) await reload('How often Tesla is asked: saved.');
    else pace.set(ov.poll);
  }

  /* ---------- adding an account */
  function addPanel() {
    const region = select(REGION, 'global', null, { label: 'Tesla region' });
    const voidBox = h('input', { type: 'checkbox', id: 'tz-void' });
    return h('div', { class: 'tz-add' },
      h('p', { class: 'tz-add-head' }, icon('plus', 'i--sm'), h('span', null, accounts.length ? 'Add another account' : 'Add a Tesla account')),
      h('label', { class: 'tz-region' }, h('span', { class: 'tz-k' }, 'Region'), region),
      h('div', { class: 'tz-way' },
        h('p', { class: 'card-title' }, 'Sign in at Tesla'),
        h('p', { class: 'card-text' }, 'Opens Tesla\'s own sign-in page in a new tab. Tesla\'s page asks for the Tesla password, its two-step code and any captcha: this server never sees them. Then you paste the address Tesla\'s page ends on.'),
        h('label', { class: 'check tz-void', for: 'tz-void' }, voidBox, h('span', null, 'End on Tesla\'s ', code('/void/callback'), ' page instead (for a browser that will not show ', code('tesla://'), ' addresses)')),
        h('div', { class: 'row' }, btn('Sign in at Tesla', (e) => signIn(e.currentTarget, region.value, voidBox.checked ? 'void' : 'app'), { kind: 'go', icon: 'ext' }))),
      h('div', { class: 'tz-way' },
        h('p', { class: 'card-title' }, 'Paste a refresh token'),
        h('p', { class: 'card-text' }, 'A Tesla refresh token from another tool. The server checks it with Tesla (one token refresh, one car list) before anything is kept, and never shows it again.'),
        h('div', { class: 'row' }, btn('Paste a refresh token', () => addByToken(region.value), { icon: 'key' }))),
      hint('A new account\'s cars are not read until you match each one to a car here or to a new car.'));
  }

  async function signIn(b, region, redirect) {
    const key = `${region}:${redirect}`;
    let s = openSignIns(ctx.api.now())[key];
    if (!s) {
      b.disabled = true;
      try { s = await ctx.api.post(`${BASE}/sign-in`, { region, redirect }); } catch (e) {
        if (e?.name !== 'AbortError') toast(`Could not start the sign-in: ${words(e).message}`, { kind: 'error' });
        return;
      } finally { b.disabled = false; }
      if (!ctx.alive()) return;
      keepSignIn(key, s, ctx.api.now());
    }
    const endsOn = redirect === 'void' ? `https://auth.tesla.${region === 'china' ? 'cn' : 'com'}/void/callback` : 'tesla://auth/callback';
    const label = field({ label: 'Name for this account', name: 'label', required: true, value: 'Tesla account', hint: 'Your own name for it, shown on this page. Not the account\'s e-mail.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const pasted = field({ label: 'The address Tesla\'s page ended on', name: 'pasted_url', type: 'textarea', required: true,
      hint: `The whole address from the address bar. It starts ${endsOn}`, attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3', placeholder: `${endsOn}?code=…&state=…` } });
    const left = Math.max(0, (s.expires_ms ?? ctx.api.now() + 600_000) - ctx.api.now());
    const intro = h('div', { class: 'stack tz-steps' },
      h('ol', { class: 'tz-step-list' },
        h('li', null, h('b', null, 'Open Tesla\'s page'), ' and sign in there. Tesla\'s own page asks for your Tesla password and code; this server never sees them. ',
          h('a', { class: 'btn btn--go btn--sm', href: s.url, target: '_blank', rel: 'noopener noreferrer external' }, icon('ext', 'i--sm'), h('span', null, 'Open Tesla\'s sign-in page'))),
        h('li', null, h('b', null, 'The last page will not load.'), ' That is expected: it ends on an address that starts ', code(endsOn), ', which a browser cannot open. It may show an error or seem to do nothing; the address is still in the address bar.'),
        h('li', null, h('b', null, 'Copy that whole address'), ' and paste it below, with your own Sentry Mate password.')),
      h('p', { class: 'note' }, icon('clock', 'i--sm'), h('span', null, `This sign-in works once and lasts ten minutes (${dur(left)} left), until ${time(s.expires_ms, ctx.tz)}.`)));
    const r = await ask({
      title: 'Sign in at Tesla', intro, submit: 'Add the account', extra: [label, pasted],
      run: async (cred, v) => ctx.api.post(`${BASE}/sign-in/${enc(s.sign_in)}`, withPw(cred, { pasted_url: v.pasted_url.trim(), label: v.label.trim() })).catch((e) => {
        if (e?.code === 'no_such_sign_in') keepSignIn(key, null, ctx.api.now()); // ran out or used: the next press starts a new one
        throw e;
      }),
    }, (v) => ({ keepPasted: isCallback(v.pasted_url, endsOn) }));
    if (r) keepSignIn(key, null, ctx.api.now());
    if (r) await reload(`${r.label} added. Match its cars below to start reading them.`);
  }

  async function addByToken(region) {
    const label = field({ label: 'Name for this account', name: 'label', required: true, value: 'Tesla account', hint: 'Your own name for it. Not the account\'s e-mail.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const reg = field({ label: 'Region', name: 'region', type: 'select', value: region, options: REGION });
    const tok = field({ label: 'Refresh token', name: 'refresh_token', type: 'textarea', required: true, hint: 'Checked with Tesla before it is kept. Sealed on the server, never shown again.', attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3' } });
    const r = await ask({
      title: 'Add a Tesla account by refresh token', submit: 'Check with Tesla and add', extra: [label, reg, tok],
      intro: h('p', null, 'Only paste a token of your own Tesla account. Anyone with it can read the account\'s cars, so it never leaves this server and no page shows it.'),
      run: async (cred, v) => ctx.api.post(`${BASE}/accounts`, withPw(cred, { label: v.label.trim(), region: v.region, refresh_token: v.refresh_token.trim() })),
    });
    if (r) await reload(`${r.label} added. Match its cars below to start reading them.`);
  }

  /* ---------- one account */
  function accountPanel(a) {
    const st = ACCOUNT_STATUS[a.status] || [a.status || 'Not known', 'off', ''];
    const err = a.last_error;
    const change = (title, intro, submit, bodyOf, { extra = [], msg = 'Saved.', danger = false } = {}) => ask({
      title, intro: intro ? h('p', null, intro) : null, submit, extra, danger,
      run: async (cred, v) => ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: a.version, ...bodyOf(v) })),
    }).then((r) => (r ? reload(msg) : null));

    const pauseBtn = btn(a.paused ? 'Resume' : 'Pause', () => change(a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`,
      a.paused ? 'Its matched cars are read again from now on.' : 'Its cars are not read until you resume it. Nothing is asked of Tesla meanwhile; the history already stored stays.',
      a.paused ? 'Resume' : 'Pause', () => ({ paused: !a.paused }), { msg: a.paused ? `${a.label} resumed.` : `${a.label} paused.` }),
    { sm: true, icon: a.paused ? 'live' : 'moon' });
    const tokenBtn = btn('Replace token', () => {
      const f = field({ label: 'New refresh token', name: 'refresh_token', type: 'textarea', required: true, hint: 'Checked with Tesla before it replaces the old one, which is deleted.', attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3' } });
      return change(`New token for ${a.label}`, 'For an account Tesla stopped taking, or a token you renewed elsewhere.', 'Check with Tesla and replace', (v) => ({ refresh_token: v.refresh_token.trim() }), { extra: [f], msg: 'Token replaced and checked with Tesla.' });
    }, { sm: true, icon: 'key', kind: a.status === 'sign_in_needed' ? 'go' : null });
    const renameBtn = btn('Rename', () => {
      const f = field({ label: 'Name', name: 'label', required: true, value: a.label, hint: 'Your own name for it. Not the account\'s e-mail.', attrs: { maxlength: '64', autocomplete: 'off' } });
      return change(`Rename ${a.label}`, null, 'Rename', (v) => ({ label: v.label.trim() }), { extra: [f], msg: 'Renamed.' });
    }, { sm: true, icon: 'sliders', kind: 'ghost' });
    const removeBtn = btn('Remove', async () => {
      const r = await ask({
        title: `Remove ${a.label}?`, danger: true, submit: 'Remove the account',
        intro: h('p', null, 'Reading stops at once and the sealed file with its tokens is deleted. The history already stored from it stays, marked as data from the Tesla API.'),
        run: async (cred) => ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred)),
      });
      if (r) await reload(`${a.label} removed. Its history stays.`);
    }, { sm: true, icon: 'x', kind: 'ghost' });

    const loc = seg(LOCATION, a.location, (v) => {
      if (v === a.location) return;
      const l = LOCATION.find((x) => x[0] === v)[1];
      change(`Ask for the position: ${l.toLowerCase()}?`, LOCATION_WHY[v], 'Save', () => ({ location: v }), { msg: 'When the position is asked for: saved.' })
        .then((r) => { if (r === null) loc.set(a.location); });
    }, { label: `When the position of ${a.label}'s cars is asked for` });

    const errText = err ? [lastErrorText(err), err.at_ms ? ` · ${ago(err.at_ms)}` : ''] : null;
    return hud({ title: a.label, icon: 'key', id: `tesla-${a.id}`, cls: 'tz-acc', label: `Tesla account ${a.label}`,
      side: [sig(st[1], st[0]), tag(a.region === 'china' ? 'China' : 'Global', 'cyan')] },
      h('div', { class: 'stack' },
        st[2] ? h('p', { class: ['tz-why', (a.status === 'sign_in_needed' || a.status === 'error') && 'warn'] }, st[2]) : null,
        h('div', { class: 'tz-acc-grid' },
          kv([
            ['Added', dateTime(a.added_ms, ctx.tz)],
            ['Last good answer', a.last_ok_ms ? h('span', { title: dateTime(a.last_ok_ms, ctx.tz) }, ago(a.last_ok_ms)) : h('span', { class: 'faint' }, 'none yet')],
            ['Last error', errText ? h('span', { class: 'warn', title: err.message || null }, errText) : h('span', { class: 'faint' }, 'none')],
            ['Requests today', requestsText(a.polls_today, ov.polls?.daily_cap)],
            ['Hosts', a.hosts?.length ? h('span', { class: 'tz-inline' }, a.hosts.map(hostChip)) : h('span', { class: 'faint' }, 'none yet')],
          ]),
          h('div', { class: 'stack' },
            h('div', { class: 'row' }, pauseBtn, tokenBtn, renameBtn, removeBtn),
            h('div', { class: 'tz-block' },
              h('p', { class: 'tz-k' }, 'Ask for the cars\' position'), loc.el,
              h('p', { class: 'tz-dim tz-loc' }, icon('pin', 'i--sm'), h('span', null, 'Asking for the position shows an icon on the car\'s screen that its location is being shared. ', LOCATION_WHY[a.location] || ''))))),
        carsTable(a)));
  }

  /* ---------- an account's cars: match each one, and what the server does for it now */
  function carsTable(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    if (!items.length) return emptyState({ icon: 'car', title: 'No cars on this account', text: 'Tesla lists no car for this account. A car added to it at Tesla shows here within a few minutes.' });
    // a car here feeds from one Tesla car only: one matched on another account is marked and offered last
    const takenBy = new Map();
    for (const o of accounts) if (o.id !== a.id) for (const c of carRows[o.id]?.items || o.cars || []) if (isMatched(c) && c.match !== 'new') takenBy.set(c.match, o.label);
    const free = cars.filter((c) => !takenBy.has(c.id)), taken = cars.filter((c) => takenBy.has(c.id));
    const opts = [['', 'Not decided (not read)'], ...free.map((c) => [c.id, c.name]), ['new', 'A new car'], ['leave_out', 'Leave out (not read)'],
      ...(taken.length ? [{ group: 'Matched on another account', items: taken.map((c) => [c.id, `${c.name} (${takenBy.get(c.id)})`]) }] : [])];
    const sels = new Map();
    const save = btn('Save matches', () => saveMatches(), { kind: 'go', sm: true, icon: 'check' });
    const clash = h('p', { class: 'field-error tz-clash', role: 'alert', hidden: true });
    const changed = () => items.filter((c) => (sels.get(c.vehicle).value || null) !== (c.match ?? null));
    // two Tesla cars of this account picked for the same car here
    function twice() {
      const by = new Map();
      for (const c of items) { const v = sels.get(c.vehicle).value; if (v && v !== 'new' && v !== 'leave_out') by.set(v, [...(by.get(v) || []), c]); }
      return [...by].filter(([, l]) => l.length > 1);
    }
    function sync() {
      const tw = twice();
      for (const c of items) { const bad = tw.some(([, l]) => l.includes(c)); if (bad) sels.get(c.vehicle).setAttribute('aria-invalid', 'true'); else sels.get(c.vehicle).removeAttribute('aria-invalid'); }
      clash.hidden = !tw.length;
      if (tw.length) replace(clash, icon('warn', 'i--sm'), h('span', null, tw.map(([v, l]) => `The Tesla cars ${l.map((c) => c.display_name || c.vehicle).join(' and ')} are both matched to ${carName(v)} here`).join('; '), '. A car here is fed by one Tesla car only: pick another car, a new car or leave one out.'));
      save.hidden = !changed().length;
      save.disabled = !!tw.length;
    }
    async function saveMatches() {
      const ch = changed();
      if (!ch.length || twice().length) return;
      const r = await ask({
        title: `Match the cars of ${a.label}`, submit: 'Save matches',
        intro: h('ul', { class: 'tz-list' }, ch.map((c) => { const v = sels.get(c.vehicle).value; return h('li', null, h('b', null, c.display_name || c.vehicle), ': ', v === 'new' ? 'a new car here, read from now on' : v === 'leave_out' ? 'left out, not read' : !v ? 'not decided, not read' : `${carName(v)}, read from now on`); })),
        run: async (cred) => ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, withPw(cred, { version: data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: sels.get(c.vehicle).value || null })) })),
      });
      if (r) await reload('Matches saved.');
    }
    const rows = items.map((c) => {
      const sv = status.find((s) => s.account === a.id && s.vehicle === c.vehicle) || {};
      const pre = c.match ?? c.suggested ?? '';
      const known = opts.some((o) => (o.group ? o.items.some(([v]) => v === pre) : o[0] === pre)) ? opts : [...opts, [pre, carName(pre)]];
      const sel = select(known, pre, sync, { label: `Car here for ${c.display_name || c.vehicle}`, cls: 'tz-sel' });
      sels.set(c.vehicle, sel);
      const mode = CAR_MODE[c.mode || sv.mode] || [c.mode || 'Not known', 'off', ''];
      const next = sv.next_poll_ms ?? null;
      const tz = cars.find((x) => x.id === c.match)?.time_zone || ctx.tz;
      return h('li', { class: 'tz-car' },
        h('div', { class: 'tz-car-id' }, h('p', { class: 'tz-car-name' }, icon('car', 'i--sm'), h('b', null, c.display_name || 'No name at Tesla')),
          h('p', { class: 'tz-dim' }, [c.year, c.model].filter(Boolean).join(' '), ' · ', h('span', { class: 'mono' }, c.vin_prefix || DASH), ' · Tesla says ', (CAR_STATE[c.state] || c.state || 'not known').toLowerCase())),
        h('div', { class: 'tz-car-match' }, h('span', { class: 'tz-k' }, 'Car here'), sel,
          c.match == null && c.suggested ? h('span', { class: 'tz-sugg' }, tag('Suggested', 'grn'), ' same VIN start as ', carName(c.suggested), '; saved only with Save matches') : null),
        h('div', { class: 'tz-car-mode', title: mode[2] }, h('span', { class: 'tz-k' }, 'Doing now'), sig(mode[1], `${mode[0]}${sv.streaming ? ' · live stream' : ''}`),
          (c.mode || sv.mode) === 'backoff' && next ? h('span', { class: 'tz-dim warn' }, `Next try ${time(next, tz)} (in ${dur(Math.max(0, next - ctx.api.now()))})`)
            : next && c.mode !== 'off' ? h('span', { class: 'tz-dim' }, `Next read ${time(next, tz)}`) : h('span', { class: 'tz-dim' }, mode[2]),
          sv.last_data_ms ? h('span', { class: 'tz-dim' }, `Newest values ${ago(sv.last_data_ms)}`) : null));
    });
    sync();
    return h('div', { class: 'tz-cars' },
      h('div', { class: 'tz-cars-head' }, h('p', { class: 'tz-k' }, `Its cars (${items.length})`), save),
      clash,
      h('ul', { class: 'tz-car-list' }, rows),
      hint('A car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN start as a car here; nothing changes until you press Save matches.'));
  }

  return { el, load };
}
