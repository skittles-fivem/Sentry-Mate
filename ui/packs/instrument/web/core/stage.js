// Off-screen drawing: a part of a page that is drawn again on a live update (a drive or charge started or
// ended) is built in an invisible box of the same width first, charts and all, and then put in place in one
// step. The old part stays on screen until then, so nothing goes away, shows a loading state and comes back.
//
//   await offstage(slot, async (box) => { replace(box, ...); await drawCharts(box); });
//
// box copies slot's tag and classes and sits in copies of its parents (tags and classes only), so the page's
// style sheets give it the same layout; it is fixed, hidden and inert, so it is never seen, focused or
// measured as part of the page. When build resolves (or throws) its children replace slot's, and the keyboard
// focus stays at its place in the part.

import { h, keepFocus } from '../shared/dom.js';

// The page on screen is being scrolled, or was a moment ago. A part of a page, or a page, built again by a live update puts
// its work (the swap with its maps and charts drawn in one task, the snap over the page) off until the scroll has rested:
// such a task takes over a frame on a big screen. The wait is a little longer than one pause of the wheel.
export const SCROLL_REST_MS = 500;
let scrolledAt = -Infinity;
document.addEventListener('scroll', () => { scrolledAt = performance.now(); }, { capture: true, passive: true });
export const scrolling = () => performance.now() - scrolledAt < SCROLL_REST_MS;
/** f runs once the page has rested from scrolling (at once when it is not scrolling). */
export function afterScroll(f) { const left = SCROLL_REST_MS - (performance.now() - scrolledAt); if (left > 0) setTimeout(() => afterScroll(f), left + 20); else f(); }

function shell(slot) {
  const chain = [];
  for (let e = slot.parentElement; e && e !== document.body && e !== document.documentElement; e = e.parentElement) chain.unshift(e);
  const outer = h('div', { 'aria-hidden': 'true', inert: '', dataset: { offstage: '' } });
  Object.assign(outer.style, {
    position: 'fixed', top: '0', left: '0', width: '100vw', height: '100vh', overflow: 'hidden',
    visibility: 'hidden', pointerEvents: 'none', zIndex: '-1', contain: 'strict',
  });
  let at = outer;
  for (const a of chain) {
    const c = document.createElement(a.localName);
    if (a.className && typeof a.className === 'string') c.className = a.className;
    Object.assign(c.style, { display: 'block', position: 'static', margin: '0', padding: '0', border: '0', width: 'auto', height: 'auto', minHeight: '0', overflow: 'visible' });
    at.append(c);
    at = c;
  }
  const box = document.createElement(slot.localName);
  if (slot.className) box.className = slot.className;
  for (const k of Object.keys(slot.dataset)) box.dataset[k] = slot.dataset[k];
  box.style.boxSizing = 'border-box';
  const r = slot.getBoundingClientRect();
  box.style.width = `${r.width}px`;
  at.append(box);
  document.body.append(outer);
  // a part drawn for the first time says where it will land: charts and maps near the screen there are drawn here,
  // the rest once they come near (shared/lazy.js); drawn again (or inside a live refresh of the page), everything
  // is drawn here, as it was on screen
  if (!slot.hasAttribute('data-staged') && !slot.closest('[inert]')) box.dataset.nearShift = String(Math.round(r.top - box.getBoundingClientRect().top));
  return { outer, box };
}

/** Build slot's new contents off screen, then swap them in at once. Resolves (or throws) with build. */
export async function offstage(slot, build) {
  if (!slot.isConnected) {
    try { return await build(slot); } finally { slot.setAttribute('data-staged', ''); }
  }
  const { outer, box } = shell(slot);
  try {
    return await build(box);
  } finally {
    await new Promise(afterScroll); // (while the page scrolls the swap waits: it is one task of snap, layout and drawing)
    if (slot.isConnected) {
      // the charts of the part that goes are taken down with it (core/chart.js keeps each on its box): left up,
      // each would keep its listeners, its observer and its whole drawing until the page is left
      for (const el of slot.querySelectorAll('*')) el.__smPlot?.destroy();
      // the control that had the focus in the old part: the one at its place in the new part gets it
      keepFocus(slot, () => slot.replaceChildren(...box.childNodes));
      slot.setAttribute('data-staged', '');
    }
    outer.remove();
  }
}
