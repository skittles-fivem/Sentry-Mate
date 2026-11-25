// A long virtual list prints every row, in Chromium on the dev API server: the logbook (shared/vlist.js) draws only
// the rows near its box, so the page printed those and blank paper. While the page prints (the print media query or
// beforeprint) every row of the list is drawn, and after it (afterprint, or the screen media again) the list is
// windowed once more at the place it was scrolled to; in normal use only the rows near the box are in the page.
// The print style sheet still makes the box and the table fit the paper.
// Run: node --test ui/tools/print-virtual-list.test.js. Skipped when Playwright is not installed.
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

function devServer(pack) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0', '--freeze-clock', '--drives', '600'], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

// the rows in the page: real rows, skeleton rows of rows still loading (the two spacers are neither)
const COUNTS = () => {
  const rows = [...document.querySelectorAll('table.vl-table > tbody > tr')];
  return {
    real: rows.filter((r) => !r.classList.contains('vl-gap') && !r.classList.contains('tbl-skel')).length,
    skel: rows.filter((r) => r.classList.contains('tbl-skel')).length,
    total: Number(document.querySelector('table.vl-table').getAttribute('aria-rowcount')) - 1,
    top: document.querySelector('.vl-box').scrollTop,
  };
};

for (const pack of BUILTIN_PACKS) {
  test(`${pack}: a long list draws every row for print and is windowed again after it`, { skip: !playwright && 'Playwright is not installed', timeout: 180000 }, async () => {
    const browser = await playwright.chromium.launch();
    const server = await devServer(pack);
    try {
      const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, deviceScaleFactor: 1 });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      await page.addInitScript(`window.__rows = ${COUNTS}`); // (the page's policy refuses eval, so no function from a string)
      await page.goto(`${server.origin}/cars/c1/logbook?preset=all`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'logbook', null, { timeout: 30000 });
      await page.waitForSelector('table.vl-table tbody tr:not(.vl-gap):not(.tbl-skel)', { timeout: 30000 });
      const everyRow = () => page.waitForFunction(() => { const c = window.__rows(); return c.real === c.total && c.skel === 0; }, null, { timeout: 60000 });
      const windowed = () => page.waitForFunction(() => { const c = window.__rows(); return c.real + c.skel < 60; }, null, { timeout: 10000 });

      // normal use: a window of the list, a scroll moves it, never every row
      const t = await page.evaluate(() => window.__rows());
      assert.ok(t.total >= 300, `a long list (${t.total} rows)`);
      assert.ok(t.real > 0 && t.real + t.skel < 60, `windowed on the screen: ${t.real} rows of ${t.total}`);
      await page.evaluate(() => { document.querySelector('.vl-box').scrollTop = 200 * 55; });
      await page.waitForTimeout(500);
      const mid = await page.evaluate(() => window.__rows());
      assert.ok(mid.real + mid.skel < 60 && mid.top > 0, `still windowed after a scroll: ${mid.real + mid.skel} rows`);

      // the page prints (the browser's own events): every row, whatever the window was
      await page.evaluate(() => window.dispatchEvent(new Event('beforeprint')));
      const during = await page.evaluate(() => window.__rows());
      assert.equal(during.real + during.skel, during.total, 'beforeprint: a row for every one of the list, at once');
      await everyRow();
      await page.evaluate(() => window.dispatchEvent(new Event('afterprint')));
      const after = await page.evaluate(() => window.__rows());
      assert.ok(after.real + after.skel < 60, `afterprint: windowed again (${after.real + after.skel} rows)`);
      assert.ok(Math.abs(after.top - mid.top) < 2, `afterprint: scrolled where it was (${after.top} for ${mid.top})`);

      // print media (a print preview, a PDF): the same, and the print style sheet still lays the list out for paper
      await page.emulateMedia({ media: 'print' });
      await everyRow();
      const sheet = await page.evaluate(() => {
        const box = document.querySelector('.vl-box');
        return {
          overflow: getComputedStyle(box).overflowY, boxH: box.getBoundingClientRect().height,
          wide: document.documentElement.scrollWidth - window.innerWidth, sidebar: getComputedStyle(document.querySelector('#sidebar')).display,
        };
      });
      assert.equal(sheet.overflow, 'visible', 'print: the box does not scroll');
      assert.ok(sheet.boxH > 5000, `print: the box is as tall as its rows (${sheet.boxH})`);
      assert.ok(sheet.wide <= 1, `print: nothing wider than the paper (${sheet.wide})`);
      assert.equal(sheet.sidebar, 'none', 'print: no menu rail');
      await page.emulateMedia({ media: 'screen' });
      await windowed();
      assert.deepEqual(errors, []);
    } finally {
      await browser.close();
      await server.stop();
    }
  });
}
