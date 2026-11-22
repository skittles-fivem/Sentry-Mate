// Sentry Mate, "Logbook" UI pack: Settings: Integrations, the TeslaFi part (admins only).
//
// The TeslaFi input, for owners who pay for TeslaFi: the server reads TeslaFi's own polls of the car with the
// owner's TeslaFi token, a third live input beside SMO and the Tesla API, labelled "Data from TeslaFi". Off until an
// admin adds a token. It only reads (TeslaFi's latest poll and its last poll awake): no wake, no command, and no page
// ever shows the token. This part shows the input's state (on or off, its host, the pace, requests today, TLS), adds
// a token, and per account its status, the car TeslaFi polls and which car here it is, the zone TeslaFi's times are
// in, pause and resume, a new token, removing it, and its errors. Every change asks for the admin's own password
// again (and the two-step code), as the Tesla account part does.
//
// API: GET integrations/teslafi, GET / POST integrations/teslafi/accounts, PATCH / DELETE
// integrations/teslafi/accounts/{id}, PUT integrations/teslafi/accounts/{id}/cars, GET integrations/teslafi/status,
// GET cars (the match picker).

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatDateTime, formatTime, formatDuration } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { section, figure, chip, button, icon, ago, errorView, empty } from '../core/ui.js';
import { field } from '../core/form.js';
import { dataTable } from '../core/table.js';
import { askAgain } from './integrations-tesla.js';
import { zoneLabel } from '../core/format.js';

/* ------------------------------------------------------------------ words */

const STATUS = {
  checking: ['Checking', null, 'Just added: waiting for TeslaFi’s first answer.'],
  ok: ['Reading', 'on', 'TeslaFi answers; the matched car is read.'],
  paused: ['Paused', null, 'Nothing is asked of TeslaFi for this account until it is resumed.'],
  token_refused: ['Token refused', 'warn', 'TeslaFi no longer takes this token (a wrong token, or the TeslaFi subscription ended). Nothing more is asked until you give it a new one.'],
  error: ['Error', 'warn', 'TeslaFi answered with errors; the server tries again by itself, more slowly each time.'],
};
const MODE = {
  off: ['Off', 'Not read: no car matched, paused, or the token was refused.'],
  asleep: ['Asleep', 'TeslaFi says the car sleeps: asked every 10 minutes.'],
  idle: ['Parked', 'TeslaFi says the car is parked and awake: asked every 3 minutes.'],
  charging: ['Charging', 'Asked once a minute.'],
  driving: ['Driving', 'Asked once a minute.'],
  backoff: ['Waiting after errors', 'TeslaFi answered with errors, or the day’s requests are used up: nothing is asked until the next try.'],
};
const ERROR_WORDS = {
  teslafi_refused: 'TeslaFi did not take this token: it may be mistyped, or the TeslaFi subscription ended. Copy it again from TeslaFi (Settings, Account, API). Nothing was kept or changed.',
  teslafi_unreachable: 'TeslaFi could not be reached, or answered with an error. Nothing was kept or changed; try again in a few minutes.',
  account_exists: 'This token’s car is already read through another TeslaFi account here. Give that one the new token instead.',
  car_taken: 'That car here already gets TeslaFi’s data from another TeslaFi car. Leave it out there first.',
  no_such_teslafi_account: 'This TeslaFi account is no longer here: another admin may have removed it.',
  no_such_account: 'This TeslaFi account is no longer here: another admin may have removed it.',
  stale_version: 'Someone changed this meanwhile; it is shown fresh now. Make your change again.',
  password_needed: 'Type your own Sentry Mate password again.',
  wrong_password: 'That is not your Sentry Mate password.',
  wrong_code: 'Not the right two-step code. Type the one your authenticator app shows now.',
  wrong_two_step: 'Not the right two-step code. Type the one your authenticator app shows now.',
  invalid: 'A value was not taken; see the field.',
  bad_input: 'A value was not taken; see the field.',
  not_allowed: 'Only an admin may change the TeslaFi input.',
};
const LAST_ERROR = {
  token_refused: 'TeslaFi refused the token',
  teslafi_refused: 'TeslaFi refused the token',
  teslafi_unreachable: 'TeslaFi could not be reached',
  rate_limited: 'TeslaFi asked to slow down',
  daily_cap: 'The day’s 1,500 requests are used up',
  server_error: 'TeslaFi answered with a server error',
};
export const teslafiErrorText = (e) => (e && ERROR_WORDS[e.code]) || (e && e.message) || 'Something went wrong.';

const code = (t) => h('code', { class: 'si-code' }, t);
const quiet = (t) => h('span', { class: 'si-quiet' }, t);
const zones = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();

/** The zone field, with the list of zones to pick from. */
function zoneField(value, hint) {
  const f = field({ label: 'TeslaFi’s time zone', name: 'time_zone', value: value || '', hint, attrs: { list: 'tf-zones', spellcheck: 'false', autocomplete: 'off' } });
  f.append(h('datalist', { id: 'tf-zones' }, zones.map((z) => h('option', { value: z }))));
  return f;
}
const zoneValue = (v) => { const z = String(v || '').trim(); return z || null; };
const tokenField = (label = 'TeslaFi token') => field({
  label, name: 'token', type: 'password',
  hint: 'From TeslaFi: Settings, Account, API (TeslaFi API token). Kept sealed on this server and never shown again.',
  attrs: { spellcheck: 'false', autocomplete: 'off', required: true },
});

/* ------------------------------------------------------------------ the part */

/** The TeslaFi part: { el, load() }. A watch account is told the part is for admins. */
export function teslafiPart(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const tz = ctx.tz;
  const slot = h('div', { class: 'tesla teslafi' });
  const el = section({ title: 'TeslaFi', note: 'a third live input, for TeslaFi subscribers, admins only', id: 'teslafi', cls: 'si-sec tesla-sec' }, slot);
  let over = null, accounts = [], status = [], cars = [], loadErr = null;
  const carName = (cid) => (cars.find((c) => c.id === cid) || {}).name || cid;
  const ask = (opts) => askAgain(ctx, { input: 'the TeslaFi input', errorText: teslafiErrorText, ...opts });

  async function load() {
    if (!admin) { draw(); return; }
    try {
      const [o, a, s, c] = await Promise.all([
        ctx.api.get('integrations/teslafi'),
        ctx.api.get('integrations/teslafi/accounts'),
        ctx.api.get('integrations/teslafi/status').catch((e) => { if (api.isAbort(e)) throw e; return { items: [] }; }),
        ctx.api.get('cars', { sort: 'name', limit: 500 }, { maxAgeMs: 60000 }).catch((e) => { if (api.isAbort(e)) throw e; return { items: [] }; }),
      ]);
      over = o; accounts = a.items || []; status = s.items || []; cars = c.items || []; loadErr = null;
    } catch (e) {
      if (api.isAbort(e)) throw e;
      loadErr = e;
    }
    if (ctx.alive()) draw();
  }
  /** Read again and draw; the control that opened the dialog (data-tf) gets the focus back when it was lost. */
  async function again(focusKey = null, fallbackId = null) {
    await load();
    if (!ctx.alive() || !focusKey) return;
    const a = document.activeElement;
    if (a && a !== document.body && document.contains(a)) return;
    let t = slot.querySelector(`[data-tf="${CSS.escape(focusKey)}"]`);
    if (!t && fallbackId) t = document.getElementById(fallbackId);
    if (!t) { t = el; if (!t.hasAttribute('tabindex')) t.setAttribute('tabindex', '-1'); }
    t.focus({ preventScroll: true });
    const r = t.getBoundingClientRect();
    if (!(r.bottom > 0 && r.top < innerHeight)) t.scrollIntoView({ block: 'nearest' });
  }
  const tf = (node, key) => { node.dataset.tf = key; return node; };

  function draw() {
    if (!admin) {
      replace(slot, h('p', { class: 'si-lead' }, 'Sentry Mate can also read a car from TeslaFi, with the owner’s TeslaFi token, as a third live input beside SMO and the Tesla API (“Data from TeslaFi”). Only admins see and change it; no page ever shows the token.'));
      return;
    }
    if (loadErr) { replace(slot, errorView(loadErr, { title: 'The TeslaFi settings could not be loaded.', retry: () => again(), home: false, eyebrow: 'TeslaFi' })); return; }
    replace(slot,
      h('p', { class: 'si-lead' },
        'If you pay for TeslaFi, Sentry Mate can read the polls TeslaFi already makes of your car (about one a minute while it is awake). They come from TeslaFi’s own database, so reading them never wakes the car and never counts against TeslaFi’s command limit. Where SMO or the Tesla API cover the same time, their values win. ',
        over.enabled ? null : h('strong', null, 'Off until a token is added: until then the server makes no connection to TeslaFi at all.')),
      overview(),
      accounts.length ? h('div', { class: 'tesla-accounts' }, accounts.map(accountCard)) : null,
      addPart(),
      statusPart());
  }

  function overview() {
    const p = over.polls || {};
    const every = (n) => (n == null ? '—' : `every ${n >= 60 ? formatDuration(n * 1000) : `${n} s`}`);
    const paceRows = [
      ['Driving', p.driving_s], ['Charging', p.charging_s], ['Parked and awake', p.parked_s], ['Asleep', p.asleep_s],
      ['While SMO or the Tesla API is live', p.other_live_s],
    ];
    const err = over.last_error;
    return h('div', { class: 'tesla-over' },
      h('div', { class: 'figs tesla-figs' },
        figure('The input', over.enabled ? 'On' : 'Off', over.enabled
          ? `${formatNumber(over.accounts)} ${over.accounts === 1 ? 'account' : 'accounts'}, ${formatNumber(over.cars)} ${over.cars === 1 ? 'car' : 'cars'} read`
          : 'off until a token is added'),
        figure('Requests today', formatNumber(over.polls_today || 0), `at most ${formatNumber(p.daily_cap || 1500)} a day per account, one every 10 s`),
        figure('Connection', over.tls ? `TLS ${over.tls}` : 'None', over.tls ? `${formatNumber(over.clients_built || 0)} set up since the server started` : 'no connection made yet'),
        figure('Last error', err ? (LAST_ERROR[err.code] || err.message || err.code) : 'None',
          err ? [err.at_ms ? ago(err.at_ms) : null, err.account ? ` · ${accountLabel(err.account)}` : null] : 'no error from TeslaFi')),
      h('div', { class: 'si-two tesla-two' },
        h('div', null,
          h('h3', { class: 'si-sub' }, 'TeslaFi host', h('span', { class: 'label-note' }, 'for a firewall')),
          over.hosts && over.hosts.length
            ? h('ul', { class: 'tesla-hosts' }, over.hosts.map((x) => h('li', null, code(x))))
            : h('p', { class: 'si-quiet' }, 'None: with no token the server talks to no TeslaFi host. Adding one uses www.teslafi.com on port 443 only, the token in a header, never in an address.')),
        h('div', null,
          h('h3', { class: 'si-sub' }, 'How often', h('span', { class: 'label-note' }, 'set by the server')),
          h('dl', { class: 'tesla-pace' }, paceRows.map(([n, s]) => h('div', { class: 'tesla-pace-row' }, h('dt', null, n), h('dd', null, every(s))))),
          h('p', { class: 'si-quiet' }, 'Errors wait a minute, doubling up to 15 minutes; when TeslaFi asks to slow down, the server waits as long as it asks.'))));
  }
  const accountLabel = (id) => (accounts.find((a) => a.id === id) || {}).label || id;

  /* ---------------- one account */
  function accountCard(a) {
    const st = STATUS[a.status] || [a.status, null, ''];
    const patch = (body, opts) => ask({ ...opts, run: (creds) => ctx.api.patch(api.apiPath`integrations/teslafi/accounts/${a.id}`, { version: a.version, ...body, ...creds }) });
    const fid = `teslafi-${a.id}`;
    const pauseBtn = tf(button(a.paused ? 'Resume' : 'Pause', {
      onclick: async () => {
        const out = await patch({ paused: !a.paused }, {
          title: a.paused ? `Resume ${a.label}?` : `Pause ${a.label}?`,
          text: a.paused ? 'Its matched car is read from TeslaFi again from now on.' : 'Nothing is asked of TeslaFi for this account until you resume it. The history already stored stays.',
          yes: a.paused ? 'Resume' : 'Pause',
        });
        if (out) ctx.flash(a.paused ? `${a.label} is read again.` : `${a.label} is paused.`);
        await again(`pause-${a.id}`, fid);
      },
    }), `pause-${a.id}`);
    const renameBtn = tf(button('Rename', {
      onclick: async () => {
        const out = await ask({
          title: 'Rename the TeslaFi account', yes: 'Rename',
          extra: [field({ label: 'Name', name: 'label', value: a.label, hint: 'Your own name for it, shown on these pages.', attrs: { maxlength: 64, required: true } })],
          run: (creds, v) => ctx.api.patch(api.apiPath`integrations/teslafi/accounts/${a.id}`, { version: a.version, label: v.label, ...creds }),
        });
        if (out) ctx.flash(`Renamed to ${out.label}.`);
        await again(`rename-${a.id}`, fid);
      },
    }), `rename-${a.id}`);
    const zoneBtn = tf(button('Time zone', {
      onclick: async () => {
        const out = await ask({
          title: `TeslaFi’s time zone for ${a.label}`, yes: 'Save the zone',
          text: 'TeslaFi writes its times as wall-clock time without a zone. Type the zone set in TeslaFi (Settings, Account); leave it empty for the server’s default zone. Polls are read in the new zone at once, and none is dropped for seeming older.',
          extra: [zoneField(a.time_zone, 'A name such as America/Chicago. Type to see the list.')],
          run: (creds, v) => ctx.api.patch(api.apiPath`integrations/teslafi/accounts/${a.id}`, { version: a.version, time_zone: zoneValue(v.time_zone), ...creds }),
        });
        if (out) ctx.flash(out.time_zone ? `TeslaFi’s times are read in ${zoneLabel(out.time_zone)} now.` : 'TeslaFi’s times are read in the server’s default zone now.');
        await again(`zone-${a.id}`, fid);
      },
    }), `zone-${a.id}`);
    const tokenBtn = tf(button('Replace the token', {
      kind: a.status === 'token_refused' ? 'primary' : 'quiet', onclick: async () => {
        const out = await ask({
          title: `A new TeslaFi token for ${a.label}`, yes: 'Check it with TeslaFi and keep it',
          text: 'Paste the token TeslaFi shows now. The server checks it with TeslaFi before anything is changed: if TeslaFi refuses it, the old token and everything else stay as they are.',
          extra: [tokenField('New TeslaFi token')],
          run: (creds, v) => {
            if (!String(v.token || '').trim()) throw Object.assign(new Error('Paste the token.'), { fields: { token: 'Paste the TeslaFi token.' } });
            return ctx.api.patch(api.apiPath`integrations/teslafi/accounts/${a.id}`, { version: a.version, token: String(v.token).trim(), ...creds });
          },
        });
        if (out) ctx.flash(`${a.label} has a new token; its car is read again.`);
        await again(`token-${a.id}`, fid);
      },
    }), `token-${a.id}`);
    const removeBtn = tf(button('Remove', {
      kind: 'danger', onclick: async () => {
        const out = await ask({
          title: `Remove ${a.label}?`, yes: 'Remove the account', danger: true,
          text: 'Its car stops being read from TeslaFi at once and the sealed file with its token is deleted. The history already stored stays, marked as data from TeslaFi.',
          run: (creds) => ctx.api.del(api.apiPath`integrations/teslafi/accounts/${a.id}`, { headers: { 'X-Sentry-Mate-Password': creds.confirm_password, ...(creds.code ? { 'X-Sentry-Mate-Code': creds.code } : {}) } }),
        });
        if (out) ctx.flash(`${a.label} was removed.`);
        await again(`remove-${a.id}`, 'teslafi');
      },
    }), `remove-${a.id}`);
    const err = a.last_error;
    const m = MODE[a.mode] || [a.mode, ''];
    const facts = [
      ['Status', h('span', null, chip(st[0], { kind: st[1] }), h('span', { class: 'tesla-fact-note' }, st[2]))],
      ['Now', h('span', null, m[0], h('span', { class: 'tesla-fact-note' }, m[1]))],
      ['Time zone of its times', a.time_zone ? zoneLabel(a.time_zone) : h('span', null, 'The server’s default', h('span', { class: 'tesla-fact-note' }, 'none set for this account'))],
      ['Last good answer', a.last_ok_ms ? h('span', null, ago(a.last_ok_ms), h('span', { class: 'tesla-fact-note' }, formatDateTime(a.last_ok_ms, tz, { now: api.now() }))) : quiet('none yet')],
      ['Last error', err ? h('span', null, LAST_ERROR[err.code] || err.message || err.code, h('span', { class: 'tesla-fact-note' }, [err.message && LAST_ERROR[err.code] ? `${err.message} ` : '', err.at_ms ? formatDateTime(err.at_ms, tz, { now: api.now() }) : ''])) : quiet('none')],
      ['Requests today', h('span', null, formatNumber(a.polls_today || 0), h('span', { class: 'tesla-fact-note' }, `of ${formatNumber((over.polls || {}).daily_cap || 1500)} a day`))],
      ['Connection', h('span', null, a.tls ? `TLS ${a.tls}` : quiet('none yet'), h('span', { class: 'tesla-hostlist tesla-fact-note' }, (a.hosts || []).map((x) => code(x))))],
      ['Added', formatDateTime(a.added_ms, tz, { now: api.now() })],
    ];
    return h('article', { class: ['tesla-account', `is-${a.status === 'token_refused' ? 'error' : a.status}`], id: fid, tabindex: '-1' },
      h('div', { class: 'tesla-account-head' },
        h('div', null,
          h('p', { class: 'eyebrow' }, `TeslaFi account ${a.id}`),
          h('h3', { class: 'tesla-account-name' }, a.label)),
        h('div', { class: 'tesla-actions' }, a.status === 'token_refused' ? tokenBtn : null, pauseBtn, renameBtn, zoneBtn, a.status === 'token_refused' ? null : tokenBtn, removeBtn)),
      a.status === 'token_refused' ? h('p', { class: 'tesla-alert', role: 'note' }, icon('warning'), 'TeslaFi refused this account’s token: it may have been renewed on TeslaFi, or the subscription ended. Nothing is asked of TeslaFi until you give it a new token (Replace the token).') : null,
      a.status === 'error' ? h('p', { class: 'tesla-alert', role: 'note' }, icon('warning'), `TeslaFi answers with errors: ${err ? LAST_ERROR[err.code] || err.message : 'no reason given'}. The server tries again by itself.`) : null,
      h('dl', { class: 'tesla-facts' }, facts.map(([n, v]) => h('div', { class: 'tesla-fact' }, h('dt', null, n), h('dd', null, v)))),
      carsPart(a));
  }

  /* ---------------- the car TeslaFi polls, and which car here it is */
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
        const out = await ask({
          title: 'Save the match?', yes: 'Save',
          text: 'A TeslaFi car matched to a car here, or to a new car, is read from now on; one left out or not decided is not read.',
          run: (creds) => ctx.api.put(api.apiPath`integrations/teslafi/accounts/${a.id}/cars`, { version: a.version, items: changes, ...creds }),
        });
        if (out) ctx.flash('The match was saved.');
        await again(`save-${a.id}`, `teslafi-${a.id}`);
      },
    }), `save-${a.id}`);
    const labelled = (label, v) => h('span', { class: 'tesla-cell' }, h('span', { class: 'tesla-cell-label', 'aria-hidden': 'true' }, label), v);
    const picker = (c) => {
      const mine = chosen.get(c.vehicle) || '';
      const sel = h('select', { class: 'field-select tesla-match', 'aria-label': `Which car here is ${nameOf(c)}` },
        h('option', { value: '' }, 'Not decided: not read'),
        h('optgroup', { label: 'A car here' }, cars.map((x) => {
          const other = takenBy.get(x.id);
          return h('option', { value: x.id, disabled: !!other && mine !== x.id }, `${x.name}${c.suggested === x.id ? ' (suggested: same VIN prefix)' : ''}${other ? ` (read through ${other})` : ''}`);
        })),
        h('option', { value: 'new' }, 'A new car'),
        h('option', { value: 'leave_out' }, 'Leave it out'));
      sel.value = mine;
      sel.addEventListener('change', () => chosen.set(c.vehicle, sel.value));
      return labelled('Is the car', sel);
    };
    const t = dataTable(ctx, {
      rows: list, itemName: ['car', 'cars'], label: `Cars of ${a.label}`, csv: false, cls: 'tesla-cars-table',
      caption: 'The cars in TeslaFi’s polls on this account; choose which car here each one is',
      columns: [
        { key: 'name', label: 'TeslaFi’s name', th: true, cls: 'tesla-c-name', cell: (c) => h('span', null, h('span', { class: 'cell-main' }, nameOf(c)), h('span', { class: 'cell-sub' }, [c.year, c.model].filter(Boolean).join(' '), ' · ', h('span', { class: 'mono' }, c.vin_prefix || ''))) },
        { key: 'match', label: 'Is the car', cls: 'tesla-c-match', cell: picker },
        { key: 'state', label: 'TeslaFi says', cls: 'tesla-c-state', cell: (c) => labelled('TeslaFi says', c.current ? (c.car_state || '—') : quiet('not polled now')) },
        {
          key: 'read', label: 'Read here', cls: 'si-wrap tesla-c-mode',
          cell: (c) => labelled('Read here', h('span', null,
            h('span', { class: 'cell-main' }, c.match && c.match !== 'leave_out' ? (c.match === 'new' ? 'Into a new car' : `Into ${carName(c.match)}`) : 'Not read'),
            h('span', { class: 'cell-sub' }, c.match == null && c.suggested ? `Suggested: ${carName(c.suggested)}, the same VIN prefix. Save the match to read it.`
              : c.match == null ? 'Not read until you choose which car it is.' : c.match === 'leave_out' ? 'Left out on purpose.' : 'Its polls are labelled “Data from TeslaFi”.'))),
        },
      ],
      empty: () => empty({ title: 'TeslaFi named no car yet.', text: 'The server asks TeslaFi for the last poll of the car awake; the car shows here once TeslaFi names it.' }),
    });
    t.load?.();
    return h('div', { class: 'tesla-cars' }, h('div', { class: 'si-sub-head' }, h('h4', { class: 'tesla-cars-title' }, 'Its car'), list.length ? save : null), t.el);
  }

  /* ---------------- adding a token */
  function addPart() {
    const addBtn = button(accounts.length ? 'Add another TeslaFi token' : 'Add a TeslaFi token', {
      kind: accounts.length ? 'quiet' : 'primary', onclick: async () => {
        const out = await ask({
          title: 'Add a TeslaFi account', yes: 'Check it with TeslaFi and add it',
          text: 'The server asks TeslaFi once with the token before anything is kept (when TeslaFi’s latest poll names no car it asks for the last poll awake as well, so this can take about 10 seconds).',
          extra: [
            field({ label: 'A name for this account', name: 'label', value: 'TeslaFi', hint: 'Your own name for it, such as “Atlas on TeslaFi”.', attrs: { maxlength: 64, required: true } }),
            tokenField(),
            zoneField('', 'The zone set in TeslaFi (Settings, Account), such as America/Chicago: TeslaFi’s times have no zone. Empty: the server’s default zone.'),
          ],
          run: (creds, v) => {
            if (!String(v.token || '').trim()) throw Object.assign(new Error('Paste the token.'), { fields: { token: 'Paste the TeslaFi token.' } });
            const body = { label: v.label, token: String(v.token).trim(), ...creds };
            const z = zoneValue(v.time_zone);
            if (z) body.time_zone = z;
            return ctx.api.post('integrations/teslafi/accounts', body);
          },
        });
        if (out) {
          ctx.flash(`${out.label} was added. Choose which car it is under “Its car”.`);
          await again();
          const t = document.getElementById(`teslafi-${out.id}`);
          if (t) { t.focus({ preventScroll: true }); t.scrollIntoView({ block: 'start' }); }
        } else await again('add');
      },
    });
    tf(addBtn, 'add');
    return h('div', { class: 'tesla-add' },
      h('h3', { class: 'si-sub' }, accounts.length ? 'Add another TeslaFi account' : 'Add a TeslaFi account', h('span', { class: 'label-note' }, 'with its API token')),
      h('div', { class: 'tesla-way' },
        h('p', null, 'In TeslaFi, open Settings, Account, API and copy the TeslaFi API token. One token reads one car. The token is checked with TeslaFi, kept sealed on this server, sent only to www.teslafi.com in a header, and never shown again.'),
        addBtn));
  }

  /* ---------------- what the server does for each account now */
  function statusPart() {
    if (!status.length) return null;
    const t = dataTable(ctx, {
      rows: status, itemName: ['TeslaFi account', 'TeslaFi accounts'], label: 'What the server does for each TeslaFi account', csv: false,
      columns: [
        { key: 'car', label: 'Car', th: true, cell: (s) => h('span', null, s.car ? h('a', { class: 'cell-main', href: state.buildUrl('car', { car: s.car }) }, carName(s.car)) : h('span', { class: 'cell-main' }, 'No car matched'), h('span', { class: 'cell-sub' }, `on ${accountLabel(s.account)}`)) },
        { key: 'mode', label: 'Now', cls: 'si-wrap', cell: (s) => h('span', null, h('span', { class: 'cell-main' }, (MODE[s.mode] || [s.mode])[0]), h('span', { class: 'cell-sub' }, s.car_state ? `TeslaFi: ${s.car_state}` : '')) },
        { key: 'last', label: 'Last asked', cell: (s) => (s.last_request_ms ? ago(s.last_request_ms) : quiet('not yet')) },
        { key: 'next', label: 'Next', cell: (s) => (s.next_request_ms ? formatTime(s.next_request_ms, tz) : quiet('not asked')) },
        { key: 'data', label: 'Newest poll', cell: (s) => (s.last_data_ms ? ago(s.last_data_ms) : quiet('none')) },
        { key: 'polls', label: 'Requests today', num: true, cell: (s) => formatNumber(s.polls_today || 0) },
        { key: 'err', label: 'Last error', cls: 'si-wrap', cell: (s) => (s.last_error ? (LAST_ERROR[s.last_error.code] || s.last_error.message || s.last_error.code) : quiet('none')) },
      ],
    });
    t.load?.();
    return h('div', { class: 'tesla-status' }, h('h3', { class: 'si-sub' }, 'Each TeslaFi account now', h('span', { class: 'label-note' }, 'what the server does for it')), t.el);
  }

  return { el, load };
}
