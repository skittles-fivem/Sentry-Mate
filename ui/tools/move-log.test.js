// The move's log in every built-in pack built on Instrument, in Chromium on the dev API server: on the new server's
// "Moving here" page (set-up flow, `--fresh`) and on the old server's move panel (Settings: Server and access,
// `--move-state copying`). Lines come in as the move goes, newest last, each with a time, the list scrolled to its
// newest line; a reload shows the lines again at once; "Download log" saves plain text (`YYYY-MM-DD HH:MM:SS UTC`,
// two spaces, the words); no line holds a code, key or path; nothing is wider than the window at 390 px. (After the
// move is done, a new visit to the address signs in: the log is shown to the page that followed the move.)
// Screenshots at 1440x900 and 390x844 go to MOVE_LOG_SHOTS when it is set (a folder; nothing is written otherwise).
// Run: node --test ui/tools/move-log.test.js. Skipped when Playwright is not installed.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { spawn, execFileSync } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }
const SHOTS = process.env.MOVE_LOG_SHOTS || null;
const PACKS = BUILTIN_PACKS.filter((p) => fs.existsSync(path.join(UI_DIR, 'packs', p, 'web', 'pages', 'move-log.js')));

function devServer(pack, args) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', ...args], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

/** A move code the dev server takes (its check is the crate's). */
function moveCode() {
  return execFileSync('python3', ['-c', 'from mockapi.resources.moving import make_move_code; print(make_move_code()[0])'],
    { cwd: path.join(UI_DIR, 'devserver') }).toString().trim();
}

const lines = (page) => page.evaluate(() => [...document.querySelectorAll('.mlog .mlog-line')].map((li) => ({
  time: li.querySelector('time')?.textContent || '', at: li.querySelector('time')?.getAttribute('datetime') || '',
  text: li.querySelector('.mlog-text')?.textContent || '' })));

async function waitLine(page, re, ms = 40000) {
  await page.waitForFunction((src) => [...document.querySelectorAll('.mlog .mlog-text')].some((e) => new RegExp(src).test(e.textContent)), re.source, { timeout: ms });
}

function checkLines(got) {
  assert.ok(got.length > 0, 'no line');
  const at = got.map((l) => Date.parse(l.at));
  assert.deepEqual([...at].sort((a, b) => a - b), at, 'oldest first, newest last');
  for (const l of got) {
    assert.match(l.time, /^\d\d:\d\d:\d\d$/, `a time on each line: ${JSON.stringify(l)}`);
    const words = l.text.replace(/192\.0\.2\.\d+(:\d+)?/g, '');
    assert.doesNotMatch(words, /SMMV-|SMRK-|SETUP-|[0-9a-f]{32}|\//i, `nothing secret: ${l.text}`);
  }
}

async function checkDownload(page) {
  const [dl] = await Promise.all([page.waitForEvent('download'), page.locator('.mlog .mlog-dl').click()]);
  assert.match(dl.suggestedFilename(), /^sentry-mate-move.*log\.txt$/);
  const text = fs.readFileSync(await dl.path(), 'utf8');
  const rows = text.trimEnd().split('\n');
  assert.equal(rows.length, (await lines(page)).length, 'every line in the file');
  for (const r of rows) assert.match(r, /^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d UTC {2}\S/, r);
}

async function atBottom(page) {
  return page.evaluate(() => { const l = document.querySelector('.mlog-list'); return l.scrollHeight - l.scrollTop - l.clientHeight < 8; });
}

async function noSideScroll(page) {
  return page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1);
}

async function shot(page, name) {
  if (!SHOTS) return;
  fs.mkdirSync(SHOTS, { recursive: true });
  await page.locator('.mlog').scrollIntoViewIfNeeded();
  await page.screenshot({ path: path.join(SHOTS, name), fullPage: false });
}

for (const pack of PACKS) {
  test(`${pack}: "Moving here" shows the move's log as it goes, after a reload too, with a plain-text download`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const server = await devServer(pack, ['--fresh']);
    const browser = await playwright.chromium.launch();
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, acceptDownloads: true });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      await page.goto(`${server.origin}/`);
      const code = page.locator('main input[name=setup_code]');
      await code.waitFor({ timeout: 15000 });
      await code.fill('SETUP-DEV-0000');
      await page.locator('main button[type=submit]').click();
      await page.locator('main button.su-choice', { hasText: 'Move here from another server' }).click();
      await page.fill('main input[name=from]', '192.0.2.20');
      await page.fill('main input[name=code]', moveCode());
      await page.locator('main button[type=submit]').click();
      await waitLine(page, /^Moving here from 192\.0\.2\.20/, 10000);
      await waitLine(page, /^Copying file \d+ of \d+ \(/);
      const early = (await lines(page)).length;
      await waitLine(page, /^Round 1 done/);
      assert.ok((await lines(page)).length > early, 'lines come in without a reload');
      assert.ok(await atBottom(page), 'the newest line is in view');
      checkLines(await lines(page));
      await shot(page, 'move-here-1440x900.png');
      // a reload: the lines are there at once (the claim cookie holds the job)
      const before = await lines(page);
      await page.reload();
      await page.locator('.mlog .mlog-line').first().waitFor({ timeout: 10000 });
      const after = await lines(page);
      assert.deepEqual(after.slice(0, before.length).map((l) => l.text), before.map((l) => l.text), 'the same lines after a reload');
      // on a phone: the same log, nothing wider than the window (the move goes on meanwhile)
      await page.setViewportSize({ width: 390, height: 844 });
      await page.waitForTimeout(300);
      checkLines(await lines(page));
      assert.ok(await noSideScroll(page), 'nothing wider than the phone');
      await shot(page, 'move-here-390x844.png');
      await page.setViewportSize({ width: 1440, height: 900 });
      await waitLine(page, /^Inputs started here:/);
      const all = await lines(page);
      for (const step of [/^Connected:/, /^Last-round pause/, /^Switching/, /^Switched:/, /^Inputs started here:/]) {
        assert.ok(all.some((l) => step.test(l.text)), `no ${step} in ${JSON.stringify(all.map((l) => l.text))}`);
      }
      await checkDownload(page);
      assert.deepEqual(errors, []);
    } finally {
      await browser.close();
      await server.stop();
    }
  });

  test(`${pack}: the old server's move panel shows the move's log up to Done, with a plain-text download`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const server = await devServer(pack, ['--move-state', 'copying']);
    const browser = await playwright.chromium.launch();
    try {
      for (const [w, hgt] of [[1440, 900], [390, 844]]) {
        const context = await browser.newContext({ viewport: { width: w, height: hgt }, acceptDownloads: true });
        const page = await context.newPage();
        const errors = [];
        page.on('pageerror', (e) => errors.push(e.message));
        await page.goto(`${server.origin}/settings/server`);
        await page.locator('.mlog .mlog-line').first().waitFor({ timeout: 20000 });
        await waitLine(page, /^Done:/);
        const all = await lines(page);
        checkLines(all);
        for (const step of [/^A move code was made/, /connected/, /^Sent \d+ %/, /^Last-round pause/, /copy matches/, /^Inputs stopped on this server:/, /^Done:/]) {
          assert.ok(all.some((l) => step.test(l.text)), `no ${step} in ${JSON.stringify(all.map((l) => l.text))}`);
        }
        assert.ok(await atBottom(page), 'the newest line is in view');
        if (w === 1440) {
          await checkDownload(page);
          await page.reload();
          await page.locator('.mlog .mlog-line').first().waitFor({ timeout: 15000 });
          assert.equal((await lines(page)).length, all.length, 'the same lines after a reload');
        } else {
          assert.ok(await noSideScroll(page), 'nothing wider than the phone');
        }
        await shot(page, `old-server-move-${w}x${hgt}.png`);
        assert.deepEqual(errors, []);
      }
    } finally {
      await browser.close();
      await server.stop();
    }
  });
}
