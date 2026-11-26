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
// fonts have loaded. After a change of content only the boxes that hold it are snapped again (a card that loads
// its figures again, a table's new rows), unless one of them changed size: then the whole page is. Each run first puts back what the last one set, so the CSS (its tiers and container
// queries) always decides the layout and the snap only rounds it. It reads every size before it writes one.
// While the router builds a page, the runs wait and one runs when it is drawn (holdSnap).

// what the page is scrolled to is kept across a run (shared/dom.js holdScroll): a run puts back the sizes it gave and
// measures them again, and Safari pulled a window scrolled to the end of the page up by a few pixels each time
import { holdScroll } from '../shared/dom.js';
// what a run changed: element -> { property: the inline value it had before }, so the next run can put it back
const snapped = new Map();
function put(e, prop, value) {
  let was = snapped.get(e);
  if (!was) { was = {}; snapped.set(e, was); }
  if (!(prop in was)) was[prop] = [e.style.getPropertyValue(prop), e.style.getPropertyPriority(prop)];
  e.style.setProperty(prop, value);
}
function putBack(e, was) {
  for (const [prop, [v, prio]] of Object.entries(was)) {
    if (v) e.style.setProperty(prop, v, prio); else e.style.removeProperty(prop);
  }
}
function restore(roots = null) {
  for (const [e, was] of snapped) {
    if (roots && e.isConnected && !roots.some((r) => r.contains(e))) continue;
    putBack(e, was);
    snapped.delete(e);
  }
}

const off = (v) => Math.abs(v - Math.round(v)) > 0.01;

function depth(e) { let d = 0; for (let x = e; x; x = x.parentElement) d++; return d; }

// On a phone (<html data-phone>) column widths, and the widths of boxes whose edges do not show, are left as the CSS
// gives them: the snap runs again on live updates and after late parts, and a column or a box half a pixel narrower or
// wider wraps a line of text differently (or pushes what shares its line onto the next), so a block grew or shrank by
// a line under the reader. At three device pixels to the CSS pixel a column edge between two of them
// does not show; borders still land on whole device pixels (nudge() moves them, without a change of layout).
const phone = () => document.documentElement.hasAttribute('data-phone');

// grid: whole px columns with the same total width
function gridPlan(g) {
  if (phone()) return null;
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
  if (phone()) return null;
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
const wraps = (e) => { const p = e.parentElement; if (!p) return false; const ps = getComputedStyle(p); return /flex/.test(ps.display) && ps.flexWrap !== 'nowrap'; };
const edged = (s) => s.backgroundColor !== 'rgba(0, 0, 0, 0)' || s.backgroundImage !== 'none' || SIDES.some((k) => s[`border${k}Style`] !== 'none' && parseFloat(s[`border${k}Width`]) > 0);
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
  const narrow = phone();
  const sets = [];
  // read everything first (a computed style read after a write would lay the page out again each time)
  for (const e of boxes) {
    const b = e.getBoundingClientRect();
    if (!b.width && !b.height) continue;
    let w = offd(b.width) ? up(b.width) : 0;
    const h = offd(b.height) ? up(b.height) : 0;
    if (!w && !h) continue;
    const cs = getComputedStyle(e);
    // on a phone only a box whose edges show (a border, a background) grows sideways: a box of words a fraction of a
    // pixel wider can push what shares its line onto the next, and the block it is in grows or shrinks by a line
    // (what follows it still lands on whole pixels: nudge() moves it there without a change of layout); and on any
    // screen an item of a flex row that wraps keeps its width: the items (and a spacer) stretched to fill a line, each
    // held at its width rounded up, no longer fit on it, and the last one went to a line of its own
    if (w && ((narrow && !edged(cs)) || wraps(e))) w = 0;
    if (!w && !h) continue;
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
// each box's size as the last run left it: a run over part of the page checks that part kept its size
const sizeAt = new WeakMap();
const sizeKey = (b) => `${b.width} ${b.height}`;

// the grids, tables, boxes and lined elements in root (and root itself), as snapAll walks them
function collect(root, into) {
  const { grids, tables, boxes, lined } = into;
  // walk the page, but not into tables, pictures or closed <details> (a grid inside a table cell is left alone)
  const accept = (e) => {
    if (e.localName === 'table') { tables.push(e); return NodeFilter.FILTER_REJECT; }
    if (e.localName === 'svg' || e.localName === 'script' || e.localName === 'template') return NodeFilter.FILTER_REJECT;
    if (e.localName === 'details' && !e.open) {
      // its content is not laid out; the box itself and its summary are on screen
      for (const x of [e, e.querySelector(':scope > summary')]) {
        if (!x) continue;
        const d = getComputedStyle(x).display;
        if (d === 'none') continue;
        if (BOXES.has(d)) boxes.push(x);
        lined.push(x);
      }
      return NodeFilter.FILTER_REJECT;
    }
    // a map or a chart draws its own boxes: neither it nor anything in it is looked at (nor styled, nor measured)
    if (e.matches(NOT_BOXES)) return NodeFilter.FILTER_REJECT;
    const d = getComputedStyle(e).display;
    if (d === 'none') return NodeFilter.FILTER_REJECT;
    if (d === 'grid' || d === 'inline-grid') grids.push(e);
    if (BOXES.has(d)) boxes.push(e);
    lined.push(e);
    return NodeFilter.FILTER_SKIP;
  };
  if (root !== document.body && accept(root) !== NodeFilter.FILTER_SKIP) return;
  const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, { acceptNode: accept });
  w.nextNode(); // the filter does the work and finds no node to stop at
}

function run(lists) {
  const { grids, tables, boxes, lined } = lists;
  // the lines inside tables' cells (their boxes keep their own sizes: a table sizes its columns by them)
  for (const t of tables) {
    if (t.offsetParent === null) continue;
    cellLines(t.querySelectorAll('td *, th *'), lined);
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
  const rows = [];
  for (const t of tables) if (t.offsetParent !== null) for (const row of Array.from(t.rows).slice(0, 2000)) rows.push(row);
  rowPass(rows);
  // and again, for boxes around a table that grew
  for (let i = 0; i < 2; i++) if (!boxPass(boxes)) break;
  // last, a box with a border or a 1 px line that still starts between two device pixels (it follows a line of
  // text, or a centred box an odd number of px smaller) moves by less than half a pixel onto whole ones
  nudge(lined);
  if (ro) for (const e of [...grids, ...tables]) if (!watched.has(e)) { watched.add(e); ro.observe(e); }
  // the sizes this run left, so a resize the run itself caused does not start another run
  for (const e of [...grids, ...tables]) left.set(e, sizeKey(e.getBoundingClientRect()));
  for (const e of boxes) sizeAt.set(e, sizeKey(e.getBoundingClientRect()));
}

// a row whose cells are an odd height inside centres their one-line content on a half pixel: it grows by one;
// a row of a fraction of a pixel high (rows of text at scale 3) grows to the next whole one
function rowPass(rows) {
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
}
// the lines inside some cells (as run() finds them in a table)
function cellLines(cells, lined) {
  for (const e of cells) {
    if (e.closest('svg, table table, ' + NOT_BOXES) || e.closest('details:not([open]) > :not(summary)')) continue;
    if (getComputedStyle(e).display !== 'none') lined.push(e);
  }
}
const lists = () => ({ grids: [], tables: [], boxes: [], lined: [] });

export function snapAll() {
  holdScroll(document.body);
  restore();
  const l = lists();
  collect(document.body, l);
  run(l);
}

// Only the parts of the page in roots, after their content changed: the rest of the page is laid out as before
// as long as each part keeps its size, so it keeps what the last run gave it (settled() below).
function snapIn(roots, level = 0) {
  holdScroll(document.body);
  const was = roots.map((r) => sizeAt.get(r));
  restore(roots);
  const l = lists();
  for (const r of roots) collect(r, l);
  run(l);
  settled(roots, was, level);
}

// Rows added under the ones a table had (the next page of a list), or put in the place of rows (a list that loads
// again with a drive changed): the table keeps its columns (fixed by the last run), so only these rows need whole
// heights, their cells' content centred and their lines on whole pixels.
function snapRows(table, rows) {
  holdScroll(document.body);
  const was = left.get(table);
  rowPass(rows);
  const lined = [];
  for (const row of rows) cellLines(row.querySelectorAll('td *, th *'), lined);
  nudge(lined);
  // a row whose cell holds more than its column (a wider figure: the columns were fixed by the last run, and a row
  // that was swapped in place may need a wider one): the table's columns are made again
  if (rows.some((row) => Array.from(row.cells).some((c) => c.scrollWidth > c.clientWidth + 0.5))) { snapAll(); return; }
  settled([table], [was]);
}

// After a run over part of the page: when a part kept its size nothing else moved. A part that only grew or
// shrank by whole device pixels, with the boxes around it still on whole ones, moved what follows it by whole
// pixels, which keeps everything there on whole pixels too. Anything else: the whole page is snapped again, in
// the same turn.
function settled(roots, was, level = 0) {
  const r = devicePixelRatio || 1;
  const whole = (v) => Math.abs(v * r - Math.round(v * r)) <= 0.02;
  const around = new Set();
  for (let i = 0; i < roots.length; i++) {
    const e = roots[i];
    if (!e.isConnected || was[i] == null) { snapAll(); return; }
    const b = e.getBoundingClientRect();
    if (was[i] === sizeKey(b)) continue;
    const [w0, h0] = was[i].split(' ').map(Number);
    if (Math.abs(w0 - b.width) > 0.01 || !whole(b.height) || !whole(h0)) {
      // a part that changed its width (a live figure that got shorter) may move what is beside it: the box around
      // it is snapped, and so on up to three boxes; only then the whole page
      const up = roots.length === 1 && level < 3 ? boxAround(e) : null;
      if (up) { snapIn([up], level + 1); return; }
      snapAll();
      return;
    }
    for (let a = e.parentElement; a && a !== document.body; a = a.parentElement) around.add(a);
  }
  for (const a of around) {
    if (!sizeAt.has(a) && !left.has(a)) continue;
    const b = a.getBoundingClientRect();
    if (b.width || b.height) if (!whole(b.height) || !whole(b.top) || !whole(b.left)) { snapAll(); return; }
  }
  // the sizes the boxes around now have are the snapped ones (no resize-run for them)
  for (const a of around) {
    if (sizeAt.has(a)) sizeAt.set(a, sizeKey(a.getBoundingClientRect()));
    if (left.has(a)) left.set(a, sizeKey(a.getBoundingClientRect()));
  }
  for (const e of roots) {
    if (sizeAt.has(e)) sizeAt.set(e, sizeKey(e.getBoundingClientRect()));
    if (left.has(e)) left.set(e, sizeKey(e.getBoundingClientRect()));
  }
}

// the nearest box around e that the last run measured (null: none inside the page)
function boxAround(e) {
  for (let a = e.parentElement; a && a !== document.body; a = a.parentElement) if (sizeAt.has(a)) return a;
  return null;
}

// the smallest boxes the last run measured that hold every change in the records (null: snap the whole page)
function changedRoots(list) {
  const roots = new Set();
  for (const m of list) {
    // a box shown or hidden: from the box around it (its own size was not measured while hidden)
    let e = m.type === 'attributes' ? m.target.parentElement : m.target.nodeType === 1 ? m.target : m.target.parentElement;
    while (e && e !== document.body && !sizeAt.has(e)) e = e.parentElement;
    if (!e || e === document.body || !e.isConnected) return null;
    roots.add(e);
  }
  const out = [...roots].filter((r) => ![...roots].some((o) => o !== r && o.contains(r)));
  return out.length > 24 ? null : out;
}
// What the records say about rows put into or taken out of the body of a table the last run snapped: they go to
// snapRows (their own sizes only), the other records come back in `others`
function swappedRows(list) {
  const tables = new Map();
  const others = [];
  for (const m of list) {
    const tb = m.target;
    const t = m.type === 'childList' && tb.localName === 'tbody' ? tb.parentElement : null;
    const trs = (ns) => Array.from(ns).every((n) => n.localName === 'tr');
    if (!t || t.localName !== 'table' || !left.has(t) || !snapped.has(t) || !trs(m.addedNodes) || !trs(m.removedNodes)) { others.push(m); continue; }
    if (!tables.has(t)) tables.set(t, new Set());
    for (const n of m.addedNodes) if (n.isConnected) tables.get(t).add(n);
  }
  return { tables: [...tables].map(([t, rows]) => [t, [...rows]]), others };
}

const left = new WeakMap();
function resized(entries) {
  for (const x of entries) {
    // a grid or table that was taken off the page (a card drawn again) reports 0 x 0: nothing on the page moved
    if (!x.target.isConnected) { ro.unobserve(x.target); watched.delete(x.target); continue; }
    const b = x.target.getBoundingClientRect();
    if (left.get(x.target) === `${b.width} ${b.height}`) continue;
    // a long list's table (shared/vlist.js, its body a [data-vl-body]) changes height as its window moves over
    // rows of other heights: like its rows, once the scrolling rests, not a run over the whole page every frame
    if (x.target.querySelector(':scope > [data-vl-body]')) { atRest(); continue; }
    later();
    return;
  }
}

// While a page is built (from kept answers, in one go) a run per change would take longer than the page itself:
// holdSnap() holds the runs back, and the release function (call it once) runs one if anything changed.
let holds = 0, held = false;
export function holdSnap() {
  holds++;
  let done = false;
  return () => {
    if (done) return;
    done = true;
    if (--holds === 0 && held) { held = false; snapAll(); }
  };
}

let queued = false;
let restTimer = 0;
function later() {
  if (queued) return;
  queued = true;
  // (a resize or the fonts while a page is being built: one run, when it is drawn)
  requestAnimationFrame(() => { queued = false; if (holds) held = true; else snapAll(); });
}

// one run once a long list's window has stopped moving
function atRest() { clearTimeout(restTimer); restTimer = setTimeout(later, 150); }

/** Starts keeping grids and tables on whole pixels: call once at start. */
export function watchTables() {
  new MutationObserver((list) => {
    // elements added or taken away, or any text in a table changed (a wider number needs a wider column); a clock
    // or a number elsewhere that only changes its text does not count
    // (a box laid over the page, [data-snap-over], moves nothing as it comes or goes: the router's copy of the
    // loading skeleton, taken away as the page it covered is put in place and snapped)
    const el = (ns) => { for (const n of ns) if (n.nodeType === 1 && !n.hasAttribute('data-snap-over')) return true; return false; };
    const inTable = (n) => { const e = n.nodeType === 1 ? n : n.parentElement; return !!(e && e.closest('table')); };
    // rows coming into and going out of a long list's window as it scrolls (shared/vlist.js, a [data-vl-body]):
    // once the scrolling rests, not every frame (the box keeps its size, so nothing outside it moves)
    const windowed = (n) => { const e = n.nodeType === 1 ? n : n.parentElement; return !!(e && e.closest('[data-vl-body]')); };
    // a map or a chart changing inside (its markers, its canvases, its legend) changes nothing the snap looks at
    const STAGES = '.view-stage, [data-offstage]';
    const staged = (m) => {
      const e = m.target.nodeType === 1 ? m.target : m.target.parentElement;
      if (e && e.closest(STAGES)) return true;
      return m.target === document.body && m.type === 'childList' && [...m.addedNodes, ...m.removedNodes].every((n) => n.nodeType !== 1 || n.matches(STAGES));
    };
    const drawn = (n) => { const e = n.nodeType === 1 ? n : n.parentElement; return !!(e && e.closest(NOT_BOXES)); };
    let rest = false;
    // at once, in the same turn as the change: nothing can measure or paint the page in between; while the
    // router builds a page (holdSnap) once, when it is released
    // only the parts that changed (a card's figures, a table's rows): a run over the whole page lays it all out
    // several times, which on a big screen takes longer than a frame (scrolling stalls while a page reloads)
    // or a box shown or hidden (a table's "Show more" foot): it moves what is around it
    const changed = [];
    for (const m of list) {
      // a change inside what was taken off the page in the same turn (the old drawing of a part drawn again): nothing
      // on screen, and the part's own swap is a record of its own
      if (!m.target.isConnected) continue;
      if (!(m.type === 'attributes' || el(m.addedNodes) || el(m.removedNodes) || inTable(m.target))) continue;
      if (drawn(m.target)) continue;
      // a page, or a part of one, built again off screen (core/router.js stageView, core/stage.js offstage: an invisible box
      // put on the body and taken off it): nothing there is on screen yet, and the swap that puts it in place is a change
      // of its own, snapped then (a run over the whole page for every change inside the box took longer than a frame)
      if (staged(m)) continue;
      if (windowed(m.target)) { rest = true; continue; }
      changed.push(m);
    }
    if (rest) atRest();
    if (!changed.length) return;
    if (holds) { held = true; return; }
    const { tables, others } = swappedRows(changed);
    for (const [table, rows] of tables) snapRows(table, rows);
    if (!others.length) return;
    const roots = changedRoots(others);
    if (roots) snapIn(roots); else snapAll();
  }).observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ['hidden'] });
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
