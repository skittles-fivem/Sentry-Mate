// Sentry Mate, "Logbook" UI pack: tables (the ledger).
//
// dataTable() draws a list resource as a ruled table with a caption line ("41 drives"), sortable column
// heads, "Show more" paging by cursor and a visible Download CSV button. Two kinds of source:
//   - path + params: a list resource of the API, paged by cursor (shared/api.js pager). Sorting is done by
//     the server when the column names a sort key the resource takes (sort: 'distance'); the CSV link is
//     the same resource with format=csv and the same filters, so it holds every matching row.
//   - rows: rows the page already has (a chart's data, a small fixed list). Sorting is done here; the CSV
//     is built in the browser (shared/csv.js).
// Cells are built by each column's cell(row) (nodes or text), so values keep their units.

import { h } from '../shared/dom.js';
import { downloadCsv, csvFilename } from '../shared/csv.js';
import { compareText, formatNumber } from '../shared/i18n.js';
import { icon, empty as emptyView, errorView, loadingBlock } from './ui.js';

/**
 * dataTable(ctx, {
 *   columns: [{ key, label, cell(row) -> node | text, sort: 'server_key' | true | false, value(row) (client
 *              sort and CSV value), num: true (right-aligned figures), cls, cover, hint (title text) }],
 *   path, params, limit,              // a cursor-paged list resource ...
 *   rows,                             // ... or rows already in hand
 *   sort: { key, dir: 'asc' | 'desc' },// the starting order (column key)
 *   itemName: ['drive', 'drives'],    // for the caption: '41 drives'
 *   csv: { filename, columns } | false, // client CSV columns (shared/csv.js column()); server CSV needs nothing
 *   empty: () => node,                // what to say when nothing matches (say which filter removed it all)
 *   rowAttrs(row) -> attrs,           // a class, a data- value, a cover id for a row
 *   onRows(items, page),              // after each page arrives (summaries in the answer: page.summary)
 *   onSort({ key, dir }),             // a column head was clicked, before the rows load again in that order
 *   caption: node,                    // extra text after the count
 *   sortAll: 2000,                    // a paged list: a client sort (sort: true) first loads every page, up to
 *                                     // this many rows, so the order covers the whole list, not one page
 *   cover, cls, label,                // ids for the table, extra class, the table's accessible name
 * }) -> { el, reload(params), items, destroy() }
 */
export function dataTable(ctx, opts) {
  const { columns, path = null, limit = 50, itemName = ['row', 'rows'], csv = {}, cover = null, cls = null, label = null } = opts;
  let params = { ...(opts.params || {}) };
  let sort = opts.sort ? { ...opts.sort } : null;
  let rows = opts.rows ? [...opts.rows] : null;
  const pager = path ? ctx.api.pager(path, serverParams(), { limit }) : null;
  let total = null;
  let drawn = false;                // something (rows or the empty view) is on screen: never show loading again

  const count = h('p', { class: 'ledger-count', 'aria-live': 'polite' });
  const csvBtn = csv === false ? null : path
    ? h('a', { class: 'button button-quiet button-small', href: '#', download: '', dataset: { csv: '' } }, icon('download'), 'Download CSV')
    : h('button', { type: 'button', class: 'button button-quiet button-small', dataset: { csv: '' }, onclick: () => clientCsv() }, icon('download'), 'Download CSV');
  const head = h('thead', null, h('tr', null, columns.map((c) => headCell(c))));
  const body = h('tbody');
  const table = h('table', { class: 'ledger-table', 'aria-label': label }, head, body);
  const more = h('button', { type: 'button', class: 'button button-quiet', hidden: true, onclick: () => loadMore() }, 'Show more');
  const foot = h('div', { class: 'ledger-foot' }, more);
  const status = h('div', { class: 'ledger-status' });
  const el = h('div', { class: ['ledger', cls], cover },
    h('div', { class: 'ledger-top' }, count, opts.caption ? h('p', { class: 'ledger-caption' }, opts.caption) : null, csvBtn),
    h('div', { class: 'ledger-scroll' }, table), status, foot);

  function serverParams() {
    const p = { ...params };
    const col = sort && columns.find((c) => c.key === sort.key);
    if (col && typeof col.sort === 'string') { p.sort = col.sort; p.dir = sort.dir; }
    return p;
  }
  function headCell(c) {
    const sortable = !!c.sort && (typeof c.sort === 'string' ? !!path : true);
    const on = sort && sort.key === c.key;
    const th = h('th', { scope: 'col', class: [c.cls, { num: c.num, 'is-sortable': sortable }], title: c.hint || null,
      'aria-sort': on ? (sort.dir === 'asc' ? 'ascending' : 'descending') : null, cover: c.cover || null });
    if (!sortable) { th.append(c.label); return th; }
    th.append(h('button', { type: 'button', class: 'sort-button', onclick: () => sortBy(c) }, c.label,
      h('span', { class: 'sort-mark', 'aria-hidden': 'true' }, on ? (sort.dir === 'asc' ? '▲' : '▼') : '')));
    return th;
  }
  function sortBy(c) {
    const dir = sort && sort.key === c.key && sort.dir === 'desc' ? 'asc' : sort && sort.key === c.key ? 'desc' : (c.firstDir || (c.num ? 'desc' : 'asc'));
    sort = { key: c.key, dir };
    opts.onSort?.({ ...sort });
    head.replaceChildren(h('tr', null, columns.map((x) => headCell(x))));
    if (pager && typeof c.sort === 'string') { total = null; pager.reset(serverParams()); quietLoad(0).then((ok) => ok && draw()); }
    else { draw(); fillForSort(); }
  }
  function clientSorted() { return !!(sort && columns.find((c) => c.key === sort.key && c.sort === true)); }
  // A client sort of a paged list orders only what is loaded: load the rest first (opts.sortAll rows at most).
  let filling = false;
  async function fillForSort() {
    if (!pager || !opts.sortAll || !clientSorted() || filling) return;
    filling = true;
    try {
      while (!pager.done && pager.items.length < opts.sortAll && clientSorted() && !ctx.signal.aborted) {
        count.textContent = `${formatNumber(pager.items.length)} ${itemName[1]} loaded, loading the rest to sort them all…`;
        if (!(await loadMore())) break;
      }
    } finally { filling = false; }
  }
  function sortedClient(list) {
    const c = sort && columns.find((x) => x.key === sort.key);
    if (!c) return list;
    const val = c.value || ((r) => r[c.key]);
    const k = sort.dir === 'asc' ? 1 : -1;
    return [...list].sort((a, b) => {
      const x = val(a), y = val(b);
      if (x == null && y == null) return 0;
      if (x == null) return 1;
      if (y == null) return -1;
      return (typeof x === 'number' && typeof y === 'number' ? x - y : compareText(String(x), String(y))) * k;
    });
  }
  const rowOf = new WeakMap();
  function rowEl(r) {
    const attrs = opts.rowAttrs ? opts.rowAttrs(r) || {} : {};
    const tr = h('tr', attrs, columns.map((c) => {
      const v = c.cell ? c.cell(r) : r[c.key];
      return h(c.th ? 'th' : 'td', { class: [c.cls, { num: c.num }], scope: c.th ? 'row' : null }, v == null ? '—' : v);
    }));
    rowOf.set(r, tr);
    return tr;
  }
  function items() { return pager ? pager.items : rows; }
  function draw() {
    const list = pager && !clientSorted() ? pager.items : sortedClient(items() || []);
    body.replaceChildren(...list.map(rowEl));
    const n = (items() || []).length;
    const [one, many] = itemName;
    const word = (x) => (x === 1 ? one : many);
    if (pager) {
      count.textContent = total != null ? `${formatNumber(total)} ${word(total)}` : pager.done ? `${formatNumber(n)} ${word(n)}` : `${formatNumber(n)} ${many} shown, more to load`;
      if (!pager.done && clientSorted()) count.textContent += `; sorted within the ${formatNumber(n)} shown`;
      more.hidden = pager.done;
    } else {
      count.textContent = `${formatNumber(n)} ${word(n)}`;
    }
    status.replaceChildren(n ? '' : (opts.empty ? opts.empty() : emptyView({ title: `No ${many}.` })));
    table.hidden = !n;
    if (csvBtn && path) csvBtn.setAttribute('href', ctx.api.csvUrl(path, serverParams()));
    if (csvBtn) csvBtn.hidden = !n;
    drawn = true;
  }
  async function loadMore(rethrow = false) {
    more.disabled = true;
    if (!pager.items.length && !drawn) status.replaceChildren(loadingBlock(4));
    try {
      const got = await pager.more();
      if (pager.last && typeof pager.last.total === 'number') total = pager.last.total;
      opts.onRows?.(got, pager.last);
      draw();
      return true;
    } catch (err) {
      if (ctx.signal.aborted) return false;
      status.replaceChildren(errorView(err, { retry: () => loadMore().then((ok) => ok && fillForSort()), eyebrow: 'Could not load the list' }));
      if (rethrow) throw err;
      return false;
    } finally {
      more.disabled = false;
    }
  }
  // Load pages without drawing until at least `want` rows (or the end) are in; then the caller draws once.
  async function quietLoad(want) {
    try {
      do {
        const got = await pager.more();
        if (pager.last && typeof pager.last.total === 'number') total = pager.last.total;
        opts.onRows?.(got, pager.last);
      } while (!pager.done && pager.items.length < want && !ctx.signal.aborted);
      return !ctx.signal.aborted;
    } catch (err) {
      if (ctx.signal.aborted) return false;
      status.replaceChildren(errorView(err, { retry: () => loadMore().then((ok) => ok && fillForSort()), eyebrow: 'Could not load the list' }));
      return false;
    }
  }
  function clientCsv() {
    const list = sortedClient(rows || []);
    const cols = csv.columns || columns.map((c) => ({ label: c.label, value: c.value || ((r) => r[c.key]) }));
    downloadCsv(csv.filename || csvFilename(...(csv.parts || ['table'])), list, cols);
  }

  const ctl = {
    el,
    /** The first page (call once after putting el in the page); resolves when it is drawn. */
    async load() { if (pager) { await loadMore(true); fillForSort(); } else { draw(); opts.onRows?.(rows, null); } },
    /** New filters: start again from the first page. opts.sort: a new column order, or null for the resource's own. */
    async reload(newParams, o = {}) {
      if (newParams) params = { ...newParams };
      if ('sort' in o) { sort = o.sort ? { ...o.sort } : null; head.replaceChildren(h('tr', null, columns.map((x) => headCell(x)))); }
      total = null;
      if (!pager) { draw(); return; }
      // The rows on screen stay until the new ones are in (no loading text, no empty flash). A refresh with the
      // same filters loads as many rows as were shown, so a list opened with "Show more" keeps its length.
      const same = !newParams && !('sort' in o);
      const shown = same ? pager.items.length : 0;
      pager.reset(serverParams());
      if (!drawn) body.replaceChildren();
      if (!(await quietLoad(shown))) return;
      draw();
      await fillForSort();
    },
    /** Replace the rows (rows source only). */
    setRows(list) { rows = [...list]; draw(); },
    /** Draw every row again (after something they show changed, such as a pin). */
    redraw() { draw(); },
    /** Draw one row again in place (a live update of that item). */
    redrawRow(item) { const old = rowOf.get(item); if (old && old.isConnected) old.replaceWith(rowEl(item)); },
    get items() { return items(); },
  };
  return ctl;
}
