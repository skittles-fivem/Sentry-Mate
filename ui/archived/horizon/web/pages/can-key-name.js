// What a CAN Bridge key is called, on Data health and Settings: Cars: the name an admin chose, or the
// automatic label ("CAN Bridge key 8d41c0") shown faint while the key has no name; and the admin's Rename
// button, which asks for the name in a small dialog (an empty name clears it).
// API: PATCH can-keys/{key} { name } (admin): answers the key row.

import { h } from '../shared/dom.js';
import { deviceName, autoDeviceLabel, checkDeviceName, renameKey, DEVICE_NAME_MAX } from '../shared/devices.js';
import { field, form } from '../core/form.js';
import { openDialog } from '../core/dialog.js';
import { flash } from '../core/flash.js';

/** The name shown for a key row; faint while it is the automatic label, not a chosen name. */
export function deviceText(k, cls = null) {
  const d = deviceName(k);
  return h('span', { class: [cls, !d.named && 'faint'], title: d.named ? null : 'No name given yet' }, d.text);
}

/** The text of the name, for a CSV cell or a sentence. */
export const deviceWords = (k) => deviceName(k).text;

/** The Rename button of a key (admins only: null for anyone else). onSaved(row) gets the key row the server answers. */
export function renameKeyButton(ctx, k, admin, onSaved) {
  if (!admin) return null;
  const d = deviceName(k);
  const label = d.named ? `Rename ${d.text}` : 'Name this device';
  return h('button', {
    class: 'btn btn--sm btn--ghost key-rename', type: 'button', 'aria-label': label, title: label,
    onclick: async () => { const row = await renameKeyDialog(ctx, k); if (row && ctx.alive()) onSaved(row); },
  }, d.named ? 'Rename' : 'Name');
}

/** Ask for a key's name and save it. Resolves with the key row, or null when nothing was saved. */
export async function renameKeyDialog(ctx, k) {
  const d = deviceName(k);
  const auto = autoDeviceLabel(k.key);
  const fld = field({
    label: 'Name', name: 'name', value: d.named ? d.text : '',
    hint: `Up to ${DEVICE_NAME_MAX} characters, such as “Screenmate in the Model Y”. Leave it empty to show “${auto}” again.`,
    attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: auto },
  });
  let saved = null;
  const cancel = h('button', { class: 'btn btn--ghost', type: 'button', onclick: () => fl.el.closest('dialog')?.close() }, 'Cancel');
  const fl = form({
    fields: [fld], submit: 'Save', secondary: cancel,
    async onSubmit(vals) {
      const why = checkDeviceName(vals.name);
      if (why) { fld.setError(why); fld.input.focus(); return; }
      saved = await renameKey(ctx.api, k.key, vals.name);
      const now = deviceName(saved);
      flash(now.named ? `Key ${k.key} is now called ${now.text}.` : `Key ${k.key} has no name now; it shows as ${now.text}.`);
      fl.el.closest('dialog')?.close();
    },
  });
  await openDialog({ title: d.named ? `Rename ${d.text}` : 'Name this device', body: [h('p', { class: 'note' }, 'Key ', h('span', { class: 'mono' }, k.key), '. The name is shown wherever this key is listed.'), fl.el], actions: [] });
  return saved;
}
