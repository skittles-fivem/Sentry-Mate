// Sentry Mate, "Pebble" UI pack: Software (/cars/{car}/software), the version the car runs now and its updates,
// one tile per update, newest first.
//
// A hero tile with the version now (and where that name came from: Sentry Mate never guesses one), the count and
// the typical time between updates, then a tile per update: when, how long, the time since the one before, the
// version (an admin can type it in a sheet), what changed, and what the car did while on it (charges, range).
// A "Tesla update installed" (found from the modules' firmware fingerprints) shows which fields changed, old hex to
// new hex, between which drives, and a box to type its version name (admins; clearing it too). Above the list, the
// car's firmware: each module's fingerprints now, in hex, and when each was first seen (#firmware, linked from Car).
// Data: GET /api/v1/cars/{car}/updates (paged, summary), PATCH /updates/{update}, GET /cars/{car}/firmware,
// GET /cars/{car}/drives (the drive before and after a fingerprint update), GET /settings (release notes).

import { h } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatDate, formatTime, formatDateTime, formatDuration, HOUR } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, chip, btn, icon, note } from '../core/ui.js';
import { srcPill, hasKind } from '../core/source.js';
import { MODULES, moduleList, fieldName, hexCode, isFingerprint, changesList, aroundDrives, betweenLine, namedLine, nameBox, VERSION_RX } from '../core/firmware.js';
import { emptyTile } from '../core/states.js';
import { createSheet } from '../core/sheet.js';
import { field, form } from '../core/form.js';
import { filterBar, statTile, moreButton, facts, sectionHead } from '../core/bits.js';

export const id = 'software';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h2'], [1, 'h1'], [1, 'h1'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Software';

const SOURCE = { teslamate: 'imported from TeslaMate', owner: 'typed by the owner', build_match: 'named from an earlier update with the same fingerprints', tesla_api: 'from the Tesla API' };
const DETECTED = { ota_status: 'the car’s update status', fingerprint: 'the modules’ firmware fingerprints', build_change: 'changed module builds', teslamate: 'the imported history', tesla_api: 'the Tesla API' };
const CHIP = { teslamate: 'Imported', owner: 'Typed by the owner', build_match: 'Same fingerprints', tesla_api: 'From the Tesla API' };
const mods = moduleList;
const hexCls = (bits) => (bits >= 56 ? 'is-long' : null);

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const path = api.apiPath`cars/${car.id}/updates`;
  const pager = ctx.api.pager(path, ctx.params, { limit: 30 });
  const [settings, fw] = await Promise.all([
    ctx.api.get('settings', null, { maxAgeMs: 300000 }).catch(() => null),
    ctx.api.get(api.apiPath`cars/${car.id}/firmware`).catch((e) => ({ error: e })),
    pager.more(),
  ]);
  if (!ctx.alive()) return {};
  // the drives on either side of each fingerprint update, read before its tile is drawn
  const around = new Map();
  const readAround = (list) => Promise.all(list.filter(isFingerprint).map(async (r) => { around.set(r.id, await aroundDrives(ctx, car.id, r)); }));
  await readAround(pager.items);
  if (!ctx.alive()) return {};
  const notesOn = !!settings?.outside_links?.release_notes;
  const sm = pager.last?.summary || {};
  const at = (ms) => formatDateTime(ms, tz, { now: api.now() });

  ctx.head({ actions: [btn('Refresh', { kind: 'soft', icon: 'refresh', iconOnly: true, cover: 'updates.control.refresh', onclick: () => ctx.reload() })] });
  const bar = filterBar(ctx, { carCover: 'updates.filter.car', periodCover: 'updates.filter.time_range' });

  /* ------------------------------------------------------------ now */
  const installing = pager.items.find((r) => r.end_ms == null);
  const changed = sm.build_changed_since != null;
  const v = sm.current_version;
  const hero = tile({ w: 2, title: car.name, icon: 'chip', cls: 'sw-hero', cover: ['updates.section.car_header', 'shots.updates'] },
    h('div', { class: 'sw-now' },
      h('span', { class: 'hero-k' }, 'Runs now'),
      h('p', { class: ['sw-ver-big', { 'is-unknown': !v || changed }] }, changed ? 'Not named yet' : v || 'Not known'),
      h('div', { class: 'item-flags' },
        installing ? chip('Installing now', { tone: 'sun', icon: 'clock' }) : null,
        changed ? chip('Software changed', { tone: 'sun', icon: 'warning' }) : null,
        !changed && v && SOURCE[sm.current_version_source] ? chip(CHIP[sm.current_version_source], { tone: 'sky', title: SOURCE[sm.current_version_source] }) : null)),
    facts([
      ['Running since', sm.current_version_since_ms != null && !changed ? at(sm.current_version_since_ms) : null],
      ['Last named version', changed ? v || 'none' : null],
      ['Changed', changed ? at(sm.build_changed_since) : null],
      ['Installing since', installing ? at(installing.start_ms) : null],
    ]),
    note(changed ? `The firmware fingerprints changed after ${v || 'the last known version'}, so the car runs a version nobody has named. ${ctx.admin ? 'Type it on the newest update below.' : 'An admin can type it below.'}`
      : 'The car does not send its version text. A version is known once imported, read from the Tesla API, typed by an admin, or named from an earlier update with the same fingerprints. It is never guessed.'),
    h('a', { class: 'tile-link', href: '#firmware' }, 'Car firmware', icon('chevronDown')));
  const countT = statTile({ label: 'Updates', value: sm.count ?? 0, kind: 'count', icon: 'download', cover: 'updates.stat.count', sub: 'started in the period' });
  const gapT = statTile({ label: 'Typical gap', node: num(sm.median_gap_ms, 'duration', { why: 'needs two updates' }), icon: 'calendar', cover: 'updates.stat.median_gap', sub: 'median time between updates' });
  const tell = tile({ w: 2, title: 'Hear about updates', icon: 'bolt', cover: 'projects.telegram-bot' },
    h('p', { class: 'note' }, 'Sentry Mate sends a “software changed” event (MQTT and the events list) as soon as an update ends. ',
      h('a', { href: state.buildUrl('settings-integrations') }, 'Integrations'), ' connects it to your chat or home automation. A waiting update is not on the CAN bus, so there is no “update available” message.'),
    h('p', { class: 'note', cover: 'updates.link.release_notes' }, notesOn ? 'A known version links to its release notes on an outside site. Nothing is sent until you tap one.'
      : 'Release-notes links are off, so nothing leaves this server. An admin can turn them on in ', notesOn ? null : h('a', { href: state.buildUrl('settings-display') }, 'Display'), notesOn ? null : '.'));

  /* ------------------------------------------------------------ the updates */
  const listStart = h('div', { class: 'sw-list-start w-full', cover: 'updates.table' },
    h('div', { class: 'section-head' }, h('h2', { class: 'section-title' }, 'Updates'), h('p', { class: 'note' }, 'Newest first. Charges and range are what the car did while on each version.')));
  const listEnd = h('div', { class: 'contents' });
  const timelineHref = (r) => state.buildUrl('timeline', { car: car.id, period: { from: r.start_ms - HOUR, to: (r.end_ms ?? api.now()) + HOUR } });
  const onVersion = (r) => ({ from: r.end_ms ?? r.start_ms, to: r.until_ms ?? api.now() });
  const notesHref = (x) => `https://www.notateslaapp.com/software-updates/version/${encodeURIComponent(x)}/release-notes`;

  function updateTile(r) {
    const going = r.end_ms == null;
    const fp = isFingerprint(r);
    const ver = r.version
      ? (notesOn && VERSION_RX.test(r.version)
        ? h('a', { class: 'sw-ver', href: notesHref(r.version), target: '_blank', rel: 'noopener noreferrer external', title: 'Release notes on an outside site (opens a new tab)' }, r.version, icon('external'))
        : h('span', { class: 'sw-ver' }, r.version))
      : h('span', { class: 'sw-ver is-unknown' }, fp ? 'Not named yet' : 'Build changed');
    const canType = !fp && ctx.admin && (r.sources ? hasKind(r, 'smo') && r.version_source !== 'tesla_api' : r.source === 'can');
    return tile({ w: 2, cls: ['item', 'sw-tile', { 'is-fp': fp }], id: `update-${r.id}`, label: fp ? `Tesla update installed, ${r.version || 'not named yet'}` : null },
      h('div', { class: 'item-top' },
        h('span', { class: ['item-ic', fp ? 'tone-coral' : 'tone-sky'], 'aria-hidden': 'true' }, icon(fp ? 'download' : 'chip')),
        h('div', { class: 'item-text' },
          fp ? h('p', { class: 'sw-kind' }, 'Tesla update installed') : null,
          h('p', { class: 'item-title', cover: 'updates.table.version' }, ver),
          h('p', { class: 'item-sub', cover: 'updates.table.date' }, h('a', { href: timelineHref(r), title: 'Open the timeline around this update' }, `${formatDate(r.start_ms, tz, { now: api.now(), year: true })}, ${formatTime(r.start_ms, tz)}`))),
        h('span', { class: 'item-flags' },
          going ? chip('Installing', { tone: 'sun' }) : null,
          r.version && SOURCE[r.version_source] ? chip(r.version_source === 'owner' ? 'Typed' : r.version_source === 'build_match' ? 'Same fingerprints' : r.version_source === 'tesla_api' ? 'Tesla API' : 'Imported', { tone: 'muted' }) : null,
          !r.version ? chip('Version not known', { tone: 'sun' }) : null)),
      fp ? namedLine(r) : null,
      h('div', { class: 'tile-nums sw-nums' },
        h('div', { cover: 'updates.table.duration' }, h('span', { class: 'hero-k' }, going ? 'So far' : fp ? 'Window' : 'Took'), num(r.duration_ms, 'duration', { size: 's' })),
        h('div', { cover: 'updates.table.since_previous' }, h('span', { class: 'hero-k' }, 'After the one before'), num(r.since_previous_ms, 'duration', { size: 's', why: 'first one recorded' })),
        h('div', { cover: 'updates.table.charge_count' }, h('span', { class: 'hero-k' }, 'Charges on it'),
          r.charges ? h('a', { class: 'sw-link', href: state.buildUrl('charges', { car: car.id, period: onVersion(r) }) }, formatNumber(r.charges)) : h('span', { class: 'sw-link is-zero' }, '0')),
        h('div', { cover: 'updates.table.avg_range' }, h('span', { class: 'hero-k' }, 'Range at 100 %'), num(r.full_range_km, 'range', { size: 's', why: 'no reading' }))),
      fp ? betweenLine(ctx, car.id, r, around.get(r.id), tz) : null,
      fp ? changesList(r) : null,
      h('p', { class: 'item-sub' }, `Found by ${DETECTED[r.detected_by] || r.detected_by || 'the car'}${!fp && r.changed_modules?.length ? ` · changed: ${mods(r.changed_modules)}` : fp && r.changed_modules?.length ? ` · ${r.changed_modules.length} of 4 modules changed` : ''}${r.full_pack_kwh != null ? ` · full pack ${format(r.full_pack_kwh, 'energy')}` : ''}`),
      h('p', { class: 'src-line' }, srcPill(r, { short: true })),
      fp ? nameBox(ctx, car.id, r, { onSaved: () => ctx.reload() }) : null,
      canType ? h('div', { class: 'item-links' }, btn(r.version ? 'Change the version' : 'Type the version', { kind: 'soft', icon: 'edit', onclick: (e) => editVersion(r, e.currentTarget) })) : null);
  }

  /* ------------------------------------------------------------ the car's firmware now */
  const fwHead = h('div', { class: 'section-head w-full', id: 'firmware' },
    h('h2', { class: 'section-title' }, 'Car firmware'),
    h('p', { class: 'note' }, 'Tesla’s version number is not on the CAN bus, but four modules send fingerprints of their code about once a second. When they change, an update is installed: it shows below as “Tesla update installed”. ',
      h('a', { href: state.buildUrl('car', { car: car.id }) }, `${car.name}’s overview`), ' links here.'));
  const fwTiles = [];
  if (fw.error) {
    fwTiles.push(tile({ w: 2, title: 'Car firmware', icon: 'chip' }, note('The fingerprints did not load. Refresh to try again.')));
  } else {
    const at = (ms) => formatDate(ms, tz, { now: api.now(), year: true });
    fwTiles.push(tile({ w: 2, title: 'Fingerprints', icon: 'chip', cls: 'sw-fw-sum' },
      facts([
        ['Last seen', fw.updated_ms != null ? formatDateTime(fw.updated_ms, tz, { now: api.now() }) : 'None yet'],
        ['Updates found from them', formatNumber(fw.updates ?? 0)],
        ['Bridge hints', fw.bridge_hints?.count ? `${formatNumber(fw.bridge_hints.count)}, last ${formatDateTime(fw.bridge_hints.last_ms, tz, { now: api.now() })}` : 'None'],
      ]),
      note('Each value is taken once it was seen three times while the car was awake, so a damaged frame never shows. Hex is lowercase: 14 digits for a git hash, 8 for a CRC.')));
    for (const m of fw.modules || []) {
      fwTiles.push(tile({ w: 2, title: MODULES[m.id] || m.name, icon: 'chip', cls: 'sw-fw-mod', side: m.can_id != null ? h('code', { class: 'fw-sig' }, `0x${Number(m.can_id).toString(16).toUpperCase()}`) : null },
        (m.fields || []).length ? h('ul', { class: 'fw-rows' }, m.fields.map((f) => h('li', { class: 'fw-row' },
          h('span', { class: 'fw-k' }, h('span', { class: 'fw-field' }, fieldName(f.field)), h('span', { class: 'fw-sig' }, f.field)),
          h('span', { class: 'fw-v' }, hexCode(f.hex, hexCls(f.bits)),
            h('span', { class: 'fw-when' }, f.first_seen_ms != null ? `since ${at(f.first_seen_ms)}` : 'first seen: not known', (f.history || []).length > 1 ? ` · ${formatNumber(f.history.length)} values so far` : '')))))
          : note(`${car.name}’s logs carry no fingerprint from this module. Some cars (an older Model S, say) do not send it.`)));
    }
  }

  const sheet = createSheet({ title: 'Version of this update' });
  ctx.onCleanup(() => sheet.el.remove());
  function editVersion(r, opener) {
    const f = field({ label: 'Version', name: 'version', value: r.version || '', hint: 'Like 2026.38.4. Leave empty to forget a typed version. Other updates with the same module builds get it too.', attrs: { autocomplete: 'off', inputmode: 'decimal', 'data-autofocus': '' } });
    sheet.body.replaceChildren(form({
      fields: [f], submitLabel: 'Save version',
      async onsubmit(vals) {
        const text = (vals.version ?? '').toString().trim();
        if (text && !VERSION_RX.test(text)) throw Object.assign(new Error('Type it like 2026.38.4.'), { fields: { version: 'Type it like 2026.38.4.' } });
        await ctx.api.patch(api.apiPath`cars/${car.id}/updates/${r.id}`, { version: text || null });
        sheet.close();
        ctx.toast(text ? `Saved as ${text}.` : 'Typed version forgotten.', { next: true });
        ctx.reload();
      },
    }));
    sheet.open(opener);
  }

  const drawn = [];
  const more = moreButton(async () => { const n = pager.items.length; await pager.more(); await readAround(pager.items.slice(n)); if (!ctx.alive()) return false; draw(pager.items.slice(n)); return !pager.done; }, { text: 'Show earlier updates' });
  function draw(list) { for (const r of list) { const t = updateTile(r); listEnd.before(t); drawn.push(t); } }
  ctx.view.replaceChildren(bar, hero, countT, gapT, tell, fwHead, ...fwTiles, listStart, listEnd);
  if (!pager.items.length) {
    listEnd.before(emptyTile({ icon: 'chip', cover: ['updates.table.version', 'updates.table.date', 'updates.table.duration', 'updates.table.since_previous', 'updates.table.charge_count', 'updates.table.avg_range'], title: 'No updates in this period', text: ctx.period?.preset === 'all' ? `Nothing recorded for ${car.name} yet. An update shows once the car installs one while recording, or with an imported history.` : 'Pick a longer period above; All time shows every update.' }));
  } else {
    draw(pager.items);
    if (!pager.done) listEnd.append(more);
  }
  ctx.ready();
  return {};
}
