// Sentry Mate, "Logbook" UI pack: the car switcher (masthead button, Ctrl K / Cmd K).
//
// A searchable list of the cars this account may see (GET /api/v1/cars?search=, in the person's own car
// order). Choosing a car keeps the page: on Drives it opens that car's Drives with the same period
// (state.linkForCar); a drive or charge goes to the new car's list, a page without a car to its overview.
// Sold cars are left out of the list until "Sold cars (n)" is opened (it opens by itself when the car of the
// page is sold); a search finds them too, marked Sold.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatNumber } from '../shared/i18n.js';
import { formatDayKey } from '../shared/time.js';
import { format } from '../shared/units.js';
import { icon, stateLabel, sep } from './ui.js';

/** createSwitcher(slot) -> { update({ car }), open(), close() }. */
export function createSwitcher(slot) {
  const name = h('span', { class: 'trigger-name' }, 'Choose a car');
  const meta = h('span', { class: 'trigger-meta' }, 'Search all cars');
  const mac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
  const trigger = h('button', { type: 'button', class: 'switcher-trigger', 'aria-haspopup': 'dialog', 'aria-keyshortcuts': 'Control+K Meta+K' },
    h('span', { class: 'trigger-text' }, name, meta), h('kbd', { class: 'kbd' }, mac ? '⌘ K' : 'Ctrl K'));
  slot.replaceChildren(trigger);

  const input = h('input', { class: 'switcher-input', type: 'text', role: 'combobox', 'aria-expanded': 'true', 'aria-controls': 'switcher-list', 'aria-autocomplete': 'list', autocomplete: 'off', spellcheck: 'false', placeholder: 'Find a car by name, model or VIN prefix', 'aria-label': 'Find a car' });
  const list = h('ul', { class: 'switcher-list', id: 'switcher-list', role: 'listbox', 'aria-label': 'Cars' });
  const count = h('span', { class: 'switcher-foot-count', 'aria-live': 'polite' });
  const dlg = h('dialog', { class: 'switcher', 'aria-label': 'Switch car' },
    h('div', { class: 'switcher-search' }, icon('search'), input, h('kbd', { class: 'kbd' }, 'Esc')),
    h('div', { class: 'switcher-body' }, list),
    h('div', { class: 'switcher-foot' },
      h('span', null, h('kbd', { class: 'kbd' }, '↑'), h('kbd', { class: 'kbd' }, '↓'), ' move'),
      h('span', null, h('kbd', { class: 'kbd' }, 'Enter'), ' open'),
      h('span', null, 'The page you are on stays open for the new car'),
      count));
  document.body.append(dlg);

  let options = [], active = 0, seq = 0, timer = 0, currentCar = null, currentSold = false, shown = null, filling = null, soldOpen = false, lastPage = null;

  function setActive(i, scroll = true) {
    if (!options.length) return;
    active = (i + options.length) % options.length;
    options.forEach((o, k) => o.setAttribute('aria-selected', String(k === active)));
    input.setAttribute('aria-activedescendant', options[active].id);
    if (scroll) options[active].scrollIntoView({ block: 'nearest' });
  }
  function optionEl(car, i) {
    const n = car.now || {};
    const stale = !!n.stale;
    const sold = state.isSold(car);
    return h('li', { class: ['switcher-option', { 'is-sold': sold }], role: 'option', id: `switcher-opt-${i}`, 'aria-selected': 'false', dataset: { id: car.id, index: String(i) } },
      h('span', { class: ['state-dot', sold ? 'state-sold' : `state-${n.state || 'unknown'}`, { 'is-stale': stale && !sold }], 'aria-hidden': 'true' }),
      h('span', { class: 'switcher-option-main' },
        h('span', { class: 'switcher-option-name' }, car.name, sold ? h('span', { class: 'tag tag-sold' }, 'Sold') : null, car.id === currentCar ? h('span', { class: 'tag' }, 'Open now') : null),
        h('span', { class: 'switcher-option-meta' }, [car.year, car.model, car.trim].filter(Boolean).join(' '), sep(), h('span', { class: 'mono' }, car.vin_prefix || car.id))),
      sold
        ? h('span', { class: 'switcher-option-side' },
          h('span', { class: 'switcher-option-state' }, 'Sold'),
          h('span', { class: 'switcher-option-place' }, car.sold_date ? formatDayKey(car.sold_date, { weekday: false }) : 'date not given'))
        : h('span', { class: 'switcher-option-side' },
          h('span', { class: ['switcher-option-state', { 'is-stale': stale }] }, stateLabel(n.state, stale), n.soc_pct != null ? ` · ${format(n.soc_pct, 'percent')}` : ''),
          h('span', { class: 'switcher-option-place' }, n.place ? n.place.name : '')));
  }
  /** The "Sold cars (n)" row: an option too, so the arrows and Enter reach it. */
  function soldToggleEl(n, i) {
    return h('li', { class: 'switcher-option switcher-sold-toggle', role: 'option', id: `switcher-opt-${i}`, 'aria-selected': 'false', 'aria-expanded': String(soldOpen), dataset: { toggle: 'sold', index: String(i) } },
      h('span', { class: 'switcher-sold-mark', 'aria-hidden': 'true' }, soldOpen ? '−' : '+'),
      h('span', { class: 'switcher-option-main' }, h('span', { class: 'switcher-option-meta' }, `${soldOpen ? 'Hide' : 'Show'} sold cars (${formatNumber(n)})`)),
      h('span', { class: 'switcher-option-side' }));
  }
  function fill() { filling = fillNow(); return filling; }
  async function fillNow() {
    const query = input.value.trim();
    shown = query;
    const my = ++seq;
    list.setAttribute('aria-busy', 'true');
    let pg;
    try {
      pg = await api.page('cars', { search: query || undefined, sort: 'custom' }, { limit: 50 });
    } catch (err) {
      if (my !== seq) return;
      list.replaceChildren(h('li', { class: 'switcher-empty', role: 'presentation' }, h('strong', null, 'The car list did not load.'), h('span', null, err.message || '')));
      return;
    }
    if (my !== seq) return;
    list.removeAttribute('aria-busy');
    lastPage = { pg, query };
    draw();
  }
  function draw(keepActive = null) {
    const { pg, query } = lastPage;
    const all = pg.items || [];
    const { active: unsold, sold } = state.splitSold(all);
    // Without a search: the cars not sold, then the sold ones behind their toggle. A search lists every match.
    const items = query ? all : [...unsold, ...(soldOpen ? sold : [])];
    const kids = [h('li', { class: 'switcher-group', role: 'presentation' }, query ? 'Matches' : 'Your cars, in your order')];
    let i = 0;
    for (const c of query ? all : unsold) kids.push(optionEl(c, i++));
    if (!query && sold.length) {
      kids.push(h('li', { class: 'switcher-group', role: 'presentation' }, 'Sold cars'));
      kids.push(soldToggleEl(sold.length, i++));
      if (soldOpen) for (const c of sold) kids.push(optionEl(c, i++));
    }
    if (pg.next) kids.push(h('li', { class: 'switcher-more', role: 'presentation' }, 'More cars match. Type more of the name to narrow the list.'));
    if (!all.length) kids.splice(0, 1, h('li', { class: 'switcher-empty', role: 'presentation' }, h('strong', null, `No car matches “${query}”.`), h('span', null, 'Try part of a name, a model or a VIN prefix.')));
    list.replaceChildren(...kids);
    options = [...list.querySelectorAll('.switcher-option')];
    const soldWords = !query && sold.length ? ` · ${formatNumber(sold.length)} sold` : '';
    count.textContent = typeof pg.total === 'number' ? `${formatNumber(pg.total - (query ? 0 : sold.length))} cars${soldWords}` : `${formatNumber(items.length)}${pg.next ? '+' : ''} found`;
    if (keepActive != null) { setActive(keepActive, false); return; }
    setActive(Math.max(0, options.findIndex((o) => o.dataset.id === currentCar && !query)), false);
  }
  /** Enter or a click on an option: open its car, or open / close the sold cars. */
  function choose(o) {
    if (!o) return;
    if (o.dataset.toggle) { soldOpen = !soldOpen; draw(+o.dataset.index); input.focus(); return; }
    go(o.dataset.id);
  }
  function go(id) {
    close();
    state.navigate(state.linkForCar(id));
  }
  function open() {
    if (dlg.open) return;
    input.value = '';
    list.replaceChildren();
    options = [];
    soldOpen = currentSold;
    dlg.showModal();
    input.focus();
    fill();
  }
  function close() { if (dlg.open) dlg.close(); }

  input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { timer = 0; fill(); }, 120); });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setActive(active + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setActive(active - 1); }
    else if (e.key === 'Enter') {
      e.preventDefault();
      // Typed faster than the list followed: show the matches for what is typed now, then open the first.
      const typed = input.value.trim();
      if (typed !== shown || timer) { clearTimeout(timer); timer = 0; fill().then(() => choose(options[active])); return; }
      Promise.resolve(filling).then(() => choose(options[active]));
    }
  });
  list.addEventListener('mousemove', (e) => { const o = e.target.closest('.switcher-option'); if (o && +o.dataset.index !== active) setActive(+o.dataset.index, false); });
  list.addEventListener('click', (e) => { const o = e.target.closest('.switcher-option'); if (o) choose(o); });
  dlg.addEventListener('click', (e) => { if (e.target === dlg) close(); });
  dlg.addEventListener('close', () => trigger.focus({ preventScroll: true }));
  trigger.addEventListener('click', open);
  document.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'k') { e.preventDefault(); if (dlg.open) close(); else open(); }
  });

  return {
    open, close,
    /** Show the car of the page (or that none is open). */
    update({ car, cars }) {
      currentCar = car ? car.id : null;
      currentSold = state.isSold(car);
      if (car) {
        const n = car.now || {};
        name.textContent = car.name;
        meta.textContent = [[car.year, car.model].filter(Boolean).join(' '), currentSold ? 'Sold' : stateLabel(n.state, !!n.stale)].filter(Boolean).join(' · ');
        trigger.title = `${car.name}: switch to another car (the page stays)`;
      } else {
        name.textContent = 'Choose a car';
        meta.textContent = typeof cars === 'number' ? `${formatNumber(cars)} ${cars === 1 ? 'car' : 'cars'} · search` : 'Search all cars';
        trigger.title = 'Open a car';
      }
    },
  };
}
