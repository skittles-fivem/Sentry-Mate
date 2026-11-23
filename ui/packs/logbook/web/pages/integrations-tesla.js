// Sentry Mate, "Logbook" UI pack: Settings: Integrations, the Tesla account part (admins only).
//
// The Tesla API input: the server signs in to a Tesla account and reads its cars from Tesla's Owner API, as a
// second live input beside SMO. Off until an admin adds an account; it only reads (no command, no wake).
// This part shows the input's state (on or off, the Tesla hosts, how often it asks, requests today), adds an
// account (sign in at Tesla and paste the address the page ends on, or paste a refresh token), and per account
// its status, last good answer and last error, pause and resume, when the position is asked for, a new token,
// removing it, and its cars: matched to a car here, a new car, or left out, each with what the server does now.
// Every change but starting a sign-in and saving the matches asks for the admin's own password again (and the
// two-step code); the matches take the admin's session alone (they store and show no token).
//
// API: GET / PATCH integrations/tesla, GET / POST integrations/tesla/accounts, POST integrations/tesla/sign-in,
// POST integrations/tesla/sign-in/{sign_in}, PATCH / DELETE integrations/tesla/accounts/{id}, PUT
// integrations/tesla/accounts/{id}/cars, GET integrations/tesla/status, GET cars (the match picker).

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatDateTime, formatTime, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { section, figure, chip, button, icon, ago, errorView, empty } from '../core/ui.js';
import { field, form, choice, setFieldError, carOptions } from '../core/form.js';
import { openDialog } from '../core/dialog.js';
import { dataTable } from '../core/table.js';

/* ------------------------------------------------------------------ words */

const STATUS = {
  checking: ['Checking', null, 'Just added: waiting for Tesla’s first answer.'],
  ok: ['Reading', 'on', 'Tesla answers; the matched cars are read.'],
  paused: ['Paused', null, 'Nothing is asked of Tesla for this account until it is resumed.'],
  sign_in_needed: ['Sign in again', 'warn', 'Tesla stopped taking this account’s sign-in. Its cars are not read, and nothing is asked of Tesla, until you sign in at Tesla again or give it a new token.'],
  error: ['Error', 'warn', 'Tesla refuses this account for now; see the last error.'],
};
const MODE = {
  off: ['Off', 'Not read: not matched, left out, or the account is paused.'],
  watching_list: ['Watching the car list', 'Asleep or offline: only Tesla’s list of cars is read, so the car is never woken.'],
  online_idle: ['Awake, idle', 'Awake with nothing going on: read at the parked pace.'],
  letting_sleep: ['Letting it sleep', 'Left alone for 21 minutes after 10 quiet ones, so it can fall asleep.'],
  charging: ['Charging', 'Read at the charging pace.'],
  driving: ['Driving', 'Read at the driving pace (slower while Tesla’s live stream runs beside it).'],
  backoff: ['Waiting after errors', 'Tesla answered with errors, or the day’s requests are used up: nothing is asked until the next try.'],
};
const CAR_STATE = { online: 'Online', asleep: 'Asleep', offline: 'Offline', unknown: 'Not known' };
const LOCATION = [
  ['driving_or_charging', 'While driving or charging'],
  ['always', 'Always'],
  ['never', 'Never'],
];
const LOCATION_WHY = {
  driving_or_charging: 'The position is asked for only while the car drives or charges (the default).',
  always: 'The position is asked for at every read, parked too.',
  never: 'No position is asked for: Tesla API rows have no place or route.',
};
const PACE = [['often', 'Often'], ['normal', 'Normal'], ['gentle', 'Gentle']];
const REGION = [['global', 'Everywhere but China'], ['china', 'China']];

// Every error code the Tesla routes give, in words.
const ERROR_WORDS = {
  tesla_refused: 'Tesla refused it: the sign-in or the token may have run out or been used already, or the Tesla password changed. Sign in at Tesla again.',
  tesla_unreachable: 'Tesla could not be reached, or answered with an error. Nothing was kept; try again in a few minutes.',
  account_exists: 'The cars of this Tesla account are already read through another Tesla account here. Give that one a new token instead.',
  car_taken: 'One of these cars is already matched to a vehicle of another Tesla account here. Leave it out there first.',
  too_many_sign_ins: 'Five sign-ins are open already. Finish one, or wait up to ten minutes for them to run out.',
  no_such_sign_in: 'This sign-in ran out (after ten minutes) or was used already. Start a new one.',
  no_such_tesla_account: 'This Tesla account is no longer here: another admin may have removed it.',
  no_such_vehicle: 'Tesla no longer lists that car on this account.',
  stale_version: 'Someone changed this meanwhile; it is shown fresh now. Make your change again.',
  password_needed: 'Type your own Sentry Mate password again.',
  wrong_password: 'That is not your Sentry Mate password.',
  wrong_code: 'Not the right two-step code. Type the one your authenticator app shows now.',
  wrong_two_step: 'Not the right two-step code. Type the one your authenticator app shows now.',
  throttled: 'Too many tries. Wait a minute, then try again.',
  invalid: 'A value was not taken; see the field.',
  bad_input: 'A value was not taken; see the field.',
  not_allowed: 'Only an admin may change the Tesla account.',
};
// An account's last error (what Tesla or the poller said), in words.
const LAST_ERROR = {
  sign_in_needed: 'Tesla stopped taking the refresh token',
  tesla_refused: 'Tesla refused the token',
  tesla_unreachable: 'Tesla could not be reached',
  endpoint_gone: 'Tesla closed this part of its API; it is not asked again',
  rate_limited: 'Tesla asked to slow down',
  daily_cap: 'The day’s 3,000 requests are used up',
  server_error: 'Tesla answered with a server error',
  forbidden: 'Tesla refused the request (403); asked again in an hour',
};
export const teslaErrorText = (e) => (e && ERROR_WORDS[e.code]) || (e && e.message) || 'Something went wrong.';

const code = (t) => h('code', { class: 'si-code' }, t);
const quiet = (t) => h('span', { class: 'si-quiet' }, t);

/* ------------------------------------------------------------------ asking for the password again */

/**
 * A dialog with the extra fields, the admin's own password and, when the account has two-step sign-in, its
 * code. run(creds, values) sends the change with { confirm_password, code? }; a wrong password or code stays in
 * the dialog under its field, other errors in clear words on its top line. Resolves with run's answer, or
 * null when closed.
 */
export function askAgain(ctx, { title, text = null, yes, extra = [], run, danger = false, input = 'the Tesla input', errorText = teslaErrorText }) {
  let twoShown = !!(ctx.me && ctx.me.two_step_required);
  const pass = field({ label: 'Your password', name: 'confirm_password', type: 'password', hint: `Your own Sentry Mate password: it is asked again for every change to ${input}.`, attrs: { autocomplete: 'current-password', required: true } });
  const two = field({ label: 'Two-step code', name: 'code', hint: 'The 6 digits from your authenticator app, or a recovery code.', attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', spellcheck: 'false' } });
  two.hidden = !twoShown;
  let result = null;
  return openDialog({
    title, actions: [], cls: 'tesla-dialog', initialFocus: extra.length ? 'input, textarea, select' : 'input[name=confirm_password]',
    body: (close) => [
      text ? (typeof text === 'string' ? h('p', { class: 'dialog-text' }, text) : text) : null,
      form({
        fields: [...extra, pass, two], submitLabel: yes, busyLabel: 'Asking…', cls: danger ? 'mv-form-danger' : null,
        onsubmit: async (v) => {
          if (!v.confirm_password) throw Object.assign(new Error('Type your password.'), { fields: { confirm_password: 'Type your own password.' } });
          if (twoShown && !String(v.code || '').trim()) throw Object.assign(new Error('Type your two-step code as well.'), { fields: { code: 'Type the 6-digit code from your authenticator app, or a recovery code.' } });
          const creds = { confirm_password: v.confirm_password };
          if (twoShown && v.code) creds.code = String(v.code).trim();
          try {
            result = await run(creds, v);
          } catch (err) {
            if (api.isAbort(err)) throw err;
            const c = err && err.code;
            if ((c === 'wrong_code' || c === 'wrong_two_step') && !twoShown) {
              twoShown = true; two.hidden = false;
              throw Object.assign(new Error('Your account asks for a two-step code as well.'), { fields: { code: 'Type the 6-digit code from your authenticator app.' } });
            }
            if (c === 'wrong_password' || c === 'password_needed') throw Object.assign(new Error(ERROR_WORDS[c]), { fields: { confirm_password: ERROR_WORDS[c] } });
            if (c === 'wrong_code' || c === 'wrong_two_step') throw Object.assign(new Error(ERROR_WORDS[c]), { fields: { code: ERROR_WORDS[c] } });
            if (err && err.status === 429) throw new Error(`Too many tries. Wait ${Math.ceil((err.retryAfterMs || 60000) / 1000)} seconds, then try again.`);
            if (err && err.fields) {
              // field errors keep the server's words; a field this dialog does not have goes to the top line
              throw Object.assign(new Error(errorText(err)), { fields: err.fields });
            }
            throw new Error(errorText(err));
          }
          if (result === false) return;
          close('done');
        },
      }),
    ],
  }).then(() => result);
}

/* ------------------------------------------------------------------ the part */

/**
 * The Tesla account part: { el, load() }. load() reads everything and draws it; it rejects only when the page
 * was left. ctx.me decides: a watch account is told the part is for admins.
 */
export function teslaPart(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const tz = ctx.tz;
  const slot = h('div', { class: 'tesla' });
  const el = section({ title: 'Tesla account', note: 'a second live input beside SMO, admins only', id: 'tesla', cls: 'si-sec tesla-sec' }, slot);
  let over = null, accounts = [], status = [], cars = [], loadErr = null;
  let signIn = null; // { sign_in, url, expires_ms, region }
  const carName = (cid) => (cars.find((c) => c.id === cid) || {}).name || cid;

  async function load() {
    if (!admin) { draw(); return; }
    try {
      const [o, a, s, c] = await Promise.all([
        ctx.api.get('integrations/tesla'),
        ctx.api.get('integrations/tesla/accounts'),
        ctx.api.get('integrations/tesla/status').catch((e) => { if (api.isAbort(e)) throw e; return { items: [], error: e }; }),
        ctx.api.get('cars', { sort: 'name', limit: 500 }).catch((e) => { if (api.isAbort(e)) throw e; return { items: [] }; }),
      ]);
      over = o; accounts = a.items || []; status = s.items || []; cars = c.items || []; loadErr = null;
    } catch (e) {
      if (api.isAbort(e)) throw e;
      loadErr = e;
    }
    if (ctx.alive()) draw();
  }
  /**
   * Read everything again and draw it. focusKey names the control that opened the dialog (data-tf): the
   * redraw replaces it, so the new one gets the keyboard focus back (else the account, else the part), but
   * only when focus was left on nothing (the dialog's opener went away with the redraw).
   */
  async function again(focusKey = null, fallbackId = null) {
    await load();
    if (!ctx.alive() || !focusKey) return;
    const a = document.activeElement;
    if (a && a !== document.body && document.contains(a)) return;
    let t = slot.querySelector(`[data-tf="${CSS.escape(focusKey)}"]`);
    if (t && !t.matches('button, select, input, textarea, a[href]')) t = t.querySelector('[role="radio"][tabindex="0"]') || t.querySelector('button');
    if (!t && fallbackId) t = document.getElementById(fallbackId);
    if (!t) { t = el; if (!t.hasAttribute('tabindex')) t.setAttribute('tabindex', '-1'); }
    t.focus({ preventScroll: true });
    if (!isInView(t)) t.scrollIntoView({ block: 'nearest' });
  }
  const isInView = (t) => { const r = t.getBoundingClientRect(); return r.bottom > 0 && r.top < innerHeight; };
  const tf = (node, key) => { node.dataset.tf = key; return node; };
  const flashDone = (text) => ctx.flash(text);

  /* ---------------- drawing */
  function draw() {
    if (!admin) {
      replace(slot, h('p', { class: 'si-lead' }, 'Sentry Mate can also read cars from a Tesla account, through the Tesla Owner API, as a second live input beside SMO. Only admins see and change it.'));
      return;
    }
    if (loadErr) { replace(slot, errorView(loadErr, { title: 'The Tesla account settings could not be loaded.', retry: () => again(), home: false, eyebrow: 'Tesla account' })); return; }
    replace(slot,
      h('p', { class: 'si-lead' },
        'Beside SMO, Sentry Mate can read your cars from Tesla’s Owner API, the unofficial API Tesla’s own app uses. It only reads: it never wakes a car or sends it a command, and no page ever shows a Tesla token. ',
        over.enabled ? null : h('strong', null, 'Off until an account is added: until then the server makes no connection to Tesla at all.')),
      overview(),
      accounts.length ? h('div', { class: 'tesla-accounts' }, accounts.map(accountCard)) : null,
      addPart(),
      statusPart());
  }

  function overview() {
    const caps = over.polls || {};
    const sec = (n) => (n == null ? '—' : n >= 60 ? formatDuration(n * 1000) : `${n} s`);
    const pace = choice({
      label: 'How often Tesla is asked', labelHidden: true, name: 'tesla-poll', value: over.poll, options: PACE,
      onchange: async (v) => {
        if (v === over.poll) return;
        const out = await askAgain(ctx, {
          title: `Ask Tesla ${v === 'often' ? 'more often' : v === 'gentle' ? 'less often' : 'at the normal pace'}?`,
          text: 'This changes how often every Tesla account’s cars are read. Asleep cars are never woken at any pace.',
          yes: 'Change it',
          run: (creds) => ctx.api.patch('integrations/tesla', { version: over.version, poll: v, ...creds }),
        });
        if (out) { flashDone('How often Tesla is asked was changed.'); } else pace.set(over.poll);
        await again('pace');
      },
    });
    const paceRows = [
      ['Parked and awake', caps.parked_s], ['Charging', caps.charging_s], ['Driving', caps.driving_s],
      ['Driving, with Tesla’s live stream', caps.driving_streaming_s], ['The list of cars (asleep cars)', caps.list_s],
    ];
    return h('div', { class: 'tesla-over' },
      h('div', { class: 'figs tesla-figs' },
        figure('The input', over.enabled ? 'On' : 'Off', over.enabled
          ? `${formatNumber(over.accounts)} ${over.accounts === 1 ? 'account' : 'accounts'}, ${formatNumber(over.cars)} ${over.cars === 1 ? 'car' : 'cars'} read`
          : 'off until an account is added'),
        figure('Requests today', formatNumber(over.polls_today || 0), `at most ${formatNumber(caps.daily_cap || 3000)} a day per account, one a second`),
        figure('Connections to Tesla', formatNumber(over.clients_built || 0), 'set up since the server started'),
        figure('Last error', over.last_error ? (LAST_ERROR[over.last_error.code] || over.last_error.message || over.last_error.code) : 'None',
          over.last_error ? [over.last_error.at_ms ? ago(over.last_error.at_ms) : null, over.last_error.account ? ` · ${accountLabel(over.last_error.account)}` : null] : 'no error from Tesla')),
      h('div', { class: 'si-two tesla-two' },
        h('div', null,
          h('h3', { class: 'si-sub' }, 'Tesla hosts', h('span', { class: 'label-note' }, 'for a firewall')),
          over.hosts && over.hosts.length
            ? h('ul', { class: 'tesla-hosts' }, over.hosts.map((x) => h('li', null, code(x))))
            : h('p', { class: 'si-quiet' }, 'None: with no account the server talks to no Tesla host. Adding one uses auth.tesla.com, owner-api.teslamotors.com and streaming.vn.teslamotors.com (Tesla’s China hosts for a China account).')),
        h('div', null,
          h('h3', { class: 'si-sub' }, 'How often', h('span', { class: 'label-note' }, `“${(PACE.find(([k]) => k === over.poll) || [0, over.poll])[1]}” now`)),
          tf(pace, 'pace'),
          h('dl', { class: 'tesla-pace' }, paceRows.map(([n, s]) => h('div', { class: 'tesla-pace-row' }, h('dt', null, n), h('dd', null, s == null ? '—' : `every ${sec(s)}`)))),
          h('p', { class: 'si-quiet' }, 'A car that is asleep is never woken: only the list of cars is read. An awake car with nothing going on is left alone for 21 minutes after 10 quiet ones so it can fall asleep.'))));
  }
  const accountLabel = (id) => (accounts.find((a) => a.id === id) || {}).label || id;

  /* ---------------- one account */
  function accountCard(a) {
    const st = STATUS[a.status] || [a.status, null, ''];
    const patch = (body, opts) => askAgain(ctx, { ...opts, run: (creds) => ctx.api.patch(api.apiPath`integrations/tesla/accounts/${a.id}`, { version: a.version, ...body, ...creds }) });
    const pauseBtn = button(a.paused ? 'Resume' : 'Pause', {
      onclick: async () => {
        const out = await patch({ paused: !a.paused }, {
          title: a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`,
          text: a.paused ? 'Its matched cars are read again from now on.' : 'Nothing is asked of Tesla for this account until you resume it. The history already stored stays.',
          yes: a.paused ? 'Resume' : 'Pause',
        });
        if (out) flashDone(a.paused ? `${a.label} reads its cars again.` : `${a.label} is paused.`);
        await again(`pause-${a.id}`, `tesla-${a.id}`);
      },
    });
    tf(pauseBtn, `pause-${a.id}`);
    const renameBtn = button('Rename', {
      onclick: async () => {
        const out = await askAgain(ctx, {
          title: 'Rename the Tesla account', yes: 'Rename',
          extra: [field({ label: 'Name', name: 'label', value: a.label, hint: 'Your own name for it, shown on these pages. Never the account’s e-mail.', attrs: { maxlength: 64, required: true } })],
          run: (creds, v) => ctx.api.patch(api.apiPath`integrations/tesla/accounts/${a.id}`, { version: a.version, label: v.label, ...creds }),
        });
        if (out) flashDone(`Renamed to ${out.label}.`);
        await again(`rename-${a.id}`, `tesla-${a.id}`);
      },
    });
    tf(renameBtn, `rename-${a.id}`);
    const tokenBtn = button('Replace the token', {
      kind: 'quiet', onclick: async () => {
        const out = await askAgain(ctx, {
          title: `A new refresh token for ${a.label}`, yes: 'Check it with Tesla and keep it',
          text: 'Paste a Tesla refresh token from another tool (for the same Tesla account). The server checks it with Tesla before anything is kept, and never shows it again.',
          extra: [field({ label: 'Refresh token', name: 'refresh_token', type: 'textarea', attrs: { rows: 3, spellcheck: 'false', autocomplete: 'off', required: true } })],
          run: (creds, v) => {
            if (!String(v.refresh_token || '').trim()) throw Object.assign(new Error('Paste the token.'), { fields: { refresh_token: 'Paste the refresh token.' } });
            return ctx.api.patch(api.apiPath`integrations/tesla/accounts/${a.id}`, { version: a.version, refresh_token: String(v.refresh_token).trim(), ...creds });
          },
        });
        if (out) flashDone(`${a.label} has a new token; its cars are read again.`);
        await again(`token-${a.id}`, `tesla-${a.id}`);
      },
    });
    tf(tokenBtn, `token-${a.id}`);
    const removeBtn = button('Remove', {
      kind: 'danger', onclick: async () => {
        const out = await askAgain(ctx, {
          title: `Remove ${a.label}?`, yes: 'Remove the account', danger: true,
          text: 'Its cars stop being read at once and the sealed file with its tokens is deleted. The history already stored stays, marked as data from the Tesla API.',
          run: (creds) => ctx.api.del(api.apiPath`integrations/tesla/accounts/${a.id}`, { headers: { 'X-Sentry-Mate-Password': creds.confirm_password, ...(creds.code ? { 'X-Sentry-Mate-Code': creds.code } : {}) } }),
        });
        if (out) flashDone(`${a.label} was removed.`);
        // removed: its card is gone, so focus goes to the part
        await again(`remove-${a.id}`, `tesla-${a.id}`);
      },
    });
    tf(removeBtn, `remove-${a.id}`);
    // Signing in at Tesla again: the server keeps one account per Tesla account and a sign-in always adds
    // one, so this one is removed first (its history stays) and a sign-in is started in its region, named as
    // it was. Its cars are matched again afterwards.
    const reSignBtn = a.status === 'sign_in_needed' ? tf(button('Sign in at Tesla again', {
      kind: 'primary', onclick: async () => {
        const out = await askAgain(ctx, {
          title: `Sign in at Tesla again for ${a.label}?`, yes: 'Remove it and start a sign-in',
          text: h('div', { class: 'dialog-text' },
            h('p', null, 'A Tesla sign-in always adds an account here, and the same Tesla account can be here only once. So this removes ', h('strong', null, a.label), ' first (its sealed tokens are deleted; the history already stored stays) and then starts a sign-in at Tesla for the same region.'),
            h('p', null, 'Finish it within ten minutes, under “Add another Tesla account”, then choose its cars again under “Its cars”. To keep the account and its matches instead, use “Replace the token” with a refresh token from another tool.')),
          run: (creds) => ctx.api.del(api.apiPath`integrations/tesla/accounts/${a.id}`, { headers: { 'X-Sentry-Mate-Password': creds.confirm_password, ...(creds.code ? { 'X-Sentry-Mate-Code': creds.code } : {}) } }),
        });
        if (!out) { await again(`resign-${a.id}`, `tesla-${a.id}`); return; }
        let started = null;
        try { started = await ctx.api.post('integrations/tesla/sign-in', { region: a.region || 'global' }); } catch (e) {
          if (api.isAbort(e)) return;
          ctx.flash(`${a.label} was removed, but a sign-in could not be started: ${teslaErrorText(e)} Start one under “Add a Tesla account”.`, { kind: 'error' });
        }
        if (started) { signIn = { ...started, region: a.region || 'global', label: a.label }; flashDone(`${a.label} was removed. Sign in at Tesla now, then paste the address its page ends on.`); }
        await again();
        const go = document.querySelector('#tesla-steps a[href]') || document.getElementById('tesla-steps') || el;
        if (!go.matches('a[href]') && !go.hasAttribute('tabindex')) go.setAttribute('tabindex', '-1');
        go.focus({ preventScroll: true });
        (document.getElementById('tesla-steps') || go).scrollIntoView({ block: 'start' });
      },
    }), `resign-${a.id}`) : null;
    const loc = choice({
      label: 'Ask for the car’s position', name: `loc-${a.id}`, value: a.location, options: LOCATION,
      onchange: async (v) => {
        if (v === a.location) return;
        const out = await patch({ location: v }, {
          title: 'When to ask for the position?',
          text: `${LOCATION_WHY[v]} While Tesla is asked for a car’s position, the car shows a “location shared” icon on its screen.`,
          yes: 'Change it',
        });
        if (out) flashDone('When the position is asked for was changed.'); else loc.set(a.location);
        await again(`loc-${a.id}`, `tesla-${a.id}`);
      },
    });
    tf(loc, `loc-${a.id}`);
    const err = a.last_error;
    const facts = [
      ['Status', h('span', null, chip(st[0], { kind: st[1] }), h('span', { class: 'tesla-fact-note' }, st[2]))],
      ['Region', a.region === 'china' ? 'China (Tesla’s China hosts; no live stream)' : 'Everywhere but China'],
      ['Last good answer', a.last_ok_ms ? h('span', null, ago(a.last_ok_ms), h('span', { class: 'tesla-fact-note' }, formatDateTime(a.last_ok_ms, tz, { now: api.now() }))) : quiet('none yet')],
      ['Last error', err ? h('span', null, LAST_ERROR[err.code] || err.message || err.code, h('span', { class: 'tesla-fact-note' }, [err.message && LAST_ERROR[err.code] ? `Tesla: ${err.message}. ` : '', err.at_ms ? formatDateTime(err.at_ms, tz, { now: api.now() }) : ''])) : quiet('none')],
      ['Requests today', formatNumber(a.polls_today || 0)],
      ['Added', formatDateTime(a.added_ms, tz, { now: api.now() })],
      ['Talks to', h('span', { class: 'tesla-hostlist' }, (a.hosts || []).map((x) => code(x)))],
    ];
    return h('article', { class: ['tesla-account', `is-${a.status}`], id: `tesla-${a.id}`, tabindex: '-1' },
      h('div', { class: 'tesla-account-head' },
        h('div', null,
          h('p', { class: 'eyebrow' }, `Tesla account ${a.id}`),
          h('h3', { class: 'tesla-account-name' }, a.label)),
        h('div', { class: 'tesla-actions' }, reSignBtn, pauseBtn, renameBtn, tokenBtn, removeBtn)),
      a.status === 'sign_in_needed' ? h('p', { class: 'tesla-alert' }, icon('warning'), 'Tesla stopped taking this account’s sign-in. Sign in at Tesla again (the account is added afresh and its cars are matched again), or keep it as it is with a new refresh token from another tool (Replace the token). Its cars are read again once Tesla takes it.') : null,
      a.status === 'error' ? h('p', { class: 'tesla-alert' }, icon('warning'), `Tesla refuses this account for now: ${err ? LAST_ERROR[err.code] || err.message : 'no reason given'}. The server tries again by itself; a new token helps when the sign-in is the cause.`) : null,
      h('dl', { class: 'tesla-facts' }, facts.map(([n, v]) => h('div', { class: 'tesla-fact' }, h('dt', null, n), h('dd', null, v)))),
      h('div', { class: 'tesla-loc' }, loc, h('p', { class: 'si-quiet' }, LOCATION_WHY[a.location] || '', ' Asking for the position shows a “location shared” icon in the car while it is asked.')),
      carsPart(a));
  }

  /* ---------------- an account's cars */
  function carsPart(a) {
    const list = a.cars || [];
    const chosen = new Map(list.map((c) => [c.vehicle, c.match ?? c.suggested ?? '']));
    const statusOf = (v) => status.find((s) => s.account === a.id && s.vehicle === v) || null;
    // cars here another Tesla account already reads into: the server refuses them (car_taken), so they are
    // not offered; one car here is fed by one Tesla car at most
    const takenBy = new Map();
    for (const o of accounts) if (o.id !== a.id) for (const c of o.cars || []) if (c.match && c.match !== 'new' && c.match !== 'leave_out') takenBy.set(c.match, o.label);
    const isCar = (v) => !!v && v !== 'new' && v !== 'leave_out';
    const nameOf = (c) => c.display_name || c.vehicle;
    const selects = new Map();
    const warn = h('p', { class: 'tesla-alert', role: 'alert', hidden: true });
    /** Tesla cars of this account matched to the same car here: [[car id, [Tesla car, ...]], ...]. */
    function doubles() {
      const by = new Map();
      for (const c of list) { const v = chosen.get(c.vehicle); if (isCar(v)) by.set(v, [...(by.get(v) || []), c]); }
      return [...by].filter(([, cs]) => cs.length > 1);
    }
    function showDoubles() {
      const d = doubles();
      for (const sel of selects.values()) sel.removeAttribute('aria-invalid');
      if (!d.length) { warn.hidden = true; replace(warn); return d; }
      for (const [, cs] of d) for (const c of cs) selects.get(c.vehicle)?.setAttribute('aria-invalid', 'true');
      replace(warn, icon('warning'), h('span', null, d.map(([cid, cs]) => `Tesla’s ${cs.map(nameOf).join(' and ')} are both matched to ${carName(cid)} here.`).join(' '),
        ' One car here is read from one Tesla car only: choose another car, a new car or Leave it out for all but one.'));
      warn.hidden = false;
      return d;
    }
    const save = tf(button('Save the matches', {
      kind: 'primary', onclick: async () => {
        const d = showDoubles();
        if (d.length) { selects.get(d[0][1][1].vehicle)?.focus(); return; }
        const items = list.map((c) => ({ vehicle: c.vehicle, match: chosen.get(c.vehicle) || null }));
        const changes = items.filter((x) => x.match !== (list.find((c) => c.vehicle === x.vehicle).match ?? null));
        if (!changes.length) { ctx.flash('Nothing changed in the matches.', { kind: 'info' }); return; }
        // the admin's session alone: no password asked again
        save.disabled = true;
        let out = null;
        try { out = await ctx.api.put(api.apiPath`integrations/tesla/accounts/${a.id}/cars`, { version: a.version, items: changes }); } catch (e) { if (api.isAbort(e)) return; ctx.flash(teslaErrorText(e), { kind: 'error' }); } finally { save.disabled = false; }
        if (out) flashDone('The matches were saved.');
        await again(`save-${a.id}`, `tesla-${a.id}`);
      },
    }), `save-${a.id}`);
    const picker = (c) => {
      const mine = chosen.get(c.vehicle) || '';
      const sel = h('select', { class: 'field-select tesla-match', 'aria-label': `Match ${nameOf(c)}` },
        h('option', { value: '' }, 'Not decided: not read'),
        carOptions(cars, { group: 'A car here', option: (x) => {
          const other = takenBy.get(x.id);
          return h('option', { value: x.id, disabled: !!other && mine !== x.id }, `${x.name}${c.suggested === x.id ? ' (suggested)' : ''}${other ? ` (read through ${other})` : ''}`);
        } }),
        h('option', { value: 'new' }, 'A new car'),
        h('option', { value: 'leave_out' }, 'Leave it out'));
      sel.value = mine;
      sel.addEventListener('change', () => { chosen.set(c.vehicle, sel.value); showDoubles(); });
      selects.set(c.vehicle, sel);
      return h('span', { class: 'tesla-cell' }, h('span', { class: 'tesla-cell-label', 'aria-hidden': 'true' }, 'Is the car'), sel);
    };
    const labelled = (label, v) => h('span', { class: 'tesla-cell' }, h('span', { class: 'tesla-cell-label', 'aria-hidden': 'true' }, label), v);
    const modeCell = (c) => {
      const m = MODE[c.mode] || [c.mode, ''];
      const s = statusOf(c.vehicle);
      return labelled('What the server does', h('span', null, h('span', { class: 'cell-main', title: m[1] }, m[0]),
        h('span', { class: 'cell-sub' }, c.mode === 'backoff' && s && s.next_poll_ms ? `next try ${formatTime(s.next_poll_ms, tz)} (${formatDuration(Math.max(0, s.next_poll_ms - api.now()))} from now)`
          : c.match == null && c.suggested ? `Not read yet. Suggested: ${carName(c.suggested)}, the same VIN prefix; save the matches to read it.`
            : c.match == null ? 'Not read until you choose which car it is.' : m[1])));
    };
    const t = dataTable(ctx, {
      rows: list, itemName: ['car', 'cars'], label: `Cars of ${a.label}`, csv: false, cls: 'tesla-cars-table',
      caption: 'Tesla’s cars on this account; choose which car here each one is',
      columns: [
        { key: 'name', label: 'Tesla’s name', th: true, cls: 'tesla-c-name', cell: (c) => h('span', null, h('span', { class: 'cell-main' }, nameOf(c)), h('span', { class: 'cell-sub' }, [c.year, c.model].filter(Boolean).join(' '), ' · ', h('span', { class: 'mono' }, c.vin_prefix || '')) ) },
        { key: 'match', label: 'Is the car', cls: 'tesla-c-match', cell: picker },
        { key: 'state', label: 'Tesla says', cls: 'tesla-c-state', cell: (c) => labelled('Tesla says', CAR_STATE[c.state] || c.state || '—') },
        { key: 'mode', label: 'What the server does', cls: 'si-wrap tesla-c-mode', cell: modeCell },
      ],
      empty: () => empty({ title: 'Tesla lists no car on this account.' }),
    });
    t.load?.();
    showDoubles();
    return h('div', { class: 'tesla-cars' }, h('div', { class: 'si-sub-head' }, h('h4', { class: 'tesla-cars-title' }, 'Its cars'), list.length ? save : null), warn, t.el);
  }

  /* ---------------- adding an account */
  function addPart() {
    const region = field({ label: 'Where the Tesla account is', name: 'region', type: 'select', value: (signIn && signIn.region) || 'global', options: REGION });
    const start = button(signIn ? 'Start a new sign-in' : 'Sign in at Tesla', {
      kind: signIn ? 'quiet' : 'primary', onclick: async () => {
        start.disabled = true;
        try {
          const r = await ctx.api.post('integrations/tesla/sign-in', { region: region.control.value });
          signIn = { ...r, region: region.control.value };
          draw();
          document.getElementById('tesla-steps')?.scrollIntoView({ block: 'nearest' });
          document.querySelector('#tesla-steps a[href]')?.focus({ preventScroll: true });
        } catch (e) {
          if (!api.isAbort(e)) ctx.flash(`Could not start a sign-in: ${teslaErrorText(e)}`, { kind: 'error' });
          start.disabled = false;
        }
      },
    });
    const left = signIn ? signIn.expires_ms - api.now() : 0;
    const steps = signIn ? h('ol', { class: 'tesla-steps', id: 'tesla-steps' },
      h('li', null, h('p', { class: 'tesla-step-name' }, 'Open Tesla’s sign-in page'),
        h('p', null, h('a', { class: 'link-arrow', href: signIn.url, target: '_blank', rel: 'external noopener noreferrer' }, 'Tesla’s sign-in page', icon('arrowRight')), ' opens in a new tab, on Tesla’s own site.')),
      h('li', null, h('p', { class: 'tesla-step-name' }, 'Sign in there'),
        h('p', null, 'Tesla’s page asks for your Tesla e-mail, your Tesla password and, if you have one, its two-step code. Sentry Mate never sees them.')),
      h('li', null, h('p', { class: 'tesla-step-name' }, 'Copy the address it ends on'),
        h('p', null, 'After the sign-in the tab ends on a page that does not load. That is expected: its address starts with ', code(`${'tesla'}://auth/callback?code=`), '. Copy the whole address from the address bar.')),
      h('li', null, h('p', { class: 'tesla-step-name' }, 'Paste it here'),
        h('p', null, left > 0 ? `Within ten minutes of starting: this sign-in runs out at ${formatTime(signIn.expires_ms, tz)}, and can be used once.` : 'This sign-in ran out: start a new one.'),
        button('Paste the address', {
          kind: 'primary', disabled: left <= 0, onclick: async () => {
            const out = await askAgain(ctx, {
              title: 'Finish signing in', yes: 'Add the account',
              text: 'The server checks the address with Tesla and reads the list of cars before anything is kept.',
              extra: [
                field({ label: 'The address Tesla’s page ended on', name: 'pasted_url', type: 'textarea', hint: 'Starts with tesla://auth/callback?code=. It did not load: that is fine.', attrs: { rows: 3, spellcheck: 'false', autocomplete: 'off', required: true } }),
                field({ label: 'A name for this account', name: 'label', value: signIn.label || 'Tesla account', hint: 'Your own name for it, such as “Family account”. Never the e-mail.', attrs: { maxlength: 64 } }),
              ],
              run: (creds, v) => {
                if (!String(v.pasted_url || '').trim()) throw Object.assign(new Error('Paste the address.'), { fields: { pasted_url: 'Paste the whole address Tesla’s page ended on.' } });
                return ctx.api.post(api.apiPath`integrations/tesla/sign-in/${signIn.sign_in}`, { pasted_url: String(v.pasted_url).trim(), label: v.label || undefined, ...creds });
              },
            });
            if (out) { signIn = null; flashDone(`${out.label} was added. Choose which cars to read under “Its cars”.`); await again(); location.hash = `#tesla-${out.id}`; document.getElementById(`tesla-${out.id}`)?.focus({ preventScroll: true }); }
          },
        }))) : null;
    const tokenBtn = button('Add with a refresh token', {
      onclick: async () => {
        const out = await askAgain(ctx, {
          title: 'Add a Tesla account with a refresh token', yes: 'Check it with Tesla and add it',
          text: 'For a refresh token from another tool. The server checks it with Tesla (one token refresh and the list of cars) before anything is kept, and never shows it again.',
          extra: [
            field({ label: 'A name for this account', name: 'label', value: 'Tesla account', hint: 'Your own name for it. Never the e-mail.', attrs: { maxlength: 64, required: true } }),
            field({ label: 'Where the Tesla account is', name: 'region', type: 'select', value: 'global', options: REGION }),
            field({ label: 'Refresh token', name: 'refresh_token', type: 'textarea', attrs: { rows: 3, spellcheck: 'false', autocomplete: 'off', required: true } }),
          ],
          run: (creds, v) => {
            if (!String(v.refresh_token || '').trim()) throw Object.assign(new Error('Paste the token.'), { fields: { refresh_token: 'Paste the refresh token.' } });
            return ctx.api.post('integrations/tesla/accounts', { label: v.label, region: v.region, refresh_token: String(v.refresh_token).trim(), ...creds });
          },
        });
        if (out) { flashDone(`${out.label} was added. Choose which cars to read under “Its cars”.`); await again(); location.hash = `#tesla-${out.id}`; document.getElementById(`tesla-${out.id}`)?.focus({ preventScroll: true }); }
      },
    });
    return h('div', { class: 'tesla-add' },
      h('h3', { class: 'si-sub' }, accounts.length ? 'Add another Tesla account' : 'Add a Tesla account', h('span', { class: 'label-note' }, 'two ways')),
      h('div', { class: 'si-two' },
        h('div', { class: 'tesla-way' },
          h('h4', { class: 'tesla-way-name' }, 'Sign in at Tesla'),
          h('p', null, 'You sign in on Tesla’s own page; Sentry Mate gets only the code Tesla gives back. Nothing is stored until you paste the address and type your password here.'),
          h('div', { class: 'tesla-start' }, region, start),
          steps),
        h('div', { class: 'tesla-way' },
          h('h4', { class: 'tesla-way-name' }, 'Paste a refresh token'),
          h('p', null, 'If another tool already gave you a Tesla refresh token, paste it instead. It is checked with Tesla, kept sealed on this server, and never shown again.'),
          tokenBtn)));
  }

  /* ---------------- what the server does for each car */
  function statusPart() {
    if (!status.length) return null;
    const vName = (s) => { const a = accounts.find((x) => x.id === s.account); const c = a && (a.cars || []).find((x) => x.vehicle === s.vehicle); return (c && c.display_name) || s.vehicle; };
    const t = dataTable(ctx, {
      rows: status, itemName: ['Tesla car', 'Tesla cars'], label: 'What the server does for each Tesla car', csv: false,
      columns: [
        { key: 'car', label: 'Car', th: true, cell: (s) => h('span', null, s.car ? h('a', { class: 'cell-main', href: state.buildUrl('car', { car: s.car }) }, carName(s.car)) : h('span', { class: 'cell-main' }, vName(s)), h('span', { class: 'cell-sub' }, `${vName(s)} on ${accountLabel(s.account)}`)) },
        { key: 'mode', label: 'Now', cls: 'si-wrap', cell: (s) => h('span', null, h('span', { class: 'cell-main' }, (MODE[s.mode] || [s.mode])[0]), h('span', { class: 'cell-sub' }, CAR_STATE[s.car_state] || s.car_state || '', s.streaming ? ' · live stream on' : '')) },
        { key: 'last', label: 'Last asked', cell: (s) => (s.last_poll_ms ? ago(s.last_poll_ms) : quiet('not yet')) },
        { key: 'next', label: 'Next', cell: (s) => (s.next_poll_ms ? h('span', null, formatTime(s.next_poll_ms, tz), s.mode === 'backoff' ? h('span', { class: 'cell-sub' }, 'waiting after errors') : null) : quiet('not asked')) },
        { key: 'data', label: 'Newest data', cell: (s) => (s.last_data_ms ? ago(s.last_data_ms) : quiet('none')) },
        { key: 'polls', label: 'Requests today', num: true, cell: (s) => formatNumber(s.polls_today || 0) },
        { key: 'err', label: 'Last error', cls: 'si-wrap', cell: (s) => (s.last_error ? (LAST_ERROR[s.last_error.code] || s.last_error.message || s.last_error.code) : quiet('none')) },
      ],
    });
    t.load?.();
    return h('div', { class: 'tesla-status' }, h('h3', { class: 'si-sub' }, 'Each Tesla car now', h('span', { class: 'label-note' }, 'what the server does for it')), t.el);
  }

  return { el, load };
}

/** The account a #tesla-<id> address points at, opened into view once the part is drawn. */
export function showTeslaHash() {
  const id = (location.hash || '').slice(1);
  const m = /^(tesla|teslafi)(-|$)/.exec(id);
  if (!m) return;
  const t = document.getElementById(id) || document.getElementById(m[1]);
  if (!t) return;
  // the account (tabindex -1) or the part gets the focus, so a keyboard user carries on from there
  if (!t.hasAttribute('tabindex')) t.setAttribute('tabindex', '-1');
  t.focus({ preventScroll: true });
  t.scrollIntoView({ block: 'start' });
}
