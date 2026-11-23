// The Storage panel's words and grouping (Settings: Server and access, GET /system/storage/detail), the same in
// every pack: the categories grouped by kind (CAN logs, time series, drives and charges, kept answers and records,
// imports, server), each group with its size on disk and its share of the total, largest first, and a short name for
// each category (the server's label stays in the CSV and the row's title).

import { format } from './units.js';

/** The groups in plain words, by the server's `group`. */
export const STORE_GROUP = {
  logs: 'CAN logs (signed originals)', series: 'Time series', rows: 'Drives and charges', kv: 'Kept answers and records',
  imports: 'Imports', server: 'Server',
};
/** A group's name without the note in brackets (a tag on a row). */
export const groupWord = (g) => String(STORE_GROUP[g] || g || '').replace(/\s*\(.*\)$/, '');

/** The steps of a compaction pass, in plain words. */
export const PACK_STEP = { logs: 'CAN logs', series: 'time series', records: 'records' };

const STREAM = {
  can: 'CAN logs', 'tesla-api': 'Tesla API', 'teslafi-api': 'TeslaFi (live)', 'teslamate-local': 'Local TeslaMate',
  teslamate: 'TeslaMate import', teslafi: 'TeslaFi import',
};
const SHORT = {
  'logs.raw': 'Signed originals', 'logs.sidecars': 'Sidecars', 'logs.packs': 'Packed by day', 'logs.ledger': 'Check results',
  'logs.quarantine': 'Failed their check (kept)', 'logs.uploading': 'Still arriving',
  'rows.snapshots': 'Drives, charges, states', 'rows.journals': 'Not yet folded in',
  'kv.engine': 'Engine records', 'kv.day-parts': 'Day parts (can be redone)', 'kv.geocode': 'Addresses looked up',
  'imports.uploads': 'Uploaded backups',
  'server.accounts': 'Accounts and sessions', 'server.audit': 'Audit log', 'server.db': 'Car list and places',
  'server.web': 'UI packs', 'server.overview': 'Overview tracker', 'server.tiles': 'Offline map tiles',
  'server.tokens': 'Upload tokens', 'server.vault': 'Vault and certificate', 'server.other': 'Everything else',
};
const PART = { snapshots: 'day series', packs: 'packed days', journals: 'not yet folded' };
const CURVE_PART = { snapshots: 'charge curves', packs: 'charge curves, packed', journals: 'charge curves, not yet folded' };

/** A short name for a category: "TeslaMate import: day series", "Signed originals"; else the server's label. */
export function shortName(c) {
  const key = String(c?.key || '');
  if (SHORT[key]) return SHORT[key];
  const m = /^(series|curves)\.(.+)\.(snapshots|packs|journals)$/.exec(key);
  if (m) return `${STREAM[m[2]] || m[2]}: ${(m[1] === 'curves' ? CURVE_PART : PART)[m[3]]}`;
  const kv = /^kv\.(.+)$/.exec(key);
  if (kv && !c?.label) return kv[1];
  return c?.label || key;
}

/**
 * The categories grouped: [{ group, name, disk_bytes, files, share (0..1 of all), rows (largest first) }], largest
 * group first. Groups the server names that this page does not know keep its word.
 */
export function storageGroups(categories) {
  const by = new Map();
  let total = 0;
  for (const c of categories || []) {
    const g = c.group || 'server';
    if (!by.has(g)) by.set(g, { group: g, name: STORE_GROUP[g] || g, disk_bytes: 0, files: 0, share: 0, rows: [] });
    const e = by.get(g);
    const d = Number(c.disk_bytes) || 0;
    e.disk_bytes += d; e.files += Number(c.files) || 0; total += d;
    e.rows.push(c);
  }
  const out = [...by.values()];
  for (const e of out) {
    e.share = total > 0 ? e.disk_bytes / total : 0;
    e.rows.sort((a, b) => (Number(b.disk_bytes) || 0) - (Number(a.disk_bytes) || 0) || String(a.key).localeCompare(String(b.key)));
  }
  out.sort((a, b) => b.disk_bytes - a.disk_bytes || a.group.localeCompare(b.group));
  return out;
}

/** A share as a percent for a row, in the person's locale: "42 %", "<1 %" for a sliver, "0 %" for none. */
export function shareText(share) {
  if (!(share > 0)) return format(0, 'percent');
  if (share < 0.01) return `<${format(1, 'percent')}`;
  return format(Math.round(share * 100), 'percent');
}
