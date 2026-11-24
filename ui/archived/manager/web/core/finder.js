// The full menu: every page grouped by task, each with a one-line description, and a type-to-find box
// that matches page names, descriptions and keywords ('drain' finds Battery, 'cost' finds Charges).
// Opened by the menu button in the top bar, by "Find a page" in the sidebar, and by the / key.
// Links carry the current car and period (shared/state.js linkTo).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { GROUPS, PAGES, pagesIn } from './pages.js';
import { icon } from './icons.js';

let dlg = null, input = null, list = null, active = -1, items = [];

const norm = (s) => String(s || '').toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '');

// text with the first match of q marked
function marked(text, q) {
  if (!q) return text;
  const i = norm(text).indexOf(q);
  if (i < 0) return text;
  return [text.slice(0, i), h('mark', null, text.slice(i, i + q.length)), text.slice(i + q.length)];
}

function score(p, q) {
  if (!q) return 1;
  const words = q.split(/\s+/).filter(Boolean);
  let s = 0;
  for (const w of words) {
    const t = norm(p.title), n = norm(p.nav), d = norm(p.desc), k = norm(p.words);
    if (t.startsWith(w) || n.startsWith(w)) s += 4;
    else if (t.includes(w) || n.includes(w)) s += 3;
    else if (k.split(/\s+/).some((x) => x.startsWith(w))) s += 2;
    else if (d.includes(w) || k.includes(w)) s += 1;
    else return 0;
  }
  return s;
}

function render() {
  const q = norm(input.value.trim());
  const here = state.current().page;
  items = [];
  const groups = GROUPS.map((g) => {
    const pages = pagesIn(g.id).map((p) => ({ p, s: score(p, q) })).filter((x) => x.s > 0);
    if (q) pages.sort((a, b) => b.s - a.s);
    if (!pages.length) return null;
    return h('div', { class: 'menu-group', role: 'group', 'aria-label': g.title },
      h('h3', null, g.title),
      pages.map(({ p }) => {
        const a = h('a', {
          class: 'menu-item', href: state.linkTo(p.id), role: 'option', id: `menu-${p.id}`,
          'aria-current': here === p.id || (p.id === 'drives' && here === 'drive') || (p.id === 'charges' && here === 'charge') ? 'page' : null,
          onclick: () => dlg.close(),
        }, icon(p.icon), h('span', null, h('b', null, marked(p.nav || p.title, q)), h('span', null, marked(p.desc, q))));
        items.push(a);
        return a;
      }));
  }).filter(Boolean);
  replace(list, groups.length ? groups : h('p', { class: 'menu-empty' }, `No page matches "${input.value.trim()}". Try a word such as cost, drain, tyres or tax.`));
  setActive(q && items.length ? 0 : -1);
}

function setActive(i) {
  items.forEach((a) => a.classList.remove('is-active'));
  active = i;
  if (i >= 0 && items[i]) {
    items[i].classList.add('is-active');
    items[i].scrollIntoView({ block: 'nearest' });
    input.setAttribute('aria-activedescendant', items[i].id);
  } else input.removeAttribute('aria-activedescendant');
}

function build() {
  input = h('input', {
    type: 'search', placeholder: 'Find a page: drives, cost, drain, tyres …', 'aria-label': 'Find a page', autocomplete: 'off',
    role: 'combobox', 'aria-controls': 'menu-list', 'aria-expanded': 'true', spellcheck: 'false',
  });
  input.addEventListener('input', render);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setActive(Math.min(items.length - 1, active + 1)); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setActive(Math.max(0, active - 1)); }
    else if (e.key === 'Enter') {
      const a = items[active >= 0 ? active : 0];
      if (a) { e.preventDefault(); dlg.close(); state.navigate(a.getAttribute('href')); }
    }
  });
  list = h('div', { class: 'menu-groups', id: 'menu-list', role: 'listbox', 'aria-label': 'Pages' });
  dlg = h('dialog', { class: 'menu-panel', 'aria-label': 'All pages' },
    h('div', { class: 'sw-search' }, icon('search'), input,
      h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', 'aria-label': 'Close the menu', onclick: () => dlg.close() }, icon('x'))),
    list,
    h('div', { class: 'sw-foot' },
      h('span', { class: 'sw-keys' }, h('span', { class: 'kbd' }, '↑'), h('span', { class: 'kbd' }, '↓'), ' choose', h('span', { class: 'sw-sep' }, '·'), h('span', { class: 'kbd' }, 'Enter'), ' open', h('span', { class: 'sw-sep' }, '·'), h('span', { class: 'kbd' }, 'Esc'), ' close'),
      h('span', null, `${PAGES.filter((p) => p.group).length} pages`)));
  dlg.addEventListener('click', (e) => { if (e.target === dlg) dlg.close(); });
  document.body.append(dlg);
}

/** Open the menu. opts.query fills the find box. */
export function openFinder({ query = '' } = {}) {
  if (!dlg) build();
  input.value = query;
  render();
  if (!dlg.open) dlg.showModal();
  input.focus();
}
