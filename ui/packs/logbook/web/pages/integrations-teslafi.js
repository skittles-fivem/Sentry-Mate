// Sentry Mate, "Logbook" UI pack: Settings: Integrations, the TeslaFi part (admins only).
//
// The TeslaFi input: for owners who pay for TeslaFi, the server reads TeslaFi's own polls of the car from
// TeslaFi's API with the owner's TeslaFi token, as a third live input beside SMO and the Tesla API. Off until
// an admin adds a token; it only reads (no wake, no command). Built like the Tesla account part
// (pages/integrations-tesla.js), with its looks and its password dialog: the input's state, how often TeslaFi
// is read (Gentle, Normal, Often), adding a token, and per account its status, last good read and last error,
// pause and resume, its time zone, a new token, removing it, and its car: matched to a car here, a new car, or
// left out. Every change asks for the admin's own password again (and the two-step code).
//
// API: GET / PATCH integrations/teslafi, GET / POST integrations/teslafi/accounts, PATCH / DELETE
// integrations/teslafi/accounts/{id}, PUT integrations/teslafi/accounts/{id}/cars, GET integrations/teslafi/status,
// GET cars (the match picker).

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatDateTime, formatTime, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { section, figure, chip, button, icon, ago, errorView, empty } from '../core/ui.js';
import { field, form, choice } from '../core/form.js';
import { dataTable } from '../core/table.js';
import { askAgain } from './integrations-tesla.js';

/* ------------------------------------------------------------------ words */

const STATUS = {
  checking: ['Checking', null, 'Just added: waiting for TeslaFi’s first answer.'],
  ok: ['Reading', 'on', 'TeslaFi answers; the matched car is read.'],
  paused: ['Paused', null, 'Nothing is asked of TeslaFi for this account until it is resumed.'],
  token_refused: ['Token refused', 'warn', 'TeslaFi no longer takes this token (a wrong token, or the TeslaFi subscription ended). Nothing is asked of TeslaFi until you give a new token.'],
  error: ['Error', 'warn', 'TeslaFi answers with errors for now; see the last error. The server tries again by itself.'],
};
const MODE = {
  off: ['Off', 'Not read: no car matched, the account is paused, or its token is refused.'],
  asleep: ['Asleep', 'TeslaFi says the car sleeps: read at the asleep pace. The car is never woken.'],
  idle: ['Awake, parked', 'Read at the parked pace.'],
  charging: ['Charging', 'Read at the charging pace.'],
  driving: ['Driving', 'Read at the driving pace.'],
  backoff: ['Waiting after errors', 'TeslaFi answered with errors, asked to wait, or the day’s requests are used up: nothing is asked until the next try.'],
};
const PACE = [['often', 'Often'], ['normal', 'Normal'], ['gentle', 'Gentle']];
const PACE_WHY = {
  often: 'As fast as TeslaFi itself refreshes.',
  normal: 'The default.',
  gentle: 'Fewer requests to TeslaFi.',
};

// Every error code the TeslaFi routes give, in words.
const ERROR_WORDS = {
  teslafi_refused: 'TeslaFi did not take this token: check it on TeslaFi (Settings, TeslaFi API), and that the TeslaFi subscription is active. Nothing was kept.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was kept; try again in a few minutes.',
  account_exists: 'The car of this TeslaFi token is already read through another TeslaFi account here. Give that one the new token instead.',
  car_taken: 'That car is already matched to the car of another TeslaFi account here. Leave it out there first.',
  no_such_teslafi_account: 'This TeslaFi account is no longer here: another admin may have removed it.',
  stale_version: 'Someone changed this meanwhile; it is shown fresh now. Make your change again.',
  password_needed: 'Type your own Sentry Mate password again.',
  wrong_password: 'That is not your Sentry Mate password.',
  wrong_code: 'Not the right two-step code. Type the one your authenticator app shows now.',
  wrong_two_step: 'Not the right two-step code. Type the one your authenticator app shows now.',
  throttled: 'Too many tries. Wait a minute, then try again.',
  invalid: 'A value was not taken; see the field.',
  bad_input: 'A value was not taken; see the field.',
  not_allowed: 'Only an admin may change the TeslaFi input.',
};
// An account's last error (what TeslaFi or the poller said), in words.
const LAST_ERROR = {
  token_refused: 'TeslaFi refused the token',
  teslafi_refused: 'TeslaFi refused the token',
  teslafi_unreachable: 'TeslaFi could not be reached',
  rate_limited: 'TeslaFi asked to wait',
  daily_cap: 'The day’s requests are used up',
  server_error: 'TeslaFi answered with a server error',
};
const errorText = (e) => (e && ERROR_WORDS[e.code]) || (e && e.message) || 'Something went wrong.';
const errWords = (err) => (err ? LAST_ERROR[err.code] || err.message || err.code : null);

const code = (t) => h('code', { class: 'si-code' }, t);
const quiet = (t) => h('span', { class: 'si-quiet' }, t);
const ask = (ctx, opts) => askAgain(ctx, { input: 'TeslaFi input', words: ERROR_WORDS, errorText, ...opts });
const pwHeaders = (creds) => ({ 'X-Sentry-Mate-Password': creds.confirm_password, ...(creds.code ? { 'X-Sentry-Mate-Code': creds.code } : {}) });
const sec = (n) => (n == null ? '—' : n >= 60 ? formatDuration(n * 1000) : `${n} s`);
/** The waits of a pace in one line: "driving or charging every 15 s, awake 1 min, asleep 3 min". */
export const paceLine = (p) => (p ? `driving or charging every ${sec(p.driving_s ?? p.charging_s)}, awake ${sec(p.parked_s)}, asleep ${sec(p.asleep_s)}` : '');

/* ------------------------------------------------------------------ the part */

/**
 * The TeslaFi part: { el, load() }. load() reads everything and draws it; it rejects only when the page was
 * left. ctx.me decides: a watch account is told the part is for admins.
 */
export function teslafiPart(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const tz = ctx.tz;
  const slot = h('div', { class: 'tesla teslafi' });
  const el = section({ title: 'TeslaFi', note: 'a third live input, from TeslaFi’s own polls, admins only', id: 'teslafi', cls: 'si-sec tesla-sec' }, slot);
  let over = null, accounts = [], status = [], cars = [], loadErr = null;
  const carName = (cid) => (cars.find((c) => c.id === cid) || {}).name || cid;

  async function load() {
    if (!admin) { draw(); return; }
    try {
      const [o, a, s, c] = await Promise.all([
        ctx.api.get('integrations/teslafi'),
        ctx.api.get('integrations/teslafi/accounts'),
        ctx.api.get('integrations/teslafi/status').catch((e) => { if (api.isAbort(e)) throw e; return { items: [] }; }),
        ctx.api.get('cars', { sort: 'name', limit: 500 }).catch((e) => { if (api.isAbort(e)) throw e; return { items: [] }; }),
      ]);
      over = o; accounts = a.items || []; status = s.items || []; cars = c.items || []; loadErr = null;
    } catch (e) {
      if (api.isAbort(e)) throw e;
      loadErr = e;
    }
    if (ctx.alive()) draw();
  }
  /** Read everything again and draw it; focus goes back to the control named focusKey (data-tf), as in the Tesla part. */
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
    const r = t.getBoundingClientRect();
    if (!(r.bottom > 0 && r.top < innerHeight)) t.scrollIntoView({ block: 'nearest' });
  }
  const tf = (node, key) => { node.dataset.tf = key; return node; };
  const flashDone = (text) => ctx.flash(text);

  /* ---------------- drawing */
  function draw() {
    if (!admin) {
      replace(slot, h('p', { class: 'si-lead' }, 'Sentry Mate can also read TeslaFi’s own polls of a car, with a TeslaFi token, as a third live input. Only admins see and change it.'));
      return;
    }
    if (loadErr) { replace(slot, errorView(loadErr, { title: 'The TeslaFi settings could not be loaded.', retry: () => again(), home: false, eyebrow: 'TeslaFi' })); return; }
    if (!accounts.length) {
      replace(slot,
        h('p', { class: 'si-lead' },
          'If you pay for TeslaFi, Sentry Mate can read TeslaFi’s own polls of your car with your TeslaFi token; ',
          h('strong', null, 'until a token is added the server makes no connection to TeslaFi at all.')),
        addPart());
      return;
    }
    replace(slot,
      h('p', { class: 'si-lead' },
        'Sentry Mate reads TeslaFi’s own polls of your car from TeslaFi’s API, with your TeslaFi token, as a third live input beside SMO and the Tesla API. It only reads: it never wakes a car or sends it a command, and no page ever shows the token. Where SMO or the Tesla API cover the same time, theirs win.'),
      overview(),
      h('div', { class: 'tesla-accounts' }, accounts.map(accountCard)),
      addPart(),
      statusPart());
  }

  function overview() {
    const caps = over.polls || {};
    const poll = over.poll || 'normal';
    const pace = choice({
      label: 'How often TeslaFi is read', labelHidden: true, name: 'teslafi-poll', value: poll, options: PACE,
      onchange: async (v) => {
        if (v === poll) return;
        const out = await ask(ctx, {
          title: `Read TeslaFi ${v === 'often' ? 'more often' : v === 'gentle' ? 'less often' : 'at the normal pace'}?`,
          text: `${PACE_WHY[v]} This changes how often every TeslaFi account is read. TeslaFi’s own waits (Retry-After) and its errors are always respected, and the car is never woken.`,
          yes: 'Change it',
          run: (creds) => ctx.api.patch('integrations/teslafi', { version: over.version, poll: v, ...creds }),
        });
        if (out) flashDone('How often TeslaFi is read was changed.'); else pace.set(poll);
        await again('fi-pace');
      },
    });
    const paceRows = [
      ['Driving', caps.driving_s], ['Charging', caps.charging_s], ['Parked and awake', caps.parked_s], ['Asleep', caps.asleep_s],
      ['A car SMO or the Tesla API sends live', caps.other_live_s],
    ];
    const lastErr = over.last_error;
    return h('div', { class: 'tesla-over' },
      h('div', { class: 'figs tesla-figs' },
        figure('The input', over.enabled ? 'On' : 'Off', over.enabled
          ? `${formatNumber(over.accounts)} ${over.accounts === 1 ? 'account' : 'accounts'}, ${formatNumber(over.cars)} ${over.cars === 1 ? 'car' : 'cars'} read`
          : 'off until a token is added'),
        figure('Requests today', formatNumber(over.polls_today || 0), `at most ${formatNumber(caps.daily_cap || 1500)} a day per account, one every 10 s`),
        figure('Connection', over.tls ? `TLS ${over.tls}` : '—', 'to www.teslafi.com on 443 only'),
        figure('Last error', lastErr ? errWords(lastErr) : 'None',
          lastErr ? [lastErr.at_ms ? ago(lastErr.at_ms) : null, lastErr.account ? ` · ${accountLabel(lastErr.account)}` : null] : 'no error from TeslaFi')),
      h('div', { class: 'si-two tesla-two' },
        h('div', null,
          h('h3', { class: 'si-sub' }, 'TeslaFi hosts', h('span', { class: 'label-note' }, 'for a firewall')),
          over.hosts && over.hosts.length
            ? h('ul', { class: 'tesla-hosts' }, over.hosts.map((x) => h('li', null, code(x))))
            : h('p', { class: 'si-quiet' }, 'None: with no token the server talks to no TeslaFi host. Adding one uses www.teslafi.com.'),
          h('p', { class: 'si-quiet' }, 'Only TeslaFi’s latest poll and its last poll of the car awake are asked for, the token in a header, never in an address.')),
        h('div', null,
          h('h3', { class: 'si-sub' }, 'How often', h('span', { class: 'label-note' }, `“${(PACE.find(([k]) => k === poll) || [0, poll])[1]}” now`)),
          tf(pace, 'fi-pace'),
          h('p', { class: 'si-quiet' }, `Now: ${paceLine(caps)}.`),
          h('dl', { class: 'tesla-pace' }, paceRows.map(([n, s]) => h('div', { class: 'tesla-pace-row' }, h('dt', null, n), h('dd', null, s == null ? '—' : `every ${sec(s)}`)))),
          h('p', { class: 'si-quiet' }, 'Often: as fast as TeslaFi itself refreshes (15 s driving or charging, 1 min awake, 3 min asleep). Normal: 1 min, 3 min, 10 min. Gentle: 2 min, 5 min, 20 min. TeslaFi’s Retry-After and its errors are always respected; these reads come from TeslaFi’s own database and never wake the car.'))));
  }
  const accountLabel = (id) => (accounts.find((a) => a.id === id) || {}).label || id;

  /* ---------------- one account */
  function accountCard(a) {
    const st = STATUS[a.status] || [a.status, null, ''];
    const path = api.apiPath`integrations/teslafi/accounts/${a.id}`;
    const patch = (body, opts) => ask(ctx, { ...opts, run: (creds) => ctx.api.patch(path, { version: a.version, ...body, ...creds }) });
    const pauseBtn = tf(button(a.paused ? 'Resume' : 'Pause', {
      onclick: async () => {
        const out = await patch({ paused: !a.paused }, {
          title: a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`,
          text: a.paused ? 'Its matched car is read from TeslaFi again from now on.' : 'Nothing is asked of TeslaFi for this account until you resume it. The history already stored stays.',
          yes: a.paused ? 'Resume' : 'Pause',
        });
        if (out) flashDone(a.paused ? `${a.label} is read again.` : `${a.label} is paused.`);
        await again(`fi-pause-${a.id}`, `teslafi-${a.id}`);
      },
    }), `fi-pause-${a.id}`);
    const zoneBtn = tf(button('Time zone', {
      onclick: async () => {
        const out = await ask(ctx, {
          title: `TeslaFi’s time zone for ${a.label}`, yes: 'Change it',
          text: 'The zone TeslaFi shows this account’s times in (TeslaFi, Settings, Account). Empty: the server’s own zone.',
          extra: [field({ label: 'Time zone', name: 'time_zone', value: a.time_zone || '', hint: 'An IANA zone such as America/Chicago or Europe/Berlin.', attrs: { spellcheck: 'false', autocomplete: 'off' } })],
          run: (creds, v) => ctx.api.patch(path, { version: a.version, time_zone: String(v.time_zone || '').trim() || null, ...creds }),
        });
        if (out) flashDone(`${a.label}: the time zone was changed.`);
        await again(`fi-zone-${a.id}`, `teslafi-${a.id}`);
      },
    }), `fi-zone-${a.id}`);
    const tokenBtn = tf(button('Replace the token', {
      kind: a.status === 'token_refused' ? 'primary' : 'quiet', onclick: async () => {
        const out = await ask(ctx, {
          title: `A new TeslaFi token for ${a.label}`, yes: 'Check it with TeslaFi and keep it',
          text: 'Copy the token from TeslaFi (Settings, TeslaFi API). The server checks it with TeslaFi before anything is kept, and never shows it again.',
          extra: [field({ label: 'TeslaFi token', name: 'token', type: 'password', attrs: { spellcheck: 'false', autocomplete: 'off', required: true } })],
          run: (creds, v) => {
            if (!String(v.token || '').trim()) throw Object.assign(new Error('Paste the token.'), { fields: { token: 'Paste the TeslaFi token.' } });
            return ctx.api.patch(path, { version: a.version, token: String(v.token).trim(), ...creds });
          },
        });
        if (out) flashDone(`${a.label} has a new token; its car is read again.`);
        await again(`fi-token-${a.id}`, `teslafi-${a.id}`);
      },
    }), `fi-token-${a.id}`);
    const removeBtn = tf(button('Remove', {
      kind: 'danger', onclick: async () => {
        const out = await ask(ctx, {
          title: `Remove ${a.label}?`, yes: 'Remove the account', danger: true,
          text: 'Its car stops being read from TeslaFi at once and the sealed file with its token is deleted. The history already stored stays, marked as data from TeslaFi.',
          run: (creds) => ctx.api.del(path, { headers: pwHeaders(creds) }),
        });
        if (out) flashDone(`${a.label} was removed.`);
        await again(`fi-remove-${a.id}`, 'teslafi');
      },
    }), `fi-remove-${a.id}`);
    const err = a.last_error;
    const facts = [
      ['Status', h('span', null, chip(st[0], { kind: st[1] }), h('span', { class: 'tesla-fact-note' }, st[2]))],
      ['Now', h('span', null, (MODE[a.mode] || [a.mode || '—'])[0], h('span', { class: 'tesla-fact-note' }, (MODE[a.mode] || [null, ''])[1]))],
      ['Last good read', a.last_ok_ms ? h('span', null, ago(a.last_ok_ms), h('span', { class: 'tesla-fact-note' }, formatDateTime(a.last_ok_ms, tz, { now: api.now() }))) : quiet('none yet')],
      ['Last error', err ? h('span', null, errWords(err), h('span', { class: 'tesla-fact-note' }, [err.message && LAST_ERROR[err.code] ? `TeslaFi: ${err.message}. ` : '', err.at_ms ? formatDateTime(err.at_ms, tz, { now: api.now() }) : ''])) : quiet('none')],
      ['Requests today', formatNumber(a.polls_today || 0)],
      ['TeslaFi’s time zone', a.time_zone || quiet('the server’s own')],
      ['Added', formatDateTime(a.added_ms, tz, { now: api.now() })],
      ['Talks to', h('span', { class: 'tesla-hostlist' }, (a.hosts || []).map((x) => code(x)), a.tls ? ` TLS ${a.tls}` : null)],
    ];
    return h('article', { class: ['tesla-account', `is-${a.status === 'token_refused' ? 'error' : a.status}`], id: `teslafi-${a.id}`, tabindex: '-1' },
      h('div', { class: 'tesla-account-head' },
        h('div', null,
          h('p', { class: 'eyebrow' }, `TeslaFi account ${a.id}`),
          h('h3', { class: 'tesla-account-name' }, a.label)),
        h('div', { class: 'tesla-actions' }, pauseBtn, zoneBtn, tokenBtn, removeBtn)),
      a.status === 'token_refused' ? h('p', { class: 'tesla-alert' }, icon('warning'), 'TeslaFi refused this token: a wrong token, or the TeslaFi subscription ended. Its car is not read, and nothing is asked of TeslaFi, until you give a new token (Replace the token).') : null,
      a.status === 'error' ? h('p', { class: 'tesla-alert' }, icon('warning'), `TeslaFi answers with errors for now: ${errWords(err) || 'no reason given'}. The server tries again by itself, waiting longer each time.`) : null,
      h('dl', { class: 'tesla-facts' }, facts.map(([n, v]) => h('div', { class: 'tesla-fact' }, h('dt', null, n), h('dd', null, v)))),
      carsPart(a));
  }

  /* ---------------- an account's cars */
  function carsPart(a) {
    const list = a.cars || [];
    const chosen = new Map(list.map((c) => [c.vehicle, c.match ?? c.suggested ?? '']));
    const takenBy = new Map();
    for (const o of accounts) if (o.id !== a.id) for (const c of o.cars || []) if (c.match && c.match !== 'new' && c.match !== 'leave_out') takenBy.set(c.match, o.label);
    const nameOf = (c) => c.display_name || c.vehicle;
    const save = tf(button('Save the match', {
      kind: 'primary', onclick: async () => {
        const items = list.map((c) => ({ vehicle: c.vehicle, match: chosen.get(c.vehicle) || null }));
        const changes = items.filter((x) => x.match !== (list.find((c) => c.vehicle === x.vehicle).match ?? null));
        if (!changes.length) { ctx.flash('Nothing changed in the match.', { kind: 'info' }); return; }
        const out = await ask(ctx, {
          title: 'Save the match?', yes: 'Save',
          text: 'A car matched to a car here, or to a new car, is read from TeslaFi from now on; one left out or not decided is not read.',
          run: (creds) => ctx.api.put(api.apiPath`integrations/teslafi/accounts/${a.id}/cars`, { version: a.version, items: changes, ...creds }),
        });
        if (out) flashDone('The match was saved.');
        await again(`fi-save-${a.id}`, `teslafi-${a.id}`);
      },
    }), `fi-save-${a.id}`);
    const picker = (c) => {
      const mine = chosen.get(c.vehicle) || '';
      const sel = h('select', { class: 'field-select tesla-match', 'aria-label': `Match ${nameOf(c)}` },
        h('option', { value: '' }, 'Not decided: not read'),
        h('optgroup', { label: 'A car here' }, cars.map((x) => {
          const other = takenBy.get(x.id);
          return h('option', { value: x.id, disabled: !!other && mine !== x.id }, `${x.name}${c.suggested === x.id ? ' (suggested)' : ''}${other ? ` (read through ${other})` : ''}`);
        })),
        h('option', { value: 'new' }, 'A new car'),
        h('option', { value: 'leave_out' }, 'Leave it out'));
      sel.value = mine;
      sel.addEventListener('change', () => { chosen.set(c.vehicle, sel.value); });
      return h('span', { class: 'tesla-cell' }, h('span', { class: 'tesla-cell-label', 'aria-hidden': 'true' }, 'Is the car'), sel);
    };
    const labelled = (label, v) => h('span', { class: 'tesla-cell' }, h('span', { class: 'tesla-cell-label', 'aria-hidden': 'true' }, label), v);
    const t = dataTable(ctx, {
      rows: list, itemName: ['car', 'cars'], label: `Cars of ${a.label}`, csv: false, cls: 'tesla-cars-table',
      caption: 'The car TeslaFi polls for this token; choose which car here it is',
      columns: [
        { key: 'name', label: 'TeslaFi’s name', th: true, cls: 'tesla-c-name', cell: (c) => h('span', null, h('span', { class: 'cell-main' }, nameOf(c)), h('span', { class: 'cell-sub' }, [c.year, c.model].filter(Boolean).join(' '), ' · ', h('span', { class: 'mono' }, c.vin_prefix || ''))) },
        { key: 'match', label: 'Is the car', cls: 'tesla-c-match', cell: picker },
        { key: 'state', label: 'TeslaFi says', cls: 'tesla-c-state', cell: (c) => labelled('TeslaFi says', c.car_state || (c.current ? '—' : 'not polled now')) },
        { key: 'mode', label: 'What the server does', cls: 'si-wrap tesla-c-mode', cell: (c) => labelled('What the server does', h('span', null,
          h('span', { class: 'cell-main' }, c.match == null || c.match === 'leave_out' ? 'Not read' : (MODE[a.mode] || [a.mode])[0]),
          h('span', { class: 'cell-sub' }, c.match == null && c.suggested ? `Suggested: ${carName(c.suggested)}, the same VIN prefix; save the match to read it.`
            : c.match == null ? 'Not read until you choose which car it is.' : c.match === 'leave_out' ? 'Left out.' : (MODE[a.mode] || [null, ''])[1]))) },
      ],
      empty: () => empty({ title: 'TeslaFi names no car for this token yet.' }),
    });
    t.load?.();
    return h('div', { class: 'tesla-cars' }, h('div', { class: 'si-sub-head' }, h('h4', { class: 'tesla-cars-title' }, 'Its car'), list.length ? save : null), t.el);
  }

  /* ---------------- adding a token */
  function addPart() {
    const label = field({ label: 'A name for it', name: 'label', value: 'TeslaFi', hint: 'Your own name, shown on these pages.', attrs: { maxlength: 64, required: true } });
    const token = field({ label: 'TeslaFi token', name: 'token', type: 'password', hint: 'On TeslaFi: Settings, TeslaFi API. Copy the token shown there.', attrs: { spellcheck: 'false', autocomplete: 'off', required: true } });
    const zone = field({ label: 'TeslaFi’s time zone (optional)', name: 'time_zone', hint: 'The zone TeslaFi shows your times in, such as America/Chicago. Empty: the server’s own.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
    const f = form({
      fields: [label, token, zone], submitLabel: 'Check it with TeslaFi and add it', busyLabel: 'Asking…', cls: 'teslafi-add-form',
      onsubmit: async (v) => {
        const tok = String(v.token || '').trim();
        if (!tok) throw Object.assign(new Error('Paste the token.'), { fields: { token: 'Paste the TeslaFi token (TeslaFi, Settings, TeslaFi API).' } });
        const body = { label: String(v.label || '').trim() || 'TeslaFi', token: tok };
        if (String(v.time_zone || '').trim()) body.time_zone = String(v.time_zone).trim();
        const out = await ask(ctx, {
          title: 'Add the TeslaFi token', yes: 'Add it',
          text: 'The server checks the token with TeslaFi before anything is kept (this can take about 10 seconds), keeps it sealed and never shows it again.',
          run: (creds) => ctx.api.post('integrations/teslafi/accounts', { ...body, ...creds }),
        });
        if (!out) return;
        flashDone(`${out.label} was added. Choose which car it is under “Its car”.`);
        await again();
        location.hash = `#teslafi-${out.id}`;
        document.getElementById(`teslafi-${out.id}`)?.focus({ preventScroll: true });
      },
    });
    return h('div', { class: 'tesla-add' },
      h('h3', { class: 'si-sub' }, accounts.length ? 'Add another TeslaFi token' : 'Add a TeslaFi token', h('span', { class: 'label-note' }, 'one per car')),
      h('div', { class: 'tesla-way' },
        h('p', null, 'The token is on TeslaFi, under Settings, TeslaFi API. It is checked with TeslaFi, kept sealed on this server, and never shown again. Your password is asked for when you add it.'),
        f));
  }

  /* ---------------- what the server does for each car */
  function statusPart() {
    if (!status.length) return null;
    const vName = (s) => { const a = accounts.find((x) => x.id === s.account); const c = a && (a.cars || []).find((x) => x.vehicle === s.vehicle); return (c && c.display_name) || s.vehicle || '—'; };
    const t = dataTable(ctx, {
      rows: status, itemName: ['TeslaFi account', 'TeslaFi accounts'], label: 'What the server does for each TeslaFi account', csv: false,
      columns: [
        { key: 'car', label: 'Car', th: true, cell: (s) => h('span', null, s.car ? h('a', { class: 'cell-main', href: state.buildUrl('car', { car: s.car }) }, carName(s.car)) : h('span', { class: 'cell-main' }, vName(s)), h('span', { class: 'cell-sub' }, `${vName(s)} on ${accountLabel(s.account)}`)) },
        { key: 'mode', label: 'Now', cls: 'si-wrap', cell: (s) => h('span', null, h('span', { class: 'cell-main' }, (MODE[s.mode] || [s.mode])[0]), h('span', { class: 'cell-sub' }, s.car_state ? `TeslaFi: ${s.car_state}` : '')) },
        { key: 'last', label: 'Last asked', cell: (s) => (s.last_request_ms ? ago(s.last_request_ms) : quiet('not yet')) },
        { key: 'next', label: 'Next', cell: (s) => (s.next_request_ms ? h('span', null, formatTime(s.next_request_ms, tz), s.mode === 'backoff' ? h('span', { class: 'cell-sub' }, 'waiting after errors') : null) : quiet('not asked')) },
        { key: 'data', label: 'Newest poll', cell: (s) => (s.last_data_ms ? ago(s.last_data_ms) : quiet('none')) },
        { key: 'polls', label: 'Requests today', num: true, cell: (s) => formatNumber(s.polls_today || 0) },
        { key: 'err', label: 'Last error', cls: 'si-wrap', cell: (s) => (s.last_error ? errWords(s.last_error) : quiet('none')) },
      ],
    });
    t.load?.();
    return h('div', { class: 'tesla-status' }, h('h3', { class: 'si-sub' }, 'Each TeslaFi account now', h('span', { class: 'label-note' }, 'what the server does for it')), t.el);
  }

  return { el, load };
}

/** The part a #teslafi or #teslafi-<id> address points at, opened into view once the part is drawn. */
export function showTeslafiHash() {
  const id = (location.hash || '').slice(1);
  if (!/^teslafi(-|$)/.test(id)) return;
  const t = document.getElementById(id) || document.getElementById('teslafi');
  if (!t) return;
  if (!t.hasAttribute('tabindex')) t.setAttribute('tabindex', '-1');
  t.focus({ preventScroll: true });
  t.scrollIntoView({ block: 'start' });
}
