// Data health and the timeline of every built-in pack (Instrument; the others are archived) in Chromium on the dev
// API server: each reason a stretch of time is unknown is said as it is. A stored log that could not be decoded
// (`unreadable`) and a log that names another car's VIN (`other_vin`) are not "the log failed its check" (that is
// `failed_check`: a signature, hash or chain that did not hold), and no reason is shown as its code.
// Run: node --test ui/tools/health-gap-reasons.test.js. Skipped when Playwright is not installed.
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

// every word the server has (docs/ui-api.md, `gap_reason`), and words its label must have
const WORDS = {
  no_log: /no log/i,
  not_recorded: /paused|not recording/i,
  failed_check: /failed its check/i,
  unreadable: /could not be read/i,
  other_vin: /another car/i,
  dropped_frames: /frames/i,
};
// the dev server's car that has a gap of the reason
const CARS = { c1: ['no_log', 'not_recorded', 'failed_check', 'dropped_frames'], c2: ['unreadable'], c3: ['other_vin'] };

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

for (const pack of BUILTIN_PACKS) {
  test(`${pack}: Data health and the timeline say each reason for unknown time in its own words`, { skip: !playwright && 'Playwright is not installed', timeout: 240000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      const seen = new Set();
      for (const [car, reasons] of Object.entries(CARS)) {
        const answers = [];
        const onResponse = async (res) => {
          const u = new URL(res.url());
          if (u.pathname === `/api/v1/cars/${car}/health` && res.ok()) answers.push(await res.json().catch(() => null));
        };
        page.on('response', onResponse);
        await page.goto(`${server.origin}/cars/${car}/health?preset=all`);
        await page.waitForFunction(() => document.documentElement.dataset.ready === 'health', null, { timeout: 30000 });
        await page.waitForLoadState('networkidle', { timeout: 20000 });
        page.off('response', onResponse);
        const hl = answers.filter(Boolean).pop();
        assert.ok(hl, `the page read ${car}'s health`);
        const lines = await page.evaluate(() => [...document.querySelectorAll('.hl-span-list li')].map((li) => li.textContent));
        assert.equal(lines.length, hl.unknown_spans.length, `${car}: one line per unknown span`);
        for (const reason of reasons) {
          const n = hl.unknown_spans.filter((s) => s.reason === reason).length;
          assert.ok(n > 0, `${car}: the dev server has a span that is ${reason}`);
          assert.equal(lines.filter((l) => WORDS[reason].test(l)).length, n, `${car}: ${reason} is said in its words on ${n} lines of\n${lines.join('\n')}`);
          seen.add(reason);
        }
        for (const l of lines) {
          assert.doesNotMatch(l, /[a-z]_[a-z]/, `${car}: no reason is shown as its code`);
          const said = Object.keys(WORDS).filter((k) => WORDS[k].test(l));
          assert.equal(said.length, 1, `${car}: one reason per line: ${l}`);
        }
        // the gaps table says the same reason under its cause
        await page.waitForSelector('.hl-gaps tbody tr', { state: 'attached', timeout: 20000 });
        const gaps = await page.evaluate(() => [...document.querySelectorAll('.hl-gaps tbody tr')].map((tr) => tr.textContent));
        for (const reason of reasons) assert.ok(gaps.some((g) => WORDS[reason].test(g)), `${car}: a gap of the table is ${reason} in its words:\n${gaps.join('\n')}`);
        for (const g of gaps) assert.doesNotMatch(g, /\b(no_log|not_recorded|failed_check|unreadable|other_vin|dropped_frames)\b/, `${car}: a gap's reason is not shown as its code`);

        // the timeline's gap rows
        const rows = [];
        const onRows = async (res) => {
          const u = new URL(res.url());
          if (u.pathname === `/api/v1/cars/${car}/timeline` && res.ok()) rows.push(...((await res.json().catch(() => null))?.items || []));
        };
        page.on('response', onRows);
        await page.goto(`${server.origin}/cars/${car}/timeline?preset=all&kinds=gap`);
        await page.waitForFunction(() => document.documentElement.dataset.ready === 'timeline', null, { timeout: 30000 });
        await page.waitForLoadState('networkidle', { timeout: 20000 });
        page.off('response', onRows);
        const text = await page.evaluate(() => document.querySelector('main')?.textContent || document.body.textContent);
        for (const reason of reasons) {
          if (!rows.some((r) => r.gap_reason === reason)) continue;
          assert.match(text, WORDS[reason], `${car}: the timeline says a ${reason} gap in its words`);
        }
        if (!reasons.includes('failed_check') && !rows.some((r) => r.gap_reason === 'failed_check')) {
          assert.doesNotMatch(text, /failed its check/i, `${car}: no gap of the timeline is a log that failed its check`);
        }
      }
      assert.deepEqual([...seen].sort(), Object.keys(WORDS).sort(), 'every reason was looked at');
      assert.deepEqual(errors, []);
      await context.close();
    } finally {
      await server.stop();
      await browser.close();
    }
  });
}
