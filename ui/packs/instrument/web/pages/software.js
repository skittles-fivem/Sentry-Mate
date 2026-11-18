// Software (/cars/{car}/software): the version a car runs now, its update history in the period, and
// the versions across the whole fleet.
// - Updates come from the car itself (its update status and the build ids of its modules) or from an
//   imported TeslaMate history. A version is only ever what someone said: imported, typed by the owner,
//   or learned from another update with the same module builds. An update seen only as changed module
//   builds says so ("build changed", with the modules) and never gets a guessed version.
// - GET /cars/{car}/updates (period, newest first, paged; summary in each page), PATCH .../updates/{id}
//   (admin: type or forget a version), GET /fleet/versions (every car, searched and paged by cursor).
// - Live: an update that starts or ends ("rows" events with kind "updates") loads the lists again.

import * as state from '../shared/state.js';
import { h, replace, cover } from '../shared/dom.js';
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

export const id = 'software';
export const css = true;

const enc = encodeURIComponent;
const VERSION_RX = /^\d{4}\.\d{1,2}(\.\d{1,3}){0,2}$/;

// where a version came from
const SOURCE = {
  teslamate: ['Imported', 'Imported from TeslaMate with the version text it recorded.'],
  owner: ['Typed by the owner', 'An admin typed this version in Sentry Mate.'],
  build_match: ['Same build', 'Another update with exactly the same module builds has this version.'],
};
// how an update was found
const DETECTED = {
  ota_status: ['Update status', 'The car reported that it was installing an update.'],
  build_change: ['Module builds changed', 'The build ids its modules send on the vehicle bus changed.'],
  teslamate: ['Imported', 'From the imported TeslaMate history.'],
};
// the modules whose build ids are compared
const MODULES = {
  bms: 'Battery management', vcfront: 'Front body controller', di_front: 'Front motor', di_rear: 'Rear motor', fc: 'Fast-charge controller',
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
        icon('car', 'ico--sm'), car.name, icon('chev-d', 'ico--sm')),
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
        kvRow(changed ? 'Source of that name' : 'Source', src ? h('span', { title: src[1] }, src[0]) : 'Nobody has said yet', 'info'),
        changed
          ? kvRow('Named version ran', `${since ? `from ${since} ` : ''}to ${at(sm.build_changed_since)}`, 'clock')
          : kvRow('Running since', since || DASH, 'clock'),
        changed ? kvRow('Software changed', at(sm.build_changed_since), 'warn') : null,
        installing ? kvRow('Installing since', at(installing.start_ms), 'clock') : null),
      h('p', { class: 'explain' }, sm.build_changed_since != null
        ? `The module builds changed after ${v || 'the last known version'} was recorded, so the car now runs a version nobody has named. ${admin ? 'Type it on the newest update below.' : 'An admin can type it on the newest update below.'}`
        : v ? `${src ? src[1] : ''} The car does not send its version text; Sentry Mate never guesses one.`
          : 'The car does not send its version text. It is known once a TeslaMate history is imported, an admin types it, or another car on the same build has one.'));
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
    ]));
  }
  const kvRow = (label, value, ic) => h('div', { class: 'kv-row' }, h('dt', null, icon(ic, 'ico--sm'), label), h('dd', null, value));

  /* ---------- update history */
  const timelineHref = (r) => state.buildUrl('timeline', { car: cid, period: { from: r.start_ms - HOUR, to: (r.end_ms ?? ctx.api.now()) + HOUR } });
  const onVersion = (r) => ({ from: r.end_ms ?? r.start_ms, to: r.until_ms ?? ctx.api.now() });
  const notesHref = (v) => `https://www.notateslaapp.com/software-updates/version/${enc(v)}/release-notes`;

  function versionCell(r) {
    const src = SOURCE[r.version_source];
    const main = r.version
      ? h('span', { class: 'sw-ver' }, notesOn && VERSION_RX.test(r.version)
        ? h('a', { class: 'link', href: notesHref(r.version), target: '_blank', rel: 'noopener noreferrer external', title: 'Release notes of this version on an outside site (opens in a new tab)' }, r.version, ' ', icon('ext', 'ico--sm'))
        : r.version,
      src ? tag(src[0], 'plain') : null)
      : h('span', { class: 'sw-ver is-unknown' }, 'Build changed', tag('Version not known', 'warm'));
    const sub = r.version
      ? (r.changed_modules?.length ? `Changed: ${moduleNames(r.changed_modules).join(', ')}` : null)
      : r.changed_modules?.length ? `Changed: ${moduleNames(r.changed_modules).join(', ')}` : 'No module list';
    return h('span', { class: 'cell-text' }, main, sub ? h('span', { class: 'sub sw-mods' }, sub) : null,
      admin && r.source === 'can' ? h('button', { class: 'btn btn--sm btn--ghost sw-edit', type: 'button', onclick: () => editVersion(r), title: 'Type the version this update installed' },
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
      key: 'version', label: 'Version', cls: 'sw-col-ver', cover: 'updates.table.version', title: 'Imported, typed by the owner, or the same build as an update with a known version. Never guessed.',
      render: versionCell,
      csv: { label: 'Version', value: (r) => r.version || '' },
    },
    { key: 'modules', label: 'Changed modules', hidden: true, csv: { label: 'Changed modules', value: (r) => moduleNames(r.changed_modules).join('; ') } },
    { key: 'vsource', label: 'Version from', hidden: true, csv: { label: 'Version from', value: (r) => (SOURCE[r.version_source] ? SOURCE[r.version_source][0] : '') } },
    {
      key: 'source', label: 'Found by', title: 'How Sentry Mate knows about this update',
      render: (r) => { const d = DETECTED[r.detected_by] || [r.detected_by, '']; return h('span', { class: 'cell-text', title: d[1] }, h('span', null, d[0]), h('span', { class: 'sub' }, r.source === 'teslamate' ? 'TeslaMate row' : 'From the car')); },
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
    !notesOn ? h('a', { class: 'link', href: state.buildUrl('settings-display') }, 'Settings: Display and units') : null, !notesOn ? '.' : null));

  let painted = false;
  const table = dataTable(ctx, {
    title: 'Update history', icon: 'chip', path, params: ctx.params, limit: 50, cls: 'sw-table', cover: 'updates.table',
    csvName: 'software updates', csv: { all: true, columns: csvOf(columns) },
    columns: columns.filter((c) => !c.hidden),
    note: 'Newest first. Charges and range are what the car did while on each version: from the end of the update to the start of the next one (or now).',
    side: notesNote,
    empty: () => emptyState({
      icon: 'chip', title: 'No updates in this period',
      text: period.preset === 'all' ? `Nothing has been recorded for ${car.name} yet: an update shows once the car installs one while recording, or with an imported TeslaMate history.`
        : 'No update started in this period. Choose a longer period (All time shows every update).',
    }),
    onPage: (pg) => {
      if (!ctx.alive()) return;
      painted = true;
      paintSummary(pg.summary, table.rows());
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
        : r.version ? h('span', { class: 'sw-ver' }, r.version, SOURCE[r.version_source] ? tag(SOURCE[r.version_source][0], 'plain') : null)
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

  replace(ctx.view, root);
  root.append(head, filters, summary, table.el, fleetTable.el);

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
    try { await Promise.all([table.reload(), fleetTable.reload()]); if (say && ctx.alive()) flash('Loaded again.', { kind: 'info' }); } finally { reloadBtn.disabled = false; }
  }

  ctx.events('rows', (ev) => {
    const d = ev?.data || {};
    if (!(d.kinds || []).includes('updates')) return;
    if (Date.now() < echoUntil) { echoUntil = 0; return; } // the echo of a version just saved here
    if (d.car === cid) table.reload(null, { quiet: true });
    fleetTable.reload(null, { quiet: true });
  });

  await table.ready;
  if (!ctx.alive()) return;
  if (!painted) paintSummary(null, []);
  ctx.ready();
}
