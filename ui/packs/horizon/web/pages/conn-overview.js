// Connections overview: every connection the server keeps, on one page (GET integrations/overview, admin).
// Used by Settings: Integrations (its first tab, overviewTab) and Data health and storage (overviewCard).
//
// The tab reads the overview again every 10 s while it is shown and the window is visible, and updates every
// row in place (keyed by the connection's id): text changes, nothing is drawn again, and the old figures stay
// until the new ones are in. Relative times ("12 s ago", "in 30 s") tick every second against the server's
// clock; their boxes have a fixed width, so nothing beside them moves.

import { h, svg, replace } from '../shared/dom.js';
import { formatDateTime, formatDuration, formatTime } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { panel, note } from '../core/page.js';
import { emptyState, errorWords } from '../core/states.js';

const POLL_MS = 10_000;
const TICK_MS = 1_000;
const NBSP = ' ';
const DASH = '—';

export const STATUS_ORDER = ['live', 'idle', 'paused', 'off', 'problem'];
export const STATUS_WORD = { live: 'Live', idle: 'Idle', paused: 'Paused', off: 'Off', problem: 'Problem' };
const STATUS_HELP = {
  live: 'Worked within its expected pace.',
  idle: 'Quiet as expected: the car asleep or parked, nothing to send.',
  paused: 'An admin paused it.',
  off: 'Not set up or turned off: no connection is made.',
  problem: 'Its last tries failed, or nothing came when something should have.',
};
// status_reason codes in words (an unknown code is shown as its words)
const REASON = {
  car_asleep: 'asleep, as expected',
  parked: 'parked, polled gently as expected',
  not_added: 'not set up',
  turned_off: 'turned off',
  offline_mode: 'offline mode: nothing leaves the server',
  paused: 'paused by an admin',
  no_car: 'no car matched to it',
  token_refused: 'sign-in refused: sign in again',
  unreachable: 'cannot be reached',
  quiet: 'nothing has come, though something should have',
  rate_limited: 'rate limit used up for now',
  connection_refused: 'connection refused',
  tesla_unreachable: 'Tesla cannot be reached',
  log_check_failed: 'the last log failed its check',
};
const reasonText = (code) => (code ? REASON[code] || String(code).replace(/_/g, ' ') : null);
const PACE = { often: 'Often', normal: 'Normal', gentle: 'Gentle' };
const UNIT = { uploads: ['upload', 'uploads'], requests: ['request', 'requests'], messages: ['message', 'messages'] };
const unitWord = (n, unit) => { const u = UNIT[unit] || ['try', 'tries']; return n === 1 ? u[0] : u[1]; };
const n0 = (v) => (v == null ? DASH : formatNumber(v));
const pct = (v) => (v == null ? null : `${formatNumber(v, { maximumFractionDigits: 1 })}${NBSP}%`);

// links the server names that live elsewhere in this pack; links to this page open its Set up tab
function packLink(link) {
  if (!link) return null;
  const [path, hash = ''] = String(link).split('#');
  if (path === '/settings/integrations') return `/settings/integrations?tab=setup${hash ? `#${hash === 'mqtt' ? 'si-broker' : hash}` : ''}`;
  if (path === '/settings/server') {
    if (hash === 'release') return '/settings/display#release-check';
    if (hash === 'maps' || hash === 'geocoding') return '/settings/display';
  }
  return link;
}

/** "12 s ago", "in 30 s", "5 min ago", against the server's clock. */
export function rel(ms, now) {
  if (ms == null) return DASH;
  const d = now - ms;
  const s = Math.round(Math.abs(d) / 1000);
  const text = s < 60 ? `${s}${NBSP}s` : formatDuration(Math.abs(d));
  return d >= 0 ? `${text} ago` : `in ${text}`;
}
const nextText = (ms, now) => (ms == null ? 'not planned' : ms <= now ? 'due now' : rel(ms, now));

/* ------------------------------------------------------------------ pieces */

const light = (status) => h('span', { class: 'ov-light', dataset: { status }, 'aria-hidden': 'true' });

// a text slot that is updated in place
function slot(cls, text = '') { const el = h('span', { class: cls }); el.textContent = text; return el; }
function setText(el, text) { if (el.textContent !== text) el.textContent = text; }

// the last 24 hours as bars, one per hour; hours with errors are red
function sparkline(hourly, unit, tz) {
  const W = 4, GAP = 1, H = 24;
  const list = hourly || [];
  const max = Math.max(1, ...list.map((x) => x.count || 0));
  const el = svg('svg', { class: 'ov-spark-svg', viewBox: `0 0 ${Math.max(1, list.length) * (W + GAP) - GAP} ${H}`, preserveAspectRatio: 'none', role: 'img',
    'aria-label': `Last 24 hours: ${formatNumber(list.reduce((a, x) => a + (x.count || 0), 0))} ${unitWord(2, unit)}, ${formatNumber(list.reduce((a, x) => a + (x.errors || 0), 0))} errors` });
  list.forEach((x, i) => {
    const c = x.count || 0;
    const bh = c ? Math.max(2, Math.round((c / max) * H)) : 1;
    el.append(svg('rect', { x: i * (W + GAP), y: H - bh, width: W, height: bh, class: x.errors ? 'err' : c ? 'ok' : 'none' },
      svg('title', null, `${formatTime(x.hour_ms, tz)}: ${formatNumber(c)} ${unitWord(c, unit)}${x.errors ? `, ${formatNumber(x.errors)} errors` : ''}`)));
  });
  return el;
}
const sparkKey = (hourly) => (hourly || []).map((x) => `${x.hour_ms}:${x.count}:${x.errors}`).join(',');

function cell(label, ...children) {
  return h('div', { class: 'ov-cell' }, h('span', { class: 'ov-k' }, label), children);
}

/* ------------------------------------------------------------------ one connection */

function connRow(c, tz, now) {
  const word = slot('ov-word');
  const reason = slot('ov-reason');
  // a CAN Bridge row: the car's logs the server is missing and has asked the car for (hidden at 0 / null)
  const missing = slot('tag tag--warm ov-missing');
  missing.title = 'CAN logs the server should have but no longer does; it has asked the car to send them again. See Data health.';
  const status = h('span', { class: 'ov-status' }, light(c.status), word);
  const lastOk = slot('ov-v ov-time');
  const next = slot('ov-sub ov-time');
  const pace = slot('ov-v');
  const paceSub = slot('ov-sub');
  const today = slot('ov-v');
  const todaySub = slot('ov-sub');
  const day = slot('ov-v');
  const daySub = slot('ov-sub');
  const week = slot('ov-v');
  const weekSub = slot('ov-sub');
  const up = slot('ov-v');
  const upSub = slot('ov-sub');
  const spark = h('div', { class: 'ov-spark', cover: 'conn.overview.sparkline' });
  const errText = slot('ov-err-text');
  const errWhen = slot('ov-err-when ov-time');
  const err = h('p', { class: 'ov-err' }, icon('warn', 'ico--sm'), h('span', null, 'Last error ', errWhen, ': ', errText));
  const label = slot('ov-label');
  const el = h('a', { class: 'ov-row', href: packLink(c.link) || '#', dataset: { id: c.id, status: c.status } },
    h('div', { class: 'ov-cell ov-main' }, label, h('span', { class: 'ov-line', cover: 'conn.overview.status' }, status, reason, missing)),
    cell('Last good contact', lastOk, next),
    cell('Pace', pace, paceSub),
    cell('Today', today, todaySub),
    cell('Last 24 h', day, daySub),
    cell('Last 7 days', week, weekSub),
    cell('Uptime', up, upSub),
    h('div', { class: 'ov-cell ov-spark-cell' }, h('span', { class: 'ov-k' }, 'Each hour, 24 h'), spark),
    err);
  let sk = null;
  let cur = c;
  function paint(x) {
    cur = x;
    el.dataset.status = x.status;
    el.querySelector('.ov-light').dataset.status = x.status;
    const href = packLink(x.link) || '#';
    if (el.getAttribute('href') !== href) el.setAttribute('href', href);
    setText(label, x.label || x.id);
    setText(word, STATUS_WORD[x.status] || x.status);
    status.title = STATUS_HELP[x.status] || '';
    setText(reason, reasonText(x.status_reason) || (x.status === 'live' ? 'working' : ''));
    missing.hidden = !(x.missing_logs > 0);
    setText(missing, x.missing_logs > 0 ? `${formatNumber(x.missing_logs)} missing` : '');
    setText(pace, x.pace ? PACE[x.pace] || x.pace : x.pace_s != null ? 'Steady' : x.status === 'off' ? 'Off' : 'As it comes');
    setText(paceSub, x.pace_s != null ? `every ${formatDuration(x.pace_s * 1000)}` : 'no fixed wait');
    const t = x.today || {};
    setText(today, t.cap != null ? `${n0(t.count)} of ${n0(t.cap)}` : `${n0(t.count)}`);
    setText(todaySub, t.cap != null ? `${pct(t.cap ? (100 * (t.count || 0)) / t.cap : 0)} of the daily cap` : `${unitWord(t.count, x.unit)} since 0:00 UTC`);
    const per = (p) => [p?.success_pct == null ? 'no tries' : `${pct(p.success_pct)} ok`, p?.count ? `${n0(p.errors)} ${p.errors === 1 ? 'error' : 'errors'} of ${n0(p.count)}` : `no ${unitWord(2, x.unit)}`];
    const [d1, d2] = per(x.day); setText(day, d1); setText(daySub, d2);
    const [w1, w2] = per(x.week); setText(week, w1); setText(weekSub, w2);
    const u = x.uptime || {};
    setText(up, u.day_pct == null ? 'off' : `${pct(u.day_pct)} in 24 h`);
    setText(upSub, u.week_pct == null ? 'off all week' : `${pct(u.week_pct)} in 7 days`);
    up.title = 'The share of the period not inside a problem.';
    const k = sparkKey(x.hourly);
    if (k !== sk) { sk = k; replace(spark, sparkline(x.hourly, x.unit, tz)); }
    const e = x.last_error;
    err.hidden = !e;
    if (e) { setText(errText, `${e.message || e.code}${e.code ? ` (${e.code})` : ''}`); errWhen.title = formatDateTime(e.at_ms, tz, { seconds: true }); }
    tick(now);
  }
  function tick(t) {
    setText(lastOk, cur.last_ok_ms == null ? 'never' : rel(cur.last_ok_ms, t));
    lastOk.title = cur.last_ok_ms == null ? '' : formatDateTime(cur.last_ok_ms, tz, { seconds: true });
    setText(next, `next ${nextText(cur.next_ms, t)}`);
    if (cur.last_error) setText(errWhen, rel(cur.last_error.at_ms, t));
  }
  paint(c);
  return { el, paint, tick };
}

/* ------------------------------------------------------------------ problems */

function problemRow(p, tz, now) {
  const dur = slot('ov-v ov-time');
  const state = h('span', { class: 'ov-pstate' });
  const el = h('li', { class: 'ov-prob', dataset: { id: p.id } },
    h('div', { class: 'ov-cell' }, h('span', { class: 'ov-k' }, 'When'), slot('ov-v', formatDateTime(p.start_ms, tz, { now }))),
    h('div', { class: 'ov-cell' }, h('span', { class: 'ov-k' }, 'Connection'), slot('ov-v', p.label || p.connection)),
    h('div', { class: 'ov-cell ov-what' }, h('span', { class: 'ov-k' }, 'What happened'), slot('ov-v', p.message || p.code || ''), p.code ? slot('ov-sub', p.code) : null),
    h('div', { class: 'ov-cell' }, h('span', { class: 'ov-k' }, 'How long'), dur),
    h('div', { class: 'ov-cell' }, h('span', { class: 'ov-k' }, 'Now'), state));
  let cur = p;
  function paint(x) {
    cur = x;
    el.dataset.resolved = String(!!x.resolved);
    replace(state, light(x.resolved ? 'live' : 'problem'), x.resolved ? `Resolved ${x.end_ms != null ? formatTime(x.end_ms, tz) : ''}`.trim() : 'Ongoing');
  }
  function tick(t) {
    const ms = cur.resolved || cur.end_ms != null ? cur.duration_ms ?? (cur.end_ms - cur.start_ms) : Math.max(cur.duration_ms || 0, t - cur.start_ms);
    setText(dur, `${formatDuration(ms)}${cur.resolved ? '' : ' so far'}`);
  }
  paint(p); tick(now);
  return { el, paint, tick, key: () => JSON.stringify([cur.end_ms, cur.resolved, cur.message]) };
}

/* ------------------------------------------------------------------ the tab */

/**
 * The Overview tab: totals, a row per connection, and the problems of the last 7 days.
 * overviewTab(ctx, { first }) -> { el, start(), stop() }. first is the first answer (or the error).
 */
export function overviewTab(ctx, { first }) {
  const tz = ctx.tz;
  let data = first && !(first instanceof Error) ? first : null;
  const now = () => ctx.api.now();

  const totalsEl = h('div', { class: 'ov-totals', role: 'list', 'aria-label': 'Connections by status' });
  const totalSlots = {};
  for (const s of STATUS_ORDER) {
    const v = slot('ov-total-n', '0');
    totalSlots[s] = v;
    totalsEl.append(h('div', { class: 'ov-total', role: 'listitem', dataset: { status: s }, title: STATUS_HELP[s] },
      h('span', { class: 'ov-total-k' }, light(s), STATUS_WORD[s]), v));
  }
  const readEl = slot('ov-read ov-time');
  const listEl = h('div', { class: 'ov-list', role: 'list', 'aria-label': 'Connections' });
  const head = h('div', { class: 'ov-headrow', 'aria-hidden': 'true' },
    ['Connection', 'Last good contact', 'Pace', 'Today', 'Last 24 h', 'Last 7 days', 'Uptime', 'Each hour, 24 h'].map((t) => h('span', null, t)));
  const connPanel = panel({ title: 'Connections', icon: 'share', cls: 'ov-panel', cover: 'conn.overview.rows',
    side: h('span', { class: 'faint ov-side' }, 'Read ', readEl, ', again every 10 s') },
  note('Every connection the server keeps. Open a row for its settings or details. Times are relative to the server’s clock; the bars are the last 24 hours, one per hour, red where something failed.'),
  head, listEl);
  const probList = h('ol', { class: 'ov-probs' });
  const probEmpty = h('div', { class: 'ov-probs-empty' }, emptyState({ icon: 'check', title: 'No problems in the last 7 days', text: 'Every connection that was on worked as expected all week.' }));
  const probPanel = panel({ title: 'Problems in the last 7 days', icon: 'warn', cls: 'ov-prob-panel', cover: 'conn.overview.problems',
    side: h('span', { class: 'faint' }, 'Newest first') },
  note('A problem begins when a connection goes to Problem and ends when it leaves it.'),
  h('div', { class: 'ov-probhead', 'aria-hidden': 'true' }, ['When', 'Connection', 'What happened', 'How long', 'Now'].map((t) => h('span', null, t))),
  probList, probEmpty);
  const errSlot = h('div', { class: 'ov-error' });
  errSlot.hidden = true;
  const el = h('div', { class: 'ov' }, errSlot, totalsEl, connPanel, probPanel);

  const rows = new Map();
  const probs = new Map();
  function paint(d) {
    const t = now();
    for (const s of STATUS_ORDER) setText(totalSlots[s], formatNumber(d.totals?.[s] ?? 0));
    for (const s of STATUS_ORDER) totalsEl.querySelector(`[data-status="${s}"]`).dataset.zero = String(!(d.totals?.[s]));
    readMs = d.read_ms ?? t;
    setText(readEl, rel(readMs, t));
    // connections, in the server's order, updated in place
    const seen = new Set();
    let prev = null;
    for (const c of d.connections || []) {
      seen.add(c.id);
      let r = rows.get(c.id);
      if (r) r.paint(c); else { r = connRow(c, tz, t); rows.set(c.id, r); }
      const want = prev ? prev.nextSibling : listEl.firstChild;
      if (want !== r.el) listEl.insertBefore(r.el, want);
      prev = r.el;
    }
    for (const [id, r] of rows) if (!seen.has(id)) { r.el.remove(); rows.delete(id); }
    if (!rows.size && !listEl.querySelector('.empty')) listEl.append(emptyState({ icon: 'share', title: 'No connections', text: 'Nothing is set up that talks to another system.' }));
    else if (rows.size) listEl.querySelector('.empty')?.remove();
    // problems, newest first
    const pseen = new Set();
    prev = null;
    for (const p of d.problems || []) {
      pseen.add(p.id);
      let r = probs.get(p.id);
      if (r) { const k = r.key(); r.paint(p); if (k !== r.key()) r.tick(t); } else { r = problemRow(p, tz, t); probs.set(p.id, r); }
      const want = prev ? prev.nextSibling : probList.firstChild;
      if (want !== r.el) probList.insertBefore(r.el, want);
      prev = r.el;
    }
    for (const [id, r] of probs) if (!pseen.has(id)) { r.el.remove(); probs.delete(id); }
    probEmpty.hidden = probs.size > 0;
    probList.hidden = probs.size === 0;
    probPanel.querySelector('.ov-probhead').hidden = probs.size === 0;
  }
  let readMs = null;
  function tick() {
    const t = now();
    if (readMs != null) setText(readEl, rel(readMs, t));
    for (const r of rows.values()) r.tick(t);
    for (const r of probs.values()) r.tick(t);
  }
  function showError(e) {
    // the figures on screen stay; a line says the last read failed
    errSlot.hidden = false;
    const w = errorWords(e);
    replace(errSlot, note(icon('warn', 'ico--sm'), ` Could not read the overview again: ${w.text || w.title}. The figures below are from the last good read.`));
  }

  if (data) paint(data);
  else {
    const w = errorWords(first);
    replace(errSlot, emptyState({ icon: 'warn', warn: true, title: w.title || 'The overview could not be read', text: w.text }));
    errSlot.hidden = false;
    totalsEl.hidden = true; connPanel.hidden = true; probPanel.hidden = true;
  }

  let on = false, busy = false;
  async function poll() {
    if (!on || busy || document.visibilityState === 'hidden') return;
    busy = true;
    try {
      const d = await ctx.api.get('integrations/overview');
      if (!ctx.alive()) return;
      data = d;
      if (errSlot.hidden === false && totalsEl.hidden === false) { errSlot.hidden = true; replace(errSlot); }
      if (totalsEl.hidden) { totalsEl.hidden = false; connPanel.hidden = false; probPanel.hidden = false; errSlot.hidden = true; replace(errSlot); }
      paint(d);
    } catch (e) {
      if (e?.name === 'AbortError' || !ctx.alive()) return;
      if (data) showError(e);
    } finally { busy = false; }
  }
  ctx.every(POLL_MS, poll);
  ctx.every(TICK_MS, () => { if (on) tick(); });

  return {
    el,
    start() { if (!on) { on = true; tick(); } },
    stop() { on = false; },
    poll,
  };
}

/** The Overview tab for an account that is not an admin: what it would show, and who sees it. */
export function lockedOverview() {
  const el = h('div', { class: 'ov', cover: ['conn.overview.rows', 'conn.overview.status', 'conn.overview.sparkline', 'conn.overview.problems'] },
    panel({ title: 'Connections', icon: 'share', cls: 'ov-panel' },
      emptyState({ icon: 'lock', title: 'For admins only', text: 'The overview of every connection (live or not, when it last worked, its pace, success and errors per hour, and the problems of the last 7 days) is shown to admins. The Set up tab shows what your account may read.' })));
  return { el, start() {}, stop() {}, poll() {} };
}

/* ------------------------------------------------------------------ the Data health card */

/**
 * A card that links to the Overview tab, with the totals (admins; others get a line that says who sees it).
 * overviewCard(ctx, admin) -> Promise<element>
 */
export async function overviewCard(ctx, admin) {
  if (!admin) {
    return h('div', { class: 'ov-card ov-card--locked', cover: 'conn.overview.link' },
      h('span', { class: 'ov-card-icon' }, icon('lock')),
      h('span', { class: 'ov-card-body' },
        h('span', { class: 'ov-card-title' }, 'Connections overview'),
        h('span', { class: 'ov-card-text' }, 'Every input and output of the server, on Settings: Integrations, Overview. An admin sees it.')));
  }
  const d = await ctx.api.get('integrations/overview').catch((e) => (e?.name === 'AbortError' ? Promise.reject(e) : null));
  const totals = d?.totals || null;
  const parts = totals ? STATUS_ORDER.filter((s) => totals[s]).map((s) => h('span', { class: 'ov-chip', dataset: { status: s } }, light(s), `${formatNumber(totals[s])} ${STATUS_WORD[s].toLowerCase()}`)) : [];
  return h('a', { class: 'ov-card', href: '/settings/integrations?tab=overview', cover: 'conn.overview.link' },
    h('span', { class: 'ov-card-icon' }, icon('share')),
    h('span', { class: 'ov-card-body' },
      h('span', { class: 'ov-card-title' }, 'Connections overview'),
      h('span', { class: 'ov-card-text' }, totals
        ? (totals.problem ? `${formatNumber(totals.problem)} ${totals.problem === 1 ? 'connection has' : 'connections have'} a problem. ` : 'No connection has a problem. ')
          + 'Every input and output: when it last worked, how busy it is and what went wrong this week.'
        : 'Every input and output: when it last worked, how busy it is and what went wrong this week.'),
      parts.length ? h('span', { class: 'ov-chips' }, parts) : null),
    icon('chev-r', 'ico--sm'));
}
