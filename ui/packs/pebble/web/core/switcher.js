// Sentry Mate, "Pebble" UI pack: the car switcher, a sheet opened from the round car button of the dock, the car
// name in each page header, or Ctrl K / Cmd K.
//
// A searchable list of the cars this account may see (GET /api/v1/cars?search=, in the person's own order).
// Choosing a car keeps the page: on Drives it opens that car's Drives with the same period (state.linkForCar);
// a drive or a charge goes to the new car's list, and a page without a car to the car's overview.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatNumber } from '../shared/i18n.js';
import { format } from '../shared/units.js';
import { createSheet } from './sheet.js';
import { icon, stateTag, initial } from './ui.js';

let sheet = null;
let input, list, count;
let options = [], active = 0, seq = 0, timer = 0, currentCar = null;
let shown = null, showSold = false; // the last answer { items, next, total, query }, and whether sold cars are open

function optionEl(car, i) {
  const n = car.now || {};
  const st = n.state || 'unknown';
  return h('li', { class: ['row', 'car-row', { 'is-current': car.id === currentCar, 'is-sold': !!car.sold }], role: 'option', id: `car-opt-${i}`, 'aria-selected': 'false', dataset: { id: car.id, index: String(i) } },
    h('span', { class: ['avatar', `st-${st}`], 'aria-hidden': 'true' }, initial(car.name)),
    h('span', { class: 'row-text' },
      h('span', { class: 'row-name' }, car.name),
      h('span', { class: 'row-sub' }, [car.year, car.model].filter(Boolean).join(' ') || car.vin_prefix || car.id)),
    car.sold ? h('span', { class: 'row-side' }, h('span', { class: 'chip tone-muted' }, h('span', null, 'Sold')))
      : h('span', { class: 'row-side' }, stateTag(st, { stale: !!n.stale }), n.soc_pct != null ? h('span', { class: 'car-soc' }, format(n.soc_pct, 'percent')) : null));
}
/** The row that opens or closes the sold cars: 'Sold cars (2)'. */
function soldToggleEl(n, i) {
  return h('li', { class: ['row', 'car-row', 'sold-toggle'], role: 'option', id: `car-opt-${i}`, 'aria-selected': 'false', 'aria-expanded': String(showSold), dataset: { toggle: '1', index: String(i) } },
    h('span', { class: 'row-ic', 'aria-hidden': 'true' }, icon(showSold ? 'chevronUp' : 'chevronDown')),
    h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, `Sold cars (${formatNumber(n)})`),
      h('span', { class: 'row-sub' }, showSold ? 'Hide the cars that were sold.' : 'Their history stays; open one to browse it.')));
}

function setActive(i, scroll = true) {
  if (!options.length) return;
  active = (i + options.length) % options.length;
  options.forEach((o, k) => o.setAttribute('aria-selected', String(k === active)));
  input.setAttribute('aria-activedescendant', options[active].id);
  if (scroll) options[active].scrollIntoView({ block: 'nearest' });
}

let filling = null;
function fill() { filling = fillNow(); return filling; }
async function fillNow() {
  const query = input.value.trim();
  const my = ++seq;
  list.setAttribute('aria-busy', 'true');
  let pg;
  try {
    pg = await api.page('cars', { search: query || undefined, sort: 'custom' }, { limit: 50 });
  } catch (err) {
    if (my !== seq) return;
    list.removeAttribute('aria-busy');
    list.replaceChildren(h('li', { class: 'list-empty', role: 'presentation' }, h('strong', null, 'The car list did not load.'), h('span', null, err.message || '')));
    return;
  }
  if (my !== seq) return;
  list.removeAttribute('aria-busy');
  shown = { items: pg.items || [], next: pg.next, total: pg.total, query };
  draw();
}
function draw(keep = false) {
  const { items, next: more, total, query } = shown;
  // Sold cars are left out of the plain list, behind 'Sold cars (n)'; a search finds them too, marked Sold.
  const { active: unsold, sold } = query ? { active: items, sold: [] } : state.splitSold(items);
  if (!query && !keep && sold.some((c) => c.id === currentCar)) showSold = true;
  const rows = unsold.map(optionEl);
  if (sold.length) {
    rows.push(soldToggleEl(sold.length, rows.length));
    if (showSold) for (const c of sold) rows.push(optionEl(c, rows.length));
  }
  const pg = { next: more, total };
  list.replaceChildren(...rows);
  if (pg.next) list.append(h('li', { class: 'list-more', role: 'presentation' }, 'More cars match. Type more of the name.'));
  if (!items.length) list.replaceChildren(h('li', { class: 'list-empty', role: 'presentation' }, h('strong', null, query ? `No car matches “${query}”.` : 'No cars yet.'), h('span', null, query ? 'Try part of a name, a model or a VIN prefix.' : 'A car shows once its CAN Bridge uploads.')));
  options = [...list.querySelectorAll('.car-row')];
  const carsWord = (n) => `${formatNumber(n)} ${n === 1 ? 'car' : 'cars'}`;
  count.textContent = typeof pg.total === 'number' ? (sold.length ? `${carsWord(pg.total - sold.length)}, ${formatNumber(sold.length)} sold` : carsWord(pg.total)) : `${formatNumber(items.length)}${pg.next ? '+' : ''} found`;
  const cur = options.findIndex((o) => o.dataset.id === currentCar && !query);
  setActive(keep ? options.findIndex((o) => o.dataset.toggle) : Math.max(0, cur), false);
}
function choose(o) {
  if (!o) return;
  if (o.dataset.toggle) { showSold = !showSold; draw(true); return; }
  go(o.dataset.id);
}

function go(id) {
  sheet.close();
  state.navigate(state.linkForCar(id));
}

function build() {
  input = h('input', { class: 'input search-input', type: 'search', role: 'combobox', 'aria-expanded': 'true', 'aria-controls': 'car-list', 'aria-autocomplete': 'list', autocomplete: 'off', spellcheck: 'false', placeholder: 'Find a car', 'aria-label': 'Find a car by name, model or VIN prefix', dataset: { autofocus: '' } });
  list = h('ul', { class: 'rows-list car-list', id: 'car-list', role: 'listbox', 'aria-label': 'Cars' });
  count = h('p', { class: 'sheet-note', 'aria-live': 'polite' });
  sheet = createSheet({ title: 'Your cars', cls: 'car-sheet' });
  sheet.body.append(h('label', { class: 'search' }, icon('search'), input), list, count,
    h('p', { class: 'sheet-note' }, 'The page you are on stays open for the car you pick.'));
  input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { timer = 0; fill(); }, 120); });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setActive(active + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setActive(active - 1); }
    else if (e.key === 'Enter') {
      e.preventDefault();
      if (timer) { clearTimeout(timer); timer = 0; fill().then(() => { choose(options[active]); }); return; }
      Promise.resolve(filling).then(() => choose(options[active]));
    }
  });
  list.addEventListener('click', (e) => { choose(e.target.closest('.car-row')); });
}

/** Open the switcher (opener gets the focus back when it closes). */
export function openSwitcher(opener) {
  if (!sheet) build();
  input.value = '';
  list.replaceChildren();
  options = [];
  showSold = false;
  sheet.open(opener);
  input.focus();
  fill();
}
/** The car the page shows now (marked in the list). */
export function setSwitcherCar(id) { currentCar = id || null; }

document.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'k' && !document.documentElement.classList.contains('is-auth')) {
    e.preventDefault();
    if (sheet && sheet.isOpen) sheet.close(); else openSwitcher(document.activeElement);
  }
});
