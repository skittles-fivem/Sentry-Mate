#!/usr/bin/env node
// Jump check: does a page stay where it is while it loads and while it is scrolled, on a phone and on a desktop?
//
//   node ui/tools/jumps.js [--packs instrument,horizon,...|all] [--pages car,drives,...|all]
//        [--devices iphone-15,iphone-13-mini,phone-chromium,desktop|all] [--max-score N] [--latency-ms N]
//        [--step-ms N] [--server-args "..."] [--jobs N] [--json] [--verbose]
//
// For each pack it starts the dev API server (live values every 2 s, a `rows` event every 10 s, every /api answer
// delayed --latency-ms as on a phone's network) and opens every page in fresh windows of each device: iPhone 15 and
// iPhone 13 mini in WebKit (Playwright's device descriptors: Safari's engine, a touch phone's viewport), a 390x844
// phone at scale 3 in Chromium's mobile emulation, and a 1440x900 desktop in Chromium. In WebKit scroll anchoring is
// off (overflow-anchor: none), as in Safari on iOS: whatever changes size above what is on screen moves what is on
// screen (Chrome keeps its own anchoring, as on Android and desktops). From before the page's first script to the end
// it watches, each animation frame:
//
//   blocks    every block of the page (its path from <body>, so a block drawn again in its place is the same block)
//             and where it is on screen, and its top and height in the box that scrolls (the window, or the page's
//             own scrolling box);
//   scroll    where that box is scrolled, and how much of that the check scrolled itself.
//
// After the page says it is ready it waits 1.5 s (live updates arrive), then scrolls down a screen at a time
// (--step-ms between steps, so the charts, maps and lists drawn when they come near are drawn), to the end, and
// back up. Then it opens another page inside the app and comes back (the kept answers are drawn first, then the
// fresh ones) and scrolls down again. A page fails when:
//
//   score     the jump score is over --max-score (0.02): for each frame, the part of the screen whose blocks moved on
//             screen (less what the check scrolled) times how far they moved, as a share of the screen's height, summed
//             over the whole visit (the same measure as the layout shift score); in Chromium also the browser's own
//             layout shift score (PerformanceObserver 'layout-shift', every entry without recent input);
//   above     a block above the screen changed its height while the page was scrolled (it moves what is on screen
//             unless something puts it back);
//   scrolled  the box scrolled by itself, by more than 1 px, and what is on screen moved with it (a page that got
//             shorter, a part emptied and drawn again, a scrollIntoView). A scroll that keeps what is on screen in
//             place (the page's own scroll anchoring) is not a jump.
//
// Blocks that show nothing of their own (no text, background, border, picture or control), blocks as tall as the
// screen that cover it before and after, and what moves within 500 ms of the check's own taps do not count.
// Exit code 0 only when every page passes on every device. Made-up data only (the dev server's). Plain Node plus
// the installed Playwright; WebKit needs `npx playwright install webkit` (and its system libraries) once: without
// it the WebKit devices are skipped with a note. See ui/tools/README.md ("The jump check").
'use strict';

const path = require('path');
const http = require('http');
const { startServer, killChildren, TILE_HOSTS, BLANK_TILE, ROUTES } = require('./check');

const UI_DIR = path.dirname(__dirname);
const PACKS = ['instrument', 'horizon', 'manager', 'console', 'pebble', 'logbook'];
const DEVICES = {
  'iphone-15': { engine: 'webkit', descriptor: 'iPhone 15' },
  'iphone-13-mini': { engine: 'webkit', descriptor: 'iPhone 13 Mini' },
  'phone-chromium': { engine: 'chromium', context: { viewport: { width: 390, height: 844 }, screen: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true } },
  desktop: { engine: 'chromium', context: { viewport: { width: 1440, height: 900 }, screen: { width: 1440, height: 900 }, deviceScaleFactor: 1 } },
};
const DEFAULTS = { maxScore: 0.02, latencyMs: 60, stepMs: 300, settleMs: 1500, readyTimeoutMs: 20000, jobs: 3 };
// a block moves when its top changes by more than this (CSS px); a height above the screen changes by more than this
const MOVE_PX = 1;
const LIST = 12; // moves listed per page and device in the report

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function within(ms, promise, what) {
  let t;
  const limit = new Promise((_, reject) => { t = setTimeout(() => reject(new Error(`${what} took more than ${Math.round(ms / 1000)} s`)), ms); });
  return Promise.race([promise, limit]).finally(() => clearTimeout(t));
}

// ------------------------------------------------------------------ in the page

// Runs before any page script. Watches the blocks and the scroll position every animation frame (see the top of
// this file) and keeps the moves, until __smJumps.take().
function jumpsInit(arg) {
  if (window.top !== window || window.__smJumps) return;
  const J = window.__smJumps = {
    phase: 'load', score: 0, cls: 0, clsByPhase: {}, scoreByPhase: {}, moves: [], above: [], selfScroll: [], noCls: false,
    expected: null, samples: 0, maxSampleMs: 0, blocks: 0, inputAt: -1e9,
  };
  // as the layout shift score: what moves within 500 ms of an input (here: the check's own taps on a link) is the
  // person's doing
  const INPUT_MS = 500;
  // Safari on iOS has no scroll anchoring; with it off here too, a block above the screen that changes its height
  // moves what is on screen, in every engine. (A constructed sheet: the pages' CSP allows no inline <style>.)
  const anchorOff = () => {
    try {
      const sheet = new CSSStyleSheet();
      sheet.replaceSync('*, *::before, *::after { overflow-anchor: none !important; }');
      document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
    } catch (_) { /* no constructed sheets: the context's own style tag (bypassCSP) does it */ }
  };
  // (in WebKit only: Chrome on Android and on a desktop has scroll anchoring, and keeps it)
  if (arg && arg.anchorOff) { if (document.documentElement) anchorOff(); else document.addEventListener('readystatechange', anchorOff, { once: true }); }
  if (!(globalThis.PerformanceObserver?.supportedEntryTypes || []).includes('layout-shift')) J.noCls = true; // WebKit
  else try {
    new PerformanceObserver((list) => {
      for (const e of list.getEntries()) {
        if (e.hadRecentInput || e.startTime - J.inputAt < INPUT_MS) continue;
        J.cls += e.value;
        J.clsByPhase[J.phase] = (J.clsByPhase[J.phase] || 0) + e.value;
        if (e.value >= 0.001 && J.moves.length < 400) {
          for (const s of (e.sources || []).slice(0, 3)) {
            if (!s.node || s.node.nodeType !== 1) continue;
            J.moves.push({ kind: 'cls', phase: J.phase, t: Math.round(e.startTime), sel: sel(s.node), dy: Math.round(s.currentRect.top - s.previousRect.top), v: +e.value.toFixed(4) });
          }
        }
      }
    }).observe({ type: 'layout-shift', buffered: true });
  } catch (_) { J.noCls = true; }

  const MOVE_PX = 1; // as MOVE_PX in Node
  const SKIP = new Set(['script', 'style', 'link', 'meta', 'template', 'noscript', 'title', 'head', 'br', 'wbr', 'option', 'svg', 'canvas', 'img', 'video', 'picture', 'iframe', 'input', 'select', 'textarea', 'button']);
  const SCROLLS = new Set(['auto', 'scroll', 'overlay']);
  function sel(e) {
    const parts = [];
    for (let x = e; x && x.nodeType === 1 && x !== document.body && parts.length < 4; x = x.parentElement) {
      let s = x.localName;
      if (x.id) { parts.unshift(s + '#' + x.id); break; }
      const cls = (x.getAttribute('class') || '').trim().split(/\s+/).filter(Boolean).slice(0, 2);
      if (cls.length) s += '.' + cls.join('.');
      parts.unshift(s);
    }
    return parts.join(' > ');
  }
  // the box that scrolls: the window, else the tallest box that scrolls up and down (looked for again now and then)
  let box = null, boxAt = -1e9;
  function scroller(t) {
    const de = document.scrollingElement || document.documentElement;
    if (box && box !== de && box.isConnected && t - boxAt < 1000) return box;
    boxAt = t;
    if (de.scrollHeight > de.clientHeight + 1) { box = de; return box; }
    let best = null, h = 0;
    for (const e of document.querySelectorAll('body *')) {
      if (e.clientHeight > h && e.scrollHeight > e.clientHeight + 1 && SCROLLS.has(getComputedStyle(e).overflowY)) { h = e.clientHeight; best = e; }
    }
    box = best || de;
    return box;
  }
  J.box = () => scroller(performance.now());
  // the check scrolls with this: what it scrolled since the last frame is taken out of what moved on screen, and a
  // scroll the page makes by itself can be told apart
  J.cmd = 0; J.cmdBox = null;
  J.scrollTo = (y) => {
    const b = J.box();
    const before = b.scrollTop;
    b.scrollTo({ top: y, behavior: 'instant' });
    if (J.cmdBox !== b) { J.cmdBox = b; J.cmd = 0; }
    J.cmd += b.scrollTop - before;
    return { top: b.scrollTop, max: b.scrollHeight - b.clientHeight, height: b.clientHeight };
  };

  // every block: its path from <body> -> { top, h } in the scrolling box's content, where it was last frame
  let last = new Map();
  let lastView = null;
  const MAX_BLOCKS = 1500, MAX_DEPTH = 9;
  function sample(t) {
    const t0 = performance.now();
    const b = scroller(t);
    const de = document.scrollingElement || document.documentElement;
    const win = b === de;
    const br = win ? { top: 0, bottom: innerHeight, left: 0, right: innerWidth } : b.getBoundingClientRect();
    const st = b.scrollTop;
    const view = { top: Math.max(0, br.top), bottom: Math.min(innerHeight, br.bottom), w: innerWidth, h: innerHeight };
    const now = new Map();
    let n = 0;
    const visit = (parent, pkey, depth, fixedish) => {
      const kids = parent.children;
      const seen = new Map();
      for (const c of kids) {
        const tag = c.localName;
        if (SKIP.has(tag)) continue;
        const i = (seen.get(tag) || 0) + 1;
        seen.set(tag, i);
        if (n >= MAX_BLOCKS) return;
        const cls = (c.getAttribute('class') || '').trim().split(/\s+/)[0];
        const key = c.id ? tag + '#' + c.id : pkey + '>' + tag + (cls ? '.' + cls : '') + ':' + i;
        if (c.classList.contains('maplibregl-map') || c.hasAttribute('data-stable-ignore') || c.inert || c.hasAttribute('data-offstage')) continue;
        const r = c.getBoundingClientRect();
        if (!(r.width > 0 || r.height > 0)) { if (depth < MAX_DEPTH) visit(c, key, depth + 1, fixedish); continue; }
        const cs = getComputedStyle(c);
        if (cs.display === 'none' || cs.display === 'contents') { if (cs.display === 'contents' && depth < MAX_DEPTH) visit(c, key, depth + 1, fixedish); continue; }
        const fixed = fixedish || cs.position === 'fixed' || cs.position === 'sticky';
        // in the box's content: from its top, scrolled by st
        const top = r.top - br.top + st;
        const far = r.bottom < view.top - 2 * view.h || r.top > view.bottom + 2 * view.h; // its children are not looked at
        // (the box that scrolls and what holds it are not blocks in it: they do not scroll with its content)
        if (!fixed && r.height >= 8 && r.width >= 24 && (win || (b.contains(c) && c !== b))) {
          n++;
          now.set(key, { el: c, top, h: r.height, vtop: r.top, vbot: r.bottom, w: Math.min(r.right, view.w) - Math.max(r.left, 0) });
        }
        if (!far && depth < MAX_DEPTH) visit(c, key, depth + 1, fixed);
      }
    };
    if (document.body) visit(document.body, 'body', 0, false);
    J.blocks = Math.max(J.blocks, n);
    // what the check scrolled since the last frame
    const cmd = J.cmdBox === b ? J.cmd : 0;
    J.cmd = 0;
    let self = null;
    // what moved on screen since the last frame (blocks on screen then or now), less what the check scrolled: content
    // that moved in the page, or the page scrolled by itself (scroll anchoring of the page's own, which keeps what is
    // on screen in place, moves nothing)
    if (lastView && lastView.box === b) {
      // a scroll the check did not make (with the blocks that changed most this frame, as its likely cause)
      if (Math.abs(st - lastView.st - cmd) > MOVE_PX) {
        const ch = [];
        for (const [k, p] of now) { const q = last.get(k); if (q && Math.abs(p.h - q.h) > MOVE_PX) ch.push([Math.abs(p.h - q.h), `${sel(p.el)} ${Math.round(q.h)} -> ${Math.round(p.h)} px`]); }
        for (const [k, q] of last) if (!now.has(k) && q.h > 40) ch.push([q.h, `${k.split('>').slice(-3).join(' > ')} went away (${Math.round(q.h)} px)`]);
        ch.sort((x, y) => y[0] - x[0]);
        self = { phase: J.phase, t: Math.round(t), from: Math.round(lastView.st + cmd), to: Math.round(st), height: Math.round(b.scrollHeight), cause: ch.slice(0, 3).map((c) => c[1]), visible: false };
        if (J.selfScroll.length < 50) J.selfScroll.push(self);
      }
      const moved = [];
      // what changed this frame, as the cause of a move: blocks that appeared, went away or changed height
      let changes = null;
      const causeOf = (k, top) => {
        if (!changes) {
          changes = [];
          for (const [ck, p] of now) {
            const q = last.get(ck);
            if (!q) changes.push({ k: ck, top: p.top, el: p.el, how: `appeared (${Math.round(p.h)} px)` });
            else if (Math.abs(p.h - q.h) > MOVE_PX) changes.push({ k: ck, top: Math.min(p.top, q.top), el: p.el, how: `${p.h > q.h ? 'grew' : 'shrank'} ${Math.round(q.h)} -> ${Math.round(p.h)} px` });
          }
          for (const [ck, q] of last) if (!now.has(ck)) changes.push({ k: ck, top: q.top, el: q.el, how: `went away (${Math.round(q.h)} px)`, gone: true });
        }
        let best = null;
        for (const c of changes) {
          if (c.top > top + 1 || k.startsWith(c.k + '>') || c.k === k) continue; // below it, or holds it
          if (!best || c.k.length > best.k.length) best = c;
        }
        return best ? `${best.gone ? best.k.split('>').slice(-3).join(' > ') : sel(best.el)} ${best.how}` : null;
      };
      for (const [k, p] of now) {
        const q = last.get(k);
        if (!q) continue;
        const dy = p.vtop - q.vtop + cmd;
        const onNow = p.vbot > view.top && p.vtop < view.bottom, onThen = q.vbot > lastView.top && q.vtop < lastView.bottom;
        // (a block that cannot be seen, visibility: hidden or opacity 0, does not count, as in the layout shift score)
        // (a block as tall as the screen or taller, over all of it before and after, shows no move of its own: what is
        // in it is looked at block by block)
        const over = q.vtop <= view.top && q.vbot >= view.bottom && p.vtop <= view.top && p.vbot >= view.bottom;
        if (Math.abs(dy) > MOVE_PX && (onNow || onThen) && !over && (!p.el.checkVisibility || p.el.checkVisibility({ visibilityProperty: true, opacityProperty: true }))) moved.push([k, p, q, dy]);
        // above the screen and changed its height while the page is scrolled
        if ((J.phase === 'scroll-down' || J.phase === 'scroll-up' || J.phase === 'again-scroll') && q.vbot <= lastView.top && p.vbot <= view.top
          && Math.abs(p.h - q.h) > MOVE_PX && J.above.length < 100) {
          // only the innermost of nested blocks that grew together is worth naming; keep all, the report folds them
          J.above.push({ phase: J.phase, t: Math.round(t), sel: sel(p.el), dh: Math.round(p.h - q.h) });
        }
      }
      // what can be seen of a block: its own text, background, border or shadow, or a picture or control in it (an
      // empty box that moves, like a view still waiting for its page, shows nothing moving)
      const PAINTED = new Set(['img', 'svg', 'canvas', 'video', 'input', 'select', 'textarea', 'button', 'picture', 'iframe', 'progress', 'meter']);
      const paints = (el) => {
        for (const c of el.childNodes) {
          if (c.nodeType === 3 && /\S/.test(c.nodeValue)) return true;
          if (c.nodeType === 1 && PAINTED.has(c.localName)) return true;
        }
        const cs = getComputedStyle(el);
        if (cs.backgroundImage !== 'none' || cs.boxShadow !== 'none') return true;
        if (!/rgba\([^)]*,\s*0\)|transparent/.test(cs.backgroundColor)) return true;
        for (const k of ['Top', 'Right', 'Bottom', 'Left']) if (cs[`border${k}Style`] !== 'none' && parseFloat(cs[`border${k}Width`]) > 0) return true;
        return false;
      };
      if (t - J.inputAt < INPUT_MS) moved.length = 0;
      for (let i = moved.length - 1; i >= 0; i--) if (!paints(moved[i][1].el)) moved.splice(i, 1);
      if (moved.length && self) self.visible = true;
      if (moved.length) {
        // the part of the screen that moved: the union of the moved blocks' rows on screen (before and after)
        const spans = [];
        let dist = 0, wide = 0;
        for (const [k, p, q, dy] of moved) {
          for (const [a, z] of [[p.vtop, p.vbot], [q.vtop, q.vbot]]) {
            const lo = Math.max(a, view.top), hi = Math.min(z, view.bottom);
            if (hi > lo) spans.push([lo, hi]);
          }
          dist = Math.max(dist, Math.abs(dy));
          wide = Math.max(wide, p.w);
          // named: the outermost block of those that moved together
          const parent = k.slice(0, k.lastIndexOf('>'));
          const pm = moved.find((m) => m[0] === parent);
          if (pm && Math.abs(pm[3] - dy) <= MOVE_PX) continue;
          if (J.moves.length < 400) J.moves.push({ kind: 'move', phase: J.phase, t: Math.round(t), sel: sel(p.el), dy: Math.round(dy * 10) / 10, h: Math.round(p.h), cause: causeOf(k, q.top) });
        }
        spans.sort((x, y) => x[0] - y[0]);
        let area = 0, cur = null;
        for (const s of spans) {
          if (!cur || s[0] > cur[1]) { if (cur) area += cur[1] - cur[0]; cur = [s[0], s[1]]; } else cur[1] = Math.max(cur[1], s[1]);
        }
        if (cur) area += cur[1] - cur[0];
        const vh = view.bottom - view.top || 1;
        add(Math.min(1, dist / vh), Math.min(1, area / vh) * Math.min(1, wide / (view.w || 1)));
      }
    }
    last = now;
    lastView = { ...view, box: b, st };
    J.samples++;
    J.maxSampleMs = Math.max(J.maxSampleMs, performance.now() - t0);
  }
  function add(dist, impact) {
    const v = dist * impact;
    if (!(v > 0)) return;
    J.score += v;
    J.scoreByPhase[J.phase] = (J.scoreByPhase[J.phase] || 0) + v;
  }
  const tick = (t) => {
    if (J.stopped) return;
    if (document.body) { try { sample(t); } catch (e) { J.error = String(e && e.message || e); } }
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  J.take = () => {
    const out = {};
    for (const k of ['score', 'cls', 'clsByPhase', 'scoreByPhase', 'moves', 'above', 'selfScroll', 'noCls', 'samples', 'maxSampleMs', 'blocks', 'error']) out[k] = J[k];
    return out;
  };
}

// ------------------------------------------------------------------ the run

function getJson(origin, p) {
  return new Promise((resolve, reject) => {
    http.get(origin + p, { headers: { Accept: 'application/json' } }, (res) => {
      let body = '';
      res.on('data', (c) => { body += c; });
      res.on('end', () => { try { resolve(JSON.parse(body)); } catch (e) { reject(e); } });
    }).on('error', reject);
  });
}

async function ids(origin) {
  const out = {};
  const cars = await getJson(origin, '/api/v1/cars?limit=1');
  out.car = cars.items && cars.items[0] ? String(cars.items[0].id) : null;
  if (out.car) {
    for (const [k, list] of [['drive', 'drives'], ['charge', 'charges']]) {
      const pg = await getJson(origin, `/api/v1/cars/${encodeURIComponent(out.car)}/${list}?limit=1`);
      out[k] = pg.items && pg.items[0] ? String(pg.items[0].id) : null;
    }
  }
  return out;
}

const fill = (route, v) => route.replace(/\{([a-z]+)\}/g, (_, k) => encodeURIComponent(v[k] || ''));

async function waitReady(page, id, ms) {
  const t0 = Date.now();
  for (;;) {
    let v = null;
    try { v = await within(5000, page.evaluate(() => document.documentElement.dataset.ready || null), 'reading data-ready'); } catch (_) { /* busy */ }
    if (v === id) return { ok: true, ms: Date.now() - t0 };
    if (typeof v === 'string' && v.startsWith('error:')) return { ok: false, ms: Date.now() - t0, value: v };
    if (Date.now() - t0 > ms) return { ok: false, ms: Date.now() - t0, value: v };
    await sleep(50);
  }
}

// scrolls the page's box down a screen at a time to the end (and back up), phase by phase
async function scrollThrough(page, phase, dir, o) {
  await page.evaluate((p) => { window.__smJumps.phase = p; }, phase);
  let at = await page.evaluate(() => window.__smJumps.scrollTo(window.__smJumps.box().scrollTop));
  for (let i = 0; i < 80; i++) {
    const step = Math.max(100, Math.round(at.height * 0.7));
    const want = dir > 0 ? Math.min(at.max, at.top + step) : Math.max(0, at.top - step);
    if (Math.abs(want - at.top) < 1) break;
    at = await page.evaluate((y) => window.__smJumps.scrollTo(y), want);
    await sleep(o.stepMs);
    // where it is now (a page that grew below has more to scroll)
    at = await page.evaluate(() => { const b = window.__smJumps.box(); return { top: b.scrollTop, max: b.scrollHeight - b.clientHeight, height: b.clientHeight }; });
  }
}

async function visit(browser, server, pack, pageId, url, devName, o) {
  const dev = DEVICES[devName];
  const pw = require('playwright');
  const ctxOpts = { ...(dev.descriptor ? pw.devices[dev.descriptor] : dev.context) };
  delete ctxOpts.defaultBrowserType;
  const context = await browser.newContext({ ...ctxOpts, reducedMotion: 'reduce' });
  const res = { pack, page: pageId, device: devName, url, ok: false, score: 0, cls: null, above: [], selfScroll: [], moves: [], notes: [], error: null };
  try {
    await context.route((u) => /^(https?|wss?):$/.test(u.protocol) && u.host !== server.host, (route) => {
      let u = null;
      try { u = new URL(route.request().url()); } catch (_) { /* odd */ }
      if (u && u.protocol === 'https:' && TILE_HOSTS.has(u.host)) return route.fulfill({ status: 200, contentType: 'image/png', headers: { 'access-control-allow-origin': '*' }, body: BLANK_TILE }).catch(() => {});
      return route.abort('blockedbyclient').catch(() => {});
    });
    await context.addInitScript(jumpsInit, { anchorOff: dev.engine === 'webkit' });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e.message || e).slice(0, 200)));
    await page.goto(server.origin + url, { waitUntil: 'commit', timeout: 30000 });
    const ready = await waitReady(page, pageId, o.readyTimeoutMs);
    if (!ready.ok) res.notes.push(`not ready after ${Math.round(ready.ms / 1000)} s (${ready.value || 'no data-ready'})`);
    await page.evaluate(() => { window.__smJumps.phase = 'settle'; });
    await sleep(o.settleMs);
    await scrollThrough(page, 'scroll-down', 1, o);
    await sleep(600);
    await scrollThrough(page, 'scroll-up', -1, o);
    await sleep(400);
    // another page inside the app and back: the kept answers first, then the fresh ones
    if (ready.ok && o.again) {
      await page.evaluate(() => { window.__smJumps.phase = 'away'; });
      const away = pageId === 'help' ? '/settings/display' : '/help';
      const nav = (to) => page.evaluate((href) => {
        window.__smJumps.inputAt = performance.now();
        const a = document.createElement('a');
        a.href = href;
        a.setAttribute('data-sm-jumps', '');
        a.style.display = 'none';
        document.body.appendChild(a);
        a.click();
        a.remove();
      }, to);
      await nav(away);
      await sleep(1200);
      await page.evaluate(() => { window.__smJumps.phase = 'again'; });
      await nav(url);
      const r2 = await waitReady(page, pageId, o.readyTimeoutMs);
      if (!r2.ok) res.notes.push('not ready again after coming back');
      await sleep(o.settleMs);
      await page.evaluate(() => { window.__smJumps.scrollTo(0); });
      await scrollThrough(page, 'again-scroll', 1, o);
      await sleep(400);
    }
    const J = await within(20000, page.evaluate(() => window.__smJumps.take()), 'reading the watch');
    if (J.error) res.notes.push(`watch error: ${J.error}`);
    if (errors.length) res.notes.push(`page errors: ${errors.slice(0, 2).join(' | ')}`);
    // the moved blocks during 'away' (another page) do not count
    const counted = (p) => p !== 'away';
    res.score = Object.entries(J.scoreByPhase).filter(([p]) => counted(p)).reduce((a, [, v]) => a + v, 0);
    res.scoreByPhase = J.scoreByPhase;
    res.cls = J.noCls ? null : Object.entries(J.clsByPhase).filter(([p]) => counted(p)).reduce((a, [, v]) => a + v, 0);
    res.clsByPhase = J.clsByPhase;
    res.above = J.above;
    // a scroll the page made by itself fails when it moved what is on screen (its own scroll anchoring does not)
    res.selfScroll = J.selfScroll.filter((s) => counted(s.phase) && s.visible);
    res.moves = J.moves.filter((m) => counted(m.phase));
    res.samples = J.samples;
    res.maxSampleMs = Math.round(J.maxSampleMs);
    res.blocks = J.blocks;
    res.ready = ready;
    res.ok = ready.ok && res.score <= o.maxScore && (res.cls == null || res.cls <= o.maxScore) && !res.above.length && !res.selfScroll.length;
  } catch (e) {
    res.error = String(e.message || e).split('\n')[0].slice(0, 300);
  } finally {
    await within(15000, context.close(), 'closing').catch(() => {});
  }
  return res;
}

function parseArgs(argv) {
  const o = { packs: PACKS, pages: Object.keys(ROUTES), devices: Object.keys(DEVICES), serverArgs: [], json: false, verbose: false, again: true, ...DEFAULTS };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const val = () => { if (i + 1 >= argv.length) throw new Error(`${a} needs a value`); return argv[++i]; };
    if (a === '--packs' || a === '--pack') { const v = val(); o.packs = v === 'all' ? PACKS : v.split(',').filter(Boolean); }
    else if (a === '--pages') { const v = val(); o.pages = v === 'all' ? Object.keys(ROUTES) : v.split(',').filter(Boolean); }
    else if (a === '--devices') { const v = val(); o.devices = v === 'all' ? Object.keys(DEVICES) : v.split(',').filter(Boolean); }
    else if (a === '--max-score') o.maxScore = +val();
    else if (a === '--latency-ms') o.latencyMs = +val();
    else if (a === '--step-ms') o.stepMs = +val();
    else if (a === '--jobs') o.jobs = Math.max(1, +val());
    else if (a === '--server-args') o.serverArgs = val().split(/\s+/).filter(Boolean);
    else if (a === '--no-again') o.again = false;
    else if (a === '--json') o.json = true;
    else if (a === '--verbose') o.verbose = true;
    else if (a === '-h' || a === '--help') o.help = true;
    else throw new Error(`unknown option ${a}`);
  }
  for (const p of o.pages) if (!ROUTES[p]) throw new Error(`no page ${p}`);
  for (const d of o.devices) if (!DEVICES[d]) throw new Error(`no device ${d} (${Object.keys(DEVICES).join(', ')})`);
  for (const p of o.packs) if (!PACKS.includes(p)) throw new Error(`no pack ${p}`);
  return o;
}

async function run(o) {
  const log = o.log || (() => {});
  const pw = require('playwright');
  const browsers = {};
  const notes = [];
  const results = [];
  try {
    for (const engine of new Set(o.devices.map((d) => DEVICES[d].engine))) {
      try { browsers[engine] = await pw[engine].launch(); } catch (e) { notes.push(`${engine} did not start (${String(e.message).split('\n')[0]}): its devices were skipped`); }
    }
    const devices = o.devices.filter((d) => browsers[DEVICES[d].engine]);
    for (const pack of o.packs) {
      const server = await startServer(path.join(UI_DIR, 'packs', pack), ['--rows-every-s', '10', '--latency-ms', String(o.latencyMs), ...o.serverArgs]);
      try {
        const v = await ids(server.origin);
        const tasks = [];
        for (const pageId of o.pages) for (const route of ROUTES[pageId]) for (const d of devices) tasks.push({ pageId, url: fill(route, v), d });
        let next = 0;
        const worker = async () => {
          while (next < tasks.length) {
            const t = tasks[next++];
            const r = await visit(browsers[DEVICES[t.d].engine], server, pack, t.pageId, t.url, t.d, o);
            results.push(r);
            log(`  ${pack} ${t.url} ${t.d}: ${r.error ? 'error ' + r.error : r.ok ? 'ok' : 'JUMPS'} score ${r.score.toFixed(3)}${r.cls != null ? ` cls ${r.cls.toFixed(3)}` : ''}${r.above.length ? ` above ${r.above.length}` : ''}${r.selfScroll.length ? ` scrolled ${r.selfScroll.length}` : ''}`);
          }
        };
        await Promise.all(Array.from({ length: Math.min(o.jobs, tasks.length) }, worker));
      } finally {
        await server.stop();
      }
    }
  } finally {
    for (const b of Object.values(browsers)) await b.close().catch(() => {});
  }
  return { pass: results.every((r) => r.ok), notes, maxScore: o.maxScore, results };
}

function describe(r) {
  const L = [];
  const head = `${r.pack} ${r.url} on ${r.device}`;
  if (r.error) return [`${head}: error: ${r.error}`];
  L.push(`${head}: jump score ${r.score.toFixed(3)}${r.cls != null ? `, layout shift ${r.cls.toFixed(3)}` : ''} (${Object.entries(r.scoreByPhase || {}).map(([k, v]) => `${k} ${v.toFixed(3)}`).join(', ')})`);
  for (const n of r.notes) L.push(`    note: ${n}`);
  for (const a of r.above.slice(0, 6)) L.push(`    above the screen while scrolling (${a.phase}): ${a.sel} changed height by ${a.dh} px`);
  for (const s of r.selfScroll.slice(0, 4)) L.push(`    scrolled by itself (${s.phase}, ${s.t} ms): ${s.from} -> ${s.to}, page ${s.height} px high${s.cause && s.cause.length ? `; changed: ${s.cause.join('; ')}` : ''}`);
  const moves = r.moves.filter((m) => m.kind === 'move');
  const seen = new Map();
  for (const m of moves) { const k = `${m.phase} ${m.sel}`; if (!seen.has(k)) seen.set(k, { ...m, n: 0 }); seen.get(k).n++; }
  for (const m of [...seen.values()].slice(0, LIST)) L.push(`    moved (${m.phase}, ${m.t} ms): ${m.sel} by ${m.dy > 0 ? '+' : ''}${m.dy} px${m.n > 1 ? `, ${m.n}x` : ''}${m.cause ? `, after ${m.cause}` : ''}`);
  for (const m of r.moves.filter((x) => x.kind === 'cls').slice(0, 4)) L.push(`    layout shift (${m.phase}, ${m.t} ms): ${m.sel} by ${m.dy} px (${m.v})`);
  return L;
}

function formatReport(rep) {
  const L = [];
  for (const n of rep.notes) L.push(`note: ${n}`);
  const bad = rep.results.filter((r) => !r.ok);
  for (const r of bad) L.push(...describe(r));
  L.push(`${rep.results.length - bad.length} of ${rep.results.length} page visits stayed still (jump score and layout shift at most ${rep.maxScore}, nothing above the screen changed size, no scroll by itself)`);
  return L.join('\n') + '\n';
}

const HELP = `Usage: node ui/tools/jumps.js [--packs LIST|all] [--pages LIST|all] [--devices LIST|all] [--max-score N]
       [--latency-ms N] [--step-ms N] [--jobs N] [--server-args "..."] [--no-again] [--json] [--verbose]
Devices: ${Object.keys(DEVICES).join(', ')}. See the top of ui/tools/jumps.js.
`;

async function main() {
  let o;
  try { o = parseArgs(process.argv.slice(2)); } catch (e) { process.stderr.write(`jumps.js: ${e.message}\n`); return 2; }
  if (o.help) { process.stdout.write(HELP); return 0; }
  if (o.verbose || !o.json) o.log = (l) => process.stderr.write(l + '\n');
  const rep = await run(o);
  process.stdout.write(o.json ? JSON.stringify(rep, null, 2) + '\n' : formatReport(rep));
  return rep.pass ? 0 : 1;
}

if (require.main === module) {
  main().then((c) => { process.exitCode = c; }, (e) => { process.stderr.write(`jumps.js: ${e.stack || e}\n`); killChildren('SIGTERM'); process.exitCode = 2; });
}

module.exports = { run, parseArgs, formatReport, describe, jumpsInit, DEVICES, PACKS };
