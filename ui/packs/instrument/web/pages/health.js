// Data health and storage: two routes.
// - /cars/{car}/health: is this car's data arriving and trusted (last upload, last log check, keys, chain
//   gaps, dropped frames, unknown time, logs waiting, rebuild), the gaps in the period, the car's totals per
//   source, what its data takes on disk (by kind and by day), and every decoded signal of its newest log
//   with a chosen series over the period.
// - /system: the server's storage (by kind, free space, growth, "full in"), the server's own health
//   (uptime, versions, memory, timings, the /health address), the one error shape of the API, and every
//   car's data status in one paged list.
// API: GET cars/{car}/health, cars/{car}/gaps, cars/{car}/totals?source=, cars/{car}/updates (summary),
// cars/{car}/storage, cars/{car}/signals/latest, cars/{car}/signals; POST cars/{car}/rebuild (admin),
// PATCH can-keys/{key} (admin: a key's name, can-key-name.js); GET, POST cars/{car}/full-rebuild and
// DELETE system/full-rebuild (admin, "Rebuild all data": pages/full-rebuild.js); GET cars/{car}/logs/unsigned, POST
// and DELETE cars/{car}/logs/use-unsigned (admin: the logs without a signature, pages/unsigned-logs.js); GET, POST
// cars/{car}/gps-layout/fit and PUT cars/{car}/gps-layout (admin, "Find GPS layout"), GET cars/{car}/gps-layout
// (the GPS layout line, everyone): shared/gpsfit.js;
// GET system/storage, system/health (admin), cars, geocode/status, integrations/overview (admin: the counts on
// the card that leads to the connections overview).
// Live: `health` events of the car change every figure of "Is data arriving?" in place (live events, polling
// only while the live stream is down); `rows` events of the car load the gaps and totals again, in place; on
// /system, `storage` events change the storage figures in place.

import * as state from '../shared/state.js';
import { srcMark, srcText } from '../core/source.js';
import { teslaCarStatus, lastErrorText, teslaAccountHref } from './tesla-account.js';
import { h, replace, cover } from '../shared/dom.js';
import { format, convert, unitLabel } from '../shared/units.js';
import { column, timeColumn, downloadCsv, csvFilename, rowsFromColumns } from '../shared/csv.js';
import { formatDateTime, formatDuration, formatDayKey, dayStart, HOUR } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, figures, pill, tag, kv, csvButton, panelLink } from '../core/page.js';
import { dataTable, windowTable } from '../core/table.js';
import { scrollBox } from '../shared/scrollbox.js';
import { emptyState, errorWords } from '../core/states.js';
import { openCarSwitcher } from '../core/carswitch.js';
import { openPeriodPicker } from '../core/period.js';
import { flash } from '../core/flash.js';
import { isAdmin } from '../core/prefs.js';
import { timeChart, barChart } from '../core/chart.js';
import { q, agoEl, statePill, stateLabel, DASH } from '../core/fmt.js';
import { overviewCard } from './conn-overview.js';
import { deviceText, renameKeyButton } from './can-key-name.js';
import { fullRebuild } from './full-rebuild.js';
import { gpsLayoutSection, hasCanData } from '../shared/gpsfit.js';
import { confirmDialog } from '../core/dialog.js';
import { follow, carOf, onRows, FALLBACK_POLL_MS } from '../shared/live.js';
import { morph, morphChildren, keepEl } from '../core/morph.js';
import { rowText as frRowText } from '../shared/fullrebuild.js';
import { unsignedLogs } from './unsigned-logs.js';

// every log of the car as the CAN Bridge app is told it now (`sentry-mate log-checks --car` prints the same)
function logChecksText(lc, fmt) {
  if (!lc) return '';
  const why = [['mismatch', lc.mismatch], ['bad signature', lc.bad_signature], ['unsigned', lc.unsigned], ['chain break', lc.chain_break]]
    .filter((x) => x[1]).map((x) => `${x[0]} ${fmt(x[1])}`).join(', ');
  return `All logs as the CAN Bridge app is told: ${fmt(lc.verified)} verified, ${fmt(lc.pending)} pending, ${fmt(lc.failed)} failed${why ? ` (${why})` : ''}.`;
}

export const id = 'health';
export const css = true;
export const title = (ctx) => (ctx.car ? 'Data health and storage' : 'Server and storage');

const enc = encodeURIComponent;
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const bytes = (v) => format(v, 'bytes');
// the period for resources that fall back to a short window without from_ms (health 30 days, signals 1 hour):
// All time sends from_ms 0, so their figures cover the same window as the gaps list
const fullPeriod = (params) => ({ ...params, from_ms: params?.from_ms ?? 0 });
// the figures of /health that cover a period: a `health` event has them for the default 30 days only
const PERIOD_KEYS = ['unknown_ms', 'unknown_spans', 'unknown_spans_total', 'chain_gaps', 'chain_gaps_total', 'dropped_frames', 'dropped_frames_total'];
// "N km are in both" when the two sources overlap and the total counts the overlap once
function overlapText(a, b, total, kind, noun) {
  const both = (a ?? 0) + (b ?? 0) - (total ?? 0);
  if (total == null || !(both > 0.5)) return '';
  const n = kind === 'count' ? `${formatNumber(Math.round(both))} ${noun}` : format(both, kind);
  return ` ${n} ${kind === 'count' && Math.round(both) === 1 ? 'is' : 'are'} in both and counted once in the total.`;
}

/* ------------------------------------------------------------------ words */

const REASONS = {
  upload_silence: {
    title: 'No log has arrived for a while',
    text: 'The CAN Bridge has not sent a log lately. Check that it has power and a network, and that its key below still uploads. Nothing is lost while the car is asleep; logs kept on the bridge come in when it is back.',
    fix: 'Check the keys', page: 'settings-cars',
  },
  trust_failed: {
    title: 'The last log failed its check',
    text: 'Its signature or hash chain did not match, so its data is kept apart and not used. Pair the bridge again if its key changed.',
    fix: 'Pair the bridge again', page: 'settings-cars',
  },
  logs_waiting_trust: {
    title: 'Logs wait for a key to be trusted',
    text: 'They arrived from a key nobody has approved yet. Approve or assign the key and they are read at once.',
    fix: 'Approve the key', page: 'settings-cars',
  },
  gps_inputs_disagree: {
    title: 'Positions from one input are out of step',
    text: 'One input’s GPS positions are far from the other inputs’ positions at the same moments, more than lag or noise explains (often a decoding problem). Its positions are not used there; the other inputs’ positions are. The rest of its data is still used.',
    fix: null, page: null,
  },
  logs_missing: {
    title: 'Logs missing on the server',
    text: 'The server lost some logs it had recorded and asked the car to send them again. They come in by themselves when the CAN Bridge has them; see Missing on the server below.',
    fix: null, page: null,
  },
  rebuild_pending: {
    title: 'Drives and charges are being rebuilt',
    text: 'They are worked out again from the stored logs. This page follows it as it goes until it is done; nothing needs doing.',
    fix: null, page: null,
  },
};
const TRUST = {
  verified: ['Verified', 'live', 'Signed by a trusted key and in the hash chain.'],
  unsigned: ['Unsigned', 'warm', 'The bridge sent it without a signature; its data is used but cannot be proven.'],
  failed: ['Failed', 'warm', 'Its signature or chain did not match; its data is not used.'],
};
const TRUST_REASONS = {
  signature_mismatch: 'the signature does not match the key',
  chain_break: 'it does not follow the log before it',
  unknown_key: 'the key is not known',
  bad_format: 'the file could not be read',
};
// why a stretch of time is unknown, by the server's word (`reason` of an unknown span, `gap_reason` of a gap). A log
// that was left out says which way: failed_check is its signature, hash or chain; unreadable is a stored log that
// could not be decoded; other_vin is a log whose frames name another car
const SPAN_REASONS = {
  no_log: 'no log arrived', not_recorded: 'recording was paused', dropped_frames: 'the bridge dropped frames',
  failed_check: 'the log failed its check', unreadable: 'the log could not be read', other_vin: 'the log names another car’s VIN',
  chain_gap: 'a log is missing from the chain', paused: 'recording was paused',
};
const GAP_CAUSES = {
  no_log: ['No log', 'Nothing arrived for this time.'],
  not_recorded: ['Not recorded', 'The car moved while the bridge was not recording.'],
  chain_gap: ['Chain gap', 'A log in the hash chain is missing.'],
  unknown: ['Unknown', 'The data here could not be used: a log failed its check, could not be read, names another car or lost frames.'],
};
// what each kind of stored data holds, in plain words
const KIND_TEXT = {
  logs: 'The CAN logs as the bridge sent them, compressed and signed. Everything else can be rebuilt from these.',
  series: 'Values over time (1 per second while driving or charging), used by charts.',
  curves: 'Power, volts and amps of every charge, second by second.',
  rows: 'One row per drive, charge, state and visit.',
  imported: 'History brought in from TeslaMate.',
  tesla_api: 'Values read from the Tesla API.',
  tiles: 'Map tiles kept on this server, so maps work offline.',
  places: 'Places, prices and settings.',
  geocode: 'Addresses already looked up.',
};

// a signal's DBC unit -> the unit kind of shared/units.js (so it follows the person's units)
const UNIT_KIND = { km: 'distance', 'km/h': 'speed', '°C': 'temperature', kPa: 'pressure', kWh: 'energy', kW: 'power', '%': 'percent', V: 'voltage', A: 'current', m: 'length' };
const signalKind = (s) => (s.series === 'odometer' ? 'odometer' : UNIT_KIND[s.unit] || null);
const COORD = new Set(['gps_latitude', 'gps_longitude']);

function signalText(s) {
  if (s.value_name) return s.value_name;
  if (s.value == null) return DASH;
  if (COORD.has(s.series)) return `${formatNumber(s.value, { minimumFractionDigits: 5, maximumFractionDigits: 5 })}°`;
  const k = signalKind(s);
  if (k) return format(s.value, k, k === 'pressure' || k === 'voltage' ? {} : undefined);
  return s.unit ? `${formatNumber(s.value, { maximumFractionDigits: 2 })} ${s.unit}` : formatNumber(s.value, { maximumFractionDigits: 2 });
}

const API_ERRORS = [
  [400, 'bad_input', 'A value has the wrong type, a field is unknown or a filter is not valid. fields names each one.'],
  [401, 'not_signed_in', 'No session, or it ended. Sign in again.'],
  [403, 'csrf', 'A change came without the X-Sentry-Mate header, so it may not be from this app.'],
  [403, 'not_allowed', 'The account may read this but not change it (a watch account), or it is for admins only.'],
  [404, 'no_such_car', 'There is no such car, or this account may not see it. Both answer the same, so a hidden car cannot be found by trying ids.'],
  [404, 'not_found', 'No such address, or a drive, charge, place or other row that is not there (each has its own no_such_… code).'],
  [405, 'method_not_allowed', 'This address does not take that method. The Allow header lists the ones it does.'],
  [409, 'stale_version', 'Someone changed it first. The answer carries the current version; load and try again.'],
  [409, 'import_running', 'Another import is not finished yet.'],
  [413, 'too_large', 'The body is over 1 MB.'],
  [422, 'invalid', 'Well formed but against a rule (an unknown time zone, an empty password). fields names each one.'],
  [429, 'throttled', 'Too many tries. Retry-After says how many seconds to wait.'],
  [500, 'server_error', 'Something broke on the server. Nothing was changed.'],
];

/* ------------------------------------------------------------------ mount */

export async function mount(ctx) {
  return ctx.car ? mountCar(ctx) : mountSystem(ctx);
}

const kvRow = (label, value, ic, opts = {}) => [label, value, { icon: ic, ...opts }];
const when = (ctx, ms) => (ms == null ? DASH : formatDateTime(ms, ctx.tz, { now: ctx.api.now() }));
const timelineAt = (car, from, to) => state.buildUrl('timeline', { car, period: { from: from - HOUR, to: to + HOUR } });

function reloadButton(onclick, coverId = null) {
  return h('button', {
    class: 'btn', type: 'button', cover: coverId, onclick,
    title: 'Read every figure on this page again now. Health, gaps and totals also change by themselves when the car’s data changes.',
  }, icon('reset', 'ico--sm'), 'Reload');
}

function kindsTable(ctx, kinds, total, { title, csvName, coverIds }) {
  const rowsOf = (ks, all) => (ks || []).map((k) => ({ ...k, share: all ? (k.bytes / all) * 100 : null }));
  const rows = rowsOf(kinds, total);
  const t = dataTable(ctx, {
    title, icon: 'server', rows, cls: 'hl-kinds', csvName, cover: coverIds,
    sort: { key: 'bytes', dir: 'desc' },
    note: 'Sizes as stored on disk: compressed, and encrypted at rest. Largest first; click a heading to sort.',
    columns: [
      {
        key: 'label', label: 'What', sort: true, cover: 'dbinfo.sizes.table', value: (r) => r.label,
        render: (r) => h('span', { class: 'cell-text' }, h('span', { class: 'main' }, r.label), h('span', { class: 'sub hl-wrap' }, KIND_TEXT[r.kind] || '')),
        csv: { label: 'What', value: (r) => r.label },
      },
      {
        key: 'bytes', label: 'On disk', num: true, sort: true, cover: 'dbinfo.sizes.data', value: (r) => r.bytes,
        render: (r) => h('span', { class: 'cell-text' }, bytes(r.bytes), h('span', { class: 'bar hl-bar', vars: { '--w': `${Math.max(1, r.share || 0)}%` } }, h('i'))),
        csv: { label: 'On disk (bytes)', value: (r) => r.bytes },
      },
      {
        key: 'share', label: 'Share', num: true, sort: true, value: (r) => r.share,
        render: (r) => format(r.share, 'percent', { digits: 1 }),
        csv: column('Share', 'share', 'percent', { digits: 2 }),
      },
    ],
    empty: () => emptyState({ icon: 'server', title: 'Nothing stored yet', text: 'Storage grows once the first log arrives.' }),
  });
  const totalVal = h('span', { class: 'hl-total-val' }, bytes(total));
  const totalEl = h('div', { class: 'hl-total', cover: 'dbinfo.sizes.total' },
    h('span', { class: 'metric-label' }, 'All kinds together'), totalVal);
  t.el.append(totalEl);
  // new sizes of the same kinds: the rows and the total, in the table already there
  let last = JSON.stringify([kinds, total]);
  t.update = (ks, all) => {
    const now = JSON.stringify([ks, all]);
    if (now === last) return;
    last = now;
    t.setRows(rowsOf(ks, all));
    if (totalVal.textContent !== bytes(all)) totalVal.textContent = bytes(all);
  };
  return t;
}

/* ================================================================== car */

async function mountCar(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const admin = isAdmin();
  const base = `cars/${enc(cid)}`;
  let readAt = ctx.api.now();

  const readAtEl = h('span', null);
  const paintReadAt = () => replace(readAtEl, `read ${formatDateTime(readAt, tz, { now: ctx.api.now(), seconds: true })}`);
  paintReadAt();

  const reloadBtn = reloadButton(() => reloadAll(true), 'dbinfo.control.refresh');
  const head = pageHead({
    eyebrow: car.name, title: 'Data health and storage', cls: 'hl-head',
    sub: [period.label, readAtEl, `times in ${tz.replace(/_/g, ' ')}`],
    purpose: 'Whether this car’s data is arriving, complete and trusted, with the fix for anything that is not; what is recorded; and how much room it takes.',
    actions: [
      h('button', { class: 'btn', type: 'button', cover: 'dbinfo.filter.car', onclick: openCarSwitcher,
        title: 'Show another car. Everything on this page is about one car; the server’s storage is on Server and storage.' },
      icon('car', 'ico--sm'), h('span', { class: 'name-cut' }, car.name), icon('chev-d', 'ico--sm')),
      h('button', { class: 'btn', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), title: 'The period of the gaps, dropped frames, unknown time and the signal chart. Totals are all-time.' },
        icon('calendar', 'ico--sm'), period.label, icon('chev-d', 'ico--sm')),
      reloadBtn,
      h('a', { class: 'btn btn--ghost', href: state.buildUrl('health') }, icon('server', 'ico--sm'), 'Server and storage'),
    ],
  });

  const root = h('div', { class: 'hl-page', cover: 'shots.database-info' });

  /* ---------- health */
  const healthSlot = h('div', { class: 'hl-health-body' });
  const healthPanel = panel({ title: 'Is data arriving?', icon: 'pulse', cls: 'hl-health', cover: 'faq.stopped-recording',
    side: panelLink(state.buildUrl('settings-cars', { car: cid }), 'Settings: Cars') }, healthSlot);

  // Rebuild all data (admin): everything worked out for this car made again from the stored data, at the
  // bottom of this card; built once and moved along when the card is painted again
  const names = new Map([[cid, car.name]]);
  const full = admin ? fullRebuild(ctx, { car, label: 'Rebuild all data', carName: (id) => names.get(id) || id, onStatus: () => paintRebuildRow(), onEnded: () => reloadAll(false) }) : null;
  // the other cars' names, for the step line of a run over every car
  if (admin) ctx.api.list('cars', { limit: 500 }).all({ max: 5000 }).then((cs) => { for (const c of cs) names.set(c.id, c.name); if (ctx.alive()) full.repaint(); }).catch(() => {});
  // the full rebuild's block, built once and kept in place under the figures painted again
  const rebuildEl = full ? h('div', { class: 'hl-rebuild hl-full', id: 'full-rebuild' }, h('h3', { class: 'hl-sub' }, 'Rebuild all data'), full.el)
    : h('p', { class: 'note hl-rebuild' }, 'An admin can rebuild all of this car’s data from the stored logs.');
  // GPS layout: the layout this car's Screenmate GPS frames are read with, always shown for a car with SMO data,
  // with Find GPS layout and Reset to standard for admins (read-only for others); built once and kept in place
  // when the card is drawn again (the fit opens under its line once used)
  const gpsLayout = gpsLayoutSection({
    api: ctx.api, car: cid, admin, alive: () => ctx.alive(), flash, when: (ms) => when(ctx, ms),
    confirm: (text, o) => confirmDialog(text, { title: o.title, ok: o.ok }),
    errorText: (e) => errorWords(e).text, onApplied: () => refreshHealth(),
    btnClass: 'btn btn--sm', applyClass: 'btn btn--sm btn--primary', resetClass: 'btn btn--sm btn--ghost', barClass: 'bar', icon: (n) => icon(n, 'ico--sm'),
  });
  ctx.onCleanup(() => gpsLayout.stop());
  let gpsRead = false;
  // the logs without a signature, with Use unsigned logs and Stop using (admins only: the answers are admin reads);
  // built once and kept in place, read again with the health figures
  const unsigned = admin ? unsignedLogs(ctx, { car, when, onChanged: () => { refreshHealth(); loadTotals().catch(() => {}); } }) : null;
  // the Rebuild row: the full rebuild's step while one runs for this car, else the period rebuild's state
  const rebuildVal = h('span');
  let lastPending = false;
  function paintRebuildRow() {
    const fs = full?.status();
    const row = fs?.this_car ? frRowText(fs, ctx.api.now(), (id) => names.get(id) || id) : null;
    if (row) {
      rebuildVal.className = row.tone;
      rebuildVal.textContent = row.text;
    } else {
      rebuildVal.className = lastPending ? 'warn' : '';
      rebuildVal.textContent = lastPending ? 'Running' : 'Nothing pending';
    }
  }

  // the figures follow the car's `health` events; its endpoint is read at the start, after a reconnect and, only
  // while the live stream is down, every few seconds (every 3 s while a rebuild runs)
  let wasPending = false;
  let teslaRows = null;
  let lastHl = null;
  let eventPeriod = null;
  let periodTimer = null;
  ctx.onCleanup(() => clearTimeout(periodTimer));
  async function readHealth() {
    const [hl, , ts] = await Promise.all([
      ctx.api.get(`${base}/health`, fullPeriod(ctx.params)),
      // the GPS layout line's first reads (so it is drawn whole), then the layout again with every read
      gpsRead ? gpsLayout.load().catch(() => {}) : gpsLayout.ready.then(() => { gpsRead = true; }),
      // the Tesla API input for this car (admins only): what the server does for it now
      admin ? ctx.api.get('integrations/tesla/status').catch(() => null) : Promise.resolve(null),
      unsigned ? unsigned.load() : null,
    ]);
    teslaRows = ts;
    return hl;
  }
  function onHealth(v, how) {
    let hl = v;
    if (how === 'event' && lastHl) {
      // an event's period figures (unknown time, chain gaps, dropped frames) cover the last 30 days: the page's own
      // read keeps them for its period, and is made again (at most every 2 s) when the event's ones changed
      const sig = JSON.stringify(PERIOD_KEYS.map((k) => v[k] ?? null));
      if (sig !== eventPeriod && !periodTimer) periodTimer = setTimeout(() => { periodTimer = null; if (ctx.alive()) healthLive.refresh(); }, 2000);
      eventPeriod = sig;
      hl = { ...v };
      for (const k of PERIOD_KEYS) hl[k] = lastHl[k];
    }
    lastHl = hl;
    paintHealth(hl);
    if (!hl.rebuild_pending && wasPending) {
      // the rebuild finished: the gaps and totals it changed are read again too
      gapsTable.reload();
      loadTotals().catch(() => {});
    }
    wasPending = !!hl.rebuild_pending;
  }
  let healthLive = null;
  const refreshHealth = () => healthLive?.refresh();

  let spanBox = null;   // the unknown spans' box (its observers end when it is drawn anew)
  let spanEl = null;
  let spanSig = null;
  let spanN = 0;
  ctx.onCleanup(() => spanBox?.destroy());
  function paintHealth(hl) {
    const trust = hl.last_log_trust || {};
    const tw = TRUST[trust.result];
    const spans = hl.unknown_spans || [];
    // the lists hold the newest 500; the totals count the whole period
    const spanCount = hl.unknown_spans_total ?? spans.length;
    const reasons = hl.reasons || [];
    const status = h('div', { class: ['hl-status', hl.healthy ? 'is-ok' : 'is-warn'] },
      h('span', { class: 'hl-status-ic' }, icon(hl.healthy ? 'check' : 'warn')),
      h('div', null,
        h('div', { class: 'hl-status-head' }, hl.healthy ? 'Data is arriving and checks out' : `${reasons.length === 1 ? 'One thing needs' : `${reasons.length} things need`} attention`),
        h('p', { class: 'explain' }, hl.healthy
          ? 'Logs arrive, pass their signature and chain checks, and nothing waits. The same test sets “healthy” in MQTT.'
          : 'Each problem is listed with what causes it and how to fix it. The same test sets “healthy” in MQTT.')));
    const fixes = reasons.length ? h('ul', { class: 'hl-fixes' }, reasons.map((code) => {
      const r = REASONS[code] || { title: code, text: '', fix: null };
      return h('li', { class: 'hl-fix' },
        h('span', { class: 'hl-fix-ic' }, icon('warn', 'ico--sm')),
        h('div', null, h('div', { class: 'hl-fix-head' }, r.title), h('p', { class: 'hl-fix-text' }, r.text),
          r.fix ? h('a', { class: 'btn btn--sm', href: state.buildUrl(r.page, { car: cid }) }, r.fix, icon('chev-r', 'ico--sm')) : null,
          code === 'gps_inputs_disagree' ? h('p', { class: 'hl-fix-text hl-fix-more' }, 'See GPS layout below: Find GPS layout works out how this car’s Screenmate GPS frames should be read.') : null));
    })) : null;

    const figs = figures([
      metric('Last upload', hl.last_upload_ms != null ? agoEl(hl.last_upload_ms, tz) : null, null, {
        why: 'Nothing has come from the CAN Bridge yet.', explain: hl.last_upload_ms != null ? `${when(ctx, hl.last_upload_ms)}: the CAN Bridge's last contact.${hl.newest_log_ms != null ? ` Newest log arrived ${when(ctx, hl.newest_log_ms)}.` : ''} A car that sleeps sends nothing; that is not a fault.` : 'Pair a CAN Bridge on Settings: Cars.',
      }),
      metric('Last log check', tw ? h('span', { class: tw[1] === 'live' ? 'ok' : 'warn' }, tw[0]) : null, null, {
        why: 'No log checked yet.',
        explain: [tw ? `${tw[2]}${trust.reason ? ` Reason: ${TRUST_REASONS[trust.reason] || trust.reason}.` : ''}${trust.log_start_ms != null ? ` Log from ${when(ctx, trust.log_start_ms)}.` : ''}` : '', logChecksText(hl.log_checks, formatNumber)].filter(Boolean).join(' ') || null,
      }),
      metric('Logs waiting', hl.logs_waiting ?? null, 'count', { explain: hl.logs_waiting ? 'Arrived but not read: their key is not trusted yet.' : 'Every log that arrived has been read.' }),
      metric('Unknown time', hl.unknown_ms != null ? formatDuration(hl.unknown_ms) : null, null, {
        why: 'Not known.', explain: `In the period: time with no usable data (not asleep, which is known). ${formatNumber(spanCount)} ${spanCount === 1 ? 'span' : 'spans'}.`,
      }),
      metric('Chain gaps', hl.chain_gaps_total ?? (hl.chain_gaps || []).length, 'count', { explain: 'Logs missing from the hash chain in the period.' }),
      metric('Dropped frames', hl.dropped_frames_total ?? (hl.dropped_frames || []).length, 'count', { explain: 'Times in the period where the bridge could not keep up and lost CAN frames.' }),
    ], 'hl-figs');

    const flags = kv([
      kvRow('Uploads', hl.uploads_enabled ? h('span', { class: 'ok' }, 'Accepted') : h('span', { class: 'warn' }, 'Turned off for this car'), 'upload'),
      kvRow('Rebuild', keepEl(rebuildVal, 'rebuild-row'), 'reset'),
    ]);

    // keys of this car
    const keys = hl.keys || [];
    const keyList = h('div', { class: 'hl-keys' },
      h('h3', { class: 'hl-sub' }, 'CAN Bridge keys'),
      keys.length ? h('ul', { class: 'hl-key-list' }, keys.map((k) => h('li', { class: 'hl-key' },
        icon('lock', 'ico--sm'),
        h('span', { class: 'cell-text' }, h('span', { class: 'main' }, deviceText(k), k.preferred ? tag('Preferred', 'live') : null,
          renameKeyButton(ctx, k, admin, refreshHealth)),
          h('span', { class: 'sub mono' }, k.key)),
        h('span', { class: 'cell-text hl-key-r' }, k.last_upload_ms != null ? agoEl(k.last_upload_ms, tz) : h('span', { class: 'faint' }, 'never uploaded'),
          h('span', { class: ['sub', k.logs_waiting ? 'warn' : null] }, k.logs_waiting ? `${formatNumber(k.logs_waiting)} waiting` : 'none waiting')))))
        : h('p', { class: 'note' }, 'No key is tied to this car yet. Pair a CAN Bridge on Settings: Cars.'));

    // unknown spans: the list is made again only when they changed (its scroll box keeps its place otherwise)
    const sig = JSON.stringify(spans);
    if (sig !== spanSig || !spanEl) {
      spanBox?.destroy(); spanBox = null;
      spanSig = sig;
      spanN += 1;
      spanEl = h('div', { class: 'hl-spans' },
        h('h3', { class: 'hl-sub' }, 'Unknown time in the period'),
        spans.length ? (spanBox = scrollBox({ cls: 'hl-span-box scroll-box--fit', label: 'Unknown time in the period' }, h('ul', { class: 'hl-span-list' }, spans.map((s) => h('li', null,
          h('a', { class: 'link', href: timelineAt(cid, s.start_ms, s.end_ms ?? ctx.api.now()), title: 'Open the timeline at this time' }, when(ctx, s.start_ms)),
          h('span', { class: 'faint' }, s.end_ms == null ? ' · still unknown' : ` · ${formatDuration(s.end_ms - s.start_ms)}`),
          h('span', null, ` · ${SPAN_REASONS[s.reason] || s.reason}`)))))).el
          : h('p', { class: 'note' }, 'None: every moment of the period is known (driving, charging, parked or asleep).'));
    }
    const spanList = keepEl(spanEl, `spans-${spanN}`);

    lastPending = !!hl.rebuild_pending;
    paintRebuildRow();

    // only the texts that changed are written; the full rebuild's panel stays as it is
    morphChildren(healthSlot, [status, fixes, figs, h('div', { class: 'hl-health-cols' }, h('div', null, keyList, flags, teslaBlock()), h('div', null, spanList, missingBlock(hl.missing_logs || {}), unsigned ? keepEl(unsigned.el, 'unsigned') : null)), hasCanData(car, hl) ? keepEl(gpsLayout.el, 'gps-layout') : null, keepEl(rebuildEl, 'rebuild')]);
  }

  // logs the server recorded but no longer has: how many, since when, whether the car was asked again, and the
  // ones the car no longer has either (lost for good: unknown time on the timeline). Always shown.
  function missingBlock(m) {
    const n = m.count || 0, gone = m.gone || 0;
    const checked = m.scanned_ms != null ? `Last checked ${when(ctx, m.scanned_ms)}. The server looks at start-up and once a day.` : 'Not checked yet. The server looks at start-up and once a day.';
    const head = h('h3', { class: 'hl-sub' }, 'Missing on the server');
    if (!n) {
      return h('div', { class: 'hl-missing' },
        head, kv([kvRow('Missing logs', 'None', 'logs')]),
        h('p', { class: 'note hl-missing-line' }, icon('check', 'ico--sm'), h('span', null, 'Every log the server recorded is here.')),
        h('p', { class: 'note' }, checked));
    }
    return h('div', { class: 'hl-missing' },
      head,
      kv([
        kvRow('Missing logs', h('span', { class: 'warn' }, `${formatNumber(n)} ${n === 1 ? 'log' : 'logs'}`), 'logs', { title: 'CAN logs (and the files that go with them) the server recorded but no longer has.' }),
        kvRow('Oldest', when(ctx, m.oldest_ms), 'clock'),
        kvRow('Found missing', when(ctx, m.found_ms), 'search'),
      ]),
      m.asked
        ? h('p', { class: 'note hl-missing-line' }, icon('upload', 'ico--sm'), h('span', null, `Asked the car to send them again${m.asked_ms != null ? ` (${when(ctx, m.asked_ms)})` : ''}. They come in by themselves.`))
        : h('p', { class: 'note hl-missing-line' }, icon('clock', 'ico--sm'), h('span', null, 'The car is asked to send them again the next time it connects.')),
      gone ? h('p', { class: 'note hl-missing-line warn' }, icon('warn', 'ico--sm'),
        h('span', null, `The car no longer has ${formatNumber(gone)} of these either: lost for good, shown as unknown time. Oldest ${when(ctx, m.gone_oldest_ms)}.`)) : null,
      h('p', { class: 'note' }, checked));
  }

  // the Tesla API rows of this car: shown to admins (the status is admin only); others get a plain line
  function teslaBlock() {
    const inputs = car.sources || [];
    const hasApi = inputs.some((x) => x.kind === 'tesla_api');
    const head = h('h3', { class: 'hl-sub' }, 'Tesla API');
    if (!admin) return hasApi ? h('div', { class: 'hl-tesla' }, head, h('p', { class: 'note' }, `${car.name} also has data from the Tesla API. An admin sees what the server asks Tesla for it.`)) : null;
    const item = (teslaRows?.items || []).find((x) => x.car === cid);
    const rows = teslaCarStatus(ctx, teslaRows?.items, cid);
    if (!rows) return h('div', { class: 'hl-tesla' }, head, h('p', { class: 'note' }, hasApi ? 'No Tesla vehicle reads this car now; its older Tesla API data stays.' : 'Not read from the Tesla API. ', h('a', { class: 'link', href: `${state.buildUrl('settings-integrations')}#tesla` }, 'Tesla account')));
    return h('div', { class: 'hl-tesla' }, head, kv(rows),
      item?.last_error ? h('p', { class: 'note warn' }, lastErrorText(item.last_error)) : null,
      h('a', { class: 'link', href: teslaAccountHref(item.account) }, 'Open its Tesla account', icon('chev-r', 'ico--sm')));
  }

  /* ---------- gaps */
  const gapsTable = dataTable(ctx, {
    title: 'Gaps in the period', icon: 'timeline', path: `${base}/gaps`, params: ctx.params, cls: 'hl-gaps', box: 'Gaps', autoMore: true, csvName: `gaps ${period.preset || ''}`, csv: { all: true },
    reuseRows: true, // reloads on every `rows` event (each second while a rebuild runs): unchanged rows stay as they are
    note: 'Times with no usable data, newest first. A gap in which the odometer moved means a drive was not recorded. Open one on the timeline.',
    columns: [
      { key: 'start', label: 'From', render: (g) => h('a', { class: 'main', href: timelineAt(cid, g.start_ms, g.end_ms ?? ctx.api.now()), title: 'Open the timeline at this gap' }, when(ctx, g.start_ms)), csv: { label: 'From (UTC)', value: (g) => iso(g.start_ms) } },
      { key: 'dur', label: 'Lasted', num: true, render: (g) => (g.end_ms == null ? pill('Still open', { tone: 'warm' }) : formatDuration(g.end_ms - g.start_ms)), csv: { label: 'Lasted (minutes)', value: (g) => (g.end_ms == null ? '' : Math.round((g.end_ms - g.start_ms) / 600) / 100) } },
      { key: 'cause', label: 'Cause', render: (g) => { const c = GAP_CAUSES[g.cause] || [g.cause, '']; return h('span', { class: 'cell-text', title: c[1] }, h('span', null, c[0]), g.gap_reason ? h('span', { class: 'sub' }, SPAN_REASONS[g.gap_reason] || g.gap_reason) : null, c[1] ? h('span', { class: 'sub hl-phone-only' }, c[1]) : null); }, csv: { label: 'Cause', value: (g) => [(GAP_CAUSES[g.cause] || [g.cause])[0], g.gap_reason ? SPAN_REASONS[g.gap_reason] || g.gap_reason : null].filter(Boolean).join(': ') } },
      { key: 'jump', label: 'Odometer moved', num: true, render: (g) => q(g.odometer_jump_km, 'distance', { why: 'not known' }), csv: column('Odometer moved', 'odometer_jump_km', 'distance') },
      { key: 'where', label: 'Where', render: (g) => h('span', { class: 'cell-text' }, g.from_place?.name || 'Unknown place', g.to_place && g.to_place.id !== g.from_place?.id ? h('span', { class: 'sub' }, `to ${g.to_place.name}`) : null), csv: { label: 'Where', value: (g) => g.from_place?.name || '' } },
      { key: 'source', label: 'Data from', render: (g) => srcMark(g, { short: true }) || DASH, csv: { label: 'Data from', value: (g) => srcText(g) } },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No gaps in this period', text: 'Every moment is accounted for. Choose a longer period to look further back.' }),
  });

  /* ---------- totals */
  const totalsSlot = h('div');
  const countsSlot = h('div');
  const totalsPanel = panel({ title: 'What is recorded', icon: 'stats', cls: 'hl-totals', side: h('span', { class: 'faint' }, 'All time, not the period') }, totalsSlot);

  let totalsKey = null;
  async function loadTotals() {
    const [all, can, api, tm, upd] = await Promise.all([
      ctx.api.get(`${base}/totals`, { source: 'all' }),
      ctx.api.get(`${base}/totals`, { source: 'smo' }),
      ctx.api.get(`${base}/totals`, { source: 'tesla_api' }),
      ctx.api.get(`${base}/totals`, { source: 'import' }),
      ctx.api.get(`${base}/updates`, { limit: 1 }).catch(() => null),
    ]);
    if (!ctx.alive()) return;
    // read again on every `rows` event: drawn again only when a figure changed
    const key = JSON.stringify([all, can, api, tm, upd?.summary || null]);
    if (key === totalsKey) return;
    totalsKey = key;
    paintTotals(all, can, api, tm, upd?.summary || null);
  }

  // by input: a row SMO and the Tesla API both saw counts under each, so the parts can add up to more than the whole
  // imported parts only for a car that has imported history at all
  let hasImport = false;
  function split(a, api, b, kind, total = null, noun = '') {
    const parts = [`${format(a ?? 0, kind)} with SMO data`, api ? `${format(api, kind)} with Tesla API data` : null, hasImport ? `${format(b ?? 0, kind)} imported` : null].filter(Boolean);
    return `${parts.join(', ')}.${api ? ' A row both live inputs saw counts under each.' : hasImport ? overlapText(a, b, total, kind, noun) : ''}`;
  }

  let counts = null;
  let countsImport = null;
  function paintTotals(all, can, apiT, tm, sm) {
    hasImport = ((tm?.drives || 0) + (tm?.charges || 0) + (tm?.updates || 0)) > 0;
    const vSrc = { teslamate: 'imported from TeslaMate', teslafi: 'imported from TeslaFi', teslamate_local: 'read live from local TeslaMate', teslafi_api: 'read live from TeslaFi',
      tesla_api: 'read from the Tesla API', owner: 'typed by the owner', build_match: 'same module builds as a named update' };
    morphChildren(totalsSlot, figures([
      metric('Logged distance', all.distance_km, 'distance', { size: 'lg', cover: 'dbinfo.stat.logged_distance', explain: `The sum of every recorded drive. ${split(can.distance_km, apiT.distance_km, tm.distance_km, 'distance', all.distance_km)}` }),
      metric('Odometer', all.odometer_last_km, 'odometer', { size: 'lg', cover: 'dbinfo.stat.odometer', why: 'The car has not sent it yet.',
        explain: all.odometer_last_ms != null ? `Last read ${when(ctx, all.odometer_last_ms)}. The car sends it while driving.` : null }),
      metric('Not logged', all.not_logged_km, 'distance', { why: 'Needs two odometer readings.',
        explain: 'Odometer change since the first reading minus the logged distance: driving nobody recorded.' }),
      metric('Drives', all.drives, 'count', { cover: 'dbinfo.stat.drive_count', explain: split(can.drives, apiT.drives, tm.drives, 'count', all.drives, 'drives') }),
      metric('Charges', all.charges, 'count', { cover: 'dbinfo.stat.charge_count', explain: split(can.charges, apiT.charges, tm.charges, 'count', all.charges, 'charges') }),
      metric('Software updates', all.updates, 'count', { cover: 'dbinfo.stat.update_count', explain: `${split(can.updates, apiT.updates, tm.updates, 'count', all.updates, 'updates')} The car’s own updates are seen by its update status and changed module builds.` }),
      metric('Software version', sm?.current_version ? h('a', { class: 'link', href: state.buildUrl('software', { car: cid }) }, sm.current_version) : null, null, {
        cover: 'dbinfo.stat.current_version',
        why: 'Not known yet.',
        explain: sm?.current_version
          ? `${vSrc[sm.current_version_source] ? `${vSrc[sm.current_version_source][0].toUpperCase()}${vSrc[sm.current_version_source].slice(1)}` : 'Known'}${sm.current_version_since_ms != null ? `, since ${when(ctx, sm.current_version_since_ms)}` : ''}.${sm.build_changed_since != null ? ' The software changed after that; the new version has no name yet.' : ''}`
          : 'The car does not send its version text. It is known once a live input (local TeslaMate, TeslaFi, the Tesla API) reports it, once imported, typed on the Software page, or matched by module builds. Never guessed.',
      }),
    ], 'hl-tot-figs'));

    const rows = [
      { kind: 'Drives', what: 'One row per drive', can: can.drives, api: apiT.drives, teslamate: tm.drives, total: all.drives },
      { kind: 'Charges', what: 'One row per charge', can: can.charges, api: apiT.charges, teslamate: tm.charges, total: all.charges },
      { kind: 'Software updates', what: 'One row per update', can: can.updates, api: apiT.updates, teslamate: tm.updates, total: all.updates },
    ];
    // the same columns as before: only the counts change, in the table already there
    if (counts && countsImport === hasImport) { counts.setRows(rows); return; }
    countsImport = hasImport;
    counts = dataTable(ctx, {
      title: 'Rows kept', icon: 'book', rows, cls: 'hl-counts', csvName: 'rows kept', cover: 'dbinfo.table.row_counts',
      sort: { key: 'total', dir: 'desc' },
      note: `Exact counts, kept up to date as rows change (nothing is counted again on load). A row SMO and the Tesla API both saw counts in both columns${hasImport ? '; where imported and live history overlap, both are counted, and Shown is what the other pages list (one side)' : ''}.`,
      columns: [
        { key: 'kind', label: 'Kind', sort: true, cover: 'dbinfo.rows.table_name', value: (r) => r.kind, render: (r) => h('span', { class: 'cell-text' }, h('span', { class: 'main' }, r.kind), h('span', { class: 'sub' }, r.what)), csv: { label: 'Kind', value: (r) => r.kind } },
        { key: 'can', label: 'SMO', num: true, sort: true, render: (r) => formatNumber(r.can ?? 0) },
        { key: 'api', label: 'Tesla API', num: true, sort: true, render: (r) => formatNumber(r.api ?? 0) },
        hasImport ? { key: 'teslamate', label: 'Imported', num: true, sort: true, render: (r) => formatNumber(r.teslamate ?? 0) } : null,
        { key: 'total', label: 'Shown', num: true, sort: true, cover: 'dbinfo.rows.row_count', render: (r) => h('b', null, formatNumber(r.total)) },
      ].filter(Boolean),
    });
    replace(countsSlot, counts.el);
  }

  /* ---------- storage of this car */
  const storageSlot = h('div', { class: 'hl-storage' });
  async function loadStorage() {
    const st = await ctx.api.get(`${base}/storage`, { limit: 1 });
    if (!ctx.alive()) return;
    const kt = kindsTable(ctx, st.kinds, st.total_bytes, { title: `What ${car.name}’s data takes`, csvName: 'storage by kind', coverIds: ['dbinfo.table.sizes'] });
    const labels = Object.fromEntries((st.kinds || []).map((k) => [k.kind, k.label]));
    const days = dataTable(ctx, {
      title: 'By day', icon: 'calendar', path: `${base}/storage`, params: {}, limit: 40, cls: 'hl-days', box: 'Storage by day', autoMore: true, csvName: 'storage by day', csv: { all: true },
      note: 'Newest first, only days with data. The days are the car’s own days.',
      columns: [
        { key: 'date', label: 'Day', render: (r) => h('a', { class: 'main', href: state.buildUrl('timeline', { car: cid, period: { from: dayStart(r.date, tz), to: dayStart(r.date, tz) + 24 * HOUR } }), title: 'Open this day on the timeline' }, formatDayKey(r.date)), csv: { label: 'Day', value: (r) => r.date } },
        { key: 'series', label: 'What', render: (r) => labels[r.series] || r.series, csv: { label: 'What', value: (r) => labels[r.series] || r.series } },
        { key: 'source', label: 'Data from', render: (r) => srcMark(r, { short: true }) || DASH, csv: { label: 'Data from', value: (r) => srcText(r) } },
        { key: 'bytes', label: 'On disk', num: true, render: (r) => bytes(r.bytes), csv: { label: 'On disk (bytes)', value: (r) => r.bytes } },
      ],
      empty: () => emptyState({ icon: 'calendar', title: 'No stored days yet', text: 'A day shows here once a log for it has arrived.' }),
    });
    replace(storageSlot, kt.el, days.el);
    // the days list is part of the first render (the page does not grow under the reader after ready)
    await days.ready;
  }

  /* ---------- signals */
  const sigSearch = h('input', { class: 'input', type: 'search', placeholder: 'Find a signal, message or value', 'aria-label': 'Find a signal' });
  const sigInfo = h('span', { class: 'faint hl-sig-info' });
  let signals = [];
  let chosen = null;
  // the signals: a box five rows high that scrolls inside itself under a header that stays (the owner's: a long list
  // need not take a big part of the page), with only the rows in view drawn (shared/vlist.js). The rows all have one
  // height, and pressing Plot marks its button in place: nothing in the box is drawn again or moves.
  let shown = [];
  const sigCols = [
    { key: 'name', label: 'Signal', sort: true, cover: 'nr.dash.state-table.parameter', value: (s) => s.comment || s.series,
      render: (s) => h('span', { class: 'cell-text' }, h('span', { class: 'main' }, s.comment || s.series), h('span', { class: 'sub mono' }, s.series)),
      csv: { label: 'Signal', value: (s) => s.series } },
    { key: 'message', label: 'Message', sort: true, value: (s) => `${s.bus} ${s.message_id}`,
      render: (s) => h('span', { class: 'cell-text' }, h('span', { class: 'mono' }, s.message_id), h('span', { class: 'sub' }, s.bus === 'chassis' ? 'Chassis bus' : s.bus === 'vehicle' ? 'Vehicle bus' : s.bus)),
      csv: { label: 'Message', value: (s) => `${s.bus} ${s.message_id}` } },
    { key: 'value', label: 'Value', num: true, sort: true, cover: 'nr.dash.state-table.value', firstDir: 'desc',
      value: (s) => (s.value_name ? s.value_name : s.value == null ? null : (signalKind(s) ? convert(s.value, signalKind(s)) : s.value)),
      render: (s) => h('span', { class: 'cell-text' }, h('span', { class: 'hl-sig-val' }, signalText(s)), s.value_name ? h('span', { class: 'sub' }, `code ${s.value}`) : null),
      csv: { label: 'Value (shown)', value: (s) => signalText(s) } },
    { key: 'raw', label: 'As sent', num: true, title: 'The value in the unit the car sends', render: (s) => h('span', { class: 'faint' }, s.value == null ? DASH : `${formatNumber(s.value, { maximumFractionDigits: 6 })}${s.unit ? ` ${s.unit}` : ''}`),
      csv: { label: 'Value (as sent)', value: (s) => s.value } },
    { key: 'unit', label: 'Unit', csv: { label: 'Unit (as sent)', value: (s) => s.unit || '' }, render: (s) => h('span', { class: 'faint' }, s.unit || 'named values') },
    { key: 'at', label: 'Read', num: true, sort: true, value: (s) => s.at_ms, render: (s) => (s.at_ms != null ? agoEl(s.at_ms, tz) : DASH), csv: { label: 'Read (UTC)', value: (s) => iso(s.at_ms) } },
    { key: 'plot', label: '', csv: false,
      render: (s) => (s.has_series ? h('button', { class: ['btn', 'btn--sm', chosen === s.series ? 'btn--primary' : 'btn--ghost'], type: 'button', dataset: { plot: s.series }, onclick: () => plot(s), 'aria-pressed': chosen === s.series ? 'true' : 'false', title: 'Show this signal over the period' }, icon('stats', 'ico--sm'), 'Plot')
        : h('span', { class: 'faint', title: 'A signal with named values has its newest value only.' }, 'latest only')) },
  ];
  const sortedSignals = (sort) => {
    const col = sort && sigCols.find((c) => c.key === sort.key);
    if (!col) return shown;
    const val = col.value || ((r) => r[col.key]);
    const k = sort.dir === 'asc' ? 1 : -1;
    return [...shown].sort((a, b) => {
      const x = val(a), y = val(b);
      if (x == null && y == null) return 0;
      if (x == null) return 1;
      if (y == null) return -1;
      return (typeof x === 'string' || typeof y === 'string' ? String(x).localeCompare(String(y), undefined, { numeric: true }) : x - y) * k;
    });
  };
  let sigOrder = { key: 'name', dir: 'asc' };
  const sigCsv = csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'signals'), sortedSignals(sigOrder), sigCols.filter((c) => c.csv).map((c) => c.csv)) });
  const sigTable = windowTable(ctx, {
    title: 'Signals from the newest log', icon: 'signals', cls: 'hl-signals', rows: 5, boxLabel: 'Signals', cover: ['nr.dash.state-table', 'nr.dash.state-table.sort-scroll'],
    side: [h('label', { class: 'field field--search' }, icon('search', 'ico--sm'), sigSearch), sigCsv],
    sort: { key: 'name', dir: 'asc' },
    note: 'Every signal decoded from the newest log, with its name, unit and value names. Sort by any heading (numbers sort as numbers); the list scrolls inside. Plot a signal to see it over the period.',
    columns: sigCols,
    load: ({ sort, offset = 0, limit = 50 }) => {
      sigOrder = sort || sigOrder;
      const rows = sortedSignals(sort);
      return { items: rows.slice(offset, offset + limit), offset, total: rows.length, next: null };
    },
    empty: () => emptyState({ icon: 'search', title: signals.length ? 'No signal matches' : 'No signals yet',
      text: signals.length ? `Nothing holds “${sigSearch.value}”. Clear the search to see all ${signals.length}.` : 'Signals show once the first log of this car has been decoded.' }),
  });

  function filterSignals() {
    const words = sigSearch.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    shown = !words.length ? signals : signals.filter((s) => {
      const hay = [s.series, s.comment, s.message_id, s.bus, s.unit, s.value_name, signalText(s)].filter(Boolean).join(' ').toLowerCase();
      return words.every((w) => hay.includes(w));
    });
    replace(sigInfo, words.length ? `${shown.length} of ${signals.length}` : `${signals.length} signals`);
    return sigTable.setParams({});
  }
  // the chosen signal's Plot button, marked in place (the rows are not drawn again)
  function markChosen() {
    for (const b of sigTable.el.querySelectorAll('button[data-plot]')) {
      const on = b.dataset.plot === chosen;
      b.classList.toggle('btn--primary', on);
      b.classList.toggle('btn--ghost', !on);
      b.setAttribute('aria-pressed', on ? 'true' : 'false');
    }
  }
  let sigTimer = null;
  ctx.on(sigSearch, 'input', () => { clearTimeout(sigTimer); sigTimer = setTimeout(() => ctx.alive() && filterSignals(), 150); });
  ctx.onCleanup(() => clearTimeout(sigTimer));

  async function loadSignals() {
    signals = await ctx.api.list(`${base}/signals/latest`, {}, { limit: 500 }).all({ max: 20000 });
    if (!ctx.alive()) return;
    await filterSignals();
  }

  // a chosen signal over the period
  const plotSlot = h('div', { class: 'hl-plot' },
    emptyState({ icon: 'stats', title: 'Choose a signal to plot', text: 'Press Plot on a signal with values over time. It is drawn over the period, in your units, with gaps where nothing is known.' }));
  const plotPanel = panel({ title: 'A signal over the period', icon: 'stats', cls: 'hl-plot-panel', side: sigInfo }, plotSlot);

  // the signal over the period. All time has no start: a coarse read from the beginning finds the first
  // recorded value, then the series is read from there (the API's own default would be the last hour only)
  async function loadSignal(series) {
    const get = (params, max) => ctx.api.get(`${base}/signals`, { series, ...params, max_points: max });
    if (ctx.params?.from_ms != null) return get(ctx.params, 1500);
    const coarse = await get(fullPeriod(ctx.params), 400);
    const first = (coarse.values?.[series] || []).findIndex((v) => v != null);
    if (first < 0) return coarse;
    return get({ ...ctx.params, from_ms: coarse.t_ms[first] }, 1500);
  }

  async function plot(s) {
    chosen = s.series;
    markChosen();
    const kind = signalKind(s);
    const unitText = kind ? unitLabel(kind) : s.unit || '';
    replace(plotSlot, h('p', { class: 'note' }, `Loading ${s.comment || s.series}…`));
    let data;
    try {
      data = await loadSignal(s.series);
    } catch (e) {
      if (!ctx.alive() || chosen !== s.series) return;
      const w = errorWords(e);
      replace(plotSlot, emptyState({ icon: 'warn', warn: true, title: w.title, text: w.text }));
      return;
    }
    if (!ctx.alive() || chosen !== s.series) return;
    const values = data.values?.[s.series] || [];
    const known = values.filter((v) => v != null).length;
    const rows = rowsFromColumns(data);
    const csv = csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'signal', s.series, period.preset), rows,
      [timeColumn('Time', 't_ms', tz), kind ? column(s.comment || s.series, s.series, kind) : { label: `${s.comment || s.series}${s.unit ? ` (${s.unit})` : ''}`, value: (r) => r[s.series] }]) });
    const readout = h('span', { class: 'val' }, 'Point at the chart to read a value.');
    const box = h('div', { class: 'chart hl-chart' });
    replace(plotSlot,
      h('div', { class: 'hl-plot-bar' },
        h('div', { class: 'chart-label' }, h('span', { class: 'name' }, s.comment || s.series), h('span', { class: 'chart-unit' }, unitText), readout),
        csv),
      known ? box : emptyState({ icon: 'moon', title: 'Nothing recorded in this period', text: 'The car was asleep or not recording. Choose a longer period.' }),
      h('p', { class: 'note' }, `${ctx.params?.from_ms == null && data.t_ms?.length ? `All time: from the first recorded value, ${when(ctx, data.t_ms[0])}. ` : ''}Each point is the mean of ${formatDuration(data.bucket_ms || 0)}; the line breaks where nothing is known.`));
    if (!known) return;
    await timeChart(box, ctx, {
      tz, t_ms: data.t_ms, digits: kind === 'pressure' ? 2 : 1,
      series: [{ label: s.comment || s.series, values, kind, color: '--live', area: true }],
      label: `${s.comment || s.series} over the period`,
      onCursor: (i) => {
        if (i == null) { replace(readout, 'Point at the chart to read a value.'); return; }
        const v = values[i];
        replace(readout, h('b', null, v == null ? 'not known' : kind ? format(v, kind) : `${formatNumber(v, { maximumFractionDigits: 3 })} ${s.unit || ''}`), ` · ${when(ctx, data.t_ms[i])}`);
      },
    });
  }

  /* ---------- page */
  replace(ctx.view, root);
  const connCard = overviewCard(ctx, admin);
  root.append(head, connCard.el, healthPanel, gapsTable.el,
    h('div', { class: 'hl-two' }, totalsPanel, countsSlot),
    storageSlot, sigTable.el, plotPanel);

  async function reloadAll(say) {
    reloadBtn.disabled = true;
    try {
      await Promise.all([refreshHealth(), gapsTable.reload(), loadTotals(), loadStorage(), loadSignals()]);
      readAt = ctx.api.now(); paintReadAt();
      if (say && ctx.alive()) flash('Read again.', { kind: 'info' });
    } catch (e) {
      if (ctx.alive()) flash(errorWords(e).text, { kind: 'error' });
    } finally { reloadBtn.disabled = false; }
  }

  // the car's rows changed (a burst of them, once it has passed): its gaps and totals, read again in place
  onRows(ctx.api, { car: cid, waitMs: 1000, run: () => {
    if (!ctx.alive()) return;
    gapsTable.reload();
    loadTotals().catch(() => {});
  } });

  let firstHealth = null;
  const healthReady = new Promise((res, rej) => { firstHealth = { res, rej }; });
  healthLive = follow(ctx.api, {
    type: 'health', match: carOf(cid), pick: (d) => d.health, load: readHealth,
    onValue: (v, how) => { if (!ctx.alive()) return; onHealth(v, how); firstHealth.res(); },
    onError: (e) => firstHealth.rej(e),
    pollMs: (hl) => (hl?.rebuild_pending ? 3000 : FALLBACK_POLL_MS),
  });

  try {
    await Promise.all([healthReady, loadTotals(), loadStorage(), loadSignals(), gapsTable.ready, connCard.load(), full?.ready]);
  } catch (e) {
    if (ctx.alive()) ctx.fail(e);
    return;
  }
  if (!ctx.alive()) return;
  ctx.ready();
}

/* ================================================================== system */

async function mountSystem(ctx) {
  const admin = isAdmin();
  const tz = ctx.tz;
  let readAt = ctx.api.now();
  const readAtEl = h('span');
  const paintReadAt = () => replace(readAtEl, `read ${formatDateTime(readAt, tz, { now: ctx.api.now(), seconds: true })}`);
  paintReadAt();

  const reloadBtn = reloadButton(() => reloadAll(true), 'dbinfo.control.refresh');
  const head = pageHead({
    eyebrow: 'Sentry Mate', title: 'Server and storage', cls: 'hl-head',
    sub: ['Whole server', readAtEl],
    purpose: 'How much room the data takes and how fast it grows, how the server itself is doing, and every car’s data status in one list. Each car’s own page shows its gaps, totals and signals.',
    actions: [reloadBtn],
  });
  const root = h('div', { class: 'hl-page hl-system' });

  const forbidden = (what, holds, coverId = null) => {
    const el = emptyState({ icon: 'lock', title: 'For admins only', text: `${what} (${holds}) is shown to admins. Your account sees the cars below.` });
    return coverId ? cover(el, coverId) : el;
  };
  const storageLocked = () => forbidden('Server storage', 'total size, free space, growth', 'dbinfo.stat.total_size');
  const serverLocked = () => forbidden('Server health', 'uptime, versions, memory and timings');

  /* ---------- storage */
  const storeSlot = h('div', { class: 'hl-sys-store' });
  const storePanel = panel({ title: 'Storage', icon: 'server', cls: 'hl-store', cover: ['dbinfo.section.database'] }, storeSlot);
  // the figures follow `storage` events, changed in place; the table by kind and the growth chart are drawn
  // again only when their rows changed (live events, the storage read again every 30 s only while the stream is down)
  let storeSig = null;
  let figsEl = null;
  let storeKinds = null;
  let storeLive = null;
  function loadStorage() {
    if (!admin) { replace(storeSlot, storageLocked()); return Promise.resolve(); }
    if (storeLive) return storeLive.refresh();
    return new Promise((res, rej) => {
      let first = true;
      const done = (f) => { if (first) { first = false; f(); } };
      storeLive = follow(ctx.api, {
        type: 'storage', load: () => ctx.api.get('system/storage'), pollMs: 30000,
        onValue: (st) => { if (!ctx.alive() || !st) return; paintStore(st).then(() => done(res), (e) => done(() => rej(e))); },
        onError: (e) => {
          if (e.status === 403) { replace(storeSlot, storageLocked()); done(res); return; }
          done(() => rej(e));
        },
      });
    });
  }
  async function paintStore(st) {
    const growth = st.growth || [];
    const perDay = st.growth_per_day_bytes ?? (growth.length ? growth.reduce((a, g) => a + g.bytes, 0) / growth.length : null);
    const used = st.total_bytes;
    const disk = used != null && st.free_bytes != null ? used + st.free_bytes : null;
    const figs = figures([
      metric('Data takes', bytes(used), null, { size: 'lg', cover: 'dbinfo.stat.total_size', explain: 'Every car’s data, imported history and offline maps together, as stored on disk.' }),
      metric('Free on the disk', st.free_bytes != null ? bytes(st.free_bytes) : null, null, { size: 'lg', why: 'Not known.',
        explain: disk ? `${format((used / disk) * 100, 'percent', { digits: 1 })} of ${bytes(disk)} is Sentry Mate’s data.` : null }),
      metric('Grows by', perDay != null ? `${bytes(perDay)} a day` : null, null, { size: 'lg', why: 'Needs a few days of history.', explain: perDay != null ? 'The mean of the last full days (at most 30), those without new data included.' : null }),
      metric('Full in', st.full_in_days != null ? `${formatNumber(st.full_in_days)} days` : null, null, { size: 'lg', why: 'Not growing, or not known.',
        explain: st.full_in_days != null ? (st.full_in_days > 3650 ? 'More than ten years at this rate: nothing to do.' : st.full_in_days < 60 ? 'Soon. Free space on the disk or move the data to a bigger one.' : 'Free space divided by the mean growth.') : null }),
    ], 'hl-tot-figs');
    // the same kinds and days: the figures and the sizes change in place, the chart stays
    const sig = JSON.stringify([(st.kinds || []).map((k) => k.kind), growth]);
    if (figsEl && sig === storeSig) { figsEl = morph(figsEl, figs); storeKinds?.update(st.kinds, st.total_bytes); return; }
    storeSig = sig;
    figsEl = figs;
    const kt = storeKinds = kindsTable(ctx, st.kinds, st.total_bytes, { title: 'By kind', csvName: 'server storage by kind', coverIds: ['dbinfo.table.sizes'] });
    const chartBox = h('div', { class: 'chart hl-growth' });
    const readout = h('span', { class: 'val' }, 'Point at a day.');
    const growthCsv = csvButton({ onClick: () => downloadCsv(csvFilename('sentry-mate', 'storage growth'), growth, [column('Day', 'date'), column('Added (bytes)', 'bytes')]) });
    replace(storeSlot,
      figs,
      h('div', { class: 'hl-store-cols' },
        kt.el,
        h('section', { class: 'panel panel-pad hl-growth-panel', 'aria-label': 'Added per day' },
          h('div', { class: 'hl-plot-bar' }, h('h2', { class: 'panel-title' }, icon('calendar'), 'Added per day'), growthCsv),
          h('div', { class: 'chart-label' }, h('span', { class: 'chart-unit' }, 'MB per day'), readout),
          growth.length ? chartBox : emptyState({ icon: 'calendar', title: 'No full day yet', text: 'Growth shows after the first full day.' }),
          h('p', { class: 'note' }, 'Bytes added on each of the last full days (UTC days), for all cars together.'))));
    if (growth.length) {
      const t = growth.map((g) => Date.parse(`${g.date}T12:00:00Z`));
      const mb = growth.map((g) => g.bytes / 1e6);
      await barChart(chartBox, ctx, { t_ms: t, values: mb, color: '--live', tz: 'UTC', label: 'Data added per day in megabytes, for all cars together, as bars',
        onCursor: (i) => replace(readout, i == null ? 'Point at a day.' : h('b', null, `${formatDayKey(growth[i].date)}: ${bytes(growth[i].bytes)}`)) });
    }
  }

  /* ---------- server health */
  const srvSlot = h('div', { class: 'hl-srv' });
  const srvPanel = panel({ title: 'Server health', icon: 'pulse', cls: 'hl-srv-panel' }, srvSlot);
  async function loadServer() {
    if (!admin) { replace(srvSlot, serverLocked(), clockNote(), healthAddr()); return; }
    let sh, geo;
    try {
      [sh, geo] = await Promise.all([ctx.api.get('system/health'), ctx.api.get('geocode/status').catch(() => null)]);
    } catch (e) {
      if (e.status === 403) { replace(srvSlot, serverLocked(), clockNote(), healthAddr()); return; }
      throw e;
    }
    if (!ctx.alive()) return;
    const v = sh.versions || {};
    const mem = sh.ingest?.memory || {};
    const memPct = mem.limit_bytes ? (mem.used_bytes / mem.limit_bytes) * 100 : null;
    const ms = (x) => (x == null ? DASH : `${formatNumber(x, { maximumFractionDigits: x < 10 ? 1 : 0 })} ms`);
    const t = sh.timings_ms || {};
    replace(srvSlot,
      h('div', { class: 'hl-srv-cols' },
        h('div', null,
          h('h3', { class: 'hl-sub' }, 'Running'),
          kv([
            kvRow('Up for', sh.uptime_ms != null ? formatDuration(sh.uptime_ms) : DASH, 'clock'),
            kvRow('Server version', h('span', { class: 'mono' }, v.server || DASH), 'chip'),
            kvRow('API version', h('span', { class: 'mono' }, v.api ?? DASH), 'chip'),
            kvRow('Storage format', h('span', { class: 'mono' }, v.db_format ?? DASH), 'server', { title: 'Raised when the way data is kept changes; older data is converted at start.' }),
            kvRow('Timeline rules', h('span', { class: 'mono' }, v.timeline_rules ?? DASH), 'timeline', { title: 'When the rules change, drives and states are rebuilt from the logs.' }),
            kvRow('Import mapping', h('span', { class: 'mono' }, v.import_mapping ?? DASH), 'upload'),
            kvRow('Last housekeeping', sh.housekeeping_last_ms != null ? agoEl(sh.housekeeping_last_ms, tz) : 'not yet', 'reset', { title: 'Checks, compaction and the daily storage figure.' }),
          ]),
          h('h3', { class: 'hl-sub' }, 'Tesla API'),
          kv([
            kvRow('Accounts', sh.tesla?.accounts ? h('a', { class: 'link', href: `${state.buildUrl('settings-integrations')}#tesla` }, `${formatNumber(sh.tesla.accounts)} reading ${formatNumber(sh.tesla.cars ?? 0)} ${sh.tesla.cars === 1 ? 'car' : 'cars'}`)
              : h('span', { class: 'faint' }, 'None: no connection to Tesla is made'), 'car'),
            kvRow('Requests today', formatNumber(sh.tesla?.requests_today ?? 0), 'pulse', { title: 'At most 3,000 a day per account.' }),
            kvRow('Last error', sh.tesla?.last_error ? h('span', { class: 'warn' }, lastErrorText(sh.tesla.last_error), sh.tesla.last_error.at_ms ? h('span', { class: 'faint' }, ' · ', agoEl(sh.tesla.last_error.at_ms, tz)) : null) : h('span', { class: 'faint' }, 'none'), 'warn'),
          ])),
        h('div', null,
          h('h3', { class: 'hl-sub' }, 'Memory and caches'),
          h('div', { class: 'hl-mem' },
            h('div', { class: 'hl-mem-row' }, h('span', null, 'Upload memory'), h('span', null, `${bytes(mem.used_bytes)} of ${bytes(mem.limit_bytes)}`)),
            h('span', { class: ['bar', memPct > 85 ? 'bar--warm' : null], vars: { '--w': `${Math.min(100, memPct || 0)}%` } }, h('i')),
            h('p', { class: 'explain' }, 'Logs waiting in memory before they are written; full means new uploads wait a moment.')),
          kv([
            kvRow('Cars open', `${formatNumber(sh.db?.open_cars ?? 0)} of ${formatNumber(sh.db?.cars ?? 0)}`, 'car', { title: 'Car stores kept open for quick answers.' }),
            kvRow('Open stores take', bytes(sh.db?.open_bytes), 'server'),
            kvRow('Cars kept in memory', formatNumber(sh.ingest?.cached_cars ?? 0), 'car'),
            kvRow('Address look-ups', [geo ? `${geo.mode === 'online' ? 'Online' : 'Offline'}, ${formatNumber(geo.cache_size ?? 0)} cached` : (sh.geocoder ? `${sh.geocoder.mode}, ${formatNumber(sh.geocoder.cache_size ?? 0)} cached` : DASH),
              // the hit rate is the row's title on a desktop; a phone has no hover, so it is written out there
              sh.geocoder?.hits_pct != null ? h('span', { class: 'hl-phone-only' }, ` · ${formatNumber(sh.geocoder.hits_pct, { maximumFractionDigits: 1 })} % from the cache`) : null], 'globe',
              { title: sh.geocoder?.hits_pct != null ? `${formatNumber(sh.geocoder.hits_pct, { maximumFractionDigits: 1 })} % answered from the cache` : null }),
          ])),
        h('div', null,
          h('h3', { class: 'hl-sub' }, 'How fast'),
          kv([
            kvRow('Read one log', ms(t.decode_per_log), 'logs', { title: 'Decoding one uploaded log, on average.' }),
            kvRow('Rebuild a window', ms(t.rebuild_per_window), 'reset'),
            kvRow('Typical answer', ms(t.api_p50), 'gauge', { title: 'Half of the API answers are faster than this.' }),
            kvRow('Slow answer', ms(t.api_p95), 'gauge', { title: '95 of 100 API answers are faster than this.' }),
            kvRow('This reading took', ms(sh.read_ms), 'clock'),
          ]))),
      clockNote(), healthAddr());
  }
  function clockNote() {
    return h('p', { class: 'note hl-clock', cover: 'faq.docker-timestamps' }, icon('clock', 'ico--sm'), ' ',
      `Server clock: ${new Date(ctx.api.now()).toISOString().replace('T', ' ').slice(0, 19)} UTC. Times are stored in UTC; every page shows them in the car’s own time zone (this page: ${tz.replace(/_/g, ' ')}). The container’s log lines are in UTC unless its TZ variable says otherwise.`);
  }
  function healthAddr() {
    return h('div', { class: 'hl-addr', cover: 'web.health-api.health-check' },
      h('h3', { class: 'hl-sub' }, 'Health address for container checks'),
      h('p', { class: 'note' }, h('span', { class: 'mono hl-code' }, 'GET /health'), ' answers 200 with an empty body while the server is serving. It needs no sign-in and tells nothing about the cars or the data, so a monitor or the container’s health check can call it freely.'),
      h('pre', { class: 'hl-pre mono' }, 'HEALTHCHECK --interval=30s CMD ["/sentry-mate", "health"]\n# or any monitor: GET https://<your server>/health  ->  200'));
  }

  /* ---------- API errors */
  const tryOut = h('div', { class: 'hl-try', 'aria-live': 'polite' });
  const tryBtn = h('button', { class: 'btn btn--sm', type: 'button', cover: 'web.health-api.unknown-car', onclick: () => tryUnknown(),
    title: 'Ask the server for a car that does not exist and show what it answers' }, icon('question', 'ico--sm'), 'Ask for a car that does not exist');
  async function tryUnknown() {
    tryBtn.disabled = true;
    try {
      await ctx.api.get('cars/no-such-car-0');
      replace(tryOut, h('span', { class: 'warn' }, 'The server answered with a car. That should not happen.'));
    } catch (e) {
      if (!ctx.alive()) return;
      replace(tryOut, h('pre', { class: 'hl-pre mono' }, `${e.status} ${JSON.stringify({ error: { code: e.code, message: e.message } }, null, 2)}`),
        h('p', { class: 'note' }, 'A car you may not see gives exactly this answer, so nobody can find out which car ids exist.'));
    } finally { tryBtn.disabled = false; }
  }
  const errRows = API_ERRORS.map(([status, code, text]) => ({ status, code, text }));
  const errTable = dataTable(ctx, {
    title: 'API errors', icon: 'info', rows: errRows, cls: 'hl-errors', csvName: 'api errors', cover: 'web.health-api.errors',
    sort: { key: 'status', dir: 'asc' },
    side: tryBtn,
    note: 'Every /api/v1 call that fails answers with one shape: { "error": { "code", "message", "fields" } }. code is for programs, message for people, fields names each bad input by its path (units.distance).',
    columns: [
      { key: 'status', label: 'Status', num: true, sort: true, render: (r) => h('span', { class: 'mono' }, String(r.status)) },
      { key: 'code', label: 'Code', sort: true, render: (r) => h('span', { class: 'mono' }, r.code) },
      { key: 'text', label: 'When', render: (r) => h('span', { class: 'hl-wrap' }, r.text) },
    ],
  });
  errTable.el.append(tryOut);

  /* ---------- every car */
  const carSearch = h('input', { class: 'input', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car' });
  const carTable = dataTable(ctx, {
    title: 'Every car', icon: 'fleet', path: 'cars', params: {}, limit: 50, cls: 'hl-cars', csvName: 'cars data status', csv: { all: true }, cover: 'faq.stopped-recording',
    side: h('label', { class: 'field field--search' }, icon('search', 'ico--sm'), carSearch),
    sort: { key: 'name', dir: 'asc' },
    note: 'Each car you may see, paged for any number of cars. Open one for its gaps, keys, totals, storage and signals.',
    columns: [
      { key: 'name', label: 'Car', sort: 'name', render: (c) => h('a', { class: 'cell-text', href: state.buildUrl('health', { car: c.id }), title: 'This car’s data health' },
        h('span', { class: 'main' }, c.name || c.id), h('span', { class: 'sub' }, [c.model, c.year].filter(Boolean).join(' · '))), csv: { label: 'Car', value: (c) => c.name || c.id } },
      { key: 'state', label: 'Now', render: (c) => statePill(c.now?.state || 'unknown', { stale: c.now?.stale }), csv: { label: 'Now', value: (c) => stateLabel(c.now?.state || 'unknown') } },
      { key: 'last', label: 'Last data', num: true, sort: 'last_activity', render: (c) => (c.now?.received_ms != null ? agoEl(c.now.received_ms, c.time_zone) : h('span', { class: 'faint' }, 'none yet')), csv: { label: 'Last data (UTC)', value: (c) => iso(c.now?.received_ms) } },
      { key: 'status', label: 'Data', render: (c) => {
        const bad = (c.attention || []).filter((a) => a === 'upload_silence' || a === 'trust_failed');
        return bad.length ? h('span', { class: 'hl-flags' }, bad.map((a) => pill(REASONS[a].title, { tone: 'warm', ic: 'warn' })))
          : h('span', { class: 'ok with-icon' }, icon('check', 'ico--sm'), 'Arriving');
      }, csv: { label: 'Data', value: (c) => (c.attention || []).filter((a) => a === 'upload_silence' || a === 'trust_failed').map((a) => REASONS[a].title).join('; ') || 'Arriving' } },
    ],
    empty: () => emptyState({ icon: 'car', title: carSearch.value ? 'No car matches' : 'No car yet', text: carSearch.value ? `No car name holds “${carSearch.value}”.` : 'Add a car on Settings: Cars.' }),
  });
  let carTimer = null;
  ctx.on(carSearch, 'input', () => { clearTimeout(carTimer); carTimer = setTimeout(() => ctx.alive() && carTable.setParams({ search: carSearch.value.trim() || null }), 250); });
  ctx.onCleanup(() => clearTimeout(carTimer));

  replace(ctx.view, root);
  const connCard = overviewCard(ctx, admin);
  root.append(head, connCard.el, storePanel, carTable.el, srvPanel, errTable.el,
    h('p', { class: 'note hl-signals-note', cover: ['nr.dash.state-table', 'nr.dash.state-table.parameter', 'nr.dash.state-table.value', 'nr.dash.state-table.sort-scroll'] },
      icon('signals', 'ico--sm'), ' Every decoded signal of a car, with its value and a chart over time, is on that car’s data health page: open a car above.'));

  async function reloadAll(say) {
    reloadBtn.disabled = true;
    try {
      await Promise.all([loadStorage(), loadServer(), carTable.reload()]);
      readAt = ctx.api.now(); paintReadAt();
      if (say && ctx.alive()) flash('Read again.', { kind: 'info' });
    } catch (e) {
      if (ctx.alive()) flash(errorWords(e).text, { kind: 'error' });
    } finally { reloadBtn.disabled = false; }
  }

  try {
    await Promise.all([loadStorage(), loadServer(), carTable.ready, connCard.load()]);
  } catch (e) {
    if (ctx.alive()) ctx.fail(e);
    return;
  }
  if (!ctx.alive()) return;
  ctx.ready();
}

