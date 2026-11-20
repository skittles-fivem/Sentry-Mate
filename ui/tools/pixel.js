// Pixel checks for ui/tools/check.js --pixel: is a page "pixel centred and correct"?
//
// pixelChecks runs in the page (page.evaluate), after the page is ready, and returns a list of findings:
//   centre    text or an icon off the centre of the box that centres it
//   row       items in one row whose tops, centres, bottoms and baselines all miss each other by a little
//   column    stacked blocks whose left edges miss each other by a little
//   fraction  a border or a 1 px line that is not on a whole device pixel
//   clip      text cut off by a box that hides its overflow
//   overflow  text that sticks out of its own box
//   overlap   text on top of other text or an icon
//   hidden    text under another element
//   scroll    the page scrolls sideways
// The rules are in ui/tools/README.md ("Pixel checks"). pixelOutline draws the bad boxes for the outlined
// screenshot and pixelOutlineRemove takes them away again. Each function is self-contained: Playwright
// sends its source to the page.

'use strict';

function pixelChecks({ tol, near, fracTol, maxFindings }) {
  const T = tol + 0.01; // "more than tol": layout works in 1/64 px, so a hair over is still on target
  const NEAR = near + 0.01;
  const de = document.documentElement;
  const body = document.body;
  const dpr = window.devicePixelRatio || 1;
  const sx = window.scrollX, sy = window.scrollY;
  const vw = de.clientWidth, vh = de.clientHeight;
  const t0 = performance.now();
  const out = { count: 0, findings: [], dpr, elements: 0, texts: 0, ms: 0 };
  if (!body) return out;

  // ---------------------------------------------------------------- helpers
  function sel(e) {
    const parts = [];
    for (let x = e; x && x.nodeType === 1 && parts.length < 4; x = x.parentElement) {
      let s = x.localName;
      if (x.id) { parts.unshift(s + '#' + CSS.escape(x.id)); break; }
      const cls = (x.getAttribute('class') || '').trim().split(/\s+/).filter(Boolean).slice(0, 2);
      if (cls.length) s += '.' + cls.map((c) => CSS.escape(c)).join('.');
      const p = x.parentElement;
      if (p && x !== body && x !== de) {
        const same = Array.prototype.filter.call(p.children, (c) => c.localName === x.localName);
        if (same.length > 1) s += ':nth-of-type(' + (same.indexOf(x) + 1) + ')';
      }
      parts.unshift(s);
      if (x === body || x === de) break;
    }
    return parts.join(' > ');
  }
  const csMap = new Map();
  const cs = (e) => { let v = csMap.get(e); if (!v) { v = getComputedStyle(e); csMap.set(e, v); } return v; };
  const rMap = new Map();
  const rect = (e) => { let v = rMap.get(e); if (!v) { v = e.getBoundingClientRect(); rMap.set(e, v); } return v; };
  const opMap = new Map();
  function transparent(e) { // opacity 0 on the element or an ancestor
    if (!e || e === de) return false;
    if (opMap.has(e)) return opMap.get(e);
    const v = cs(e).opacity === '0' || transparent(e.parentElement);
    opMap.set(e, v);
    return v;
  }
  const px = (v) => parseFloat(v) || 0;
  const r2 = (v) => Math.round(v * 100) / 100;
  const f2 = (v) => r2(v).toFixed(2);
  function alpha(color) {
    if (!color || color === 'transparent') return 0;
    let m = /rgba?\(([^)]*)\)/.exec(color);
    if (m) {
      const p = m[1].split(/[\s,/]+/).filter(Boolean);
      return p.length >= 4 ? parseFloat(p[3]) * (p[3].endsWith('%') ? 0.01 : 1) : 1;
    }
    m = /\/\s*([\d.]+%?)\s*\)\s*$/.exec(color);
    return m ? parseFloat(m[1]) * (m[1].endsWith('%') ? 0.01 : 1) : 1;
  }
  const SIDES = ['Top', 'Right', 'Bottom', 'Left'];
  const sideOn = (s, k) => px(s['border' + k + 'Width']) > 0 && !/none|hidden/.test(s['border' + k + 'Style']) && alpha(s['border' + k + 'Color']) > 0;
  const shown = (e) => { const r = rect(e); const s = cs(e); return r.width > 0 && r.height > 0 && s.visibility === 'visible' && !transparent(e); };
  const inFlow = (s) => s.position !== 'absolute' && s.position !== 'fixed' && s.float === 'none';
  const inSvg = (e) => e instanceof SVGElement && !!e.ownerSVGElement;
  const REPLACED = /^(img|svg|canvas|video|iframe|input|select|textarea|object|embed|picture|meter|progress)$/;
  // a box you can see: a background, a border all round or a shadow
  function boxy(e) {
    const s = cs(e);
    return alpha(s.backgroundColor) > 0.05 || s.backgroundImage !== 'none' || s.boxShadow !== 'none' || SIDES.every((k) => sideOn(s, k));
  }
  // the box text is centred against: inside the borders for a box you can see, else the content box
  function frameOf(e) {
    const r = rect(e), s = cs(e);
    const b = { left: r.left + px(s.borderLeftWidth), top: r.top + px(s.borderTopWidth), right: r.right - px(s.borderRightWidth), bottom: r.bottom - px(s.borderBottomWidth) };
    if (boxy(e)) return b;
    return { left: b.left + px(s.paddingLeft), top: b.top + px(s.paddingTop), right: b.right - px(s.paddingRight), bottom: b.bottom - px(s.paddingBottom) };
  }
  const paddingBox = (e) => {
    const r = rect(e), s = cs(e);
    return { left: r.left + px(s.borderLeftWidth), top: r.top + px(s.borderTopWidth), right: r.right - px(s.borderRightWidth), bottom: r.bottom - px(s.borderBottomWidth) };
  };
  function flowPseudo(e) { // a ::before or ::after that takes part in the layout: its size cannot be measured
    for (const p of ['::before', '::after']) {
      const s = getComputedStyle(e, p);
      if (s.content && s.content !== 'none' && s.content !== 'normal' && s.display !== 'none' && inFlow(s)) return true;
    }
    return false;
  }
  const range = document.createRange();
  // the boxes of the words of a text node (spaces left out, so a space at a line end never counts)
  function wordRects(n, list, cap) {
    const v = n.nodeValue;
    const re = /\S+/g;
    let m, k = 0;
    while ((m = re.exec(v)) && k++ < cap) {
      range.setStart(n, m.index);
      range.setEnd(n, m.index + m[0].length);
      for (const q of range.getClientRects()) if (q.width > 0 && q.height > 0) list.push(q);
    }
  }
  // what an element shows inside it: the words, inline boxes and child boxes, in flow and visible
  function contentRects(e, list, state) {
    for (const c of e.childNodes) {
      if (c.nodeType === 3) { if (/\S/.test(c.nodeValue)) { state.text = true; wordRects(c, list, 300); } continue; }
      if (c.nodeType !== 1) continue;
      const s = cs(c);
      if (s.display === 'none' || !inFlow(s)) continue;
      if (s.visibility !== 'visible' || transparent(c)) continue;
      if (s.display === 'contents') { contentRects(c, list, state); continue; }
      if (s.display === 'inline' && !REPLACED.test(c.localName)) {
        if (flowPseudo(c)) state.unknown = true;
        const plain = !boxy(c) && px(s.paddingLeft) + px(s.paddingRight) === 0;
        if (plain) contentRects(c, list, state);
        else for (const q of c.getClientRects()) if (q.width > 0 && q.height > 0) list.push(q);
        if (/\S/.test(c.textContent)) state.text = true;
        continue;
      }
      const q = rect(c);
      if (q.width > 0 && q.height > 0) list.push(q);
      if (/\S/.test(c.textContent)) state.text = true; else state.icon = true;
    }
  }
  const union = (rs) => rs.reduce((u, q) => (u ? { left: Math.min(u.left, q.left), top: Math.min(u.top, q.top), right: Math.max(u.right, q.right), bottom: Math.max(u.bottom, q.bottom) }
    : { left: q.left, top: q.top, right: q.right, bottom: q.bottom }), null);
  function lines(rs) {
    const sorted = rs.slice().sort((a, b) => a.top - b.top || a.left - b.left);
    const L = [];
    for (const q of sorted) {
      const h = q.bottom - q.top;
      const l = L.find((x) => Math.min(x.bottom, q.bottom) - Math.max(x.top, q.top) >= 0.5 * Math.min(x.bottom - x.top, h));
      if (l) { l.left = Math.min(l.left, q.left); l.right = Math.max(l.right, q.right); l.top = Math.min(l.top, q.top); l.bottom = Math.max(l.bottom, q.bottom); }
      else L.push({ left: q.left, top: q.top, right: q.right, bottom: q.bottom });
    }
    return L;
  }
  const mid = (a, b) => (a + b) / 2;
  const isText = (state) => (state.text ? 'text' : 'icon');

  const seen = new Set();
  function add(kind, e, what, by, box) {
    const s = sel(e);
    const key = kind + ' ' + s + ' ' + what.replace(/[\d.]+/g, '#');
    if (seen.has(key)) return;
    seen.add(key);
    out.count++;
    if (out.findings.length >= maxFindings) return;
    const b = box || rect(e);
    out.findings.push({
      n: out.findings.length + 1, kind, selector: s, what, by: r2(by),
      box: { x: r2(b.left + sx), y: r2(b.top + sy), w: r2(b.right - b.left), h: r2(b.bottom - b.top) },
    });
  }
  function offCentre(e, kind, label, c, f, axis, box) {
    const d = axis === 'x' ? mid(c.left, c.right) - mid(f.left, f.right) : mid(c.top, c.bottom) - mid(f.top, f.bottom);
    if (Math.abs(d) <= T) return;
    const where = axis === 'x' ? (d > 0 ? 'right of' : 'left of') : (d > 0 ? 'below' : 'above');
    add(kind, e, `${label} ${f2(Math.abs(d))} px ${where} the centre of its box`, Math.abs(d), box || c);
  }

  // font descent, for baselines: the text box ends at baseline + descent (rounded to device pixels)
  const ctx = document.createElement('canvas').getContext('2d');
  const descMap = new Map();
  function descent(e) {
    const s = cs(e);
    const f = `${s.fontStyle} ${s.fontWeight} ${s.fontSize} ${s.fontFamily}`;
    if (!descMap.has(f)) {
      ctx.font = f;
      const m = ctx.measureText('Hg');
      descMap.set(f, Math.round((m.fontBoundingBoxDescent || 0) * dpr) / dpr);
    }
    return descMap.get(f);
  }
  function firstBaseline(e) {
    const w = document.createTreeWalker(e, NodeFilter.SHOW_TEXT);
    for (let n = w.nextNode(), k = 0; n && k < 200; n = w.nextNode(), k++) {
      if (!/\S/.test(n.nodeValue) || !n.parentElement) continue;
      const rs = [];
      wordRects(n, rs, 1);
      if (rs.length) return rs[0].bottom - descent(n.parentElement);
    }
    return null;
  }
  const tracks = (v) => (!v || v === 'none' ? 0 : v.replace(/\[[^\]]*\]/g, ' ').trim().split(/\s+/).filter(Boolean).length);

  const all = body.querySelectorAll('*');
  out.elements = all.length;

  // ---------------------------------------------------------------- centre
  for (const e of all) {
    if (inSvg(e) || REPLACED.test(e.localName) || e.closest('[data-sm-pixel-outline]')) continue;
    const s = cs(e);
    const d = s.display;
    if (d === 'none' || d === 'inline' || d === 'contents' || /table$/.test(d) || !shown(e)) continue;
    const flex = /flex$/.test(d), grid = /grid$/.test(d);
    if (flex || grid) {
      // centring by flex or grid
      const items = [];
      let unknown = flowPseudo(e);
      for (const c of e.childNodes) {
        if (c.nodeType === 3) {
          if (!/\S/.test(c.nodeValue)) continue;
          const rs = [];
          wordRects(c, rs, 300);
          if (rs.length) items.push({ r: union(rs), el: null, text: true });
          continue;
        }
        if (c.nodeType !== 1) continue;
        const cst = cs(c);
        if (cst.display === 'none' || !inFlow(cst)) continue;
        if (cst.display === 'contents') { unknown = true; continue; }
        const q = rect(c);
        if (q.width === 0 && q.height === 0) continue;
        if (cst.visibility !== 'visible' || transparent(c)) { unknown = true; continue; }
        items.push({ r: q, el: c, text: /\S/.test(c.textContent), self: cst.alignSelf, jself: cst.justifySelf });
      }
      if (unknown || !items.length) continue;
      const f = frameOf(e);
      const u = union(items.map((i) => i.r));
      if (u.right - u.left > f.right - f.left + 0.5 || u.bottom - u.top > f.bottom - f.top + 0.5) continue; // too big: overflow
      const label = items.some((i) => i.text) ? 'text' : 'icon';
      if (flex) {
        const row = s.flexDirection.startsWith('row');
        const main = row ? 'x' : 'y', cross = row ? 'y' : 'x';
        const oneLine = s.flexWrap === 'nowrap' || lines(items.map((i) => i.r)).length <= 1;
        if (!oneLine) continue;
        if (/(^|\s)center$/.test(s.justifyContent)) offCentre(e, 'centre', label, u, f, main);
        for (const i of items) {
          const a = i.self && i.self !== 'auto' ? i.self : s.alignItems;
          if (/(^|\s)center$/.test(a)) offCentre(i.el || e, 'centre', i.text ? 'text' : 'icon', i.r, f, cross, i.el ? null : i.r);
        }
      } else if (items.length === 1 && tracks(s.gridTemplateColumns) <= 1 && tracks(s.gridTemplateRows) <= 1) {
        const i = items[0];
        const jx = i.jself && !/auto|normal/.test(i.jself) ? i.jself : s.justifyItems;
        const ay = i.self && i.self !== 'auto' ? i.self : s.alignItems;
        const target = i.el || e;
        if (/(^|\s)center$/.test(jx) || /(^|\s)center$/.test(s.justifyContent)) offCentre(target, 'centre', label, i.r, f, 'x', i.el ? null : i.r);
        if (/(^|\s)center$/.test(ay) || /(^|\s)center$/.test(s.alignContent)) offCentre(target, 'centre', label, i.r, f, 'y', i.el ? null : i.r);
      }
      continue;
    }
    if (!/center/.test(s.textAlign)) continue;
    // centring by text-align: only boxes that hold inline content (no block children)
    let blocky = false;
    for (const c of e.children) {
      const cd = cs(c);
      if (cd.display !== 'none' && inFlow(cd) && !cd.display.startsWith('inline') && cd.display !== 'contents') { blocky = true; break; }
    }
    if (blocky || flowPseudo(e)) continue;
    const rs = [];
    const state = { text: false, icon: false, unknown: false };
    contentRects(e, rs, state);
    if (state.unknown || !rs.length) continue;
    const f = frameOf(e);
    const L = lines(rs);
    if (L.some((l) => l.right - l.left > f.right - f.left + 0.5)) continue; // wider than the box: overflow
    for (const l of L) offCentre(e, 'centre', isText(state), l, f, 'x');
    if (L.length === 1 && boxy(e)) {
      const l = L[0];
      const free = (f.bottom - f.top) - (l.bottom - l.top);
      if (free >= -0.5 && free <= Math.max(16, l.bottom - l.top)) offCentre(e, 'centre', isText(state), l, f, 'y');
    }
  }

  // ---------------------------------------------------------------- rows and columns
  function measure(item, contentOnly) {
    let r = rect(item);
    if (contentOnly) {
      const rs = [];
      contentRects(item, rs, {});
      if (!rs.length) return null;
      r = union(rs);
    }
    return { top: r.top, mid: mid(r.top, r.bottom), bottom: r.bottom, base: firstBaseline(item), box: r };
  }
  const NAMES = { top: 'tops', mid: 'centres', bottom: 'bottoms', base: 'baselines' };
  for (const p of all) {
    if (inSvg(p) || p.closest('[data-sm-pixel-outline]')) continue;
    const s = cs(p);
    const d = s.display;
    if (d === 'none' || d === 'contents' || !shown(p)) continue;
    const flexRow = /flex$/.test(d) && s.flexDirection.startsWith('row');
    const tr = d === 'table-row';
    const kids = [];
    for (const c of p.children) {
      const cst = cs(c);
      if (cst.display === 'none' || cst.display === 'contents' || !inFlow(cst) || !shown(c) || inSvg(c)) continue;
      kids.push(c);
    }
    if (kids.length < 2) continue;
    // a row: flex row items, grid items, table cells, inline blocks and form fields side by side
    const rowKids = flexRow || tr || /grid$/.test(d) ? kids
      : kids.filter((c) => cs(c).display !== 'inline' && cs(c).display.startsWith('inline'));
    let prev = null;
    for (const c of rowKids) {
      const m = measure(c, tr);
      if (!m) continue;
      if (prev) {
        const a = prev.m.box, b = m.box;
        const overlapY = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
        const beside = b.left >= a.right - 1 || a.left >= b.right - 1;
        if (beside && overlapY >= 0.5 * Math.min(a.bottom - a.top, b.bottom - b.top)) {
          let best = null;
          for (const k of ['top', 'mid', 'bottom', 'base']) {
            if (prev.m[k] == null || m[k] == null) continue;
            const diff = Math.abs(prev.m[k] - m[k]);
            if (!best || diff < best.diff) best = { k, diff };
          }
          if (best && best.diff > T && best.diff <= NEAR) add('row', c, `${NAMES[best.k]} miss ${sel(prev.el)} by ${f2(best.diff)} px`, best.diff, b);
        }
      }
      prev = { el: c, m };
    }
    // a column: blocks stacked one under the other
    if (tr || flexRow) continue;
    const stacked = kids.filter((c) => !cs(c).display.startsWith('inline') && !/^table-/.test(cs(c).display));
    for (let i = 1; i < stacked.length; i++) {
      const a = rect(stacked[i - 1]), b = rect(stacked[i]);
      if (b.top < a.bottom - 0.5) continue;
      if (Math.min(a.right, b.right) <= Math.max(a.left, b.left)) continue;
      const dl = Math.abs(a.left - b.left);
      if (dl <= T || dl > NEAR) continue;
      if (Math.abs(mid(a.left, a.right) - mid(b.left, b.right)) <= T || Math.abs(a.right - b.right) <= T) continue; // centred or right-aligned
      add('column', stacked[i], `left edge misses ${sel(stacked[i - 1])} by ${f2(dl)} px`, dl, b);
    }
  }

  // ---------------------------------------------------------------- borders and 1 px lines on device pixels
  const offDev = (v) => { const dv = v * dpr; return Math.abs(dv - Math.round(dv)); };
  for (const e of all) {
    if (inSvg(e) || e.closest('[data-sm-pixel-outline]') || !shown(e)) continue;
    const s = cs(e);
    if (s.transform !== 'none') {
      const m = /^matrix\(([^)]*)\)$/.exec(s.transform);
      if (!m) continue; // 3D: not measured
      const v = m[1].split(',').map(Number);
      if (Math.abs(v[1]) > 1e-6 || Math.abs(v[2]) > 1e-6) continue; // turned: edges are not straight
    }
    const r = rect(e);
    const sides = SIDES.filter((k) => sideOn(s, k));
    const hair = ((r.height <= 1.5 && r.width > 4) || (r.width <= 1.5 && r.height > 4)) && (alpha(s.backgroundColor) > 0 || s.backgroundImage !== 'none');
    if (!sides.length && !hair) continue;
    const edges = [];
    const edge = { Top: r.top + sy, Bottom: r.bottom + sy, Left: r.left + sx, Right: r.right + sx };
    for (const k of hair ? SIDES : sides) {
      edges.push({ name: k.toLowerCase() + ' edge', v: edge[k] });
      if (!hair) edges.push({ name: k.toLowerCase() + ' border width', v: px(s['border' + k + 'Width']) });
    }
    let worst = null;
    for (const x of edges) { const o = offDev(x.v); if (!worst || o > worst.o) worst = { ...x, o }; }
    if (worst && worst.o > fracTol) {
      add('fraction', e, `${hair ? '1 px line' : 'border'}: ${worst.name} at ${r2(worst.v)} px is ${r2(worst.v * dpr)} device px at scale ${dpr}`, worst.o / dpr);
    }
  }

  // ---------------------------------------------------------------- text: clipped, overflowing, overlapping, hidden
  const textBoxes = [];
  const icons = [];
  for (const e of all) {
    if (!/^(svg|img|canvas)$/.test(e.localName) || inSvg(e) || !shown(e)) continue;
    const r = rect(e);
    if (r.width <= 64 && r.height <= 64) icons.push(e);
  }
  const isCB = (s) => s.position !== 'static' || s.transform !== 'none' || s.filter !== 'none' || /paint|layout|strict|content/.test(s.contain);
  const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
  nodes: for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const v = n.nodeValue;
    if (!/\S/.test(v)) continue;
    const t = n.parentElement;
    if (!t || t.closest('script, style, noscript, template, title, svg, [data-sm-pixel-outline]')) continue;
    const ts = cs(t);
    if (ts.visibility !== 'visible' || transparent(t)) continue;
    const a = v.search(/\S/);
    const b = v.length - /\s*$/.exec(v)[0].length;
    range.setStart(n, a);
    range.setEnd(n, b);
    const raw = Array.from(range.getClientRects()).filter((q) => q.width > 0.5 && q.height > 0.5);
    if (!raw.length) continue;
    out.texts++;
    // the line box of the font: a text box (ascent to descent, from the Range) taller than its line height is
    // counted as the line height high for overlaps, and its extra height is slack for cutting and sticking out:
    // the glyphs sit inside the line, and Chromium rounds where the text box sits in it
    const lh = parseFloat(ts.lineHeight);
    const rs = raw.map((q) => {
      let top = q.top, bottom = q.bottom;
      const slack = Number.isFinite(lh) && lh > 0 && q.height > lh ? q.height - lh : 0;
      if (slack) { const m = mid(top, bottom); top = m - lh / 2; bottom = m + lh / 2; }
      return { left: q.left, right: q.right, top, bottom, raw: q, slack };
    });
    let blk = t;
    while (blk !== body && /^(inline|contents)$/.test(cs(blk).display) && blk.parentElement) blk = blk.parentElement;
    // the boxes that cut it (overflow hidden or clip; a scrolling box shows the rest when scrolled)
    const clips = [];
    let mode = 'normal';
    for (let x = t; x && x !== de; x = x.parentElement) {
      const xs = cs(x);
      if (xs.clipPath !== 'none' || (xs.clip && xs.clip !== 'auto')) continue nodes; // hidden on purpose (screen reader text)
      const can = mode === 'normal' || isCB(xs);
      if (can) {
        mode = 'normal';
        if (x !== body && (xs.overflowX !== 'visible' || xs.overflowY !== 'visible')) {
          const xr = rect(x);
          if (xr.width <= 2 || xr.height <= 2) continue nodes; // a 1 px box: screen reader text
          const hide = (o) => o === 'hidden' || o === 'clip';
          clips.push({
            el: x, box: paddingBox(x), hx: hide(xs.overflowX) && xs.textOverflow !== 'ellipsis' && cs(blk).textOverflow !== 'ellipsis',
            hy: hide(xs.overflowY) && (!xs.webkitLineClamp || xs.webkitLineClamp === 'none'), scroll: !hide(xs.overflowX) || !hide(xs.overflowY),
          });
        }
      }
      if (xs.position === 'fixed') break;
      if (xs.position === 'absolute') mode = 'abs';
    }
    const bs = cs(blk);
    for (const q of rs) {
      // the part that can be seen
      const raw = q.raw;
      let vis = { left: q.left, right: q.right, top: q.top, bottom: q.bottom };
      let cut = null;
      for (const c of clips) {
        const k = c.box;
        const o = { left: k.left - raw.left, right: raw.right - k.right, top: k.top - raw.top - q.slack, bottom: raw.bottom - k.bottom - q.slack };
        vis = { left: Math.max(vis.left, k.left), right: Math.min(vis.right, k.right), top: Math.max(vis.top, k.top), bottom: Math.min(vis.bottom, k.bottom) };
        if (vis.right - vis.left <= 0.5 || vis.bottom - vis.top <= 0.5) { vis = null; break; }
        const by = Math.max(c.hx ? Math.max(o.left, o.right) : 0, c.hy ? Math.max(o.top, o.bottom) : 0);
        if (by > T && (!cut || by > cut.by)) cut = { el: c.el, by };
      }
      if (!vis) continue; // cut off completely: hidden on purpose (a closed panel, a carousel)
      if (cut) add('clip', t, `text cut off by ${sel(cut.el)} by ${f2(cut.by)} px`, cut.by, q);
      if (blk !== body && bs.overflowX === 'visible' && bs.overflowY === 'visible') {
        const k = paddingBox(blk);
        const sides = [['left', k.left - raw.left], ['right', raw.right - k.right], ['top', k.top - raw.top - q.slack], ['bottom', raw.bottom - k.bottom - q.slack]];
        const [side, by] = sides.reduce((w, x) => (x[1] > w[1] ? x : w));
        if (by > T) add('overflow', t, `text sticks out of the ${side} of its box ${blk === t ? '' : sel(blk) + ' '}by ${f2(by)} px`, by, q);
      }
      textBoxes.push({ t, n, q: vis });
    }
  }

  // text on text, text on icons: a grid of 128 px cells; a pair is tested in the cell that holds the top left
  // corner of where the two meet, so it is tested once
  const CELL = 128;
  const grid = new Map();
  const cells = (q, f) => {
    for (let gx = Math.floor(q.left / CELL); gx <= Math.floor(q.right / CELL); gx++) {
      for (let gy = Math.floor(q.top / CELL); gy <= Math.floor(q.bottom / CELL); gy++) f(gx + ',' + gy, gx, gy);
    }
  };
  textBoxes.forEach((x, i) => cells(x.q, (k) => { let l = grid.get(k); if (!l) { l = []; grid.set(k, l); } l.push(i); }));
  const inter = (a, b) => ({ w: Math.min(a.right, b.right) - Math.max(a.left, b.left), h: Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) });
  const home = (a, b, gx, gy) => Math.floor(Math.max(a.left, b.left) / CELL) === gx && Math.floor(Math.max(a.top, b.top) / CELL) === gy;
  for (const [key, list] of grid) {
    const [gx, gy] = key.split(',').map(Number);
    for (let i = 0; i < list.length; i++) {
      const A = textBoxes[list[i]];
      for (let j = i + 1; j < list.length; j++) {
        const B = textBoxes[list[j]];
        if (A.n === B.n || A.t === B.t) continue;
        const x = inter(A.q, B.q);
        if (x.w > 1 && x.h > 1 && home(A.q, B.q, gx, gy)) add('overlap', B.t, `text overlaps the text of ${sel(A.t)} by ${f2(Math.min(x.w, x.h))} px`, Math.min(x.w, x.h), B.q);
      }
    }
  }
  for (const ic of icons) {
    const ir = rect(ic);
    const done = new Set();
    cells(ir, (k) => {
      for (const i of grid.get(k) || []) {
        if (done.has(i)) continue;
        done.add(i);
        const x = textBoxes[i];
        if (ic.contains(x.t) || x.t.contains(ic)) continue;
        const o = inter(x.q, ir);
        if (o.w > 1 && o.h > 1) add('overlap', x.t, `text overlaps ${sel(ic)} by ${f2(Math.min(o.w, o.h))} px`, Math.min(o.w, o.h), x.q);
      }
    });
  }

  // text under another element, in the window (elementFromPoint at three points of each line)
  const opaque = (e) => {
    const s = cs(e);
    if (s.visibility !== 'visible' || transparent(e)) return false;
    return REPLACED.test(e.localName) || alpha(s.backgroundColor) >= 0.5 || s.backgroundImage !== 'none';
  };
  function coverAt(t, x, y) {
    const h = document.elementFromPoint(x, y);
    if (!h || h === t || t.contains(h) || h.contains(t)) return null;
    for (let e = h; e && !e.contains(t); e = e.parentElement) {
      const s = cs(e);
      if (s.position === 'fixed' || s.position === 'sticky') return null; // a bar the page scrolls under
      if (opaque(e)) return e;
    }
    return null;
  }
  for (const x of textBoxes) {
    if (cs(x.t).pointerEvents === 'none') continue; // the point test cannot see it
    const q = x.q;
    const y = mid(q.top, q.bottom);
    if (y < 0 || y >= vh) continue;
    for (const fx of [0.2, 0.5, 0.8]) {
      const px0 = q.left + (q.right - q.left) * fx;
      if (px0 < 0 || px0 >= vw) continue;
      const c = coverAt(x.t, px0, y);
      if (c) {
        const k = inter(q, rect(c));
        add('hidden', x.t, `text is hidden behind ${sel(c)}`, Math.max(0, Math.min(k.w, k.h)), q);
        break;
      }
    }
  }

  // ---------------------------------------------------------------- sideways scroll
  if (de.scrollWidth > de.clientWidth) {
    add('scroll', de, `the page scrolls sideways: ${de.scrollWidth} px wide in a ${de.clientWidth} px window`, de.scrollWidth - de.clientWidth,
      { left: -sx, top: -sy, right: de.scrollWidth - sx, bottom: Math.min(de.scrollHeight, vh) - sy });
  }

  out.ms = Math.round(performance.now() - t0);
  return out;
}

// Draws each finding's box (page coordinates) with its number and kind, for the outlined screenshot.
function pixelOutline(boxes) {
  const de = document.documentElement;
  const W = Math.max(de.scrollWidth, de.clientWidth), H = Math.max(de.scrollHeight, de.clientHeight);
  const COLOURS = { centre: '#e0004d', row: '#ff6a00', column: '#c000c0', fraction: '#0066ff', clip: '#7a00ff', overflow: '#7a00ff', overlap: '#e0004d', hidden: '#e0004d', scroll: '#ff6a00' };
  const st = (e, props) => { for (const [k, v] of Object.entries(props)) e.style.setProperty(k, v, 'important'); };
  const root = document.createElement('div');
  root.setAttribute('data-sm-pixel-outline', '');
  st(root, { position: 'absolute', left: '0', top: '0', width: '0', height: '0', margin: '0', padding: '0', border: '0', 'z-index': '2147483647', 'pointer-events': 'none' });
  for (const b of boxes) {
    if (b.x >= W || b.y >= H) continue;
    const c = COLOURS[b.kind] || '#e0004d';
    const d = document.createElement('div');
    st(d, {
      position: 'absolute', left: (b.x - 2) + 'px', top: (b.y - 2) + 'px', width: Math.min(b.w + 4, W) + 'px', height: Math.min(b.h + 4, H) + 'px',
      outline: '2px solid ' + c, background: 'transparent', margin: '0', padding: '0', border: '0', 'box-sizing': 'border-box',
    });
    const tag = document.createElement('span');
    tag.textContent = b.n + ' ' + b.kind;
    st(tag, {
      position: 'absolute', left: '-2px', top: '-17px', font: '11px/15px monospace', color: '#fff', background: c, padding: '0 3px',
      'white-space': 'nowrap', 'letter-spacing': '0', 'text-transform': 'none',
    });
    d.append(tag);
    root.append(d);
  }
  de.append(root);
  return root.childElementCount;
}

function pixelOutlineRemove() {
  for (const e of document.querySelectorAll('[data-sm-pixel-outline]')) e.remove();
}

module.exports = { pixelChecks, pixelOutline, pixelOutlineRemove };
