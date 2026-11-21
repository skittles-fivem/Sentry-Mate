// The Tesla API input in words: what an account's status, a car's mode and Tesla's errors mean, and the
// status table (GET /integrations/tesla/status) that Settings: Integrations and Data health both show.
// Everything about Tesla accounts is admin only.

import * as state from '../shared/state.js';
import { h } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { tag } from './ui.js';
import { tracks } from './tracks.js';
import { emptyState } from './states.js';
import { ago, dateTime, DASH } from './fmt.js';
import { sig } from './setting.js';

/** An account's status: [word, signal tone, what it means]. */
export const ACCOUNT_STATUS = Object.freeze({
  checking: ['Checking', 'live', 'The server is asking Tesla for this account\'s cars for the first time.'],
  ok: ['Reading', 'ok', 'Tesla takes the sign-in; the cars matched below are read.'],
  paused: ['Paused', 'off', 'Paused here: no request goes to Tesla for this account until you resume it.'],
  sign_in_needed: ['Sign in again', 'bad', 'Tesla stopped taking the sign-in (the password changed, or the token ran out or was used elsewhere). Its cars are not read until you sign in again or paste a new refresh token.'],
  error: ['Tesla refuses it', 'warn', 'Tesla answers this account with an error, so the server waits before it asks again.'],
});

/** What the server does for a car now: [word, tone, what it means]. */
export const CAR_MODE = Object.freeze({
  off: ['Off', 'off', 'Not read: the car is left out, not matched yet, or its account is paused or needs a sign-in.'],
  watching_list: ['Watching the car list', 'live', 'Only the account\'s car list is read, which never wakes a car; values come once it is online.'],
  online_idle: ['Online, parked', 'live', 'Online and parked: read at the parked pace.'],
  letting_sleep: ['Letting it sleep', 'off', 'The car may fall asleep: the server stops asking for its values so it is not kept awake.'],
  charging: ['Charging', 'ok', 'Read at the charging pace.'],
  driving: ['Driving', 'ok', 'Read at the driving pace (or the streaming connection while driving).'],
  backoff: ['Waiting after an error', 'warn', 'Tesla answered with an error, so the server waits before it asks again.'],
});

/** What Tesla says about the car itself. */
export const CAR_STATE = Object.freeze({ online: 'Online', asleep: 'Asleep', offline: 'Offline', unknown: 'Not known' });

/** When the car's position is asked for. */
export const LOCATION = Object.freeze([
  ['driving_or_charging', 'While driving or charging'],
  ['always', 'Always'],
  ['never', 'Never'],
]);
export const LOCATION_WORD = Object.freeze(Object.fromEntries(LOCATION));

/** The poll pace choices. */
export const POLL = Object.freeze([['often', 'Often'], ['normal', 'Normal'], ['gentle', 'Gentle']]);

/** Every error the Tesla routes give, in plain words. */
export const TESLA_ERRORS = Object.freeze({
  tesla_refused: 'Tesla refused the sign-in or the token. It may have run out or been used already, or the Tesla password changed. Sign in again.',
  tesla_unreachable: 'Tesla could not be reached, or answered with an error. Nothing was changed; try again in a few minutes.',
  account_exists: 'The cars of this Tesla account are already read through another account here.',
  car_taken: 'That car here is already matched to another Tesla car. Pick another, a new car, or leave this one out.',
  too_many_sign_ins: 'Too many sign-ins are open at once. Finish one, or wait ten minutes for them to run out.',
  no_such_sign_in: 'That sign-in ran out (ten minutes) or was used already. Start a new one.',
  no_such_tesla_account: 'This Tesla account is gone: someone removed it meanwhile. The page shows the accounts as they are now.',
  no_such_vehicle: 'Tesla no longer lists that car on this account.',
  password_needed: 'Type your own password again to change this.',
  wrong_password: 'That is not your password. Type the one you sign in with.',
  wrong_two_step: 'That two-step code is not right. Type the one your authenticator app shows now.',
  wrong_code: 'That two-step code is not right, or it is missing. Type the one your authenticator app shows now.',
  stale_version: 'Someone changed this account meanwhile. The page shows their change now; try again.',
  throttled: 'Too many tries. Wait a minute, then try again.',
  not_allowed: 'Only an admin can change the Tesla account.',
});

/** Words for an error from a Tesla route (falls back to the server's own message). */
export const teslaWords = (e) => TESLA_ERRORS[e?.code] || e?.message || 'That did not work. Try again.';

/** Words for an account's or a car's last_error code. */
export const LAST_ERROR = Object.freeze({
  sign_in_needed: 'Tesla stopped taking the refresh token',
  tesla_refused: 'Tesla refused the request',
  tesla_unreachable: 'Tesla could not be reached',
  rate_limited: 'Tesla asked the server to slow down',
  endpoint_gone: 'Tesla closed the address the server used',
  forbidden: 'Tesla refused access to this account (the server waits an hour)',
});
export const lastErrorText = (le) => (le ? `${LAST_ERROR[le.code] || le.code}${le.message && !LAST_ERROR[le.code] ? '' : le.message ? `: ${le.message}` : ''}` : null);

/** Where an account sits on Settings: Integrations. */
export const accountHref = (id) => `${state.buildUrl('settings-integrations')}#tesla-${encodeURIComponent(id)}`;

/** The mode of a status row as a signal light, with the next poll when it is backing off. */
export function modeSig(row, tz) {
  const m = CAR_MODE[row.mode] || [row.mode || 'Not known', 'off', ''];
  return h('span', { class: 'cell', title: m[2] }, sig(m[1], m[0]),
    row.mode === 'backoff' && row.next_poll_ms ? h('span', { class: 'cell-sub' }, `asks again ${dateTime(row.next_poll_ms, tz)}`) : null);
}

/**
 * The status table: what the server does for each Tesla car now. rows: the items of /integrations/tesla/status;
 * carName(id) and accountLabel(id) give names. One car's rows on Data health, every row on Integrations.
 */
export function statusTable(ctx, rows, { carName = (id) => id, accountLabel = (id) => id, title = 'Tesla API: each car now', cover, note, tz } = {}) {
  return tracks(ctx, {
    title, icon: 'antenna', rows, csvName: 'tesla-status', label: 'What the server does for each Tesla car', cover, dense: true, sort: { key: 'car', dir: 'asc' },
    note: note || 'Read from Tesla\'s Owner API, as a second live input beside SMO. The server never wakes a car: a car asleep is only watched in the account\'s car list.',
    columns: [
      { key: 'car', label: 'Car', lead: true, sort: true, value: (r) => (r.car ? carName(r.car) : 'zz'), render: (r) => h('span', { class: 'cell' },
        r.car ? h('a', { class: 'cell-main', href: state.buildUrl('car', { car: r.car }) }, carName(r.car)) : h('span', { class: 'cell-main faint' }, 'Not matched'),
        h('span', { class: 'cell-sub' }, h('a', { class: 'plc', href: accountHref(r.account) }, accountLabel(r.account)), ` · ${r.vehicle}`)),
      csv: (r) => (r.car ? carName(r.car) : '') },
      { key: 'mode', label: 'Doing now', sort: true, value: (r) => r.mode, render: (r) => modeSig(r, tz), csv: (r) => CAR_MODE[r.mode]?.[0] || r.mode },
      { key: 'car_state', label: 'Tesla says', sort: true, render: (r) => CAR_STATE[r.car_state] || r.car_state || DASH, csv: (r) => r.car_state || '' },
      { key: 'last_poll_ms', label: 'Last asked', num: true, sort: true, render: (r) => (r.last_poll_ms ? ago(r.last_poll_ms) : h('span', { class: 'faint' }, 'not yet')), csv: (r) => (r.last_poll_ms ? new Date(r.last_poll_ms).toISOString() : '') },
      { key: 'next_poll_ms', label: 'Next', num: true, sort: true, render: (r) => (r.next_poll_ms ? dateTime(r.next_poll_ms, tz) : h('span', { class: 'faint' }, DASH)), csv: (r) => (r.next_poll_ms ? new Date(r.next_poll_ms).toISOString() : '') },
      { key: 'last_data_ms', label: 'Newest values', num: true, sort: true, render: (r) => (r.last_data_ms ? ago(r.last_data_ms) : h('span', { class: 'faint' }, 'none yet')), csv: (r) => (r.last_data_ms ? new Date(r.last_data_ms).toISOString() : '') },
      { key: 'streaming', label: 'Streaming', render: (r) => (r.streaming ? tag('Streaming', 'violet') : h('span', { class: 'faint' }, 'no')), csv: (r) => (r.streaming ? 'yes' : 'no') },
      { key: 'last_error', label: 'Last problem', render: (r) => (r.last_error ? h('span', { class: 'warn' }, lastErrorText(r.last_error)) : h('span', { class: 'faint' }, 'none')), csv: (r) => lastErrorText(r.last_error) || '' },
    ],
    empty: () => emptyState({ icon: 'antenna', title: 'No Tesla car', text: 'No Tesla account is added, or none of its cars is matched to a car here.' }),
  });
}

/** "1 470 requests today" with the daily cap. */
export const requestsText = (n, cap) => `${formatNumber(n ?? 0)} ${n === 1 ? 'request' : 'requests'} today${cap ? ` of at most ${formatNumber(cap)}` : ''}`;
