// Settings: Server and access in both built-in packs, in Chromium on the dev API server, at phone size:
//   - the CAN Bridge upload address breaks only after a slash or a colon, never at the hyphen of a host name;
//   - the names in the certificate start with localhost, as the real server's do;
//   - with the owner's own certificate (--own-certificate) the page offers no "Make a new certificate" (the server
//     refuses it there) and says how to change the files instead, also in the "certificate expires" row.
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
