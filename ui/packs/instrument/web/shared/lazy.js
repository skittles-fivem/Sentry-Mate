// Sentry Mate shared module: lazy.js (draw a block when it comes near the screen).
// Canonical source: ui/shared/lazy.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// whenNear(el, mount, { margin, root, signal }) calls mount() once, when el is within `margin` screen heights of
// what is on screen (default 1), and returns a function that cancels it. A chart, a map or a long list below the
// fold is then built only when the person scrolls toward it, and the first paint does less. The block keeps its
// final height from the start (CSS), so nothing moves when it is drawn.
//
// - It mounts at once when el is on screen already (measured now; once the page is ready, also when it is within
//   the margin, so a part drawn again keeps its charts), when the browser has no IntersectionObserver, and before
//   the page is printed. While a page is first drawn, what is within the margin but off screen is drawn just after
//   the first paint.
// - In an off-screen staging box (an [inert] box a live refresh builds the next page or part in, cut at one screen)
//   it mounts at once: what was drawn before is there again when the box is swapped in, so nothing goes away and
//   comes back. A box that builds a part for the first time can say where it will land instead, with
//   data-near-shift="<px>" (how far below the box): what lands near the screen mounts at once, the rest waits until
//   it is in place and comes near.
// - The screen is the nearest box that scrolls up and down (a pack whose page scrolls inside a fixed box), else
//   the window; `root` names it instead.
// - An element that is not in the page yet is looked at once it is (after this task, or at the next frames).
// - A hidden element (display: none, a closed tab) waits until it is shown and near.
// - What comes near later is drawn one block per task, never all in one long task.

const pending = new Set();
let printHooked = false;

// Blocks that came near are drawn one per task, so several charts coming near at once never make one long task
// (the page can answer a tap between two of them).
const queue = [];
let running = false;
function drawSoon(entry) {
  queue.push(entry);
  if (running) return;
  running = true;
  setTimeout(drawNext, 0);
}
function drawNext() {
  const entry = queue.shift();
  entry?.now();
  if (queue.length) setTimeout(drawNext, 0);
  else running = false;
}

function hookPrint() {
  if (printHooked || typeof globalThis.addEventListener !== 'function') return;
  printHooked = true;
  // a printed page has every block
  globalThis.addEventListener('beforeprint', () => { for (const p of [...pending]) p.now(); });
}

const SCROLLS = new Set(['auto', 'scroll', 'overlay']);
// the page said it is ready (shared/dom.js ready()): it is not being drawn for the first time
const readyNow = () => !!globalThis.document?.documentElement?.dataset?.ready;

// What the ancestors walked in this task scroll in (element -> the box its children scroll in): a page that asks
// for many blocks at once walks each ancestor and reads its style and size once, not once per block. Forgotten
// after the task (the layout may change after it).
let rootMemo = null;
function memo() {
  if (!rootMemo) {
    rootMemo = new Map();
    queueMicrotask(() => { rootMemo = null; });
  }
  return rootMemo;
}

/** The nearest ancestor of el that scrolls up and down and has more to show, else null (the window). */
export function scrollRoot(el) {
  const view = globalThis.getComputedStyle;
  if (typeof view !== 'function') return null;
  const m = memo();
  const walked = [];
  let root = null;
  for (let p = el.parentElement; p; p = p.parentElement) {
    if (m.has(p)) { root = m.get(p); break; }
    if (p === globalThis.document?.body || p === globalThis.document?.documentElement) break;
    walked.push(p);
    if (p.scrollHeight > p.clientHeight + 1 && SCROLLS.has(view(p).overflowY)) { root = p; break; }
  }
  for (const p of walked) m.set(p, root);
  return root;
}

/** Whether el is within margin screen heights of root's visible box (the window when root is null). */
export function isNear(el, root = null, margin = 1) {
  const r = el.getBoundingClientRect();
  if (!r.width && !r.height && !(el.getClientRects?.().length)) return false; // display: none
  const box = root ? root.getBoundingClientRect() : { top: 0, left: 0, bottom: globalThis.innerHeight, right: globalThis.innerWidth };
  const mh = (box.bottom - box.top) * margin, mw = (box.right - box.left) * margin;
  return r.top <= box.bottom + mh && r.bottom >= box.top - mh && r.left <= box.right + mw && r.right >= box.left - mw;
}

/** Whether el is near the screen now (in the page, shown, within margin screens of the box that scrolls it). */
export function nearNow(el, { margin = 1, root = undefined } = {}) {
  if (!el.isConnected) return false;
  return isNear(el, root === undefined ? scrollRoot(el) : root, margin);
}

/**
 * Call mount() once el is near the screen (see the top of this file). Returns cancel(): after it, mount is never
 * called. Pages cancel on leaving: ctx.onCleanup(whenNear(el, draw)), or pass { signal }.
 */
export function whenNear(el, mount, { margin = 1, root = undefined, signal = null } = {}) {
  let done = false, io = null, frame = 0, tries = 0, staged = 0;
  const entry = {
    now() { if (done) return; stop(); mount(); },
  };
  function stop() {
    done = true;
    pending.delete(entry);
    io?.disconnect(); io = null;
    if (frame) globalThis.cancelAnimationFrame?.(frame);
    frame = 0;
    signal?.removeEventListener?.('abort', stop);
  }
  if (signal?.aborted) return () => {};
  if (typeof globalThis.IntersectionObserver !== 'function') { mount(); return () => {}; }
  signal?.addEventListener?.('abort', stop);
  pending.add(entry);
  hookPrint();

  const start = () => {
    frame = 0;
    if (done) return;
    if (!el.isConnected) {
      // not in the page yet: look again at the next frames; after a second watch it with the window as the screen
      if (++tries < 60 && typeof globalThis.requestAnimationFrame === 'function') { frame = globalThis.requestAnimationFrame(start); return; }
    } else if (staged) {
      // waiting in a staging box: look again next frame, until it is swapped into the page (at most a minute)
      if (++staged < 3600 && el.closest?.('[inert]') && typeof globalThis.requestAnimationFrame === 'function') { frame = globalThis.requestAnimationFrame(start); return; }
    } else {
      const box = el.closest?.('[data-near-shift]');
      if (!box && el.closest?.('[inert]')) { entry.now(); return; }
      if (box) {
        // built off screen: near where it will land mounts now, else look again once it is in place
        const dy = Number(box.dataset.nearShift) || 0;
        const r = el.getBoundingClientRect(), hgt = globalThis.innerHeight, mh = hgt * (readyNow() ? margin : 0);
        if (r.top + dy <= hgt + mh && r.bottom + dy >= -mh) { entry.now(); return; }
        staged = 1;
        if (typeof globalThis.requestAnimationFrame === 'function') { frame = globalThis.requestAnimationFrame(start); return; }
      }
    }
    const r = root === undefined ? (el.isConnected ? scrollRoot(el) : null) : root;
    // while the page is first drawn only what is on screen is drawn at once, the rest just after the first paint;
    // once the page is ready (a part drawn again) everything near is drawn at once, so nothing goes away a moment
    if (el.isConnected && isNear(el, r, readyNow() ? margin : 0)) { entry.now(); return; }
    const pct = `${Math.round(margin * 100)}%`;
    io = new globalThis.IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) { io?.disconnect(); io = null; drawSoon(entry); }
    }, { root: r, rootMargin: pct, scrollMargin: pct, threshold: 0 }); // scrollMargin: boxes that scroll inside it too
    io.observe(el);
  };
  if (el.isConnected) start();
  else queueMicrotask(start);
  return stop;
}
