// Sentry Mate, "Pebble" UI pack: the Connections overview, the first tab of Settings: Integrations.
//
// Every connection the server keeps, on one page: the totals (live, idle, paused, off, problem), then one rounded
// card per connection (its status light and word, why, the last good contact and the next one, its pace, today
// against its cap, the last 24 h and 7 days, its uptime, the last error and a 24 h bar sparkline with the error
// hours red); a tap on a card opens its details. Then the problems of the last 7 days.
//
// Live without jumps: the cards are built once with every line in place (a value that is missing says so in
// words), a poll every 10 s writes the new text into them, and the relative times tick every second. A failed
// poll keeps what is shown. Admins only (the server answers 403 to anyone else).
// Data: GET integrations/overview.

import { h, svg } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { formatDateTime, formatTime, formatDuration, formatRelative } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { format } from '../shared/units.js';
import { tile, chip, note, icon } from '../core/ui.js';
import { errorWords } from '../core/states.js';

const POLL_MS = 10000;

/** Status -> [word, tone, what it means]. */
export const STATUS = Object.freeze({
  live: ['Live', 'teal', 'Worked within its expected pace'],
  idle: ['Idle', 'sky', 'Quiet, as expected'],
  paused: ['Paused', 'sun', 'An admin paused it'],
  off: ['Off', 'muted', 'Not set up or turned off: no connection is made'],
  problem: ['Problem', 'bad', 'Its last tries failed, or nothing came when something should have'],
});
export const STATUS_ORDER = Object.freeze(['live', 'idle', 'paused', 'off', 'problem']);

const REASONS = {
  car_asleep: 'The car is asleep, as expected',
  parked: 'Parked: quiet, as expected',
  not_added: 'Not set up yet',
  offline_mode: 'Off: the server runs in offline mode',
  turned_off: 'Turned off',
  paused: 'Paused by an admin',
  token_refused: 'The token was refused',
  sign_in_needed: 'Needs a new sign-in',
  unreachable: 'Could not be reached',
  tesla_unreachable: 'Tesla did not answer',
  quiet: 'Nothing came when something should have',
  mqtt_quiet: 'No messages came when some should have',
  rate_limited: 'Slowed down: too many requests',
  connection_refused: 'The connection was refused',
  auth_failed: 'The user name or password was refused',
  pin_mismatch: 'The certificate did not match its pin',
  catchup_failed: 'The database catch-up failed',
  log_check_failed: 'The last log failed its check',
  gap: 'A gap in what came',
};
const cap1 = (s) => s.charAt(0).toUpperCase() + s.slice(1);
const reasonText = (c) => (c.status_reason ? REASONS[c.status_reason] || cap1(String(c.status_reason).replace(/_/g, ' '))
  : c.status === 'live' ? 'Working within its pace' : (STATUS[c.status] || STATUS.off)[2]);
const UNIT = { uploads: ['upload', 'uploads'], requests: ['request', 'requests'], messages: ['message', 'messages'] };
const unitWord = (c, n) => { const u = UNIT[c.unit] || UNIT.requests; return n === 1 ? u[0] : u[1]; };
const n0 = (n) => formatNumber(n ?? 0);
const pct = (v) => (v == null ? null : format(v, 'percent', { digits: v >= 99.95 || v === 0 ? 0 : 1 }));
const PACE = { often: 'Often', normal: 'Normal', gentle: 'Gentle' };

/** '12 s ago', 'in 30 s', then the shared words from a minute on. */
export function rel(ms, now = api.now()) {
  if (ms == null) return null;
  const d = now - ms;
  if (Math.abs(d) < 60000) {
    const s = Math.round(Math.abs(d) / 1000);
    if (s < 1) return 'now';
    return d < 0 ? `in ${formatDuration(s * 1000)}` : `${formatDuration(s * 1000)} ago`;
  }
  return formatRelative(ms, now);
}

/** The words of the totals: '6 live · 1 idle · 1 problem' (the zero ones left out). */
export function totalsLine(t) {
  if (!t) return '';
  return STATUS_ORDER.filter((k) => t[k]).map((k) => `${n0(t[k])} ${k}`).join(' · ') || 'No connection yet';
}

/** A span whose text is a relative time that ticks (data-rel = ms, data-none = the words when there is none). */
function setRel(el, ms, none, { future = false } = {}) {
  if (future) el.dataset.future = '1';
  if (ms == null) { delete el.dataset.rel; setText(el, none); } else { el.dataset.rel = String(ms); setText(el, relText(ms, future)); }
}
// a planned time already past says 'due now' until the next reading brings the next one
const relText = (ms, future = false) => { const now = api.now(); return future && ms <= now ? 'due now' : rel(ms, now); };
function setText(el, text) { if (el.textContent !== text) el.textContent = text; }

/* ---------------------------------------------------------------- the sparkline */

const SPARK_W = 240, SPARK_H = 40, BAR = 10;
/** 24 bars, one per hour, oldest first; hours with errors red. Drawn as SVG (attributes, no styles). */
function sparkline(c, tz) {
  const hours = (c.hourly || []).slice(-24);
  const max = Math.max(1, ...hours.map((x) => x.count || 0));
  const s = svg('svg', { class: 'cn-spark', viewBox: `0 0 ${SPARK_W} ${SPARK_H}`, preserveAspectRatio: 'none', role: 'img',
    'aria-label': `${c.label}: ${n0(c.day && c.day.count)} ${unitWord(c, c.day && c.day.count)} in the last 24 hours, ${n0(c.day && c.day.errors)} errors` });
  const off = 24 - hours.length;
  hours.forEach((x, i) => {
    const n = x.count || 0;
    const hh = n ? Math.max(3, Math.round((n / max) * (SPARK_H - 2))) : 1;
    const r = svg('rect', { class: ['cn-bar', { 'is-err': (x.errors || 0) > 0, 'is-zero': !n }], x: (off + i) * BAR + 1, y: SPARK_H - hh, width: BAR - 2, height: hh, rx: 1.5 });
    r.append(svg('title', null, `${formatTime(x.hour_ms, tz)}: ${n0(n)} ${unitWord(c, n)}${x.errors ? `, ${n0(x.errors)} ${x.errors === 1 ? 'error' : 'errors'}` : ''}`));
    s.append(r);
  });
  return s;
}

/* ---------------------------------------------------------------- one card */

function card(c, tz) {
  const refs = {};
  const dd = (k, cls = null) => (refs[k] = h('dd', { class: cls }));
  const row = (label, k, cls) => h('div', { class: 'cn-fact' }, h('dt', null, label), dd(k, cls));
  refs.light = h('span', { class: 'cn-light', 'aria-hidden': 'true' });
  refs.word = h('span', { class: 'cn-word' });
  refs.reason = h('p', { class: 'cn-reason' });
  refs.lastOk = h('dd');
  refs.next = h('dd');
  refs.err = h('p', { class: 'cn-err-text' });
  refs.errAt = h('span', { class: 'cn-err-at' });
  refs.spark = h('div', { class: 'cn-spark-box', cover: 'conn.overview.sparkline' });
  refs.sparkFrom = h('span');
  refs.sparkTo = h('span', null, 'now');
  const el = tile({ w: 2, href: c.link || null, cls: 'cn-card', cover: 'conn.overview.rows' },
    h('div', { class: 'cn-top' },
      h('h3', { class: 'cn-label' }, c.label),
      h('span', { class: 'cn-status', cover: 'conn.overview.status' }, refs.light, refs.word),
      c.link ? icon('chevronRight', 'cn-go') : null),
    refs.reason,
    h('dl', { class: 'cn-facts' },
      h('div', { class: 'cn-fact' }, h('dt', null, 'Last good contact'), refs.lastOk),
      h('div', { class: 'cn-fact' }, h('dt', null, 'Next'), refs.next),
      row('Pace', 'pace'),
      row('Today', 'today'),
      row('Last 24 h', 'day'),
      row('Last 7 days', 'week'),
      row('Uptime 24 h / 7 d', 'uptime')),
    h('div', { class: 'cn-err' }, h('div', { class: 'cn-err-head' }, h('span', { class: 'cn-err-k' }, 'Last error'), refs.errAt), refs.err),
    refs.spark,
    h('div', { class: 'cn-spark-axis', 'aria-hidden': 'true' }, refs.sparkFrom, refs.sparkTo));
  let sparkKey = null;
  function set(x) {
    const st = STATUS[x.status] ? x.status : 'off';
    for (const k of STATUS_ORDER) el.classList.toggle(`is-${k}`, k === st);
    setText(refs.word, STATUS[st][0]);
    setText(refs.reason, reasonText(x));
    setRel(refs.lastOk, x.last_ok_ms, x.status === 'off' ? 'None: it is off' : 'None in 7 days');
    setRel(refs.next, x.next_ms, x.status === 'off' || x.status === 'paused' ? 'None planned' : x.unit === 'messages' ? 'When it has news' : 'When there is something', { future: true });
    setText(refs.pace, x.pace ? `${PACE[x.pace] || cap1(x.pace)}${x.pace_s != null ? `, every ${formatDuration(x.pace_s * 1000)}` : ''}`
      : x.pace_s != null ? `Every ${formatDuration(x.pace_s * 1000)}` : 'No fixed pace');
    const t = x.today || {};
    setText(refs.today, t.cap != null ? `${n0(t.count)} of ${n0(t.cap)} ${unitWord(x, t.cap)} cap` : `${n0(t.count)} ${unitWord(x, t.count)}`);
    const period = (p) => (!p || !p.count ? 'No tries' : `${pct(p.success_pct)} ok · ${n0(p.errors)} ${p.errors === 1 ? 'error' : 'errors'}`);
    setText(refs.day, period(x.day));
    setText(refs.week, period(x.week));
    const u = x.uptime || {};
    setText(refs.uptime, u.day_pct == null && u.week_pct == null ? 'Off all along' : `${pct(u.day_pct) ?? 'off'} / ${pct(u.week_pct) ?? 'off'}`);
    const e = x.last_error;
    el.classList.toggle('has-err', !!e);
    setText(refs.err, e ? e.message || cap1(String(e.code || 'error').replace(/_/g, ' ')) : 'None kept.');
    setText(refs.errAt, e && e.at_ms != null ? formatDateTime(e.at_ms, tz, { now: api.now() }) : '');
    const hours = x.hourly || [];
    const key = JSON.stringify(hours);
    if (key !== sparkKey) { sparkKey = key; refs.spark.replaceChildren(sparkline(x, tz)); setText(refs.sparkFrom, hours.length ? formatTime(hours[Math.max(0, hours.length - 24)].hour_ms, tz) : '24 h ago'); }
  }
  set(c);
  return { el, set };
}

/* ---------------------------------------------------------------- the totals and the problems */

function totalsTile() {
  const nums = {};
  const el = tile({ w: 'full', cls: 'cn-totals', title: 'Connections', icon: 'pulse', cover: 'conn.overview.status' },
    h('ul', { class: 'cn-tot' }, STATUS_ORDER.map((k) => h('li', { class: ['cn-tot-i', `is-${k}`] },
      nums[k] = h('span', { class: 'cn-tot-n' }, '0'),
      h('span', { class: 'cn-tot-k' }, h('span', { class: 'cn-light', 'aria-hidden': 'true' }), STATUS[k][0])))));
  return { el, set: (t) => { for (const k of STATUS_ORDER) setText(nums[k], n0(t && t[k])); } };
}

function problemsTile(tz) {
  const list = h('ul', { class: 'cn-probs' });
  const el = tile({ w: 'full', title: 'Problems in the last 7 days', icon: 'warning', cls: 'cn-probs-tile', cover: 'conn.overview.problems' },
    h('div', { class: 'cn-prob cn-prob-head', 'aria-hidden': 'true' }, h('span', null, 'When'), h('span', null, 'Connection'), h('span', null, 'What happened'), h('span', null, 'How long'), h('span', null, 'Now')),
    list,
    note('Each began when a connection went to problem and ended when it left it.'));
  let key = null;
  const rows = new Map();
  function set(probs) {
    const k = (probs || []).map((p) => `${p.id}|${p.resolved}`).join(',');
    if (k !== key) {
      key = k;
      rows.clear();
      list.replaceChildren(...(probs || []).map((p) => {
        const dur = h('span', { class: 'cn-prob-dur' });
        rows.set(p.id, dur);
        return h('li', { class: ['cn-prob', p.resolved ? 'is-done' : 'is-open'] },
          h('span', { class: 'cn-prob-at' }, formatDateTime(p.start_ms, tz, { now: api.now() })),
          h('span', { class: 'cn-prob-conn' }, p.label || p.connection),
          h('span', { class: 'cn-prob-what' }, p.message || cap1(String(p.code || 'problem').replace(/_/g, ' '))),
          dur,
          h('span', { class: 'cn-prob-st' }, chip(p.resolved ? 'Resolved' : 'Ongoing', { tone: p.resolved ? 'teal' : 'bad' })));
      }));
      if (!(probs || []).length) list.append(h('li', { class: 'cn-prob-none' }, 'No problem in the last 7 days: every connection kept working.'));
    }
    for (const p of probs || []) { const d = rows.get(p.id); if (d) setText(d, p.resolved ? formatDuration(p.duration_ms) : `${formatDuration(p.duration_ms)} so far`); }
  }
  return { el, set };
}

/* ---------------------------------------------------------------- the whole overview */

/**
 * The overview for Settings: Integrations. Returns { el, load(), start() }: el is a display: contents box for the
 * grid; load() reads the first answer (call it before the page is ready, so the cards go in filled); start()
 * begins the poll and the ticking times.
 */
export function connectionsOverview(ctx) {
  const el = h('div', { class: 'contents' });
  const tz = ctx.tz;
  let data = null, err = null;
  let cards = new Map(), idsKey = null, totals = null, probs = null, grid = null, staleNote = null;

  function build() {
    if (!ctx.admin) {
      el.replaceChildren(tile({ w: 'full', title: 'Connections', icon: 'lock', cover: ['conn.overview.rows', 'conn.overview.status', 'conn.overview.sparkline', 'conn.overview.problems'] },
        note('Every connection the server keeps (the CAN Bridge of each car, the Tesla API, TeslaFi, local TeslaMate, the MQTT broker, map tiles, address lookup and the release check), whether it is live and what went wrong: shown to admins only.')));
      return;
    }
    if (!data) {
      const w = errorWords(err);
      el.replaceChildren(tile({ w: 'full', title: 'Connections', icon: 'warning', cover: ['conn.overview.rows', 'conn.overview.status'] },
        h('p', { class: 'cn-reason' }, w.title), note(`${w.text} This page asks again every 10 seconds.`)));
      return;
    }
    totals = totalsTile();
    probs = problemsTile(tz);
    cards = new Map();
    for (const c of data.connections || []) cards.set(c.id, card(c, tz));
    idsKey = (data.connections || []).map((c) => c.id).join(',');
    staleNote = h('p', { class: 'note cn-read' });
    el.replaceChildren(
      totals.el,
      ...[...cards.values()].map((x) => x.el),
      probs.el,
      h('div', { class: 'w-full' }, staleNote));
    paint();
  }

  function paint() {
    totals.set(data.totals);
    for (const c of data.connections || []) cards.get(c.id)?.set(c);
    probs.set(data.problems);
    setText(staleNote, err ? `Could not read the newest state (${errorWords(err).title.toLowerCase()}); shown as read at ${formatTime(data.read_ms, tz, { seconds: true })}.`
      : `Read at ${formatTime(data.read_ms ?? api.now(), tz, { seconds: true })}; updated every 10 seconds.`);
  }

  async function read() {
    try { const d = await ctx.api.get('integrations/overview'); err = null; return d; } catch (e) { if (api.isAbort(e)) throw e; err = e; return null; }
  }

  async function load() {
    if (!ctx.admin) { build(); return; }
    data = await read();
    build();
  }

  async function poll() {
    if (!ctx.admin || document.hidden) return;
    let d;
    try { d = await read(); } catch { return; }
    if (!ctx.alive()) return;
    if (!d) { if (data) paint(); return; }
    const had = !!data;
    data = d;
    const k = (d.connections || []).map((c) => c.id).join(',');
    if (!had || k !== idsKey) build(); else paint();
  }

  function tick() {
    for (const x of el.querySelectorAll('[data-rel]')) setText(x, relText(Number(x.dataset.rel), x.dataset.future === '1'));
  }

  function start() {
    if (!ctx.admin) return;
    ctx.every(POLL_MS, poll);
    ctx.every(1000, tick);
  }
  return { el, load, start, totals: () => (data && data.totals) || null };
}
