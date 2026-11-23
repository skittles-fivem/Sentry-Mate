// Sentry Mate, "Logbook" UI pack: Settings: Server and access (/settings/server).
//
// Who can sign in, what each account may see, and how the server is reached. Everyone gets: the check
// of the address in this browser against the certificate's host names and the server's web port, with
// a plain explanation of every mismatch; Server updates (this build and the release check's last answer:
// GET /about, GET /release; Check now for an admin, POST /release/check; the switch stays in Display and
// units); how access works; the account they are signed in with; and their own sessions (sign out one, every
// other one, or here). Admins also get: the certificate (its host names changed in a dialog, saved with the
// settings' version), the recovery key and backups,
// the accounts (roles, car lists, two-step sign-in, a new password) and the audit log, whose entries
// say what was done in words and link to the item they changed (filtered by what and who, loaded as the
// list scrolls), and Storage beside the move: the space on disk by kind and what compaction saved
// (GET /system/storage/detail), with "Compact now" (POST /system/storage/compact; while a pass runs the
// detail is read again every 2 s, until the page is left). Every table has a Download CSV button. Admins also
// get the full rebuild of every car (system/full-rebuild, its progress read again while it runs:
// rebuild-parts.js) and its automatic switch (full_rebuild.auto, saved with the settings' version).

import { h, replace, setVars } from '../shared/dom.js';
import { downloadCsv } from '../shared/csv.js';
import { storageGroups, shortName, shareText, groupWord, PACK_STEP } from '../shared/storage.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { format } from '../shared/units.js';
import { pageHead, section, empty, ago, flag, button, icon, note, sep, q } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { field, form, choice, switchField, setFieldError, soldPickRows } from '../core/form.js';
import { openDialog, confirmDialog, passwordOk } from '../core/dialog.js';
import { dateTime, date, zoneLabel, count } from '../core/format.js';
import { moveBlock, askPassword } from './settings-move.js';
import { bigCode, downloadText } from './move-parts.js';
import { fullRebuild } from './rebuild-parts.js';
import { AUDIT_GROUPS, actionLabel, auditItem } from '../shared/audit.js';
import { serverUpdates, RELEASE_OFF_TEXT, RELEASE_SETTING } from '../shared/release.js';

export const id = 'settings-server';
export const css = true;
export const title = () => 'Settings: Server and access';

const ROLE = { admin: 'Admin', watch: 'Watch only' };
const ROLE_TEXT = {
  admin: 'Sees every car and every setting, and may change everything: accounts, cars, places, integrations and imports.',
  watch: 'Sees only the cars on its list, and changes only its own preferences and sessions.',
};
const HOST_RE = /^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*$/;
const isIpv4 = (s) => /^\d{1,3}(\.\d{1,3}){3}$/.test(s);
const isLoopback = (s) => s === 'localhost' || s === '[::1]' || s === '::1' || /^127\./.test(s);
const errText = (e) => (e && e.fields ? Object.values(e.fields).join(' ') : (e && e.message) || 'Something went wrong.');
const bytes = (v) => q(v, 'bytes');

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
      action ? h('div', { class: 'ss-out-action' }, action) : null));
}

const mono = (text) => h('code', { class: 'mono ss-code' }, text);
// a SHA-256 in colon hex, in groups of four pairs that never break inside, so it wraps cleanly on a phone
const shaGroups = (sha) => h('code', { class: 'mono ss-value ss-sha', 'aria-label': `SHA-256 ${sha}` },
  (String(sha).match(/([0-9A-Fa-f]{2}:?){1,4}/g) || [String(sha)]).map((g) => h('span', { class: 'ss-sha-group' }, g)));
// an address that may break only after a slash or a colon
// (each part kept whole: a browser would otherwise also break after the hyphen in a host name)
const addressEl = (text) => h('code', { class: 'mono ss-value ss-address' },
  String(text).split(/(?<=[/:])/).map((part, i) => [i ? h('wbr') : null, h('span', { class: 'ss-address-part' }, part)]));

/** A Copy button for a value: the clipboard on HTTPS, else the value selected for Ctrl+C or the phone's Copy. */
function copyValue(text, valueEl) {
  const said = h('span', { class: 'ss-copied', 'aria-live': 'polite' });
  return h('span', { class: 'ss-copy' },
    button('Copy', { iconName: 'check', cls: 'button-small', onclick: async () => {
      try {
        if (!window.isSecureContext || !navigator.clipboard) throw new Error('no clipboard');
        await navigator.clipboard.writeText(text);
        said.textContent = 'Copied.';
      } catch {
        const sel = window.getSelection && window.getSelection();
        if (sel && valueEl) { sel.removeAllRanges(); const r = document.createRange(); r.selectNodeContents(valueEl); sel.addRange(r); }
        said.textContent = 'Selected: press Ctrl+C (on a phone, tap Copy) to copy it.';
      }
    } }), said);
}

const origin = (proto, host, port) => `${proto}//${host}${port && !((proto === 'https:' && port === 443) || (proto === 'http:' && port === 80)) ? `:${port}` : ''}`;

/* ================================================================== the page */

export async function mount(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const [settingsFirst, networkFirst, notices, cars] = await Promise.all([
    ctx.api.get('settings'),
    admin ? ctx.api.get('network').catch(() => null) : null,
    admin ? ctx.api.get('notices', null, { maxAgeMs: 5000 }).catch(() => null) : null, // the frame's answer of this load
    (async () => { const out = []; for await (const c of ctx.api.list('cars', {}, { limit: 200 })) out.push(c); return out; })().catch(() => []),
  ]);
  let settings = settingsFirst;
  let network = networkFirst;
  // the names in the certificate the server runs with now (admins); saved host names go into the next one
  const certNames = () => (network && network.certificate && Array.isArray(network.certificate.names) ? network.certificate.names.map((n) => n.toLowerCase()) : null);
  // the owner's own certificate (put in data/tls): the server never makes a new one in its place
  const certOwn = () => !!(network && network.certificate && network.certificate.own);
  const ownSteps = () => h('span', null, 'Put the new ', mono('cert.pem'), ' and ', mono('key.pem'), ' in ', mono('data/tls'), ' and start Sentry Mate again; then enter the new SHA-256 in each CAN Bridge.');
  const carName = new Map(cars.map((c) => [c.id, c.name || c.vin_prefix || c.id]));
  const tz = ctx.tz;

  /* ---------------------------------------------------------------- how this server is reached */
  const loc = window.location;
  const here = { proto: loc.protocol, host: loc.hostname, port: loc.port ? Number(loc.port) : (loc.protocol === 'https:' ? 443 : 80) };
  const inbound = (network && network.inbound) || [];
  const webPort = inbound.find((p) => p.purpose === 'web')?.port ?? null;
  const addressBox = h('ol', { class: 'ss-outcomes', cover: 'docs.env.web.port' });

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
    const inCert = certNames();
    if (inCert && lower.includes(here.host.toLowerCase()) && !inCert.includes(here.host.toLowerCase())) {
      list.push(outcome('info', `“${here.host}” is saved, not yet in the certificate.`,
        certOwn()
          ? `The certificate the server uses now is your own and names ${inCert.join(', ') || 'no host'}. Browsers keep warning about ${here.host} until your certificate names it too.`
          : `The certificate the server uses now names ${inCert.join(', ') || 'no host'}. Browsers keep warning about ${here.host} until you make a new certificate.`,
        !admin ? null : certOwn() ? ownSteps() : button('Make a new certificate', { iconName: 'reset', onclick: () => makeCertificate() })));
    } else if (lower.includes(here.host.toLowerCase())) {
      list.push(outcome('ok', `“${here.host}” is on the certificate.`, 'Browsers accept the certificate under this name (once they trust it: it is self-signed unless you put your own in).'));
    } else if (isLoopback(here.host)) {
      list.push(outcome('info', `You reached it as ${shown}, the computer itself.`,
        `Other devices use one of its names (${names.join(', ') || 'none set yet'}). The certificate does not need ${here.host}.`));
    } else {
      list.push(outcome('warn', `“${here.host}” is not on the certificate.`,
        `${isIpv4(here.host) ? 'You reached the server by its address. ' : ''}The certificate names ${names.length ? names.join(', ') : 'nothing yet'}, so browsers warn about this name. Pages still work after the warning, but it is easy to get used to clicking past a real one.`,
        !admin ? 'Ask an admin to add it to the host names.'
          : certOwn() ? 'The server runs with your own certificate: get one that names it as well (Change names, under Certificate, recovery key and backups, has the steps).'
            : h('span', null, 'Add the name, then make a new certificate: ', button('Change names', { iconName: 'settings', onclick: () => changeNames() }))));
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

  /* ---------------------------------------------------------------- host names (Change names: a dialog) */
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

  const certSteps = certOwn() ? h('ol', { class: 'ss-steps' },
    h('li', null, 'The server runs with your own certificate, so it never makes one in its place: these names go into a certificate it makes, not into yours.'),
    h('li', null, 'Get a certificate with the names you need, put its ', mono('cert.pem'), ' and ', mono('key.pem'), ' in ', mono('data/tls'), ' and start Sentry Mate again. The key is sealed at start.'),
    h('li', null, 'A new certificate has a new SHA-256: copy it from Certificate, recovery key and backups and enter it in each CAN Bridge, or they stop uploading.'),
    h('li', null, 'To go back to a certificate the server makes: stop Sentry Mate, delete ', mono('data/tls/cert.pem'), ', start it again.'))
  : h('ol', { class: 'ss-steps' },
    h('li', null, 'Save the names.'),
    h('li', null, 'Press ', h('strong', null, 'Make a new certificate'), ' (here, or under Certificate, recovery key and backups). The server makes it with these names and uses it at once, without a restart.'),
    h('li', null, 'A new certificate has a new SHA-256: copy it there and enter it in each CAN Bridge, or they stop uploading.'),
    h('li', null, 'With a shell instead: stop Sentry Mate, delete ', mono('data/tls/cert.pem'), ' (or put your own ', mono('cert.pem'), ' and ', mono('key.pem'), ' there), start it again, then run ', mono('sentry-mate fingerprint'), '.'));

  const namesEditor = h('div', { class: 'ss-names-block' },
    note('The names and addresses people and devices use to reach this server. They go into the HTTPS certificate the server makes, so browsers accept those names, and the first one is the address the server puts in links to itself.'),
    namesList,
    // the Add button sits in the field's row, beside the input (centred on it), with the hint below both
    admin ? h('div', { class: 'ss-add' }, (addField.querySelector('.field-control').append(button('Add', { onclick: addName, cls: 'ss-add-btn' })), addField)) : null,
    admin ? h('div', { class: 'ss-save' }, saveBtn, undoBtn, namesNote) : null,
    h('details', { class: 'ss-details' },
      h('summary', null, 'Put a change into the certificate'),
      note('Saved names are used the next time a certificate is made:'),
      certSteps));
  async function changeNames() {
    draft = [...(settings.host_names || [])];
    setFieldError(addField, null);
    drawNames();
    const go = await openDialog({ title: 'Names for the certificate', wide: true, cover: 'docs.env.web.virtual-host', body: namesEditor,
      actions: certOwn() ? [{ label: 'Close', value: null }] : [{ label: 'Close', value: null }, { label: 'Make a new certificate', value: 'cert', kind: 'primary' }] });
    if (go === 'cert' && ctx.alive()) makeCertificate();
  }

  /* ---------------------------------------------------------------- server updates (everyone; Check now: admins) */
  let about = null, release = null;
  [about, release] = await Promise.all([ctx.api.get('about', null, { maxAgeMs: 600000 }).catch(() => null), ctx.api.get('release').catch(() => null)]);
  if (!ctx.alive()) return undefined;
  const updBox = h('div', { class: 'ss-upd' });
  let updChecking = false, updTimer = null;
  ctx.onCleanup?.(() => clearTimeout(updTimer));
  async function updCheckNow() {
    updChecking = true; drawUpdates();
    try {
      release = await ctx.api.post('release/check', {});
      if (release && release.status === 'error') ctx.flash(`Could not check: ${(release.error && release.error.message) || 'no answer'}`, { kind: 'error' });
    } catch (err) {
      if (!ctx.alive()) return;
      ctx.flash(err && err.code === 'release_check_off' ? 'Not checked: the release check is off.' : `Not checked: ${errText(err)}`, { kind: 'error' });
      release = await ctx.api.get('release').catch(() => release);
    }
    updChecking = false;
    if (ctx.alive()) drawUpdates();
  }
  function drawUpdates() {
    const u = serverUpdates(about, release, api.now());
    const busy = updChecking || u.busy;
    replace(updBox,
      h('dl', { class: 'ss-upd-facts' }, u.rows.map((r) => h('div', { class: 'ss-upd-row' }, h('dt', null, r.label),
        h('dd', { class: r.tone ? `ss-upd--${r.tone}` : null }, h('span', { title: r.title || null }, r.text), r.badge ? [' ', flag(r.badge)] : null)))),
      h('p', { class: 'ss-upd-on' }, h('strong', null, `The release check is ${u.on ? 'on' : 'off'}. `),
        u.on ? 'Once a day the server asks GitHub whether a newer image is published.' : RELEASE_OFF_TEXT,
        !u.on ? [' ', admin ? h('a', { href: `${state.buildUrl(RELEASE_SETTING.page)}#${RELEASE_SETTING.hash}` }, `${RELEASE_SETTING.text}.`) : 'An admin can turn it on.'] : null),
      h('p', { class: 'note ss-upd-how' }, `${u.how} Steps for Docker and TrueNAS: `, u.doc.href ? h('a', { href: u.doc.href, target: '_blank', rel: 'noopener noreferrer external' }, u.doc.text) : mono(u.doc.path), '.'),
      admin ? h('p', { class: 'ss-cert-act' }, button(busy ? 'Checking…' : 'Check now', { iconName: 'reset', disabled: !u.on || busy, title: u.on ? null : 'Turn the release check on first', onclick: updCheckNow })) : null);
    if (release && release.status === 'checking' && !updChecking) {
      clearTimeout(updTimer);
      updTimer = setTimeout(async () => { release = await ctx.api.get('release').catch(() => release); if (ctx.alive()) drawUpdates(); }, 4000);
    }
  }
  drawUpdates();
  const updatesSection = section({ title: 'Server updates', cls: 'ss-updates', id: 'updates' }, updBox);

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
  // the certificate in use: its SHA-256, its names and the CAN Bridge's upload address, each to copy, and a
  // new one made here (no shell, no restart)
  const certBox = h('div', { class: 'ss-cert' });
  function drawCert() {
    const c = (network && network.certificate) || null;
    const rows = [];
    if (c && c.sha256) {
      const v = shaGroups(c.sha256);
      rows.push(h('div', { class: 'ss-fact' }, h('dt', null, 'SHA-256'), h('dd', null, v, copyValue(c.sha256, v))));
    }
    if (c && c.names && c.names.length) {
      rows.push(h('div', { class: 'ss-fact', cover: 'docs.env.web.virtual-host' }, h('dt', null, 'Names in it'),
        h('dd', null, h('ul', { class: 'ss-cert-names' }, c.names.map((n) => h('li', null, h('code', { class: 'mono ss-value' }, n)))),
          button('Change names', { iconName: 'settings', cls: 'button-small ss-names-btn', onclick: () => changeNames() }))));
    }
    if (network && network.upload_address) {
      const v = addressEl(network.upload_address);
      rows.push(h('div', { class: 'ss-fact' }, h('dt', null, 'CAN Bridge upload address'), h('dd', null, v, copyValue(network.upload_address, v))));
    }
    replace(certBox,
      rows.length ? h('dl', { class: 'ss-certfacts' }, rows) : h('p', { class: 'ss-quiet' }, 'The server did not say which certificate it runs with.'),
      c && c.sha256 ? h('p', { class: 'ss-cert-need' }, 'The CAN Bridge app needs this SHA-256 in its upload settings.') : null,
      certOwn()
        ? h('p', { class: 'ss-cert-own' }, 'This is your own certificate (the ', mono('cert.pem'), ' in ', mono('data/tls'), '), so the server never replaces it. To change it, put the new ', mono('cert.pem'), ' and ', mono('key.pem'), ' there and start Sentry Mate again. For a certificate the server makes instead, delete ', mono('data/tls/cert.pem'), ' and start it again.')
        : h('p', { class: 'ss-cert-act' }, button('Make a new certificate', { iconName: 'reset', onclick: () => makeCertificate() })));
  }
  if (admin) drawCert();
  // the names the next certificate is made for: localhost and the saved host names (what the server does)
  const nextNames = () => ['localhost', ...(settings.host_names || []).filter((n) => n.toLowerCase() !== 'localhost')];
  async function makeCertificate() {
    const names = nextNames();
    const out = await askPassword(ctx, {
      title: 'Make a new certificate',
      text: h('div', { class: 'dialog-text ss-newcert' },
        h('p', null, 'The server makes a new certificate and uses it at once on both ports, without a restart. Before you go on:'),
        h('ul', { class: 'ss-newcert-list' },
          h('li', null, h('strong', null, 'The SHA-256 changes. '), 'The CAN Bridge app’s uploads are refused until you enter the new SHA-256 in its upload settings.'),
          h('li', null, h('strong', null, 'Browsers ask again '), 'to accept the new certificate, on every device that opens these pages.'),
          h('li', null, h('strong', null, 'The names in it: '), names.map((n, i) => [i ? ', ' : '', h('code', { class: 'mono ss-newcert-name' }, n)]), '. Change them first with Change names, if one is missing.'))),
      yes: 'Make a new certificate', danger: true,
      hint: 'It is asked again because each CAN Bridge stops uploading until it has the new SHA-256.',
      run: (creds) => ctx.api.post('network/certificate', creds),
    });
    if (!out) return;
    try { network = await ctx.api.get('network'); } catch { network = { ...(network || {}), certificate: { names: out.names, sha256: out.sha256 }, upload_address: out.upload_address }; }
    drawCert(); drawAddress();
    const v = shaGroups(out.sha256);
    await openDialog({
      title: 'The new certificate is in use', cls: 'ss-newcert-dialog',
      body: h('div', { class: 'ss-newcert' },
        h('p', { class: 'dialog-text' }, 'Enter this SHA-256 in each CAN Bridge app’s upload settings now: until then it refuses this server.'),
        h('div', { class: 'ss-fact ss-fact-big' }, h('p', { class: 'ss-fact-label' }, 'New SHA-256'), v, copyValue(out.sha256, v)),
        h('p', { class: 'dialog-text' }, 'Reload this page to accept the new certificate in this browser.')),
      actions: [{ label: 'Done', value: true, kind: 'primary' }],
    });
  }
  let safetySection = null;
  if (admin) {
    const items = (notices && notices.items) || [];
    const cert = items.find((n) => n.code === 'certificate_expiring');
    const rk = items.find((n) => n.code === 'recovery_key_unsaved');
    const list = [];
    if (cert) {
      const d = cert.detail || {};
      list.push(outcome('warn', d.expires_ms != null ? `The certificate expires ${date(d.expires_ms, tz)} (in ${count(d.days, 'day')}).` : 'The certificate expires soon.',
        certOwn()
          ? 'After that browsers and the CAN Bridges refuse the connection. It is your own certificate, so the server cannot make a new one: renew it where you got it, then put its files in place as below.'
          : 'After that browsers and the CAN Bridges refuse the connection. Make a new one with the button below.', certBox));
    } else {
      list.push(outcome('ok', 'The certificate is not close to expiring.', 'It keeps the same SHA-256 across restarts, updates and moves, until you make a new one.', certBox));
    }
    // the recovery key: one list item, drawn again as it changes (saved, shown again, a new one made)
    let rkUnsaved = !!rk;
    const keyItem = () => (rkUnsaved
      ? outcome('warn', 'The recovery key is not saved yet.',
        'Nobody ticked “I have saved it”, so the server still keeps the key in its keys folder. With the keys folder it is the only way to open the data: lose both and nobody can. Show it once more, keep it somewhere safe away from the server (a password manager, paper), then tick that you saved it: the server then deletes its copy.',
        h('span', { class: 'ss-actions ss-rk-actions' },
          button('Show the recovery key', { kind: 'primary', onclick: () => showRecoveryKey() }),
          button('I saved it already', { onclick: () => savedAlready() }),
          button('Make a new recovery key', { onclick: () => newRecoveryKey() })))
      : outcome('ok', 'No recovery key file waits on the server.',
        'Keep the recovery key apart from the data backups and the keys folder. The server cannot show it again: use the copy you saved. Lost it? Make a new one here: the old one then stops working.',
        h('span', { class: 'ss-actions ss-rk-actions' }, button('Make a new recovery key', { onclick: () => newRecoveryKey() }))));
    let rkEl = keyItem();
    list.push(rkEl);
    const redrawKey = () => { const n = keyItem(); rkEl.replaceWith(n); rkEl = n; };
    async function markSaved() {
      await ctx.api.post('system/recovery-key/saved', {});
      rkUnsaved = false;
      redrawKey();
    }
    async function savedAlready() {
      const ok = await confirmDialog({
        title: 'You saved the recovery key already?',
        text: 'The server then deletes its copy, and nothing can show the key again. Say yes only if you have it somewhere safe (from the set-up page, or from “Show the recovery key”).',
        yes: 'Yes, I have it',
      });
      if (!ok) return;
      try { await markSaved(); } catch (err) { await openDialog({ title: 'The server did not hear it', body: h('p', { class: 'dialog-text' }, `${errText(err)} Try again in a moment. The key file stays on the server meanwhile.`) }); }
    }
    async function showRecoveryKey() {
      const out = await askPassword(ctx, {
        title: 'Show the recovery key',
        text: h('div', { class: 'dialog-text' },
          h('p', null, 'The key the server keeps until someone ticks “I have saved it”. Make sure nobody looks over your shoulder.')),
        yes: 'Show the key',
        hint: 'It is asked again because this key opens all the data.',
        run: async (creds) => {
          try { return await ctx.api.post('system/recovery-key/show', creds); } catch (err) {
            if (err && err.code === 'recovery_key_saved') return { gone: true };
            throw err;
          }
        },
      });
      if (!out) return;
      if (out.gone) { rkUnsaved = false; redrawKey(); return; }
      await keyDialog(out.recovery_key, { title: 'Save the recovery key', why: 'shown again' });
    }
    async function newRecoveryKey() {
      const out = await askPassword(ctx, {
        title: 'Make a new recovery key',
        text: h('div', { class: 'dialog-text' },
          h('p', null, 'The new key is shown once. The old one stops working at once: keep it only for export files and backups made before now, which still open with the old key alone.'),
          h('p', null, 'Make one when the old key is lost, or when someone you no longer trust has seen it.')),
        yes: 'Make the new key', danger: true,
        hint: 'It is asked again because this key opens all the data.',
        run: async (creds) => {
          try { return await ctx.api.post('system/recovery-key/new', creds); } catch (err) {
            if (err && err.code === 'moved') throw new Error('This server has moved: make the new key on the new server.');
            if (err && err.code === 'setup_busy') throw new Error('A move or an import comes in now. Make the new key once it is done.');
            if (err && err.status === 503) throw new Error(`The server is busy with another key. Wait ${Math.ceil((err.retryAfterMs || 5000) / 1000)} seconds, then try again.`);
            throw err;
          }
        },
      });
      if (!out) return;
      rkUnsaved = true;
      redrawKey();
      await keyDialog(out.recovery_key, { title: 'Your new recovery key', why: 'new key made' });
    }
    async function keyDialog(key, { title, why }) {
      const fileText = [
        'Sentry Mate recovery key', '', key, '',
        `Saved ${new Date(api.now()).toISOString().slice(0, 10)} from ${window.location.host} (${why}).`,
        'Keep it away from the server and its backups: in a password manager, or on paper.',
        'It opens this server\'s data and its export files. Lose it together with the keys folder, and nobody can.', '',
      ].join('\n');
      const tick = h('input', { type: 'checkbox', id: 'ss-rk-saved' });
      const err = h('p', { class: 'form-error', role: 'alert', hidden: true });
      const go = button('Done', { kind: 'primary', disabled: true });
      tick.addEventListener('change', () => { go.disabled = !tick.checked; });
      await openDialog({
        title, actions: [], cls: 'ss-rk-dialog',
        body: (close) => {
          go.addEventListener('click', async () => {
            go.disabled = true; err.hidden = true;
            try { await markSaved(); } catch (e) {
              err.textContent = `The server did not hear it: ${errText(e)} Try again in a moment. The key stays valid either way.`;
              err.hidden = false; go.disabled = false; return;
            }
            close('saved');
          });
          return [
            bigCode(key, { label: 'Recovery key' }),
            h('p', { class: 'ss-rk-download' }, button('Download as a file', { iconName: 'download', onclick: () => downloadText('sentry-mate-recovery-key.txt', fileText) })),
            h('p', { class: 'dialog-text' }, 'Keep it in a password manager or on paper, away from the server and its backups. It opens the data when the keys folder is lost, and it opens export files on a new server.'),
            h('label', { class: 'ss-check ss-rk-tick', for: 'ss-rk-saved' }, tick, h('span', null, 'I have saved the recovery key somewhere safe, away from this server.')),
            err,
            h('p', { class: 'ss-rk-go' }, go),
            h('p', { class: 'dialog-text ss-rk-note' }, 'Closing this without the tick keeps the server’s copy: you can show it here again. Ticking it deletes that copy.'),
          ];
        },
      });
    }
    list.push(outcome('info', 'The easiest backup: an export file.',
      'One file with everything, made and downloaded here (Move to another server, below). It opens only with the recovery key, so it can go anywhere.'));
    list.push(outcome('info', 'Or back up the data folder like any folder.',
      'It is sealed, so the copy can go anywhere. Stop Sentry Mate for a clean copy, keep the files’ names and layout, and copy it off the server. On TrueNAS, a periodic snapshot of the data dataset works while the app runs. Never put the keys folder in the same backup.',
      h('span', { class: 'ss-quiet' }, 'With a shell (Docker): ', mono('docker compose stop'), ' · ', mono('sudo tar -C data -cpf ~/sentry-mate-data.tar .'), ' · ', mono('docker compose start'))));
    safetySection = section({ title: 'Certificate, recovery key and backups', cover: 'web.shell.hosting', cls: 'ss-safety', id: 'recovery-key' }, h('ol', { class: 'ss-outcomes' }, list));
  }

  /* ---------------------------------------------------------------- moving to another server (admin) */
  const mover = admin ? moveBlock(ctx, { network }) : null;

  /* ---------------------------------------------------------------- full rebuild (admin) */
  let full = null;
  let fullSection = null;
  if (admin) {
    const autoOn = () => !settings.full_rebuild || settings.full_rebuild.auto !== false;
    const auto = switchField({
      label: 'Rebuild automatically when a new server version decodes the stored logs differently',
      hint: 'On by default. Off: a notice offers the full rebuild instead, and you start it here.',
      checked: autoOn(),
      onchange: async (on) => {
        try {
          settings = await ctx.api.patch('settings', { version: settings.version, full_rebuild: { auto: on } });
          ctx.flash('Saved for everyone on this server.');
        } catch (err) {
          if (err && (err.code === 'stale_version' || err.status === 409)) {
            settings = await ctx.api.get('settings').catch(() => settings);
            ctx.flash('Someone else changed the server settings meanwhile. The switch shows the saved setting now; make your change again.', { kind: 'error' });
          } else ctx.flash(`Not saved: ${errText(err)}`, { kind: 'error' });
          if (ctx.alive()) auto.set(autoOn());
        }
      },
    });
    full = fullRebuild(ctx, { carName: (c) => carName.get(c) || c, after: h('div', { class: 'fr-auto' }, auto) });
    fullSection = section({ title: 'Full rebuild', note: 'every car’s history made again from the stored data', cls: 'ss-full', id: 'full-rebuild' }, full.el);
  }

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
    let soldOpen = false;
    function draw() {
      const q = search.value.trim().toLowerCase();
      const shown = cars.filter((c) => !q || `${c.name || ''} ${c.model || ''} ${c.vin_prefix || ''}`.toLowerCase().includes(q));
      // Sold cars wait behind their own toggle unless ticked or searched for.
      const { rows } = soldPickRows(shown, {
        chosen: (c) => chosen.has(c.id), searching: !!q, open: soldOpen, ontoggle: () => { soldOpen = !soldOpen; draw(); },
        row: (c) => h('li', null, h('label', { class: 'ss-check' },
          h('input', { type: 'checkbox', checked: chosen.has(c.id), onchange: (e) => { if (e.target.checked) chosen.add(c.id); else chosen.delete(c.id); drawCount(); } }),
          h('span', null, c.name || c.id), c.model ? h('span', { class: 'ss-quiet' }, ` ${[c.model, c.year].filter(Boolean).join(' ')}`) : null,
          state.isSold(c) ? [' ', flag('Sold')] : null)),
      });
      replace(listEl, shown.length ? rows
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
    const pass = field({ label: isNew ? 'Password' : 'New password', name: 'password', type: 'password', hint: isNew ? 'Tell it to the person yourself.' : 'Leave empty to keep the current one.', attrs: { autocomplete: 'new-password' } });
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
        check: async (v) => {
          const n = v.name.trim();
          if (!n) throw Object.assign(new Error('Check the name.'), { fields: { name: 'Type a name.' } });
          if (!isNew && !v.password) return true;
          if (!v.password) throw Object.assign(new Error('Check the password.'), { fields: { password: 'Type a password.' } });
          return passwordOk(n, v.password, pass.control);
        },
        onsubmit: async (v) => {
          const body = { name: v.name.trim(), role, cars: picker.value(), two_step_required: twoStep };
          if (v.password) body.password = v.password;
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
  // Each entry in words (shared/audit.js): what was done (the code in the tooltip), and the item it changed,
  // linked where it still exists. Filters: what (a group of actions) and who. The list scrolls in a box five
  // entries high; ten entries first, more as the end of the box comes near (or with Load more), added under
  // the ones already there.
  let auditTable = null;
  let auditSection = null;
  let auditPrep = null;
  if (admin) {
    const people = new Map();   // account id -> name
    const trips = new Map();
    auditPrep = Promise.all([
      ctx.api.list('accounts', {}).all({ max: 2000 }).then((list) => { for (const a of list) people.set(a.id, a.name); }).catch(() => {}),
      ctx.api.list('trips', {}).all({ max: 2000 }).then((list) => { for (const t of list) trips.set(String(t.id), t); }).catch(() => {}),
    ]);
    const itemCtx = { carName: (c) => carName.get(c), accountName: (a) => people.get(a), trip: (t) => trips.get(String(t)) };
    const itemCell = (e) => {
      const it = auditItem(e, itemCtx);
      if (!it) return h('span', { class: 'ss-quiet' }, '—');
      if (!it.route || it.faint) return h('span', { class: 'ss-quiet' }, it.text);
      return h('a', { class: 'ss-target', href: state.buildUrl(it.route[0], it.route[1] || {}) + (it.hash || '') }, it.text, icon('arrowRight'));
    };
    const filters = { group: '', who: '' };
    const params = () => ({ ...(filters.group ? { group: filters.group } : {}), ...(filters.who ? { who: filters.who } : {}) });
    const whatField = field({ label: 'What', name: 'audit-group', type: 'select', value: '', options: [['', 'All'], ...AUDIT_GROUPS.map((g) => [g.id, g.label])] });
    const whoField = field({ label: 'Who', name: 'audit-who', type: 'select', value: '', options: [['', 'All'], ['*server', 'The server']] });
    const whoOptions = () => {
      const names = [...new Set(people.values())].sort((a, b) => a.localeCompare(b));
      whoField.control.replaceChildren(h('option', { value: '' }, 'All'), ...names.map((n) => h('option', { value: n }, n)), h('option', { value: '*server' }, 'The server'));
      whoField.control.value = filters.who;
    };
    const refilter = () => { filters.group = whatField.control.value; filters.who = whoField.control.value; auditTable.reload(params()); };
    whatField.control.addEventListener('change', refilter);
    whoField.control.addEventListener('change', refilter);
    auditTable = dataTable(ctx, {
      path: 'audit', limit: 10, itemName: ['entry', 'entries'], label: 'Audit log', cls: 'ss-audit-log',
      append: true, autoMore: true, moreLabel: 'Load more', box: 'Audit log entries',
      filters: h('div', { class: 'ss-audit-filters' }, whatField, whoField),
      columns: [
        { key: 'at_ms', label: 'When', th: true, cls: 'ss-a-when', cell: (e) => h('span', null, h('span', { class: 'cell-main' }, dateTime(e.at_ms, tz)), h('span', { class: 'cell-sub' }, ago(e.at_ms))) },
        { key: 'account', label: 'Who', cls: 'ss-a-who', cell: (e) => (e.account ? h('span', null, e.account) : h('span', { class: 'ss-quiet' }, 'The server')) },
        { key: 'action', label: 'Action', cls: 'ss-a-action', cell: (e) => h('span', { class: 'ss-action', title: e.action }, actionLabel(e.action)) },
        { key: 'summary', label: 'What happened', cls: 'ss-summary-col ss-a-summary', cell: (e) => h('span', null, e.summary || '—') },
        { key: 'target', label: 'Item', cls: 'ss-a-item', cell: itemCell, value: (e) => e.target },
      ],
      empty: () => (filters.group || filters.who
        ? empty({ title: 'No entry matches.', text: 'Nothing recorded fits this what and who. Choose All in one of them to see more.' })
        : empty({ title: 'Nothing recorded yet.', text: 'Each sign-in and each change appears here as it happens.' })),
    });
    auditPrep = auditPrep.then(whoOptions);
    auditSection = section({ title: 'Audit log', note: 'every sign-in and every change, newest first', cover: 'web.shell.access-model', cls: 'ss-audit' },
      note(`Times are in ${zoneLabel(tz)}. The last column opens the item that was changed, where it still exists. The log is sealed: no one can edit or remove an entry, admins included.`),
      auditTable.el);
  }

  /* ---------------------------------------------------------------- storage and compaction (admin) */
  // A compact block beside the move: one row of figures (on disk, free, saved by compaction, the last pass) and
  // Compact now, then the space by kind (shared/storage.js): one row per group with its size, a bar and its share;
  // a group opens to its categories by short name, at most five rows tall (the CSV has every category with the
  // server's own label). While a pass runs (also one the server started by itself) the detail is read again every
  // 2 s for its progress, and when it ends the groups are drawn again and a note says what was freed.
  let storageSection = null;
  let storage = null;
  if (admin) {
    let detail = null, readErr = null, pollErr = null, timer = null;
    ctx.onCleanup(() => clearTimeout(timer));
    const compactBtn = button('Compact now', { kind: 'primary', iconName: 'reset', cls: 'button-small', onclick: () => compactNow() });
    const csvBtn = button('Download CSV', { iconName: 'download', cls: 'button-small', onclick: () => downloadCsv('sentry-mate-storage-on-disk.csv', (detail && detail.categories) || [], [
      { label: 'What', value: (c) => c.label }, { label: 'Kind', value: (c) => groupWord(c.group) }, { label: 'Key', value: (c) => c.key },
      { label: 'Files', value: (c) => c.files }, { label: 'Size (bytes)', value: (c) => c.bytes }, { label: 'On disk (bytes)', value: (c) => c.disk_bytes }]) });
    const figsBox = h('dl', { class: 'st-figs' });
    const line = h('p', { class: 'ss-stor-line', 'aria-live': 'polite' });
    const list = h('ul', { class: 'st-groups', 'aria-label': 'Space on disk by kind' });
    const open = new Set();
    let opened = false;
    const LOSSLESS = 'Compaction packs data that is no longer written, losslessly: every value stays readable exactly, and the logs’ signed originals are kept byte for byte.';
    const fig = (label, value, title) => h('div', { class: 'st-fig', title }, h('dt', null, label), h('dd', null, value));
    const drawGroups = () => {
      const groups = storageGroups(detail && detail.categories);
      if (!opened && groups.length) { open.add(groups[0].group); opened = true; }  // the largest, open at first
      if (!groups.length) {
        replace(list, h('li', { class: 'st-none' }, `The server did not say what takes the space${readErr ? `: ${errText(readErr)}` : '.'}`));
        return;
      }
      replace(list, groups.map((g) => {
        const isOpen = open.has(g.group);
        const bar = h('span', { class: 'st-bar', 'aria-hidden': 'true' }, h('i'));
        setVars(bar, { '--w': `${Math.max(g.share > 0 ? 1 : 0, g.share * 100).toFixed(1)}%` });
        const rows = h('ul', { class: 'st-rows', tabindex: '0', 'aria-label': g.name, hidden: !isOpen },
          g.rows.map((c) => h('li', { class: 'st-row', title: c.label },
            h('span', { class: 'st-row-name' }, shortName(c)), h('span', { class: 'st-row-size' }, bytes(c.disk_bytes)))));
        const head = h('button', { class: 'st-group-head', type: 'button', 'aria-expanded': String(isOpen), onclick: () => {
          if (open.has(g.group)) open.delete(g.group); else open.add(g.group);
          drawGroups();
        } }, icon(isOpen ? 'chevronDown' : 'chevronRight'), h('span', { class: 'st-group-name' }, g.name), bar,
        h('span', { class: 'st-group-size' }, bytes(g.disk_bytes)), h('span', { class: 'st-group-share' }, shareText(g.share)));
        return h('li', { class: ['st-group', isOpen && 'is-open'] }, head, rows);
      }));
    };
    const draw = () => {
      const c = (detail && detail.compaction) || null;
      const running = !!c && c.status === 'running';
      compactBtn.disabled = running || !c;
      const num = (v) => (v == null ? q(null, 'bytes', { why: 'not known' }) : bytes(v));
      replace(figsBox,
        fig('On disk', num(detail ? detail.total_disk_bytes : null), 'Everything below, as it lies on disk.'),
        fig('Free', num(detail ? detail.free_bytes : null), 'Room left on the disk for new data.'),
        fig('Saved', num(c ? c.saved_bytes : null), 'What compaction has freed in all.'),
        fig('Last pass', !c || c.finished_ms == null ? 'Not yet' : c.last_saved_bytes ? h('span', null, bytes(c.last_saved_bytes), ' freed') : 'Nothing left to pack',
          c && c.finished_ms != null ? `Finished ${dateTime(c.finished_ms, tz)} (${zoneLabel(tz)}).` : 'No pass has finished yet.'));
      const tip = h('span', { class: 'st-tip', title: LOSSLESS, 'aria-label': LOSSLESS, role: 'img' }, icon('info'));
      line.classList.toggle('ss-warn', !!c && c.status === 'failed');
      if (running) {
        replace(line, h('span', null, h('strong', null, 'Packing…'), ` ${PACK_STEP[c.step] || c.step || 'getting ready'}, `,
          c.total ? `${format(c.done ?? 0, 'count')} of ${format(c.total, 'count')}` : 'counting',
          pollErr ? ` (the progress could not be read just now: ${errText(pollErr)})` : null), tip);
      } else if (c && c.status === 'failed') {
        replace(line, h('span', null, h('strong', null, 'The last pass failed: '), c.error || 'the server gave no reason.'), tip);
      } else {
        replace(line, h('span', null, !c ? 'The server did not say how compaction stands.'
          : c.automatic ? 'Compaction runs by itself when the server is idle.' : 'Compaction does not run by itself here.'), tip);
      }
    };
    const schedule = () => { clearTimeout(timer); if (detail && detail.compaction && detail.compaction.status === 'running') timer = setTimeout(poll, 2000); };
    async function poll() {
      try {
        const d = await ctx.api.get('system/storage/detail');
        if (!ctx.alive()) return;
        detail = d; pollErr = null;
        const c = d.compaction || {};
        if (c.status !== 'running') {
          drawGroups();
          if (c.status === 'failed') ctx.flash(`Compaction failed: ${c.error || 'the server gave no reason.'}`, { kind: 'error' });
          else ctx.flash(c.last_saved_bytes ? `Compaction is done: ${format(c.last_saved_bytes, 'bytes')} freed on disk.` : 'Compaction is done: nothing was left to pack.');
        }
      } catch (err) {
        if (!ctx.alive()) return;
        pollErr = err;
      }
      draw(); schedule();
    }
    async function compactNow() {
      compactBtn.disabled = true;
      try {
        const r = await ctx.api.post('system/storage/compact');
        if (!ctx.alive()) return;
        detail = { ...(detail || {}), compaction: r.compaction };
        pollErr = null;
      } catch (err) {
        if (!ctx.alive()) return;
        ctx.flash(`Compaction not started: ${errText(err)}`, { kind: 'error' });
      }
      draw(); schedule();
    }
    draw();
    storageSection = section({ title: 'Storage', note: 'by kind', cls: 'ss-storage', id: 'storage', actions: [csvBtn, compactBtn] },
      figsBox, line, list);
    storage = {
      async load() {
        try { detail = await ctx.api.get('system/storage/detail'); } catch (err) { if (!ctx.alive()) return; readErr = err; }
        if (!ctx.alive()) return;
        drawGroups(); draw(); schedule();
      },
    };
  }

  /* ---------------------------------------------------------------- put together */
  drawNames();
  replace(ctx.main,
    pageHead({
      eyebrow: 'Settings', title: 'Server and access', deck: ctx.info.purpose,
      aside: ctx.me ? h('p', { class: 'ss-who' }, 'Signed in as ', h('strong', null, ctx.me.name), sep(), ROLE[ctx.me.role] || ctx.me.role) : null,
    }),
    h('div', { class: 'ss-grid' },
      h('div', { class: 'ss-col' }, addressSection, accessSection),
      h('div', { class: 'ss-col' }, updatesSection, safetySection)),
    fullSection,
    // the move and Storage side by side (one above the other on a phone or a narrow window)
    mover || storageSection ? h('div', { class: 'ss-pair' }, mover ? mover.el : null, storageSection) : null,
    accountsSection, sessionsSection, auditSection);

  try {
    await Promise.all([sessionsTable.load(), accountsTable && accountsTable.load(), auditTable && auditPrep.then(() => auditTable.load()), mover && mover.load(), storage && storage.load(), full && full.ready]);
  } catch (err) {
    if (!ctx.alive()) return undefined;
    throw err;
  }
  if (!ctx.alive()) return undefined;
  if (focusAccount) document.getElementById(`account-${focusAccount}`)?.scrollIntoView({ block: 'center' });
  else if (location.hash === '#move' || location.hash === '#full-rebuild') document.getElementById(location.hash.slice(1))?.scrollIntoView({ block: 'start' });
  ctx.ready();
  return {};
}
