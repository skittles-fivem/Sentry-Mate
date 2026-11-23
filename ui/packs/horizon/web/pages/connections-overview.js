// The Connections overview (Settings: Integrations, its first tab, for admins): every connection the server
// keeps, on one screen. A totals strip, one row per connection (status light and word, why, last good contact,
// next poll, pace, today's count of its cap, success and errors over 24 h and 7 days, the last error, uptime,
// and a 24 h sparkline of the hours), then the problems of the last 7 days.
// API: GET integrations/overview (admin). It has no live event, so it is read again every 10 s while the tab is
// shown; every value is written in place (old values stay until new ones arrive) and the times tick each second.

import { h, replace, setVars } from '../shared/dom.js';
import { formatDuration, formatTime } from '../shared/time.js';
import { icon } from '../core/icons.js';
import { panel } from '../core/page.js';
import { errorState, errorWords, emptyState } from '../core/states.js';
import { num, when, dateTime, DASH } from '../core/fmt.js';

export const POLL_MS = 10_000;

// status -> [word, tone, what it means]
export const CONN_STATUS = {
  live: ['Live', 'live', 'Worked within its expected pace.'],
  idle: ['Idle', 'idle', 'Quiet as expected: the car asleep or parked, nothing to send.'],
  paused: ['Paused', 'paused', 'An admin paused it.'],
  off: ['Off', 'off', 'Not set up or turned off: no connection is made.'],
  problem: ['Problem', 'problem', 'Its last tries failed, or nothing came when something should have.'],
};
export const STATUS_ORDER = ['live', 'idle', 'paused', 'off', 'problem'];

const REASONS = {
  car_asleep: 'Car asleep, as expected',
  parked: 'Car parked: nothing new to read, polls slower',
  not_added: 'Not set up yet',
  offline_mode: 'Offline mode: no outside requests',
  turned_off: 'Turned off',
  paused: 'Paused by an admin',
  token_refused: 'The sign-in was refused: sign in again',
  unreachable: 'Could not be reached',
  quiet: 'Nothing came when something should have',
  connection_refused: 'The connection was refused',
  auth_failed: 'The user name or password was refused',
  pin_mismatch: 'The certificate did not match its pin',
  catchup_failed: 'The catch-up failed',
  log_check_failed: 'The last log failed its check',
  rate_limited: 'Asked to slow down (rate limit)',
  tesla_unreachable: 'Tesla did not answer',
};
const reasonText = (c) => {
  if (c.status_reason) return REASONS[c.status_reason] || cap(String(c.status_reason).replace(/_/g, ' '));
  return { live: 'Working within its pace', idle: 'Quiet, as expected', paused: 'Paused by an admin', off: 'Not set up', problem: 'Its last tries failed' }[c.status] || DASH;
};
const cap = (s) => (s ? s[0].toUpperCase() + s.slice(1) : s);
const PACE = { often: 'Often', normal: 'Normal', gentle: 'Gentle' };
const UNIT = { uploads: ['upload', 'uploads'], requests: ['request', 'requests'], messages: ['message', 'messages'] };
const unitWord = (u, n) => (UNIT[u] ? UNIT[u][n === 1 ? 0 : 1] : u || '');
const pct = (v) => (v == null ? DASH : `${num(v, v === 100 || v === 0 ? 0 : 1)} %`);

/** '12 s ago' (seconds under a minute), against now. */
export function agoText(ms, now) {
  if (ms == null) return 'never';
  const d = now - ms;
  if (d < 1000) return 'just now';
  return `${formatDuration(d)} ago`;
}
/** 'in 30 s', or 'due now' once past. */
function inText(ms, now) {
  if (ms == null) return null;
  const d = ms - now;
  if (d < 1000) return 'due now';
  return `in ${formatDuration(d)}`;
}

// links in the overview that name a section by its server name: the section's id on this pack's page
const HASH_ALIAS = { mqtt: 'si-broker' };
export function sectionId(hash) {
  const id = decodeURIComponent(String(hash || '').replace(/^#/, ''));
  return HASH_ALIAS[id] || id;
}
function linkOf(c) {
  const l = c.link || '/settings/integrations';
  const i = l.indexOf('#');
  return i < 0 ? l : `${l.slice(0, i)}#${sectionId(l.slice(i))}`;
}

/** The totals as text: '6 live · 1 idle · 1 problem'. */
export function totalsText(t) {
  return STATUS_ORDER.filter((s) => t?.[s]).map((s) => `${num(t[s])} ${CONN_STATUS[s][0].toLowerCase()}`).join(' · ') || 'no connection yet';
}

/** A status light with its word (never colour alone). */
export function statusLight(status, { lg = false } = {}) {
  const st = CONN_STATUS[status] ? status : 'off';
  const word = h('span', { class: 'co-word' }, CONN_STATUS[st][0]);
  const el = h('span', { class: ['co-light', lg && 'co-light--lg'], dataset: { st }, title: CONN_STATUS[st][2] }, h('i', { 'aria-hidden': 'true' }), word);
  el.set = (s) => { const k = CONN_STATUS[s] ? s : 'off'; el.dataset.st = k; word.textContent = CONN_STATUS[k][0]; el.title = CONN_STATUS[k][2]; };
  return el;
}

/**
 * The overview, for ctx (an admin). Returns { el, ready, show(on) }: ready resolves after the first read
 * (with data or with its error drawn), show(true) while its tab is on screen (it reads again and ticks only then).
 */
export function connectionsOverview(ctx, { onSameRoute = null } = {}) {
  const tz = ctx.tz;
  let data = null, shown = true, readAt = null, failAt = null, failText = null, busy = false;

  // the head: when it was read, kept the same height whether it worked or not
  const readEl = h('span', { class: 'co-read', 'aria-live': 'polite' }, 'Reading…');
  const paintRead = () => {
    const now = ctx.api.now();
    readEl.classList.toggle('warn', !!failAt);
    readEl.textContent = failAt
      ? `Could not read again (${failText}), ${agoText(failAt, now)}; shown: read ${agoText(readAt, now)}`
      : readAt != null ? `Read ${agoText(readAt, now)} · again every ${POLL_MS / 1000} s` : 'Reading…';
  };

  /* ---------- totals */
  const totalEls = {};
  const totalsEl = h('ul', { class: 'co-totals', 'aria-label': 'Connections by status', cover: 'conn.overview.status' },
    STATUS_ORDER.map((s) => {
      const n = h('b', { class: 'co-total-n' }, DASH);
      totalEls[s] = n;
      return h('li', { class: 'co-total', dataset: { st: s }, title: CONN_STATUS[s][2] },
        h('i', { class: 'co-total-light', 'aria-hidden': 'true' }), n, h('span', { class: 'co-total-w' }, CONN_STATUS[s][0]));
    }));
  const paintTotals = () => { for (const s of STATUS_ORDER) { totalEls[s].textContent = num(data?.totals?.[s] ?? 0); totalEls[s].parentElement.classList.toggle('is-zero', !data?.totals?.[s]); } };

  /* ---------- rows */
  const listEl = h('div', { class: 'co-list', role: 'list', 'aria-label': 'Connections', cover: 'conn.overview.rows' });
  // the column heads (wide screens; each card names its own values)
  const headEl = h('div', { class: 'co-row co-head', 'aria-hidden': 'true' },
    h('span', { class: 'co-cell co-c-st' }, 'Status'),
    h('span', { class: 'co-cell co-c-name' }, 'Connection'),
    h('span', { class: 'co-cell co-c-contact' }, 'Last good', h('br'), 'Next'),
    h('span', { class: 'co-cell co-c-load' }, 'Pace', h('br'), 'Today'),
    h('span', { class: 'co-cell co-c-ok' }, 'Success 24 h', h('br'), 'Success 7 d'),
    h('span', { class: 'co-cell co-c-up' }, 'Uptime', h('br'), '24 h / 7 d'),
    h('span', { class: 'co-cell co-c-spark' }, 'Last 24 h, per hour'));
  let rows = new Map(), order = '';

  const line = (k, v) => h('span', { class: 'co-line' }, h('span', { class: 'co-k' }, k), v);
  function buildRow(c) {
    const r = {};
    r.light = statusLight(c.status);
    r.label = h('span', { class: 'co-label' }, c.label);
    r.reason = h('span', { class: 'co-reason' });
    r.errMsg = h('span', { class: 'co-err-msg' });
    r.errAt = h('span', { class: 'co-err-at' });
    r.err = h('span', { class: 'co-err' }, h('b', null, 'Last error: '), r.errMsg, r.errAt);
    r.last = h('span', { class: 'co-v' });
    r.next = h('span', { class: 'co-v' });
    r.pace = h('span', { class: 'co-v' });
    r.today = h('span', { class: 'co-v' });
    r.day = h('span', { class: 'co-v' });
    r.week = h('span', { class: 'co-v' });
    r.upDay = h('span', { class: 'co-v' });
    r.upWeek = h('span', { class: 'co-v' });
    r.bars = Array.from({ length: 24 }, () => h('i'));
    r.spark = h('span', { class: 'co-spark', role: 'img', cover: 'conn.overview.sparkline' }, r.bars);
    r.sparkNote = h('span', { class: 'co-spark-note' });
    r.el = h('a', { class: 'co-row', role: 'listitem', href: linkOf(c), dataset: { id: c.id }, onclick: (e) => onRowClick(e, r.el) },
      h('span', { class: 'co-cell co-c-st' }, r.light),
      h('span', { class: 'co-cell co-c-name' }, r.label, r.reason, r.err),
      h('span', { class: 'co-cell co-c-contact' }, line('Last good', r.last), line('Next', r.next)),
      h('span', { class: 'co-cell co-c-load' }, line('Pace', r.pace), line('Today', r.today)),
      h('span', { class: 'co-cell co-c-ok' }, line('OK 24 h', r.day), line('OK 7 d', r.week)),
      h('span', { class: 'co-cell co-c-up' }, line('Up 24 h', r.upDay), line('Up 7 d', r.upWeek)),
      h('span', { class: 'co-cell co-c-spark' }, h('span', { class: 'co-k' }, 'Last 24 h, per hour'), r.spark, r.sparkNote));
    return r;
  }
  function paintRow(r, c) {
    r.c = c;
    r.el.dataset.st = c.status;
    r.light.set(c.status);
    r.label.textContent = c.label;
    r.reason.textContent = reasonText(c);
    const le = c.last_error;
    r.err.hidden = !le;
    r.errMsg.textContent = le ? (le.message || le.code || 'Error') : '';
    r.errAt.dataset.at = le?.at_ms ?? '';
    r.errAt.title = le?.at_ms != null ? dateTime(le.at_ms, tz) : '';
    r.last.title = c.last_ok_ms != null ? dateTime(c.last_ok_ms, tz) : '';
    r.next.title = c.next_ms != null ? dateTime(c.next_ms, tz) : '';
    r.pace.textContent = c.pace ? `${PACE[c.pace] || cap(c.pace)}${c.pace_s != null ? ` · ${formatDuration(c.pace_s * 1000)}` : ''}`
      : c.pace_s != null ? `Every ${formatDuration(c.pace_s * 1000)}` : c.status === 'off' ? DASH : c.unit === 'messages' ? 'As they come' : 'When needed';
    const t = c.today || {};
    r.today.textContent = t.cap != null ? `${num(t.count)} of ${num(t.cap)}` : `${num(t.count ?? 0)} ${unitWord(c.unit, t.count)}`;
    r.today.title = t.cap != null ? `${num(t.count)} ${unitWord(c.unit, t.count)} since midnight UTC, of a daily cap of ${num(t.cap)}` : `${unitWord(c.unit, 2)} since midnight UTC; no daily cap`;
    const okLine = (p) => (p?.success_pct == null ? 'no tries' : `${pct(p.success_pct)} · ${num(p.errors)} err`);
    r.day.textContent = okLine(c.day);
    r.week.textContent = okLine(c.week);
    r.day.classList.toggle('co-bad', !!c.day?.errors);
    r.week.classList.toggle('co-bad', !!c.week?.errors);
    r.day.title = `Last 24 h: ${num(c.day?.count ?? 0)} ${unitWord(c.unit, c.day?.count)}, ${num(c.day?.ok ?? 0)} good, ${num(c.day?.errors ?? 0)} errors`;
    r.week.title = `Last 7 days: ${num(c.week?.count ?? 0)} ${unitWord(c.unit, c.week?.count)}, ${num(c.week?.ok ?? 0)} good, ${num(c.week?.errors ?? 0)} errors`;
    r.upDay.textContent = pct(c.uptime?.day_pct);
    r.upWeek.textContent = pct(c.uptime?.week_pct);
    // the sparkline: one bar per hour, scaled to the busiest hour; hours with errors marked
    const hrs = (c.hourly || []).slice(-24);
    while (hrs.length < 24) hrs.unshift(null);
    const max = Math.max(1, ...hrs.map((x) => x?.count || 0));
    let errHours = 0, total = 0;
    hrs.forEach((x, i) => {
      const b = r.bars[i];
      const n = x?.count || 0, e = x?.errors || 0;
      total += n; if (e) errHours++;
      setVars(b, { '--h': (n / max).toFixed(3) });
      b.className = e ? 'is-err' : n ? '' : 'is-none';
      b.title = x ? `${formatTime(x.hour_ms, tz)}: ${num(n)} ${unitWord(c.unit, n)}${e ? `, ${num(e)} ${e === 1 ? 'error' : 'errors'}` : ''}` : '';
    });
    r.spark.setAttribute('aria-label', `Last 24 hours, per hour: ${num(total)} ${unitWord(c.unit, total)}, ${errHours ? `${errHours} ${errHours === 1 ? 'hour' : 'hours'} with errors` : 'no errors'}`);
    r.sparkNote.textContent = errHours ? `${errHours} h with errors` : total ? 'no errors' : `no ${unitWord(c.unit, 2)}`;
    r.sparkNote.classList.toggle('co-bad', !!errHours);
    r.el.setAttribute('href', linkOf(c));
    r.el.setAttribute('aria-label', `${c.label}: ${CONN_STATUS[c.status]?.[0] || c.status}, ${reasonText(c)}. Open its details.`);
  }
  function paintRows() {
    const list = data?.connections || [];
    const key = list.map((c) => c.id).join('|');
    if (key !== order) {
      // a connection came or went: the list is built again, in one go
      const next = new Map();
      for (const c of list) next.set(c.id, rows.get(c.id) || buildRow(c));
      rows = next; order = key;
      replace(listEl, list.length ? [...rows.values()].map((r) => r.el)
        : emptyState({ icon: 'share', title: 'No connection yet', text: 'Add a car, a Tesla or TeslaFi account, or a broker under the Settings tab; each appears here.' }));
    }
    for (const c of list) paintRow(rows.get(c.id), c);
  }

  /* ---------- problems */
  const probList = h('ol', { class: 'co-probs', 'aria-label': 'Problems in the last 7 days' });
  const probEmpty = h('p', { class: 'note co-probs-empty' }, icon('check', 'ico--sm'), ' No problem in the last 7 days: every connection kept working.');
  const probPanel = panel({ title: 'Problems in the last 7 days', icon: 'warn', id: 'si-problems', cls: 'co-probs-panel', cover: 'conn.overview.problems',
    side: h('span', { class: 'faint co-probs-count' }) }, probList, probEmpty);
  const probCount = probPanel.querySelector('.co-probs-count');
  let probKey = null;
  const probDurs = [];
  function paintProblems() {
    const list = data?.problems || [];
    const key = list.map((p) => `${p.id}:${p.resolved ? 1 : 0}:${p.end_ms ?? ''}`).join('|');
    const open = list.filter((p) => !p.resolved).length;
    probCount.textContent = list.length ? `${num(list.length)} · ${open ? `${num(open)} ongoing` : 'all resolved'}` : 'none';
    probEmpty.hidden = !!list.length;
    probList.hidden = !list.length;
    if (key === probKey) return;
    probKey = key;
    probDurs.length = 0;
    const conns = new Map((data?.connections || []).map((c) => [c.id, c]));
    replace(probList, list.map((p) => {
      const durEl = h('span', { class: 'co-p-dur' }, formatDuration(p.duration_ms));
      if (!p.resolved) probDurs.push([durEl, p]);
      const c = conns.get(p.connection);
      return h('li', { class: ['co-prob', !p.resolved && 'is-open'] },
        h('span', { class: 'co-p-when', title: dateTime(p.start_ms, tz) }, when(p.start_ms, tz)),
        h('span', { class: 'co-p-what' },
          c ? h('a', { class: 'co-p-conn', href: linkOf(c), onclick: (e) => onRowClick(e, e.currentTarget) }, p.label) : h('span', { class: 'co-p-conn' }, p.label),
          h('span', { class: 'co-p-msg' }, p.message || REASONS[p.code] || p.code || 'A problem')),
        h('span', { class: 'co-p-how' }, h('span', { class: 'co-k' }, p.resolved ? 'Lasted' : 'For'), durEl),
        h('span', { class: 'co-p-st' }, p.resolved
          ? h('span', { class: 'co-light', dataset: { st: 'resolved' } }, icon('check', 'ico--sm'), h('span', { class: 'co-word' }, 'Resolved'))
          : h('span', { class: 'co-light', dataset: { st: 'problem' } }, h('i', { 'aria-hidden': 'true' }), h('span', { class: 'co-word' }, 'Ongoing'))));
    }));
  }

  /* ---------- ticking: relative times in place */
  function tick() {
    const now = ctx.api.now();
    for (const r of rows.values()) {
      const c = r.c; if (!c) continue;
      r.last.textContent = agoText(c.last_ok_ms, now);
      r.next.textContent = inText(c.next_ms, now) ?? (c.status === 'off' ? 'none planned' : c.unit === 'messages' ? 'as they come' : 'none planned');
      if (r.errAt.dataset.at) r.errAt.textContent = ` · ${agoText(Number(r.errAt.dataset.at), now)}`;
    }
    for (const [el, p] of probDurs) el.textContent = formatDuration(Math.max(p.duration_ms || 0, now - p.start_ms));
    paintRead();
  }

  /* ---------- reading */
  const body = h('div', { class: 'co-body' }, totalsEl, h('div', { class: 'co-table' }, headEl, listEl));
  const mainPanel = panel({ title: 'Connections', icon: 'pulse', id: 'si-overview', cls: 'co-panel', side: readEl }, body);
  const el = h('div', { class: 'co-wrap' }, mainPanel, probPanel);

  async function load() {
    if (busy) return;
    busy = true;
    try {
      const d = await ctx.api.get('integrations/overview');
      if (!ctx.alive()) return;
      data = d; readAt = ctx.api.now(); failAt = null;
      paintTotals(); paintRows(); paintProblems(); tick();
    } catch (e) {
      if (e?.name === 'AbortError' || !ctx.alive()) return;
      if (!data) {
        // nothing to keep: say why, with a way to try again (the totals stay, at zero)
        replace(body, errorState(e, { retry: () => { replace(body, totalsEl, h('div', { class: 'co-table' }, headEl, listEl)); load(); }, home: false, compact: true }));
        probPanel.hidden = true;
      } else { failAt = ctx.api.now(); failText = errorWords(e).title.toLowerCase(); paintRead(); }
    } finally { busy = false; }
  }

  function onRowClick(e, a) {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const u = new URL(a.href, location.href);
    if (u.pathname !== location.pathname || !u.hash || !onSameRoute) return; // another page: the router goes there
    e.preventDefault();
    onSameRoute(u.hash);
  }

  ctx.every(POLL_MS, () => { if (shown && !document.hidden) load(); });
  ctx.every(1000, () => { if (shown && data) tick(); });
  ctx.on(document, 'visibilitychange', () => { if (shown && !document.hidden && readAt != null && ctx.api.now() - readAt > POLL_MS) load(); });

  const ready = load();
  return {
    el, ready,
    show(on) {
      const was = shown; shown = !!on;
      if (shown && !was && (readAt == null || ctx.api.now() - readAt > POLL_MS / 2)) load();
      if (shown) tick();
    },
  };
}

/**
 * The card on Data health (admins): the totals and a link to the overview. Returns { el, ready };
 * the totals stay a plain link when they cannot be read.
 */
export function connectionsCard(ctx) {
  const lights = h('ul', { class: 'co-card-totals', 'aria-label': 'Connections by status' });
  const el = h('section', { class: 'panel panel-pad co-card co-wrap', 'aria-label': 'Connections overview' },
    h('div', { class: 'co-card-text' },
      h('h2', { class: 'panel-title' }, icon('pulse'), 'Connections overview'),
      h('p', { class: 'note' }, 'Every connection the server keeps (the CAN Bridge of each car, Tesla, TeslaFi, local TeslaMate, the broker, map tiles, address lookup, the release check): is it live, when it last worked, its errors and problems.')),
    lights,
    h('a', { class: 'btn co-card-link', href: '/settings/integrations', cover: 'conn.overview.link' }, 'Open the overview', icon('chev-r', 'ico--sm')));
  const ready = ctx.api.get('integrations/overview').then((d) => {
    if (!ctx.alive()) return;
    replace(lights, STATUS_ORDER.filter((s) => d?.totals?.[s] || s === 'live' || s === 'problem').map((s) =>
      h('li', null, h('span', { class: 'co-light', dataset: { st: s }, title: CONN_STATUS[s][2] }, h('i', { 'aria-hidden': 'true' }),
        h('b', { class: 'co-card-n' }, num(d?.totals?.[s] ?? 0)), h('span', { class: 'co-word' }, CONN_STATUS[s][0].toLowerCase())))));
  }).catch((e) => { if (e?.name === 'AbortError') throw e; lights.hidden = true; });
  return { el, ready };
}
