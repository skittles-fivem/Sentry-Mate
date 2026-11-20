// The track list: Horizon's table. A list from the API (cursor paged, sorted by the server) or rows the page
// already has (sorted here), in a HUD panel with a head (title, count, CSV), a header row that sorts, and a
// "Load more" button. Rows can open a page. On a phone each row becomes a small card: every cell keeps its
// column name (data-label), so nothing scrolls sideways.
//
//   const t = tracks(ctx, {
//     title, icon, tone, cover, label, note,
//     path, params, limit,        an API list (GET path?params&sort=&dir=&limit=&after=)
//     rows,                       or rows the page already has
//     columns: [{ key, label, num, sort, firstDir, render(row), value(row), csv, cover, cls, title, lead }],
//     sort: { key, dir }, onSort, rowHref(row), empty(page), csv: true | false | { columns, name, all },
//     side, onPage(page), onError(err), csvName,
//   });
//   t.el, await t.ready, t.reload(params, { quiet }), t.setRows(rows), t.rows()
//
// A column's sort is the API's sort name (string) for API lists, or true for local rows. lead: true marks
// the column a phone card shows as its title.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { csvFilename, downloadCsv } from '../shared/csv.js';
import { formatNumber, compareText } from '../shared/i18n.js';
import { icon, hud } from './ui.js';
import { errorWords } from './states.js';
import { csvBtn } from './kit.js';

export function tracks(ctx, o) {
  const { columns, rowHref = null, limit = 50, csv = true } = o;
  let params = { ...(o.params || {}) };
  let sort = o.sort ? { ...o.sort } : null;
  let pager = null, loaded = [], done = false, total = null, gen = 0;

  const count = h('span', { class: 'trk-count', 'aria-live': 'polite' });
  const csvSlot = h('span', { class: 'trk-csv' });
  const thead = h('thead');
  const tbody = h('tbody');
  const table = h('table', { class: ['trk', o.dense && 'trk--dense'], 'aria-label': o.label || (typeof o.title === 'string' ? o.title : null) }, thead, tbody);
  const moreBtn = h('button', { class: 'btn btn--sm', type: 'button', onclick: () => more(gen) }, icon('chev-d', 'i--sm'), h('span', null, 'Load more'));
  const footText = h('span', { class: 'trk-foot-text' });
  const foot = h('div', { class: 'trk-foot', hidden: true }, footText, moreBtn);
  const el = hud({ title: o.title, icon: o.icon, tone: o.tone, cover: o.cover, label: o.label, pad: false, cls: ['trk-panel', o.cls],
    side: [o.side || null, count, csvSlot] },
  o.note ? h('p', { class: 'trk-note', cover: o.noteCover }, icon('info', 'i--sm'), h('span', null, o.note)) : null,
  h('div', { class: 'trk-wrap' }, table), foot);

  const sortParams = () => {
    if (!sort) return {};
    const c = columns.find((x) => x.key === sort.key);
    return c && typeof c.sort === 'string' ? { sort: c.sort, dir: sort.dir } : {};
  };

  function head() {
    replace(thead, h('tr', null, columns.map((c) => {
      const on = sort && sort.key === c.key;
      return h('th', { scope: 'col', class: [c.num && 'is-num', c.cls], title: c.title || null, cover: c.cover,
        'aria-sort': on ? (sort.dir === 'asc' ? 'ascending' : 'descending') : null },
      c.sort ? h('button', { class: ['trk-sort', on && 'is-on'], type: 'button', onclick: () => setSort(c.key) },
        h('span', null, c.label), icon('chev-d', on && sort.dir === 'asc' ? 'i--sm is-up' : 'i--sm')) : c.label);
    })));
  }

  function setSort(key) {
    const c = columns.find((x) => x.key === key);
    const dir = sort && sort.key === key ? (sort.dir === 'asc' ? 'desc' : 'asc') : (c.firstDir || (c.num ? 'desc' : 'asc'));
    sort = { key, dir };
    head();
    o.onSort?.({ ...sort });
    if (o.rows) renderLocal(); else load();
  }

  const labelText = (c) => (typeof c.label === 'string' ? c.label : c.label?.textContent || '');
  function rowEl(r, i) {
    const href = rowHref ? rowHref(r) : null;
    const tr = h('tr', { class: [href && 'is-link', o.rowCls?.(r)] }, columns.map((c) => h('td', {
      class: [c.num && 'is-num', c.cls, c.lead && 'is-lead'], dataset: { label: labelText(c) || null },
    }, c.render ? c.render(r, i) : (r[c.key] ?? '—'))));
    if (href) {
      tr.addEventListener('click', (e) => {
        if (e.defaultPrevented || e.target.closest('a, button, input, select, label, summary')) return;
        if (String(window.getSelection?.() || '').length) return;
        state.navigate(href);
      });
    }
    return tr;
  }

  const skeleton = () => Array.from({ length: 4 }, () => h('tr', { class: 'trk-skel', 'aria-hidden': 'true' }, columns.map(() => h('td', null, h('span', { class: 'skel skel--line' })))));
  const emptyRow = (node) => h('tr', { class: 'trk-empty' }, h('td', { colspan: String(columns.length) }, node));

  function paintCount() {
    const n = loaded.length;
    count.textContent = total != null ? `${formatNumber(total)} in all` : done ? (n === 1 ? '1 row' : `${formatNumber(n)} rows`) : `${formatNumber(n)} shown`;
    foot.hidden = done || !n;
    footText.textContent = total != null ? `${formatNumber(n)} of ${formatNumber(total)} shown` : `${formatNumber(n)} shown, more to load`;
  }

  function paintCsv() {
    if (!csv) { replace(csvSlot); return; }
    const name = [ctx.car?.name || 'sentry-mate', o.csvName || (typeof o.title === 'string' ? o.title : 'list')];
    if (csv === true && o.path) {
      replace(csvSlot, csvBtn({ href: ctx.api.csvUrl(o.path, { ...params, ...sortParams() }), filename: csvFilename(...name) }));
      return;
    }
    const cols = (csv.columns || columns.filter((c) => c.csv !== false).map((c) => (c.csv && typeof c.csv === 'object' ? c.csv : { label: labelText(c), value: c.csv || c.value || ((r) => r[c.key]) })));
    const b = h('button', { class: 'btn btn--sm btn--ghost csv', type: 'button', title: 'Download this as a CSV file', onclick: async () => {
      if (!(csv.all && o.path) || done) { downloadCsv(csvFilename(...name), o.rows ? sorted() : loaded, cols); return; }
      b.disabled = true;
      try { const all = await ctx.api.list(o.path, { ...params, ...sortParams() }, { limit: 500 }).all(); if (ctx.alive()) downloadCsv(csvFilename(...name), all, cols); } catch (e) { o.onError?.(e); } finally { b.disabled = false; }
    } }, icon('download', 'i--sm'), h('span', null, 'CSV'));
    replace(csvSlot, b);
  }

  function sorted() {
    const rows = [...(o.rows || [])];
    if (!sort) return rows;
    const c = columns.find((x) => x.key === sort.key);
    const v = c?.value || ((r) => r[sort.key]);
    const k = sort.dir === 'asc' ? 1 : -1;
    return rows.sort((a, b) => {
      const x = v(a), y = v(b);
      if (x == null && y == null) return 0;
      if (x == null) return 1;
      if (y == null) return -1;
      return (typeof x === 'string' ? compareText(x, y) : x - y) * k;
    });
  }

  function renderLocal() {
    loaded = sorted();
    done = true; total = null;
    replace(tbody, loaded.length ? loaded.map(rowEl) : emptyRow(o.empty ? o.empty() : h('p', { class: 'trk-none' }, 'Nothing to show.')));
    paintCount();
    paintCsv();
  }

  // quiet: a live refresh keeps the rows on screen until the new first page is in
  async function load(quiet = false) {
    const my = ++gen;
    const keep = quiet && loaded.length > 0;
    if (!keep) { loaded = []; done = false; total = null; replace(tbody, skeleton()); foot.hidden = true; count.textContent = ''; }
    pager = ctx.api.pager(o.path, { ...params, ...sortParams() }, { limit });
    paintCsv();
    await more(my, keep);
  }

  async function more(my, fresh = false) {
    if (!pager || (pager.done && !fresh)) return;
    moreBtn.disabled = true;
    try {
      const items = (await pager.more()) || [];
      if (my !== gen || !ctx.alive()) return;
      const pg = pager.last || { items, next: null };
      if (fresh || !loaded.length) { loaded = []; tbody.replaceChildren(); }
      const at = loaded.length;
      loaded.push(...items);
      tbody.append(...items.map((r, i) => rowEl(r, at + i)));
      done = !pg.next;
      if (pg.total != null) total = pg.total;
      if (!loaded.length) replace(tbody, emptyRow(o.empty ? o.empty(pg) : h('p', { class: 'trk-none' }, 'Nothing in this period.')));
      paintCount();
      o.onPage?.(pg);
    } catch (e) {
      if (my !== gen || !ctx.alive() || e.name === 'AbortError') return;
      if (fresh && loaded.length) return; // a failed live refresh keeps what is shown
      const w = errorWords(e);
      replace(tbody, emptyRow(h('div', { class: 'trk-none', role: 'alert' }, h('b', null, w.title), h('span', null, w.text),
        h('button', { class: 'btn btn--sm', type: 'button', onclick: () => load() }, icon('arrow-r', 'i--sm'), h('span', null, 'Try again')))));
      o.onError?.(e);
    } finally {
      if (my === gen) moreBtn.disabled = false;
    }
  }

  head();
  const ready = o.rows ? (renderLocal(), Promise.resolve()) : load();
  return {
    el, ready,
    reload(p, opt = {}) { if (p) params = { ...p }; if (o.rows) { renderLocal(); return Promise.resolve(); } return load(!!opt.quiet); },
    setRows(rows) { o.rows = rows; renderLocal(); },
    rows: () => loaded,
    done: () => done,
  };
}
