// Off-screen drawing: a part of a page that is drawn again on a live update (a drive or charge started or
// ended) is built in an invisible box of the same width first, charts and all, and then put in place in one
// step. The old part stays on screen until then, so nothing goes away, shows a loading state and comes back.
//
//   await offstage(slot, async (box) => { replace(box, ...); await drawCharts(box); });
//
// box copies slot's tag and classes and sits in copies of its parents (tags and classes only), so the page's
// style sheets give it the same layout; it is fixed, hidden and inert, so it is never seen, focused or
// measured as part of the page. When build resolves (or throws) its children replace slot's.

import { h } from '../shared/dom.js';

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
  box.style.width = `${slot.getBoundingClientRect().width}px`;
  at.append(box);
  document.body.append(outer);
  return { outer, box };
}

/** Build slot's new contents off screen, then swap them in at once. Resolves (or throws) with build. */
export async function offstage(slot, build) {
  if (!slot.isConnected) return build(slot);
  const { outer, box } = shell(slot);
  try {
    return await build(box);
  } finally {
    if (slot.isConnected) slot.replaceChildren(...box.childNodes);
    outer.remove();
  }
}
