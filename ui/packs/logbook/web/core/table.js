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

import { h, holdScroll } from '../shared/dom.js';
import { downloadCsv, csvFilename } from '../shared/csv.js';
import { compareText, formatNumber } from '../shared/i18n.js';
import { icon, empty as emptyView, errorView, loadingBlock } from './ui.js';
import { scrollBox } from '../shared/scrollbox.js';
import { windowList, dayJumpMs } from '../shared/vlist.js';

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
 *   rowTitle(row) -> text,            // the row's hover title: where its data came from, kept out of the cells
 *   keep(row) -> bool,                // rows loaded but not shown (and not counted), such as sold cars
 *   onRows(items, page),              // after each page arrives (summaries in the answer: page.summary)
 *   onSort({ key, dir }),             // a column head was clicked, before the rows load again in that order
 *   caption: node,                    // extra text after the count
 *   sortAll: 2000,                    // a paged list: a client sort (sort: true) first loads every page, up to
 *                                     // this many rows, so the order covers the whole list, not one page
 *   cover, cls, label,                // ids for the table, extra class, the table's accessible name
 *   append: true,                     // a paged list in the server's order: "Show more" appends the new rows
 *                                     // (the rows on screen stay as they are, the page does not move)
 *   autoMore: true,                   // with append: the next page also loads when the end of the table
 *                                     // scrolls into view (the button stays, for the keyboard and as fallback)
 *   moreLabel: 'Show more',           // the button's words
 *   filters: node,                    // controls put between the caption line and the table
 *   box: true | 'name',               // a long log-like list: the rows (and the loading, empty or error view)
 *                                     // scroll in a box about five rows high (shared/scrollbox.js, .scroll-box in
 *                                     // app.css; a page sets --box-rows / --box-row for other rows). A paged list
 *                                     // has that height from the start and, with autoMore, loads the next page as
 *                                     // the end of the box comes near ("Show more" stays under it); rows in hand
 *                                     // make a box at most that high (.scroll-box--fit). The column heads stay at
 *                                     // the top while the rows scroll.
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
  const more = h('button', { type: 'button', class: 'button button-quiet', hidden: true, onclick: () => loadMore() }, opts.moreLabel || 'Show more');
  const foot = h('div', { class: 'ledger-foot' }, more);
  const status = h('div', { class: 'ledger-status' });
  const sentinel = opts.autoMore && !opts.box ? h('div', { class: 'ledger-sentinel', 'aria-hidden': 'true' }) : null;
  const box = opts.box
    ? scrollBox({
      label: typeof opts.box === 'string' ? opts.box : label, cls: ['ledger-scroll', 'ledger-box', pager ? null : 'scroll-box--fit'], signal: ctx.signal,
      onEnd: opts.autoMore && pager ? () => (pager.done || pager.loading || !drawn || more.hidden || more.disabled ? false : loadMore()) : null,
    }, table, status)
    : null;
  const el = h('div', { class: ['ledger', cls], cover },
    h('div', { class: 'ledger-top' }, count, opts.caption ? h('p', { class: 'ledger-caption' }, opts.caption) : null, csvBtn),
    opts.filters || null,
    ...(box ? [box.el, foot] : [h('div', { class: 'ledger-scroll' }, table), status, sentinel, foot]));
  // the next page when the end of the table comes into view (looked at again after each page, so a tall
  // window fills up; a failed page waits for the button or the retry)
  let seen = false;
  const io = sentinel && typeof IntersectionObserver === 'function'
    ? new IntersectionObserver((entries) => {
      seen = entries.some((e) => e.isIntersecting);
      if (seen && pager && !pager.done && !pager.loading && drawn && !more.hidden && !more.disabled) loadMore().then((ok) => { if (ok) recheck(); });
    }, { rootMargin: '0px 0px 300px 0px' })
    : null;
  function recheck() {
    if (box) { if (!ctx.signal.aborted) box.recheck(); return; }
    if (io && !ctx.signal.aborted && pager && !pager.done) { io.unobserve(sentinel); io.observe(sentinel); }
  }
  if (io) { io.observe(sentinel); ctx.signal.addEventListener('abort', () => io.disconnect(), { once: true }); }

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
    const attrs = { ...(opts.rowAttrs ? opts.rowAttrs(r) || {} : {}) };
    if (opts.rowTitle && attrs.title == null) attrs.title = opts.rowTitle(r) || null;
    const tr = h('tr', attrs, columns.map((c) => {
      const v = c.cell ? c.cell(r) : r[c.key];
      return h(c.th ? 'th' : 'td', { class: [c.cls, { num: c.num }], scope: c.th ? 'row' : null }, v == null ? '—' : v);
    }));
    rowOf.set(r, tr);
    return tr;
  }
  function items() { return pager ? pager.items : rows; }
  const kept = (list) => (opts.keep ? list.filter(opts.keep) : list);
  function draw(added = null) {
    const list = kept(pager && !clientSorted() ? pager.items : sortedClient(items() || []));
    if (added) body.append(...kept(added).map(rowEl));
    else { holdScroll(body); body.replaceChildren(...list.map(rowEl)); }
    const loaded = (items() || []).length;
    const n = list.length;
    const left = loaded - n; // loaded but not shown: the total counts them too
    const [one, many] = itemName;
    const word = (x) => (x === 1 ? one : many);
    if (pager) {
      count.textContent = total != null ? `${formatNumber(total - left)} ${word(total - left)}` : pager.done ? `${formatNumber(n)} ${word(n)}` : `${formatNumber(n)} ${many} shown, more to load`;
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
      const shown = pager.items.length;
      const got = await pager.more();
      if (pager.last && typeof pager.last.total === 'number') total = pager.last.total;
      opts.onRows?.(got, pager.last);
      // append: only the new rows are drawn, under the ones already there
      draw(opts.append && drawn && shown && !clientSorted() && body.children.length ? got : null);
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
    async load() { if (pager) { await loadMore(true); fillForSort(); recheck(); } else { draw(); opts.onRows?.(rows, null); box?.recheck(); } },
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
      if (box && !same) box.top();   // new filters or order: the list starts at its top
      recheck();
      await fillForSort();
    },
    /** Replace the rows (rows source only). */
    setRows(list) { rows = [...list]; draw(); box?.recheck(); },
    /** Draw every row again (after something they show changed, such as a pin). */
    redraw() { draw(); },
    /** Draw one row again in place (a live update of that item). */
    // (Safari can lay the page out with the old row out and the new one not in yet, and pull a window scrolled to the
    // end of the page up by a row's height: holdScroll puts it back, shared/dom.js)
    redrawRow(item) { const old = rowOf.get(item); if (old && old.isConnected) { holdScroll(old.parentElement); old.replaceWith(rowEl(item)); } },
    get items() { return items(); },
  };
  return ctl;
}

/**
 * windowLedger(ctx, {
 *   columns: [{ key, label, cell(row), sort: 'server_key', firstDir, num, th, cls, cover, hint }],
 *   path, params, limit,              // a list the API pages by cursor and by offset ({ items, next, offset, total })
 *   sort: { key, dir }, firstSort,    // the starting order; Jump to date works in firstSort's column only
 *   itemName, label, cls, cover, caption,
 *   rowAttrs(row), empty(page),
 *   search: { label, placeholder, hint } | null,   // the API's q, applied when typing pauses
 *   jump: true,                       // "Jump to date" (the API's at_ms)
 *   tools: node,                      // more controls in the tool line (an order choice)
 *   onPage(page), onError(err),
 *   load({ ...params, sort: { key, dir }, offset, limit }) -> { items, next, offset, total }
 *                                     // in place of path: a list the page holds already (a chunk of it, at once;
 *                                     // no Download CSV of the server's, the page gives its own in tools)
 *   rows: N,                          // a box N rows high instead of about ten
 * }) -> { el, ready, setParams(patch, { sort }), refresh(), total(), params() }
 *
 * The rows sit in a box about ten rows high that scrolls under its head: only the rows in view and a few more are
 * in the page (shared/vlist.js), loaded from the API a chunk at a time as the box comes to them. Every row is one
 * height (.vl-ledger in app.css: two lines a cell at most).
 */
export function windowLedger(ctx, opts) {
  const { columns, itemName = ['row', 'rows'], cover = null, cls = null, label = null } = opts;
  let params = { ...(opts.params || {}) };
  let sort = opts.sort ? { ...opts.sort } : null;
  const base = opts.firstSort || opts.sort || null;
  let total = null;

  const count = h('p', { class: 'ledger-count vl-count', 'aria-live': 'polite' });
  const csvBtn = h('a', { class: 'button button-quiet button-small', href: '#', download: '', dataset: { csv: '' }, title: 'Every row the filters leave, as the server writes it (metric, times as epoch ms)' }, icon('download'), 'Download CSV');
  const search = opts.search ? h('input', { class: 'field-input vl-search', type: 'search', 'aria-label': opts.search.label || 'Search', placeholder: opts.search.placeholder || 'Search', title: opts.search.hint || null, value: params.q || '' }) : null;
  const jumpIn = opts.jump ? h('input', { class: 'field-input vl-jump-date', type: 'date', 'aria-label': 'Jump to date', title: 'Scroll the log to the first drive of this day' }) : null;
  const jumpBtn = opts.jump ? h('button', { type: 'button', class: 'button button-quiet button-small', onclick: () => jump() }, 'Jump to date') : null;
  const head = h('thead', null, h('tr', null, columns.map((c) => headCell(c))));
  const body = h('tbody', { dataset: { vlBody: '' } });
  const table = h('table', { class: 'ledger-table vl-ledger', 'aria-label': label }, head, body);
  const box = h('div', { class: ['ledger-scroll ledger-box vl-box', opts.rows && 'vl-box--rows'], vars: opts.rows ? { '--vl-rows': String(opts.rows) } : null, tabindex: '0', role: 'region', 'aria-label': opts.boxLabel || label }, table);
  const status = h('div', { class: 'ledger-status' });
  const el = h('div', { class: ['ledger', cls], cover },
    h('div', { class: 'ledger-top vl-tools' }, opts.tools || null, search, opts.jump ? h('span', { class: 'vl-jump' }, jumpIn, jumpBtn) : null, count, opts.path ? csvBtn : null),
    opts.caption ? h('p', { class: 'ledger-caption vl-caption' }, opts.caption) : null,
    box, status);

  function serverParams() {
    const p = { ...params };
    const col = sort && columns.find((c) => c.key === sort.key);
    if (col && typeof col.sort === 'string') { p.sort = col.sort; p.dir = sort.dir; }
    return p;
  }
  function headCell(c) {
    const on = sort && sort.key === c.key;
    const th = h('th', { scope: 'col', class: [c.cls, { num: c.num, 'is-sortable': !!c.sort }], title: c.hint || null,
      'aria-sort': on ? (sort.dir === 'asc' ? 'ascending' : 'descending') : null, cover: c.cover || null });
    if (!c.sort) { th.append(c.label); return th; }
    th.append(h('button', { type: 'button', class: 'sort-button', onclick: () => sortBy(c) }, c.label,
      h('span', { class: 'sort-mark', 'aria-hidden': 'true' }, on ? (sort.dir === 'asc' ? '▲' : '▼') : '')));
    return th;
  }
  function drawHead() { head.replaceChildren(h('tr', null, columns.map((x) => headCell(x)))); }
  function sortBy(c) {
    const dir = sort && sort.key === c.key ? (sort.dir === 'asc' ? 'desc' : 'asc') : (c.firstDir || (c.num ? 'desc' : 'asc'));
    sort = { key: c.key, dir };
    drawHead();
    opts.onSort?.({ ...sort });
    restart().catch((e) => fail(e));
  }
  function rowEl(r, i) {
    const attrs = opts.rowAttrs ? opts.rowAttrs(r) || {} : {};
    return h('tr', { ...attrs, 'aria-rowindex': String(i + 2), dataset: { vlRow: String(i) } }, columns.map((c) => {
      const v = c.cell ? c.cell(r) : r[c.key];
      return h(c.th ? 'th' : 'td', { class: [c.cls, { num: c.num }], scope: c.th ? 'row' : null }, v == null ? '—' : v);
    }));
  }
  const skelRow = (i) => h('tr', { class: 'vl-skel', 'aria-rowindex': String(i + 2) }, columns.map(() => h('td', null, h('span', { class: 'vl-skel-bar' }))));
  const gapRow = () => h('tr', null, h('td', { colspan: String(columns.length) }));

  const list = windowList({
    box, body, head, limit: opts.limit || 50, overscan: 6, rowHeight: 70,
    row: rowEl, placeholder: skelRow, spacer: gapRow,
    load: (p) => (opts.load ? Promise.resolve(opts.load({ ...serverParams(), sort: sort ? { ...sort } : null, ...paging(p) })) : ctx.api.get(opts.path, { ...serverParams(), ...paging(p) }, { signal: p.signal })),
    onPage: (pg) => { if (pg.total !== total) { total = pg.total; paint(); } opts.onPage?.(pg); },
    onError: (e) => opts.onError?.(e),
  });
  ctx.signal.addEventListener('abort', () => list.destroy(), { once: true });

  function paint() {
    const n = total ?? 0;
    const [one, many] = itemName;
    count.textContent = `${formatNumber(n)} ${n === 1 ? one : many}`;
    table.setAttribute('aria-rowcount', String(n + 1));
    box.hidden = !n;
    csvBtn.hidden = !n;
    if (opts.path) csvBtn.setAttribute('href', ctx.api.csvUrl(opts.path, serverParams()));
    status.replaceChildren(n ? '' : (opts.empty ? opts.empty({ total: n }) : emptyView({ title: `No ${many}.` })));
    if (jumpIn) {
      const can = !!n && sort && base && sort.key === base.key;
      jumpIn.disabled = !can; jumpBtn.disabled = !can;
      jumpBtn.title = can ? 'Scroll the log to the first drive of this day' : 'Jump to date works in the order of the drives (by number or start).';
    }
  }
  function fail(err) {
    if (ctx.signal.aborted) return;
    status.replaceChildren(errorView(err, { retry: () => restart().catch(() => {}), eyebrow: 'Could not load the list' }));
    opts.onError?.(err);
  }
  async function restart() {
    total = null;
    const pg = await list.start();
    if (pg) { total = pg.total; paint(); opts.onPage?.(pg); }
    return pg;
  }
  async function jump() {
    if (!jumpIn.value) { jumpIn.focus(); return; }
    try {
      const pg = await list.jump(dayJumpMs(jumpIn.value, ctx.tz, sort?.dir === 'desc'));
      if (pg) { total = pg.total; paint(); box.focus({ preventScroll: true }); }
    } catch (e) { fail(e); }
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
        restart().catch((e) => fail(e));
      }, 300);
    });
    ctx.signal.addEventListener('abort', () => clearTimeout(t), { once: true });
  }

  const ready = restart();
  return {
    el,
    ready,
    /** New filters (null removes one), and with { sort } a new order: load again from the top. */
    setParams(patch, o = {}) {
      for (const [k, v] of Object.entries(patch || {})) { if (v == null || v === '') delete params[k]; else params[k] = v; }
      if ('sort' in o) { sort = o.sort ? { ...o.sort } : null; drawHead(); }
      return restart();
    },
    /** The rows in view loaded again (a live change); nothing moves until they are in. */
    refresh() { return list.refresh().then((pg) => { if (pg) { total = pg.total; paint(); } return pg; }); },
    total: () => total ?? 0,
    params: () => serverParams(),
  };
}

// what vlist asks with: a cursor, a date or an offset
function paging(p) {
  const out = { limit: p.limit };
  if (p.after) out.after = p.after;
  else if (p.at_ms != null) out.at_ms = p.at_ms;
  else out.offset = p.offset || 0;
  return out;
}
