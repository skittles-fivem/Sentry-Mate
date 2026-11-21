// The notices row: the server's notices (vault, certificate, recovery key file, move under way, a Tesla account
// that needs a sign-in or that Tesla refuses), each with
// its cause and a link to the fix, plus an "offline" line while this browser has no network or the server
// does not answer. Information notices can be put away for this tab. The opt-in release notice goes to the
// footer instead (onNotices).

import * as api from '../shared/api.js';
import { h, replace } from '../shared/dom.js';
import { parseUrl } from '../shared/state.js';
import { icon, btn } from './ui.js';
import { station, fullName } from './stations.js';

const PUT_AWAY = 'sentrymate.horizon.notices-away';
let box = null;
let items = [];
let offline = null;   // null, 'device' or 'server'
const subs = new Set();

const away = () => { try { return new Set(JSON.parse(sessionStorage.getItem(PUT_AWAY) || '[]')); } catch { return new Set(); } };
const putAway = (code) => { try { const s = away(); s.add(code); sessionStorage.setItem(PUT_AWAY, JSON.stringify([...s])); } catch { /* this view only */ } };

/** f(items) after the notices changed. Returns off(). */
export function onNotices(f) { subs.add(f); return () => subs.delete(f); }
export const getNotices = () => items;

function fixText(link) {
  try { const s = station(parseUrl(link).page); if (s) return `Open ${fullName(s)}`; } catch { /* another site */ }
  return 'How to fix it';
}

// a Tesla account's notice (admins only) opens that account on Settings: Integrations
const TESLA = new Set(['tesla_sign_in_needed', 'tesla_account_error']);
function fixOf(n) {
  if (TESLA.has(n.code) && n.detail?.account) return `${(n.fix_link || '/settings/integrations').split('#')[0]}#tesla-${encodeURIComponent(n.detail.account)}`;
  return n.fix_link || null;
}

function paint() {
  if (!box) return;
  const gone = away();
  const list = items.filter((n) => n.code !== 'release_available' && !(n.level === 'info' && gone.has(n.code)));
  const rows = list.map((n) => {
    const level = n.level === 'error' || n.level === 'warning' ? n.level : 'info';
    return h('div', { class: 'notice', dataset: { level, code: n.code }, role: level === 'info' ? 'status' : 'alert' },
      icon(level === 'info' ? 'info' : 'warn'),
      h('span', { class: 'notice-text' }, n.message || n.code),
      fixOf(n) ? h('a', { class: 'btn btn--sm', href: fixOf(n) }, h('span', null, TESLA.has(n.code) ? 'Open the Tesla account' : fixText(n.fix_link)), icon('arrow-r', 'i--sm')) : null,
      level === 'info' ? btn(null, () => { putAway(n.code); paint(); }, { kind: 'ghost', icon: 'x', sm: true, label: 'Put this notice away for now' }) : null);
  });
  if (offline) {
    rows.unshift(h('div', { class: 'notice', dataset: { level: 'offline' }, role: 'alert' }, icon('signal'),
      h('span', { class: 'notice-text' }, offline === 'device'
        ? 'This device is offline. What is on screen stays; pages update again when the network is back.'
        : 'No signal from the server. It keeps trying; check that Sentry Mate is running.')));
  }
  box.hidden = !rows.length;
  replace(box, rows);
}

/** Load the notices (at start, after a `notice` event, after signing in). */
export async function loadNotices() {
  try { items = (await api.get('notices')).items || []; } catch { items = []; }
  paint();
  for (const f of [...subs]) { try { f(items); } catch (e) { console.error(e); } }
  return items;
}

/** The frame calls this once with #notices. */
export function initNotices(el) {
  box = el;
  const net = () => { offline = !navigator.onLine ? 'device' : api.network().ok ? null : 'server'; paint(); };
  window.addEventListener('online', net);
  window.addEventListener('offline', net);
  api.on('network', net);
  net();
}
