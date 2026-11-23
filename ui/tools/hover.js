// Hover checks of ui/tools/check.js (--hover): does anything move when the mouse goes over the page?
//
// The owner's rule: hover and focus may only change colour, opacity, shadow and transform, never a width,
// height, margin, padding, border width or font weight, and tooltips sit in an overlay that never takes room.
// check.js moves the real mouse (Playwright) over a grid of points in the window, over every interactive
// element (links, buttons, fields, rows, anything with a pointer cursor or a title) and over every chart
// (svg, canvas, map) at several points, then focuses each element that takes focus. After each move this file
// measures, in the page, the box of every element in and near the window against the boxes with the mouse
// outside the window, and lists each element outside the hovered one that moved or changed size by more than
// 0.5 CSS px. A move is only reported when the same hover moves it again after the mouse went away and the
// page was back where it was (so a live update between two looks is not taken for a hover).
//
// Not counted: the hovered (or focused) element and what is inside it; elements that are hidden (opacity 0,
// visibility hidden) with the mouse away or on it, or out of the flow (absolute or fixed) and hidden with the
// mouse away, a tooltip (role="tooltip"), not hit by the mouse (pointer-events: none) or marked
// data-hover-overlay: an overlay layer; elements under a transform that changed (transforms are allowed); an
// element whose own text or children the hover changed (a readout: its box may follow its text, what is around
// it still counts); what is inside svg, canvas and a MapLibre map (their own box counts). The page scrolling,
// or its scroller changing its inner size (a scrollbar that came or went), is reported too. The scroller is the
// document, or the box that fills most of the window when the document does not scroll (Pebble's .scroller).
//
// Every function here runs in the page (page.evaluate); hoverStart installs the rest on window.__smHover.
// See ui/tools/README.md ("Hover checks").

'use strict';

// arg: { tol, grid: [cols, rows], perKind, maxTargets, chartPoints, band }
function hoverStart(arg) {
  const tol = arg.tol;
  const H = window.__smHover = { arg, rest: null, els: [], targets: [], tested: new WeakSet(), ms: 0 };
  // Transitions end at once, so a hover's last state is measured right away; and what only paints (blur behind
  // glass, filters, shadows, gradients) is left out while this runs: none of it takes room, and at 4K it made
  // every frame, so every mouse move, take a quarter of a second. (A constructed style sheet: the pages' CSP
  // refuses a <style> element.)
  const sheet = new CSSStyleSheet();
  sheet.replaceSync('*,*::before,*::after{transition-duration:0s!important;transition-delay:0s!important;caret-color:transparent!important;'
    + 'backdrop-filter:none!important;-webkit-backdrop-filter:none!important;filter:none!important;box-shadow:none!important;text-shadow:none!important;background-image:none!important}');
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  H.sheet = sheet;

  const frames = (n) => new Promise((r) => { const f = () => (--n <= 0 ? r() : requestAnimationFrame(f)); requestAnimationFrame(f); });
  const SKIP = new Set(['script', 'style', 'link', 'meta', 'template', 'noscript', 'title', 'head', 'option', 'br', 'wbr', 'source', 'track', 'param', 'optgroup', 'datalist']);
  const leaf = (e) => e.localName === 'svg' || e.localName === 'canvas' || e.localName === 'iframe' || e.classList.contains('maplibregl-map');

  function sel(e) {
    if (!e) return '?';
    const parts = [];
    for (let x = e; x && x.nodeType === 1 && parts.length < 4; x = x.parentElement) {
      let s = x.localName;
      if (x.id) { parts.unshift(s + '#' + CSS.escape(x.id)); break; }
      const cls = (typeof x.className === 'string' ? x.className : (x.getAttribute('class') || '')).trim().split(/\s+/).filter(Boolean).slice(0, 2);
      if (cls.length) s += '.' + cls.map((c) => CSS.escape(c)).join('.');
      const p = x.parentElement;
      if (p && x !== document.body) {
        const same = Array.prototype.filter.call(p.children, (c) => c.localName === x.localName);
        if (same.length > 1) s += ':nth-of-type(' + (same.indexOf(x) + 1) + ')';
      }
      parts.unshift(s);
      if (x === document.body) break;
    }
    return parts.join(' > ');
  }
  // the same selector without places: one kind of element (a row, a cell of a column, a chip)
  function kindOf(e) {
    const parts = [];
    for (let x = e; x && x.nodeType === 1 && parts.length < 4; x = x.parentElement) {
      const cls = (typeof x.className === 'string' ? x.className : (x.getAttribute('class') || '')).trim().split(/\s+/).filter(Boolean).slice(0, 2);
      parts.unshift(x.localName + (cls.length ? '.' + cls.join('.') : ''));
      if (x === document.body) break;
    }
    return parts.join('>');
  }
  const visible = (e) => (typeof e.checkVisibility === 'function' ? e.checkVisibility({ opacityProperty: true, visibilityProperty: true }) : true);
  const tf = (cs) => cs.transform + '|' + cs.translate + '|' + cs.scale + '|' + cs.rotate;

  // The elements in and near the window (a band of arg.band window heights above and below), with their boxes
  // and what they look like with the mouse away.
  function collect() {
    const vh = innerHeight;
    const els = [];
    const walk = (p) => {
      for (const c of p.children) {
        if (SKIP.has(c.localName) || c.hasAttribute('data-sm-stable-outline')) continue;
        const r = c.getBoundingClientRect();
        const rendered = c.getClientRects().length > 0;
        if (rendered && r.bottom >= -arg.band * vh && r.top <= (1 + arg.band) * vh) {
          if (els.length < arg.maxElements) els.push(c);
        }
        if (!leaf(c) && els.length < arg.maxElements) walk(c);
      }
    };
    walk(document.body);
    return els;
  }
  function boxes(els) {
    const out = new Float64Array(els.length * 4);
    for (let i = 0; i < els.length; i++) {
      const e = els[i];
      if (!e.isConnected) { out[i * 4] = NaN; continue; }
      const r = e.getBoundingClientRect();
      out[i * 4] = r.left; out[i * 4 + 1] = r.top; out[i * 4 + 2] = r.width; out[i * 4 + 3] = r.height;
    }
    return out;
  }
  function restState(els) {
    return els.map((e) => {
      const cs = getComputedStyle(e);
      return { tf: tf(cs), vis: visible(e), pos: cs.position, pe: cs.pointerEvents, cursor: cs.cursor };
    });
  }
  // What scrolls the page: the document, or a box that fills most of the window when the document does not
  // scroll (Pebble's .scroller).
  const de = document.scrollingElement || document.documentElement;
  function findScroller() {
    if (de.scrollHeight > de.clientHeight + 1) return de;
    let best = de, area = 0.5 * innerWidth * innerHeight;
    for (const e of document.body.querySelectorAll('*')) {
      if (e.scrollHeight <= e.clientHeight + 1) continue;
      const cs = getComputedStyle(e);
      if (cs.overflowY !== 'auto' && cs.overflowY !== 'scroll') continue;
      const r = e.getBoundingClientRect();
      if (r.width * r.height > area) { best = e; area = r.width * r.height; }
    }
    return best;
  }
  const S = H.scroller = findScroller();
  const scrollTo = (top, left) => (S === de ? window.scrollTo({ top, left, behavior: 'instant' }) : S.scrollTo({ top, left, behavior: 'instant' }));
  // (a scrollbar of the page that comes or goes changes clientWidth or clientHeight)
  const page = () => ({ sx: scrollX + (S === de ? 0 : S.scrollLeft), sy: scrollY + (S === de ? 0 : S.scrollTop), cw: S.clientWidth, ch: S.clientHeight, dw: document.documentElement.clientWidth, dh: document.documentElement.clientHeight });

  // Takes the boxes with the mouse away (the mouse is outside the window when this runs).
  H.rest = async function rest(a) {
    await frames(2);
    if (a && a.reset) { H.els = collect(); H.state = restState(H.els); H.index = new Map(H.els.map((e, i) => [e, i])); }
    if (H.mo) { H.mo.takeRecords(); H.fresh = new Set(); }
    const b = boxes(H.els);
    const p = page();
    let changed = 0;
    if (H.restBoxes && !(a && a.reset)) {
      for (let i = 0; i < H.els.length; i++) if (differs(H.restBoxes, b, i)) changed++;
      if (p.sx !== H.restPage.sx || p.sy !== H.restPage.sy || p.cw !== H.restPage.cw || p.dw !== H.restPage.dw) changed++;
    }
    H.restBoxes = b;
    H.restPage = p;
    return { changed, elements: H.els.length };
  };
  function differs(a, b, i) {
    const k = i * 4;
    if (Number.isNaN(a[k]) || Number.isNaN(b[k])) return false;
    return Math.abs(a[k] - b[k]) > tol || Math.abs(a[k + 1] - b[k + 1]) > tol || Math.abs(a[k + 2] - b[k + 2]) > tol || Math.abs(a[k + 3] - b[k + 3]) > tol;
  }

  // Scrolls the window to y and lists what to hover there: arg.grid points, the interactive elements and the
  // charts whose middle is in the window and not hidden under something else, each element once per page.
  H.group = async function group(a) {
    scrollTo(a.y, 0);
    await frames(2);
    await H.rest({ reset: true });
    const vw = document.documentElement.clientWidth, vh = document.documentElement.clientHeight;
    const targets = [];
    const inWindow = (x, y) => x >= 2 && y >= 2 && x <= vw - 2 && y <= vh - 2;
    const hits = (e, x, y) => { const t = document.elementFromPoint(x, y); return !!t && (t === e || e.contains(t)); };
    // a chart is hit when the point is on it or on what is drawn over it in the same box (uPlot's cursor layer)
    const onChart = (e, x, y) => { const t = document.elementFromPoint(x, y); return !!t && (t === e || e.contains(t) || (!!e.parentElement && e.parentElement.contains(t) && !t.contains(e))); };
    const perKind = new Map();
    const take = (e, kind, x, y, extra) => {
      const k = kind + ':' + kindOf(e) + (extra || '');
      const n = perKind.get(k) || 0;
      if (n >= arg.perKind) return false;
      perKind.set(k, n + 1);
      targets.push({ el: e, kind, x, y, focus: kind === 'element' && focusable(e) });
      return true;
    };
    const focusable = (e) => e.matches('a[href], button:not([disabled]), input:not([disabled]):not([type=hidden]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex="-1"])');
    const INTERACTIVE = 'a[href], button, input:not([type=hidden]), select, textarea, summary, label, [tabindex], [role=button], [role=tab], [role=link], [role=menuitem], [role=switch], [role=checkbox], [role=radio], [role=option], [role=slider], [title], [data-tip], [data-tooltip], [aria-describedby], tr, th';
    for (let i = 0; i < H.els.length; i++) {
      const e = H.els[i];
      if (H.tested.has(e)) continue;
      const s = H.state[i];
      if (!s.vis) continue;
      const chart = e.localName === 'svg' || e.localName === 'canvas' || e.classList.contains('maplibregl-map') || e.classList.contains('uplot');
      if (chart && e.localName === 'canvas' && e.parentElement && e.parentElement.closest('.uplot, .maplibregl-map')) continue; // the chart's own canvas
      const r = e.getBoundingClientRect();
      if (chart) {
        if (r.width < 80 || r.height < 40) { if (!(e.closest('button, a'))) continue; }
        else {
          const cx = r.left + r.width / 2, cy = r.top + r.height / 2;
          if (!inWindow(cx, cy)) continue;
          H.tested.add(e);
          const n = arg.chartPoints;
          for (const fy of [0.35, 0.7]) for (let j = 0; j < n; j++) {
            const x = r.left + r.width * (j + 0.5) / n, y = r.top + r.height * fy;
            if (inWindow(x, y) && onChart(e, x, y)) targets.push({ el: e, kind: 'chart', x, y, focus: false });
          }
          // the marks of an svg chart that react to the mouse (bars, points with a pointer cursor or a title)
          if (e.localName === 'svg') {
            let m = 0;
            for (const c of e.querySelectorAll('rect, circle, path, g, polyline, line')) {
              if (m >= arg.perKind * 4) break;
              const cs = getComputedStyle(c);
              if (cs.cursor !== 'pointer' && !c.querySelector(':scope > title') && !c.hasAttribute('data-tip') && !c.hasAttribute('tabindex')) continue;
              const q = c.getBoundingClientRect();
              const x = q.left + q.width / 2, y = q.top + q.height / 2;
              if (q.width < 1 || q.height < 1 || !inWindow(x, y) || !hits(c, x, y)) continue;
              targets.push({ el: c, kind: 'chart', x, y, focus: false });
              m++;
            }
          }
          continue;
        }
      }
      const pointerRoot = s.cursor === 'pointer' && !(e.parentElement && H.index.has(e.parentElement) && H.state[H.index.get(e.parentElement)].cursor === 'pointer');
      if (!pointerRoot && !e.matches(INTERACTIVE)) continue;
      const rects = e.getClientRects();
      const q = rects.length ? rects[0] : r;
      if (q.width < 1 || q.height < 1) continue;
      const x = q.left + q.width / 2, y = q.top + q.height / 2;
      if (!inWindow(x, y) || !hits(e, x, y)) continue;
      if (take(e, 'element', x, y)) H.tested.add(e);
    }
    // the grid: whatever is under each point
    const [cols, rows] = arg.grid;
    const pointed = new Set();
    for (let gy = 0; gy < rows; gy++) for (let gx = 0; gx < cols; gx++) {
      const x = vw * (gx + 0.5) / cols, y = vh * (gy + 0.5) / rows;
      const t = document.elementFromPoint(x, y);
      if (!t) continue;
      // a point on something already hovered (an element listed above, a chart) or on the same element as a
      // point before tells nothing new
      let known = false;
      for (let e = t; e && e !== document.body; e = e.parentElement) if (H.tested.has(e) || pointed.has(e)) { known = true; break; }
      if (known) continue;
      pointed.add(t);
      targets.push({ el: t, kind: 'point', x, y, focus: false });
    }
    if (targets.length > arg.maxTargets) targets.length = arg.maxTargets;
    H.targets = targets;
    return { y: page().sy, vw, vh, targets: targets.map((t, i) => ({ i, kind: t.kind, x: t.x, y: t.y, focus: t.focus })) };
  };

  // What moved since the mouse went away, leaving out the hovered element t (and what is in it) and overlays.
  // Elements whose content the hover changed (a readout, a tooltip's text): their own box may follow their
  // text; what is around them may not move.
  // (Collected in the observer's callback too: it runs before this file's next look and takes the records with it.
  // Cleared after each look and when the mouse is away, so what changed is what changed since then.)
  H.fresh = new Set();
  const note = (records) => {
    for (const m of records) {
      const e = m.type === 'characterData' ? m.target.parentElement : m.target;
      if (e) H.fresh.add(e);
    }
  };
  const mo = new MutationObserver(note);
  mo.observe(document.body, { subtree: true, childList: true, characterData: true });
  H.mo = mo;
  function rewritten() {
    note(mo.takeRecords());
    return H.fresh;
  }
  function moved(t) {
    const fresh = rewritten();
    // (only the element whose children or text changed: its other children, and what is around it, still count)
    const inFresh = (e) => fresh.has(e);
    const b = boxes(H.els);
    const p = page();
    const list = [];
    const overlay = new Map(); // element -> part of an overlay layer
    const turned = new Map(); // element -> under a transform that changed
    const isOverlay = (e) => {
      if (overlay.has(e)) return overlay.get(e);
      let v = false;
      const i = H.index.get(e);
      const s = i === undefined ? null : H.state[i];
      const cs = getComputedStyle(e);
      const out = (s ? s.pos : cs.position) === 'absolute' || (s ? s.pos : cs.position) === 'fixed' || cs.position === 'absolute' || cs.position === 'fixed';
      if (e.hasAttribute('data-hover-overlay') || e.getAttribute('role') === 'tooltip') v = true;
      else if (out && ((s && !s.vis) || cs.pointerEvents === 'none' || (s && s.pe === 'none'))) v = true;
      else if (e.parentElement && e.parentElement !== document.documentElement) v = isOverlay(e.parentElement);
      overlay.set(e, v);
      return v;
    };
    const isTurned = (e) => {
      if (turned.has(e)) return turned.get(e);
      let v = false;
      const i = H.index.get(e);
      if (i !== undefined && tf(getComputedStyle(e)) !== H.state[i].tf) v = true;
      else if (e.parentElement && e.parentElement !== document.documentElement) v = isTurned(e.parentElement);
      turned.set(e, v);
      return v;
    };
    const cand = new Set();
    for (let i = 0; i < H.els.length; i++) {
      if (!differs(H.restBoxes, b, i)) continue;
      const e = H.els[i];
      if (t && (e === t || t.contains(e))) continue;
      if (!H.state[i].vis || !visible(e)) continue;
      if (isOverlay(e) || isTurned(e) || (fresh.size && inFresh(e))) continue;
      cand.add(i);
    }
    for (const i of cand) {
      const e = H.els[i];
      // only the outermost of elements that moved together
      const pi = e.parentElement ? H.index.get(e.parentElement) : undefined;
      if (pi !== undefined && cand.has(pi)) {
        const k = i * 4, pk = pi * 4;
        const dx = b[k] - H.restBoxes[k], dy = b[k + 1] - H.restBoxes[k + 1];
        const pdx = b[pk] - H.restBoxes[pk], pdy = b[pk + 1] - H.restBoxes[pk + 1];
        if (Math.abs(dx - pdx) <= tol && Math.abs(dy - pdy) <= tol) continue;
      }
      const k = i * 4;
      const r = (v) => Math.round(v * 100) / 100;
      list.push({
        selector: sel(e),
        dx: r(b[k] - H.restBoxes[k]), dy: r(b[k + 1] - H.restBoxes[k + 1]),
        dw: r(b[k + 2] - H.restBoxes[k + 2]), dh: r(b[k + 3] - H.restBoxes[k + 3]),
        box: { x: r(b[k] + scrollX), y: r(b[k + 1] + scrollY), w: r(b[k + 2]), h: r(b[k + 3]) },
      });
    }
    const page2 = {
      scrolled: p.sx !== H.restPage.sx || p.sy !== H.restPage.sy,
      scrollbar: p.cw !== H.restPage.cw || p.ch !== H.restPage.ch || p.dw !== H.restPage.dw || p.dh !== H.restPage.dh,
    };
    return { count: list.length, moved: list.slice(0, arg.maxMoved), ...page2 };
  }

  // After the mouse went to target i: what moved. With focus, the target is focused (as from the keyboard),
  // measured and let go again instead.
  H.measure = async function measure(a) {
    const t0 = performance.now();
    const t = H.targets[a.i];
    if (!t) return { count: 0, moved: [], scrolled: false, scrollbar: false, gone: true };
    const keep = S === de ? [scrollY, scrollX] : [S.scrollTop, S.scrollLeft];
    if (a.focus) {
      try { t.el.focus({ preventScroll: true, focusVisible: true }); } catch (_) { /* not focusable */ }
      if (document.activeElement !== t.el) return { count: 0, moved: [], scrolled: false, scrollbar: false, gone: true };
    }
    let res = moved(t.el);
    // a chart that draws its readout in an animation frame asked for it during the mouse move, before this
    // one: one frame later it has drawn
    if (!res.count && (t.kind === 'chart' || t.kind === 'point') && !a.focus) { await frames(1); res = moved(t.el); }
    if (a.focus) { t.el.blur(); scrollTo(keep[0], keep[1]); }
    mo.takeRecords();
    H.fresh = new Set();
    res.target = sel(t.el);
    res.kind = a.focus ? 'focus' : t.kind;
    H.ms += performance.now() - t0;
    return res;
  };
  return { scrollHeight: S.scrollHeight, innerHeight: S.clientHeight };
}

// Removes what hoverStart put in the page.
function hoverStop() {
  const H = window.__smHover;
  if (H && H.mo) H.mo.disconnect();
  if (H && H.sheet) document.adoptedStyleSheets = document.adoptedStyleSheets.filter((x) => x !== H.sheet);
  const ms = window.__smHover ? window.__smHover.ms : 0;
  delete window.__smHover;
  return { ms };
}

// Node side: runs the hover checks on a ready page. Returns
// { targets, points, charts, focused, groups, ms, count, findings: [{ n, kind, target, x, y, moved: [...], count,
//   scrolled, scrollbar }] }.
async function hoverRun(page, opts) {
  const t0 = Date.now();
  const arg = {
    tol: opts.tol, grid: opts.grid, perKind: opts.perKind, maxTargets: opts.maxTargets, chartPoints: opts.chartPoints,
    band: 0.5, maxElements: opts.maxElements, maxMoved: 8,
  };
  const away = async () => { await page.mouse.move(-10, -10); };
  await away();
  const m = await page.evaluate(hoverStart, arg);
  const out = { targets: 0, points: 0, charts: 0, focused: 0, groups: 0, ms: 0, count: 0, noise: 0, findings: [] };
  const seen = new Set();
  const screens = Math.max(1, Math.min(opts.maxScreens, Math.ceil(m.scrollHeight / Math.max(1, m.innerHeight * 0.9))));
  for (let g = 0; g < screens; g++) {
    const y = Math.round(g * m.innerHeight * 0.9);
    if (g > 0 && y >= m.scrollHeight - m.innerHeight * 0.1) break;
    await away();
    const grp = await page.evaluate((a) => window.__smHover.group(a), { y });
    out.groups++;
    for (const t of grp.targets) {
      if (Date.now() - t0 > opts.budgetMs) { out.truncated = true; break; }
      if (t.kind === 'point') out.points++; else if (t.kind === 'chart') out.charts++; else out.targets++;
      const tries = t.focus ? [false, true] : [false];
      for (const focus of tries) {
        if (focus) out.focused++;
        // Straight from the last target to this one: nothing moved is the usual answer. When something moved,
        // the mouse goes away and the page must be back where it was, and the same hover must move it again
        // (so a page that changed by itself is not taken for a hover).
        let res = null, hits = 0;
        for (let attempt = 0; attempt < 4 && hits < 2; attempt++) {
          if (!focus) await page.mouse.move(t.x, t.y);
          const r = await page.evaluate((a) => window.__smHover.measure(a), { i: t.i, focus });
          if (!(r.count || r.scrolled || r.scrollbar)) break;
          await away();
          const back = await page.evaluate(() => window.__smHover.rest());
          if (back.changed) { out.noise++; hits = 0; continue; } // the page changed by itself: look again
          hits++;
          res = r;
        }
        if (hits < 2) res = null;
        if (!res || !(res.count || res.scrolled || res.scrollbar)) continue;
        const key = res.kind + '|' + res.target.replace(/:nth-of-type\(\d+\)/g, '') + '|' + res.moved.map((x) => x.selector.replace(/:nth-of-type\(\d+\)/g, '')).slice(0, 2).join(',');
        out.count++;
        if (seen.has(key) || out.findings.length >= opts.maxFindings) continue;
        seen.add(key);
        out.findings.push({ n: out.findings.length + 1, kind: res.kind, target: res.target, x: Math.round(t.x), y: Math.round(t.y + grp.y), scrolled: res.scrolled, scrollbar: res.scrollbar, count: res.count, moved: res.moved });
      }
    }
    if (out.truncated) break;
  }
  const done = await page.evaluate(hoverStop);
  await away();
  out.ms = Date.now() - t0;
  out.pageMs = Math.round(done.ms);
  return out;
}

module.exports = { hoverStart, hoverStop, hoverRun };
