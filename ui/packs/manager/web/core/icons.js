// Icons from the sprite in index.html: icon('bolt') -> <svg class="ico"><use href="#i-bolt"/></svg>.
import { svg } from '../shared/dom.js';

/** An icon from the sprite. cls adds classes ('ico--sm', 'ico--lg'). Decorative: hidden from screen readers. */
export function icon(id, cls = '') {
  return svg('svg', { class: ['ico', cls], 'aria-hidden': 'true', focusable: 'false' }, svg('use', { href: `#i-${id}` }));
}

/** The brand mark (the logo square with the pulse line). */
export function brandMark(cls = 'brand-mark') {
  return svg('svg', { class: cls, viewBox: '0 0 32 32', 'aria-hidden': 'true' },
    svg('rect', { x: 1, y: 1, width: 30, height: 30, rx: 9, fill: 'url(#g-logo)' }),
    svg('path', { d: 'M7.5 17.5h4.2l2.6-6.3 3.4 9.6 2.4-5.4h4.4', fill: 'none', stroke: '#06111C', 'stroke-width': 2.4, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
}
