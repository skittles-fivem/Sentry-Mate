// Sentry Mate, "Logbook" UI pack: the car switcher (masthead button, Ctrl K / Cmd K).
//
// A searchable list of the cars this account may see (GET /api/v1/cars?search=, in the person's own car
// order). Choosing a car keeps the page: on Drives it opens that car's Drives with the same period
// (state.linkForCar); a drive or charge goes to the new car's list, a page without a car to its overview.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatNumber } from '../shared/i18n.js';
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

  let options = [], active = 0, seq = 0, timer = 0, currentCar = null, shown = null, filling = null;

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
    return h('li', { class: 'switcher-option', role: 'option', id: `switcher-opt-${i}`, 'aria-selected': 'false', dataset: { id: car.id, index: String(i) } },
      h('span', { class: ['state-dot', `state-${n.state || 'unknown'}`, { 'is-stale': stale }], 'aria-hidden': 'true' }),
      h('span', { class: 'switcher-option-main' },
        h('span', { class: 'switcher-option-name' }, car.name, car.id === currentCar ? h('span', { class: 'tag' }, 'Open now') : null),
        h('span', { class: 'switcher-option-meta' }, [car.year, car.model, car.trim].filter(Boolean).join(' '), sep(), h('span', { class: 'mono' }, car.vin_prefix || car.id))),
      h('span', { class: 'switcher-option-side' },
        h('span', { class: ['switcher-option-state', { 'is-stale': stale }] }, stateLabel(n.state, stale), n.soc_pct != null ? ` · ${format(n.soc_pct, 'percent')}` : ''),
        h('span', { class: 'switcher-option-place' }, n.place ? n.place.name : '')));
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
    const items = pg.items || [];
    const kids = [h('li', { class: 'switcher-group', role: 'presentation' }, query ? 'Matches' : 'Your cars, in your order')];
    items.forEach((c, i) => kids.push(optionEl(c, i)));
    if (pg.next) kids.push(h('li', { class: 'switcher-more', role: 'presentation' }, 'More cars match. Type more of the name to narrow the list.'));
    if (!items.length) kids.splice(0, 1, h('li', { class: 'switcher-empty', role: 'presentation' }, h('strong', null, `No car matches “${query}”.`), h('span', null, 'Try part of a name, a model or a VIN prefix.')));
    list.replaceChildren(...kids);
    options = [...list.querySelectorAll('.switcher-option')];
    count.textContent = typeof pg.total === 'number' ? `${formatNumber(pg.total)} cars` : `${formatNumber(items.length)}${pg.next ? '+' : ''} found`;
    setActive(Math.max(0, items.findIndex((c) => c.id === currentCar && !query)), false);
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
      if (typed !== shown || timer) { clearTimeout(timer); timer = 0; fill().then(() => { if (options[active]) go(options[active].dataset.id); }); return; }
      Promise.resolve(filling).then(() => { if (options[active]) go(options[active].dataset.id); });
    }
  });
  list.addEventListener('mousemove', (e) => { const o = e.target.closest('.switcher-option'); if (o && +o.dataset.index !== active) setActive(+o.dataset.index, false); });
  list.addEventListener('click', (e) => { const o = e.target.closest('.switcher-option'); if (o) go(o.dataset.id); });
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
      if (car) {
        const n = car.now || {};
        name.textContent = car.name;
        meta.textContent = [[car.year, car.model].filter(Boolean).join(' '), stateLabel(n.state, !!n.stale)].filter(Boolean).join(' · ');
        trigger.title = `${car.name}: switch to another car (the page stays)`;
      } else {
        name.textContent = 'Choose a car';
        meta.textContent = typeof cars === 'number' ? `${formatNumber(cars)} ${cars === 1 ? 'car' : 'cars'} · search` : 'Search all cars';
        trigger.title = 'Open a car';
      }
    },
  };
}
