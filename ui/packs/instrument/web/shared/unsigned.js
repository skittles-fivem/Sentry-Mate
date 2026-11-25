// Unsigned CAN logs of a paired car (docs/ui-api.md: /cars/{car}/logs, /cars/{car}/logs/unsigned,
// /cars/{car}/logs/use-unsigned; docs/security.md "Pairing and the owner's word"), in the same words on every pack:
// why a log has no signature, a log used on the owner's word, the counts, the confirm text that says exactly how
// many logs of which kinds and that bad or other-key logs are never included, and the request body with the password
// asked again. Pure functions: a page renders them in its own look.

/** Why a log has no signature, in plain words (`unsigned_kind`). */
export const KIND_WORDS = {
  no_sidecar: 'no sidecar came with it',
  no_integrity: 'made before the CAN Bridge app signed logs',
  signed_false: 'the app sent it marked as not signed',
  could_not_sign: 'the app tried to sign it and could not',
  unknown: 'not signed (checked by an older version, so why is not known)',
};

/** The same, for a count of them ("6 made before the CAN Bridge app signed logs"). */
const KIND_COUNTED = {
  no_sidecar: 'with no sidecar',
  no_integrity: 'made before the CAN Bridge app signed logs',
  signed_false: 'marked as not signed by the app',
  could_not_sign: 'the app could not sign',
  unknown: 'not signed for a reason not known',
};

/** The order kinds are listed in. */
const ORDER = ['no_integrity', 'no_sidecar', 'signed_false', 'could_not_sign', 'unknown'];

/** What a log used on the owner's word is called. */
export const USED_BY_OWNER = 'unsigned, used by the owner';

/** The button that opens the confirm dialog, and the one that takes it back. */
export const USE_BUTTON = 'Use unsigned logs';
export const STOP_BUTTON = 'Stop using';

const n = (x, fmt) => (fmt ? fmt(x) : String(x));
const logs = (x, fmt) => `${n(x, fmt)} ${x === 1 ? 'log' : 'logs'}`;

/** A log row's kind in plain words, with the bridge's reason for one it could not sign; '' for a signed one. */
export function kindText(row) {
  const k = row?.unsigned_kind;
  if (!k) return '';
  const words = KIND_WORDS[k] || KIND_WORDS.unknown;
  if (k === 'could_not_sign') {
    const why = /\((.+)\)\s*$/.exec(row.reason || '')?.[1];
    return why ? `${words}: ${why}` : words;
  }
  return words;
}

/** How a log row's check reads: "unsigned, used by the owner" for one used on the owner's word. */
export function statusText(row) {
  if (row?.override_use) return USED_BY_OWNER;
  if (row?.unsigned_kind) return row.result === 'could_not_sign' ? 'could not be signed, not used' : 'unsigned, not used';
  return row?.result || '';
}

/** "6 made before the CAN Bridge app signed logs, 1 with no sidecar" of `kinds` ({ kind: count }). */
export function kindsText(kinds, fmt = null) {
  const rank = (k) => (ORDER.includes(k) ? ORDER.indexOf(k) : ORDER.length);
  const keys = Object.keys(kinds || {}).filter((k) => kinds[k] > 0).sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
  return keys.map((k) => `${n(kinds[k], fmt)} ${KIND_COUNTED[k] || k}`).join(', ');
}

/** The summary of `/logs/unsigned` in a sentence or three; '' when the car has none. */
export function summaryText(u, fmt = null) {
  if (!u || (!u.can_use && !u.in_use)) return '';
  const parts = [];
  if (u.can_use) {
    const kinds = kindsText(u.kinds, fmt);
    parts.push(`${logs(u.can_use, fmt)} failed ${u.can_use === 1 ? 'its' : 'their'} check only for having no signature and ${u.can_use === 1 ? 'is' : 'are'} not used${kinds ? ` (${kinds})` : ''}.`);
  }
  if (u.in_use) parts.push(`${logs(u.in_use, fmt)} without a signature ${u.in_use === 1 ? 'is' : 'are'} used, as an admin chose.`);
  if (u.never) parts.push(`${logs(u.never, fmt)} failed for something else (a bad signature, another key’s, a changed file) and ${u.never === 1 ? 'is' : 'are'} never used.`);
  return parts.join(' ');
}

/** The choice of the dialog: the logs from before the first signed log only, when there are some (preselected). */
export function choice(u, fmt = null) {
  const before = u?.can_use_before_first_signed || 0;
  if (!before) return null;
  return {
    label: `Only the ${logs(before, fmt)} from before this car’s first signed log`,
    hint: before === u.can_use
      ? 'That is all of them.'
      : `The other ${logs(u.can_use - before, fmt)} came after this car’s CAN Bridge signed logs, which is less likely to be your own app.`,
    value: true,
  };
}

/** The dialog's text: exactly how many logs, of which kinds, and what is never included. */
export function confirmText(u, carName, onlyBefore = false, fmt = null) {
  const count = onlyBefore ? u.can_use_before_first_signed : u.can_use;
  const kinds = onlyBefore ? null : kindsText(u.kinds, fmt);
  return [
    `Use ${logs(count, fmt)} of ${carName} that failed ${count === 1 ? 'its' : 'their'} check only for having no signature${kinds ? ` (${kinds})` : ''}${onlyBefore ? ', all from before its first signed log' : ''}. Their drives and charges then appear.`,
    `Logs with a bad signature, signed by another key, or changed after signing are never included${u.never ? ` (${logs(u.never, fmt)} of this car)` : ''}.`,
    'A log’s time comes from its name, which anyone with this car’s upload token can choose: use them only if you know your own CAN Bridge sent them. You can stop using them at any time.',
  ];
}

/** The dialog's text for taking it back. */
export function stopText(u, carName, fmt = null) {
  return `Stop using the ${logs(u.in_use, fmt)} of ${carName} without a signature that an admin chose to use? Their data is left out again, as unknown time.`;
}

/** The POST (use) or DELETE (stop) body: every such log of the car, or only those before its first signed log. */
export function body({ password, code = null, onlyBefore = false, names = null }) {
  const b = names ? { names } : { all: true };
  if (!names && onlyBefore) b.before_first_signed = true;
  b.confirm_password = password;
  if (code) b.code = code;
  return b;
}

/** What the page says once it is done. */
export function doneText(out, on, fmt = null) {
  const c = out?.changed || 0;
  if (!c) return on ? 'Nothing to change: those logs are used already.' : 'Nothing to change: none were in use.';
  return on
    ? `Using ${logs(c, fmt)} without a signature. Their data is read now; drives appear as it is done.`
    : `Stopped using ${logs(c, fmt)} without a signature. Their data is left out again.`;
}
