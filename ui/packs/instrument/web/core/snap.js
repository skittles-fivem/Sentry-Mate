// The page on whole pixels.
//
// A grid shares its free width out among its fr columns, and a table among its columns, in fractions of a
// pixel, so every column edge after the first can land between two pixels, and with it every border, button
// and line in the columns; a box as wide as its words ends between two pixels too, and moves what follows it.
// snapAll() rounds grid and table columns (the same total width), grows boxes that end between two device
// pixels to the next one, gives table rows whole heights with an even height inside, moves a table cell whose
// content is a pixel off the row's common centre, and last moves a box with a border or a 1 px line that still
// starts between two device pixels onto whole ones (core/README.md, "Whole pixels").
//
// It runs again when the page adds or changes content, when a snapped grid or table changes size and when the
// fonts have loaded. Each run first puts back what the last one set, so the CSS (its tiers and container
// queries) always decides the layout and the snap only rounds it. It reads every size before it writes one.

// what a run changed: element -> { property: the inline value it had before }, so the next run can put it back
const snapped = new Map();
function put(e, prop, value) {
  let was = snapped.get(e);
  if (!was) { was = {}; snapped.set(e, was); }
  if (!(prop in was)) was[prop] = [e.style.getPropertyValue(prop), e.style.getPropertyPriority(prop)];
  e.style.setProperty(prop, value);
}
function restore() {
  for (const [e, was] of snapped) {
    for (const [prop, [v, prio]] of Object.entries(was)) {
      if (v) e.style.setProperty(prop, v, prio); else e.style.removeProperty(prop);
    }
  }
  snapped.clear();
}

const off = (v) => Math.abs(v - Math.round(v)) > 0.01;

function depth(e) { let d = 0; for (let x = e; x; x = x.parentElement) d++; return d; }

// grid: whole px columns with the same total width
function gridPlan(g) {
  const v = getComputedStyle(g).gridTemplateColumns;
  if (!v || v === 'none' || /[^\d.\spx-]/.test(v)) return null; // subgrid, names or anything not plain px
  const tracks = v.trim().split(/\s+/).map(parseFloat);
  if (tracks.length < 2 || tracks.some((w) => !Number.isFinite(w)) || !tracks.some(off)) return null;
  // each column rounded up, so no column gets narrower than what is in it by more than a pixel, and the widest
  // ones give one pixel each back until the total is the same (never all of it from one column)
  const total = Math.round(tracks.reduce((a, b) => a + b, 0));
  const out = tracks.map((w) => Math.ceil(w - 0.01));
  let over = out.reduce((a, b) => a + b, 0) - total;
  const order = tracks.map((w, i) => i).sort((a, b) => tracks[b] - tracks[a] || a - b);
  for (let k = 0; over > 0 && k < order.length * 2; k++) { out[order[k % order.length]] -= 1; over -= 1; }
  if (over < 0) out[order[0]] -= over;
  return () => put(g, 'grid-template-columns', out.map((w) => `${w}px`).join(' '));
}

// table: each column gets its width rounded to a whole px and the widest takes what is left over (a table
// wider than its box keeps all its widths, rounded up); the table gets the sum as its width and a fixed
// layout, so it has nothing left to share out
function tablePlan(t) {
  const row = (t.tHead && t.tHead.rows[0]) || t.rows[0];
  if (!row || row.cells.length < 2) return null;
  const cells = Array.from(row.cells);
  if (cells.some((c) => c.colSpan > 1)) return null;
  const ws = cells.map((c) => c.getBoundingClientRect().width);
  const tw = t.getBoundingClientRect().width;
  if (!tw || ws.some((w) => !w)) return null;
  if (!ws.some(off) && !off(tw)) return null;
  const scrolls = t.parentElement && tw > t.parentElement.clientWidth + 0.5;
  let grow = 0;
  ws.forEach((w, i) => { if (w > ws[grow]) grow = i; });
  // to the nearest px (a column with no room to spare loses half a pixel at most); in a table wider than its box,
  // up, so no column gets narrower than what is in it
  const px = ws.map((w) => (scrolls ? Math.ceil(w - 0.01) : Math.round(w)));
  const total = scrolls ? px.reduce((a, b) => a + b, 0) : Math.round(tw);
  if (!scrolls) px[grow] = total - px.reduce((a, b, i) => (i === grow ? a : a + b), 0);
  const sets = cells.map((c, i) => {
    const cs = getComputedStyle(c);
    // a content-box cell's width leaves out its padding and borders
    const side = cs.boxSizing === 'content-box'
      ? ['paddingLeft', 'paddingRight', 'borderLeftWidth', 'borderRightWidth'].reduce((a, k) => a + (parseFloat(cs[k]) || 0), 0) : 0;
    return [c, `${px[i] - side}px`];
  });
  return () => {
    for (const [c, w] of sets) put(c, 'width', w);
    put(t, 'width', `${total}px`);
    put(t, 'table-layout', 'fixed'); // the first row decides, whatever widths the other rows ask for
  };
}

// what a cell shows: its words, and the boxes in it (a pill, a bar, an icon)
const REPLACED = /^(img|svg|canvas|video|input|select|textarea|meter|progress)$/;
function contentBox(e, u) {
  const range = document.createRange();
  for (const c of e.childNodes) {
    if (c.nodeType === 3) {
      if (!/\S/.test(c.nodeValue)) continue;
      range.selectNodeContents(c);
      for (const q of range.getClientRects()) if (q.width > 0 && q.height > 0) grow(u, q);
      continue;
    }
    if (c.nodeType !== 1) continue;
    const s = getComputedStyle(c);
    if (s.display === 'none' || s.position === 'absolute' || s.position === 'fixed' || s.visibility !== 'visible') continue;
    if (s.display === 'contents' || (s.display === 'inline' && !REPLACED.test(c.localName) && !seen(s))) { contentBox(c, u); continue; }
    for (const q of c.getClientRects()) if (q.width > 0 && q.height > 0) grow(u, q);
  }
  return u;
}
const seen = (s) => s.backgroundColor !== 'rgba(0, 0, 0, 0)' || s.backgroundImage !== 'none' || s.borderTopStyle !== 'none';
function grow(u, q) {
  if (u.top == null || q.top < u.top) u.top = q.top;
  if (u.bottom == null || q.bottom > u.bottom) u.bottom = q.bottom;
}

// a row's cells centred up and down: one whose content is a pixel or so off the centre most cells share (text a
// pixel shorter than a pill, two lines of a smaller font) moves by whole pixels, so the row reads as one line
function centrePlan(row) {
  const cells = Array.from(row.cells).filter((c) => getComputedStyle(c).verticalAlign === 'middle');
  if (cells.length < 2) return [];
  const r = devicePixelRatio || 1;
  const ms = [];
  for (const c of cells) {
    const u = contentBox(c, {});
    if (u.top != null) ms.push([c, (u.top + u.bottom) / 2]);
  }
  const count = new Map();
  for (const [, m] of ms) { const k = Math.round(m * 2 * r); count.set(k, (count.get(k) || 0) + 1); }
  let ref = null, most = 0;
  for (const [k, n] of count) if (n > most) { most = n; ref = k / 2 / r; }
  if (ref == null) return [];
  const out = [];
  for (const [c, m] of ms) {
    const d = Math.round(ref - m); // whole px: layout keeps 1/64 px, so a third of one would not add up
    if (!d || Math.abs(ref - m) <= 0.5 || Math.abs(d) > 3) continue;
    const cs = getComputedStyle(c);
    const pt = parseFloat(cs.paddingTop) || 0, pb = parseFloat(cs.paddingBottom) || 0;
    if (pt + d < 0 || pb - d < 0) continue;
    out.push(() => {
      put(c, 'padding-top', `${pt + d}px`);
      put(c, 'padding-bottom', `${pb - d}px`);
    });
  }
  return out;
}

// the first baseline of a cell's words, as the eye sees it: the bottom of the first word less the font's descent
const canvas = document.createElement('canvas').getContext('2d');
const descents = new Map();
function firstWord(cell) {
  const w = document.createTreeWalker(cell, NodeFilter.SHOW_TEXT);
  const range = document.createRange();
  for (let n = w.nextNode(), k = 0; n && k < 50; n = w.nextNode(), k++) {
    const m = /\S+/.exec(n.nodeValue);
    if (!m || !n.parentElement) continue;
    range.setStart(n, m.index);
    range.setEnd(n, m.index + m[0].length);
    const q = range.getClientRects()[0];
    if (!q || !q.height) continue;
    const s = getComputedStyle(n.parentElement);
    const f = `${s.fontStyle} ${s.fontWeight} ${s.fontSize} ${s.fontFamily}`;
    if (!descents.has(f)) { canvas.font = f; descents.set(f, canvas.measureText('Hg').fontBoundingBoxDescent || 0); }
    const r = devicePixelRatio || 1;
    return { node: n, base: q.bottom - Math.round(descents.get(f) * r) / r };
  }
  return null;
}

// a row's cells on one baseline: a box in a cell whose words stand a pixel or so off the baseline most cells
// share (a pill or a dot beside a word sets its own baseline) moves by whole pixels
function baselinePlan(row) {
  const cells = Array.from(row.cells).filter((c) => getComputedStyle(c).verticalAlign === 'baseline');
  if (cells.length < 2) return [];
  const bs = [];
  for (const c of cells) { const f = firstWord(c); if (f) bs.push([c, f]); }
  const count = new Map();
  for (const [, f] of bs) { const k = Math.round(f.base); count.set(k, (count.get(k) || 0) + 1); }
  let ref = null, most = 0;
  for (const [k, n] of count) if (n > most) { most = n; ref = k; }
  if (ref == null || most < 2) return [];
  // the words of a cell that stands on the common baseline: what a box alone in a cell lines up with
  const refCell = bs.find(([, f]) => Math.round(f.base) === ref);
  const refBox = refCell && contentBox(refCell[0], {});
  const out = [];
  for (const [c, f] of bs) {
    let d = Math.round(ref - f.base);
    if (Math.abs(ref - f.base) <= 0.5) continue;
    // the cell's own child that holds the first word (words straight in the cell cannot move)
    let box = f.node.parentElement;
    while (box && box.parentElement !== c) box = box.parentElement;
    if (!box || box === c) continue;
    if (Math.abs(d) > 3) {
      // a box (a button, a pill) whose words sit higher inside it: its edges or its middle meet the line's
      if (c.children.length !== 1 || !refBox || refBox.top == null) continue;
      const bs2 = getComputedStyle(box);
      if (bs2.backgroundColor === 'rgba(0, 0, 0, 0)' && bs2.borderTopStyle === 'none') continue; // only a box you can see
      const b = box.getBoundingClientRect();
      const tries = [refBox.bottom - b.bottom, refBox.top - b.top, (refBox.top + refBox.bottom - b.top - b.bottom) / 2];
      d = null;
      for (const t of tries) { const k = Math.round(t); if (Math.abs(k) <= 3 && Math.abs(t - k) <= 0.5 && (d == null || Math.abs(k) < Math.abs(d))) d = k; }
      if (!d) continue;
    }
    if (!d) continue;
    const s = getComputedStyle(box);
    if (s.translate !== 'none' || s.position !== 'static') continue;
    out.push(() => {
      if (s.display === 'inline') { put(box, 'position', 'relative'); put(box, 'top', `${d}px`); } else put(box, 'translate', `0 ${d}px`);
    });
  }
  return out;
}

// boxes that can carry a size, and the ones left alone: maps and charts draw their own boxes
const BOXES = new Set(['block', 'inline-block', 'flex', 'inline-flex', 'grid', 'inline-grid', 'flow-root', 'list-item']);
const NOT_BOXES = '.maplibregl-map, .uplot, .mv-spin, [data-nosnap]';

// grows each box that ends between two device pixels to the next one (read them all, then write); false when
// none needed it
function boxPass(boxes) {
  const r = devicePixelRatio || 1;
  const up = (v) => Math.ceil(v * r - 0.02) / r;
  const offd = (v) => Math.abs(v * r - Math.round(v * r)) > 0.02;
  const sets = [];
  // read everything first (a computed style read after a write would lay the page out again each time)
  for (const e of boxes) {
    const b = e.getBoundingClientRect();
    if (!b.width && !b.height) continue;
    const w = offd(b.width) ? up(b.width) : 0;
    const h = offd(b.height) ? up(b.height) : 0;
    if (!w && !h) continue;
    const cs = getComputedStyle(e);
    // min-width and min-height size the border box only for border-box boxes
    const side = (ks) => (cs.boxSizing === 'content-box' ? ks.reduce((x, k) => x + (parseFloat(cs[k]) || 0), 0) : 0);
    sets.push([e, w && w - side(['paddingLeft', 'paddingRight', 'borderLeftWidth', 'borderRightWidth']),
      h && h - side(['paddingTop', 'paddingBottom', 'borderTopWidth', 'borderBottomWidth'])]);
  }
  for (const [e, w, h] of sets) {
    if (w) put(e, 'min-width', `${w}px`);
    if (h) put(e, 'min-height', `${h}px`);
  }
  return sets.length > 0;
}

// a border you can see, or a line 1 px thick drawn by a background
const SIDES = ['Top', 'Right', 'Bottom', 'Left'];
function lineish(e, s, b) {
  for (const k of SIDES) if (s[`border${k}Style`] !== 'none' && parseFloat(s[`border${k}Width`]) > 0) return true;
  return ((b.height <= 1.5 && b.width > 4) || (b.width <= 1.5 && b.height > 4)) && (s.backgroundColor !== 'rgba(0, 0, 0, 0)' || s.backgroundImage !== 'none');
}
const STRUCT = /^(table|thead|tbody|tfoot|tr|td|th|col|colgroup|caption|html|body)$/;
function nudge(list) {
  const r = devicePixelRatio || 1;
  const frac = (v) => Math.round(v * r) / r - v;
  const levels = new Map();
  for (const e of list) {
    if (STRUCT.test(e.localName) || e instanceof SVGElement) continue;
    const d = depth(e);
    if (!levels.has(d)) levels.set(d, []);
    levels.get(d).push(e);
  }
  for (const d of [...levels.keys()].sort((a, b) => a - b)) {
    const sets = [];
    for (const e of levels.get(d)) {
      const s = getComputedStyle(e);
      if (s.display === 'none' || s.display === 'contents' || s.translate !== 'none') continue;
      const inline = s.display === 'inline';
      if (inline && s.position !== 'static') continue; // an inline box moves only by its own place, not a transform
      if (s.transform !== 'none' && !/^matrix\(1, 0, 0, 1,/.test(s.transform)) continue; // turned or scaled
      const b = e.getBoundingClientRect();
      if (!b.width || !b.height || !lineish(e, s, b)) continue;
      const dx = frac(b.left), dy = frac(b.top);
      // an inline box (a word with a border) cannot take a width: its right padding takes what the width misses
      const dw = inline && e.getClientRects().length === 1 ? Math.ceil((b.right + dx) * r - 0.02) / r - (b.right + dx) : 0;
      if (Math.abs(dx) > 0.01 || Math.abs(dy) > 0.01 || dw > 0.01) sets.push([e, inline, dx, dy, dw > 0.01 ? (parseFloat(s.paddingRight) || 0) + dw : null]);
    }
    for (const [e, inline, dx, dy, pr] of sets) {
      if (Math.abs(dx) > 0.01 || Math.abs(dy) > 0.01) {
        if (inline) { put(e, 'position', 'relative'); put(e, 'left', `${dx.toFixed(4)}px`); put(e, 'top', `${dy.toFixed(4)}px`); }
        else put(e, 'translate', `${dx.toFixed(4)}px ${dy.toFixed(4)}px`);
      }
      if (pr != null) put(e, 'padding-right', `${pr.toFixed(4)}px`);
    }
  }
}

let ro = null;
const watched = new WeakSet();

export function snapAll() {
  restore();
  const grids = [], tables = [], boxes = [], lined = [];
  // walk the page, but not into tables, pictures or closed <details> (a grid inside a table cell is left alone)
  const w = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT, {
    acceptNode(e) {
      if (e.localName === 'table') { tables.push(e); return NodeFilter.FILTER_REJECT; }
      if (e.localName === 'svg' || e.localName === 'script' || e.localName === 'template') return NodeFilter.FILTER_REJECT;
      if (e.localName === 'details' && !e.open) return NodeFilter.FILTER_REJECT; // its content is not laid out
      const d = getComputedStyle(e).display;
      if (d === 'none') return NodeFilter.FILTER_REJECT;
      if (d === 'grid' || d === 'inline-grid') grids.push(e);
      if (!e.closest(NOT_BOXES)) {
        if (BOXES.has(d)) boxes.push(e);
        lined.push(e);
      }
      return NodeFilter.FILTER_SKIP;
    },
  });
  w.nextNode(); // the filter does the work and finds no node to stop at
  // the lines inside tables' cells (their boxes keep their own sizes: a table sizes its columns by them)
  for (const t of tables) {
    if (t.offsetParent === null) continue;
    for (const e of t.querySelectorAll('td *, th *')) {
      if (e.closest('svg, table table, ' + NOT_BOXES) || e.closest('details:not([open]) > :not(summary)')) continue;
      if (getComputedStyle(e).display !== 'none') lined.push(e);
    }
  }
  // outer grids first: a grid inside another gets its width from the outer one's columns
  const levels = new Map();
  for (const g of grids) { const d = depth(g); if (!levels.has(d)) levels.set(d, []); levels.get(d).push(g); }
  for (const d of [...levels.keys()].sort((a, b) => a - b)) {
    const plans = levels.get(d).map((g) => [g, gridPlan(g)]).filter((p) => p[1]); // read them all, then write
    for (const [, run] of plans) run();
  }
  // every other box whose width or height ends between two device pixels grows to the next one, so what follows
  // it starts on a whole pixel (before the tables: a box that grows can change a table's width)
  for (let i = 0; i < 3; i++) if (!boxPass(boxes)) break;
  const plans = tables.filter((t) => t.offsetParent !== null).map(tablePlan).filter(Boolean);
  for (const run of plans) run();
  // a row whose cells are an odd height inside centres their one-line content on a half pixel: it grows by one;
  // a row of a fraction of a pixel high (rows of text at scale 3) grows to the next whole one
  const rows = [];
  for (const t of tables) if (t.offsetParent !== null) for (const row of Array.from(t.rows).slice(0, 2000)) rows.push(row);
  const odd = [];
  for (const row of rows) {
    const c = row.cells[0];
    if (!c) continue;
    const h = row.getBoundingClientRect().height;
    const s = getComputedStyle(c);
    const inner = c.getBoundingClientRect().height - ['paddingTop', 'paddingBottom', 'borderTopWidth', 'borderBottomWidth'].reduce((a, k) => a + (parseFloat(s[k]) || 0), 0);
    if (!h) continue;
    // a whole px height, one more when the inside is odd
    const want = Math.ceil(h - 0.02) + (Math.round(inner + Math.ceil(h - 0.02) - h) % 2);
    if (Math.abs(want - h) > 0.01) odd.push([row, want]);
  }
  for (const [row, h] of odd) put(row, 'height', `${h}px`);
  // then cells centred up and down whose content misses the row's common centre by a pixel or so move by that much
  const shifts = rows.flatMap((row) => [...centrePlan(row), ...baselinePlan(row)]);
  for (const run of shifts) run();
  // and again, for boxes around a table that grew
  for (let i = 0; i < 2; i++) if (!boxPass(boxes)) break;
  // last, a box with a border or a 1 px line that still starts between two device pixels (it follows a line of
  // text, or a centred box an odd number of px smaller) moves by less than half a pixel onto whole ones
  nudge(lined);
  if (ro) for (const e of [...grids, ...tables]) if (!watched.has(e)) { watched.add(e); ro.observe(e); }
  // the sizes this run left, so a resize the run itself caused does not start another run
  for (const e of [...grids, ...tables]) { const b = e.getBoundingClientRect(); left.set(e, `${b.width} ${b.height}`); }
}
const left = new WeakMap();
function resized(entries) {
  for (const x of entries) {
    const b = x.target.getBoundingClientRect();
    if (left.get(x.target) !== `${b.width} ${b.height}`) { later(); return; }
  }
}

let queued = false;
let restTimer = 0;
function later() {
  if (queued) return;
  queued = true;
  requestAnimationFrame(() => { queued = false; snapAll(); });
}

/** Starts keeping grids and tables on whole pixels: call once at start. */
export function watchTables() {
  new MutationObserver((list) => {
    // elements added or taken away, or any text in a table changed (a wider number needs a wider column); a clock
    // or a number elsewhere that only changes its text does not count
    const el = (ns) => { for (const n of ns) if (n.nodeType === 1) return true; return false; };
    const inTable = (n) => { const e = n.nodeType === 1 ? n : n.parentElement; return !!(e && e.closest('table')); };
    // rows coming into and going out of a long list's window as it scrolls (shared/vlist.js, a [data-vl-body]):
    // once the scrolling rests, not every frame (the box keeps its size, so nothing outside it moves)
    const windowed = (n) => { const e = n.nodeType === 1 ? n : n.parentElement; return !!(e && e.closest('[data-vl-body]')); };
    let rest = false;
    // at once, in the same turn as the change: nothing can measure or paint the page in between
    for (const m of list) {
      if (!(el(m.addedNodes) || el(m.removedNodes) || inTable(m.target))) continue;
      if (windowed(m.target)) { rest = true; continue; }
      snapAll(); return;
    }
    if (rest) { clearTimeout(restTimer); restTimer = setTimeout(later, 150); }
  }).observe(document.body, { childList: true, subtree: true, characterData: true });
  if ('ResizeObserver' in window) ro = new ResizeObserver(resized);
  addEventListener('resize', later);
  addEventListener('phonelayout', later); // the phone layout came or went (core/frame.js moves the period bar)
  document.addEventListener('toggle', later, true); // a <details> opened or closed
  if (document.fonts) {
    document.fonts.ready.then(later);
    document.fonts.addEventListener?.('loadingdone', later);
  }
  later();
}
