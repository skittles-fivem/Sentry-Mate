// Data health and storage, on two routes.
// - /cars/{car}/health: the car's warning-light panel (is data arriving and trusted: last upload, last log
//   check, logs waiting, unknown time, chain gaps, dropped frames), what to do about each problem, its
//   CAN Bridge keys, unknown spans and a rebuild (admin); the gaps of the period; what is recorded (trip
//   counters per source and the rows kept); what its data takes on disk (by kind and by day); and every
//   decoded signal of its newest log, with a chosen signal drawn over the period.
// - /system: the server's storage (total, free, growth, full in), every car's data status, the server's own
//   health (admin), the /health address for container checks and the one error shape of the API.
// API: cars/{car}/health, gaps, totals?source=, updates (summary), storage, signals/latest, signals;
// POST cars/{car}/rebuild; system/storage, system/health, geocode/status, cars. "rows" events reload.
// Admins also see what the server does for the car through the Tesla API (integrations/tesla/status), and the
// whole server's Tesla input on /system. Rows say where their data came from (SMO, the Tesla API, imported).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format, convert, unitLabel } from '../shared/units.js';
import { column, timeColumn, downloadCsv, csvFilename, rowsFromColumns } from '../shared/csv.js';
import { formatDateTime, formatDuration, formatDayKey, dayStart, HOUR } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, plate, btn, linkBtn, tag, kv, note, bar, lamp } from '../core/ui.js';
import { controls, slot, periodBtn, carBtn, counters, eq, csvBtn, band, search, plural } from '../core/kit.js';
import { lights } from '../core/gauges.js';
import { tracks } from '../core/tracks.js';
import { scope, columns as colChart, well } from '../core/chart.js';
import { emptyState, errorWords } from '../core/states.js';
import { confirmDialog, toast } from '../core/layer.js';
import { agoEl, stateWord, DASH } from '../core/fmt.js';
import { isAdmin } from '../core/prefs.js';
import { srcChip, sourceLabel } from '../core/sources.js';
import { statusTable, requestsText, lastErrorText } from '../core/tesla.js';

export const id = 'health';
export const css = true;
export const title = (ctx) => (ctx.car ? 'Data health' : 'Server and storage');

const enc = encodeURIComponent;
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const bytes = (v) => format(v, 'bytes');
const took = (ms) => (ms == null ? DASH : ms < 1000 ? `${formatNumber(Math.round(ms))} ms` : formatDuration(ms));
// health and signals fall back to a short window without from_ms: All time sends 0
const fullPeriod = (p) => ({ ...p, from_ms: p?.from_ms ?? 0 });

const REASONS = {
  upload_silence: { title: 'No log has arrived for a while', text: 'The CAN Bridge has not sent a log lately. Check that it has power and a network, and that its key below still uploads. Nothing is lost while the car sleeps; logs kept on the bridge come in when it is back.', fix: 'Check the keys' },
  trust_failed: { title: 'The last log failed its check', text: 'Its signature or hash chain did not match, so its data is kept apart and not used. Pair the bridge again if its key changed.', fix: 'Pair the bridge again' },
  logs_waiting_trust: { title: 'Logs wait for a key to be trusted', text: 'They came from a key nobody has approved yet. Approve or assign the key and they are read at once.', fix: 'Approve the key' },
  rebuild_pending: { title: 'Drives and charges are being rebuilt', text: 'They are worked out again from the stored logs. This page reads the figures again every few seconds until it is done.', fix: null },
};
const TRUST = {
  verified: ['Verified', 'Signed by a trusted key and in the hash chain.'],
  unsigned: ['Unsigned', 'Sent without a signature: its data is used but cannot be proven.'],
  failed: ['Failed', 'Its signature or chain did not match: its data is not used.'],
};
const TRUST_REASONS = { signature_mismatch: 'the signature does not match the key', chain_break: 'it does not follow the log before it', unknown_key: 'the key is not known', bad_format: 'the file could not be read' };
const SPAN_REASONS = { no_log: 'no log arrived', dropped_frames: 'the bridge dropped frames', failed_check: 'the log failed its check', chain_gap: 'a log is missing from the chain', paused: 'recording was paused' };
const GAP_CAUSES = { no_log: 'No log', not_recorded: 'Not recorded', chain_gap: 'Chain gap', unknown: 'Unknown' };
const KIND_TEXT = {
  logs: 'CAN logs as the bridge sent them. Everything else can be rebuilt from these.', series: 'Values over time, used by charts.',
  curves: 'Power, volts and amps of every charge, second by second.', rows: 'One row per drive, charge, state and stop.',
  imported: 'History brought in from TeslaMate.', tiles: 'Map tiles kept on this server.', places: 'Places, prices and settings.', geocode: 'Addresses already looked up.',
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
  [400, 'bad_input', 'A value has the wrong type, a field is unknown or a filter is not valid; fields names each one.'],
  [401, 'not_signed_in', 'No session, or it ended. Sign in again.'],
  [403, 'csrf', 'A change came without the X-Sentry-Mate header, so it may not be from this app.'],
  [403, 'not_allowed', 'The account may read this but not change it, or it is for admins only.'],
  [404, 'no_such_car', 'No such car, or this account may not see it. Both answer the same, so hidden cars cannot be found by trying ids.'],
  [404, 'not_found', 'No such address, or a row that is not there (each kind has its own no_such_ code).'],
  [405, 'method_not_allowed', 'This address does not take that method; the Allow header lists the ones it does.'],
  [409, 'stale_version', 'Someone changed it first. The answer carries the current version; load and try again.'],
  [409, 'import_running', 'Another import is not finished yet.'],
  [413, 'too_large', 'The body is over 1 MB.'],
  [422, 'invalid', 'Well formed but against a rule (an unknown time zone, a password too short); fields names each one.'],
  [429, 'throttled', 'Too many tries. Retry-After says how many seconds to wait.'],
  [500, 'server_error', 'Something broke on the server. Nothing was changed.'],
];

export async function mount(ctx) {
  return ctx.car ? mountCar(ctx) : mountSystem(ctx);
}

const when = (ctx, ms) => (ms == null ? DASH : formatDateTime(ms, ctx.tz, { now: ctx.api.now() }));
const timelineAt = (car, from, to) => state.buildUrl('timeline', { car, period: { from: from - HOUR, to: to + HOUR } });

/** The stored kinds as equaliser bars, with the total under them. */
function kindsPanel(kinds, total, { title, side }) {
  const list = (kinds || []).filter((k) => k.bytes != null).sort((a, b) => b.bytes - a.bytes);
  const max = Math.max(1, ...list.map((k) => k.bytes));
  return hud({ title, icon: 'server', cover: ['dbinfo.table.sizes', 'dbinfo.sizes.table', 'dbinfo.sizes.data'], side: [side, csvBtn({ name: ['sentry-mate', 'storage-by-kind'], rows: list, columns: [column('What', 'label'), column('Kind', 'kind'), column('Bytes', 'bytes')] })] },
    eq(list.map((k) => ({ label: k.label || k.kind, value: k.bytes / max, text: bytes(k.bytes), title: KIND_TEXT[k.kind] || null })), { label: title, empty: 'Nothing stored yet.' }),
    h('div', { class: 'hl-total', cover: 'dbinfo.sizes.total' }, h('span', { class: 'stat-label' }, h('span', null, 'All kinds together')), h('span', { class: 'hl-total-v' }, bytes(total))));
}

/* ================================================================== one car */

async function mountCar(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const admin = isAdmin();
  const base = `cars/${enc(cid)}`;

  /* ---------- the warning-light panel */
  const lightsEl = h('div');
  const healthBody = h('div', { class: 'hl-body' });
  let pendingTimer = 0, wasPending = false;
  ctx.onCleanup(() => clearTimeout(pendingTimer));
  async function loadHealth() {
    const hl = await ctx.api.get(`${base}/health`, fullPeriod(ctx.params));
    if (!ctx.alive()) return;
    paintHealth(hl);
    clearTimeout(pendingTimer);
    if (hl.rebuild_pending) pendingTimer = setTimeout(() => { if (ctx.alive()) loadHealth().catch(() => {}); }, 3000);
    else if (wasPending) { gaps.reload(); loadTotals().catch(() => {}); }
    wasPending = !!hl.rebuild_pending;
  }
  function paintHealth(hl) {
    const trust = hl.last_log_trust || {};
    const tw = TRUST[trust.result];
    const spans = hl.unknown_spans || [];
    const reasons = hl.reasons || [];
    const silent = reasons.includes('upload_silence');
    replace(lightsEl, lights([
      { icon: 'upload', text: 'Upload', on: silent, tone: 'red', title: silent ? REASONS.upload_silence.title : 'Logs arrive' },
      { icon: 'lock', text: 'Log check', on: trust.result === 'failed' || trust.result === 'unsigned', tone: trust.result === 'failed' ? 'red' : undefined, title: tw ? tw[1] : 'No log checked yet' },
      { icon: 'clock', text: 'Waiting', on: !!hl.logs_waiting, title: `${formatNumber(hl.logs_waiting || 0)} logs waiting for a trusted key` },
      { icon: 'signal', text: 'Unknown time', on: spans.length > 0, tone: 'cyan', title: 'Time in the period with no usable data' },
      { icon: 'timeline', text: 'Chain gaps', on: (hl.chain_gaps_total ?? (hl.chain_gaps || []).length) > 0, title: 'Logs missing from the hash chain' },
      { icon: 'pulse', text: 'Dropped frames', on: (hl.dropped_frames_total ?? (hl.dropped_frames || []).length) > 0, title: 'The bridge lost CAN frames' },
      { icon: 'arrow-r', text: 'Rebuild', on: !!hl.rebuild_pending, tone: 'grn', title: hl.rebuild_pending ? 'A rebuild is running' : 'Nothing pending' },
    ]));
    const keys = hl.keys || [];
    replace(healthBody,
      h('div', { class: ['hl-verdict', hl.healthy ? 'is-ok' : 'is-warn'], role: 'status' },
        icon(hl.healthy ? 'check' : 'warn', 'i--lg'),
        h('div', null, h('p', { class: 'hl-verdict-head' }, hl.healthy ? 'Data is arriving and checks out' : `${reasons.length === 1 ? 'One thing needs' : `${reasons.length} things need`} a look`),
          h('p', { class: 'hl-verdict-text' }, 'The same test sets "healthy" in MQTT. A car that sleeps sends nothing; that is not a fault.'))),
      reasons.length ? h('ul', { class: 'hl-fixes' }, reasons.map((code) => {
        const r = REASONS[code] || { title: code, text: '' };
        return h('li', { class: 'hl-fix' }, icon('warn', 'i--sm'), h('div', null, h('p', { class: 'hl-fix-head' }, r.title), h('p', { class: 'hl-fix-text' }, r.text),
          r.fix ? linkBtn(r.fix, state.buildUrl('settings-cars', { car: cid }), { sm: true, icon: 'arrow-r' }) : null));
      })) : null,
      counters([
        { label: 'Last upload', text: hl.last_upload_ms != null ? agoEl(hl.last_upload_ms) : '—', how: hl.last_upload_ms != null ? when(ctx, hl.last_upload_ms) : 'pair a CAN Bridge in Car settings' },
        { label: 'Last log check', text: tw ? tw[0] : '—', how: tw ? `${trust.reason ? TRUST_REASONS[trust.reason] || trust.reason : tw[1]}` : 'no log checked yet' },
        { label: 'Logs waiting', value: hl.logs_waiting ?? 0, kind: 'count', how: hl.logs_waiting ? 'their key is not trusted yet' : 'every log was read' },
        { label: 'Unknown time', text: hl.unknown_ms != null ? formatDuration(hl.unknown_ms) : '—', how: `${plural(hl.unknown_spans_total ?? spans.length, 'span')} in the period` },
        { label: 'Chain gaps', value: hl.chain_gaps_total ?? (hl.chain_gaps || []).length, kind: 'count', how: 'logs missing from the chain' },
        { label: 'Dropped frames', value: hl.dropped_frames_total ?? (hl.dropped_frames || []).length, kind: 'count', how: 'times the bridge fell behind' },
      ]),
      h('div', { class: 'cols hl-cols' },
        h('div', { class: 'stack' },
          h('h3', { class: 'hl-sub' }, 'CAN Bridge keys'),
          keys.length ? h('ul', { class: 'hl-keys' }, keys.map((k) => h('li', { class: 'hl-key' },
            icon('lock', 'i--sm'),
            h('span', { class: 'cell' }, h('span', { class: 'cell-main' }, k.device || 'Unnamed device', k.preferred ? [' ', tag('Preferred', 'grn')] : null), h('span', { class: 'cell-sub mono ell' }, k.key)),
            h('span', { class: 'cell hl-key-r' }, k.last_upload_ms != null ? agoEl(k.last_upload_ms) : h('span', { class: 'faint' }, 'never uploaded'),
              h('span', { class: ['cell-sub', k.logs_waiting && 'warn'] }, k.logs_waiting ? `${formatNumber(k.logs_waiting)} waiting` : 'none waiting')))))
            : note('No key is tied to this car yet. Pair a CAN Bridge in Car settings.'),
          kv([['Uploads', hl.uploads_enabled ? h('span', { class: 'ok' }, 'Accepted') : h('span', { class: 'warn' }, 'Turned off for this car')], ['Rebuild', hl.rebuild_pending ? h('span', { class: 'warn' }, 'Running') : 'Nothing pending']])),
        h('div', { class: 'stack' },
          h('h3', { class: 'hl-sub' }, 'Unknown time in the period'),
          spans.length ? h('ul', { class: 'hl-spans' }, spans.slice(0, 12).map((s) => h('li', null,
            h('a', { class: 'plc', href: timelineAt(cid, s.start_ms, s.end_ms ?? ctx.api.now()) }, when(ctx, s.start_ms)),
            h('span', { class: 'faint' }, s.end_ms == null ? ' · still unknown' : ` · ${formatDuration(s.end_ms - s.start_ms)}`),
            h('span', null, ` · ${SPAN_REASONS[s.reason] || s.reason || 'no reason given'}`))))
            : note('None: every moment of the period is known (driving, charging, parked or asleep).'))),
      admin ? h('div', { class: 'hl-rebuild' }, btn(`Rebuild ${period.label.toLowerCase()}`, (e) => rebuildNow(e.currentTarget), { icon: 'arrow-r', sm: true }),
        h('span', { class: 'note' }, 'Nothing is repaired by hand: drives, charges, states and stops are worked out again from the stored logs.'))
        : note('An admin can rebuild drives and charges from the stored logs.'));
  }
  async function rebuildNow(b) {
    const ok = await confirmDialog(`Work out the drives, charges, states and stops of ${car.name} again from the stored logs for ${period.label.toLowerCase()}? Typed names and prices stay.`, { title: 'Rebuild from logs', ok: 'Rebuild' });
    if (!ok || !ctx.alive()) return;
    b.disabled = true;
    try {
      const r = await ctx.api.post(`${base}/rebuild`, { from_ms: ctx.params.from_ms ?? 0, to_ms: ctx.params.to_ms ?? ctx.api.now() });
      if (!ctx.alive()) return;
      toast(`Rebuilt from ${plural(r.logs_read ?? 0, 'log')} in ${took(r.took_ms ?? 0)}.`);
      reloadAll(false);
    } catch (e) { if (ctx.alive()) toast(errorWords(e).text, { kind: 'error' }); } finally { b.disabled = false; }
  }

  /* ---------- gaps */
  const gaps = tracks(ctx, {
    title: 'Gaps in the period', icon: 'timeline', tone: 'orange', path: `${base}/gaps`, params: ctx.params, csvName: 'gaps', csv: { all: true }, dense: true,
    note: 'Times with no usable data, newest first. A gap in which the odometer moved means a drive was not recorded.',
    columns: [
      { key: 'start', label: 'From', lead: true, render: (g) => h('a', { class: 'cell-main', href: timelineAt(cid, g.start_ms, g.end_ms ?? ctx.api.now()) }, when(ctx, g.start_ms)), csv: (g) => iso(g.start_ms) },
      { key: 'dur', label: 'Lasted', num: true, render: (g) => (g.end_ms == null ? tag('Still open', 'org') : formatDuration(g.end_ms - g.start_ms)), csv: (g) => (g.end_ms == null ? '' : Math.round((g.end_ms - g.start_ms) / 60000)) },
      { key: 'cause', label: 'Cause', render: (g) => h('span', { class: 'cell' }, h('span', null, GAP_CAUSES[g.cause] || g.cause), g.gap_reason ? h('span', { class: 'cell-sub' }, SPAN_REASONS[g.gap_reason] || g.gap_reason) : null), csv: (g) => GAP_CAUSES[g.cause] || g.cause },
      { key: 'jump', label: 'Odometer moved', num: true, render: (g) => format(g.odometer_jump_km, 'distance'), csv: column('Odometer moved', 'odometer_jump_km', 'distance') },
      { key: 'where', label: 'Where', render: (g) => h('span', { class: 'cell' }, g.from_place?.name || 'Unknown place', g.to_place && g.to_place.id !== g.from_place?.id ? h('span', { class: 'cell-sub' }, `to ${g.to_place.name}`) : null), csv: (g) => g.from_place?.name || '' },
      { key: 'source', label: 'Source', render: (g) => srcChip(g) || DASH, csv: (g) => sourceLabel(g) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No gaps in this period', text: 'Every moment is accounted for. Pick a longer period to look further back.' }),
  });

  /* ---------- the Tesla API input for this car (admins) */
  const teslaSlot = h('div');
  async function loadTesla() {
    if (!admin) return;
    let rows = [], accounts = [];
    try {
      [rows, accounts] = await Promise.all([ctx.api.list('integrations/tesla/status').all(), ctx.api.list('integrations/tesla/accounts').all().catch(() => [])]);
    } catch (e) { if (e.name === 'AbortError') throw e; replace(teslaSlot, note('What the server does through the Tesla API could not be read.')); return; }
    if (!ctx.alive()) return;
    const mine = rows.filter((r) => r.car === cid);
    if (!mine.length) {
      replace(teslaSlot, h('p', { class: 'note hl-tesla-none' }, icon('antenna', 'i--sm'), h('span', null, `${car.name} is not read through the Tesla API: no car of a Tesla account is matched to it. `,
        h('a', { class: 'plc', href: `${state.buildUrl('settings-integrations')}#tesla` }, accounts.length ? 'Match it under Tesla account' : 'Add a Tesla account'))));
      return;
    }
    const label = (id) => accounts.find((a) => a.id === id)?.label || id;
    const t = statusTable(ctx, mine, { carName: () => car.name, accountLabel: label, title: 'Tesla API: this car now', tz,
      note: 'The second live input beside SMO. The server never wakes the car: while it sleeps only the account\'s car list is read.' });
    replace(teslaSlot, t.el);
    await t.ready;
  }

  /* ---------- what is recorded */
  const totalsEl = h('div');
  const rowsSlot = h('div');
  async function loadTotals() {
    const [all, can, ta, tm, upd] = await Promise.all([
      ctx.api.get(`${base}/totals`, { source: 'all' }), ctx.api.get(`${base}/totals`, { source: 'smo' }),
      ctx.api.get(`${base}/totals`, { source: 'tesla_api' }), ctx.api.get(`${base}/totals`, { source: 'import' }),
      ctx.api.get(`${base}/updates`, { limit: 1 }).catch(() => null),
    ]);
    if (!ctx.alive()) return;
    const sm = upd?.summary || {};
    const split = (a, t, b, kind) => `${format(a ?? 0, kind)} from SMO, ${format(t ?? 0, kind)} from the Tesla API, ${format(b ?? 0, kind)} imported`;
    replace(totalsEl, counters([
      { label: 'Logged distance', value: all.distance_km, kind: 'distance', cover: 'dbinfo.stat.logged_distance', how: split(can.distance_km, ta.distance_km, tm.distance_km, 'distance') },
      { label: 'Odometer', value: all.odometer_last_km, kind: 'odometer', cover: 'dbinfo.stat.odometer', how: all.odometer_last_ms != null ? `read ${when(ctx, all.odometer_last_ms)}` : 'not sent yet' },
      { label: 'Not logged', value: all.not_logged_km, kind: 'distance', how: 'driven while nothing was recorded' },
      { label: 'Drives', value: all.drives, kind: 'count', cover: 'dbinfo.stat.drive_count', how: split(can.drives, ta.drives, tm.drives, 'count') },
      { label: 'Charges', value: all.charges, kind: 'count', cover: 'dbinfo.stat.charge_count', how: split(can.charges, ta.charges, tm.charges, 'count') },
      { label: 'Updates', value: all.updates, kind: 'count', cover: 'dbinfo.stat.update_count', how: split(can.updates, ta.updates, tm.updates, 'count') },
      { label: 'Version', text: sm.current_version || '—', cover: 'dbinfo.stat.current_version', how: sm.current_version ? (sm.build_changed_since != null ? 'changed since; the new one has no name' : 'the last named version') : 'never guessed: import, type or match it' },
    ]));
    const rows = [
      { kind: 'Drives', what: 'One row per drive', can: can.drives, tesla: ta.drives, teslamate: tm.drives, total: all.drives },
      { kind: 'Charges', what: 'One row per charge', can: can.charges, tesla: ta.charges, teslamate: tm.charges, total: all.charges },
      { kind: 'Software updates', what: 'One row per update', can: can.updates, tesla: ta.updates, teslamate: tm.updates, total: all.updates },
    ].map((r) => ({ ...r, total: r.total ?? (r.can || 0) + (r.tesla || 0) + (r.teslamate || 0) }));
    const t = tracks(ctx, {
      title: 'Rows kept', icon: 'book', rows, csvName: 'rows-kept', cover: 'dbinfo.table.row_counts', sort: { key: 'total', dir: 'desc' }, dense: true,
      note: 'Exact counts, kept up to date as rows change. A row SMO and the Tesla API both saw counts in both columns, once in Rows. Where imported and live history overlap, both are counted.',
      columns: [
        { key: 'kind', label: 'Kind', sort: true, lead: true, cover: 'dbinfo.rows.table_name', value: (r) => r.kind, render: (r) => h('span', { class: 'cell' }, h('span', { class: 'cell-main' }, r.kind), h('span', { class: 'cell-sub' }, r.what)) },
        { key: 'can', label: 'From SMO', num: true, sort: true, render: (r) => formatNumber(r.can ?? 0) },
        { key: 'tesla', label: 'From the Tesla API', num: true, sort: true, render: (r) => formatNumber(r.tesla ?? 0) },
        { key: 'teslamate', label: 'Imported', num: true, sort: true, render: (r) => formatNumber(r.teslamate ?? 0) },
        { key: 'total', label: 'Rows', num: true, sort: true, cover: 'dbinfo.rows.row_count', render: (r) => h('b', null, formatNumber(r.total)) },
      ],
    });
    replace(rowsSlot, t.el);
  }

  /* ---------- storage */
  const storageSlot = h('div', { class: 'stack' });
  async function loadStorage() {
    const st = await ctx.api.get(`${base}/storage`, { limit: 1 });
    if (!ctx.alive()) return;
    const labels = Object.fromEntries((st.kinds || []).map((k) => [k.kind, k.label]));
    const days = tracks(ctx, {
      title: 'Stored by day', icon: 'calendar', path: `${base}/storage`, params: {}, limit: 12, csvName: 'storage-by-day', csv: { all: true }, dense: true,
      note: 'Newest first, only days with data. Logs are the car\'s own days; series and curves are kept per UTC day.',
      columns: [
        { key: 'date', label: 'Day', lead: true, render: (r) => h('a', { class: 'cell-main', href: state.buildUrl('timeline', { car: cid, period: { from: dayStart(r.date, tz), to: dayStart(r.date, tz) + 24 * HOUR } }) }, formatDayKey(r.date)), csv: (r) => r.date },
        { key: 'series', label: 'What', render: (r) => labels[r.series] || r.series, csv: (r) => r.series },
        { key: 'source', label: 'Source', render: (r) => srcChip(r, { full: true }) || DASH, csv: (r) => sourceLabel(r) },
        { key: 'bytes', label: 'On disk', num: true, render: (r) => bytes(r.bytes), csv: (r) => r.bytes },
      ],
      empty: () => emptyState({ icon: 'calendar', title: 'No stored days yet', text: 'A day shows here once a log for it has arrived.' }),
    });
    replace(storageSlot, h('div', { class: 'cols' }, kindsPanel(st.kinds, st.total_bytes, { title: `What ${car.name} takes` }), days.el));
    await days.ready;
  }

  /* ---------- signals */
  let signals = [], chosen = null, needle = '';
  const sigInfo = h('span', { class: 'trk-count' });
  const sigList = tracks(ctx, {
    title: 'Signals from the newest log', icon: 'pulse', rows: [], csvName: 'signals', cover: ['nr.dash.state-table', 'nr.dash.state-table.sort-scroll'], cls: 'hl-sigs', dense: true,
    side: search('', (v) => { needle = v; filterSignals(); }, { placeholder: 'Find a signal, message or value', ctx }),
    sort: { key: 'name', dir: 'asc' },
    note: 'Every signal decoded from the newest log. Sort by any heading (numbers sort as numbers); the list scrolls inside. Plot one to see it over the period.',
    columns: [
      { key: 'name', label: 'Signal', sort: true, lead: true, cover: 'nr.dash.state-table.parameter', value: (s) => s.comment || s.series,
        render: (s) => h('span', { class: 'cell' }, h('span', { class: 'cell-main' }, s.comment || s.series), h('span', { class: 'cell-sub mono' }, s.series)), csv: (s) => s.series },
      { key: 'message', label: 'Message', sort: true, value: (s) => `${s.bus} ${s.message_id}`, render: (s) => h('span', { class: 'cell' }, h('span', { class: 'mono' }, s.message_id), h('span', { class: 'cell-sub' }, s.bus === 'chassis' ? 'chassis bus' : s.bus === 'vehicle' ? 'vehicle bus' : s.bus)), csv: (s) => `${s.bus} ${s.message_id}` },
      { key: 'value', label: 'Value', num: true, sort: true, firstDir: 'desc', cover: 'nr.dash.state-table.value',
        value: (s) => (s.value_name ? s.value_name : s.value == null ? null : signalKind(s) ? convert(s.value, signalKind(s)) : s.value),
        render: (s) => h('span', { class: 'cell' }, h('span', { class: 'hl-sig-v' }, signalText(s)), s.value_name ? h('span', { class: 'cell-sub' }, `code ${s.value}`) : null), csv: (s) => signalText(s) },
      { key: 'raw', label: 'As sent', num: true, render: (s) => h('span', { class: 'faint' }, s.value == null ? DASH : `${formatNumber(s.value, { maximumFractionDigits: 6 })}${s.unit ? ` ${s.unit}` : ''}`), csv: (s) => s.value },
      { key: 'at', label: 'Read', num: true, sort: true, value: (s) => s.at_ms, render: (s) => (s.at_ms != null ? agoEl(s.at_ms) : DASH), csv: (s) => iso(s.at_ms) },
      { key: 'plot', label: '', cls: 'is-act', csv: false, render: (s) => (s.has_series
        ? btn('Plot', () => plot(s), { sm: true, kind: chosen === s.series ? 'go' : 'ghost', icon: 'stats', title: 'Draw this signal over the period' })
        : h('span', { class: 'faint', title: 'A signal with named values has its newest value only.' }, 'latest only')) },
    ],
    empty: () => emptyState({ icon: 'search', title: signals.length ? 'No signal matches' : 'No signals yet', text: signals.length ? `Nothing holds "${needle}". Clear the search to see all ${signals.length}.` : 'Signals show once the first log of this car has been decoded.' }),
  });
  function filterSignals() {
    const words = needle.toLowerCase().split(/\s+/).filter(Boolean);
    const rows = !words.length ? signals : signals.filter((s) => { const hay = [s.series, s.comment, s.message_id, s.bus, s.unit, s.value_name, signalText(s)].filter(Boolean).join(' ').toLowerCase(); return words.every((w) => hay.includes(w)); });
    sigList.setRows(rows);
    sigInfo.textContent = words.length ? `${formatNumber(rows.length)} of ${formatNumber(signals.length)}` : plural(signals.length, 'signal');
  }
  async function loadSignals() {
    signals = await ctx.api.list(`${base}/signals/latest`, {}, { limit: 500 }).all({ max: 20000 });
    if (ctx.alive()) filterSignals();
  }
  const plotSlot = h('div', { class: 'hl-plot' }, emptyState({ icon: 'stats', title: 'Pick a signal to plot', text: 'Press Plot on a signal with values over time. It is drawn over the period in your units, with gaps where nothing is known.' }));
  async function loadSignal(series) {
    const get = (p, max) => ctx.api.get(`${base}/signals`, { series, ...p, max_points: max });
    if (ctx.params?.from_ms != null) return get(ctx.params, 1500);
    const coarse = await get(fullPeriod(ctx.params), 400);
    const first = (coarse.values?.[series] || []).findIndex((v) => v != null);
    return first < 0 ? coarse : get({ ...ctx.params, from_ms: coarse.t_ms[first] }, 1500);
  }
  async function plot(s) {
    chosen = s.series;
    filterSignals();
    const kind = signalKind(s);
    replace(plotSlot, h('p', { class: 'note' }, icon('clock', 'i--sm'), h('span', null, `Loading ${s.comment || s.series}…`)));
    let data;
    try { data = await loadSignal(s.series); } catch (e) {
      if (!ctx.alive() || chosen !== s.series) return;
      const w = errorWords(e); replace(plotSlot, emptyState({ icon: 'warn', title: w.title, text: w.text })); return;
    }
    if (!ctx.alive() || chosen !== s.series) return;
    const values = data.values?.[s.series] || [];
    const known = values.some((v) => v != null);
    const read = h('span', { class: 'scope-read' });
    const box = well('well--lg', s.comment || s.series);
    replace(plotSlot,
      h('div', { class: 'scope-head' }, h('span', { class: 'scope-title' }, `${s.comment || s.series}${kind ? ` · ${unitLabel(kind)}` : s.unit ? ` · ${s.unit}` : ''}`), h('span'), h('span', { class: 'row' }, read,
        csvBtn({ name: [car.name, 'signal', s.series], rows: () => rowsFromColumns(data), columns: [timeColumn('Time', 't_ms', tz), kind ? column(s.comment || s.series, s.series, kind) : column(s.comment || s.series, s.series)] }))),
      known ? box : emptyState({ icon: 'moon', title: 'Nothing recorded in this period', text: 'The car was asleep or not recording. Pick a longer period.' }),
      note(`Each point is the mean of ${formatDuration(data.bucket_ms || 0)}; the line breaks where nothing is known.`));
    if (!known) return;
    await scope(box, ctx, { tz, t_ms: data.t_ms, series: [{ label: s.comment || s.series, values, kind, color: '--cyan', fill: true }], axes: { y: { digits: kind === 'pressure' ? 2 : 1 } },
      onCursor: (i) => { read.textContent = i == null ? '' : `${when(ctx, data.t_ms[i])} · ${values[i] == null ? 'not known' : kind ? format(values[i], kind) : `${formatNumber(values[i], { maximumFractionDigits: 3 })} ${s.unit || ''}`}`; } });
  }

  /* ---------- page */
  const reloadBtn = btn('Read again', () => reloadAll(true), { icon: 'arrow-r', kind: 'ghost', sm: true, cover: 'dbinfo.control.refresh' });
  async function reloadAll(say) {
    reloadBtn.disabled = true;
    try {
      await Promise.all([loadHealth(), gaps.reload(), loadTotals(), loadStorage(), loadSignals(), loadTesla()]);
      if (say && ctx.alive()) toast('Read again.', { kind: 'info' });
    } catch (e) { if (ctx.alive()) toast(errorWords(e).text, { kind: 'error' }); } finally { reloadBtn.disabled = false; }
  }

  replace(ctx.view, h('div', { class: 'page hl', cover: 'shots.database-info' },
    plate({ kicker: `Set-up · ${car.name}`, title: 'Data health', aim: 'Whether this car\'s data is arriving, complete and trusted, with the fix for anything that is not; what is recorded, and how much room it takes.',
      tools: [carBtn(ctx, 'dbinfo.filter.car'), reloadBtn, linkBtn('Server and storage', state.buildUrl('health'), { icon: 'server', sm: true, kind: 'ghost' })] }),
    controls({ note: 'The period sets the gaps, unknown time and the signal chart. What is recorded and the storage are all time.' }, slot('Period', periodBtn(ctx))),
    hud({ title: 'Is data arriving?', icon: 'pulse', tone: 'cyan', cover: 'faq.stopped-recording', side: lightsEl }, healthBody),
    gaps.el,
    teslaSlot,
    band(1, 'What is recorded', h('span', { class: 'trk-count' }, 'all time, not the period')),
    hud({ title: 'Trip counters', icon: 'stats' }, totalsEl), rowsSlot,
    band(2, 'Storage'),
    storageSlot,
    band(3, 'Signals', sigInfo),
    sigList.el,
    hud({ title: 'A signal over the period', icon: 'stats' }, plotSlot)));

  ctx.events('rows', (ev) => {
    if (ev?.data?.car && ev.data.car !== cid) return;
    loadHealth().catch(() => {}); gaps.reload(null, { quiet: true }); loadTotals().catch(() => {});
  });

  try { await Promise.all([loadHealth(), loadTotals(), loadStorage(), loadSignals(), loadTesla(), gaps.ready]); } catch (e) { if (ctx.alive()) ctx.fail(e); return; }
  if (ctx.alive()) ctx.ready();
}

/* ================================================================== the server */

async function mountSystem(ctx) {
  const admin = isAdmin();
  const tz = ctx.tz;
  const locked = (what) => emptyState({ icon: 'lock', title: 'For admins only', text: `${what} is shown to admins. Your account sees the cars below.` });

  /* ---------- storage */
  const storeSlot = h('div', { class: 'stack' });
  async function loadStorage() {
    if (!admin) { replace(storeSlot, h('div', { cover: 'dbinfo.stat.total_size' }, locked('Server storage (total size, free space, growth)'))); return; }
    let st;
    try { st = await ctx.api.get('system/storage'); } catch (e) {
      if (e.status === 403) { replace(storeSlot, h('div', { cover: 'dbinfo.stat.total_size' }, locked('Server storage'))); return; }
      throw e;
    }
    if (!ctx.alive()) return;
    const growth = st.growth || [];
    const perDay = growth.length ? growth.reduce((a, g) => a + g.bytes, 0) / growth.length : null;
    const used = st.total_bytes;
    const disk = used != null && st.free_bytes != null ? used + st.free_bytes : null;
    const chartEl = well('', 'Bytes added per day');
    replace(storeSlot,
      h('div', { class: 'hl-disk' },
        counters([
          { label: 'Data takes', text: bytes(used), cover: 'dbinfo.stat.total_size', how: 'every car, imported history and offline maps' },
          { label: 'Free on the disk', text: st.free_bytes != null ? bytes(st.free_bytes) : '—', how: disk ? `${format((used / disk) * 100, 'percent', { digits: 1 })} of ${bytes(disk)} is Sentry Mate's` : 'not known' },
          { label: 'Grows by', text: perDay != null ? `${bytes(perDay)}/day` : '—', how: perDay != null ? `mean of the last ${plural(growth.length, 'full day')}` : 'needs a few days' },
          { label: 'Full in', text: st.full_in_days != null ? `${formatNumber(st.full_in_days)} d` : '—', how: st.full_in_days == null ? 'not growing, or not known' : st.full_in_days > 3650 ? 'over ten years: nothing to do' : st.full_in_days < 60 ? 'soon: free space or move to a bigger disk' : 'free space over the mean growth' },
        ]),
        disk ? h('div', { class: 'hl-diskbar' }, bar(used / disk, { label: 'Share of the disk Sentry Mate uses', warm: used / disk > 0.85 }), h('span', { class: 'trk-count' }, `${format((used / disk) * 100, 'percent', { digits: 1 })} used by Sentry Mate`)) : null),
      h('div', { class: 'cols' },
        kindsPanel(st.kinds, st.total_bytes, { title: 'By kind' }),
        hud({ title: 'Added per day', icon: 'calendar', side: csvBtn({ name: ['sentry-mate', 'storage-growth'], rows: growth, columns: [column('Day', 'date'), column('Bytes', 'bytes')] }) },
          growth.length ? chartEl : emptyState({ icon: 'calendar', title: 'No full day yet', text: 'Growth shows after the first full day.' }),
          note('MB added on each of the last full days (UTC), for all cars together.'))));
    if (growth.length) await colChart(chartEl, ctx, { labels: growth.map((g) => formatDayKey(g.date, { weekday: false, year: false })), values: growth.map((g) => g.bytes / 1e6), color: '--cyan' });
  }

  /* ---------- server health */
  const srvSlot = h('div');
  const clockNote = () => h('p', { class: 'note', cover: 'faq.docker-timestamps' }, icon('clock', 'i--sm'), h('span', null,
    `Server clock: ${new Date(ctx.api.now()).toISOString().replace('T', ' ').slice(0, 19)} UTC. Times are stored in UTC; every page shows them in the car's own time zone (this page: ${tz.replace(/_/g, ' ')}). The container's log lines are in UTC unless its TZ variable says otherwise.`));
  const healthAddr = () => h('div', { class: 'hl-addr', cover: 'web.health-api.health-check' },
    h('h3', { class: 'hl-sub' }, 'Health address for container checks'),
    note(h('span', { class: 'mono' }, 'GET /health'), ' answers 200 with an empty body while the server is serving. It needs no sign-in and tells nothing about the cars or the data, so a monitor can call it freely.'),
    h('pre', { class: 'hl-pre' }, 'HEALTHCHECK --interval=30s CMD ["/sentry-mate", "health"]\n# or any monitor: GET https://<your server>/health  ->  200'));
  const teslaLine = (t) => (!t || !t.accounts ? h('span', { class: 'faint' }, 'Off: no Tesla account')
    : h('span', { class: 'cell' }, h('span', null, `${plural(t.accounts, 'account')}, ${plural(t.cars, 'car')} read`), h('span', { class: 'cell-sub' }, requestsText(t.requests_today)),
      t.last_error ? h('span', { class: 'cell-sub warn' }, lastErrorText(t.last_error)) : null));
  const teslaSlot = h('div');
  async function loadTesla() {
    if (!admin) return;
    let rows = [], accounts = [], cars = [];
    try {
      [rows, accounts, cars] = await Promise.all([ctx.api.list('integrations/tesla/status').all(), ctx.api.list('integrations/tesla/accounts').all().catch(() => []), ctx.api.list('cars', { limit: 100 }).all({ max: 500 }).catch(() => [])]);
    } catch (e) { if (e.name === 'AbortError') throw e; replace(teslaSlot, note('What the server does through the Tesla API could not be read.')); return; }
    if (!ctx.alive()) return;
    if (!accounts.length) {
      replace(teslaSlot, h('p', { class: 'note' }, icon('antenna', 'i--sm'), h('span', null, 'The Tesla API input is off: no Tesla account is added, so the server makes no connection to Tesla. ',
        h('a', { class: 'plc', href: `${state.buildUrl('settings-integrations')}#tesla` }, 'Tesla account'))));
      return;
    }
    const t = statusTable(ctx, rows, { carName: (id) => cars.find((c) => c.id === id)?.name || id, accountLabel: (id) => accounts.find((a) => a.id === id)?.label || id, tz: ctx.tz });
    replace(teslaSlot, t.el);
    await t.ready;
  }
  async function loadServer() {
    if (!admin) { replace(srvSlot, locked('Server health (uptime, versions, memory and timings)'), clockNote(), healthAddr()); return; }
    let sh, geo;
    try { [sh, geo] = await Promise.all([ctx.api.get('system/health'), ctx.api.get('geocode/status').catch(() => null)]); } catch (e) {
      if (e.status === 403) { replace(srvSlot, locked('Server health'), clockNote(), healthAddr()); return; }
      throw e;
    }
    if (!ctx.alive()) return;
    const v = sh.versions || {}, mem = sh.ingest?.memory || {}, t = sh.timings_ms || {};
    const ms = (x) => (x == null ? DASH : `${formatNumber(x, { maximumFractionDigits: x < 10 ? 1 : 0 })} ms`);
    const memF = mem.limit_bytes ? mem.used_bytes / mem.limit_bytes : null;
    replace(srvSlot,
      h('div', { class: 'cols--3 cols hl-srv' },
        h('div', { class: 'stack' }, h('h3', { class: 'hl-sub' }, 'Running'), kv([
          ['Up for', sh.uptime_ms != null ? formatDuration(sh.uptime_ms) : DASH], ['Server version', h('span', { class: 'mono' }, v.server || DASH)], ['API version', h('span', { class: 'mono' }, v.api ?? DASH)],
          ['Storage format', h('span', { class: 'mono' }, v.db_format ?? DASH)], ['Timeline rules', h('span', { class: 'mono' }, v.timeline_rules ?? DASH)], ['Import mapping', h('span', { class: 'mono' }, v.import_mapping ?? DASH)],
          ['Last housekeeping', sh.housekeeping_last_ms != null ? agoEl(sh.housekeeping_last_ms) : 'not yet'],
          ['Tesla API', teslaLine(sh.tesla)],
        ])),
        h('div', { class: 'stack' }, h('h3', { class: 'hl-sub' }, 'Memory and caches'),
          h('div', { class: 'hl-mem' }, h('span', { class: 'hl-mem-row' }, h('span', null, 'Upload memory'), h('span', { class: 'mono' }, `${bytes(mem.used_bytes)} of ${bytes(mem.limit_bytes)}`)),
            bar(memF ?? 0, { label: 'Upload memory in use', warm: memF > 0.85 })),
          kv([
            ['Cars open', `${formatNumber(sh.db?.open_cars ?? 0)} of ${formatNumber(sh.db?.cars ?? 0)}`], ['Open stores take', bytes(sh.db?.open_bytes)], ['Cars in memory', formatNumber(sh.ingest?.cached_cars ?? 0)],
            ['Address look-ups', geo ? `${geo.mode === 'online' ? 'Online' : 'Offline'}, ${formatNumber(geo.cache_size ?? 0)} kept` : sh.geocoder ? `${sh.geocoder.mode}, ${formatNumber(sh.geocoder.cache_size ?? 0)} kept` : DASH],
          ])),
        h('div', { class: 'stack' }, h('h3', { class: 'hl-sub' }, 'How fast'), kv([
          ['Read one log', ms(t.decode_per_log)], ['Rebuild a window', ms(t.rebuild_per_window)], ['Typical answer', ms(t.api_p50)], ['Slow answer (1 in 20)', ms(t.api_p95)], ['This reading took', ms(sh.read_ms)],
        ]))),
      clockNote(), healthAddr());
  }

  /* ---------- API errors */
  const tryOut = h('div', { class: 'hl-try', 'aria-live': 'polite' });
  const tryBtn = btn('Ask for a car that does not exist', () => tryUnknown(), { sm: true, kind: 'ghost', icon: 'question', cover: 'web.health-api.unknown-car' });
  async function tryUnknown() {
    tryBtn.disabled = true;
    try { await ctx.api.get('cars/no-such-car-0'); replace(tryOut, h('p', { class: 'warn' }, 'The server answered with a car. That should not happen.')); } catch (e) {
      if (!ctx.alive()) return;
      replace(tryOut, h('pre', { class: 'hl-pre' }, `${e.status} ${JSON.stringify({ error: { code: e.code, message: e.message } }, null, 2)}`),
        note('A car you may not see gives exactly this answer, so nobody can find out which car ids exist.'));
    } finally { tryBtn.disabled = false; }
  }
  const errList = tracks(ctx, {
    title: 'API errors', icon: 'info', rows: API_ERRORS.map(([status, code, text]) => ({ status, code, text })), csvName: 'api-errors', cover: 'web.health-api.errors', sort: { key: 'status', dir: 'asc' }, dense: true,
    side: tryBtn,
    note: 'Every /api/v1 call that fails answers with one shape: { "error": { "code", "message", "fields" } }. code is for programs, message for people, fields names each bad input by its path.',
    columns: [
      { key: 'status', label: 'Status', num: true, sort: true, lead: true, render: (r) => h('span', { class: 'mono' }, String(r.status)) },
      { key: 'code', label: 'Code', sort: true, render: (r) => h('span', { class: 'mono' }, r.code) },
      { key: 'text', label: 'When', cls: 'is-wide', render: (r) => r.text },
    ],
  });
  errList.el.append(h('div', { class: 'hl-try-wrap' }, tryOut));

  /* ---------- every car */
  let carText = '';
  const carList = tracks(ctx, {
    title: 'Every car', icon: 'grid', path: 'cars', params: {}, limit: 50, csvName: 'cars-data-status', csv: { all: true }, cover: 'faq.stopped-recording', sort: { key: 'name', dir: 'asc' },
    side: search('', (v) => { carText = v; carList.reload({ search: v || null }); }, { placeholder: 'Find a car', ctx }),
    note: 'Each car you may see. Open one for its gaps, keys, totals, storage and signals.',
    rowHref: (c) => state.buildUrl('health', { car: c.id }),
    columns: [
      { key: 'name', label: 'Car', sort: 'name', lead: true, render: (c) => h('a', { class: 'cell', href: state.buildUrl('health', { car: c.id }) }, h('span', { class: 'cell-main' }, c.name || c.id), h('span', { class: 'cell-sub' }, [c.model, c.year].filter(Boolean).join(' · '))), csv: (c) => c.name || c.id },
      { key: 'state', label: 'Now', render: (c) => h('span', { class: 'flagline' }, lamp(c.now?.state, { stale: c.now?.stale }), stateWord(c.now?.state)), csv: (c) => stateWord(c.now?.state) },
      { key: 'last', label: 'Last data', num: true, sort: 'last_activity', render: (c) => (c.now?.received_ms != null ? agoEl(c.now.received_ms) : h('span', { class: 'faint' }, 'none yet')), csv: (c) => iso(c.now?.received_ms) },
      { key: 'status', label: 'Data', render: (c) => {
        const bad = (c.attention || []).filter((a) => a === 'upload_silence' || a === 'trust_failed');
        return bad.length ? h('span', { class: 'flagline' }, bad.map((a) => tag(REASONS[a].title, 'org'))) : h('span', { class: 'ok' }, 'Arriving');
      }, csv: (c) => (c.attention || []).join('; ') || 'arriving' },
    ],
    empty: () => emptyState({ icon: 'car', title: carText ? 'No car matches' : 'No car yet', text: carText ? `No car name holds "${carText}".` : 'Add a car in Car settings.' }),
  });

  const reloadBtn = btn('Read again', () => reloadAll(true), { icon: 'arrow-r', kind: 'ghost', sm: true, cover: 'dbinfo.control.refresh' });
  async function reloadAll(say) {
    reloadBtn.disabled = true;
    try { await Promise.all([loadStorage(), loadServer(), carList.reload(), loadTesla()]); if (say && ctx.alive()) toast('Read again.', { kind: 'info' }); } catch (e) { if (ctx.alive()) toast(errorWords(e).text, { kind: 'error' }); } finally { reloadBtn.disabled = false; }
  }

  replace(ctx.view, h('div', { class: 'page hl hl--sys' },
    plate({ kicker: 'Set-up · whole server', title: 'Server and storage', aim: 'How much room the data takes and how fast it grows, how the server is doing, and every car\'s data status in one list.', tools: [reloadBtn] }),
    hud({ title: 'Storage', icon: 'server', tone: 'cyan', cover: 'dbinfo.section.database' }, storeSlot),
    carList.el,
    hud({ title: 'Server health', icon: 'pulse' }, srvSlot),
    teslaSlot,
    errList.el,
    h('p', { class: 'note', cover: ['nr.dash.state-table', 'nr.dash.state-table.parameter', 'nr.dash.state-table.value', 'nr.dash.state-table.sort-scroll'] }, icon('pulse', 'i--sm'),
      h('span', null, 'Every decoded signal of a car, with its value and a chart over time, is on that car\'s Data health page: open a car above.'))));

  try { await Promise.all([loadStorage(), loadServer(), carList.ready, loadTesla()]); } catch (e) { if (ctx.alive()) ctx.fail(e); return; }
  if (ctx.alive()) ctx.ready();
}
