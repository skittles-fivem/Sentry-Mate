// Settings: Server and access in both built-in packs, in Chromium on the dev API server, at phone size:
//   - the CAN Bridge upload address breaks only after a slash or a colon, never at the hyphen of a host name;
//   - the names in the certificate start with localhost, as the real server's do;
//   - with the owner's own certificate (--own-certificate) the page offers no "Make a new certificate" (the server
//     refuses it there) and says how to change the files instead, also in the "certificate expires" row.
// And its Storage panel in all six packs, at desktop size (admins only):
//   - the categories of GET /system/storage/detail are listed largest first, with the total, the free space, what
//     compaction saved and its last pass, and the sentence that says compaction is lossless;
//   - "Compact now" posts, the progress shows (the step of three, days done of the total, what the pass freed so far,
//     the estimate and the last progress, read again every 2 s),
//     and when the pass ends the list is drawn again (the day packs first) and a note says what was freed;
//   - a failed pass shows the server's error; a watch account gets no panel and asks for no detail;
//   - the reading stops when the page is left while a pass runs.
// Run: node --test ui/tools/settings-server.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

function devServer(pack, args = []) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', '--freeze-clock', ...args], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

async function openPage(browser, server) {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2 });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.goto(`${server.origin}/settings/server`);
  await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-server', null, { timeout: 30000 });
  return { page, context, errors };
}

// Where the upload address breaks: the character before each new line.
function breaksOf(page) {
  return page.evaluate(() => {
    const el = [...document.querySelectorAll('dt')].find((d) => /upload address/i.test(d.textContent))?.nextElementSibling?.querySelector('code');
    if (!el) return null;
    const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    const chars = [];
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      for (let i = 0; i < n.data.length; i++) {
        const r = document.createRange();
        r.setStart(n, i); r.setEnd(n, i + 1);
        const rect = r.getClientRects()[0];
        if (rect) chars.push({ c: n.data[i], top: Math.round(rect.top) });
      }
    }
    const before = [];
    for (let i = 1; i < chars.length; i++) if (chars[i].top > chars[i - 1].top + 2) before.push(chars[i - 1].c);
    return { text: chars.map((x) => x.c).join(''), before };
  });
}

const visibleText = (page) => page.evaluate(() => document.querySelector('main, #app, body').innerText);
const certButtons = (page) => page.evaluate(() => [...document.querySelectorAll('button')].filter((b) => /make a new certificate/i.test(b.textContent)).length);

for (const pack of ['instrument', 'logbook']) {
  test(`${pack}: Settings: Server at phone size, the server's certificate and the owner's own`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    try {
      // a certificate the server made: the address breaks only after / or :, localhost in the names, the button
      let server = await devServer(pack);
      try {
        const { page, context, errors } = await openPage(browser, server);
        const b = await breaksOf(page);
        assert.ok(b, 'the upload address is on the page');
        assert.match(b.text, /^https:\/\/[a-z0-9.-]+-[a-z0-9.-]+:\d+\/api\/can\/logs$/, 'a host name with a hyphen');
        assert.ok(b.before.length >= 1, `it wraps at 390 px: ${b.text}`);
        for (const c of b.before) assert.ok(c === '/' || c === ':', `a break after ${JSON.stringify(c)} in ${b.text}`);
        const names = await page.evaluate(() => [...document.querySelectorAll('.ss-cert-names li')].map((li) => li.textContent.trim()));
        assert.equal(names[0], 'localhost', `names in it: ${names}`);
        assert.ok(await certButtons(page) >= 1, 'Make a new certificate is offered');
        assert.doesNotMatch(await visibleText(page), /certificate expires/i, 'a certificate the server made does not expire soon');
        assert.deepEqual(errors, []);
        await context.close();
      } finally { await server.stop(); }

      // the owner's own certificate: no button that can only fail, the files to change instead
      server = await devServer(pack, ['--own-certificate']);
      try {
        const { page, context, errors } = await openPage(browser, server);
        assert.equal(await certButtons(page), 0, 'no Make a new certificate with the owner\'s own certificate');
        const text = await visibleText(page);
        assert.match(text, /certificate expires/i, 'the expiring row');
        assert.doesNotMatch(text, /button below|with Make a new certificate/i, 'it does not point to a button that fails');
        assert.match(text, /your own certificate/i);
        assert.match(text, /key\.pem/);
        assert.deepEqual(errors, []);
        await context.close();
      } finally { await server.stop(); }
    } finally {
      await browser.close();
    }
  });
}

/* ------------------------------------------------------------------ the Storage panel, in all six packs */

const DETAIL = /\/api\/v1\/system\/storage\/detail(\?|$)/;
async function openDesk(browser, server) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  const errors = [], reads = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('request', (r) => { if (DETAIL.test(r.url())) reads.push(Date.now()); });
  return { page, context, errors, reads };
}
const ready = (page, id = 'settings-server') => page.waitForFunction((want) => document.documentElement.dataset.ready === want, id, { timeout: 30000 });
// the page's text with every run of white space as one space (a figure and its unit are separate elements)
const bodyText = (page) => page.evaluate(() => document.body.innerText.replace(/\s+/g, ' '));
const compactButton = (page) => page.getByRole('button', { name: /compact now/i });

for (const pack of ['instrument', 'logbook', 'horizon', 'console', 'pebble', 'manager']) {
  test(`${pack}: Settings: Server, the Storage panel and Compact now`, { skip: !playwright && 'Playwright is not installed', timeout: 240000 }, async () => {
    const browser = await playwright.chromium.launch();
    try {
      let server = await devServer(pack);
      try {
        // idle: the categories largest first, the figures, the lossless sentence
        let { page, context, errors, reads } = await openDesk(browser, server);
        await page.goto(`${server.origin}/settings/server`);
        await ready(page);
        let text = await bodyText(page);
        const raw = text.indexOf('CAN logs as uploaded (signed originals)');
        const packs = text.indexOf('CAN logs and sidecars as uploaded, packed by day');
        assert.ok(raw >= 0 && packs > raw, 'the logs as uploaded come first, then the day packs');
        assert.ok(text.indexOf('Offline map tiles') > packs, 'largest first');
        assert.match(text, /kept answers and records/i, 'the group in plain words');
        assert.doesNotMatch(text, /\bkv\b/, 'no jargon');
        assert.match(text, /saved so far|saved 830 MB so far/i);
        assert.match(text, /830 MB/, 'saved so far');
        assert.match(text, /9\.5 MB freed/i, 'what the last pass freed');
        assert.match(text, /runs by itself when the server is idle/i);
        assert.match(text, /442 GB/, 'the free space');
        assert.match(text, /21 GB/, 'the total on disk');
        assert.match(text, /losslessly: every value stays readable exactly, and the logs[’'] signed originals are kept byte for byte/i);
        assert.equal(reads.length, 1, 'idle: the detail is read once');

        // Compact now: progress, then the list again and what was freed
        await compactButton(page).click();
        await page.waitForFunction(() => /Step 1 of 3[^\n]*CAN logs/i.test(document.body.innerText) && /[\d,.]+ of [\d,.]+ days/.test(document.body.innerText), null, { timeout: 15000 });
        text = await bodyText(page);
        assert.match(text, /Freed so far in this pass/i, 'what the pass freed so far');
        assert.match(text, /left in this step/i, 'the estimate');
        assert.match(text, /Last step [\d,.]+\s*s ago/i, 'when it last got on');
        assert.ok(await compactButton(page).isDisabled(), 'no second pass while one runs');
        await page.waitForFunction(() => /Compaction is done: 3\.0\sGB freed on disk\./.test(document.body.innerText), null, { timeout: 30000 });
        text = await bodyText(page);
        assert.ok(text.indexOf('CAN logs and sidecars as uploaded, packed by day') < text.indexOf('CAN logs as uploaded (signed originals)'), 'the list was drawn again: the day packs are the largest now');
        assert.match(text, /3\.0 GB freed/i, 'the last pass');
        assert.match(text, /3\.9 GB/, 'saved so far grew');
        assert.ok(await compactButton(page).isEnabled());
        const n = reads.length;
        await page.waitForTimeout(4500);
        assert.equal(reads.length, n, 'no reading once the pass is done');

        // a second pass finds nothing left; leaving the page while it runs stops the reading
        await compactButton(page).click();
        await page.waitForFunction(() => /Compaction is done: nothing was left to pack\./.test(document.body.innerText), null, { timeout: 30000 });
        assert.match(await bodyText(page), /nothing left to pack/i);
        await compactButton(page).click();
        await page.waitForFunction(() => /Step \d of 3/i.test(document.body.innerText), null, { timeout: 15000 });
        await page.evaluate(() => { history.pushState({}, '', '/settings/display'); dispatchEvent(new PopStateEvent('popstate')); });
        await ready(page, 'settings-display');
        await page.waitForTimeout(2500);   // a read already under way may still land
        const left = reads.length;
        await page.waitForTimeout(5000);
        assert.equal(reads.length, left, 'the page was left: nothing reads the detail any more');
        assert.deepEqual(errors, []);
        await context.close();

        // a failed pass says why (the answer is changed on its way to the page)
        ({ page, context, errors, reads } = await openDesk(browser, server));
        await page.route(DETAIL, async (route) => {
          const res = await route.fetch();
          const d = await res.json();
          Object.assign(d.compaction, { status: 'failed', step: null, error: 'No space left on the disk (made up).' });
          await route.fulfill({ response: res, json: d });
        });
        await page.goto(`${server.origin}/settings/server`);
        await ready(page);
        assert.match(await bodyText(page), /No space left on the disk \(made up\)/);
        assert.ok(await compactButton(page).isEnabled(), 'a failed pass can be started again');
        assert.deepEqual(errors, []);
        await context.close();
      } finally { await server.stop(); }

      // a watch account: no panel, no button, no read
      server = await devServer(pack, ['--role', 'watch']);
      try {
        const { page, context, errors, reads } = await openDesk(browser, server);
        await page.goto(`${server.origin}/settings/server`);
        await ready(page);
        assert.equal(await compactButton(page).count(), 0, 'no Compact now for a watch account');
        assert.doesNotMatch(await bodyText(page), /signed originals/);
        assert.equal(reads.length, 0);
        assert.deepEqual(errors, []);
        await context.close();
      } finally { await server.stop(); }
    } finally {
      await browser.close();
    }
  });
}
