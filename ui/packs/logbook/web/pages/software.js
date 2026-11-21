// Sentry Mate, "Logbook" UI pack: Software (/cars/{car}/software).
//
// The version a car runs now, its update history in the period, and the version every car runs. Honest
// about what is known: the car never sends its version text, so a version is only ever what someone said
// (imported from TeslaMate, typed by an admin, or learned from another update with exactly the same module
// builds). An update seen only as changed module builds says "build changed" with the modules, never a guess.
// Data:
//   GET /cars/{car}/updates            the history in the period (newest first, paged, CSV from the server),
//                                      with a summary in every page (count, median gap, the version now)
//   PATCH /cars/{car}/updates/{id}     admin: type or forget the version an update installed
//   GET /fleet/versions                every car with its version (searched, paged by cursor, CSV)
//   GET /settings                      whether the owner allows release-notes links (outside_links)
// Live: an update that starts or ends ("rows" events naming "updates") loads both lists again.

import { h, replace } from '../shared/dom.js';
import { formatDate, formatTime, formatDateTime, formatDuration, periodLabel, HOUR, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, icon, empty, flag, button, sep, loadingBlock } from '../core/ui.js';
import { sourceMark, hasSmo, isImported } from '../core/sources.js';
import { dataTable } from '../core/table.js';
import { openDialog } from '../core/dialog.js';
import { field, form } from '../core/form.js';
import { zoneLabel } from '../core/format.js';

export const id = 'software';
export const css = true;
export const title = () => 'Software';

const VERSION_RX = /^\d{4}\.\d{1,2}(\.\d{1,3}){0,2}$/;

/** Where a version came from: [short name, the sentence that explains it]. */
const SOURCE = {
  teslamate: ['Imported', 'Imported from TeslaMate with the version text it recorded.'],
  tesla_api: ['Tesla API', 'The Tesla API named the version the car runs.'],
  owner: ['Typed by an admin', 'An admin typed this version from the car’s screen.'],
  build_match: ['Same build', 'Another update with exactly the same module builds has this version.'],
};
/** How an update was found. */
const DETECTED = {
  ota_status: ['Update status', 'The car reported that it was installing an update.'],
  build_change: ['Module builds changed', 'The build ids its modules send on the vehicle bus changed.'],
  teslamate: ['Imported', 'A row of the imported TeslaMate history.'],
  tesla_api: ['Tesla API', 'The Tesla API reported a new version.'],
};
/** The modules whose build ids are compared. */
const MODULES = {
  bms: 'Battery management', vcfront: 'Front body controller', di_front: 'Front motor', di_rear: 'Rear motor', fc: 'Fast-charge controller',
};
const moduleNames = (list) => (list || []).map((m) => MODULES[m] || m).join(', ');

/** A long gap in words beside the short duration: 'about 7.8 months', '23 days'. */
function gapWords(ms) {
  if (ms == null) return null;
  const days = ms / DAY;
  if (days >= 60) return `about ${formatNumber(days / 30.44, { maximumFractionDigits: 1 })} months`;
  if (days >= 2) return `${formatNumber(Math.floor(days))} days`;
  return null;
}

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const now = api.now();
  const admin = !!(ctx.me && ctx.me.role === 'admin');
  const path = api.apiPath`cars/${cid}/updates`;

  // The owner's choice about outside links (off when it cannot be read).
  const settings = await ctx.api.get('settings', null, { maxAgeMs: 300000 }).catch(() => null);
  if (!ctx.alive()) return {};
  const notesOn = !!(settings && settings.outside_links && settings.outside_links.release_notes);

  /* ---------------------------------------------------------------- head */

  const asOf = h('p', { class: 'sw-asof' });
  const drawAsOf = () => replace(asOf, 'As of ', formatTime(api.now(), tz), ' · the lists also load again by themselves when an update starts or ends.');
  drawAsOf();
  const reloadBtn = button('Load again', {
    iconName: 'reset', cls: 'button-small', cover: 'updates.control.refresh',
    title: 'Load the figures and both lists again now.',
    onclick: () => reloadAll(true),
  });
  ctx.main.append(pageHead({
    eyebrow: h('span', { class: 'sw-scope' }, 'Reports', sep(), h('span', { class: 'sw-car-name', cover: 'updates.section.car_header' }, car.name || cid)),
    title: 'Software',
    deck: 'Which version the car runs and when it changed. The car does not send its version text, so a version shows only when someone said what it is; otherwise the page says the software changed and which modules changed.',
    aside: h('div', { class: 'sw-head-aside' }, reloadBtn, asOf),
    cover: 'shots.updates',
  }));

  /* ---------------------------------------------------------------- filters */

  const periodLine = h('div', { class: 'sw-filter', cover: 'updates.filter.time_range' },
    h('span', { class: 'field-label' }, 'Period'),
    h('p', { class: 'sw-filter-value' }, periodLabel(ctx.period, tz, now)),
    h('p', { class: 'field-hint' }, 'An update belongs to the period it started in. Times in ', zoneLabel(tz), '. The version now does not depend on the period.'),
    button('Change the period', { cls: 'button-small', iconName: 'calendar', onclick: () => document.querySelector('.period-trigger')?.click() }));
  const carLine = h('div', { class: 'sw-filter', cover: 'updates.filter.car' },
    h('span', { class: 'field-label' }, 'Car'),
    h('p', { class: 'sw-filter-value' }, car.name || cid),
    h('p', { class: 'field-hint' }, 'One car’s history here; every car’s version is in one list below, not a page per car.'),
    h('div', { class: 'sw-filter-actions' },
      button('Choose another car', { cls: 'button-small', iconName: 'search', onclick: () => document.querySelector('.switcher-trigger')?.click() }),
      h('a', { class: 'link-arrow', href: '#sw-fleet' }, 'Versions of every car', icon('arrowRight'))));
  ctx.main.append(h('section', { class: 'sw-filters', 'aria-label': 'What the page covers' }, periodLine, carLine));

  /* ---------------------------------------------------------------- summary */

  const nowBox = h('div', { class: 'sw-now-box' }, loadingBlock(3));
  const figsBox = h('div', { class: 'sw-figs-box' });
  ctx.main.append(h('section', { class: 'block sw-summary', 'aria-labelledby': 'sw-sum-title' },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'sw-sum-title' }, 'Summary', h('span', { class: 'label-note' }, 'the version now, and the updates in the period'))),
    h('div', { class: 'sw-sum-grid' },
      nowBox,
      h('div', { class: 'sw-sum-side' }, figsBox,
        h('p', { class: 'note sw-notify', cover: 'projects.telegram-bot' }, icon('info'),
          h('span', null, 'Want a message when the software changes? Sentry Mate publishes a “software changed” event (MQTT and the events list) as soon as an update ends; ',
            h('a', { href: state.buildUrl('settings-integrations') }, 'Settings: Integrations'),
            ' connects it to a chat or home automation. A waiting update is not on the CAN bus, so there is no “update available” message.'))))));

  let rowsSeen = [];
  // The car's newest update of all time (not only the period's), for the "Type the version" button of the summary.
  let newestEver = null;
  async function findNewest() {
    if (!admin) return;
    try {
      const r = await ctx.api.get(path, { limit: 1 });
      const u = r && r.items && r.items[0];
      newestEver = u && u.until_ms == null && u.end_ms != null && hasSmo(u) ? u : null;
    } catch { newestEver = null; }
  }
  function drawSummary(sm, failed = false) {
    sm = sm || {};
    const installing = rowsSeen.find((r) => r.end_ms == null);
    const newest = rowsSeen.find((r) => r.until_ms == null && r.end_ms != null && hasSmo(r)) || newestEver;
    const changed = sm.build_changed_since != null;
    const last = sm.current_version; // after a change, the version the car ran before it (no longer what it runs)
    const v = changed ? null : last;
    const src = SOURCE[sm.current_version_source];
    const lastFrom = sm.current_version_since_ms != null ? formatDate(sm.current_version_since_ms, tz, { now, year: true }) : null;
    replace(nowBox,
      h('p', { class: 'sw-now-name' }, 'Version now'),
      h('p', { class: ['sw-now-version', { 'is-unknown': !v }] }, failed ? 'Could not load' : v || (changed ? 'Changed, not named yet' : 'Not known')),
      h('div', { class: 'sw-now-flags' },
        installing ? flag('Installing now', { kind: 'warn' }) : null,
        changed ? flag('Software changed', { kind: 'warn' }) : null,
        v && src ? flag(src[0], { title: src[1] }) : null),
      failed
        ? h('p', { class: 'note' }, 'The update history could not be loaded, so the version and the figures are not shown. The list of every car below is loaded on its own.')
        : h('dl', { class: 'sw-facts' },
          changed
            ? fact('Last known version', last
              ? [last, src ? ` (${src[0].toLowerCase()})` : '', ', ran ', lastFrom ? `from ${lastFrom} ` : '', `to ${formatDate(sm.build_changed_since, tz, { now, year: true })}`]
              : 'none: no version was ever named for this car')
            : fact('Where it came from', src ? src[1] : 'Nobody has said yet: not imported, not typed, and no other car on the same build.'),
          changed
            ? fact('Running since', [formatDateTime(sm.build_changed_since, tz, { now }), h('span', { class: 'sw-faint' }, ' (when the module builds changed)')])
            : fact('Running since', sm.current_version_since_ms != null ? formatDateTime(sm.current_version_since_ms, tz, { now }) : 'not known'),
          installing ? fact('Installing since', formatDateTime(installing.start_ms, tz, { now })) : null),
      failed ? null : h('p', { class: 'note' }, changed
        ? `The module builds changed after ${last || 'the last known version'} was recorded, so the car now runs a version nobody has named. ${admin ? (newest ? 'Type it from the car’s screen.' : 'Type it from the car’s screen, on the newest update in the history below.') : 'An admin can type it from the car’s screen.'}`
        : v ? 'Sentry Mate never guesses a version; this one is what is written above.'
          : 'The version is known once a TeslaMate history is imported, an admin types it, or another car on the same build has one.'),
      failed ? button('Try again', { cls: 'button-small', iconName: 'reset', onclick: () => reloadAll(false).catch(() => {}) }) : null,
      !failed && admin && changed && newest ? button('Type the version', { cls: 'button-small', iconName: 'settings', onclick: () => editVersion(newest) }) : null);
    const n = sm.count ?? 0;
    replace(figsBox, h('div', { class: 'figs sw-figs' },
      figure('Updates', q(sm.count ?? null, 'count'),
        'Updates that started in the period: seen by the car (its update status or changed module builds) or imported.', { cover: 'updates.stat.count' }),
      figure('Typical time between updates',
        sm.median_gap_ms != null ? h('span', { class: 'sw-gap' }, formatDuration(sm.median_gap_ms), gapWords(sm.median_gap_ms) ? h('span', { class: 'sw-gap-words' }, gapWords(sm.median_gap_ms)) : null)
          : q(null, 'duration', { why: failed ? 'not loaded' : n === 0 ? 'no update in the period' : 'the first update recorded has no previous one' }),
        'The median of the gaps before each update in the period: from the end of the update before (which may be outside the period) to its start.', { cover: 'updates.stat.median_gap' })));
  }
  const fact = (name, value) => h('div', { class: 'sw-fact' }, h('dt', null, name), h('dd', null, value));

  /* ---------------------------------------------------------------- update history */

  const timelineHref = (r) => state.buildUrl('timeline', { car: cid, period: { from: r.start_ms - HOUR, to: (r.end_ms ?? api.now()) + HOUR } });
  const onVersion = (r) => ({ from: r.end_ms ?? r.start_ms, to: r.until_ms ?? api.now() });
  const notesHref = (v) => `https://www.notateslaapp.com/software-updates/version/${encodeURIComponent(v)}/release-notes`;

  function versionCell(r) {
    const src = SOURCE[r.version_source];
    const main = r.version
      ? h('span', { class: 'sw-ver' },
        notesOn && VERSION_RX.test(r.version)
          ? h('a', { href: notesHref(r.version), target: '_blank', rel: 'external noopener noreferrer', title: 'Release notes of this version on an outside site (opens in a new tab)' }, r.version)
          : h('span', { class: 'sw-ver-text' }, r.version),
        src ? flag(src[0], { title: src[1] }) : null)
      : h('span', { class: 'sw-ver is-unknown' }, h('span', { class: 'sw-ver-text' }, 'Build changed'), flag('version not known', { kind: 'warn' }));
    const mods = r.changed_modules && r.changed_modules.length ? `Changed: ${moduleNames(r.changed_modules)}` : isImported(r) ? 'Imported row: no module list' : null;
    return h('div', { class: 'cell-main sw-ver-cell' }, main,
      mods ? h('span', { class: 'cell-sub' }, mods) : null,
      admin && hasSmo(r) && r.end_ms != null
        ? h('button', { type: 'button', class: 'sw-edit', onclick: () => editVersion(r), title: 'Type the version this update installed, from the car’s screen' },
          icon('settings'), r.version && r.version_source === 'owner' ? 'Change the version' : 'Type the version')
        : null);
  }

  const columns = [
    {
      key: 'start', label: 'Started', th: true, cover: 'updates.table.date', hint: 'When the update started, in the car’s time zone. Opens the timeline around it.',
      cell: (r) => h('div', { class: 'cell-main' },
        h('a', { href: timelineHref(r), title: 'The timeline around this update' }, formatDate(r.start_ms, tz, { now, year: true })),
        h('span', { class: 'cell-sub' }, formatTime(r.start_ms, tz))),
      value: (r) => r.start_ms,
    },
    {
      key: 'duration', label: 'Took', num: true, cover: 'updates.table.duration', hint: 'From the start of the update to its end (the car’s own update status, or the module builds changing).',
      cell: (r) => (r.end_ms == null
        ? h('div', { class: 'cell-main' }, flag('Installing', { kind: 'warn' }), h('span', { class: 'cell-sub' }, `${formatDuration(r.duration_ms)} so far`))
        : h('div', { class: 'cell-main' }, formatDuration(r.duration_ms), h('span', { class: 'cell-sub' }, `ended ${formatTime(r.end_ms, tz)}`))),
      value: (r) => r.duration_ms,
    },
    {
      key: 'since', label: 'After the previous', num: true, cover: 'updates.table.since_previous', hint: 'From the end of the update before to the start of this one, exact to the minute (not calendar dates).',
      cell: (r) => (r.since_previous_ms == null
        ? h('div', { class: 'cell-main' }, '—', h('span', { class: 'cell-sub' }, 'first one recorded'))
        : h('div', { class: 'cell-main' }, formatDuration(r.since_previous_ms), gapWords(r.since_previous_ms) ? h('span', { class: 'cell-sub' }, gapWords(r.since_previous_ms)) : null)),
      value: (r) => r.since_previous_ms,
    },
    {
      key: 'version', label: 'Version', cover: ['updates.table.version', 'updates.link.release_notes'], hint: 'Imported, typed by an admin, or the same build as an update with a known version. Never guessed.',
      cell: versionCell, value: (r) => r.version,
    },
    {
      key: 'found', label: 'Found by', hint: 'How Sentry Mate knows about this update, and where the row comes from.',
      cell: (r) => { const d = DETECTED[r.detected_by] || [r.detected_by, '']; return h('div', { class: 'cell-main', title: d[1] }, d[0], h('span', { class: 'cell-sub' }, isImported(r) ? [flag('Imported', { kind: 'estimated' }), ' '] : null, sourceMark(r, { short: true }))); },
      value: (r) => r.detected_by,
    },
    {
      key: 'charges', label: 'Charges on it', num: true, cover: 'updates.table.charge_count', hint: 'Charges that started while this version was installed, by exact time (no hour buckets). Opens them.',
      cell: (r) => h('div', { class: 'cell-main' },
        r.charges ? h('a', { href: state.buildUrl('charges', { car: cid, period: onVersion(r) }), title: 'The charges while on this version' }, formatNumber(r.charges)) : formatNumber(r.charges ?? 0),
        h('span', { class: 'cell-sub' }, r.until_ms == null ? 'until now' : `until ${formatDate(r.until_ms, tz, { now, weekday: false, year: true })}`)),
      value: (r) => r.charges,
    },
    {
      key: 'range', label: 'Range at 100 %', num: true, cover: 'updates.table.avg_range', hint: 'The range a full battery gives while on this version: the car’s range scaled to 100 %.',
      cell: (r) => q(r.full_range_km, 'distance', { why: 'no reading' }), value: (r) => r.full_range_km,
    },
    {
      key: 'pack', label: 'Full pack', num: true, hint: 'Full-pack energy the battery management system reported while on this version. Imported rows have no such reading.',
      cell: (r) => (r.full_pack_kwh == null ? h('span', { class: 'sw-faint' }, isImported(r) ? 'not measured (imported)' : 'no reading') : q(r.full_pack_kwh, 'energy')),
      value: (r) => r.full_pack_kwh,
    },
  ];

  let lastSummary = null;
  const table = dataTable(ctx, {
    path, params: ctx.params, limit: 50, columns, cls: 'sw-table', cover: 'updates.table', label: 'Update history',
    itemName: ['update', 'updates'],
    caption: 'Newest first. Charges and range are what the car did while on each version: from the end of the update to the start of the next one, or now.',
    empty: () => empty({
      title: 'No update in this period.',
      text: ctx.period && ctx.period.preset === 'all'
        ? `Nothing is recorded for ${car.name || cid} yet: an update shows once the car installs one while recording, or with an imported TeslaMate history.`
        : 'No update started in this period. Choose a longer period with the period picker (All time shows every update).',
    }),
    onRows: (_items, pg) => { rowsSeen = table.items || []; lastSummary = pg && pg.summary; drawSummary(lastSummary); },
  });
  const notesNote = h('p', { class: 'note sw-notes' }, icon('info'),
    h('span', null, notesOn
      ? 'A known version links to its release notes on an outside site, opened in a new tab by your own click; this server sends nothing.'
      : ['Release-notes links are off, so nothing leaves this server. ', admin ? 'Turn them on in ' : 'An admin can turn them on in ',
        h('a', { href: state.buildUrl('settings-display') }, 'Settings: Display and units'), '.']));
  ctx.main.append(h('section', { class: 'block sw-history', 'aria-labelledby': 'sw-hist-title' },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'sw-hist-title' }, 'Update history', h('span', { class: 'label-note' }, `${car.name || cid}, ${periodLabel(ctx.period, tz, now)}`))),
    notesNote, table.el));

  /* ---------------------------------------------------------------- versions of every car */

  const search = h('input', { class: 'field-input', id: 'sw-search', type: 'search', value: ctx.query.q || '', placeholder: 'Car name or version', autocomplete: 'off', spellcheck: 'false' });
  const fleetTable = dataTable(ctx, {
    path: 'fleet/versions', params: ctx.query.q ? { search: ctx.query.q } : {}, limit: 50, cls: 'sw-fleet-table', label: 'Versions of every car',
    itemName: ['car', 'cars'],
    caption: 'One row per car you may see. Search by name or version; Show more pages through any number of cars. Dates are in each car’s own time zone (point at one to see which).',
    rowAttrs: (r) => (r.car === cid ? { class: 'is-current' } : null),
    columns: [
      {
        key: 'car', label: 'Car', th: true,
        cell: (r) => h('div', { class: 'cell-main' }, h('a', { href: state.buildUrl('software', { car: r.car, period: ctx.state.period }), title: 'This car’s software page' }, r.name || r.car),
          r.car === cid ? h('span', { class: 'cell-sub' }, 'shown above') : null),
        value: (r) => r.name || r.car,
      },
      {
        key: 'version', label: 'Version now',
        hint: 'The version each car runs now. After its module builds changed, the version is not known until someone names it; the one it ran before is shown below it.',
        cell: (r) => (r.version && r.build_changed_since == null
          ? h('span', { class: 'sw-ver' }, h('span', { class: 'sw-ver-text' }, r.version), SOURCE[r.version_source] ? flag(SOURCE[r.version_source][0], { title: SOURCE[r.version_source][1] }) : null)
          : h('div', { class: 'cell-main' },
            h('span', { class: 'sw-ver is-unknown' }, h('span', { class: 'sw-ver-text' }, r.build_changed_since != null ? 'Changed, not named yet' : 'Not known')),
            r.version ? h('span', { class: 'cell-sub' }, `last known ${r.version}${SOURCE[r.version_source] ? ` (${SOURCE[r.version_source][0].toLowerCase()})` : ''}`) : null)),
        value: (r) => r.version,
      },
      {
        key: 'changed', label: 'Software changed since', hint: 'The module builds changed after the last known version: the version it runs now has no name yet.',
        cell: (r) => (r.build_changed_since != null ? h('span', { class: 'sw-warn', title: `In ${zoneLabel(r.time_zone || tz)}` }, formatDateTime(r.build_changed_since, r.time_zone || tz, { now }))
          : h('span', { class: 'sw-faint' }, 'no change since')),
        value: (r) => r.build_changed_since,
      },
      {
        key: 'last', label: 'Last update', num: true,
        cell: (r) => (r.last_update_ms != null ? h('span', { title: `In ${zoneLabel(r.time_zone || tz)}` }, formatDate(r.last_update_ms, r.time_zone || tz, { now, year: true })) : h('span', { class: 'sw-faint' }, 'none recorded')),
        value: (r) => r.last_update_ms,
      },
    ],
    empty: () => empty({
      title: 'No car matches.',
      text: search.value.trim() ? `No car name or version holds “${search.value.trim()}”. Clear the search to see every car.` : 'There is no car on this server yet.',
    }),
  });
  let searchTimer = 0;
  search.addEventListener('input', () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      if (!ctx.alive()) return;
      const v = search.value.trim();
      ctx.setQuery({ q: v || null });
      fleetTable.reload(v ? { search: v } : {});
    }, 350);
  });
  ctx.onCleanup(() => clearTimeout(searchTimer));
  ctx.main.append(h('section', { class: 'block sw-fleet', id: 'sw-fleet', 'aria-labelledby': 'sw-fleet-title' },
    h('div', { class: 'block-head' },
      h('h2', { class: 'label', id: 'sw-fleet-title' }, 'Versions of every car', h('span', { class: 'label-note' }, 'which car runs which build, now')),
      h('div', { class: 'block-actions' },
        h('div', { class: 'field field-search sw-search' },
          h('label', { class: 'sr-only', for: 'sw-search' }, 'Find a car or version'),
          h('div', { class: 'field-control' }, h('span', { class: 'field-icon' }, icon('search')), search)))),
    fleetTable.el));

  /* ---------------------------------------------------------------- typing a version (admin) */

  async function editVersion(r) {
    const input = field({
      label: 'Version', name: 'version', value: r.version_source === 'owner' ? r.version : '',
      hint: 'As the car’s screen shows it (Controls, Software), such as 2026.38.4. Leave it empty to forget a typed version. Other updates with the same module builds, on any car, get it too.',
      attrs: { autocomplete: 'off', spellcheck: 'false', inputmode: 'decimal' },
    });
    let closeIt = null;
    const f = form({
      fields: [input], submitLabel: 'Save the version',
      onsubmit: async (values) => {
        const v = String(values.version || '').trim();
        if (v && !VERSION_RX.test(v)) throw Object.assign(new Error('Not a version.'), { fields: { version: 'A version such as 2026.38.4.' } });
        await ctx.api.patch(api.apiPath`cars/${cid}/updates/${r.id}`, { version: v || null });
        if (!ctx.alive()) return;
        ctx.flash(v ? `Saved: this update installed ${v}.` : 'The typed version is forgotten.');
        if (closeIt) closeIt(true);
        reloadAll(false);
      },
    });
    await openDialog({
      title: `The update of ${formatDate(r.start_ms, tz, { year: true })}`,
      body: (close) => {
        closeIt = close;
        return [
          h('p', { class: 'note' }, r.changed_modules && r.changed_modules.length ? `Modules whose builds changed: ${moduleNames(r.changed_modules)}.` : 'No module list for this update.'),
          r.version && r.version_source !== 'owner' ? h('p', { class: 'note' }, `Now shown as ${r.version} (${(SOURCE[r.version_source] || ['unknown source'])[0].toLowerCase()}). A typed version takes its place.`) : null,
          f,
        ];
      },
      actions: [],
      signal: ctx.signal,
      initialFocus: 'input',
    });
  }

  async function reloadAll(say) {
    reloadBtn.disabled = true;
    try {
      await Promise.all([table.reload(), fleetTable.reload(), findNewest()]);
      if (ctx.alive() && !table.items?.length && !lastSummary) drawSummary(null, true);
      drawAsOf();
      if (say && ctx.alive()) ctx.flash('Loaded again.', { kind: 'info' });
    } finally { reloadBtn.disabled = false; }
  }

  let rowsTimer = 0;
  ctx.events(['rows'], (ev) => {
    const d = ev.data || {};
    if (d.kinds && !d.kinds.includes('updates')) return;
    clearTimeout(rowsTimer);
    rowsTimer = setTimeout(() => { if (ctx.alive()) reloadAll(false).catch(() => {}); }, 1500);
  });
  ctx.onCleanup(() => clearTimeout(rowsTimer));

  // Each list fails on its own: the table shows its error with Try again, and the other list stays.
  let historyFailed = false;
  await Promise.all([table.load().catch(() => { historyFailed = true; }), fleetTable.load().catch(() => {}), findNewest()]);
  if (!ctx.alive()) return {};
  if (!lastSummary) drawSummary(null, historyFailed);
  else if (newestEver) drawSummary(lastSummary);
  ctx.ready();
  return {};
}
