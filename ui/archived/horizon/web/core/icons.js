// Icons from the sprite in index.html: icon('bolt') -> <svg class="ico" viewBox="0 0 24 24"> with a copy of the
// shapes of <symbol id="i-bolt">.
//
// A copy, not a <use href="#i-bolt">: Safari (WebKit) lays the page out the moment a <use> is taken out of it, so a
// part of a page drawn again (a live refresh swapping the page, a list's rows) was for that instant as short as the
// page without it, the window was scrolled up to fit, and the reader was thrown back towards the top.
import { svg } from '../shared/dom.js';

const symbols = new Map(); // id -> the sprite's <symbol>, or null

/** An icon from the sprite. cls adds classes ('ico--sm', 'ico--lg'). Decorative: hidden from screen readers. */
export function icon(id, cls = '') {
  let sym = symbols.get(id);
  if (sym === undefined) { sym = document.getElementById(`i-${id}`); symbols.set(id, sym); }
  const el = svg('svg', { class: ['ico', cls], viewBox: sym?.getAttribute('viewBox') || '0 0 24 24', 'aria-hidden': 'true', focusable: 'false' });
  if (sym) for (const c of sym.childNodes) if (c.nodeType === 1) el.append(c.cloneNode(true));
  return el;
}

/** The brand mark: Horizon's striped sun over the grid lines. */
export function brandMark(cls = 'brand-mark') {
  return svg('svg', { class: cls, viewBox: '0 0 32 32', 'aria-hidden': 'true' },
    svg('circle', { cx: 16, cy: 14, r: 9, fill: 'url(#g-sun)' }),
    svg('path', { class: 'brand-mark-grid', d: 'M5 17.5h22M6.5 20.5h19M4 24h24M8 27.5h16', fill: 'none', 'stroke-width': 1.6, 'stroke-linecap': 'round' }));
}
