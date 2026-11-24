// What an open page costs while nothing is touched (docs/plans/performance.md, "The pages while they stay open"):
// a pack's page on the dev API server with live values coming in, left alone; every `--every` seconds the main
// thread's busy time, script, layout and style time, the heap after a collection, the DOM nodes and the listeners
// (Chromium's own counters), and the CPU time of all of the browser's processes (`ps`, so the canvas and the
// compositor count too).
//
//   node ui/tools/perf/idle.js --pack instrument [--page /cars/c1] [--seconds 120] [--every 30]
//        [--size 1440x900] [--server-args "--sse-interval-ms 500 --rows-every-s 3"] [--json]
//
// Made-up data only (the dev server's). Needs Node and the Playwright that is already installed.
'use strict';
const { spawn, execFileSync } = require('node:child_process');
const path = require('node:path');

function args() {
  const a = process.argv.slice(2);
  const o = { pack: null, page: '/cars/c1', seconds: 120, every: 30, size: '1440x900', serverArgs: '', json: false };
  for (let i = 0; i < a.length; i++) {
    if (a[i] === '--pack') o.pack = a[++i];
    else if (a[i] === '--page') o.page = a[++i];
    else if (a[i] === '--seconds') o.seconds = Number(a[++i]);
    else if (a[i] === '--every') o.every = Number(a[++i]);
    else if (a[i] === '--size') o.size = a[++i];
    else if (a[i] === '--server-args') o.serverArgs = a[++i];
    else if (a[i] === '--json') o.json = true;
    else throw new Error(`unknown option ${a[i]}`);
  }
  if (!o.pack) throw new Error('--pack NAME is needed (a built-in pack of ui/packs: instrument)');
  return o;
}

function startServer(pack, extra) {
  const root = path.resolve(__dirname, '..', '..', '..');
  const dir = path.isAbsolute(pack) ? pack : path.join(root, 'ui', 'packs', pack);
  const more = extra.split(/\s+/).filter(Boolean);
  const child = spawn('python3', [path.join(root, 'ui', 'devserver', 'serve.py'), '--pack', dir, '--port', '0', ...more], {
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  return new Promise((resolve, reject) => {
    let out = '';
    child.stdout.on('data', (d) => {
      out += d;
      const m = out.match(/listening on (http:\/\/[^\s]+)/);
      if (m) resolve({ child, base: m[1].replace(/\/$/, '') });
    });
    child.on('exit', (c) => reject(new Error(`the dev server stopped (${c})`)));
  });
}

// CPU seconds (user + system) of a process and everything under it.
function treeCpu(pid) {
  const rows = execFileSync('ps', ['-eo', 'pid=,ppid=,time='], { encoding: 'utf8' }).trim().split('\n');
  const kids = new Map();
  const secs = new Map();
  for (const r of rows) {
    const [p, pp, t] = r.trim().split(/\s+/);
    const parts = t.split(/[-:]/).map(Number);
    let s = 0;
    for (const x of parts) s = s * 60 + x;
    if (t.includes('-')) s = parts[0] * 86400 + parts[1] * 3600 + parts[2] * 60 + parts[3];
    secs.set(p, s);
    if (!kids.has(pp)) kids.set(pp, []);
    kids.get(pp).push(p);
  }
  let total = 0;
  const walk = (p) => { total += secs.get(p) || 0; for (const k of kids.get(p) || []) walk(k); };
  walk(String(pid));
  return total;
}

(async () => {
  const o = args();
  const { chromium } = require('playwright');
  const server = await startServer(o.pack, o.serverArgs);
  // (through a browser server, whose process id is known: the CPU time of its whole tree is read with `ps`)
  const own = await chromium.launchServer();
  const browser = await chromium.connect(own.wsEndpoint());
  const stop = async () => { await browser.close().catch(() => {}); await own.close().catch(() => {}); server.child.kill(); };
  try {
    const [w, h] = o.size.split('x').map(Number);
    const page = await browser.newPage({ viewport: { width: w, height: h } });
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Performance.enable');
    await page.goto(server.base + o.page, { waitUntil: 'networkidle' }).catch(() => {});
    await page.waitForTimeout(5000);
    const pid = own.process()?.pid;
    const read = async () => {
      await cdp.send('HeapProfiler.collectGarbage');
      const m = Object.fromEntries((await cdp.send('Performance.getMetrics')).metrics.map((x) => [x.name, x.value]));
      return {
        at: Date.now(), task: m.TaskDuration, script: m.ScriptDuration, layout: m.LayoutDuration, style: m.RecalcStyleDuration,
        heap: m.JSHeapUsedSize, nodes: m.Nodes, listeners: m.JSEventListeners, cpu: pid ? treeCpu(pid) : 0,
      };
    };
    const first = await read();
    const rows = [];
    let prev = first;
    for (let t = o.every; t <= o.seconds; t += o.every) {
      await page.waitForTimeout(o.every * 1000);
      const now = await read();
      const dt = (now.at - prev.at) / 1000;
      const pct = (a, b) => Math.round(((a - b) / dt) * 1000) / 10;
      rows.push({
        s: Math.round((now.at - first.at) / 1000), main_pct: pct(now.task, prev.task), script_pct: pct(now.script, prev.script),
        layout_pct: pct(now.layout, prev.layout), style_pct: pct(now.style, prev.style), browser_cpu_pct: pct(now.cpu, prev.cpu),
        heap_mb: Math.round(now.heap / 1e4) / 100, nodes: now.nodes, listeners: now.listeners,
      });
      prev = now;
    }
    const all = (prev.at - first.at) / 1000;
    const mean = (k) => Math.round(((prev[k] - first[k]) / all) * 1000) / 10;
    const sum = {
      pack: o.pack, page: o.page, size: o.size, seconds: Math.round(all), main_pct: mean('task'), script_pct: mean('script'),
      layout_pct: mean('layout'), style_pct: mean('style'), browser_cpu_pct: mean('cpu'),
      heap_mb_first: Math.round(first.heap / 1e4) / 100, heap_mb_last: Math.round(prev.heap / 1e4) / 100,
      nodes_first: first.nodes, nodes_last: prev.nodes, listeners_first: first.listeners, listeners_last: prev.listeners,
    };
    if (o.json) console.log(JSON.stringify({ summary: sum, rows }));
    else {
      console.log(`${o.pack} ${o.page} ${o.size}, ${sum.seconds} s: main thread ${sum.main_pct} % busy (script ${sum.script_pct} %, layout ${sum.layout_pct} %, style ${sum.style_pct} %), `
        + `the browser's processes ${sum.browser_cpu_pct} % of a core; heap ${sum.heap_mb_first} -> ${sum.heap_mb_last} MB, `
        + `nodes ${sum.nodes_first} -> ${sum.nodes_last}, listeners ${sum.listeners_first} -> ${sum.listeners_last}`);
      for (const r of rows) console.log(`  ${String(r.s).padStart(5)} s: main ${r.main_pct} %, browser ${r.browser_cpu_pct} %, heap ${r.heap_mb} MB, nodes ${r.nodes}, listeners ${r.listeners}`);
    }
  } finally {
    await stop();
  }
})().catch((e) => { console.error(e.message || e); process.exit(1); });
