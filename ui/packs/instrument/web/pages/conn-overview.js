// Settings: Integrations, the Overview tab: every connection the server keeps on one panel. Is it live,
// when did it last work, when is the next poll, how busy is it today, how often did it work in the last 24 h
// and 7 days, what went wrong last, and its last 24 hours hour by hour. Then the problems of the last 7 days.
// Also the small card on Data health that leads here (overviewCard).
//
// Admins only (the API answers 403 to anyone else). Follows `connections` events (live events, polling every
// 10 s only while the live stream is down); every text is changed in place, so the rows never go away and come
// back, and the times ("12 s ago", "in 30 s") tick each second.
//
// API: GET integrations/overview (at the start and after a reconnect), then `connections` events.

import { h, svg, replace } from '../shared/dom.js';
import { scrollBox } from '../shared/scrollbox.js';
import { buildUrl } from '../shared/state.js';
import { formatNumber } from '../shared/i18n.js';
import { formatDuration } from '../shared/time.js';
import { icon } from '../core/icons.js';
import { panel, note } from '../core/page.js';
import { errorWords } from '../core/states.js';
import { dateTime, tzName, DASH } from '../core/fmt.js';
import { follow } from '../shared/live.js';
import { morphChildren } from '../core/morph.js';

const PATH = 'integrations/overview';
// how often the overview is read while the live stream is down
const POLL_MS = 10_000;
const NBSP = ' ';
// a connection belongs to no car: its times are on this computer's clock, with the zone named
const LOCAL = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';

/* ---------------------------------------------------------------------- words */

// status: [word, what it means]
export const STATUS = {
  live: ['Live', 'Worked within its expected pace.'],
  idle: ['Idle', 'Quiet, as expected: the car is asleep or parked, or there is nothing to send.'],
  paused: ['Paused', 'An admin paused it: nothing is asked meanwhile.'],
  off: ['Off', 'Not set up or turned off: no connection is made.'],
  problem: ['Problem', 'Its last tries failed, or nothing came when something should have.'],
};
const ORDER = ['live', 'idle', 'paused', 'off', 'problem'];

const REASON = {
  car_asleep: 'Car asleep, as expected',
  parked: 'Car parked, nothing new',
  not_added: 'Not set up',
  turned_off: 'Turned off',
  offline_mode: 'Offline mode: no lookups',
  paused: 'Paused by an admin',
  token_refused: 'Token refused: give a new one',
  sign_in_needed: 'Sign in again',
  unreachable: 'Could not be reached',
  quiet: 'Nothing came when it should have',
  connection_refused: 'Connection refused',
  auth_failed: 'Sign-in refused',
  pin_mismatch: 'Certificate does not match the pin',
  log_check_failed: 'The last log failed its check',
  tesla_unreachable: 'Tesla did not answer',
  rate_limited: 'Asked to slow down',
};
const reasonText = (c) => {
  if (c.status_reason) return REASON[c.status_reason] || cap(String(c.status_reason).replace(/_/g, ' '));
  return { live: 'Working', idle: 'Quiet, as expected', paused: 'Paused by an admin', off: 'Not set up', problem: 'Failing' }[c.status] || '';
};
const cap = (s) => (s ? s[0].toUpperCase() + s.slice(1) : s);
const UNIT = { uploads: ['upload', 'uploads'], requests: ['request', 'requests'], messages: ['message', 'messages'] };
const unitWord = (c, n) => { const u = UNIT[c.unit] || ['try', 'tries']; return n === 1 ? u[0] : u[1]; };
// what the errors of a CAN Bridge row were, as `by_kind` names them (the logs the server left out of the car's history)
const KIND = {
  could_not_sign: ['could not be signed', 'The CAN Bridge could not sign these logs (its own note on each says why); an admin may choose to use them.'],
  not_signed: ['without a signature', 'Logs of a paired car that came without a signature; an admin may choose to use them.'],
  failed_check: ['failed their check', 'Logs whose signature, hash, chain or name did not check out, so their data is not used.'],
  other_vin: ['of another car', 'Logs that name another car\'s VIN than this car\'s, so they are left out.'],
  unreadable: ['unreadable', 'Logs that could not be read (a damaged file).'],
  unknown: ['not named', 'Errors counted before the server kept the reason.'],
};
const kindWord = (k) => KIND[k]?.[0] || String(k).replace(/_/g, ' ');
const n0 = (v) => (v == null ? DASH : formatNumber(Math.round(v)));
const pct = (v) => (v == null ? DASH : `${formatNumber(v, { maximumFractionDigits: v >= 99.95 || v === 0 ? 0 : 1 })}${NBSP}%`);
const at = (ms) => (ms == null ? DASH : dateTime(ms, LOCAL));
const zone = () => tzName(LOCAL);

/** A time against now, to the second while it is near: "12 s ago", "in 30 s", "4 min ago", "2 h 05 min ago". */
export function rel(ms, now) {
  if (ms == null || now == null) return DASH;
  const d = ms - now;
  const s = Math.round(Math.abs(d) / 1000);
  const span = s < 60 ? `${s}${NBSP}s` : s < 3600 ? `${Math.round(s / 60)}${NBSP}min` : formatDuration(s * 1000);
  return d > 0 ? `in ${span}` : `${span} ago`;
}
// the next poll: "in 30 s"; once its time has come, "due now" (the server polls, this page only reads)
const nextText = (c, now) => {
  if (c.next_ms == null) return c.status === 'off' ? 'Nothing planned' : c.unit === 'messages' ? 'As they come' : 'None planned';
  return c.next_ms <= now ? 'Next: due now' : `Next ${rel(c.next_ms, now)}`;
};
const paceText = (c) => {
  const every = c.pace_s != null ? `every ${formatDuration(c.pace_s * 1000)}` : null;
  if (c.pace) return `${cap(c.pace)} pace${every ? `, ${every}` : ''}`;
  if (every) return cap(every);
  return c.unit === 'messages' ? 'As messages come' : 'No fixed pace';
};

/* ---------------------------------------------------------------------- pieces */

const light = (status) => h('span', { class: 'co-light', 'aria-hidden': 'true', dataset: { status } });
// one figure: the label (shown over it on cards), the value, a line under it
function cell(cls, label) {
  const v = h('span', { class: 'co-v' }), s = h('span', { class: 'co-s' });
  return { el: h('div', { class: ['co-c', cls] }, h('span', { class: 'co-k' }, label), v, s), v, s };
}
const setText = (el, text) => { const t = text ?? ''; if (el.textContent !== t) el.textContent = t; };
const setAttr = (el, k, v) => { const s = String(v); if (el.getAttribute(k) !== s) el.setAttribute(k, s); };

// the 24 hours as bars: height by count (against the row's busiest hour), an hour with errors in red
const SP_W = 4, SP_H = 24;
function spark() {
  const bars = Array.from({ length: 24 }, (_, i) => svg('rect', { x: i * SP_W + 0.5, y: SP_H - 1, width: SP_W - 1, height: 1, class: 'co-sp-zero' }));
  const title = svg('title');
  const el = svg('svg', { class: 'co-spark', viewBox: `0 0 ${24 * SP_W} ${SP_H}`, preserveAspectRatio: 'none', role: 'img', cover: 'conn.overview.sparkline' }, title, bars);
  function paint(c) {
    const hourly = (c.hourly || []).slice(-24);
    while (hourly.length < 24) hourly.unshift({ count: 0, errors: 0 });
    const max = Math.max(1, ...hourly.map((x) => x.count || 0));
    let errHours = 0;
    hourly.forEach((x, i) => {
      const r = bars[i];
      const n = x.count || 0, e = x.errors || 0;
      if (e) errHours++;
      const hgt = e ? Math.max(8, (n / max) * SP_H) : n ? Math.max(2, (n / max) * SP_H) : 1;
      setAttr(r, 'y', +(SP_H - hgt).toFixed(2));
      setAttr(r, 'height', +hgt.toFixed(2));
      setAttr(r, 'class', e ? 'co-sp-err' : n ? 'co-sp-bar' : 'co-sp-zero');
    });
    const words = `Last 24 hours, hour by hour: ${n0(c.day?.count)} ${unitWord(c, c.day?.count)}, ${errHours ? `errors in ${errHours} ${errHours === 1 ? 'hour' : 'hours'}` : 'no errors'}.`;
    setText(title, words);
    setAttr(el, 'aria-label', words);
    return errHours;
  }
  return { el, paint };
}

/* ---------------------------------------------------------------------- one connection */

function connRow(onLink) {
  let c = null;
  const st = h('div', { class: 'co-c co-st' });
  const lt = light('off'), word = h('span', { class: 'co-word' }), reason = h('span', { class: 'co-s co-reason' });
  st.append(h('span', { class: 'co-k' }, 'Status'), h('span', { class: 'co-v co-stline' }, lt, word), reason);
  const label = h('span', { class: 'co-label' }), pace = h('span', { class: 'co-s' }), err = h('span', { class: 'co-err' });
  // CAN Bridge rows: logs the server lost and asked the car for again (a small warm tag, only when there are some)
  const miss = h('span', { class: 'tag tag--warm co-miss', hidden: true });
  const name = h('div', { class: 'co-c co-name' }, h('span', { class: 'co-nameline' }, label, miss), pace, err);
  const contact = cell('co-contact', 'Last contact');
  const today = cell('co-today', 'Today');
  const bar = h('span', { class: 'bar co-bar', 'aria-hidden': 'true' }, h('i'));
  today.el.insertBefore(bar, today.s);
  const ok = cell('co-ok', 'Worked 24 h / 7 d');
  // what the errors were, one line each (CAN Bridge rows), then what was not an error; a kind with a page opens it
  const kinds = h('span', { class: 'co-kinds', hidden: true });
  ok.el.append(kinds);
  let kindsKey = '';
  const up = cell('co-up', 'Uptime 24 h / 7 d');
  const sp = spark();
  const spc = h('div', { class: 'co-c co-spc' }, h('span', { class: 'co-k' }, 'Last 24 h'), sp.el, h('span', { class: 'co-s co-sp-axis', 'aria-hidden': 'true' }, h('span', null, '24 h ago'), h('span', null, 'now')));
  // a click on a kind of error opens its own page: the router follows the link's address, so it is set for that click
  const open = (e) => {
    const k = e.target.closest?.('.co-kind[data-link]');
    if (k) {
      setAttr(el, 'href', k.dataset.link);
      setTimeout(() => setAttr(el, 'href', c?.link || '#'), 0);
    }
    onLink(e, k?.dataset.link || c?.link);
  };
  const el = h('a', { class: 'co-row', href: '#', onclick: open }, st, name, contact.el, today.el, ok.el, up.el, spc);

  function paint(next) {
    c = next;
    setAttr(el, 'href', c.link || '#');
    el.dataset.status = c.status;
    lt.dataset.status = c.status;
    setText(word, STATUS[c.status]?.[0] || cap(c.status));
    setText(reason, reasonText(c));
    setAttr(st, 'title', STATUS[c.status]?.[1] || '');
    setText(label, c.label);
    const nMiss = c.kind === 'smo' && c.missing_logs > 0 ? c.missing_logs : 0;
    miss.hidden = !nMiss;
    setText(miss, nMiss ? `${n0(nMiss)} missing` : '');
    setAttr(miss, 'title', nMiss ? `${n0(nMiss)} ${nMiss === 1 ? 'log is' : 'logs are'} missing on the server; the car was asked to send ${nMiss === 1 ? 'it' : 'them'} again.` : '');
    setText(pace, `${cap(UNIT[c.unit]?.[1] || 'tries')} · ${paceText(c)}`);
    const capN = c.today?.cap;
    setText(today.v, capN != null ? `${n0(c.today?.count)} of ${n0(capN)}` : n0(c.today?.count));
    setText(today.s, capN != null ? `${unitWord(c, 2)}; cap per UTC day` : `${unitWord(c, c.today?.count)} since 0:00 UTC`);
    bar.hidden = capN == null;
    if (capN != null) {
      const f = Math.min(1, (c.today?.count || 0) / Math.max(1, capN));
      bar.style.setProperty('--w', `${(f * 100).toFixed(1)}%`);
      bar.classList.toggle('bar--warm', f >= 0.8);
    }
    const d = c.day || {}, w = c.week || {};
    setText(ok.v, d.success_pct == null && w.success_pct == null ? 'No tries' : `${pct(d.success_pct)} / ${pct(w.success_pct)}`);
    setText(ok.s, !d.errors && !w.errors ? 'No errors' : `Errors ${n0(d.errors)} / ${n0(w.errors)}`);
    paintKinds(d, w);
    setAttr(ok.el, 'title', `Last 24 h: ${n0(d.ok)} of ${n0(d.count)} worked, ${n0(d.errors)} errors${d.expected ? `, ${n0(d.expected)} asked of the car again (not errors)` : ''}. Last 7 days: ${n0(w.ok)} of ${n0(w.count)}, ${n0(w.errors)} errors${w.expected ? `, ${n0(w.expected)} asked of the car again` : ''}.`);
    const u = c.uptime || {};
    setText(up.v, u.day_pct == null && u.week_pct == null ? DASH : `${pct(u.day_pct)} / ${pct(u.week_pct)}`);
    setText(up.s, u.week_pct == null ? 'Off all week' : 'Not in a problem');
    setAttr(up.el, 'title', 'The share of the last 24 h and 7 days not inside a problem.');
    sp.paint(c);
  }
  // the errors by kind (24 h / 7 d, 7 d alone when the day has none), then the logs the server asked the car for again
  function paintKinds(d, w) {
    const day = d.by_kind || [], week = w.by_kind || [];
    const names = [...new Set([...day, ...week].map((x) => x.kind))];
    const lines = names.map((k) => {
      const a = day.find((x) => x.kind === k), b = week.find((x) => x.kind === k);
      return { k, a: a?.count || 0, b: b?.count || 0, link: (a || b).link };
    });
    const again = [d.expected || 0, w.expected || 0];
    const key = JSON.stringify([lines, again]);
    if (key === kindsKey) return;
    kindsKey = key;
    const rows = lines.map((x) => h('span', { class: ['co-kind', x.link && 'is-link'], dataset: x.link ? { link: x.link } : {}, title: `${KIND[x.k]?.[1] || ''}${x.link ? ' Click to see them.' : ''}`.trim() },
      `${n0(x.a)} / ${n0(x.b)} ${kindWord(x.k)}`));
    if (again[0] || again[1]) rows.push(h('span', { class: 'co-kind co-kind--ok', title: 'The server lost these logs and asked the car to send them again: not errors, and not counted as tries.' }, `${n0(again[0])} / ${n0(again[1])} asked of the car again`));
    kinds.replaceChildren(...rows);
    kinds.hidden = !rows.length;
  }
  function tick(now = null) {
    if (!c) return;
    const t = now ?? Date.now();
    setText(contact.v, c.last_ok_ms == null ? 'Never' : rel(c.last_ok_ms, t));
    setText(contact.s, nextText(c, t));
    setAttr(contact.el, 'title', c.last_ok_ms == null ? 'No good contact kept.' : `${at(c.last_ok_ms)} ${zone()}`);
    setText(err, c.last_error ? `Last error ${rel(c.last_error.at_ms, t)}: ${c.last_error.message || c.last_error.code}` : 'No error kept');
    err.classList.toggle('is-none', !c.last_error);
    setAttr(err, 'title', c.last_error ? `${at(c.last_error.at_ms)} ${zone()} (${c.last_error.code})` : '');
  }
  return { el, paint, tick };
}

/* ---------------------------------------------------------------------- one problem */

function probRow(connOf, onLink) {
  let p = null;
  const when = h('span', { class: 'co-p-when' });
  const conn = h('a', { class: 'co-p-conn', href: '#', onclick: (e) => onLink(e, connOf(p?.connection)?.link) });
  const msg = h('span', { class: 'co-p-msg' });
  const long = h('span', { class: 'co-p-dur' });
  const state = h('span', { class: 'tag' });
  const el = h('li', { class: 'co-prob' }, when, h('span', { class: 'co-p-what' }, conn, msg), long, state);
  function paint(next, now) {
    p = next;
    el.dataset.state = p.resolved ? 'resolved' : 'ongoing';
    setText(when, at(p.start_ms));
    setAttr(when, 'title', `${at(p.start_ms)} ${zone()}${p.end_ms != null ? ` to ${at(p.end_ms)}` : ''}`);
    setText(conn, p.label || p.connection);
    setAttr(conn, 'href', connOf(p.connection)?.link || '#');
    setText(msg, p.message || p.code || '');
    setText(state, p.resolved ? 'Resolved' : 'Ongoing');
    setAttr(state, 'class', `tag ${p.resolved ? 'tag--plain' : 'tag--danger'}`);
    tick(now);
  }
  function tick(now) {
    if (!p) return;
    const ms = p.resolved || p.end_ms != null ? p.duration_ms : Math.max(p.duration_ms || 0, (now ?? Date.now()) - p.start_ms);
    setText(long, p.resolved ? formatDuration(ms) : `${formatDuration(ms)} so far`);
  }
  return { el, paint, tick, id: () => p?.id };
}

/* ---------------------------------------------------------------------- the overview */

/**
 * The Overview tab. onLocal(id) opens a section of this page (a link like /settings/integrations#tesla);
 * other links navigate. Returns { el, load() (the first read, before the page is ready), start() }.
 */
export function connectionsOverview(ctx, { onLocal }) {
  let data = null, readAt = null, failedAt = null, failErr = null;
  const now = () => ctx.api.now();

  function onLink(e, link) {
    if (!link) { e.preventDefault(); return; }
    const u = new URL(link, location.href);
    if (u.pathname === location.pathname) { e.preventDefault(); onLocal(u.hash.slice(1) || null); }
    // anything else: a plain link the router follows
  }

  // the totals: one figure per status, the light and the word beside the count
  const totals = Object.fromEntries(ORDER.map((s) => [s, h('span', { class: 'co-t-n' }, '0')]));
  const totalsEl = h('div', { class: 'co-totals', role: 'list', 'aria-label': 'Connections by status', cover: 'conn.overview.status' },
    ORDER.map((s) => h('div', { class: 'co-total', role: 'listitem', dataset: { status: s }, title: STATUS[s][1] },
      light(s), totals[s], h('span', { class: 'co-t-w' }, STATUS[s][0]))));
  const fresh = h('span', { class: 'co-fresh', role: 'status' });

  const rows = new Map();
  const head = h('div', { class: 'co-headrow', 'aria-hidden': 'true' },
    ['Status', 'Connection', 'Last contact', 'Today', 'Worked 24 h / 7 d', 'Uptime 24 h / 7 d', 'Last 24 h'].map((t) => h('span', null, t)));
  const list = h('div', { class: 'co-list', cover: 'conn.overview.rows' });

  const probs = new Map();
  const probList = h('ol', { class: 'co-probs' });
  // a long week of problems scrolls in a box of about five rows (app.css .scroll-box)
  const probBox = scrollBox({ cls: 'co-probs-box scroll-box--fit', label: 'Problems in the last 7 days', onCleanup: ctx.onCleanup }, probList);
  const probEmpty = h('p', { class: 'note co-p-empty', hidden: true }, icon('check', 'ico--sm'), ' No problems in the last 7 days.');
  const probCount = h('span', { class: 'faint co-p-count' });
  const connOf = (id) => data?.connections?.find((c) => c.id === id) || null;

  const el = h('div', { class: 'co-wrap' },
    panel({ title: 'Connections', icon: 'pulse', cls: 'co-panel', side: fresh },
      totalsEl,
      h('p', { class: 'note co-note' }, 'Every connection the server keeps. Idle means quiet as expected (the car asleep or parked), not broken. Open a row for its settings or its car’s data health. It changes here as it changes on the server.'),
      h('div', { class: 'co-table' }, head, list)),
    panel({ title: 'Problems in the last 7 days', icon: 'warn', cls: 'co-prob-panel', side: probCount, cover: 'conn.overview.problems' },
      probBox.el, probEmpty,
      note('A problem begins when a connection goes to Problem and ends when it leaves it. Times on this computer’s clock.')));

  function paint() {
    if (!data) return;
    const t = now();
    for (const s of ORDER) { setText(totals[s], n0(data.totals?.[s] ?? 0)); totals[s].parentElement.classList.toggle('is-on', (data.totals?.[s] ?? 0) > 0); }
    const conns = data.connections || [];
    // the same connections in the same order: change the rows in place; else build the list again
    const ids = conns.map((c) => c.id);
    if (ids.join('\n') !== [...rows.keys()].join('\n')) {
      const next = new Map(ids.map((id) => [id, rows.get(id) || connRow(onLink)]));
      rows.clear(); for (const [k, v] of next) rows.set(k, v);
      replace(list, [...rows.values()].map((r) => r.el));
    }
    conns.forEach((c) => rows.get(c.id).paint(c));
    const ps = data.problems || [];
    const pids = ps.map((p) => p.id);
    if (pids.join('\n') !== [...probs.keys()].join('\n')) {
      const next = new Map(pids.map((id) => [id, probs.get(id) || probRow(connOf, onLink)]));
      probs.clear(); for (const [k, v] of next) probs.set(k, v);
      replace(probList, [...probs.values()].map((r) => r.el));
    }
    ps.forEach((p) => probs.get(p.id).paint(p, t));
    probEmpty.hidden = ps.length > 0;
    probBox.el.hidden = !ps.length;
    probBox.edge();
    const open = ps.filter((p) => !p.resolved).length;
    setText(probCount, ps.length ? `${open} ongoing · ${ps.length} in all` : 'None');
    tick();
  }
  function tick() {
    const t = now();
    for (const r of rows.values()) r.tick(t);
    for (const r of probs.values()) r.tick(t);
    const base = readAt == null ? 'Not read yet' : `Read ${rel(readAt, t)}`;
    setText(fresh, failedAt != null ? `${base}; the last try failed: ${errorWords(failErr).text}` : base);
    fresh.classList.toggle('warn', failedAt != null);
  }

  // the first value (or failure) resolves load(): the page is ready once the overview is drawn
  let live = null;
  let first = null;
  const firstValue = new Promise((res) => { first = res; });
  function load() {
    live ||= follow(ctx.api, {
      type: 'connections', load: () => ctx.api.get(PATH), pollMs: POLL_MS,
      onValue: (d) => {
        if (!ctx.alive() || !d) return;
        data = d; readAt = now(); failedAt = null; failErr = null;
        paint();
        first();
      },
      onError: (e) => {
        if (!ctx.alive()) return;
        failedAt = now(); failErr = e;
        // nothing to show yet: say why in the panel (kept until a read works)
        if (!data) replace(list, h('p', { class: 'note co-p-empty' }, icon('warn', 'ico--sm'), ' ', errorWords(e).text));
        tick();
        first();
      },
    });
    return firstValue;
  }
  function start() {
    load();
    ctx.every(1000, tick);
  }
  return { el, load, start };
}

/* ---------------------------------------------------------------------- Data health: the card that leads here */

/** A card with the counts by status and a link to the Overview tab. Admins get the counts; others a note. */
export function overviewCard(ctx, admin) {
  const href = buildUrl('settings-integrations');
  const counts = h('span', { class: 'co-card-counts' });
  const el = h('a', { class: 'panel panel-pad co-card', href, cover: 'conn.overview.link' },
    h('span', { class: 'co-card-ic', 'aria-hidden': 'true' }, icon('pulse')),
    h('span', { class: 'co-card-text' },
      h('span', { class: 'co-card-title' }, 'Connections overview'),
      h('span', { class: 'co-card-sub' }, admin
        ? 'Every input and outside service the server keeps (CAN Bridge, Tesla, TeslaFi, TeslaMate, MQTT, maps): is it live, when it last worked, its errors and problems.'
        : 'Every input and outside service the server keeps, in Settings: Integrations. Only an admin sees it.'),
      counts),
    h('span', { class: 'co-card-go' }, 'Open', icon('chev-r', 'ico--sm')));
  // the counts follow `connections` events, changed in place (read every 10 s only while the stream is down)
  function load() {
    if (!admin) return Promise.resolve();
    return new Promise((done) => {
      follow(ctx.api, {
        type: 'connections', load: () => ctx.api.get(PATH), pollMs: POLL_MS,
        onValue: (d) => {
          if (!ctx.alive() || !d) return;
          const t = d.totals || {};
          morphChildren(counts, ORDER.filter((s) => t[s] || s === 'live' || s === 'problem').map((s) =>
            h('span', { class: 'co-total co-total--sm', dataset: { status: s } }, light(s), h('span', { class: 'co-t-n' }, n0(t[s] || 0)), h('span', { class: 'co-t-w' }, STATUS[s][0]))));
          done();
        },
        // the link still works
        onError: () => done(),
      });
    });
  }
  return { el, load };
}
