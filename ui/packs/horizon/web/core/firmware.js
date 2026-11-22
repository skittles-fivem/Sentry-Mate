// The car's firmware as its modules broadcast it, and the "Tesla update installed" marker found from it.
// Tesla's version number is not on CAN, but the front body controller, the battery management, the rear and
// the front drive unit each send fingerprints of their code (CRCs and git hashes) on the vehicle bus. The
// server decodes them (GET /cars/{car}/firmware); when they change it records an update with
// detected_by "fingerprint" (in /updates and on the timeline): which fields changed, old hex to new hex,
// between which drives. Its version stays unnamed until an admin types it (PATCH /updates/{id}); updates
// with the same fingerprint set are then named by themselves ("named from an earlier update with the same
// fingerprints").
//
//   firmwarePanel(ctx, car)        the "Car firmware" HUD (#firmware): { el, load(), refresh() }
//   fingerprintPanel(ctx, car)     the "Tesla updates installed" HUD: each with its changes, drives and name box
//   nameDialog(ctx, car, u, done)  the name box in a dialog (the timeline's "Name it")
//   isFingerprint(u), MODULE, moduleWords(list), changeList(u), hexCode(hex)

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, hud, btn, tag, note } from './ui.js';
import { field, form } from './form.js';
import { openDialog, toast } from './layer.js';
import { emptyState, errorWords } from './states.js';
import { date, dateTime, ago, DASH } from './fmt.js';
import { isAdmin } from './prefs.js';

const enc = encodeURIComponent;
export const MODULE = Object.freeze({ vcfront: 'Front body controller', bms: 'Battery management', dir: 'Rear drive unit', dif: 'Front drive unit' });
const MODULE_ORDER = ['vcfront', 'bms', 'dir', 'dif'];
export const isFingerprint = (u) => u?.detected_by === 'fingerprint';
export const moduleWords = (list) => (list || []).map((m) => MODULE[m] || m);
export const DETECTED_WORD = 'Tesla update installed';
const VERSION_RX = /^\d{4}\.\d{1,2}(\.\d{1,3}){0,2}$/;

/** A fingerprint: lowercase hex in monospace, kept whole. */
export const hexCode = (hex, cls) => h('code', { class: ['fw-hex', cls], translate: 'no' }, String(hex ?? '').toLowerCase());
/** A DBC signal name that may break after its underscores. */
const sigName = (t) => h('span', { class: 'fw-sig' }, String(t || '').split(/(?<=_)/).flatMap((p, i) => (i ? [h('wbr'), p] : [p])));
const canId = (n) => (n == null ? null : `0x${Number(n).toString(16).toUpperCase().padStart(3, '0')}`);

/** The changed fields of a fingerprint update, grouped by module: field, old hex → new hex. */
export function changeList(u) {
  const by = new Map();
  for (const c of u?.changes || []) { if (!by.has(c.module)) by.set(c.module, []); by.get(c.module).push(c); }
  const mods = [...by.keys()].sort((a, b) => MODULE_ORDER.indexOf(a) - MODULE_ORDER.indexOf(b));
  if (!mods.length) return h('p', { class: 'faint small' }, u?.changed_modules?.length ? `Changed: ${moduleWords(u.changed_modules).join(', ')}.` : 'No field is listed for this update.');
  return h('ul', { class: 'fw-changes', 'aria-label': 'Fingerprints that changed' }, mods.map((m) => h('li', { class: 'fw-chg-mod' },
    h('p', { class: 'fw-chg-name' }, icon('chip', 'i--sm'), h('b', null, by.get(m)[0].module_name || MODULE[m] || m)),
    h('ul', { class: 'fw-chg-fields' }, by.get(m).map((c) => h('li', { class: 'fw-chg' },
      sigName(c.field),
      h('span', { class: 'fw-chg-hex' }, hexCode(c.old, 'is-old'), h('span', { class: 'fw-arrow', 'aria-label': 'changed to' }, '→'), hexCode(c.new, 'is-new'))))))));
}

/** Words for how an update got its name. */
export function nameWords(u) {
  if (!u?.version) return null;
  if (u.named_by === 'auto' || u.version_source === 'build_match') return 'named from an earlier update with the same fingerprints';
  if (u.named_by === 'owner' || u.version_source === 'owner') return 'typed by an admin';
  if (u.version_source === 'tesla_api') return 'named by the Tesla API';
  if (u.version_source === 'teslamate') return 'from the imported TeslaMate history';
  return null;
}

/**
 * The name box: a version field with Save, and Clear when it has one. Admins only (others get a line that
 * says who can). done(updated) runs after a save. Returns an element.
 */
export function nameBox(ctx, car, u, done, { dialog = null } = {}) {
  if (!isAdmin()) return h('p', { class: 'fw-name-ro small faint' }, u.version ? 'Only an admin can change the name.' : 'Not named yet. An admin can type the version the car screen shows.');
  const input = field({ label: 'Version name', name: 'version', path: 'version', value: u.version || '',
    hint: 'As the car screen shows it, such as 2026.38.4. Later updates to the same fingerprints get it too.',
    attrs: { autocomplete: 'off', spellcheck: 'false', inputmode: 'decimal', placeholder: '2026.38.4', maxlength: '20', class: 'input mono' } });
  const path = `cars/${enc(car.id)}/updates/${enc(u.id)}`;
  async function save(version) {
    const r = await ctx.api.patch(path, { version });
    if (!ctx.alive()) return;
    toast(version ? `Saved: this update installed ${version}.` : 'The name is cleared. This update has none now.');
    dialog?.close();
    done?.(r);
  }
  const clear = u.version ? btn('Clear the name', async (e) => {
    const b = e.currentTarget; b.disabled = true;
    try { await save(null); } catch (err) { f.setMessage(err?.code === 'no_such_update' ? 'This update is gone: newer logs changed it. Load the page again.' : errorWords(err).text); } finally { b.disabled = false; }
  }, { kind: 'ghost', sm: true, icon: 'x', title: 'Forget the name: this update then has none, even where its fingerprints are named elsewhere.' }) : null;
  const f = form({ fields: [input], submit: u.version ? 'Save the name' : 'Name it', secondary: clear, async onSubmit(v) {
    const text = String(v.version || '').trim();
    if (!text) { input.setError(u.version ? 'Type a version, or press Clear the name.' : 'Type the version, such as 2026.38.4.'); input.input.focus(); return; }
    if (!VERSION_RX.test(text)) { input.setError('A version looks like 2026.38.4: the year, a dot, then one or two numbers.'); input.input.focus(); return; }
    try { await save(text); } catch (err) {
      if (err?.code === 'no_such_update') throw Object.assign(err, { message: 'This update is gone: newer logs changed it. Load the page again.' });
      throw err;
    }
  } });
  f.el.classList.add('fw-name');
  return f.el;
}

/** The name box in a dialog, for the timeline. */
export function nameDialog(ctx, car, u, done) {
  const tz = car.time_zone || ctx.tz;
  let d = null;
  const holder = h('div', { class: 'stack' });
  d = openDialog({ title: `${DETECTED_WORD}, ${date(u.end_ms ?? u.start_ms, tz, { year: true })}`, body: [
    note('The car does not send its version text. Type the one its screen showed after this update (Controls, Software).'),
    changeList(u), holder] });
  replace(holder, nameBox(ctx, car, u, done, { dialog: d }));
  d.querySelector('input[name="version"]')?.focus();
  return d;
}

/* ---------- the drives on each side of an update */
const driveCache = new Map();
async function sideDrives(ctx, car, u) {
  const key = `${car.id}:${u.id}:${u.start_ms}:${u.end_ms}`;
  if (driveCache.has(key)) return driveCache.get(key);
  const base = `cars/${enc(car.id)}/drives`;
  const [before, after] = await Promise.all([
    ctx.api.get(base, { to_ms: u.start_ms, limit: 1, sort: 'start', dir: 'desc' }).catch(() => null),
    ctx.api.get(base, { from_ms: u.end_ms ?? u.start_ms, limit: 1, sort: 'start', dir: 'asc' }).catch(() => null),
  ]);
  const r = { before: before?.items?.[0] || null, after: after?.items?.[0] || null, failed: !before && !after };
  driveCache.set(key, r);
  return r;
}
function driveLink(car, d, tz, empty) {
  if (!d) return h('span', { class: 'faint' }, empty);
  const where = d.end?.label || d.start?.label;
  return h('a', { class: 'plc', href: state.buildUrl('drive', { car: car.id, drive: d.id }) }, dateTime(d.start_ms, tz, { year: true }), where ? ` · ${where}` : '');
}

/* ---------- the Car firmware panel */
export function firmwarePanel(ctx, car) {
  const tz = car.time_zone || ctx.tz;
  const side = h('span', { class: 'trk-count' });
  const body = h('div', { class: 'fw' });
  const el = hud({ title: 'Car firmware', icon: 'chip', id: 'firmware', side, cls: 'fw-panel' }, body);
  let last = '';
  async function load() {
    let fw;
    try { fw = await ctx.api.get(`cars/${enc(car.id)}/firmware`); } catch (e) {
      if (e?.name === 'AbortError') throw e;
      replace(side, '');
      replace(body, emptyState({ icon: 'warn', title: 'Firmware could not be read', text: errorWords(e).text }));
      last = '';
      return;
    }
    if (!ctx.alive()) return;
    // the values and their history: last_seen_ms moves on with every log, which only the side line shows
    replace(side, fw.updated_ms ? `last seen ${ago(fw.updated_ms)}` : 'none seen yet');
    const sig = JSON.stringify([(fw.modules || []).map((m) => [m.id, (m.fields || []).map((f) => [f.field, f.hex, f.first_seen_ms, (f.history || []).map((x) => [x.hex, x.first_seen_ms])])]), fw.updates, fw.bridge_hints?.count]);
    if (sig === last) return;
    last = sig;
    paint(fw);
  }
  // earlier values: the list is made only while it is open (a closed one holds nothing)
  function histEl(hist) {
    const list = h('ul');
    const d = h('details', { class: 'fw-hist' }, h('summary', null, `${formatNumber(hist.length)} earlier ${hist.length === 1 ? 'value' : 'values'}`), list);
    d.addEventListener('toggle', () => replace(list, d.open ? [...hist].reverse().map((x) => h('li', null, hexCode(x.hex, 'is-old'), h('span', { class: 'fw-seen' }, `${date(x.first_seen_ms, tz, { year: true })} to ${date(x.last_seen_ms, tz, { year: true })}`))) : []));
    return d;
  }
  function paint(fw) {
    const mods = [...(fw.modules || [])].sort((a, b) => MODULE_ORDER.indexOf(a.id) - MODULE_ORDER.indexOf(b.id));
    const any = mods.some((m) => m.fields?.length);
    replace(body,
      h('p', { class: 'small fw-intro' }, 'Tesla\'s version number is not on the CAN bus, but these modules send fingerprints of their code about once a second. A change of fingerprints is a Tesla update installed. Each value is taken only once it was seen three times while the car was awake.'),
      any ? null : emptyState({ icon: 'chip', title: 'No fingerprint seen yet', text: 'They show once SMO has a log of the car awake. Some cars (an older Model S) send none.' }),
      h('div', { class: 'fw-mods' }, mods.map((m) => h('section', { class: 'fw-mod', 'aria-label': m.name || MODULE[m.id] || m.id },
        h('p', { class: 'fw-mod-head' }, h('b', null, m.name || MODULE[m.id] || m.id), m.can_id != null ? h('span', { class: 'fw-can', title: 'The CAN message it comes in' }, canId(m.can_id)) : null),
        m.fields?.length
          ? h('ul', { class: 'fw-fields' }, m.fields.map((f) => {
            const hist = (f.history || []).filter((x) => x.hex !== f.hex);
            return h('li', { class: 'fw-field' },
              h('span', { class: 'fw-fname' }, sigName(f.field)),
              hexCode(f.hex),
              h('span', { class: 'fw-seen' }, 'since ', h('span', { title: dateTime(f.first_seen_ms, tz, { year: true }) }, date(f.first_seen_ms, tz, { year: true }))),
              hist.length ? histEl(hist) : null);
          }))
          : h('p', { class: 'faint small' }, 'Nothing from this module in the logs.')))),
      h('p', { class: 'fw-foot small' },
        h('span', null, `${formatNumber(fw.updates ?? 0)} ${fw.updates === 1 ? 'update' : 'updates'} found from fingerprints`),
        fw.bridge_hints?.count ? h('span', null, ` · the CAN Bridge noted a firmware change ${formatNumber(fw.bridge_hints.count)} ${fw.bridge_hints.count === 1 ? 'time' : 'times'}${fw.bridge_hints.last_ms ? `, last ${date(fw.bridge_hints.last_ms, tz, { year: true })}` : ''} (a hint only)`) : null));
  }
  return { el, load, refresh: () => load().catch(() => {}) };
}

/* ---------- the Tesla updates installed panel */
export function fingerprintPanel(ctx, car, { onNamed } = {}) {
  const tz = car.time_zone || ctx.tz;
  const side = h('span', { class: 'trk-count' });
  const body = h('div', { class: 'stack' });
  const el = hud({ title: 'Tesla updates installed', icon: 'chip', tone: 'cyan', side, id: 'installed' }, body);
  let last = '';
  const cards = new Map();
  async function load() {
    let items;
    try { items = (await ctx.api.get(`cars/${enc(car.id)}/updates`, { limit: 50 })).items || []; } catch (e) {
      if (e?.name === 'AbortError') throw e;
      replace(body, emptyState({ icon: 'warn', title: 'Updates could not be read', text: errorWords(e).text }));
      last = '';
      return;
    }
    if (!ctx.alive()) return;
    const fp = items.filter(isFingerprint);
    const sig = JSON.stringify(fp);
    if (sig === last) return;
    last = sig;
    replace(side, fp.length ? `${formatNumber(fp.length)} found` : '');
    if (!fp.length) {
      replace(body, emptyState({ icon: 'chip', title: 'None found yet', text: 'An update shows here once the modules\' fingerprints change between two times the car was awake.' }));
      return;
    }
    // a card that did not change keeps its element (and any half-typed name)
    const next = new Map();
    const els = fp.map((u) => {
      const k = JSON.stringify(u);
      const old = cards.get(u.id);
      const c = old && old.k === k ? old : { k, el: card(u) };
      next.set(u.id, c);
      return c.el;
    });
    cards.clear(); for (const [i, c] of next) cards.set(i, c);
    replace(body, h('ol', { class: 'fw-ups' }, els));
  }
  function card(u) {
    const drives = h('dd', null, h('span', { class: 'faint' }, 'Looking…'));
    const drivesTo = h('dd', null, h('span', { class: 'faint' }, 'Looking…'));
    sideDrives(ctx, car, u).then((r) => {
      if (!ctx.alive()) return;
      replace(drives, r.failed ? h('span', { class: 'faint' }, 'could not be read') : driveLink(car, r.before, tz, 'none before it'));
      replace(drivesTo, r.failed ? h('span', { class: 'faint' }, 'could not be read') : driveLink(car, r.after, tz, 'none since'));
    });
    const how = nameWords(u);
    const [from, to] = u.between_ms || [u.start_ms, u.end_ms];
    return h('li', { class: 'fw-up' },
      h('div', { class: 'fw-up-head' },
        h('p', { class: 'fw-up-title' }, tag(DETECTED_WORD, 'cyan'), h('b', null, date(to ?? from, tz, { year: true }))),
        h('p', { class: 'fw-up-ver' }, u.version ? h('span', { class: 'sw-ver' }, u.version) : h('span', { class: 'sw-ver is-unknown' }, 'Version not named'),
          how ? h('span', { class: 'fw-up-how' }, how) : null)),
      h('div', { class: 'fw-up-main' },
      h('dl', { class: 'kv fw-up-kv' },
        h('dt', null, 'Between'), h('dd', null, `${dateTime(from, tz, { year: true })} and ${dateTime(to, tz, { year: true })}`, h('span', { class: 'faint' }, ' (old fingerprints last seen, new ones first seen)')),
        h('dt', null, 'Last drive before'), drives,
        h('dt', null, 'First drive after'), drivesTo,
        h('dt', null, 'Modules'), h('dd', null, moduleWords(u.changed_modules).join(', ') || DASH)),
      changeList(u)),
      nameBox(ctx, car, u, () => { last = ''; load(); onNamed?.(); }));
  }
  return { el, load, refresh: () => { load().catch(() => {}); } };
}
