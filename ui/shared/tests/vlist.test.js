// vlist.js: a list drawn a window at a time, loaded in chunks (by cursor when it goes on, else by offset).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { windowList, dayJumpMs } from '../vlist.js';

// stand-ins for the few element features the module uses
function el(kind, extra = {}) {
  const props = new Map();
  const node = {
    kind, ...extra, children: [], parentNode: null, classList: { add() {} }, attrs: {},
    style: { setProperty: (k, v) => props.set(k, v), get: (k) => props.get(k) },
    setAttribute(k, v) { this.attrs[k] = v; },
    replaceChildren(...n) { for (const c of this.children) c.parentNode = null; this.children = n; for (const c of n) c.parentNode = this; },
    insertBefore(n, ref) { n.parentNode?.children.splice(n.parentNode.children.indexOf(n), 1); const i = ref ? this.children.indexOf(ref) : -1; if (i < 0) this.children.push(n); else this.children.splice(i, 0, n); n.parentNode = this; },
    remove() { if (this.parentNode) { this.parentNode.children.splice(this.parentNode.children.indexOf(this), 1); this.parentNode = null; } },
    replaceWith(n) { const p = this.parentNode; if (!p) return; p.children[p.children.indexOf(this)] = n; n.parentNode = p; this.parentNode = null; },
    getBoundingClientRect: () => ({ height: extra.height ?? 40 }),
  };
  return node;
}
function box(clientHeight = 400) {
  const ls = {};
  return { scrollTop: 0, clientHeight, addEventListener: (t, f) => { ls[t] = f; }, removeEventListener() {}, fire: (t) => ls[t]?.() };
}
const tick = () => new Promise((r) => setTimeout(r, 30));

// a made-up list of n rows; records what was asked
function api(n, log) {
  return async ({ offset, after, at_ms: at, limit }) => {
    log.push(after ? { after } : at != null ? { at } : { offset });
    let from = offset ?? 0;
    if (after) from = Number(after.slice(1));
    if (at != null) from = Math.min(n - 1, at);
    const items = Array.from({ length: Math.max(0, Math.min(limit, n - from)) }, (_, k) => ({ seq: from + k + 1 }));
    return { items, offset: from, total: n, next: from + items.length < n ? `c${from + items.length}` : null };
  };
}

function make(n, log, extra = {}) {
  const b = box();
  const body = el('tbody');
  const list = windowList({
    box: b, body, limit: 20, overscan: 2, rowHeight: 40, load: api(n, log),
    row: (it, i) => el('row', { seq: it.seq, i }), placeholder: (i) => el('skel', { i }), spacer: () => el('gap'),
    ...extra,
  });
  return { b, body, list };
}
const rows = (body) => body.children.filter((x) => x.kind === 'row').map((x) => x.seq);

test('only the rows in view and a few more are drawn, with spacers for the rest', async () => {
  const log = [];
  const { b, body, list } = make(2035, log);
  const pg = await list.start();
  assert.equal(pg.total, 2035);
  assert.deepEqual(log, [{ offset: 0 }]);
  // 400 px / 40 px = 10 rows, and 2 below
  assert.deepEqual(rows(body), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
  assert.equal(body.children[0].style.get('--vl-h'), '0px');
  assert.equal(body.children.at(-1).style.get('--vl-h'), `${(2035 - 12) * 40}px`);
  // scrolling to the end of the first chunk asks for the next one with its cursor
  b.scrollTop = 15 * 40; b.fire('scroll');
  await tick();
  assert.deepEqual(log[1], { after: 'c20' });
  await tick();
  assert.equal(rows(body)[0], 14);
  assert.ok(body.children.length < 20);
  // a jump far down asks by offset, and draws placeholders until it is in
  b.scrollTop = 1500 * 40; b.fire('scroll');
  await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual(log[2], { offset: 1480 });
  await tick();
  assert.equal(rows(body)[0], 1499);
  const got = rows(body);
  assert.deepEqual(got, [...got].sort((a, b) => a - b));
  // a small scroll back up puts rows above, in order, and keeps the gaps first and last
  b.scrollTop = 1495 * 40; b.fire('scroll');
  await tick();
  const up = rows(body);
  assert.equal(up[0], 1494);
  assert.deepEqual(up, [...up].sort((a, b) => a - b));
  assert.equal(body.children[0].kind, 'gap');
  assert.equal(body.children.at(-1).kind, 'gap');
  assert.equal(body.children.filter((x) => x.kind === 'skel').length, 0);
  list.destroy();
});

test('a date jump scrolls to the row the API names; refresh keeps the rows in view', async () => {
  const log = [];
  const { b, body, list } = make(500, log);
  await list.start();
  const pg = await list.jump(300);
  assert.equal(pg.offset, 300);
  assert.equal(b.scrollTop, 300 * 40);
  await tick();
  assert.equal(rows(body)[2], 301);
  log.length = 0;
  // the rows on screen are swapped one by one, never all taken out first (a box empty for a moment is scrolled up)
  const swap = body.replaceChildren;
  let emptied = 0;
  body.replaceChildren = function (...n) { emptied++; return swap.apply(this, n); };
  await list.refresh();
  body.replaceChildren = swap;
  assert.equal(emptied, 0);
  assert.deepEqual(log, [{ offset: 280 }, { offset: 300 }]);
  assert.equal(rows(body)[2], 301);
  assert.deepEqual(list.range(), { first: 298, last: 312 });
  list.destroy();
});

test('an empty list draws nothing; a short one only its rows', async () => {
  const { body, list } = make(0, []);
  await list.start();
  assert.deepEqual(rows(body), []);
  const s = make(3, []);
  await s.list.start();
  assert.deepEqual(rows(s.body), [1, 2, 3]);
  assert.equal(s.body.children.at(-1).style.get('--vl-h'), '0px');
});

test('a measured row height replaces the guess', async () => {
  const { body, list } = make(100, [], { row: (it, i) => el('row', { seq: it.seq, i, height: 80 }) });
  await list.start();
  await tick();
  assert.equal(list.rowHeight(), 80);
  assert.deepEqual(rows(body), [1, 2, 3, 4, 5, 6, 7]);
});

test('rows the guessed height drew too many of are taken out in the same step, never shown and taken away a frame later', async () => {
  const seen = [];
  const { body, list } = make(100, [], { row: (it, i) => el('row', { seq: it.seq, i, height: 80 }), onRange: (r) => seen.push([r.first, r.last]) });
  await list.start();
  // no frame has passed since the rows came in: the window is already the one of the measured height
  assert.deepEqual(rows(body), [1, 2, 3, 4, 5, 6, 7]);
  assert.equal(list.rowHeight(), 80);
  assert.deepEqual(seen.at(-1), [0, 7], 'and the range told last is that window');
  await tick();
  assert.deepEqual(rows(body), [1, 2, 3, 4, 5, 6, 7]);
});

test('a day to jump to, in the car\'s zone', () => {
  assert.equal(dayJumpMs('2026-03-08', 'America/Los_Angeles'), Date.UTC(2026, 2, 8, 8));
  // newest first: the day after begins it (a change to summer time that night: 23 hours later)
  assert.equal(dayJumpMs('2026-03-08', 'America/Los_Angeles', true), Date.UTC(2026, 2, 9, 7));
});
