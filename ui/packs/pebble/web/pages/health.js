// Sentry Mate, "Pebble" UI pack: Health, two routes.
// - /cars/{car}/health: is this car's data arriving and trusted (one check per row with a status word and its
//   dot, the fix for each problem, the bridge keys, Rebuild all data for admins with its progress), the gaps
//   and unknown time of the period as a log, what is recorded (totals and rows kept), what its data takes on
//   disk, and every signal of its newest log in a box five rows high that sorts and scrolls on its own.
// - /system: the server's storage (total, free, growth, full in, by kind), the server's own checks (uptime,
//   versions, memory, timings), the clock, the /health address for container checks, the one error shape of the
//   API with a live try, and every car's data status.
// Data: GET cars/{car}/health, /gaps, /totals?source=, /updates, /storage, /signals/latest;
// GET/POST cars/{car}/full-rebuild and DELETE system/full-rebuild (admin: Rebuild all data, in the keys tile);
// GET integrations/tesla/status, integrations/teslafi/status, integrations/teslamate (admin: the live inputs' tiles);
// GET system/storage, system/health (admin), geocode/status, cars; GET integrations/overview (admin) for the
// Connections overview tile on both routes, a link to Settings: Integrations (its Overview tab).
// Live: `health` events of the car change every check, key and missing-log figure in place (live events, polling
// only while the live stream is down); the full rebuild follows its `job` events; `rows` events of the car read the
// gaps and totals again, in place, once a burst has passed. Reload reads the whole page again.

import { h } from '../shared/dom.js';
import { windowList } from '../shared/vlist.js';
import { format, convert } from '../shared/units.js';
import { formatDateTime, formatDuration, formatDayKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, btn, linkBtn, icon, note, stateTag, ago } from '../core/ui.js';
import { emptyTile, errorWords, ghostTile } from '../core/states.js';
import { bars } from '../core/chart.js';
import { filterBar, statTile, checkRow, sectionHead, pills, moreButton, listBox, nums, facts } from '../core/bits.js';
import { srcShort, srcPill } from '../core/source.js';
import { teslaCarFacts, teslaAccountHref, lastErrorText } from './tesla.js';
import { teslafiCarFacts } from './teslafi.js';
import { teslamateCarFacts, teslamateHref } from './teslamate.js';
import { TM_BASE, TM_SOURCE } from '../shared/teslamate.js';
import { VERSION_SOURCE } from '../core/firmware.js';
import { STATUS, STATUS_ORDER } from './connections.js';
import { deviceText, keyRows } from './device-name.js';
import { fullRebuildTile } from '../core/fullrebuild.js';
import * as fr from '../shared/fullrebuild.js';
import { follow, carOf, onRows, setAttr, FALLBACK_POLL_MS } from '../shared/live.js';
import { morph, morphChildren, keepEl } from '../core/morph.js';

// every log of the car as the CAN Bridge app is told it now (`sentry-mate log-checks --car` prints the same)
function logChecksText(lc, fmt) {
  if (!lc) return '';
  const why = [['mismatch', lc.mismatch], ['bad signature', lc.bad_signature], ['unsigned', lc.unsigned], ['chain break', lc.chain_break]]
    .filter((x) => x[1]).map((x) => `${x[0]} ${fmt(x[1])}`).join(', ');
  return `All logs as the CAN Bridge app is told: ${fmt(lc.verified)} verified, ${fmt(lc.pending)} pending, ${fmt(lc.failed)} failed${why ? ` (${why})` : ''}.`;
}

export const id = 'health';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h3'], [2, 'h3'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1']];
export const title = (ctx) => (ctx.car ? 'Health' : 'System');

const HOUR = 36e5;
const bytes = (v) => format(v, 'bytes');
const n0 = (v) => (v == null ? '—' : formatNumber(v, { maximumFractionDigits: 0 }));
const took = (ms) => (ms == null ? '—' : ms < 1000 ? `${formatNumber(Math.round(ms))} ms` : formatDuration(ms));
const fullPeriod = (p) => ({ ...p, from_ms: p?.from_ms ?? 0 });
// the figures of /health that cover a period: a `health` event has them for its default window only
const PERIOD_KEYS = ['unknown_ms', 'unknown_spans', 'unknown_spans_total', 'chain_gaps', 'chain_gaps_total', 'dropped_frames', 'dropped_frames_total'];

const REASONS = {
  upload_silence: ['No log has arrived for a while', 'Check that the CAN Bridge has power and a network, and that its key still uploads. Nothing is lost while the car sleeps.', 'Check the keys'],
  trust_failed: ['The last log failed its check', 'Its signature or hash chain did not match, so its data is kept apart. Pair the bridge again if its key changed.', 'Pair again'],
  logs_waiting_trust: ['Logs wait for a key to be trusted', 'They came from a key nobody has approved yet. Approve it and they are read at once.', 'Approve the key'],
  gps_inputs_disagree: ['Positions from one input are out of step', 'One input’s GPS positions are far from the other inputs’ positions at the same moments, more than lag or noise explains (often a decoding problem). Its positions are not used there; the other inputs’ positions are. The rest of its data is still used.', null],
  logs_missing: ['Logs missing on the server', 'The server lost some logs and asked the car to send them again. Nothing needs doing; see Missing on the server below.', null],
  rebuild_pending: ['Drives and charges are being rebuilt', 'This page follows it until it is done. Nothing needs doing.', null],
};
const TRUST = { verified: ['Verified', 'ok'], unsigned: ['Unsigned', 'warn'], failed: ['Failed', 'bad'] };
const SPAN_REASONS = { no_log: 'no log arrived', dropped_frames: 'frames were dropped', failed_check: 'the log failed its check', chain_gap: 'a log is missing from the chain', paused: 'recording was paused' };
const GAP_CAUSES = { no_log: 'No log', not_recorded: 'Not recorded', chain_gap: 'Chain gap', unknown: 'Unknown' };
const KIND_TEXT = {
  logs: 'CAN logs as the bridge sent them; everything else is rebuilt from these', series: 'Values over time, for the charts', curves: 'Power, volts and amps of each charge',
  rows: 'One row per drive, charge, state and visit', imported: 'History from TeslaMate', tiles: 'Map tiles for offline maps', places: 'Places, prices and settings', geocode: 'Addresses already looked up',
};
const UNIT_KIND = { km: 'distance', 'km/h': 'speed', '°C': 'temperature', kPa: 'pressure', kWh: 'energy', kW: 'power', '%': 'percent', V: 'voltage', A: 'current', m: 'length' };
const API_ERRORS = [
  [400, 'bad_input', 'A value has the wrong type or a filter is not valid; fields names each one.'],
  [401, 'not_signed_in', 'No session, or it ended. Sign in again.'],
  [403, 'not_allowed', 'The account may read this but not change it, or it is for admins only.'],
  [404, 'no_such_car', 'No such car, or this account may not see it. Both answer the same.'],
  [409, 'stale_version', 'Someone changed it first. Load it again and retry.'],
  [413, 'too_large', 'The body is over 1 MB.'],
  [422, 'invalid', 'Well formed but against a rule; fields names each one.'],
  [429, 'throttled', 'Too many tries. Retry-After says how long to wait.'],
  [500, 'server_error', 'Something broke on the server. Nothing was changed.'],
];

export async function mount(ctx) {
  return ctx.car ? mountCar(ctx) : mountSystem(ctx);
}

/** What a kind of stored data takes: one row per kind with a share bar, and the total. */
function kindsTile(kinds, total, { title, w = 2 }) {
  const list = (kinds || []).slice().sort((a, b) => (b.bytes || 0) - (a.bytes || 0));
  return tile({ w, title, icon: 'server', cover: 'dbinfo.table.sizes' },
    list.length ? h('ul', { class: 'kinds' }, list.map((k) => {
      const fill = h('span', { class: 'cov-fill' });
      const share = total ? (k.bytes || 0) / total : 0;
      if (share > 0) fill.style.setProperty('--v', share.toFixed(4)); else fill.hidden = true;
      return h('li', { class: 'kind' },
        h('span', { class: 'kind-text' }, h('b', { cover: 'dbinfo.sizes.table' }, k.label), h('span', null, KIND_TEXT[k.kind] || '')),
        h('span', { class: 'kind-n', cover: 'dbinfo.sizes.data' }, bytes(k.bytes)),
        h('span', { class: 'cov kind-bar', 'aria-hidden': 'true' }, fill));
    })) : emptyTile({ icon: 'server', title: 'Nothing stored yet', text: 'Storage grows once the first log arrives.', w }),
    h('p', { class: 'kinds-total', cover: 'dbinfo.sizes.total' }, h('span', null, 'All kinds together'), h('b', null, bytes(total))),
    note('As stored on disk: compressed and encrypted at rest.'));
}

/* ================================================================== one car */

async function mountCar(ctx) {
  const { car, tz } = ctx;
  const base = api.apiPath`cars/${car.id}`;
  const when = (ms) => (ms == null ? '—' : formatDateTime(ms, tz, { now: api.now() }));
  const timelineAt = (a, b) => state.buildUrl('timeline', { car: car.id, period: { from: a - HOUR, to: b + HOUR } });

  ctx.head({ actions: [
    btn('Reload', { kind: 'soft', icon: 'refresh', iconOnly: true, cover: 'dbinfo.control.refresh', onclick: () => ctx.reload() }),
    linkBtn('System', state.buildUrl('health'), { icon: 'server' }),
  ] });
  const bar = filterBar(ctx, { carCover: 'dbinfo.filter.car' }, h('span', { class: 'filter-note' }, 'Checks and gaps follow the period; totals and storage are all time.'));

  // The signals of the newest log are decoded from the stored logs (the slowest answer by far): they load beside
  // the rest and fill their own tile when they are in. The other tiles wait in grey tiles of their size meanwhile.
  const sigP = ctx.api.list(`${base}/signals/latest`, {}, { limit: 500 }).all({ max: 5000 }).catch(() => []);
  ctx.view.replaceChildren(bar, ghostTile(2, 'h3'), ghostTile(2, 'h2'), ghostTile(2, 'h2'), ghostTile(2, 'h2'), ghostTile(2, 'h2'), ghostTile(1, 'h1'), ghostTile(1, 'h1'), ghostTile(1, 'h1'), ghostTile(1, 'h1'), ghostTile('full', 'h3'));
  const [hl0, gaps0, all0, can0, tm0, upd0, st, tapi0, tst, tfi0, tfst, tmst, ov] = await Promise.all([
    ctx.api.get(`${base}/health`, fullPeriod(ctx.params)),
    ctx.api.page(`${base}/gaps`, ctx.params, { limit: 40 }).catch(() => ({ items: [] })),
    ctx.api.get(`${base}/totals`, { source: 'all' }).catch(() => ({})),
    ctx.api.get(`${base}/totals`, { source: 'smo' }).catch(() => ({})),
    ctx.api.get(`${base}/totals`, { source: 'import' }).catch(() => ({})),
    ctx.api.get(`${base}/updates`, { limit: 1 }).catch(() => null),
    ctx.api.get(`${base}/storage`, { limit: 14 }).catch(() => null),
    ctx.api.get(`${base}/totals`, { source: 'tesla_api' }).catch(() => ({})),
    ctx.admin ? ctx.api.list('integrations/tesla/status').all().catch(() => null) : Promise.resolve(null),
    ctx.api.get(`${base}/totals`, { source: 'teslafi_api' }).catch(() => ({})),
    ctx.admin ? ctx.api.list('integrations/teslafi/status').all().catch(() => null) : Promise.resolve(null),
    ctx.admin ? ctx.api.get(TM_BASE).catch(() => null) : Promise.resolve(null),
    ctx.admin ? ctx.api.get('integrations/overview').catch(() => null) : Promise.resolve(null),
  ]);
  if (!ctx.alive()) return {};
  // what the live parts are painted from: the health (its events), the gaps and the totals (read again on `rows`)
  let hl = hl0;
  const R = { gaps: gaps0, all: all0, can: can0, tm: tm0, upd: upd0, tapi: tapi0, tfi: tfi0 };

  /* ------------------------------------------------------------ the checks */
  // the Rebuild row: a full rebuild of this car when one runs (its step), else the period rebuild's state
  const rebuildRow = h('li', { class: 'check' });
  const paintRebuild = (fs, ls) => {
    const step = fs && fr.running(fs) && fs.this_car && ls ? ls[1].text : '';
    const busy = !!step || !!hl.rebuild_pending;
    lastFull = [fs, ls];
    setAttr(rebuildRow, 'class', `check is-${busy ? 'warn' : 'ok'}`);
    morphChildren(rebuildRow, [...checkRow({ name: 'Rebuild', sub: step ? `Full rebuild: ${step}` : 'Drives and charges from the logs', word: busy ? 'Running' : 'Idle' }).childNodes]);
  };
  let lastFull = [null, null];
  paintRebuild(null);
  // Rebuild all data (admins): everything worked out made again from the stored data, in the keys tile
  const full = ctx.admin ? fullRebuildTile(ctx, { car, label: 'Rebuild all data', bare: true, onStatus: paintRebuild }) : null;

  // each live tile is built by a function of its data: the first time it goes on the page, later ones are made
  // like it in place (core/morph.js: only the texts that changed are written)
  const statusOf = () => {
    const reasons = hl.reasons || [];
    const trust = hl.last_log_trust || {};
    const tw = TRUST[trust.result];
    const spans = hl.unknown_spans || [];
    return tile({ w: 2, title: 'Is data arriving?', icon: 'pulse', cls: ['hl-status', hl.healthy ? 'is-ok' : 'is-warn'], cover: ['faq.stopped-recording', 'shots.database-info'],
      side: h('a', { class: 'tile-link', href: state.buildUrl('settings-cars', { car: car.id }) }, 'Car settings', icon('chevronRight')) },
      h('div', { class: 'hl-verdict' },
        h('span', { class: 'hl-verdict-ic', 'aria-hidden': 'true' }, icon(hl.healthy ? 'check' : 'warning')),
        h('div', null, h('p', { class: 'hl-verdict-t' }, hl.healthy ? 'All good' : `${reasons.length === 1 ? 'One thing needs' : `${reasons.length} things need`} a look`),
          h('p', { class: 'tile-line' }, 'The same test sets “healthy” in MQTT.'))),
      h('ul', { class: 'checks' },
        checkRow({ name: 'Last log arrived', sub: hl.last_upload_ms != null ? when(hl.last_upload_ms) : 'Pair a CAN Bridge in car settings.', word: hl.last_upload_ms != null ? ago(hl.last_upload_ms) : 'Never', level: reasons.includes('upload_silence') ? 'bad' : hl.last_upload_ms != null ? 'ok' : 'off' }),
        checkRow({ name: 'Last log check', sub: trust.reason ? `Reason: ${trust.reason.replace(/_/g, ' ')}` : trust.log_start_ms != null ? `Log from ${when(trust.log_start_ms)}` : 'Signature and hash chain', word: tw ? tw[0] : 'None yet', level: tw ? tw[1] : 'off' }),
        checkRow({ name: 'All log checks', sub: logChecksText(hl.log_checks, n0) || 'Not known yet', word: hl.log_checks ? `${n0(hl.log_checks.failed)} failed` : '—', level: !hl.log_checks ? 'off' : hl.log_checks.failed ? 'warn' : 'ok' }),
        checkRow({ name: 'Logs waiting', sub: hl.logs_waiting ? 'Their key is not trusted yet.' : 'Every log that arrived was read.', word: n0(hl.logs_waiting ?? 0), level: hl.logs_waiting ? 'warn' : 'ok' }),
        checkRow({ name: 'Unknown time', sub: `${n0(hl.unknown_spans_total ?? spans.length)} spans in the period (asleep is known, not unknown)`, word: formatDuration(hl.unknown_ms ?? 0), level: hl.unknown_ms ? 'warn' : 'ok' }),
        checkRow({ name: 'Chain gaps', sub: 'Logs missing from the hash chain', word: n0(hl.chain_gaps_total ?? (hl.chain_gaps || []).length), level: (hl.chain_gaps || []).length ? 'warn' : 'ok' }),
        checkRow({ name: 'Dropped frames', sub: 'Times the bridge could not keep up', word: n0(hl.dropped_frames_total ?? (hl.dropped_frames || []).length), level: (hl.dropped_frames || []).length ? 'warn' : 'ok' }),
        checkRow({ name: 'Uploads', sub: 'From this car’s bridge', word: hl.uploads_enabled ? 'On' : 'Off', level: hl.uploads_enabled ? 'ok' : 'warn' }),
        keepEl(rebuildRow, 'rebuild')),
      reasons.length ? h('ul', { class: 'fixes' }, reasons.map((code) => {
        const r = REASONS[code] || [code, '', null];
        return h('li', { class: 'fix' }, h('b', null, r[0]), h('span', null, r[1]),
          r[2] ? linkBtn(r[2], state.buildUrl('settings-cars', { car: car.id }), { icon: 'key', cls: 'fix-btn' }) : null);
      })) : null);
  };

  /* the keys and the rebuild */
  const fullEl = full ? full.el : note('An admin can rebuild all of this car’s data from the stored logs and API data.');
  const keysOf = () => {
    const keys = hl.keys || [];
    return tile({ w: 2, title: 'CAN Bridge keys', icon: 'key' },
      keys.length ? h('ul', { class: ['checks', 'hl-keys'] }, keyRows(ctx, keys, (k, rename) => checkRow({ name: deviceText(k), sub: k.key, word: k.last_upload_ms != null ? ago(k.last_upload_ms) : 'Never',
        level: k.logs_waiting ? 'warn' : k.last_upload_ms != null ? 'ok' : 'off',
        side: h('span', { class: 'hl-key-side' }, h('span', { class: 'check-word' }, k.preferred ? 'Preferred · ' : '', k.last_upload_ms != null ? ago(k.last_upload_ms) : 'never'), rename) })))
        : note('No key is tied to this car yet. Pair a CAN Bridge in car settings.'),
      keepEl(fullEl, 'full-rebuild'));
  };

  /* logs the server should have but lost: asked of the car again, and those the car no longer has either */
  const plural = (n, one, many) => `${n0(n)} ${n === 1 ? one : many}`;
  const missOf = () => {
    const ml = hl.missing_logs || {};
    const mn = ml.count || 0, mg = ml.gone || 0;
    return tile({ w: 2, title: 'Missing on the server', icon: 'server', cls: 'hl-miss' },
      h('ul', { class: 'checks' },
        checkRow({ name: 'Missing logs', sub: mn ? `Oldest from ${when(ml.oldest_ms)}${ml.found_ms != null ? `, found ${when(ml.found_ms)}` : ''}` : 'Every log the server recorded is here.',
          word: mn ? plural(mn, 'log', 'logs') : 'None', level: mn ? 'warn' : 'ok' }),
        checkRow({ name: 'Asked the car', sub: !mn ? 'Nothing to ask for.' : ml.asked ? `Asked the car to send them again${ml.asked_ms != null ? `, ${when(ml.asked_ms)}` : ''}.` : 'The server will ask the car to send them again.',
          word: !mn ? '—' : ml.asked ? (ml.asked_ms != null ? ago(ml.asked_ms) : 'Asked') : 'Not yet', level: !mn ? 'off' : 'warn' }),
        checkRow({ name: 'Lost for good', sub: mg ? `The car no longer has ${n0(mg)} of these either; oldest from ${when(ml.gone_oldest_ms)}. Their time shows as unknown time.` : 'The car still has every missing log.',
          word: mg ? plural(mg, 'log', 'logs') : 'None', level: mg ? 'bad' : 'ok' })),
      note(ml.scanned_ms != null ? `Last checked ${when(ml.scanned_ms)}. The server looks at start-up and once a day.` : 'Not checked yet. The server looks at start-up and once a day.'));
  };

  /* the gaps and unknown time as a log */
  const logLines = () => [
    ...(R.gaps.items || []).map((g) => ({ at: g.start_ms, end: g.end_ms, what: `${GAP_CAUSES[g.cause] || g.cause}${g.odometer_jump_km ? `, odometer moved ${format(g.odometer_jump_km, 'distance')}` : ''}` })),
    ...(hl.unknown_spans || []).map((s) => ({ at: s.start_ms, end: s.end_ms, what: `Unknown: ${SPAN_REASONS[s.reason] || s.reason}` })),
  ].sort((a, b) => b.at - a.at).slice(0, 60);
  const logOf = (lines) => tile({ w: 2, title: 'Gaps in the period', icon: 'timeline' },
    // the gaps and unknown spans scroll in a box about five lines high (at most: a short log leaves no empty box)
    lines.length ? listBox(ctx, { fit: true, label: 'Gaps and unknown time in the period', cls: 'log-box' }, h('div', { class: 'log', role: 'list' }, lines.map((l) => h('div', { class: 'log-row', role: 'listitem' },
      h('a', { class: 'log-k', href: timelineAt(l.at, l.end ?? api.now()), title: 'Open the timeline here' }, `${when(l.at)}  ${l.what}`),
      h('span', { class: 'log-v' }, l.end == null ? 'open' : formatDuration(l.end - l.at)))))).el
      : h('p', { class: 'lede' }, 'No gaps: every moment of the period is known.'),
    note('Newest first. A gap in which the odometer moved means a drive was not recorded.'));

  /* ------------------------------------------------------------ what is recorded */
  const totsOf = () => {
    const { all, can, tm, upd, tapi, tfi } = R;
    const sm = upd?.summary || {};
    return [
      statTile({ label: 'Logged distance', value: all.distance_km, kind: 'distance', icon: 'route', cover: 'dbinfo.stat.logged_distance', sub: `${format(can.distance_km ?? 0, 'distance')} from SMO, ${format(tapi.distance_km ?? 0, 'distance')} from the Tesla API${tfi.distance_km ? `, ${format(tfi.distance_km, 'distance')} from TeslaFi` : ''}` }),
      statTile({ label: 'Odometer', value: all.odometer_last_km, kind: 'odometer', icon: 'gauge', cover: 'dbinfo.stat.odometer', why: 'not sent yet', sub: all.odometer_last_ms != null ? h('span', null, 'read ', ago(all.odometer_last_ms)) : null }),
      statTile({ label: 'Drives', value: all.drives ?? 0, kind: 'count', icon: 'route', cover: 'dbinfo.stat.drive_count', sub: `${n0(tm.drives ?? 0)} imported` }),
      statTile({ label: 'Charges', value: all.charges ?? 0, kind: 'count', icon: 'bolt', cover: 'dbinfo.stat.charge_count', sub: `${n0(tm.charges ?? 0)} imported` }),
      statTile({ label: 'Software updates', value: all.updates ?? 0, kind: 'count', icon: 'download', cover: 'dbinfo.stat.update_count', sub: `${n0(tm.updates ?? 0)} imported` }),
      tile({ w: 1, title: 'Software version', icon: 'chip', cls: 'tile-stat', cover: 'dbinfo.stat.current_version', href: state.buildUrl('software', { car: car.id }) },
        h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, sm.current_version || '—')),
        h('p', { class: 'tile-line' }, sm.current_version ? (VERSION_SOURCE[sm.current_version_source] ? `${VERSION_SOURCE[sm.current_version_source]}, never guessed` : 'never guessed') : 'not known yet')),
    ];
  };
  const rowKinds = [['Drives', 'drives'], ['Charges', 'charges'], ['Software updates', 'updates']];
  const rowsOf = () => {
    const { all, can, tm, tapi, tfi } = R;
    return tile({ w: 2, title: 'Rows kept', icon: 'book', cover: 'dbinfo.table.row_counts' },
      h('div', { class: 'hl-rows', role: 'table', 'aria-label': 'Rows kept' },
        h('div', { class: 'hl-row hl-row-head', role: 'row' }, h('span', { role: 'columnheader', cover: 'dbinfo.rows.table_name' }, 'Kind'), h('span', { role: 'columnheader' }, 'SMO'), h('span', { role: 'columnheader' }, 'Tesla API'), h('span', { role: 'columnheader' }, 'TeslaFi'), h('span', { role: 'columnheader' }, 'Imported'), h('span', { role: 'columnheader', cover: 'dbinfo.rows.row_count' }, 'Rows')),
        rowKinds.map(([name, k]) => h('div', { class: 'hl-row', role: 'row' }, h('span', { role: 'cell' }, name), h('span', { role: 'cell' }, n0(can[k] ?? 0)), h('span', { role: 'cell' }, n0(tapi[k] ?? 0)), h('span', { role: 'cell' }, n0(tfi[k] ?? 0)), h('span', { role: 'cell' }, n0(tm[k] ?? 0)), h('b', { role: 'cell' }, n0(all[k] ?? 0))))),
      note('Exact counts, kept up to date as rows change. A row two live inputs (SMO, the Tesla API, TeslaFi) both saw counts in both columns, and once under Rows; where imported and live history overlap both are counted.'));
  };
  const storeT = st ? kindsTile(st.kinds, st.total_bytes, { title: `What ${car.name}’s data takes` }) : tile({ w: 2, title: 'Storage', icon: 'server', cover: 'dbinfo.table.sizes' }, note('Storage could not be read.'));
  const days = (st?.items || []).slice(0, 14);
  const daysT = tile({ w: 2, title: 'Stored by day', icon: 'calendar' },
    days.length ? h('div', { class: 'log' }, days.map((d) => h('div', { class: 'log-row' }, h('span', { class: 'log-k' }, `${formatDayKey(d.date)}  ${d.series}${srcShort(d) ? `  ${srcShort(d)}` : ''}`), h('span', { class: 'log-v' }, bytes(d.bytes)))))
      : h('p', { class: 'lede' }, 'No stored days yet.'),
    note('Newest first, only days with data, with where each came from: SMO, the Tesla API or imported.'));

  /* the Tesla API input of this car (admins only) */
  const tf = tst ? teslaCarFacts(ctx, tst, car.id) : null;
  const teslaT = !ctx.admin ? null : tile({ w: 2, title: 'Tesla API input', icon: 'key', side: tf ? srcPill({ sources: [{ kind: 'tesla_api', label: 'Data from the Tesla API' }] }, { short: true }) : null },
    tf ? facts(tf) : tst == null ? note('The Tesla API status could not be read.')
      : [note('No Tesla account reads this car: its data comes from SMO and any import only.'), h('a', { class: 'tile-link', href: `${state.buildUrl('settings-integrations')}#tesla` }, 'Tesla account settings', icon('chevronRight'))]);
  /* the TeslaFi input of this car (admins only) */
  const fif = tfst ? teslafiCarFacts(ctx, tfst, car.id) : null;
  const teslafiT = !ctx.admin ? null : tile({ w: 2, title: 'TeslaFi input', icon: 'key', side: fif ? srcPill({ sources: [{ kind: 'teslafi_api', label: 'Data from TeslaFi' }] }, { short: true }) : null },
    fif ? facts(fif) : tfst == null ? note('The TeslaFi status could not be read.')
      : [note('No TeslaFi token reads this car.'), h('a', { class: 'tile-link', href: `${state.buildUrl('settings-integrations')}#teslafi` }, 'TeslaFi settings', icon('chevronRight'))]);
  /* the local TeslaMate input of this car (admins only) */
  const tmf = tmst ? teslamateCarFacts(ctx, tmst, car.id) : null;
  const teslamateT = !ctx.admin ? null : tile({ w: 2, title: 'Local TeslaMate input', icon: 'key', side: tmf ? srcPill({ sources: [TM_SOURCE] }, { short: true }) : null },
    tmf ? facts(tmf) : tmst == null ? note('The local TeslaMate status could not be read.')
      : [note(tmst.enabled ? 'No TeslaMate car is matched to this car.' : 'No local TeslaMate is added: none reads this car.'), h('a', { class: 'tile-link', href: teslamateHref() }, 'Local TeslaMate settings', icon('chevronRight'))]);

  /* ------------------------------------------------------------ signals */
  const sigHead = sectionHead('Signals from the newest log', 'Every decoded signal with its value. Find one, sort, and scroll the list.');
  const kindOf = (s) => (s.series === 'odometer' ? 'odometer' : UNIT_KIND[s.unit] || null);
  const text = (s) => {
    if (s.value_name) return s.value_name;
    if (s.value == null) return '—';
    if (/^gps_(latitude|longitude)$/.test(s.series)) return `${formatNumber(s.value, { minimumFractionDigits: 5, maximumFractionDigits: 5 })}°`;
    const k = kindOf(s);
    return k ? format(s.value, k) : `${formatNumber(s.value, { maximumFractionDigits: 2 })}${s.unit ? ` ${s.unit}` : ''}`;
  };
  let sort = 'name', sig = [];
  const search = h('input', { type: 'search', placeholder: 'Find a signal or value', 'aria-label': 'Find a signal', autocomplete: 'off' });
  const count = h('span', { class: 'filter-note' });
  // The signals: a box five rows high that scrolls inside itself under a head that stays (a long list need not take a
  // big part of the page), the search and the sort above it. Only the rows in view are drawn (shared/vlist.js), from
  // the list the page holds; every row is one height (each line cut short, its whole text in its title).
  let shown = [];
  const busWord = (s) => (s.bus === 'chassis' ? 'chassis' : s.bus === 'vehicle' ? 'vehicle' : s.bus);
  const sigRow = (s, i) => h('div', { class: 'sig-row', role: 'row', dataset: { vlRow: String(i) } },
    h('span', { role: 'cell', class: 'sig-name', title: s.comment && s.comment !== s.series ? `${s.comment} (${s.series})` : s.series },
      h('b', null, s.comment || s.series), h('span', { class: 'sig-id' }, s.series)),
    h('span', { role: 'cell', class: 'sig-msg' }, `${s.message_id} · ${busWord(s)}`),
    h('span', { role: 'cell', class: 'sig-val', title: text(s) }, text(s)));
  const sigSkel = (i) => h('div', { class: 'sig-row is-skel', role: 'row', 'aria-hidden': 'true', dataset: { vlSkel: String(i) } },
    h('span', { class: 'sig-name' }, h('b', null, '…')));
  const sigHeadRow = h('div', { class: 'sig-row sig-head', role: 'row' }, h('span', { role: 'columnheader', cover: 'nr.dash.state-table.parameter' }, 'Signal'), h('span', { role: 'columnheader', class: 'sig-mh' }, 'Message'), h('span', { role: 'columnheader', cover: 'nr.dash.state-table.value' }, 'Value'));
  const sigBody = h('div', { class: 'sig-body', role: 'rowgroup' });
  const sigNone = h('p', { class: 'lede sig-none', hidden: true });
  // (is-loading: the box is its full five rows high until the signals are in)
  const listEl = h('div', { class: 'sig-list is-loading', role: 'table', 'aria-label': 'Signals (scrolls)', tabindex: '0', cover: ['nr.dash.state-table', 'nr.dash.state-table.sort-scroll'] }, sigHeadRow, sigBody, sigNone);
  const sigList = windowList({
    box: listEl, body: sigBody, head: sigHeadRow, limit: 100, overscan: 6, rowHeight: 56,
    row: sigRow, placeholder: sigSkel, spacer: () => h('div', { class: 'vl-gap' }),
    load: ({ offset = 0, limit = 100 }) => Promise.resolve({ items: shown.slice(offset, offset + limit), offset, total: shown.length, next: null }),
  });
  ctx.onCleanup(() => sigList.destroy());
  const sortPills = pills({ label: 'Sort by', value: sort, options: [['name', 'Name'], ['value', 'Value'], ['message', 'Message'], ['at', 'Newest']], onchange: (v) => { sort = v; paint(); } });
  function paint() {
    const words = search.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    let rows = sig.filter((s) => !words.length || words.every((w) => [s.series, s.comment, s.message_id, s.bus, s.unit, s.value_name, text(s)].filter(Boolean).join(' ').toLowerCase().includes(w)));
    const val = (s) => (s.value_name ? null : s.value == null ? null : kindOf(s) ? convert(s.value, kindOf(s)) : s.value);
    const cmp = { name: (a, b) => (a.comment || a.series).localeCompare(b.comment || b.series), value: (a, b) => (val(b) ?? -Infinity) - (val(a) ?? -Infinity), message: (a, b) => `${a.bus}${a.message_id}`.localeCompare(`${b.bus}${b.message_id}`), at: (a, b) => (b.at_ms || 0) - (a.at_ms || 0) }[sort];
    shown = rows.slice().sort(cmp);
    count.textContent = words.length ? `${n0(shown.length)} of ${n0(sig.length)}` : `${n0(sig.length)} signals`;
    sigNone.hidden = !!shown.length;
    sigNone.textContent = shown.length ? '' : sig.length ? 'No signal matches.' : 'Signals show once the first log is decoded.';
    // (the box keeps its full height until its rows are in: the window of rows is worked out from that height)
    return sigList.start().finally(() => listEl.classList.remove('is-loading'));
  }
  let timer = 0;
  ctx.on(search, 'input', () => { clearTimeout(timer); timer = setTimeout(() => ctx.alive() && paint(), 150); });
  ctx.onCleanup(() => clearTimeout(timer));
  const sigT = tile({ w: 'full', title: 'Signals', icon: 'pulse', cls: 'sig-tile' },
    h('div', { class: 'page-bar' }, h('label', { class: 'search-pill' }, icon('search'), search), sortPills, count), listEl);

  let status = statusOf(), keysT = keysOf(), missT = missOf(), rowsT = rowsOf(), tots = totsOf();
  let logSig = JSON.stringify(logLines());
  let logT = logOf(logLines());
  ctx.view.replaceChildren(bar, status, keysT, missT, connTile(ctx, ov, car.id), teslaT, teslafiT, teslamateT, logT, rowsT, ...tots, storeT, daysT, sigHead, sigT);

  // the gaps log: drawn again only when its lines changed (its scroll box keeps its place otherwise)
  function paintLog() {
    const lines = logLines();
    const sig = JSON.stringify(lines);
    if (sig === logSig) return;
    logSig = sig;
    const next = logOf(lines);
    logT.replaceWith(next);
    logT = next;
  }
  function paintHealth() {
    status = morph(status, statusOf());
    keysT = morph(keysT, keysOf());
    missT = morph(missT, missOf());
    paintRebuild(...lastFull);
    paintLog();
  }
  // the gaps and totals of the car, read again (after its rows changed, or a rebuild ended)
  let rowsBusy = false;
  async function loadRows() {
    if (rowsBusy) return;
    rowsBusy = true;
    try {
      const [gaps, all, can, tm, upd, tapi, tfi] = await Promise.all([
        ctx.api.page(`${base}/gaps`, ctx.params, { limit: 40 }).catch(() => R.gaps),
        ctx.api.get(`${base}/totals`, { source: 'all' }).catch(() => R.all),
        ctx.api.get(`${base}/totals`, { source: 'smo' }).catch(() => R.can),
        ctx.api.get(`${base}/totals`, { source: 'import' }).catch(() => R.tm),
        ctx.api.get(`${base}/updates`, { limit: 1 }).catch(() => R.upd),
        ctx.api.get(`${base}/totals`, { source: 'tesla_api' }).catch(() => R.tapi),
        ctx.api.get(`${base}/totals`, { source: 'teslafi_api' }).catch(() => R.tfi),
      ]);
      if (!ctx.alive()) return;
      Object.assign(R, { gaps, all, can, tm, upd, tapi, tfi });
      rowsT = morph(rowsT, rowsOf());
      tots = totsOf().map((t, i) => morph(tots[i], t));
      paintLog();
    } finally { rowsBusy = false; }
  }
  onRows(ctx.api, { car: car.id, waitMs: 1000, run: () => { if (ctx.alive()) loadRows().catch(() => {}); } });

  // the health follows the car's `health` events; its endpoint is read after a reconnect and, only while the live
  // stream is down, every few seconds (every 3 s while drives are worked out again). The page's own read is the first.
  let firstHealth = true, eventPeriod = null, periodT = 0;
  ctx.onCleanup(() => clearTimeout(periodT));
  const healthLive = follow(ctx.api, {
    type: 'health', match: carOf(car.id), pick: (d) => d.health,
    load: () => {
      if (firstHealth) { firstHealth = false; return Promise.resolve(hl0); }
      return ctx.api.get(`${base}/health`, fullPeriod(ctx.params));
    },
    pollMs: (v) => (v?.rebuild_pending ? 3000 : FALLBACK_POLL_MS),
    onValue: (v, how) => {
      if (!ctx.alive() || !v) return;
      const was = !!hl.rebuild_pending;
      let next = v;
      if (how === 'event') {
        // an event's period figures (unknown time, chain gaps, dropped frames) are for its own window: the page's
        // read keeps them for the period shown, and is made again (at most every 2 s) when the event's ones changed
        const sig = JSON.stringify(PERIOD_KEYS.map((k) => v[k] ?? null));
        if (eventPeriod != null && sig !== eventPeriod && !periodT) periodT = setTimeout(() => { periodT = 0; if (ctx.alive()) healthLive.refresh(); }, 2000);
        eventPeriod = sig;
        next = { ...v };
        for (const k of PERIOD_KEYS) next[k] = hl[k];
      }
      hl = next;
      paintHealth();
      // drives and charges worked out again: the gaps and totals they changed are read again too
      if (was && !hl.rebuild_pending) loadRows().catch(() => {});
    },
  });
  [sig] = await Promise.all([sigP, full?.ready]);
  if (!ctx.alive()) return {};
  await paint();
  if (!ctx.alive()) return {};
  ctx.ready();
  return {};
}

/* ================================================================== both routes */

/**
 * The Connections overview tile: a link to Settings: Integrations (its Overview tab, the default) with the totals
 * and, on a car's page, the status of that car's CAN Bridge. ov: GET integrations/overview, or null.
 */
function connTile(ctx, ov, carId = null) {
  const t = ov && ov.totals;
  const mine = ov && carId ? (ov.connections || []).find((c) => c.kind === 'smo' && c.car === carId) : null;
  const bad = t ? t.problem || 0 : 0;
  return tile({ w: 2, href: state.buildUrl('settings-integrations'), title: 'Connections overview', icon: 'plug', cls: ['hl-conn', bad ? 'is-warn' : null], cover: 'conn.overview.link',
    side: icon('chevronRight', 'hl-conn-go') },
    t ? h('ul', { class: 'hl-conn-tot' }, STATUS_ORDER.map((k) => h('li', { class: `is-${k}` }, h('b', null, n0(t[k] || 0)), ' ', STATUS[k][0].toLowerCase())))
      : null,
    mine ? h('p', { class: 'note' }, `This car’s CAN Bridge: ${STATUS[mine.status] ? STATUS[mine.status][0].toLowerCase() : mine.status}${mine.last_ok_ms != null ? `, last good contact ${formatDateTime(mine.last_ok_ms, ctx.tz, { now: api.now() })}` : ''}.`) : null,
    note(t ? 'Every connection the server keeps, with its last good contact, counts, a 24 h sparkline and the problems of the last 7 days.'
      : ctx.admin ? 'Every connection the server keeps (CAN Bridge, Tesla API, TeslaFi, local TeslaMate, MQTT, maps, address lookup, release check): live or not, counts and problems.'
        : 'Every connection the server keeps, live or not, with counts and problems. Admins open it in Settings: Integrations.'));
}

/* ================================================================== the server */

async function mountSystem(ctx) {
  const admin = !!ctx.admin;
  ctx.head({ actions: [btn('Reload', { kind: 'soft', icon: 'refresh', iconOnly: true, onclick: () => ctx.reload() })] });
  // the car list's first page is asked for with the rest, not after it
  const pager = ctx.api.pager('cars', { sort: 'name' }, { limit: 30 });
  const firstCars = pager.more().catch(() => null);
  const [st, sh, geo, ov] = admin ? await Promise.all([
    ctx.api.get('system/storage').catch((e) => ({ error: e })),
    ctx.api.get('system/health').catch((e) => ({ error: e })),
    ctx.api.get('geocode/status').catch(() => null),
    ctx.api.get('integrations/overview').catch(() => null),
  ]) : [null, null, null, null];
  await firstCars;
  if (!ctx.alive()) return {};

  /* ------------------------------------------------------------ storage */
  const tiles = [sectionHead('Storage', 'Every car’s data, imported history and offline maps, as stored on disk.', { cover: 'dbinfo.section.database' })];
  if (!st || st.error) {
    tiles.push(tile({ w: 2, title: 'Storage', icon: 'lock', cover: 'dbinfo.stat.total_size' }, note(admin ? 'Storage could not be read. Reload to try again.' : 'Storage (total size, free space, growth) is shown to admins only.')));
  } else {
    const growth = st.growth || [];
    const perDay = growth.length ? growth.reduce((a, g) => a + (g.bytes || 0), 0) / growth.length : null;
    tiles.push(
      statTile({ label: 'Data takes', node: h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, bytes(st.total_bytes))), icon: 'server', cover: 'dbinfo.stat.total_size', sub: 'all cars and maps' }),
      statTile({ label: 'Free on the disk', node: h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, st.free_bytes != null ? bytes(st.free_bytes) : '—')), icon: 'download', sub: st.free_bytes != null && st.total_bytes != null ? `${format((st.total_bytes / (st.total_bytes + st.free_bytes)) * 100, 'percent', { digits: 1 })} is Sentry Mate’s` : 'not known' }),
      statTile({ label: 'Grows by', node: h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, perDay != null ? bytes(perDay) : '—')), icon: 'bars', sub: perDay != null ? `a day, mean of ${growth.length} days` : 'needs a few days' }),
      statTile({ label: 'Full in', value: st.full_in_days, kind: 'count', icon: 'calendar', why: 'not growing', sub: st.full_in_days == null ? 'or not known' : st.full_in_days > 3650 ? 'days: nothing to do' : st.full_in_days < 60 ? 'days: free some space soon' : 'days at this pace' }),
      kindsTile(st.kinds, st.total_bytes, { title: 'By kind' }),
      tile({ w: 2, title: 'Added per day', icon: 'calendar', cls: 'chart-tile' },
        growth.length ? bars(growth.map((g) => ({ label: g.date.slice(8), value: g.bytes || 0, text: '', title: `${formatDayKey(g.date)}: ${bytes(g.bytes)}` })), { tone: 'c2', label: 'Bytes added per day' })
          : h('p', { class: 'lede' }, 'Growth shows after the first full day.'),
        note('Bytes added on each of the last full days (UTC days), all cars together.')));
  }

  /* ------------------------------------------------------------ the server's own checks */
  tiles.push(sectionHead('Server', 'Is the server healthy, and how fast does it answer.'));
  if (sh && !sh.error) {
    const v = sh.versions || {}, mem = sh.ingest?.memory || {}, t = sh.timings_ms || {};
    const memPct = mem.limit_bytes ? (mem.used_bytes / mem.limit_bytes) * 100 : null;
    tiles.push(tile({ w: 2, title: 'Checks', icon: 'pulse' },
      h('ul', { class: 'checks' },
        checkRow({ name: 'Running', sub: `Up for ${sh.uptime_ms != null ? formatDuration(sh.uptime_ms) : '—'}`, word: 'Up', level: 'ok' }),
        checkRow({ name: 'Upload memory', sub: `${bytes(mem.used_bytes)} of ${bytes(mem.limit_bytes)}`, word: memPct == null ? '—' : format(memPct, 'percent'), level: memPct > 85 ? 'warn' : 'ok' }),
        checkRow({ name: 'Cars open', sub: `${bytes(sh.db?.open_bytes)} in open stores · ${n0(sh.ingest?.cached_cars ?? 0)} kept in memory`, word: `${n0(sh.db?.open_cars ?? 0)} of ${n0(sh.db?.cars ?? 0)}`, level: 'ok' }),
        checkRow({ name: 'Address look-ups', sub: geo ? `${n0(geo.cache_size ?? 0)} cached${geo.online?.last_error ? ` · last error: ${geo.online.last_error}` : ''}` : 'Not known', word: geo ? (geo.mode === 'online' ? 'Online' : 'Offline') : '—', level: geo?.online?.last_error ? 'warn' : 'ok' }),
        sh.tesla ? checkRow({ name: 'Tesla API', sub: sh.tesla.accounts ? `${n0(sh.tesla.accounts)} ${sh.tesla.accounts === 1 ? 'account' : 'accounts'}, ${n0(sh.tesla.cars ?? 0)} ${sh.tesla.cars === 1 ? 'car' : 'cars'} read, ${n0(sh.tesla.requests_today ?? 0)} requests today${sh.tesla.last_error ? ` · last error: ${lastErrorText(sh.tesla.last_error)}` : ''}` : 'Off: no Tesla account, so no connection to Tesla.',
          word: !sh.tesla.accounts ? 'Off' : sh.tesla.last_error ? 'Look' : 'On', level: !sh.tesla.accounts ? 'off' : sh.tesla.last_error ? 'warn' : 'ok',
          side: h('a', { class: 'check-word place-link', href: `${state.buildUrl('settings-integrations')}#tesla` }, !sh.tesla.accounts ? 'Off' : sh.tesla.last_error ? 'Look' : 'On') }) : null,
        checkRow({ name: 'Housekeeping', sub: 'Checks, compaction and the daily storage figure', word: sh.housekeeping_last_ms != null ? ago(sh.housekeeping_last_ms) : 'Not yet', level: sh.housekeeping_last_ms != null ? 'ok' : 'off' }))),
    tile({ w: 2, title: 'Versions and timings', icon: 'chip' },
      nums([
        ['Server', h('span', { class: 'mono-n' }, v.server || '—')], ['API', h('span', { class: 'mono-n' }, String(v.api ?? '—'))],
        ['Storage format', h('span', { class: 'mono-n' }, String(v.db_format ?? '—'))], ['Timeline rules', h('span', { class: 'mono-n' }, String(v.timeline_rules ?? '—'))],
        ['Read one log', h('span', { class: 'mono-n' }, took(t.decode_per_log))], ['Rebuild a window', h('span', { class: 'mono-n' }, took(t.rebuild_per_window))],
        ['Typical answer', h('span', { class: 'mono-n' }, took(t.api_p50))], ['Slow answer (95th)', h('span', { class: 'mono-n' }, took(t.api_p95))],
      ]),
      note(`This reading took ${took(sh.read_ms)}.`)));
  } else {
    tiles.push(tile({ w: 2, title: 'Checks', icon: 'lock' }, note(admin ? 'The server’s health could not be read.' : 'Uptime, versions, memory and timings are shown to admins only.')));
  }
  tiles.push(connTile(ctx, ov));
  tiles.push(tile({ w: 2, title: 'Clock and health address', icon: 'clock' },
    h('p', { class: 'note', cover: 'faq.docker-timestamps' }, `Server clock: ${new Date(api.now()).toISOString().replace('T', ' ').slice(0, 19)} UTC. Times are stored in UTC; every page shows them in the car’s own time zone. The container’s log lines are in UTC unless its TZ variable says otherwise.`),
    h('div', { cover: 'web.health-api.health-check' },
      h('p', { class: 'note' }, 'GET /health answers 200 with an empty body while the server serves. It needs no sign-in and tells nothing about the cars, so a monitor or the container’s health check can call it freely.'),
      h('pre', { class: 'log' }, 'HEALTHCHECK --interval=30s CMD ["/sentry-mate", "health"]\n# or any monitor: GET https://<your server>/health  ->  200'))));

  /* ------------------------------------------------------------ API errors */
  const out = h('div', { class: 'try-out', 'aria-live': 'polite' });
  const tryB = btn('Ask for a car that does not exist', { kind: 'soft', icon: 'help', cover: 'web.health-api.unknown-car', onclick: async () => {
    tryB.disabled = true;
    try { await ctx.api.get('cars/no-such-car-0'); out.replaceChildren(note('The server answered with a car. That should not happen.')); } catch (e) {
      if (ctx.alive()) out.replaceChildren(h('pre', { class: 'log' }, `${e.status} ${JSON.stringify({ error: { code: e.code, message: e.message } }, null, 2)}`), note('A car you may not see gives exactly this answer, so nobody can find out which car ids exist.'));
    } finally { tryB.disabled = false; }
  } });
  tiles.push(tile({ w: 2, title: 'API errors', icon: 'info', cover: 'web.health-api.errors' },
    note('Every /api/v1 call that fails answers with one shape: { "error": { "code", "message", "fields" } }.'),
    h('ul', { class: 'checks' }, API_ERRORS.map(([s, code, text]) => checkRow({ name: code, sub: text, word: String(s), level: s >= 500 ? 'bad' : s === 429 || s === 409 ? 'warn' : 'off' }))),
    h('div', { class: 'item-links' }, tryB), out));

  /* ------------------------------------------------------------ every car */
  const carsHead = sectionHead('Every car', 'Open one for its checks, gaps, totals, storage and signals.', { cover: 'faq.stopped-recording' });
  const carBox = h('div', { class: 'contents' });
  const drawCars = (list) => {
    for (const c of list) {
      const bad = (c.attention || []).filter((a) => a === 'upload_silence' || a === 'trust_failed');
      carBox.append(tile({ w: 1, href: state.buildUrl('health', { car: c.id }), cls: 'hl-car', label: `${c.name || c.id}: data health` },
        h('p', { class: 'item-title' }, c.name || c.id),
        stateTag(c.now?.state || 'unknown', { stale: !!c.now?.stale }),
        h('ul', { class: 'checks' }, checkRow({ name: bad.length ? REASONS[bad[0]][0] : 'Data arriving', sub: c.now?.received_ms != null ? h('span', null, 'last ', ago(c.now.received_ms)) : 'no data yet', word: bad.length ? 'Look' : 'OK', level: bad.length ? 'bad' : c.now?.received_ms != null ? 'ok' : 'off' }))));
    }
  };
  drawCars(pager.items || []);
  const more = moreButton(async () => { const n = pager.items.length; await pager.more(); drawCars(pager.items.slice(n)); return !pager.done; }, { text: 'Show more cars' });
  if (!pager.items?.length) carBox.append(emptyTile({ icon: 'car', title: 'No car yet', text: 'Add a car in Settings: Cars.' }));
  else if (!pager.done) carBox.append(more);
  tiles.push(carsHead, carBox,
    tile({ w: 'full', cls: 'st-precise', cover: ['nr.dash.state-table', 'nr.dash.state-table.parameter', 'nr.dash.state-table.value', 'nr.dash.state-table.sort-scroll'] },
      note('Every decoded signal of a car, with its value, is on that car’s Health page: open a car above.')));
  ctx.view.replaceChildren(...tiles);
  ctx.ready();
  return {};
}
