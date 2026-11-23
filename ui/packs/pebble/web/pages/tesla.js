// Sentry Mate, "Pebble" UI pack: the Tesla account, on Settings: Integrations (#tesla).
//
// The Tesla API input: this server can sign in to a Tesla account and read its cars from Tesla's Owner API (the
// one Tesla's own app uses), as a second live input beside SMO. It is off until an admin adds an account; before
// that the server makes no connection to Tesla at all. It only reads: it never wakes a car or sends it a command,
// and no answer ever holds a Tesla token.
//
// Admins only, reading included. Tiles: the input's status (on or off, Tesla hosts, requests today, the last
// error), how often Tesla is asked (gentle, normal, often, with the seconds each gives), adding an account (sign in
// at Tesla and paste the address its page ends on, or paste a refresh token), and per account a tile with its
// status and actions (pause or resume, when the position is asked for, a new token, rename, remove) and a tile of
// its cars, each matched to a car here, a new car or left out, with what the server does for it now. Every change
// but starting a sign-in and saving the matches asks for the admin's own password again (and the two-step code):
// askPassword(). The matches take the admin's session alone (they store and show no token).
// Each account tile has the id tesla-<account>, so a notice can open it.
//
// Data: GET/PATCH integrations/tesla, GET/POST integrations/tesla/accounts, POST integrations/tesla/sign-in,
// POST integrations/tesla/sign-in/{sign_in}, PATCH/DELETE integrations/tesla/accounts/{id},
// GET/PUT integrations/tesla/accounts/{id}/cars, GET integrations/tesla/status.

import { h } from '../shared/dom.js';
import * as state from '../shared/state.js';
import { formatTime, formatDateTime, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { tile, btn, chip, note, icon, ago } from '../core/ui.js';
import { field, segmented } from '../core/form.js';
import { errorWords } from '../core/states.js';
import { facts } from '../core/bits.js';
import { group, infoRow, pickRow, said, askPassword, tick } from '../core/settings.js';
import { invoicesTile, invoicesSlot, loadUnmatched } from './invoices-tile.js';

const enc = encodeURIComponent;
const BASE = 'integrations/tesla';
// Sign-ins started here and not finished, by region and redirect: the server has no call that ends one and keeps
// at most five open, so a sheet closed without finishing leaves its sign-in for the next press (while it has
// two minutes left).
const openSignIns = new Map();

/* ---------------------------------------------------------------------- words */

// account status: [chip word, tone, what it means]
export const ACCOUNT_STATUS = {
  checking: ['Checking', 'muted', 'Just added: the server is asking Tesla for its cars.'],
  ok: ['Reading', 'teal', 'Tesla takes its sign-in; its matched cars are read.'],
  paused: ['Paused', 'muted', 'Its cars are not read until you resume it. Nothing is asked of Tesla meanwhile.'],
  sign_in_needed: ['Sign in again', 'sun', 'Tesla stopped taking its sign-in, so its cars are not read. Replace its token, or remove it and sign in at Tesla again.'],
  error: ['Error', 'bad', 'Tesla refuses it, or answers with errors. See the last error.'],
};
// what the server does for a car: [words, why]
export const CAR_MODE = {
  off: ['Not read', 'Not matched, left out, or its account is paused.'],
  watching_list: ['Watching the car list', 'Asleep or offline: only the list of cars is read, so the car is never woken.'],
  online_idle: ['Awake, idle', 'Awake with nothing going on: read at the parked pace.'],
  letting_sleep: ['Letting it sleep', 'Awake and quiet for 10 minutes: left alone for a while so it can fall asleep.'],
  charging: ['Charging', 'Read at the charging pace.'],
  driving: ['Driving', 'Read at the driving pace.'],
  backoff: ['Waiting after errors', 'Tesla answered with errors, or the day’s 3,000 requests are used up: nothing is asked until the next try.'],
};
const CAR_STATE = { online: 'online', asleep: 'asleep', offline: 'offline', unknown: 'not known' };
const LOCATION = [
  ['driving_or_charging', 'Driving or charging', 'The position is asked for only while the car drives or charges.'],
  ['always', 'Always', 'Also while parked and awake.'],
  ['never', 'Never', 'No position from the Tesla API at all: its drives have no route and its charges no place.'],
];
const PACE = [['gentle', 'Gentle'], ['normal', 'Normal'], ['often', 'Often']];
const REGION = [['global', 'Global (tesla.com)'], ['china', 'China (tesla.cn)']];

// error codes of a change, in words
const ERROR_WORDS = {
  tesla_refused: 'Tesla said no to this sign-in or token: it may have run out, been used already, or the Tesla password changed since. Sign in at Tesla again, or paste a new refresh token.',
  tesla_unreachable: 'Tesla could not be reached, or answered with an error. Nothing was kept. Try again in a few minutes.',
  // the same two on a pasted sign-in address: Tesla has used up that sign-in, so the sheet starts a new one
  signin_refused: 'Tesla said no to this sign-in: it may have run out at Tesla, or been used already. That sign-in is used up.',
  signin_unreachable: 'Tesla could not be reached, or answered with an error, and nothing was kept. That sign-in is used up.',
  account_exists: 'The cars of this Tesla account are already read through another Tesla account here. Replace that account’s token instead, or remove it first.',
  car_taken: 'One of these cars is already matched to a car of another Tesla account here. Leave it out there first, or pick another car.',
  too_many_sign_ins: 'Five sign-ins are open already. Finish one, or wait up to ten minutes for them to run out.',
  no_such_sign_in: 'This sign-in ran out (it lasts ten minutes) or was used already. Close this and start a new one.',
  no_such_tesla_account: 'This Tesla account is not here any more: another admin may have removed it. Nothing was changed; the page now shows the accounts there are.',
  version_conflict: 'Someone else changed this a moment ago, so nothing was saved. The page now shows their change: make yours again if it is still needed.',
};
// last_error codes of an account, a car or the whole input
const LAST_ERROR_WORDS = {
  sign_in_needed: 'Tesla stopped taking the sign-in',
  tesla_refused: 'Tesla refused the token',
  tesla_unreachable: 'Tesla could not be reached',
  endpoint_gone: 'Tesla closed the address the server reads, so it stops asking',
  rate_limited: 'Tesla asked the server to slow down',
  daily_cap: 'the day’s 3,000 requests are used up',
  forbidden: 'Tesla refused the request (403)',
  server_error: 'Tesla answered with a server error',
  timeout: 'Tesla took too long to answer',
};
export const lastErrorText = (e) => (e ? (LAST_ERROR_WORDS[e.code] || e.message || e.code) : null);
const cap1 = (t) => (t ? t.charAt(0).toUpperCase() + t.slice(1) : t);

/** Our words on an error of a change; the password sheet handles wrong_password and wrong_two_step itself. */
function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (ERROR_WORDS[e.code]) e.message = ERROR_WORDS[e.code];
  else if (e.status === 409) e.message = ERROR_WORDS.version_conflict;
  if (e.fields) {
    const f = { ...e.fields };
    delete f.confirm_password; delete f.code;
    // where to find the address helps only when what was pasted is not that address
    if (f.pasted_url && /another sign-in/i.test(String(f.pasted_url))) f.pasted_url = 'This address is from another sign-in. Open Tesla’s page with the button above, sign in, and paste the address it ends on.';
    else if (f.pasted_url && !/start a new/i.test(String(f.pasted_url))) f.pasted_url = `${String(f.pasted_url).replace(/\.$/, '')}. It is in the address bar of Tesla’s last page and starts tesla:// (or https://auth.tesla…/void/callback).`;
    e.fields = Object.keys(f).length ? f : null;
  }
  return e;
}
// the account or the input changed, or the account went away, since the page read it
const isStale = (e) => !!e && (e.code === 'no_such_tesla_account' || e.code === 'version_conflict' || e.code === 'stale_version' || (e.status === 409 && !ERROR_WORDS[e.code]));
/** run() for askPassword: our words on an error; a stale page closes the sheet with { stale: words } instead. */
const guarded = (fn) => async (...a) => {
  try { return await fn(...a); } catch (e) { words(e); if (isStale(e)) return { stale: e.message }; throw e; }
};
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const mono = (t) => h('code', { class: 'set-mono' }, t);
const secs = (s) => (s == null ? '—' : s >= 120 && s % 60 === 0 ? `${formatNumber(s / 60)} min` : `${formatNumber(s)} s`);
const HINT = 'Tesla account changes ask for it again.';

export const teslaAccountHref = (id) => `${state.buildUrl('settings-integrations')}#tesla-${enc(id)}`;

/* ---------------------------------------------------------------------- the tiles */

/**
 * The Tesla account tiles. cars: Sentry Mate's cars ({ id, name, time_zone }) for the match picker.
 * Returns { el, load() }: el is a display: contents box for the grid; load reads everything and paints.
 */
export function teslaTiles(ctx, { cars = [] } = {}) {
  const el = h('div', { class: 'contents' });
  let ov = null, accounts = [], status = [], carRows = {}, unmatched = null;
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  const head = () => h('div', { class: 'section-head w-full', id: 'tesla' },
    h('h2', { class: 'section-title' }, 'Tesla account'),
    h('p', { class: 'note' }, 'A second live input beside SMO: this server can sign in to a Tesla account and read its cars from Tesla’s Owner API, the one Tesla’s own app uses. It only reads: it never wakes a car or sends it a command. Every page then says whether its data came from SMO, the Tesla API, or both.'));

  async function load() {
    if (!ctx.admin) {
      el.replaceChildren(head(), tile({ w: 2, title: 'Tesla account', icon: 'key', side: chip('Admins only', { tone: 'muted' }) },
        said('info', 'Only an admin sees and changes the Tesla account: its status, its cars and how often Tesla is asked. Every page still says where its data came from (SMO, the Tesla API or an import).')), invoicesSlot('tesla'));
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
      el.replaceChildren(head(), tile({ w: 2, title: 'Tesla account', icon: 'key', side: chip('Not read', { tone: 'bad' }) }, said('bad', 'The Tesla account could not be read: ', errorWords(e).text)), invoicesSlot('tesla'));
      return;
    }
    if (!ctx.alive()) return;
    unmatched = await loadUnmatched(ctx, 'tesla', ov);
    if (!ctx.alive()) return;
    el.replaceChildren(head(), statusTile(), paceTile(), invoicesTile(ctx, { source: 'tesla', ov, unmatched, setOn: setInvoices }), ...accounts.flatMap((a) => [accountTile(a), carsTile(a)]), addTile());
  }
  const reload = async (msg) => { if (msg) ctx.toast(msg); await load(); };
  /** After a password sheet: nothing when closed; a stale page is read again, with why; else msg and read again. */
  const after = async (r, msg) => {
    if (!r) return false;
    if (r.stale) { ctx.toast(r.stale, { kind: 'info' }); await load(); return true; }
    await reload(typeof msg === 'function' ? msg(r) : msg);
    return true;
  };

  /* ---------- the input as a whole */
  function statusTile() {
    const cap = ((ov.polls && ov.polls.daily_cap) || 3000) * Math.max(1, ov.accounts || 0);
    const side = !ov.enabled ? chip('Off', { tone: 'muted', icon: 'minus' })
      : ov.cars ? chip(`Reading ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'}`, { tone: 'teal', icon: 'check' }) : chip('No car matched yet', { tone: 'sun', icon: 'warning' });
    const check = (level, name, sub, word) => h('li', { class: ['check', `is-${level}`] }, h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
      h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, name), sub ? h('span', { class: 'check-sub' }, sub) : null), h('span', { class: 'check-word' }, word));
    const le = ov.last_error;
    return tile({ w: 2, title: 'Tesla API input', icon: 'key', side },
      !ov.enabled ? said('info', 'Off until an account is added. Until then the server makes no connection to Tesla at all.') : null,
      h('ul', { class: 'checks' },
        check(ov.enabled ? 'ok' : 'off', ov.enabled ? `${formatNumber(ov.accounts)} ${ov.accounts === 1 ? 'account' : 'accounts'}, ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'} read` : 'No Tesla account', ov.enabled ? 'Each matched car is read from the Tesla API as well as from SMO.' : 'Add one below to read its cars.', ov.enabled ? 'On' : 'Off'),
        check(ov.enabled ? 'ok' : 'off', 'Requests today', ov.enabled ? `Of at most ${formatNumber(cap)}: 3,000 a day per account.` : 'None: there is no account.', formatNumber(ov.polls_today ?? 0)),
        check('ok', 'Connections set up', 'To Tesla, since the server started.', formatNumber(ov.clients_built ?? 0)),
        check(le ? 'warn' : 'ok', 'Last error', le ? [cap1(lastErrorText(le)), le.at_ms ? `, ${formatDateTime(le.at_ms, ctx.tz, { now: ctx.api.now() })}` : ''] : 'None.', le ? 'Yes' : 'None')),
      group('Tesla hosts it talks to', ov.hosts && ov.hosts.length ? ov.hosts.map((x) => infoRow({ icon: 'globe', name: mono(x), sub: 'Over HTTPS only' }))
        : [infoRow({ icon: 'globe', name: 'None', sub: 'No host while there is no account. Once one is added: Tesla’s sign-in host, its Owner API and, outside China, its live stream, all over HTTPS.' })]));
  }

  function paceTile() {
    const p = ov.polls || {};
    const seg = segmented({ name: 'How often Tesla is asked', value: ov.poll, options: PACE, onchange: (v) => setPace(v, seg) });
    return tile({ w: 2, title: 'How often Tesla is asked', icon: 'clock' },
      seg,
      facts([
        ['Parked, awake', `every ${secs(p.parked_s)}`],
        ['Charging', `every ${secs(p.charging_s)}`],
        ['Driving', `every ${secs(p.driving_s)}`],
        ['Driving, with Tesla’s live stream', `every ${secs(p.driving_streaming_s)}`],
        ['The car list (all an asleep car gets)', `every ${secs(p.list_s)}`],
        ['Requests a day', `at most ${formatNumber(p.daily_cap ?? 3000)} per account`],
      ]),
      note('At every pace an asleep car is never woken (only the list of cars is read), and an awake car with nothing going on is left alone so it can fall asleep. A faster pace gives finer drives and charges from the API and uses more of the day’s requests.'));
  }
  async function setInvoices(on, sw) {
    const r = await askPassword(ctx, {
      title: on ? 'Turn Supercharger invoices on' : 'Turn Supercharger invoices off', yes: on ? 'Turn on' : 'Turn off', hint: HINT,
      text: on ? 'The server reads each Supercharger session from Tesla’s charging history, older ones first, and keeps its invoice (PDF). Its cost becomes the charge’s cost; an entered cost still wins.' : 'No more invoices are fetched. The costs and invoices already kept stay.',
      run: guarded((cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, invoices: on }))),
    });
    if (!(await after(r, on ? 'Supercharger invoices on.' : 'Supercharger invoices off.'))) sw.set(!on);
  }
  async function setPace(v, seg) {
    if (v === ov.poll) return;
    const r = await askPassword(ctx, {
      title: `Pace: ${PACE.find((x) => x[0] === v)[1]}`, yes: 'Change the pace', hint: HINT,
      text: 'How often the server reads each car from the Tesla API.',
      run: guarded((cred) => ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v }))),
    });
    if (!(await after(r, `Pace saved: ${PACE.find((x) => x[0] === v)[1].toLowerCase()}.`))) seg.set(ov.poll);
  }

  /* ---------- adding an account */
  function addTile() {
    let reg = 'global';
    const region = segmented({ label: 'Tesla region', name: 'tesla-region', value: reg, options: REGION, onchange: (v) => { reg = v; } });
    const voidTick = tick('End on Tesla’s /void/callback page instead (for a browser that refuses tesla:// addresses)');
    const choice = (ic, name, text, b) => h('div', { class: 'tesla-way' },
      h('span', { class: 'row-ic tone-coral', 'aria-hidden': 'true' }, icon(ic)),
      h('div', { class: 'tesla-way-text' }, h('p', { class: 'row-name' }, name), h('p', { class: 'row-sub' }, text), b));
    return tile({ w: 2, title: accounts.length ? 'Add another Tesla account' : 'Add a Tesla account', icon: 'plus' },
      region,
      choice('external', 'Sign in at Tesla', 'Opens Tesla’s own sign-in page in a new tab. Tesla’s page asks for the Tesla password, its two-step code and any captcha: this server never sees them. Then you paste the address Tesla’s page ends on.',
        h('div', { class: 'tesla-way-act' }, btn('Sign in at Tesla', { kind: 'primary', icon: 'external', onclick: (e) => signIn(e.currentTarget, reg, voidTick.input.checked ? 'void' : 'app') }), voidTick)),
      choice('key', 'Paste a refresh token', 'A Tesla refresh token from another tool. The server checks it with Tesla (one token refresh, one car list) before anything is kept, and never shows it again.',
        h('div', { class: 'tesla-way-act' }, btn('Paste a refresh token', { kind: 'soft', icon: 'key', onclick: () => addByToken(reg) }))),
      note('A new account’s cars are not read until you match each one to a car here, to a new car, or leave it out.'));
  }

  /** A sign-in for this region and redirect: the one still open from an earlier press, else a new one. */
  async function startSignIn(region, redirect, { fresh = false } = {}) {
    const key = `${region}|${redirect}`;
    const open = openSignIns.get(key);
    if (!fresh && open && open.expires_ms - ctx.api.now() > 120000) return open;
    openSignIns.delete(key);
    const s = await ctx.api.post(`${BASE}/sign-in`, { region, redirect });
    openSignIns.set(key, s);
    return s;
  }

  async function signIn(b, region, redirect) {
    const key = `${region}|${redirect}`;
    b.disabled = true;
    let s;
    try { s = await startSignIn(region, redirect); } catch (e) {
      if (e?.name !== 'AbortError') ctx.toast(`Could not start the sign-in: ${words(e).message || errorWords(e).text}`, { kind: 'error' });
      return;
    } finally { b.disabled = false; }
    if (!ctx.alive()) return;
    const host = region === 'china' ? 'auth.tesla.cn' : 'auth.tesla.com';
    const endsOn = redirect === 'void' ? `https://${host}/void/callback` : 'tesla://auth/callback';
    const label = field({ label: 'Name', name: 'label', value: 'Tesla account', hint: 'Your own name for it, shown on this page. Not the account’s e-mail.', attrs: { maxlength: '64', autocomplete: 'off', required: true } });
    const pasted = field({ label: 'The address Tesla’s page ended on', name: 'pasted_url', type: 'textarea', hint: `The whole address, starting ${endsOn}`, attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3', required: true, placeholder: `${endsOn}?code=…` } });
    const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    const openLink = h('a', { class: 'btn btn-primary', href: s.url, target: '_blank', rel: 'noopener noreferrer external' }, icon('external'), h('span', { class: 'btn-text' }, 'Open Tesla’s sign-in page'));
    const lasts = h('span', null);
    const showLasts = () => { lasts.textContent = `This sign-in lasts until ${formatTime(s.expires_ms, localZone)} on this device’s clock (ten minutes from its start), and works once.`; };
    showLasts();
    const steps = h('div', { class: 'tesla-steps' },
      h('ol', { class: 'set-list' },
        h('li', null, h('b', null, 'Open Tesla’s page'), ' and sign in there. Tesla’s own page asks for your Tesla password (and its code); this server never sees them.'),
        h('li', null, h('b', null, 'The last page will not load.'), ' That is expected: Tesla ends on an address that starts ', mono(endsOn), redirect === 'void' ? ', a blank page.' : ', which a browser cannot open. It may show an error or seem to do nothing; the address is still in the address bar.'),
        h('li', null, h('b', null, 'Copy that whole address'), ' and paste it below, with your own Sentry Mate password.')),
      openLink,
      said('warn', lasts));
    // Tesla refused or could not be reached on a pasted address: that sign-in is used up, so a new one is
    // started here and the sheet stays open for it
    async function startAgain(why) {
      openSignIns.delete(key);
      try { s = await startSignIn(region, redirect, { fresh: true }); } catch (e2) {
        throw new Error(`${why} A new sign-in could not be started (${words(e2).message || errorWords(e2).text}): close this and press Sign in at Tesla again later.`);
      }
      openLink.href = s.url;
      showLasts();
      const ta = pasted.querySelector('textarea');
      if (ta) ta.value = '';
      const again = `${why} A new sign-in is ready: open Tesla’s page again with the button above, sign in, and paste the new address here.`;
      throw Object.assign(new Error(again), { fields: { pasted_url: again } });
    }
    const r = await askPassword(ctx, {
      title: 'Sign in at Tesla', text: steps, yes: 'Add the account', extra: [label, pasted], hint: HINT,
      run: async (cred, v) => {
        if (!String(v.pasted_url || '').trim()) throw Object.assign(new Error('Paste the address first.'), { fields: { pasted_url: `Paste the whole address, starting ${endsOn}.` } });
        try {
          const a = await ctx.api.post(`${BASE}/sign-in/${enc(s.sign_in)}`, withPw(cred, { pasted_url: v.pasted_url.trim(), label: String(v.label || '').trim() || undefined }));
          openSignIns.delete(key);
          return a;
        } catch (e) {
          if (e?.code === 'tesla_refused') return startAgain(ERROR_WORDS.signin_refused);
          if (e?.code === 'tesla_unreachable') return startAgain(ERROR_WORDS.signin_unreachable);
          if (e?.code === 'no_such_sign_in') return startAgain('This sign-in ran out (it lasts ten minutes) or was used already.');
          throw words(e);
        }
      },
    });
    if (r) await reload(`${r.label || 'The account'} added. Match its cars below to start reading them.`);
  }

  async function addByToken(region) {
    const label = field({ label: 'Name', name: 'label', value: 'Tesla account', hint: 'Your own name for it. Not the account’s e-mail.', attrs: { maxlength: '64', autocomplete: 'off', required: true } });
    const reg = field({ label: 'Region', name: 'region', type: 'select', value: region, options: REGION });
    const tok = field({ label: 'Refresh token', name: 'refresh_token', type: 'textarea', hint: 'Checked with Tesla before it is kept. It is sealed on the server and never shown again.', attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3', required: true } });
    const r = await askPassword(ctx, {
      title: 'Add by refresh token', yes: 'Check with Tesla and add', extra: [label, reg, tok], hint: HINT,
      text: 'Only paste a token of your own Tesla account. Anyone with it can read the account’s cars, so it never leaves this server and no page shows it.',
      run: async (cred, v) => {
        const bad = {};
        if (!String(v.label || '').trim()) bad.label = 'Give it a name.';
        if (!String(v.refresh_token || '').trim()) bad.refresh_token = 'Paste the token.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Not added yet.'), { fields: bad });
        try { return await ctx.api.post(`${BASE}/accounts`, withPw(cred, { label: v.label.trim(), region: v.region, refresh_token: v.refresh_token.trim() })); } catch (e) { throw words(e); }
      },
    });
    await after(r, (x) => `${x.label || 'The account'} added. Match its cars below to start reading them.`);
  }

  /* ---------- one account */
  function accountTile(a) {
    const st = ACCOUNT_STATUS[a.status] || [a.status, 'muted', ''];
    const err = a.last_error;
    const patch = async ({ title, text = null, yes, body, extra = [], msg = 'Saved.', danger = false }) => {
      const r = await askPassword(ctx, {
        title, text, yes, extra, danger, hint: HINT,
        run: guarded((cred, v) => ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: a.version, ...body(v) }))),
      });
      await after(r, msg);
      return r;
    };
    const pause = btn(a.paused ? 'Resume' : 'Pause', { kind: a.paused ? 'primary' : 'soft', icon: a.paused ? 'refresh' : 'minus', onclick: () => patch({
      title: a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`, yes: a.paused ? 'Resume' : 'Pause',
      text: a.paused ? 'Its matched cars are read again from now on.' : 'Its cars are not read until you resume it. Nothing is asked of Tesla meanwhile; the history already stored stays.',
      body: () => ({ paused: !a.paused }), msg: a.paused ? `${a.label} resumed.` : `${a.label} paused.` }) });
    const token = btn('Replace the token', { kind: a.status === 'sign_in_needed' ? 'primary' : 'soft', icon: 'key', onclick: () => {
      const f = field({ label: 'New refresh token', name: 'refresh_token', type: 'textarea', hint: 'Checked with Tesla before it replaces the old one.', attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3', required: true } });
      return patch({ title: `New token for ${a.label}`, yes: 'Check with Tesla and replace', extra: [f],
        text: 'For an account Tesla stopped taking, or a token you renewed elsewhere. The old token is deleted.',
        body: (v) => ({ refresh_token: String(v.refresh_token || '').trim() }), msg: 'Token replaced and checked with Tesla.' });
    } });
    const rename = btn('Rename', { kind: 'soft', icon: 'edit', onclick: () => {
      const f = field({ label: 'Name', name: 'label', value: a.label, hint: 'Your own name for it. Not the account’s e-mail.', attrs: { maxlength: '64', autocomplete: 'off', required: true } });
      return patch({ title: `Rename ${a.label}`, yes: 'Rename', extra: [f], body: (v) => ({ label: String(v.label || '').trim() }), msg: 'Renamed.' });
    } });
    const remove = btn('Remove', { kind: 'danger', icon: 'trash', onclick: async () => {
      const r = await askPassword(ctx, {
        title: `Remove ${a.label}?`, danger: true, yes: 'Remove the account', hint: HINT,
        text: 'Reading stops at once and the sealed file with its tokens is deleted. The history already stored from it stays, marked as data from the Tesla API.',
        // a 204 answers null: true stands for done
        run: guarded(async (cred) => (await ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred))) ?? true),
      });
      await after(r, `${a.label} removed. Its history stays.`);
    } });
    const loc = pickRow({
      name: 'Ask for the position', sub: 'Shows an icon in the car',
      value: a.location, options: LOCATION, sheetTitle: 'Ask for the position', sheetNote: 'While the position is asked for, the car shows an icon on its screen that its location is shared.',
      onpick: async (v) => {
        const l = LOCATION.find((x) => x[0] === v);
        const r = await patch({ title: `Position: ${l[1].toLowerCase()}?`, yes: 'Save', text: `${l[2]} While the position is asked for, the car shows an icon on its screen that its location is shared.`, body: () => ({ location: v }), msg: 'When the position is asked for: saved.' });
        if (!r) throw new Error('not saved');
      },
    });
    const sevBad = a.status === 'sign_in_needed' || a.status === 'error';
    return tile({ w: 2, title: a.label, icon: 'person', id: `tesla-${a.id}`, cls: 'tesla-acc', side: chip(st[0], { tone: st[1] }) },
      st[2] ? said(sevBad ? 'warn' : a.status === 'ok' ? 'ok' : 'info', st[2]) : null,
      facts([
        ['Status', st[0]],
        ['Region', a.region === 'china' ? 'China (tesla.cn)' : 'Global (tesla.com)'],
        ['Added', formatDateTime(a.added_ms, ctx.tz, { now: ctx.api.now() })],
        ['Last good answer', a.last_ok_ms ? ago(a.last_ok_ms) : 'None yet'],
        ['Last error', err ? h('span', { class: 'tesla-err' }, cap1(lastErrorText(err)), err.message && LAST_ERROR_WORDS[err.code] ? h('span', { class: 'tile-line' }, err.message) : null,
          err.at_ms ? h('span', { class: 'tile-line' }, formatDateTime(err.at_ms, ctx.tz, { now: ctx.api.now() })) : null) : 'None'],
        ['Requests today', `${formatNumber(a.polls_today ?? 0)} of 3,000`],
        ['Tesla hosts', h('span', { class: 'tesla-hosts' }, (a.hosts || []).map((x) => mono(x)))],
      ], { cls: 'tesla-facts' }),
      group(null, [loc], { note: 'While the position is asked for, the car shows an icon on its screen that its location is shared.' }),
      h('div', { class: 'form-actions tesla-acts' }, pause, token, rename, remove));
  }

  /* ---------- an account's cars: match each, and what the server does for it now */
  function carsTile(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    // cars here another Tesla account reads already: the server refuses them (car_taken)
    const takenBy = new Map();
    for (const o of accounts) {
      if (o.id === a.id) continue;
      for (const c of (carRows[o.id] && carRows[o.id].items) || []) if (c.match && c.match !== 'new' && c.match !== 'leave_out') takenBy.set(c.match, o.label);
    }
    const isCar = (v) => !!v && v !== 'new' && v !== 'leave_out';
    const nameOf = (c) => c.display_name || c.vehicle;
    // a suggestion another account reads already is not taken over: it starts at Not decided
    const picked = new Map(items.map((c) => [c.vehicle, c.match ?? (takenBy.has(c.suggested) ? '' : c.suggested) ?? '']));
    const changed = () => items.filter((c) => (picked.get(c.vehicle) || null) !== (c.match ?? null));
    // one car here takes one Tesla car: two cars of this account on one car here would both read into it
    const doubled = () => {
      const by = new Map();
      for (const c of items) { const v = picked.get(c.vehicle); if (isCar(v)) by.set(v, [...(by.get(v) || []), c]); }
      return [...by].filter(([, list]) => list.length > 1);
    };
    const optsFor = (c) => [['', 'Not decided', 'Not read until you choose'],
      ...cars.map((x) => {
        const other = items.find((o) => o.vehicle !== c.vehicle && picked.get(o.vehicle) === x.id);
        return [x.id, x.name || x.id, takenBy.has(x.id) ? `Read through ${takenBy.get(x.id)} already: leave it out there first`
          : other ? `Picked for the Tesla car ${nameOf(other)} already: one car here takes one Tesla car` : 'Read into this car', state.isSold(x)];
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
      problem.replaceChildren(...(dup.length ? [said('warn', dup.map(([car, list]) => `${list.map(nameOf).join(' and ')} are both matched to ${carName(car)}.`).join(' '), ' One car here takes one Tesla car: pick another car, A new car, or Leave out.')] : []));
      problem.hidden = !dup.length;
      save.hidden = !n;
      save.disabled = !!dup.length;
      pending.textContent = n ? `${n} ${n === 1 ? 'change' : 'changes'} not saved yet.` : '';
    };
    const statusOf = (c) => status.find((s) => s.account === a.id && s.vehicle === c.vehicle) || {};
    const zoneOf = () => ctx.tz;
    const modeText = (c) => {
      const m = CAR_MODE[c.mode] || [c.mode || 'Not known', ''];
      const sv = statusOf(c);
      const next = sv.next_poll_ms ?? null;
      const bits = [m[0], sv.streaming ? 'live stream' : null];
      if (c.mode === 'backoff' && next) bits.push(`next try ${formatTime(next, zoneOf(c))} (in ${formatDuration(Math.max(0, next - ctx.api.now()))})`);
      else if (next && c.mode !== 'off') bits.push(`next read ${formatTime(next, zoneOf(c))}`);
      if (sv.last_data_ms) bits.push(['last data ', ago(sv.last_data_ms)]);
      return bits.filter(Boolean).map((b, i) => (i ? [', ', b] : b));
    };
    const rows = items.map((c) => {
      const sugg = c.match == null && c.suggested;
      const known = optsFor(c);
      if (!known.some(([v]) => v === picked.get(c.vehicle))) known.push([picked.get(c.vehicle), carName(picked.get(c.vehicle))]);
      optLists.set(c.vehicle, known);
      const m = CAR_MODE[c.mode] || ['', ''];
      return h('div', { class: 'tesla-car' },
        pickRow({
          name: c.display_name || 'No name at Tesla',
          sub: [[c.year, c.model].filter(Boolean).join(' '), c.vin_prefix ? ` · ${c.vin_prefix}` : '', ` · Tesla says ${CAR_STATE[c.state] || c.state || 'not known'}`, sugg && !takenBy.has(c.suggested) ? ` · Suggested: ${carName(c.suggested)}, same VIN prefix` : ''],
          value: picked.get(c.vehicle), options: known, sheetTitle: `Car for ${c.display_name || c.vehicle}`,
          sheetNote: 'A car is read only once it is matched to a car here or to a new car. Saved with Save the matches.',
          onpick: (v) => { picked.set(c.vehicle, v); sync(); },
        }),
        h('p', { class: ['tesla-mode', { 'is-warn': c.mode === 'backoff' }], title: m[1] || null },
          h('span', { class: ['src-dot', c.mode === 'off' ? 'tone-muted' : c.mode === 'backoff' ? 'tone-sun' : 'tone-sky'], 'aria-hidden': 'true' }),
          h('span', null, modeText(c))));
    });
    async function saveMatches() {
      const ch = changed();
      if (!ch.length || doubled().length) return;
      // the admin's session alone: no password asked again (a match stores and shows no token)
      save.disabled = true;
      let r = null;
      try { r = await guarded(() => ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, { version: data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: picked.get(c.vehicle) || null })) }))(); } catch (e) { if (e?.name !== 'AbortError') ctx.toast(errorWords(e).text, { kind: 'error' }); } finally { save.disabled = false; }
      await after(r, 'Matches saved.');
    }
    sync();
    return tile({ w: 2, title: `Cars of ${a.label}`, icon: 'car', side: chip(`${formatNumber(items.length)} at Tesla`, { tone: 'muted' }) },
      items.length ? h('div', { class: 'rows' }, h('div', { class: 'rows-list' }, rows))
        : said('info', 'Tesla lists no car for this account. A car added to it at Tesla shows here within a few minutes.'),
      problem,
      note('Pick the car each one is read into. A suggestion has the same VIN prefix as a car here; it is not saved until you press Save the matches.'),
      h('div', { class: 'form-actions' }, save, pending));
  }

  return { el, load };
}

/** Facts rows for one car's Tesla input (Data health), or null when no Tesla car reads it. */
export function teslaCarFacts(ctx, statusItems, carId) {
  const s = (statusItems || []).find((x) => x.car === carId);
  if (!s) return null;
  const m = CAR_MODE[s.mode] || [s.mode, ''];
  const now = ctx.api.now();
  return [
    ['What the server does', `${m[0]}${s.streaming ? ', live stream' : ''} (Tesla says ${CAR_STATE[s.car_state] || s.car_state || 'not known'})`],
    ['Last Tesla data', s.last_data_ms ? formatDateTime(s.last_data_ms, ctx.tz, { now }) : 'None yet'],
    [s.mode === 'backoff' ? 'Next try' : 'Next read', s.next_poll_ms ? `${formatTime(s.next_poll_ms, ctx.tz)}${s.mode === 'backoff' ? ', waiting after errors' : ''}` : 'Not planned'],
    ['Tesla requests today', formatNumber(s.polls_today ?? 0)],
    s.last_error ? ['Tesla error', cap1(lastErrorText(s.last_error))] : null,
    ['Account', h('a', { class: 'place-link', href: teslaAccountHref(s.account) }, 'Open its Tesla account')],
  ].filter(Boolean);
}
