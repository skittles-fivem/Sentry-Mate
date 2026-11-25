// Software (/cars/{car}/software): the version a car runs now, its update history in the period, and
// the versions across the whole fleet.
// - Updates come from the car itself (its update status and the build ids of its modules) or from an
//   imported TeslaMate or TeslaFi history. A version is only ever what someone said: read from a live input
//   (local TeslaMate, TeslaFi, the Tesla API), imported, typed by the owner, or learned from another update
//   with the same module builds. An update seen only as changed module
//   builds says so ("build changed", with the modules) and never gets a guessed version.
// - GET /cars/{car}/updates (period, newest first, paged; summary in each page), PATCH .../updates/{id}
//   (admin: type or forget a version), GET /fleet/versions (every car, searched and paged by cursor).
// - A period with no update says when the last one was (summary.last_update, any period) with a "Show all
//   time" button that switches the page's period.
// - Live: an update that starts or ends ("rows" events with kind "updates") loads the lists again.
// - Tesla updates installed: an update found from the modules' firmware fingerprints (detected_by
//   "fingerprint") has its own card (#fingerprint-updates): which fields changed (old hex -> new hex), between
//   which drives it was installed (the last drive with the old set, the first with the new one), and its name (or
//   "Not named yet") with where the name came from. An admin's "Name it" / "Edit name" opens a box in place to
//   type the version (PATCH; Clear forgets it; Escape or Cancel closes it, a save closes it). An auto-named one
//   says it was named from an earlier update with the same fingerprints, or from the live input that reported the
//   version within a day after it.
// - Car firmware (#firmware): GET /cars/{car}/firmware, each module's fingerprints as lowercase hex and when the
//   current value was first seen. The car page links here.
// - Module firmware history (#firmware-history): GET /cars/{car}/firmware/history, every fingerprint each module
//   has had (filter by module, paged with Show more) and a "versions over time" strip from its spans; reloads
//   with the other lists on "rows" events with kind "updates". The car page links here.

import * as state from '../shared/state.js';
import { h, replace, cover, setVars, disclose } from '../shared/dom.js';
import { column } from '../shared/csv.js';
import { formatDate, formatTime, formatDateTime, formatDuration, HOUR } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, figures, pill, tag } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState } from '../core/states.js';
import { openPeriodPicker } from '../core/period.js';
import { openCarSwitcher } from '../core/carswitch.js';
import { openDialog } from '../core/dialog.js';
import { field, form } from '../core/form.js';
import { flash } from '../core/flash.js';
import { isAdmin } from '../core/prefs.js';
import { q, DASH } from '../core/fmt.js';
import { srcTitle, hasKind } from '../core/source.js';

export const id = 'software';
export const css = true;

const enc = encodeURIComponent;
const VERSION_RX = /^\d{4}\.\d{1,2}(\.\d{1,3}){0,2}$/;

// where a version came from: [tag, what it means, where the version now is from]
const SOURCE = {
  teslamate: ['Imported', 'Imported from a TeslaMate backup with the version text it recorded.', 'Imported from TeslaMate'],
  teslafi: ['Imported', 'Imported from a TeslaFi backup with the version text it recorded.', 'Imported from TeslaFi'],
  teslamate_local: ['TeslaMate', 'Read live from your local TeslaMate, which reports the installed version.', 'From TeslaMate'],
  teslafi_api: ['TeslaFi', 'Read live from TeslaFi, which reports the installed version.', 'From TeslaFi'],
  tesla_api: ['Tesla API', 'Read from the Tesla API, which names the installed version.', 'From the Tesla API'],
  owner: ['Typed by the owner', 'An admin typed this version in Sentry Mate.', 'Typed by an admin'],
  build_match: ['Same build', 'Another update with exactly the same module builds has this version.', 'From an update with the same builds'],
};
// the live inputs that name an update found from fingerprints (the version they reported within a day after it)
const INPUT_NAMED = { teslamate_local: 'Named from local TeslaMate', teslafi_api: 'Named from TeslaFi', tesla_api: 'Named from the Tesla API' };
const inputNamed = (r) => (r.version && r.named_by !== 'owner' ? INPUT_NAMED[r.version_source] || null : null);
// how an update was found
const DETECTED = {
  ota_status: ['Update status', 'The car reported that it was installing an update.'],
  build_change: ['Module builds changed', 'The build ids its modules send on the vehicle bus changed.'],
  fingerprint: ['Tesla update installed', 'The firmware fingerprints the car\'s modules send on the vehicle bus changed.'],
  teslamate: ['Imported', 'From the imported TeslaMate history.'],
  teslafi: ['Imported', 'From the imported TeslaFi history.'],
  tesla_api: ['Tesla API', 'The version the Tesla API reports changed.'],
};
// the modules whose build ids are compared
const MODULES = {
  bms: 'Battery management', vcfront: 'Front body controller', dir: 'Rear drive unit', dif: 'Front drive unit', di_front: 'Front motor', di_rear: 'Rear motor', fc: 'Fast-charge controller',
};
const NAMED = {
  owner: 'Named by an admin.',
  auto: 'Named from an earlier update with the same fingerprints.',
};
const moduleNames = (list) => (list || []).map((m) => MODULES[m] || m);
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const cid = car.id;
  const admin = isAdmin();
  const path = `cars/${enc(cid)}/updates`;

  // the owner's choice about outside links (read-only for watchers; off when it cannot be read)
  const settings = await ctx.api.get('settings', null, { maxAgeMs: 300000 }).catch(() => null);
  if (!ctx.alive()) return;
  const notesOn = !!settings?.outside_links?.release_notes;

  const root = h('div', { class: 'sw-page', cover: 'shots.updates' });

  /* ---------- head and filters */
  const reloadBtn = h('button', {
    class: 'btn', type: 'button', cover: 'updates.control.refresh',
    title: 'Load the figures and both lists again now. They also reload by themselves when an update starts or ends.',
    onclick: () => reloadAll(true),
  }, icon('reset', 'ico--sm'), 'Reload');
  const head = pageHead({
    eyebrow: car.name, title: 'Software', cls: 'sw-head',
    sub: [period.label, `times in ${tz.replace(/_/g, ' ')}`],
    purpose: 'Which version each car runs and when it changed. A version is shown only when someone said what it is; otherwise the page says the software changed and which modules changed.',
    actions: [reloadBtn],
  });
  cover(head, 'updates.section.car_header');
  const filters = h('section', { class: 'panel panel-pad sw-filters', 'aria-label': 'What the page covers' },
    h('div', { class: 'filter-bar' },
      h('button', { class: 'btn', type: 'button', cover: 'updates.filter.time_range', onclick: (e) => openPeriodPicker(e.currentTarget),
        title: 'The period of the update list and its figures (the same picker as in the top bar). An update belongs to it when it started in it.' },
      icon('calendar', 'ico--sm'), period.label, icon('chev-d', 'ico--sm')),
      h('button', { class: 'btn', type: 'button', cover: 'updates.filter.car', onclick: openCarSwitcher, title: 'Show another car on this page' },
        icon('car', 'ico--sm'), h('span', { class: 'name-cut' }, car.name), icon('chev-d', 'ico--sm')),
      h('button', { class: 'btn btn--ghost', type: 'button', title: 'Every car with the version it runs, in one list (further down this page)',
        onclick: () => fleetTable.el.scrollIntoView({ behavior: 'smooth', block: 'start' }) }, icon('fleet', 'ico--sm'), 'Versions of every car')),
    h('p', { class: 'note' }, icon('live', 'ico--sm'), ' The lists reload by themselves when an update starts or ends; Reload is only for impatience. The version now does not depend on the period.'));

  /* ---------- summary */
  const nowSlot = h('div', { class: 'sw-now-body' });
  const figSlot = h('div');
  const summary = h('div', { class: 'sw-summary' },
    panel({ title: 'Version now', icon: 'chip', cls: 'sw-now' }, nowSlot),
    panel({ title: 'In this period', icon: 'stats', cls: 'sw-figs' }, figSlot,
      h('p', { class: 'note sw-notify', cover: 'projects.telegram-bot' }, icon('bolt', 'ico--sm'),
        ' Want a message when the software changes? Sentry Mate publishes a “software changed” event (MQTT and the events list) as soon as an update ends. ',
        h('a', { class: 'link', href: state.buildUrl('settings-integrations') }, 'Settings: Integrations'),
        ' connects it to your chat or home automation. A pending update is not on the CAN bus, so there is no “update available” message.')));

  function paintSummary(sm, rows) {
    sm = sm || {};
    const installing = rows.find((r) => r.end_ms == null);
    const v = sm.current_version;
    const src = SOURCE[sm.current_version_source];
    const changed = sm.build_changed_since != null; // the car runs software nobody has named yet
    const at = (ms) => formatDateTime(ms, tz, { now: ctx.api.now() });
    const since = sm.current_version_since_ms != null ? at(sm.current_version_since_ms) : null;
    replace(nowSlot,
      h('div', { class: 'sw-version' },
        h('span', { class: ['sw-version-text', (!v || changed) && 'is-unknown'] }, changed ? 'Not named yet' : v || 'Not known'),
        installing ? pill('Installing now', { tone: 'warm', ic: 'clock' }) : null,
        changed ? pill('Software changed', { tone: 'warm', ic: 'warn' }) : null),
      h('dl', { class: 'kv sw-kv' },
        changed ? kvRow('Last named version', v || 'none', 'chip') : null,
        kvRow(changed ? 'Source of that name' : 'Source', src ? h('span', { title: src[1] }, src[2]) : 'Nobody has said yet', 'info'),
        changed
          ? kvRow('Named version ran', `${since ? `from ${since} ` : ''}to ${at(sm.build_changed_since)}`, 'clock')
          : kvRow('Running since', since || DASH, 'clock'),
        changed ? kvRow('Software changed', at(sm.build_changed_since), 'warn') : null,
        installing ? kvRow('Installing since', at(installing.start_ms), 'clock') : null),
      h('p', { class: 'explain' }, sm.build_changed_since != null
        ? `The module builds changed after ${v || 'the last known version'} was recorded, so the car now runs a version nobody has named. ${admin ? 'Type it on the newest update below.' : 'An admin can type it on the newest update below.'}`
        : v ? `${src ? src[1] : ''} The car's bus does not carry its version text; Sentry Mate never guesses one.`
          : 'The car does not send its version text. It is known once a live input (local TeslaMate, TeslaFi or the Tesla API) reports it, a TeslaMate or TeslaFi history is imported, an admin types it, or another car on the same build has one.'));
    const median = sm.median_gap_ms;
    replace(figSlot, figures([
      metric('Updates', sm.count ?? null, 'count', {
        cover: 'updates.stat.count', size: 'lg',
        explain: 'Updates that started in the period: seen by the car (update status or changed module builds) or imported.',
      }),
      metric('Typical time between updates', median != null ? formatDuration(median) : null, null, {
        cover: 'updates.stat.median_gap', size: 'lg',
        why: 'Needs an update in the period with one before it.',
        explain: 'The median gap before each update that started in the period, measured from the end of the update before it (which may be earlier than the period).',
      }),
    ]),
    // a narrow period with nothing in it still says when the last update was
    !sm.count && !rows.length && sm.last_update?.start_ms != null && period.preset !== 'all'
      ? h('p', { class: 'note sw-last' }, icon('clock', 'ico--sm'), ` ${lastWords(sm.last_update)}, before this period.`) : null);
  }
  const lastWords = (u) => `The last update was ${formatDate(u.start_ms, tz, { now: ctx.api.now(), year: true })}${u.version ? ` (${u.version})` : ''}`;
  const allTimeBtn = () => h('button', { class: 'btn', type: 'button', title: 'Switch the period of this page to all time',
    onclick: () => state.setPeriod({ preset: 'all' }, { push: true }) }, icon('calendar', 'ico--sm'), 'Show all time');
  const kvRow = (label, value, ic) => h('div', { class: 'kv-row' }, h('dt', null, icon(ic, 'ico--sm'), label), h('dd', null, value));

  /* ---------- update history */
  const timelineHref = (r) => state.buildUrl('timeline', { car: cid, period: { from: r.start_ms - HOUR, to: (r.end_ms ?? ctx.api.now()) + HOUR } });
  const onVersion = (r) => ({ from: r.end_ms ?? r.start_ms, to: r.until_ms ?? ctx.api.now() });
  const notesHref = (v) => `https://www.notateslaapp.com/software-updates/version/${enc(v)}/release-notes`;

  function versionCell(r) {
    const src = SOURCE[r.version_source];
    const main = r.version
      // where the version came from is the title, not a tag in the row
      ? h('span', { class: 'sw-ver', title: src ? `${src[2]}. ${src[1]}` : null }, notesOn && VERSION_RX.test(r.version)
        ? h('a', { class: 'link', href: notesHref(r.version), target: '_blank', rel: 'noopener noreferrer external', title: 'Release notes of this version on an outside site (opens in a new tab)' }, r.version, ' ', icon('ext', 'ico--sm'))
        : r.version)
      : h('span', { class: 'sw-ver is-unknown' }, r.detected_by === 'fingerprint' ? 'Tesla update installed' : 'Build changed', tag(r.detected_by === 'fingerprint' ? 'Not named yet' : 'Version not known', 'warm'));
    if (r.detected_by === 'fingerprint') {
      return h('span', { class: 'cell-text' }, main,
        h('span', { class: 'sub sw-mods' }, inputNamed(r) || (r.named_by === 'auto' ? 'Named from an earlier update with the same fingerprints' : `Changed: ${moduleNames(r.changed_modules).join(', ')}`)),
        h('a', { class: 'link sw-fp-jump', href: `#fp-${r.id}`, title: 'The fingerprints that changed, between which drives, and the name box' }, admin ? (r.version ? 'Details and name' : 'Details, name it') : 'Details'));
    }
    const sub = r.version
      ? (r.changed_modules?.length ? `Changed: ${moduleNames(r.changed_modules).join(', ')}` : null)
      : r.changed_modules?.length ? `Changed: ${moduleNames(r.changed_modules).join(', ')}` : 'No module list';
    return h('span', { class: 'cell-text' }, main, sub ? h('span', { class: 'sub sw-mods' }, sub) : null,
      admin && hasKind(r, 'smo') && r.version_source !== 'tesla_api' && r.detected_by !== 'fingerprint' ? h('button', { class: 'btn btn--sm btn--ghost sw-edit', type: 'button', onclick: () => editVersion(r), title: 'Type the version this update installed' },
        icon('settings', 'ico--sm'), r.version ? 'Change version' : 'Type the version') : null);
  }

  const columns = [
    {
      key: 'start', label: 'Started', cover: 'updates.table.date', title: 'When the update started (the car’s time zone). Opens the timeline around it.',
      render: (r) => h('span', { class: 'cell-text' },
        h('a', { class: 'main', href: timelineHref(r), title: 'Open the timeline around this update' }, formatDate(r.start_ms, tz, { now: ctx.api.now(), year: true })),
        h('span', { class: 'sub' }, formatTime(r.start_ms, tz))),
      csv: { label: 'Started (UTC)', value: (r) => iso(r.start_ms) },
    },
    {
      key: 'duration', label: 'Took', num: true, cover: 'updates.table.duration', title: 'From the start of the update to its end, from the car’s own update status when recorded.',
      render: (r) => (r.end_ms == null
        ? h('span', { class: 'cell-text' }, pill('Installing', { tone: 'warm' }), h('span', { class: 'sub' }, `${formatDuration(r.duration_ms)} so far`))
        : r.readings_gap ? h('span', { class: 'cell-text', title: 'No fingerprint readings for a day or more before the new ones: when it was installed in there is not known' }, h('span', { class: 'faint' }, 'not known'), h('span', { class: 'sub' }, `seen ${formatTime(r.end_ms, tz)}`))
          : h('span', { class: 'cell-text' }, formatDuration(r.duration_ms), h('span', { class: 'sub' }, `ended ${formatTime(r.end_ms, tz)}`))),
      csv: { label: 'Took (minutes)', value: (r) => (r.duration_ms == null ? '' : Math.round(r.duration_ms / 6000) / 10) },
    },
    {
      key: 'since', label: 'After the previous', num: true, cover: 'updates.table.since_previous', title: 'From the end of the update before to the start of this one, to the minute.',
      render: (r) => (r.since_previous_ms == null ? h('span', { class: 'cell-text' }, DASH, h('span', { class: 'sub' }, 'first one recorded'))
        : h('span', { class: 'cell-text' }, formatDuration(r.since_previous_ms), h('span', { class: 'sub' }, `${formatNumber(r.since_previous_ms / 86400000, { maximumFractionDigits: 0 })} days`))),
      csv: { label: 'After the previous (days)', value: (r) => (r.since_previous_ms == null ? '' : Math.round(r.since_previous_ms / 864000) / 100) },
    },
    {
      key: 'version', label: 'Version', cls: 'sw-col-ver', cover: 'updates.table.version', title: 'Read from a live input (local TeslaMate, TeslaFi, the Tesla API), imported, typed by an admin, or the same build as an update with a known version. Never guessed.',
      render: versionCell,
      csv: { label: 'Version', value: (r) => r.version || '' },
    },
    { key: 'modules', label: 'Changed modules', hidden: true, csv: { label: 'Changed modules', value: (r) => moduleNames(r.changed_modules).join('; ') } },
    { key: 'vsource', label: 'Version from', hidden: true, csv: { label: 'Version from', value: (r) => (SOURCE[r.version_source] ? SOURCE[r.version_source][0] : '') } },
    {
      key: 'source', label: 'Found by', title: 'How Sentry Mate knows about this update',
      // where the data came from is in the row's hover title, not a line here
      render: (r) => { const d = DETECTED[r.detected_by] || [r.detected_by, '']; return h('span', { title: d[1] }, d[0]); },
      csv: { label: 'Found by', value: (r) => (DETECTED[r.detected_by] ? DETECTED[r.detected_by][0] : r.detected_by) },
    },
    {
      key: 'charges', label: 'Charges on it', num: true, cover: 'updates.table.charge_count', title: 'Charges that started while this version was installed, by exact time. Opens them.',
      render: (r) => (r.charges ? h('a', { class: 'link', href: state.buildUrl('charges', { car: cid, period: onVersion(r) }), title: 'The charges while on this version' }, formatNumber(r.charges)) : formatNumber(r.charges ?? 0)),
      csv: { label: 'Charges on it', value: (r) => r.charges },
    },
    {
      key: 'range', label: 'Range at 100 %', num: true, cover: 'updates.table.avg_range', title: 'The range a full battery gives while on this version (the car’s rated range scaled to 100 %).',
      render: (r) => q(r.full_range_km, 'distance', { why: 'No reading' }),
      csv: column('Range at 100 %', 'full_range_km', 'distance'),
    },
    {
      key: 'pack', label: 'Full pack', num: true, title: 'Full-pack energy the battery management system reported while on this version. Imported rows have none.',
      render: (r) => (r.full_pack_kwh == null ? h('span', { class: 'faint', title: 'Imported rows carry no battery-management reading.' }, 'not measured') : q(r.full_pack_kwh, 'energy')),
      csv: column('Full pack', 'full_pack_kwh', 'energy'),
    },
  ];

  // the download holds readable columns (units in the headers, dates in UTC), hidden ones included
  const csvOf = (list) => list.filter((c) => c.csv).map((c) => (typeof c.csv === 'object' ? c.csv : { label: c.label, value: c.csv }));

  const notesNote = h('p', { class: 'note sw-notes', cover: 'updates.link.release_notes' }, icon(notesOn ? 'ext' : 'lock', 'ico--sm'), ' ',
    h('span', null, notesOn ? 'A known version links to its release notes on an outside site. Nothing is sent until you click one.'
      : `Release-notes links are off, so nothing leaves this server. ${admin ? 'Turn them on in ' : 'An admin can turn them on in '}`,
    !notesOn ? h('a', { class: 'link', href: `${state.buildUrl('settings-maps')}#outside-links` }, 'Settings: Maps and addresses') : null, !notesOn ? '.' : null));

  let painted = false;
  const table = dataTable(ctx, {
    title: 'Update history', icon: 'chip', path, params: ctx.params, limit: 50, cls: 'sw-table', cover: 'updates.table', box: 'Updates', autoMore: true,
    csvName: 'software updates', csv: { all: true, columns: csvOf(columns) },
    columns: columns.filter((c) => !c.hidden),
    rowTitle: (r) => srcTitle(r) || 'Data from the car',
    note: 'Newest first. Charges and range are what the car did while on each version: from the end of the update to the start of the next one (or now).',
    side: notesNote,
    empty: (pg) => (pg?.summary?.last_update?.start_ms != null && period.preset !== 'all' ? emptyState({
      icon: 'chip', title: `No updates in this period; the last one was ${formatDate(pg.summary.last_update.start_ms, tz, { now: ctx.api.now(), year: true })}${pg.summary.last_update.version ? ` (${pg.summary.last_update.version})` : ''}`,
      text: 'All time shows it with every other update.', action: allTimeBtn(),
    }) : emptyState({
      icon: 'chip', title: 'No updates in this period',
      text: period.preset === 'all' ? `Nothing has been recorded for ${car.name} yet: an update shows once the car installs one while recording, or with an imported TeslaMate history.`
        : 'No update started in this period. Choose a longer period (All time shows every update).',
    })),
    onPage: (pg) => {
      if (!ctx.alive()) return;
      painted = true;
      paintSummary(pg.summary, table.rows());
      fpPaint = paintFingerprints(table.rows());
    },
  });

  /* ---------- versions of every car */
  const search = h('input', { class: 'input', type: 'search', placeholder: 'Find a car or version', 'aria-label': 'Find a car or version' });
  const fleetColumns = [
    {
      key: 'car', label: 'Car',
      render: (r) => h('a', { class: ['main', r.car === cid && 'is-current'], href: state.buildUrl('software', { car: r.car, period: state.current().period }), title: 'This car’s software page' }, r.name || r.car),
      csv: { label: 'Car', value: (r) => r.name || r.car },
    },
    {
      key: 'version', label: 'Version now',
      title: 'Only a version someone named. After a build change the car runs software nobody has named yet; the last named version is shown under it.',
      render: (r) => (r.build_changed_since != null
        ? h('span', { class: 'cell-text' }, h('span', { class: 'sw-ver is-unknown' }, 'Not named yet'), h('span', { class: 'sub' }, r.version ? `last named ${r.version}` : 'none named before'))
        : r.version ? h('span', { class: 'sw-ver', title: SOURCE[r.version_source] ? SOURCE[r.version_source][2] : null }, r.version)
          : h('span', { class: 'sw-ver is-unknown' }, 'Not known')),
      csv: { label: 'Version now', value: (r) => (r.build_changed_since != null ? '' : r.version || '') },
    },
    { key: 'named', label: 'Last named version', hidden: true, csv: { label: 'Last named version', value: (r) => r.version || '' } },
    {
      key: 'changed', label: 'Software changed since',
      title: 'The module builds changed after the last known version; the version it runs now has no name yet.',
      render: (r) => (r.build_changed_since != null ? h('span', { class: 'warn' }, formatDateTime(r.build_changed_since, tz, { now: ctx.api.now() })) : h('span', { class: 'faint' }, 'no change since')),
      csv: { label: 'Software changed since (UTC)', value: (r) => iso(r.build_changed_since) },
    },
    {
      key: 'last', label: 'Last update', num: true,
      render: (r) => (r.last_update_ms != null ? formatDate(r.last_update_ms, tz, { now: ctx.api.now(), year: true }) : h('span', { class: 'faint' }, 'none recorded')),
      csv: { label: 'Last update (UTC)', value: (r) => iso(r.last_update_ms) },
    },
  ];
  const fleetTable = dataTable(ctx, {
    title: 'Versions of every car', icon: 'fleet', path: 'fleet/versions', params: {}, limit: 50, cls: 'sw-fleet', csvName: 'versions of every car',
    csv: { all: true, columns: csvOf(fleetColumns) },
    side: h('label', { class: 'field field--search' }, search),
    note: 'One row per car you may see, in one list instead of a page per car. Search by name or version; Show more pages through any number of cars.',
    columns: fleetColumns.filter((c) => !c.hidden),
    empty: () => emptyState({ icon: 'search', title: 'No car matches', text: search.value ? `No car name or version holds “${search.value}”. Clear the search to see every car.` : 'There is no car yet.' }),
  });
  fleetTable.el.id = 'sw-fleet';
  let searchTimer = null;
  ctx.on(search, 'input', () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => { if (ctx.alive()) fleetTable.setParams({ search: search.value.trim() || null }); }, 250);
  });
  ctx.onCleanup(() => clearTimeout(searchTimer));

  /* ---------- Tesla updates installed (fingerprint updates in the period) */
  let fpPaint = null;
  const fpSlot = h('div', { class: 'sw-fp-list' });
  const fpPanel = panel({ title: 'Tesla updates installed', icon: 'chip', cls: 'sw-fp', id: 'fingerprint-updates' },
    h('p', { class: 'note' }, 'Found from the firmware fingerprints the car\'s modules send about once a second: when they change, Tesla installed an update. The car never sends the version\'s name, so it stays “Not named yet” until ',
      admin ? 'you type it (as the car\'s screen shows it, under Software). Every later update to the same fingerprints gets that name by itself.' : 'an admin types it. Every later update to the same fingerprints then gets that name by itself.'),
    fpSlot);
  fpPanel.hidden = true;
  const fpCards = new Map(); // update id -> { key, el, input }
  const driveCache = new Map();
  async function driveNear(r) {
    if (driveCache.has(r.id)) return driveCache.get(r.id);
    const p = `cars/${enc(cid)}/drives`;
    const [b, a] = await Promise.all([
      ctx.api.get(p, { to_ms: r.between_ms?.[0] ?? r.start_ms, sort: 'start', dir: 'desc', limit: 1 }).then((x) => x.items?.[0] || null).catch(() => null),
      ctx.api.get(p, { from_ms: r.between_ms?.[1] ?? r.end_ms ?? r.start_ms, sort: 'start', dir: 'asc', limit: 1 }).then((x) => x.items?.[0] || null).catch(() => null),
    ]);
    const out = { before: b, after: a };
    driveCache.set(r.id, out);
    return out;
  }
  const driveLink = (d) => h('a', { class: 'link', href: state.buildUrl('drive', { car: cid, drive: d.id }) },
    `${formatDate(d.start_ms, tz, { year: true })} ${formatTime(d.start_ms, tz)}`, d.start?.label || d.end?.label ? ` (${[d.start?.label, d.end?.label].filter(Boolean).join(' → ')})` : '');
  const hexEl = (x) => h('code', { class: 'sw-hex' }, x);

  function fpCard(r, near) {
    const at = (ms) => formatDateTime(ms, tz, { year: true });
    const between = near.before || near.after
      ? h('p', { class: 'sw-fp-between' }, icon('route', 'ico--sm'), h('span', null, 'Installed between ',
        near.before ? ['the drive of ', driveLink(near.before)] : 'the start of the record', ' and ',
        near.after ? ['the drive of ', driveLink(near.after)] : 'now (no drive since)', '.'))
      : null;
    const seen = h('p', { class: 'sw-fp-between' }, icon('clock', 'ico--sm'),
      h('span', null, r.readings_gap
        ? `Installed between ${at(r.between_ms[0])} and ${at(r.between_ms[1])} (no readings in between): the old fingerprints were last seen then, the new ones first seen then.`
        : `Old fingerprints last seen ${at(r.between_ms?.[0] ?? r.start_ms)}, new ones first seen ${at(r.between_ms?.[1] ?? r.end_ms)}.`));
    const byMod = new Map();
    for (const c of r.changes || []) { if (!byMod.has(c.module)) byMod.set(c.module, { name: c.module_name || MODULES[c.module] || c.module, rows: [] }); byMod.get(c.module).rows.push(c); }
    const changes = h('div', { class: 'sw-fp-changes', role: 'list', 'aria-label': 'Fingerprints that changed' },
      [...byMod.values()].map((m) => h('div', { class: 'sw-fp-mod', role: 'listitem' },
        h('h3', { class: 'sw-fp-mod-name' }, m.name),
        h('dl', { class: 'sw-fp-rows' }, m.rows.map((c) => h('div', { class: 'sw-fp-row' },
          h('dt', { class: 'sw-fp-field' }, c.field),
          h('dd', { class: 'sw-fp-hexes' }, hexEl(c.old), h('span', { class: 'sw-fp-arrow', 'aria-label': 'became' }, '→'), hexEl(c.new))))))));
    const namedWord = !r.version ? 'The car does not send the version\'s name.'
      : inputNamed(r) ? `${inputNamed(r)}: it reported this version within a day after the update.`
        : NAMED[r.named_by] || (SOURCE[r.version_source]?.[1] ?? '');
    // the name shows on the card; an admin's "Edit name" / "Name it" opens the box in place (Escape or Cancel closes it)
    let box = null, input = null, editBtn = null, dz = null;
    if (admin) {
      const fid = `fp-name-${r.id}`;
      input = h('input', { class: 'input sw-fp-input', id: fid, type: 'text', value: r.version || '', placeholder: '2026.38.4', autocomplete: 'off', spellcheck: 'false', inputmode: 'decimal',
        pattern: '\\d{4}\\.\\d{1,2}(\\.\\d{1,3}){0,2}', 'aria-describedby': `${fid}-hint ${fid}-err` });
      const err = h('p', { class: 'form-error', id: `${fid}-err`, role: 'alert', hidden: true });
      const save = h('button', { class: 'btn btn--primary btn--sm', type: 'submit' }, icon('check', 'ico--sm'), 'Save name');
      const clear = h('button', { class: 'btn btn--ghost btn--sm', type: 'button', hidden: !r.version, title: 'Forget the name typed for this update' }, icon('x', 'ico--sm'), 'Clear');
      const cancel = h('button', { class: 'btn btn--ghost btn--sm', type: 'button', title: 'Close the box without saving (Escape)' }, 'Cancel');
      const showErr = (t) => { err.hidden = !t; replace(err, t ? [icon('warn'), t] : null); input.toggleAttribute('aria-invalid', !!t); };
      input.addEventListener('input', () => showErr(null));
      const send = async (v) => {
        save.disabled = clear.disabled = true;
        try {
          await ctx.api.patch(`${path}/${enc(r.id)}`, { version: v });
          if (!ctx.alive()) return;
          dz.close();
          fpCards.delete(r.id);
          flash(v ? `Named: this update installed ${v}. Later updates to the same fingerprints get the name too.` : 'The name is cleared.');
          echoUntil = Date.now() + 5000;
          await reloadAll(false);
          // the card was drawn again: the focus goes back to its Edit button
          if (ctx.alive()) document.getElementById(`fp-${r.id}`)?.querySelector('.sw-fp-edit')?.focus({ preventScroll: true });
        } catch (e) {
          if (e?.name === 'AbortError') return;
          showErr(e?.fields?.version || (e?.code === 'no_such_update' ? 'This update is not there any more (the logs were read again). The list now shows the updates there are.' : e?.message || 'Could not save the name.'));
          if (e?.code === 'no_such_update') reloadAll(false);
        } finally { save.disabled = clear.disabled = false; }
      };
      box = h('form', { class: 'sw-fp-name', id: `${fid}-box`, novalidate: '', onsubmit: (e) => {
        e.preventDefault();
        const v = input.value.trim();
        if (!v) { if (r.version) send(null); else showErr('Type the version as the car\'s screen shows it, such as 2026.38.4.'); return; }
        if (!VERSION_RX.test(v)) { showErr('A version looks like 2026.38.4: the year, a dot and one to three numbers.'); return; }
        if (v === r.version && r.named_by === 'owner') { showErr('That is the name it has already.'); return; }
        send(v);
      } },
      h('label', { class: 'form-label', for: fid }, r.version ? 'Version name' : 'Name this version'),
      h('div', { class: 'sw-fp-name-row' }, input, save, clear, cancel),
      h('p', { class: 'form-hint', id: `${fid}-hint` }, 'As the car\'s screen shows it (Controls, Software), such as 2026.38.4. Clear forgets it.'),
      err);
      clear.addEventListener('click', () => send(null));
      editBtn = h('button', { class: 'btn btn--sm btn--ghost sw-fp-edit', type: 'button', title: r.version ? 'Type another name for this update' : 'Type the version this update installed' },
        icon('settings', 'ico--sm'), r.version ? 'Edit name' : 'Name it');
      dz = disclose(editBtn, box, { focus: input, onClose: () => { input.value = r.version || ''; showErr(null); } });
      cancel.addEventListener('click', () => dz.close({ focus: true }));
    }
    const el = h('article', { class: 'sw-fp-card', id: `fp-${r.id}` },
      h('div', { class: 'sw-fp-head' },
        pill('Tesla update installed', { tone: 'live', ic: 'chip' }),
        h('span', { class: 'sw-fp-when' }, formatDate(r.end_ms ?? r.start_ms, tz, { year: true }))),
      h('div', { class: 'sw-fp-ver' },
        h('span', { class: ['sw-fp-vname', !r.version && 'is-unknown'] }, r.version || 'Not named yet'),
        r.version && SOURCE[r.version_source] ? tag(r.named_by === 'auto' && !inputNamed(r) ? 'Same fingerprints' : SOURCE[r.version_source][0], 'plain') : null,
        editBtn),
      h('p', { class: 'sw-fp-named faint' }, namedWord),
      box,
      between, seen,
      h('p', { class: 'sw-fp-sub metric-label' }, `${(r.changes || []).length} ${(r.changes || []).length === 1 ? 'fingerprint' : 'fingerprints'} changed in ${byMod.size} ${byMod.size === 1 ? 'module' : 'modules'}`),
      changes,
      admin ? null : h('p', { class: 'note sw-fp-ro' }, icon('lock', 'ico--sm'), ' Only an admin can name an update.'));
    return { el, input, isOpen: () => !!dz?.isOpen() };
  }

  async function paintFingerprints(rows) {
    const list = rows.filter((r) => r.detected_by === 'fingerprint');
    const nears = await Promise.all(list.map(driveNear));
    if (!ctx.alive()) return;
    const keep = new Set();
    const els = list.map((r, i) => {
      keep.add(r.id);
      const key = JSON.stringify([r, nears[i].before?.id, nears[i].after?.id]);
      const old = fpCards.get(r.id);
      // a card whose name box is open is left alone; an unchanged one is kept as it is
      if (old && (old.key === key || old.isOpen())) return old.el;
      const c = fpCard(r, nears[i]);
      fpCards.set(r.id, { key, el: c.el, isOpen: c.isOpen });
      return c.el;
    });
    for (const k of [...fpCards.keys()]) if (!keep.has(k)) fpCards.delete(k);
    const same = els.length === fpSlot.children.length && els.every((e, i) => fpSlot.children[i] === e);
    if (!same) replace(fpSlot, els);
    fpPanel.hidden = !list.length;
  }

  /* ---------- car firmware */
  const fwSlot = h('div', { class: 'sw-fw-body' });
  const fwPanel = panel({ title: 'Car firmware', icon: 'chip', cls: 'sw-fw', id: 'firmware',
    side: h('span', { class: 'sw-fw-links' }, h('a', { class: 'panel-link', href: '#firmware-history', onclick: (e) => { e.preventDefault(); fhTable.el.scrollIntoView({ behavior: 'smooth', block: 'start' }); } }, 'History', icon('chev-d', 'ico--sm')),
      h('a', { class: 'panel-link', href: `${ctx.link('car')}#vehicle-info` }, 'Vehicle info', icon('chev-r', 'ico--sm'))) }, fwSlot);
  let fwKey = null;
  async function loadFirmware() {
    let fw;
    try { fw = await ctx.api.get(`cars/${enc(cid)}/firmware`); } catch (e) {
      if (e?.name === 'AbortError' || !ctx.alive()) return;
      if (fwKey == null) replace(fwSlot, h('p', { class: 'warn' }, icon('warn', 'ico--sm'), ' The firmware fingerprints could not be read. Reload the page to try again.'));
      return;
    }
    if (!ctx.alive()) return;
    const key = JSON.stringify(fw);
    if (key === fwKey) return;
    fwKey = key;
    const ftz = fw.time_zone || tz;
    const day = (ms) => formatDate(ms, ftz, { year: true });
    const mods = fw.modules || [];
    replace(fwSlot,
      h('p', { class: 'note' }, 'Tesla\'s version name is not on the CAN bus, but the main modules send fingerprints of their code (checksums and code hashes). They are shown as the car sends them, in lowercase hex; a change of them is a Tesla update.'),
      h('dl', { class: 'kv sw-fw-kv' },
        kvRow('Last seen', fw.updated_ms != null ? formatDateTime(fw.updated_ms, ftz, { now: ctx.api.now() }) : 'none yet', 'clock'),
        kvRow('Updates found from them', formatNumber(fw.updates ?? 0), 'chip'),
        fw.bridge_hints?.count ? kvRow('CAN Bridge hints', `${formatNumber(fw.bridge_hints.count)} “firmware changed” lines in its logs, the last ${day(fw.bridge_hints.last_ms)}`, 'info') : null),
      h('div', { class: 'sw-fw-mods' }, mods.map((m) => h('section', { class: 'sw-fw-mod', 'aria-label': m.name },
        h('h3', { class: 'sw-fw-name' }, m.name, m.can_id != null ? h('span', { class: 'faint sw-fw-can' }, `0x${m.can_id.toString(16).toUpperCase()}`) : null),
        m.fields?.length
          ? h('dl', { class: 'sw-fw-fields' }, m.fields.map((f) => h('div', { class: 'sw-fw-row' },
            h('dt', { class: 'sw-fw-field' }, f.field),
            h('dd', { class: 'sw-fw-val' }, hexEl(f.hex),
              h('span', { class: 'sw-fw-seen' }, `since ${day(f.first_seen_ms)}`, f.history?.length > 1 ? `, ${f.history.length - 1} earlier ${f.history.length === 2 ? 'value' : 'values'}` : '')))))
          : h('p', { class: 'faint sw-fw-none' }, 'No fingerprints in this car\'s logs: this module does not send them on this car, or no log with them has arrived.')))));
  }

  /* ---------- module firmware history (#firmware-history) */
  const FH_MODS = ['vcfront', 'bms', 'dir', 'dif'];
  const fhPath = `cars/${enc(cid)}/firmware/history`;
  let fhModule = null;
  let fhKey = null;
  const fhDay = (ms) => (ms == null ? DASH : formatDate(ms, tz, { year: true }));
  const fhBtns = new Map();
  const fhSeg = h('div', { class: 'seg sw-fh-seg', role: 'group', 'aria-label': 'Which module' },
    [null, ...FH_MODS].map((m) => {
      const b = h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(m === fhModule),
        title: m ? `Only the fingerprints of ${MODULES[m] || m}` : 'Every module, with the car versions imported from TeslaMate or TeslaFi',
        onclick: () => {
          if (fhModule === m) return;
          fhModule = m;
          for (const [k, el] of fhBtns) el.setAttribute('aria-pressed', String(k === m));
          fhTable.setParams({ module: m });
        } }, m ? MODULES[m] || m : 'All');
      fhBtns.set(m, b);
      return b;
    }));
  const UNNAMED_WHY = 'no input reported a version name while this ran';
  const fhColumns = [
    {
      key: 'module', label: 'Module', title: 'The module that sent the fingerprint, or a car version imported without module fingerprints',
      render: (r) => h('span', { class: 'cell-text' },
        h('span', { class: 'sw-fh-mod' }, r.kind === 'version' ? 'Car version' : r.module_name || MODULES[r.module] || r.module,
          r.current ? pill('Current', { tone: 'live' }) : null),
        r.kind === 'version' ? h('span', { class: 'sub' }, 'Version only') : null),
      csv: { label: 'Module', value: (r) => (r.kind === 'version' ? 'Car version' : r.module_name || r.module || '') },
    },
    {
      key: 'fingerprint', label: 'Fingerprint', title: 'The module\'s main fingerprint (its app checksum), in lowercase hex as the car sends it',
      render: (r) => (r.fingerprint
        ? h('span', { class: 'cell-text' }, hexEl(r.fingerprint), r.previous ? h('span', { class: 'sub' }, 'after ', h('code', { class: 'sw-hex sw-hex--sub' }, r.previous)) : h('span', { class: 'sub' }, 'first one seen'))
        : h('span', { class: 'faint' }, 'no fingerprint')),
      csv: { label: 'Fingerprint', value: (r) => r.fingerprint || '' },
    },
    {
      key: 'seen', label: 'From – to', title: 'First and last seen (the car’s time zone); “now” for the one it runs now',
      render: (r) => h('span', { class: 'cell-text' },
        h('span', null, `${fhDay(r.first_seen_ms)} – ${r.current ? 'now' : fhDay(r.until_ms ?? r.last_seen_ms)}`),
        !r.current && r.until_ms != null && r.last_seen_ms != null ? h('span', { class: 'sub' }, `last seen ${fhDay(r.last_seen_ms)}`) : null),
      csv: { label: 'First seen (UTC)', value: (r) => iso(r.first_seen_ms) },
    },
    { key: 'last', label: 'Last seen', hidden: true, csv: { label: 'Last seen (UTC)', value: (r) => iso(r.last_seen_ms) } },
    {
      key: 'duration', label: 'Ran for', num: true, title: 'How long it ran: from first to last seen',
      render: (r) => formatDuration(r.duration_ms),
      csv: { label: 'Ran for (days)', value: (r) => (r.duration_ms == null ? '' : Math.round(r.duration_ms / 864000) / 100) },
    },
    {
      key: 'version', label: 'Came with', cls: 'sw-col-ver', title: 'The name of the version it came with: read from a live input, imported, typed by an admin, or the same fingerprints as a named update. Never guessed: a version last known before it is shown as "probably", apart.',
      render: (r) => {
        if (r.version) return h('span', { class: 'sw-ver', title: SOURCE[r.version_source ?? r.update?.version_source]?.[1] ?? null }, r.version);
        if (r.kind !== 'module') return h('span', { class: 'faint' }, 'unnamed');
        const nameBtn = admin ? h('button', { class: 'btn btn--sm btn--ghost sw-edit', type: 'button', onclick: () => nameSegment(r), title: 'Type the version this stretch of fingerprints ran' },
          icon('settings', 'ico--sm'), 'Name this version') : null;
        if (r.name_inferred) {
          return h('span', { class: 'cell-text' },
            h('span', { class: 'sw-ver sw-ver--inferred', title: 'No input named this stretch. The last version known before it (an import or an input) has no update between it and the first fingerprints seen, and nothing contradicts it, so it is probably still this one.' },
              `probably ${r.name_inferred.version} (${r.name_inferred.from})`),
            nameBtn);
        }
        return h('span', { class: 'cell-text' },
          h('span', { class: 'faint', title: UNNAMED_WHY }, 'unnamed'),
          h('span', { class: 'sub' }, UNNAMED_WHY),
          nameBtn);
      },
      csv: { label: 'Came with version', value: (r) => r.version || '' },
    },
    {
      key: 'probably', label: 'Probably', hidden: true, csv: { label: 'Probably (last known before)', value: (r) => r.name_inferred?.version || '' },
    },
  ];
  const fhStrip = h('div', { class: 'sw-fh-strip', 'aria-busy': 'true' }, h('span', { class: 'skel sw-fh-skel' }));
  const fhTable = dataTable(ctx, {
    title: 'Module firmware history', icon: 'chip', path: fhPath, params: {}, limit: 50, cls: 'sw-fh', csvName: 'module firmware history', box: 'Firmware fingerprints', autoMore: true,
    csv: { all: true, columns: csvOf(fhColumns) },
    columns: fhColumns.filter((c) => !c.hidden),
    rowClass: (r) => [r.kind === 'version' && 'sw-fh-vrow', r.current && 'is-current'].filter(Boolean).join(' ') || null,
    rowTitle: (r) => r.source_label || null,
    note: 'Every fingerprint each module has had, newest first, and the version name it came with when someone named it. All also shows the car versions an import brought without module fingerprints.',
    empty: () => emptyState({ icon: 'chip', title: fhModule ? `No fingerprints of ${MODULES[fhModule] || fhModule} yet` : 'No firmware history yet',
      text: 'Fingerprints show once a log with them arrives from the car; imported versions show once a TeslaMate or TeslaFi history is imported.' }),
    onPage: (pg) => { if (ctx.alive()) paintStrip(pg); },
    onError: () => { if (fhKey == null) replace(fhStrip, h('p', { class: 'warn' }, icon('warn', 'ico--sm'), ' The firmware history could not be read.')); fhStrip.removeAttribute('aria-busy'); },
  });
  fhTable.el.id = 'firmware-history';
  fhTable.el.insertBefore(h('div', { class: 'sw-fh-top' }, fhSeg, fhStrip), fhTable.el.querySelector('.tbl-scroll'));

  const segName = (s) => s.version || (s.name_inferred ? `probably ${s.name_inferred.version}` : 'unnamed');

  function paintStrip(pg) {
    const key = JSON.stringify([pg.modules, pg.spans, pg.range]);
    if (key === fhKey) return;
    fhKey = key;
    fhStrip.removeAttribute('aria-busy');
    const mods = pg.modules?.length ? pg.modules : FH_MODS.map((m) => ({ id: m, name: MODULES[m] }));
    for (const m of mods) {
      const b = fhBtns.get(m.id);
      if (b) replace(b, m.name || MODULES[m.id] || m.id, m.fingerprints != null ? h('span', { class: 'count' }, formatNumber(m.fingerprints)) : null);
    }
    const from = pg.range?.from_ms, to = pg.range?.to_ms;
    const spans = pg.spans || [];
    if (from == null || to == null || !spans.length) {
      replace(fhStrip, h('p', { class: 'faint sw-fw-none' }, 'No fingerprints yet, so there is nothing to draw over time.'));
      return;
    }
    const len = Math.max(1, to - from);
    const pc = (ms) => `${(Math.min(100, Math.max(0, ((ms - from) / len) * 100))).toFixed(3)}%`;
    const lanes = mods.map((m) => {
      const own = spans.filter((s) => s.module === m.id);
      const name = m.name || MODULES[m.id] || m.id;
      const track = h('div', { class: 'sw-fh-track', role: 'img',
        'aria-label': own.length ? `${name}: ${own.map((s) => `${segName(s)} from ${fhDay(s.from_ms)}`).join(', ')}` : `${name}: no fingerprints` },
      own.map((s, i) => {
        const seg = h('span', { class: ['sw-fh-seg-bar', i % 2 && 'is-alt', s.current && 'is-current', !s.version && !s.name_inferred && 'is-unnamed', !s.version && s.name_inferred && 'is-inferred'],
          title: `${name}: ${s.fingerprint} · ${segName(s)}${s.version || s.name_inferred ? '' : ` (${UNNAMED_WHY})`} · ${fhDay(s.from_ms)} – ${s.current ? 'now' : fhDay(s.to_ms)}` },
        h('span', { class: 'sw-fh-seg-text' }, segName(s)));
        setVars(seg, { '--x': pc(s.from_ms), '--w': `${Math.max(0, ((Math.min(to, s.to_ms ?? to) - Math.max(from, s.from_ms)) / len) * 100).toFixed(3)}%` });
        return seg;
      }),
      own.slice(1).map((s) => {
        const t = h('i', { class: 'sw-fh-tick', title: `Changed ${fhDay(s.from_ms)}` });
        setVars(t, { '--x': pc(s.from_ms) });
        return t;
      }));
      return h('div', { class: 'sw-fh-lane' },
        h('span', { class: 'sw-fh-lane-name' }, name, m.current ? h('code', { class: 'sw-hex sw-hex--sub' }, m.current) : null),
        own.length ? track : h('span', { class: 'faint sw-fh-lane-none' }, 'never seen'));
    });
    replace(fhStrip,
      h('h3', { class: 'sw-fh-strip-title metric-label' }, 'Versions over time'),
      h('div', { class: 'sw-fh-lanes' }, lanes),
      h('div', { class: 'sw-fh-axis', 'aria-hidden': 'true' }, h('span', null, fhDay(from)), h('span', null, fhDay(to))));
  }

  replace(ctx.view, root);
  root.append(head, filters, summary, fpPanel, table.el, fwPanel, fhTable.el, fleetTable.el);

  /* ---------- naming a module's fingerprint segment (admin; the typed versions of the updates, no password) */
  async function nameSegment(r) {
    const input = field({
      label: 'Version', name: 'version', path: 'version', value: r.name_inferred?.version || '',
      hint: 'As the car’s screen shows it, such as 2026.38.4.' + (r.name_inferred ? ' It is filled in with the version last known before this stretch: keep it only if you know it is right.' : ''),
      attrs: { pattern: '\\d{4}\\.\\d{1,2}(\\.\\d{1,3}){0,2}', autocomplete: 'off', spellcheck: 'false' },
    });
    const f = form({
      fields: [input], submit: 'Save',
      async onSubmit(values) {
        const v = String(values.version || '').trim();
        if (!VERSION_RX.test(v)) { input.setError('A version looks like 2026.38.4: the year, a dot and one to three numbers.'); return; }
        await ctx.api.patch(`${fhPath}/${enc(r.id)}`, { version: v });
        flash(`Saved: ${r.module_name || 'this module'} ran ${v} from ${fhDay(r.first_seen_ms)}.`);
        f.busy(false);
        f.el.closest('dialog')?.close();
        echoUntil = Date.now() + 5000;
        reloadAll(false);
      },
    });
    await openDialog({
      title: `Name ${r.module_name || 'this module'}’s version`,
      body: [h('p', { class: 'note' }, `Fingerprint ${r.fingerprint}, seen ${fhDay(r.first_seen_ms)} to ${r.current ? 'now' : fhDay(r.until_ms ?? r.last_seen_ms)}. ${r.update ? 'It names the update this came with, and every update to the same fingerprints.' : 'No update came with it, so the name is kept for this first stretch alone.'}`), f.el],
      actions: [],
    });
  }

  /* ---------- typing a version (admin) */
  async function editVersion(r) {
    const input = field({
      label: 'Version', name: 'version', path: 'version', value: r.version || '',
      hint: 'As the car’s screen shows it, such as 2026.38.4. Leave it empty to forget a typed version. Other cars on the same build get it too.',
      attrs: { pattern: '\\d{4}\\.\\d{1,2}(\\.\\d{1,3}){0,2}', autocomplete: 'off', spellcheck: 'false' },
    });
    const f = form({
      fields: [input], submit: 'Save',
      async onSubmit(values) {
        const v = String(values.version || '').trim();
        await ctx.api.patch(`${path}/${enc(r.id)}`, { version: v || null });
        flash(v ? `Saved: this update installed ${v}.` : 'The typed version is forgotten.');
        f.el.closest('dialog')?.close();
        echoUntil = Date.now() + 5000; // the server's "rows" event for this change would load the lists a second time
        reloadAll(false);
      },
    });
    await openDialog({
      title: `Update of ${formatDate(r.start_ms, tz, { year: true })}`,
      body: [h('p', { class: 'note' }, r.changed_modules?.length ? `Modules that changed: ${moduleNames(r.changed_modules).join(', ')}.` : 'No module list for this update.'), f.el],
      actions: [],
    });
  }

  let echoUntil = 0;
  async function reloadAll(say) {
    reloadBtn.disabled = true;
    try { await Promise.all([table.reload(), fleetTable.reload(), loadFirmware(), fhTable.reload()]); await fpPaint; if (say && ctx.alive()) flash('Loaded again.', { kind: 'info' }); } finally { reloadBtn.disabled = false; }
  }

  ctx.events('rows', (ev) => {
    const d = ev?.data || {};
    if (!(d.kinds || []).includes('updates')) return;
    if (Date.now() < echoUntil) { echoUntil = 0; return; } // the echo of a version just saved here
    if (d.car === cid) { table.reload(null, { quiet: true }); loadFirmware(); fhTable.reload(null, { quiet: true }); }
    fleetTable.reload(null, { quiet: true });
  });

  // both lists are part of the first render: the page does not change height once it says it is ready
  await Promise.all([table.ready, fleetTable.ready, loadFirmware(), fhTable.ready]);
  await fpPaint;
  if (!ctx.alive()) return;
  if (!painted) paintSummary(null, []);
  ctx.ready();
}
