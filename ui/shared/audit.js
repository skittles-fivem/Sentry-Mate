// The audit log in words: each action code's label and group, and the changed item as readable text with a
// link where one fits. Pure data and functions (no DOM): each pack renders the result with its own `h`.
//
//   AUDIT_GROUPS            [{ id, label }] in display order
//   AUDIT_ACTIONS           { code: 'Past-tense label' } for every code the server writes (ui/shared/audit-codes.json)
//   AUDIT_GROUP_OF          { code: group id }
//   actionLabel(code)       the label; an unknown code -> 'Other: <code in words>'
//   actionGroup(code)       the group id; 'other' when unknown
//   groupLabel(id)          the group's label; 'Other' when unknown
//   auditItem(row, ctx)     -> { text, route: [name, params] | null, hash: string | null, faint } | null (no target)
//       ctx = { carName(id), accountName(id), trip(id) -> { name, car, from_ms, to_ms }, packName(id) } (all optional)

export const AUDIT_GROUPS = [
  { id: 'sign-ins', label: 'Sign-ins' },
  { id: 'accounts', label: 'Accounts and access' },
  { id: 'settings', label: 'Settings' },
  { id: 'cars', label: 'Cars and trips' },
  { id: 'integrations', label: 'Integrations' },
  { id: 'imports', label: 'Imports and moves' },
];

const BY_GROUP = {
  'sign-ins': {
    'setup-code-made': 'Made a setup code',
    'setup-code-wrong': 'Wrong setup code',
    setup: 'Set up the server',
    'sign-in': 'Signed in',
    'sign-in-failed': 'Wrong password',
    'sign-in-refused': 'Sign-in refused',
    'sign-out': 'Signed out',
    'signed-out': 'Signed out by the server',
    'two-step-failed': 'Wrong two-step code',
    'confirm-failed': 'Wrong password to confirm',
    'session-revoked': 'Ended a session',
    'sessions-revoked': 'Ended the other sessions',
  },
  accounts: {
    'account-created': 'Added an account',
    'account-removed': 'Removed an account',
    'account-changed': 'Changed an account',
    'role-changed': 'Changed a role',
    'password-changed': 'Changed a password',
    'cars-changed': 'Changed which cars an account sees',
    'two-step-on': 'Turned on two-step sign-in',
    'two-step-off': 'Turned off two-step sign-in',
    'two-step-policy': 'Changed the two-step rule',
    'recovery-codes-new': 'Made new recovery codes',
  },
  settings: {
    'settings-changed': 'Changed the server settings',
    'preferences-changed': 'Changed preferences',
    'ui-pack-chosen': 'Chose a design',
    'ui-pack-installed': 'Installed a design',
    'ui-pack-removed': 'Removed a design',
    'certificate-made': 'Made a new certificate',
    'release-checked': 'Checked for a newer build',
    'storage-compacted': 'Started packing the stored data',
    'recovery-key-shown': 'Showed the recovery key',
    'recovery-key-new': 'Made a new recovery key',
    'recovery-key-saved': 'Saved the recovery key',
    change: 'Changed something',
  },
  cars: {
    'car-changed': 'Changed a car',
    'car-settings-changed': 'Changed car settings',
    'car-rebuild': 'Rebuilt a car’s history',
    'car-full-rebuild': 'Ran a full rebuild',
    'car-gps-fit-removed': 'Removed a past GPS layout fit of a car',
    'car-gps-layout': 'Set how a car’s Screenmate GPS frames are read',
    'car-overlap': 'Found overlapping data',
    'can-logs-missing': 'Found CAN logs missing on the server',
    'can-logs-rechecked': 'Checked failed CAN logs again',
    'can-logs-unsigned-used': 'Used unsigned CAN logs',
    'can-logs-unsigned-stopped': 'Stopped using unsigned CAN logs',
    'charge-cost-entered': 'Entered a charge cost',
    'charge-cost-removed': 'Removed a charge cost',
    'drive-purpose-set': 'Set a drive’s purpose',
    'drive-purpose-removed': 'Removed a drive’s purpose',
    'purpose-rule-created': 'Added a purpose rule',
    'purpose-rule-removed': 'Removed a purpose rule',
    'trip-saved': 'Saved a trip',
    'trip-changed': 'Changed a trip',
    'trip-removed': 'Removed a trip',
    'update-version': 'Corrected a software version',
    'place-created': 'Added a place',
    'place-changed': 'Changed a place',
    'place-deleted': 'Removed a place',
    'place-invoice-priced': 'Priced a place from its Supercharger invoices',
    'place-repriced': 'Repriced a place’s charges',
    'key-assigned': 'Assigned a key',
    'key-unassigned': 'Unassigned a key',
    'key-preferred': 'Chose the preferred key',
    'can-key-renamed': 'Named a key',
    'token-created': 'Made a car token',
    'token-revoked': 'Revoked a car token',
  },
  integrations: {
    'mqtt-changed': 'Changed MQTT',
    'mqtt-tested': 'Tested MQTT',
    'tesla-account-added': 'Added a Tesla account',
    'tesla-account-removed': 'Removed a Tesla account',
    'tesla-account-paused': 'Paused a Tesla account',
    'tesla-account-resumed': 'Resumed a Tesla account',
    'tesla-account-changed': 'Changed a Tesla account',
    'tesla-token-replaced': 'Replaced a Tesla token',
    'tesla-car-matched': 'Matched a Tesla car',
    'tesla-poll-changed': 'Changed the Tesla API pace',
    'tesla-invoices-changed': 'Turned Tesla invoices on or off',
    'tesla-sign-in-needed': 'Tesla asked to sign in again',
    'tesla-account-error': 'Could not read a Tesla account',
    'tesla-rate-limited': 'Tesla started limiting requests',
    'teslafi-account-added': 'Added a TeslaFi account',
    'teslafi-account-removed': 'Removed a TeslaFi account',
    'teslafi-account-paused': 'Paused a TeslaFi account',
    'teslafi-account-resumed': 'Resumed a TeslaFi account',
    'teslafi-account-changed': 'Changed a TeslaFi account',
    'teslafi-token-replaced': 'Replaced a TeslaFi token',
    'teslafi-token-refused': 'TeslaFi refused the token',
    'teslafi-car-matched': 'Matched a TeslaFi car',
    'teslafi-poll-changed': 'Changed the TeslaFi pace',
    'teslafi-invoices-changed': 'Turned TeslaFi invoice costs on or off',
    'invoices-downloaded': 'Downloaded Supercharger invoices',
    'teslafi-account-error': 'Could not read a TeslaFi account',
    'teslamate-local-added': 'Connected the local TeslaMate',
    'teslamate-local-changed': 'Changed the local TeslaMate',
    'teslamate-local-paused': 'Paused the local TeslaMate',
    'teslamate-local-resumed': 'Resumed the local TeslaMate',
    'teslamate-local-tested': 'Tested the local TeslaMate',
    'teslamate-local-car-matched': 'Matched a TeslaMate car',
    'teslamate-local-removed': 'Disconnected the local TeslaMate',
    'teslamate-local-connection-refused': 'Could not reach the local TeslaMate',
    'teslamate-local-auth-failed': 'The local TeslaMate refused the password',
    'teslamate-local-pin-mismatch': 'The local TeslaMate’s certificate changed',
    'teslamate-local-quiet': 'The local TeslaMate went quiet',
    'teslamate-local-catchup-failed': 'Could not catch up from the local TeslaMate',
  },
  imports: {
    'import-created': 'Started an import',
    'import-uploaded': 'Uploaded an import file',
    'import-discarded': 'Discarded an import',
    'import-cars': 'Chose the cars to import',
    'import-started': 'Ran an import',
    'import-rows-moved': 'Moved imported rows to another car',
    'import-rows-ignored': 'Left aside imported rows that wait for a car',
    'move-code-made': 'Made a move code',
    'move-cancelled': 'Cancelled a move',
    'move-undone': 'Undid a move',
    'export-made': 'Made an export',
    'export-removed': 'Removed an export',
    'export-downloaded': 'Downloaded an export',
    'setup-restore-started': 'Started restoring from a backup',
    'setup-restore-cancelled': 'Gave up restoring from a backup',
    'setup-move-started': 'Started moving the data here',
    'setup-move-resumed': 'Carried on moving the data here',
    'setup-move-cancelled': 'Gave up moving the data here',
    'setup-move-finished-forced': 'Finished a move with the recovery key',
  },
};

export const AUDIT_ACTIONS = Object.freeze(Object.assign({}, ...Object.values(BY_GROUP)));
export const AUDIT_GROUP_OF = Object.freeze(Object.fromEntries(
  Object.entries(BY_GROUP).flatMap(([g, codes]) => Object.keys(codes).map((c) => [c, g]))));

const has = (o, k) => Object.prototype.hasOwnProperty.call(o, k);
const words = (s) => String(s || '').replace(/[-_:]+/g, ' ').trim();
const cap = (s) => (s ? s[0].toUpperCase() + s.slice(1) : s);

export function actionLabel(code) {
  const c = String(code ?? '');
  if (has(AUDIT_ACTIONS, c)) return AUDIT_ACTIONS[c];
  return c ? `Other: ${words(c)}` : 'Other';
}

export function actionGroup(code) {
  const c = String(code ?? '');
  return has(AUDIT_GROUP_OF, c) ? AUDIT_GROUP_OF[c] : 'other';
}

export function groupLabel(id) {
  return AUDIT_GROUPS.find((g) => g.id === id)?.label || 'Other';
}

const SERVER_ITEMS = { certificate: 'The certificate', release: 'Software release', 'recovery-key': 'The recovery key' };
const SETTINGS_ITEMS = {
  server: ['Server defaults', 'settings-display', null],
  mqtt: ['MQTT settings', 'settings-integrations', '#mqtt'],
  tesla: ['Tesla API pace', 'settings-integrations', '#tesla'],
  teslafi: ['TeslaFi pace', 'settings-integrations', '#teslafi'],
  'tesla-invoices': ['Tesla Supercharger invoices', 'settings-integrations', '#tesla'],
  'teslafi-invoices': ['TeslaFi invoice costs', 'settings-integrations', '#teslafi'],
};

export function auditItem(row, ctx = {}) {
  const t = String(row?.target ?? '');
  if (!t) return null;
  const call = (f, id) => { try { return typeof f === 'function' && id ? f(id) : undefined; } catch { return undefined; } };
  const carName = (id) => call(ctx.carName, id);
  const i = t.indexOf(':');
  const kind = i < 0 ? t : t.slice(0, i);
  const rest = i < 0 ? '' : t.slice(i + 1);
  const parts = rest ? rest.split(':') : [];
  const item = (text, route = null, hash = null, faint = false) => ({ text, route, hash, faint });
  // drive:<car>:<id> etc.; row.car wins for the car
  const carOf = () => row?.car || (parts.length > 1 ? parts[0] : null);
  const lastId = () => parts[parts.length - 1] || '';

  switch (kind) {
    case 'car': {
      const id = parts[0] || row?.car;
      const n = carName(id);
      return n ? item(n, ['settings-cars', { car: id }]) : item('A car no longer here', null, null, true);
    }
    case 'drive':
    case 'charge':
    case 'update': {
      const c = carOf();
      const n = carName(c);
      const what = { drive: 'A drive', charge: 'A charge', update: 'A software update' }[kind];
      if (!n) return item(what, null, null, true);
      const route = kind === 'drive' ? ['drive', { car: c, drive: lastId() }]
        : kind === 'charge' ? ['charge', { car: c, charge: lastId() }] : ['software', { car: c }];
      return item(`${what} of ${n}`, route);
    }
    case 'trip': {
      const tr = call(ctx.trip, parts[0]);
      const c = row?.car || tr?.car;
      const n = carName(c);
      if (!n) return item(tr?.name ? `Trip ${tr.name}` : 'A saved trip', null, null, true);
      const period = tr && tr.from_ms != null ? { from: tr.from_ms, to: tr.to_ms } : null;
      return item(tr?.name ? `Trip ${tr.name}` : `A saved trip of ${n}`, ['trip', { car: c, period }]);
    }
    case 'place': return item('A place', ['places', { query: { place: parts[0] } }]);
    case 'import': return item('An import', ['import', {}]);
    case 'import-hold': return item('Imported rows that wait for a car', ['import', {}]);
    case 'export': return item('An export', ['settings-server', {}], '#move');
    case 'move': return item('A move to another server', ['settings-server', {}], '#move');
    case 'account': {
      const n = call(ctx.accountName, parts[0]);
      return n ? item(`Account ${n}`, ['settings-server', { query: { account: parts[0] } }]) : item('An account no longer here', null, null, true);
    }
    case 'session': return item('A session', ['settings-server', { query: { session: parts[0] } }]);
    case 'key': {
      const c = carOf();
      const n = carName(c);
      const k = parts.length > 1 ? parts.slice(1).join(':') : parts[0];
      return item(n ? `Key ${k} of ${n}` : `Key ${k}`, ['settings-cars', n ? { car: c } : {}]);
    }
    case 'token': {
      const c = carOf();
      const n = carName(c);
      return item(n ? `A car token of ${n}` : 'A car token', ['settings-cars', n ? { car: c } : {}]);
    }
    case 'server': {
      const what = parts[0];
      if (what === 'release') return item('Software release', ['settings-server', {}]);
      return item(SERVER_ITEMS[what] || 'The server', ['settings-server', {}]);
    }
    case 'settings': {
      if (parts[0] === 'teslamate_local' || parts[0] === 'teslamate-local') return item('Local TeslaMate', ['settings-integrations', {}], '#teslamate');
      const s = SETTINGS_ITEMS[parts[0]];
      return s ? item(s[0], [s[1], {}], s[2]) : item('Server settings', ['settings-server', {}]);
    }
    case 'tesla':
    case 'teslafi': {
      const name = kind === 'tesla' ? 'Tesla' : 'TeslaFi';
      const acc = parts[0] || '';
      const hash = acc ? `#${kind}-${encodeURIComponent(acc)}` : `#${kind}`;
      return item(parts.length > 1 ? `A car of a ${name} account` : `A ${name} account`, ['settings-integrations', {}], hash);
    }
    case 'teslamate': return item('Local TeslaMate', ['settings-integrations', {}], '#teslamate');
    case 'ui-pack': {
      const n = call(ctx.packName, parts[0]) || (parts[0] ? cap(words(parts[0])) : '');
      return item(n ? `The ${n} design` : 'A design', ['settings-display', {}]);
    }
    default: return item('Something else', null, null, true);
  }
}
