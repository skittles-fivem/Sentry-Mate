// Runs inside Chromium for browser.test.js, under the same Content-Security-Policy the dev server sends.
// Each check pushes { name, ok, detail }; window.__results is set when all are done.
import { h, svg, cover, ready, unready, failed, setTitle, quantity, setVars, safeUrl, readyPage, disclose, steady, longest, textShapes, replace, replaceChanged, markScrolling } from './shared/dom.js';
import { setLocale, t } from './shared/i18n.js';
import { setUnits } from './shared/units.js';
import * as api from './shared/api.js';
import * as state from './shared/state.js';
import { downloadCsv, column } from './shared/csv.js';
import { resolvePeriod } from './shared/time.js';
import { scrollBox } from './shared/scrollbox.js';

const results = [];
const check = (name, ok, detail = '') => results.push({ name, ok: !!ok, detail: String(detail) });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const violations = [];
document.addEventListener('securitypolicyviolation', (e) => violations.push(`${e.violatedDirective} ${e.blockedURI}`));

async function run() {
  // ---- dom.js
  const clicked = [];
  const el = h('button', { class: ['a', { b: true, c: false }], dataset: { carId: 'c1' }, vars: { '--w': '42%' }, style: { 'min-width': '3rem' }, onClick: () => clicked.push(1), cover: 'x.one x.two', disabled: false, 'aria-label': 'Go' }, 'Go ', 3, null, ['!']);
  document.body.append(el);
  el.click();
  check('h builds classes, data, attributes and text', el.className === 'a b' && el.dataset.carId === 'c1' && el.textContent === 'Go 3!' && !el.hasAttribute('disabled') && el.getAttribute('aria-label') === 'Go', el.outerHTML);
  check('h wires event handlers', clicked.length === 1);
  check('CSS custom properties and styles go through CSSOM', getComputedStyle(el).getPropertyValue('--w').trim() === '42%' && getComputedStyle(el).minWidth === '48px', el.getAttribute('style'));
  check('cover marks item ids without repeats', cover(el, 'x.two', 'x.three').dataset.cover === 'x.one x.two x.three', el.dataset.cover);
  // replaceChanged(): only the children that differ are swapped; a kept node, and one equal to it, stay
  const slot = h('div');
  const keep = h('section', null, 'map');
  replace(slot, h('p', { class: 'x' }, 'one'), keep, h('p', null, 'three'));
  const [p1, , p3] = slot.children;
  const seen = [];
  const mo = new MutationObserver((l) => seen.push(...l));
  mo.observe(slot, { childList: true, subtree: true, characterData: true, attributes: true });
  replaceChanged(slot, h('p', { class: 'x' }, 'one'), keep, h('p', null, 'three'));
  await Promise.resolve();
  check('replaceChanged leaves equal children alone', seen.length === 0 && slot.children[0] === p1 && slot.children[1] === keep && slot.children[2] === p3, seen.length);
  replaceChanged(slot, h('p', { class: 'x' }, 'one'), keep, h('p', null, 'four'));
  await Promise.resolve();
  check('replaceChanged swaps only the child that changed', slot.children[0] === p1 && slot.children[1] === keep && slot.children[2] !== p3 && slot.textContent === 'onemapfour' && seen.length === 1, slot.outerHTML);
  replaceChanged(slot, keep, 'and text', null);
  check('replaceChanged with another count or order draws again', slot.childNodes.length === 2 && slot.firstChild === keep && slot.textContent === 'mapand text', slot.outerHTML);
  mo.disconnect();
  // holdScroll() (through replace()): a part above the screen drawn again taller leaves what is on screen where it was
  // (scroll anchoring of its own: Safari on iOS has none; Chromium's is switched off here), and a part drawn again in
  // the middle of the screen keeps where the window is scrolled
  {
    const de = document.documentElement;
    de.style.overflowAnchor = 'none'; document.body.style.overflowAnchor = 'none';
    const tall = (n, px) => Array.from({ length: n }, (_, i) => h('div', { style: { height: `${px}px` } }, `row ${i}`));
    const above = h('div', null, tall(5, 100));
    const mid = h('div', null, tall(30, 100));
    const page = h('div', null, above, mid, h('div', { style: { height: '3000px' } }));
    document.body.append(page);
    const pageTop = page.getBoundingClientRect().top + scrollY;
    scrollTo(0, pageTop + 1000);
    await new Promise((r) => requestAnimationFrame(r));
    const y0 = scrollY;
    const mark = mid.children[10];
    const at = mark.getBoundingClientRect().top;
    replace(above, tall(7, 100)); // 200 px taller, above the screen
    await Promise.resolve(); await Promise.resolve();
    check('holdScroll keeps what is on screen in place when a part above it grows', Math.abs(mark.getBoundingClientRect().top - at) < 1 && Math.abs(scrollY - (y0 + 200)) < 1, `${y0} -> ${scrollY}, ${at} -> ${mark.getBoundingClientRect().top}`);
    const y1 = scrollY;
    replace(mid, tall(30, 100)); // the part on screen itself, the same height
    await Promise.resolve(); await Promise.resolve();
    check('holdScroll keeps the scroll when the part on screen is drawn again', Math.abs(scrollY - y1) < 1, `${y1} -> ${scrollY}`);
    page.remove();
    scrollTo(0, 0);
    de.style.overflowAnchor = ''; document.body.style.overflowAnchor = '';
  }
  // markScrolling(): data-scrolling on the root while something scrolls, gone a moment after
  const stopMark = markScrolling(60);
  const scroller = h('div', { style: { height: '50px', overflow: 'auto' } }, h('div', { style: { height: '500px' } }));
  document.body.append(scroller);
  scroller.scrollTop = 100;
  await new Promise((r) => setTimeout(r, 30));
  const during = 'scrolling' in document.documentElement.dataset;
  await new Promise((r) => setTimeout(r, 120));
  check('markScrolling marks the root while a box scrolls, and not after', during && !('scrolling' in document.documentElement.dataset), during);
  stopMark();
  scroller.remove();
  // disclose(): a button that opens a small form in place, focus in and back out, Escape closes it
  const edit = h('button', { type: 'button' }, 'Edit');
  const box = h('form', null, h('label', { for: 'dz-in' }, 'Version'), h('input', { id: 'dz-in' }), h('button', { type: 'submit' }, 'Save'));
  const toggles = [];
  document.body.append(edit, box);
  const dz = disclose(edit, box, { onOpen: () => toggles.push('open'), onClose: () => toggles.push('close') });
  check('disclose starts closed and names its part', box.hidden && edit.getAttribute('aria-expanded') === 'false' && edit.getAttribute('aria-controls') === box.id && !!box.id, edit.outerHTML);
  edit.click();
  check('disclose opens on click and focuses the first control', !box.hidden && dz.isOpen() && edit.getAttribute('aria-expanded') === 'true' && document.activeElement?.id === 'dz-in', document.activeElement?.outerHTML);
  document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  check('Escape closes it and gives the focus back to the button', box.hidden && edit.getAttribute('aria-expanded') === 'false' && document.activeElement === edit, document.activeElement?.outerHTML);
  dz.open();
  dz.close();
  check('closing from code keeps the focus where it is unless it was inside', box.hidden && toggles.join(',') === 'open,close,open,close', toggles.join(','));
  edit.remove(); box.remove();
  const evil = h('div', null, '<img src=x onerror="window.__pwned=1">');
  check('text is never markup', evil.childElementCount === 0 && evil.textContent.startsWith('<img'));
  const link = h('a', { href: 'javascript:window.__pwned=1', onmouseover: 'window.__pwned=1' }, 'x');
  check('javascript: links and string handlers are dropped', !link.hasAttribute('href') && !link.hasAttribute('onmouseover'));
  check('safeUrl keeps ordinary links', safeUrl('/cars/c1') === '/cars/c1' && safeUrl('https://tile.openstreetmap.org/1/1/1.png') !== null && safeUrl(' JaVa\tScript:alert(1)') === null && safeUrl('data:text/html,x') === null);
  const s = svg('svg', { viewBox: '0 0 10 10', class: 'ico' }, svg('path', { d: 'M0 0h10' }));
  check('svg elements get the SVG namespace', s.namespaceURI === 'http://www.w3.org/2000/svg' && s.firstChild.namespaceURI === s.namespaceURI && s.getAttribute('class') === 'ico');
  setUnits({ distance: 'mi' });
  const q = quantity(16.09344, 'distance');
  const qm = quantity(null, 'temperature', { why: 'No reading while asleep' });
  check('quantity shows the number and unit', q.querySelector('.q-v').textContent === '10.0' && q.querySelector('.q-u').textContent === 'mi', q.outerHTML);
  check('a blank quantity shows its reason as text', qm.classList.contains('is-missing') && qm.querySelector('.q-why').textContent === 'No reading while asleep');
  setVars(el, { w: '10%' });
  check('setVars accepts names without --', el.style.getPropertyValue('--w') === '10%');
  check('tab title', setTitle('Drives', 'Model Y') === 'Drives · Model Y · Sentry Mate' && document.title === 'Drives · Model Y · Sentry Mate');
  // steady: a readout in a box as wide as its longest text (the packs' CSS, here as a constructed sheet)
  const sheet = new CSSStyleSheet();
  sheet.replaceSync('.steady{display:inline-grid}.steady>*{grid-area:1/1}.steady-size{visibility:hidden}');
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  const val = h('b', { class: 'legend-val', cover: 'x.val', id: 'v1' }, '');
  const row = h('div', { style: { display: 'flex', 'font-variant-numeric': 'tabular-nums' } }, h('span', null, 'Battery '), steady(val, ['12 %', '100 %', null, '—']), h('span', { class: 'after' }, ' next'));
  document.body.append(row);
  const after = row.querySelector('.after');
  const xs = [];
  for (const t of ['', '7 %', '100 %', '—']) { val.textContent = t; xs.push(after.getBoundingClientRect().left); }
  const sizers = [...row.querySelectorAll('.steady-size')];
  const sizer = sizers.find((x) => x.textContent === '000 %');
  check('longest picks the longest text', longest(['a', null, 'abc', 'ab']) === 'abc' && longest(null) === '');
  check('textShapes: digits as 0, each shape once, the longest kept', textShapes(['12 %', '34 %', '100 %', null, '']).join('|') === '00 %|000 %' && textShapes(['1', '22', '333'], 2).join('|') === '000|00');
  check('steady keeps what follows a readout in place while its text changes', xs.every((x) => x === xs[0]) && xs[0] > row.firstChild.getBoundingClientRect().right, xs.join(' '));
  check('steady sizes with unseen copies that are not items, targets or names', sizers.length === 3 && !!sizer && sizer.tagName === 'B' && sizer.classList.contains('legend-val')
    && row.querySelector('.legend-val') === val && sizer.getAttribute('aria-hidden') === 'true' && !sizer.hasAttribute('data-cover') && !sizer.id && getComputedStyle(sizer).visibility === 'hidden', row.innerHTML);
  // a narrow digit run against wide letters: the shape with the wider letters sizes the box
  const word = h('span', null, 'x');
  const row2 = h('div', { style: { display: 'flex' } }, steady(word, ['Mar 1', 'May 1', 'WWW 1']), h('span', { class: 'after2' }, 'next'));
  document.body.append(row2);
  const a2 = row2.querySelector('.after2');
  const ys = [];
  for (const t of ['Mar 1', 'WWW 1', 'iii 1']) { word.textContent = t; ys.push(a2.getBoundingClientRect().left); }
  check('steady keeps the room of the widest shape, not only of the longest text', ys.every((y) => y === ys[0]), ys.join(' '));
  row2.remove();
  val.parentElement.fit(['1 000 000 %']);
  check('steady fits new data', row.querySelectorAll('.steady-size').length === 1 && row.querySelector('.steady-size').textContent === '0 000 000 %' && after.getBoundingClientRect().left > xs[0]);
  row.remove();
  ready('fleet');
  const r1 = document.documentElement.dataset.ready;
  unready();
  const r2 = document.documentElement.dataset.ready;
  failed('drives');
  check('ready, unready and failed set data-ready', r1 === 'fleet' && r2 === undefined && readyPage() === 'error:drives');
  unready();

  // ---- i18n.js
  setLocale('en-GB');
  check('setLocale sets <html lang>', document.documentElement.lang === 'en-GB' && t('{n} cars', { n: 1200 }) === '1,200 cars');
  setLocale('en-US');

  // ---- api.js against the test server
  const me = await api.get('me');
  check('get reads JSON', me && me.id === 'a1');
  check('the server clock comes from the response', Math.abs(api.now() - Date.parse('2026-11-16T17:30:00Z')) < 5000, new Date(api.now()).toISOString());
  const preset = resolvePeriod('24h', { now: api.now(), tz: 'America/Chicago' });
  check('presets use the server clock', preset.to_ms - Date.parse('2026-11-16T17:30:00Z') < 5000);
  await Promise.all([api.get('slow'), api.get('slow'), api.get('slow')]);
  const hits = await api.get('hits');
  check('the same read at once is sent once', hits.slow === 1, JSON.stringify(hits));
  try { await api.get(api.apiPath`cars/${'nope'}`); check('errors throw', false); } catch (e) {
    check('errors arrive as ApiError', e instanceof api.ApiError && e.status === 404 && e.code === 'no_such_car', `${e.name} ${e.status} ${e.code}`);
  }
  const echo = await api.patch('echo', { theme: 'dark' });
  check('changes carry the CSRF header and JSON', echo.csrf === '1' && echo.body.theme === 'dark' && echo.method === 'PATCH', JSON.stringify(echo));
  const scope = api.scope();
  const pending = scope.get('hang');
  await sleep(50);
  scope.abort();
  let aborted = false;
  try { await pending; } catch (e) { aborted = api.isAbort(e); }
  await sleep(50);
  const hang = await api.get('hits');
  check('a left page cancels its requests', aborted && hang.hang_closed === 1, JSON.stringify(hang));
  let signins = 0;
  api.on('unauthorized', () => signins++);
  try { await api.get('private'); } catch { /* expected */ }
  check('401 asks the frame for sign-in', signins === 1);

  // live events: the server sends one event and closes; the client comes back by itself
  const got = [];
  const stop = api.events((m) => got.push(m));
  for (let i = 0; i < 60 && got.length < 2; i++) await sleep(100);
  stop();
  const ev = await api.get('hits');
  check('live events arrive and the stream reconnects after a drop', got.length >= 2 && got[0].type === 'now' && got[0].data.car === 'c1' && ev.events >= 2, `${got.length} events, ${ev.events} connections`);

  // ---- csv.js download
  setUnits({ distance: 'km' });
  downloadCsv('test drives.csv', [{ distance_km: 12.5, name: 'Home, "north"' }], [column('Distance', 'distance_km', 'distance'), column('Place', 'name')]);

  // ---- state.js routing: a link click changes the URL without a page load
  window.__samePage = 'yes';
  const stopRouting = state.startRouting();
  const changes = [];
  state.subscribe((s2) => changes.push(s2.page));
  ready('fleet');
  const a = h('a', { href: state.buildUrl('drives', { car: 'c1', period: { preset: '7d' } }) }, 'Drives');
  document.body.append(a);
  a.click();
  await sleep(20);
  check('a link click navigates inside the app', location.pathname === '/cars/c1/drives' && location.search === '?preset=7d' && window.__samePage === 'yes' && changes.includes('drives') && document.documentElement.dataset.ready === undefined, `${location.href} ${changes}`);
  check('the store has the car and period', state.current().car === 'c1' && state.current().period.preset === '7d');
  history.back();
  await sleep(100);
  check('back goes back', location.pathname === '/' && state.current().page === 'fleet', location.href);

  // an in-page #anchor (the browser fires popstate for it): no re-route, data-ready stays, the page lands on it
  const before = changes.length;
  const spacer = h('div', {});
  const part = h('section', {}, 'MQTT');
  part.id = 'mqtt';
  const toPart = h('a', { href: '#mqtt' }, 'MQTT');
  document.body.append(toPart, spacer, part);
  spacer.style.height = '3000px';
  part.style.height = '3000px';
  ready('fleet');
  window.scrollTo(0, 0);
  toPart.click();
  await sleep(50);
  check('an in-page anchor keeps data-ready and the store', location.hash === '#mqtt' && document.documentElement.dataset.ready === 'fleet' && changes.length === before && state.current().page === 'fleet', `${location.href} ${document.documentElement.dataset.ready} ${changes}`);
  check('an in-page anchor lands on its part', Math.abs(part.getBoundingClientRect().top) < 2, String(part.getBoundingClientRect().top));
  history.back();
  await sleep(100);
  check('back to no anchor keeps data-ready', location.hash === '' && document.documentElement.dataset.ready === 'fleet' && changes.length === before, `${location.href} ${document.documentElement.dataset.ready}`);
  state.navigate('/#mqtt');
  await sleep(50);
  check('navigate() to the same page with a hash keeps data-ready', location.hash === '#mqtt' && document.documentElement.dataset.ready === 'fleet' && changes.length === before, `${location.href} ${document.documentElement.dataset.ready}`);
  toPart.remove(); spacer.remove(); part.remove();
  stopRouting();

  // ---- scrollbox.js: a box of fixed height, data-below, the next page as its end comes near
  {
    const list = h('div', { class: 'rows' });
    let pages = 0;
    const addPage = () => { for (let i = 0; i < 5; i++) list.append(h('div', null, `row ${pages * 5 + i}`)); pages++; };
    addPage();
    const ac = new AbortController();
    const sb = scrollBox({ label: 'Rows', cls: 'test-box', signal: ac.signal, onEnd: async () => { if (pages >= 4) return false; await sleep(10); addPage(); return true; } }, list);
    sb.el.style.height = '100px'; sb.el.style.overflowY = 'auto';
    for (const r of [list]) r.style.lineHeight = '40px';
    document.body.append(sb.el);
    check('scrollBox is a focusable region with its name', sb.el.getAttribute('role') === 'region' && sb.el.tabIndex === 0 && sb.el.getAttribute('aria-label') === 'Rows' && sb.el.classList.contains('scroll-box') && sb.el.classList.contains('test-box') && sb.el.lastElementChild === sb.end, sb.el.outerHTML.slice(0, 200));
    await sleep(80);
    check('scrollBox marks rows hidden below its edge', sb.el.hasAttribute('data-below'), sb.el.scrollHeight);
    check('scrollBox does not load more until its end comes near', pages === 1, pages);
    sb.el.scrollTop = sb.el.scrollHeight;
    for (let i = 0; i < 40 && pages < 2; i++) await sleep(20);
    check('scrollBox loads the next page as its end comes near', pages >= 2 && sb.el.offsetHeight === 100, `${pages} ${sb.el.offsetHeight}`);
    for (let n = 0; n < 6; n++) { sb.el.scrollTop = sb.el.scrollHeight; await sleep(40); }
    check('scrollBox stops when onEnd says there is no more', pages === 4, pages);
    sb.el.scrollTop = sb.el.scrollHeight; await sleep(40);
    check('scrollBox drops data-below at the end', !sb.el.hasAttribute('data-below'), sb.el.scrollTop);
    sb.top();
    check('scrollBox top() goes back to the first row', sb.el.scrollTop === 0 && sb.el.hasAttribute('data-below'));
    ac.abort();
    const before = pages;
    sb.el.scrollTop = sb.el.scrollHeight; await sleep(40);
    check('scrollBox stops when its signal aborts', pages === before);
    sb.el.remove();
  }

  check('no CSP violations', violations.length === 0 && !window.__pwned, violations.join('; '));
}

run().catch((e) => check('page script', false, e && e.stack)).finally(() => { window.__results = results; });
