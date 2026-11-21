// Grid columns and table columns on whole pixels.
//
// A grid shares its free width out among its fr columns, and a table among its columns, in fractions of a
// pixel, so every column edge after the first can land between two pixels, and with it every border, button
// and line in the columns. snapAll() puts each column edge on a whole px: every column is rounded up to a
// whole px and the widest gives back the difference, so the total width stays the same and no column gets
// narrower than what is in it.
//
// It runs again when the page adds or changes content, when a snapped box changes size and when the fonts
// have loaded. Each run first takes its own widths off again, so the CSS (its tiers and container queries)
// always decides the layout and the snap only rounds it.

const snapped = new Set(); // elements that carry a snapped width

const off = (v) => Math.abs(v - Math.round(v)) > 0.01;

function depth(e) { let d = 0; for (let x = e; x; x = x.parentElement) d++; return d; }

// grid: whole px columns with the same total width
function gridPlan(g) {
  const v = getComputedStyle(g).gridTemplateColumns;
  if (!v || v === 'none' || /[^\d.\spx-]/.test(v)) return null; // subgrid, names or anything not plain px
  const tracks = v.trim().split(/\s+/).map(parseFloat);
  if (tracks.length < 2 || tracks.some((w) => !Number.isFinite(w)) || !tracks.some(off)) return null;
  // each column rounded up, so no column gets narrower than what is in it, and the widest gives back the
  // difference (it is the one that shares out the free width)
  const total = Math.round(tracks.reduce((a, b) => a + b, 0));
  const out = tracks.map((w) => Math.ceil(w - 0.01));
  let big = 0;
  tracks.forEach((w, i) => { if (w > tracks[big]) big = i; });
  out[big] = total - out.reduce((a, b, i) => (i === big ? a : a + b), 0);
  return () => g.style.setProperty('grid-template-columns', out.map((w) => `${w}px`).join(' '));
}

// table: each column gets its width rounded up to a whole px and the widest takes what is left over (a table
// wider than its box keeps all its rounded widths); the table gets the sum as its width and a fixed layout,
// so it has nothing left to share out
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
  const px = ws.map((w) => Math.ceil(w - 0.01)); // rounded up: no column gets narrower than what is in it
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
    for (const [c, w] of sets) { c.style.setProperty('width', w); snapped.add(c); }
    t.style.setProperty('width', `${total}px`);
    t.style.setProperty('table-layout', 'fixed'); // the first row decides, whatever widths the other rows ask for
    snapped.add(t);
  };
}

let ro = null;
const watched = new WeakSet();

export function snapAll() {
  for (const e of snapped) {
    e.style.removeProperty('grid-template-columns');
    e.style.removeProperty('width');
    e.style.removeProperty('table-layout');
  }
  snapped.clear();
  const grids = [], tables = [];
  // walk the page, but not into tables, pictures or closed <details> (a grid inside a table cell is left alone)
  const w = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT, {
    acceptNode(e) {
      if (e.localName === 'table') { tables.push(e); return NodeFilter.FILTER_REJECT; }
      if (e.localName === 'svg' || e.localName === 'script' || e.localName === 'template') return NodeFilter.FILTER_REJECT;
      if (e.localName === 'details' && !e.open) return NodeFilter.FILTER_REJECT; // its content is not laid out
      const d = getComputedStyle(e).display;
      if (d === 'none') return NodeFilter.FILTER_REJECT;
      if (d === 'grid' || d === 'inline-grid') grids.push(e);
      return NodeFilter.FILTER_SKIP;
    },
  });
  w.nextNode(); // the filter does the work and finds no node to stop at
  // outer grids first: a grid inside another gets its width from the outer one's columns
  const levels = new Map();
  for (const g of grids) { const d = depth(g); if (!levels.has(d)) levels.set(d, []); levels.get(d).push(g); }
  for (const d of [...levels.keys()].sort((a, b) => a - b)) {
    const plans = levels.get(d).map((g) => [g, gridPlan(g)]).filter((p) => p[1]); // read them all, then write
    for (const [g, run] of plans) { run(); snapped.add(g); }
  }
  const plans = tables.filter((t) => t.offsetParent !== null).map(tablePlan).filter(Boolean);
  for (const run of plans) run();
  if (ro) for (const e of [...grids, ...tables]) if (!watched.has(e)) { watched.add(e); ro.observe(e); }
}

let queued = false;
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
    // at once, in the same turn as the change: nothing can measure or paint the page in between
    for (const m of list) if (el(m.addedNodes) || el(m.removedNodes) || inTable(m.target)) { snapAll(); return; }
  }).observe(document.body, { childList: true, subtree: true, characterData: true });
  if ('ResizeObserver' in window) ro = new ResizeObserver(later);
  addEventListener('resize', later);
  document.addEventListener('toggle', later, true); // a <details> opened or closed
  if (document.fonts) {
    document.fonts.ready.then(later);
    document.fonts.addEventListener?.('loadingdone', later);
  }
  later();
}
