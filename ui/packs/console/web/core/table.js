// Dense text tables: an API list (sorted and paged by the server, a "more" line, CSV) or rows the page
// already has (sorted here). Numbers right-aligned with the unit muted after them; one row per item, each
// with data-row so j / k / Enter work; a totals row at the bottom when the page gives one.
//
// Phone (up to 860 px): the header row and the cells go, and each row shows one cell of two lines instead:
// the main value left, the secondary values muted under it, the number on the right. A column says where
// it goes there with ph: 'main' | 'sub' | 'num' (others are left out on the phone). A sort line above the
// table takes the place of the header buttons.
//
//   const t = table(ctx, {
//     title, side, cover, note, noteCover,       the section around it ('## drives', its item ids)
//     path, params, limit,                       an API list (GET path?params&sort=&dir=&limit=&after=)
//     rows,                                      or rows the page has
//     columns: [{ key, label, num, sort, firstDir, render(row), value(row), title, cover, ph, desk, cls, total(page, rows) }],
//     sort: { key, dir }, onSort({ key, dir }),
//     rowHref(row), empty(), onPage(page), csv: true | false, csvName,
//     rowTitle(row),                             the row's hover title (where its data came from: kept out of the cells)
//     keep(row),                                 an API list: only the rows it says yes to are shown (and counted)
//     filters,                                   controls put above the table
//     autoMore: true, moreLabel,                 an API list: the next page also loads when the end of the table
//                                                scrolls into view (the "more" button stays, for the keyboard)
//     box: true | 'name',                        the rows (and the loading or empty line) scroll inside a box that the
//                                                page's CSS gives its height (.tbl-box, from the start); with autoMore
//                                                the next page loads as the end of the box comes near. The box is a
//                                                focusable region (arrow keys scroll it), data-below while rows are
//                                                hidden under its bottom edge (for a fade)
//   });
//   t.el; await t.ready; t.reload(params); t.rows();

import { h, replace, cover as addCover } from '../shared/dom.js';
import { compareText, formatNumber } from '../shared/i18n.js';
import { csvFilename } from '../shared/csv.js';
import { empty as emptyLine, loadingLine, errorWords } from './states.js';
import { section } from './ui.js';

export function table(ctx, opts) {
  const { rowHref = null, limit = 50 } = opts;
  const kept = (rows) => (opts.keep && !opts.rows ? rows.filter(opts.keep) : rows);
  const allCols = opts.columns;
  const columns = allCols.filter((c) => c.desk !== false);   // desk: false = only in the phone row
  let params = { ...(opts.params || {}) };
  let sort = opts.sort ? { ...opts.sort } : null;
  let pager = null, items = [], gen = 0;
  let localShown = limit;   // local rows: shown a page at a time too

  const count = h('span', { class: 'tbl-count muted' });
  const csvSlot = h('span', { class: 'tbl-csv' });
  const thead = h('thead');
  const tbody = h('tbody');
  const tfoot = h('tfoot');
  const tbl = h('table', { class: ['tbl', opts.cls], 'aria-label': opts.label || opts.title || null }, thead, tbody, tfoot);
  const moreBtn = h('button', { class: 'btn', type: 'button', onclick: () => more() }, opts.moreLabel || 'more');
  const foot = h('div', { class: 'tbl-more', hidden: true }, moreBtn);
  const status = h('div', { class: 'tbl-status' });
  const sortSel = h('select', { class: 'in tbl-sortsel', 'aria-label': 'Sort by' });
  const sortLine = h('label', { class: 'tbl-sortline' }, h('span', { class: 'muted' }, 'sort: '), sortSel);
  const sortable = columns.filter((c) => c.sort);
  const note = opts.note ? h('p', { class: 'note tbl-note', cover: opts.noteCover || null }, opts.note) : null;
  const sentinel = opts.autoMore && !opts.rows ? h('div', { class: 'tbl-sentinel', 'aria-hidden': 'true' }) : null;
  const box = opts.box
    ? h('div', { class: 'tbl-scroll tbl-box', tabindex: '0', role: 'region', 'aria-label': typeof opts.box === 'string' ? opts.box : opts.label || opts.title || null }, tbl, status, sentinel)
    : null;
  const body = box
    ? [note, opts.filters || null, sortable.length ? sortLine : null, box, foot]
    : [note, opts.filters || null, sortable.length ? sortLine : null, h('div', { class: 'tbl-scroll' }, tbl), status, sentinel, foot];
  const el = opts.bare
    ? h('div', { class: ['tbl-wrap', opts.wrapCls] }, body)
    : section({ title: opts.title, cls: ['tbl-sec', opts.wrapCls], id: opts.id || null, side: [opts.side || null, count, csvSlot] }, body);
  if (opts.cover) addCover(el, ...[].concat(opts.cover));

  // one column takes the free width, so the others keep their whole-pixel content widths (a table that shares
  // the free width out puts every column edge on a fraction of a pixel)
  const growKey = (columns.find((c) => c.grow) || columns.find((c) => c.cls === 'wrap-cell'))?.key;
  const colCls = (c) => [c.num && 'num', c.cls, c.key === growKey && 'grow-col'];
  const phCols = { main: allCols.filter((c) => c.ph === 'main'), sub: allCols.filter((c) => c.ph === 'sub'), num: allCols.filter((c) => c.ph === 'num') };

  function sortParams() {
    if (!sort || opts.rows) return {};
    const col = columns.find((c) => c.key === sort.key);
    return col && typeof col.sort === 'string' ? { sort: col.sort, dir: sort.dir } : {};
  }

  function head() {
    replace(thead, h('tr', null, columns.map((c) => {
      const on = sort && sort.key === c.key;
      const label = c.sort
        ? h('button', { class: 'tbl-sort', type: 'button', onclick: () => setSort(c.key), title: c.title || null },
          c.label, on ? h('span', { class: 'tbl-dir', 'aria-hidden': 'true' }, sort.dir === 'asc' ? ' ^' : ' v') : null)
        : h('span', { title: c.title || null }, c.label);
      return h('th', { class: colCls(c), scope: 'col', cover: c.cover || null, 'aria-sort': on ? (sort.dir === 'asc' ? 'ascending' : 'descending') : null }, label);
    })));
    replace(sortSel, sortable.flatMap((c) => ['desc', 'asc'].map((d) => h('option', { value: `${c.key}:${d}`, selected: !!(sort && sort.key === c.key && sort.dir === d) },
      `${typeof c.label === 'string' ? c.label : c.key} ${d === 'asc' ? '^' : 'v'}`))));
  }
  sortSel.addEventListener('change', () => { const [key, dir] = sortSel.value.split(':'); setSortTo({ key, dir }); });

  function setSort(key) {
    const col = columns.find((c) => c.key === key);
    const dir = sort && sort.key === key ? (sort.dir === 'asc' ? 'desc' : 'asc') : (col.firstDir || (col.num ? 'desc' : 'asc'));
    setSortTo({ key, dir });
  }
  function setSortTo(s) {
    sort = s;
    head();
    opts.onSort?.({ ...sort });
    if (opts.rows) paintLocal(); else load();
  }
  function paintLocal() {
    const all = sortedLocal();
    paintRows(all.slice(0, localShown), true, all);
    foot.hidden = all.length <= localShown;
    paintCount(true);
  }

  const cell = (c, r) => { const v = c.render ? c.render(r) : r[c.key]; return v === false ? '' : v == null || v === '' ? h('span', { class: 'muted' }, '\u2014') : v; };   // false: nothing to show here (no dash)

  function rowEl(r) {
    const href = rowHref ? rowHref(r) : null;
    const tr = h('tr', { dataset: { row: '', href: href || null }, title: opts.rowTitle ? opts.rowTitle(r) || null : null },
      columns.map((c) => h('td', { class: colCls(c) }, cell(c, r))),
      h('td', { class: 'tbl-ph', colspan: String(columns.length) },
        h('div', { class: 'tbl-ph-main' }, phCols.main.map((c) => h('span', { class: 'tbl-ph-m' }, cell(c, r)))),
        phCols.sub.length ? h('div', { class: 'tbl-ph-sub' }, phCols.sub.map((c) => h('span', { class: 'tbl-ph-s' }, cell(c, r)))) : null,
        phCols.num.length ? h('div', { class: 'tbl-ph-num' }, phCols.num.map((c) => h('span', { class: 'tbl-ph-n' }, cell(c, r)))) : null));
    if (href) tr.addEventListener('click', (e) => { if (!(e.target instanceof Element) || e.target.closest('a, button, input, select, label')) return; ctx.navigate(href); });
    return tr;
  }

  function paintRows(rows, fresh, allRows = null) {
    if (fresh) replace(tbody);
    tbody.append(...kept(rows).map(rowEl));
    const all = allRows || (opts.rows ? rows : kept(items));
    replace(status, all.length ? null : (opts.empty ? opts.empty() : emptyLine('nothing here')));
    tbl.hidden = !all.length;
    sortLine.hidden = all.length < 2;
    const hasTotal = columns.some((c) => c.total);
    if (hasTotal && all.length) {
      replace(tfoot, h('tr', { class: 'tbl-total' }, columns.map((c, i) => h('td', { class: colCls(c) }, c.total ? c.total(pager?.last || null, all) : i === 0 ? 'total' : null)),
        h('td', { class: 'tbl-ph', colspan: String(columns.length) },
          h('div', { class: 'tbl-ph-main' }, 'total'),
          h('div', { class: 'tbl-ph-num' }, phCols.num.map((c) => h('span', { class: 'tbl-ph-n' }, c.total ? c.total(pager?.last || null, all) : null))))));
    } else replace(tfoot);
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

  function paintCount(done) {
    const n = opts.rows ? Math.min(localShown, opts.rows.length) : kept(items).length;
    const left = items.length - n;   // rows keep() left out: not in the total either
    const total = pager?.last?.total == null ? null : done ? n : pager.last.total - left;
    const all = opts.rows ? opts.rows.length : total;
    count.textContent = `${formatNumber(n)}${!done ? '+' : ''}${all != null && all !== n ? ` of ${formatNumber(all)}` : ''} ${(all ?? n) === 1 ? 'row' : 'rows'}`;
    if (opts.csv !== false && opts.path) {
      replace(csvSlot, h('a', { class: 'lnk', href: ctx.api.csvUrl(opts.path, { ...params, ...sortParams() }), download: csvFilename(ctx.car?.name || 'sentry-mate', opts.csvName || opts.title || 'list') }, 'csv'));
    }
  }

  async function more() {
    if (opts.rows) { localShown += limit; paintLocal(); return; }
    const g = gen;
    moreBtn.disabled = true;
    let got;
    try { got = await pager.more(); } catch (e) {
      if (g !== gen || !ctx.alive()) return;
      moreBtn.disabled = false;
      throw e;
    }
    if (g !== gen || !ctx.alive()) return;
    items = pager.items;
    opts.onPage?.(pager.last, items);
    paintRows(got, false);
    foot.hidden = pager.done;
    moreBtn.disabled = false;
    paintCount(pager.done);
  }

  async function load({ quiet = false } = {}) {
    const g = ++gen;
    if (opts.rows) { localShown = limit; paintLocal(); return; }
    pager = ctx.api.pager(opts.path, { ...params, ...sortParams() }, { limit });
    if (!quiet) { replace(tbody); replace(tfoot); tbl.hidden = true; foot.hidden = true; replace(status, loadingLine(`loading ${opts.title || 'rows'}`)); }
    if (!quiet && box) box.scrollTop = 0;   // a new list starts at its top
    let got;
    try { got = await pager.more(); } catch (e) {
      if (g !== gen || !ctx.alive()) return;
      const w = errorWords(e);
      replace(status, h('p', { class: 'errbox-line' }, h('span', { class: 'tag tag--bad' }, `[${w.tag}]`), ' ', w.text));
      opts.onError?.(e);
      throw e;
    }
    if (g !== gen || !ctx.alive()) return;
    items = pager.items;
    opts.onPage?.(pager.last, items);
    paintRows(got, true);
    foot.hidden = pager.done;
    paintCount(pager.done);
  }

  // the next page when the end of the table comes into view; looked at again after each page, so a tall
  // window fills up (a page that failed waits for the button)
  const io = sentinel && typeof IntersectionObserver === 'function'
    ? new IntersectionObserver((entries) => {
      if (!ctx.alive()) { io.disconnect(); return; }
      if (!entries.some((e) => e.isIntersecting) || !pager || pager.done || pager.loading || foot.hidden || moreBtn.disabled) return;
      more().then(recheck, () => {});
    }, { root: box, rootMargin: `0px 0px ${box ? 96 : 300}px 0px` })
    : null;
  function recheck() { if (io && ctx.alive() && pager && !pager.done) { io.unobserve(sentinel); io.observe(sentinel); } }
  if (io) { io.observe(sentinel); ctx.signal?.addEventListener('abort', () => io.disconnect(), { once: true }); }
  // a box: data-below while rows are hidden under its bottom edge
  if (box) {
    const edge = () => box.toggleAttribute('data-below', box.scrollHeight - box.scrollTop - box.clientHeight > 1);
    box.addEventListener('scroll', edge, { passive: true });
    if (typeof ResizeObserver === 'function') {
      const ro = new ResizeObserver(edge);
      ro.observe(box); ro.observe(tbl);
      ctx.signal?.addEventListener('abort', () => ro.disconnect(), { once: true });
    }
  }

  head();
  const ready = load();
  ready.then(recheck, () => {});
  ready.catch(() => {});
  return {
    el, ready,
    reload(p, o) { if (p) params = { ...p }; const r = load(o); r.then(recheck, () => {}); return r; },
    setRows(rows) { opts.rows = rows; load(); },
    /** Draw the rows again from what is loaded (a cell's data changed). */
    repaint() { if (opts.rows) paintLocal(); else paintRows(items, true); },
    rows: () => (opts.rows ? sortedLocal() : kept(items)),
    last: () => pager?.last || null,
  };
}

/** A text bar cell: '########............ 62 %'-style, n cells of '#', the rest '.'. */
export function cellBar(frac, cells = 10, tone = 'accent') {
  const v = frac == null || !Number.isFinite(frac) ? null : Math.max(0, Math.min(1, frac));
  const on = v == null ? 0 : Math.round(v * cells);
  return h('span', { class: ['tbar', `tbar--${tone}`], 'aria-hidden': 'true' }, h('span', { class: 'tbar-on' }, '#'.repeat(on)), h('span', { class: 'tbar-off' }, '.'.repeat(cells - on)));
}
