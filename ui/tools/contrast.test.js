// Text that must be readable in both themes, in Chromium on the dev API server: the small words of a tag
// (Preferred, estimated, Ongoing), of the AC and DC pills of the Charges list, of an unnamed version in the
// Software page's strip and of a Logbook purpose's total that has no drives, measured on the pixels drawn (the box's most common colour is its background) against
// the text's colour: 4.5:1 or more (WCAG AA for small text).
// `PACKS=a,b` runs only those. Run: node --test ui/tools/contrast.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const zlib = require('zlib');
const { spawn } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const ONLY = process.env.PACKS ? new Set(process.env.PACKS.split(',')) : null;
const packs = BUILTIN_PACKS.filter((p) => p === 'instrument' && (!ONLY || ONLY.has(p)));
const SKIP = !playwright && 'Playwright is not installed';

function devServer(pack) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', '--freeze-clock'], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
    let out = '', err = '';
    const timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error(`no dev server in 60 s: ${err}`)); }, 60000);
    child.stderr.on('data', (d) => { err = (err + d).slice(-5000); });
    child.stdout.on('data', (d) => {
      out += d;
      const m = /listening on (http:\/\/\S+?)\/?\s/.exec(out);
      if (m) {
        clearTimeout(timer);
        resolve({ origin: m[1], stop: () => new Promise((r) => { child.once('exit', r); child.kill('SIGTERM'); }) });
      }
    });
    child.once('exit', (code) => { clearTimeout(timer); reject(new Error(`the dev server stopped (${code}): ${err}`)); });
  });
}

/** The pixels of a PNG (8 bit RGB or RGBA, not interlaced: what a screenshot is). */
function pixels(buf) {
  let at = 8, w = 0, h = 0, kind = 0;
  const parts = [];
  while (at < buf.length) {
    const len = buf.readUInt32BE(at), type = buf.toString('ascii', at + 4, at + 8), d = buf.subarray(at + 8, at + 8 + len);
    if (type === 'IHDR') { w = d.readUInt32BE(0); h = d.readUInt32BE(4); kind = d[9]; }
    if (type === 'IDAT') parts.push(d);
    at += 12 + len;
  }
  const n = kind === 6 ? 4 : 3, raw = zlib.inflateSync(Buffer.concat(parts)), line = w * n, out = Buffer.alloc(h * line);
  for (let y = 0; y < h; y++) {
    const f = raw[y * (line + 1)];
    for (let x = 0; x < line; x++) {
      const v = raw[y * (line + 1) + 1 + x], a = x >= n ? out[y * line + x - n] : 0, b = y ? out[(y - 1) * line + x] : 0, c = x >= n && y ? out[(y - 1) * line + x - n] : 0;
      let p = 0;
      if (f === 1) p = a; else if (f === 2) p = b; else if (f === 3) p = (a + b) >> 1;
      else if (f === 4) { const q = a + b - c, pa = Math.abs(q - a), pb = Math.abs(q - b), pc = Math.abs(q - c); p = pa <= pb && pa <= pc ? a : pb <= pc ? b : c; }
      out[y * line + x] = (v + p) & 255;
    }
  }
  return { count: w * h, n, data: out };
}
const lum = ([r, g, b]) => { const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }; return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b); };
const ratio = (a, b) => { const x = lum(a), y = lum(b); return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05); };

/** The contrast of the first visible `selector`: its text colour (with every opacity above it) on the colour most of its box has. */
async function contrastOf(page, selector) {
  const el = page.locator(selector).filter({ visible: true }).first();
  await el.waitFor({ timeout: 15000 });
  await el.scrollIntoViewIfNeeded();
  const ink = await el.evaluate((e) => {
    let opacity = 1;
    for (let x = e; x; x = x.parentElement) opacity *= +getComputedStyle(x).opacity;
    return { color: getComputedStyle(e).color, opacity, text: e.textContent.trim().slice(0, 30) };
  });
  const img = pixels(await el.screenshot());
  const seen = new Map();
  for (let i = 0; i < img.count; i++) { const k = `${img.data[i * img.n]},${img.data[i * img.n + 1]},${img.data[i * img.n + 2]}`; seen.set(k, (seen.get(k) || 0) + 1); }
  const bg = [...seen.entries()].sort((a, b) => b[1] - a[1])[0][0].split(',').map(Number);
  const m = /rgba?\(([^)]+)\)/.exec(ink.color)[1].split(/[,\s/]+/).map(Number);
  const alpha = (m[3] == null || Number.isNaN(m[3]) ? 1 : m[3]) * ink.opacity;
  const fg = [0, 1, 2].map((i) => m[i] * alpha + bg[i] * (1 - alpha));
  return { ratio: ratio(fg, bg), text: ink.text };
}

const PARTS = [
  ['/cars/c1/health', 'health', 'main .tag--live'],
  ['/cars/c1/drives', 'drives', 'main .tag--live'],
  ['/cars/c1/efficiency', 'efficiency', 'main .tag--warm'],
  ['/settings/integrations', 'settings-integrations', 'main .tag--danger'],
  ['/cars/c1/charges', 'charges', 'main .kind-pill--ac'],
  ['/cars/c1/charges', 'charges', 'main .kind-pill--dc'],
  ['/cars/c1/software', 'software', 'main .sw-fh-seg-bar.is-unnamed .sw-fh-seg-text'],
  ['/cars/c1/logbook', 'logbook', 'main .lb-total.is-zero .explain'],
  ['/cars/c1/logbook', 'logbook', 'main .lb-total.is-zero .lb-total-km .q-u'],
];

for (const pack of packs) {
  for (const theme of ['light', 'dark']) {
    test(`${pack}: in the ${theme} theme the words of tags, of the AC and DC pills and of an empty Logbook total reach 4.5:1`, { skip: SKIP, timeout: 240000 }, async () => {
      const browser = await playwright.chromium.launch();
      const server = await devServer(pack);
      try {
        const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
        await context.route(/^https?:\/\/(?!127\.0\.0\.1)/, (r) => r.abort());   // nothing leaves the machine
        const saved = await context.request.patch(`${server.origin}/api/v1/me/preferences`, { headers: { 'X-Sentry-Mate': '1' }, data: { theme } });
        assert.equal(saved.status(), 200);
        const page = await context.newPage();
        const low = [];
        let at = null;
        for (const [route, id, selector] of PARTS) {
          if (at !== route) {
            await page.goto(server.origin + route);
            await page.waitForFunction((id) => document.documentElement.dataset.ready === id, id, { timeout: 30000 });
            assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), theme);
            await page.waitForTimeout(500);
            at = route;
          }
          const c = await contrastOf(page, selector);
          if (c.ratio < 4.5) low.push(`${route} ${selector} "${c.text}": ${c.ratio.toFixed(2)}:1`);
        }
        assert.deepEqual(low, []);
      } finally {
        await server.stop();
        await browser.close();
      }
    });
  }
}
