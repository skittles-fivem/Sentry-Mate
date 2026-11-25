// Settings: Integrations in Chromium on the dev API server, every built-in pack (Instrument; the others are archived):
// this server sets no request cap of its own, TeslaFi does the limiting. The TeslaFi section shows the requests of the
// day as information only (no "of 1,500", no cap per UTC day, no "at most ... a day"), and while TeslaFi limits the
// account (`rate_limited_until_ms`) says so with the time of the next try. The Overview tab's TeslaFi row names no cap.
// Run: node --test ui/tools/teslafi-limits.test.js. `PACKS=a,b` runs only those. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

const PACKS = (process.env.PACKS || BUILTIN_PACKS.join(',')).split(',').filter(Boolean);
// words of a cap of this server's own, which no page may say any more
const CAP_WORDS = /cap per UTC day|6,000|1,500|a day per account|used up/i;

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

/** The page at `url` once it is drawn, with `answers` ({ pathname: (body) => void }) changing what the API says. */
async function open(context, origin, url, answers = {}) {
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  for (const [pathname, f] of Object.entries(answers)) {
    await page.route((u) => u.pathname === pathname, async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      f(body);
      await route.fulfill({ response: res, json: body });
    });
  }
  await page.goto(origin + url);
  await page.waitForFunction(() => document.documentElement.dataset.ready === 'settings-integrations', null, { timeout: 30000 });
  return { page, errors };
}

for (const pack of PACKS) {
  test(`${pack}: the TeslaFi section shows the day's requests without a cap and says when TeslaFi limits`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const until = Date.now() + 10 * 60_000;
      // the real server's answers: no `daily_cap` in `polls`, and TeslaFi's 429 wait on the input and the account
      const limit = (b) => { delete b.polls.daily_cap; b.rate_limited_until_ms = until; };
      const accounts = (b) => { for (const a of b.items) a.rate_limited_until_ms = until; };
      const { page, errors } = await open(context, server.origin, '/settings/integrations?tab=details#teslafi', {
        '/api/v1/integrations/teslafi': limit,
        '/api/v1/integrations/teslafi/accounts': accounts,
      });
      await page.waitForFunction(() => /Requests today/.test(document.getElementById('teslafi')?.innerText || ''), null, { timeout: 30000 });
      const text = await page.evaluate(() => document.getElementById('teslafi').innerText);
      assert.doesNotMatch(text, CAP_WORDS, `${pack}: the TeslaFi section names a cap of this server's own:\n${text}`);
      assert.doesNotMatch(text, /Requests today\s*\n?\s*\d[\d,]* of /, `${pack}: the requests are shown as "n of cap"`);
      assert.match(text, /TeslaFi is limiting requests, next try at /, `${pack}: the wait is told:\n${text}`);
      assert.match(text, /no daily limit of its own|no cap on this side/, `${pack}: it says the limit is TeslaFi's`);
      assert.deepEqual(errors, []);
      await page.close();

      // the Overview tab's TeslaFi row: requests since midnight UTC, no cap
      const o = await open(context, server.origin, '/settings/integrations', {
        '/api/v1/integrations/overview': (b) => { for (const c of b.connections) if (c.kind === 'teslafi') c.today.cap = null; },
      });
      const row = await o.page.waitForFunction(() => {
        const r = [...document.querySelectorAll('.co-row')].find((x) => /TeslaFi/.test(x.innerText));
        return r ? r.innerText : null;
      }, null, { timeout: 30000 }).then((x) => x.jsonValue());
      assert.doesNotMatch(row, CAP_WORDS, `${pack}: the TeslaFi row names a cap:\n${row}`);
      assert.match(row, /since 0:00 UTC/, `${pack}: the row counts the requests since midnight:\n${row}`);
      assert.deepEqual(o.errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
