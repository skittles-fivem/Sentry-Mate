// States a page can be in besides "here is the data": loading, empty, error, offline and signed out.
// Each says what is going on in plain words and offers a way on.

import { h } from '../shared/dom.js';
import { icon, hud, btn, linkBtn } from './ui.js';

/** The loading picture the router shows until a page draws: a plate and `n` panels of placeholder lines. */
export function loadingDeck(n = 3) {
  const lines = (k) => Array.from({ length: k }, (_, i) => h('span', { class: 'skel skel--line', vars: { '--w': `${92 - i * 14}%` } }));
  return h('div', { class: 'loading-deck', 'aria-label': 'Loading', role: 'status' },
    h('div', { class: 'hud' }, h('span', { class: 'skel skel--title' }), lines(1)),
    Array.from({ length: n }, (_, i) => h('div', { class: 'hud' }, lines(2), i === 0 ? h('span', { class: 'skel skel--block' }) : null)));
}

/** A state box: state({ icon, title, text, actions, tone: 'warn' | 'bad', code }). */
export function state({ icon: ic = 'info', title, text, actions, tone, code } = {}) {
  return h('div', { class: ['state', tone && `state--${tone}`] },
    h('span', { class: 'state-ic' }, icon(ic, 'i--lg')),
    h('h2', { class: 'state-title' }, title),
    text ? h('p', { class: 'state-text' }, text) : null,
    actions ? h('div', { class: 'row' }, actions) : null,
    code ? h('p', { class: 'state-code' }, code) : null);
}

/** Empty: say exactly why there is nothing and how to change that. */
export const emptyState = ({ icon: ic = 'info', title, text, actions } = {}) => state({ icon: ic, title, text, actions });

/** An error in words: { title, text, icon, tone } for an ApiError (or anything thrown). */
export function errorWords(err) {
  const s = err?.status;
  if (s === 0 || err?.code === 'network' || err?.code === 'timeout') {
    return { icon: 'signal', tone: 'warn', title: 'No signal from the server',
      text: err?.code === 'timeout' ? 'The server took too long to answer. It may be busy; try again in a moment.' : 'This browser could not reach Sentry Mate. Check the network, or that the server is running, then try again.' };
  }
  if (s === 401) return { icon: 'lock', tone: 'warn', title: 'Signed out', text: 'Your session has ended. Sign in again to go on.' };
  if (s === 403) return { icon: 'lock', tone: 'warn', title: 'Not for this account', text: err?.message || 'Your account may not do this. An admin can change what it may do.' };
  if (s === 404) return { icon: 'search', tone: 'warn', title: 'Not found', text: err?.message || 'There is nothing at this address, or your account may not see it.' };
  if (s === 409) return { icon: 'warn', tone: 'warn', title: 'Changed meanwhile', text: err?.message || 'Something changed while you were looking. Load the page again.' };
  if (s === 429) return { icon: 'clock', tone: 'warn', title: 'Too many tries', text: `${err?.message || 'Wait a moment.'}${err?.retryAfterMs ? ` Try again in ${Math.ceil(err.retryAfterMs / 1000)} s.` : ''}` };
  if (s >= 500) return { icon: 'server', tone: 'bad', title: 'The server hit a problem', text: err?.message || 'Something went wrong on the server. Try again; if it keeps happening, look at the server log.' };
  return { icon: 'warn', tone: 'bad', title: 'That did not work', text: err?.message || 'Something went wrong. Try again.' };
}

/** The error state: errorState(err, { retry, home }) with Try again and All cars buttons. */
export function errorState(err, { retry = null, home = true } = {}) {
  const w = errorWords(err);
  const code = err?.code && err.status ? `${err.status} ${err.code}` : null;
  return state({
    ...w, code,
    actions: [retry ? btn('Try again', retry, { kind: 'go', icon: 'arrow-r' }) : null, home ? linkBtn('All cars', '/', { icon: 'grid' }) : null],
  });
}

/** Offline: the browser has no network at all. */
export const offlineState = (retry) => state({
  icon: 'signal', tone: 'warn', title: 'This device is offline',
  text: 'The browser says there is no network. Pages come back by themselves when it returns.',
  actions: retry ? btn('Try again', retry, { kind: 'go', icon: 'arrow-r' }) : null,
});

/** Signed out, with a Sign in button that comes back here. */
export const signedOutState = (here = location.pathname + location.search) => state({
  icon: 'lock', tone: 'warn', title: 'Signed out', text: 'Sign in again to see this page.',
  actions: linkBtn('Sign in', `/sign-in?next=${encodeURIComponent(here)}`, { kind: 'go', icon: 'user' }),
});

/** A state inside a HUD panel (the router's error page). */
export const statePanel = (child) => hud({ pad: false, cls: 'state-panel' }, child);
