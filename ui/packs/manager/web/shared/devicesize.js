// Sentry Mate shared module: devicesize.js (the Interface size on this device).
// Canonical source: ui/shared/devicesize.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// The person's Interface size (interface_size and follow_screen in /api/v1/me/preferences) follows their
// account to every screen. A person who uses several machines (one monitor at 100 % scaling, a laptop at
// 150 %) may want a different size on each, so this browser can keep its own size, which wins over the
// account's here and nowhere else:
//
//   null (Automatic, the default)   the account's Interface size, with Auto for large screens if it is on
//   80 | 90 | 100 | 110 | 125       that percent of the standard size (the browser's 16 px), nothing on top
//
// It lives only in this browser's localStorage under one key for every pack (the packs share the origin),
// never on the server. Every pack's boot.js reads the same key before the first paint (a classic script
// cannot import this module, so it repeats the few lines of readDeviceSize; change both together).
// Storage can be blocked (a private window, cleared site data): reading then gives null and writing does
// nothing, and the page works on the account's size.

/** The localStorage key (a JSON number, or absent for Automatic). */
export const DEVICE_SIZE_KEY = 'sentrymate.device-size';

/** The steps offered, in percent of the standard size. */
export const DEVICE_SIZES = Object.freeze([80, 90, 100, 110, 125]);

/** A stored or picked value -> one of DEVICE_SIZES, or null (Automatic). */
export function normalDeviceSize(v) {
  if (v == null || v === '' || v === 'auto') return null;
  const n = Math.round(Number(v));
  return DEVICE_SIZES.includes(n) ? n : null;
}

function defaultStorage() {
  try { return globalThis.localStorage || null; } catch { return null; }
}

/** The size kept in this browser, or null (Automatic, or storage blocked). */
export function readDeviceSize(storage = defaultStorage()) {
  try {
    const raw = storage?.getItem(DEVICE_SIZE_KEY);
    return raw == null ? null : normalDeviceSize(JSON.parse(raw));
  } catch { return null; }
}

/**
 * Keep a size in this browser (null or 'auto' for Automatic, which removes the key). Returns the value now in
 * effect for this page (normalised), even when storage is blocked and it could not be kept.
 */
export function writeDeviceSize(v, storage = defaultStorage()) {
  const n = normalDeviceSize(v);
  try {
    if (n == null) storage?.removeItem(DEVICE_SIZE_KEY);
    else storage?.setItem(DEVICE_SIZE_KEY, JSON.stringify(n));
  } catch { /* storage blocked: this page still uses it until it is closed */ }
  return n;
}

/** 'Automatic' or '110 %'. */
export function deviceSizeLabel(v) {
  const n = normalDeviceSize(v);
  return n == null ? 'Automatic' : `${n} %`;
}

/** The picker's options: [{ value: 'auto' | '80' ..., label }] with Automatic first. */
export function deviceSizeOptions() {
  return [{ value: 'auto', label: 'Automatic' }, ...DEVICE_SIZES.map((n) => ({ value: String(n), label: `${n} %` }))];
}

/**
 * One line on what the device size means now.
 *   device   this browser's size (null for Automatic)
 *   account  the account's Interface size in percent (interface_size), optional
 *   shown    the size the account's setting gives on this screen (Auto x Interface size), optional
 */
export function deviceSizeNote(device, { account = null, shown = null } = {}) {
  const n = normalDeviceSize(device);
  if (n == null) {
    const what = account != null ? `your account’s Interface size (${account} %` + (shown != null && shown !== account ? `, ${shown} % on this screen)` : ')') : 'your account’s Interface size';
    return `Automatic: this device uses ${what}. Pick a size to change it on this device only.`;
  }
  return `This device shows everything at ${n} % of the standard size. Only this browser on this device; your account’s Interface size still applies on your other devices.`;
}

/**
 * f(size | null) when another tab or window of this browser changes the device size. Returns off().
 * (The tab that changes it tells its own page itself.)
 */
export function onDeviceSizeStorage(f, target = globalThis.window) {
  if (!target?.addEventListener) return () => {};
  const on = (e) => { if (e.key === DEVICE_SIZE_KEY || e.key === null) f(readDeviceSize()); };
  target.addEventListener('storage', on);
  return () => target.removeEventListener('storage', on);
}
