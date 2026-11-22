// Tables: a list from the API (paged by cursor, sorted by the server) or from rows the page already has
// (sorted here), with a header that sorts, a "Show more" button, a count, and a Download CSV button on
// every table. Rows can open a page (rowHref). Cells are built by the column's render(row), which
// returns text or elements (never HTML).

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
 *   empty(),                            what to show when there are no rows (say why)
 *   csv: true | false | { filename, columns: [{ label, value(row) }], all }   (true: the server's CSV;
 *                                       an object: CSV from the columns' csv specs, all: every page of an API list)
 *   side,                               more elements in the bar (filters, a note)
 *   onPage(page),                       after each API page (the first holds summary or totals)
 *   cover,                              item ids for the table panel
 *   note, noteCover,                    a line under the bar that explains the table (and its item ids)
 * }) -> { el, reload(params), setParams(patch), rows(), done() }
 *
 * A column's sort is the API's sort name (a string) for API lists, or true for local rows (sorted by
 * the value of row[key], or column.value(row)).
 */
export function dataTable(ctx, opts) {
  const { columns, rowHref = null, limit = 50, csv = true } = opts;
  let params = { ...(opts.params || {}) };
  let sort = opts.sort ? { ...opts.sort } : null;
  let pager = null, loaded = [], done = false, total = null, busy = false, gen = 0;
  let fresh = false, freshQuiet = false, prevPager = null, answered = false;

  const count = h('span', { class: 'tbl-count', 'aria-live': 'polite' });
  const csvSlot = h('span', { class: 'tbl-csv' });
  const bar = h('div', { class: 'tbl-bar' },
    opts.title ? h('h2', { class: 'panel-title' }, opts.icon ? icon(opts.icon) : null, opts.title) : h('span', { class: 'grow' }),
    opts.side || null, count, csvSlot);
  const thead = h('thead');
  const tbody = h('tbody');
  const table = h('table', { class: 'tbl', 'aria-label': opts.label || opts.title || null }, thead, tbody);
  const moreBtn = h('button', { class: 'btn btn--sm', type: 'button', onclick: () => more() }, icon('chev-d', 'ico--sm'), 'Show more');
  const footText = h('span');
  const foot = h('div', { class: 'tbl-foot', hidden: true }, footText, moreBtn);
  const noteEl = opts.note ? h('p', { class: 'note tbl-note', cover: opts.noteCover || null }, opts.note) : null;
  const el = h('section', { class: ['panel', 'tbl-panel', opts.cls], 'aria-label': opts.label || opts.title || null },
    bar, noteEl, h('div', { class: 'tbl-scroll' }, table), foot);
  if (opts.cover) cover(el, opts.cover);

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
    const tr = h('tr', { class: href ? 'is-link' : null }, columns.map((c) => h('td', { class: [c.num && 'is-num', c.cls] }, c.render ? c.render(r) : r[c.key] ?? '—')));
    if (href) {
      tr.addEventListener('click', (e) => {
        if (e.defaultPrevented || e.target.closest('a, button, input, select, label')) return;
        if (window.getSelection && String(window.getSelection()).length) return;
        state.navigate(href);
      });
    }
    return tr;
  }

  function skeleton(n = 4) {
    return Array.from({ length: n }, () => h('tr', { class: 'tbl-skel', 'aria-hidden': 'true' }, columns.map(() => h('td', null, h('span', { class: 'skel' })))));
  }

  function emptyRow(node) {
    return h('tr', { class: 'tbl-empty' }, h('td', { colspan: String(columns.length) }, node));
  }

  function setCount() {
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
    replace(tbody, loaded.length ? loaded.map(rowEl) : emptyRow(opts.empty ? opts.empty() : h('div', { class: 'empty' }, h('h3', null, 'Nothing to show'))));
    setCount();
    setCsv();
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
      foot.hidden = true;
      count.textContent = '';
    }
    prevPager = pager;
    pager = ctx.api.pager(opts.path, { ...params, ...sortParams() }, { limit });
    if (!freshQuiet) setCsv();
    await more(my);
  }

  async function more(my = gen) {
    if (busy || (done && !fresh) || !pager) return;
    busy = true;
    moreBtn.disabled = true;
    try {
      const items = (await pager.more()) || [];
      const pg = pager.last || { items, next: null };
      if (my !== gen || !ctx.alive()) return;
      if (fresh) { fresh = false; loaded = []; total = null; tbody.replaceChildren(); if (freshQuiet) setCsv(); }
      loaded.push(...items);
      tbody.append(...items.map(rowEl));
      answered = true;
      done = !pg.next;
      if (pg.total != null) total = pg.total;
      if (!loaded.length) replace(tbody, emptyRow(opts.empty ? opts.empty(pg) : h('div', { class: 'empty' }, h('h3', null, 'Nothing in this period'))));
      setCount();
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
