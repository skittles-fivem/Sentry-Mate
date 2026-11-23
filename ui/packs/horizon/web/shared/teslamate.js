// The local TeslaMate input (docs/ui-api.md, "Integrations: Local TeslaMate"): what every pack's Settings:
// Integrations section (#teslamate) shares. The words of states and errors, the drafts sent to the server from the
// forms' values, and the server's own rules checked before anything is sent (the plain text rule, the kept password,
// the pin, the topic prefix), so a pack shows the same message under the same field as the server would.
//
// Pure functions, no DOM: each pack draws its own forms and calls these.

export const TM_BASE = 'integrations/teslamate';
export const TM_LABEL = 'Data from local TeslaMate';
export const TM_SOURCE = Object.freeze({ kind: 'teslamate_local', label: TM_LABEL });

/** The defaults the server answers while nothing is added. */
export const TM_DEFAULTS = Object.freeze({
  mqtt: Object.freeze({ host: '', port: 1883, tls: false, pin_sha256: null, username: null, password_set: false, prefix: 'teslamate', allow_unencrypted: false }),
  database: Object.freeze({ enabled: false, host: '', port: 5432, database: 'teslamate', username: 'teslamate', password_set: false, tls: false, pin_sha256: null, allow_unencrypted: false, interval_s: 300 }),
});

/** [word, tone, explanation] per state; tone is one of live, plain, warm. */
export const MQTT_STATE = Object.freeze({
  off: ['Off', 'plain', 'Not added: no connection to a TeslaMate broker is made.'],
  connecting: ['Connecting', 'plain', 'The server is connecting to the broker.'],
  connected: ['Connected', 'live', 'Subscribed to TeslaMate\'s topics; it only reads and never publishes.'],
  error: ['Not connected', 'warm', 'The broker could not be read. The server tries again by itself, waiting longer each time.'],
  paused: ['Paused', 'plain', 'Nothing is read until you resume it.'],
});
export const DB_STATE = Object.freeze({
  off: ['Off', 'plain', 'The database catch-up is off: only MQTT is read.'],
  syncing: ['Catching up', 'live', 'Reading what MQTT missed from TeslaMate\'s database.'],
  ok: ['Caught up', 'live', 'The last catch-up worked.'],
  error: ['Failed', 'warm', 'The last catch-up failed. It is tried again at the next interval.'],
  paused: ['Paused', 'plain', 'Nothing is read until you resume it.'],
});

/** The words of a last_error or test error code. */
export const TM_ERROR_WORDS = Object.freeze({
  connection_refused: 'The host refused the connection: check the host and port, and that it listens there',
  timeout: 'No answer from the host: check the host, and that this server can reach it',
  auth_failed: 'The user name or password was not taken',
  tls_failed: 'The TLS handshake failed: the host may not offer TLS on this port',
  pin_mismatch: 'The host\'s certificate does not have the pinned fingerprint',
  not_teslamate: 'The database has no TeslaMate tables: check the database name',
  query_failed: 'A query on the database failed',
});
export const tmErrorText = (e) => (e ? (TM_ERROR_WORDS[e.code] || e.message || e.code) : null);

export const UNENCRYPTED_LABEL = 'Allow unencrypted (local network only)';
export const UNENCRYPTED_WARNING = 'Without TLS the password and every value travel as plain text: anyone on the network between this server and TeslaMate can read them. Tick this only when both are on your own local network.';

const API_WORDS = {
  plaintext_password: 'A password over plain TCP can be read by anyone on the network: turn TLS on, or tick "Allow unencrypted (local network only)".',
  password_resend: 'The saved password goes only to the host it was typed for: type it again for the new host, port or TLS setting, or remove it.',
  car_taken: 'That car is already matched to another TeslaMate car. Leave that one out first, or pick another car.',
  teslamate_not_added: 'The local TeslaMate is not added any more: another admin may have removed it. The page now shows what there is.',
  stale_version: 'Someone else changed this a moment ago, so nothing was saved. The page now shows their change: make yours again if it is still needed.',
};

/** Puts the section's own words on an API error (in place) and returns it. */
export function tmWords(e) {
  if (!e || e.name === 'AbortError') return e;
  if (e.code === 'password_needed') e.code = 'wrong_password';
  if (e.code === 'wrong_code') e.code = 'wrong_two_step';
  const key = e.code === 'stale_version' || (e.status === 409 && !API_WORDS[e.code]) ? 'stale_version' : e.code;
  if (API_WORDS[key]) e.message = API_WORDS[key];
  if (e.fields) {
    delete e.fields.confirm_password; delete e.fields.code;
    if ((e.code === 'plaintext_password' || e.code === 'password_resend')) for (const k of Object.keys(e.fields)) e.fields[k] = API_WORDS[e.code];
    if (!Object.keys(e.fields).length) e.fields = null;
  }
  return e;
}
/** Whether an error means the page should read the settings again (someone else changed them). */
export const tmStale = (e) => !!e && (e.code === 'teslamate_not_added' || e.code === 'stale_version' || (e.status === 409 && e.code !== 'car_taken'));

const trimOrNull = (v) => { const s = v == null ? '' : String(v).trim(); return s || null; };
const toPort = (v, d) => { if (v === '' || v == null) return d; const n = Number(v); return Number.isFinite(n) ? n : NaN; };

/**
 * The draft of one part ('mqtt' or 'database') from a form's values: { host, port, tls, pin_sha256, username,
 * password, password_remove, prefix | database + interval_s + enabled, allow_unencrypted }. `password`: typed text
 * sets it; `password_remove` true sends null; else it is left out (the saved one is kept).
 */
export function tmDraft(part, v) {
  const d = TM_DEFAULTS[part];
  const out = {
    host: trimOrNull(v.host), port: toPort(v.port, d.port), tls: !!v.tls,
    pin_sha256: v.tls ? trimOrNull(v.pin_sha256) : null,
    username: trimOrNull(v.username), allow_unencrypted: !v.tls && !!v.allow_unencrypted,
  };
  if (part === 'mqtt') out.prefix = trimOrNull(v.prefix) || '';
  else {
    out.enabled = !!v.enabled;
    out.database = trimOrNull(v.database);
    out.interval_s = v.interval_s === '' || v.interval_s == null ? d.interval_s : Math.round(Number(v.interval_s));
  }
  if (v.password) out.password = String(v.password);
  else if (v.password_remove) out.password = null;
  return out;
}

/** The saved settings of a part as a PUT sends them back unchanged (no password: the saved one is kept). */
export function tmSaved(part, saved) {
  const s = { ...TM_DEFAULTS[part], ...(saved || {}) };
  delete s.password_set;
  return s;
}

const PIN_RX = /^(?:[0-9A-Fa-f]{64}|(?:[0-9A-Fa-f]{2}:){31}[0-9A-Fa-f]{2})$/;
const HOST_RX = /^(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)*|\[?[0-9A-Fa-f:]+\]?)$/;

/**
 * The server's rules on a draft, before it is sent: { 'mqtt.host': text, ... } (empty when it is fine). `saved`:
 * the part as GET answered it (password_set, host, port, tls), or null when nothing is saved.
 */
export function tmCheck(part, draft, saved) {
  const f = {};
  const k = (name) => `${part}.${name}`;
  const on = part === 'mqtt' || draft.enabled;
  if (draft.host && (/:\/\//.test(draft.host) || draft.host.includes('/') || !HOST_RX.test(draft.host))) f[k('host')] = 'A host name or an IP address, without mqtt:// or a path (for example 192.0.2.10).';
  else if (on && !draft.host) f[k('host')] = 'Required: the host name or IP address of TeslaMate\'s ' + (part === 'mqtt' ? 'MQTT broker.' : 'database.');
  if (!Number.isInteger(draft.port) || draft.port < 1 || draft.port > 65535) f[k('port')] = 'A port from 1 to 65535.';
  if (draft.pin_sha256 && !PIN_RX.test(draft.pin_sha256)) f[k('pin_sha256')] = 'The certificate\'s SHA-256 fingerprint: 64 hex digits, colons allowed.';
  if (part === 'mqtt') {
    const p = draft.prefix;
    if (!p) f[k('prefix')] = 'Required (TeslaMate\'s default is teslamate).';
    else if (/[+#]/.test(p)) f[k('prefix')] = 'No + or # (they are wildcards).';
    else if (p.startsWith('$')) f[k('prefix')] = 'A prefix must not start with $ (those topics belong to the broker).';
    else if (p.split('/').some((x) => !x)) f[k('prefix')] = 'No empty level (a / at either end, or //).';
  } else {
    if (on && !draft.database) f[k('database')] = 'Required (TeslaMate\'s is teslamate).';
    if (!Number.isInteger(draft.interval_s) || draft.interval_s < 60 || draft.interval_s > 86400) f[k('interval_s')] = 'Seconds between catch-ups, from 60 to 86400.';
  }
  if (Object.keys(f).length) return f;
  const kept = !('password' in draft) && !!saved?.password_set;
  if (kept && (draft.host !== (saved.host || null) || draft.port !== saved.port || draft.tls !== !!saved.tls)) f[k('password')] = API_WORDS.password_resend;
  else if ((typeof draft.password === 'string' || kept) && !draft.tls && !draft.allow_unencrypted) f[k('password')] = API_WORDS.plaintext_password;
  return f;
}

/** Whether a password would go over plain TCP with this draft (the tick then matters). */
export const tmPlainPassword = (draft, saved) => !draft.tls && (typeof draft.password === 'string' || (!('password' in draft) && !!saved?.password_set));

/** The PUT body for one edited part; the other part goes back as saved. */
export function tmPutBody(t, part, draft) {
  return {
    version: t?.enabled ? t.version : 0,
    mqtt: part === 'mqtt' ? draft : tmSaved('mqtt', t?.mqtt),
    database: part === 'database' ? draft : tmSaved('database', t?.database),
  };
}

/** The match rows that differ from the saved ones: [{ teslamate_car, match }]. `chosen`: Map(teslamate_car -> value). */
export function tmMatchChanges(items, chosen) {
  return (items || []).filter((c) => chosen.has(c.teslamate_car) && (chosen.get(c.teslamate_car) || null) !== (c.match ?? null))
    .map((c) => ({ teslamate_car: c.teslamate_car, match: chosen.get(c.teslamate_car) || null }));
}

/** What a car's select starts on: its match, else the suggestion (saved only with Save matches). */
export const tmPreselect = (c) => c.match ?? c.suggested ?? '';

/** The one-line words of a match value. */
export function tmMatchWords(v, carName = (x) => x) {
  if (v === 'new') return 'a new car here, read from now on';
  if (v === 'leave_out') return 'left out, not read';
  if (!v) return 'not decided, not read';
  return `${carName(v)}, read from now on`;
}

/** The section's headline: [word, tone]. */
export function tmHeadline(t) {
  if (!t?.enabled) return ['Off until it is added', 'plain'];
  if (t.paused) return ['Paused', 'plain'];
  const m = t.status?.mqtt?.state;
  const d = t.status?.database?.state;
  if (m === 'error' || d === 'error') return ['Has errors', 'warm'];
  const read = (t.cars || []).filter((c) => c.match && c.match !== 'leave_out').length;
  if (!read) return ['No car matched yet', 'warm'];
  if (m === 'connected') return [read === 1 ? 'Reading one car' : `Reading ${read} cars`, 'live'];
  return [MQTT_STATE[m]?.[0] || m || 'Off', MQTT_STATE[m]?.[1] || 'plain'];
}

/* ------------------------------------------------------------------ notices */

/**
 * The server's notices of a local TeslaMate in trouble (admins only; docs/ui-api.md, "Integrations: Local
 * TeslaMate"): its broker refused the connection, the user name or password, or has another certificate than the
 * pinned one; it went quiet; a database catch-up failed. Each links to Settings: Integrations (#teslamate).
 */
export const TM_NOTICE_CODES = Object.freeze(['teslamate_connection_refused', 'teslamate_auth_failed', 'teslamate_pin_mismatch', 'teslamate_quiet', 'teslamate_catchup_failed']);
/** Whether a notice is one of the local TeslaMate's. */
export const isTmNotice = (n) => !!n && TM_NOTICE_CODES.includes(n.code);
/** The words of a local TeslaMate notice's link. */
export const TM_NOTICE_ACTION = 'Open local TeslaMate';
/** Where a local TeslaMate notice links: its section of Settings: Integrations, whatever `fix_link` says after #. */
export const tmNoticeFix = (n, settings = '/settings/integrations') => `${String(n?.fix_link || settings).split('#')[0]}#teslamate`;
