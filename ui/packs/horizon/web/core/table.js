// Tables: a list from the API (paged by cursor, sorted by the server) or from rows the page already has
// (sorted here), with a header that sorts, a "Show more" button, a count, and a Download CSV button on
// every table. Rows can open a page (rowHref). Cells are built by the column's render(row), which
// returns text or elements (never HTML).
// Drawing stays cheap while lists load again (a rebuild sends `rows` events every second): with reuseRows, a row
// whose data is the same as before keeps its element, and when no row changed the table is not touched at all. Rows the page
// already has are drawn 200 at a time; the next 200 come as the foot scrolls into view (or "Show more").

import * as state from '../shared/state.js';
import { h, replace, cover } from '../shared/dom.js';
import { downloadCsv, csvFilename } from '../shared/csv.js';
import { formatNumber, compareText } from '../shared/i18n.js';
import { icon } from './icons.js';
import { csvButton } from './page.js';
import { errorWords } from './states.js';

/**
 * dataTable(ctx, {
 *   title, icon, label,                 the bar above the table (title may be null)
 *   path, params, limit,                an API list (GET path?params&sort=&dir=&limit=&after=)
 *   rows,                               or rows the page already has (no paging)
 *   columns: [{ key, label, num, sort, render(row), csv(row), title, cls, cover }],
 *   sort: { key, dir: 'asc' | 'desc' }, the first order (key: a column key)
 *   onSort(sort),                      called when the person sorts by a header ({ key, dir })
 *   rowHref(row),                       a row opens this page
 *   rowClass(row),                      more classes for a row (a string, or null)
 *   empty(),                            what to show when there are no rows (say why)
 *   csv: true | false | { filename, columns: [{ label, value(row) }], all }   (true: the server's CSV;
 *                                       an object: CSV from the columns' csv specs, all: every page of an API list)
 *   side,                               more elements in the bar (filters, a note)
 *   onPage(page),                       after each API page (the first holds summary or totals)
 *   cover,                              item ids for the table panel
 *   note, noteCover,                    a line under the bar that explains the table (and its item ids)
 *   autoMore,                           an API list: the next page loads as the foot scrolls into view
 *                                       (IntersectionObserver); the button stays, for the keyboard
 *   moreLabel,                          the button's words (default "Show more")
 *   reuseRows,                          a row that loads again with the same data keeps its element (only for
 *                                       columns whose cells come from the row alone, not from other rows or state)
 *   box,                                true or the box's accessible name: the rows scroll inside a box (the
 *                                       page's CSS gives .tbl-box its height, from the start), and with autoMore
 *                                       the next page loads as the end of the box scrolls into view; the box
 *                                       is a focusable region (arrow keys scroll it), data-below while rows
 *                                       are hidden under its bottom edge (for a fade)
 * }) -> { el, reload(params), setParams(patch), rows(), done() }
 *
 * A column's sort is the API's sort name (a string) for API lists, or true for local rows (sorted by
 * the value of row[key], or column.value(row)).
 */
const LOCAL_STEP = 200; // rows the page already has, drawn at a time

export function dataTable(ctx, opts) {
  const { columns, rowHref = null, limit = 50, csv = true } = opts;
  let params = { ...(opts.params || {}) };
  let sort = opts.sort ? { ...opts.sort } : null;
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
  const noteEl = opts.note ? h('p', { class: 'note tbl-note', cover: opts.noteCover || null }, opts.note) : null;
  const boxEnd = opts.box ? h('div', { class: 'tbl-box-end', 'aria-hidden': 'true' }) : null;
  const scroller = h('div', opts.box
    ? { class: 'tbl-scroll tbl-box', tabindex: '0', role: 'region', 'aria-label': typeof opts.box === 'string' ? opts.box : opts.label || opts.title || null }
    : { class: 'tbl-scroll' }, table, boxEnd);
  const el = h('section', { class: ['panel', 'tbl-panel', opts.cls], 'aria-label': opts.label || opts.title || null },
    bar, noteEl, scroller, foot);
  if (opts.cover) cover(el, opts.cover);

  // autoMore: the foot is the sentinel (in a box: the end of the box, watched against the box). New rows go
  // under the ones on screen (appended, never re-drawn), so nothing above moves; after each page, a foot
  // still in view (a tall screen, a box not yet full) asks for the next one.
  const AHEAD = opts.box ? 96 : 240;
  let watcher = null;
  if (((opts.autoMore && opts.path) || opts.rows) && typeof IntersectionObserver === 'function') {
    watcher = new IntersectionObserver((entries) => {
      if (!ctx.alive()) { watcher.disconnect(); return; }
      if (entries.some((e) => e.isIntersecting) && !foot.hidden) more();
    }, { root: opts.box ? scroller : null, rootMargin: `0px 0px ${AHEAD}px 0px` });
    watcher.observe(boxEnd || foot);
    ctx.onCleanup?.(() => watcher.disconnect());
  }
  function footNear() {
    if (!watcher || foot.hidden || !foot.isConnected) return false;
    if (opts.box) {
      const b = scroller.getBoundingClientRect();
      return b.height > 0 && boxEnd.getBoundingClientRect().top < b.bottom + AHEAD;
    }
    const r = foot.getBoundingClientRect();
    return r.height > 0 && r.top < (window.innerHeight || 0) + AHEAD && r.bottom > 0;
  }
  // a box: data-below while rows are hidden under its bottom edge
  function boxEdge() {
    if (!opts.box) return;
    scroller.toggleAttribute('data-below', scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight > 1);
  }
  if (opts.box) {
    scroller.addEventListener('scroll', boxEdge, { passive: true });
    if (typeof ResizeObserver === 'function') {
      const ro = new ResizeObserver(() => boxEdge());
      ro.observe(scroller);
      ro.observe(table);
      ctx.onCleanup?.(() => ro.disconnect());
    }
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
    opts.onSort?.({ ...sort });
    if (opts.rows) { renderLocal(); return; }
    load();
  }

  function setSort(key) {
    const col = columns.find((c) => c.key === key);
    const dir = sort && sort.key === key ? (sort.dir === 'asc' ? 'desc' : 'asc') : (col.firstDir || (col.num ? 'desc' : 'asc'));
    sort = { key, dir };
    head();
    opts.onSort?.({ ...sort });
    if (opts.rows) { renderLocal(); return; }
    load();
  }

  function rowEl(r) {
    const href = rowHref ? rowHref(r) : null;
    const tr = h('tr', { class: [href && 'is-link', opts.rowClass?.(r)] }, columns.map((c) => h('td', { class: [c.num && 'is-num', c.cls], dataset: { label: labelText(c) || null } }, c.render ? c.render(r) : r[c.key] ?? '—')));
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
    tbody.replaceChildren(...trs);
  }

  function skeleton(n = 4) {
    return Array.from({ length: n }, () => h('tr', { class: 'tbl-skel', 'aria-hidden': 'true' }, columns.map(() => h('td', null, h('span', { class: 'skel' })))));
  }

  function emptyRow(node) {
    return h('tr', { class: 'tbl-empty' }, h('td', { colspan: String(columns.length) }, node));
  }

  function setCount() {
    if (opts.rows) {
      const n = loaded.length, shown = Math.min(shownLocal, n);
      count.textContent = `${formatNumber(n)} ${n === 1 ? 'row' : 'rows'}`;
      foot.hidden = shown >= n;
      footText.textContent = `Showing ${formatNumber(shown)} of ${formatNumber(n)}`;
      return;
    }
    const n = loaded.length;
    count.textContent = total != null ? `${formatNumber(total)} in all` : done ? `${formatNumber(n)} ${n === 1 ? 'row' : 'rows'}` : `${formatNumber(n)} shown`;
    foot.hidden = done || !n;
    footText.textContent = total != null ? `Showing ${formatNumber(n)} of ${formatNumber(total)}` : `Showing ${formatNumber(n)}. There are more.`;
  }

  function setCsv() {
    if (!csv) { replace(csvSlot); return; }
    if (csv === true && opts.path) {
      replace(csvSlot, csvButton({ href: ctx.api.csvUrl(opts.path, { ...params, ...sortParams() }), filename: csvFilename(ctx.car?.name || 'sentry-mate', opts.csvName || opts.title || 'list') }));
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
      if (opts.box) scroller.scrollTop = 0;   // a new list starts at its top
      foot.hidden = true;
      count.textContent = '';
    }
    prevPager = pager;
    pager = ctx.api.pager(opts.path, { ...params, ...sortParams() }, { limit });
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
      let first = false;
      if (fresh) { fresh = false; first = true; loaded = []; total = null; if (freshQuiet) setCsv(); }
      loaded.push(...items);
      // the first page replaces the rows on screen, keeping the elements of rows that did not change
      if (first) setBody(rowEls(items)); else tbody.append(...rowEls(items, true));
      answered = true;
      done = !pg.next;
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
