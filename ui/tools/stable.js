// Stability checks of ui/tools/check.js (--stable): does a page stay still while live updates arrive?
//
// Every function here runs in the page (Playwright page.addInitScript / page.evaluate), so each one
// brings everything it needs. stableInit runs before any page script and watches from the first paint:
//
//   layout shift   a PerformanceObserver for 'layout-shift' (the CLS entries): the sum of the values without
//                  recent input after the page said it was ready, and the moved nodes of each entry;
//   moved blocks   the layout position (offsetTop up the offsetParent chain, so transforms and spinners do not
//                  count) of every block-level element, sampled every 250 ms after ready: a block whose top
//                  moves by more than 1 px without input, with what came before it (appeared, grew, removed);
//   flicker        at each animation frame after a DOM change (the frames that get painted), the set of visible
//                  elements and of placeholder texts ("—", "Loading…", "No drives"): everything that appears or
//                  goes away is counted, and anything that does so more than once (gone and back, or shown and
//                  gone) toggles;
//   before ready   the shift score between the first paint and ready, and how long that took (not a failure);
//   updates        the event stream events and /api requests the page received during the watch.
//
// Elements inside svg, canvas and map widgets (MapLibre) are left out: charts and maps redraw by design.
// See ui/tools/README.md ("Stability checks").

'use strict';

// Runs before any page script, in every frame. arg: { pageId }.
function stableInit(arg) {
  if (window.top !== window || window.__smStable) return;
  const S = window.__smStable = {
    pageId: arg && arg.pageId, readyAt: null, fcp: null, shifts: [], lastInput: -1e9,
    events: {}, eventsBefore: 0, started: false,
  };
  const now = () => performance.now();
  for (const t of ['pointerdown', 'keydown', 'wheel', 'touchstart', 'mousedown']) {
    addEventListener(t, () => { S.lastInput = now(); }, { capture: true, passive: true });
  }
  try {
    new PerformanceObserver((list) => {
      for (const e of list.getEntries()) {
        S.shifts.push({
          t: e.startTime, v: e.value, input: e.hadRecentInput,
          // (a rect is empty when the node was or is out of the window: then its move is not known)
          sources: (e.sources || []).map((s) => ({
            node: s.node || null,
            dy: s.currentRect.height > 0 && s.previousRect.height > 0 ? s.currentRect.top - s.previousRect.top : null,
          })),
        });
      }
    }).observe({ type: 'layout-shift', buffered: true });
  } catch (_) { S.noShiftApi = true; }
  try {
    new PerformanceObserver((list) => {
      for (const e of list.getEntries()) if (e.name === 'first-contentful-paint' || (e.name === 'first-paint' && S.fcp == null)) S.fcp = e.startTime;
    }).observe({ type: 'paint', buffered: true });
  } catch (_) { /* no paint timing */ }

  // events of every EventSource the page opens, counted by type
  const ES = window.EventSource;
  if (typeof ES === 'function') {
    const count = (type) => { if (S.started) S.events[type] = (S.events[type] || 0) + 1; else S.eventsBefore++; };
    const Wrapped = function EventSource(url, init) {
      const es = new ES(url, init);
      const add = es.addEventListener.bind(es);
      const seen = new Set();
      const hook = (type) => { if (seen.has(type)) return; seen.add(type); add(type, () => count(type)); };
      es.addEventListener = function (type, ...rest) { hook(String(type)); return add(type, ...rest); };
      for (const t of ['message', 'now', 'rows', 'fleet', 'prefs', 'notice']) hook(t);
      return es;
    };
    Wrapped.prototype = ES.prototype;
    for (const k of ['CONNECTING', 'OPEN', 'CLOSED']) Wrapped[k] = ES[k];
    window.EventSource = Wrapped;
  }

  // ---- helpers shared by the watchers
  const SKIP_TAGS = new Set(['script', 'style', 'link', 'meta', 'template', 'noscript', 'title', 'head', 'option', 'br', 'wbr', 'source', 'track', 'param']);
  const ignoredRoot = (e) => e.localName === 'svg' || e.localName === 'canvas' || e.classList.contains('maplibregl-map')
    || e.hasAttribute('data-sm-pixel-outline') || e.hasAttribute('data-sm-stable-outline') || e.hasAttribute('data-stable-ignore');
  // Walks body top-down and gives each element its key: its path from body (tag, id or first class, place among
  // its kind), built from the parent's key so a long list costs one pass. visit(e, key) returns false to skip
  // the element's children.
  function walk(visit) {
    const body = document.body;
    if (!body) return;
    const rec = (parent, pkey) => {
      const kids = parent.children;
      let totals = null;
      if (kids.length > 1) { totals = new Map(); for (const c of kids) totals.set(c.localName, (totals.get(c.localName) || 0) + 1); }
      const seen = new Map();
      for (const c of kids) {
        const t = c.localName;
        if (SKIP_TAGS.has(t)) continue;
        const i = (seen.get(t) || 0) + 1;
        seen.set(t, i);
        let k;
        if (c.id) k = t + '#' + CSS.escape(c.id);
        else {
          const cls = (c.getAttribute('class') || '').trim().split(/\s+/)[0];
          k = pkey + ' > ' + t + (cls ? '.' + CSS.escape(cls) : '') + (totals && totals.get(t) > 1 ? ':nth-of-type(' + i + ')' : '');
        }
        if (visit(c, k) !== false) rec(c, k);
      }
    };
    rec(body, 'body');
  }
  // a short readable selector, as check.js shows them
  function sel(e) {
    if (!e) return '?';
    if (e.nodeType !== 1) e = e.parentElement;
    if (!e) return '?';
    const parts = [];
    for (let x = e; x && x.nodeType === 1 && parts.length < 4; x = x.parentElement) {
      let s = x.localName;
      if (x.id) { parts.unshift(s + '#' + CSS.escape(x.id)); break; }
      const cls = (x.getAttribute('class') || '').trim().split(/\s+/).filter(Boolean).slice(0, 2);
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
  S.sel = sel;
  const short = (k) => k.split(' > ').slice(-4).join(' > ');
  const PLACEHOLDER = /^(?:[—–\-−…?·.]+|loading\b.*|loading…|n\/a|unknown|waiting\b.*|fetching\b.*|updating\b.*|refreshing\b.*|no\s.*|nothing\b.*|none\b.*|not\s+(?:yet|available|known)\b.*|empty\b.*)$/i;
  // What is watched: the window and three more window heights below it (what you see and scroll to soon), so a
  // page of tens of thousands of rows costs no more than its first screens. 'in', 'out' (outside the band:
  // its children are skipped too) or 'hidden'.
  const where = (e) => {
    if (typeof e.checkVisibility === 'function' && !e.checkVisibility({ opacityProperty: true, visibilityProperty: true })) return 'hidden';
    const r = e.getBoundingClientRect();
    if (!(r.width > 0 && r.height > 0)) return 'hidden';
    const vh = innerHeight;
    return r.bottom < -vh || r.top > 4 * vh ? 'out' : 'in';
  };

  // all visible elements and placeholder texts, by key (for flicker)
  function snapshot() {
    const out = new Map();
    walk((e, k) => {
      const w = where(e);
      if (w !== 'in') return w === 'hidden' && getComputedStyle(e).display === 'contents';
      out.set(k, e);
      if (ignoredRoot(e)) return false;
      // placeholder text in this element's own text nodes
      let text = '';
      for (const n of e.childNodes) if (n.nodeType === 3) text += n.nodeValue;
      text = text.trim().replace(/\s+/g, ' ');
      if (text && PLACEHOLDER.test(text)) out.set(k + ' ::placeholder', e);
      return true;
    });
    return out;
  }

  // block-level elements and their layout tops (for moved blocks)
  function layoutTop(e) {
    let y = 0;
    for (let x = e; x; x = x.offsetParent) y += x.offsetTop;
    return y;
  }
  const BLOCKISH = /^(block|flex|grid|table|list-item|flow-root|table-row|table-row-group|table-header-group|table-footer-group|inline-block|inline-flex|inline-grid)$/;
  function blocks(limit) {
    const out = new Map();
    walk((e, k) => {
      if (out.size >= limit || ignoredRoot(e) || !(e instanceof HTMLElement)) return false;
      const cs = getComputedStyle(e);
      if (cs.display === 'none') return false;
      const r = e.getBoundingClientRect();
      if (r.height > 0 && (r.bottom < -innerHeight || r.top > 4 * innerHeight)) return false;
      const w = e.offsetWidth, h = e.offsetHeight;
      if (w >= 40 && h >= 8 && BLOCKISH.test(cs.display) && cs.visibility === 'visible') {
        out.set(k, { e, top: layoutTop(e), h, fixed: cs.position === 'fixed' || cs.position === 'sticky' });
      }
      return true;
    });
    return out;
  }

  // ---- the watch after ready
  function start() {
    if (S.started) return;
    S.started = true;
    S.readyAt = now();
    // the frame that paints the ready page is part of loading it: shifts count from the frame after it
    S.countFrom = Infinity;
    requestAnimationFrame(() => setTimeout(() => { S.countFrom = now(); }, 0));
    const LIMIT = 3000;
    const toggles = new Map(); // key -> { sel, times, first, at: [ms] }
    const moves = new Map(); // key -> { sel, times, max, at, cause }
    const late = new Map(); // key -> { sel, h, at, pushed }
    // The watch may not slow the page down: a snapshot or sample that took c ms waits at least 3c ms before the
    // next one (a big page is then looked at less often; `cost` in the result says how often).
    S.watch = { toggles, moves, late, frames: 0, snapMs: 0, snapMaxMs: 0, samples: 0, sampleMs: 0, sampleMaxMs: 0 };
    let nextSnap = 0;
    let prevSnap = snapshot();
    let dirty = false;
    const mo = new MutationObserver(() => { dirty = true; });
    mo.observe(document, { subtree: true, childList: true, attributes: true, characterData: true, attributeFilter: ['hidden', 'class', 'style', 'open', 'aria-hidden', 'aria-busy'] });
    S.mo = mo;
    const frame = () => {
      if (S.stopped) return;
      if (dirty && now() >= nextSnap) {
        dirty = false;
        const t0 = now();
        const snap = snapshot();
        S.watch.frames++;
        const t = Math.round(now() - S.readyAt);
        const note = (k, e, what) => {
          let v = toggles.get(k);
          // (shown by its key: an element that went away is no longer in the page)
          if (!v) { v = { sel: short(k.replace(/ ::placeholder$/, '')) + (k.endsWith('::placeholder') ? ' (placeholder text)' : ''), times: 0, first: what, at: [], text: null }; toggles.set(k, v); }
          v.times++;
          if (v.at.length < 8) v.at.push(t);
          if (k.endsWith('::placeholder') && !v.text) v.text = (e.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 40);
        };
        for (const [k, e] of snap) if (!prevSnap.has(k)) note(k, e, 'appeared');
        for (const [k, e] of prevSnap) if (!snap.has(k)) note(k, e, 'went away');
        prevSnap = snap;
        const c = now() - t0;
        S.watch.snapMs += c;
        S.watch.snapMaxMs = Math.max(S.watch.snapMaxMs, c);
        nextSnap = now() + (c > 8 ? 3 * c : 0);
      }
      requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);

    let prev = blocks(LIMIT);
    const sample = () => {
      if (S.stopped) return;
      const t0 = now();
      const cur = blocks(LIMIT);
      S.watch.samples++;
      const t = Math.round(now() - S.readyAt);
      const recentInput = now() - S.lastInput < 500;
      const moved = [];
      for (const [k, b] of cur) {
        const p = prev.get(k);
        if (!p || b.fixed || p.fixed) continue;
        const dy = b.top - p.top;
        if (Math.abs(dy) > 1 && !recentInput) moved.push({ k, b, dy });
      }
      if (moved.length) {
        const movedKeys = new Map(moved.map((m) => [m.k, m]));
        let listed = 0;
        // what changed before them: new blocks, blocks that grew or shrank, blocks that went away
        const changed = [];
        for (const [k, b] of cur) {
          const p = prev.get(k);
          if (!p) changed.push({ k, what: 'appeared', h: b.h, bottom: b.top + b.h, old: false, e: b.e });
          else if (Math.abs(b.h - p.h) > 1) changed.push({ k, what: b.h > p.h ? 'grew' : 'shrank', h: b.h - p.h, bottom: b.top + b.h, old: false, e: b.e });
        }
        // (a block that went away is compared where it was, with the old layout)
        for (const [k, p] of prev) if (!cur.has(k)) changed.push({ k, what: 'went away', h: p.h, bottom: p.top + p.h, old: true, e: p.e });
        for (const m of moved) {
          // outermost only: skip a block whose parent block moved by the same amount
          const parentKey = m.k.slice(0, m.k.lastIndexOf(' > '));
          let skip = false;
          for (let pk = parentKey; pk; pk = pk.slice(0, Math.max(0, pk.lastIndexOf(' > ')))) {
            const pm = movedKeys.get(pk);
            if (pm && Math.abs(pm.dy - m.dy) <= 1) { skip = true; break; }
            if (pk.lastIndexOf(' > ') < 0) break;
          }
          if (skip || ++listed > 200) continue;
          // the cause: the last changed block that ends above it and is not its ancestor
          let cause = null;
          for (const c of changed) {
            if (m.k.startsWith(c.k + ' > ') || c.k.startsWith(m.k + ' > ') || c.k === m.k) continue;
            const top = c.old ? m.b.top - m.dy : m.b.top;
            if (c.bottom <= top + 1 && (!cause || c.bottom >= cause.bottom)) cause = c;
          }
          let v = moves.get(m.k);
          if (!v) { v = { sel: sel(m.b.e), times: 0, max: 0, at: [], cause: null, e: m.b.e }; moves.set(m.k, v); }
          v.e = m.b.e;
          v.times++;
          if (Math.abs(m.dy) > Math.abs(v.max)) v.max = Math.round(m.dy * 10) / 10;
          if (v.at.length < 8) v.at.push(t);
          if (cause && !v.cause) v.cause = `${cause.what === 'appeared' ? `${sel(cause.e)} appeared (${Math.round(cause.h)} px)` : cause.what === 'went away' ? `${sel(cause.e)} went away (${Math.round(cause.h)} px)` : `${sel(cause.e)} ${cause.what} by ${Math.abs(Math.round(cause.h))} px`}`;
          if (cause && cause.what === 'appeared') {
            let l = late.get(cause.k);
            if (!l) { l = { sel: sel(cause.e), h: Math.round(cause.h), at: t, pushed: 0 }; late.set(cause.k, l); }
            l.pushed++;
          }
        }
      }
      prev = cur;
      const c = now() - t0;
      S.watch.sampleMs += c;
      S.watch.sampleMaxMs = Math.max(S.watch.sampleMaxMs, c);
      S.sampler = setTimeout(sample, Math.max(250, 4 * c));
    };
    S.sampler = setTimeout(sample, 250);
  }
  S.start = start;

  // start at ready: data-ready set to this page's id
  const check = () => {
    const de = document.documentElement;
    const v = de && de.dataset ? de.dataset.ready : null;
    if (v && v === S.pageId) { start(); return true; }
    return false;
  };
  // (the init script runs before <html> exists, so the document itself is watched)
  if (!check()) {
    const ro = new MutationObserver(() => { if (check()) ro.disconnect(); });
    ro.observe(document, { subtree: true, attributes: true, attributeFilter: ['data-ready'] });
  }
}

// The results of the watch so far (and stops it). arg: { maxList }.
function stableCollect(arg) {
  const S = window.__smStable;
  if (!S) return { error: 'the stability watch was not installed' };
  if (!S.started) S.start(); // ready came before the watch could see it: start now (late)
  S.stopped = true;
  clearTimeout(S.sampler);
  if (S.mo) S.mo.disconnect();
  const max = (arg && arg.maxList) || 100;
  const W = S.watch;
  const box = (e) => {
    if (!e || !e.isConnected) return null;
    const r = e.getBoundingClientRect();
    return { x: Math.round(r.left + scrollX), y: Math.round(r.top + scrollY), w: Math.round(r.width), h: Math.round(r.height) };
  };
  // shift entries: after ready (counts) and from the first paint to ready
  let after = 0, before = 0, withInput = 0;
  const sources = new Map();
  for (const s of S.shifts) {
    if (s.input) { withInput += s.v; continue; }
    if (s.t >= S.countFrom) {
      after += s.v;
      for (const src of s.sources) {
        if (!src.node || (src.dy != null && Math.abs(src.dy) < 0.5)) continue;
        const k = S.sel(src.node);
        const v = sources.get(k) || { selector: k, times: 0, px: null };
        v.times++;
        if (src.dy != null && (v.px == null || Math.abs(src.dy) > Math.abs(v.px))) v.px = Math.round(src.dy);
        sources.set(k, v);
      }
    } else if (S.fcp == null || s.t >= S.fcp) before += s.v;
  }
  // toggling: more than one appearance or disappearance; outermost only (a parent that toggled as often covers it)
  const tog = [...W.toggles.entries()].filter(([, v]) => v.times > 1);
  const togKeys = new Map(tog);
  const toggling = [];
  for (const [k, v] of tog) {
    const base = k.endsWith(' ::placeholder') ? k.slice(0, -14) : k;
    let covered = false;
    if (base !== k && togKeys.has(base) && togKeys.get(base).times >= v.times) covered = true;
    for (let pk = base.slice(0, Math.max(0, base.lastIndexOf(' > '))); !covered && pk; pk = pk.slice(0, Math.max(0, pk.lastIndexOf(' > ')))) {
      const pv = togKeys.get(pk);
      if (pv && pv.times >= v.times) covered = true;
      if (pk.lastIndexOf(' > ') < 0) break;
    }
    if (!covered) toggling.push({ selector: v.sel, times: v.times, first: v.first, atMs: v.at, ...(v.text ? { text: v.text } : {}), _k: k });
  }
  toggling.sort((a, b) => b.times - a.times);
  const moved = [...W.moves.values()].map((v) => ({ selector: v.sel, px: v.max, times: v.times, atMs: v.at, cause: v.cause, box: box(v.e) }));
  moved.sort((a, b) => Math.abs(b.px) * b.times - Math.abs(a.px) * a.times);
  const late = [...W.late.values()].map((l) => ({ selector: l.sel, h: l.h, atMs: l.at, pushed: l.pushed }));
  // boxes of toggling elements for the outlined screenshot (when they are on the page now)
  const boxes = [];
  for (const t of toggling.slice(0, 40)) {
    let e = null;
    try { e = document.querySelector(t._k.replace(/ ::placeholder$/, '')); } catch (_) { e = null; }
    const b = box(e);
    if (b && b.w && b.h) boxes.push({ kind: 'flicker', ...b, selector: t.selector });
    delete t._k;
  }
  for (const t of toggling.slice(40)) delete t._k;
  for (const m of moved.slice(0, 40)) if (m.box && m.box.w && m.box.h) boxes.push({ kind: 'moved', ...m.box, selector: m.selector });
  const events = { ...S.events };
  const total = Object.entries(events).filter(([k]) => k !== 'message').reduce((a, [, n]) => a + n, 0) + (events.message || 0);
  return {
    watchedMs: Math.round(performance.now() - S.readyAt),
    readyMs: Math.round(S.readyAt), firstPaintMs: S.fcp == null ? null : Math.round(S.fcp),
    shift: Math.round(after * 10000) / 10000, shiftBeforeReady: Math.round(before * 10000) / 10000,
    shiftWithInput: Math.round(withInput * 10000) / 10000, noShiftApi: !!S.noShiftApi,
    shiftSources: [...sources.values()].sort((a, b) => b.times - a.times).slice(0, max),
    moved: moved.slice(0, max), movedCount: moved.length,
    toggling: toggling.slice(0, max), togglingCount: toggling.length,
    late: late.slice(0, max),
    events, eventCount: total,
    cost: { frames: W.frames, snapMs: Math.round(W.snapMs), snapMaxMs: Math.round(W.snapMaxMs), samples: W.samples, sampleMs: Math.round(W.sampleMs), sampleMaxMs: Math.round(W.sampleMaxMs) },
    boxes,
  };
}

// Outlines boxes ({ n, kind, x, y, w, h }) for the "after" screenshot; stableOutlineRemove takes them away.
function stableOutline(boxes) {
  const de = document.documentElement;
  const st = (e, props) => { for (const [k, v] of Object.entries(props)) e.style.setProperty(k, v, 'important'); };
  const root = document.createElement('div');
  root.setAttribute('data-sm-stable-outline', '');
  st(root, { position: 'absolute', left: '0', top: '0', width: '0', height: '0', 'z-index': '2147483647', 'pointer-events': 'none' });
  for (const b of boxes) {
    const c = b.kind === 'flicker' ? '#7a00ff' : '#e0004d';
    const d = document.createElement('div');
    st(d, { position: 'absolute', left: (b.x - 2) + 'px', top: (b.y - 2) + 'px', width: (b.w + 4) + 'px', height: (b.h + 4) + 'px', outline: '2px solid ' + c, 'box-sizing': 'border-box' });
    const tag = document.createElement('span');
    tag.textContent = b.n + ' ' + b.kind;
    st(tag, { position: 'absolute', left: '-2px', top: '-17px', font: '11px/15px monospace', color: '#fff', background: c, padding: '0 3px', 'white-space': 'nowrap' });
    d.append(tag);
    root.append(d);
  }
  de.append(root);
}

function stableOutlineRemove() {
  for (const e of document.querySelectorAll('[data-sm-stable-outline]')) e.remove();
}

module.exports = { stableInit, stableCollect, stableOutline, stableOutlineRemove };
