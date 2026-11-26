// Tables: a list from the API (paged by cursor, sorted by the server) or from rows the page already has
// (sorted here), with a header that sorts, a "Show more" button, a count, and a Download CSV button on
// every table. Rows can open a page (rowHref). Cells are built by the column's render(row), which
// returns text or elements (never HTML).
// Drawing stays cheap while lists load again (a rebuild sends `rows` events every second): with reuseRows, a row
// whose data is the same as before keeps its element, and when no row changed the table is not touched at all. Rows the page
// already has are drawn 200 at a time; the next 200 come as the foot scrolls into view (or "Show more").

import * as state from '../shared/state.js';
import { h, replace, replaceChanged, cover, holdScroll } from '../shared/dom.js';
import { downloadCsv, csvFilename } from '../shared/csv.js';
import { scrollBox } from '../shared/scrollbox.js';
import { formatNumber, compareText } from '../shared/i18n.js';
import { icon } from './icons.js';
import { csvButton } from './page.js';
import { errorWords } from './states.js';
import { windowList, dayJumpMs } from '../shared/vlist.js';

/**
 * dataTable(ctx, {
 *   title, icon, label,                 the bar above the table (title may be null)
 *   path, params, limit,                an API list (GET path?params&sort=&dir=&limit=&after=)
 *   rows,                               or rows the page already has (no paging)
 *   columns: [{ key, label, num, sort, render(row), csv(row), title, cls, cover }],
 *   sort: { key, dir: 'asc' | 'desc' }, the first order (key: a column key)
 *   keepSort,                           false: a live update does not give the table the reader's order back
 *   onSort(sort),                      called when the person sorts by a header ({ key, dir })
 *   rowHref(row),                       a row opens this page
 *   rowClass(row),                      more classes for a row (a string, or null)
 *   rowTitle(row),                      the row's hover title (e.g. where its data came from), or null
 *   empty(),                            what to show when there are no rows (say why)
 *   csv: true | false | { filename, columns: [{ label, value(row) }], all }   (true: the server's CSV;
 *                                       an object: CSV from the columns' csv specs, all: every page of an API list)
 *   side,                               more elements in the bar (filters, a note)
 *   onPage(page),                       after each API page (the first holds summary or totals)
 *   cover,                              item ids for the table panel
 *   note, noteCover, noteMore,          a line under the bar that explains the table (and its item ids);
 *                                       noteMore: the longer explanation, as the line's hover title
 *   autoMore,                           an API list: the next page loads as the foot scrolls into view
 *                                       (IntersectionObserver); the button stays, for the keyboard
 *   moreLabel,                          the button's words (default "Show more")
 *   reuseRows,                          a row that loads again with the same data keeps its element (only for
 *                                       columns whose cells come from the row alone, not from other rows or state)
 *   box,                                true or the box's accessible name: the rows scroll inside a box of about
 *                                       five rows (shared/scrollbox.js; app.css .scroll-box gives it its height
 *                                       from the start, the column header stays at its top); with autoMore the
 *                                       next page loads as the end of the box comes near. Local rows, and an
 *                                       API list whose first page holds it all, get a box only as high as its
 *                                       rows, up to five (boxFit: true or false sets it for local rows)
 * }) -> { el, reload(params), setParams(patch), rows(), done() }
 *
 * A column's sort is the API's sort name (a string) for API lists, or true for local rows (sorted by
 * the value of row[key], or column.value(row)).
 */
const LOCAL_STEP = 200; // rows the page already has, drawn at a time

// The order the reader picked, by page address and table name: a live update builds the page again, and its tables
// start in the order the reader chose, not the first one. The router forgets them when another page is opened.
const chosenSorts = new Map();
const sortKey = (opts) => { const n = opts.title || opts.label; return n && opts.keepSort !== false ? `${location.pathname}|${n}` : null; };
export function forgetSorts() { chosenSorts.clear(); }
function firstSort(opts, columns) {
  const k = sortKey(opts), c = k && chosenSorts.get(k);
  if (c && columns.some((x) => x.key === c.key && x.sort)) return { ...c };
  return opts.sort ? { ...opts.sort } : null;
}
function rememberSort(opts, sort) { const k = sortKey(opts); if (k && sort) chosenSorts.set(k, { ...sort }); }

export function dataTable(ctx, opts) {
  const { columns, rowHref = null, limit = 50, csv = true } = opts;
  let params = { ...(opts.params || {}) };
  let sort = firstSort(opts, columns);
  let pager = null, loaded = [], done = false, total = null, busy = false, gen = 0;
  let fresh = false, freshQuiet = false, prevPager = null, answered = false;
  let shownLocal = LOCAL_STEP; // rows the page already has: how many are drawn
  // the row elements drawn last, by their data: a row that comes again unchanged keeps its element
  let drawn = new Map();

  const count = h('span', { class: 'tbl-count', 'aria-live': 'polite' });
  // on a phone the rows are cards without the header, so the order is picked here (app.css shows it there)
  const sortable = columns.filter((c) => c.sort);
  const sortPick = sortable.length ? h('select', { class: 'select tbl-sort-pick', 'aria-label': 'Order of the rows', onchange: (e) => {
    const [key, dir] = e.target.value.split(':');
    pickSort(key, dir);
  } }) : null;
  const csvSlot = h('span', { class: 'tbl-csv' });
  const bar = h('div', { class: 'tbl-bar' },
    opts.title ? h('h2', { class: 'panel-title' }, opts.icon ? icon(opts.icon) : null, opts.title) : h('span', { class: 'grow' }),
    opts.side || null, sortPick, count, csvSlot);
  const thead = h('thead');
  const tbody = h('tbody');
  const table = h('table', { class: 'tbl', 'aria-label': opts.label || opts.title || null }, thead, tbody);
  const moreBtn = h('button', { class: 'btn btn--sm', type: 'button', onclick: () => more() }, icon('chev-d', 'ico--sm'), opts.moreLabel || 'Show more');
  const footText = h('span');
  const foot = h('div', { class: 'tbl-foot', hidden: true }, footText, moreBtn);
  const noteEl = opts.note ? h('p', { class: 'note tbl-note', cover: opts.noteCover || null, title: opts.noteMore || null }, opts.note) : null;
  const scroller = h('div', { class: 'tbl-scroll' }, table);
  // box: the rows scroll in a box of about five rows; with autoMore its end marker asks for the next page
  const fit = opts.boxFit ?? !!opts.rows;
  const box = opts.box ? scrollBox({
    el: scroller, cls: ['tbl-box', fit && 'scroll-box--fit'], onCleanup: ctx.onCleanup,
    label: typeof opts.box === 'string' ? opts.box : opts.label || opts.title || null,
    onEnd: opts.autoMore && opts.path ? async () => {
      if (busy || done || foot.hidden || !ctx.alive()) return false;
      const n = loaded.length;
      await more();
      return loaded.length > n;
    } : opts.rows ? async () => {
      // a long local list: its next rows as the end of the box comes near
      const n = shownLocal;
      moreLocal();
      return shownLocal > n && shownLocal < loaded.length + LOCAL_STEP;
    } : null,
  }) : null;
  const el = h('section', { class: ['panel', 'tbl-panel', opts.cls], 'aria-label': opts.label || opts.title || null },
    bar, noteEl, scroller, foot);
  if (opts.cover) cover(el, opts.cover);

  // autoMore (no box): the foot is the sentinel. New rows go under the ones on screen (appended, never
  // re-drawn), so nothing above moves; after each page, a foot still in view (a tall screen) asks for the next.
  const AHEAD = 240;
  let watcher = null;
  if (((opts.autoMore && opts.path) || opts.rows) && !box && typeof IntersectionObserver === 'function') {
    watcher = new IntersectionObserver((entries) => {
      if (!ctx.alive()) { watcher.disconnect(); return; }
      if (entries.some((e) => e.isIntersecting)) more();
    }, { rootMargin: `0px 0px ${AHEAD}px 0px` });
    watcher.observe(foot);
    ctx.onCleanup?.(() => watcher.disconnect());
  }
  function footNear() {
    if (!watcher || foot.hidden || !foot.isConnected) return false;
    const r = foot.getBoundingClientRect();
    return r.height > 0 && r.top < (window.innerHeight || 0) + AHEAD && r.bottom > 0;
  }

  function sortParams() {
    if (!sort) return {};
    const col = columns.find((c) => c.key === sort.key);
    return col && typeof col.sort === 'string' ? { sort: col.sort, dir: sort.dir } : {};
  }

  function head() {
    replace(thead, h('tr', null, columns.map((c) => {
      const sorted = sort && sort.key === c.key;
      const th = h('th', {
        scope: 'col', class: [c.num && 'is-num', c.cls], title: c.title || null,
        'aria-sort': sorted ? (sort.dir === 'asc' ? 'ascending' : 'descending') : null,
      }, c.sort ? h('button', { class: 'tbl-sort', type: 'button', onclick: () => setSort(c.key) }, c.label, icon('chev-d')) : c.label);
      if (c.cover) cover(th, c.cover);
      return th;
    })));
    if (sortPick) {
      replace(sortPick, sort ? null : h('option', { value: '' }, 'Usual order'), sortable.flatMap((c) => {
        const name = labelText(c);
        return [h('option', { value: `${c.key}:desc` }, `${name}: ${c.num ? 'highest' : 'last'} first`),
          h('option', { value: `${c.key}:asc` }, `${name}: ${c.num ? 'lowest' : 'first'} first`)];
      }));
      sortPick.value = sort ? `${sort.key}:${sort.dir}` : '';
    }
  }

  function pickSort(key, dir) {
    if (!key) return;
    sort = { key, dir: dir === 'asc' ? 'asc' : 'desc' };
    head();
    rememberSort(opts, sort);
    opts.onSort?.({ ...sort });
    if (opts.rows) { renderLocal(); return; }
    load();
  }

  function setSort(key) {
    const col = columns.find((c) => c.key === key);
    const dir = sort && sort.key === key ? (sort.dir === 'asc' ? 'desc' : 'asc') : (col.firstDir || (col.num ? 'desc' : 'asc'));
    sort = { key, dir };
    head();
    rememberSort(opts, sort);
    opts.onSort?.({ ...sort });
    if (opts.rows) { renderLocal(); return; }
    load();
  }

  function rowEl(r) {
    const href = rowHref ? rowHref(r) : null;
    const tr = h('tr', { class: [href && 'is-link', opts.rowClass?.(r)], title: opts.rowTitle?.(r) || null }, columns.map((c) => h('td', { class: [c.num && 'is-num', c.cls], dataset: { label: labelText(c) || null } }, c.render ? c.render(r) : r[c.key] ?? '—')));
    if (href) {
      tr.addEventListener('click', (e) => {
        if (e.defaultPrevented || e.target.closest('a, button, input, select, label')) return;
        if (window.getSelection && String(window.getSelection()).length) return;
        state.navigate(href);
      });
    }
    return tr;
  }

  // the elements for rows: an unchanged row (the same data) keeps the element it had; append: rows added under
  // the ones on screen (always new elements, never one taken from higher up)
  const keyOf = (r) => { if (!opts.reuseRows) return null; try { return JSON.stringify(r); } catch (_) { return null; } };
  function remember(k, tr) { if (k == null) return; if (!drawn.has(k)) drawn.set(k, []); drawn.get(k).push(tr); }
  function rowEls(rows, append = false) {
    const was = append ? null : drawn;
    if (!append) drawn = new Map();
    return rows.map((r) => {
      const k = keyOf(r);
      const list = was && k != null && opts.reuseRows ? was.get(k) : null;
      const tr = list && list.length ? list.shift() : rowEl(r);
      remember(k, tr);
      return tr;
    });
  }
  // the body shows exactly these rows: nothing is touched when they are the ones on screen already
  function setBody(trs) {
    const now = tbody.children;
    if (now.length === trs.length && trs.every((tr, i) => now[i] === tr)) return;
    holdScroll(tbody);
    // only the rows that differ are put in (a list that loads again with one drive changed touches one row, not all of
    // them, and the whole-pixel snap then measures that row alone)
    const want = new Set(trs);
    for (const n of Array.from(tbody.childNodes)) if (!want.has(n)) n.remove();
    trs.forEach((tr, i) => { const cur = tbody.children[i]; if (cur !== tr) tbody.insertBefore(tr, cur || null); });
  }

  function skeleton(n = 4) {
    return Array.from({ length: n }, () => h('tr', { class: 'tbl-skel', 'aria-hidden': 'true' }, columns.map(() => h('td', null, h('span', { class: 'skel' })))));
  }

  // The empty note (or the error) of a table wider than its box: it stays in the part of the table that is in
  // view, as wide as that part, so it is centred there and never cut. --tbl-view is the box's inner width.
  let viewWatch = null;
  function emptyRow(node) {
    if (!viewWatch && typeof ResizeObserver === 'function') {
      viewWatch = new ResizeObserver(() => {
        const cs = getComputedStyle(scroller);
        scroller.style.setProperty('--tbl-view', `${Math.max(0, scroller.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight))}px`);
      });
      viewWatch.observe(scroller);
      ctx.onCleanup?.(() => viewWatch.disconnect());
    }
    return h('tr', { class: 'tbl-empty' }, h('td', { colspan: String(columns.length) }, h('div', { class: 'tbl-empty-view' }, node)));
  }

  // text and attributes set to what they already are still count as a change to the page's mutation observers (the
  // whole-pixel snap measures what a change touched), so a live reload that finds the same figures sets nothing
  const text = (el, t) => { if (el.textContent !== t) el.textContent = t; };
  const hide = (el, v) => { if (el.hidden !== v) el.hidden = v; };

  function setCount() {
    if (opts.rows) {
      const n = loaded.length, shown = Math.min(shownLocal, n);
      text(count, `${formatNumber(n)} ${n === 1 ? 'row' : 'rows'}`);
      hide(foot, shown >= n);
      text(footText, `Showing ${formatNumber(shown)} of ${formatNumber(n)}`);
      return;
    }
    const n = loaded.length;
    text(count, total != null ? `${formatNumber(total)} in all` : done ? `${formatNumber(n)} ${n === 1 ? 'row' : 'rows'}` : `${formatNumber(n)} shown`);
    hide(foot, done || !n);
    text(footText, total != null ? `Showing ${formatNumber(n)} of ${formatNumber(total)}` : `Showing ${formatNumber(n)}. There are more.`);
  }

  function setCsv() {
    if (!csv) { replace(csvSlot); return; }
    if (csv === true && opts.path) {
      replaceChanged(csvSlot, csvButton({ href: ctx.api.csvUrl(opts.path, { ...params, ...sortParams() }), filename: csvFilename(ctx.car?.name || 'sentry-mate', opts.csvName || opts.title || 'list') }));
    } else {
      // a column's csv: false (left out), a function (the value), or a shared/csv.js column() (units in the header)
      const cols = csv.columns || columns.filter((c) => c.csv !== false)
        .map((c) => (c.csv && typeof c.csv === 'object' ? c.csv : { label: c.label, value: c.csv || ((r) => r[c.key]) }));
      const name = () => csv.filename || csvFilename(ctx.car?.name || 'sentry-mate', opts.csvName || opts.title || 'table');
      // csv.all on an API list: every row of the list (all pages, same filters and sort), not only the rows shown
      const btn = csvButton({
        onClick: async () => {
          if (!(csv.all && opts.path)) { downloadCsv(name(), opts.rows ? sortedLocal() : loaded, cols); return; }
          btn.disabled = true;
          try {
            const rows = done ? loaded : await ctx.api.list(opts.path, { ...params, ...sortParams() }, { limit: 500 }).all();
            if (ctx.alive()) downloadCsv(name(), rows, cols);
          } catch (e) {
            if (ctx.alive() && e.name !== 'AbortError') opts.onError?.(e);
          } finally { btn.disabled = false; }
        },
      });
      replace(csvSlot, btn);
    }
  }

  function sortedLocal() {
    const rows = [...(opts.rows || [])];
    if (!sort) return rows;
    const col = columns.find((c) => c.key === sort.key);
    const val = col?.value || ((r) => r[sort.key]);
    const k = sort.dir === 'asc' ? 1 : -1;
    return rows.sort((a, b) => {
      const x = val(a), y = val(b);
      if (x == null && y == null) return 0;
      if (x == null) return 1;
      if (y == null) return -1;
      return (typeof x === 'string' ? compareText(x, y) : x - y) * k;
    });
  }

  function renderLocal() {
    loaded = sortedLocal();
    done = true; total = null;
    if (loaded.length) setBody(rowEls(loaded.slice(0, shownLocal)));
    else { drawn = new Map(); replace(tbody, emptyRow(opts.empty ? opts.empty() : h('div', { class: 'empty' }, h('h3', null, 'Nothing to show')))); }
    box?.edge();
    setCount();
    setCsv();
  }
  // the next rows of a long local list, under the ones on screen
  function moreLocal() {
    if (shownLocal >= loaded.length) return;
    const from = shownLocal;
    shownLocal += LOCAL_STEP;
    const add = loaded.slice(from, shownLocal);
    tbody.append(...rowEls(add, true));
    setCount();
    if (watcher && ctx.alive()) requestAnimationFrame(() => { if (footNear()) moreLocal(); });
  }

  // quiet (live refreshes, the default once the table has had an answer): the rows (or the empty state) on
  // screen stay until the new first page arrives, with no skeleton
  async function load(quiet = false) {
    const my = ++gen;
    busy = false; // an earlier load still under way must not block this one
    fresh = true; freshQuiet = quiet && answered;
    if (!freshQuiet) {
      loaded = []; done = false; total = null;
      replace(tbody, skeleton());
      box?.top();   // a new list starts at its top
      foot.hidden = true;
      count.textContent = '';
    }
    prevPager = pager;
    // a live refresh asks for as many rows as are on screen (at most a server page): the list keeps its length, so
    // nothing below it moves and a page scrolled down the list is not cut back to the first page
    const n = freshQuiet ? Math.min(500, Math.max(limit, loaded.length)) : limit;
    pager = ctx.api.pager(opts.path, { ...params, ...sortParams() }, { limit: n });
    if (!freshQuiet) setCsv();
    await more(my);
  }

  async function more(my = gen) {
    if (opts.rows) { moreLocal(); return; }
    if (busy || (done && !fresh) || !pager) return;
    busy = true;
    moreBtn.disabled = true;
    let drew = false;
    try {
      const items = (await pager.more()) || [];
      const pg = pager.last || { items, next: null };
      if (my !== gen || !ctx.alive()) return;
      let reload = false;
      if (fresh) { fresh = false; reload = true; loaded = []; total = null; if (freshQuiet) setCsv(); }
      loaded.push(...items);
      // the first page of a reload replaces the rows on screen, keeping the elements of rows that did not change
      if (reload) setBody(rowEls(items)); else tbody.append(...rowEls(items, true));
      const first = !answered;
      answered = true;
      done = !pg.next;
      // a box whose whole list came with the first page (before the page is shown) is only as high as its rows
      if (box && first && done) scroller.classList.add('scroll-box--fit');
      if (pg.total != null) total = pg.total;
      if (!loaded.length) replace(tbody, emptyRow(opts.empty ? opts.empty(pg) : h('div', { class: 'empty' }, h('h3', null, 'Nothing in this period'))));
      setCount();
      drew = true;
      opts.onPage?.(pg);
    } catch (e) {
      if (my !== gen || !ctx.alive() || e.name === 'AbortError') return;
      // a failed live refresh keeps the rows on screen; the next refresh tries again
      if (fresh && freshQuiet) { fresh = false; pager = prevPager; return; }
      fresh = false;
      const w = errorWords(e);
      replace(tbody, emptyRow(h('div', { class: 'empty empty--compact', role: 'alert' }, h('h3', null, w.title), h('p', null, w.text),
        h('button', { class: 'btn btn--sm', type: 'button', onclick: () => load() }, icon('reset'), 'Try again'))));
      opts.onError?.(e);
    } finally {
      // a stale load leaves busy alone: load() cleared it and the current load owns it now
      if (my === gen) { busy = false; moreBtn.disabled = false; }
    }
    if (drew && my === gen && box && ctx.alive()) box.recheck();
    if (drew && my === gen && watcher && !done && ctx.alive()) requestAnimationFrame(() => { if (my === gen && !busy && !done && footNear()) more(); });
  }

  head();
  const ready = opts.rows ? (renderLocal(), Promise.resolve()) : load();

  return {
    el,
    /** Resolves when the first page has been drawn. */
    ready,
    /** Load again from the first page, with new filters if given. The rows on screen stay until the new ones
     * arrive (no skeleton, nothing jumps); { quiet: false } shows the skeleton instead. */
    reload(p, o = {}) { if (p) params = { ...p }; if (opts.rows) renderLocal(); else return load(o.quiet !== false); return Promise.resolve(); },
    /** Set the order shown in the header ({ key, dir } or null) without loading; reload() follows. */
    setSortKey(next) { sort = next ? { ...next } : null; head(); },
    /** Change some filters (null removes one) and load again. */
    setParams(patch) {
      for (const [k, v] of Object.entries(patch)) { if (v == null || v === '') delete params[k]; else params[k] = v; }
      return load();
    },
    /** New local rows. */
    setRows(rows) { opts.rows = rows; renderLocal(); },
    rows: () => loaded,
    done: () => done,
  };
}

// a column's name as text: the caption of its value when the row is a card on a phone
function labelText(c) {
  if (c.cardLabel != null) return c.cardLabel;
  if (typeof c.label === 'string') return c.label;
  return c.label?.textContent?.trim() || c.title || '';
}

/**
 * For a table a page builds itself (class "tbl"): give each cell its column's name from the header, so on
 * a phone, where the rows are cards, every value has its caption. Call after the rows are in.
 */
export function cardLabels(table) {
  const names = [...table.querySelectorAll('thead th')].map((th) => th.textContent.trim());
  for (const tr of table.querySelectorAll('tbody tr')) {
    [...tr.children].forEach((td, i) => { if (names[i] && !td.dataset.label) td.dataset.label = names[i]; });
  }
  return table;
}

/**
 * windowTable(ctx, {
 *   title, icon, label, cls, cover, note,   as dataTable
 *   path, params, limit,                    an API list paged by cursor and by offset ({ items, next, offset, total })
 *   columns: [{ key, label, num, sort, render(row), title, cls, cover }],   sort: the API's sort name (a string)
 *   sort: { key, dir }, firstSort,          the first order; the order "jump to a date" works in (firstSort.key)
 *   rowHref(row), rowClass(row), empty(page),
 *   search: { label, placeholder } | null,  a search field (the API's q), applied as typing pauses
 *   jump: true,                             a "Jump to date" field (the API's at_ms), in the first order only
 *   csv(params): href,                      the Download CSV link (the server's, every row the filters show)
 *   onPage(page),                           after each answer (every one holds the totals of the whole list)
 *   boxLabel,                               the box's accessible name
 *   load({ ...params, sort: { key, dir }, offset, limit }) -> { items, next, offset, total }
 *                                           in place of path: a list the page holds already (a chunk of it, at once)
 *   rows: N,                                a box N rows high instead of about ten
 * }) -> { el, ready, setParams(patch), refresh(), total() }
 *
 * The rows sit in a box about ten rows high that scrolls under a header that stays: only the rows in view and a few
 * more are in the page (shared/vlist.js), and they load from the API a chunk at a time as the box reaches them. The
 * pack's CSS gives .vl-box its height and every row the same height.
 */
export function windowTable(ctx, opts) {
  const { columns, rowHref = null } = opts;
  let params = { ...(opts.params || {}) };
  let sort = firstSort(opts, columns);
  const base = opts.firstSort || opts.sort || null;
  let total = null;

  const count = h('span', { class: 'tbl-count', 'aria-live': 'polite' });
  const csvSlot = h('span', { class: 'tbl-csv' });
  const search = opts.search ? h('input', {
    class: 'input input--sm vl-search', type: 'search', 'aria-label': opts.search.label || 'Search', placeholder: opts.search.placeholder || 'Search',
    title: opts.search.title || null, value: params.q || '',
  }) : null;
  const jumpIn = opts.jump ? h('input', { class: 'input input--sm vl-jump-date', type: 'date', 'aria-label': 'Jump to date', title: 'Scroll the table to the first drive of this day' }) : null;
  const jumpBtn = opts.jump ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => jump() }, 'Jump to date') : null;
  const jumpBox = opts.jump ? h('span', { class: 'vl-jump' }, jumpIn, jumpBtn) : null;
  const bar = h('div', { class: 'tbl-bar' },
    opts.title ? h('h2', { class: 'panel-title' }, opts.icon ? icon(opts.icon) : null, opts.title) : h('span', { class: 'grow' }),
    opts.side || null, search, jumpBox, count, csvSlot);
  const thead = h('thead');
  const tbody = h('tbody', { dataset: { vlBody: '' } });
  const table = h('table', { class: 'tbl tbl--keep vl-table', 'aria-label': opts.label || opts.title || null }, thead, tbody);
  const noteEl = opts.note ? h('p', { class: 'note tbl-note', cover: opts.noteCover || null }, opts.note) : null;
  const box = h('div', { class: ['tbl-scroll tbl-box vl-box', opts.rows && 'vl-box--rows'], vars: opts.rows ? { '--vl-rows': String(opts.rows) } : null, tabindex: '0', role: 'region', 'aria-label': opts.boxLabel || opts.label || opts.title || null }, table);
  const emptyBox = h('div', { class: 'vl-empty', hidden: true });
  const el = h('section', { class: ['panel', 'tbl-panel', 'vl-panel', opts.cls], 'aria-label': opts.label || opts.title || null }, bar, noteEl, box, emptyBox);
  if (opts.cover) cover(el, opts.cover);

  const sortParams = () => {
    const col = sort && columns.find((c) => c.key === sort.key);
    return col && typeof col.sort === 'string' ? { sort: col.sort, dir: sort.dir } : {};
  };
  const query = () => ({ ...params, ...sortParams() });

  function head() {
    replace(thead, h('tr', null, columns.map((c) => {
      const sorted = sort && sort.key === c.key;
      const th = h('th', {
        scope: 'col', class: [c.num && 'is-num', c.cls], title: c.title || null,
        'aria-sort': sorted ? (sort.dir === 'asc' ? 'ascending' : 'descending') : null,
      }, c.sort ? h('button', { class: 'tbl-sort', type: 'button', onclick: () => setSort(c.key) }, c.label, icon('chev-d')) : c.label);
      if (c.cover) cover(th, c.cover);
      return th;
    })));
  }

  function rowEl(r, i) {
    const href = rowHref ? rowHref(r) : null;
    const tr = h('tr', { class: [href && 'is-link', opts.rowClass?.(r)], 'aria-rowindex': String(i + 2), dataset: { vlRow: String(i) } },
      columns.map((c) => h('td', { class: [c.num && 'is-num', c.cls], dataset: { label: labelText(c) || null } }, c.render ? c.render(r) : r[c.key] ?? '—')));
    if (href) {
      tr.addEventListener('click', (e) => {
        if (e.defaultPrevented || e.target.closest('a, button, input, select, label')) return;
        if (window.getSelection && String(window.getSelection()).length) return;
        state.navigate(href);
      });
    }
    return tr;
  }
  const skelRow = (i) => h('tr', { class: 'tbl-skel', 'aria-rowindex': String(i + 2) }, columns.map(() => h('td', null, h('span', { class: 'skel' }))));
  const gapRow = () => h('tr', null, h('td', { colspan: String(columns.length) }));

  const list = windowList({
    box, body: tbody, head: thead, limit: opts.limit || 50, overscan: 6, rowHeight: 55,
    row: rowEl, placeholder: skelRow, spacer: gapRow,
    load: (p) => (opts.load ? Promise.resolve(opts.load({ ...query(), sort: sort ? { ...sort } : null, ...pick(p) })) : ctx.api.get(opts.path, { ...query(), ...pick(p) }, { signal: p.signal })),
    onPage: (pg) => { if (pg.total !== total) { total = pg.total; paintCount(); } opts.onPage?.(pg); },
    onError: (e) => opts.onError?.(e),
  });
  ctx.onCleanup?.(() => list.destroy());

  function paintCount() {
    const n = total ?? 0;
    count.textContent = `${formatNumber(n)} ${n === 1 ? 'row' : 'rows'}`;
    table.setAttribute('aria-rowcount', String(n + 1));
    box.hidden = !n;
    // (set only when it changes: the whole-pixel snap takes every write of `hidden` for a box shown or hidden)
    if (emptyBox.hidden !== !!n) emptyBox.hidden = !!n;
    if (!n) replace(emptyBox, opts.empty ? opts.empty({ total: n }) : h('div', { class: 'empty' }, h('h3', null, 'Nothing to show')));
    if (jumpBox) {
      const can = !!n && sort && base && sort.key === base.key;
      jumpIn.disabled = !can; jumpBtn.disabled = !can;
      jumpBox.title = can ? '' : 'Jump to date works in the order of the drives (sort by number or date).';
    }
  }
  function paintCsv() {
    if (opts.csv) replace(csvSlot, csvButton({ href: opts.csv(query()), filename: csvFilename(ctx.car?.name || 'sentry-mate', opts.csvName || opts.title || 'table') }));
  }

  async function restart() {
    total = null;
    paintCsv();
    const pg = await list.start();
    if (pg) { total = pg.total; paintCount(); opts.onPage?.(pg); }
    return pg;
  }

  function setSort(key) {
    const col = columns.find((c) => c.key === key);
    const dir = sort && sort.key === key ? (sort.dir === 'asc' ? 'desc' : 'asc') : (col.firstDir || (col.num ? 'desc' : 'asc'));
    sort = { key, dir };
    rememberSort(opts, sort);
    head();
    restart().catch((e) => opts.onError?.(e));
  }

  async function jump() {
    const key = jumpIn.value;
    if (!key) { jumpIn.focus(); return; }
    try {
      const pg = await list.jump(dayJumpMs(key, ctx.tz, sort?.dir === 'desc'));
      if (pg) { total = pg.total; paintCount(); box.focus({ preventScroll: true }); }
    } catch (e) { opts.onError?.(e); }
  }
  jumpIn?.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); jump(); } });

  if (search) {
    let t = 0;
    search.addEventListener('input', () => {
      clearTimeout(t);
      t = setTimeout(() => {
        const v = search.value.trim();
        if ((params.q || '') === v) return;
        if (v) params.q = v; else delete params.q;
        opts.onSearch?.(v);
        restart().catch((e) => opts.onError?.(e));
      }, 300);
    });
    ctx.onCleanup?.(() => clearTimeout(t));
  }

  head();
  const ready = restart();

  return {
    el,
    ready,
    /** Change some filters (null removes one) and load again from the top. */
    setParams(patch) {
      for (const [k, v] of Object.entries(patch)) { if (v == null || v === '') delete params[k]; else params[k] = v; }
      return restart();
    },
    /** The rows in view again (a live change), with nothing moving until they are in. */
    refresh() { paintCsv(); return list.refresh().then((pg) => { if (pg) { total = pg.total; paintCount(); } return pg; }); },
    total: () => total ?? 0,
    params: () => query(),
  };
}

// the paging parameters vlist asks with (offset, after or at_ms)
function pick(p) {
  const out = {};
  if (p.after) out.after = p.after;
  else if (p.at_ms != null) out.at_ms = p.at_ms;
  else out.offset = p.offset || 0;
  out.limit = p.limit;
  return out;
}
