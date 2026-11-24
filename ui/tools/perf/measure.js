#!/usr/bin/env node
// Browser-delivery measurement for Sentry Mate's UI packs: for each pack x page, a cold load (new browser context,
// empty cache) and a warm load (the same URL again in the same page), on an emulated phone with a throttled CPU and
// network. Made-up data only (a `dev seed` data folder).
//
//   node measure.js --base https://127.0.0.1:10101 --user owner --pass 'correct horse battery staple' \
//     [--packs instrument,logbook,...] [--pages car,drives,charges,battery,settings] \
//     [--out results.json] [--md results.md] [--label baseline] [--timeout 90000] [--quiet 500]
//     [--desktop] [--nav]
//
//   --desktop  a 1440 x 900 desktop (deviceScaleFactor 1, no touch) with no CPU or network throttling
//   --nav      after the warm load of the car page, in-app navigation to every other page of --pages (a click on the
//              pack's own link to it where there is one, else the router's history.pushState + popstate), each
//              timed from the click to the new view being ready (rows with phase "nav")
//     [--device phone|laptop] [--scroll]
//
// --device laptop: 1440 x 900 at 1x, no touch, CPU 2x slower (a mid laptop), no network throttling unless --latency
// is given (a server on the home network). --scroll: after each load, the list on the page (the box holding its first
// row, else the page) is scrolled 60 px a frame for 180 frames; frames per second and slow frames are reported.
// rows ms: when the page's first list row (a drive link in a table body, a Pebble .lb-row, or a [data-vl-row]) was
// first painted (the frame after it was in the document), in the page's clock; list ms: the same, counted from the
// start of the page's first request for its list (the API path ending in /<page>, e.g. /cars/c1/logbook); dom rows:
// list rows in the document at the end.
//
// Conditions (stated in the output too):
//   CPU      Emulation.setCPUThrottlingRate { rate: 4 }
//   network  Network.emulateNetworkConditions { latency: 150 ms, download 1.6 Mbit/s * 0.9 = 180000 B/s,
//            upload 750 kbit/s * 0.9 = 84375 B/s } (Lighthouse's mobile "slow 4G / fast 3G" figures)
//            on the page and on the site's service worker too: the page's own throttling leaves a request the worker
//            answers alone, so without it the worker's network reads (every API read once it controls the page: warm
//            loads and navigations) took no round trip at all (--no-sw-throttle: as before, for comparison)
//   device   390 x 844 CSS px, deviceScaleFactor 3, isMobile, hasTouch
//
// Time to usable (ms after navigation start, in the page's own clock):
//   the later of (a) the moment the page's main view (#view, else #main, else <main>) last became "ready" — it
//   has content (an element child and some text), no aria-busy="true" on it or inside it, and no loading
//   placeholder inside it (.loading, .page--loading, .lb-loading, .loading-grid, .loading-head, .skeleton,
//   [class*="skeleton"]) — and (b) the last responseEnd of any resource except /api/v1/events (the SSE stream);
//   the run waits until the view is ready and the network (CDP, excluding /api/v1/events) has had no request in
//   flight for --quiet ms (500 by default). All counts (bytes, requests, duplicates, long tasks) are taken over
//   the load up to that settle point.
'use strict';
const { chromium } = require('playwright');
const fs = require('fs');
const os = require('os');
const path = require('path');

const args = (() => {
  const a = {};
  const v = process.argv.slice(2);
  for (let i = 0; i < v.length; i++) {
    if (v[i].startsWith('--')) {
      const k = v[i].slice(2);
      const nx = v[i + 1];
      if (nx === undefined || nx.startsWith('--')) a[k] = true; else { a[k] = nx; i++; }
    }
  }
  return a;
})();

const BASE = (args.base || 'https://127.0.0.1:10101').replace(/\/$/, '');
const USER = args.user || 'owner';
const PASS = args.pass || process.env.SM_PASS || 'correct horse battery staple';
const PACKS = args.packs ? args.packs.split(',') : [...require('../builtin-packs').BUILTIN_PACKS];
const PAGES = (args.pages || 'car,drives,drive,charges,battery,statistics,settings,settings-integrations').split(',');
const TIMEOUT = Number(args.timeout || 90000);
const QUIET = Number(args.quiet || 500);
const LABEL = args.label || 'run';
const OUT = args.out || 'perf-results.json';
const MD = args.md || OUT.replace(/\.json$/, '') + '.md';

const LAPTOP = args.device === 'laptop';
const NET = { offline: false, latency: Number(args.latency || 150), downloadThroughput: Math.round(1.6e6 * 0.9 / 8), uploadThroughput: Math.round(750e3 * 0.9 / 8) };
const CPU = Number(args.cpu || (LAPTOP ? 2 : 4));
const DESKTOP = !!args.desktop;
if (DESKTOP) args['no-throttle'] = true;
const THROTTLE_NET = !LAPTOP || args.latency != null;
const DEVICE = DESKTOP || LAPTOP ? { viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1, isMobile: false, hasTouch: false }
  : { viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true,
    userAgent: 'Mozilla/5.0 (Linux; Android 13; Phone) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Mobile Safari/537.36' };

// every pack shares ui/packs/*/web/shared/state.js's ROUTES; "settings" is Settings: Display and units
const PATHS = {
  car: (c) => `/cars/${c}`,
  drives: (c) => `/cars/${c}/drives`,
  charges: (c) => `/cars/${c}/charges`,
  battery: (c) => `/cars/${c}/battery`,
  drive: (c, d) => `/cars/${c}/drives/${d}`,
  statistics: (c) => `/cars/${c}/statistics`,
  logbook: (c) => `/cars/${c}/logbook`,
  settings: () => '/settings/display',
  'settings-integrations': () => '/settings/integrations',
};

const ROW_SELECTOR = '[data-vl-row], tbody a[href*="/drives/"], .lb-row';
const READY_SELECTOR_BLOCKERS = '[aria-busy="true"], .loading, .page--loading, .lb-loading, .loading-grid, .loading-head, .skeleton, [class*="skeleton"]';

// runs in every document before its scripts: long tasks, a resource buffer big enough, and the "view ready" clock
const INIT = `(() => {
  try { performance.setResourceTimingBufferSize(100000); } catch (e) {}
  const lt = { count: 0, total: 0, max: 0 };
  window.__perf = { lt, readyAt: null, ready: false, blocker: 'no view' };
  try {
    new PerformanceObserver((l) => { for (const e of l.getEntries()) { lt.count++; lt.total += e.duration; if (e.duration > lt.max) lt.max = e.duration; } })
      .observe({ type: 'longtask', buffered: true });
  } catch (e) {}
  const BLOCK = ${JSON.stringify(READY_SELECTOR_BLOCKERS)};
  const ROWS = ${JSON.stringify(ROW_SELECTOR)};
  function check() {
    const p = window.__perf;
    if (p.rowsSeen == null && document.querySelector(ROWS)) {
      p.rowsSeen = performance.now();
      requestAnimationFrame(() => { p.rowsAt = performance.now(); });
    }
    const v = document.getElementById('view') || document.getElementById('main') || document.querySelector('main');
    let blocker = null;
    if (!v) blocker = 'no view';
    else if (v.getAttribute('aria-busy') === 'true') blocker = 'view aria-busy';
    else if (!v.firstElementChild || (v.textContent || '').trim().length < 2) blocker = 'view empty';
    else if (window.__perf.navTo && (location.pathname !== window.__perf.navTo || !document.documentElement.dataset.ready)) blocker = 'old view';
    else { const b = v.querySelector(BLOCK); if (b) blocker = 'placeholder: ' + (b.getAttribute('aria-busy') === 'true' ? '[aria-busy]' : '.' + String(b.className).split(' ').join('.')); }
    const ready = blocker === null;
    if (ready && !window.__perf.ready) window.__perf.readyAt = performance.now();
    window.__perf.ready = ready; window.__perf.blocker = blocker;
  }
  window.__perfCheck = check;
  const start = () => {
    new MutationObserver(check).observe(document.documentElement, { subtree: true, childList: true, attributes: true, characterData: true });
    check();
  };
  if (document.documentElement) start(); else document.addEventListener('readystatechange', start, { once: true });
})();`;

const isSse = (u) => new URL(u).pathname === '/api/v1/events';

async function load(page, cdp, url) {
  const t0 = Date.now();
  // in flight, as Playwright sees it (it follows workers' requests to the end, which the page's CDP session
  // does not); the SSE stream never ends and is left out
  const pw = new Set();
  const ctx = page.context();
  const pwReq = (q) => { if (!isSse(q.url()) && !q.url().startsWith('data:')) { pw.add(q); lastActivity = Date.now(); } };
  const pwDone = [];
  const pwEnd = (q) => { if (pw.delete(q)) { lastActivity = Date.now(); pwDone.push(q); } };
  ctx.on('request', pwReq); ctx.on('requestfinished', pwEnd); ctx.on('requestfailed', pwEnd);
  const reqs = new Map(); // requestId -> {url, method, start, protocol, bytes, done, status, fromCache}
  let inflight = 0;
  let lastActivity = Date.now();
  const on = (ev, fn) => { cdp.on(ev, fn); return () => cdp.off(ev, fn); };
  const offs = [
    on('Network.requestWillBeSent', (e) => {
      if (e.request.url.startsWith('data:')) return;
      if (reqs.has(e.requestId)) { reqs.get(e.requestId).redirects++; return; }
      const sse = isSse(e.request.url);
      reqs.set(e.requestId, { url: e.request.url, method: e.request.method, type: e.type, sse, done: false, bytes: 0, redirects: 0, at: Date.now() - t0 , staleOk: Object.entries(e.request.headers || {}).some(([k, v]) => k.toLowerCase() === 'x-sentry-mate-stale-ok' && v === '1') });
      if (!sse) inflight++;
      lastActivity = Date.now();
    }),
    on('Network.responseReceived', (e) => {
      const r = reqs.get(e.requestId); if (!r) return;
      r.respondedAt = Date.now(); lastActivity = Date.now();
      r.memory = Object.keys(e.response.headers || {}).some((k) => k.toLowerCase() === 'x-sentry-mate-stale');
      r.protocol = e.response.protocol; r.status = e.response.status; r.fromCache = !!(e.response.fromDiskCache || e.response.fromServiceWorker || e.response.fromPrefetchCache);
    }),
    on('Network.dataReceived', (e) => { const r = reqs.get(e.requestId); if (r) { r.received = (r.received || 0) + (e.encodedDataLength || 0); if (!r.sse) lastActivity = Date.now(); } }),
    on('Network.loadingFinished', (e) => {
      const r = reqs.get(e.requestId); if (!r || r.done) return;
      r.done = true; r.bytes = e.encodedDataLength; if (!r.sse) inflight--; lastActivity = Date.now();
    }),
    on('Network.loadingFailed', (e) => {
      const r = reqs.get(e.requestId); if (!r || r.done) return;
      r.done = true; r.failed = e.errorText; if (!r.sse) inflight--; lastActivity = Date.now();
    }),
  ];
  let navError = null;
  await page.goto(url, { waitUntil: 'commit', timeout: TIMEOUT }).catch((e) => { navError = String(e.message || e); });
  let timedOut = false;
  for (;;) {
    await new Promise((r) => setTimeout(r, 100));
    let ready = false;
    try { ready = await page.evaluate(() => (window.__perfCheck && window.__perfCheck(), !!(window.__perf && window.__perf.ready))); } catch (e) { /* navigating */ }
    const now = Date.now();
    const pending = pw.size;
    if (ready && pending <= 0 && now - lastActivity >= QUIET) break;
    if (Date.now() - t0 > TIMEOUT) { timedOut = true; break; }
  }
  offs.forEach((f) => f());
  ctx.off('request', pwReq); ctx.off('requestfinished', pwEnd); ctx.off('requestfailed', pwEnd);
  const listName = '/' + new URL(url).pathname.split('/').pop();
  const m = await page.evaluate((listName) => {
    const p = window.__perf || {};
    const listReq = performance.getEntriesByType('resource').find((e) => { try { const u = new URL(e.name); return u.pathname.startsWith('/api/') && u.pathname.endsWith(listName); } catch (x) { return false; } });
    const fcp = performance.getEntriesByName('first-contentful-paint')[0];
    const res = performance.getEntriesByType('resource').filter((e) => !/\/api\/v1\/events(\?|$)/.test(new URL(e.name).pathname));
    const nav = performance.getEntriesByType('navigation')[0];
    const lastRes = Math.max(nav ? nav.responseEnd : 0, ...res.map((e) => e.responseEnd));
    const rowsIn = document.querySelectorAll('[data-vl-row], tbody tr:not(.vl-gap):not(.tbl-skel), .lb-row').length;
    return { listAt: listReq ? listReq.startTime : null, rowsAt: p.rowsAt ?? null, rowsIn, nodes: document.getElementsByTagName('*').length, fcp: fcp ? fcp.startTime : null, readyAt: p.readyAt, ready: p.ready, blocker: p.blocker, lastRes, lt: p.lt,
      dcl: nav ? nav.domContentLoadedEventEnd : null, load: nav ? nav.loadEventEnd : null, finalPath: location.pathname };
  }, listName).catch((e) => ({ error: String(e) }));
  const list = [...reqs.values()];
  // a worker's own script: its response never reaches the page's CDP session; take its size from Playwright
  for (const r of list) {
    if (r.done || r.sse) continue;
    const q = pwDone.find((x) => x.url() === r.url);
    if (!q) continue;
    try {
      const z = await q.sizes(); const resp = await q.response();
      r.done = true; r.bytes = z.responseBodySize + z.responseHeadersSize; r.status = resp ? resp.status() : r.status; r.via = 'playwright';
    } catch (e) { /* gone */ }
  }
  const api = list.filter((r) => new URL(r.url).pathname.startsWith('/api/'));
  const stat = list.filter((r) => !new URL(r.url).pathname.startsWith('/api/'));
  const sum = (a) => a.reduce((s, r) => s + (r.done ? (r.bytes || 0) : (r.received || 0)), 0);
  const gets = {};
  // a stale-ok read the service worker answers from its memory reaches no server (counted apart)
  for (const r of api) if (r.method === 'GET' && !r.sse && !r.memory) gets[r.url] = (gets[r.url] || 0) + 1;
  const dups = Object.entries(gets).filter(([, n]) => n > 1).map(([u, n]) => ({ url: new URL(u).pathname + new URL(u).search, n }));
  const protos = {};
  for (const r of list) if (r.protocol) protos[r.protocol] = (protos[r.protocol] || 0) + 1;
  const ttu = m.readyAt != null ? Math.round(Math.max(m.readyAt, m.lastRes || 0)) : null;
  return {
    url: new URL(url).pathname, final_path: m.finalPath, nav_error: navError, timed_out: timedOut, blocker: timedOut ? m.blocker : null,
    inflight_at_end: timedOut ? [...pw].map((q) => new URL(q.url()).pathname) : undefined,
    late_requests: timedOut ? list.filter((r) => r.at > TIMEOUT - 5000).map((r) => new URL(r.url).pathname).slice(0, 20) : undefined,
    bytes: sum(list), bytes_static: sum(stat), bytes_api: sum(api),
    requests: list.length, requests_static: stat.length, requests_api: api.length,
    not_modified: list.filter((r) => r.status === 304).length, from_cache: list.filter((r) => r.fromCache).length,
    failed: list.filter((r) => r.failed).length,
    protocols: protos,
    fcp_ms: m.fcp != null ? Math.round(m.fcp) : null,
    view_ready_ms: m.readyAt != null ? Math.round(m.readyAt) : null,
    rows_ms: m.rowsAt != null ? Math.round(m.rowsAt) : null,
    list_ms: m.rowsAt != null && m.listAt != null ? Math.round(m.rowsAt - m.listAt) : null, dom_rows: m.rowsIn ?? null, dom_nodes: m.nodes ?? null,
    ttu_ms: ttu,
    dcl_ms: m.dcl != null ? Math.round(m.dcl) : null,
    long_tasks: m.lt ? m.lt.count : null, long_tasks_ms: m.lt ? Math.round(m.lt.total) : null, long_task_max_ms: m.lt ? Math.round(m.lt.max) : null,
    dup_api_gets: dups.reduce((s, d) => s + d.n - 1, 0), dup_api_list: dups, memory_api_gets: api.filter((r) => r.memory).length,
    all: args.verbose ? list.map((r) => [new URL(r.url).pathname, r.status, r.done ? r.bytes : r.received, r.type, r.at]) : undefined,
    biggest: list.map((r) => ({ path: new URL(r.url).pathname, status: r.status, bytes: r.done ? r.bytes : (r.received || 0) }))
      .sort((x, y) => y.bytes - x.bytes).slice(0, 8),
    api_gets: Object.keys(gets).map((u) => new URL(u).pathname + new URL(u).search),
  };
}

// An in-app navigation: the pack's own link to `target` clicked where one is on the page (a touchstart/hover first,
// as a person's finger or mouse gives), else the router told through history.pushState + popstate. Timed in the
// page's clock from the click to the new view being ready (its data-ready cleared before the click: the new page sets
// it when it has drawn, and until then, or on the old path, the view is not ready). Bytes and requests are counted from the click until the network is
// quiet.
async function navigate(page, cdp, base, target) {
  const reqs = []; let lastActivity = Date.now(); const pw = new Set();
  const ctx = page.context();
  const pwReq = (q) => { if (!isSse(q.url()) && !q.url().startsWith('data:')) { pw.add(q); reqs.push(q); lastActivity = Date.now(); } };
  const pwEnd = (q) => { if (pw.delete(q)) lastActivity = Date.now(); };
  ctx.on('request', pwReq); ctx.on('requestfinished', pwEnd); ctx.on('requestfailed', pwEnd);
  const t0 = Date.now();
  const via = await page.evaluate((to) => {
    delete document.documentElement.dataset.ready;
    window.__perf.ready = false; window.__perf.readyAt = null; window.__perf.navTo = to;
    const links = [...document.querySelectorAll('a[href]')].filter((a) => {
      try { const u = new URL(a.href, location.href); return u.origin === location.origin && u.pathname === to; } catch (e) { return false; }
    });
    const visible = links.find((a) => { const r = a.getBoundingClientRect(); return r.width > 0 && r.height > 0; });
    window.__perf.navStart = performance.now();
    if (visible) {
      visible.dispatchEvent(new PointerEvent('pointerenter', { bubbles: false }));
      visible.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));
      visible.dispatchEvent(new MouseEvent('mouseenter', { bubbles: false }));
      try { visible.dispatchEvent(new TouchEvent('touchstart', { bubbles: true, cancelable: true })); } catch (e) {}
      visible.click();
      return 'link';
    }
    history.pushState(null, '', to);
    window.dispatchEvent(new PopStateEvent('popstate', { state: null }));
    return 'pushState';
  }, target);
  let timedOut = false;
  for (;;) {
    await new Promise((r) => setTimeout(r, 50));
    let ready = false;
    try { ready = await page.evaluate(() => (window.__perfCheck && window.__perfCheck(), !!(window.__perf && window.__perf.ready))); } catch (e) { /* navigating */ }
    if (ready && pw.size <= 0 && Date.now() - lastActivity >= QUIET) break;
    if (Date.now() - t0 > TIMEOUT) { timedOut = true; break; }
  }
  ctx.off('request', pwReq); ctx.off('requestfinished', pwEnd); ctx.off('requestfailed', pwEnd);
  const m = await page.evaluate(() => { const p = window.__perf; const r = { readyAt: p.readyAt, start: p.navStart, blocker: p.blocker, path: location.pathname }; p.navTo = null; return r; }).catch((e) => ({ error: String(e) }));
  const sizes = [];
  for (const q of reqs) { try { const z = await q.sizes(); sizes.push(z.responseBodySize + z.responseHeadersSize); } catch (e) { sizes.push(0); } }
  const isApi = (q) => new URL(q.url()).pathname.startsWith('/api/');
  return {
    url: target, final_path: m.path, via, timed_out: timedOut, blocker: timedOut ? m.blocker : null,
    ttu_ms: m.readyAt != null && m.start != null ? Math.round(m.readyAt - m.start) : null, fcp_ms: null,
    bytes: sizes.reduce((a, b) => a + b, 0), requests: reqs.length, requests_static: reqs.filter((q) => !isApi(q)).length,
    requests_api: reqs.filter(isApi).length, not_modified: 0, dup_api_gets: 0, dup_api_list: [], memory_api_gets: 0, protocols: {},
    long_tasks: null, long_tasks_ms: null,
  };
}

// scrolls the list holding the first row (its scrolling box, else the page) 60 px a frame and times the frames
async function scrollRun(page) {
  return page.evaluate(async (sel) => {
    const first = document.querySelector(sel);
    let box = first ? first.parentElement : null;
    while (box && !(box.scrollHeight > box.clientHeight + 1 && /(auto|scroll)/.test(getComputedStyle(box).overflowY))) box = box.parentElement;
    const el = box || document.scrollingElement;
    const times = [];
    await new Promise((done) => {
      let n = 0;
      const step = (t) => {
        times.push(t);
        el.scrollTop += 60;
        if (++n < 180) requestAnimationFrame(step); else done();
      };
      requestAnimationFrame(step);
    });
    const d = times.slice(1).map((t, i) => t - times[i]);
    const sorted = [...d].sort((a, b) => a - b);
    return { target: box ? 'box' : 'page', frames: times.length, fps: Math.round((d.length * 1000) / (times.at(-1) - times[0])),
      p95_ms: Math.round(sorted[Math.floor(sorted.length * 0.95)] * 10) / 10, slow_frames: d.filter((x) => x > 25).length,
      max_ms: Math.round(sorted.at(-1)), scrolled_px: Math.round(el.scrollTop) };
  }, ROW_SELECTOR).catch((e) => ({ error: String(e) }));
}

// The network conditions on every service worker of the browser too (the page's CDP session cannot reach them):
// the browser's own CDP endpoint, auto-attached to each new target, which waits until it is set. Returns close().
async function throttleWorkers(port, net) {
  const v = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
  const ws = new WebSocket(v.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', rej); });
  let id = 0;
  const pend = new Map();
  const send = (method, params = {}, sessionId) => new Promise((res) => {
    const i = ++id; pend.set(i, res); ws.send(JSON.stringify({ id: i, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  ws.addEventListener('message', async (m) => {
    const d = JSON.parse(m.data);
    if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); return; }
    if (d.method !== 'Target.attachedToTarget') return;
    const { sessionId, targetInfo } = d.params;
    if (targetInfo.type === 'service_worker') {
      await send('Network.enable', {}, sessionId);
      const r = await send('Network.emulateNetworkConditions', net, sessionId);
      if (r.error) console.error('service worker not throttled:', r.error.message);
    }
    await send('Runtime.runIfWaitingForDebugger', {}, sessionId);
  });
  const r = await send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
  if (r.error) throw new Error('Target.setAutoAttach: ' + r.error.message);
  return () => { try { ws.close(); } catch { /* gone */ } };
}

async function main() {
  const browser = await chromium.launch({ args: ['--ignore-certificate-errors'] });
  // one sign-in for the whole run (the server limits right-password sign-ins per minute)
  const auth = await browser.newContext({ ignoreHTTPSErrors: true });
  const h = { 'X-Sentry-Mate': '1', 'Content-Type': 'application/json' };
  const s = await auth.request.post(BASE + '/api/v1/session', { headers: h, data: { name: USER, password: PASS } });
  if (!s.ok()) throw new Error('sign-in: ' + s.status() + ' ' + (await s.text()));
  const cars = await (await auth.request.get(BASE + '/api/v1/cars', { headers: h })).json();
  const car = args.car || cars.items[0].id;
  const drives = await (await auth.request.get(BASE + `/api/v1/cars/${car}/drives?limit=5`, { headers: h })).json().catch(() => ({ items: [] }));
  const drive = args.drive || (drives.items && drives.items[0] && drives.items[0].id);
  const cookies = (await auth.storageState()).cookies;
  const about = await (await auth.request.get(BASE + '/api/v1/about', { headers: h })).json().catch(() => ({}));
  const results = { label: LABEL, base: BASE, when: new Date().toISOString(), server_version: about.version || null,
    build: about.build || null, car, cars: cars.items.length,
    conditions: { desktop: DESKTOP, laptop: LAPTOP, throttled: !args['no-throttle'], network_throttled: THROTTLE_NET, service_worker_throttled: !args['no-throttle'] && THROTTLE_NET && !args['no-sw-throttle'], cpu_throttling_rate: CPU, network: NET, device: { ...DEVICE, userAgent: undefined }, quiet_ms: QUIET, timeout_ms: TIMEOUT,
      ttu: 'max(main view ready: content, no aria-busy, no placeholders; last resource responseEnd excl. /api/v1/events), after ' + QUIET + ' ms network quiet',
      warm: 'second page.goto of the same URL in the same page and persistent profile (fresh profile folder per pack/page) (HTTP cache warm; packs answer Cache-Control: no-cache, so 304 revalidations)' },
    runs: [] };
  for (const pack of PACKS) {
    const pr = await auth.request.patch(BASE + '/api/v1/me/preferences', { headers: h, data: { ui_pack: pack } });
    if (!pr.ok()) { console.error(`pack ${pack}: ${pr.status()} ${await pr.text()}`); continue; }
    const got = (await pr.json()).ui_pack;
    for (const pg of PAGES) {
      const url = BASE + PATHS[pg](car, drive);
      // a persistent profile in a new empty folder: an empty disk cache, like a phone's browser (an incognito
      // context's in-memory cache does not keep maplibre-gl.mjs, 1.1 MB, for the warm load)
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'sm-perf-'));
      const swNet = !args['no-throttle'] && THROTTLE_NET && !args['no-sw-throttle'];
      const dport = 20000 + Math.floor(Math.random() * 20000);
      const ctx = await chromium.launchPersistentContext(dir, { ignoreHTTPSErrors: true, args: ['--ignore-certificate-errors', ...(swNet ? [`--remote-debugging-port=${dport}`] : [])], ...DEVICE });
      const unthrottle = swNet ? await throttleWorkers(dport, NET) : null;
      await ctx.addCookies(cookies);
      await ctx.addInitScript(INIT);
      const page = ctx.pages()[0] || await ctx.newPage();
      const cdp = await ctx.newCDPSession(page);
      await cdp.send('Network.enable');
      await cdp.send('Network.setCacheDisabled', { cacheDisabled: false });
      if (!args['no-throttle']) {
        await cdp.send('Emulation.setCPUThrottlingRate', { rate: CPU });
        if (THROTTLE_NET) await cdp.send('Network.emulateNetworkConditions', NET);
      }
      for (const phase of ['cold', 'warm']) {
        if (phase === 'warm') await new Promise((r) => setTimeout(r, Number(args['warm-pause'] || 0)));
        const r = await load(page, cdp, url);
        if (args.scroll) r.scroll = await scrollRun(page);
        const row = { pack, pack_served: got, page: pg, phase, ...r };
        results.runs.push(row);
        console.log(`${pack.padEnd(10)} ${pg.padEnd(8)} ${phase} rows=${r.rows_ms}ms list=${r.list_ms}ms dom_rows=${r.dom_rows} nodes=${r.dom_nodes}${r.scroll ? ` fps=${r.scroll.fps} slow=${r.scroll.slow_frames} p95=${r.scroll.p95_ms}ms` : ''} ttu=${r.ttu_ms}ms fcp=${r.fcp_ms}ms ${(r.bytes / 1024).toFixed(1)}KiB req=${r.requests_static}+${r.requests_api}api dup=${r.dup_api_gets} lt=${r.long_tasks}/${r.long_tasks_ms}ms ${JSON.stringify(r.protocols)}${r.timed_out ? ' TIMEOUT ' + r.blocker : ''}`);
      }
      if (args.nav && pg === PAGES[0]) {
        for (const to of PAGES.slice(1)) {
          const r = await navigate(page, cdp, BASE, PATHS[to](car, drive));
          const row = { pack, pack_served: got, page: to, phase: 'nav', ...r };
          results.runs.push(row);
          console.log(`${pack.padEnd(10)} ${to.padEnd(8)} nav ttu=${r.ttu_ms}ms via=${r.via} ${(r.bytes / 1024).toFixed(1)}KiB req=${r.requests_static}+${r.requests_api}api${r.timed_out ? ' TIMEOUT ' + r.blocker : ''}`);
          // back to the first page, ready, before the next one
          await navigate(page, cdp, BASE, PATHS[PAGES[0]](car, drive));
        }
      }
      unthrottle?.();
      await ctx.close();
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }
  await auth.request.patch(BASE + '/api/v1/me/preferences', { headers: h, data: { ui_pack: null } }).catch(() => {});
  await browser.close();
  fs.writeFileSync(OUT, JSON.stringify(results, null, 2));
  fs.writeFileSync(MD, table(results));
  console.log('wrote', OUT, MD);
}

function table(res) {
  const c = res.conditions;
  let s = `# ${res.label}: browser delivery per pack and page\n\n`;
  s += `Server ${res.server_version || '?'} at ${res.base}, ${res.when}. Car ${res.car} (${res.cars} cars).\n`;
  s += c.desktop ? 'Desktop 1440x900 @1x, no CPU or network throttling.\n'
    : c.laptop ? `Laptop 1440x900 @1x, CPU x${c.cpu_throttling_rate}, ${c.network_throttled ? `network latency ${c.network.latency} ms` : 'no network throttling'}.\n`
    : `Phone 390x844 @3x, CPU x${c.cpu_throttling_rate}, network latency ${c.network.latency} ms, down ${c.network.downloadThroughput} B/s, up ${c.network.uploadThroughput} B/s.\n`;
  s += `TTU = ${c.ttu}. Warm = ${c.warm}.\n\n`;
  s += '| pack | page | phase | TTU ms | FCP ms | rows ms | list ms | dom rows | nodes | fps | slow frames | KiB | static req | api req | 304 | dup GETs | long tasks (n / ms) | proto |\n';
  s += '|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|\n';
  for (const r of res.runs) {
    s += `| ${r.pack} | ${r.page} | ${r.phase} | ${r.timed_out ? 'timeout' : r.ttu_ms} | ${r.fcp_ms ?? '-'} | ${r.rows_ms ?? '-'} | ${r.list_ms ?? '-'} | ${r.dom_rows ?? '-'} | ${r.dom_nodes ?? '-'} | ${r.scroll?.fps ?? '-'} | ${r.scroll?.slow_frames ?? '-'} | ${(r.bytes / 1024).toFixed(1)} | ${r.requests_static} | ${r.requests_api} | ${r.not_modified} | ${r.dup_api_gets} | ${r.long_tasks} / ${r.long_tasks_ms} | ${Object.keys(r.protocols).join(',')} |\n`;
  }
  // per pack means
  s += '\n| pack | phase | mean TTU ms | mean KiB | mean requests | total dup GETs |\n|---|---|---:|---:|---:|---:|\n';
  for (const p of [...new Set(res.runs.map((r) => r.pack))]) for (const ph of ['cold', 'warm', 'nav']) {
    const rs = res.runs.filter((r) => r.pack === p && r.phase === ph);
    if (!rs.length) continue;
    const mean = (f) => rs.reduce((a, r) => a + (f(r) || 0), 0) / rs.length;
    s += `| ${p} | ${ph} | ${Math.round(mean((r) => r.ttu_ms))} | ${mean((r) => r.bytes / 1024).toFixed(1)} | ${mean((r) => r.requests).toFixed(1)} | ${rs.reduce((a, r) => a + r.dup_api_gets, 0)} |\n`;
  }
  const dups = res.runs.filter((r) => r.dup_api_list.length);
  if (dups.length) {
    s += '\n## Duplicate API GETs\n\n';
    for (const r of dups) s += `- ${r.pack} ${r.page} ${r.phase}: ${r.dup_api_list.map((d) => `${d.url} x${d.n}`).join(', ')}\n`;
  }
  return s;
}

main().catch((e) => { console.error(e); process.exit(1); });
