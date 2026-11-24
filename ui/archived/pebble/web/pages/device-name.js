// Sentry Mate, "Pebble" UI pack: what a CAN Bridge key is called, and naming it (Health, Settings: Cars).
// - deviceText(k): its chosen name, or the automatic label in muted text while no admin named it.
// - keyRows(ctx, keys, draw): one row per key; draw(k, renameButton) builds it (the button is null for a viewer
//   who is not an admin). A saved name redraws that row in place.
// The name is asked for in a small sheet; an empty name clears it. PATCH can-keys/{key} { name } (admin).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { deviceName, checkDeviceName, renameKey, DEVICE_NAME_MAX } from '../shared/devices.js';
import { btn } from '../core/ui.js';
import { openSheet } from '../core/sheet.js';
import { field, setFieldError } from '../core/form.js';
import { notSaved } from '../core/settings.js';

/** The name of a key, muted while it is the automatic label. */
export function deviceText(k) {
  const d = deviceName(k);
  return h('span', { class: ['dev-name', { muted: !d.named }], title: d.named ? null : 'Not named yet' }, d.text);
}

/** Asks for a new name in a sheet; resolves with the saved key row, or null. */
export function nameDevice(ctx, k) {
  const d = deviceName(k);
  const f = field({ label: 'Name', name: 'name', value: d.named ? d.text : '', hint: `Up to ${DEVICE_NAME_MAX} characters, such as “Comet’s logger”. Empty it to go back to “${deviceName({ key: k.key }).text}”.`,
    attrs: { maxlength: String(DEVICE_NAME_MAX), autocomplete: 'off', spellcheck: 'false', 'data-autofocus': '' } });
  let saved = null;
  const save = async () => {
    const text = f.control.value;
    const why = checkDeviceName(text);
    setFieldError(f, why);
    if (why) { f.control.focus(); return false; }
    try {
      saved = await renameKey(ctx.api, k.key, text);
      api.invalidate('');
      ctx.toast(text.trim() ? `Key ${k.key} is now called ${text.trim()}.` : `Key ${k.key} has no name now.`);
      return true;
    } catch (err) {
      if (err && err.fields && err.fields.name) setFieldError(f, err.fields.name);
      else ctx.toast(notSaved(err), { kind: 'error' });
      return false;
    }
  };
  f.control.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); f.closest('dialog')?.querySelector('.sheet-actions .btn-primary')?.click(); } });
  return openSheet({
    title: d.named ? `Rename ${d.text}` : 'Name this device', center: true,
    body: h('div', { class: 'form' }, h('p', { class: 'sheet-text' }, `CAN Bridge key ${k.key}. The name shows wherever this logger is listed.`), f),
    actions: [{ label: 'Cancel', value: false }, { label: 'Save', kind: 'primary', value: true, onclick: save }],
  }).then((v) => (v === true && saved ? saved : null));
}

/** The pencil button that names a key (admins); onSaved(row) after a save. */
export function renameButton(ctx, k, onSaved) {
  const d = deviceName(k);
  return btn(d.named ? `Rename ${d.text}` : 'Name this device', { kind: 'ghost', icon: 'edit', iconOnly: true, cls: ['btn-sm', 'dev-rename'], onclick: async () => {
    const row = await nameDevice(ctx, k);
    if (row) onSaved(row);
  } });
}

/** One row per key, each redrawn in place once it is renamed: draw(k, renameButtonOrNull) -> element. */
export function keyRows(ctx, keys, draw) {
  return keys.map((k0) => {
    let el = null;
    const make = (k) => draw(k, ctx.admin ? renameButton(ctx, k, (row) => {
      const next = make({ ...k, ...row });
      el.replaceWith(next);
      el = next;
      next.querySelector('.dev-rename')?.focus({ preventScroll: true });
    }) : null);
    el = make(k0);
    return el;
  });
}
