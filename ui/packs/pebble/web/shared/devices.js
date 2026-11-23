// Sentry Mate shared module: devices.js (what a CAN Bridge key is called, and naming it).
// Canonical source: ui/shared/devices.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// Every answer that lists CAN Bridge keys (/can-keys, PATCH /can-keys/{key}, /cars/{car}/health keys, the
// upload token rows with a key) gives `device`: the name an admin chose, or null, and `device_label`: that
// name, or the server's automatic label while unnamed ("CAN Bridge key 8d41c0"). Pages show the label and
// mute it while it is not a chosen name.
//
// deviceName(k)            -> { text, named }: what to show for a key row (works on older answers too)
// autoDeviceLabel(key)     -> the automatic label the server gives an unnamed key
// checkDeviceName(text)    -> null when `text` (trimmed) is a name the server takes, else the reason in words
// renameKey(client, key, text) -> PATCH /can-keys/{key} { name } (an empty text clears the name): the key row

import { t } from './i18n.js';

/** The most characters a device name has (after trimming). */
export const DEVICE_NAME_MAX = 60;

/** The short id of a key: its part after the last `_`, first 6 characters. */
export function shortKeyId(key) {
  const s = String(key ?? '');
  const tail = s.includes('_') ? s.slice(s.lastIndexOf('_') + 1) : s;
  return (tail || s).slice(0, 6);
}

/** What the server calls a key no one has named. */
export function autoDeviceLabel(key) {
  const id = shortKeyId(key);
  return id ? t('CAN Bridge key {id}', { id }) : t('CAN Bridge key');
}

/** What to show for a key row: its chosen name (`named`), else the automatic label. */
export function deviceName(k) {
  const name = typeof k?.device === 'string' ? k.device.trim() : '';
  if (name) return { text: name, named: true };
  const label = typeof k?.device_label === 'string' && k.device_label.trim() ? k.device_label.trim() : autoDeviceLabel(k?.key);
  return { text: label, named: false };
}

/** Null when the text is a name the server takes (empty clears the name), else why not. */
export function checkDeviceName(text) {
  const s = String(text ?? '').trim();
  if ([...s].length > DEVICE_NAME_MAX) return t('A name has at most {n} characters.', { n: DEVICE_NAME_MAX });
  return null;
}

/** Names a key (an empty or blank text clears its name); answers the key row. */
export function renameKey(client, key, text) {
  const s = String(text ?? '').trim();
  return client.patch(`can-keys/${encodeURIComponent(key)}`, { name: s ? s : null });
}
