// Sentry Mate, "Logbook" UI pack: Data health and storage. Two addresses, one module.
//
// /cars/{car}/health   Is this car's data arriving, complete and trusted? Last upload, last log check,
//                      the bridge keys, chain gaps, dropped frames, unknown time, logs waiting and
//                      Rebuild all data (from the stored logs and API data, with its progress), each
//                      with its cause and the fix; the gaps of the period; what is recorded (all time,
//                      per source) and the rows kept; what the car's data takes on disk, by kind and by
//                      day; every decoded signal of the newest log, searchable, with a chosen signal
//                      drawn over the period.
// /system              The server: storage by kind, free space, growth and "full in"; every car's data
//                      status in one paged list; the server's own health (uptime, versions, memory,
//                      housekeeping, timings), its clock and the /health address; the one error shape of
//                      the API, with a live try of an unknown car.
// Data: GET cars/{car}/health, cars/{car}/gaps, cars/{car}/totals?source=, cars/{car}/updates (summary),
//       cars/{car}/storage, cars/{car}/signals/latest, cars/{car}/signals; GET/POST cars/{car}/full-rebuild
//       and DELETE system/full-rebuild (admin: Rebuild all data);
//       GET system/storage, system/health (admin), geocode/status, cars.
// Live: a "rows" event for the car loads its health, gaps and totals again.

import { h, svg, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { format, convert, unitLabel, DASH } from '../shared/units.js';
import { column, timeColumn, downloadCsv, csvFilename, rowsFromColumns } from '../shared/csv.js';
import { formatDateTime, formatTime, formatDuration, formatDayKey, dayStart, HOUR, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { pageHead, section, figure, figures, q, ago, icon, empty, errorView, errorWords, flag, chip, button, sep, stateMark, loadingBlock, note } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { stackedCharts } from '../core/chart.js';
import { zoneLabel, count } from '../core/format.js';
import { sourceMark } from '../core/sources.js';
import { overviewCard } from './integrations-overview.js';
import { deviceText, renameButton } from './device-name.js';
import { fullRebuild } from './rebuild-parts.js';
import * as fr from '../shared/fullrebuild.js';

export const id = 'health';
export const css = true;
export const title = (ctx) => (ctx.car ? 'Data health and storage' : 'Server and storage');

/* ================================================================== words */

/** Why a car is not healthy: what it means, and the fix in the app. */
const REASONS = {
  upload_silence: {
    title: 'No log has arrived for over an hour',
    text: 'The CAN Bridge has not sent anything lately. Check that it has power and a network and that its key below still uploads. A car that sleeps sends little, and logs kept on the bridge arrive when it is back, so nothing is lost while it waits.',
    fix: 'Check the bridge keys', page: 'settings-cars',
  },
  trust_failed: {
    title: 'The last log failed its check',
    text: 'Its signature or its place in the hash chain did not match, so its data is kept apart and not used. If the bridge was reset or replaced, pair it again so its new key is trusted.',
    fix: 'Pair the bridge again', page: 'settings-cars',
  },
  logs_waiting_trust: {
    title: 'Logs wait for a key to be trusted',
    text: 'They came from a key nobody has approved for this car yet. Approve or assign the key and they are read at once; nothing has to be uploaded again.',
    fix: 'Approve the key', page: 'settings-cars',
  },
  gps_inputs_disagree: {
    title: 'Positions from one input are out of step',
    text: 'One input’s GPS positions are far from the other inputs’ positions at the same moments, more than lag or noise explains (often a decoding problem). Its positions are not used there; the other inputs’ positions are. The rest of its data is still used.',
    fix: null, page: null,
  },
  logs_missing: {
    title: 'Logs missing on the server',
    text: 'The server lost some logs and asked the car to send them again. They are read as they arrive; any the car no longer has either stay unknown time. See Missing on the server below.',
    fix: null, page: null,
  },
  rebuild_pending: {
    title: 'Drives and charges are being rebuilt',
    text: 'They are being worked out again from the stored logs. The figures settle within a few seconds; nothing needs doing.',
    fix: null, page: null,
  },
};
/** The result of the last log check: [word, kind, sentence]. */
const TRUST = {
  verified: ['Verified', 'ok', 'Signed by a trusted key and in its place in the hash chain.'],
  unsigned: ['Unsigned', 'warn', 'The bridge sent it without a signature: its data is used, but cannot be proven unchanged.'],
  failed: ['Failed', 'warn', 'Its signature or chain did not match: its data is not used.'],
};
const TRUST_REASONS = {
  signature_mismatch: 'the signature does not match the key',
  chain_break: 'it does not follow the log before it',
  unknown_key: 'the key is not known',
  bad_format: 'the file could not be read',
};
const SPAN_REASONS = {
  no_log: 'no log arrived', dropped_frames: 'the bridge dropped frames', failed_check: 'the log failed its check',
  chain_gap: 'a log is missing from the chain', paused: 'recording was paused',
};
const GAP_CAUSES = {
  no_log: ['No log', 'Nothing arrived for this time.'],
  not_recorded: ['Not recorded', 'The car moved while the bridge was not recording.'],
  chain_gap: ['Chain gap', 'A log of the hash chain is missing.'],
  unknown: ['Unusable data', 'Data arrived but could not be used.'],
};
/** What each kind of stored data holds, in plain words. */
const KIND_TEXT = {
  logs: 'The CAN logs as the bridge sent them, compressed and sealed. Everything else can be rebuilt from these.',
  series: 'Values over time (one a second while driving or charging) that the charts draw.',
  curves: 'Power, volts and amps of every charge, second by second.',
  rows: 'One row per drive, charge, state, visit and update.',
  imported: 'History brought in from TeslaMate, kept as it came.',
  tiles: 'Map tiles kept on this server, so maps work without the internet.',
  places: 'Places, prices and settings.',
  geocode: 'Addresses already looked up, so each is looked up once.',
};
const VERSION_SOURCE = {
  teslamate: 'imported from TeslaMate', teslafi: 'imported from TeslaFi', teslafi_api: 'TeslaFi (read live)', teslamate_local: 'local TeslaMate (read live)',
  tesla_api: 'the Tesla API', owner: 'typed by an admin', build_match: 'the same module builds as a named update',
};

/** A signal's unit (as its DBC names it) -> the kind of shared/units.js, so it follows the person's units. */
const UNIT_KIND = { km: 'distance', 'km/h': 'speed', '°C': 'temperature', kPa: 'pressure', kWh: 'energy', kW: 'power', '%': 'percent', V: 'voltage', A: 'current', m: 'length', '°': 'degrees' };
const COORDS = new Set(['gps_latitude', 'gps_longitude']);
const signalKind = (s) => (s.series === 'odometer' ? 'odometer' : COORDS.has(s.series) ? null : UNIT_KIND[s.unit] || null);
const BUS = { vehicle: 'Vehicle bus', chassis: 'Chassis bus' };

function signalText(s) {
  if (s.value_name) return s.value_name;
  if (s.value == null) return DASH;
  if (COORDS.has(s.series)) return `${formatNumber(s.value, { minimumFractionDigits: 5, maximumFractionDigits: 5 })}°`;
  const k = signalKind(s);
  if (k) return format(s.value, k);
  return s.unit ? `${formatNumber(s.value, { maximumFractionDigits: 2 })} ${s.unit}` : formatNumber(s.value, { maximumFractionDigits: 2 });
}

/** The error codes of the API, in plain words: [status, code, when]. */
const API_ERRORS = [
  [400, 'bad_input', 'A value has the wrong type, a field is unknown or a filter is not valid; fields names each one.'],
  [401, 'not_signed_in', 'No session, or it ended. Sign in again.'],
  [403, 'csrf', 'A change came without the X-Sentry-Mate header, so it may not come from this app.'],
  [403, 'not_allowed', 'The account may read this but not change it (a watch account), or it is for admins only.'],
  [404, 'no_such_car', 'No such car, or this account may not see it. Both answer the same, so a hidden car cannot be found by trying ids.'],
  [404, 'not_found', 'No such address. A missing drive, charge, place or other row has its own no_such_… code.'],
  [405, 'method_not_allowed', 'This address does not take that method; the Allow header lists the ones it does.'],
  [409, 'stale_version', 'Someone changed it first. The answer carries the current version: load it and try again.'],
  [409, 'import_running', 'Another import is not finished yet.'],
  [413, 'too_large', 'The body is over 1 MB.'],
  [422, 'invalid', 'Well formed but against a rule (an unknown time zone, an empty password); fields names each one.'],
  [429, 'throttled', 'Too many tries. Retry-After says how many seconds to wait.'],
  [500, 'server_error', 'Something broke on the server. Nothing was changed.'],
];

/* ================================================================== small parts */

const bytes = (v) => q(v, 'bytes');
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const isAdmin = (ctx) => !!(ctx.me && ctx.me.role === 'admin');
const when = (ms, tz) => (ms == null ? DASH : formatDateTime(ms, tz, { now: api.now() }));
const timelineAt = (car, from, to) => state.buildUrl('timeline', { car, period: { from: from - HOUR, to: to + HOUR } });

/** A ruled list of facts: [[name, value, explanation?], …]. */
function facts(list, { cls = null } = {}) {
  return h('dl', { class: ['hl-facts', cls] }, list.filter(Boolean).map(([name, value, why, cover]) =>
    h('div', { class: 'hl-fact', cover: cover || null },
      h('dt', null, name),
      h('dd', null, h('span', { class: 'hl-fact-v' }, value ?? DASH), why ? h('span', { class: 'hl-fact-why' }, why) : null))));
}

/** The "as of" line and the Load again button of a page head. */
function headAside(ctx, onReload, text) {
  const asOf = h('p', { class: 'hl-asof' });
  const paint = () => replace(asOf, 'As of ', formatTime(api.now(), ctx.tz, { seconds: true }), '. ', text);
  paint();
  const btn = button('Load again', { iconName: 'reset', cls: 'button-small', cover: 'dbinfo.control.refresh', title: 'Read every figure on this page again now.', onclick: () => onReload(btn) });
  return { el: h('div', { class: 'hl-head-aside' }, btn, asOf), paint, btn };
}

/** A table of storage kinds with a share bar, largest first, and the total under it. */
function kindsTable(ctx, kinds, total, { parts, label }) {
  const rows = (kinds || []).map((k) => ({ ...k, share: total ? (k.bytes / total) * 100 : null }));
  const t = dataTable(ctx, {
    rows, label, itemName: ['kind', 'kinds'], sort: { key: 'bytes', dir: 'desc' }, cls: 'hl-kinds',
    cover: 'dbinfo.table.sizes',
    caption: 'As stored on disk: compressed and encrypted. Click a heading to sort.',
    csv: { parts, columns: [{ label: 'What', value: (r) => r.label }, { label: 'Kind', value: (r) => r.kind }, column('On disk (bytes)', 'bytes'), column('Share', 'share', 'percent', { digits: 2 })] },
    columns: [
      { key: 'label', label: 'What it holds', th: true, sort: true, cover: 'dbinfo.sizes.table', value: (r) => r.label,
        cell: (r) => [h('span', { class: 'cell-main' }, r.label), KIND_TEXT[r.kind] ? h('span', { class: 'cell-sub hl-wrap' }, KIND_TEXT[r.kind]) : null] },
      { key: 'bytes', label: 'On disk', num: true, sort: true, cover: 'dbinfo.sizes.data', value: (r) => r.bytes,
        cell: (r) => h('span', { class: 'hl-size' }, bytes(r.bytes),
          h('span', { class: 'hl-bar', 'aria-hidden': 'true' }, h('span', { class: 'hl-bar-fill', vars: { '--w': `${Math.max(0.5, Math.min(100, r.share || 0))}%` } }))) },
      { key: 'share', label: 'Share', num: true, sort: true, value: (r) => r.share, cell: (r) => q(r.share, 'percent', { digits: 1 }) },
    ],
    empty: () => empty({ title: 'Nothing is stored yet.', text: 'Storage grows once the first log arrives.' }),
  });
  const totalEl = h('div', { class: 'hl-total', cover: 'dbinfo.sizes.total' },
    h('span', { class: 'hl-total-name' }, 'All kinds together'), h('span', { class: 'hl-total-v' }, bytes(total)));
  return { table: t, el: h('div', { class: 'hl-kinds-box' }, t.el, totalEl) };
}

/**
 * One section's loader that never takes the page down: an error is drawn in the section's own
 * box with a Try again button. Only a car that does not exist (or may not be seen) is passed on,
 * so the frame can say so for the whole page. Resolves true when the section drew its data.
 */
function guarded(ctx, eyebrow, onError, fn) {
  const run = async () => {
    try {
      await fn();
      return true;
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return false;
      if (e && e.status === 404 && e.code === 'no_such_car') throw e;
      onError(errorView(e, { retry: () => { run().catch(() => {}); }, eyebrow, title: 'This part could not be read; the rest of the page is fine.', home: false }), e);
      return false;
    }
  };
  return run;
}

/** Waits for every section; fails the page only when the car itself is not there. */
async function settleAll(list) {
  const out = await Promise.allSettled(list);
  const gone = out.find((r) => r.status === 'rejected' && r.reason && r.reason.status === 404 && r.reason.code === 'no_such_car');
  if (gone) throw gone.reason;
  return out.filter((r) => r.status === 'rejected' || r.value === false).length;
}

/** The flash after Load again: says which parts could not be read instead of hiding them. */
function reloadedFlash(ctx, failed) {
  if (!failed) ctx.flash('Loaded again.', { kind: 'info' });
  else ctx.flash(`Loaded again; ${failed === 1 ? 'one part' : `${formatNumber(failed)} parts`} could not be read and ${failed === 1 ? 'shows' : 'show'} why in place.`, { kind: 'error' });
}

/* ================================================================== mount */

export async function mount(ctx) {
  return ctx.car ? mountCar(ctx) : mountSystem(ctx);
}

/** Connections (admins): a card to the Connections overview of Settings: Integrations, with the totals. */
function connSection(ctx) {
  if (!isAdmin(ctx)) {
    ctx.main.append(section({ title: 'Connections', note: 'every input and outside connection', cls: 'hl-block', id: 'hl-conn' },
      empty({ cover: 'conn.overview.link', title: 'For admins only.', text: 'The connections overview (is each input and outside connection live, and what went wrong this week) is shown to admins.' })));
    return Promise.resolve(true);
  }
  const card = overviewCard(ctx, { href: state.buildUrl('settings-integrations') });
  ctx.main.append(section({ title: 'Connections', note: 'every input and outside connection, for admins', cls: 'hl-block', id: 'hl-conn' }, card.el));
  return card.load().then(() => true, () => false);
}

/* ================================================================== one car */

async function mountCar(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const admin = isAdmin(ctx);
  const base = (rest) => `${api.apiPath`cars/${cid}`}/${rest}`;
  const periodWords = ctx.period ? ctx.period.label : 'the period';

  const aside = headAside(ctx, (btn) => reloadAll(btn), 'Health, gaps and totals also load again by themselves when the car’s data changes.');
  ctx.main.append(pageHead({
    eyebrow: h('span', null, 'Set-up', sep(), car.name || cid),
    title: 'Data health and storage',
    deck: 'Whether this car’s data arrives, is complete and can be trusted, with the cause and the fix for anything that is not; what has been recorded; and how much room it takes.',
    aside: aside.el, cover: 'shots.database-info',
  }));

  // what the page covers: one car (the switcher at the top), the period for gaps; totals are all time
  ctx.main.append(h('section', { class: 'hl-scope', 'aria-label': 'What the page covers' },
    h('div', { class: 'hl-scope-item', cover: 'dbinfo.filter.car' },
      h('span', { class: 'field-label' }, 'Car'),
      h('p', { class: 'hl-scope-v' }, car.name || cid, car.model ? h('span', { class: 'hl-scope-sub' }, ` ${[car.model, car.year].filter(Boolean).join(' ')}`) : null),
      h('p', { class: 'field-hint' }, 'Every figure here is about this car. The storage of the whole server and every car’s status in one list are on Server and storage.'),
      h('div', { class: 'hl-scope-actions' },
        button('Choose another car', { cls: 'button-small', iconName: 'search', onclick: () => document.querySelector('.switcher-trigger')?.click() }),
        h('a', { class: 'link-arrow', href: state.buildUrl('health') }, 'Server and storage', icon('arrowRight')))),
    h('div', { class: 'hl-scope-item' },
      h('span', { class: 'field-label' }, 'Period'),
      h('p', { class: 'hl-scope-v' }, periodWords),
      h('p', { class: 'field-hint' }, 'The gaps, dropped frames, unknown time and the signal chart follow the period. Totals and storage are all time. Times in ', zoneLabel(tz), '.'),
      h('div', { class: 'hl-scope-actions' },
        button('Change the period', { cls: 'button-small', iconName: 'calendar', onclick: () => document.querySelector('.period-trigger')?.click() })))));

  /* ---------------------------------------------------------------- health */

  // Rebuild all data (admins): everything worked out made again from the stored data, at the foot of the
  // checks; while one runs for this car its step shows on the Rebuild line
  const rebuildWord = h('span');
  let lastHl = null;
  let fullLines = null;
  function paintRebuild(fs = full && full.status(), ls = fullLines) {
    fullLines = ls;
    const step = fs && fr.running(fs) && fs.this_car && ls ? ls[1].text : '';
    replace(rebuildWord, step ? h('span', { class: 'hl-warn' }, `Running: ${step}`)
      : lastHl && lastHl.rebuild_pending ? h('span', { class: 'hl-warn' }, 'Running') : 'Nothing pending');
  }
  const full = admin ? fullRebuild(ctx, { car, label: 'Rebuild all data', onStatus: paintRebuild }) : null;

  const healthBox = h('div', { class: 'hl-health' }, loadingBlock(5));
  ctx.main.append(section({ title: 'Is data arriving?', note: 'checked now, over the period', cover: 'faq.stopped-recording', cls: 'hl-block', id: 'hl-health',
    actions: h('a', { class: 'link-arrow', href: state.buildUrl('settings-cars', { car: cid }) }, 'Settings: Cars', icon('arrowRight')) }, healthBox));
  const connCard = connSection(ctx);


  const loadHealth = guarded(ctx, 'Could not check the data', (v) => replace(healthBox, v), async () => {
    const hl = await ctx.api.get(base('health'), ctx.params);
    if (ctx.alive()) drawHealth(hl);
  });

  function drawHealth(hl) {
    const reasons = hl.reasons || [];
    const trust = hl.last_log_trust || {};
    const tw = TRUST[trust.result];
    const spans = hl.unknown_spans || [];
    const gaps = hl.chain_gaps || [];
    const dropped = hl.dropped_frames || [];
    const keys = hl.keys || [];
    const now = api.now();

    const status = h('div', { class: ['hl-status', hl.healthy ? 'is-ok' : 'is-warn'], role: 'status' },
      h('span', { class: 'hl-status-mark' }, icon(hl.healthy ? 'check' : 'warning')),
      h('div', null,
        h('p', { class: 'hl-status-title' }, hl.healthy ? 'Data arrives and checks out.' : reasons.length === 1 ? 'One thing needs a look.' : `${formatNumber(reasons.length)} things need a look.`),
        h('p', { class: 'hl-status-text' }, hl.healthy
          ? 'Logs arrive, pass their signature and chain checks, and none waits. The same test sets “healthy” for this car in MQTT.'
          : 'Each one is listed with its cause and the fix. The same test sets “healthy” for this car in MQTT.')));

    const fixes = reasons.length ? h('ul', { class: 'hl-fixes' }, reasons.map((code) => {
      const r = REASONS[code] || { title: code, text: '', fix: null };
      return h('li', { class: 'hl-fix' },
        h('span', { class: 'hl-fix-mark' }, icon('warning')),
        h('div', null,
          h('p', { class: 'hl-fix-title' }, r.title),
          r.text ? h('p', { class: 'hl-fix-text' }, r.text) : null,
          r.fix ? h('a', { class: 'link-arrow', href: state.buildUrl(r.page, { car: cid }) }, r.fix, icon('arrowRight')) : null));
    })) : null;

    const figs = figures([
      figure('Last upload', hl.last_upload_ms != null ? ago(hl.last_upload_ms, { stale: now - hl.last_upload_ms > HOUR }) : q(null, 'count', { why: 'nothing yet' }),
        hl.last_upload_ms != null ? `${when(hl.last_upload_ms, tz)}. ${now - hl.last_upload_ms > HOUR ? (reasons.includes('upload_silence') ? 'Over an hour ago: see the fix above.' : 'Over an hour ago, but the car is asleep: nothing to fix.') : 'Logs arrive as the car sends them.'}` : 'No log has arrived. Pair a CAN Bridge on Settings: Cars.'),
      figure('Last log check', tw ? h('span', { class: `hl-${tw[1]}` }, tw[0]) : q(null, 'count', { why: 'none checked yet' }),
        tw ? [tw[2], trust.reason ? ` Why: ${TRUST_REASONS[trust.reason] || trust.reason}.` : '', trust.log_start_ms != null ? ` The log from ${when(trust.log_start_ms, tz)}.` : ''].join('') : 'A log is checked as soon as it arrives.'),
      figure('Logs waiting', q(hl.logs_waiting ?? 0, 'count'), hl.logs_waiting ? 'Arrived, not read yet: their key is not trusted.' : 'Every log that arrived has been read.'),
      figure('Unknown time', hl.unknown_ms != null ? q(hl.unknown_ms, 'duration') : q(null, 'duration', { why: 'not known' }),
        `In the period: time with no usable data, in ${count(spans.length, 'span')}. Asleep is known time, not counted here.`),
      figure('Chain gaps', q(gaps.length, 'count'), gaps.length ? 'Logs missing from the hash chain in the period: listed under Gaps.' : 'No log is missing from the hash chain in the period.'),
      figure('Dropped frames', q(dropped.length, 'count'), dropped.length ? `Times the bridge could not keep up and lost CAN frames, ${formatDuration(dropped.reduce((a, d) => a + Math.max(0, (d.end_ms ?? now) - d.start_ms), 0))} in all.` : 'The bridge kept up the whole period.'),
    ], { cls: 'hl-figs' });

    const keyList = h('div', { class: 'hl-col' },
      h('h3', { class: 'hl-sub' }, 'CAN Bridge keys of this car'),
      keys.length ? h('ul', { class: 'hl-list' }, keys.map((k) => h('li', { class: 'hl-key' },
        h('div', null,
          h('span', { class: 'hl-key-name' }, deviceText(k), k.preferred ? [' ', flag('Preferred', { title: 'When two keys send the same time, this one is used.' })] : null),
          h('span', { class: 'cell-sub mono' }, k.key)),
        h('div', { class: 'hl-key-side' },
          k.last_upload_ms != null ? ago(k.last_upload_ms) : h('span', { class: 'hl-faint' }, 'never uploaded'),
          h('span', { class: ['cell-sub', { 'hl-warn': !!k.logs_waiting }] }, k.logs_waiting ? `${formatNumber(k.logs_waiting)} waiting` : 'none waiting'),
          renameButton(ctx, k, (row) => { Object.assign(k, row); drawHealth(hl); })))))
        : note('No key is tied to this car yet. Pair a CAN Bridge on Settings: Cars.'),
      facts([
        ['Uploads', hl.uploads_enabled ? h('span', { class: 'hl-ok' }, 'Accepted') : h('span', { class: 'hl-warn' }, 'Turned off for this car'), hl.uploads_enabled ? null : 'Turn them on again on Settings: Cars.'],
        ['Rebuild', rebuildWord],
      ]));

    const spanList = h('div', { class: 'hl-col' },
      h('h3', { class: 'hl-sub' }, 'Unknown time in the period'),
      spans.length ? h('ul', { class: 'hl-list' }, spans.map((s) => h('li', { class: 'hl-span' },
        h('a', { href: timelineAt(cid, s.start_ms, s.end_ms ?? now), title: 'Open the timeline at this time' }, when(s.start_ms, tz)),
        h('span', { class: 'cell-sub' }, s.end_ms == null ? 'still unknown' : `for ${formatDuration(s.end_ms - s.start_ms)}`, ' · ', SPAN_REASONS[s.reason] || s.reason || 'reason not known'))))
        : note('None: every moment of the period is known (driving, charging, parked or asleep).'),
      missingCard(hl.missing_logs || {}));

    lastHl = hl;
    paintRebuild();
    const rebuild = full ? h('div', { class: 'hl-full', id: 'full-rebuild' }, full.el)
      : note('Nothing is ever repaired by hand: an admin can make all of this car’s data again from the stored logs and API data.');

    replace(healthBox, status, fixes, figs, h('div', { class: 'hl-cols' }, keyList, spanList), rebuild);
  }

  /** Missing on the server: logs the server should have but no longer does, asked of the car again. Always the same rows. */
  function missingCard(m) {
    const n = m.count || 0;
    const gone = m.gone || 0;
    return [
      h('h3', { class: 'hl-sub hl-sub-gap', id: 'hl-missing' }, 'Missing on the server'),
      facts([
        ['Missing', n ? h('span', { class: 'hl-warn' }, count(n, 'log')) : h('span', { class: 'hl-ok' }, 'None'),
          n ? `CAN logs (and their sidecars) the server should have but no longer does${m.found_ms != null ? `; found ${when(m.found_ms, tz)}` : ''}.` : 'Every log the server recorded is here.'],
        ['Oldest missing', n && m.oldest_ms != null ? when(m.oldest_ms, tz) : DASH, n ? 'When the oldest of them starts.' : 'Nothing is missing.'],
        ['Sent again', !n ? 'Nothing to ask' : m.asked ? 'Asked the car to send them again' : h('span', { class: 'hl-warn' }, 'Not asked yet'),
          !n ? 'The car is asked only for logs the server lost.' : m.asked ? `${m.asked_ms != null ? `Asked ${when(m.asked_ms, tz)}. ` : ''}Each is read as it arrives.` : 'The car is asked the next time it uploads.'],
        ['Lost for good', gone ? h('span', { class: 'hl-warn' }, `The car no longer has ${formatNumber(gone)} of these either`) : 'None',
          gone ? `Their time shows as unknown time on the timeline${m.gone_oldest_ms != null ? `; the oldest from ${when(m.gone_oldest_ms, tz)}` : ''}.` : 'Nothing is lost for good.'],
        ['Last checked', m.scanned_ms != null ? when(m.scanned_ms, tz) : 'Not checked yet', 'The server looks at start-up and once a day.'],
      ], { cls: 'hl-missing' }),
    ];
  }

  /* ---------------------------------------------------------------- gaps */

  const gapsTable = dataTable(ctx, {
    path: base('gaps'), params: ctx.params, itemName: ['gap', 'gaps'], cls: 'hl-gaps', label: 'Gaps in the period',
    caption: 'Newest first. A gap in which the odometer moved is a drive nobody recorded. Open one on the timeline.',
    columns: [
      { key: 'start', label: 'From', th: true, cell: (g) => h('a', { class: 'cell-main', href: timelineAt(cid, g.start_ms, g.end_ms ?? api.now()), title: 'Open the timeline at this gap' }, when(g.start_ms, tz)) },
      { key: 'dur', label: 'Lasted', num: true, cell: (g) => (g.end_ms == null ? flag('Still open', { kind: 'warn' }) : q(g.duration_ms ?? g.end_ms - g.start_ms, 'duration')) },
      { key: 'cause', label: 'Cause', cell: (g) => { const c = GAP_CAUSES[g.cause] || [g.cause || 'Not known', '']; return [h('span', null, c[0]), h('span', { class: 'cell-sub' }, g.gap_reason ? SPAN_REASONS[g.gap_reason] || g.gap_reason : c[1])]; } },
      { key: 'jump', label: 'Odometer moved', num: true, cell: (g) => q(g.odometer_jump_km, 'distance', { why: 'not known' }) },
      { key: 'where', label: 'Where', cell: (g) => [h('span', null, g.from_place?.name || 'No known place'), g.to_place && g.to_place.id !== g.from_place?.id ? h('span', { class: 'cell-sub' }, `to ${g.to_place.name}`) : null] },
      { key: 'source', label: 'Data from', cell: (g) => sourceMark(g, { short: true }) || DASH },
    ],
    empty: () => empty({ title: 'No gap in this period.', text: 'Every moment is accounted for. Choose a longer period to look further back.' }),
  });
  ctx.main.append(section({ title: 'Gaps in the period', note: 'time with no usable data', cls: 'hl-block', id: 'hl-gaps' }, gapsTable.el));

  /* ---------------------------------------------------------------- the Tesla API input for this car */

  const TESLA_MODE = {
    off: 'Off: not matched, left out, or its account is paused', watching_list: 'Watching the car list (asleep or offline; never woken)',
    online_idle: 'Awake, idle: read at the parked pace', letting_sleep: 'Letting it sleep (left alone so it can fall asleep)',
    charging: 'Charging: read at the charging pace', driving: 'Driving: read at the driving pace', backoff: 'Waiting after errors from Tesla',
  };
  const teslaBox = h('div', { class: 'hl-tesla' });
  const teslaLink = () => h('a', { class: 'link-arrow', href: `${state.buildUrl('settings-integrations')}#tesla` }, 'Tesla account settings', icon('arrowRight'));
  const loadTesla = guarded(ctx, 'Could not read the Tesla API input', (v) => replace(teslaBox, v), async () => {
    if (!admin) { replace(teslaBox, note('What the server asks of the Tesla API for this car is shown to admins.')); return; }
    const st = await ctx.api.get('integrations/tesla/status');
    if (!ctx.alive()) return;
    const mine = (st.items || []).filter((x) => x.car === cid);
    if (!mine.length) {
      replace(teslaBox, note(['This car is not read from a Tesla account: its data comes from SMO', (car.sources || []).some((x) => x.kind === 'import') ? ' and an import' : '', '. ', teslaLink()]));
      return;
    }
    replace(teslaBox, ...mine.map((x) => facts([
      ['Now', TESLA_MODE[x.mode] || x.mode, x.mode === 'backoff' && x.next_poll_ms ? `Nothing is asked until ${formatTime(x.next_poll_ms, tz)}.` : null],
      ['Tesla says', { online: 'Online', asleep: 'Asleep', offline: 'Offline' }[x.car_state] || 'Not known', x.streaming ? 'Tesla’s live stream runs beside the reads.' : null],
      ['Last asked', x.last_poll_ms ? ago(x.last_poll_ms) : 'not yet', x.next_poll_ms ? `Next at ${formatTime(x.next_poll_ms, tz)}.` : null],
      ['Newest data', x.last_data_ms ? ago(x.last_data_ms) : 'none yet'],
      ['Requests today', formatNumber(x.polls_today || 0), 'For its whole Tesla account.'],
      ['Last error', x.last_error ? (x.last_error.message || x.last_error.code) : 'None'],
    ], { cls: 'hl-tesla-facts' })), h('p', { class: 'hl-tesla-link' }, teslaLink()));
  });
  ctx.main.append(section({ title: 'Tesla API input', note: 'the second live input, beside SMO', cls: 'hl-block', id: 'hl-tesla' }, teslaBox));

  /* ---------------------------------------------------------------- totals and rows kept */

  const totalsBox = h('div', { class: 'hl-totals' }, loadingBlock(3));
  const rowsBox = h('div', { class: 'hl-rows' });
  ctx.main.append(section({ title: 'What is recorded', note: 'all time, not the period; from SMO, the Tesla API and imports', cls: 'hl-block', id: 'hl-totals' },
    totalsBox, h('h3', { class: 'hl-sub hl-sub-gap' }, 'Rows kept'), rowsBox));

  const loadTotals = guarded(ctx, 'Could not load the totals', (v) => { replace(totalsBox, v); replace(rowsBox, note('The row counts come with the totals: press Try again above.')); }, async () => {
    const [all, can, ta, tm, upd] = await Promise.all([
      ctx.api.get(base('totals'), { source: 'all' }),
      ctx.api.get(base('totals'), { source: 'smo' }),
      ctx.api.get(base('totals'), { source: 'tesla_api' }),
      ctx.api.get(base('totals'), { source: 'import' }),
      ctx.api.get(base('updates'), { limit: 1 }).catch(() => null),
    ]);
    if (ctx.alive()) drawTotals(all, can, ta, tm, upd && upd.summary);
  });

  // a row seen by SMO and the Tesla API counts on both sides
  const split = (a, t, b, kind) => `${format(a ?? 0, kind)} with data from SMO, ${format(t ?? 0, kind)} from the Tesla API, ${format(b ?? 0, kind)} imported.`;

  function drawTotals(all, can, ta, tm, sm) {
    sm = sm || {};
    const vs = VERSION_SOURCE[sm.current_version_source];
    replace(totalsBox, figures([
      figure('Logged distance', q(all.distance_km, 'distance'), `The sum of every recorded drive. ${split(can.distance_km, ta.distance_km, tm.distance_km, 'distance')}`, { cover: 'dbinfo.stat.logged_distance' }),
      figure('Odometer', q(all.odometer_last_km, 'odometer', { why: 'not sent yet' }), all.odometer_last_km != null && all.odometer_last_ms != null ? `Last read ${when(all.odometer_last_ms, tz)}; the car sends it while awake.` : 'The car sends it while awake.', { cover: 'dbinfo.stat.odometer' }),
      figure('Not logged', q(all.not_logged_km, 'distance', { why: 'needs two readings' }), 'The odometer’s change since the first reading minus the logged distance: driving nobody recorded.'),
      figure('Drives', q(all.drives, 'count'), split(can.drives, ta.drives, tm.drives, 'count'), { cover: 'dbinfo.stat.drive_count' }),
      figure('Charges', q(all.charges, 'count'), split(can.charges, ta.charges, tm.charges, 'count'), { cover: 'dbinfo.stat.charge_count' }),
      figure('Software updates', q(all.updates, 'count'), `${split(can.updates, ta.updates, tm.updates, 'count')} The car’s own are seen by its update status and by changed module builds.`, { cover: 'dbinfo.stat.update_count' }),
      figure('Software version', sm.current_version ? h('a', { href: ctx.link('software') }, sm.current_version) : h('span', { class: 'hl-unknown' }, 'Not known'),
        sm.current_version
          ? `${vs ? `From ${vs}` : 'Known'}${sm.current_version_since_ms != null ? `, since ${when(sm.current_version_since_ms, tz)}` : ''}.${sm.build_changed_since != null ? ' The software changed after that and the new version has no name yet.' : ''}`
          : 'The car does not send its version text. It is known once imported, typed on the Software page, or matched by module builds; never guessed.',
        { cover: 'dbinfo.stat.current_version' }),
    ], { cls: 'hl-tot-figs' }));

    const rows = [
      { kind: 'Drives', what: 'One row per drive, start to end', can: can.drives, api: ta.drives, teslamate: tm.drives, total: all.drives, page: 'drives' },
      { kind: 'Charges', what: 'One row per charge, with its curve kept apart', can: can.charges, api: ta.charges, teslamate: tm.charges, total: all.charges, page: 'charges' },
      { kind: 'Software updates', what: 'One row per update', can: can.updates, api: ta.updates, teslamate: tm.updates, total: all.updates, page: 'software' },
    ];
    const t = dataTable(ctx, {
      rows, itemName: ['kind of row', 'kinds of row'], sort: { key: 'total', dir: 'desc' }, cls: 'hl-counts', label: 'Rows kept',
      cover: 'dbinfo.table.row_counts',
      caption: 'Exact counts, kept up to date as rows change. A row SMO and the Tesla API both saw counts in both columns; where imported and live history overlap, both are counted here and the lists show one side.',
      csv: { parts: [car.name || cid, 'rows kept'], columns: [{ label: 'Kind', value: (r) => r.kind }, column('Data from SMO', 'can'), column('Data from the Tesla API', 'api'), column('Imported', 'teslamate'), column('Rows', 'total')] },
      columns: [
        { key: 'kind', label: 'Kind', th: true, sort: true, cover: 'dbinfo.rows.table_name', value: (r) => r.kind, cell: (r) => [h('a', { class: 'cell-main', href: ctx.link(r.page) }, r.kind), h('span', { class: 'cell-sub' }, r.what)] },
        { key: 'can', label: 'SMO', num: true, sort: true, hint: 'Rows with data from SMO', cell: (r) => q(r.can ?? 0, 'count') },
        { key: 'api', label: 'Tesla API', num: true, sort: true, hint: 'Rows with data from the Tesla API', cell: (r) => q(r.api ?? 0, 'count') },
        { key: 'teslamate', label: 'Imported', num: true, sort: true, cell: (r) => q(r.teslamate ?? 0, 'count') },
        { key: 'total', label: 'Rows', num: true, sort: true, cover: 'dbinfo.rows.row_count', cell: (r) => h('b', null, q(r.total, 'count')) },
      ],
    });
    replace(rowsBox, t.el);
    t.load();
  }

  /* ---------------------------------------------------------------- storage of this car */

  const storeBox = h('div', { class: 'hl-store' }, loadingBlock(4));
  ctx.main.append(section({ title: `What ${car.name || cid}’s data takes`, note: 'all time, on this server’s disk', cls: 'hl-block', id: 'hl-store' }, storeBox));

  const loadStorage = guarded(ctx, 'Could not load the storage', (v) => replace(storeBox, v), async () => {
    const st = await ctx.api.get(base('storage'), { limit: 1 });
    if (!ctx.alive()) return;
    const k = kindsTable(ctx, st.kinds, st.total_bytes, { parts: [car.name || cid, 'storage by kind'], label: 'Storage by kind' });
    const labels = Object.fromEntries((st.kinds || []).map((x) => [x.kind, x.label]));
    const days = dataTable(ctx, {
      path: base('storage'), params: {}, limit: 30, itemName: ['day', 'days'], cls: 'hl-days', label: 'Storage by day',
      caption: 'Newest first, only days with data, in the car’s own days.',
      columns: [
        { key: 'date', label: 'Day', th: true, cell: (r) => h('a', { class: 'cell-main', href: state.buildUrl('timeline', { car: cid, period: { from: dayStart(r.date, tz), to: dayStart(r.date, tz) + DAY } }), title: 'Open this day on the timeline' }, formatDayKey(r.date)) },
        { key: 'series', label: 'What', cell: (r) => labels[r.series] || r.series },
        { key: 'source', label: 'Data from', cell: (r) => sourceMark(r, { short: true }) || DASH },
        { key: 'bytes', label: 'On disk', num: true, cell: (r) => bytes(r.bytes) },
      ],
      empty: () => empty({ title: 'No stored day yet.', text: 'A day shows here once a log for it has arrived.' }),
    });
    replace(storeBox, h('div', { class: 'hl-store-cols' },
      h('div', null, h('h3', { class: 'hl-sub' }, 'By kind'), k.el),
      h('div', null, h('h3', { class: 'hl-sub' }, 'By day'), days.el)));
    await Promise.all([k.table.load(), days.load().catch(() => {})]);
  });

  /* ---------------------------------------------------------------- signals */

  const search = h('input', { class: 'field-input', type: 'search', id: 'hl-sig-search', placeholder: 'Find a signal, message or value', autocomplete: 'off' });
  const sigInfo = h('span', { class: 'hl-faint' });
  let signals = [];
  let chosen = null;
  const sigTable = dataTable(ctx, {
    rows: [], itemName: ['signal', 'signals'], sort: { key: 'name', dir: 'asc' }, cls: 'hl-signals', label: 'Signals of the newest log',
    cover: ['nr.dash.state-table', 'nr.dash.state-table.sort-scroll'],
    caption: 'Sort by any heading; numbers sort as numbers. The list scrolls inside its box.',
    csv: { parts: [car.name || cid, 'signals'], columns: [
      { label: 'Signal', value: (s) => s.series }, { label: 'Name', value: (s) => s.comment || '' }, { label: 'Message', value: (s) => s.message_id },
      { label: 'Bus', value: (s) => s.bus }, { label: 'Value (as sent)', value: (s) => s.value }, { label: 'Unit (as sent)', value: (s) => s.unit || '' },
      { label: 'Value name', value: (s) => s.value_name || '' }, { label: 'Value (shown)', value: (s) => signalText(s) }, timeColumn('Read', 'at_ms', tz)] },
    columns: [
      { key: 'name', label: 'Signal', th: true, sort: true, cover: 'nr.dash.state-table.parameter', value: (s) => s.comment || s.series,
        cell: (s) => [h('span', { class: 'cell-main' }, s.comment || s.series), h('span', { class: 'cell-sub mono' }, s.series)] },
      { key: 'message', label: 'Message', sort: true, value: (s) => `${s.bus} ${s.message_id}`,
        cell: (s) => [h('span', { class: 'mono' }, s.message_id), h('span', { class: 'cell-sub' }, BUS[s.bus] || s.bus)] },
      { key: 'value', label: 'Value', num: true, sort: true, cover: 'nr.dash.state-table.value',
        value: (s) => (s.value_name ? s.value_name : s.value == null ? null : signalKind(s) ? convert(s.value, signalKind(s)) : s.value),
        cell: (s) => [h('span', { class: 'hl-sig-v' }, signalText(s)), s.value_name ? h('span', { class: 'cell-sub' }, `code ${s.value}`) : null] },
      { key: 'raw', label: 'As sent', num: true, hint: 'The value in the unit the car sends it in',
        cell: (s) => h('span', { class: 'hl-faint' }, s.value == null ? DASH : `${formatNumber(s.value, { maximumFractionDigits: 6 })}${s.unit ? ` ${s.unit}` : ''}`) },
      { key: 'at', label: 'Read', num: true, sort: true, value: (s) => s.at_ms, cell: (s) => ago(s.at_ms) },
      { key: 'plot', label: 'Over time',
        cell: (s) => (s.has_series
          ? h('button', { type: 'button', class: ['button', 'button-small', chosen === s.series ? 'button-primary' : 'button-quiet'], 'aria-pressed': chosen === s.series ? 'true' : 'false', onclick: () => plot(s), title: 'Draw this signal over the period' }, 'Draw')
          : h('span', { class: 'hl-faint', title: 'A signal with named values keeps its newest value only.' }, 'newest only')) },
    ],
    empty: () => empty({ title: signals.length ? 'No signal matches.' : 'No signal yet.',
      text: signals.length ? `Nothing holds “${search.value.trim()}”. Clear the search to see all ${formatNumber(signals.length)}.` : 'Signals show once the first log of this car has been decoded.' }),
  });

  const chooseEmpty = () => empty({ title: 'Choose a signal to draw.', text: 'Press Draw on a signal with values over time. It is drawn over the period in your units, with asleep and unknown time shaded apart and a break where nothing is known.' });
  const plotBox = h('div', { class: 'hl-plot' }, chooseEmpty());
  const sigErr = h('div', { class: 'hl-sig-err' });

  ctx.main.append(section({ title: 'Signals', note: 'every decoded signal of the newest log, with its name, unit and value names', cls: 'hl-block', id: 'hl-signals',
    actions: h('div', { class: 'field field-search hl-search' },
      h('label', { class: 'sr-only', for: 'hl-sig-search' }, 'Find a signal'),
      h('div', { class: 'field-control' }, h('span', { class: 'field-icon' }, icon('search')), search)) },
  h('p', { class: 'note hl-sig-info' }, sigInfo, 'Values are in your units; “as sent” is the value in the car’s own unit. A signal with named values (gear, charging status) shows its name and code.'),
  sigErr,
  sigTable.el,
  h('h3', { class: 'hl-sub hl-sub-gap' }, 'A signal over the period'),
  plotBox));

  function filterSignals() {
    const words = search.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const rows = !words.length ? signals : signals.filter((s) => {
      const hay = [s.series, s.comment, s.message_id, s.bus, BUS[s.bus], s.unit, s.value_name, signalText(s)].filter(Boolean).join(' ').toLowerCase();
      return words.every((w) => hay.includes(w));
    });
    sigTable.setRows(rows);
    replace(sigInfo, words.length ? `${formatNumber(rows.length)} of ${count(signals.length, 'signal')} match “${search.value.trim()}”. ` : '');
  }
  let timer = null;
  search.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => ctx.alive() && filterSignals(), 150); });
  ctx.onCleanup(() => clearTimeout(timer));

  const loadSignals = guarded(ctx, 'Could not load the signals', (v) => {
    replace(sigErr, v);
    sigTable.el.hidden = true;
    if (chosen == null) replace(plotBox, empty({ title: 'Nothing to draw until the signals load.', text: 'Press Try again above; the chart works once the list of signals has been read.' }));
  }, async () => {
    const list = await ctx.api.list(base('signals/latest'), {}, { limit: 500 }).all({ max: 20000 });
    if (!ctx.alive()) return;
    signals = list;
    replace(sigErr);
    sigTable.el.hidden = false;
    filterSignals();
    if (!signals.some((s) => s.has_series)) {
      chosen = null;
      replace(plotBox, empty({ title: 'Nothing can be drawn yet.', text: signals.length
        ? 'None of these signals has values over time: each keeps its newest value only.'
        : 'A signal can be drawn over time once the first log of this car has been decoded.' }));
    } else if (chosen == null) replace(plotBox, chooseEmpty());
  });

  let chart = null;
  async function plot(s) {
    chosen = s.series;
    sigTable.redraw();
    const kind = signalKind(s);
    const name = s.comment || s.series;
    replace(plotBox, loadingBlock(4));
    let d;
    try {
      d = await ctx.api.get(base('signals'), { series: s.series, ...ctx.params, max_points: 1500 });
    } catch (e) {
      if (ctx.alive() && chosen === s.series) replace(plotBox, errorView(e, { retry: () => plot(s), eyebrow: 'Could not load the signal' }));
      return;
    }
    if (!ctx.alive() || chosen !== s.series) return;
    const values = (d.values && d.values[s.series]) || [];
    const known = values.filter((v) => v != null).length;
    const unitText = kind ? unitLabel(kind) : s.unit || '';
    const csvBtn = button('Download CSV', { iconName: 'download', cls: 'button-small', onclick: () => downloadCsv(csvFilename(car.name || cid, 'signal', s.series), rowsFromColumns(d),
      [timeColumn('Time', 't_ms', tz), kind ? column(name, s.series, kind) : { label: `${name}${s.unit ? ` (${s.unit})` : ''}`, value: (r) => r[s.series] }]) });
    csvBtn.dataset.csv = '';
    const readout = h('p', { class: 'readout hl-readout' }, 'Point at the chart to read a value.');
    const charts = h('div', { class: 'charts hl-charts' });
    if (chart) { chart.destroy?.(); chart = null; }
    replace(plotBox,
      h('div', { class: 'hl-plot-head' }, h('p', { class: 'hl-plot-name' }, name, unitText ? h('span', { class: 'hl-faint' }, ` in ${unitText}`) : null), csvBtn),
      known ? [readout, charts] : empty({ title: 'Nothing recorded in this period.', text: 'The car was asleep or not recording. Choose a longer period.' }),
      note(`Each point is the mean of ${formatDuration(d.bucket_ms || 0)}. Shading marks asleep and unknown time: values are held across asleep time (nothing changes then) and never across unknown time.`));
    if (!known) return;
    const states = d.state || null;
    chart = await stackedCharts(ctx, charts, {
      tz, t_ms: d.t_ms, states,
      spans: spansOf(d.t_ms, states, d.bucket_ms || 0),
      rows: [{ key: 'signal', name, kind, unit: kind ? undefined : s.unit || '', values, colour: '--accent', fill: '--chart-wash', digits: kind === 'pressure' ? 2 : undefined }],
      onHover: (i) => {
        if (i == null) { replace(readout, 'Point at the chart to read a value.'); return; }
        const v = values[i];
        replace(readout, h('time', { class: 'readout-time' }, when(d.t_ms[i], tz)), ' ',
          h('b', null, v == null ? (states && states[i] === 'asleep' ? 'asleep' : 'not known') : kind ? format(v, kind) : `${formatNumber(v, { maximumFractionDigits: 3 })}${s.unit ? ` ${s.unit}` : ''}`));
      },
    });
  }
  function spansOf(t, states, step) {
    if (!states) return [];
    const out = [];
    for (let i = 0; i < t.length; i++) {
      const st = states[i];
      if (st !== 'asleep' && st !== 'unknown') continue;
      const last = out[out.length - 1];
      if (last && last.state === st && last.end_ms === t[i]) last.end_ms = t[i] + step;
      else out.push({ state: st, start_ms: t[i], end_ms: t[i] + step });
    }
    return out;
  }

  /* ---------------------------------------------------------------- load */

  async function reloadAll(btn) {
    if (btn) btn.disabled = true;
    try {
      const failed = await settleAll([loadHealth(), gapsTable.reload().then(() => true), loadTesla(), loadTotals(), loadStorage(), loadSignals()]);
      aside.paint();
      if (btn && ctx.alive()) reloadedFlash(ctx, failed);
    } catch (e) {
      if (ctx.alive()) ctx.flash(`Not loaded: ${errorWords(e).text}`, { kind: 'error' });
    } finally { if (btn) btn.disabled = false; }
  }

  ctx.events(['rows'], (ev) => {
    if (!ev || !ev.data || ev.data.car !== cid) return;
    loadHealth().catch(() => {});
    gapsTable.reload();
    loadTotals().catch(() => {});
    aside.paint();
  });

  await settleAll([loadHealth(), gapsTable.load(), loadTesla(), loadTotals(), loadStorage(), loadSignals(), connCard, full && full.ready]);
  if (!ctx.alive()) return {};
  // start with the battery level drawn, so the chart is there before anything is chosen
  const first = signals.find((s) => s.series === 'soc_display' && s.has_series) || signals.find((s) => s.has_series);
  if (first) await Promise.race([plot(first).catch(() => {}), new Promise((r) => { const t = setTimeout(r, 4000); ctx.onCleanup(() => clearTimeout(t)); })]);
  if (!ctx.alive()) return {};
  ctx.ready();
  return { unmount() { if (chart) chart.destroy?.(); } };
}

/* ================================================================== the server */

async function mountSystem(ctx) {
  const admin = isAdmin(ctx);
  const tz = ctx.tz;

  const aside = headAside(ctx, (btn) => reloadAll(btn), 'Each car’s own gaps, totals and signals are on its page: open one in the list.');
  ctx.main.append(pageHead({
    eyebrow: h('span', null, 'Set-up', sep(), 'Whole server'),
    title: 'Server and storage',
    deck: 'How much room the data takes and how fast it grows, every car’s data status in one list, how the server itself is doing, and how the API answers when something goes wrong.',
    aside: aside.el, cover: 'shots.database-info',
  }));

  const adminOnly = (what, coverId = null) => empty({ cover: coverId, title: 'For admins only.', text: `${what} is shown to admins. Your account sees its cars in the list below.` });

  /* ---------------------------------------------------------------- storage */

  const storeBox = h('div', { class: 'hl-store' }, loadingBlock(5));
  ctx.main.append(section({ title: 'Storage', note: 'every car, imported history and offline maps, on this server’s disk', cover: 'dbinfo.section.database', cls: 'hl-block', id: 'hl-store' }, storeBox));

  const loadStorage = guarded(ctx, 'Could not load the storage', (v) => replace(storeBox, v), async () => {
    if (!admin) { replace(storeBox, adminOnly('Server storage (total size, free space, growth)', 'dbinfo.stat.total_size')); return; }
    let st;
    try { st = await ctx.api.get('system/storage'); } catch (e) {
      if (e && e.status === 403) { replace(storeBox, adminOnly('Server storage (total size, free space, growth)', 'dbinfo.stat.total_size')); return; }
      throw e;
    }
    if (!ctx.alive()) return;
    const growth = st.growth || [];
    const perDay = st.growth_per_day_bytes ?? (growth.length ? growth.reduce((a, g) => a + g.bytes, 0) / growth.length : null);
    const used = st.total_bytes;
    const disk = used != null && st.free_bytes != null ? used + st.free_bytes : null;
    const full = st.full_in_days;
    const k = kindsTable(ctx, st.kinds, used, { parts: ['sentry-mate', 'storage by kind'], label: 'Server storage by kind' });
    replace(storeBox,
      figures([
        figure('Data takes', bytes(used), 'Every car’s data, imported history and offline maps together, as stored on disk.', { cover: 'dbinfo.stat.total_size' }),
        figure('Free on the disk', st.free_bytes != null ? bytes(st.free_bytes) : q(null, 'bytes', { why: 'not known' }),
          disk ? `Sentry Mate’s data is ${format((used / disk) * 100, 'percent', { digits: 1 })} of ${format(disk, 'bytes')}.` : 'The disk did not say.'),
        figure('Grows by', perDay != null ? h('span', null, bytes(perDay), h('span', { class: 'q-u' }, ' a day')) : q(null, 'bytes', { why: 'needs a few days' }),
          `The mean of the last ${count(growth.length, 'full day')}.`),
        figure('Full in', full != null ? h('span', { class: ['q', { 'hl-warn': full < 60 }] }, h('span', { class: 'q-v' }, formatNumber(Math.round(full))), h('span', { class: 'q-u' }, full === 1 ? 'day' : 'days')) : q(null, 'count', { why: 'not growing' }),
          full == null ? 'The data is not growing, or the free space is not known.'
            : full > 3650 ? `About ${formatNumber(Math.floor(full / 365))} years at this rate: nothing to do.`
              : full < 60 ? 'Soon: free space on the disk, or move the data to a bigger one.' : 'Free space divided by the mean growth.'),
      ], { cls: 'hl-store-figs' }),
      h('div', { class: 'hl-store-cols' },
        h('div', null, h('h3', { class: 'hl-sub' }, 'By kind'), k.el),
        h('div', null, h('h3', { class: 'hl-sub' }, 'Added per day'), growthChart(growth))));
    await k.table.load();
  });

  function growthChart(growth) {
    if (!growth.length) return empty({ title: 'No full day yet.', text: 'Growth shows after the first full day.' });
    const max = Math.max(...growth.map((g) => g.bytes), 1);
    const w = 100 / growth.length;
    const readout = h('p', { class: 'readout hl-readout' }, 'Point at a day to read it.');
    const svgEl = svg('svg', { class: 'hl-growth-svg', viewBox: '0 0 100 100', preserveAspectRatio: 'none', role: 'img',
      'aria-label': `Bytes added on each of the last ${growth.length} full days`, onpointerleave: () => replace(readout, 'Point at a day to read it.') },
    growth.map((g, i) => {
      const hgt = Math.max(0.4, (g.bytes / max) * 100);
      return svg('rect', { class: 'hl-growth-bar', x: i * w + w * 0.15, width: w * 0.7, y: 100 - hgt, height: hgt,
        onpointerenter: () => replace(readout, h('time', { class: 'readout-time' }, formatDayKey(g.date)), ' ', h('b', null, format(g.bytes, 'bytes')), ' added') },
      svg('title', null, `${formatDayKey(g.date)}: ${format(g.bytes, 'bytes')}`));
    }));
    const csvBtn = button('Download CSV', { iconName: 'download', cls: 'button-small',
      onclick: () => downloadCsv(csvFilename('sentry-mate', 'storage growth'), growth, [{ label: 'Day (UTC)', value: (g) => g.date }, column('Added (bytes)', 'bytes')]) });
    csvBtn.dataset.csv = '';
    return h('div', { class: 'hl-growth' },
      h('div', { class: 'hl-plot-head' }, readout, csvBtn),
      h('div', { class: 'hl-growth-plot' },
        h('div', { class: 'hl-growth-axis', 'aria-hidden': 'true' }, h('span', null, format(max, 'bytes')), h('span', null, format(max / 2, 'bytes')), h('span', null, '0 B')),
        h('div', { class: 'hl-growth-box' }, svgEl)),
      h('div', { class: 'hl-growth-x', 'aria-hidden': 'true' }, h('span', null, formatDayKey(growth[0].date, { weekday: false, year: false })), h('span', null, formatDayKey(growth[growth.length - 1].date, { weekday: false, year: false }))),
      note('Bytes added on each of the last full days (UTC days), every car together. Days with drives and charges add the most; asleep days add almost nothing.'));
  }

  /* ---------------------------------------------------------------- every car */

  const carSearch = h('input', { class: 'field-input', type: 'search', id: 'hl-car-search', placeholder: 'Find a car by name, model or VIN', autocomplete: 'off' });
  const DATA_BAD = new Set(['upload_silence', 'trust_failed', 'logs_waiting_trust', 'logs_missing', 'gps_inputs_disagree']);
  const carTable = dataTable(ctx, {
    path: 'cars', params: {}, limit: 50, itemName: ['car', 'cars'], sort: { key: 'name', dir: 'asc' }, cls: 'hl-cars', label: 'Every car',
    cover: 'faq.stopped-recording',
    caption: 'Paged, for any number of cars. Open one for its gaps, keys, totals, storage and signals.',
    columns: [
      { key: 'name', label: 'Car', th: true, sort: 'name', cell: (c) => [h('a', { class: 'cell-main', href: state.buildUrl('health', { car: c.id }) }, c.name || c.id), h('span', { class: 'cell-sub' }, [c.model, c.year].filter(Boolean).join(' · '))] },
      { key: 'state', label: 'Now', cell: (c) => stateMark(c.now?.state || 'unknown', { stale: !!c.now?.stale }) },
      { key: 'last', label: 'Last data', num: true, sort: 'last_activity', cell: (c) => (c.now?.received_ms != null ? ago(c.now.received_ms, { stale: !!c.now?.stale }) : h('span', { class: 'hl-faint' }, 'none yet')) },
      { key: 'data', label: 'Data', cell: (c) => {
        const bad = (c.attention || []).map((a) => (typeof a === 'string' ? a : a.code)).filter((a) => DATA_BAD.has(a));
        return bad.length ? h('span', { class: 'hl-chips' }, bad.map((a) => chip(REASONS[a].title, { kind: 'warn', iconName: 'warning' })))
          : h('span', { class: 'hl-ok hl-arriving' }, icon('check'), 'Arriving and trusted');
      } },
      { key: 'open', label: '', cell: (c) => h('a', { class: 'link-arrow', href: state.buildUrl('health', { car: c.id }) }, 'Data health', icon('arrowRight')) },
    ],
    empty: () => empty({ title: carSearch.value.trim() ? 'No car matches.' : 'No car yet.', text: carSearch.value.trim() ? `No car’s name, model or VIN holds “${carSearch.value.trim()}”.` : 'A car shows once its CAN Bridge has sent a first log. Add one on Settings: Cars.' }),
  });
  let carTimer = null;
  carSearch.addEventListener('input', () => { clearTimeout(carTimer); carTimer = setTimeout(() => ctx.alive() && carTable.reload({ search: carSearch.value.trim() || undefined }), 250); });
  ctx.onCleanup(() => clearTimeout(carTimer));
  ctx.main.append(section({ title: 'Every car', note: 'is each car’s data arriving and trusted?', cls: 'hl-block', id: 'hl-cars',
    actions: h('div', { class: 'field field-search hl-search' },
      h('label', { class: 'sr-only', for: 'hl-car-search' }, 'Find a car'),
      h('div', { class: 'field-control' }, h('span', { class: 'field-icon' }, icon('search')), carSearch)) }, carTable.el));

  /* ---------------------------------------------------------------- server health */

  const connCard = connSection(ctx);

  const srvBox = h('div', { class: 'hl-srv' }, loadingBlock(5));
  ctx.main.append(section({ title: 'Server health', note: 'the server itself, now', cls: 'hl-block', id: 'hl-srv' }, srvBox));

  const ms = (x) => (x == null ? DASH : `${formatNumber(x, { maximumFractionDigits: x < 10 ? 1 : 0 })} ms`);
  function clockNote() {
    const now = api.now();
    return h('div', { class: 'hl-clock', cover: 'faq.docker-timestamps' },
      h('h3', { class: 'hl-sub' }, 'Clock and time zones'),
      facts([
        ['Server clock', h('span', { class: 'mono' }, `${new Date(now).toISOString().replace('T', ' ').slice(0, 19)} UTC`), 'Every time is stored in UTC.'],
        ['This page', formatDateTime(now, tz, { seconds: true }), `Shown in ${zoneLabel(tz)}. Car pages show times in each car’s own time zone.`],
      ]),
      note('The container’s own log lines are written in UTC unless its TZ variable names another zone, so they can differ from the times on these pages by the zone’s offset. Nothing is wrong when they do.'));
  }
  function healthAddress() {
    return h('div', { class: 'hl-addr', cover: 'web.health-api.health-check' },
      h('h3', { class: 'hl-sub' }, 'Health address for container checks'),
      h('p', { class: 'hl-addr-line' }, h('code', { class: 'mono hl-code' }, 'GET /health'), ' → ', h('code', { class: 'mono hl-code' }, '200'), ' with an empty body while the server is serving.'),
      note('It needs no sign-in and tells nothing about the cars or the data, so a monitor or the container’s health check may call it freely. The Docker image already checks it every 30 seconds.'));
  }

  const loadServer = guarded(ctx, 'Could not read the server’s health', (v) => replace(srvBox, v, h('div', { class: 'hl-cols' }, clockNote(), healthAddress())), async () => {
    if (!admin) { replace(srvBox, adminOnly('Server health (uptime, versions, memory, timings)'), h('div', { class: 'hl-cols' }, clockNote(), healthAddress())); return; }
    let sh, geo;
    try {
      [sh, geo] = await Promise.all([ctx.api.get('system/health'), ctx.api.get('geocode/status').catch(() => null)]);
    } catch (e) {
      if (e && e.status === 403) { replace(srvBox, adminOnly('Server health (uptime, versions, memory, timings)'), h('div', { class: 'hl-cols' }, clockNote(), healthAddress())); return; }
      throw e;
    }
    if (!ctx.alive()) return;
    const v = sh.versions || {};
    const mem = (sh.ingest && sh.ingest.memory) || {};
    const memPct = mem.limit_bytes ? (mem.used_bytes / mem.limit_bytes) * 100 : null;
    const t = sh.timings_ms || {};
    const db = sh.db || {};
    replace(srvBox,
      h('div', { class: 'hl-srv-cols' },
        h('div', null, h('h3', { class: 'hl-sub' }, 'Running'), facts([
          ['Up for', sh.uptime_ms != null ? q(sh.uptime_ms, 'duration') : DASH, 'Since the server last started.'],
          ['Server version', h('span', { class: 'mono' }, v.server || DASH)],
          ['API version', h('span', { class: 'mono' }, String(v.api ?? DASH)), 'What the pages and integrations talk to.'],
          ['Storage format', h('span', { class: 'mono' }, String(v.db_format ?? DASH)), 'Raised when the way data is kept changes; older data is converted at start.'],
          ['Timeline rules', h('span', { class: 'mono' }, String(v.timeline_rules ?? DASH)), 'When they change, drives and states are rebuilt from the logs.'],
          ['Import mapping', h('span', { class: 'mono' }, String(v.import_mapping ?? DASH)), 'How imported TeslaMate rows are read.'],
          ['Last housekeeping', sh.housekeeping_last_ms != null ? ago(sh.housekeeping_last_ms) : 'not yet', 'Checks, compaction and the daily storage figure.'],
          sh.tesla ? ['Tesla API input', sh.tesla.accounts ? `${formatNumber(sh.tesla.accounts)} ${sh.tesla.accounts === 1 ? 'account' : 'accounts'}, ${formatNumber(sh.tesla.cars ?? 0)} ${sh.tesla.cars === 1 ? 'car' : 'cars'} read` : 'Off: no Tesla account',
            [sh.tesla.accounts ? `${formatNumber(sh.tesla.requests_today ?? 0)} requests to Tesla today. ` : 'No connection to Tesla is made until an account is added. ',
              sh.tesla.last_error ? `Last error: ${sh.tesla.last_error.message || sh.tesla.last_error.code}. ` : '',
              h('a', { href: `${state.buildUrl('settings-integrations')}#tesla` }, 'Tesla account')]] : null,
        ])),
        h('div', null, h('h3', { class: 'hl-sub' }, 'Memory and caches'),
          h('div', { class: 'hl-mem' },
            h('p', { class: 'hl-mem-line' }, h('span', null, 'Upload memory'), h('span', null, bytes(mem.used_bytes), ' of ', bytes(mem.limit_bytes))),
            h('span', { class: ['hl-bar', 'hl-bar-wide', { 'is-warn': memPct > 85 }], role: 'img', 'aria-label': memPct != null ? `${Math.round(memPct)} percent used` : 'not known' },
              h('span', { class: 'hl-bar-fill', vars: { '--w': `${Math.max(0.5, Math.min(100, memPct || 0))}%` } })),
            note('Logs wait here before they are written. When it is full, new uploads wait a moment; nothing is lost.')),
          facts([
            ['Car stores open', `${formatNumber(db.open_cars ?? 0)} of ${formatNumber(db.cars ?? 0)}`, 'Kept open for quick answers.'],
            ['Open stores take', bytes(db.open_bytes)],
            ['Cars kept in memory', q(sh.ingest?.cached_cars ?? 0, 'count')],
            ['Address look-ups', geo ? `${geo.mode === 'online' ? 'Online' : 'Offline'}, ${formatNumber(geo.cache_size ?? 0)} kept` : sh.geocoder ? `${sh.geocoder.mode}, ${formatNumber(sh.geocoder.cache_size ?? 0)} kept` : DASH,
              sh.geocoder?.hits_pct != null ? `${format(sh.geocoder.hits_pct, 'percent', { digits: 1 })} answered from what is kept.` : null],
          ])),
        h('div', null, h('h3', { class: 'hl-sub' }, 'How fast'), facts([
          ['Read one log', ms(t.decode_per_log), 'Decoding one uploaded log, on average.'],
          ['Rebuild a window', ms(t.rebuild_per_window), 'Working drives and charges out again for one window.'],
          ['Typical answer', ms(t.api_p50), 'Half of the API answers are faster than this.'],
          ['Slow answer', ms(t.api_p95), '95 of 100 API answers are faster than this.'],
          ['This reading took', ms(sh.read_ms)],
        ]))),
      h('div', { class: 'hl-cols' }, clockNote(), healthAddress()));
  });

  /* ---------------------------------------------------------------- API errors */

  const tryOut = h('div', { class: 'hl-try', 'aria-live': 'polite' });
  const tryBtn = button('Ask for a car that does not exist', { iconName: 'search', cls: 'button-small', cover: 'web.health-api.unknown-car',
    title: 'Ask the server for a car id nobody has, and show the answer.', onclick: () => tryUnknown() });
  async function tryUnknown() {
    tryBtn.disabled = true;
    try {
      await ctx.api.get('cars/no-such-car-0');
      if (ctx.alive()) replace(tryOut, h('p', { class: 'hl-warn' }, 'The server answered with a car. That should never happen.'));
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      replace(tryOut,
        h('pre', { class: 'mono hl-pre' }, `${e.status} ${JSON.stringify({ error: { code: e.code, message: e.message } }, null, 2)}`),
        note('A car this account may not see gives exactly the same answer, so nobody can learn which car ids exist by trying them.'));
    } finally { tryBtn.disabled = false; }
  }
  const errTable = dataTable(ctx, {
    rows: API_ERRORS.map(([status, code, text]) => ({ status, code, text })), itemName: ['error code', 'error codes'], sort: { key: 'status', dir: 'asc' },
    cls: 'hl-errors', label: 'API errors', cover: 'web.health-api.errors',
    csv: { parts: ['sentry-mate', 'api errors'], columns: [column('Status', 'status'), column('Code', 'code'), column('When', 'text')] },
    columns: [
      { key: 'status', label: 'Status', sort: true, cell: (r) => h('span', { class: 'mono' }, String(r.status)) },
      { key: 'code', label: 'Code', th: true, sort: true, cell: (r) => h('code', { class: 'mono hl-code' }, r.code) },
      { key: 'text', label: 'When', cell: (r) => h('span', { class: 'hl-wrap' }, r.text) },
    ],
  });
  ctx.main.append(section({ title: 'API errors', note: 'one shape for every failed call', cls: 'hl-block', id: 'hl-errors', actions: tryBtn },
    h('p', { class: 'hl-shape' }, 'Every ', h('code', { class: 'mono hl-code' }, '/api/v1'), ' call that fails answers ',
      h('code', { class: 'mono hl-code' }, '{ "error": { "code", "message", "fields" } }'),
      ': code for programs, message for people, and fields naming each bad input by its path (', h('code', { class: 'mono hl-code' }, 'units.distance'), ').'),
    tryOut, errTable.el,
    note('Every decoded signal of a car, with its value, unit and a chart over time, is on that car’s data health page: open a car in the list above.')));

  /* ---------------------------------------------------------------- load */

  async function reloadAll(btn) {
    if (btn) btn.disabled = true;
    try {
      const failed = await settleAll([loadStorage(), loadServer(), carTable.reload().then(() => true)]);
      aside.paint();
      if (btn && ctx.alive()) reloadedFlash(ctx, failed);
    } catch (e) {
      if (ctx.alive()) ctx.flash(`Not loaded: ${errorWords(e).text}`, { kind: 'error' });
    } finally { if (btn) btn.disabled = false; }
  }

  await settleAll([loadStorage(), loadServer(), carTable.load(), errTable.load(), connCard]);
  if (!ctx.alive()) return {};
  ctx.ready();
  return {};
}
