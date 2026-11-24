// The Import page's report of a TeslaFi file in every built-in pack, in Chromium on the dev API server: "Rows per
// car" says for each car what its own figures say (report.cars[].tables.positions). The same file a second time
// stores nothing (every poll already imported), and the car's row says so: 0 to import and every poll skipped,
// never "all imported, none skipped".
// Run: node --test ui/tools/import-report.test.js. Skipped when Playwright is not installed. Takes about 40 s: the
// dev server's check and import take their seconds.
'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { spawn } = require('child_process');

const { BUILTIN_PACKS } = require('./builtin-packs');
const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }

function devServer(pack) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', pack),
      '--port', '0'], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

// the dev server's made-up TeslaFi month
const FILE = fs.readFileSync(path.join(UI_DIR, 'devserver', 'mockapi', 'data', 'TeslaFi32023.csv'));
const SHA = crypto.createHash('sha256').update(FILE).digest('hex');

for (const pack of BUILTIN_PACKS) {
  test(`${pack}: a TeslaFi file imported again shows its car with nothing to import and every poll skipped`, { skip: !playwright && 'Playwright is not installed', timeout: 240000 }, async () => {
    const server = await devServer(pack);
    const browser = await playwright.chromium.launch();
    try {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      await page.goto(`${server.origin}/settings/import`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'import', null, { timeout: 30000 });
      // through the API: the file once (checked, matched to a new car, imported), then the same file again (checked)
      const api = (method, url, body, headers = {}) => page.evaluate(async ([m, u, b, hd]) => {
        const bytes = Array.isArray(b) ? new Uint8Array(b) : null;
        const r = await fetch(`/api/v1/${u}`, { method: m, headers: { 'X-Sentry-Mate': '1', ...(bytes ? {} : b ? { 'Content-Type': 'application/json' } : {}), ...hd },
          body: bytes || (b ? JSON.stringify(b) : undefined) });
        return { status: r.status, body: await r.json().catch(() => null) };
      }, [method, url, Buffer.isBuffer(body) ? [...body] : body, headers]);
      const until = async (id, status) => {
        for (let i = 0; i < 120; i++) {
          const j = (await api('GET', `imports/${id}`)).body;
          if (j.status === status) return j;
          assert.notEqual(j.status, 'failed', JSON.stringify(j.error));
          await page.waitForTimeout(500);
        }
        throw new Error(`the import did not reach ${status}`);
      };
      const upload = async (name) => {
        const made = await api('POST', 'imports', { name, bytes_total: FILE.length });
        assert.equal(made.status, 201, JSON.stringify(made.body));
        const sent = await api('PATCH', `imports/${made.body.id}`, FILE, { 'Upload-Offset': '0', 'Upload-Length': String(FILE.length), 'Upload-SHA256': SHA, 'Content-Type': 'application/offset+octet-stream' });
        assert.equal(sent.status, 200, JSON.stringify(sent.body));
        return made.body.id;
      };
      const first = await upload('TeslaFi32023.csv');
      await until(first, 'ready');
      assert.equal((await api('PUT', `imports/${first}/cars`, { items: [{ tm_car: 1, match: 'new' }] })).status, 200);
      assert.equal((await api('POST', `imports/${first}/start`, {})).status, 200);
      await until(first, 'done');
      const second = await upload('again.csv');
      const job = await until(second, 'ready');
      const polls = job.report.rows.read;
      assert.ok(polls > 0 && job.report.rows.used === 0, JSON.stringify(job.report.rows));

      await page.goto(`${server.origin}/settings/import?import=${second}`);
      await page.waitForFunction(() => document.documentElement.dataset.ready === 'import', null, { timeout: 30000 });
      const cells = async () => page.evaluate(() => {
        const table = [...document.querySelectorAll('main table')].find((t) => /Rows read, imported and skipped per imported car/.test(t.getAttribute('aria-label') || t.caption?.textContent || ''));
        if (!table) return null;
        const heads = [...table.querySelectorAll('thead th')].map((th) => th.textContent.trim());
        const row = [...table.querySelectorAll('tbody tr')].find((tr) => /Blue Whale/.test(tr.textContent));
        if (!row) return null;
        const tds = [...row.querySelectorAll('td, th')].map((td) => td.textContent.trim());
        return Object.fromEntries(heads.map((hd, i) => [hd, tds[i]]));
      });
      await page.waitForFunction(() => /Blue Whale/.test(document.querySelector('main')?.textContent || ''), null, { timeout: 15000 });
      let row = null;
      for (let i = 0; i < 20 && !row; i++) { row = await cells(); if (!row) await page.waitForTimeout(250); }
      assert.ok(row, 'the Rows per car table has the made-up car');
      const digits = (t) => (t || '').replace(/[^\d]/g, '');
      assert.equal(digits(row.Read), String(polls), JSON.stringify(row));
      assert.equal(digits(row.Imported), '0', JSON.stringify(row));
      assert.equal(digits(row.Skipped), String(polls), JSON.stringify(row));
      assert.deepEqual(errors, []);
    } finally {
      await browser.close();
      await server.stop();
    }
  });
}
