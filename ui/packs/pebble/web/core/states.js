// Sentry Mate, "Pebble" UI pack: loading, empty and error states.
//
// loadingTiles(shape) draws grey placeholder tiles of the final sizes, so nothing jumps when the data comes:
// shape is a list of [w, 'h1' | 'h2' | 'h3'] (columns and a height step), or a page id with a shape of its own.
// emptyTile() says plainly why there is nothing and how to change that; errorTile() names the cause and offers
// a way out. errorWords(err) turns an ApiError into a title and a line of help.

import { h } from '../shared/dom.js';
import { icon, btn, linkBtn } from './ui.js';

/** Placeholder shapes per page: [columns, height step]. Pages may pass their own. */
const SHAPES = {
  fleet: [[2, 'h3'], [2, 'h3'], [2, 'h3']],
  car: [[2, 'h3'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [2, 'h2'], [2, 'h2']],
  drive: [['full', 'h3'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [2, 'h2'], [2, 'h2']],
  charge: [[2, 'h2'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1']],
  default: [[2, 'h2'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [2, 'h1'], [2, 'h1']],
};

/** Grey tiles of the final size while a page loads (aria-hidden: #main says aria-busy). */
export function loadingTiles(shape = 'default') {
  const list = Array.isArray(shape) ? shape : SHAPES[shape] || SHAPES.default;
  return list.map(([w, hh]) => h('div', { class: ['tile', 'tile-ghost', `w-${w}`, hh], 'aria-hidden': 'true' },
    h('span', { class: 'ghost-line ghost-title' }), h('span', { class: 'ghost-line ghost-big' }), h('span', { class: 'ghost-line' })));
}

/** 'No drives this week' with what to do: emptyTile({ icon, title, text, action, w }). */
export function emptyTile({ icon: ic = 'spark', title, text = null, action = null, w = 'full', cover = null } = {}) {
  return h('section', { class: ['tile', 'tile-empty', `w-${w}`], cover },
    h('span', { class: 'empty-ic', 'aria-hidden': 'true' }, icon(ic)),
    h('h2', { class: 'empty-title' }, title),
    text ? h('p', { class: 'empty-text' }, text) : null,
    action ? h('div', { class: 'empty-act' }, action) : null);
}

/** What went wrong, in words: { title, text, icon }. */
export function errorWords(err) {
  const s = err && err.status;
  if (s === 0 || (err && (err.code === 'network' || err.code === 'timeout'))) {
    return { icon: 'offline', title: 'Sentry Mate does not answer', text: 'The server may be restarting, or this device is offline. It tries again when you press Try again.' };
  }
  if (s === 401) return { icon: 'lock', title: 'You are signed out', text: 'Your session ended. Sign in again to go on; you come back to this page.' };
  if (s === 403) return { icon: 'lock', title: 'Not for this account', text: (err && err.message) || 'This account may not open this. An admin can change what it may see.' };
  if (s === 404) return { icon: 'search', title: 'Nothing here', text: (err && err.message) || 'There is nothing at this address: it may have been removed, or the link is wrong.' };
  if (s === 429) return { icon: 'info', title: 'A short pause', text: 'Too many requests at once. Wait a moment, then try again.' };
  if (s === 501) return { icon: 'info', title: 'Not built yet', text: (err && err.message) || 'This server does not offer this yet.' };
  return { icon: 'warning', title: 'Something went wrong', text: (err && err.message) || 'The server could not answer this request.' };
}

/** The error state of a page: the cause in words and a way out. */
export function errorTile(err, { retry = null, home = true, w = 'full' } = {}) {
  const words = errorWords(err);
  const signIn = err && err.status === 401;
  return h('section', { class: ['tile', 'tile-empty', 'tile-error', `w-${w}`], role: 'alert' },
    h('span', { class: 'empty-ic', 'aria-hidden': 'true' }, icon(words.icon)),
    h('h2', { class: 'empty-title' }, words.title),
    h('p', { class: 'empty-text' }, words.text),
    h('div', { class: 'empty-act' },
      signIn ? linkBtn('Sign in', `/sign-in?next=${encodeURIComponent(location.pathname + location.search)}`, { kind: 'primary', icon: 'person' })
        : retry ? btn('Try again', { kind: 'primary', icon: 'reset', onclick: retry }) : null,
      home && location.pathname !== '/' ? linkBtn('Fleet', '/', { kind: 'soft', icon: 'fleet' }) : null));
}
