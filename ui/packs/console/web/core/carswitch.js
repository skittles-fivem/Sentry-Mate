// The car switcher: a dialog with a filter line and the cars as rows (name, state, battery). Choosing one
// keeps the page (a drive or charge goes to the new car's list). Large fleets page with "more".
// Sold cars are left out (the open one stays) until "sold cars (n)" is pressed; a search finds them, marked.

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { openDialog } from './dialog.js';
import { stateWord, numText, tag } from './fmt.js';
import { errorWords } from './states.js';

export function openCarSwitcher() {
  const q = h('input', { class: 'in in--wide', type: 'search', placeholder: 'filter cars', 'aria-label': 'Filter cars', autocomplete: 'off', spellcheck: 'false' });
  const list = h('ul', { class: 'pick', role: 'listbox', 'aria-label': 'Cars' });
  const more = h('button', { class: 'btn', type: 'button', hidden: true }, 'more');
  const soldBtn = h('button', { class: 'key', type: 'button', hidden: true, 'aria-pressed': 'false', onclick: () => { showSold = !showSold; paintList(); } });
  let showSold = false;
  const here = state.current().car;
  let pager = null, seq = 0, sel = 0;

  const rows = () => [...list.querySelectorAll('.pick-row')];
  const mark = (i) => {
    const r = rows();
    if (!r.length) return;
    sel = Math.max(0, Math.min(r.length - 1, i));
    r.forEach((x, k) => x.setAttribute('aria-selected', String(k === sel)));
    r[sel].scrollIntoView({ block: 'nearest' });
  };
  const row = (c) => {
    const sold = state.isSold(c);
    return h('li', { class: ['pick-row', sold && 'pick-row--sold'], role: 'option', 'aria-selected': 'false' },
      h('a', { class: 'pick-link', href: state.linkForCar(c.id), onclick: () => d.close() },
        h('span', { class: 'pick-main' }, c.name || c.id, c.id === here ? h('span', { class: 'muted' }, ' (open)') : null),
        sold ? h('span', { title: c.sold_date ? `Sold on ${c.sold_date}` : 'Sold' }, tag('sold', 'muted')) : stateWord(c.now?.state, { stale: c.now?.stale }),
        h('span', { class: 'pick-num' }, sold ? (c.sold_date || '') : numText(c.now?.soc_pct, 'percent'))));
  };
  // with no filter text: the cars not sold (and the open one), then the sold ones when asked for
  function shownCars() {
    const items = pager?.items || [];
    if (q.value.trim()) return items;
    const { active, sold } = state.splitSold(items);
    const n = sold.length;
    soldBtn.hidden = !n;
    soldBtn.setAttribute('aria-pressed', String(showSold));
    soldBtn.textContent = `sold cars (${n})`;
    if (showSold) return [...active, ...sold];
    return [...active, ...sold.filter((c) => c.id === here)];
  }
  function paintList() {
    if (q.value.trim()) soldBtn.hidden = true;
    const cars = shownCars();
    replace(list, cars.length ? cars.map(row) : h('li', { class: 'pick-note' }, '-- no car matches --'));
    mark(0);
  }

  async function load() {
    const my = ++seq;
    pager = api.pager('cars', { sort: 'name', search: q.value.trim() || undefined }, { limit: 50 });
    replace(list, h('li', { class: 'pick-note' }, 'loading', h('span', { class: 'caret' })));
    try {
      await pager.more();
      if (my !== seq) return;
      paintList();
      more.hidden = pager.done;
    } catch (e) {
      if (my !== seq || api.isAbort(e)) return;
      replace(list, h('li', { class: 'pick-note' }, `[bad] ${errorWords(e).text}`));
    }
  }
  more.addEventListener('click', async () => {
    const n = pager.items.length;
    await pager.more();
    if (pager.items.length > n) paintList();
    more.hidden = pager.done;
  });
  let t = 0;
  q.addEventListener('input', () => { clearTimeout(t); t = setTimeout(load, 150); });
  q.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); mark(sel + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); mark(sel - 1); }
    else if (e.key === 'Enter') { e.preventDefault(); rows()[sel]?.querySelector('a')?.click(); }
  });
  const d = openDialog({ title: 'switch car', cls: 'dlg--pick', body: [h('div', { class: 'pick-q' }, h('span', { class: 'prompt' }, '>'), q), list], actions: [soldBtn, more] });
  q.focus();
  load();
  return d;
}
