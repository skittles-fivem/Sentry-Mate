// Two search dialogs:
// - the car picker (the cartridge in the console row, or Ctrl/Cmd+K): searches the cars this account may see
//   (GET /cars?search=, in the person's own order) and keeps the page: /cars/c1/battery becomes
//   /cars/c2/battery; a drive or charge goes to the other car's list; a page without a car to its cluster.
//   Any number of cars: it asks for as many matches as fit and says when there are more.
// - the scan box (the radio face, or /): type to find a page by its name, its line or its words ('drain').

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { icon, lamp } from './ui.js';
import { openDialog } from './layer.js';
import { BANDS, inBand, DIAL, channel, fullName } from './stations.js';
import { carLine, carStatus, agoEl } from './fmt.js';

const MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || '');
export const CAR_KEYS = MAC ? '⌘ K' : 'Ctrl K';

function marked(text, q) {
  const s = String(text ?? '');
  if (!q) return s;
  const i = s.toLowerCase().indexOf(q.toLowerCase());
  return i < 0 ? s : [s.slice(0, i), h('mark', null, s.slice(i, i + q.length)), s.slice(i + q.length)];
}

// arrow keys move a highlight through the options; Enter picks it
function keyNav(input, list, pick) {
  let at = -1;
  const opts = () => [...list.querySelectorAll('[data-opt]')];
  const mark = (i) => {
    const o = opts();
    o.forEach((x) => { x.classList.remove('is-active'); x.setAttribute('aria-selected', 'false'); });
    at = o.length ? Math.max(0, Math.min(i, o.length - 1)) : -1;
    if (at >= 0) { o[at].classList.add('is-active'); o[at].setAttribute('aria-selected', 'true'); o[at].scrollIntoView({ block: 'nearest' }); input.setAttribute('aria-activedescendant', o[at].id); }
  };
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); mark(at + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); mark(at - 1); }
    else if (e.key === 'Enter') { const o = opts()[at]; if (o) { e.preventDefault(); pick(o); } }
  });
  return { reset: () => mark(0) };
}

/* ------------------------------------------------------------------ the car picker */

export function openCarPicker() {
  const input = h('input', { class: 'input', type: 'search', placeholder: 'Find a car by name, model or VIN start', 'aria-label': 'Find a car', autocomplete: 'off', spellcheck: 'false', role: 'combobox', 'aria-controls': 'hz-cars', 'aria-expanded': 'true' });
  const list = h('div', { class: 'picker-list', id: 'hz-cars', role: 'listbox', 'aria-label': 'Cars' }, h('p', { class: 'picker-empty' }, 'Loading the cars…'));
  const foot = h('div', { class: 'picker-foot' }, h('span', null, '↑ ↓ to choose, Enter to open this page for that car'), h('a', { href: '/', onclick: () => d.close() }, 'All cars'));
  const d = openDialog({ cls: 'picker', label: 'Switch car', body: [h('div', { class: 'picker-top' }, icon('car'), input), list, foot] });
  d.querySelector('.dlg-body').classList.add('picker-body');
  const go = (id) => { d.close(); state.rememberCar(id); state.navigate(state.linkForCar(id)); };
  const nav = keyNav(input, list, (o) => go(o.dataset.car));
  let seq = 0, t = 0;
  async function load() {
    const my = ++seq;
    const q = input.value.trim();
    const limit = Math.max(8, Math.min(50, Math.floor((window.innerHeight * 0.6) / 56)));
    try {
      const pg = await api.page('cars', { search: q || null, sort: 'custom', limit });
      if (my !== seq) return;
      const open = state.current().car || state.lastCar();
      replace(list, pg.items.length ? pg.items.map((c) => h('div', {
        class: 'car-opt', role: 'option', id: `hz-car-${c.id}`, 'aria-selected': 'false', dataset: { opt: '', car: c.id }, onclick: () => go(c.id),
      },
      lamp(c.now?.state, { stale: c.now?.stale }),
      h('span', { class: 'car-opt-text' }, h('span', { class: 'car-opt-name' }, marked(c.name, q), c.id === open ? ' (open now)' : ''), h('span', { class: 'car-opt-meta' }, marked([carLine(c), c.now?.place?.name].filter(Boolean).join(' · '), q))),
      h('span', { class: 'car-opt-side' }, h('span', null, carStatus(c)), c.now?.at_ms ? agoEl(c.now.at_ms) : h('span', null, 'No data yet'))))
        : h('p', { class: 'picker-empty' }, q ? `No car matches "${q}". Try its name, model or the start of its VIN.` : 'No cars yet. A car appears when its CAN Bridge sends its first log.'),
      pg.next ? h('p', { class: 'picker-empty' }, 'More cars match: type more of the name, or ', h('a', { href: state.buildUrl('fleet', { query: q ? { search: q } : null }), onclick: () => d.close() }, 'see them all'), '.') : null);
      nav.reset();
    } catch (e) {
      if (my !== seq || api.isAbort(e)) return;
      replace(list, h('p', { class: 'picker-empty' }, `Could not load the cars: ${e.message}`));
    }
  }
  input.addEventListener('input', () => { clearTimeout(t); t = setTimeout(load, 150); });
  input.focus();
  load();
}

/* ------------------------------------------------------------------ the scan box */

export function openScan() {
  const input = h('input', { class: 'input', type: 'search', placeholder: "Find a page: 'drain', 'cost', 'tyres'…", 'aria-label': 'Find a page', autocomplete: 'off', spellcheck: 'false', role: 'combobox', 'aria-controls': 'hz-scan', 'aria-expanded': 'true' });
  const list = h('div', { class: 'scan-list', id: 'hz-scan', role: 'listbox', 'aria-label': 'Pages' });
  const d = openDialog({ cls: 'scan', label: 'Find a page', body: [h('div', { class: 'scan-top' }, icon('search'), input), list,
    h('div', { class: 'scan-foot' }, h('span', null, '↑ ↓ to choose, Enter to open'), h('span', null, '[ and ] tune to the page before or after'))] });
  const here = state.current().page;
  const go = (o) => { d.close(); state.navigate(o.dataset.href); };
  const nav = keyNav(input, list, go);
  const opt = (s, q) => h('a', {
    class: 'station', href: state.linkTo(s.id), role: 'option', id: `hz-scan-${s.id}`, 'aria-selected': 'false', dataset: { opt: '', href: state.linkTo(s.id) },
    'aria-current': s.id === here ? 'page' : null, onclick: () => d.close(),
  }, icon(s.icon), h('span', { class: 'station-text' }, h('span', { class: 'station-name' }, marked(fullName(s), q)), h('span', { class: 'station-line' }, marked(s.line, q))),
  h('span', { class: 'station-ch' }, `CH ${String(channel(s.id)).padStart(2, '0')}`));
  function paint() {
    const q = input.value.trim().toLowerCase();
    if (!q) {
      replace(list, BANDS.map((b) => [h('p', { class: 'scan-band' }, b.full || b.name), inBand(b.id).map((s) => opt(s, ''))]));
    } else {
      const hits = DIAL.filter((s) => [s.name, s.full, s.short, s.line, s.words, ...(s.parts || [])].filter(Boolean).join(' ').toLowerCase().includes(q));
      replace(list, hits.length ? hits.map((s) => opt(s, q)) : h('p', { class: 'scan-empty' }, `No page matches "${input.value.trim()}". Try a shorter word.`));
    }
    nav.reset();
  }
  input.addEventListener('input', paint);
  paint();
  input.focus();
}
