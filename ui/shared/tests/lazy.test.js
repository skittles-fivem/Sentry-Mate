// Tests for lazy.js: a block mounts when it comes near the screen, at once when it is near already, without
// IntersectionObserver, inside an off-screen staging box and before printing; cancel stops it; a page that
// scrolls inside a box watches that box.
import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';

import { define } from './fake-browser.js';
define('addEventListener', (type, f) => { if (type === 'beforeprint') printListeners.push(f); });
const { whenNear, isNear, nearNow, scrollRoot } = await import('../lazy.js');

// a tiny element: a box at top..top+height, in the page or not, inside parent
function el({ top = 0, height = 100, connected = true, parent = null, inert = false, hidden = false, shift = null } = {}) {
  const e = {
    top, height, isConnected: connected, parentElement: parent, inert, hidden, scrollHeight: height, clientHeight: height, overflowY: 'visible',
    dataset: shift == null ? {} : { nearShift: String(shift) },
    getBoundingClientRect() { return hidden ? { top: 0, bottom: 0, left: 0, right: 0, width: 0, height: 0 } : { top: e.top, bottom: e.top + e.height, left: 0, right: 400, width: 400, height: e.height }; },
    getClientRects() { return hidden ? [] : [e.getBoundingClientRect()]; },
    closest(sel) {
      const has = { '[inert]': (p) => p.inert, '[data-near-shift]': (p) => p.dataset.nearShift != null }[sel];
      assert.ok(has, sel);
      for (let p = e; p; p = p.parentElement) if (has(p)) return p;
      return null;
    },
  };
  return e;
}

let observers, frames;
const printListeners = []; // the module hooks printing once per process
class FakeIO {
  constructor(cb, opts) { this.cb = cb; this.opts = opts; this.targets = []; this.off = false; observers.push(this); }
  observe(t) { this.targets.push(t); }
  disconnect() { this.off = true; }
  fire(isIntersecting) { if (!this.off) this.cb(this.targets.map((target) => ({ target, isIntersecting }))); }
}

beforeEach(() => {
  observers = []; frames = [];
  define('innerHeight', 800);
  define('innerWidth', 400);
  define('IntersectionObserver', FakeIO);
  define('requestAnimationFrame', (f) => { frames.push(f); return frames.length; });
  define('cancelAnimationFrame', (n) => { frames[n - 1] = null; });
  define('getComputedStyle', (e) => ({ overflowY: e.overflowY }));
});
afterEach(() => { define('IntersectionObserver', FakeIO); });

const tick = () => new Promise((r) => setTimeout(r, 0));
const runFrames = () => { const f = frames.splice(0); for (const x of f) x?.(); };

const html = () => globalThis.document.documentElement;

test('while the page is first drawn a block on screen mounts at once, one within a screen below just after', async () => {
  delete html().dataset.ready;
  let n = 0;
  whenNear(el({ top: 100 }), () => n++);
  whenNear(el({ top: 1500 }), () => n++); // 800 + 800 margin
  assert.equal(n, 1);
  assert.equal(observers.length, 1);
  observers[0].fire(true); // the first look after layout
  await tick();
  assert.equal(n, 2);
});

test('once the page is ready a block on screen or within one screen below mounts at once', () => {
  html().dataset.ready = 'drives';
  try {
    let n = 0;
    whenNear(el({ top: 100 }), () => n++);
    whenNear(el({ top: 1500 }), () => n++);
    assert.equal(n, 2);
    assert.equal(observers.length, 0);
  } finally { delete html().dataset.ready; }
});

test('a block further down mounts when it comes near, once, with a margin of a screen, in a task of its own', async () => {
  let n = 0;
  const e = el({ top: 3000 });
  whenNear(e, () => n++);
  assert.equal(n, 0);
  assert.equal(observers.length, 1);
  assert.equal(observers[0].opts.rootMargin, '100%');
  assert.equal(observers[0].opts.root, null);
  observers[0].fire(false);
  assert.equal(n, 0);
  observers[0].fire(true);
  observers[0].fire(true);
  assert.equal(n, 0, 'drawn in a later task');
  await tick();
  assert.equal(n, 1);
  assert.ok(observers[0].off, 'the observer is disconnected');
});

test('the margin can be set', () => {
  let n = 0;
  html().dataset.ready = 'drives';
  try { whenNear(el({ top: 1500 }), () => n++, { margin: 0.5 }); } finally { delete html().dataset.ready; }
  assert.equal(n, 0);
  assert.equal(observers[0].opts.rootMargin, '50%');
});

test('cancel and an aborted signal stop it, also while it waits for its task', async () => {
  let n = 0;
  const cancel = whenNear(el({ top: 3000 }), () => n++);
  cancel();
  assert.ok(observers[0].off);
  observers[0].fire(true);
  const ac = new AbortController();
  whenNear(el({ top: 3000 }), () => n++, { signal: ac.signal });
  ac.abort();
  observers[1].fire(true);
  const gone = new AbortController();
  gone.abort();
  whenNear(el({ top: 0 }), () => n++, { signal: gone.signal });
  const queued = whenNear(el({ top: 3000 }), () => n++);
  observers[2].fire(true);
  queued();
  await tick();
  assert.equal(n, 0);
});

test('without IntersectionObserver it mounts at once', () => {
  define('IntersectionObserver', undefined);
  let n = 0;
  whenNear(el({ top: 9000 }), () => n++);
  assert.equal(n, 1);
});

test('a hidden block waits for the observer', () => {
  let n = 0;
  whenNear(el({ hidden: true }), () => n++);
  assert.equal(n, 0);
  assert.equal(observers.length, 1);
  assert.equal(isNear(el({ hidden: true })), false);
});

test('a block in an off-screen staging box (a live refresh) mounts at once, wherever it is', () => {
  let n = 0;
  const stage = el({ top: 0, height: 800, inert: true });
  whenNear(el({ top: 5000, parent: stage }), () => n++);
  assert.equal(n, 1);
});

test('a block that is not in the page yet is looked at once it is', async () => {
  let n = 0;
  const far = el({ top: 100, connected: false });
  whenNear(far, () => n++);
  assert.equal(n, 0);
  far.isConnected = true; // the page put it in after building it, in the same task
  await tick();
  assert.equal(n, 1);

  const later = el({ top: 4000, connected: false });
  whenNear(later, () => n++);
  await tick();
  assert.equal(observers.length, 0, 'still waiting for the page');
  later.isConnected = true;
  runFrames();
  assert.equal(observers.length, 1);
  observers[0].fire(true);
  await tick();
  assert.equal(n, 2);
});

test('a page that scrolls inside a box watches that box', () => {
  const scroller = el({ top: 0, height: 600 });
  Object.assign(scroller, { overflowY: 'auto', scrollHeight: 5000, clientHeight: 600 });
  const inner = el({ top: 0, height: 4000, parent: scroller });
  const block = el({ top: 2000, parent: inner });
  assert.equal(scrollRoot(block), scroller);
  assert.equal(scrollRoot(el({ parent: el() })), null);
  let n = 0;
  whenNear(block, () => n++);
  assert.equal(n, 0, '2000 is more than 600 + 600 below the box');
  assert.equal(observers[0].opts.root, scroller);
  assert.ok(isNear(el({ top: 1100, parent: inner }), scroller));
});

test('every block still waiting mounts before the page is printed', () => {
  let n = 0;
  whenNear(el({ top: 3000 }), () => n++);
  whenNear(el({ top: 4000 }), () => n++);
  const cancel = whenNear(el({ top: 5000 }), () => n++);
  cancel();
  for (const f of printListeners) f();
  assert.equal(n, 2);
  for (const f of printListeners) f();
  assert.equal(n, 2, 'once');
});

test('a part built off screen mounts what lands near the screen, the rest once it is in place and near', async () => {
  let n = 0;
  // the box sits at the top of the window, inert; its contents land 600 px lower
  const box = el({ top: 0, height: 800, inert: true, shift: 600 });
  whenNear(el({ top: 100, parent: box }), () => n++); // lands at 700: on screen
  const far = el({ top: 500, parent: box }); // lands at 1100: below the screen, drawn once in place
  whenNear(far, () => n++);
  assert.equal(n, 1);
  assert.equal(observers.length, 0);
  runFrames();
  assert.equal(observers.length, 0, 'still in the box: looked at again next frame');
  far.parentElement = el({ top: 0, height: 4000 }); // swapped into the page
  far.top = 1100;
  runFrames();
  assert.equal(n, 1);
  assert.equal(observers.length, 1);
  observers[0].fire(true);
  await tick();
  assert.equal(n, 2);
});

test('nearNow says whether a block is near the screen now', () => {
  assert.equal(nearNow(el({ top: 100 })), true);
  assert.equal(nearNow(el({ top: 3000 })), false);
  assert.equal(nearNow(el({ top: 100, connected: false })), false);
  assert.equal(nearNow(el({ top: 1000 }), { margin: 0 }), false);
});

test('blocks that come near together are drawn one per task', async () => {
  const order = [];
  for (const i of [1, 2, 3]) whenNear(el({ top: 3000 * i }), () => order.push(i));
  for (const o of observers) o.fire(true);
  assert.deepEqual(order, []);
  await tick();
  assert.deepEqual(order, [1]);
  await tick();
  await tick();
  assert.deepEqual(order, [1, 2, 3]);
});
