// Sentry Mate, "Logbook" UI pack: Settings: Server and access (/settings/server).
//
// Who can sign in, what each account may see, and how the server is reached. Everyone gets: the check
// of the address in this browser against the certificate's host names and the server's web port, with
// a plain explanation of every mismatch; how access works; the account they are signed in with; and
// their own sessions (sign out one, every other one, or here). Admins also get: the host names (edited
// here, saved with the settings' version), the ports, the certificate, the recovery key and backups,
// the accounts (roles, car lists, two-step sign-in, a new password) and the audit log, whose entries
// link to the item they changed. Every table has a Download CSV button.

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, section, empty, ago, flag, button, icon, note, sep } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { field, form, choice, switchField, setFieldError } from '../core/form.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { dateTime, date, zoneLabel, count } from '../core/format.js';
import { moveBlock } from './settings-move.js';

export const id = 'settings-server';
export const css = true;
export const title = () => 'Settings: Server and access';

const ROLE = { admin: 'Admin', watch: 'Watch only' };
const ROLE_TEXT = {
  admin: 'Sees every car and every setting, and may change everything: accounts, cars, places, integrations and imports.',
  watch: 'Sees only the cars on its list, and changes only its own preferences and sessions.',
};
const PURPOSE = { web: 'Web pages and the API', upload: 'CAN Bridge uploads' };
const PURPOSE_NOTE = {
  web: 'This page, every other page and /api/v1. People sign in here.',
  upload: 'Where each CAN Bridge sends its logs, with its car’s upload token. No one signs in here.',
};
const HOST_RE = /^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*$/;
const isIpv4 = (s) => /^\d{1,3}(\.\d{1,3}){3}$/.test(s);
const isLoopback = (s) => s === 'localhost' || s === '[::1]' || s === '::1' || /^127\./.test(s);
const errText = (e) => (e && e.fields ? Object.values(e.fields).join(' ') : (e && e.message) || 'Something went wrong.');

/* ================================================================== small parts */

/** One setting row: the name and its explanation on the left, the value or control on the right. */
function row(name, desc, control, { cover = null, cls = null } = {}) {
  return h('div', { class: ['ss-row', cls], cover },
    h('div', { class: 'ss-row-text' }, h('h3', { class: 'ss-row-name' }, name), desc ? h('p', { class: 'ss-row-desc' }, desc) : null),
    control != null ? h('div', { class: 'ss-row-control' }, control) : null);
}

/** One outcome of a check: ok (green), info (slate) or warn (rust), with what it means and what to do. */
function outcome(kind, titleText, text, action = null) {
  const mark = kind === 'ok' ? 'check' : kind === 'warn' ? 'warning' : 'info';
  return h('li', { class: ['ss-out', `ss-out-${kind}`] },
    h('span', { class: 'ss-out-mark' }, icon(mark), h('span', { class: 'sr-only' }, kind === 'ok' ? 'Fine: ' : kind === 'warn' ? 'Needs a look: ' : 'Note: ')),
    h('div', null,
      h('p', { class: 'ss-out-title' }, titleText),
      text ? h('p', { class: 'ss-out-text' }, text) : null,
      action ? h('p', { class: 'ss-out-action' }, action) : null));
}

const mono = (text) => h('code', { class: 'mono ss-code' }, text);
const origin = (proto, host, port) => `${proto}//${host}${port && !((proto === 'https:' && port === 443) || (proto === 'http:' && port === 80)) ? `:${port}` : ''}`;

/* ================================================================== the page */

export async function mount(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const [settingsFirst, network, notices, cars] = await Promise.all([
    ctx.api.get('settings'),
    admin ? ctx.api.get('network').catch(() => null) : null,
    admin ? ctx.api.get('notices').catch(() => null) : null,
    (async () => { const out = []; for await (const c of ctx.api.list('cars', {}, { limit: 200 })) out.push(c); return out; })().catch(() => []),
  ]);
  let settings = settingsFirst;
  const carName = new Map(cars.map((c) => [c.id, c.name || c.vin_prefix || c.id]));
  const tz = ctx.tz;

  /* ---------------------------------------------------------------- how this server is reached */
  const loc = window.location;
  const here = { proto: loc.protocol, host: loc.hostname, port: loc.port ? Number(loc.port) : (loc.protocol === 'https:' ? 443 : 80) };
  const inbound = (network && network.inbound) || [];
  const webPort = inbound.find((p) => p.purpose === 'web')?.port ?? null;
  const addressBox = h('ol', { class: 'ss-outcomes' });

  function drawAddress() {
    const names = settings.host_names || [];
    const lower = names.map((n) => n.toLowerCase());
    const list = [];
    // 1. encryption
    if (here.proto === 'https:') {
      list.push(outcome('ok', 'This page came over HTTPS.', 'Passwords, codes and everything on the page travel encrypted (TLS 1.3).'));
    } else if (isLoopback(here.host)) {
      list.push(outcome('info', 'This page came over plain HTTP, on this computer only.',
        'That happens only with the development server on 127.0.0.1. Sentry Mate itself answers HTTPS only, so a password never travels unencrypted.'));
    } else {
      list.push(outcome('warn', 'This page came over plain HTTP.',
        'Sentry Mate itself answers HTTPS only, so something between you and the server removed the encryption (a reverse proxy set up without TLS). Do not sign in this way.',
        `Open ${origin('https:', here.host, webPort || 443)}/ instead, or give the proxy a certificate.`));
    }
    // 2. the name on the certificate
    const shown = loc.host;
    if (lower.includes(here.host.toLowerCase())) {
      list.push(outcome('ok', `“${here.host}” is on the certificate.`, 'Browsers accept the certificate under this name (once they trust it: it is self-signed unless you put your own in).'));
    } else if (isLoopback(here.host)) {
      list.push(outcome('info', `You reached it as ${shown}, the computer itself.`,
        `Other devices use one of the names below (${names.join(', ') || 'none set yet'}). The certificate does not need ${here.host}.`));
    } else {
      list.push(outcome('warn', `“${here.host}” is not on the certificate.`,
        `${isIpv4(here.host) ? 'You reached the server by its address. ' : ''}The certificate names ${names.length ? names.join(', ') : 'nothing yet'}, so browsers warn about this name. Pages still work after the warning, but it is easy to get used to clicking past a real one.`,
        admin ? 'Add the name below, then make a new certificate (the steps are under the names).' : 'Ask an admin to add it to the host names.'));
    }
    // 3. the port
    if (webPort == null) {
      list.push(outcome('info', `This page came through port ${here.port}.`, admin ? 'The server did not say which port it listens on.' : 'An admin sees the ports the server itself listens on.'));
    } else if (webPort === here.port) {
      list.push(outcome('ok', `Port ${here.port} is the server’s web port.`, 'You reach the server directly, with nothing in between.'));
    } else {
      list.push(outcome('info', `This page came through port ${here.port}; the server listens on ${webPort}.`,
        'Something maps one to the other: a port line in the Docker compose file (left number outside, right number inside), or a reverse proxy. Both are fine. A proxy must pass every address unchanged from the root (the pages live at /, not under a sub-path), and the server reads the visitor’s address from X-Forwarded-For only when the proxy is a trusted one.'));
    }
    // 4. the address meant for links: only the name is known here, not the port other devices use,
    //    so this is a condition to check, shown to admins (who see the server's ports), never a claim.
    if (names.length && webPort != null) {
      list.push(outcome('info', `Make sure ${names[0]} opens this server from your other devices.`,
        'The first host name is the one meant for links to this server, such as those in notices. Use it with the port other devices reach the server through: the left number of the port line in the compose file, or the port of your reverse proxy. The server itself cannot see that port.'));
    }
    replace(addressBox, list);
  }

  const addressSection = section({ title: 'How you reached this server', cover: 'web.shell.hosting', cls: 'ss-address' },
    h('p', { class: 'ss-lede' }, 'This page: ', mono(`${loc.origin}/`)),
    addressBox);

  /* ---------------------------------------------------------------- host names */
  let draft = [...(settings.host_names || [])];
  const namesList = h('ul', { class: 'ss-names' });
  const addField = field({ label: 'Add a name or address', name: 'host', hint: 'For example sentry-mate.local, nas.home.arpa or 192.168.1.20.', attrs: { autocomplete: 'off', spellcheck: 'false', maxlength: 253 } });
  const saveBtn = button('Save the names', { kind: 'primary', onclick: () => saveNames() });
  const undoBtn = button('Undo changes', { onclick: () => { draft = [...settings.host_names]; setFieldError(addField, null); drawNames(); } });
  const namesNote = h('p', { class: 'ss-state', 'aria-live': 'polite' });

  function drawNames() {
    const saved = settings.host_names || [];
    const changed = draft.length !== saved.length || draft.some((n, i) => n !== saved[i]);
    replace(namesList, draft.map((n, i) => h('li', { class: 'ss-name' },
      h('span', { class: 'mono ss-name-text' }, n),
      i === 0 ? flag('Used for links') : null,
      admin ? button('Remove', { cls: 'button-small', disabled: draft.length === 1, title: draft.length === 1 ? 'The server needs at least one name.' : null,
        onclick: () => { draft.splice(i, 1); drawNames(); } }) : null)));
    saveBtn.disabled = !changed;
    undoBtn.hidden = !changed;
    namesNote.textContent = changed ? 'Not saved yet.' : `${count(saved.length, 'name')}, saved.`;
    drawAddress();
  }
  function addName() {
    const v = addField.control.value.trim().replace(/\.$/, '');
    if (!v) return;
    if (!HOST_RE.test(v) || v.length > 253) { setFieldError(addField, 'A host name or address, such as sentry-mate.local: letters, digits, dashes and dots.'); return; }
    if (draft.some((n) => n.toLowerCase() === v.toLowerCase())) { setFieldError(addField, `${v} is already on the list.`); return; }
    if (draft.length >= 10) { setFieldError(addField, 'At most 10 names.'); return; }
    setFieldError(addField, null);
    draft.push(v);
    addField.control.value = '';
    drawNames();
  }
  addField.control.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); addName(); } });
  async function saveNames() {
    saveBtn.disabled = true;
    try {
      settings = await ctx.api.patch('settings', { version: settings.version, host_names: draft });
      draft = [...settings.host_names];
      ctx.flash('Host names saved. They go on the next certificate the server makes.');
    } catch (err) {
      if (err && err.code === 'stale_version') {
        settings = await ctx.api.get('settings');
        draft = [...settings.host_names];
        ctx.flash('Someone else changed the server settings meanwhile. The names shown are the saved ones now; make your change again.', { kind: 'error' });
      } else {
        const msg = err && err.fields ? Object.entries(err.fields).map(([k, m]) => { const i = Number(k.split('.')[1]); return Number.isFinite(i) && draft[i] ? `${draft[i]}: ${m}` : m; }).join(' ') : errText(err);
        setFieldError(addField, msg);
      }
    }
    drawNames();
  }

  const certSteps = h('ol', { class: 'ss-steps' },
    h('li', null, 'Stop Sentry Mate: ', mono('docker compose stop'), ' (TrueNAS: Stop the app).'),
    h('li', null, 'Delete ', mono('data/tls/cert.pem'), ', or put your own ', mono('cert.pem'), ' and ', mono('key.pem'), ' there.'),
    h('li', null, 'Start it again: ', mono('docker compose start'), '. It makes a new certificate with these names.'),
    h('li', null, 'A new certificate has a new SHA-256: run ', mono('sentry-mate fingerprint'), ' and enter it in each CAN Bridge, or they stop uploading.'));

  const namesSection = section({ title: 'Host names for the certificate', cover: 'docs.env.web.virtual-host', cls: 'ss-names-block' },
    note('The names and addresses people and devices use to reach this server. They go into the HTTPS certificate the server makes, so browsers accept those names, and the first one is the address the server puts in links to itself.'),
    namesList,
    admin ? h('div', { class: 'ss-add' }, addField, button('Add', { onclick: addName, cls: 'ss-add-btn' })) : null,
    admin ? h('div', { class: 'ss-save' }, saveBtn, undoBtn, namesNote) : h('p', { class: 'ss-state' }, 'Only an admin can change the names.'),
    h('details', { class: 'ss-details' },
      h('summary', null, 'Put a change into the certificate'),
      note('A running server keeps the certificate it started with. Saved names are used the next time a certificate is made:'),
      certSteps));

  /* ---------------------------------------------------------------- ports */
  const portsTable = inbound.length ? dataTable(ctx, {
    rows: inbound, itemName: ['port', 'ports'], label: 'Ports the server listens on',
    csv: { filename: 'sentry-mate-ports.csv' },
    columns: [
      { key: 'purpose', label: 'For', th: true, value: (p) => PURPOSE[p.purpose] || p.purpose,
        cell: (p) => h('span', null, h('span', { class: 'cell-main' }, PURPOSE[p.purpose] || p.purpose), h('span', { class: 'cell-sub' }, PURPOSE_NOTE[p.purpose] || '')) },
      { key: 'port', label: 'Port', num: true, value: (p) => p.port, cell: (p) => h('span', { class: 'ss-port' }, String(p.port), h('span', { class: 'q-u' }, ' TCP')) },
      { key: 'tls', label: 'Encryption', value: (p) => (p.tls ? 'TLS 1.3' : 'none'),
        cell: (p) => (p.tls ? h('span', { class: 'ss-ok ss-nowrap' }, 'HTTPS, TLS 1.3') : h('span', { class: 'ss-warn' }, 'None')) },
    ],
  }) : null;
  const portsSection = section({ title: 'Ports', cover: 'docs.env.web.port', cls: 'ss-ports' },
    note(inbound.length
      ? 'What the server listens on inside its container. Change one with the port line in the compose file: for another port outside, change only the left number.'
      : `This page’s address uses port ${here.port}. That is the port your browser connected to, which may belong to a port mapping or a reverse proxy rather than to the server. Only an admin sees the ports the server itself listens on.`),
    portsTable ? portsTable.el : null);

  /* ---------------------------------------------------------------- how access works */
  const myCars = ctx.me && ctx.me.cars === 'all' ? null : ((ctx.me && ctx.me.cars) || []);
  const myCarsText = myCars == null ? 'every car, including cars that appear later'
    : myCars.length ? myCars.map((c) => carName.get(c) || c).join(', ') : 'no car yet';
  const accessSection = section({ title: 'How access works', cover: 'web.shell.access-model', cls: 'ss-access' },
    h('div', { class: 'ss-me' },
      h('p', { class: 'ss-me-name' }, ctx.me ? ctx.me.name : '—', ' ', flag(ROLE[ctx.me && ctx.me.role] || 'Unknown role')),
      h('p', { class: 'ss-me-text' }, 'You see ', h('strong', null, myCarsText), '. ', admin ? 'You may change everything: accounts, cars, places, integrations and imports.' : 'You change only your own preferences and sessions; an admin changes the rest.')),
    h('dl', { class: 'ss-facts' },
      h('dt', null, 'Sign-in'), h('dd', null, 'Every page and every API call needs a signed-in account. Only the health address for container checks answers without one, and it says nothing but “serving”.'),
      h('dt', null, 'Roles'), h('dd', null, h('strong', null, 'Admin: '), ROLE_TEXT.admin, ' ', h('strong', null, 'Watch only: '), ROLE_TEXT.watch),
      h('dt', null, 'Cars'), h('dd', null, 'Each account has its own list of cars, or all of them. A car that is not on the list looks exactly like a car that does not exist.'),
      h('dt', null, 'Two-step'), h('dd', null, 'An admin can require a six-digit code from an authenticator app at each sign-in, per account.'),
      h('dt', null, 'Passwords'), h('dd', null, 'Only over HTTPS. Five wrong tries for one name within a minute, or 30 from one address, and sign-in waits a minute.'),
      h('dt', null, 'Other sites'), h('dd', null, 'The session cookie is sent to this site only, and every change carries a header another site cannot add, so a page elsewhere cannot act in your name.'),
      h('dt', null, 'Uploads'), h('dd', null, 'A CAN Bridge never uses a person’s account: it uploads with its car’s own token, made in Settings: Cars.'),
      h('dt', null, 'Audit log'), h('dd', null, 'Every sign-in and every change is recorded with who, when and what. ', admin ? 'Admins read it below.' : 'Only admins can read it.'),
      h('dt', null, 'At rest'), h('dd', null, 'Everything the server stores is encrypted, always.')));

  /* ---------------------------------------------------------------- certificate, recovery key, backups */
  let safetySection = null;
  if (admin) {
    const items = (notices && notices.items) || [];
    const cert = items.find((n) => n.code === 'certificate_expiring');
    const rk = items.find((n) => n.code === 'recovery_key_unsaved');
    const list = [];
    if (cert) {
      const d = cert.detail || {};
      list.push(outcome('warn', d.expires_ms != null ? `The certificate expires ${date(d.expires_ms, tz)} (in ${count(d.days, 'day')}).` : 'The certificate expires soon.',
        'After that browsers and the CAN Bridges refuse the connection. Make a new one with the steps under the host names.'));
    } else {
      list.push(outcome('ok', 'The certificate is not close to expiring.', 'A notice appears here and at the top of every page a few weeks before it does.'));
    }
    if (rk) {
      list.push(outcome('warn', 'The recovery key file is still on the server.',
        'With the key file it is the only way to open the data: lose both and nobody can. Copy the line that starts with SMRK- somewhere safe away from the server (a password manager, paper), then delete the file.',
        h('span', null, mono('sudo cat keys/RECOVERY-KEY.txt'), ' then ', mono('sudo rm keys/RECOVERY-KEY.txt'))));
    } else {
      list.push(outcome('ok', 'No recovery key file waits on the server.', 'Keep the recovery key apart from both the data backups and the key file. A new one: sentry-mate recovery-key new (the old one stops working).'));
    }
    list.push(outcome('info', 'The easiest backup: an export file.',
      'One file with everything, made and downloaded here (Move to another server, below). It opens only with the recovery key, so it can go anywhere.'));
    list.push(outcome('info', 'Or back up the data folder like any folder.',
      'It is sealed, so the copy can go anywhere. Stop Sentry Mate for a clean copy, keep the files’ names and layout, and copy it off the server. On TrueNAS, a periodic snapshot of the data dataset works while the app runs. Never put the keys folder in the same backup.',
      h('span', null, mono('docker compose stop'), ' · ', mono('sudo tar -C data -cpf ~/sentry-mate-data.tar .'), ' · ', mono('docker compose start'))));
    safetySection = section({ title: 'Certificate, recovery key and backups', cover: 'web.shell.hosting', cls: 'ss-safety' }, h('ol', { class: 'ss-outcomes' }, list));
  }

  /* ---------------------------------------------------------------- moving to another server (admin) */
  const mover = admin ? moveBlock(ctx, { network }) : null;

  /* ---------------------------------------------------------------- accounts (admin) */
  let accountsTable = null;
  let accountsSection = null;
  const focusAccount = ctx.query.account ? String(ctx.query.account) : null;
  if (admin) {
    const carsCell = (a) => (a.cars === 'all' ? h('span', null, 'All cars') : a.cars.length
      ? h('span', null, a.cars.map((c) => carName.get(c) || 'a car no longer here').join(', '))
      : h('span', { class: 'ss-quiet' }, 'No car: sees nothing yet'));
    accountsTable = dataTable(ctx, {
      path: 'accounts', limit: 50, itemName: ['account', 'accounts'], label: 'Accounts',
      rowAttrs: (a) => ({ class: { 'is-focus': a.id === focusAccount }, id: `account-${a.id}` }),
      columns: [
        { key: 'name', label: 'Name', th: true, cell: (a) => h('span', null, h('span', { class: 'cell-main' }, a.name), a.id === ctx.me.id ? [' ', flag('You')] : null) },
        { key: 'role', label: 'Role', cell: (a) => h('span', { title: ROLE_TEXT[a.role] }, ROLE[a.role] || a.role) },
        { key: 'cars', label: 'Sees', cls: 'ss-cars-col', cell: carsCell },
        { key: 'two_step_required', label: 'Two-step sign-in', cell: (a) => (a.two_step_required ? h('span', { class: 'ss-ok' }, 'Required') : h('span', { class: 'ss-quiet' }, 'Password only')) },
        { key: 'created_ms', label: 'Made', cell: (a) => date(a.created_ms, tz) },
        { key: 'actions', label: 'Change', cell: (a) => h('span', { class: 'ss-actions' },
          button('Edit', { cls: 'button-small', onclick: () => editAccount(a) }),
          button('Remove', { cls: 'button-small', kind: 'danger', onclick: () => removeAccount(a) })) },
      ],
      empty: () => empty({ title: 'No accounts.', text: 'There is always at least one admin; reload the page.' }),
    });
    accountsSection = section({ title: 'Accounts', note: 'who can sign in, and what each one sees', cover: 'web.shell.access-model', cls: 'ss-accounts',
      actions: button('New account', { kind: 'primary', onclick: () => editAccount(null) }) },
    accountsTable.el);
  }

  function carPicker(initial) {
    const chosen = new Set(initial === 'all' ? [] : initial);
    let mode = initial === 'all' ? 'all' : 'some';
    const search = h('input', { class: 'field-input', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car', autocomplete: 'off' });
    const listEl = h('ul', { class: 'ss-carlist' });
    const countEl = h('p', { class: 'ss-state' });
    const box = h('div', { class: 'ss-carpick' }, search, listEl, countEl);
    function draw() {
      const q = search.value.trim().toLowerCase();
      const shown = cars.filter((c) => !q || `${c.name || ''} ${c.model || ''} ${c.vin_prefix || ''}`.toLowerCase().includes(q));
      replace(listEl, shown.length ? shown.map((c) => h('li', null, h('label', { class: 'ss-check' },
        h('input', { type: 'checkbox', checked: chosen.has(c.id), onchange: (e) => { if (e.target.checked) chosen.add(c.id); else chosen.delete(c.id); drawCount(); } }),
        h('span', null, c.name || c.id), c.model ? h('span', { class: 'ss-quiet' }, ` ${[c.model, c.year].filter(Boolean).join(' ')}`) : null)))
        : h('li', { class: 'ss-quiet' }, q ? `No car matches “${search.value.trim()}”.` : 'There is no car yet.'));
      drawCount();
    }
    function drawCount() { countEl.textContent = `${count(chosen.size, 'car')} chosen of ${cars.length}.`; }
    search.addEventListener('input', draw);
    const pick = choice({ label: 'Sees', name: 'cars_mode', value: mode, options: [['all', 'All cars'], ['some', 'Only these cars']],
      onchange: (v) => { mode = v; box.hidden = v === 'all'; } });
    box.hidden = mode === 'all';
    draw();
    return { el: h('div', { class: 'field ss-cars-field', dataset: { field: 'cars' } }, pick, box, h('p', { class: 'field-error', hidden: true })), value: () => (mode === 'all' ? 'all' : [...chosen]) };
  }

  async function editAccount(a) {
    const isNew = !a;
    let role = a ? a.role : 'watch';
    let twoStep = a ? a.two_step_required : false;
    const name = field({ label: 'Name', name: 'name', value: a ? a.name : '', hint: 'What the person types to sign in.', attrs: { required: true, maxlength: 64, autocomplete: 'off' } });
    const pass = field({ label: isNew ? 'Password' : 'New password', name: 'password', type: 'password', hint: isNew ? 'At least 10 characters. Tell it to the person yourself.' : 'Leave empty to keep the current one. At least 10 characters.', attrs: { autocomplete: 'new-password', minlength: 10 } });
    const roleNote = h('p', { class: 'field-hint' }, ROLE_TEXT[role]);
    const rolePick = h('div', { class: 'field', dataset: { field: 'role' } },
      choice({ label: 'Role', name: 'role', value: role, options: [['watch', 'Watch only'], ['admin', 'Admin']], onchange: (v) => { role = v; roleNote.textContent = ROLE_TEXT[v]; } }),
      roleNote, h('p', { class: 'field-error', hidden: true }));
    const picker = carPicker(a ? a.cars : []);
    const two = h('div', { class: 'field', dataset: { field: 'two_step_required' } },
      switchField({ label: 'Require a two-step code at sign-in', checked: twoStep, onchange: (v) => { twoStep = v; },
        hint: 'The person sets up an authenticator app at their next sign-in, then types its six-digit code each time.' }),
      h('p', { class: 'field-error', hidden: true }));
    await openDialog({
      title: isNew ? 'New account' : `Edit ${a.name}`, wide: true, actions: [],
      body: (close) => form({
        fields: [name, pass, rolePick, picker.el, two], submitLabel: isNew ? 'Make the account' : 'Save the account',
        onsubmit: async (v) => {
          const body = { name: v.name.trim(), role, cars: picker.value(), two_step_required: twoStep };
          if (isNew || v.password) {
            if (!v.password || v.password.length < 10) throw Object.assign(new Error('Check the password.'), { fields: { password: 'At least 10 characters.' } });
            body.password = v.password;
          }
          if (!body.name) throw Object.assign(new Error('Check the name.'), { fields: { name: 'Type a name.' } });
          try {
            if (isNew) await ctx.api.post('accounts', body);
            else await ctx.api.patch(api.apiPath`accounts/${a.id}`, { version: a.version, ...body });
          } catch (err) {
            if (err && err.code === 'name_taken') throw Object.assign(new Error(err.message), { fields: { name: 'That name is taken.' } });
            if (err && err.code === 'stale_version') throw Object.assign(new Error('Someone else changed this account meanwhile. Close this, and edit it again.'), { fields: null });
            if (err && err.code === 'last_admin') throw Object.assign(new Error('This is the last admin account: make another admin first.'), { fields: { role: 'The last admin stays an admin.' } });
            throw err;
          }
          ctx.flash(isNew ? `Account ${body.name} made (${ROLE[role]}).` : `Account ${body.name} saved.`);
          close('saved');
          await accountsTable.reload({});
        },
      }),
    });
  }

  async function removeAccount(a) {
    const self = a.id === ctx.me.id;
    const ok = await confirmDialog({
      title: `Remove the account ${a.name}?`,
      text: `${self ? 'This is your own account: you are signed out at once. ' : ''}It can no longer sign in, and every session it has ends now. Its preferences go; the audit log keeps what it did. This cannot be undone.`,
      yes: 'Remove the account', danger: true,
    });
    if (!ok) return;
    try {
      await ctx.api.del(api.apiPath`accounts/${a.id}`);
      if (self) { ctx.flash(`Account ${a.name} removed.`, { next: true }); location.assign('/sign-in'); return; }
      ctx.flash(`Account ${a.name} removed.`);
      await accountsTable.reload({});
    } catch (err) {
      ctx.flash(err && err.code === 'last_admin' ? 'Not removed: this is the last admin account. Make another admin first.' : `Not removed: ${errText(err)}`, { kind: 'error' });
    }
  }

  /* ---------------------------------------------------------------- your sessions */
  const focusSession = ctx.query.session ? String(ctx.query.session) : null;
  const sessionsTable = dataTable(ctx, {
    path: 'me/sessions', limit: 50, itemName: ['session', 'sessions'], label: 'Your sessions',
    rowAttrs: (s) => ({ class: { 'is-focus': s.id === focusSession, 'is-this': s.this_one } }),
    columns: [
      { key: 'device', label: 'Browser', th: true, cell: (s) => h('span', null, h('span', { class: 'cell-main' }, s.device || 'A browser'), s.this_one ? [' ', flag('This one')] : null) },
      { key: 'created_ms', label: 'Signed in', cell: (s) => dateTime(s.created_ms, tz) },
      { key: 'last_seen_ms', label: 'Last used', cell: (s) => (s.this_one ? 'Now' : ago(s.last_seen_ms)) },
      { key: 'actions', label: 'Sign out', cell: (s) => button(s.this_one ? 'Sign out here' : 'Sign out', { cls: 'button-small', onclick: () => endSession(s) }) },
    ],
    empty: () => empty({ title: 'No session listed.', text: 'This browser is signed in, so it should be here: reload the page.' }),
  });
  async function endSession(s) {
    if (s.this_one) {
      try { await ctx.api.del('session'); } catch { /* signed out either way */ }
      ctx.flash('Signed out.', { kind: 'info', next: true });
      location.assign('/sign-in');
      return;
    }
    try {
      await ctx.api.del(api.apiPath`me/sessions/${s.id}`);
      ctx.flash(`Signed out ${s.device || 'that browser'}.`);
    } catch (err) {
      ctx.flash(err && err.status === 404 ? 'That session had already ended.' : `Not signed out: ${errText(err)}`, { kind: err && err.status === 404 ? 'info' : 'error' });
    }
    await sessionsTable.reload({});
  }
  async function endOthers() {
    const others = sessionsTable.items.filter((s) => !s.this_one).length;
    const ok = await confirmDialog({
      title: 'Sign out everywhere else?',
      text: `Every other browser signed in with your account (${count(others, 'session')} listed) must sign in again. This browser stays signed in.`,
      yes: 'Sign out the others', danger: true,
    });
    if (!ok) return;
    try {
      const r = await ctx.api.del('me/sessions');
      const n = r && r.ended != null ? r.ended : others;
      ctx.flash(n ? `Signed out ${count(n, 'other session')}.` : 'No other session was signed in.');
    } catch (err) { ctx.flash(`Not signed out: ${errText(err)}`, { kind: 'error' }); }
    await sessionsTable.reload({});
  }
  const sessionsSection = section({ title: 'Your sessions', note: 'each browser signed in with your account', cover: 'web.shell.access-model', cls: 'ss-sessions',
    actions: button('Sign out everywhere else', { onclick: endOthers }) },
  note('A session lasts 30 days from its last use. Sign out one you do not recognise, and change your password if you did not sign in there.'),
  sessionsTable.el);

  /* ---------------------------------------------------------------- audit log (admin) */
  let auditTable = null;
  let auditSection = null;
  if (admin) {
    const link = (href, text) => h('a', { class: 'ss-target', href }, text, icon('arrowRight'));
    // a drive, charge or update id does not say its car: find it when the link is used
    const finder = (text, find) => h('button', { type: 'button', class: 'ss-target ss-target-find', onclick: async (e) => {
      const b = e.currentTarget;
      b.disabled = true;
      try {
        const href = await find();
        if (href) { ctx.go(href); return; }
        ctx.flash(`${text} is no longer there, or belongs to a car no account here sees.`, { kind: 'info' });
      } catch (err) { if (!api.isAbort(err)) ctx.flash(`Could not open it: ${errText(err)}`, { kind: 'error' }); }
      b.disabled = false;
    } }, text, icon('arrowRight'));
    const inCars = async (fn) => { for (const c of cars) { const r = await fn(c.id).catch((err) => { if (err && err.status === 404) return null; throw err; }); if (r) return r; } return null; };
    const target = (t, summary) => {
      const s = String(t || '');
      const i = s.indexOf(':');
      const kind = i < 0 ? s : s.slice(0, i);
      const tid = i < 0 ? '' : s.slice(i + 1);
      switch (kind) {
        case 'account': return link(state.buildUrl('settings-server', { query: { account: tid } }) + `#account-${tid}`, 'The account');
        case 'session': return link(state.buildUrl('settings-server', { query: { session: tid } }), 'A session');
        case 'car': return carName.has(tid) ? link(state.buildUrl('settings-cars', { car: tid }), carName.get(tid)) : h('span', { class: 'ss-quiet' }, 'A car no longer here');
        case 'key': return link(state.buildUrl('settings-cars'), `CAN Bridge key ${tid}`);
        case 'place': return link(state.buildUrl('places', { query: { place: tid } }), 'The place');
        case 'import': return link(state.buildUrl('import'), 'The import');
        case 'settings': {
          if (tid === 'mqtt') return link(state.buildUrl('settings-integrations'), 'MQTT settings');
          const names = /host_names/.test(summary || '');
          const other = /units|range|car_order|geocode|tiles|release_check|outside_links/.test(summary || '');
          return h('span', { class: 'ss-targets' },
            names || !other ? link(state.buildUrl('settings-server'), 'Host names') : null,
            other || !names ? link(state.buildUrl('settings-display'), 'Server defaults') : null);
        }
        case 'trip': return finder('The saved trip', async () => {
          const tr = await ctx.api.get(api.apiPath`trips/${tid}`).catch((err) => { if (err && err.status === 404) return null; throw err; });
          return tr ? state.buildUrl('trip', { car: tr.car, period: { from: tr.from_ms, to: tr.to_ms } }) : null;
        });
        case 'drive': return finder('The drive', () => inCars(async (c) => (await ctx.api.get(api.apiPath`cars/${c}/drives/${tid}`)) && state.buildUrl('drive', { car: c, drive: tid })));
        case 'charge': return finder('The charge', () => inCars(async (c) => (await ctx.api.get(api.apiPath`cars/${c}/charges/${tid}`)) && state.buildUrl('charge', { car: c, charge: tid })));
        case 'update': return finder('The update', () => inCars(async (c) => {
          for await (const u of ctx.api.list(api.apiPath`cars/${c}/updates`, {}, { limit: 500 })) if (String(u.id) === tid) return state.buildUrl('software', { car: c });
          return null;
        }));
        default: return h('span', { class: 'mono ss-quiet' }, s || '—');
      }
    };
    auditTable = dataTable(ctx, {
      path: 'audit', limit: 50, itemName: ['entry', 'entries'], label: 'Audit log',
      columns: [
        { key: 'at_ms', label: 'When', th: true, cell: (e) => h('span', null, h('span', { class: 'cell-main' }, dateTime(e.at_ms, tz)), h('span', { class: 'cell-sub' }, ago(e.at_ms))) },
        { key: 'account', label: 'Who', cell: (e) => (e.account ? h('span', null, e.account) : h('span', { class: 'ss-quiet' }, 'The server itself')) },
        { key: 'summary', label: 'What happened', cls: 'ss-summary-col', cell: (e) => h('span', null, h('span', { class: 'cell-main' }, e.summary || '—'), h('span', { class: 'cell-sub mono' }, e.action)) },
        { key: 'target', label: 'Changed item', cell: (e) => target(e.target, e.summary), value: (e) => e.target },
      ],
      empty: () => empty({ title: 'Nothing recorded yet.', text: 'Each sign-in and each change appears here as it happens.' }),
    });
    auditSection = section({ title: 'Audit log', note: 'every sign-in and every change, newest first', cover: 'web.shell.access-model', cls: 'ss-audit' },
      note(`Times are in ${zoneLabel(tz)}. The last column opens the item that was changed, where it still exists. The log is sealed: no one can edit or remove an entry, admins included.`),
      auditTable.el);
  }

  /* ---------------------------------------------------------------- put together */
  drawNames();
  replace(ctx.main,
    pageHead({
      eyebrow: 'Settings', title: 'Server and access', deck: ctx.info.purpose,
      aside: ctx.me ? h('p', { class: 'ss-who' }, 'Signed in as ', h('strong', null, ctx.me.name), sep(), ROLE[ctx.me.role] || ctx.me.role) : null,
    }),
    h('div', { class: 'ss-grid' },
      h('div', { class: 'ss-col' }, addressSection, namesSection, portsSection),
      h('div', { class: 'ss-col' }, accessSection, safetySection)),
    mover ? mover.el : null, accountsSection, sessionsSection, auditSection);

  try {
    await Promise.all([portsTable && portsTable.load(), sessionsTable.load(), accountsTable && accountsTable.load(), auditTable && auditTable.load(), mover && mover.load()]);
  } catch (err) {
    if (!ctx.alive()) return undefined;
    throw err;
  }
  if (!ctx.alive()) return undefined;
  if (focusAccount) document.getElementById(`account-${focusAccount}`)?.scrollIntoView({ block: 'center' });
  else if (location.hash === '#move') document.getElementById('move')?.scrollIntoView({ block: 'start' });
  ctx.ready();
  return {};
}
