// The connections overview (Settings: Integrations, the "overview" tab): every connection the server keeps on one
// screen. Totals by status, then one dense row per connection (a status light and word, why, the last good contact
// and the next, pace and today's count against the cap, 24 h and 7 d success and errors, uptime, the last error
// and a 24 h bar line, error hours red), then the problems of the last 7 days.
// Live: `connections` events changed in place (live events, polling only while the live stream is down: every 10 s;
// the old values stay until the new answer is in; every cell has its width in characters up front); relative times
// tick every second. Admins only (the API is admin only).
// API: GET integrations/overview, then `connections` events.

import { h, svg, replace, cover } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { formatDuration } from '../shared/time.js';
import { format } from '../shared/units.js';
import { section, note } from '../core/ui.js';
import { scrollBox } from '../shared/scrollbox.js';
import { tag, dateTime, time, DASH } from '../core/fmt.js';
import { loadingLine, errorWords } from '../core/states.js';
import { follow } from '../shared/live.js';

export const STATUSES = ['live', 'idle', 'paused', 'off', 'problem'];
const POLL_MS = 10_000;
const NB = ' ';

// status_reason codes in words; a code not here is shown with its underscores as spaces
const REASON = {
  car_asleep: 'asleep, as expected', asleep: 'asleep, as expected', parked: 'parked, nothing new', not_added: 'not set up',
  turned_off: 'turned off', off: 'turned off', offline_mode: 'offline mode', paused: 'paused by an admin',
  token_refused: 'token refused', sign_in_needed: 'sign in again', unreachable: 'cannot reach it', quiet: 'nothing came',
  connection_refused: 'connection refused', auth_failed: 'sign-in refused', pin_mismatch: 'certificate pin differs',
  tls_failed: 'tls failed', timeout: 'no answer', log_check_failed: 'log failed its check', throttled: 'asked to wait',
  catchup_failed: 'catch-up failed', not_teslamate: 'not a teslamate database', query_failed: 'query failed',
};
const PLAIN = { live: 'working', idle: 'quiet, as expected', paused: 'paused by an admin', off: 'not set up', problem: 'failing' };
export const reasonText = (c) => (c.status_reason ? REASON[c.status_reason] || c.status_reason.replace(/_/g, ' ') : PLAIN[c.status] || '');
const UNIT_ONE = { uploads: 'upload', requests: 'request', messages: 'message' };

/** '12 s ago', 'in 30 s', '2 h 05 min ago': seconds under a minute, so it visibly ticks. */
export function rel(ms, now) {
  if (ms == null) return DASH;
  const d = now - ms;
  const s = Math.round(Math.abs(d) / 1000);
  const t = s < 60 ? `${s}${NB}s` : formatDuration(Math.abs(d));
  return d >= 0 ? `${t}${NB}ago` : `in${NB}${t}`;
}
const pct = (v) => (v == null ? DASH : format(v, 'percent', { digits: 1 }));
const count = (n, unit) => `${formatNumber(n ?? 0)}${NB}${n === 1 ? UNIT_ONE[unit] || unit : unit || ''}`;
const plural = (n, one, many) => `${formatNumber(n ?? 0)}${NB}${n === 1 ? one : many}`;

/** The status light and word: a small square in the status colour and the word after it. */
export const light = (status) => h('span', { class: ['ov-light', `ov-light--${status}`] }, h('span', { class: 'ov-dot', 'aria-hidden': 'true' }), status);

/** Totals line: '6 live  1 idle  0 paused  3 off  1 problem', each with its light; counts right-aligned in 3 places. */
export function totalsLine(totals) {
  const el = h('p', { class: 'ov-tot', 'aria-label': 'Connections by status' });
  const parts = Object.fromEntries(STATUSES.map((s) => [s, h('span', { class: 'ov-tot-n' })]));
  el.append(...STATUSES.map((s) => h('span', { class: ['ov-tot-i', `ov-light--${s}`] }, h('span', { class: 'ov-dot', 'aria-hidden': 'true' }), parts[s], h('span', { class: 'ov-tot-w' }, s))));
  const set = (t) => { for (const s of STATUSES) { const v = String(t?.[s] ?? 0); if (parts[s].textContent !== v) parts[s].textContent = v; } };
  set(totals);
  return { el, set };
}

// set text only when it changed (an equal text is not touched, so nothing flickers)
function put(el, text) { const v = text == null ? '' : String(text); if (el.textContent !== v) el.textContent = v; }

/** The 24 h bars: one per hour, height by count, error hours red, hours with nothing a thin base line. */
function bars(hourly, unit, tz) {
  const H = 100, W = 10;
  const hs = (hourly || []).slice(-24);
  const pad = 24 - hs.length;
  const top = Math.max(1, ...hs.map((x) => x.count || 0));
  const label = hs.length ? `last 24 hours: ${count(hs.reduce((a, x) => a + (x.count || 0), 0), unit)}, ${plural(hs.reduce((a, x) => a + (x.errors || 0), 0), 'error', 'errors')}` : 'no hours yet';
  return svg('svg', { class: 'ov-spk', viewBox: `0 0 ${24 * W} ${H}`, preserveAspectRatio: 'none', role: 'img', 'aria-label': label, focusable: 'false' },
    hs.map((x, i) => {
      const n = x.count || 0, err = (x.errors || 0) > 0;
      const bh = n ? Math.max(8, Math.round((n / top) * (H - 4))) : 4;
      return svg('rect', { class: ['ov-bar', err ? 'ov-bar--err' : n ? 'ov-bar--on' : 'ov-bar--0'], x: (pad + i) * W + 1, y: H - bh, width: W - 2, height: bh },
        svg('title', null, `${time(x.hour_ms, tz)}  ${count(n, unit)}${err ? `, ${plural(x.errors, 'error', 'errors')}` : ''}`));
    }));
}
const barsKey = (c) => JSON.stringify((c.hourly || []).slice(-24).map((x) => [x.count, x.errors]));

/**
 * overviewSection(ctx, { href }) -> { el, ready, totals() }. href(link): the address a row opens for its link (the
 * link itself by default; an #anchor of the page it is on opens there in place). The first answer is read before ready resolves; then
 * every 10 s.
 */
export function overviewSection(ctx, { href = (l) => l } = {}) {
  const go = (l) => ctx.navigate(href(l));
  const tz = ctx.tz;
  const now = () => ctx.api.now();
  const readEl = h('span', { class: 'ov-read muted', 'aria-live': 'off' });
  const errEl = h('span', { class: 'ov-err' });
  const tot = totalsLine(null);
  const tbody = h('tbody');
  const ptbody = h('tbody');
  const pnote = h('p', { class: 'note ov-pnone', hidden: true }, 'no problems in the last 7 days.');
  const head = (cols) => h('thead', null, h('tr', null, cols.map(([label, cls]) => h('th', { class: cls, scope: 'col' }, label))));
  const tbl = h('table', { class: 'ov', 'aria-label': 'Connections', cover: ['conn.overview.rows'] },
    head([['state', 'ov-c-st'], ['connection', 'ov-c-name'], ['last good, next', 'ov-c-last'], ['pace, today', 'ov-c-pace'],
      ['24 h', 'ov-c-day num'], ['7 d', 'ov-c-week num'], ['uptime', 'ov-c-up num'], ['last 24 h', 'ov-c-spk']]),
    tbody);
  const ptbl = h('table', { class: 'ov ov--pb', 'aria-label': 'Problems in the last 7 days', cover: 'conn.overview.problems' },
    head([['began', 'ov-c-when'], ['connection', 'ov-c-conn'], ['what happened', 'ov-c-what'], ['how long', 'ov-c-dur num'], ['', 'ov-c-res']]), ptbody);
  const pbox = scrollBox({ el: h('div', { class: 'ov-scroll scroll-box--fit ov-pbox' }), label: 'Problems in the last 7 days', onCleanup: ctx.onCleanup }, ptbl);
  const body = h('div', { class: 'ov-body', hidden: true }, tot.el, h('div', { class: 'ov-scroll' }, tbl),
    h('h3', { class: 'sub-title ov-ptitle' }, 'problems in the last 7 days'), pnote,
    // a long week of problems scrolls in a box five rows high (at most: a short list leaves no empty box)
    pbox.el);
  const first = loadingLine('reading every connection');
  const el = h('div', { class: 'ov-wrap' },
    section({ title: 'connections', cls: 'ov-sec', side: [errEl, readEl] }, first, body,
      note('read again every 10 s. a row opens the page with its details; j / k and Enter work too.')));

  let data = null, readAt = null;
  const rows = new Map();   // connection id -> { tr, cells, key }
  const prows = new Map();  // problem id -> { tr, dur, res }

  function rowFor(c) {
    const ln = (cls) => h('div', { class: ['ov-ln', cls] });
    const c1 = { word: h('div', { class: 'ov-ln' }), why: ln('muted') };
    const c2 = { label: ln('ov-label'), name: h('span'), miss: h('span', { class: 'ov-miss tone--warn' }), err: ln('ov-lasterr muted') };
    c2.label.append(c2.name, c2.miss);
    const c3 = { last: ln(), next: ln('muted') };
    const c4 = { pace: ln(), today: ln('muted') };
    const c5 = { pct: ln(), err: ln('muted') };
    const c6 = { pct: ln(), err: ln('muted') };
    const c7 = { day: ln(), week: ln('muted') };
    const spk = h('div', { class: 'ov-spk-box', cover: 'conn.overview.sparkline' });
    const tds = [
      h('td', { class: 'ov-c-st', cover: 'conn.overview.status' }, c1.word, c1.why),
      h('td', { class: 'ov-c-name' }, c2.label, c2.err),
      h('td', { class: 'ov-c-last', dataset: { k: 'last good' } }, c3.last, c3.next),
      h('td', { class: 'ov-c-pace', dataset: { k: 'pace' } }, c4.pace, c4.today),
      h('td', { class: 'ov-c-day num', dataset: { k: '24 h' } }, c5.pct, c5.err),
      h('td', { class: 'ov-c-week num', dataset: { k: '7 d' } }, c6.pct, c6.err),
      h('td', { class: 'ov-c-up num', dataset: { k: 'uptime' } }, c7.day, c7.week),
      h('td', { class: 'ov-c-spk', dataset: { k: 'last 24 h' } }, spk),
    ];
    const tr = h('tr', { class: 'ov-row', tabindex: '0', dataset: { row: '', id: c.id } }, tds);
    const r = { tr, c1, c2, c3, c4, c5, c6, c7, spk, status: null, bkey: null, conn: c };
    tr.addEventListener('click', (e) => { if (e.target instanceof Element && e.target.closest('a, button')) return; if (r.conn.link) go(r.conn.link); });
    tr.addEventListener('keydown', (e) => { if (e.key === 'Enter' && e.target === tr && r.conn.link) { e.preventDefault(); e.stopPropagation(); go(r.conn.link); } });
    return r;
  }

  function fillRow(r, c) {
    r.conn = c;
    if (c.link) r.tr.dataset.href = href(c.link); else delete r.tr.dataset.href;
    r.tr.setAttribute('aria-label', `${c.label}: ${c.status}, ${reasonText(c)}${c.missing_logs > 0 ? `, ${c.missing_logs} logs missing` : ''}`);
    if (r.status !== c.status) {
      replace(r.c1.word, light(c.status));
      if (r.status) r.tr.classList.remove(`ov-row--${r.status}`);
      r.tr.classList.add(`ov-row--${c.status}`);
      r.status = c.status;
    }
    put(r.c1.why, reasonText(c));
    put(r.c2.name, c.label);
    // a CAN Bridge row: logs the server lost and asked the car for again (nothing when none)
    put(r.c2.miss, c.missing_logs > 0 ? `  ${formatNumber(c.missing_logs)}${NB}missing` : '');
    r.c2.miss.title = c.missing_logs > 0 ? 'logs the server lost and asked the car to send again (see the car\'s health page)' : '';
    const le = c.last_error;
    if (!le) put(r.c2.err, 'no error kept');
    r.c2.err.classList.toggle('ov-lasterr--on', !!le);
    r.c2.err.title = le ? `${le.code}: ${le.message || ''} (${dateTime(le.at_ms, tz)})` : '';
    const ps = c.pace_s != null ? `${formatNumber(c.pace_s)}${NB}s` : null;
    put(r.c4.pace, c.pace ? `${c.pace}${ps ? `, ${ps}` : ''}` : ps ? `every ${ps}` : DASH);
    const t = c.today || {};
    put(r.c4.today, t.cap != null ? `${formatNumber(t.count ?? 0)} of ${formatNumber(t.cap)} today` : `${count(t.count, c.unit)} today`);
    r.c4.today.classList.toggle('tone--warn', t.cap != null && (t.count ?? 0) >= t.cap * 0.9);
    const period = (cells, p) => {
      put(cells.pct, p?.success_pct == null ? DASH : pct(p.success_pct));
      put(cells.err, plural(p?.errors, 'error', 'errors'));
      cells.err.classList.toggle('tone--bad', (p?.errors ?? 0) > 0);
      cells.pct.title = p ? `${count(p.count, c.unit)}, ${formatNumber(p.ok ?? 0)} ok` : '';
    };
    period(r.c5, c.day); period(r.c6, c.week);
    put(r.c7.day, `${pct(c.uptime?.day_pct)}${NB}24h`);
    put(r.c7.week, `${pct(c.uptime?.week_pct)}${NB}${NB}7d`);
    const bk = barsKey(c);
    if (bk !== r.bkey) { replace(r.spk, bars(c.hourly, c.unit, tz)); r.bkey = bk; }
  }

  function tickRow(r, n) {
    const c = r.conn;
    put(r.c3.last, c.last_ok_ms == null ? 'never' : rel(c.last_ok_ms, n));
    if (c.last_error) put(r.c2.err, `${c.last_error.at_ms != null ? `${rel(c.last_error.at_ms, n)}: ` : ''}${c.last_error.message || c.last_error.code}`);
    put(r.c3.next, c.next_ms == null ? 'none planned' : c.next_ms <= n ? 'next due now' : `next ${rel(c.next_ms, n)}`);
  }

  function probRow(p) {
    const dur = h('td', { class: 'ov-c-dur num', dataset: { k: 'how long' } });
    const res = h('td', { class: 'ov-c-res' });
    const link = data?.connections?.find((c) => c.id === p.connection)?.link || null;
    const tr = h('tr', { class: 'ov-prow', tabindex: link ? '0' : null, dataset: { row: '', href: link ? href(link) : null, id: p.id } },
      h('td', { class: 'ov-c-when', dataset: { k: 'began' } }, dateTime(p.start_ms, tz)),
      h('td', { class: 'ov-c-conn' }, p.label || p.connection),
      h('td', { class: 'ov-c-what' }, p.message || (p.code || '').replace(/_/g, ' ')),
      dur, res);
    if (link) {
      tr.addEventListener('click', () => go(link));
      tr.addEventListener('keydown', (e) => { if (e.key === 'Enter' && e.target === tr) { e.preventDefault(); e.stopPropagation(); go(link); } });
    }
    return { tr, dur, res, p, resolved: null };
  }
  function fillProb(r, p) {
    r.p = p;
    if (r.resolved !== p.resolved) {
      replace(r.res, p.resolved ? tag('resolved', 'good') : tag('ongoing', 'bad'));
      r.tr.classList.toggle('ov-prow--open', !p.resolved);
      r.resolved = p.resolved;
    }
  }
  function tickProb(r, n) {
    const p = r.p;
    const ms = p.resolved || p.end_ms != null ? p.duration_ms ?? (p.end_ms - p.start_ms) : n - p.start_ms;
    put(r.dur, formatDuration(ms));
  }

  // keep the rows in the answer's order, reusing the ones already there
  function sync(map, items, make, fill, parent) {
    const seen = new Set();
    let prev = null;
    for (const it of items) {
      seen.add(it.id);
      let r = map.get(it.id);
      if (!r) { r = make(it); map.set(it.id, r); }
      fill(r, it);
      const want = prev ? prev.tr.nextSibling : parent.firstChild;
      if (want !== r.tr) parent.insertBefore(r.tr, want);
      prev = r;
    }
    for (const [k, r] of map) if (!seen.has(k)) { r.tr.remove(); map.delete(k); }
  }

  function tick() {
    const n = now();
    for (const r of rows.values()) tickRow(r, n);
    for (const r of prows.values()) tickProb(r, n);
    put(readEl, readAt == null ? '' : `read ${rel(readAt, n)}`);
  }

  function paintData(d) {
    data = d;
    readAt = now();
    tot.set(d.totals);
    sync(rows, d.connections || [], rowFor, fillRow, tbody);
    sync(prows, d.problems || [], probRow, fillProb, ptbody);
    pnote.hidden = !!(d.problems || []).length;
    ptbl.hidden = !(d.problems || []).length;
    pbox.el.hidden = ptbl.hidden;
    pbox.recheck();
    first.remove();
    body.hidden = false;
    tick();
  }

  // GET once at the start and after a reconnect, then the `connections` events
  let first1;
  const ready = new Promise((res) => { first1 = res; });
  follow(ctx.api, {
    type: 'connections',
    load: () => ctx.api.get('integrations/overview'),
    pollMs: POLL_MS,
    onValue: (d) => {
      if (!ctx.alive() || !d) return;
      put(errEl, '');
      paintData(d);
      first1();
    },
    onError: (e) => {
      if (!ctx.alive()) return;
      // the old figures stay; a word says they are not new
      if (data) { put(errEl, `not read again: ${errorWords(e).text}  `); errEl.className = 'ov-err tone--warn'; }
      else replace(first, h('span', { class: 'tone--bad' }, `the overview could not be read: ${errorWords(e).text}`));
      first1();
    },
  });
  ctx.every(1000, tick);
  return { el, ready, totals: () => data?.totals || null };
}

/** For pages that are not for admins: the overview is theirs only. */
export function adminOnly() {
  return section({ title: 'connections', cls: 'ov-sec' },
    cover(h('p', { class: 'note' }, tag('admin', 'muted'), ' the connections overview (every connection the server keeps, how it is doing and what went wrong) is shown to admins.'),
      'conn.overview.rows', 'conn.overview.status', 'conn.overview.sparkline', 'conn.overview.problems'));
}
