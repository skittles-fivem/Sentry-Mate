// settings/integrations, the tesla account block (#tesla): the Tesla API input. This server can sign in to a
// Tesla account and read its cars from Tesla's Owner API, as a second live input beside SMO. It is off until an
// admin adds an account; before that the server makes no connection to Tesla at all. It only reads: it never
// wakes a car or sends it a command, and no answer ever holds a Tesla token.
//
// Admins only, reading included. Every change except starting a sign-in and saving the car matches asks for the
// admin's own password again (and the two-step code when the account has it): askPassword() from core/secret.js,
// sent as confirm_password and code. The car matches take the admin's session alone (they store and show no token).
// Each account block has the id tesla-<account> so a notice can link to it.
//
// API: GET/PATCH integrations/tesla, GET/POST integrations/tesla/accounts, POST integrations/tesla/sign-in,
// POST integrations/tesla/sign-in/{sign_in}, PATCH/DELETE integrations/tesla/accounts/{id},
// GET/PUT integrations/tesla/accounts/{id}/cars, GET integrations/tesla/status.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { section, kv, btn, note, link } from '../core/ui.js';
import { table } from '../core/table.js';
import { pairOptions } from '../core/carpick.js';
import { field } from '../core/form.js';
import { errorWords } from '../core/states.js';
import { tag, dateTime, time, ago, dur, DASH } from '../core/fmt.js';
import { toast } from '../core/toast.js';
import { askPassword } from '../core/secret.js';
import { invoicesBlock, invoicesSlot, loadUnmatched } from './invoices-block.js';

const enc = encodeURIComponent;
const BASE = 'integrations/tesla';

/* ---------------------------------------------------------------------- words */

// account status: [tag word, tone, what it means]
export const ACCOUNT_STATUS = {
  checking: ['checking', 'muted', 'Just added: the server is asking Tesla for its cars.'],
  ok: ['reading', 'ok', 'Tesla takes its sign-in; its matched cars are read.'],
  paused: ['paused', 'muted', 'Its cars are not read until you resume it. Nothing is asked of Tesla meanwhile.'],
  sign_in_needed: ['sign in again', 'warn', 'Tesla stopped taking its sign-in, so its cars are not read. Replace its token, or remove it and sign in at Tesla again. Nothing is asked of Tesla meanwhile.'],
  error: ['error', 'bad', 'Tesla refuses it, or answers with errors. See the last error.'],
};

// what the server does for a car: [words, why]
export const CAR_MODE = {
  off: ['not read', 'Not matched, left out, or its account is paused.'],
  watching_list: ['watching the car list', 'Asleep or offline: only the list of cars is read, so the car is never woken.'],
  online_idle: ['awake, idle', 'Awake with nothing going on: read at the parked pace.'],
  letting_sleep: ['letting it sleep', 'Awake and quiet for 10 minutes: left alone for 21 minutes so it can fall asleep.'],
  charging: ['charging', 'Read at the charging pace.'],
  driving: ['driving', 'Read at the driving pace.'],
  backoff: ['waiting after errors', 'Tesla answered with errors, or the day\'s 3,000 requests are used up: nothing is asked until the next try. After a server error it waits longer each time, up to 15 minutes; after a refusal (403) an hour.'],
};
const CAR_STATE = { online: 'online', asleep: 'asleep', offline: 'offline', unknown: 'not known' };
const LOCATION = [
  ['driving_or_charging', 'driving or charging', 'The position is asked for only while the car drives or charges.'],
  ['always', 'always', 'Also while parked and awake.'],
  ['never', 'never', 'No position from the Tesla API at all: its drives have no route and its charges no place.'],
];
const PACE = [['gentle', 'gentle'], ['normal', 'normal'], ['often', 'often']];
const REGION = [['global', 'global (tesla.com)'], ['china', 'china (tesla.cn)']];

// error codes of a change, in words
const ERROR_WORDS = {
  tesla_refused: 'Tesla said no to this sign-in or token: it may have run out, been used already, or the Tesla password changed since. Sign in at Tesla again, or paste a new refresh token.',
  tesla_unreachable: 'Tesla could not be reached, or answered with an error. Nothing was kept. Try again in a few minutes.',
  account_exists: 'The cars of this Tesla account are already read through another Tesla account here. Replace that account\'s token instead, or remove it first.',
  car_taken: 'One of these cars is already matched to a vehicle of another Tesla account here. Leave it out there first, or pick another car.',
  too_many_sign_ins: 'Five sign-ins are open already. Finish one, or wait up to ten minutes for them to run out.',
  no_such_sign_in: 'This sign-in ran out (it lasts ten minutes) or was used already. Close this and start a new one.',
  no_such_tesla_account: 'This Tesla account is not here any more: another admin may have removed it. The page shows the accounts there are now.',
  version_conflict: 'Someone else changed this Tesla account a moment ago. The page shows their change now; make yours again.',
};
// last_error codes of an account, a car or the whole input
const LAST_ERROR_WORDS = {
  sign_in_needed: 'Tesla stopped taking the sign-in',
  tesla_refused: 'Tesla refused the token',
  tesla_unreachable: 'Tesla could not be reached',
  endpoint_gone: 'Tesla closed the address the server reads; the server stops asking it',
  rate_limited: 'Tesla asked the server to slow down',
  daily_cap: 'the day\'s 3,000 requests are used up',
  forbidden: 'Tesla refused the request (403)',
  server_error: 'Tesla answered with a server error',
  timeout: 'Tesla took too long to answer',
};
export const lastErrorText = (e) => (e ? (LAST_ERROR_WORDS[e.code] || e.message || e.code) : null);

/** Our words on an error of a change. Mutates and returns it; the password dialog handles its own codes. */
function words(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  if (ERROR_WORDS[e.code]) e.message = ERROR_WORDS[e.code];
  else if (e.status === 409) e.message = ERROR_WORDS.version_conflict;
  if (e.fields) {
    delete e.fields.confirm_password; delete e.fields.code;
    if (e.fields.pasted_url && e.code === 'invalid') {
      const t = String(e.fields.pasted_url).trim().replace(/[.\s]+$/, '');
      // an address of another sign-in needs a new sign-in (the dialog offers one), not a better paste
      e.fields.pasted_url = /sign-in/i.test(t) ? `${t}.` : `${t}. Paste the whole address from the address bar, starting tesla:// (or https://auth.tesla…/void/callback).`;
    }
    if (!Object.keys(e.fields).length) e.fields = null;
  }
  return e;
}
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const code = (t) => h('span', { class: 'prompt' }, t);
const secs = (s) => (s == null ? DASH : s >= 120 && s % 60 === 0 ? `${formatNumber(s / 60)} min` : `${formatNumber(s)} s`);
const lines = (rows) => h('ul', { class: 'ln-list' }, rows.filter(Boolean).map(([t, text]) => h('li', null, t, h('span', { class: 'ln-list-t' }, text))));

export const teslaAccountHref = (id) => `${state.buildUrl('settings-integrations')}#tesla-${enc(id)}`;

/* ---------------------------------------------------------------------- the block */

/**
 * The tesla account block. cars: Sentry Mate's cars ({ id, name, time_zone }) for the match picker.
 * Returns { el, side, load() }: load reads everything and paints (the page awaits it before ready()).
 */
export function teslaSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const side = h('span');
  const body = h('div', { 'aria-live': 'polite' });
  const el = section({ title: 'tesla account', id: 'tesla', side },
    note('A second live input beside SMO: this server can sign in to a Tesla account and read its cars from Tesla\'s Owner API (the one Tesla\'s own app uses). It only reads: it never wakes a car or sends it a command. Off until an admin adds an account; before that the server makes no connection to Tesla at all. Every page then says whether its data came from SMO, the Tesla API, or both.'),
    body);

  let ov = null, accounts = [], status = [], carRows = {}, unmatched = null;
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;
  const tables = [];

  async function load() {
    if (!admin) {
      replace(side, tag('admins only', 'muted'));
      replace(body, note(tag('admin', 'muted'), ' Only an admin sees and changes the Tesla account: its status, its cars and how often Tesla is asked. Every page still says where its data came from (SMO, the Tesla API or an import).'), invoicesSlot('tesla'));
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
      replace(body, h('p', { class: 'note tone--bad' }, 'The Tesla account could not be read: ', errorWords(e).text), invoicesSlot('tesla'));
      return;
    }
    if (!ctx.alive()) return;
    unmatched = await loadUnmatched(ctx, 'tesla', ov);
    if (!ctx.alive()) return;
    paint();
    await Promise.all(tables.map((t) => t.ready)).catch(() => {});
  }
  // the load in flight: a change waits for it before it puts the focus back
  let pending = null;
  const refresh = () => (pending = load().catch(() => {}));
  const reload = async (msg) => { if (msg) toast(msg); await refresh(); };

  // A change repaints the block, so the button that opened its dialog is gone when the dialog closes: the focus
  // goes to the button with the same data-fk in the new paint, else to the first of the fallbacks there is.
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
  // run fn (it opens a dialog and may reload), then put the focus back on keys
  const keep = (keys, fn) => async (...args) => {
    try { return await fn(...args); } finally { await pending; if (ctx.alive()) refocus([...keys, 'tesla-top']); }
  };
  // a change refused because the page is behind: read everything again behind the dialog, so the page shows
  // what is there now and a second try sends the new version
  const stale = (e) => { if (e && (e.code === 'version_conflict' || e.code === 'no_such_tesla_account' || (e.status === 409 && !ERROR_WORDS[e.code]))) refresh(); return e; };
  const fail = (e) => stale(words(e));

  function paint() {
    tables.length = 0;
    replace(side, !ov.enabled ? tag('off until an account is added', 'muted')
      : ov.cars ? tag(`reading ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'}`, 'ok') : tag('no car matched yet', 'warn'));
    replace(body, statusBlock(), invoicesBlock(ctx, { source: 'tesla', ov, unmatched, fk, setOn: (on) => keep([`inv-tesla:${on ? 'on' : 'off'}`], () => setInvoices(on))() }), addBlock(), accounts.map(accountBlock));
  }

  /* ---------- the input as a whole */
  function statusBlock() {
    const p = ov.polls || {};
    const cap = (p.daily_cap || 3000) * Math.max(1, ov.accounts || 0);
    const pace = h('div', { class: 'keys', role: 'group', 'aria-label': 'How often Tesla is asked' },
      PACE.map(([v, l]) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(ov.poll === v), dataset: { fk: `pace:${v}` }, onclick: keep([`pace:${v}`], () => setPace(v)) }, l)));
    return h('div', null,
      h('p', { class: 'sub-title', tabindex: '-1', dataset: { fk: 'tesla-top' } }, 'status'),
      kv([
        ['state', ov.enabled ? [tag('on', 'ok'), ` ${formatNumber(ov.accounts)} ${ov.accounts === 1 ? 'account' : 'accounts'}, ${formatNumber(ov.cars)} ${ov.cars === 1 ? 'car' : 'cars'} read`]
          : [tag('off', 'muted'), ' off until an account is added: no connection to Tesla is made']],
        ['requests today', [formatNumber(ov.polls_today ?? 0), h('span', { class: 'muted' }, ov.enabled ? ` of at most ${formatNumber(cap)} (3,000 a day per account)` : ' none: there is no account')]],
        ['connections', [formatNumber(ov.clients_built ?? 0), h('span', { class: 'muted' }, ' set up to Tesla since the server started')]],
        ['last error', ov.last_error ? [lastErrorText(ov.last_error), ov.last_error.at_ms ? h('span', { class: 'muted' }, ` ${dateTime(ov.last_error.at_ms, ctx.tz)} (${ago(ov.last_error.at_ms)})`) : null] : h('span', { class: 'muted' }, 'none'), { tone: ov.last_error ? 'warn' : null }],
        ['tesla hosts', ov.hosts?.length ? h('span', { class: 'tesla-hosts' }, ov.hosts.map((x) => code(x)))
          : h('span', { class: 'muted' }, 'none while there is no account. Once one is added: Tesla\'s sign-in host, its Owner API and (outside China) its live stream, all over https.')],
      ], { cols: 1 }),
      h('p', { class: 'sub-title' }, 'how often tesla is asked'),
      pace,
      kv([
        ['parked, awake', `every ${secs(p.parked_s)}`],
        ['charging', `every ${secs(p.charging_s)}`],
        ['driving', `every ${secs(p.driving_s)}`],
        ['driving, streamed', [`every ${secs(p.driving_streaming_s)}`, h('span', { class: 'muted' }, ' with Tesla\'s live stream')]],
        ['car list', [`every ${secs(p.list_s)}`, h('span', { class: 'muted' }, ' all an asleep car gets')]],
      ]),
      note('In every pace an asleep car is never woken (only the list of cars is read), and an awake car with nothing going on is left alone so it can fall asleep. At most one request a second goes to Tesla per account.'));
  }
  async function setInvoices(on) {
    const r = await askPassword(ctx, {
      title: `supercharger invoices: ${on ? 'on' : 'off'}`, submit: on ? 'turn invoices on' : 'turn invoices off',
      intro: note(on ? 'The server reads each Supercharger session from Tesla\'s charging history (older ones first, then new ones as they come) and keeps its invoice (PDF). Its cost becomes the charge\'s cost; a typed cost still wins.' : 'No more invoices are fetched. The costs and invoices already kept stay.'),
      run: async (cred) => { try { return await ctx.api.patch(BASE, withPw(cred, { version: ov.version, invoices: on })); } catch (e) { throw fail(e); } },
    });
    if (r) await reload(on ? 'Supercharger invoices on.' : 'Supercharger invoices off.');
  }
  async function setPace(v) {
    if (v === ov.poll) return;
    const r = await askPassword(ctx, {
      title: `pace: ${v}`, submit: 'change the pace',
      intro: note('How often the server reads each car from the Tesla API. A faster pace gives finer drives and charges from the API, and uses more of the 3,000 requests a day.'),
      run: async (cred) => { try { return await ctx.api.patch(BASE, withPw(cred, { version: ov.version, poll: v })); } catch (e) { throw fail(e); } },
    });
    if (r) await reload(`Pace saved: ${v}.`);
  }

  /* ---------- adding an account */
  function addBlock() {
    const region = h('div', { class: 'keys', role: 'group', 'aria-label': 'Tesla region' });
    let reg = 'global';
    const paintRegion = () => replace(region, REGION.map(([v, l]) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(reg === v), onclick: () => { reg = v; paintRegion(); } }, l)));
    paintRegion();
    const voidBox = h('input', { class: 'chk', type: 'checkbox', id: 'tesla-void' });
    return h('div', null,
      h('p', { class: 'sub-title' }, accounts.length ? 'add another account' : 'add a tesla account'),
      h('div', { class: 'carline' }, h('span', { class: 'muted' }, 'region'), region),
      lines([
        [tag('sign in', 'accent'), [h('b', null, 'Sign in at Tesla. '), 'Opens Tesla\'s own sign-in page. Tesla\'s page asks for the Tesla password, the two-step code and any captcha: this server never sees them. Then you paste the address Tesla ends on.',
          h('label', { class: 'fld-checkline tesla-void', for: 'tesla-void' }, voidBox, h('span', null, 'end on Tesla\'s /void/callback page instead (for a browser that refuses tesla:// addresses)')),
          h('div', { class: 'acts' }, fk(btn('sign in at tesla', (e) => { const b = e.currentTarget; return keep(['signin'], () => signIn(b, reg, voidBox.checked ? 'void' : 'app'))(); }, { kind: 'primary' }), 'signin'))]],
        [tag('token', 'muted'), [h('b', null, 'Paste a refresh token. '), 'A Tesla refresh token from another tool. The server checks it with Tesla (one token refresh, one car list) before anything is kept, and never shows it again.',
          h('div', { class: 'acts' }, fk(btn('paste a refresh token', keep(['addtoken'], () => addByToken(reg))), 'addtoken'))]],
      ]),
      note('A new account\'s cars are not read until you match each one to a car here, to a new car, or leave it out.'));
  }

  const startSignIn = (region, redirect) => ctx.api.post(`${BASE}/sign-in`, { region, redirect });
  const startError = (e) => `Could not start the sign-in: ${words(e).message || errorWords(e).text}`;

  async function signIn(b, region, redirect) {
    b.disabled = true;
    let s;
    try { s = await startSignIn(region, redirect); } catch (e) {
      if (e?.name !== 'AbortError') toast(startError(e), { kind: 'error' });
      return;
    } finally { b.disabled = false; }
    if (!ctx.alive()) return;
    const host = region === 'china' ? 'auth.tesla.cn' : 'auth.tesla.com';
    const endsOn = redirect === 'void' ? `https://${host}/void/callback` : 'tesla://auth/callback';
    const label = field({ label: 'name', name: 'label', required: true, value: 'Tesla account', hint: 'Your own name for it, shown on this page. Not the account\'s e-mail.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const pasted = field({ label: 'address', name: 'pasted_url', path: 'pasted_url', type: 'textarea', required: true,
      hint: `The whole address Tesla ended on, starting ${endsOn}`, attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3', placeholder: `${endsOn}?code=...` } });
    // A sign-in works once: after Tesla was asked with its code (Tesla refused it, or could not be reached) or
    // once it ran out, only a new one goes on. The dialog starts it in place, with a new link to Tesla's page.
    let used = false, frm = null;
    const open = h('a', { class: 'btn btn--primary', href: s.url, target: '_blank', rel: 'noopener noreferrer external' }, 'open tesla\'s sign-in page');
    const again = btn('start a new sign-in', () => restart());
    again.hidden = true;
    const said = h('p', { class: 'note', role: 'status', hidden: true });
    const until = h('span');
    const paintUntil = () => replace(until, ` This sign-in lasts ten minutes, until ${time(s.expires_ms, Intl.DateTimeFormat().resolvedOptions().timeZone)} on this computer's clock, and works once.`);
    paintUntil();
    const dead = (text) => {
      used = true; again.hidden = false; open.hidden = true;
      replace(said, tag('used up', 'warn'), ` ${text}`); said.hidden = false;
    };
    async function restart() {
      again.disabled = true;
      try { s = await startSignIn(region, redirect); } catch (e) {
        if (e?.name !== 'AbortError') frm?.setMessage(startError(e));
        return;
      } finally { again.disabled = false; }
      if (!ctx.alive()) return;
      used = false; open.href = s.url; open.hidden = false; again.hidden = true;
      paintUntil();
      pasted.input.value = ''; pasted.setError(null); frm?.setMessage(null);
      replace(said, tag('new', 'accent'), ' A new sign-in is open: open Tesla\'s page again, sign in, and paste the address it ends on.'); said.hidden = false;
      open.focus();
    }
    const intro = h('div', { class: 'hc' },
      h('ol', { class: 'tesla-steps' },
        h('li', null, h('span', null, h('b', null, 'Open Tesla\'s page'), ' and sign in there. Tesla\'s own page asks for your Tesla password (and its code); this server never sees them. ',
          h('span', { class: 'acts' }, open, again))),
        h('li', null, h('span', null, h('b', null, 'The last page will not load.'), ' That is expected: Tesla ends on an address that starts ', code(endsOn), redirect === 'void' ? ', a blank page.' : ', which a browser cannot open. It may show an error or seem to do nothing; the address is still in the address bar.')),
        h('li', null, h('span', null, h('b', null, 'Copy that whole address'), ' and paste it below, with your own Sentry Mate password.'))),
      said,
      h('p', { class: 'note' }, tag('10 min', 'warn'), until));
    const USED_UP = {
      tesla_unreachable: 'Tesla could not be reached, or answered with an error. Nothing was kept, and this sign-in is used up: press start a new sign-in, open Tesla\'s page again and paste the new address.',
      tesla_refused: 'Tesla said no to this sign-in: its code may have run out or been used already. Nothing was kept, and this sign-in is used up: press start a new sign-in and sign in at Tesla again.',
      no_such_sign_in: 'This sign-in ran out (it lasts ten minutes) or was used already. Press start a new sign-in, open Tesla\'s page again and paste the new address.',
    };
    const r = await askPassword(ctx, {
      title: 'sign in at tesla', intro, submit: 'add the account', extra: [label, pasted], focus: open,
      onOpen: (d, f) => { frm = f; },
      run: async (cred, v) => {
        if (used) { again.focus(); throw new Error('This sign-in is used up. Press start a new sign-in first, then open Tesla\'s page again.'); }
        try { return await ctx.api.post(`${BASE}/sign-in/${enc(s.sign_in)}`, withPw(cred, { pasted_url: v.pasted_url.trim(), label: v.label.trim() })); } catch (e) {
          words(e);
          if (USED_UP[e.code]) { e.message = USED_UP[e.code]; dead(e.code === 'no_such_sign_in' ? 'This sign-in ran out or was used.' : 'Tesla was asked with this sign-in\'s code, so it works no more.'); }
          else if (e.code === 'invalid' && /sign-in/i.test(e.fields?.pasted_url || '')) { again.hidden = false; }
          throw e;
        }
      },
    });
    if (r) await reload(`${r.label || 'The account'} added. Match its cars below to start reading them.`);
  }

  async function addByToken(region) {
    const label = field({ label: 'name', name: 'label', required: true, value: 'Tesla account', hint: 'Your own name for it. Not the account\'s e-mail.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const reg = field({ label: 'region', name: 'region', type: 'select', value: region, options: REGION });
    const tok = field({ label: 'token', name: 'refresh_token', path: 'refresh_token', type: 'textarea', required: true, hint: 'Checked with Tesla before it is kept. It is sealed on the server and never shown again.', attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3' } });
    const r = await askPassword(ctx, {
      title: 'add a tesla account by refresh token', submit: 'check with tesla and add', extra: [label, reg, tok],
      intro: note('Only paste a token of your own Tesla account. Anyone with it can read the account\'s cars, so it never leaves this server and no page shows it.'),
      run: async (cred, v) => { try { return await ctx.api.post(`${BASE}/accounts`, withPw(cred, { label: v.label.trim(), region: v.region, refresh_token: v.refresh_token.trim() })); } catch (e) { throw fail(e); } },
    });
    if (r) await reload(`${r.label || 'The account'} added. Match its cars below to start reading them.`);
  }

  /* ---------- one account */
  function accountBlock(a) {
    const st = ACCOUNT_STATUS[a.status] || [a.status, 'muted', ''];
    const err = a.last_error;
    // the account as the page has it now: after a refresh behind a dialog, its new version
    const cur = () => accounts.find((x) => x.id === a.id) || a;
    const K = (k) => `acc:${a.id}:${k}`;
    const back = [`acc:${a.id}`];   // the account's heading, when the button is gone
    const patch = (title, intro, submit, bodyOf, extra = [], msg = 'Saved.', danger = false) => async () => {
      const r = await askPassword(ctx, {
        title, intro: intro ? note(intro) : null, submit, extra, danger,
        run: async (cred, v) => { try { return await ctx.api.patch(`${BASE}/accounts/${enc(a.id)}`, withPw(cred, { version: cur().version, ...bodyOf(v) })); } catch (e) { throw fail(e); } },
      });
      if (r) await reload(msg);
    };
    const pause = btn(a.paused ? 'resume' : 'pause', keep([K('pause'), ...back], patch(a.paused ? `resume ${a.label}?` : `pause ${a.label}?`,
      a.paused ? 'Its matched cars are read again from now on.' : 'Its cars are not read until you resume it. Nothing is asked of Tesla meanwhile; the history already stored stays.',
      a.paused ? 'resume' : 'pause', () => ({ paused: !a.paused }), [], a.paused ? `${a.label} resumed.` : `${a.label} paused.`)), { kind: a.paused ? 'primary' : '' });
    const rename = btn('rename', keep([K('rename'), ...back], () => {
      const f = field({ label: 'name', name: 'label', required: true, value: a.label, attrs: { maxlength: '64', autocomplete: 'off' } });
      return patch(`rename ${a.label}`, null, 'rename', (v) => ({ label: v.label.trim() }), [f], 'Renamed.')();
    }), { kind: 'quiet' });
    const token = btn('replace token', keep([K('token'), ...back], () => {
      const f = field({ label: 'token', name: 'refresh_token', path: 'refresh_token', type: 'textarea', required: true, hint: 'Checked with Tesla before it replaces the old one.', attrs: { spellcheck: 'false', autocomplete: 'off', rows: '3' } });
      return patch(`new token for ${a.label}`, 'For an account Tesla stopped taking (sign in again), or a token you renewed elsewhere. The old token is deleted.', 'check with tesla and replace', (v) => ({ refresh_token: v.refresh_token.trim() }), [f], 'Token replaced and checked with Tesla.')();
    }), { kind: a.status === 'sign_in_needed' ? 'primary' : '' });
    const remove = btn('remove', keep([K('remove'), ...back], async () => {
      const r = await askPassword(ctx, {
        title: `remove ${a.label}?`, danger: true, submit: 'remove the account',
        intro: note('Reading stops at once and the sealed file with its tokens is deleted. The history already stored from it stays, marked as data from the Tesla API.'),
        run: async (cred) => { try { return await ctx.api.send('DELETE', `${BASE}/accounts/${enc(a.id)}`, withPw(cred)); } catch (e) { throw fail(e); } },
      });
      if (r) await reload(`${a.label} removed. Its history stays.`);
    }), { kind: 'danger' });
    fk(pause, K('pause')); fk(rename, K('rename')); fk(token, K('token')); fk(remove, K('remove'));
    const loc = h('div', { class: 'keys', role: 'group', 'aria-label': `When the position of ${a.label}'s cars is asked for` },
      LOCATION.map(([v, l, why]) => h('button', { class: 'key', type: 'button', title: why, 'aria-pressed': String(a.location === v), dataset: { fk: K(`loc:${v}`) },
        onclick: keep([K(`loc:${v}`), ...back], () => (a.location === v ? null : patch(`position: ${l}?`, `${why} While the position is asked for, the car shows an icon on its screen that its location is shared.`, 'save', () => ({ location: v }), [], 'When the position is asked for: saved.')())) }, l)));

    return h('div', { class: 'tesla-acc', id: `tesla-${a.id}` },
      h('div', { class: 'tesla-acc-head' }, h('h3', { tabindex: '-1', dataset: { fk: `acc:${a.id}` } }, a.label), tag(st[0], st[1]), h('span', { class: 'muted' }, a.region === 'china' ? 'china' : 'global')),
      st[2] ? h('p', { class: ['note', a.status === 'sign_in_needed' || a.status === 'error' ? 'tone--warn' : null] }, st[2]) : null,
      kv([
        ['status', [tag(st[0], st[1]), a.running ? h('span', { class: 'muted' }, ' running') : null]],
        ['added', dateTime(a.added_ms, ctx.tz)],
        ['last ok', a.last_ok_ms ? h('span', { title: dateTime(a.last_ok_ms, ctx.tz) }, ago(a.last_ok_ms)) : h('span', { class: 'muted' }, 'none yet')],
        ['last error', err ? [lastErrorText(err), err.message && LAST_ERROR_WORDS[err.code] ? h('span', { class: 'muted' }, ` (${err.message})`) : null, err.at_ms ? h('span', { class: 'muted' }, `, ${ago(err.at_ms)}`) : null] : h('span', { class: 'muted' }, 'none'), { tone: err ? 'warn' : null }],
        ['requests today', `${formatNumber(a.polls_today ?? 0)} of 3,000`],
        ['hosts', h('span', { class: 'tesla-hosts' }, (a.hosts || []).map((x) => code(x)))],
      ]),
      h('div', { class: 'acts tesla-acts' }, pause, token, rename, remove),
      h('p', { class: 'sub-title' }, 'ask for the cars\' position'),
      loc,
      note(tag('icon', 'warn'), ' While the position is asked for, the car shows an icon on its screen that its location is shared. ', LOCATION.find((x) => x[0] === a.location)?.[2] || ''),
      carsBlock(a));
  }

  /* ---------- an account's cars: match each, and what the server does for it now */
  function carsBlock(a) {
    const data = carRows[a.id] || { items: a.cars || [], version: a.version };
    const items = data.items || [];
    const opts = [['', 'not decided (not read)'], ...cars.map((c) => [c.id, c.name || c.id]), ['new', 'a new car'], ['leave_out', 'leave out (not read)']];
    // the choice of each car (the table draws a select per car for the desktop row and the phone row)
    const picked = new Map(items.map((c) => [c.vehicle, c.match ?? c.suggested ?? '']));
    const K = (k) => `acc:${a.id}:${k}`;
    const save = fk(btn('save matches', keep([K('save'), K('cars'), `acc:${a.id}`], () => saveMatches()), { kind: 'primary' }), K('save'));
    save.hidden = true;
    const changed = () => items.filter((c) => (picked.get(c.vehicle) || null) !== (c.match ?? null));
    const vName = (c) => c.display_name || c.vehicle;
    // one Sentry Mate car takes one Tesla vehicle: two vehicles of this account on one car are refused here
    // (the server refuses it across accounts, car_taken)
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
      replace(dupeMsg, d.length ? [tag('two on one', 'bad'), ` ${d.map(([car, l]) => `${l.map(vName).join(' and ')} are both matched to ${carName(car)}.`).join(' ')} One Sentry Mate car takes one Tesla vehicle: pick another car, a new car or leave out for one of them.`] : '');
    };
    const select = (c) => {
      const pre = picked.get(c.vehicle);
      const known = opts.some(([v]) => v === pre) ? opts : [...opts, [pre, carName(pre)]];
      const sel = h('select', { class: 'in in--cell tesla-sel', 'aria-label': `Sentry Mate car for ${c.display_name || c.vehicle}` }, pairOptions(known, cars, pre));
      sel.addEventListener('change', () => { picked.set(c.vehicle, sel.value); sync(); });
      return sel;
    };
    const sels = { get: (v) => ({ value: picked.get(v) }) };
    async function saveMatches() {
      const ch = changed();
      if (!ch.length || dupes().length) { sync(); return; }
      // the admin's session alone: no password asked again (a match stores and shows no token)
      save.disabled = true;
      let r = null;
      try { r = await ctx.api.put(`${BASE}/accounts/${enc(a.id)}/cars`, { version: carRows[a.id]?.version ?? data.version ?? a.version, items: ch.map((c) => ({ vehicle: c.vehicle, match: sels.get(c.vehicle).value || null })) }); } catch (e) { if (e?.name !== 'AbortError') toast(errorWords(fail(e)).text, { kind: 'error' }); } finally { save.disabled = false; sync(); }
      if (r) await reload('Matches saved.');
    }
    const statusOf = (c) => status.find((s) => s.account === a.id && s.vehicle === c.vehicle) || {};
    const zoneOf = (c) => cars.find((x) => x.id === c.match)?.time_zone || ctx.tz;
    const t = table(ctx, {
      bare: true, rows: items, csv: false, label: `Cars of the Tesla account ${a.label}`,
      columns: [
        { key: 'car', label: 'car at tesla', ph: 'main', cls: 'tesla-car-col', render: (c) => [h('b', null, c.display_name || 'no name at Tesla'), h('span', { class: 'muted' }, ` ${[c.year, c.model].filter(Boolean).join(' ')} ${c.vin_prefix || ''}`)] },
        { key: 'match', label: 'sentry mate car', ph: 'sub', render: (c) => [select(c), c.match == null && c.suggested ? [' ', tag('suggested: same vin prefix', 'accent')] : null] },
        { key: 'state', label: 'tesla says', ph: 'sub', render: (c) => [h('span', { class: 'ph-k' }, 'tesla says '), CAR_STATE[c.state] || c.state || DASH] },
        { key: 'mode', label: 'what the server does', grow: true, ph: 'sub', cls: 'wrap-cell', render: (c) => {
          const m = CAR_MODE[c.mode] || [c.mode, ''];
          const sv = statusOf(c);
          const next = sv.next_poll_ms ?? null;
          return h('span', { title: m[1] }, h('span', { class: 'ph-k' }, 'server: '), m[0], sv.streaming ? h('span', { class: 'muted' }, ', live stream') : null,
            c.mode === 'backoff' && next ? h('span', { class: 'tone--warn' }, `; next try ${time(next, zoneOf(c))} (in ${dur(Math.max(0, next - ctx.api.now()))})`)
              : next && c.mode !== 'off' ? h('span', { class: 'muted' }, `; next read ${time(next, zoneOf(c))}`) : h('span', { class: 'muted' }, `: ${m[1].charAt(0).toLowerCase()}${m[1].slice(1)}`));
        } },
        { key: 'last', label: 'last data', num: true, ph: 'num', render: (c) => { const sv = statusOf(c); return sv.last_data_ms ? h('span', { title: dateTime(sv.last_data_ms, zoneOf(c)) }, ago(sv.last_data_ms)) : h('span', { class: 'muted' }, DASH); } },
      ],
      empty: () => h('div', { class: 'empty' }, h('p', { class: 'empty-line' }, '-- no cars on this account --'), h('p', { class: 'empty-hint' }, 'Tesla lists no car for this account. A car added to it at Tesla shows here within a few minutes.')),
    });
    tables.push(t);
    sync();
    return h('div', null, h('div', { class: 'tesla-cars-head' }, h('p', { class: 'sub-title', tabindex: '-1', dataset: { fk: K('cars') } }, `its cars (${items.length})`), save), dupeMsg, t.el, note('A car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN prefix as a car here; it is not saved until you press save matches.'));
  }

  return { el, side, load };
}

/** kv rows for one car's Tesla input (Data health), or null when no Tesla vehicle reads it. */
export function teslaCarRows(ctx, statusItems, carId) {
  const s = (statusItems || []).find((x) => x.car === carId);
  if (!s) return null;
  const m = CAR_MODE[s.mode] || [s.mode, ''];
  return [
    ['tesla api', [h('span', { title: m[1] }, m[0]), s.streaming ? h('span', { class: 'muted' }, ', live stream') : null, h('span', { class: 'muted' }, ` (Tesla says ${CAR_STATE[s.car_state] || s.car_state || 'not known'})`)]],
    ['last tesla data', s.last_data_ms ? h('span', { title: dateTime(s.last_data_ms, ctx.tz) }, ago(s.last_data_ms)) : h('span', { class: 'muted' }, 'none yet')],
    [s.mode === 'backoff' ? 'next try' : 'next read', s.next_poll_ms ? [time(s.next_poll_ms, ctx.tz), s.mode === 'backoff' ? h('span', { class: 'tone--warn' }, ' waiting after errors') : null] : h('span', { class: 'muted' }, 'not planned'), { tone: s.mode === 'backoff' ? 'warn' : null }],
    ['tesla requests', formatNumber(s.polls_today ?? 0)],
    s.last_error ? ['tesla error', lastErrorText(s.last_error), { tone: 'warn' }] : null,
    ['account', link(teslaAccountHref(s.account), 'open its tesla account')],
  ].filter(Boolean);
}
