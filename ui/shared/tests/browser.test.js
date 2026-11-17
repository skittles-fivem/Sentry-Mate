// Runs the shared modules in Chromium (the pre-installed Playwright) under the Content-Security-Policy the
// dev server sends: dom.js, the CSV download, real fetch and EventSource with reconnect, link routing.
// Skipped when Playwright is not available. The page side is browser-page.js.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const sharedDir = join(here, '..');
let playwright = null;
try { playwright = createRequire(import.meta.url)('playwright'); } catch { /* not installed: skip */ }

// contract 1: the dev server's policy
const CSP = "default-src 'self'; img-src 'self' data: blob: https://tile.openstreetmap.org; worker-src 'self' blob:; style-src 'self'; script-src 'self'; connect-src 'self' https://tile.openstreetmap.org; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
const VIRTUAL_NOW = Date.parse('2026-11-16T17:30:00Z');
const PAGE = '<!doctype html><html lang="en"><head><meta charset="utf-8"><title>shared modules</title><script type="module" src="/test-page.js"></script></head><body></body></html>';

function startServer() {
  const started = Date.now();
  const hits = { slow: 0, hang_closed: 0, events: 0 };
  const server = createServer(async (req, res) => {
    const u = new URL(req.url, 'http://127.0.0.1');
    const head = { 'Content-Security-Policy': CSP, 'X-Content-Type-Options': 'nosniff', 'Referrer-Policy': 'no-referrer' };
    const send = (status, type, body, extra = {}) => { res.writeHead(status, { ...head, 'Content-Type': type, ...extra }); res.end(body); };
    const sendJson = (status, body) => send(status, 'application/json', JSON.stringify(body), { 'Cache-Control': 'no-store', 'X-Sentry-Mate-Now-Ms': String(VIRTUAL_NOW + (Date.now() - started)) });
    const p = u.pathname;
    try {
      if (p === '/test-page.js') return send(200, 'text/javascript', await readFile(join(here, 'browser-page.js')));
      const m = /^\/shared\/([a-z0-9-]+\.js)$/.exec(p);
      if (m) return send(200, 'text/javascript', await readFile(join(sharedDir, m[1])));
      if (p === '/api/v1/me') return sendJson(200, { id: 'a1', name: 'owner', role: 'admin', cars: ['c1'] });
      if (p === '/api/v1/hits') return sendJson(200, hits);
      if (p === '/api/v1/slow') { hits.slow++; setTimeout(() => sendJson(200, { ok: true }), 150); return; }
      if (p === '/api/v1/cars/nope') return sendJson(404, { error: { code: 'no_such_car', message: 'No such car.' } });
      if (p === '/api/v1/private') return sendJson(401, { error: { code: 'signed_out' } });
      if (p === '/api/v1/hang') { res.on('close', () => { hits.hang_closed++; }); return; }
      if (p === '/api/v1/echo') {
        if (req.headers['x-sentry-mate'] !== '1') return sendJson(403, { error: { code: 'csrf' } });
        let body = '';
        for await (const chunk of req) body += chunk;
        return sendJson(200, { csrf: req.headers['x-sentry-mate'], method: req.method, body: JSON.parse(body) });
      }
      if (p === '/api/v1/events') {
        hits.events++;
        res.writeHead(200, { ...head, 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store' });
        res.write(`event: now\nid: ${hits.events}\ndata: ${JSON.stringify({ car: 'c1', now: { soc_pct: { value: 80, at_ms: VIRTUAL_NOW } } })}\n\n`);
        setTimeout(() => res.end(), 50); // the connection drops: the client must come back by itself
        return;
      }
      if (p.startsWith('/api/')) return sendJson(404, { error: { code: 'not_found' } });
      if (p === '/favicon.ico') { res.writeHead(204, head); return res.end(); }
      return send(200, 'text/html; charset=utf-8', PAGE);
    } catch (e) {
      send(500, 'text/plain', String(e));
    }
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve({ server, port: server.address().port })));
}

test('shared modules in Chromium under the page policy', { skip: !playwright && 'Playwright is not installed', timeout: 60000 }, async () => {
  const { server, port } = await startServer();
  const browser = await playwright.chromium.launch();
  try {
    const context = await browser.newContext({ acceptDownloads: true });
    const page = await context.newPage();
    const problems = [];
    page.on('console', (m) => { if (m.type() === 'error' && !/401|404/.test(m.text())) problems.push('console: ' + m.text()); });
    page.on('pageerror', (e) => problems.push('page error: ' + e.message));
    page.on('request', (r) => { if (!r.url().startsWith(`http://127.0.0.1:${port}/`)) problems.push('outside request: ' + r.url()); });
    const download = page.waitForEvent('download', { timeout: 30000 });
    await page.goto(`http://127.0.0.1:${port}/`);
    await page.waitForFunction(() => window.__results, null, { timeout: 30000 });
    const results = await page.evaluate(() => window.__results);
    if (process.env.SHOW_CHECKS) console.log(results.map((r) => `${r.ok ? 'ok  ' : 'FAIL'} ${r.name}`).join('\n'));
    for (const r of results) assert.ok(r.ok, `${r.name}: ${r.detail}`);
    assert.ok(results.length >= 25, `only ${results.length} checks ran`);

    const d = await download;
    assert.equal(d.suggestedFilename(), 'test drives.csv');
    const text = await readFile(await d.path(), 'utf8');
    assert.equal(text, '﻿Distance (km),Place\r\n12.5,"Home, ""north"""\r\n');
    assert.deepEqual(problems, []);
  } finally {
    await browser.close();
    server.close();
  }
});
