// Text that must be readable in both themes, in Chromium on the dev API server: the small words of a tag
// (Preferred, estimated, Ongoing), of the AC and DC pills of the Charges list, of an unnamed version in the
// Software page's strip, of a Logbook purpose's total that has no drives, of a warning in amber (drives without a
// purpose, stale cars, an estimate's mark, a middling efficiency), of a problem in red and of a link or a good word
// in teal, measured on the pixels drawn (the box's most common colour is its background) against
// the text's colour: 4.5:1 or more (WCAG AA for small text).
// Then the labels that sit on a fill or over a map (the B pin of a route and the A pin, the primary button's
// label, also while pointed at), and a sweep: every text of every page and of the sign-in and setup screens, in
// both themes, against the pixels drawn behind its glyphs (see lowTexts below).
// `PACKS=a,b` runs only those. Run: node --test ui/tools/contrast.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const zlib = require('zlib');
const { spawn } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const { ROUTES, FRAME_SCREENS } = require('./check');
const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const ONLY = process.env.PACKS ? new Set(process.env.PACKS.split(',')) : null;
const packs = BUILTIN_PACKS.filter((p) => p === 'instrument' && (!ONLY || ONLY.has(p)));
const SKIP = !playwright && 'Playwright is not installed';

function devServer(pack, more = []) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', '--freeze-clock', ...more], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

// ---- every text on every page ----
// What is swept: every element that holds text of its own (a text node, an input's value or placeholder, an SVG
// text), on every page and frame screen, in both themes. The text's colour (with every opacity above it) is set
// against the pixels really drawn behind it: the page is drawn once more with the glyphs not painted (fills, tiles,
// gradients, the map and its glass stay), and the text must reach 4.5:1 on nine tenths of the pixels behind its
// glyph boxes (a hairline that crosses a word is not its background). Not swept: text a control that is turned off
// shows (WCAG leaves it out), text that is covered or clipped away, text drawn in a canvas.
const HIDE_GLYPHS = `*, *::before, *::after { -webkit-text-fill-color: transparent !important; text-shadow: none !important; text-decoration-color: transparent !important; caret-color: transparent !important; animation: none !important; transition: none !important; }
  ::placeholder { -webkit-text-fill-color: transparent !important; }
  svg text, svg tspan { fill: transparent !important; stroke: transparent !important; }`;
const STILL = '*, *::before, *::after { animation: none !important; transition: none !important; caret-color: transparent !important; }';

/** Every visible text of the page: its words, its colour and the boxes of its glyphs (page px). Run in the page. */
function textsOfPage(rootSelector) {
  const out = [];
  const roots = rootSelector ? [...document.querySelectorAll(rootSelector)] : [document.body];
  const rgba = (s) => { const m = /rgba?\(([^)]+)\)/.exec(s); if (!m) return null; const v = m[1].split(/[,\s/]+/).map(Number); return [v[0], v[1], v[2], v[3] == null || Number.isNaN(v[3]) ? 1 : v[3]]; };
  const name = (e) => {
    const parts = [];
    for (let x = e, i = 0; x && x !== document.body && i < 3; x = x.parentElement, i++) {
      const cls = typeof x.className === 'string' ? x.className : (x.className && x.className.baseVal) || '';
      parts.unshift(x.tagName.toLowerCase() + (cls.trim() ? '.' + cls.trim().split(/\s+/).join('.') : ''));
    }
    return parts.join(' > ');
  };
  const off = (e) => !!e.closest(':disabled, [aria-disabled="true"], [inert]');
  const shown = (e) => {
    let opacity = 1;
    for (let x = e; x; x = x.parentElement) {
      const cs = getComputedStyle(x);
      if (cs.display === 'none' || cs.visibility === 'hidden' || cs.visibility === 'collapse') return 0;
      opacity *= +cs.opacity;
    }
    return opacity;
  };
  // the part of a glyph box that can be seen: inside every clipping ancestor and the window, and on top there
  const visiblePart = (e, r) => {
    let [l, t, rr, b] = [r.left, r.top, r.right, r.bottom];
    for (let x = e.parentElement; x; x = x.parentElement) {
      const cs = getComputedStyle(x);
      if (/(hidden|auto|scroll|clip)/.test(cs.overflowX + cs.overflowY)) { const c = x.getBoundingClientRect(); l = Math.max(l, c.left); t = Math.max(t, c.top); rr = Math.min(rr, c.right); b = Math.min(b, c.bottom); }
    }
    l = Math.max(l, 0); t = Math.max(t, 0); rr = Math.min(rr, innerWidth); b = Math.min(b, innerHeight);
    if (rr - l < 2 || b - t < 4) return null;
    const top = document.elementFromPoint((l + rr) / 2, (t + b) / 2);
    if (!top || !(top === e || e.contains(top) || top.contains(e))) return null;
    return [l, t, rr, b];
  };
  const add = (e, text, color, rects, pseudo) => {
    const opacity = shown(e);
    if (!opacity || off(e) || !color) return;
    // a mark between two words that is hidden from a screen reader (the "/" of the trail) is an ornament, not text
    if (!/[\p{L}\p{N}]/u.test(text) && e.closest('[aria-hidden="true"]')) return;
    const boxes = [];
    for (const r of rects) { const v = visiblePart(e, r); if (v) boxes.push(v); }
    if (!boxes.length) return;
    const cs = getComputedStyle(e, pseudo || null);
    out.push({ name: name(e) + (pseudo || ''), text: text.trim().replace(/\s+/g, ' ').slice(0, 40), color: [color[0], color[1], color[2]], alpha: color[3] * opacity, boxes,
      size: parseFloat(cs.fontSize), weight: +cs.fontWeight || 400 });
  };
  for (const root of roots) {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) textNode(n);
  }
  function textNode(n) {
    if (!n.nodeValue.trim()) return;
    const e = n.parentElement;
    if (!e || /^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE|OPTION|TITLE)$/i.test(e.tagName)) return;
    const cs = getComputedStyle(e);
    const svg = e instanceof SVGElement;
    const range = document.createRange();
    range.selectNodeContents(n);
    // a glyph fill that is not the colour (a gradient cut to the letters) cannot be read from the style
    const fill = svg ? cs.fill : (cs.webkitTextFillColor || cs.color);
    add(e, n.nodeValue, rgba(fill), svg ? [e.getBoundingClientRect()] : [...range.getClientRects()]);
  }
  for (const e of roots.flatMap((r) => [...r.querySelectorAll('input, textarea, select')])) {
    if (/^(checkbox|radio|range|file|color|hidden)$/.test(e.type)) continue;
    const cs = getComputedStyle(e), r = e.getBoundingClientRect();
    const pad = [parseFloat(cs.paddingLeft) + parseFloat(cs.borderLeftWidth), parseFloat(cs.paddingTop) + parseFloat(cs.borderTopWidth), parseFloat(cs.paddingRight) + parseFloat(cs.borderRightWidth), parseFloat(cs.paddingBottom) + parseFloat(cs.borderBottomWidth)];
    const inner = { left: r.left + pad[0], top: r.top + pad[1], right: r.right - pad[2], bottom: r.bottom - pad[3] };
    const value = e.tagName === 'SELECT' ? (e.selectedOptions[0] ? e.selectedOptions[0].textContent : '') : e.value;
    if (value && value.trim()) add(e, value, rgba(cs.color), [inner]);
    else if (e.placeholder) { const ps = getComputedStyle(e, '::placeholder'); const c = rgba(ps.color); if (c) { c[3] *= +ps.opacity || 1; add(e, e.placeholder, c, [inner], '::placeholder'); } }
  }
  return out;
}

/** The texts of the page (or only those inside `rootSelector`) that do not reach 4.5:1 on what is drawn behind them, and how many were measured. */
async function lowTexts(page, rootSelector) {
  const sheet = (css) => page.evaluate((css) => { const s = new CSSStyleSheet(); s.replaceSync(css); document.adoptedStyleSheets = [...document.adoptedStyleSheets, s]; }, css);   // the page's rules let no style tag in
  const before = await page.evaluate(() => document.adoptedStyleSheets.length);
  await sheet(STILL);
  // the whole page in the window, so nothing waits below the fold
  const tall = await page.evaluate(() => Math.max(document.documentElement.scrollHeight, ...[...document.querySelectorAll('main, [data-scroll]')].map((e) => e.scrollHeight)));
  const size = page.viewportSize();
  if (!rootSelector && tall > size.height) { await page.setViewportSize({ width: size.width, height: Math.min(tall + 200, 8000) }); await page.waitForTimeout(400); }
  if (!rootSelector) await page.evaluate(() => window.scrollTo(0, 0));
  const texts = await page.evaluate(textsOfPage, rootSelector || null);
  await sheet(HIDE_GLYPHS);
  const png = await page.screenshot();
  await page.evaluate((n) => { document.adoptedStyleSheets = document.adoptedStyleSheets.slice(0, n); }, before);
  const dim = { w: png.readUInt32BE(16), h: png.readUInt32BE(20) };
  const img = pixels(png);
  const low = [];
  let measured = 0;
  for (const t of texts) {
    const ratios = [];
    let worstBg = null, worst = Infinity;
    for (const [l, top, r, b] of t.boxes) {
      // the middle of the line box: the glyphs' own band
      const trim = (b - top) * 0.15;
      for (let y = Math.max(0, Math.ceil(top + trim)); y < Math.min(dim.h, Math.floor(b - trim)); y++) {
        for (let x = Math.max(0, Math.ceil(l)); x < Math.min(dim.w, Math.floor(r)); x++) {
          const i = (y * dim.w + x) * img.n, bg = [img.data[i], img.data[i + 1], img.data[i + 2]];
          const fg = [0, 1, 2].map((k) => t.color[k] * t.alpha + bg[k] * (1 - t.alpha));
          const c = ratio(fg, bg);
          ratios.push(c);
          if (c < worst) { worst = c; worstBg = bg; }
        }
      }
    }
    if (!ratios.length) continue;
    measured++;
    ratios.sort((a, b) => a - b);
    const at = ratios[Math.floor(ratios.length * 0.1)];
    if (at < 4.5) {
      low.push({ name: t.name, text: t.text, ratio: at, color: t.color.map(Math.round).join(','), alpha: +t.alpha.toFixed(2), worstBg: worstBg.join(',') });
    }
  }
  return { low, measured };
}

const PARTS = [
  ['/cars/c1/health', 'health', 'main .tag--live'],
  ['/cars/c1/drives', 'drives', 'main .tag--live'],
  ['/cars/c1/efficiency', 'efficiency', 'main .tag--warm'],
  ['/settings/integrations', 'settings-integrations', 'main .tag--danger'],
  ['/cars/c1/charges', 'charges', 'main .kind-pill--ac'],
  ['/cars/c1/charges', 'charges', 'main .kind-pill--dc'],
  ['/cars/c1/software', 'software', 'main .sw-fh-seg-bar.is-unnamed .sw-fh-seg-text'],
  ['/cars/c3/software', 'software', 'main .sw-fh-seg-bar.is-inferred .sw-fh-seg-text'],
  ['/cars/c3/software', 'software', 'main .sw-ver--inferred'],
  ['/cars/c1/logbook', 'logbook', 'main .lb-total.is-zero .explain'],
  ['/cars/c1/logbook', 'logbook', 'main .lb-total.is-zero .lb-total-km .q-u'],
  // the accents as plain words: a warning (amber) and a link or a good word (teal)
  ['/cars/c1/logbook', 'logbook', 'main .lb-unset'],
  ['/', 'fleet', 'main .sub-warn'],
  ['/cars/c1/drives', 'drives', 'main .est-mark'],
  ['/cars/c1/efficiency', 'efficiency', 'main .ef-eff.is-mid'],
  ['/cars/c1/statistics', 'statistics', 'main a.link'],
  ['/travel', 'travel', 'main .tv-city-btn:not(.is-chosen)'],
  ['/system', 'health', 'main span.ok'],
  ['/settings/integrations', 'settings-integrations', 'main .co-row[data-status="problem"] .co-word'],   // a problem (red)
];
// Labels on a fill or on the map's glass: measured like the sweep (the pixels behind the glyphs), `hover` while
// pointed at.
const LABELS = [
  ['/cars/c1/drives/{drive}', 'drive', 'main .map .route-pin--b'],
  ['/cars/c1/drives/{drive}', 'drive', 'main .map .route-pin--a'],
  ['/cars/c1/drives/{drive}', 'drive', 'main .route-legend .route-pin--b'],
  ['/cars/c1/trip', 'trip', 'main .trip-map-legend .route-pin--b'],
  ['/places', 'places', 'main .page-actions a.btn--primary'],
  ['/places', 'places', 'main .page-actions a.btn--primary', 'hover'],
  ['/settings/cars/c1', 'settings-cars', 'main button.btn--sm.btn--primary'],
  ['/settings/maps', 'settings-maps', 'main .seg-btn[aria-pressed="true"] .tp-meta'],
  ['/cars/c1', 'car', 'main .src-via'],
];
const say = (route, l) => `${route} ${l.name} "${l.text}": ${l.ratio.toFixed(2)}:1 (rgb ${l.color}${l.alpha < 1 ? ` at ${l.alpha}` : ''} on rgb ${l.worstBg})`;

for (const pack of packs) {
  for (const theme of ['light', 'dark']) {
    test(`${pack}: in the ${theme} theme the words of tags, of the AC and DC pills, of an empty Logbook total, of a warning, of a problem and of a link reach 4.5:1`, { skip: SKIP, timeout: 240000 }, async () => {
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
        const drives = await (await context.request.get(`${server.origin}/api/v1/cars/c1/drives?limit=1`)).json();
        const drive = Object.values(drives).find(Array.isArray)[0].id;
        for (const [r, id, selector, how] of LABELS) {
          const route = r.replace('{drive}', drive);
          if (at !== route) {
            await page.goto(server.origin + route);
            await page.waitForFunction((id) => document.documentElement.dataset.ready === id, id, { timeout: 30000 });
            await page.waitForTimeout(500);
            at = route;
          }
          const el = page.locator(selector).filter({ visible: true }).first();
          await el.waitFor({ timeout: 15000 });
          await el.scrollIntoViewIfNeeded();
          if (how === 'hover') await el.hover(); else await page.mouse.move(0, 0);
          await page.waitForTimeout(300);
          const got = await lowTexts(page, selector);
          assert.ok(got.measured > 0, `${route} ${selector}: no text measured`);
          for (const l of got.low) low.push(say(`${route}${how ? ` (${how})` : ''}`, l));
        }
        assert.deepEqual(low, []);
      } finally {
        await server.stop();
        await browser.close();
      }
    });
  }
}

/** One finding per kind of text and colour (the lowest seen), with how often it was seen. */
function gather(seen, route, low) {
  for (const l of low) {
    const k = `${l.name}|${l.color}|${l.alpha}`;
    const was = seen.get(k);
    seen.set(k, { line: !was || l.ratio < was.ratio ? say(route, l) : was.line, ratio: Math.min(l.ratio, was ? was.ratio : Infinity), n: (was ? was.n : 0) + 1 });
  }
}

for (const pack of packs) {
  for (const theme of ['light', 'dark']) {
    test(`${pack}: in the ${theme} theme every text of every page and of the sign-in and setup screens reaches 4.5:1 on what is drawn behind it`, { skip: SKIP, timeout: 600000 }, async () => {
      const browser = await playwright.chromium.launch();
      const seen = new Map();
      let measured = 0;
      try {
        const server = await devServer(pack);
        try {
          const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: theme });
          await context.route(/^https?:\/\/(?!127\.0\.0\.1)/, (r) => r.abort());   // nothing leaves the machine
          const saved = await context.request.patch(`${server.origin}/api/v1/me/preferences`, { headers: { 'X-Sentry-Mate': '1' }, data: { theme } });
          assert.equal(saved.status(), 200);
          const first = async (list) => Object.values(await (await context.request.get(`${server.origin}/api/v1/cars/c1/${list}?limit=1`)).json()).find(Array.isArray)[0].id;
          const ids = { car: 'c1', drive: await first('drives'), charge: await first('charges') };
          for (const [id, routes] of Object.entries(ROUTES)) {
            for (const r of routes) {
              const route = r.replace(/\{(\w+)\}/g, (_, k) => ids[k]);
              const page = await context.newPage();
              await page.goto(server.origin + route);
              await page.waitForFunction((id) => document.documentElement.dataset.ready === id, id, { timeout: 30000 });
              assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), theme);
              await page.waitForTimeout(600);
              const got = await lowTexts(page);
              assert.ok(got.measured > 10, `${route}: only ${got.measured} texts measured`);
              measured += got.measured;
              gather(seen, route, got.low);
              await page.close();
            }
          }
        } finally {
          await server.stop();
        }
        for (const [id, screen] of Object.entries(FRAME_SCREENS)) {
          const server = await devServer(pack, screen.serverArgs);
          try {
            const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: theme });
            await context.route(/^https?:\/\/(?!127\.0\.0\.1)/, (r) => r.abort());
            const page = await context.newPage();
            await page.goto(server.origin + screen.route);
            await page.waitForFunction((id) => document.documentElement.dataset.ready === id, id, { timeout: 30000 });
            assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), theme);   // signed out: the device's theme
            await page.waitForTimeout(600);
            const got = await lowTexts(page);
            assert.ok(got.measured > 3, `${screen.route}: only ${got.measured} texts measured`);
            measured += got.measured;
            gather(seen, screen.route, got.low);
          } finally {
            await server.stop();
          }
        }
        assert.ok(measured > 2000, `only ${measured} texts measured`);
        assert.deepEqual([...seen.values()].map((s) => `${s.line}${s.n > 1 ? ` (${s.n} times)` : ''}`), []);
      } finally {
        await browser.close();
      }
    });
  }
}
