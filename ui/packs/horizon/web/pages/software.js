// Software (/cars/{car}/software): the version the car runs now on an LCD face, the update count and the
// typical gap as trip counters, the update history as a track list (GET /cars/{car}/updates, paged, with
// its summary on the first page) and every car's version in one list (GET /fleet/versions). A version is
// shown only when someone named it (imported, typed by an admin, or the same build as a named update);
// otherwise the page says the software changed. Admins type a version on an update (PATCH). "rows" events
// for updates reload both lists. Without a car (the address /software is not one, but the station allows
// it) only the list of every car is shown.
// With a car, two more panels (core/firmware.js): "Tesla updates installed" (the updates found from the modules'
// firmware fingerprints, each with its changed fields old hex -> new hex, the drives on either side and a name
// box for admins) and "Car firmware" (#firmware, GET /cars/{car}/firmware: every module's fingerprints now).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { column } from '../shared/csv.js';
import { format } from '../shared/units.js';
import { formatDate, formatDateTime, formatDuration, HOUR } from '../shared/time.js';
import { icon, hud, plate, btn, tag, kv, note } from '../core/ui.js';
import { controls, slot, periodBtn, carBtn, counters, search } from '../core/kit.js';
import { srcChip, sourceLabel } from '../core/sources.js';
import { tracks } from '../core/tracks.js';
import { emptyState } from '../core/states.js';
import { openDialog, toast } from '../core/layer.js';
import { field, form } from '../core/form.js';
import { isAdmin } from '../core/prefs.js';
import { firmwarePanel, fingerprintPanel, nameDialog, nameWords, isFingerprint, DETECTED_WORD } from '../core/firmware.js';

export const id = 'software';
export const css = true;

const enc = encodeURIComponent;
const iso = (ms) => (ms == null ? '' : new Date(ms).toISOString());
const SOURCE = {
  teslamate: ['Imported', 'Named in the imported TeslaMate history.'],
  owner: ['Typed', 'Typed by an admin on this page.'],
  build_match: ['Same build', 'The same module builds as an update whose version is known.'],
  tesla_api: ['Tesla API', 'Named by the Tesla API, which reports the version the car runs.'],
};
const DETECTED = { fingerprint: 'changed firmware fingerprints (SMO)', ota_status: 'update status (SMO)', build_change: 'changed module builds (SMO)', teslamate: 'the TeslaMate import', tesla_api: 'the Tesla API' };
const MODULES = { bms: 'battery management', vcfront: 'front body controller', dir: 'rear drive unit', dif: 'front drive unit', di_front: 'front motor', di_rear: 'rear motor', fc: 'fast charging' };
const moduleNames = (list) => (list || []).map((m) => MODULES[m] || m);

export async function mount(ctx) {
  const { car, tz, period } = ctx;
  const admin = isAdmin();
  const settings = await ctx.api.get('settings', null, { maxAgeMs: 300000 }).catch(() => null);
  if (!ctx.alive()) return;
  const notesOn = !!settings?.outside_links?.release_notes;
  const at = (ms) => formatDateTime(ms, tz, { now: ctx.api.now() });

  /* ---------- every car's version */
  let findText = '';
  const fleetList = tracks(ctx, {
    title: 'Versions of every car', icon: 'grid', path: 'fleet/versions', params: {}, limit: 50, csvName: 'versions', csv: { all: true },
    side: search('', (v) => { findText = v; fleetList.reload({ search: v || null }); }, { placeholder: 'Find a car or version', ctx }),
    note: 'One row per car you may see. A car that changed its software after its last named version runs one nobody has named yet.',
    columns: [
      { key: 'car', label: 'Car', lead: true, render: (r) => h('a', { class: 'cell-main', href: state.buildUrl('software', { car: r.car, period: state.current().period }) }, r.name || r.car), csv: (r) => r.name || r.car },
      { key: 'version', label: 'Version now', render: (r) => (r.build_changed_since != null
        ? h('span', { class: 'cell' }, h('span', { class: 'sw-ver is-unknown' }, 'Not named yet'), h('span', { class: 'cell-sub' }, r.version ? `last named ${r.version}` : 'none named before'))
        : r.version ? h('span', { class: 'flagline' }, h('span', { class: 'sw-ver' }, r.version), SOURCE[r.version_source] ? tag(SOURCE[r.version_source][0], 'cyan') : null)
          : h('span', { class: 'sw-ver is-unknown' }, 'Not known')), csv: (r) => (r.build_changed_since != null ? '' : r.version || '') },
      { key: 'changed', label: 'Changed since', render: (r) => (r.build_changed_since != null ? h('span', { class: 'warn' }, formatDateTime(r.build_changed_since, r.time_zone || tz, { now: ctx.api.now() })) : h('span', { class: 'faint' }, 'no change since')), csv: (r) => iso(r.build_changed_since) },
      { key: 'last', label: 'Last update', num: true, render: (r) => (r.last_update_ms != null ? formatDate(r.last_update_ms, r.time_zone || tz, { now: ctx.api.now(), year: true }) : h('span', { class: 'faint' }, 'none recorded')), csv: (r) => iso(r.last_update_ms) },
    ],
    empty: () => emptyState({ icon: 'search', title: findText ? 'No car matches' : 'No car yet', text: findText ? `No car name or version holds "${findText}".` : 'Add a car in Car settings.' }),
  });

  if (!car) {
    replace(ctx.view, h('div', { class: 'page sw', cover: 'shots.updates' },
      plate({ kicker: 'Reports · every car', title: 'Software', aim: ctx.info.aim }), fleetList.el));
    await fleetList.ready;
    if (ctx.alive()) ctx.ready();
    return;
  }

  const cid = car.id;
  const path = `cars/${enc(cid)}/updates`;

  /* ---------- the LCD and the counters */
  const lcd = h('div', { class: 'sw-lcd-wrap' });
  const figs = h('div');
  function paintSummary(sm, rows) {
    sm = sm || {};
    const installing = rows.find((r) => r.end_ms == null);
    const v = sm.current_version;
    const src = SOURCE[sm.current_version_source];
    const changed = sm.build_changed_since != null;
    const since = sm.current_version_since_ms != null ? at(sm.current_version_since_ms) : null;
    replace(lcd,
      h('div', { class: ['sw-lcd', (!v || changed) && 'is-unknown'], role: 'img', 'aria-label': `Version now: ${changed ? 'not named yet' : v || 'not known'}` },
        h('span', { class: 'sw-lcd-cap' }, 'Version now'),
        h('span', { class: 'sw-lcd-ver' }, changed ? 'NOT NAMED' : v || 'UNKNOWN'),
        h('span', { class: 'sw-lcd-sub' }, installing ? 'Installing now' : changed ? 'Software changed' : src ? src[0] : 'Nobody has named it')),
      kv([
        changed ? ['Last named version', v || 'none'] : null,
        [changed ? 'Source of that name' : 'Source', src ? h('span', { title: src[1] }, src[1]) : 'Nobody has said yet'],
        changed ? ['Named version ran', `${since ? `from ${since} ` : ''}to ${at(sm.build_changed_since)}`] : ['Running since', since || '—'],
        changed ? ['Software changed', h('span', { class: 'warn' }, at(sm.build_changed_since))] : null,
        installing ? ['Installing since', at(installing.start_ms)] : null,
      ]),
      note(changed ? `The module builds changed after ${v || 'the last named version'} was recorded. ${admin ? 'Type the new version on the newest update below.' : 'An admin can type it on the newest update below.'}`
        : 'The car does not send its version text, so Sentry Mate never guesses one. It is known once a TeslaMate history is imported, an admin types it, or another update has the same build.'));
    replace(figs, counters([
      { label: 'Updates', value: sm.count ?? null, kind: 'count', cover: 'updates.stat.count', how: 'started in the period' },
      { label: 'Typical gap', text: sm.median_gap_ms != null ? formatDuration(sm.median_gap_ms) : '—', cover: 'updates.stat.median_gap', how: sm.median_gap_ms != null ? 'median time between updates' : 'needs two updates' },
    ]));
  }

  /* ---------- the history */
  const timelineHref = (r) => state.buildUrl('timeline', { car: cid, period: { from: r.start_ms - HOUR, to: (r.end_ms ?? ctx.api.now()) + HOUR } });
  const onVersion = (r) => ({ from: r.end_ms ?? r.start_ms, to: r.until_ms ?? ctx.api.now() });
  const notesLink = (v) => {
    const tpl = settings?.outside_links?.release_notes_url;
    return notesOn && v && tpl ? h('a', { class: 'plc', href: String(tpl).replace('{version}', enc(v)), target: '_blank', rel: 'noopener noreferrer' }, 'notes') : null;
  };
  let painted = false;
  const list = tracks(ctx, {
    title: 'Update history', icon: 'chip', path, params: ctx.params, limit: 50, csvName: 'software-updates', csv: { all: true }, cover: 'updates.table',
    note: 'Newest first. Charges and range are what the car did while on each version, from the end of the update to the next one.',
    columns: [
      { key: 'start', label: 'Started', lead: true, cover: 'updates.table.date', render: (r, i) => h('span', { class: 'cell' },
        h('a', { class: 'cell-main', href: timelineHref(r), title: 'Open the timeline around this update' }, h('span', { class: 'trk-no' }, `${String(i + 1).padStart(2, '0')} `), formatDate(r.start_ms, tz, { now: ctx.api.now(), year: true })),
        isFingerprint(r) ? h('span', { class: 'cell-sub' }, tag(DETECTED_WORD, 'cyan')) : null,
        h('span', { class: 'cell-sub' }, `${formatDateTime(r.start_ms, tz, { weekday: false, year: false }).split(', ').pop()} · seen by ${DETECTED[r.detected_by] || r.detected_by || 'the car'}`)), csv: (r) => iso(r.start_ms) },
      { key: 'duration', label: 'Took', num: true, cover: 'updates.table.duration', render: (r) => (r.end_ms == null ? tag('Installing', 'org') : formatDuration(r.duration_ms)), csv: (r) => (r.duration_ms == null ? '' : Math.round(r.duration_ms / 60000)) },
      { key: 'since', label: 'After the one before', num: true, cover: 'updates.table.since_previous', render: (r) => (r.since_previous_ms != null ? formatDuration(r.since_previous_ms) : h('span', { class: 'faint' }, 'first one')), csv: (r) => (r.since_previous_ms == null ? '' : Math.round(r.since_previous_ms / 60000)) },
      { key: 'version', label: 'Version', cover: 'updates.table.version', render: (r) => h('span', { class: 'flagline' },
        r.version ? h('span', { class: 'sw-ver' }, r.version) : h('span', { class: 'sw-ver is-unknown' }, r.detected_by === 'build_change' || isFingerprint(r) ? 'Not named' : 'Not known'),
        r.version && SOURCE[r.version_source] ? h('span', { class: 'tag tag--cyan', title: nameWords(r) || SOURCE[r.version_source][1] }, SOURCE[r.version_source][0]) : null,
        r.version ? notesLink(r.version) : null,
        admin ? btn(r.version ? 'Change' : 'Name it', () => editVersion(r), { sm: true, kind: 'ghost', icon: 'chip' }) : null,
        r.version && r.named_by === 'auto' ? h('span', { class: 'cell-sub sw-mods' }, nameWords(r)) : null,
        r.changed_modules?.length ? h('span', { class: 'cell-sub sw-mods' }, `changed: ${moduleNames(r.changed_modules).join(', ')}`, isFingerprint(r) ? [' · ', h('a', { class: 'plc', href: '#installed' }, 'fingerprints')] : null) : null), csv: (r) => r.version || '' },
      { key: 'charges', label: 'Charges on it', num: true, cover: 'updates.table.charge_count', render: (r) => (r.charges ? h('a', { class: 'plc', href: state.buildUrl('charges', { car: cid, period: onVersion(r) }) }, String(r.charges)) : '0'), csv: (r) => r.charges },
      { key: 'range', label: 'Range at 100 %', num: true, cover: 'updates.table.avg_range', render: (r) => format(r.full_range_km, 'range'), csv: column('Range at 100 %', 'full_range_km', 'range') },
      { key: 'pack', label: 'Full pack', num: true, render: (r) => (r.full_pack_kwh == null ? h('span', { class: 'faint' }, 'not measured') : format(r.full_pack_kwh, 'energy')), csv: column('Full pack', 'full_pack_kwh', 'energy') },
      { key: 'src', label: 'Source', render: (r) => srcChip(r), csv: (r) => sourceLabel(r) },
    ],
    empty: () => emptyState({ icon: 'chip', title: 'No updates in this period', text: period?.preset === 'all' ? `Nothing is recorded for ${car.name} yet. An update shows once the car installs one while recording, or with an imported history.` : 'No update started in this period. All time shows every update.' }),
    onPage: (pg) => { if (!ctx.alive()) return; painted = true; paintSummary(pg.summary, list.rows()); },
  });

  async function editVersion(r) {
    if (isFingerprint(r)) { nameDialog(ctx, car, r, () => reloadAll()); return; }
    const input = field({ label: 'Version', name: 'version', path: 'version', value: r.version || '',
      hint: 'As the car screen shows it, such as 2026.38.4. Leave it empty to forget a typed version. Updates with the same build get it too.',
      attrs: { pattern: '\\d{4}\\.\\d{1,2}(\\.\\d{1,3}){0,2}', autocomplete: 'off', spellcheck: 'false' } });
    let d = null;
    const f = form({ fields: [input], submit: 'Save', async onSubmit(v) {
      const text = String(v.version || '').trim();
      await ctx.api.patch(`${path}/${enc(r.id)}`, { version: text || null });
      toast(text ? `Saved: this update installed ${text}.` : 'The typed version is forgotten.');
      d?.close();
      reloadAll();
    } });
    d = openDialog({ title: `Update of ${formatDate(r.start_ms, tz, { year: true })}`, body: [r.changed_modules?.length ? note(`Modules that changed: ${moduleNames(r.changed_modules).join(', ')}.`) : null, f.el] });
  }

  const reloadBtn = btn('Reload', () => reloadAll(true), { icon: 'arrow-r', kind: 'ghost', sm: true, cover: 'updates.control.refresh', title: 'Load the figures and both lists again. They also reload by themselves when an update starts or ends.' });
  async function reloadAll(say = false) {
    reloadBtn.disabled = true;
    try { await Promise.all([list.reload(), fleetList.reload(), installed.load(), fwPanel.load()]); if (say && ctx.alive()) toast('Loaded again.', { kind: 'info' }); } finally { reloadBtn.disabled = false; }
  }

  const installed = fingerprintPanel(ctx, car, { onNamed: () => { list.reload(null, { quiet: true }); fleetList.reload(null, { quiet: true }); } });
  const fwPanel = firmwarePanel(ctx, car);

  const deck = controls({ note: [h('span', null, 'An update belongs to the period it started in; the version now does not depend on it. '),
    h('span', { cover: 'updates.link.release_notes' }, notesOn ? 'A named version links to its release notes on an outside site; nothing is sent until you click.' : 'Release notes links are off, so nothing leaves this server; an admin can turn them on in Display and units.')] },
  slot('Period', periodBtn(ctx, 'updates.filter.time_range')));

  replace(ctx.view, h('div', { class: 'page sw', cover: 'shots.updates' },
    plate({ kicker: `Reports · ${car.name}`, title: 'Software', aim: ctx.info.aim, cover: 'updates.section.car_header',
      tools: [carBtn(ctx, 'updates.filter.car'), reloadBtn] }),
    deck,
    h('div', { class: 'cols sw-top' },
      hud({ title: 'On the dash', icon: 'chip', tone: 'cyan' }, lcd),
      hud({ title: 'In this period', icon: 'stats' }, figs,
        h('p', { class: 'note sw-tell', cover: 'projects.telegram-bot' }, icon('bolt', 'i--sm'), h('span', null,
          'Want a message when the software changes? Sentry Mate sends a "software changed" event (MQTT and the events list) when an update ends. ',
          h('a', { class: 'plc', href: state.buildUrl('settings-integrations') }, 'Integrations'), ' connects it to your chat or home automation. A waiting update is not on the CAN bus, so there is no "update available" message.')))),
    list.el, installed.el, fwPanel.el, fleetList.el));

  ctx.events('rows', (ev) => {
    const d = ev?.data || {};
    if (d.kinds && !d.kinds.includes('updates')) return;
    if (!d.car || d.car === cid) { list.reload(null, { quiet: true }); installed.refresh(); fwPanel.refresh(); }
    fleetList.reload(null, { quiet: true });
  });

  try { await Promise.all([list.ready, fleetList.ready, installed.load(), fwPanel.load()]); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  if (!painted) paintSummary(null, []);
  ctx.ready();
}
