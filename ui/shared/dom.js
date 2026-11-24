// Sentry Mate shared module: dom.js (small safe DOM helpers).
// Canonical source: ui/shared/dom.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// h('a', { class: 'x', href }, 'text', child) builds elements with DOM calls only, so car and place names
// from the API are always text, never markup, and pages work under the strict Content-Security-Policy:
// no innerHTML, no inline style attributes (sizes go through CSSOM as CSS custom properties), no string
// event handlers, no javascript: links. Also: cover() for coverage marking (data-cover) and ready() /
// unready() / failed() for the page readiness contract (data-ready on <html>), and redraw() / keepFocus() for
// parts drawn again on live updates.

import { parts as unitParts } from './units.js';

export const SVG_NS = 'http://www.w3.org/2000/svg';
const URL_ATTRS = new Set(['href', 'src', 'action', 'formaction', 'xlink:href', 'poster', 'cite', 'data']);
const PROPS = new Set(['value', 'checked', 'selected', 'indeterminate', 'muted']);
// What h() set a control to (value / checked / selected are properties, so the markup's defaults stay empty): a page
// drawn again asks what the reader changed by comparing a control with this (builtWith), not with its defaults.
const BUILT = new WeakMap();
/** What h() built el with for prop ('value', 'checked' or 'selected'), or undefined when it did not set it. */
export function builtWith(el, prop) { const b = BUILT.get(el); return b && prop in b ? b[prop] : undefined; }

/** Escape text for the rare place a pack still builds HTML text (prefer h()). */
export function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

/**
 * A URL that is safe in href or src: relative, http(s), mailto, tel, blob, or a #fragment. Anything else
 * (javascript:, data:, vbscript: ...) gives null. data: images are allowed in src when allowData is set.
 */
export function safeUrl(u, { allowData = false } = {}) {
  if (u == null) return null;
  const s = String(u).trim();
  if (!s) return null;
  // drop invisible and control characters a browser would ignore when reading the scheme
  // eslint-disable-next-line no-control-regex
  const probe = s.replace(/[\u0000-\u0020\u007f-\u009f\u200b-\u200f\u2028\u2029\ufeff]/g, '').toLowerCase();
  const m = /^([a-z][a-z0-9+.-]*):/.exec(probe);
  if (!m) return s;
  const scheme = m[1];
  if (['http', 'https', 'mailto', 'tel', 'blob'].includes(scheme)) return s;
  if (allowData && scheme === 'data' && /^data:image\/(png|jpeg|gif|webp);/.test(probe)) return s;
  return null;
}

function classText(c) {
  if (!c) return '';
  if (Array.isArray(c)) return c.map(classText).filter(Boolean).join(' ');
  if (typeof c === 'object') return Object.keys(c).filter((k) => c[k]).join(' ');
  return String(c);
}

/** Set CSS custom properties through CSSOM: setVars(el, { '--w': '42%', '--n': 3 }). */
export function setVars(el, vars) {
  if (!el || !vars) return el;
  for (const [k, v] of Object.entries(vars)) {
    const name = k.startsWith('--') ? k : '--' + k;
    if (v == null || v === false) el.style.removeProperty(name);
    else el.style.setProperty(name, String(v));
  }
  return el;
}
/** Read a CSS custom property (theme colours for charts and maps). */
export function cssVar(name, el = globalThis.document?.documentElement) {
  if (!el) return '';
  return getComputedStyle(el).getPropertyValue(name.startsWith('--') ? name : '--' + name).trim();
}

function setAttrs(el, attrs, isSvg) {
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === 'class' || k === 'className') {
      const c = classText(v);
      if (c) el.setAttribute('class', c);
    } else if (k === 'dataset') {
      for (const [dk, dv] of Object.entries(v)) if (dv != null) el.dataset[dk] = String(dv);
    } else if (k === 'vars') {
      setVars(el, v);
    } else if (k === 'style') {
      if (typeof v === 'object') for (const [sk, sv] of Object.entries(v)) { if (sv != null) el.style.setProperty(sk, String(sv)); }
      else console.warn('dom.js: style must be an object (inline style text is blocked by the page policy)');
    } else if (k === 'cover') {
      cover(el, ...(Array.isArray(v) ? v : [v]));
    } else if (k.startsWith('on')) {
      if (typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
      // a string handler would be code in an attribute: never set
    } else if (k === 'html' || k === 'innerHTML' || k === 'outerHTML' || k === 'srcdoc') {
      console.warn(`dom.js: ${k} is not supported; pass children instead`);
    } else if (URL_ATTRS.has(k)) {
      const u = safeUrl(v, { allowData: k === 'src' });
      if (u != null) el.setAttribute(k, u);
    } else if (!isSvg && PROPS.has(k)) {
      el[k] = v;
      let b = BUILT.get(el);
      if (!b) BUILT.set(el, b = {});
      b[k] = el[k];
    } else if (v === true) {
      el.setAttribute(k, '');
    } else {
      el.setAttribute(k, String(v));
    }
  }
}

/** Append children: strings and numbers become text, arrays are flattened, null / false / undefined skipped. */
export function append(el, ...children) {
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false || c === true || c === '') continue;
    const doc = el.ownerDocument || globalThis.document;
    el.append(typeof c === 'object' && 'nodeType' in c ? c : doc.createTextNode(String(c)));
  }
  return el;
}

/**
 * Build an element. attrs: class (text, array or { name: on }), dataset, vars (CSS custom properties),
 * style (an object, set through CSSOM), cover (item ids), on<event> functions, value / checked
 * properties, and plain attributes (true gives an empty attribute; null and false are left out).
 */
export function h(tag, attrs, ...children) {
  const el = globalThis.document.createElement(tag);
  if (attrs && (typeof attrs !== 'object' || 'nodeType' in attrs || Array.isArray(attrs))) { children.unshift(attrs); attrs = null; }
  if (attrs) setAttrs(el, attrs, false);
  return append(el, children);
}

/** The same for SVG elements: svg('path', { d: 'M0 0h10' }). */
export function svg(tag, attrs, ...children) {
  const el = globalThis.document.createElementNS(SVG_NS, tag);
  if (attrs && (typeof attrs !== 'object' || 'nodeType' in attrs || Array.isArray(attrs))) { children.unshift(attrs); attrs = null; }
  if (attrs) setAttrs(el, attrs, true);
  return append(el, children);
}

/** A text node. */
export const text = (s) => globalThis.document.createTextNode(String(s ?? ''));
/** A fragment of several children. */
export const frag = (...children) => append(globalThis.document.createDocumentFragment(), children);
// Where the page is scrolled, kept across the swaps of one task: scroll anchoring for the parts a page draws again.
// Safari on iOS has no scroll anchoring of its own, so a part above the screen that is drawn again a line taller (a
// list with a new row at its top, a total whose note wraps) moved everything on screen down; and Safari (WebKit) can
// lay a page out in the middle of a swap (the old children out, the new ones not in yet) and scroll the window up to
// fit that shorter page, throwing the reader towards the top. So the first swap of a part on the page in a task notes,
// for each box that scrolls (the window, and any element marked data-scroll-box: a pack whose page scrolls inside a
// fixed box), where it is scrolled and the first block of the page from the top of what it shows; at the end of the task (a microtask,
// before anything is painted) the box is scrolled by as much as that block moved, or back where it was when the block
// itself was drawn again. A part built off the page (not connected, or in an [inert] staging box) costs nothing.
let held = null;
const FIXED = new Set(['fixed', 'sticky']);
function anchorIn(doc, b) {
  const win = b === doc.scrollingElement;
  const view = doc.defaultView;
  const r = win ? { left: 0, top: 0, width: view.innerWidth, height: view.innerHeight } : b.getBoundingClientRect();
  // the first block of the page from the top of what the box shows, as browsers anchor: what is above it changing
  // moves nothing on screen (the points step down past a bar fixed over the top)
  for (const f of [0.1, 0.2, 0.35, 0.5]) {
    let a = doc.elementFromPoint?.(r.left + r.width / 2, r.top + r.height * f) || null;
    for (let x = a; x && x !== b && x !== doc.body; x = x.parentElement) if (FIXED.has(view.getComputedStyle(x).position)) { a = null; break; }
    if (a && b.contains(a) && a !== b) return a;
  }
  return null;
}
/** Keep what is on screen in place across the swaps of el's children in this task (see above). */
export function holdScroll(el) {
  if (held || !el?.isConnected || el.closest?.('[inert]')) return;
  const doc = el.ownerDocument || globalThis.document;
  const boxes = [];
  const note = (b) => {
    const t = b.scrollTop;
    if (!(t > 0)) return;
    const a = anchorIn(doc, b);
    boxes.push([b, t, a, a ? a.getBoundingClientRect().top : 0]);
  };
  if (doc.scrollingElement) note(doc.scrollingElement);
  for (const b of doc.querySelectorAll?.('[data-scroll-box]') || []) note(b);
  held = boxes;
  // looked at again after the mutation observers this change set off (the whole-pixel snap of a pack, which gives
  // rows drawn again their whole-pixel heights back): their callbacks were queued after this one, so this waits one
  // turn more. Measured before them, the rows' heights of a moment would be taken for a move, and the page scrolled
  // by what the snap then puts back.
  queueMicrotask(() => queueMicrotask(() => {
    const list = held;
    held = null;
    for (const [b, t, a, at] of list) {
      if (!b.isConnected) continue;
      // the anchor's move on screen, plus what the box itself was scrolled meanwhile, is how far it moved in the page
      const want = a?.isConnected ? t + (a.getBoundingClientRect().top - at) + (b.scrollTop - t) : t;
      if (Math.abs(b.scrollTop - want) >= 1) b.scrollTop = want;
      // a page that got shorter under a reader at its end (a list above drawn again with fewer rows) is held as tall
      // as it was, so the box is not pulled up: the room is given back as the page grows again or the reader scrolls up
      if (want - b.scrollTop >= 1) { growPad(doc, b, want - b.scrollTop); b.scrollTop = want; }
      fitPad(b);
    }
  }));
}

// room under a page, kept so it does not get shorter under the reader (see holdScroll)
const pads = new Map(); // box -> { el, px }
function growPad(doc, b, px) {
  let p = pads.get(b);
  if (!p) {
    const el = doc.createElement('div');
    el.setAttribute('aria-hidden', 'true');
    el.dataset.scrollPad = '';
    p = { el, px: 0 };
    pads.set(b, p);
    // it shrinks as soon as the reader scrolls up (what goes is below the screen, so nothing moves)
    (b === doc.scrollingElement ? doc : b).addEventListener('scroll', () => fitPad(b), { passive: true });
  }
  p.px += px;
  p.el.style.height = `${Math.ceil(p.px)}px`;
  const host = b === doc.scrollingElement ? doc.body : b;
  if (p.el.parentNode !== host || host.lastElementChild !== p.el) host.append(p.el);
}
function fitPad(b) {
  const p = pads.get(b);
  if (!p || !p.px) return;
  // what is below the screen: the pad can give that much back
  const spare = b.scrollHeight - b.scrollTop - b.clientHeight;
  if (spare < 1) return;
  p.px = Math.max(0, p.px - spare);
  p.el.style.height = `${Math.ceil(p.px)}px`;
  if (!p.px) p.el.remove();
}

/** Remove every child. */
export function clear(el) { holdScroll(el); el.replaceChildren(); return el; }
/** Replace every child with these. */
export function replace(el, ...children) { holdScroll(el); el.replaceChildren(); return append(el, children); }

/* ------------------------------------------------------------------ drawing a live part again */

const HOLDS_FOCUS = 'a[href], button, input, select, textarea, summary, [tabindex], [contenteditable]';
// The element on the page (o) has every attribute the new drawing (n) has, with the same value, and no other.
// Inline styles are compared by property: every one n sets is on o with that value; o may have more (a pack's
// layout code, such as the whole-pixel snap, adds sizes to drawn elements after they are on the page).
function sameAttrs(o, n) {
  for (const x of n.attributes) {
    if (x.name === 'style') {
      for (const prop of n.style) if (o.style.getPropertyValue(prop) !== n.style.getPropertyValue(prop)) return false;
    } else if (o.getAttributeNS(x.namespaceURI, x.localName) !== x.value) return false;
  }
  for (const x of o.attributes) if (x.name !== 'style' && !n.hasAttributeNS(x.namespaceURI, x.localName)) return false;
  return true;
}
/** o (on the page) looks as n (drawn new) would: the same tags, attributes (sameAttrs) and text, all the way down. */
function looksSame(o, n) {
  if (o.nodeType !== n.nodeType) return false;
  if (o.nodeType !== 1) return o.nodeValue === n.nodeValue;
  if (o.localName !== n.localName || o.namespaceURI !== n.namespaceURI || o.childNodes.length !== n.childNodes.length || !sameAttrs(o, n)) return false;
  for (let i = 0; i < o.childNodes.length; i++) if (!looksSame(o.childNodes[i], n.childNodes[i])) return false;
  return true;
}

/** The path of child indexes from root down to el, or null when el is not inside root. */
function pathTo(root, el) {
  const path = [];
  for (let e = el; e !== root; e = e.parentElement) {
    if (!e || !e.parentElement) return null;
    path.unshift([...e.parentElement.children].indexOf(e));
  }
  return path;
}

/**
 * Run paint(), which draws part of root again, and give the keyboard focus back when it threw away the control
 * that had it: the element at the same place in the new drawing gets the focus, if it is the same kind of
 * element (without scrolling). A focus that paint() moved somewhere itself is left alone. For a paint that is a
 * promise, the focus is given back when it settles. Returns what paint() returns.
 */
export function keepFocus(root, paint) {
  const doc = root.ownerDocument || globalThis.document;
  const was = doc.activeElement;
  const path = was && was !== root && root.contains(was) ? pathTo(root, was) : null;
  const back = () => {
    if (!path || was.isConnected || (doc.activeElement && doc.activeElement !== doc.body)) return;
    let e = root;
    for (const i of path) e = e?.children[i];
    if (e && e.localName === was.localName && typeof e.focus === 'function') e.focus({ preventScroll: true });
  };
  let out;
  try { out = paint(); } catch (err) { back(); throw err; }
  if (out && typeof out.then === 'function') return out.then((v) => { back(); return v; }, (err) => { back(); throw err; });
  back();
  return out;
}

function patch(parent, wanted) {
  const old = [...parent.childNodes];
  wanted.forEach((n, i) => {
    const o = old[i];
    if (!o) { parent.append(n); return; }
    if (looksSame(o, n)) return;                                   // drawn the same: the node on the page stays
    if (o.nodeType === 3 && n.nodeType === 3) { o.data = n.data; return; }
    // a box that only differs inside: keep it and look at its children. A control, a box with other
    // attributes and anything with a canvas (its picture is not in the DOM) is put in new.
    if (o.nodeType === 1 && n.nodeType === 1 && o.localName === n.localName && sameAttrs(o, n)
      && !o.matches(HOLDS_FOCUS) && !n.querySelector('canvas') && !o.querySelector('canvas')) {
      patch(o, [...n.childNodes]);
      return;
    }
    o.replaceWith(n);
  });
  for (const o of old.slice(wanted.length)) o.remove();
}

/**
 * replace() for a part of a page that is drawn again and again (live values every few seconds): the children
 * are built new as always, but a node that would look the same as the one on the page (tag, attributes, text
 * and descendants; an inline style the page added after drawing does not count) is left on the page, and only what differs is put in. So a button or link whose
 * text and attributes did not change is still the same element after the update, and keeps the keyboard focus,
 * the hover and a screen reader's place. A control that did change is replaced, and the focus moves to the
 * one at its place (keepFocus). The elements that stay keep their own event listeners: use it where the
 * listeners do not close over the values drawn (they read the newest state when they run).
 */
export function redraw(el, ...children) {
  const wanted = [...frag(children).childNodes];
  return keepFocus(el, () => { patch(el, wanted); return el; });
}

// the nodes append() would add, without moving any of them yet (null when a fragment is among them)
function flatNodes(children, out = []) {
  for (const c of children) {
    if (Array.isArray(c)) { if (!flatNodes(c, out)) return null; continue; }
    if (c == null || c === false || c === true || c === '') continue;
    if (typeof c === 'object' && 'nodeType' in c) { if (c.nodeType === 11) return null; out.push(c); continue; }
    out.push(globalThis.document.createTextNode(String(c)));
  }
  return out;
}
/**
 * Like replace(), for parts drawn again on every live update: a child that is the same as the one already in its
 * place (the node itself, or isEqualNode: the same tags, attributes and text) stays, and only the others are
 * swapped, so the page lays out, snaps and paints only what changed. Event handlers are not compared: use it for
 * children whose handlers do not depend on the values drawn.
 */
export function replaceChanged(el, ...children) {
  const next = flatNodes(children);
  const now = Array.from(el.childNodes);
  // a node that moves within el, or a different count: everything again
  holdScroll(el);
  if (!next || next.length !== now.length || next.some((n, i) => n !== now[i] && n.parentNode === el)) {
    el.replaceChildren();
    return append(el, children);
  }
  for (let i = 0; i < next.length; i++) {
    if (next[i] !== now[i] && !now[i].isEqualNode(next[i])) el.replaceChild(next[i], now[i]);
  }
  return el;
}

/**
 * While the page or a box in it scrolls, and for `ms` after, the root element carries data-scrolling. CSS pauses
 * endless animations with it (a pulsing dot, a halo), so a frame of a scroll only moves what is painted already
 * instead of drawing them again too. The listener is passive (it never holds a scroll up), and the attribute
 * changes twice per scroll, not on every scroll event. Call once at start; returns a remover.
 */
export function markScrolling(ms = 200) {
  const root = globalThis.document.documentElement;
  let t = 0;
  const end = () => { t = 0; delete root.dataset.scrolling; };
  const onScroll = () => {
    if (!t) root.dataset.scrolling = '';
    else clearTimeout(t);
    t = setTimeout(end, ms);
  };
  globalThis.document.addEventListener('scroll', onScroll, { capture: true, passive: true });
  return () => { globalThis.document.removeEventListener('scroll', onScroll, { capture: true }); if (t) { clearTimeout(t); end(); } };
}

/** The longest of some texts (null and undefined count as ''). */
export function longest(texts) {
  let w = '';
  for (const t of texts ?? []) { const s = t == null ? '' : String(t); if (s.length > w.length) w = s; }
  return w;
}

/**
 * The shapes of some texts: each with its digits as 0 (readouts use tabular figures, so every digit is as wide),
 * each shape once, at most max of them (the longest).
 */
export function textShapes(texts, max = 24) {
  const seen = new Set();
  for (const t of texts ?? []) if (t != null && t !== '') seen.add(String(t).replace(/[0-9]/g, '0'));
  const out = [...seen];
  if (out.length > max) out.sort((a, b) => b.length - a.length).length = max;
  return out;
}

/**
 * A readout whose text changes in place (the value under a chart's cursor, the day pointed at) in a box as wide as
 * the widest text it can show, so a new value never moves what is beside or under it, and never wraps a line anew:
 * steady(el, texts) puts el in a box (span.steady) followed by unseen copies of el (same tag and class, aria-hidden,
 * class steady-size), one for each shape of texts (textShapes). All sit in one grid cell, so the box is as wide as
 * the widest of them. Set el's text as before; box.fit(texts) sizes the box again for new data. (The copies come
 * after el, so code that finds el by its class still finds el first.) Each pack styles .steady and .steady-size
 * (app.css).
 */
export function steady(el, texts = []) {
  const box = h('span', { class: 'steady' }, el);
  box.fit = (t) => {
    const shapes = textShapes(t);
    box.replaceChildren(el, ...(shapes.length ? shapes : ['']).map((x) => {
      const size = el.cloneNode(false);
      for (const a of ['id', 'data-cover', 'title', 'role', 'aria-live']) size.removeAttribute(a);
      size.classList.add('steady-size');
      size.setAttribute('aria-hidden', 'true');
      size.textContent = x;
      return size;
    }));
    return box;
  };
  return box.fit(texts);
}

/** addEventListener that returns its own remover. */
export function on(target, type, fn, opts) {
  target.addEventListener(type, fn, opts);
  return () => target.removeEventListener(type, fn, opts);
}

let discloseSeq = 0;
/**
 * A button that opens a part of the page in place (a small form behind an "Edit" button). The part starts
 * hidden; the button carries aria-expanded and aria-controls. Clicking the button opens it (or closes it again);
 * opening moves the focus to `focus` (an element, or a function that returns one: the first control by
 * default); Escape inside the part closes it, and closing gives the focus back to the button when the focus was
 * inside the part. onOpen() and onClose() run after each change. Returns { open({ focus }), close({ focus }), isOpen() }:
 * open({ focus: false }) opens it again after a redraw without moving the focus.
 */
export function disclose(button, part, { focus = null, onOpen = null, onClose = null } = {}) {
  if (!part.id) part.id = `disclose-${++discloseSeq}`;
  button.setAttribute('aria-controls', part.id);
  button.setAttribute('aria-expanded', 'false');
  part.hidden = true;
  const target = () => (typeof focus === 'function' ? focus() : focus)
    || part.querySelector('input:not([type="hidden"]), select, textarea, button, [href], [tabindex]:not([tabindex="-1"])');
  const isOpen = () => !part.hidden;
  function open({ focus: into = true } = {}) {
    if (isOpen()) return;
    part.hidden = false;
    button.setAttribute('aria-expanded', 'true');
    if (into) target()?.focus();
    onOpen?.();
  }
  function close({ focus: back = null } = {}) {
    if (!isOpen()) return;
    const doc = part.ownerDocument || globalThis.document;
    const inside = part.contains(doc.activeElement);
    part.hidden = true;
    button.setAttribute('aria-expanded', 'false');
    if (back ?? inside) button.focus();
    onClose?.();
  }
  button.addEventListener('click', () => (isOpen() ? close({ focus: true }) : open()));
  part.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || e.defaultPrevented) return;
    e.preventDefault();
    e.stopPropagation(); // a dialog around it stays open
    close({ focus: true });
  });
  return { open, close, isOpen };
}

const sheets = new Map();
/**
 * Add <link rel="stylesheet" href> to <head> once (per resolved URL) and resolve when it has loaded, so a
 * map or chart is only drawn once its style sheet applies. A sheet that fails to load resolves too (false)
 * and is tried again next time: a missing style must not stop the page. Give it a URL relative to the
 * calling module (new URL('../x.css', import.meta.url)), so it follows the pack's versioned prefix.
 * { first: true } puts it before the page's other style sheets, as if index.html linked it ahead of app.css,
 * so the pack's own rules still win over the library's at the same specificity.
 */
export function styleSheet(href, { first = false } = {}) {
  const doc = globalThis.document;
  const url = new URL(String(href), doc.baseURI).href;
  let p = sheets.get(url);
  if (p) return p;
  p = new Promise((resolve) => {
    let link = [...doc.querySelectorAll('link[rel="stylesheet"]')].find((l) => l.href === url);
    if (link?.sheet) { resolve(true); return; }
    const fresh = !link;
    if (fresh) { link = doc.createElement('link'); link.rel = 'stylesheet'; }
    link.addEventListener('load', () => resolve(true), { once: true });
    link.addEventListener('error', () => { sheets.delete(url); if (fresh) link.remove(); resolve(false); }, { once: true });
    if (fresh) {
      link.href = url;
      const ahead = first ? doc.head.querySelector('link[rel="stylesheet"]') : null;
      if (ahead) ahead.before(link); else doc.head.append(link);
    }
  });
  sheets.set(url, p);
  return p;
}

/* ------------------------------------------------------------------ coverage and readiness */

/** Add inventory item ids to an element's data-cover (space separated, no repeats). Returns el. */
export function cover(el, ...ids) {
  if (!el) return el;
  const have = new Set(String(el.dataset.cover || '').split(/\s+/).filter(Boolean));
  for (const id of ids.flat(Infinity)) for (const one of String(id ?? '').split(/\s+/)) if (one) have.add(one);
  if (have.size) el.dataset.cover = [...have].join(' ');
  return el;
}

const rootEl = () => globalThis.document?.documentElement;
/** The page finished its first render with data: <html data-ready="<page id>">. */
export function ready(pageId) { const r = rootEl(); if (r) r.dataset.ready = String(pageId); }
/** A navigation started: remove data-ready. */
export function unready() { const r = rootEl(); if (r) delete r.dataset.ready; }
/** The page could not load its data: <html data-ready="error:<page id>">. */
export function failed(pageId) { const r = rootEl(); if (r) r.dataset.ready = 'error:' + String(pageId); }
/** The page id that is ready now, or null. */
export const readyPage = () => rootEl()?.dataset.ready ?? null;

/** The tab title: setTitle('Drives', 'Model Y') -> 'Drives · Model Y · Sentry Mate'. Empty parts are left out. */
export function setTitle(...partsList) {
  const t = [...partsList.flat(), 'Sentry Mate'].filter((x) => x != null && String(x).trim() !== '').join(' · ');
  if (globalThis.document) globalThis.document.title = t;
  return t;
}

/**
 * A value with its unit as elements, in the person's units:
 *   <span class="q"><span class="q-v">7.6</span><span class="q-u">mi</span></span>
 * A null value gives <span class="q is-missing"><span class="q-v">—</span> <span class="q-why">reason</span></span>,
 * the reason visible (from opts.why or the units.js hook). The number and its unit never part at a line
 * break. opts: as units.format, plus cls (extra class) and why.
 */
export function quantity(value, kind, opts = {}) {
  const p = unitParts(value, kind, opts);
  const cls = ['q', p.missing && 'is-missing', opts.cls].filter(Boolean).join(' ');
  if (p.missing) {
    return h('span', { class: cls }, h('span', { class: 'q-v' }, p.number), p.why ? [' ', h('span', { class: 'q-why' }, p.why)] : null);
  }
  return h('span', { class: cls }, h('span', { class: 'q-v' }, p.number), p.unit ? [p.sep, h('span', { class: 'q-u' }, p.unit)] : null);
}
