// Sentry Mate shared module: dom.js (small safe DOM helpers).
// Canonical source: ui/shared/dom.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// h('a', { class: 'x', href }, 'text', child) builds elements with DOM calls only, so car and place names
// from the API are always text, never markup, and pages work under the strict Content-Security-Policy:
// no innerHTML, no inline style attributes (sizes go through CSSOM as CSS custom properties), no string
// event handlers, no javascript: links. Also: cover() for coverage marking (data-cover) and ready() /
// unready() / failed() for the page readiness contract (data-ready on <html>).

import { parts as unitParts } from './units.js';

export const SVG_NS = 'http://www.w3.org/2000/svg';
const URL_ATTRS = new Set(['href', 'src', 'action', 'formaction', 'xlink:href', 'poster', 'cite', 'data']);
const PROPS = new Set(['value', 'checked', 'selected', 'indeterminate', 'muted']);

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
/** Remove every child. */
export function clear(el) { el.replaceChildren(); return el; }
/** Replace every child with these. */
export function replace(el, ...children) { el.replaceChildren(); return append(el, children); }

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
