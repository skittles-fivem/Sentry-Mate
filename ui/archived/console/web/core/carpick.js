// A page's own car choices (key rows and selects) with sold cars behind a "sold cars" group, like the car
// switcher: the cars not sold come first; a sold car shows only when it is kept (the open or chosen car) or
// once "sold cars (n)" is pressed. A select lists the sold ones in an optgroup "sold cars".

import * as state from '../shared/state.js';
import { h } from '../shared/dom.js';

const keepFn = (keep) => (typeof keep === 'function' ? keep : (c) => [].concat(keep ?? []).includes(c.id));

// carKeys(items, key, { keep }) -> nodes: key(c) per car not sold or kept, then the other sold cars hidden
// behind a toggle button. Put the nodes where the keys go (they share the parent).
export function carKeys(items, key, { keep = null } = {}) {
  const kept = keepFn(keep);
  const { active, sold } = state.splitSold(items);
  const shown = sold.filter(kept), rest = sold.filter((c) => !kept(c));
  const mark = (c) => {
    const el = key(c);
    if (el?.classList) { el.classList.add('key--sold'); el.title = el.title || (c.sold_date ? `Sold on ${c.sold_date}` : 'Sold'); }
    return el;
  };
  const out = [...active.map(key), ...shown.map(mark)];
  if (!rest.length) return out;
  const hidden = rest.map((c) => { const el = mark(c); if (el) el.hidden = true; return el; });
  const toggle = h('button', { class: 'key key--soldtoggle', type: 'button', 'aria-expanded': 'false', title: 'Show or hide the sold cars',
    onclick: () => {
      const on = toggle.getAttribute('aria-expanded') !== 'true';
      toggle.setAttribute('aria-expanded', String(on));
      for (const el of hidden) if (el) el.hidden = !on;
    } }, `sold cars (${rest.length})`);
  return [...out, toggle, ...hidden];
}

// carOptions(items, option) -> the options of the cars not sold, then an optgroup "sold cars" with the sold ones.
export function carOptions(items, option) {
  const { active, sold } = state.splitSold(items);
  return [...active.map(option), sold.length ? h('optgroup', { label: 'sold cars' }, sold.map(option)) : null];
}

// pairOptions(pairs, cars, selected) for [value, label] lists: a pair whose value is a sold car's id goes
// into the optgroup "sold cars" at the end; the others keep their order.
export function pairOptions(pairs, cars, selected) {
  const sold = new Set((cars || []).filter(state.isSold).map((c) => c.id));
  const opt = ([v, l]) => h('option', { value: v, selected: v === selected }, l);
  const late = pairs.filter(([v]) => sold.has(v));
  return [...pairs.filter(([v]) => !sold.has(v)).map(opt), late.length ? h('optgroup', { label: 'sold cars' }, late.map(opt)) : null];
}
