// The car switcher: a dialog with a filter line and the cars as rows (name, state, battery). Choosing one
// keeps the page (a drive or charge goes to the new car's list). Large fleets page with "more".

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { openDialog } from './dialog.js';
import { stateWord, numText } from './fmt.js';
import { errorWords } from './states.js';

export function openCarSwitcher() {
  const q = h('input', { class: 'in in--wide', type: 'search', placeholder: 'filter cars', 'aria-label': 'Filter cars', autocomplete: 'off', spellcheck: 'false' });
  const list = h('ul', { class: 'pick', role: 'listbox', 'aria-label': 'Cars' });
  const more = h('button', { class: 'btn', type: 'button', hidden: true }, 'more');
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
  const row = (c) => h('li', { class: 'pick-row', role: 'option', 'aria-selected': 'false' },
    h('a', { class: 'pick-link', href: state.linkForCar(c.id), onclick: () => d.close() },
      h('span', { class: 'pick-main' }, c.name || c.id, c.id === here ? h('span', { class: 'muted' }, ' (open)') : null),
      stateWord(c.now?.state, { stale: c.now?.stale }),
      h('span', { class: 'pick-num' }, numText(c.now?.soc_pct, 'percent'))));

  async function load() {
    const my = ++seq;
    pager = api.pager('cars', { sort: 'name', search: q.value.trim() || undefined }, { limit: 50 });
    replace(list, h('li', { class: 'pick-note' }, 'loading', h('span', { class: 'caret' })));
    try {
      await pager.more();
      if (my !== seq) return;
      replace(list, pager.items.length ? pager.items.map(row) : h('li', { class: 'pick-note' }, '-- no car matches --'));
      more.hidden = pager.done;
      mark(0);
    } catch (e) {
      if (my !== seq || api.isAbort(e)) return;
      replace(list, h('li', { class: 'pick-note' }, `[bad] ${errorWords(e).text}`));
    }
  }
  more.addEventListener('click', async () => {
    const n = pager.items.length;
    await pager.more();
    list.append(...pager.items.slice(n).map(row));
    more.hidden = pager.done;
  });
  let t = 0;
  q.addEventListener('input', () => { clearTimeout(t); t = setTimeout(load, 150); });
  q.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); mark(sel + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); mark(sel - 1); }
    else if (e.key === 'Enter') { e.preventDefault(); rows()[sel]?.querySelector('a')?.click(); }
  });
  const d = openDialog({ title: 'switch car', cls: 'dlg--pick', body: [h('div', { class: 'pick-q' }, h('span', { class: 'prompt' }, '>'), q), list], actions: [more] });
  q.focus();
  load();
  return d;
}
