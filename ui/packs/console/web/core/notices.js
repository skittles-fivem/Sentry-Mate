// Server notices (certificate, vault, recovery key file, ...) as lines under the command bar:
// '[warn] The certificate ends in 9 days.  fix: settings/server'. Information notices can be put away for this tab.
// Offline is a notice of this browser: shown while the server does not answer.

import * as api from '../shared/api.js';
import { h, replace } from '../shared/dom.js';

const HIDDEN_KEY = 'sentrymate.console.notices-hidden';
let box = null;
let items = [];
let offline = false;
const subs = new Set();

const hidden = () => { try { return new Set(JSON.parse(sessionStorage.getItem(HIDDEN_KEY) || '[]')); } catch { return new Set(); } };
const hide = (code) => { try { const s = hidden(); s.add(code); sessionStorage.setItem(HIDDEN_KEY, JSON.stringify([...s])); } catch { /* fine */ } };

export const getNotices = () => items;
export function onNotices(f) { subs.add(f); return () => subs.delete(f); }

function render() {
  if (!box) return;
  const gone = hidden();
  const list = items.filter((n) => n.code !== 'release_available' && !(n.level === 'info' && gone.has(n.code)));
  const lines = list.map((n) => {
    const tone = n.level === 'error' ? 'bad' : n.level === 'warning' ? 'warn' : 'info';
    return h('div', { class: ['ntc', `ntc--${tone}`], role: tone === 'info' ? 'status' : 'alert' },
      h('span', { class: `tag tag--${tone}` }, `[${tone}]`),
      h('span', { class: 'ntc-text' }, n.message || n.code),
      n.fix_link ? h('a', { class: 'lnk', href: n.fix_link }, `fix: ${String(n.fix_link).replace(/^\//, '').split(/[?#]/)[0]}`) : null,
      tone === 'info' ? h('button', { class: 'ntc-x', type: 'button', 'aria-label': 'Hide this notice for now', onclick: () => { hide(n.code); render(); } }, 'hide') : null);
  });
  if (offline) {
    lines.unshift(h('div', { class: 'ntc ntc--bad', role: 'alert' }, h('span', { class: 'tag tag--bad' }, '[offline]'),
      h('span', { class: 'ntc-text' }, 'The server does not answer. Pages keep what they have and update again when it is back.')));
  }
  box.hidden = !lines.length;
  replace(box, h('div', { class: 'wrap ntc-wrap' }, lines));
}

/** Load the notices (at start, after a `notice` event, after signing in). */
export async function loadNotices() {
  try { items = (await api.get('notices')).items || []; } catch { items = []; }
  render();
  for (const f of [...subs]) { try { f(items); } catch (e) { console.error(e); } }
  return items;
}

/** The browser lost or found the server. */
export function setOffline(on) { if (offline !== on) { offline = on; render(); } }

export function initNotices(el) { box = el; render(); }
