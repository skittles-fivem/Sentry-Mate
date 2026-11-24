// Sentry Mate, "Logbook" UI pack: the Connections overview of Settings: Integrations (its first tab), and the
// small totals card the Data health page links to it with.
//
// API: GET integrations/overview (admins): every connection the server keeps, with its status, last good
// contact, next poll, pace, today's count, 24 h and 7 d counts, uptime, last error and 24 hourly counts; the
// problems of the last 7 days; and the totals per status.
//
// Live: `connections` events while the overview is shown (live events, polling every 10 s only while the live
// stream is down); every text is set in place (the rows, the bars and the problems keep their elements), and
// relative times tick every second. Nothing is emptied while a read is on its way, and a read that fails keeps
// what was read before and says so on the line under the head.

import { h, svg, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { format } from '../shared/units.js';
import { formatDateTime, formatDuration, formatRelative } from '../shared/time.js';
import { section, icon, empty, errorView, errorWords } from '../core/ui.js';
import { scrollBox } from '../shared/scrollbox.js';
import { follow } from '../shared/live.js';

const POLL_MS = 10000;
const NBSP = ' ';

/* ------------------------------------------------------------------ words */

export const STATUS_ORDER = ['live', 'idle', 'paused', 'off', 'problem'];
export const STATUS_WORD = { live: 'Live', idle: 'Idle', paused: 'Paused', off: 'Off', problem: 'Problem' };
const STATUS_NOTE = {
  live: 'worked within its expected pace',
  idle: 'quiet as expected: the car asleep or parked',
  paused: 'paused by an admin',
  off: 'not set up or turned off: no connection is made',
  problem: 'its last tries failed, or nothing came',
};

// status_reason, in words
const REASON = {
  car_asleep: 'The car is asleep, as expected',
  parked: 'Parked: nothing to send, as expected',
  paused: 'Paused by an admin',
  not_added: 'Not added yet: no connection is made',
  turned_off: 'Turned off: no connection is made',
  offline_mode: 'Offline mode: no outside connection is made',
  no_car: 'No car is read on this account yet',
  token_refused: 'The token was refused',
  sign_in_needed: 'Needs a new sign-in',
  unreachable: 'Could not be reached',
  quiet: 'Nothing came, though something should have',
  rate_limited: 'Asked to slow down',
  connection_refused: 'The connection was refused',
  auth_failed: 'The user name or password was refused',
  tls_failed: 'The secure connection failed',
  pin_mismatch: 'The certificate did not match its pin',
  timeout: 'Timed out',
  query_failed: 'A query failed',
  not_teslamate: 'The database has no TeslaMate tables',
  tesla_unreachable: 'Tesla could not be reached',
  teslafi_refused: 'TeslaFi refused the token',
  teslafi_unreachable: 'TeslaFi could not be reached',
};
const LIVE_WORD = 'Working within its pace';
const PACE_WORD = { often: 'Often', normal: 'Normal', gentle: 'Gentle' };

function reasonText(c) {
  if (c.status_reason) return REASON[c.status_reason] || (c.status_reason.charAt(0).toUpperCase() + c.status_reason.slice(1)).replace(/_/g, ' ');
  return c.status === 'live' ? LIVE_WORD : STATUS_NOTE[c.status] || '';
}

const count = (n) => format(n ?? 0, 'count');
const pct = (v) => (v == null ? null : format(v, 'percent', { digits: Number.isInteger(v) ? 0 : 1 }));
const unitWord = (u, n) => {
  const one = { uploads: 'upload', requests: 'request', messages: 'message' }[u] || u;
  return n === 1 ? one : u;
};

/** '12 s ago', '4 min ago', 'in 30 s', 'due now'. */
export function relText(ms, now) {
  if (ms == null || now == null) return '';
  const d = now - ms;
  if (d >= 0 && d < 60000) return `${Math.floor(d / 1000)}${NBSP}s ago`;
  if (d < 0 && d > -60000) return `in ${Math.ceil(-d / 1000)}${NBSP}s`;
  return formatRelative(ms, now);
}
/** An element whose text is a relative time that ticks (see tickTimes). */
function relEl(cls) { return h('span', { class: cls, dataset: { coAt: '' } }); }
function setRel(el, ms, { future = false, none = '' } = {}) {
  el.dataset.coAt = ms == null ? '' : String(ms);
  el.dataset.coFuture = future ? '1' : '';
  el.dataset.coNone = none;
  paintRel(el, api.now());
}
function paintRel(el, now) {
  const ms = el.dataset.coAt === '' ? null : Number(el.dataset.coAt);
  let t;
  if (ms == null) t = el.dataset.coNone || '';
  else if (el.dataset.coFuture && ms - now < 1000) t = 'due now';
  else t = relText(ms, now);
  if (el.textContent !== t) el.textContent = t;
}
export function tickTimes(root) {
  const now = api.now();
  for (const el of root.querySelectorAll('[data-co-at]')) paintRel(el, now);
}
const setText = (el, t) => { const v = t == null ? '' : String(t); if (el.textContent !== v) el.textContent = v; };
const setClass = (el, cls, on) => el.classList.toggle(cls, !!on);

/** The status light: a mark of its own shape for each status, and the word beside it. */
export function statusMark(status) {
  const dot = h('span', { class: ['co-dot', `co-${status}`], 'aria-hidden': 'true' });
  const word = h('span', { class: 'co-word' }, STATUS_WORD[status] || status);
  return h('span', { class: 'co-mark' }, dot, word);
}
function setStatus(mark, status) {
  const dot = mark.firstChild;
  for (const s of STATUS_ORDER) setClass(dot, `co-${s}`, s === status);
  setText(mark.lastChild, STATUS_WORD[status] || status);
}

/* ------------------------------------------------------------------ the sparkline */

const BARS = 24;
const VB_W = 240, VB_H = 40, BAR_W = 7, STEP = 10, TOP = 2, BASE = 33;

function sparkEl() {
  const bars = [], ticks = [];
  for (let i = 0; i < BARS; i++) {
    bars.push(svg('rect', { class: 'co-bar', x: String(i * STEP + 1.5), y: String(BASE - 1), width: String(BAR_W), height: '1' }));
    ticks.push(svg('rect', { class: 'co-tick', x: String(i * STEP + 1.5), y: String(BASE + 3), width: String(BAR_W), height: '4' }));
  }
  const base = svg('line', { class: 'co-base', x1: '0', x2: String(VB_W), y1: String(BASE + 0.5), y2: String(BASE + 0.5) });
  const el = svg('svg', { class: 'co-spark-svg', viewBox: `0 0 ${VB_W} ${VB_H}`, preserveAspectRatio: 'none', role: 'img', 'aria-label': '' }, base, bars, ticks);
  return { el, bars, ticks };
}
function paintSpark(sp, hourly, unit) {
  const list = (hourly || []).slice(-BARS);
  const pad = BARS - list.length;
  const max = Math.max(1, ...list.map((x) => x.count || 0));
  let sum = 0, errs = 0, errHours = 0;
  for (let i = 0; i < BARS; i++) {
    const x = i < pad ? null : list[i - pad];
    const n = x ? x.count || 0 : 0;
    const e = x ? x.errors || 0 : 0;
    sum += n; errs += e; if (e) errHours++;
    const hgt = n ? Math.max(2, Math.round(((BASE - TOP) * n) / max)) : 1;
    const bar = sp.bars[i];
    bar.setAttribute('y', String(BASE - hgt));
    bar.setAttribute('height', String(hgt));
    bar.classList.toggle('is-err', e > 0);
    bar.classList.toggle('is-zero', !n);
    bar.classList.toggle('is-now', i === BARS - 1);
    sp.ticks[i].classList.toggle('is-err', e > 0);
  }
  sp.el.setAttribute('aria-label', `Last 24 hours, one bar an hour: ${count(sum)} ${unitWord(unit, sum)}, ${count(errs)} ${errs === 1 ? 'error' : 'errors'}${errHours ? ` in ${errHours} ${errHours === 1 ? 'hour' : 'hours'}, marked red with a tick under the bar` : ''}.`);
}

/* ------------------------------------------------------------------ one row */

function cell(cls, label, ...kids) {
  return h('div', { class: ['co-c', cls] }, h('span', { class: 'co-k' }, label), kids);
}

function makeRow(ctx) {
  const mark = statusMark('off');
  const label = h('span', { class: 'co-label' });
  const go = h('span', { class: 'co-go', 'aria-hidden': 'true' }, icon('arrowRight'));
  const reasonText0 = h('span');
  const missing = h('span', { class: 'co-missing' });
  const reason = h('span', { class: 'co-reason' }, reasonText0, missing);
  const errLine = h('span', { class: 'co-err' }, h('span', { class: 'co-err-k' }), relEl(''), h('span', null));
  const lastOk = relEl('co-v');
  const next = h('span', { class: 'co-s' }, h('span', null, 'next '), relEl(''));
  const pace = h('span', { class: 'co-v' });
  const paceSub = h('span', { class: 'co-s' });
  const today = h('span', { class: 'co-v' });
  const todaySub = h('span', { class: 'co-s' });
  const ok = h('span', { class: 'co-v' });
  const okSub = h('span', { class: 'co-s' });
  const up = h('span', { class: 'co-v' });
  const upSub = h('span', { class: 'co-s' }, '24 h · 7 d');
  const sp = sparkEl();
  const sparkSub = h('span', { class: 'co-s co-spark-axis', 'aria-hidden': 'true' }, h('span', null, '24 h ago'), h('span', null, 'now'));
  const el = h('a', { class: 'co-row' },
    h('div', { class: 'co-c co-state' }, h('span', { class: 'co-k' }, 'Status'), mark),
    h('div', { class: 'co-c co-name' }, h('span', { class: 'co-title' }, label, go), reason, errLine),
    cell('co-contact', 'Last good contact', lastOk, next),
    cell('co-pace', 'Pace', pace, paceSub),
    cell('co-today', 'Today', today, todaySub),
    cell('co-ok', 'Success 24 h · 7 d', ok, okSub),
    cell('co-up', 'Uptime', up, upSub),
    h('div', { class: 'co-c co-spark', cover: 'conn.overview.sparkline' }, h('span', { class: 'co-k' }, 'Last 24 hours'), sp.el, sparkSub));

  function paint(c) {
    el.href = c.link || '#';
    el.dataset.status = c.status;
    setClass(el, 'is-problem', c.status === 'problem');
    setStatus(mark, c.status);
    setText(label, c.label);
    setText(reasonText0, reasonText(c));
    const nm = c.missing_logs || 0;
    setText(missing, nm > 0 ? ` · ${count(nm)} ${nm === 1 ? 'log' : 'logs'} missing` : '');
    if (nm > 0) missing.title = 'Logs missing on the server; the car was asked to send them again.'; else missing.removeAttribute('title');
    const e = c.last_error;
    const [ek, eat, emsg] = errLine.children;
    setText(ek, e ? 'Last error ' : 'No error kept');
    setRel(eat, e ? e.at_ms : null);
    setText(emsg, e ? `: ${e.message || e.code}` : '');
    setClass(errLine, 'is-bad', !!e && c.status === 'problem');
    setClass(errLine, 'is-none', !e);

    setRel(lastOk, c.last_ok_ms, { none: c.status === 'off' ? 'never: off' : 'not yet' });
    const nx = next.lastChild;
    if (c.next_ms != null) { setText(next.firstChild, 'next '); setRel(nx, c.next_ms, { future: true }); } else {
      setText(next.firstChild, c.status === 'live' ? 'as data comes' : c.status === 'off' || c.status === 'paused' ? 'nothing planned' : 'no poll planned');
      setRel(nx, null);
    }

    setText(pace, c.pace ? `${PACE_WORD[c.pace] || c.pace}` : c.pace_s != null ? 'Steady' : c.status === 'off' ? 'Off' : 'As it comes');
    setText(paceSub, c.pace_s != null ? `every ${formatDuration(c.pace_s * 1000)}` : c.status === 'live' ? 'pushed to the server' : 'no wait set');

    const t = c.today || {};
    setText(today, count(t.count));
    setText(todaySub, t.cap != null ? `of ${count(t.cap)} ${unitWord(c.unit, t.cap)} a day` : `${unitWord(c.unit, t.count)}, no cap`);

    const d = c.day || {}, w = c.week || {};
    setText(ok, d.success_pct == null && w.success_pct == null ? 'no tries' : `${pct(d.success_pct) ?? 'no tries'} · ${pct(w.success_pct) ?? 'no tries'}`);
    setText(okSub, `${count(d.errors)} · ${count(w.errors)} ${w.errors === 1 ? 'error' : 'errors'}`);
    setClass(okSub, 'is-bad', (d.errors || 0) > 0);

    const u = c.uptime || {};
    setText(up, u.day_pct == null && u.week_pct == null ? 'off all week' : `${pct(u.day_pct) ?? 'off'} · ${pct(u.week_pct) ?? 'off'}`);
    setClass(up, 'is-bad', u.day_pct != null && u.day_pct < 99);

    paintSpark(sp, c.hourly, c.unit);
  }
  return { el, paint };
}

/* ------------------------------------------------------------------ problems */

function makeProblem(ctx) {
  const time = h('time', { class: 'co-p-time' });
  const who = h('span', { class: 'co-p-who' });
  const what = h('span', { class: 'co-p-what' });
  const long = h('span', { class: 'co-p-long' });
  const st = h('span', { class: 'co-p-state' });
  const el = h('li', { class: 'co-p' },
    h('div', { class: 'co-p-c' }, h('span', { class: 'co-k' }, 'Began'), time),
    h('div', { class: 'co-p-c' }, h('span', { class: 'co-k' }, 'Connection'), who),
    h('div', { class: 'co-p-c co-p-msg' }, h('span', { class: 'co-k' }, 'What happened'), what),
    h('div', { class: 'co-p-c' }, h('span', { class: 'co-k' }, 'How long'), long),
    h('div', { class: 'co-p-c' }, h('span', { class: 'co-k' }, 'Now'), st));
  function paint(p) {
    time.setAttribute('datetime', new Date(p.start_ms).toISOString());
    setText(time, formatDateTime(p.start_ms, ctx.tz, { now: api.now() }));
    setText(who, p.label);
    setText(what, p.message || reasonText({ status_reason: p.code }));
    setText(long, p.resolved ? `lasted ${formatDuration(p.duration_ms)}` : `${formatDuration(p.duration_ms)} so far`);
    setClass(el, 'is-open', !p.resolved);
    const want = p.resolved ? 'Resolved' : 'Ongoing';
    if (st.dataset.v !== want) {
      st.dataset.v = want;
      replace(st, h('span', { class: ['chip', p.resolved ? null : 'chip-warn'] }, icon(p.resolved ? 'check' : 'warning'), want));
    }
  }
  return { el, paint };
}

/* ------------------------------------------------------------------ the part */

/**
 * The overview: overviewPart(ctx) -> { el, load(), start(), stop() }. load() reads it once and draws it (the
 * first time it throws nothing: a failed first read shows an error with Try again in the part).
 */
export function overviewPart(ctx) {
  let data = null;
  let readAt = null;
  let lastFail = null;
  let live = null;
  let ticker = null;
  let running = false;
  let inflight = null;

  // the totals strip
  const totalEls = {};
  const totals = h('div', { class: 'figs co-totals', role: 'list', 'aria-label': 'Connections by status' },
    STATUS_ORDER.map((s) => {
      const v = h('span', { class: 'co-total-n' }, '0');
      totalEls[s] = v;
      return h('div', { class: ['fig', 'co-total', `co-total-${s}`], role: 'listitem' },
        h('p', { class: 'fig-name' }, statusMark(s)),
        h('p', { class: 'fig-value' }, v),
        h('p', { class: 'fig-note' }, STATUS_NOTE[s]));
    }));

  const readLine = h('p', { class: 'co-read', 'aria-live': 'off' }, h('span', null, 'Read '), relEl(''), h('span', null, ' · updated as it changes while this tab is open'));

  const head = h('div', { class: 'co-head', 'aria-hidden': 'true' },
    [['Status'], ['Connection'], ['Last good', 'next contact'], ['Pace'], ['Today'], ['Success', '24 h · 7 d'], ['Uptime', '24 h · 7 d'], ['Last 24 hours', 'a bar an hour']]
      .map(([t, sub]) => h('span', { class: 'co-head-c' }, t, sub ? h('span', { class: 'co-head-sub' }, sub) : null)));
  const list = h('div', { class: 'co-list', role: 'list', cover: ['conn.overview.rows', 'conn.overview.status'] });
  const rows = new Map();
  const listBox = h('div', { class: 'co-box' }, head, list);

  const probList = h('ol', { class: 'co-probs' });
  // a long week of problems scrolls in a box about five high (at most: a short list leaves no empty room)
  const probScroll = scrollBox({ cls: 'scroll-box--fit co-probs-scroll', label: 'Problems in the last 7 days', signal: ctx.signal }, probList);
  const probBox = h('div', { class: 'co-probs-box' }, probScroll.el);
  const probs = new Map();

  const body = h('div', { class: 'co-body' },
    section({ title: 'Connections', note: 'every input and outside connection; a row opens its details', cls: 'co-sec', id: 'co-conns' }, readLine, totals, listBox),
    section({ title: 'Problems in the last 7 days', note: 'newest first; a problem lasts while its connection is in trouble', cls: 'co-sec', id: 'co-problems', cover: 'conn.overview.problems' }, probBox));
  const el = h('div', { class: 'co' }, body);

  function paintRows(conns) {
    const ids = conns.map((c) => c.id);
    const same = ids.length === rows.size && ids.every((id, i) => [...rows.keys()][i] === id);
    if (!same) {
      const next = new Map();
      for (const c of conns) next.set(c.id, rows.get(c.id) || makeRow(ctx));
      rows.clear();
      for (const [k, v] of next) rows.set(k, v);
      replace(list, [...rows.values()].map((r) => { r.el.setAttribute('role', 'listitem'); return r.el; }));
    }
    for (const c of conns) rows.get(c.id).paint(c);
  }

  function paintProblems(list2) {
    const ids = list2.map((p) => p.id);
    const same = ids.length === probs.size && ids.every((id, i) => [...probs.keys()][i] === id);
    if (!same) {
      const next = new Map();
      for (const p of list2) next.set(p.id, probs.get(p.id) || makeProblem(ctx));
      probs.clear();
      for (const [k, v] of next) probs.set(k, v);
      if (list2.length) replace(probBox, probScroll.el); else replace(probBox, empty({ title: 'No problem in the last 7 days.', text: 'Every connection either worked or was off, paused or quiet as expected all week.' }));
      replace(probList, [...probs.values()].map((r) => r.el));
    }
    for (const p of list2) probs.get(p.id).paint(p);
    probScroll.recheck();
  }

  function paint() {
    if (!data) return;
    for (const s of STATUS_ORDER) setText(totalEls[s], count((data.totals || {})[s] || 0));
    paintRows(data.connections || []);
    paintProblems(data.problems || []);
    const at = readLine.children[1];
    setText(readLine.firstChild, lastFail ? 'Could not read again (' : 'Read ');
    setRel(at, readAt);
    setText(readLine.lastChild, lastFail
      ? `${errorWords(lastFail).text.replace(/\.$/, '')}); showing what was read then · trying again`
      : ' · updated as it changes while this tab is open');
    setClass(readLine, 'is-bad', !!lastFail);
  }

  async function read() {
    if (inflight) return inflight;
    inflight = (async () => {
      try {
        const d = await ctx.api.get('integrations/overview');
        if (!ctx.alive()) return;
        data = d; readAt = d.read_ms ?? api.now(); lastFail = null;
      } catch (e) {
        if (api.isAbort(e)) return;
        if (!data) throw e;
        lastFail = e;
      }
      paint();
    })().finally(() => { inflight = null; });
    return inflight;
  }

  async function load() {
    try { await read(); } catch (e) {
      if (api.isAbort(e)) throw e;
      replace(el, errorView(e, { title: 'The connections overview could not be read.', home: false, retry: () => { replace(el, body); load(); } }));
    }
  }

  // while shown: the `connections` events, a read after a reconnect (and at the start when asked), and only while
  // the live stream is down a read every POLL_MS
  function start({ now = false } = {}) {
    if (running) return;
    running = true;
    let skip = !now;
    live = follow(ctx.api, {
      type: 'connections',
      load: () => { if (skip) { skip = false; return Promise.resolve(null); } return read().catch(() => {}).then(() => null); },
      pollMs: POLL_MS,
      onValue: (d) => {
        if (!d || !ctx.alive()) return;
        data = d; readAt = d.read_ms ?? api.now(); lastFail = null;
        paint();
      },
    });
    clearInterval(ticker);
    ticker = setInterval(() => tickTimes(el), 1000);
  }
  function stop() { running = false; live?.stop(); live = null; clearInterval(ticker); }
  ctx.onCleanup(stop);

  return { el, load, start, stop, get data() { return data; } };
}

/* ------------------------------------------------------------------ the card for Data health */

/** A card that links to the overview, with the totals: overviewCard(ctx) -> { el, load() }. */
export function overviewCard(ctx, { href, cls = null } = {}) {
  const nums = {};
  const line = h('ul', { class: 'co-card-totals', 'aria-label': 'Connections by status' },
    STATUS_ORDER.map((s) => {
      const n = h('span', { class: 'co-card-n' }, '0');
      nums[s] = n;
      return h('li', { class: ['co-card-t', `co-total-${s}`] }, h('span', { class: ['co-dot', `co-${s}`], 'aria-hidden': 'true' }), n, h('span', { class: 'co-card-w' }, STATUS_WORD[s].toLowerCase()));
    }));
  const note = h('p', { class: 'co-card-note' }, 'Every input and outside connection on one page: is it live, when it last worked, how busy it is and what went wrong this week.');
  const el = h('a', { class: ['co-card', cls], href, cover: 'conn.overview.link' },
    h('span', { class: 'co-card-title' }, 'Connections overview', icon('arrowRight')),
    note, line);
  async function load() {
    try {
      const d = await ctx.api.get('integrations/overview');
      if (!ctx.alive()) return;
      for (const s of STATUS_ORDER) setText(nums[s], count((d.totals || {})[s] || 0));
      const open = (d.problems || []).filter((p) => !p.resolved).length;
      setText(note, `${count((d.connections || []).length)} connections on one page: is each live, when it last worked, how busy it is and what went wrong this week.${open ? ` ${count(open)} ${open === 1 ? 'problem is' : 'problems are'} going on now.` : ''}`);
      el.classList.toggle('is-problem', open > 0);
    } catch (e) {
      if (api.isAbort(e)) throw e;
      line.hidden = true;
    }
  }
  return { el, load };
}
