// The car switcher: a search over the cars this account may see (GET /api/v1/cars?search=, in the
// person's own order), opened by the car button in the sidebar or Ctrl/Cmd+K. Choosing a car keeps the
// page: /cars/c1/battery becomes /cars/c2/battery (shared/state.js linkForCar); a drive or charge goes
// to the other car's list; a page without a car goes to the car's overview. Works for any number of
// cars: it asks the server for as many matches as fit and says how many there are.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { icon } from './icons.js';
import { ago, stateLabel, carMeta } from './fmt.js';

let dlg = null, input = null, list = null, foot = null, active = -1, rows = [], timer = 0, seq = 0, showSold = false, lastPage = null;
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || '');
export const SWITCH_KEYS = IS_MAC ? ['⌘', 'K'] : ['Ctrl', 'K'];

function mark(text, q) {
  if (!q) return text;
  const i = String(text).toLowerCase().indexOf(q.toLowerCase());
  if (i < 0) return text;
  return [text.slice(0, i), h('mark', null, text.slice(i, i + q.length)), text.slice(i + q.length)];
}

function option(c, q, current) {
  const n = c.now || {};
  const st = n.state || 'unknown';
  const where = n.place?.name || null;
  const meta = [carMeta(c), where].filter(Boolean).join(' · ');
  const o = h('div', {
    class: ['sw-opt', c.id === current && 'is-current', state.isSold(c) && 'is-sold'], role: 'option', id: `sw-${c.id}`, dataset: { state: st, car: c.id },
    'aria-selected': 'false', onclick: () => choose(c),
  },
  h('span', { class: ['dot', n.stale && 'is-stale'], 'aria-hidden': 'true' }),
  h('span', { class: 'sw-opt-main' },
    h('span', { class: 'sw-opt-name' }, h('span', { class: 'sw-opt-name-text' }, mark(c.name, q)), state.isSold(c) ? h('span', { class: 'tag tag--plain' }, 'Sold') : null, c.id === current ? h('span', { class: 'tag tag--plain' }, 'Open now') : null),
    h('span', { class: 'sw-opt-meta' }, mark(meta, q))),
  state.isSold(c) ? h('span', { class: 'sw-opt-side' }, h('span', null, c.sold_date ? `Sold on ${c.sold_date}` : 'Sold'), h('span', { class: 'age age--old' }, n.at_ms ? `last data ${ago(n.at_ms)}` : 'no data')) : h('span', { class: 'sw-opt-side' },
    h('span', null, `${stateLabel(st)}${n.soc_pct != null ? ` · ${format(n.soc_pct, 'percent')}` : ''}`),
    h('span', { class: ['age', n.stale ? 'age--stale' : 'age--old'] }, n.at_ms ? ago(n.at_ms) : 'no data yet')),
  h('span', { class: 'kbd enter', 'aria-hidden': 'true' }, 'Enter'));
  return o;
}

async function load() {
  const my = ++seq;
  const q = input.value.trim();
  const limit = Math.max(8, Math.min(50, Math.floor((window.innerHeight * 0.6) / 56)));
  try {
    const page = await api.page('cars', { search: q || null, sort: 'custom', limit });
    if (my !== seq) return;
    lastPage = { page, q };
    paint();
  } catch (e) {
    if (my !== seq || api.isAbort(e)) return;
    replace(list, h('p', { class: 'sw-more' }, 'Could not load the cars. ', e.message));
  }
}

// a search lists sold cars too (marked Sold); without one they wait behind "Sold cars (n)", except the open car
function paint() {
  const { page, q } = lastPage;
  const current = state.current().car || state.lastCar();
  const { sold } = state.splitSold(page.items);
  const soldOther = q ? [] : sold.filter((c) => c.id !== current);
  const main = q ? page.items : page.items.filter((c) => !soldOther.includes(c));
  rows = showSold ? [...main, ...soldOther] : main;
  const soldBtn = soldOther.length ? h('button', { class: 'sw-sold-toggle', type: 'button', 'aria-expanded': String(showSold),
    onclick: () => { showSold = !showSold; paint(); input.focus(); } }, icon('chev-d', showSold ? 'ico--sm sw-rot' : 'ico--sm'), `Sold cars (${soldOther.length})`) : null;
  replace(list,
    h('div', { class: 'sw-group-label' }, h('span', null, q ? 'Matching cars' : 'Your cars'), h('span', null, page.total != null ? `${page.total - (q ? 0 : soldOther.length)} in all` : page.next ? 'more below' : `${main.length}`)),
    main.length ? main.map((c) => option(c, q, current)) : h('p', { class: 'sw-more' }, q ? `No car matches "${q}". Search by name, model or VIN prefix.` : soldOther.length ? 'Every car is sold.' : 'No cars yet. A car appears when its CAN Bridge first uploads.'),
    soldBtn, showSold ? soldOther.map((c) => option(c, q, current)) : null,
    page.next ? h('p', { class: 'sw-more' }, 'More cars match. Type more of the name, or ', h('a', { href: state.buildUrl('fleet', { query: q ? { search: q } : null }), onclick: () => dlg.close() }, 'see them all in All cars'), '.') : null);
  setActive(rows.length ? 0 : -1);
}

function setActive(i) {
  const opts = [...list.querySelectorAll('.sw-opt')];
  opts.forEach((o) => o.setAttribute('aria-selected', 'false'));
  active = i;
  if (i >= 0 && opts[i]) {
    opts[i].setAttribute('aria-selected', 'true');
    opts[i].scrollIntoView({ block: 'nearest' });
    input.setAttribute('aria-activedescendant', opts[i].id);
  }
}

function choose(car) {
  dlg.close();
  state.rememberCar(car.id, { sold: state.isSold(car) });
  state.navigate(state.linkForCar(car.id));
}

function build() {
  input = h('input', { type: 'search', placeholder: 'Find a car by name, model or VIN prefix', 'aria-label': 'Find a car', autocomplete: 'off', spellcheck: 'false', role: 'combobox', 'aria-controls': 'sw-list', 'aria-expanded': 'true' });
  input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(load, 160); });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setActive(Math.min(rows.length - 1, active + 1)); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setActive(Math.max(0, active - 1)); }
    else if (e.key === 'Enter' && rows[active]) { e.preventDefault(); choose(rows[active]); }
  });
  list = h('div', { class: 'sw-list', id: 'sw-list', role: 'listbox', 'aria-label': 'Cars' });
  foot = h('div', { class: 'sw-foot' },
    h('span', { class: 'sw-keys' }, h('span', { class: 'kbd' }, '↑'), h('span', { class: 'kbd' }, '↓'), ' choose', h('span', { class: 'sw-sep' }, '·'), h('span', { class: 'kbd' }, 'Enter'), ' open this page for the car'),
    h('a', { class: 'sw-all', href: '/', onclick: () => dlg.close() }, icon('fleet', 'ico--sm'), 'All cars'));
  dlg = h('dialog', { class: 'switcher', 'aria-label': 'Switch car' }, h('div', { class: 'sw-search' }, icon('car'), input), list, foot);
  dlg.addEventListener('click', (e) => { if (e.target === dlg) dlg.close(); });
  document.body.append(dlg);
}

/** Open the car switcher. */
export function openCarSwitcher() {
  if (!dlg) build();
  input.value = '';
  showSold = false;
  replace(list, h('p', { class: 'sw-more' }, 'Loading the cars …'));
  if (!dlg.open) dlg.showModal();
  input.focus();
  load();
}

/* ---------- sold cars in a page's own car pickers (like the switcher above and Fleet)
 * soldParts(items, keep, { search, isSold, id }) -> { main, sold }: sold cars wait in `sold` unless their id is in
 * `keep` (the current or picked car) or a search is typed; `main` keeps the order given.
 * soldOptions(sold, mk) -> an <optgroup> "Sold cars (n)" of mk(car) options, or null, for a <select>.
 * soldToggle(n, open, onToggle) -> a "Sold cars (n)" button for a checkbox list, or null when n is 0. */
export function soldParts(items = [], keep = [], { search = false, isSold = state.isSold, id = (c) => c.id } = {}) {
  const ids = new Set([].concat(keep ?? []).filter((x) => x != null && x !== '').map(String));
  const main = [], sold = [];
  for (const c of items || []) (!search && isSold(c) && !ids.has(String(id(c))) ? sold : main).push(c);
  return { main, sold };
}
export function soldOptions(sold, mk) {
  return sold && sold.length ? h('optgroup', { label: `Sold cars (${sold.length})` }, sold.map(mk)) : null;
}
export function soldToggle(n, open, onToggle) {
  if (!n) return null;
  return h('button', { class: 'sw-sold-toggle pick-sold-toggle', type: 'button', 'aria-expanded': String(!!open), onclick: onToggle },
    icon('chev-d', open ? 'ico--sm sw-rot' : 'ico--sm'), `Sold cars (${n})`);
}
