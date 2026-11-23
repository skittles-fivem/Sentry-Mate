// Sentry Mate, "Pebble" UI pack: Software (/cars/{car}/software), the version the car runs now and its updates,
// one tile per update, newest first.
//
// A hero tile with the version now (and where that name came from: Sentry Mate never guesses one), the count and
// the typical time between updates, then a tile per update: when, how long, the time since the one before, the
// version (an admin can type it in a sheet), what changed, and what the car did while on it (charges, range).
// A "Tesla update installed" (found from the modules' firmware fingerprints) shows which fields changed, old hex to
// new hex, between which drives, and a box to type its version name (admins; clearing it too). Above the list, the
// car's firmware: each module's fingerprints now, in hex, and when each was first seen (#firmware, linked from Car),
// then the module firmware history (#firmware-history, linked from Car): a "versions over time" strip, one lane per
// module, and every fingerprint each module ran (filter by module, Show more), with the version name it came with.
// Read-only for everyone; live: a `rows` event with updates reads the history again and redraws it in one go.
// Data: GET /api/v1/cars/{car}/updates (paged, summary), PATCH /updates/{update}, GET /cars/{car}/firmware,
// GET /cars/{car}/drives (the drive before and after a fingerprint update), GET /settings (release notes),
// GET /cars/{car}/firmware/history (paged, with modules, spans and range).

import { h } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatDate, formatTime, formatDateTime, formatDuration, HOUR } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, chip, btn, icon, note } from '../core/ui.js';
import { srcPill, hasKind } from '../core/source.js';
import { MODULES, moduleName, HISTORY_ID, moduleList, fieldName, hexCode, isFingerprint, changesList, aroundDrives, betweenLine, namedLine, nameBox, VERSION_RX, VERSION_SOURCE as SOURCE, VERSION_CHIP as CHIP, VERSION_CHIP_SHORT } from '../core/firmware.js';
import { emptyTile } from '../core/states.js';
import { createSheet } from '../core/sheet.js';
import { field, form } from '../core/form.js';
import { filterBar, statTile, moreButton, facts, sectionHead, pills, dur } from '../core/bits.js';

export const id = 'software';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h2'], [1, 'h1'], [1, 'h1'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Software';

const DETECTED = { ota_status: 'the car’s update status', fingerprint: 'the modules’ firmware fingerprints', build_change: 'changed module builds', teslamate: 'the imported TeslaMate history', teslafi: 'the imported TeslaFi history', tesla_api: 'the Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'TeslaMate' };
const mods = moduleList;
const hexCls = (bits) => (bits >= 56 ? 'is-long' : null);

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz;
  const path = api.apiPath`cars/${car.id}/updates`;
  const pager = ctx.api.pager(path, ctx.params, { limit: 30 });
  const histPath = api.apiPath`cars/${car.id}/firmware/history`;
  const HIST_LIMIT = 20;
  let hist = ctx.api.pager(histPath, {}, { limit: HIST_LIMIT });
  let histErr = null;
  const [settings, fw] = await Promise.all([
    ctx.api.get('settings', null, { maxAgeMs: 300000 }).catch(() => null),
    ctx.api.get(api.apiPath`cars/${car.id}/firmware`).catch((e) => ({ error: e })),
    pager.more(),
    hist.more().catch((e) => { histErr = e; }),
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
      : 'The car does not send its version text. A version is known once imported, read from TeslaFi, TeslaMate or the Tesla API, typed by an admin (a typed one wins), or named from an earlier update with the same fingerprints. It is never guessed.'),
    h('a', { class: 'tile-link', href: '#firmware' }, 'Car firmware', icon('chevronDown')));
  const countT = statTile({ label: 'Updates', value: sm.count ?? 0, kind: 'count', icon: 'download', cover: 'updates.stat.count', sub: 'started in the period' });
  const gapT = statTile({ label: 'Typical gap', node: num(sm.median_gap_ms, 'duration', { why: 'needs two updates' }), icon: 'calendar', cover: 'updates.stat.median_gap', sub: 'median time between updates' });
  const tell = tile({ w: 2, title: 'Hear about updates', icon: 'bolt', cover: 'projects.telegram-bot' },
    h('p', { class: 'note' }, 'Sentry Mate sends a “software changed” event (MQTT and the events list) as soon as an update ends. ',
      h('a', { href: state.buildUrl('settings-integrations') }, 'Integrations'), ' connects it to your chat or home automation. A waiting update is not on the CAN bus, so there is no “update available” message.'),
    h('p', { class: 'note', cover: 'updates.link.release_notes' }, notesOn ? 'A known version links to its release notes on an outside site. Nothing is sent until you tap one.'
      : 'Release-notes links are off, so nothing leaves this server. An admin can turn them on in ', notesOn ? null : h('a', { href: `${state.buildUrl('settings-maps')}#outside-links` }, 'Maps and addresses'), notesOn ? null : '.'));

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
          r.version && SOURCE[r.version_source] ? chip(VERSION_CHIP_SHORT[r.version_source], { tone: 'muted', title: SOURCE[r.version_source] }) : null,
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

  /* ------------------------------------------------------------ module firmware history */
  const day = (ms) => (ms == null ? 'not known' : formatDate(ms, tz, { now: api.now(), year: true }));
  const histHead = h('div', { class: 'section-head w-full', id: HISTORY_ID },
    h('h2', { class: 'section-title' }, 'Module firmware history'),
    h('p', { class: 'note' }, 'Every fingerprint each module ran, newest first, with the version name its update had. Imported versions carry no fingerprints and show as version rows.'));
  const spanBody = h('div', { class: 'fwh-body' });
  const spanTile = tile({ w: 'full', title: 'Versions over time', icon: 'clock', cls: 'fwh-strip-tile' }, spanBody);
  let hMod = '';
  const modPills = pills({ label: 'Module', value: '', options: [['', 'All'], ...Object.keys(MODULES).map((m) => [m, moduleName(m)])], onchange: (m) => pickModule(m) });
  const rowList = h('ul', { class: 'fwh-rows' });
  const listBody = h('div', { class: 'fwh-body' });
  const histTile = tile({ w: 'full', title: 'Fingerprints', icon: 'chip', cls: 'fwh-list-tile' }, modPills, listBody);
  const histMore = moreButton(async () => {
    const got = await hist.more();
    if (!ctx.alive()) return false;
    rowList.append(...got.map(histRow));
    return !hist.done;
  });

  function laneSeg(s, i, pct) {
    const a = pct(s.from_ms), b = pct(s.to_ms ?? api.now());
    const words = `${s.version || 'unnamed'}: ${String(s.fingerprint ?? '').toLowerCase()}, ${day(s.from_ms)} to ${s.current ? 'now' : day(s.to_ms)}`;
    const el = h('span', { class: ['fwh-seg', { 'is-alt': i % 2 === 1, 'is-current': !!s.current, 'is-unnamed': !s.version }], title: words },
      b - a >= 0.16 ? h('span', { class: 'fwh-seg-label' }, s.version || 'unnamed') : null);
    el.style.setProperty('--a', a.toFixed(5));
    el.style.setProperty('--b', Math.max(a, b).toFixed(5));
    return el;
  }
  function drawLanes(d) {
    const spans = d.spans || [];
    const r = d.range || {};
    if (r.from_ms == null || !spans.length) return [note(`No fingerprint seen yet. ${car.name}’s modules send theirs while the car is awake; the strip fills in from the first log.`)];
    const from = r.from_ms;
    const to = Math.max(from + 1, r.to_ms ?? from, ...spans.map((s) => s.to_ms ?? api.now()));
    const pct = (t) => Math.max(0, Math.min(1, (t - from) / (to - from)));
    const mods = (d.modules || []).length ? d.modules : Object.keys(MODULES).map((id) => ({ id }));
    const lanes = mods.map((m) => {
      const mine = spans.filter((s) => s.module === m.id);
      const name = moduleName(m.id, m.name);
      const ticks = mine.slice(1).map((s) => { const t = h('span', { class: 'fwh-tick' }); t.style.setProperty('--a', pct(s.from_ms).toFixed(5)); return t; });
      return h('div', { class: 'fwh-lane' },
        h('span', { class: 'fwh-lane-name' }, name),
        h('div', { class: 'fwh-bar', role: 'img', 'aria-label': mine.length ? `${name}: ${mine.map((s) => `${s.version || 'unnamed'} from ${day(s.from_ms)}`).join(', ')}` : `${name}: no fingerprint seen` },
          mine.map((s, i) => laneSeg(s, i, pct)), ticks));
    });
    const changes = Math.max(0, spans.length - mods.filter((m) => spans.some((s) => s.module === m.id)).length);
    return [
      h('div', { class: 'fwh-lanes' }, lanes,
        h('div', { class: 'fwh-lane fwh-axis-lane', 'aria-hidden': 'true' }, h('span', { class: 'fwh-lane-name' }),
          h('div', { class: 'fwh-axis' }, h('span', null, day(from)), h('span', null, spans.some((s) => s.current) ? 'now' : day(to))))),
      note(`${formatNumber(changes)} ${changes === 1 ? 'change' : 'changes'} since ${day(from)}. Each mark is a new fingerprint; the version names are those of the updates they came with (hover or tap for the fingerprint).`),
    ];
  }

  function histRow(r) {
    const isVer = r.kind === 'version';
    const ver = h('span', { class: ['fwh-ver', { 'is-unnamed': !r.version }], title: r.version && SOURCE[r.update?.version_source] ? `${r.version}, ${SOURCE[r.update.version_source]}` : null }, r.version || 'unnamed');
    return h('li', { class: ['fwh-row', { 'is-version': isVer, 'is-current': !!r.current }] },
      h('div', { class: 'fwh-main' },
        h('span', { class: 'fwh-mod' }, isVer ? 'Car version' : moduleName(r.module, r.module_name)),
        isVer ? ver : hexCode(r.fingerprint)),
      h('div', { class: 'fwh-when' },
        h('span', { class: 'fwh-dates' }, `${day(r.first_seen_ms)} – ${r.current ? 'now' : day(r.until_ms ?? r.last_seen_ms)}`),
        h('span', { class: 'fwh-dur' }, `ran ${dur(r.duration_ms)}`)),
      h('div', { class: 'fwh-side' },
        isVer ? null : ver,
        r.current ? chip('Current', { tone: 'teal' }) : null));
  }
  function drawHist() {
    if (histErr) {
      spanBody.replaceChildren(note('The firmware history did not load. Refresh to try again.'));
      listBody.replaceChildren(note('The firmware history did not load. Refresh to try again.'));
      histMore.hidden = true;
      return;
    }
    const d = hist.last || {};
    spanBody.replaceChildren(...drawLanes(d));
    rowList.replaceChildren(...hist.items.map(histRow));
    listBody.replaceChildren(hist.items.length ? rowList
      : note(hMod ? `${moduleName(hMod)}: no fingerprint recorded yet. Some cars do not send one from every module.` : `No fingerprint or imported version recorded for ${car.name} yet.`));
    histMore.hidden = hist.done;
    histMore.querySelector('button').hidden = false;
  }
  // a new pager for the module filter (or a live reload, as many rows as are shown); the old rows stay until it answers
  let histSeq = 0;
  async function readHist(limit = HIST_LIMIT) {
    const my = ++histSeq;
    const next = ctx.api.pager(histPath, hMod ? { module: hMod } : {}, { limit: Math.min(500, Math.max(HIST_LIMIT, limit)) });
    listBody.setAttribute('aria-busy', 'true');
    let err = null;
    await next.more().catch((e) => { err = e; });
    if (!ctx.alive() || my !== histSeq) return;
    listBody.removeAttribute('aria-busy');
    if (err && api.isAbort(err)) return;
    hist = next; histErr = err;
    drawHist();
  }
  function pickModule(m) { hMod = m; readHist(); }
  let liveT = null;
  ctx.events(['rows'], (ev) => {
    if (ev.data?.car && ev.data.car !== car.id) return;
    if (ev.data?.kinds && !ev.data.kinds.includes('updates') && !ev.data.kinds.includes('firmware')) return;
    clearTimeout(liveT);
    liveT = setTimeout(() => { if (ctx.alive()) readHist(hist.items.length); }, 1500);
  });
  ctx.onCleanup(() => clearTimeout(liveT));
  drawHist();

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
  ctx.view.replaceChildren(bar, hero, countT, gapT, tell, fwHead, ...fwTiles, histHead, spanTile, histTile, histMore, listStart, listEnd);
  const last = sm.last_update;
  if (!pager.items.length && last && ctx.period?.preset !== 'all') {
    // a narrow period (say one day carried over from another page): say when the last one was, and offer all time
    listEnd.before(emptyTile({ icon: 'chip', cover: ['updates.table.version', 'updates.table.date', 'updates.table.duration', 'updates.table.since_previous', 'updates.table.charge_count', 'updates.table.avg_range'],
      title: `No updates in this period; the last one was ${formatDate(last.end_ms ?? last.start_ms, tz, { now: api.now(), year: true })}${last.version ? ` (${last.version})` : ''}`,
      text: 'All time shows it and every update before it.',
      action: btn('Show all time', { kind: 'soft', icon: 'calendar', onclick: () => state.setPeriod({ preset: 'all' }, { push: true }) }) }));
  } else if (!pager.items.length) {
    listEnd.before(emptyTile({ icon: 'chip', cover: ['updates.table.version', 'updates.table.date', 'updates.table.duration', 'updates.table.since_previous', 'updates.table.charge_count', 'updates.table.avg_range'], title: 'No updates in this period', text: ctx.period?.preset === 'all' ? `Nothing recorded for ${car.name} yet. An update shows once the car installs one while recording, or with an imported history.` : 'Pick a longer period above; All time shows every update.' }));
  } else {
    draw(pager.items);
    if (!pager.done) listEnd.append(more);
  }
  ctx.ready();
  return {};
}
