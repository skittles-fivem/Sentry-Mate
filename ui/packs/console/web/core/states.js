// States every page shares: loading (a blinking caret line, never a spinner), empty ('-- no drives in this
// range --'), error (the cause in words and a way out), offline and signed out (core/frame.js, core/app.js).

import * as api from '../shared/api.js';
import { h } from '../shared/dom.js';

/** 'loading drives' with a blinking caret. */
export const loadingLine = (what = 'loading') => h('p', { class: 'loading', role: 'status' }, h('span', null, what), h('span', { class: 'caret', 'aria-hidden': 'true' }));

/** The router's placeholder while a page loads. */
export const loadingPage = (what) => h('div', { class: 'page page--loading' }, loadingLine(what));

/** '-- no drives in this range --', with an optional second line that says how to change it. */
export const empty = (text, hint = null, attrs = {}) => h('div', { class: 'empty', ...attrs },
  h('p', { class: 'empty-line' }, `-- ${text} --`), hint ? h('p', { class: 'empty-hint' }, hint) : null);

/** { tag, text }: what an error means, in plain words. */
export function errorWords(err) {
  const e = err || {};
  if (e.status === 0 || e.code === 'network') return { tag: 'offline', text: 'The server does not answer. Check the network; the page tries again when it is back.' };
  if (e.code === 'timeout') return { tag: 'slow', text: 'The server took too long to answer. Try again.' };
  if (e.status === 401) return { tag: 'signed out', text: 'Your session ended. Sign in again.' };
  if (e.status === 403) return { tag: 'no access', text: e.message || 'Your account may not see this.' };
  if (e.status === 404) return { tag: 'not found', text: e.message || 'There is nothing at this address.' };
  if (e.status === 409) return { tag: 'busy', text: e.message || 'Something else is under way. Try again in a moment.' };
  if (e.status === 429) return { tag: 'slow down', text: `Too many requests.${e.retryAfterMs ? ` Wait ${Math.ceil(e.retryAfterMs / 1000)} s.` : ''}` };
  if (e.status >= 500) return { tag: 'server', text: e.message || 'The server had a problem with this request.' };
  return { tag: 'error', text: e.message || String(e) };
}

/** The error block: '[bad] not found: There is no car at this address.' and [retry] [fleet]. */
export function errorBlock(err, { retry = null, home = true } = {}) {
  const w = errorWords(err);
  return h('div', { class: 'errbox', role: 'alert' },
    h('p', { class: 'errbox-line' }, h('span', { class: 'tag tag--bad' }, `[${w.tag}]`), ' ', w.text),
    err?.code && !api.isAbort(err) ? h('p', { class: 'errbox-code' }, `code: ${err.code}${err.status ? ` (http ${err.status})` : ''}`) : null,
    h('div', { class: 'acts' },
      retry ? h('button', { class: 'btn btn--primary', type: 'button', onclick: retry }, 'retry') : null,
      home ? h('a', { class: 'btn', href: '/' }, 'fleet') : null));
}
