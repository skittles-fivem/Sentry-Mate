// Place a popover (an element with the popover attribute, shown in the top layer) next to the button
// that opened it, inside the window. Positions are set through CSSOM (the page policy blocks inline
// style attributes, not el.style).

/**
 * placePopover(pop, anchor, { side: 'below' | 'above', align: 'start' | 'end', gap }) after showPopover().
 * Flips to the other side when there is no room, and keeps a margin from the window's edges.
 */
export function placePopover(pop, anchor, { side = 'below', align = 'start', gap = 8 } = {}) {
  const a = anchor.getBoundingClientRect();
  const w = pop.offsetWidth, hgt = pop.offsetHeight;
  const vw = document.documentElement.clientWidth, vh = window.innerHeight;
  const m = 8;
  let top = side === 'above' ? a.top - gap - hgt : a.bottom + gap;
  if (side === 'below' && top + hgt > vh - m && a.top - gap - hgt >= m) top = a.top - gap - hgt;
  if (side === 'above' && top < m && a.bottom + gap + hgt <= vh - m) top = a.bottom + gap;
  let left = align === 'end' ? a.right - w : a.left;
  left = Math.max(m, Math.min(left, vw - w - m));
  top = Math.max(m, Math.min(top, vh - hgt - m));
  pop.style.setProperty('top', `${Math.round(top)}px`);
  pop.style.setProperty('left', `${Math.round(left)}px`);
}
