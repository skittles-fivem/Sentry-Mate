// Sentry Mate, "Pebble" UI pack: the car's firmware fingerprints and the "Tesla update installed" marker.
//
// Tesla's version number is not on the CAN bus, but four modules broadcast fingerprints of their code (CRCs and
// git hashes, raw integers shown as lowercase hex, 14 digits for a 56-bit hash, 8 for a 32-bit CRC). When they
// change, the server makes an update with detected_by "fingerprint": which fields changed (old hex to new hex) and
// when, between the last drive with the old set and the first with the new one. Its version is not known until an
// admin types it; later updates to the same set are then named by themselves (named_by "auto").
//
//   MODULES, moduleName(id)        the four modules in words
//   fieldName(signal)              "App CRC" for VCFRONT_infoAppCrc (the DBC name stays beside it, small)
//   hexCode(hex)                   the hex in the monospace face, never translated, breaking anywhere on a phone
//   isFingerprint(r)               an update found from the fingerprints
//   changesList(r)                 its changes, per module: field, old hex -> new hex
//   aroundDrives(ctx, carId, r)    the drive before and the drive after it (two reads), for betweenLine()
//   betweenLine(ctx, carId, r, d)  "Between the drive Ocean Beach to Home (…) and the drive Home to Work (…)"
//   namedLine(r)                   "Named from an earlier update with the same fingerprints", or null
//   nameBox(ctx, carId, r, { onSaved })  the version name and who gave it; admins open a box with Edit to type or
//                                  clear it (PATCH updates/{update}), Escape or Cancel closes it; others read
//   HISTORY_ID, historyHref(carId) the "Module firmware history" section on Software (#firmware-history), linked from Car
//
// Styles: app.css (.fw-*).

import { h, disclose } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatDateTime } from '../shared/time.js';
import { icon, btn } from './ui.js';
import { endLabel } from './bits.js';
import { errorWords } from './states.js';
import { setFieldError } from './form.js';

export const MODULES = Object.freeze({ vcfront: 'Front body controller', bms: 'Battery management', dir: 'Rear drive unit', dif: 'Front drive unit' });
export const moduleName = (id, fallback = null) => MODULES[id] || fallback || id;
export const moduleList = (ids) => (ids || []).map((m) => moduleName(m)).join(', ');
export const isFingerprint = (r) => !!r && r.detected_by === 'fingerprint';
export const VERSION_RX = /^\d{4}\.\d{1,2}(\.\d{1,3}){0,2}$/;
/** The anchor of the module firmware history on Software, and a link to it. */
export const HISTORY_ID = 'firmware-history';
export const historyHref = (carId) => `${state.buildUrl('software', { car: carId })}#${HISTORY_ID}`;

// the DBC's signal names in words, by what follows the module prefix
const FIELD_WORDS = [
  [/^(info)?app(lication)?crc$/i, 'App CRC'],
  [/^(info)?boot(loader)?crc$/i, 'Boot CRC'],
  [/^(info)?bootgithash$/i, 'Boot git hash'],
  [/^(info)?appgithash$/i, 'App git hash'],
  [/^subcomponentgithash$/i, 'Subcomponent git hash'],
  [/^(info)?buildtype$/i, 'Build type'],
  [/^(info)?buildconfig(uration)?id$/i, 'Build configuration'],
  [/^hcmlappcrc$/i, 'Left headlight CRC'],
  [/^hcmrappcrc$/i, 'Right headlight CRC'],
  [/^oilpumpappcrc$/i, 'Oil pump CRC'],
  [/^oilpumpbuildtype$/i, 'Oil pump build type'],
];
/** A signal name in words: "App CRC" for VCFRONT_infoAppCrc; an unknown one split at its capitals. */
export function fieldName(signal) {
  const rest = String(signal || '').replace(/^[A-Z]+_/, '');
  for (const [rx, words] of FIELD_WORDS) if (rx.test(rest)) return words;
  const w = rest.replace(/([a-z0-9])([A-Z])/g, '$1 $2').replace(/_/g, ' ').trim();
  return w ? w.charAt(0).toUpperCase() + w.slice(1) : String(signal || '');
}

/** The hex as the API gives it (lowercase, zero padded), in the monospace face. */
export const hexCode = (hex, cls = null) => h('code', { class: ['fw-hex', cls], translate: 'no' }, String(hex ?? '').toLowerCase());

/** The changes of a fingerprint update, per module: each field with old hex -> new hex. */
export function changesList(r, { cls = null } = {}) {
  const by = new Map();
  for (const c of r.changes || []) {
    if (!by.has(c.module)) by.set(c.module, { name: moduleName(c.module, c.module_name), rows: [] });
    by.get(c.module).rows.push(c);
  }
  if (!by.size) {
    return (r.changed_modules || []).length ? h('p', { class: 'note' }, `Changed: ${moduleList(r.changed_modules)}.`) : null;
  }
  return h('div', { class: ['fw-changes', cls] }, [...by.values()].map((m) => h('section', { class: 'fw-mod' },
    h('h3', { class: 'fw-mod-name' }, m.name),
    h('ul', { class: 'fw-list' }, m.rows.map((c) => h('li', { class: 'fw-change' },
      h('span', { class: 'fw-k' }, h('span', { class: 'fw-field' }, fieldName(c.field)), h('span', { class: 'fw-sig' }, c.field)),
      h('span', { class: 'fw-diff' },
        hexCode(c.old, 'is-old'),
        h('span', { class: 'fw-to' }, icon('arrowRight'), h('span', { class: 'sr-only' }, ' changed to ')),
        hexCode(c.new, 'is-new'))))))));
}

/** The drive that ended last before the old fingerprints were last seen, and the first one after the new ones were. */
export async function aroundDrives(ctx, carId, r) {
  const [a, b] = r.between_ms || [r.start_ms, r.end_ms];
  if (a == null && b == null) return { before: null, after: null };
  const path = api.apiPath`cars/${carId}/drives`;
  const one = (p) => ctx.api.page(path, p, { limit: 1 }).then((x) => (x.items || [])[0] || null).catch(() => null);
  const [before, after] = await Promise.all([
    a != null ? one({ to_ms: a + 1 }) : null,
    b != null ? one({ from_ms: b, sort: 'start', dir: 'asc' }) : null,
  ]);
  return { before, after };
}

/** "Between <the drive before> and <the drive after>", with links; the times the old and new sets were seen. */
export function betweenLine(ctx, carId, r, d, tz) {
  const now = api.now();
  const at = (ms) => formatDateTime(ms, tz, { now });
  const drive = (x) => h('a', { class: 'place-link', href: state.buildUrl('drive', { car: carId, drive: x.id }) },
    `${endLabel(x.start)} to ${x.end_ms ? endLabel(x.end) : 'now'}`, ` (${at(x.start_ms)})`);
  const [a, b] = r.between_ms || [r.start_ms, r.end_ms];
  const before = d && d.before, after = d && d.after;
  return h('div', { class: 'fw-between' },
    h('p', { class: 'fw-between-line' },
      before && after ? ['Installed between the drive ', drive(before), ' and the drive ', drive(after), '.']
        : before ? ['Installed after the drive ', drive(before), '; no drive with the new fingerprints yet.']
          : after ? ['Installed before the drive ', drive(after), '; no drive recorded with the old fingerprints.']
            : 'No drive recorded on either side of it.'),
    h('p', { class: 'note' }, r.readings_gap && a != null && b != null
      ? `Installed between ${at(a)} and ${at(b)} (no readings in between): the old fingerprints were last seen then, the new ones first seen then.`
      : `Old fingerprints last seen ${a != null ? at(a) : 'not known'}; new ones first seen ${b != null ? at(b) : 'not known'}.`));
}

/**
 * Where a version name came from (version_source), every source the server sends:
 * VERSION_SOURCE words for a line or a title, VERSION_CHIP a chip, VERSION_CHIP_SHORT the chip on an update tile.
 */
export const VERSION_SOURCE = {
  owner: 'typed by the owner', build_match: 'named from an earlier update with the same fingerprints',
  tesla_api: 'from the Tesla API', teslafi_api: 'from TeslaFi', teslamate_local: 'from TeslaMate',
  teslamate: 'imported from TeslaMate', teslafi: 'imported from TeslaFi',
};
export const VERSION_CHIP = {
  owner: 'Typed by the owner', build_match: 'Same fingerprints', tesla_api: 'From the Tesla API', teslafi_api: 'From TeslaFi',
  teslamate_local: 'From TeslaMate', teslamate: 'Imported from TeslaMate', teslafi: 'Imported from TeslaFi',
};
export const VERSION_CHIP_SHORT = {
  owner: 'Typed', build_match: 'Same fingerprints', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'TeslaMate',
  teslamate: 'TeslaMate import', teslafi: 'TeslaFi import',
};
/** The live inputs that name a fingerprint update with the version they reported after it, in a sentence. */
const INPUT_NAME = { tesla_api: 'the Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'TeslaMate' };
/** How an auto-named update got its name, as a sentence (null when it was typed or came with its source). */
export const namedWords = (r) => (!r || !r.version || r.version_source === 'owner' || r.named_by === 'owner' ? null
  : INPUT_NAME[r.version_source] ? `Named from ${INPUT_NAME[r.version_source]}: it reported this version after the update.`
    : r.named_by === 'auto' || r.version_source === 'build_match' ? 'Named from an earlier update with the same fingerprints.' : null);

/** The line of an update named by itself: from a live input that reported it, or from an earlier one with the same fingerprints. */
export const namedLine = (r) => {
  const w = namedWords(r);
  return w ? h('p', { class: 'fw-named' }, icon(INPUT_NAME[r.version_source] ? 'download' : 'copy'), h('span', null, w)) : null;
};

let seq = 0;
const NAME_WORDS = {
  not_allowed: 'Only an admin can name an update.',
  no_such_update: 'This update is not here any more: the logs changed it (another field joined it). The page now shows it again.',
};

/** After a save the page is drawn again: the focus goes back to that update's Edit button once it is there. */
function refocus(updateId, tries = 50) {
  const b = document.querySelector(`.fw-name-edit[data-update="${CSS.escape(String(updateId))}"]`);
  if (b && b.isConnected) { b.focus({ preventScroll: true }); return; }
  if (tries > 0) setTimeout(() => refocus(updateId, tries - 1), 100);
}

/**
 * The version name of a fingerprint update: the name (or "Not named yet") and who gave it. Admins also get an
 * "Edit" ("Name it" while it has none) button that opens the box in place: type it, Save, Clear (null forgets
 * it), Cancel or Escape closes it again. onSaved(text) runs after the server took it (the page reads again,
 * since other updates with the same fingerprints change too).
 */
export function nameBox(ctx, carId, r, { onSaved = null } = {}) {
  const words = r.version ? (r.named_by === 'owner' || r.version_source === 'owner' ? 'Typed by an admin.' : namedWords(r) || 'Named by its source.')
    : ctx.admin ? 'Type the version the car shows (Controls, Software) once it is known.' : 'An admin can type the version the car shows (Controls, Software) once it is known.';
  const shown = h('div', { class: 'fw-name-shown' },
    h('div', { class: 'fw-name-text' },
      h('p', { class: 'fw-name-k' }, 'Version'),
      h('p', { class: ['fw-name-v', { 'is-unknown': !r.version }] }, r.version || 'Not named yet'),
      h('p', { class: 'note' }, words)));
  if (!ctx.admin) return h('div', { class: 'fw-name is-read' }, shown);
  const id = `fw-name-${++seq}`;
  const edit = btn(r.version ? 'Edit' : 'Name it', { kind: 'soft', icon: 'edit', cls: 'btn-sm fw-name-edit',
    title: r.version ? 'Type another name for this update' : 'Type the version this update installed', attrs: { 'data-update': String(r.id) } });
  shown.append(edit);
  const input = h('input', {
    id, class: 'input fw-input', name: 'version', value: r.version || '', placeholder: '2026.38.4', autocomplete: 'off', spellcheck: 'false', inputmode: 'decimal',
    maxlength: '20', 'aria-describedby': `${id}-hint ${id}-err`,
  });
  input.value = r.version || '';
  const err = h('p', { class: 'field-error', id: `${id}-err`, role: 'alert', hidden: true });
  const cancel = btn('Cancel', { kind: 'soft', title: 'Close without saving (Escape)' });
  const fieldEl = h('div', { class: 'field fw-field-box' },
    h('label', { class: 'label', for: id }, 'Version name'),
    h('div', { class: 'fw-name-row' }, input,
      h('span', { class: 'fw-name-acts' },
        btn('Save', { kind: 'primary', type: 'submit', icon: 'check' }),
        r.version ? btn('Clear', { kind: 'soft', icon: 'close', onclick: () => save(null) }) : null,
        cancel)),
    h('p', { class: 'hint', id: `${id}-hint` }, 'As the car shows it (Controls, Software), like 2026.38.4. Other updates with the same fingerprints get it too; Clear forgets it.'),
    err);
  fieldEl.control = input;
  const form = h('form', { class: 'fw-name-form', novalidate: true }, fieldEl);
  const dz = disclose(edit, form, { focus: input, onClose: () => { input.value = r.version || ''; setFieldError(fieldEl, null); } });
  cancel.addEventListener('click', () => dz.close({ focus: true }));
  let busy = false;
  async function save(text) {
    if (busy) return;
    setFieldError(fieldEl, null);
    if (text && !VERSION_RX.test(text)) { setFieldError(fieldEl, 'Type it like 2026.38.4: the year, the week and the build.'); input.focus(); return; }
    if (text && text === r.version && (r.named_by === 'owner' || r.version_source === 'owner')) { setFieldError(fieldEl, 'That is its name already.'); return; }
    if (!text && !r.version) { setFieldError(fieldEl, 'Type the version first.'); input.focus(); return; }
    busy = true;
    form.setAttribute('aria-busy', 'true');
    for (const b of form.querySelectorAll('button')) b.disabled = true;
    try {
      await ctx.api.patch(api.apiPath`cars/${carId}/updates/${r.id}`, { version: text || null });
      api.invalidate('');
      ctx.toast(text ? `Named ${text}.` : 'Name cleared.', { next: true });
      dz.close({ focus: true });
      onSaved?.(text);
      refocus(r.id);
    } catch (e) {
      if (api.isAbort(e)) return;
      if (e && e.code === 'no_such_update') { ctx.toast(NAME_WORDS.no_such_update, { kind: 'info', next: true }); onSaved?.(null); return; }
      const f = e && e.fields && (e.fields.version || Object.values(e.fields)[0]);
      setFieldError(fieldEl, NAME_WORDS[e && e.code] || (f ? String(f) : null) || `Not saved: ${errorWords(e).text}`);
      input.focus();
    } finally {
      busy = false;
      form.removeAttribute('aria-busy');
      for (const b of form.querySelectorAll('button')) b.disabled = false;
    }
  }
  form.addEventListener('submit', (e) => { e.preventDefault(); save(input.value.trim()); });
  return h('div', { class: 'fw-name' }, shown, form);
}
