// Sentry Mate shared module: scrollbox.js (a long list in a box of about five rows that scrolls by itself).
// Canonical source: ui/shared/scrollbox.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// scrollBox(opts, ...children) -> { el, end, near(), recheck(), top(), edge(), destroy() }
//
// The box is a focusable region (Tab reaches it, the arrow keys, Page Up/Down, Home and End scroll it) whose
// height the pack's CSS gives it from the start (class `scroll-box`, plus opts.cls), so the page never grows
// while rows load. The children go in it, then an end marker (`scroll-box-end`, 1 px, aria-hidden):
//
//   - while rows are hidden under the bottom edge the box has `data-below` (for the pack's fade at the bottom),
//     looked at again on scroll and when the box or a child changes size (ResizeObserver);
//   - with opts.onEnd, an IntersectionObserver with the box as its root calls onEnd() as the end marker comes
//     within opts.ahead px (default 96) of the bottom edge. onEnd loads the next page and resolves truthy when
//     it added rows: the marker is then looked at again, so a box that is not full yet asks for the next page
//     by itself. It is never called again while one call is under way.
//
// A table in the box keeps its column header visible with `position: sticky; top: 0` on its th (the pack's
// CSS). A CSV download is the page's own and still exports every row, not only those loaded in the box.
//
// opts: { label (the region's accessible name), cls, onEnd, ahead, signal (AbortSignal), onCleanup (fn) }.
// Cleanup: when opts.signal aborts, through opts.onCleanup(fn), or destroy().
// `el` may be an element the page already made (opts.el): it gets the same attributes and the end marker.

import { h } from './dom.js';

export function scrollBox(opts = {}, ...children) {
  const end = h('div', { class: 'scroll-box-end', 'aria-hidden': 'true' });
  const el = opts.el || h('div', null);
  el.classList.add('scroll-box');
  for (const c of [].concat(opts.cls || []).filter(Boolean)) el.classList.add(...String(c).split(/\s+/).filter(Boolean));
  el.setAttribute('tabindex', '0');
  el.setAttribute('role', 'region');
  if (opts.label) el.setAttribute('aria-label', opts.label);
  el.append(...children.flat(Infinity).filter((c) => c != null && c !== false), end);
  const ahead = Number.isFinite(opts.ahead) ? opts.ahead : 96;
  const G = globalThis;
  const offs = [];
  let gone = false;

  /** Whether rows are hidden under the bottom edge: data-below for the fade. */
  function edge() {
    if (gone) return false;
    const below = el.scrollHeight - el.scrollTop - el.clientHeight > 1;
    el.toggleAttribute('data-below', below);
    return below;
  }
  /** Whether the end marker is within `ahead` px of the box's bottom edge (a box that is not full: yes). */
  function near() {
    if (gone || !el.isConnected) return false;
    const b = el.getBoundingClientRect();
    return b.height > 0 && end.getBoundingClientRect().top < b.bottom + ahead;
  }
  /** Back to the top (new filters or a new order: the list starts at its top). */
  function top() { el.scrollTop = 0; edge(); }

  // the next page as the end comes near
  let io = null, busy = false;
  function fire() {
    if (gone || busy || !opts.onEnd) return;
    busy = true;
    let p;
    try { p = Promise.resolve(opts.onEnd()); } catch (e) { p = Promise.reject(e); }
    p.then((more) => { busy = false; edge(); if (more) recheck(); }, () => { busy = false; });
  }
  /** Look at the end marker again (after rows were added or the list was drawn anew). */
  function recheck() {
    if (gone) return;
    edge();
    if (io) { io.unobserve(end); io.observe(end); }
  }
  if (opts.onEnd && typeof G.IntersectionObserver === 'function') {
    io = new G.IntersectionObserver((entries) => {
      if (gone) return;
      if (entries.some((e) => e.isIntersecting)) fire();
    }, { root: el, rootMargin: `0px 0px ${ahead}px 0px` });
    io.observe(end);
    offs.push(() => io.disconnect());
  }

  el.addEventListener('scroll', edge, { passive: true });
  offs.push(() => el.removeEventListener('scroll', edge));
  if (typeof G.ResizeObserver === 'function') {
    const ro = new G.ResizeObserver(() => edge());
    ro.observe(el);
    for (const c of el.children) if (c !== end) ro.observe(c);
    // rows put in later (a list drawn anew) are watched through their parent, which is a child of the box
    offs.push(() => ro.disconnect());
  }

  function destroy() {
    if (gone) return;
    gone = true;
    for (const f of offs.splice(0)) { try { f(); } catch { /* already gone */ } }
  }
  if (opts.signal) {
    if (opts.signal.aborted) destroy();
    else opts.signal.addEventListener('abort', destroy, { once: true });
  }
  if (typeof opts.onCleanup === 'function') opts.onCleanup(destroy);

  return { el, end, near, recheck, top, edge, destroy };
}
