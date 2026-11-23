// Loading, empty and error states, the same everywhere.

import { h } from '../shared/dom.js';
import { PAGE_FAILED, isPageFailed } from '../shared/build.js';
import { icon } from './icons.js';

/** A shimmering bar: skel('text' | 'num' | 'title' | 'block', { width: '60%' }). */
export const skel = (kind = 'text', { width = null } = {}) => h('span', { class: ['skel', `skel--${kind}`], style: width ? { width } : null });

/** A page that is loading: a title bar and a few empty panels (n, default 3). */
export function loadingPage(n = 3) {
  return h('div', { class: 'loading', 'aria-busy': 'true' },
    h('div', { class: 'loading-head' }, skel('text', { width: '8rem' }), h('div', { class: 'row' }, skel('title'))),
    h('div', { class: 'loading-grid' }, Array.from({ length: n }, () => h('div', { class: 'panel' }, skel('text'), h('br'), skel('num')))));
}

/**
 * An empty state: emptyState({ icon: 'route', title: 'No drives in this period', text: 'Pick a longer
 * period.', action: h('a', { class: 'btn', href }, 'Show 90 days') }). Say exactly why it is empty.
 */
export function emptyState({ icon: ic = 'info', title, text = null, action = null, warn = false } = {}) {
  return h('div', { class: 'empty' },
    h('div', { class: ['empty-icon', warn && 'empty-icon--warn'] }, icon(ic)),
    h('h3', null, title),
    text ? h('p', null, text) : null,
    action);
}

/** The words for an ApiError (or any error): { title, text }. */
export function errorWords(err) {
  const s = err?.status;
  if (s === 404) return { title: 'Not found', text: (err.message ? err.message + ' ' : '') + 'It may have been removed, or the link is wrong. A car you may not see looks the same.' };
  if (s === 401) return { title: 'Signed out', text: 'Your session ended. Sign in again to go on.' };
  if (s === 403) return { title: 'Not allowed', text: err.message || 'Your account may not do this. An admin can.' };
  if (isPageFailed(err)) return { title: PAGE_FAILED.title, text: PAGE_FAILED.text };
  if (s === 0) return { title: 'No answer from the server', text: 'Check that Sentry Mate is running and this computer is on the same network, then try again.' };
  if (s === 429) return { title: 'Too many requests', text: 'Wait a moment, then try again.' };
  if (s >= 500) return { title: 'The server had a problem', text: 'Try again. If it keeps happening, look at Data health.' };
  return { title: 'Could not load this', text: err?.message || 'Something went wrong. Try again.' };
}

/**
 * An error state with the cause and a way out: errorState(err, { retry: () => ..., home: true }).
 * The error code is shown small, for a bug report.
 */
export function errorState(err, { retry = null, home = true, compact = false } = {}) {
  const w = errorWords(err);
  return h('div', { class: ['empty', compact && 'empty--compact'], role: 'alert' },
    h('div', { class: 'empty-icon empty-icon--warn' }, icon(err?.status === 404 ? 'question' : 'warn')),
    h('h3', null, w.title),
    h('p', null, w.text),
    err?.code ? h('span', { class: 'empty-code' }, `${err.status || ''} ${err.code}`.trim()) : null,
    h('div', { class: 'row' },
      retry ? h('button', { class: 'btn', type: 'button', onclick: retry }, icon('reset'), isPageFailed(err) ? PAGE_FAILED.action : 'Try again') : null,
      home ? h('a', { class: 'btn btn--ghost', href: '/' }, icon('fleet'), 'All cars') : null));
}

/** A panel that holds only a state (empty or error), centred. */
export const statePanel = (child) => h('section', { class: 'panel state-panel' }, child);
