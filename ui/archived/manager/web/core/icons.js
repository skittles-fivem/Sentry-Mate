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

/** The brand mark (the logo square with the pulse line). */
export function brandMark(cls = 'brand-mark') {
  return svg('svg', { class: cls, viewBox: '0 0 32 32', 'aria-hidden': 'true' },
    svg('rect', { x: 1, y: 1, width: 30, height: 30, rx: 9, fill: 'url(#g-logo)' }),
    svg('path', { d: 'M7.5 17.5h4.2l2.6-6.3 3.4 9.6 2.4-5.4h4.4', fill: 'none', stroke: '#06111C', 'stroke-width': 2.4, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
}
