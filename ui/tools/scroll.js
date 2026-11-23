#!/usr/bin/env node
// Scroll check: does a page keep drawing while it is scrolled on a 4K screen, also while a rebuild is running?
//
//   node ui/tools/scroll.js [--packs instrument,horizon,...|all] [--pages health,car,...] [--sizes 3840x2160@1,1920x1080@2]
//        [--steps N] [--step-px N] [--step-ms N] [--max-frame-ms N] [--quiet-server] [--server-args "..."]
//        [--trace DIR] [--add-style CSS] [--json]
//
// For each pack it starts the dev API server with a rebuild that never ends (--rebuild-running: Data health says
// "Drives and charges are being rebuilt" and reads itself again, and every featured car gets a `rows` event every
// second) and live values every 500 ms. It opens each page at each size in Chromium, waits for it to be ready, then
// turns the mouse wheel down the page and back up in steps and records, in the page:
// - frame times: the time between animation frames (a frame over 50 ms is a stall the person sees);
// - long tasks: PerformanceObserver 'longtask' (main thread busy over 50 ms);
// and from a Chrome DevTools Protocol trace of the same scroll (Tracing.start): the longest main-thread task with
// what took its time (script, style, layout, paint), and the time spent painting and rastering.
// A page fails when a frame takes over --max-frame-ms (50). Exit code 0 only when every page passes.
// Sizes are a 4K screen by default: 3840x2160 CSS px at device scale 1, and 1920x1080 at device scale 2 (the same
// screen at 200 %). --add-style adds CSS to each page before the scroll, to find which rule makes painting slow.
// Made-up data only (the dev server's). Plain Node plus the installed Playwright.
'use strict';

const fs = require('fs');
const path = require('path');
const { startServer, killChildren, TILE_HOSTS, BLANK_TILE, ROUTES } = require('./check');

const UI_DIR = path.dirname(__dirname);
const PACKS = ['instrument', 'horizon', 'manager', 'console', 'pebble', 'logbook'];
const DEFAULT_PAGES = ['health', 'car', 'drives', 'charges', 'statistics', 'settings-server', 'logbook'];
const DEFAULT_SIZES = '3840x2160@1,1920x1080@2';
const REBUILD_ARGS = ['--rebuild-running', '--sse-interval-ms', '500', '--rows-every-s', '1'];
// the trace: main-thread tasks and paint, and raster (the cc, gpu and blink categories cost frames of their own)
const TRACE_CATS = ['devtools.timeline', 'disabled-by-default-devtools.timeline', 'toplevel'];

// animation frame times jitter by a few tenths of a ms: three 60 Hz frames (50.0 ms) can read 50.1
const FRAME_SLACK_MS = 0.5;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function parseArgs(argv) {
  const o = { packs: PACKS, pages: DEFAULT_PAGES, sizes: DEFAULT_SIZES, steps: 30, stepPx: 100, stepMs: 50, maxFrameMs: 50, rebuild: true, serverArgs: [], trace: null, addStyle: null, json: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const val = () => { if (i + 1 >= argv.length) throw new Error(`${a} needs a value`); return argv[++i]; };
    if (a === '--packs' || a === '--pack') { const v = val(); o.packs = v === 'all' ? PACKS : v.split(',').filter(Boolean); }
    else if (a === '--pages') o.pages = val().split(',').filter(Boolean);
    else if (a === '--sizes') o.sizes = val();
    else if (a === '--steps') o.steps = +val();
    else if (a === '--step-px') o.stepPx = +val();
    else if (a === '--step-ms') o.stepMs = +val();
    else if (a === '--max-frame-ms') o.maxFrameMs = +val();
    else if (a === '--quiet-server') o.rebuild = false;
    else if (a === '--server-args') o.serverArgs = val().split(/\s+/).filter(Boolean);
    else if (a === '--trace') o.trace = val();
    else if (a === '--add-style') o.addStyle = val();
    else if (a === '--json') o.json = true;
    else if (a === '-h' || a === '--help') o.help = true;
    else throw new Error(`unknown option ${a}`);
  }
  o.sizes = o.sizes.split(',').filter(Boolean).map((s) => {
    const m = /^(\d+)x(\d+)(?:@(\d+(?:\.\d+)?))?$/.exec(s);
    if (!m) throw new Error(`--sizes: ${s} is not WxH[@S]`);
    return { w: +m[1], h: +m[2], scale: m[3] ? +m[3] : 1, label: s.includes('@') ? s : `${s}@1` };
  });
  for (const p of o.pages) if (!ROUTES[p]) throw new Error(`no page ${p}`);
  return o;
}

// In the page, before its scripts: long tasks and animation frames, kept until __smScroll.take().
function scrollInit() {
  const S = window.__smScroll = { on: false, frames: [], long: [], last: 0 };
  try {
    new PerformanceObserver((list) => { if (S.on) for (const e of list.getEntries()) S.long.push({ at: e.startTime, ms: e.duration }); })
      .observe({ type: 'longtask', buffered: false });
  } catch (_) { /* no long task timing */ }
  const tick = (t) => { if (S.on) { if (S.last) S.frames.push(t - S.last); S.last = t; } requestAnimationFrame(tick); };
  requestAnimationFrame(tick);
  S.start = () => { S.frames = []; S.long = []; S.last = 0; S.on = true; };
  S.take = () => { S.on = false; return { frames: S.frames, long: S.long }; };
}

const pct = (xs, p) => { if (!xs.length) return 0; const s = [...xs].sort((a, b) => a - b); return s[Math.min(s.length - 1, Math.floor(p * s.length))]; };
const r1 = (x) => Math.round(x * 10) / 10;

// The trace's renderer main thread: the longest task (with its own biggest parts) and paint and raster time.
function readTrace(events) {
  const mains = new Set();
  for (const e of events) if (e.name === 'thread_name' && e.args && e.args.name === 'CrRendererMain') mains.add(`${e.pid}:${e.tid}`);
  const tasks = [], parts = {};
  let paint = 0, raster = 0;
  for (const e of events) {
    if (e.ph !== 'X' || e.dur == null) continue;
    const onMain = mains.has(`${e.pid}:${e.tid}`);
    if (e.name === 'RasterTask') raster += e.dur;
    if (!onMain) continue;
    if (e.name === 'RunTask' || e.name === 'ThreadControllerImpl::RunTask') tasks.push(e);
    if (e.name === 'Paint') paint += e.dur;
  }
  tasks.sort((a, b) => b.dur - a.dur);
  const top = tasks[0];
  if (top) {
    const end = top.ts + top.dur;
    for (const e of events) {
      if (e.ph !== 'X' || !mains.has(`${e.pid}:${e.tid}`) || e.ts < top.ts || e.ts > end || e === top) continue;
      if (['FunctionCall', 'EvaluateScript', 'v8.callFunction', 'UpdateLayoutTree', 'Layout', 'Paint', 'PrePaint', 'Layerize', 'Commit', 'ParseHTML', 'HitTest', 'EventDispatch', 'TimerFire', 'FireAnimationFrame', 'XHRReadyStateChange'].includes(e.name)) {
        parts[e.name] = (parts[e.name] || 0) + e.dur;
      }
    }
  }
  return {
    longestTaskMs: top ? r1(top.dur / 1000) : 0,
    tasksOver50: tasks.filter((t) => t.dur > 50000).length,
    longestParts: Object.fromEntries(Object.entries(parts).sort((a, b) => b[1] - a[1]).slice(0, 5).map(([k, v]) => [k, r1(v / 1000)])),
    paintMs: r1(paint / 1000), rasterMs: r1(raster / 1000),
  };
}

async function scrollOne(browser, server, o, pack, pageId, size) {
  const route = ROUTES[pageId][0].replace('{car}', 'c1');
  const context = await browser.newContext({ viewport: { width: size.w, height: size.h }, screen: { width: size.w, height: size.h }, deviceScaleFactor: size.scale, bypassCSP: !!o.addStyle });
  await context.route((u) => /^(https?|wss?):$/.test(u.protocol) && u.host !== server.host, (r) => {
    let u = null; try { u = new URL(r.request().url()); } catch (_) { /* odd */ }
    if (u && TILE_HOSTS.has(u.host)) return r.fulfill({ status: 200, contentType: 'image/png', headers: { 'access-control-allow-origin': '*' }, body: BLANK_TILE }).catch(() => {});
    return r.abort('blockedbyclient').catch(() => {});
  });
  const page = await context.newPage();
  await page.addInitScript(scrollInit);
  const res = { pack, page: pageId, size: size.label, route, ok: false, error: null };
  try {
    await page.goto(server.origin + route, { waitUntil: 'commit', timeout: 30000 });
    await page.waitForFunction((id) => document.documentElement.dataset.ready === id, pageId, { timeout: 30000, polling: 100 });
    // --add-style: more CSS on the page (to find which rule makes painting slow)
    if (o.addStyle) await page.addStyleTag({ content: o.addStyle });
    await sleep(1500); // the first live values and the first reloads of a rebuild
    const room = await page.evaluate(() => {
      // the element that scrolls: the document, else the tallest scrollable box
      let best = document.scrollingElement, extra = best.scrollHeight - best.clientHeight;
      for (const el of document.querySelectorAll('*')) {
        const cs = getComputedStyle(el);
        if (!/(auto|scroll)/.test(cs.overflowY)) continue;
        const x = el.scrollHeight - el.clientHeight;
        if (x > extra + 50) { best = el; extra = x; }
      }
      return { extra, dom: document.getElementsByTagName('*').length };
    });
    res.scrollable = room.extra;
    res.dom = room.dom;
    await page.mouse.move(size.w / 2, size.h / 2);
    const cdp = await context.newCDPSession(page);
    const trace = [];
    let traced = null;
    cdp.on('Tracing.dataCollected', (ev) => { for (const e of ev.value) trace.push(e); });
    const done = new Promise((r) => cdp.once('Tracing.tracingComplete', r));
    await cdp.send('Tracing.start', { traceConfig: { includedCategories: TRACE_CATS, recordMode: 'recordAsMuchAsPossible' }, transferMode: 'ReportEvents' });
    await page.evaluate(() => window.__smScroll.start());
    const t0 = Date.now();
    for (const dir of [1, -1]) {
      for (let i = 0; i < o.steps; i++) {
        await page.mouse.wheel(0, dir * o.stepPx);
        await sleep(o.stepMs);
      }
    }
    await sleep(300);
    const got = await page.evaluate(() => window.__smScroll.take());
    res.seconds = r1((Date.now() - t0) / 1000);
    await cdp.send('Tracing.end');
    await done;
    traced = readTrace(trace);
    if (o.trace) {
      fs.mkdirSync(o.trace, { recursive: true });
      fs.writeFileSync(path.join(o.trace, `${pack}-${pageId}-${size.label.replace('@', 'at')}.json`), JSON.stringify({ traceEvents: trace }));
    }
    const f = got.frames;
    Object.assign(res, {
      frames: f.length, maxFrameMs: r1(Math.max(0, ...f)), p95FrameMs: r1(pct(f, 0.95)), framesOver50: f.filter((x) => x > o.maxFrameMs + FRAME_SLACK_MS).length,
      longTasks: got.long.length, maxLongTaskMs: r1(Math.max(0, ...got.long.map((x) => x.ms))), trace: traced,
    });
    res.ok = res.maxFrameMs <= o.maxFrameMs + FRAME_SLACK_MS;
  } catch (e) {
    res.error = String(e.message || e).split('\n')[0];
  } finally {
    await context.close().catch(() => {});
  }
  return res;
}

async function run(o) {
  const { chromium } = require('playwright');
  const browser = await chromium.launch();
  const results = [];
  try {
    for (const pack of o.packs) {
      const server = await startServer(path.join(UI_DIR, 'packs', pack), [...(o.rebuild ? REBUILD_ARGS : []), ...o.serverArgs]);
      try {
        for (const pageId of o.pages) {
          for (const size of o.sizes) {
            const r = await scrollOne(browser, server, o, pack, pageId, size);
            results.push(r);
            process.stderr.write(`${line(r)}\n`);
          }
        }
      } finally { await server.stop(); }
    }
  } finally { await browser.close(); }
  return { pass: results.every((r) => r.ok), maxFrameMs: o.maxFrameMs, rebuild: o.rebuild, results };
}

function line(r) {
  const head = `${r.ok ? 'ok  ' : 'FAIL'} ${r.pack.padEnd(10)} ${r.page.padEnd(15)} ${r.size.padEnd(12)}`;
  if (r.error) return `${head} ${r.error}`;
  const t = r.trace || {};
  const parts = Object.entries(t.longestParts || {}).map(([k, v]) => `${k} ${v}`).join(', ');
  return `${head} frames ${String(r.frames).padStart(4)}  max ${String(r.maxFrameMs).padStart(6)} ms  p95 ${String(r.p95FrameMs).padStart(5)} ms  >50ms ${String(r.framesOver50).padStart(3)}  long tasks ${r.longTasks} (max ${r.maxLongTaskMs} ms)  task ${t.longestTaskMs} ms${parts ? ` [${parts}]` : ''}  paint ${t.paintMs} ms  raster ${t.rasterMs} ms  scroll ${r.scrollable} px  dom ${r.dom}`;
}

const HELP = fs.readFileSync(__filename, 'utf8').split('\n').slice(1).filter((l) => l.startsWith('//')).map((l) => l.slice(3)).join('\n') + '\n';

async function main() {
  let o;
  try { o = parseArgs(process.argv.slice(2)); } catch (e) { process.stderr.write(`scroll.js: ${e.message}\n`); return 2; }
  if (o.help) { process.stdout.write(HELP); return 0; }
  const rep = await run(o);
  if (o.json) process.stdout.write(JSON.stringify(rep, null, 2) + '\n');
  else {
    const bad = rep.results.filter((r) => !r.ok);
    process.stdout.write(`${rep.results.length - bad.length} of ${rep.results.length} pass (no frame over ${o.maxFrameMs} ms while scrolling${o.rebuild ? ', a rebuild running' : ''})\n`);
    for (const r of bad) process.stdout.write(`${line(r)}\n`);
  }
  return rep.pass ? 0 : 1;
}

if (require.main === module) {
  main().then((c) => { process.exitCode = c; }, (e) => { process.stderr.write(`scroll.js: ${e.stack || e}\n`); killChildren('SIGTERM'); process.exitCode = 2; });
}

module.exports = { run, parseArgs, readTrace, DEFAULT_PAGES, PACKS };
