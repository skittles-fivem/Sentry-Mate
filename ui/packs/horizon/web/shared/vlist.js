// Long lists in a box that scrolls, drawn a window at a time: only the rows in view and a few on either side
// are in the page, the rest is two spacers of the right height. Rows are loaded from the API in chunks as the
// window reaches them: by the cursor of the chunk before when there is one (`after`), else by `offset`. Every row
// has the same height (the pack's CSS fixes it; the first row drawn is measured), so a row's place is its index
// times that height and a jump anywhere costs one request.
//
//   const list = windowList({
//     box,                     the element that scrolls (its height comes from the pack's CSS, from the start)
//     body,                    where the rows go: a <tbody>, or any element in the box
//     load({ offset, after, at_ms, limit, signal }) -> { items, next, offset, total }   one chunk of the list
//     row(item, index),        the element of a loaded row
//     placeholder(index),      the element of a row still loading (a skeleton of the same height)
//     spacer(),                an empty element (a <tr> with one cell across, or a <div>) standing for rows not drawn
//     head,                    an element above the rows inside the box (a sticky <thead>): rows start under it
//     limit: 50, overscan: 6, rowHeight: 44 (until a row is measured)
//     onPage(page),            after each chunk (the first one holds the page's totals)
//     onRange({ first, last, total }), after each draw: the rows in view (0 based, last excluded)
//   })
//   await list.start()        the first chunk (or list.jump(atMs) for the chunk at a date), then drawn
//   list.refresh()            load the rows in view again, quietly (the old rows stay until the new ones are in)
//   list.jump(atMs)           the first row on the far side of that time (the API's at_ms), scrolled to the top
//   list.scrollTo(index), list.total(), list.item(index), list.range(), list.destroy()
//
// dayJumpMs(key, tz, newestFirst): the at_ms of "jump to this day" (a date input's YYYY-MM-DD in the car's zone).
//
// Spacers get their height as the CSS custom property --vl-h (the page policy blocks inline style text): the pack's
// CSS gives .vl-gap (or its cell) height: var(--vl-h).

import { addDays, dayStart } from './time.js';

const raf = typeof requestAnimationFrame === 'function' ? requestAnimationFrame : (f) => setTimeout(f, 16);

export function windowList(opts) {
  const { box, body, load, row, placeholder, spacer, head = null } = opts;
  const limit = opts.limit || 50;
  const overscan = opts.overscan ?? 6;
  let rowH = opts.rowHeight || 44;
  let measured = false;

  let items = new Map();     // index -> item
  let cursors = new Map();   // index -> the cursor that goes on from there
  let inflight = new Map();  // chunk start -> promise
  let failed = new Map();    // chunk start -> when it failed (not asked again for a few seconds)
  let total = null;
  let gen = 0;
  let ctrl = null;           // aborts the requests of an older generation
  let drawn = new Map();     // index -> [element, loaded?]
  let view = { first: 0, last: 0 };
  let queued = false;
  let dead = false;

  const topGap = spacer();
  const bottomGap = spacer();
  for (const g of [topGap, bottomGap]) {
    g.classList?.add('vl-gap');
    g.setAttribute?.('aria-hidden', 'true');
  }

  const setGap = (el, px) => el.style.setProperty('--vl-h', `${Math.max(0, Math.round(px))}px`);
  // the head's height, read again when the box changes size (not every frame)
  let headPx = null;
  const headH = () => (headPx ??= head ? head.offsetHeight || 0 : 0);

  function want() {
    if (total == null) return { first: 0, last: 0 };
    // row i sits at head + i * rowH; the sticky head covers the top of the box
    const top = box.scrollTop;
    const tall = (box.clientHeight || rowH * 10) - headH();
    const first = Math.max(0, Math.min(total, Math.floor(top / rowH) - overscan));
    const last = Math.max(first, Math.min(total, Math.ceil((top + tall) / rowH) + overscan));
    return { first, last };
  }

  // Only the rows that leave the window are taken out and only those that come into it are put in (a scroll of a
  // row or two moves two elements, not the whole window), so the browser lays out and styles little each frame.
  function draw() {
    queued = false;
    if (dead) return;
    if (topGap.parentNode !== body) clear();
    const { first, last } = want();
    const next = new Map();
    for (const [i, [el]] of drawn) if (i < first || i >= last) el.remove();
    let anchor = null; // the first row kept: new rows above the old window go before it
    const below = [];
    const above = typeof document !== 'undefined' && document.createDocumentFragment ? document.createDocumentFragment() : null;
    const aboveList = [];
    for (let i = first; i < last; i++) {
      const has = items.has(i);
      const old = drawn.get(i);
      if (old && old[1] === has) {
        next.set(i, old);
        if (!anchor) anchor = old[0];
        continue;
      }
      const el = has ? row(items.get(i), i) : placeholder(i);
      next.set(i, [el, has]);
      if (old) old[0].replaceWith(el);                 // a placeholder whose row came in
      else if (i < view.first && drawn.size) aboveList.push(el);
      else below.push(el);
      if (old && !anchor) anchor = el;
    }
    if (aboveList.length) {
      if (above) { for (const el of aboveList) above.append(el); body.insertBefore(above, anchor || bottomGap); }
      else for (const el of aboveList) body.insertBefore(el, anchor || bottomGap);
    }
    for (const el of below) body.insertBefore(el, bottomGap);
    setGap(topGap, first * rowH);
    setGap(bottomGap, (total == null ? 0 : total - last) * rowH);
    drawn = next;
    view = { first, last };
    if (!measured) measure();
    for (let i = first; i < last; i++) if (!items.has(i)) fetchAt(Math.floor(i / limit) * limit);
    opts.onRange?.({ first, last, total: total ?? 0 });
  }

  function clear() {
    drawn = new Map();
    view = { first: 0, last: 0 };
    body.replaceChildren(topGap, bottomGap);
  }

  // the height of a loaded row, once: the box's rows are all that tall
  function measure() {
    for (const [, [el, has]] of drawn) {
      if (!has) continue;
      const hgt = el.getBoundingClientRect ? el.getBoundingClientRect().height : 0;
      if (hgt > 0) {
        measured = true;
        if (Math.abs(hgt - rowH) > 0.5) { rowH = hgt; schedule(); }
      }
      return;
    }
  }

  function schedule() {
    if (queued || dead) return;
    queued = true;
    raf(draw);
  }

  function keep(pg) {
    const at = pg.offset || 0;
    (pg.items || []).forEach((it, k) => items.set(at + k, it));
    if (pg.next) cursors.set(at + (pg.items || []).length, pg.next);
    total = pg.total ?? Math.max(total ?? 0, at + (pg.items || []).length);
  }

  function ask(params, my) {
    return load({ ...params, limit, signal: ctrl?.signal }).then((pg) => {
      if (my !== gen || dead) return null;
      return pg;
    });
  }

  function fetchAt(start) {
    if (inflight.has(start) || inflight.size >= 3 || (total != null && start >= total)) return inflight.get(start);
    if (Date.now() - (failed.get(start) || 0) < 3000) return null;
    const my = gen;
    const params = cursors.has(start) ? { after: cursors.get(start) } : { offset: start };
    const p = ask(params, my).then((pg) => {
      if (!pg) return;
      keep(pg);
      opts.onPage?.(pg);
      schedule();
    }).catch((e) => {
      if (my !== gen || e?.name === 'AbortError') return;
      failed.set(start, Date.now());
      setTimeout(schedule, 3100);
      opts.onError?.(e);
    }).finally(() => {
      if (inflight.get(start) === p) inflight.delete(start);
      if (my === gen) schedule();
    });
    inflight.set(start, p);
    return p;
  }

  function fresh() {
    gen++;
    ctrl?.abort();
    ctrl = typeof AbortController === 'function' ? new AbortController() : null;
    inflight = new Map();
    failed = new Map();
  }

  function onScroll() { schedule(); }
  box.addEventListener('scroll', onScroll, { passive: true });
  let ro = null;
  if (typeof ResizeObserver === 'function') {
    ro = new ResizeObserver(() => { headPx = null; schedule(); });
    ro.observe(box);
  }

  return {
    /** The first chunk, then drawn from the top. Rejects when it fails (the page shows its error). */
    async start() {
      fresh();
      const my = gen;
      const pg = await ask({ offset: 0 }, my);
      if (!pg) return null;
      items = new Map(); cursors = new Map(); clear(); total = null;
      keep(pg);
      box.scrollTop = 0;
      draw();
      return pg;
    },
    /** The rows in view loaded again; the old ones stay on screen until the new ones are in. */
    async refresh() {
      if (total == null) return this.start();
      fresh();
      const my = gen;
      const { first, last } = want();
      const a = Math.floor(first / limit) * limit;
      const starts = [];
      for (let s = a; s < Math.max(last, a + 1); s += limit) starts.push(s);
      const pages = await Promise.all(starts.map((s) => ask({ offset: s }, my)));
      if (pages.some((p) => !p)) return null;
      items = new Map(); cursors = new Map(); clear(); total = null;
      for (const pg of pages) keep(pg);
      const end = Math.max(0, headH() + total * rowH - box.clientHeight);
      if (box.scrollTop > end) box.scrollTop = end;
      draw();
      pages.forEach((p) => opts.onPage?.(p));
      return pages[0];
    },
    /** The first row on the far side of atMs (the API's at_ms), at the top of the box. */
    async jump(atMs) {
      fresh();
      const my = gen;
      const pg = await ask({ at_ms: atMs }, my);
      if (!pg) return null;
      const kept = total != null;
      if (!kept) { items = new Map(); cursors = new Map(); clear(); }
      keep(pg);
      opts.onPage?.(pg);
      this.scrollTo(pg.offset || 0);
      draw();
      return pg;
    },
    /** Row `index` just under the head. */
    scrollTo(index) { box.scrollTop = Math.max(0, index) * rowH; schedule(); },
    total: () => total ?? 0,
    item: (i) => items.get(i),
    range: () => ({ ...view }),
    rowHeight: () => rowH,
    destroy() {
      dead = true;
      ctrl?.abort();
      box.removeEventListener('scroll', onScroll);
      ro?.disconnect();
    },
  };
}

/** The API's at_ms for "jump to this day" (a YYYY-MM-DD key in the car's zone): its first instant oldest first, the
 * first instant of the day after newest first (the list then starts at the day's last row). */
export function dayJumpMs(key, tz, newestFirst = false) {
  return dayStart(newestFirst ? addDays(key, 1) : key, tz);
}
