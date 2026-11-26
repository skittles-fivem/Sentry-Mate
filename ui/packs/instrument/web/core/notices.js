// The notices bar: server notices (certificate, vault, recovery key file, the opt-in release notice), each
// with its cause and a link to the fix. Loaded at start and after a `notice` event; information notices
// can be put away for this browser tab, the release notice for that newer build in this browser.

import * as api from '../shared/api.js';
import { h, replace } from '../shared/dom.js';
import { icon } from './icons.js';
import { pageInfo } from './pages.js';
import { parseUrl } from '../shared/state.js';
import { releaseDismissed, dismissRelease } from '../shared/release.js';
import { isTmNotice, tmNoticeFix, TM_NOTICE_ACTION } from '../shared/teslamate.js';

const HIDDEN_KEY = 'sentrymate.notices-hidden';
let box = null;
let items = [];
const subs = new Set();
let firstDone = null;
/** Resolves once the notices were loaded the first time and are in the bar (or could not be loaded). */
export const noticesLoaded = new Promise((resolve) => { firstDone = resolve; });

const hidden = () => { try { return new Set(JSON.parse(sessionStorage.getItem(HIDDEN_KEY) || '[]')); } catch { return new Set(); } };
const hide = (code) => { try { const s = hidden(); s.add(code); sessionStorage.setItem(HIDDEN_KEY, JSON.stringify([...s])); } catch { /* fine */ } };

/** The notices now (the /notices items). */
export const getNotices = () => items;
/** f(items) after the notices changed. Returns off(). */
export function onNotices(f) { subs.add(f); return () => subs.delete(f); }

// 'Settings: Server and access' for a fix link inside the app
function fixLabel(link) {
  try {
    const p = parseUrl(link).page;
    const info = pageInfo(p);
    if (info) return `Open ${info.title}`;
  } catch { /* not an app link */ }
  return 'How to fix';
}

// a Tesla account's notice (admins only) links to that account on Settings: Integrations, a local TeslaMate notice
// (admins only) to its section there
const TESLA = new Set(['tesla_sign_in_needed', 'tesla_account_error']);
function fixOf(n) {
  if (TESLA.has(n.code) && n.detail?.account) return `${(n.fix_link || '/settings/integrations').split('#')[0]}#tesla-${encodeURIComponent(n.detail.account)}`;
  if (isTmNotice(n)) return tmNoticeFix(n);
  return n.fix_link || null;
}
function fixWords(n) {
  if (TESLA.has(n.code)) return 'Open the Tesla account';
  if (isTmNotice(n)) return TM_NOTICE_ACTION;
  return fixLabel(n.fix_link);
}

// a damaged drives table (admins only): Repair puts what reads back, reads the imports' kept uploads again and makes
// the rest from the logs (POST cars/{car}/repair); the notices are read again when it is done
const repairCars = (n) => (n.code === 'data_damaged'
  ? [...new Set((n.detail?.files || []).filter((f) => f.mend === 'repair').map((f) => f.car))] : []);
async function repair(car, button) {
  button.disabled = true;
  button.textContent = 'Repairing…';
  try {
    await api.post(`cars/${car}/repair`, { table: 'drives' });
  } catch (e) {
    button.disabled = false;
    button.textContent = 'Repair';
    button.title = e?.message || 'The repair did not start';
  }
}

function render() {
  if (!box) return;
  const gone = hidden();
  // the release notice (admins only) is put away per newer build, in this browser; the others per tab
  const shown = items.filter((n) => (n.code === 'release_available' ? !releaseDismissed(n) : !(n.level === 'info' && gone.has(n.code))));
  box.hidden = !shown.length;
  replace(box, shown.map((n) => {
    const level = n.level === 'error' || n.level === 'warning' ? n.level : 'info';
    const row = h('div', { class: 'notice', dataset: { level, code: n.code }, role: level === 'info' ? 'status' : 'alert' },
      icon(level === 'info' ? 'info' : 'warn'),
      h('span', { class: 'notice-text' }, n.message || n.code),
      repairCars(n).map((car) => h('button', { class: 'btn btn--sm', type: 'button', onclick: (ev) => repair(car, ev.currentTarget) },
        repairCars(n).length > 1 ? `Repair ${car}` : 'Repair')),
      fixOf(n) ? h('a', { class: 'btn btn--sm', href: fixOf(n) }, fixWords(n), icon('arrow-r', 'ico--sm')) : null,
      level === 'info' ? h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', 'aria-label': 'Hide this notice for now',
        onclick: () => { if (n.code === 'release_available') dismissRelease(n); else hide(n.code); render(); } }, icon('x', 'ico--sm')) : null);
    return row;
  }));
}

/** Load the notices into the bar. Safe to call again (after a `notice` event, after signing in). */
export async function loadNotices() {
  try {
    const r = await api.get('notices', null, { keep: true }); // a page that lists notices too reuses it
    items = r.items || [];
  } catch { items = []; }
  render();
  firstDone();
  for (const f of [...subs]) { try { f(items); } catch (e) { console.error(e); } }
  return items;
}

/** The frame calls this once with the #notices element. */
export function initNotices(el) { box = el; }
