// Painting live figures in place. A page builds a part of itself again from new data (with the same helpers it
// built it with the first time) and morph() makes the elements already on the page look like it, changing only
// the texts and attributes that differ: nothing is taken out and put back, so nothing moves, scroll boxes keep
// their place and a figure that stayed the same costs nothing.
//
//   morphChildren(slot, [status, figs, list])   the slot's children made like these
//   keepEl(el, 'rebuild')                        an element built once and kept as it is (a panel with its own
//                                                state): the first paint puts it in, later ones leave it alone
//
// Rules: an element of another tag, a button that changed (its click handler goes with its data) and an element
// whose data-keep differs are replaced whole; an element with the same data-keep is left as it is.

const KEEP = 'data-keep';

/** The element itself the first time, then a stand-in that tells morph() to leave the one on the page alone. */
export function keepEl(el, name) {
  el.setAttribute(KEEP, name);
  if (!el.parentNode) return el;
  const stand = document.createElement(el.tagName);
  stand.setAttribute(KEEP, name);
  return stand;
}

function syncAttrs(el, to) {
  for (const a of [...el.attributes]) if (!to.hasAttribute(a.name)) el.removeAttribute(a.name);
  for (const a of [...to.attributes]) {
    if (el.getAttribute(a.name) === a.value) continue;
    // custom properties (setVars) through the style object: the policy refuses a style attribute set as text
    if (a.name === 'style') el.style.cssText = to.style.cssText;
    else el.setAttribute(a.name, a.value);
  }
}

/** Makes `el` look like `to` (a node built the same way and not on the page); returns the node on the page. */
export function morph(el, to) {
  if (!el) return to;
  if (el === to) return el;
  if (el.nodeType !== to.nodeType || el.nodeName !== to.nodeName) { el.replaceWith(to); return to; }
  if (el.nodeType === 3 || el.nodeType === 8) {
    if (el.nodeValue !== to.nodeValue) el.nodeValue = to.nodeValue;
    return el;
  }
  if (el.nodeType !== 1) { el.replaceWith(to); return to; }
  const k = to.getAttribute(KEEP);
  if (k != null || el.hasAttribute(KEEP)) {
    if (k === el.getAttribute(KEEP) && !to.firstChild) return el;
    el.replaceWith(to);
    return to;
  }
  if (el.nodeName === 'BUTTON' || el.nodeName === 'INPUT' || el.nodeName === 'SELECT' || el.nodeName === 'TEXTAREA') {
    if (el.isEqualNode(to)) return el;
    el.replaceWith(to);
    return to;
  }
  syncAttrs(el, to);
  morphChildren(el, [...to.childNodes]);
  return el;
}

/** Makes `parent`'s children look like `nodes` (nodes or strings; null and false are left out). */
export function morphChildren(parent, nodes) {
  const want = [].concat(nodes).flat(Infinity).filter((n) => n != null && n !== false)
    .map((n) => (typeof n === 'object' ? n : document.createTextNode(String(n))));
  let i = 0;
  for (; i < want.length; i++) {
    const old = parent.childNodes[i];
    if (!old) { parent.append(...want.slice(i)); return; }
    morph(old, want[i]);
  }
  while (parent.childNodes.length > want.length) parent.lastChild.remove();
}
