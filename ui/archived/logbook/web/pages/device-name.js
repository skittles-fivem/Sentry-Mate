// Sentry Mate, "Logbook" UI pack: what a CAN Bridge key is called, and naming it (Health, Settings: Cars).
// - deviceText(k, cls): its chosen name, or the automatic label in quiet italics while no admin named it.
// - renameButton(ctx, k, onSaved): "Rename" (or "Name it") for admins (ctx.me.role), null for others. A small
//   dialog asks for the name; an empty one clears it. onSaved(row) gets the key row the server answered.
// PATCH can-keys/{key} { name } (admin).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { deviceName, checkDeviceName, renameKey, DEVICE_NAME_MAX } from '../shared/devices.js';
import { button } from '../core/ui.js';
import { field, setFieldError } from '../core/form.js';
import { openDialog } from '../core/dialog.js';

/** The name of a key; quiet (and italic) while it is the automatic label. */
export function deviceText(k, cls = null) {
  const d = deviceName(k);
  return h('span', { class: [cls, { nil: !d.named, 'dev-auto': !d.named }], title: d.named ? null : 'Not named yet' }, d.text);
}

/** Asks for the name in a dialog; resolves with the saved key row, or null. */
export async function nameDevice(ctx, k) {
  const d = deviceName(k);
  const f = field({ label: 'Name', name: 'name', value: d.named ? d.text : '', hint: `Up to ${DEVICE_NAME_MAX} characters. Leave it empty to go back to “${deviceName({ key: k.key }).text}”.`,
    attrs: { maxlength: String(DEVICE_NAME_MAX), autocomplete: 'off', spellcheck: 'false' } });
  let saved = null;
  const save = async () => {
    const text = f.control.value;
    const why = checkDeviceName(text);
    setFieldError(f, why);
    if (why) { f.control.focus(); return false; }
    try {
      saved = await renameKey(ctx.api, k.key, text);
    } catch (err) {
      setFieldError(f, (err && err.fields && err.fields.name) || `Not saved: ${(err && err.message) || 'the server did not answer'}`);
      return false;
    }
    api.invalidate('');
    ctx.flash(text.trim() ? `Key ${k.key} is now called ${text.trim()}.` : `Key ${k.key} has no name now.`);
    return true;
  };
  let dlgSave = null;
  f.control.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); dlgSave?.click(); } });
  const body = h('div', { class: 'dev-form' }, h('p', { class: 'dialog-text' }, `CAN Bridge key `, h('span', { class: 'mono' }, k.key), '. The name shows wherever this logger is listed.'), f);
  const opened = openDialog({
    title: d.named ? `Rename ${d.text}` : 'Name this device', body, signal: ctx.signal, initialFocus: '.field-input',
    actions: [{ label: 'Cancel', value: null, kind: 'quiet' }, { label: 'Save the name', value: 'saved', kind: 'primary', onclick: save }],
  });
  dlgSave = body.closest('dialog')?.querySelector('.dialog-actions .button:last-child') || null;
  const v = await opened;
  return v === 'saved' && saved ? { ...k, ...saved } : null;
}

/** "Rename" for admins (null for others); onSaved(row) after a save. */
export function renameButton(ctx, k, onSaved) {
  if (!(ctx.me && ctx.me.role === 'admin')) return null;
  const d = deviceName(k);
  const b = button(d.named ? 'Rename' : 'Name it', { kind: 'quiet', cls: 'button-small dev-rename', title: d.named ? `Rename ${d.text}` : 'Name this device', onclick: async () => {
    const row = await nameDevice(ctx, k);
    if (!row) return;
    await onSaved(row);
    // the row is drawn again: back to its button
    setTimeout(() => [...document.querySelectorAll('.dev-rename')].find((x) => x.dataset.key === k.key)?.focus({ preventScroll: true }), 0);
  } });
  b.dataset.key = k.key;
  b.setAttribute('aria-label', d.named ? `Rename ${d.text}` : 'Name this device');
  return b;
}
