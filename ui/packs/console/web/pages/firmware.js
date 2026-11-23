// The car's firmware fingerprints and the "Tesla update installed" markers found from them, for the Software page
// (and the words the timeline and the car overview share).
//
// Tesla's version number is not on CAN, but the main modules broadcast fingerprints of their code on the vehicle bus:
// the front body controller (vcfront), the battery management (bms), the rear drive unit (dir) and the front drive
// unit (dif). GET /cars/{car}/firmware lists every field with its value as lowercase hex (14 digits for a 56-bit git
// hash, 8 for a 32-bit CRC, 4 for a configuration, 1 for a build type) and when that value was first seen.
// An update found from them (/updates rows with detected_by "fingerprint") lists the changed fields, old -> new, and
// sits between the last drive with the old set and the first with the new one. Its version is null until an admin
// types it (PATCH /updates/{id} { version }, null clears it); the server then names every later update with the same
// fingerprint set by itself (named_by "auto"), or from the version a live input (teslafi, local teslamate, the tesla
// api) reported within a day after it (named_by "auto", version_source the input). A typed name always wins.
//
//   firmwarePanel(ctx)          { el, ready, reload() }: ## car firmware, one table per module
//   fingerprintUpdates(ctx, o)  { el, ready, reload(), open(id) }: ## tesla updates installed, a block per update with
//                               its name and, for an admin, [edit] that opens a name box in place
//   MODULE_WORDS, modWords(ids) the modules' names in the page's words

import * as state from '../shared/state.js';
import { h, replace, disclose } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { section, note, link, btn, kv } from '../core/ui.js';
import { tag, dateTime, ago, dur, DASH } from '../core/fmt.js';
import { table } from '../core/table.js';
import { empty, errorWords } from '../core/states.js';
import { toast } from '../core/toast.js';
import { src } from '../core/source.js';

const enc = encodeURIComponent;

export const MODULE_WORDS = { vcfront: 'front body controller', bms: 'battery management', dir: 'rear drive unit', dif: 'front drive unit' };
const SHORT = { vcfront: 'front body', bms: 'battery', dir: 'rear drive', dif: 'front drive' };
export const modWords = (ids, { short = false } = {}) => (ids || []).map((m) => (short ? SHORT : MODULE_WORDS)[m] || m).join(', ');
const VERSION_RX = /^\d{4}\.\d{1,2}(\.\d{1,3}){0,2}$/;
/** Where a version came from (version_source), as a short tag and as words. Every source the server sends. */
export const VERSION_SOURCE = {
  owner: 'typed', build_match: 'same fingerprints', tesla_api: 'from tesla api', teslafi_api: 'from teslafi',
  teslamate_local: 'from teslamate', teslamate: 'teslamate import', teslafi: 'teslafi import',
};
export const VERSION_SOURCE_WORDS = {
  owner: 'typed by an admin', build_match: 'named from an earlier update with the same fingerprints',
  tesla_api: 'read from the tesla api after it', teslafi_api: 'read from teslafi after it', teslamate_local: 'read from local teslamate after it',
  teslamate: 'imported from a teslamate backup', teslafi: 'imported from a teslafi backup',
};
/** The source tag of a version, or null for a source this pack does not know. */
export const versionTag = (vs) => (VERSION_SOURCE[vs] ? tag(VERSION_SOURCE[vs], 'muted') : null);
const none = (t) => h('span', { class: 'muted' }, t);
/** A fingerprint as monospace hex that may break anywhere on a narrow screen. */
export const hexEl = (v) => (v == null ? none(DASH) : h('code', { class: 'hex' }, v));
const canId = (n) => (n == null ? '' : `0x${n.toString(16).toUpperCase()}`);
const sig = (x) => JSON.stringify(x);

/* ------------------------------------------------------------------ ## car firmware */

export function firmwarePanel(ctx) {
  const cid = ctx.car.id, tz = ctx.tz;
  const side = h('span', { class: 'muted' });
  const body = h('div');
  const el = section({ title: 'car firmware', id: 'firmware', side }, body);
  let last = null;
  const tables = [];

  function paint(d) {
    tables.length = 0;
    const mods = d.modules || [];
    replace(side, d.updated_ms ? ['fingerprints seen ', h('span', { title: dateTime(d.updated_ms, tz) }, ago(d.updated_ms))] : 'no fingerprint seen yet');
    replace(body,
      note('Tesla\'s version number is not on the CAN bus, but the main modules send fingerprints of their code on the vehicle bus about once a second. ',
        'Each is shown as lowercase hex: 14 digits for a git hash, 8 for a CRC. A new value means new code in that module: a Tesla update installed.'),
      kv([
        ['updates found', [formatNumber(d.updates ?? 0), none(' from fingerprints, '), link(`${state.buildUrl('software', { car: cid })}#tesla-updates`, 'listed below')]],
        ['last seen', d.updated_ms ? [dateTime(d.updated_ms, tz), none(` (${ago(d.updated_ms)})`)] : none('not yet: the logs carried no fingerprint')],
        ['history', [link(`${state.buildUrl('software', { car: cid })}#firmware-history`, 'every fingerprint so far'), none(', and the versions over time')]],
        ['bridge hints', d.bridge_hints?.count ? [formatNumber(d.bridge_hints.count), none(` "firmware changed" lines in the logs, last ${dateTime(d.bridge_hints.last_ms, tz)}; a hint only`)] : none('none')],
        ['from', src({ sources: [{ kind: 'smo', label: 'Data from SMO' }] }, { full: true })],
      ]),
      mods.map((m) => {
        const head = h('p', { class: 'sub-title fw-mod' }, h('span', { class: 'fw-mod-n' }, MODULE_WORDS[m.id] || m.name || m.id), none(`  ${canId(m.can_id)}${m.name ? `  ${m.name}` : ''}`));
        if (!(m.fields || []).length) return h('div', { class: 'fw-block' }, head, empty('no fingerprints from this module', 'The car\'s logs carried none (an older car may not send them, or the module was asleep in every log).'));
        const t = table(ctx, {
          bare: true, rows: m.fields, csv: false, label: `Firmware fingerprints of the ${MODULE_WORDS[m.id] || m.name}`,
          columns: [
            { key: 'field', label: 'field', ph: 'main', render: (f) => h('span', { class: 'fw-field' }, f.field) },
            { key: 'hex', label: 'fingerprint', ph: 'sub', render: (f) => [h('span', { class: 'ph-k' }, 'now '), hexEl(f.hex), none(` ${f.bits} bit`)] },
            { key: 'first', label: 'first seen', ph: 'sub', render: (f) => [h('span', { class: 'ph-k' }, 'first seen '), dateTime(f.first_seen_ms, tz)] },
            { key: 'hist', label: 'values', num: true, ph: 'num', grow: true, title: 'How many values this field had in the logs',
              render: (f) => ((f.history || []).length > 1
                ? h('span', { title: f.history.map((x) => `${x.hex} from ${dateTime(x.first_seen_ms, tz)}`).join('\n') }, h('span', { class: 'n' }, formatNumber(f.history.length)), none(' values'))
                : none('1 value')) },
          ],
        });
        tables.push(t);
        return h('div', { class: 'fw-block' }, head, t.el);
      }));
  }

  async function reload() {
    let d;
    try { d = await ctx.api.get(`cars/${enc(cid)}/firmware`); } catch (e) {
      if (e?.name === 'AbortError' || !ctx.alive()) return;
      if (!last) { replace(side, tag('not read', 'bad')); replace(body, empty('the firmware could not be read', errorWords(e).text)); }
      return;
    }
    if (!ctx.alive()) return;
    const s = sig(d);
    if (s === last) return;   // nothing new: nothing on the page moves
    last = s;
    paint(d);
    await Promise.all(tables.map((t) => t.ready)).catch(() => {});
  }
  return { el, ready: reload(), reload };
}

/* ------------------------------------------------------------------ ## tesla updates installed */

const driveWords = (d, tz) => (d ? `${d.start?.label || 'a drive'} -> ${d.end?.label || '?'}, ${dateTime(d.start_ms, tz)}` : null);

/**
 * The updates found from fingerprints in the period, newest first, each with what changed, between which drives,
 * and (admins) a box to type its version. onNamed(): after a save or a clear (the page reloads its other lists).
 */
export function fingerprintUpdates(ctx, { onNamed = null } = {}) {
  const cid = ctx.car.id, tz = ctx.tz;
  const path = `cars/${enc(cid)}/updates`;
  const side = h('span', { class: 'muted' });
  const body = h('div');
  const el = section({ title: 'tesla updates installed', id: 'tesla-updates', side }, body);
  let last = null, rows = [];
  const around = new Map();   // update id -> { before, after } drives
  const tables = [];
  let busy = false;
  const boxes = new Map();   // update id -> its name box's disclose()

  async function drivesAround(u) {
    const key = `${u.id}:${u.start_ms}:${u.end_ms}`;
    if (around.has(key)) return around.get(key);
    const [b, a] = await Promise.all([
      ctx.api.get(`cars/${enc(cid)}/drives`, { to_ms: u.start_ms, limit: 1 }).catch(() => null),
      ctx.api.get(`cars/${enc(cid)}/drives`, { from_ms: u.end_ms, to_ms: (u.until_ms ?? ctx.api.now()) + 1, sort: 'start', dir: 'asc', limit: 1 }).catch(() => null),
    ]);
    const v = { before: b?.items?.[0] || null, after: a?.items?.[0] || null };
    around.set(key, v);
    return v;
  }

  // the name is shown in the block; [edit] ([name it] while it has none) opens the box in place, esc or [cancel] closes it
  function nameBox(u) {
    const input = h('input', { class: 'in fw-name-in', type: 'text', value: u.version || '', size: '14', maxlength: '20', autocomplete: 'off', spellcheck: 'false',
      inputmode: 'decimal', placeholder: '2026.38.4', 'aria-label': `Version of the Tesla update installed ${dateTime(u.end_ms, tz)}`, 'aria-describedby': `fw-hint-${u.id}` });
    const msg = h('p', { class: 'ke-msg', role: 'alert', hidden: true });
    const say = (t) => { msg.hidden = !t; replace(msg, t || ''); };
    const save = h('button', { class: 'btn btn--primary', type: 'submit' }, 'save');
    const clear = btn('clear', () => send(null), { title: 'forget the typed version', disabled: !u.version });
    const cancel = btn('cancel', () => dz.close({ focus: true }), { title: 'close without saving (esc)' });
    const edit = h('button', { class: 'btn btn--line tone--accent fw-edit', type: 'button', id: `fw-edit-${u.id}`, title: u.version ? 'type another version for this update' : 'type the version this update installed' },
      u.version ? '[edit]' : '[name it]');
    async function send(v) {
      if (busy) return;
      if (v != null && !VERSION_RX.test(v)) { say('A version as the car\'s screen shows it, like 2026.38.4.'); input.focus(); return; }
      if (v == null && !u.version) { say('Type the version first, like 2026.38.4.'); input.focus(); return; }
      busy = true; save.disabled = clear.disabled = true; say(null);
      try {
        await ctx.api.patch(`${path}/${enc(u.id)}`, { version: v });
        if (!ctx.alive()) return;
        toast(v ? `Saved: this update installed ${v}. Later updates with the same fingerprints get it too.` : 'The version is cleared.', { kind: 'ok' });
        busy = false;
        dz.close();
        await reload(true);
        if (!ctx.alive()) return;
        // the block was drawn again: the focus goes back to this update's [edit]
        document.getElementById(`fw-edit-${u.id}`)?.focus({ preventScroll: true });
        onNamed?.();
      } catch (e) {
        if (!ctx.alive()) return;
        say(e?.fields?.version || errorWords(e).text);
        input.focus();
      } finally { busy = false; save.disabled = false; clear.disabled = !u.version; }
    }
    const f = h('form', { class: 'fw-name', novalidate: true, onsubmit: (e) => { e.preventDefault(); const v = input.value.trim(); send(v || null); } },
      h('label', { class: 'fw-name-k muted', for: `fw-in-${u.id}` }, 'version'), input, h('span', { class: 'acts' }, save, clear, cancel));
    input.id = `fw-in-${u.id}`;
    const part = h('div', { class: 'fw-name-box', id: `fw-box-${u.id}` }, f, h('p', { class: 'fld-hint', id: `fw-hint-${u.id}` }, 'As the car\'s screen shows it (Controls, Software), e.g. 2026.38.4. Empty, or clear, forgets it. Later updates to the same fingerprints are named by themselves.'), msg);
    const dz = disclose(edit, part, { focus: input, onClose: () => { input.value = u.version || ''; say(null); } });
    boxes.set(u.id, dz);
    return { edit, part };
  }

  function block(u, dr) {
    const named = u.version
      ? [h('b', null, u.version), ' ', versionTag(u.version_source) || (u.named_by === 'auto' ? tag('same fingerprints', 'muted') : tag('typed', 'muted'))]
      : h('span', { class: 'tone--warn' }, 'not named yet');
    const t = table(ctx, {
      bare: true, rows: u.changes || [], csv: false, label: `What changed in the Tesla update installed ${dateTime(u.end_ms, tz)}`,
      columns: [
        { key: 'module', label: 'module', ph: 'main', render: (c) => MODULE_WORDS[c.module] || c.module_name || c.module },
        { key: 'field', label: 'field', ph: 'sub', render: (c) => h('span', { class: 'fw-field' }, c.field) },
        { key: 'change', label: 'old -> new', ph: 'sub', grow: true, render: (c) => h('span', { class: 'fw-chg' }, hexEl(c.old), h('span', { class: 'arrow' }, ' -> '), hexEl(c.new)) },
      ],
      empty: () => empty('no field listed'),
    });
    tables.push(t);
    const box = ctx.admin ? nameBox(u) : null;
    const dl = (d) => (d ? link(state.buildUrl('drive', { car: cid, drive: d.id }), driveWords(d, tz)) : none('none in the logs'));
    return h('div', { class: 'fw-upd', id: `upd-${u.id}`, dataset: { row: '' } },
      h('p', { class: 'fw-upd-head' }, tag('installed', 'accent'), ' ', h('b', null, 'Tesla update installed'), ' ', h('span', { class: 'muted' }, dateTime(u.end_ms, tz))),
      kv([
        ['version', box ? [named, ' ', box.edit] : named],
        u.version && u.named_by === 'auto' ? ['named', none(VERSION_SOURCE_WORDS[u.version_source] || 'named from an earlier update with the same fingerprints')] : null,
        ['between', [h('span', { class: 'muted' }, 'last drive before '), dl(dr.before)]],
        ['and', [h('span', { class: 'muted' }, 'first drive after '), dl(dr.after)]],
        ['window', [dateTime(u.between_ms?.[0] ?? u.start_ms, tz), ' .. ', dateTime(u.between_ms?.[1] ?? u.end_ms, tz), none(` (${dur((u.between_ms?.[1] ?? u.end_ms) - (u.between_ms?.[0] ?? u.start_ms))}: old fingerprints last seen, new first seen)`)]],
        ['modules', [modWords(u.changed_modules), none(` (${formatNumber((u.changes || []).length)} field${(u.changes || []).length === 1 ? '' : 's'})`)]],
        ['build id', u.build_id ? hexEl(u.build_id) : none(DASH)],
        ['timeline', link(state.buildUrl('timeline', { car: cid, period: { from: u.start_ms - 3600000, to: u.end_ms + 3600000 } }), 'around it')],
      ], { cols: 1 }),
      box ? box.part : null,
      t.el,
      box ? null : note(tag('admin', 'muted'), u.version ? ' Only an admin can change the name.' : ' Only an admin can name it.'));
  }

  async function reload(force = false) {
    let list;
    try { list = await ctx.api.list(path, { ...ctx.params, limit: 100 }).all({ max: 500 }); } catch (e) {
      if (e?.name === 'AbortError' || !ctx.alive()) return;
      if (last == null) { replace(side, tag('not read', 'bad')); replace(body, empty('the updates could not be read', errorWords(e).text)); }
      return;
    }
    if (!ctx.alive()) return;
    const found = list.filter((u) => u.detected_by === 'fingerprint');
    const s = sig(found);
    if (s === last && !force) return;
    // a name box is open (someone is typing a name): wait, so the box does not change under them
    if (!force && last != null && [...boxes.values()].some((b) => b.isOpen())) return;
    const drs = await Promise.all(found.slice(0, 30).map(drivesAround));
    if (!ctx.alive()) return;
    last = s; rows = found;
    tables.length = 0;
    boxes.clear();
    replace(side, found.length ? `${formatNumber(found.length)} in this range` : 'none in this range');
    replace(body,
      note('Found from the modules\' fingerprints: the old ones were last seen at the start of the window, the new ones first seen at its end. The car does not send the version text: teslafi, local teslamate or the tesla api name it when they report it, or an admin types it once (typed wins); the server then names every later update with the same fingerprints.'),
      found.length ? found.slice(0, 30).map((u, i) => block(u, drs[i]))
        : empty('no update found from fingerprints in this range', 'A Tesla update shows here once the car\'s logs carry new fingerprints. Choose a longer range; all shows every one.'),
      found.length > 30 ? note(`The newest 30 of ${formatNumber(found.length)} are shown; a shorter range shows older ones.`) : null);
    await Promise.all(tables.map((t) => t.ready)).catch(() => {});
  }
  // open(id): opens that update's name box (the [edit] in the versions table)
  return { el, ready: reload(), reload, rows: () => rows, open: (id) => boxes.get(id)?.open() };
}
