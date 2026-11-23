// Health, two routes.
// - /cars/{car}/health: is this car's data arriving and trusted (key: value lines with [ok] / [warn] / [bad]),
//   a log tail of what happened to its data, the gaps in the period, the totals per source, what its data takes
//   on disk (by kind and by day), and every decoded signal of its newest log with one plotted over the period.
// - /system: the server (storage, growth, running, memory, speed), a log tail, every car's data status,
//   the /health address for container checks and the one error shape of the API.
// API: GET cars/{car}/health, gaps, totals?source=, updates (summary), storage, signals/latest, signals;
// POST cars/{car}/rebuild (admin); GET system/storage, system/health (admin), cars, geocode/status.

import * as state from '../shared/state.js';
import { h, replace, cover } from '../shared/dom.js';
import { format, convert, unitLabel } from '../shared/units.js';
import { formatDateTime, formatDuration, formatDayKey, dayStart, toIsoZoned, HOUR, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, note, link, btn, textBar } from '../core/ui.js';
import { num, tag, stateWord, ago, DASH } from '../core/fmt.js';
import { table, cellBar } from '../core/table.js';
import { empty, errorWords, loadingLine } from '../core/states.js';
import { confirmDialog } from '../core/dialog.js';
import { toast } from '../core/toast.js';
import { chartOrNote, line, timeAxis, valueAxis, cursor, readout } from '../core/chart.js';
import { rpx } from '../core/display.js';
import { src, srcText } from '../core/source.js';
import { teslaCarRows, lastErrorText } from './tesla.js';
import { versionTag } from './firmware.js';
import { totalsLine } from './connections.js';
import { deviceName } from '../shared/devices.js';
import { deviceText, renameBtn } from './device-name.js';

export const id = 'health';
export const title = (ctx) => (ctx.car ? 'Health' : 'System');

const enc = encodeURIComponent;
const bytes = (v) => format(v, 'bytes');
const took = (ms) => (ms == null ? DASH : ms < 1000 ? `${formatNumber(Math.round(ms))} ms` : formatDuration(ms));
const fullPeriod = (p) => ({ ...p, from_ms: p?.from_ms ?? 0 });
const none = (t) => h('span', { class: 'muted' }, t);
const ok = (word = 'ok') => tag(word, 'good');
const warn = (word = 'warn') => tag(word, 'warn');
const bad = (word = 'bad') => tag(word, 'bad');
const status = (t, ...rest) => h('span', null, t, ' ', ...rest);

const REASONS = {
  upload_silence: ['no log has arrived for a while', 'check the bridge has power and a network, and that its key still uploads. nothing is lost while the car sleeps.', 'settings-cars'],
  trust_failed: ['the last log failed its check', 'its signature or chain did not match, so its data is kept apart. pair the bridge again if its key changed.', 'settings-cars'],
  logs_waiting_trust: ['logs wait for a key to be trusted', 'approve or assign the key and they are read at once.', 'settings-cars'],
  gps_inputs_disagree: ['positions from one input are out of step', 'one input’s GPS positions are far from the other inputs’ positions at the same moments, more than lag or noise explains (often a decoding problem). Its positions are not used there; the other inputs’ positions are. The rest of its data is still used.', null],
  logs_missing: ['logs missing on the server', 'the server lost some logs and asked the car to send them again; see missing on the server below.', null],
  rebuild_pending: ['drives and charges are being rebuilt', 'worked out again from the stored logs; this page reads again until it is done.', null],
};
const TRUST = { verified: ['verified', 'good'], unsigned: ['unsigned', 'warn'], failed: ['failed', 'bad'] };
const TRUST_REASONS = { signature_mismatch: 'the signature does not match the key', chain_break: 'it does not follow the log before it', unknown_key: 'the key is not known', bad_format: 'the file could not be read' };
const SPAN_REASONS = { no_log: 'no log arrived', dropped_frames: 'the bridge dropped frames', failed_check: 'the log failed its check', chain_gap: 'a log is missing from the chain', paused: 'recording was paused' };
const GAP_CAUSES = { no_log: 'no log', not_recorded: 'not recorded', chain_gap: 'chain gap', unknown: 'unknown' };
const KIND_TEXT = {
  logs: 'can logs as sent, compressed and signed; all else is rebuilt from these', series: 'values over time for charts',
  curves: 'charge power, volts and amps', rows: 'drives, charges, states, visits', imported: 'imported history', tesla_api: 'values read from the tesla api',
  tiles: 'map tiles kept for offline maps', places: 'places, prices, settings', geocode: 'addresses looked up',
};
const UNIT_KIND = { km: 'distance', 'km/h': 'speed', '°C': 'temperature', kPa: 'pressure', kWh: 'energy', kW: 'power', '%': 'percent', V: 'voltage', A: 'current', m: 'length' };
const signalKind = (s) => (s.series === 'odometer' ? 'odometer' : UNIT_KIND[s.unit] || null);
const COORD = new Set(['gps_latitude', 'gps_longitude']);
function signalText(s) {
  if (s.value_name) return s.value_name;
  if (s.value == null) return DASH;
  if (COORD.has(s.series)) return `${formatNumber(s.value, { minimumFractionDigits: 5, maximumFractionDigits: 5 })}°`;
  const k = signalKind(s);
  if (k) return format(s.value, k);
  return s.unit ? `${formatNumber(s.value, { maximumFractionDigits: 2 })} ${s.unit}` : formatNumber(s.value, { maximumFractionDigits: 2 });
}
const API_ERRORS = [
  [400, 'bad_input', 'a value has the wrong type, a field is unknown or a filter is not valid; fields names each one'],
  [401, 'not_signed_in', 'no session, or it ended: sign in again'],
  [403, 'csrf', 'a change came without the X-Sentry-Mate header'],
  [403, 'not_allowed', 'the account may read this but not change it, or it is for admins only'],
  [404, 'no_such_car', 'no such car, or this account may not see it: both answer the same'],
  [404, 'not_found', 'no such address or row (each kind has its own no_such_ code)'],
  [405, 'method_not_allowed', 'this address does not take that method; Allow lists the ones it does'],
  [409, 'stale_version', 'someone changed it first; load and try again'],
  [409, 'import_running', 'another import is not finished yet'],
  [413, 'too_large', 'the body is over 1 MB'],
  [422, 'invalid', 'well formed but against a rule; fields names each one'],
  [429, 'throttled', 'too many tries; Retry-After says how many seconds to wait'],
  [500, 'server_error', 'something broke on the server; nothing was changed'],
];

export async function mount(ctx) { return ctx.car ? mountCar(ctx) : mountSystem(ctx); }

const timelineAt = (car, from, to) => state.buildUrl('timeline', { car, period: { from: from - HOUR, to: to + HOUR } });

/** A log tail: lines '2026-11-16 10:40:12  [ok]   text', oldest first, in a mono box. */
function logTail(lines, tz, { cover: cv = null, title = 'log tail' } = {}) {
  const sorted = lines.filter((l) => l.ms != null).sort((a, b) => a.ms - b.ms).slice(-14);
  const stamp = (ms) => toIsoZoned(ms, tz).slice(0, 19).replace('T', ' ');
  return section({ title, cover: cv, side: none(`${sorted.length} lines, ${tz}`) },
    sorted.length ? h('pre', { class: 'mono-box log-tail', tabindex: '0', 'aria-label': title },
      sorted.map((l, i) => [i ? '\n' : null, h('span', { class: 'muted' }, stamp(l.ms)), '  ', tag(l.tag, l.tone), ' '.repeat(Math.max(1, 7 - l.tag.length)), l.text]))
      : empty('nothing to show yet'));
}

function kindsTable(ctx, kinds, total, { title, csvName, coverIds }) {
  const rows = (kinds || []).map((k) => ({ ...k, share: total ? (k.bytes / total) * 100 : null }));
  const t = table(ctx, {
    title, rows, csv: false, csvName, cover: coverIds, sort: { key: 'bytes', dir: 'desc' },
    note: 'as stored on disk: compressed and encrypted at rest.',
    columns: [
      { key: 'label', label: 'what', sort: true, ph: 'main', cover: 'dbinfo.sizes.table', value: (r) => r.label, render: (r) => r.label },
      { key: 'about', label: 'holds', ph: 'sub', grow: true, cls: 'wrap-cell', render: (r) => none(KIND_TEXT[r.kind] || '') },
      { key: 'bar', label: 'share', render: (r) => cellBar((r.share || 0) / 100, 20) },
      { key: 'share', label: '%', num: true, sort: true, value: (r) => r.share, render: (r) => format(r.share, 'percent', { digits: 1 }) },
      { key: 'bytes', label: 'on disk', num: true, sort: true, ph: 'num', cover: 'dbinfo.sizes.data', value: (r) => r.bytes, render: (r) => bytes(r.bytes),
        total: () => h('span', { cover: 'dbinfo.sizes.total' }, bytes(total)) },
    ],
    empty: () => empty('nothing stored yet', 'storage grows once the first log arrives'),
  });
  return t;
}

/* ================================================================== car */

async function mountCar(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const base = `cars/${enc(cid)}`;
  const admin = !!ctx.admin;
  let readAt = ctx.api.now();
  const readEl = h('span', { class: 'muted' });
  const paintRead = () => replace(readEl, `read ${formatDateTime(readAt, tz, { now: ctx.api.now(), seconds: true })}`);
  paintRead();

  const hd = head(ctx);
  const carBtn = hd.querySelectorAll('.ph-btn')[1];
  if (carBtn) cover(carBtn, 'dbinfo.filter.car');
  const reloadBtn = btn('[reload]', () => reloadAll(true), { kind: 'line', cover: 'dbinfo.control.refresh', title: 'read every figure again' });
  const toolLine = h('p', { class: 'subline' }, readEl, '  ', reloadBtn, '  ', link(state.buildUrl('health'), 'system'), '  ', link(state.buildUrl('settings-cars', { car: cid }), 'settings/cars'),
    admin ? ['  ', link(state.buildUrl('settings-integrations'), 'connections overview', { cover: 'conn.overview.link', title: 'every connection the server keeps: live, errors, problems' })] : null);

  /* ---------- status */
  const statusSlot = h('div', null, loadingLine('reading health'));
  const missSlot = h('div', null, loadingLine('reading health'));
  const tailSlot = h('div');
  let pendingT = 0, wasPending = false;
  ctx.onCleanup(() => clearTimeout(pendingT));
  let teslaItems = null;
  async function loadHealth() {
    const [hl, ts] = await Promise.all([
      ctx.api.get(`${base}/health`, fullPeriod(ctx.params)),
      // the Tesla API input for this car (admins only): what the server asks Tesla for it now
      admin ? ctx.api.list('integrations/tesla/status').all().catch(() => null) : Promise.resolve(null),
    ]);
    if (!ctx.alive()) return;
    teslaItems = ts;
    paintHealth(hl);
    clearTimeout(pendingT);
    if (hl.rebuild_pending) pendingT = setTimeout(() => { if (ctx.alive()) loadHealth().catch(() => {}); }, 3000);
    else if (wasPending) { gaps.reload(); loadTotals().catch(() => {}); }
    wasPending = !!hl.rebuild_pending;
  }

  function paintHealth(hl) {
    const trust = hl.last_log_trust || {};
    const tw = TRUST[trust.result];
    const spans = hl.unknown_spans || [];
    const reasons = hl.reasons || [];
    const keys = hl.keys || [];
    const silent = hl.last_upload_ms == null || reasons.includes('upload_silence');
    replace(statusSlot,
      h('p', { class: 'hl-verdict' }, hl.healthy ? ok() : warn(), ' ', hl.healthy ? 'data arrives, passes its checks and nothing waits' : `${reasons.length} ${reasons.length === 1 ? 'thing needs' : 'things need'} attention`, none('  (the same test sets "healthy" in mqtt)')),
      reasons.length ? h('ul', { class: 'hl-fixes' }, reasons.map((code) => {
        const r = REASONS[code] || [code, '', null];
        return h('li', null, code === 'rebuild_pending' ? tag('running', 'warn') : warn(), ' ', r[0], none(`: ${r[1]}`), r[2] ? ['  ', link(state.buildUrl(r[2], { car: cid }), 'fix')] : null);
      })) : null,
      kv([
        ['last upload', hl.last_upload_ms != null ? status(silent ? warn() : ok(), ago(hl.last_upload_ms), none(` ${formatDateTime(hl.last_upload_ms, tz, { now: ctx.api.now() })}`)) : status(warn(), 'no log yet')],
        ['log check', tw ? status(tag(tw[0], tw[1]), trust.reason ? none(TRUST_REASONS[trust.reason] || trust.reason) : none('signature and hash chain match')) : none('no log checked yet')],
        ['logs waiting', status(hl.logs_waiting ? warn() : ok(), formatNumber(hl.logs_waiting ?? 0), none(hl.logs_waiting ? ' key not trusted yet' : ' every log was read'))],
        ['unknown time', status(hl.unknown_ms ? warn() : ok(), hl.unknown_ms != null ? formatDuration(hl.unknown_ms) : DASH, none(` in ${spans.length} ${spans.length === 1 ? 'span' : 'spans'}`))],
        ['chain gaps', status((hl.chain_gaps || []).length ? bad() : ok(), formatNumber((hl.chain_gaps || []).length))],
        ['dropped frames', status((hl.dropped_frames || []).length ? warn() : ok(), formatNumber((hl.dropped_frames || []).length), none(' times the bridge lost frames'))],
        ['uploads', hl.uploads_enabled ? status(ok(), 'accepted') : status(warn(), 'turned off for this car')],
        ['rebuild', hl.rebuild_pending ? status(tag('running', 'warn'), 'working out drives again') : status(ok(), 'nothing pending')],
        ...keys.map((k, i) => [keys.length > 1 ? `bridge key ${i + 1}` : 'bridge key', h('span', null, k.preferred ? tag('preferred', 'accent') : null, k.preferred ? ' ' : null, deviceText(k), admin ? [' ', renameBtn(ctx, k, (row) => { Object.assign(k, row); paintHealth(hl); }, { line: true })] : null, ' ', h('span', { class: 'muted break' }, k.key), '  ',
          k.last_upload_ms != null ? ago(k.last_upload_ms) : none('never uploaded'), k.logs_waiting ? [' ', warn(`${formatNumber(k.logs_waiting)} waiting`)] : null)]),
        keys.length ? null : ['bridge keys', status(warn(), 'none tied to this car: pair one on settings/cars')],
        ['data from', src(car, { full: true }) || none('no data yet')],
        ...teslaLines(),
      ]),
      admin ? h('p', { class: 'acts' }, btn(`rebuild ${String(ctx.period.label).toLowerCase()}`, (e) => rebuildNow(e.currentTarget)),
        none('  drives, charges, states and visits are worked out again from the stored logs; typed names and prices stay.'))
        : note('an admin can rebuild drives and charges from the stored logs.'));

    paintMissing(hl.missing_logs || {});

    const lines = [];
    if (hl.last_upload_ms != null) lines.push({ ms: hl.last_upload_ms, tag: silent ? 'warn' : 'ok', tone: silent ? 'warn' : 'good', text: 'last log arrived' });
    if (trust.log_start_ms != null && tw) lines.push({ ms: trust.log_start_ms, tag: tw[1] === 'good' ? 'ok' : tw[1], tone: tw[1], text: `log check: ${tw[0]}${trust.reason ? `, ${TRUST_REASONS[trust.reason] || trust.reason}` : ''}` });
    for (const g of hl.chain_gaps || []) lines.push({ ms: g.from_ms, tag: 'bad', tone: 'bad', text: `chain gap until ${formatDateTime(g.to_ms, tz, { now: ctx.api.now() })}` });
    for (const d of hl.dropped_frames || []) lines.push({ ms: d.start_ms, tag: 'warn', tone: 'warn', text: `dropped frames for ${formatDuration((d.end_ms ?? d.start_ms) - d.start_ms)}` });
    for (const s of spans) lines.push({ ms: s.start_ms, tag: 'warn', tone: 'warn', text: `unknown ${s.end_ms == null ? 'since then' : `for ${formatDuration(s.end_ms - s.start_ms)}`}: ${SPAN_REASONS[s.reason] || s.reason}` });
    for (const k of keys) if (k.last_upload_ms != null) lines.push({ ms: k.last_upload_ms, tag: 'ok', tone: 'good', text: `upload from ${deviceName(k).text}` });
    replace(tailSlot, logTail(lines, tz, { cover: 'faq.stopped-recording' }));
  }

  // logs the server should have but no longer does (it looks at start-up and once a day): the same five lines always
  function paintMissing(m) {
    const at = (ms) => formatDateTime(ms, tz, { now: ctx.api.now() });
    const n = m.count ?? 0, gone = m.gone ?? 0;
    const logs = (k) => `${formatNumber(k)} ${k === 1 ? 'log' : 'logs'}`;
    replace(missSlot, kv([
      ['missing', n ? status(warn(), logs(n), none(' (with their sidecars) the server should have but no longer does'))
        : status(ok(), 'none', none(' every log the server recorded is here'))],
      ['oldest', n && m.oldest_ms != null ? h('span', null, at(m.oldest_ms), m.found_ms != null ? none(`  found missing ${at(m.found_ms)}`) : null) : none(DASH)],
      ['asked the car', !n ? none('nothing to ask for')
        : m.asked ? status(tag('asked', 'accent'), 'asked the car to send them again', m.asked_ms != null ? none(` ${at(m.asked_ms)}`) : null)
          : status(warn(), 'not asked yet', none(' the car is asked when it next connects'))],
      ['lost for good', gone ? status(bad(), `the car no longer has ${formatNumber(gone)} of these either`, m.gone_oldest_ms != null ? none(`  oldest ${at(m.gone_oldest_ms)}`) : null)
        : none(n ? 'none so far' : DASH)],
      ['timeline', gone ? status(warn(), 'their time shows as unknown time') : none(n ? 'filled in once the car sends them' : DASH)],
      ['last checked', m.scanned_ms != null ? h('span', null, at(m.scanned_ms), none('  at start-up and once a day')) : none('not checked yet')],
    ], { cols: 1 }));
  }

  // the Tesla API rows of this car: admins see what the server asks Tesla; others a plain line
  function teslaLines() {
    const hasApi = (car.sources || []).some((x) => x.kind === 'tesla_api');
    if (!admin) return hasApi ? [['tesla api', none('also read from the tesla api; an admin sees what is asked of tesla')]] : [];
    const rows = teslaCarRows(ctx, teslaItems, cid);
    if (rows) return rows;
    return [['tesla api', [none(hasApi ? 'no tesla vehicle reads this car now; its older tesla api data stays.  ' : 'not read from the tesla api.  '), link(`${state.buildUrl('settings-integrations')}#tesla`, 'tesla account')]]];
  }

  async function rebuildNow(b) {
    const yes = await confirmDialog(`work out drives, charges, states and visits of ${car.name} again from the stored logs for ${String(ctx.period.label).toLowerCase()}? typed names and prices stay.`, { title: 'rebuild from logs', ok: 'rebuild' });
    if (!yes || !ctx.alive()) return;
    b.disabled = true;
    try {
      const r = await ctx.api.post(`${base}/rebuild`, { from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms });
      if (!ctx.alive()) return;
      toast(`rebuilt from ${formatNumber(r.logs_read ?? 0)} logs in ${took(r.took_ms ?? 0)}`, { kind: 'ok' });
      reloadAll(false);
    } catch (e) { if (ctx.alive()) toast(errorWords(e).text, { kind: 'error' }); } finally { b.disabled = false; }
  }

  /* ---------- gaps */
  const gaps = table(ctx, {
    title: 'gaps', path: `${base}/gaps`, params: ctx.params, csvName: 'gaps',
    note: 'times with no usable data, newest first. a gap in which the odometer moved is a drive nobody recorded.',
    columns: [
      { key: 'start', label: 'from', ph: 'main', render: (g) => h('a', { class: 'lnk-plain', href: timelineAt(cid, g.start_ms, g.end_ms ?? ctx.api.now()) }, formatDateTime(g.start_ms, tz, { now: ctx.api.now(), weekday: false })) },
      { key: 'dur', label: 'lasted', num: true, ph: 'num', render: (g) => (g.end_ms == null ? tag('open', 'warn') : formatDuration(g.end_ms - g.start_ms)) },
      { key: 'cause', label: 'cause', ph: 'sub', render: (g) => [GAP_CAUSES[g.cause] || g.cause, g.gap_reason ? none(`: ${SPAN_REASONS[g.gap_reason] || g.gap_reason}`) : null] },
      { key: 'jump', label: 'odometer moved', num: true, render: (g) => num(g.odometer_jump_km, 'distance') },
      { key: 'where', label: 'where', grow: true, render: (g) => [g.from_place?.name || none('unknown place'), g.to_place && g.to_place.id !== g.from_place?.id ? none(` to ${g.to_place.name}`) : null] },
      { key: 'source', label: 'data', title: 'where the data came from: smo, api (the Tesla API), imp (imported)', render: (g) => src(g) || DASH, value: (g) => srcText(g) },
    ],
    empty: () => empty('no gaps in this range', 'every moment is accounted for'),
  });

  /* ---------- totals */
  const totalsSlot = h('div', null, loadingLine('reading totals'));
  async function loadTotals() {
    const [all, can, api, tm, upd] = await Promise.all([
      ctx.api.get(`${base}/totals`, { source: 'all' }), ctx.api.get(`${base}/totals`, { source: 'smo' }),
      ctx.api.get(`${base}/totals`, { source: 'tesla_api' }), ctx.api.get(`${base}/totals`, { source: 'import' }),
      ctx.api.get(`${base}/updates`, { limit: 1 }).catch(() => null),
    ]);
    if (!ctx.alive()) return;
    const sm = upd?.summary || null;
    // by input: a row SMO and the Tesla API both saw counts under each, so the parts can add up to more than the whole
    const split = (a, x, b, kind) => none(`  smo ${format(a ?? 0, kind)}${x ? `, tesla api ${format(x, kind)}` : ''}, imported ${format(b ?? 0, kind)}`);
    const counts = table(ctx, {
      title: 'rows kept', rows: [
        { kind: 'drives', can: can.drives, api: api.drives, tm: tm.drives, total: all.drives }, { kind: 'charges', can: can.charges, api: api.charges, tm: tm.charges, total: all.charges },
        { kind: 'software updates', can: can.updates, api: api.updates, tm: tm.updates, total: all.updates },
      ], csv: false, cover: 'dbinfo.table.row_counts', sort: { key: 'total', dir: 'desc' },
      note: 'exact counts kept as rows change. a row smo and the tesla api both saw counts in both columns; where imported and live history overlap both are counted, and shown is what the other pages list.',
      columns: [
        { key: 'kind', label: 'kind', sort: true, ph: 'main', grow: true, cover: 'dbinfo.rows.table_name' },
        { key: 'can', label: 'smo', num: true, sort: true, ph: 'sub', render: (r) => formatNumber(r.can ?? 0) },
        { key: 'api', label: 'tesla api', num: true, sort: true, render: (r) => formatNumber(r.api ?? 0) },
        { key: 'tm', label: 'imported', num: true, sort: true, render: (r) => formatNumber(r.tm ?? 0) },
        { key: 'total', label: 'shown', num: true, sort: true, ph: 'num', cover: 'dbinfo.rows.row_count', render: (r) => h('b', null, formatNumber(r.total)) },
      ],
    });
    replace(totalsSlot, kv([
      ['logged distance', [num(all.distance_km, 'distance'), split(can.distance_km, api.distance_km, tm.distance_km, 'distance')], { cover: 'dbinfo.stat.logged_distance' }],
      ['odometer', [num(all.odometer_last_km, 'odometer'), all.odometer_last_ms != null ? none(`  read ${ago(all.odometer_last_ms)}`) : none('  not sent yet')], { cover: 'dbinfo.stat.odometer' }],
      ['not logged', all.not_logged_km != null ? status(all.not_logged_km > 1 ? warn() : ok(), num(all.not_logged_km, 'distance')) : none('needs two odometer readings')],
      ['drives', [formatNumber(all.drives ?? 0), split(can.drives, api.drives, tm.drives, 'count')], { cover: 'dbinfo.stat.drive_count' }],
      ['charges', [formatNumber(all.charges ?? 0), split(can.charges, api.charges, tm.charges, 'count')], { cover: 'dbinfo.stat.charge_count' }],
      ['updates', [formatNumber(all.updates ?? 0), split(can.updates, api.updates, tm.updates, 'count')], { cover: 'dbinfo.stat.update_count' }],
      ['software', sm?.current_version ? [link(state.buildUrl('software', { car: cid }), sm.current_version), versionTag(sm.current_version_source) ? [' ', versionTag(sm.current_version_source)] : null, sm.build_changed_since != null ? [' ', warn('changed since')] : null] : none('not known: the car does not send its version text'), { cover: 'dbinfo.stat.current_version' }],
    ]), counts.el);
  }

  /* ---------- storage */
  const storageSlot = h('div', null, loadingLine('reading storage'));
  async function loadStorage() {
    const st = await ctx.api.get(`${base}/storage`, { limit: 1 });
    if (!ctx.alive()) return;
    const kt = kindsTable(ctx, st.kinds, st.total_bytes, { title: `storage of ${car.name}`, csvName: 'storage by kind', coverIds: ['dbinfo.table.sizes'] });
    const labels = Object.fromEntries((st.kinds || []).map((k) => [k.kind, k.label]));
    const days = table(ctx, {
      title: 'storage by day', path: `${base}/storage`, params: {}, limit: 16, csvName: 'storage by day',
      note: 'newest first, only days with data, in the car’s days.',
      columns: [
        { key: 'date', label: 'day', ph: 'main', render: (r) => h('a', { class: 'lnk-plain', href: state.buildUrl('timeline', { car: cid, period: { from: dayStart(r.date, tz), to: dayStart(r.date, tz) + DAY } }) }, formatDayKey(r.date, { weekday: false })) },
        { key: 'series', label: 'what', ph: 'sub', grow: true, render: (r) => labels[r.series] || r.series },
        { key: 'source', label: 'data', render: (r) => src(r) || DASH, value: (r) => srcText(r) },
        { key: 'bytes', label: 'on disk', num: true, ph: 'num', render: (r) => bytes(r.bytes) },
      ],
      empty: () => empty('no stored days yet'),
    });
    replace(storageSlot, kt.el, days.el);
    await days.ready;
  }

  /* ---------- signals */
  const sigSearch = h('input', { class: 'in in--search', type: 'search', placeholder: 'signal or value', 'aria-label': 'Find a signal', spellcheck: 'false' });
  let signals = [], chosen = null;
  const sigTable = table(ctx, {
    title: 'signals', rows: [], csv: false, cover: ['nr.dash.state-table', 'nr.dash.state-table.sort-scroll'], sort: { key: 'name', dir: 'asc' }, limit: 40,
    side: h('label', { class: 'filt filt--inline' }, h('span', { class: 'filt-p' }, 'find>'), sigSearch),
    note: 'every signal decoded from the newest log. sort by any heading; [plot] draws one over the range below.',
    columns: [
      { key: 'name', label: 'signal', sort: true, ph: 'main', grow: true, cover: 'nr.dash.state-table.parameter', value: (s) => s.comment || s.series,
        render: (s) => [s.comment || s.series, s.comment ? none(` ${s.series}`) : null] },
      { key: 'message', label: 'message', sort: true, value: (s) => `${s.bus} ${s.message_id}`, render: (s) => [s.message_id, none(` ${s.bus}`)] },
      { key: 'value', label: 'value', num: true, sort: true, ph: 'num', firstDir: 'desc', cover: 'nr.dash.state-table.value',
        value: (s) => (s.value_name ? s.value_name : s.value == null ? null : signalKind(s) ? convert(s.value, signalKind(s)) : s.value), render: (s) => signalText(s) },
      { key: 'raw', label: 'as sent', num: true, render: (s) => none(s.value == null ? DASH : `${formatNumber(s.value, { maximumFractionDigits: 6 })}${s.unit ? ` ${s.unit}` : ''}`) },
      { key: 'at', label: 'read', num: true, sort: true, ph: 'sub', value: (s) => s.at_ms, render: (s) => (s.at_ms != null ? ago(s.at_ms) : DASH) },
      { key: 'plot', label: '', render: (s) => (s.has_series ? h('button', { class: ['btn', 'btn--line', chosen === s.series ? 'tone--accent' : null], type: 'button', 'aria-pressed': String(chosen === s.series), onclick: () => plotSignal(s) }, '[plot]') : none('latest')) },
    ],
    empty: () => empty(signals.length ? `no signal holds "${sigSearch.value}"` : 'no signals yet', signals.length ? null : 'signals show once the first log is decoded'),
  });
  const filterSignals = () => {
    const words = sigSearch.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    sigTable.setRows(!words.length ? signals : signals.filter((s) => {
      const hay = [s.series, s.comment, s.message_id, s.bus, s.unit, s.value_name, signalText(s)].filter(Boolean).join(' ').toLowerCase();
      return words.every((w) => hay.includes(w));
    }));
  };
  let sigT = 0;
  ctx.on(sigSearch, 'input', () => { clearTimeout(sigT); sigT = setTimeout(() => ctx.alive() && filterSignals(), 150); });
  ctx.onCleanup(() => clearTimeout(sigT));
  async function loadSignals() {
    signals = await ctx.api.list(`${base}/signals/latest`, {}, { limit: 500 }).all({ max: 20000 });
    if (!ctx.alive()) return;
    filterSignals();
  }
  const plotSlot = h('div', null, empty('no signal plotted', 'press [plot] on a signal with values over time'));
  async function plotSignal(s) {
    chosen = s.series;
    sigTable.repaint();
    const kind = signalKind(s);
    replace(plotSlot, loadingLine(`reading ${s.comment || s.series}`));
    let data;
    try {
      const get = (params, max) => ctx.api.get(`${base}/signals`, { series: s.series, ...params, max_points: max });
      if (ctx.params?.from_ms != null) data = await get(ctx.params, 1500);
      else {
        const coarse = await get(fullPeriod(ctx.params), 400);
        const first = (coarse.values?.[s.series] || []).findIndex((v) => v != null);
        data = first < 0 ? coarse : await get({ ...ctx.params, from_ms: coarse.t_ms[first] }, 1500);
      }
    } catch (e) {
      if (ctx.alive() && chosen === s.series) replace(plotSlot, h('p', { class: 'errbox-line' }, bad(errorWords(e).tag), ' ', errorWords(e).text));
      return;
    }
    if (!ctx.alive() || chosen !== s.series) return;
    const raw = data.values?.[s.series] || [];
    const vals = raw.map((v) => (v == null ? null : kind ? convert(v, kind) : v));
    const box = h('div', { class: 'chart' });
    const rd = readout('move over the chart for a value');
    replace(plotSlot,
      h('div', { class: 'chart-head' }, h('b', null, s.comment || s.series), h('span', null, kind ? unitLabel(kind) : s.unit || '')),
      vals.some((v) => v != null) ? [box, rd.el] : empty('nothing recorded in this range'),
      note(`each point is the mean of ${formatDuration(data.bucket_ms || 0)}; the line breaks where nothing is known.`));
    if (!vals.some((v) => v != null)) return;
    await chartOrNote(box, ctx, () => ({
      padding: [rpx(6), 0, 0, 0], cursor: cursor(), select: { show: false },
      scales: { x: { time: true }, y: { auto: true } },
      axes: [timeAxis(tz), valueAxis((v) => formatNumber(v))],
      series: [{}, line(s.series, 0, { width: 1.5 })],
    }), [data.t_ms, vals], { onCursor: (i) => rd.show(i == null ? null : [['at', formatDateTime(data.t_ms[i], tz, { now: ctx.api.now(), seconds: true })], ['value', vals[i] == null ? 'not known' : kind ? format(raw[i], kind) : `${formatNumber(vals[i], { maximumFractionDigits: 3 })} ${s.unit || ''}`]]) });
  }

  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'health' }, cover: 'shots.database-info' },
    hd, toolLine,
    section({ title: 'status' }, statusSlot), section({ title: 'missing on the server' }, missSlot), tailSlot, gaps.el,
    section({ title: 'what is recorded', side: none('all time, not the range') }, totalsSlot),
    storageSlot, sigTable.el, section({ title: 'one signal over the range' }, plotSlot)));

  async function reloadAll(say) {
    reloadBtn.disabled = true;
    try {
      await Promise.all([loadHealth(), gaps.reload(), loadTotals(), loadStorage(), loadSignals()]);
      readAt = ctx.api.now(); paintRead();
      if (say && ctx.alive()) toast('read again', { kind: 'info' });
    } catch (e) { if (ctx.alive()) toast(errorWords(e).text, { kind: 'error' }); } finally { reloadBtn.disabled = false; }
  }
  ctx.events('rows', (ev) => {
    if (ev?.data?.car !== cid) return;
    loadHealth().catch(() => {}); gaps.reload(null, { quiet: true }); loadTotals().catch(() => {});
  });

  try { await Promise.all([loadHealth(), loadTotals(), loadStorage(), loadSignals(), gaps.ready]); } catch (e) { if (ctx.alive()) ctx.fail(e); return; }
  if (!ctx.alive()) return;
  ctx.ready();
}

/* ================================================================== system */

async function mountSystem(ctx) {
  const admin = !!ctx.admin;
  const tz = ctx.tz;
  let readAt = ctx.api.now();
  const readEl = h('span', { class: 'muted' });
  const paintRead = () => replace(readEl, `read ${formatDateTime(readAt, tz, { now: ctx.api.now(), seconds: true })}`);
  paintRead();
  const hd = head(ctx, { title: 'System', period: false, car: false, extra: h('span', { class: 'ph-extra muted' }, 'the whole server') });
  const reloadBtn = btn('[reload]', () => reloadAll(true), { kind: 'line', cover: 'dbinfo.control.refresh', title: 'read every figure again' });
  const toolLine = h('p', { class: 'subline' }, readEl, '  ', reloadBtn);
  const locked = (what) => h('p', { class: 'note' }, tag('admin', 'muted'), ` ${what} is shown to admins. your account sees the cars below.`);

  /* ---------- storage */
  const storeSlot = h('div', null, loadingLine('reading storage'));
  async function loadStorage() {
    let st = null;
    if (admin) {
      try { st = await ctx.api.get('system/storage'); } catch (e) { if (e.status !== 403) throw e; }
    }
    if (!ctx.alive()) return;
    if (!st) { replace(storeSlot, cover(locked('server storage (total size, free space, growth)'), 'dbinfo.stat.total_size')); return; }
    const growth = st.growth || [];
    const perDay = st.growth_per_day_bytes ?? (growth.length ? growth.reduce((a, g) => a + g.bytes, 0) / growth.length : null);
    const used = st.total_bytes;
    const disk = used != null && st.free_bytes != null ? used + st.free_bytes : null;
    const usedPct = disk ? (used / disk) * 100 : null;
    const full = st.full_in_days;
    const kt = kindsTable(ctx, st.kinds, used, { title: 'by kind', csvName: 'server storage by kind', coverIds: ['dbinfo.table.sizes'] });
    const growthBox = h('div', { class: 'chart chart--pane' });
    const rd = readout('move over a day');
    replace(storeSlot, kv([
      ['data takes', h('b', null, bytes(used)), { cover: 'dbinfo.stat.total_size' }],
      ['free on disk', st.free_bytes != null ? [bytes(st.free_bytes), disk ? none(` of ${bytes(disk)}`) : null] : none('not known')],
      ['disk use', usedPct != null ? [textBar(usedPct, { tone: usedPct > 85 ? 'bad' : usedPct > 70 ? 'warn' : 'accent' }), ' ', format(usedPct, 'percent', { digits: 1 })] : none('not known')],
      ['grows by', perDay != null ? [bytes(perDay), none(` a day, mean of ${growth.length} days`)] : none('needs a few days')],
      ['full in', full != null ? status(full < 60 ? bad() : full < 365 ? warn() : ok(), `${formatNumber(full)} days`) : none('not growing, or not known')],
    ]), kt.el,
    h('div', { class: 'blk' }, h('div', { class: 'chart-head' }, h('b', null, 'added per day'), h('span', null, 'MB')),
      growth.length ? [growthBox, rd.el] : empty('no full day yet')));
    if (growth.length) {
      const t = growth.map((g) => Date.parse(`${g.date}T12:00:00Z`));
      const mb = growth.map((g) => g.bytes / 1e6);
      await chartOrNote(growthBox, ctx, ({ colors: c }) => ({
        padding: [rpx(6), 0, 0, 0], cursor: { ...cursor(), x: true }, select: { show: false },
        scales: { x: { time: true }, y: { range: (u, a, b) => [0, Math.max(1, (b || 0) * 1.1)] } },
        axes: [timeAxis('UTC'), valueAxis((v) => formatNumber(v))],
        series: [{}, { label: 'MB', stroke: c.c1, fill: c.c1, width: 0, paths: window.uPlot.paths.bars({ size: [0.6, rpx(24)] }), points: { show: false } }],
      }), [t, mb], { onCursor: (i) => rd.show(i == null ? null : [['day', formatDayKey(growth[i].date, { weekday: false })], ['added', bytes(growth[i].bytes)]]) });
    }
  }

  /* ---------- server */
  const srvSlot = h('div', null, loadingLine('reading the server'));
  const tailSlot = h('div');
  async function loadServer() {
    let sh = null, geo = null;
    if (admin) {
      try { [sh, geo] = await Promise.all([ctx.api.get('system/health'), ctx.api.get('geocode/status').catch(() => null)]); } catch (e) { if (e.status !== 403) throw e; }
    }
    if (!ctx.alive()) return;
    if (!sh) { replace(srvSlot, locked('server health (uptime, versions, memory, timings)')); replace(tailSlot); return; }
    const v = sh.versions || {};
    const mem = sh.ingest?.memory || {};
    const memPct = mem.limit_bytes ? (mem.used_bytes / mem.limit_bytes) * 100 : null;
    const ms = (x) => (x == null ? DASH : `${formatNumber(x, { maximumFractionDigits: x < 10 ? 1 : 0 })} ms`);
    const t = sh.timings_ms || {};
    const slow = t.api_p95 != null && t.api_p95 > 1000;
    replace(srvSlot, kv([
      ['state', status(ok(), 'serving')],
      ['up for', sh.uptime_ms != null ? formatDuration(sh.uptime_ms) : DASH],
      ['server', v.server || DASH],
      ['api', v.api ?? DASH],
      ['storage format', v.db_format ?? DASH],
      ['timeline rules', v.timeline_rules ?? DASH],
      ['import mapping', v.import_mapping ?? DASH],
      ['housekeeping', sh.housekeeping_last_ms != null ? [ago(sh.housekeeping_last_ms), none('  checks, compaction, storage figure')] : status(warn(), 'not yet')],
      ['upload memory', [textBar(memPct, { tone: memPct > 85 ? 'warn' : 'accent' }), ' ', `${bytes(mem.used_bytes)} of ${bytes(mem.limit_bytes)}`], { tone: memPct > 85 ? 'warn' : null }],
      ['cars open', `${formatNumber(sh.db?.open_cars ?? 0)} of ${formatNumber(sh.db?.cars ?? 0)}, ${bytes(sh.db?.open_bytes)}`],
      ['cars in memory', formatNumber(sh.ingest?.cached_cars ?? 0)],
      ['tesla api', sh.tesla?.accounts ? link(`${state.buildUrl('settings-integrations')}#tesla`, `${formatNumber(sh.tesla.accounts)} ${sh.tesla.accounts === 1 ? 'account' : 'accounts'} reading ${formatNumber(sh.tesla.cars ?? 0)} ${sh.tesla.cars === 1 ? 'car' : 'cars'}`)
        : [none('no account: no connection to tesla is made  '), link(`${state.buildUrl('settings-integrations')}#tesla`, 'tesla account')]],
      ['tesla requests', [formatNumber(sh.tesla?.requests_today ?? 0), none(' today, at most 3,000 a day per account')]],
      ['tesla error', sh.tesla?.last_error ? status(warn(), lastErrorText(sh.tesla.last_error), sh.tesla.last_error.at_ms ? none(`  ${ago(sh.tesla.last_error.at_ms)}`) : null) : none('none'), { tone: sh.tesla?.last_error ? 'warn' : null }],
      ['addresses', geo ? `${geo.mode}, ${formatNumber(geo.cache_size ?? 0)} cached` : sh.geocoder ? `${sh.geocoder.mode}, ${formatNumber(sh.geocoder.cache_size ?? 0)} cached` : DASH],
      ['read one log', ms(t.decode_per_log)],
      ['rebuild window', ms(t.rebuild_per_window)],
      ['typical answer', ms(t.api_p50)],
      ['slow answer', status(slow ? warn() : ok(), ms(t.api_p95), none(' 95 of 100 are faster'))],
      ['this reading', ms(sh.read_ms)],
    ]));
    const now = ctx.api.now();
    const lines = [];
    if (sh.uptime_ms != null) lines.push({ ms: now - sh.uptime_ms, tag: 'ok', tone: 'good', text: `server ${v.server || ''} started` });
    if (sh.housekeeping_last_ms != null) lines.push({ ms: sh.housekeeping_last_ms, tag: 'ok', tone: 'good', text: 'housekeeping done' });
    if (sh.tesla?.last_error?.at_ms) lines.push({ ms: sh.tesla.last_error.at_ms, tag: 'warn', tone: 'warn', text: `tesla api: ${lastErrorText(sh.tesla.last_error)}` });
    if (geo?.online?.last_error) lines.push({ ms: now - (geo.online.retry_ms || 0), tag: 'warn', tone: 'warn', text: `address look-up: ${geo.online.last_error}` });
    lines.push({ ms: now, tag: memPct > 85 ? 'warn' : 'ok', tone: memPct > 85 ? 'warn' : 'good', text: `upload memory ${format(memPct, 'percent', { digits: 0 })} used` });
    lines.push({ ms: now, tag: slow ? 'warn' : 'ok', tone: slow ? 'warn' : 'good', text: `api answers p50 ${ms(t.api_p50)}, p95 ${ms(t.api_p95)}` });
    replace(tailSlot, logTail(lines, tz));
  }

  /* ---------- connections: the totals by status, and the way to the overview */
  const connSlot = h('div', null, admin ? loadingLine('reading the connections') : null);
  let connTot = null;
  async function loadConn() {
    if (!admin) { replace(connSlot, cover(locked('the connections overview'), 'conn.overview.link')); return; }
    let ov = null;
    try { ov = await ctx.api.get('integrations/overview'); } catch (e) { if (e.status !== 403 && e.status !== 404) throw e; }
    if (!ctx.alive()) return;
    const go = link(state.buildUrl('settings-integrations'), 'connections overview');
    if (!ov) { replace(connSlot, h('p', { cover: 'conn.overview.link' }, go, none('  the overview could not be read'))); return; }
    if (!connTot) {
      connTot = totalsLine(ov.totals);
      replace(connSlot, h('div', { cover: 'conn.overview.link' }, connTot.el,
        h('p', null, go, none(ov.totals?.problem ? `  ${formatNumber(ov.totals.problem)} with a problem now: see what went wrong` : '  every connection the server keeps, how it is doing and what went wrong'))));
    } else connTot.set(ov.totals);
  }

  /* ---------- every car */
  const carSearch = h('input', { class: 'in in--search', type: 'search', placeholder: 'car name', 'aria-label': 'Find a car', spellcheck: 'false' });
  const cars = table(ctx, {
    title: 'every car', path: 'cars', params: {}, limit: 50, csvName: 'cars data status', cover: 'faq.stopped-recording', sort: { key: 'name', dir: 'asc' },
    side: h('label', { class: 'filt filt--inline' }, h('span', { class: 'filt-p' }, 'find>'), carSearch),
    note: 'each car you may see; open one for its gaps, keys, totals, storage and signals.',
    rowHref: (c) => state.buildUrl('health', { car: c.id }),
    columns: [
      { key: 'name', label: 'car', sort: 'name', ph: 'main', grow: true, render: (c) => [link(state.buildUrl('health', { car: c.id }), c.name || c.id), none(` ${[c.model, c.year].filter(Boolean).join(' ')}`)] },
      { key: 'state', label: 'now', ph: 'sub', render: (c) => stateWord(c.now?.state || 'unknown', { stale: c.now?.stale }) },
      { key: 'from', label: 'inputs', title: 'what the car has data from: smo, api (the Tesla API), imp (imported)', render: (c) => src(c) || none(DASH), value: (c) => srcText(c) },
      { key: 'last', label: 'last data', num: true, sort: 'last_activity', render: (c) => (c.now?.received_ms != null ? ago(c.now.received_ms) : none('none yet')) },
      { key: 'status', label: 'data', ph: 'num', render: (c) => {
        const b = (c.attention || []).filter((a) => a === 'upload_silence' || a === 'trust_failed');
        return b.length ? b.map((a, i) => [i ? ' ' : null, warn(), ' ', REASONS[a][0]]) : [ok(), ' arriving'];
      } },
    ],
    empty: () => empty(carSearch.value ? `no car name holds "${carSearch.value}"` : 'no cars yet'),
  });
  let carT = 0;
  ctx.on(carSearch, 'input', () => { clearTimeout(carT); carT = setTimeout(() => ctx.alive() && cars.reload({ search: carSearch.value.trim() || undefined }), 250); });
  ctx.onCleanup(() => clearTimeout(carT));

  /* ---------- the health address and the API errors */
  const clock = h('p', { class: 'note', cover: 'faq.docker-timestamps' },
    `server clock ${new Date(ctx.api.now()).toISOString().replace('T', ' ').slice(0, 19)} UTC. times are stored in UTC and shown in the car’s zone (here ${tz}). the container’s log lines are UTC unless its TZ variable says otherwise.`);
  const addr = section({ title: 'health address', cover: 'web.health-api.health-check' },
    note('GET /health answers 200 with no body while the server serves. no sign-in, tells nothing about the cars, so a monitor or the container check can call it freely.'),
    h('pre', { class: 'mono-box' }, 'HEALTHCHECK --interval=30s CMD ["/sentry-mate", "health"]\n# or any monitor: GET https://<your server>/health  ->  200'));
  const tryOut = h('div', { 'aria-live': 'polite' });
  const tryBtn = btn('[ask for a car that does not exist]', () => tryUnknown(), { kind: 'line', cover: 'web.health-api.unknown-car' });
  async function tryUnknown() {
    tryBtn.disabled = true;
    try {
      await ctx.api.get('cars/no-such-car-0');
      replace(tryOut, h('p', null, bad(), ' the server answered with a car; that should not happen.'));
    } catch (e) {
      if (!ctx.alive()) return;
      replace(tryOut, h('pre', { class: 'mono-box' }, `${e.status} ${JSON.stringify({ error: { code: e.code, message: e.message } }, null, 2)}`),
        note('a car you may not see gives exactly this answer, so nobody can find out which ids exist.'));
    } finally { tryBtn.disabled = false; }
  }
  const errs = table(ctx, {
    title: 'api errors', rows: API_ERRORS.map(([st, code, text]) => ({ st, code, text })), csv: false, cover: 'web.health-api.errors', sort: { key: 'st', dir: 'asc' },
    side: tryBtn,
    note: 'every /api/v1 call that fails answers { "error": { "code", "message", "fields" } }: code for programs, message for people, fields names each bad input by its path.',
    columns: [
      { key: 'st', label: 'status', num: true, sort: true, ph: 'num' },
      { key: 'code', label: 'code', sort: true, ph: 'main' },
      { key: 'text', label: 'when', ph: 'sub', grow: true, cls: 'wrap-cell', render: (r) => none(r.text) },
    ],
  });

  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'system' }, cover: 'shots.database-info' },
    hd, toolLine,
    section({ title: 'connections' }, connSlot),
    section({ title: 'storage', cover: 'dbinfo.section.database' }, storeSlot),
    section({ title: 'server' }, srvSlot), tailSlot, clock,
    cars.el,
    h('p', { class: 'note', cover: ['nr.dash.state-table', 'nr.dash.state-table.parameter', 'nr.dash.state-table.value', 'nr.dash.state-table.sort-scroll'] },
      'every decoded signal of a car, with its value and a chart, is on that car’s health page: open a car above.'),
    addr, errs.el, tryOut));

  async function reloadAll(say) {
    reloadBtn.disabled = true;
    try {
      await Promise.all([loadStorage(), loadServer(), loadConn(), cars.reload()]);
      readAt = ctx.api.now(); paintRead();
      if (say && ctx.alive()) toast('read again', { kind: 'info' });
    } catch (e) { if (ctx.alive()) toast(errorWords(e).text, { kind: 'error' }); } finally { reloadBtn.disabled = false; }
  }

  try { await Promise.all([loadStorage(), loadServer(), loadConn(), cars.ready]); } catch (e) { if (ctx.alive()) ctx.fail(e); return; }
  if (!ctx.alive()) return;
  ctx.ready();
}
