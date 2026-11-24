// Page state and wording on Instrument's car and history pages: what a live update keeps, where the period picker opens,
// Back after a period change, currencies, fleet figures, print, stale and sold cars, filters, sorting and screen-reader
// basics. Browser tests in Chromium on the dev server with made-up data.
// Run: node --test ui/tools/page_state.test.js. Skipped when Playwright is not installed.
'use strict';

const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { spawn } = require('child_process');

const UI_DIR = path.dirname(__dirname);
const REPO_DIR = path.dirname(UI_DIR);
let playwright = null;
try { playwright = require('playwright'); } catch { /* not installed: skip */ }
const SKIP = !playwright && 'Playwright is not installed';

function devServer(extra) {
  return new Promise((resolve, reject) => {
    const child = spawn('python3', [path.join(UI_DIR, 'devserver', 'serve.py'), '--pack', path.join(UI_DIR, 'packs', 'instrument'),
      '--port', '0', ...extra], { cwd: REPO_DIR, stdio: ['ignore', 'pipe', 'pipe'] });
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

let browser = null, still = null, live = null;
before(async () => {
  if (!playwright) return;
  browser = await playwright.chromium.launch();
  still = await devServer(['--freeze-clock']);                 // nothing moves: for the static findings
  live = await devServer(['--rows-every-s', '1']);             // a `rows` event every second: the page is built again
});
after(async () => {
  await browser?.close();
  await still?.stop();
  await live?.stop();
});

async function open(server, url, ready, opts = {}) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'en-US', timezoneId: 'America/Los_Angeles', ...opts });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.goto(`${server.origin}${url}`);
  if (ready) await page.waitForFunction((id) => document.documentElement.dataset.ready === id, ready, { timeout: 30000 });
  return { context, page, errors };
}
const json = async (page, server, p) => (await page.request.get(`${server.origin}/api/v1${p}`)).json();

// ---------------------------------------------------------------------------------------------------------------
// 1. A live update builds the page again and throws away what the reader typed or chose.
// A marker on the element tells when the page was swapped for a new one; then the typed value must still be there.
const rebuilt = (page, sel) => page.waitForFunction((s) => { const e = document.querySelector(s); return e && !e.__mark; }, sel, { timeout: 25000 });
const mark = (page, sel) => page.evaluate((s) => { document.querySelector(s).__mark = 1; }, sel);

test('live update: Battery keeps a number typed in "Usable capacity when new"', { skip: SKIP, timeout: 90000 }, async () => {
  const { context, page } = await open(live, '/cars/c1/battery', 'battery');
  try {
    const sel = 'input[name=capacity_new_kwh]';
    await page.fill(sel, '77.7');
    await page.focus(sel);
    await mark(page, sel);
    await rebuilt(page, sel);
    assert.equal(await page.evaluate((s) => document.querySelector(s).value, sel), '77.7', 'the typed value is wiped by the live rebuild');
  } finally { await context.close(); }
});

test('live update: the charge page keeps the cost amount typed on a charge under way', { skip: SKIP, timeout: 90000 }, async () => {
  const probe = await open(live, '/', null);
  const items = (await json(probe.page, live, '/cars/c2/charges?limit=5&flags=in_progress')).items || [];
  await probe.context.close();
  assert.ok(items.length, 'the dev server has a charge under way for c2');
  const { context, page } = await open(live, `/cars/c2/charges/${items[0].id}`, 'charge');
  try {
    const sel = 'input[name=amount]';
    await page.fill(sel, '12.34');
    await page.focus(sel);
    await mark(page, sel);
    await rebuilt(page, sel);
    assert.equal(await page.evaluate((s) => document.querySelector(s).value, sel), '12.34', 'the typed cost is replaced by the stored one');
  } finally { await context.close(); }
});

test('live update: Travel keeps the search text and the focus in its field', { skip: SKIP, timeout: 90000 }, async () => {
  const { context, page } = await open(live, '/travel', 'travel');
  try {
    const sel = 'input[aria-label="Text in the place or address"]';
    await page.fill(sel, 'abc');
    await page.focus(sel);
    await mark(page, sel);
    await rebuilt(page, sel);
    assert.equal(await page.evaluate((s) => document.querySelector(s).value, sel), 'abc', 'the search text is wiped');
    assert.equal(await page.evaluate(() => document.activeElement?.tagName), 'INPUT', 'the focus drops to the page');
  } finally { await context.close(); }
});

test('live update: Statistics keeps the table sort the reader chose', { skip: SKIP, timeout: 90000 }, async () => {
  const { context, page } = await open(live, '/statistics', 'statistics');
  try {
    const sorted = () => page.evaluate(() => [...document.querySelectorAll('.st-table th[aria-sort]')].filter((t) => t.getAttribute('aria-sort') !== 'none').map((t) => t.textContent.trim()).join('|'));
    await page.locator('.st-table th button', { hasText: 'Driving' }).first().click();
    assert.match(await sorted(), /^Driving/);
    await page.evaluate(() => { document.querySelector('.st-table').__mark = 1; });
    await rebuilt(page, '.st-table');
    assert.match(await sorted(), /^Driving/, 'the sort goes back to Period after the live rebuild');
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 2. The period picker opened from a page's own button low in the window is placed off screen the first time.
for (const [url, id] of [['/cars/c1/battery', 'battery'], ['/cars/c1', 'car']]) {
  test(`period picker: ${id} opens inside the window the first time when its button is near the bottom`, { skip: SKIP, timeout: 60000 }, async () => {
    const { context, page } = await open(still, url, id);
    try {
      const sel = 'button[title^="Choose the period"]';
      await page.evaluate((s) => { const y = document.querySelector(s).getBoundingClientRect().top; window.scrollBy(0, y - 840); }, sel);
      await page.waitForTimeout(300);
      await page.locator(sel).first().click();
      await page.waitForTimeout(400);
      const r = await page.evaluate(() => { const b = document.querySelector('.period-pop').getBoundingClientRect(); return { top: b.top, bottom: b.bottom, vh: innerHeight }; });
      assert.ok(r.top >= 0 && r.bottom <= r.vh, `popover ${Math.round(r.top)}..${Math.round(r.bottom)} in a window ${r.vh} px high`);
    } finally { await context.close(); }
  });
}

// ---------------------------------------------------------------------------------------------------------------
// 3. Back after changing the period changes the address but not the page.
test('period: Back after picking a preset shows the period of the address again', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/cars/c1/drives', 'drives');
  try {
    const label = () => page.evaluate(() => document.querySelector('.period-label').textContent);
    assert.equal(await label(), 'Last 30 days');
    await page.click('button.period-btn');
    await page.locator('.period-preset', { hasText: 'Last 24 hours' }).click();
    await page.waitForFunction(() => document.querySelector('.period-label').textContent === 'Last 24 hours');
    await page.goBack();
    await page.waitForFunction(() => location.pathname === '/cars/c1/drives' && !/24h/.test(location.search));
    await page.waitForTimeout(1000);
    assert.equal(await label(), 'Last 30 days', 'the address is back at the first view, the page still shows the new period');
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 4. Amounts in different currencies look the same: a CAD cost is written with a bare "$".
test('currency: a charge billed in CAD says CAD on the charge page and in the Charges list', { skip: SKIP, timeout: 60000 }, async () => {
  const probe = await open(still, '/', null);
  const items = (await json(probe.page, still, '/cars/c2/charges?limit=200&preset=all')).items || [];
  await probe.context.close();
  const cad = items.find((c) => c.cost?.currency === 'CAD');
  assert.ok(cad, 'Maple has a charge priced in CAD');
  const { context, page } = await open(still, `/cars/c2/charges/${cad.id}`, 'charge');
  try {
    const big = await page.textContent('.cost-amount');
    assert.match(big, /CAD|CA\$|C\$/, `the cost figure reads "${big}"`);
    const total = (await json(page, still, '/cars/c2/charging-stats?preset=all')).cost_by_currency;
    assert.ok(Array.isArray(total));
    await page.goto(`${still.origin}/cars/c2/charges?preset=all`);
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'charges');
    // the list shows 25 charges at a time: more of them are asked for until this one is in it
    for (let i = 0; i < 6 && !(await page.$(`a[href*="${cad.id}"]`)); i++) {
      await page.locator('.charges-table .tbl-foot button', { hasText: 'Show more' }).click();
      await page.waitForTimeout(500);
    }
    const cell = await page.evaluate((id) => { const a = document.querySelector(`a[href*="${id}"]`); return a?.closest('tr')?.querySelector('a.cost-link')?.textContent || null; }, cad.id);
    assert.match(cell || '', /CAD|CA\$|C\$/, `the list cell reads "${cell}"`);
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 5. Statistics for all cars: the cost per day is divided by days times cars.
test('statistics: the fleet cost per day is the total over the days of the period, not over days times cars', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/statistics', null);
  try {
    let q = null;
    page.on('request', (r) => { const u = new URL(r.url()); if (u.pathname === '/api/v1/statistics') q = u.searchParams; });
    await page.waitForFunction(() => document.documentElement.dataset.ready === 'statistics', null, { timeout: 30000 });
    await page.waitForTimeout(500);
    const text = await page.evaluate(() => document.body.innerText);
    const total = Number(/Total cost\s+\$([\d,.]+) USD/.exec(text)?.[1].replace(/,/g, ''));
    const perDay = Number(/\$([\d,.]+) USD(?:, [^\n]*?)? per day/.exec(text)?.[1].replace(/,/g, ''));
    assert.ok(total > 0 && perDay > 0, `read total ${total} and per day ${perDay}`);
    // the period of the page: January 1st to now (the request's from_ms and to_ms)
    const days = (Number(q?.get('to_ms')) - Number(q?.get('from_ms'))) / 86400000 || 319.4;
    assert.ok(Math.abs(perDay - total / days) / (total / days) < 0.03, `per day $${perDay}; total $${total} over ${days.toFixed(1)} days is $${(total / days).toFixed(2)}`);
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 6. All-cars Charges: Back-to-back charging shows another car's start times in the page's zone.
test('charges: Back-to-back charging shows each run in its car\'s time zone (Maple: America/Chicago)', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/charges?preset=all', 'charges');
  try {
    const runs = (await json(page, still, '/charging-stats?cars=all&preset=all')).runs.filter((r) => r.car === 'c2');
    assert.ok(runs.length > 0, 'Maple has runs of back-to-back charges');
    const want = (ms) => `${new Intl.DateTimeFormat('en-US', { weekday: 'short', month: 'short', day: 'numeric', timeZone: 'America/Chicago' }).format(ms)}, ${new Intl.DateTimeFormat('en-US', { hour: 'numeric', minute: '2-digit', timeZone: 'America/Chicago' }).format(ms)}`.replace(/\s/g, ' ');
    const rows = await page.evaluate(() => [...document.querySelectorAll('.runs-table tbody tr')].map((r) => r.innerText.replace(/\s+/g, ' ')));
    const missing = runs.map((r) => want(r.start_ms)).filter((w) => !rows.some((t) => t.startsWith('Maple') && t.replace(/\s/g, ' ').includes(w)));
    assert.deepEqual(missing.slice(0, 3), [], 'run starts not shown in Maple\'s own time');
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 7. Printing: no print style sheet.
test('print: the menu rail and the top bar are not printed', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/statistics', 'statistics');
  try {
    await page.emulateMedia({ media: 'print' });
    const shown = await page.evaluate(() => ['#sidebar', '#topbar'].filter((s) => getComputedStyle(document.querySelector(s)).display !== 'none'));
    assert.deepEqual(shown, [], 'these still print');
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 8. Stale and sold cars are described in the present tense.
test('stale car: the overview does not say a car that sent nothing for 2 days "should fall asleep soon"', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/cars/c3', 'car');
  try {
    const now = await json(page, still, '/cars/c3/now');
    assert.equal(now.stale, true, 'the made-up Atlas is stale');
    await page.waitForTimeout(500);
    assert.doesNotMatch(await page.evaluate(() => document.body.innerText), /should fall asleep soon/);
  } finally { await context.close(); }
});

test('sold car: the overview of a sold car says it was sold', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/cars/c0', 'car');
  try {
    await page.waitForTimeout(500);
    assert.match(await page.evaluate(() => document.querySelector('main, .view').innerText), /sold/i);
  } finally { await context.close(); }
});

test('stale car: Battery\'s "Now" panel does not say Live for a car with old data', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/cars/c0/battery', 'battery');
  try {
    await page.waitForTimeout(500);
    const head = await page.evaluate(() => document.querySelector('.bat-nowp')?.querySelector('.panel-head')?.textContent || '');
    assert.doesNotMatch(head, /Live/, `the panel head reads "${head}"`);
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 9. Timeline's place filter lists every place of every car.
test('timeline: the Place filter lists only the places this car used', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/cars/c1/timeline', 'timeline');
  try {
    await page.waitForTimeout(800);
    const n = await page.$$eval('select[aria-label=Place] option', (o) => o.length - 1);
    // the Drives page's own rule: a place the car has drives to or from (counts.drives of `/places?car=`)
    const items = (await json(page, still, '/places?car=c1&limit=200')).items;
    const used = items.filter((p) => (p.counts?.drives || 0) > 0).length;
    assert.ok(used < items.length, 'the car uses fewer places than there are');
    assert.ok(n <= used, `the select holds ${n} places, the car used ${used} (all cars: ${items.length})`);
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 10. Drives with a non-number in min_speed or min_distance.
for (const q of ['min_speed=abc', 'min_distance=abc', 'min_distance=1e999']) {
  test(`drives: ?${q} does not replace the page with a JavaScript error`, { skip: SKIP, timeout: 60000 }, async () => {
    const { context, page } = await open(still, `/cars/c1/drives?${q}`, null);
    try {
      await page.waitForFunction(() => /^(drives|error:drives)$/.test(document.documentElement.dataset.ready || ''), null, { timeout: 30000 });
      const text = await page.evaluate(() => document.querySelector('main, .view').innerText);
      assert.doesNotMatch(text, /toFixed|Cannot read properties/);
    } finally { await context.close(); }
  });
}

// ---------------------------------------------------------------------------------------------------------------
// 11. Dev server: battery health compares the full pack with "when new" (the page labels say usable).
test('battery (dev server): the page\'s usable now, usable when new, difference and health agree', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/cars/c1/battery', 'battery');
  try {
    const cap = (await json(page, still, '/cars/c1/battery')).capacity;
    assert.ok(Math.abs(cap.difference_kwh - (cap.new_kwh - cap.usable_kwh)) < 0.06, `difference ${cap.difference_kwh} against new ${cap.new_kwh} - usable ${cap.usable_kwh}`);
    assert.ok(Math.abs(cap.health_pct - (cap.usable_kwh / cap.new_kwh) * 100) < 0.1, `health ${cap.health_pct} against ${(cap.usable_kwh / cap.new_kwh * 100).toFixed(1)}`);
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 12. Trip's drives sorted by Consumption put the drive under way in the middle of the numbers.
test('trip: sorted by Consumption, drives with no figure (still driving) are at an end of the list', { skip: SKIP, timeout: 60000 }, async () => {
  const { context, page } = await open(still, '/cars/c1/trip', 'trip');
  try {
    await page.locator('th button', { hasText: 'Consumption' }).first().click();
    await page.waitForTimeout(600);
    const col = await page.evaluate(() => {
      const th = [...document.querySelectorAll('th')].find((t) => /Consumption/.test(t.textContent));
      const i = [...th.parentElement.children].indexOf(th);
      return [...th.closest('table').querySelectorAll('tbody tr')].map((r) => /^\s*\d/.test(r.children[i]?.innerText || ''));
    });
    assert.ok(col.includes(false), 'a drive without a figure is in the list');
    const str = col.map((x) => (x ? 't' : 'f')).join('');
    assert.match(str, /^(t*f*|f*t*)$/, `rows with a figure (t) and without (f): ${str}`);
  } finally { await context.close(); }
});

// ---------------------------------------------------------------------------------------------------------------
// 13. Screen-reader basics: heading levels skip, charts have no name.
for (const [url, id] of [['/cars/c1/software', 'software'], ['/places', 'places']]) {
  test(`headings: ${id} never skips a level`, { skip: SKIP, timeout: 60000 }, async () => {
    const { context, page } = await open(still, url, id);
    try {
      await page.waitForTimeout(800);
      const levels = await page.$$eval('main h1, main h2, main h3, main h4, main h5, main h6', (hs) => hs.map((h) => Number(h.tagName[1])));
      const skips = levels.map((l, i) => (i && l > levels[i - 1] + 1 ? `h${levels[i - 1]} then h${l}` : null)).filter(Boolean);
      assert.deepEqual(skips, []);
    } finally { await context.close(); }
  });
}

test('charts: every chart on Car overview, Battery and Drives has a name and a role', { skip: SKIP, timeout: 90000 }, async () => {
  const unnamed = [];
  for (const [url, id] of [['/cars/c1', 'car'], ['/cars/c1/battery', 'battery'], ['/cars/c1/drives', 'drives']]) {
    const { context, page } = await open(still, url, id);
    try {
      await page.waitForTimeout(1200);
      const n = await page.$$eval('.uplot', (cs) => cs.filter((c) => { const named = c.closest('[role=img][aria-label]') || (c.getAttribute('role') === 'img' && c.getAttribute('aria-label')); return !named; }).length);
      if (n) unnamed.push(`${id}: ${n}`);
    } finally { await context.close(); }
  }
  assert.deepEqual(unnamed, []);
});
