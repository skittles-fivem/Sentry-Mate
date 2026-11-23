// Sentry Mate, "Console" UI pack: what a CAN Bridge key is called, and naming it (health, settings/cars).
// - deviceText(k): its chosen name, or the automatic label in muted text while no admin named it.
// - renameBtn(ctx, k, onSaved, { line }): [rename] (or [name it]) for admins, null for others; a small dialog
//   asks for the name, an empty one clears it; onSaved(row) gets the key row the server answered.
//   line: true outside a table (the brackets are typed; a table cell draws its own).
// PATCH can-keys/{key} { name } (admin).

import { h } from '../shared/dom.js';
import { invalidate } from '../shared/api.js';
import { deviceName, checkDeviceName, renameKey, DEVICE_NAME_MAX } from '../shared/devices.js';
import { note } from '../core/ui.js';
import { field, form } from '../core/form.js';
import { openDialog } from '../core/dialog.js';
import { toast } from '../core/toast.js';

/** The name of a key, muted while it is the automatic label. */
export function deviceText(k) {
  const d = deviceName(k);
  return h('span', { class: d.named ? null : 'muted', title: d.named ? null : 'not named yet' }, d.text);
}

/** Opens the naming dialog; onSaved(row) after a save. */
export function nameDevice(ctx, k, onSaved) {
  const d = deviceName(k);
  const name = field({ label: 'name', name: 'name', value: d.named ? d.text : '', hint: `up to ${DEVICE_NAME_MAX} characters; empty: back to "${deviceName({ key: k.key }).text}"`,
    attrs: { maxlength: String(DEVICE_NAME_MAX), autocomplete: 'off', spellcheck: 'false' } });
  let dlg;
  const f = form({ fields: [name], submit: 'save', async onSubmit(v) {
    const why = checkDeviceName(v.name);
    if (why) { name.setError(why); name.input.focus(); return; }
    const row = await renameKey(ctx.api, k.key, v.name);
    invalidate('');
    toast(v.name.trim() ? `Key ${k.key} is now called ${v.name.trim()}.` : `Key ${k.key} has no name now.`);
    dlg.close();
    onSaved({ ...k, ...row });
    // the row is drawn again: back to its button
    setTimeout(() => [...document.querySelectorAll('.dev-rename')].find((b) => b.dataset.key === k.key)?.focus({ preventScroll: true }), 0);
  } });
  dlg = openDialog({ title: d.named ? `rename ${d.text}` : 'name this device', body: [note(`CAN Bridge key ${k.key}. The name shows wherever this logger is listed.`), f.el] });
  name.input.focus();
  name.input.select();
}

/** [rename] for admins (null for others). */
export function renameBtn(ctx, k, onSaved, { line = false } = {}) {
  if (!ctx.admin) return null;
  const d = deviceName(k);
  const word = d.named ? 'rename' : 'name it';
  return h('button', { class: ['btn', 'dev-rename', { 'btn--line': line, 'tone--accent': line }], type: 'button', dataset: { key: k.key }, 'aria-label': d.named ? `Rename ${d.text}` : 'Name this device',
    onclick: () => nameDevice(ctx, k, onSaved) }, line ? `[${word}]` : word);
}
