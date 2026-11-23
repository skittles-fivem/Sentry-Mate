// Sentry Mate, "Pebble" UI pack: Health, two routes.
// - /cars/{car}/health: is this car's data arriving and trusted (one check per row with a status word and its
//   dot, the fix for each problem, the bridge keys, a rebuild for admins), the gaps and unknown time of the period
//   as a log, what is recorded (totals and rows kept), what its data takes on disk, and every signal of its newest
//   log in a list that sorts and scrolls on its own.
// - /system: the server's storage (total, free, growth, full in, by kind), the server's own checks (uptime,
//   versions, memory, timings), the clock, the /health address for container checks, the one error shape of the
//   API with a live try, and every car's data status.
// Data: GET cars/{car}/health, /gaps, /totals?source=, /updates, /storage, /signals/latest; POST /rebuild (admin);
// GET integrations/tesla/status, integrations/teslafi/status, integrations/teslamate (admin: the live inputs' tiles);
// GET system/storage, system/health (admin), geocode/status, cars; GET integrations/overview (admin) for the
// Connections overview tile on both routes, a link to Settings: Integrations (its Overview tab).

import { h } from '../shared/dom.js';
import { format, convert } from '../shared/units.js';
import { formatDateTime, formatDuration, formatDayKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, btn, linkBtn, icon, note, stateTag, ago } from '../core/ui.js';
import { emptyTile, errorWords, ghostTile, plotGhost } from '../core/states.js';
import { confirmSheet } from '../core/sheet.js';
import { bars } from '../core/chart.js';
import { filterBar, statTile, checkRow, sectionHead, pills, moreButton, listBox, nums, facts } from '../core/bits.js';
import { srcShort, srcPill } from '../core/source.js';
import { teslaCarFacts, teslaAccountHref, lastErrorText } from './tesla.js';
import { teslafiCarFacts } from './teslafi.js';
import { teslamateCarFacts, teslamateHref } from './teslamate.js';
import { TM_BASE, TM_SOURCE } from '../shared/teslamate.js';
import { VERSION_SOURCE } from '../core/firmware.js';
import { STATUS, STATUS_ORDER } from './connections.js';

export const id = 'health';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h3'], [2, 'h3'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1']];
export const title = (ctx) => (ctx.car ? 'Health' : 'System');

const HOUR = 36e5;
const bytes = (v) => format(v, 'bytes');
const n0 = (v) => (v == null ? '—' : formatNumber(v, { maximumFractionDigits: 0 }));
const took = (ms) => (ms == null ? '—' : ms < 1000 ? `${formatNumber(Math.round(ms))} ms` : formatDuration(ms));
const fullPeriod = (p) => ({ ...p, from_ms: p?.from_ms ?? 0 });

const REASONS = {
  upload_silence: ['No log has arrived for a while', 'Check that the CAN Bridge has power and a network, and that its key still uploads. Nothing is lost while the car sleeps.', 'Check the keys'],
  trust_failed: ['The last log failed its check', 'Its signature or hash chain did not match, so its data is kept apart. Pair the bridge again if its key changed.', 'Pair again'],
  logs_waiting_trust: ['Logs wait for a key to be trusted', 'They came from a key nobody has approved yet. Approve it and they are read at once.', 'Approve the key'],
  logs_missing: ['Logs missing on the server', 'The server lost some logs and asked the car to send them again. Nothing needs doing; see Missing on the server below.', null],
  rebuild_pending: ['Drives and charges are being rebuilt', 'This page reads again every few seconds until it is done. Nothing needs doing.', null],
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
  const [hl, gaps, all, can, tm, upd, st, tapi, tst, tfi, tfst, tmst, ov] = await Promise.all([
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
  if (hl.rebuild_pending) { const t = setTimeout(() => ctx.alive() && ctx.reload(), 4000); ctx.onCleanup(() => clearTimeout(t)); }

  /* ------------------------------------------------------------ the checks */
  const reasons = hl.reasons || [];
  const trust = hl.last_log_trust || {};
  const tw = TRUST[trust.result];
  const spans = hl.unknown_spans || [];
  const status = tile({ w: 2, title: 'Is data arriving?', icon: 'pulse', cls: ['hl-status', hl.healthy ? 'is-ok' : 'is-warn'], cover: ['faq.stopped-recording', 'shots.database-info'],
    side: h('a', { class: 'tile-link', href: state.buildUrl('settings-cars', { car: car.id }) }, 'Car settings', icon('chevronRight')) },
    h('div', { class: 'hl-verdict' },
      h('span', { class: 'hl-verdict-ic', 'aria-hidden': 'true' }, icon(hl.healthy ? 'check' : 'warning')),
      h('div', null, h('p', { class: 'hl-verdict-t' }, hl.healthy ? 'All good' : `${reasons.length === 1 ? 'One thing needs' : `${reasons.length} things need`} a look`),
        h('p', { class: 'tile-line' }, 'The same test sets “healthy” in MQTT.'))),
    h('ul', { class: 'checks' },
      checkRow({ name: 'Last log arrived', sub: hl.last_upload_ms != null ? when(hl.last_upload_ms) : 'Pair a CAN Bridge in car settings.', word: hl.last_upload_ms != null ? ago(hl.last_upload_ms) : 'Never', level: reasons.includes('upload_silence') ? 'bad' : hl.last_upload_ms != null ? 'ok' : 'off' }),
      checkRow({ name: 'Last log check', sub: trust.reason ? `Reason: ${trust.reason.replace(/_/g, ' ')}` : trust.log_start_ms != null ? `Log from ${when(trust.log_start_ms)}` : 'Signature and hash chain', word: tw ? tw[0] : 'None yet', level: tw ? tw[1] : 'off' }),
      checkRow({ name: 'Logs waiting', sub: hl.logs_waiting ? 'Their key is not trusted yet.' : 'Every log that arrived was read.', word: n0(hl.logs_waiting ?? 0), level: hl.logs_waiting ? 'warn' : 'ok' }),
      checkRow({ name: 'Unknown time', sub: `${n0(hl.unknown_spans_total ?? spans.length)} spans in the period (asleep is known, not unknown)`, word: formatDuration(hl.unknown_ms ?? 0), level: hl.unknown_ms ? 'warn' : 'ok' }),
      checkRow({ name: 'Chain gaps', sub: 'Logs missing from the hash chain', word: n0(hl.chain_gaps_total ?? (hl.chain_gaps || []).length), level: (hl.chain_gaps || []).length ? 'warn' : 'ok' }),
      checkRow({ name: 'Dropped frames', sub: 'Times the bridge could not keep up', word: n0(hl.dropped_frames_total ?? (hl.dropped_frames || []).length), level: (hl.dropped_frames || []).length ? 'warn' : 'ok' }),
      checkRow({ name: 'Uploads', sub: 'From this car’s bridge', word: hl.uploads_enabled ? 'On' : 'Off', level: hl.uploads_enabled ? 'ok' : 'warn' }),
      checkRow({ name: 'Rebuild', sub: 'Drives and charges from the logs', word: hl.rebuild_pending ? 'Running' : 'Idle', level: hl.rebuild_pending ? 'warn' : 'ok' })),
    reasons.length ? h('ul', { class: 'fixes' }, reasons.map((code) => {
      const r = REASONS[code] || [code, '', null];
      return h('li', { class: 'fix' }, h('b', null, r[0]), h('span', null, r[1]),
        r[2] ? linkBtn(r[2], state.buildUrl('settings-cars', { car: car.id }), { icon: 'key', cls: 'fix-btn' }) : null);
    })) : null);

  /* the keys and the rebuild */
  const keys = hl.keys || [];
  const keysT = tile({ w: 2, title: 'CAN Bridge keys', icon: 'key' },
    keys.length ? h('ul', { class: 'checks' }, keys.map((k) => checkRow({ name: k.device || 'Unnamed device', sub: k.key, word: k.last_upload_ms != null ? ago(k.last_upload_ms) : 'Never',
      level: k.logs_waiting ? 'warn' : k.last_upload_ms != null ? 'ok' : 'off', side: h('span', { class: 'check-word' }, k.preferred ? 'Preferred · ' : '', k.last_upload_ms != null ? ago(k.last_upload_ms) : 'never') })))
      : note('No key is tied to this car yet. Pair a CAN Bridge in car settings.'),
    ctx.admin ? h('div', { class: 'hl-rebuild' },
      btn('Rebuild this period', { kind: 'soft', icon: 'reset', onclick: (e) => rebuild(e.currentTarget) }),
      note('Drives, charges, states and visits are worked out again from the stored logs. Typed names and prices stay.'))
      : note('An admin can rebuild drives and charges from the stored logs.'));
  async function rebuild(b) {
    const ok = await confirmSheet({ title: 'Rebuild from the logs?', text: `Work out drives, charges, states and visits of ${car.name} again for this period.`, yes: 'Rebuild' });
    if (!ok || !ctx.alive()) return;
    b.disabled = true;
    try {
      const r = await ctx.api.post(`${base}/rebuild`, { from_ms: ctx.params.from_ms, to_ms: ctx.params.to_ms });
      ctx.toast(`Rebuilt from ${n0(r.logs_read ?? 0)} logs in ${took(r.took_ms)}.`, { next: true });
      ctx.reload();
    } catch (e) { ctx.toast(errorWords(e).text, { kind: 'error' }); } finally { b.disabled = false; }
  }

  /* logs the server should have but lost: asked of the car again, and those the car no longer has either */
  const ml = hl.missing_logs || {};
  const mn = ml.count || 0, mg = ml.gone || 0;
  const plural = (n, one, many) => `${n0(n)} ${n === 1 ? one : many}`;
  const missT = tile({ w: 2, title: 'Missing on the server', icon: 'server', cls: 'hl-miss' },
    h('ul', { class: 'checks' },
      checkRow({ name: 'Missing logs', sub: mn ? `Oldest from ${when(ml.oldest_ms)}${ml.found_ms != null ? `, found ${when(ml.found_ms)}` : ''}` : 'Every log the server recorded is here.',
        word: mn ? plural(mn, 'log', 'logs') : 'None', level: mn ? 'warn' : 'ok' }),
      checkRow({ name: 'Asked the car', sub: !mn ? 'Nothing to ask for.' : ml.asked ? `Asked the car to send them again${ml.asked_ms != null ? `, ${when(ml.asked_ms)}` : ''}.` : 'The server will ask the car to send them again.',
        word: !mn ? '—' : ml.asked ? (ml.asked_ms != null ? ago(ml.asked_ms) : 'Asked') : 'Not yet', level: !mn ? 'off' : 'warn' }),
      checkRow({ name: 'Lost for good', sub: mg ? `The car no longer has ${n0(mg)} of these either; oldest from ${when(ml.gone_oldest_ms)}. Their time shows as unknown time.` : 'The car still has every missing log.',
        word: mg ? plural(mg, 'log', 'logs') : 'None', level: mg ? 'bad' : 'ok' })),
    note(ml.scanned_ms != null ? `Last checked ${when(ml.scanned_ms)}. The server looks at start-up and once a day.` : 'Not checked yet. The server looks at start-up and once a day.'));

  /* the gaps and unknown time as a log */
  const lines = [
    ...(gaps.items || []).map((g) => ({ at: g.start_ms, end: g.end_ms, what: `${GAP_CAUSES[g.cause] || g.cause}${g.odometer_jump_km ? `, odometer moved ${format(g.odometer_jump_km, 'distance')}` : ''}` })),
    ...spans.map((s) => ({ at: s.start_ms, end: s.end_ms, what: `Unknown: ${SPAN_REASONS[s.reason] || s.reason}` })),
  ].sort((a, b) => b.at - a.at).slice(0, 60);
  const logT = tile({ w: 2, title: 'Gaps in the period', icon: 'timeline' },
    // the gaps and unknown spans scroll in a box about five lines high (at most: a short log leaves no empty box)
    lines.length ? listBox(ctx, { fit: true, label: 'Gaps and unknown time in the period', cls: 'log-box' }, h('div', { class: 'log', role: 'list' }, lines.map((l) => h('div', { class: 'log-row', role: 'listitem' },
      h('a', { class: 'log-k', href: timelineAt(l.at, l.end ?? api.now()), title: 'Open the timeline here' }, `${when(l.at)}  ${l.what}`),
      h('span', { class: 'log-v' }, l.end == null ? 'open' : formatDuration(l.end - l.at)))))).el
      : h('p', { class: 'lede' }, 'No gaps: every moment of the period is known.'),
    note('Newest first. A gap in which the odometer moved means a drive was not recorded.'));

  /* ------------------------------------------------------------ what is recorded */
  const sm = upd?.summary || {};
  const tots = [
    statTile({ label: 'Logged distance', value: all.distance_km, kind: 'distance', icon: 'route', cover: 'dbinfo.stat.logged_distance', sub: `${format(can.distance_km ?? 0, 'distance')} from SMO, ${format(tapi.distance_km ?? 0, 'distance')} from the Tesla API${tfi.distance_km ? `, ${format(tfi.distance_km, 'distance')} from TeslaFi` : ''}` }),
    statTile({ label: 'Odometer', value: all.odometer_last_km, kind: 'odometer', icon: 'gauge', cover: 'dbinfo.stat.odometer', why: 'not sent yet', sub: all.odometer_last_ms != null ? h('span', null, 'read ', ago(all.odometer_last_ms)) : null }),
    statTile({ label: 'Drives', value: all.drives ?? 0, kind: 'count', icon: 'route', cover: 'dbinfo.stat.drive_count', sub: `${n0(tm.drives ?? 0)} imported` }),
    statTile({ label: 'Charges', value: all.charges ?? 0, kind: 'count', icon: 'bolt', cover: 'dbinfo.stat.charge_count', sub: `${n0(tm.charges ?? 0)} imported` }),
    statTile({ label: 'Software updates', value: all.updates ?? 0, kind: 'count', icon: 'download', cover: 'dbinfo.stat.update_count', sub: `${n0(tm.updates ?? 0)} imported` }),
    tile({ w: 1, title: 'Software version', icon: 'chip', cls: 'tile-stat', cover: 'dbinfo.stat.current_version', href: state.buildUrl('software', { car: car.id }) },
      h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, sm.current_version || '—')),
      h('p', { class: 'tile-line' }, sm.current_version ? (VERSION_SOURCE[sm.current_version_source] ? `${VERSION_SOURCE[sm.current_version_source]}, never guessed` : 'never guessed') : 'not known yet')),
  ];
  const rowKinds = [['Drives', 'drives'], ['Charges', 'charges'], ['Software updates', 'updates']];
  const rowsT = tile({ w: 2, title: 'Rows kept', icon: 'book', cover: 'dbinfo.table.row_counts' },
    h('div', { class: 'hl-rows', role: 'table', 'aria-label': 'Rows kept' },
      h('div', { class: 'hl-row hl-row-head', role: 'row' }, h('span', { role: 'columnheader', cover: 'dbinfo.rows.table_name' }, 'Kind'), h('span', { role: 'columnheader' }, 'SMO'), h('span', { role: 'columnheader' }, 'Tesla API'), h('span', { role: 'columnheader' }, 'TeslaFi'), h('span', { role: 'columnheader' }, 'Imported'), h('span', { role: 'columnheader', cover: 'dbinfo.rows.row_count' }, 'Rows')),
      rowKinds.map(([name, k]) => h('div', { class: 'hl-row', role: 'row' }, h('span', { role: 'cell' }, name), h('span', { role: 'cell' }, n0(can[k] ?? 0)), h('span', { role: 'cell' }, n0(tapi[k] ?? 0)), h('span', { role: 'cell' }, n0(tfi[k] ?? 0)), h('span', { role: 'cell' }, n0(tm[k] ?? 0)), h('b', { role: 'cell' }, n0(all[k] ?? 0))))),
    note('Exact counts, kept up to date as rows change. A row two live inputs (SMO, the Tesla API, TeslaFi) both saw counts in both columns, and once under Rows; where imported and live history overlap both are counted.'));
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
  const listEl = h('div', { class: 'sig-list', role: 'table', 'aria-label': 'Signals', tabindex: '0', cover: ['nr.dash.state-table', 'nr.dash.state-table.sort-scroll'] });
  const sortPills = pills({ label: 'Sort by', value: sort, options: [['name', 'Name'], ['value', 'Value'], ['message', 'Message'], ['at', 'Newest']], onchange: (v) => { sort = v; paint(); } });
  function paint() {
    const words = search.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    let rows = sig.filter((s) => !words.length || words.every((w) => [s.series, s.comment, s.message_id, s.bus, s.unit, s.value_name, text(s)].filter(Boolean).join(' ').toLowerCase().includes(w)));
    const val = (s) => (s.value_name ? null : s.value == null ? null : kindOf(s) ? convert(s.value, kindOf(s)) : s.value);
    const cmp = { name: (a, b) => (a.comment || a.series).localeCompare(b.comment || b.series), value: (a, b) => (val(b) ?? -Infinity) - (val(a) ?? -Infinity), message: (a, b) => `${a.bus}${a.message_id}`.localeCompare(`${b.bus}${b.message_id}`), at: (a, b) => (b.at_ms || 0) - (a.at_ms || 0) }[sort];
    rows = rows.slice().sort(cmp);
    count.textContent = words.length ? `${n0(rows.length)} of ${n0(sig.length)}` : `${n0(sig.length)} signals`;
    listEl.replaceChildren(
      h('div', { class: 'sig-row sig-head', role: 'row' }, h('span', { role: 'columnheader', cover: 'nr.dash.state-table.parameter' }, 'Signal'), h('span', { role: 'columnheader' }, 'Message'), h('span', { role: 'columnheader', cover: 'nr.dash.state-table.value' }, 'Value')),
      ...(rows.length ? rows.slice(0, 400).map((s) => h('div', { class: 'sig-row', role: 'row' },
        h('span', { role: 'cell', class: 'sig-name' }, h('b', null, s.comment || s.series), h('span', { class: 'sig-id' }, s.series)),
        h('span', { role: 'cell', class: 'sig-msg' }, `${s.message_id} · ${s.bus === 'chassis' ? 'chassis' : s.bus === 'vehicle' ? 'vehicle' : s.bus}`),
        h('span', { role: 'cell', class: 'sig-val' }, text(s)))) : [h('p', { class: 'lede sig-none' }, sig.length ? 'No signal matches.' : 'Signals show once the first log is decoded.')]));
  }
  let timer = 0;
  ctx.on(search, 'input', () => { clearTimeout(timer); timer = setTimeout(() => ctx.alive() && paint(), 150); });
  ctx.onCleanup(() => clearTimeout(timer));
  const sigT = tile({ w: 'full', title: 'Signals', icon: 'pulse', cls: 'sig-tile' },
    h('div', { class: 'page-bar' }, h('label', { class: 'search-pill' }, icon('search'), search), sortPills, count), listEl);
  listEl.append(plotGhost());

  ctx.view.replaceChildren(bar, status, keysT, missT, connTile(ctx, ov, car.id), teslaT, teslafiT, teslamateT, logT, rowsT, ...tots, storeT, daysT, sigHead, sigT);
  sig = await sigP;
  if (!ctx.alive()) return {};
  paint();
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
