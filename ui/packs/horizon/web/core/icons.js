// Icons from the sprite in index.html: icon('bolt') -> <svg class="ico"><use href="#i-bolt"/></svg>.
import { svg } from '../shared/dom.js';

/** An icon from the sprite. cls adds classes ('ico--sm', 'ico--lg'). Decorative: hidden from screen readers. */
export function icon(id, cls = '') {
  return svg('svg', { class: ['ico', cls], 'aria-hidden': 'true', focusable: 'false' }, svg('use', { href: `#i-${id}` }));
}

/** The brand mark: Horizon's striped sun over the grid lines. */
export function brandMark(cls = 'brand-mark') {
  return svg('svg', { class: cls, viewBox: '0 0 32 32', 'aria-hidden': 'true' },
    svg('circle', { cx: 16, cy: 14, r: 9, fill: 'url(#g-sun)' }),
    svg('path', { class: 'brand-mark-grid', d: 'M5 17.5h22M6.5 20.5h19M4 24h24M8 27.5h16', fill: 'none', 'stroke-width': 1.6, 'stroke-linecap': 'round' }));
}
