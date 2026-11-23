// Settings: Server and access (/settings/server): who can sign in, what each account may see, and how
// the server is reached. Every visitor gets: the check of the address in this browser against the
// server's host names and port, how access works, and their own sessions (sign out one, all others, or
// here). Admins also get the accounts (roles, car lists, two-step sign-in), the audit log (each entry
// links to the item it changed), the host names for the certificate, the ports, and where the recovery
// key and backups stand, and the move to another server (a move code or an export file:
// pages/settings-server-move.js). Changes are saved through the API with the object's version; a short note
// confirms each one.

import { h, replace } from '../shared/dom.js';
import * as state from '../shared/state.js';
import { format } from '../shared/units.js';
import { icon } from '../core/icons.js';
import { soldParts, soldToggle } from '../core/carswitch.js';
import { pageHead, panel, kv, tag, pill, metric, figures } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { field, form } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { openDialog, confirmDialog, passwordOk } from '../core/dialog.js';
import { signOut } from '../core/session.js';
import { dateTime, date, agoEl, tzName, DASH } from '../core/fmt.js';
import { timeColumn } from '../shared/csv.js';
import { AUDIT_GROUPS, actionLabel, auditItem } from '../shared/audit.js';
import { movePanel, showRecoveryKey, newRecoveryKey, markKeySaved, askPassword } from './settings-server-move.js';

export const id = 'settings-server';
export const css = true;

const enc = encodeURIComponent;
const ROLE = { admin: 'Admin', watch: 'Watch-only' };
const ROLE_TEXT = {
  admin: 'Sees every car and every setting, and can change everything: accounts, cars, places, integrations, imports.',
  watch: 'Sees only the cars on its list and can change only its own preferences and sessions.',
};
const PURPOSE = { web: 'Web pages and API', upload: 'CAN Bridge uploads' };
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);

// a row: the label and its explanation on the left, the control on the right (the app's .setting)
const setting = (titleText, text, control, cv = null) => h('div', { class: 'setting', cover: cv },
  h('div', null, h('h3', null, titleText), text ? h('p', null, text) : null), h('div', { class: 'setting-control' }, control));

// one line of a check: ok, warn or info, a headline and the explanation under it
const checkRow = (tone, head, text, action = null) => h('li', { class: ['ss-check', `ss-check--${tone}`] },
  h('span', { class: 'ss-check-ic' }, icon(tone === 'ok' ? 'check' : tone === 'warn' ? 'warn' : 'info')),
  h('div', null, h('p', { class: 'ss-check-head' }, head), h('p', { class: 'ss-check-text' }, text), action));

const mono = (t) => h('code', { class: 'mono ss-code' }, t);

// a SHA-256 in colon hex, in groups of four pairs that never break inside, so it wraps cleanly on a phone
const shaGroups = (sha) => h('code', { class: 'mono ss-value ss-sha', 'aria-label': `SHA-256 ${sha}` },
  (String(sha).match(/([0-9A-Fa-f]{2}:?){1,4}/g) || [String(sha)]).map((g) => h('span', { class: 'ss-sha-group' }, g)));
// an address that may break only after a slash or a colon (each part kept whole: a browser would otherwise also
// break after the hyphen in a host name)
const addressEl = (text) => h('code', { class: 'mono ss-value ss-address' },
  String(text).split(/(?<=[/:])/).map((part, i) => [i ? h('wbr') : null, h('span', { class: 'ss-address-part' }, part)]));

/** A Copy button for a value: the clipboard on HTTPS, else the value selected for Ctrl+C or the phone's Copy. */
function copyValue(text, valueEl, label = 'Copy') {
  const said = h('span', { class: 'ss-copied', 'aria-live': 'polite' });
  return h('span', { class: 'ss-copy' },
    h('button', { class: 'btn btn--sm', type: 'button', onclick: async () => {
      try {
        if (!globalThis.isSecureContext || !navigator.clipboard) throw new Error('no clipboard');
        await navigator.clipboard.writeText(text);
        said.textContent = 'Copied.';
      } catch {
        const sel = globalThis.getSelection?.();
        if (sel && valueEl) { sel.removeAllRanges(); const r = document.createRange(); r.selectNodeContents(valueEl); sel.addRange(r); }
        said.textContent = 'Selected: press Ctrl+C (on a phone, tap Copy) to copy it.';
      }
    } }, icon('check', 'ico--sm'), label), said);
}

/* ====================================================================== the page */

export async function mount(ctx) {
  const admin = ctx.admin;
  const me = ctx.me || {};
  const query = ctx.query || {};
  const S = { settings: null, network: null, notices: [], accounts: [], cars: [], sessions: [], trips: [] };

  async function loadAccounts() { S.accounts = admin ? await ctx.api.list('accounts', { limit: 500 }).all({ max: 5000 }) : []; }
  async function loadSessions() { S.sessions = await ctx.api.list('me/sessions', { limit: 500 }).all({ max: 5000 }); }

  try {
    const [settings, network, notices, cars] = await Promise.all([
      ctx.api.get('settings'),
      admin ? ctx.api.get('network').catch(() => null) : null,
      ctx.api.get('notices', null, { maxAgeMs: 5000 }).catch(() => null), // the frame's answer of this load
      ctx.api.list('cars', { sort: 'custom', limit: 500 }).all({ max: 5000 }).catch(() => []),
      loadAccounts(),
      loadSessions(),
      admin ? ctx.api.list('trips', { limit: 500 }).all({ max: 5000 }).then((t) => { S.trips = t; }).catch(() => {}) : null,
    ]);
    Object.assign(S, { settings, network, notices: notices?.items || [], cars });
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const carName = new Map(S.cars.map((c) => [c.id, c.name]));
  // the zone the times on this page are in, named with them: the frame clock's (the person's preferred
  // car's), else this browser's
  const clockCar = S.cars.find((c) => c.id === state.preferredCar(S.cars)) || null;
  const tz = clockCar?.time_zone || ctx.tz;
  const zone = tzName(tz);
  const zoneText = clockCar?.time_zone ? `${zone}, ${clockCar.name}'s time zone like the clock at the top` : `${zone}, this browser's time zone`;
  const isoNote = `ISO 8601 with its UTC offset, ${tz}`;

  /* ---------------------------------------------------------------- how this server is reached */
  const loc = globalThis.location;
  const here = { https: loc.protocol === 'https:', host: loc.hostname.toLowerCase(), port: Number(loc.port || (loc.protocol === 'https:' ? 443 : 80)) };
  const webIn = S.network?.inbound?.find((x) => x.purpose === 'web') || null;
  const isLocal = here.host === 'localhost' || here.host === '127.0.0.1' || here.host === '::1' || here.host === '[::1]';

  const hostingBox = h('div');
  // the names in the certificate the server runs with now (admins, from /network); saved host names
  // only go into the next certificate the server makes
  let certNames = null;
  const readCert = () => { certNames = Array.isArray(S.network?.certificate?.names) ? S.network.certificate.names.map((n) => n.toLowerCase()) : null; };
  readCert();
  // the owner's own certificate (put in data/tls): the server never makes a new one in its place
  const certOwn = () => !!S.network?.certificate?.own;
  const ownSteps = () => h('span', null, 'Put the new ', mono('cert.pem'), ' and ', mono('key.pem'), ' in ', mono('data/tls'), ' and start the container again; then enter the new SHA-256 in each CAN Bridge.');
  function paintHosting() {
    const saved = (S.settings.host_names || []).map((n) => n.toLowerCase());
    const names = certNames || saved;
    const named = names.includes(here.host);
    const savedOnly = !named && saved.includes(here.host);
    const rows = [];
    rows.push(here.https
      ? checkRow('ok', 'HTTPS', 'This page came over an encrypted connection, so the password you sign in with never travels in plain text.')
      : checkRow('warn', 'Plain HTTP', `This page came over plain HTTP. Sentry Mate itself answers only HTTPS (TLS 1.3); plain HTTP means something else serves it${isLocal ? ' (on this machine: a development server)' : ', such as a proxy'}. Never type a password on a plain HTTP page that reaches over a network.`));
    rows.push(named
      ? checkRow('ok', `Host name ${here.host}`, certNames
        ? 'This name is in the certificate the server uses now, so the browser trusts the address without a warning (once it trusts the certificate).'
        : 'This name is on the server\'s list of names for its certificate, so the browser trusts the address without a warning (once it trusts the certificate). An admin sees whether the certificate in use already has it.')
      : savedOnly
        ? (certOwn()
          ? checkRow('info', `Host name ${here.host} is saved, not yet in the certificate`,
            `The name is on the list below, but the certificate the server uses now is your own and names ${names.join(', ') || 'no host'}. Browsers keep warning about ${here.host} until your certificate names it too.`,
            admin ? ownSteps() : null)
          : checkRow('info', `Host name ${here.host} is saved, not yet in the certificate`,
            `The name is on the list below, but the certificate the server uses now names ${names.join(', ') || 'no host'}. Browsers keep warning about ${here.host} until you make a new certificate: the button is here and under Certificate, recovery key and backups.`,
            admin ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => makeCertificate() }, icon('reset', 'ico--sm'), 'Make a new certificate') : null))
        : checkRow('warn', `Host name ${here.host} is not on the certificate`,
          `You opened Sentry Mate as ${here.host}, but the certificate names ${names.join(', ') || 'no host'}. Browsers warn about a name that is not on it. The CAN Bridge is not affected: it pins the certificate's SHA-256 and does not check names.`,
          admin ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => addName(here.host) }, icon('plus', 'ico--sm'), `Add ${here.host} to the names`) : null));
    if (webIn) {
      rows.push(here.port === webIn.port
        ? checkRow('ok', `Port ${here.port}`, `The same port the server listens on for the web pages (${webIn.port}).`)
        : checkRow('info', `Port ${here.port}, server port ${webIn.port}`, `Your address uses port ${here.port}; the server listens on ${webIn.port} inside its container. A port mapping such as "${here.port}:${webIn.port}" in the compose file (or a proxy) joins the two. That is fine; it only matters when you write the address into another device.`));
    } else {
      rows.push(checkRow('info', `Port ${here.port}`, 'The port in your address. An admin sees the ports the server itself listens on.'));
    }
    rows.push(checkRow('ok', 'At the root of its address', `Sentry Mate always lives at / on its own host name or port, so links never need a path prefix. Behind a shared proxy, give it its own name (such as ${names[0] || 'sentry-mate.local'}) rather than a path.`));
    rows.push(checkRow('ok', 'Works from any address', 'There is no list of allowed origins to keep in step: the session cookie is never sent by other sites (SameSite=Strict) and every change carries a header other sites cannot set, so the page works under each of its names.'));
    replace(hostingBox, h('ul', { class: 'ss-checks' }, rows));
  }

  /* ---------------------------------------------------------------- host names for the certificate */
  const namesBox = h('div', { class: 'ss-names' });
  let draft = [...(S.settings.host_names || [])];
  const nameInput = h('input', { class: 'input ss-name-input', type: 'text', placeholder: 'sentry-mate.local', 'aria-label': 'Host name to add', autocomplete: 'off', spellcheck: 'false', disabled: !admin,
    onkeydown: (e) => { if (e.key === 'Enter') { e.preventDefault(); addDraft(); } } });
  const nameErr = h('p', { class: 'form-error', hidden: true });
  const saveNames = h('button', { class: 'btn btn--primary btn--sm', type: 'button', disabled: true, onclick: () => putNames(draft) }, 'Save host names');
  function paintNames() {
    const changed = JSON.stringify(draft) !== JSON.stringify(S.settings.host_names || []);
    saveNames.disabled = !admin || !changed || !draft.length;
    replace(namesBox,
      h('ul', { class: 'ss-name-list', 'aria-label': 'Host names for the certificate' }, draft.map((n, i) => h('li', { class: 'ss-name' },
        h('span', { class: 'mono' }, n),
        n.toLowerCase() === here.host ? tag('this address', 'live') : null,
        admin ? h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', 'aria-label': `Remove ${n}`, disabled: draft.length <= 1,
          title: draft.length <= 1 ? 'The certificate needs at least one name' : null,
          onclick: () => { draft = draft.filter((_, j) => j !== i); paintNames(); } }, icon('x', 'ico--sm')) : null))),
      admin ? h('div', { class: 'row ss-name-add' }, nameInput,
        h('button', { class: 'btn btn--sm', type: 'button', onclick: addDraft }, icon('plus', 'ico--sm'), 'Add')) : null,
      nameErr,
      admin ? h('div', { class: 'row' }, saveNames, changed ? h('button', { class: 'btn btn--ghost btn--sm', type: 'button', onclick: () => { draft = [...S.settings.host_names]; paintNames(); } }, 'Undo changes') : null,
        changed ? h('span', { class: 'setting-note' }, 'Not saved yet.') : null) : h('p', { class: 'setting-note' }, 'Only an admin can change them.'));
  }
  function addDraft() {
    const v = nameInput.value.trim().toLowerCase();
    nameErr.hidden = true;
    if (!v) return;
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/.test(v) || v.length > 253) {
      replace(nameErr, icon('warn'), 'A host name has letters, digits, hyphens and dots, such as sentry-mate.local or an address such as 192.168.1.20.');
      nameErr.hidden = false;
      return;
    }
    if (!draft.includes(v)) draft.push(v);
    if (draft.length > 10) { draft.pop(); replace(nameErr, icon('warn'), 'The certificate takes up to 10 names.'); nameErr.hidden = false; return; }
    nameInput.value = '';
    paintNames();
  }
  // the person's unsaved edits (names added, names removed) put on top of another list
  function mergeDraft(base, onto) {
    const added = draft.filter((n) => !base.includes(n));
    const removed = base.filter((n) => !draft.includes(n));
    const out = onto.filter((n) => !removed.includes(n));
    added.forEach((n) => { if (!out.includes(n)) out.push(n); });
    return out.length ? out.slice(0, 10) : [...onto];
  }
  // list: the whole list to save. fromDraft: the list is the draft (a save of the panel); otherwise the
  // draft keeps its unsaved edits on top of the saved list.
  async function putNames(list, { fromDraft = true } = {}) {
    nameErr.hidden = true;
    const before = [...(S.settings.host_names || [])];
    try {
      S.settings = await ctx.api.patch('settings', { version: S.settings.version, host_names: list });
      draft = fromDraft ? [...S.settings.host_names] : mergeDraft(before, S.settings.host_names);
      flash('Host names saved. They go into the next certificate the server makes; the one in use keeps its names until then.');
      paintNames(); paintHosting();
    } catch (e) {
      if (e.status === 409) {
        S.settings = await ctx.api.get('settings');
        draft = mergeDraft(before, S.settings.host_names);
        paintNames(); paintHosting();
        flash(fromDraft
          ? 'Someone else changed the server settings meanwhile. Your additions and removals are kept on top of their list below: check it and save again.'
          : 'Someone else changed the server settings meanwhile; this page shows theirs now. Try again.', { kind: 'info' });
        return;
      }
      replace(nameErr, icon('warn'), errText(e)); nameErr.hidden = false;
    }
  }
  // the button in the check: saves only this name on top of the saved list, never the panel's unsaved edits
  function addName(n) {
    const saved = S.settings.host_names || [];
    if (saved.includes(n)) return;
    if (saved.length >= 10) { flash('The certificate takes up to 10 names. Remove one under Host names first.', { kind: 'error' }); return; }
    putNames([...saved, n], { fromDraft: false });
  }

  /* ---------------------------------------------------------------- ports */
  const portRows = S.network?.inbound?.length
    ? S.network.inbound.map((p) => [PURPOSE[p.purpose] || p.purpose, h('span', { class: 'ss-port' },
      h('b', { class: 'mono' }, String(p.port)), p.tls ? pill('HTTPS, TLS 1.3', { tone: 'live', ic: 'lock' }) : pill('no TLS', { tone: 'warm', ic: 'warn' }))])
    : [['Your address', h('span', { class: 'mono' }, String(here.port))]];
  const upIn = S.network?.inbound?.find((x) => x.purpose === 'upload') || null;
  const webPort = webIn ? webIn.port : null;
  // the example mapping: the port in this address on the host, the web port inside the container
  const hostPort = webPort != null && here.port !== webPort ? here.port : (webPort === 443 ? 8443 : 10443);
  const portsPanel = panel({ title: 'Ports', icon: 'plug', pad: true, cover: 'docs.env.web.port' },
    kv(portRows),
    h('p', { class: 'note ss-gap' }, S.network
      ? `${upIn ? 'Both ports speak' : 'The port speaks'} only HTTPS with TLS 1.3; there is no plain HTTP port to forget about.${upIn ? ` The upload port (${upIn.port}) takes only CAN Bridge uploads with a car's upload token, turns away wrong tokens after a few tries, and takes connections only while uploads are on: with uploads off it stays closed.` : ''}`
      : 'An admin sees which ports the server listens on.'),
    h('p', { class: 'note' }, webPort != null
      ? ['To reach the web pages on another port of the host, change only the left number of the web port\'s mapping in the compose or TrueNAS file, for example ',
        mono(`${hostPort}:${webPort}`), ` (host port ${hostPort}, web port ${webPort} inside the container), and use that number in the address.${upIn ? ` The upload port maps the same way, for example ${upIn.port}:${upIn.port}; a CAN Bridge then uses the left number.` : ''} The listening address stays 0.0.0.0 inside the container; the mapping decides what is reachable.`]
      : 'To use another port on the host, change only the left number of the port mapping in the compose or TrueNAS file (host port:container port) and use that number in the address. The listening address stays 0.0.0.0 inside the container; the mapping decides what is reachable.'));

  /* ---------------------------------------------------------------- the certificate, the recovery key and backups */
  const cert = S.notices.find((n) => n.code === 'certificate_expiring');
  let keyUnsaved = !!S.notices.find((n) => n.code === 'recovery_key_unsaved');
  const keyBtn = (label, run, kind = '') => h('button', { class: ['btn', 'btn--sm', kind && `btn--${kind}`], type: 'button', onclick: async () => {
    const r = await run(ctx, { tz });
    if (!r || !ctx.alive()) return;
    keyUnsaved = r === 'unsaved';
    paintKey();
  } }, label);
  // the recovery key: no shell needed to save, show again or replace it
  function keyRow() {
    if (keyUnsaved) {
      return checkRow('warn', 'The recovery key is not marked as saved yet',
        h('span', null, 'Until you say it is saved, the server keeps a copy of it in its keys folder. ',
          admin ? 'Show it again (your password first), keep it in a password manager or on paper away from the server, then tick "I have saved it": the server deletes its copy.'
            : 'An admin can show it again here and mark it as saved.'),
        admin ? h('div', { class: 'row ss-keyacts' }, keyBtn('Show the key again', showRecoveryKey, 'primary'), keyBtn('I have saved it already', markKeySaved), keyBtn('Make a new recovery key', newRecoveryKey, 'ghost')) : null);
    }
    return checkRow('ok', 'Recovery key',
      h('span', null, 'Saved away from the server; the server keeps no copy. ',
        admin ? 'Lost it, or someone saw it? Make a new one: the old one stops working at once, and export files and backups made before still need the old one.' : 'If it is lost, an admin can make a new one here.'),
      admin ? h('div', { class: 'row ss-keyacts' }, keyBtn('Make a new recovery key', newRecoveryKey)) : null);
  }
  let keyEl = null;
  function paintKey() { const n = keyRow(); if (keyEl) keyEl.replaceWith(n); keyEl = n; }
  paintKey();
  // the certificate in use: its SHA-256, its names and the CAN Bridge's upload address, each to copy, and a
  // new one made here (no shell, no restart)
  const certBox = h('div', { class: 'ss-cert' });
  function paintCert() {
    const c = S.network?.certificate || null;
    const rows = [];
    if (c?.sha256) {
      const v = shaGroups(c.sha256);
      rows.push(h('div', { class: 'ss-fact' }, h('dt', null, 'SHA-256'), h('dd', null, v, copyValue(c.sha256, v))));
    }
    if (c?.names?.length) {
      rows.push(h('div', { class: 'ss-fact' }, h('dt', null, 'Names in it'),
        h('dd', null, h('ul', { class: 'ss-cert-names' }, c.names.map((n) => h('li', null, h('code', { class: 'mono ss-value' }, n)))))));
    }
    if (S.network?.upload_address) {
      const v = addressEl(S.network.upload_address);
      rows.push(h('div', { class: 'ss-fact' }, h('dt', null, 'CAN Bridge upload address'), h('dd', null, v, copyValue(S.network.upload_address, v))));
    }
    replace(certBox,
      rows.length ? h('dl', { class: 'ss-facts' }, rows) : h('p', { class: 'note' }, 'The server did not say which certificate it runs with.'),
      c?.sha256 ? h('p', { class: 'ss-cert-need' }, icon('info', 'ico--sm'), 'The CAN Bridge app needs this SHA-256 in its upload settings.') : null,
      certOwn()
        ? h('p', { class: 'ss-cert-own' }, 'This is your own certificate (the ', mono('cert.pem'), ' in ', mono('data/tls'), '), so the server never replaces it. To change it, put the new ', mono('cert.pem'), ' and ', mono('key.pem'), ' there and start the container again. For a certificate the server makes instead, delete ', mono('data/tls/cert.pem'), ' and start it again.')
        : h('div', { class: 'row ss-keyacts' },
          h('button', { class: 'btn btn--sm', type: 'button', onclick: () => makeCertificate() }, icon('reset', 'ico--sm'), 'Make a new certificate')));
  }
  paintCert();
  // the names the next certificate is made for: localhost and the saved host names (what the server does)
  const nextNames = () => ['localhost', ...(S.settings.host_names || []).filter((n) => n.toLowerCase() !== 'localhost')];
  async function makeCertificate() {
    const names = nextNames();
    const out = await askPassword(ctx, {
      title: 'Make a new certificate',
      intro: h('div', { class: 'ss-newcert' },
        h('p', null, 'The server makes a new certificate and uses it at once on both ports, without a restart. Before you go on:'),
        h('ul', { class: 'ss-newcert-list' },
          h('li', null, h('b', null, 'The SHA-256 changes. '), 'The CAN Bridge app\'s uploads are refused until you enter the new SHA-256 in its upload settings.'),
          h('li', null, h('b', null, 'Browsers ask again '), 'to accept the new certificate, on every device that opens these pages.'),
          h('li', null, h('b', null, 'The names in it: '), h('span', { class: 'ss-newcert-names' }, names.map((n, i) => [i ? ', ' : '', h('code', { class: 'mono' }, n)])), '. Change them under Host names for the certificate first, if one is missing.'))),
      submit: 'Make a new certificate', danger: true,
      run: (cred) => ctx.api.post('network/certificate', cred),
    });
    if (!out || !ctx.alive()) return;
    try { S.network = await ctx.api.get('network'); } catch { S.network = { ...(S.network || {}), certificate: { names: out.names, sha256: out.sha256 }, upload_address: out.upload_address }; }
    if (!ctx.alive()) return;
    readCert(); paintCert(); paintHosting();
    const v = shaGroups(out.sha256);
    await openDialog({
      title: 'The new certificate is in use', wide: true,
      body: h('div', { class: 'ss-newcert' },
        h('p', null, 'Enter this SHA-256 in each CAN Bridge app\'s upload settings now: until then it refuses this server.'),
        h('div', { class: 'ss-fact ss-fact--big' }, h('p', { class: 'ss-fact-label' }, 'New SHA-256'), v, copyValue(out.sha256, v)),
        h('p', { class: 'note' }, 'Reload this page to accept the new certificate in this browser.')),
      actions: [{ label: 'Done', value: true, kind: 'primary', focus: true }],
    });
  }
  const safety = panel({ title: 'Certificate, recovery key and backups', icon: 'lock', pad: true },
    h('ul', { class: 'ss-checks' },
      cert
        ? checkRow('warn', `The certificate expires ${cert.detail?.expires_ms ? date(cert.detail.expires_ms, tz) : 'soon'}${cert.detail?.days != null ? ` (in ${format(cert.detail.days, 'count')} days)` : ''}`,
          certOwn()
            ? h('span', null, 'After that browsers and the CAN Bridges refuse the connection. It is your own certificate, so the server cannot make a new one: renew it where you got it, then put the new ', mono('cert.pem'), ' and ', mono('key.pem'), ' in ', mono('data/tls'), ' and start the container again. A new certificate has a new SHA-256: enter it again in each CAN Bridge.')
            : h('span', null, 'Make a new one before then with Make a new certificate. A new certificate has a new SHA-256: enter it again in each CAN Bridge.'), certBox)
        : checkRow('ok', 'Certificate', 'The server reports no problem with its certificate. It keeps the same SHA-256 across restarts, updates and moves, until you make a new one.', certBox),
      keyEl,
      checkRow('ok', 'Everything is encrypted at rest',
        h('span', null, 'Logs, the database, tokens and the certificate\'s key are sealed. A backup is a copy of the data folder: it opens only with the key file (kept apart in ', mono('/keys'), ') or the recovery key, so keep those out of the same backup.'))));

  /* ---------------------------------------------------------------- how access works */
  const nAdmins = S.accounts.filter((a) => a.role === 'admin').length;
  const accessPanel = panel({ title: 'How access works', icon: 'user', pad: true, cover: 'web.shell.access-model' },
    h('p', { class: 'ss-you' }, 'You are signed in as ', h('b', null, me.name || DASH), ' ', pill(ROLE[me.role] || me.role || DASH, { tone: me.role === 'admin' ? 'warm' : 'plain' }),
      ' ', me.cars === 'all' ? 'and see every car.' : `and see ${format(Array.isArray(me.cars) ? me.cars.length : 0, 'count')} ${Array.isArray(me.cars) && me.cars.length === 1 ? 'car' : 'cars'}.`),
    admin ? figures([
      metric('Accounts', S.accounts.length, 'count', { explain: 'Everyone who can sign in.', size: 'sm' }),
      metric('Admins', nAdmins, 'count', { explain: 'At least one is always kept.', size: 'sm' }),
      metric('Watch-only', S.accounts.length - nAdmins, 'count', { explain: 'See their cars, change nothing shared.', size: 'sm' }),
      metric('Two-step required', S.accounts.filter((a) => a.two_step_required).length, 'count', { explain: 'A code after the password.', size: 'sm' }),
    ], 'ss-figs') : null,
    h('ul', { class: 'ss-rules' },
      h('li', null, h('b', null, 'Nothing without signing in.'), ' Every page and every API call needs a session; there is no open view and no open API.'),
      h('li', null, h('b', null, 'Two roles.'), ` Admin: ${ROLE_TEXT.admin} Watch-only: ${ROLE_TEXT.watch}`),
      h('li', null, h('b', null, 'A car list per account.'), ' A car that is not on an account\'s list looks exactly like a car that does not exist: it is in no list, total, map or export.'),
      h('li', null, h('b', null, 'Two-step sign-in.'), ' An account can be made to give a six-digit code from an authenticator app after its password.'),
      h('li', null, h('b', null, 'Guessing is slowed down.'), ' Five wrong passwords for one name within a minute, or 30 from one address, and sign-in waits a minute.'),
      h('li', null, h('b', null, 'Sessions you can see.'), ' A session lasts 30 days in its browser. Below you can end one, or every other one.'),
      h('li', null, h('b', null, 'Cars never sign in.'), ' A CAN Bridge uploads with its car\'s own upload token on the upload port, made under ', h('a', { class: 'link', href: state.buildUrl('settings-cars') }, 'Settings: Cars'), '.'),
      h('li', null, h('b', null, 'Every change is recorded.'), admin ? ' The audit log below says who changed what, and when.' : ' Admins can read who changed what, and when.')));

  /* ---------------------------------------------------------------- accounts (admin) */
  const carsText = (cars) => {
    if (cars === 'all') return 'All cars';
    if (!Array.isArray(cars) || !cars.length) return 'No cars';
    const names = cars.map((c) => carName.get(c) || c);
    return names.length > 3 ? `${names.slice(0, 3).join(', ')} and ${names.length - 3} more` : names.join(', ');
  };
  const focusAccount = query.account || null;
  const accountsTable = admin ? dataTable(ctx, {
    title: 'Accounts', icon: 'user', rows: S.accounts, label: 'Accounts that can sign in', csvName: 'accounts',
    note: 'Everyone who can sign in, with what they may see. Edit an account to change its role, its cars, its password or two-step sign-in.',
    side: h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: () => editAccount(null) }, icon('plus', 'ico--sm'), 'Add account'),
    sort: { key: 'name', dir: 'asc' },
    columns: [
      { key: 'name', label: 'Name', sort: true, render: (a) => h('span', { class: 'row ss-acc' }, h('b', null, a.name),
        a.id === me.id ? tag('you', 'live') : null, a.id === focusAccount ? tag('from the audit log', 'warm') : null) },
      { key: 'role', label: 'Role', sort: true, render: (a) => pill(ROLE[a.role] || a.role, { tone: a.role === 'admin' ? 'warm' : 'plain' }), csv: (a) => ROLE[a.role] || a.role },
      { key: 'cars', label: 'Cars it sees', value: (a) => (a.cars === 'all' ? -1 : a.cars.length), sort: true, render: (a) => carsText(a.cars), csv: (a) => (a.cars === 'all' ? 'all' : a.cars.map((c) => carName.get(c) || c).join('; ')) },
      { key: 'two_step_required', label: 'Two-step sign-in', sort: true, render: (a) => (a.two_step_required ? h('span', { class: 'ok' }, 'Required') : h('span', { class: 'faint' }, 'Password only')), csv: (a) => (a.two_step_required ? 'required' : 'no') },
      { key: 'created_ms', label: `Made (${zone})`, sort: true, render: (a) => date(a.created_ms, tz), csv: timeColumn(`Made (${isoNote})`, 'created_ms', tz) },
      { key: 'act', label: 'Change', csv: false, render: (a) => h('div', { class: 'row ss-acts' },
        h('button', { class: 'btn btn--sm', type: 'button', onclick: () => editAccount(a) }, 'Edit'),
        h('button', { class: 'btn btn--sm btn--ghost', type: 'button', disabled: a.id === me.id,
          title: a.id === me.id ? 'You cannot remove the account you are signed in with' : null, onclick: () => removeAccount(a) }, 'Remove')) },
    ],
    empty: () => emptyState({ icon: 'user', title: 'No accounts', text: 'There is always at least one admin; this list is empty only while it loads.' }),
  }) : null;
  const goneNote = admin && focusAccount && !S.accounts.some((a) => a.id === focusAccount)
    ? h('p', { class: 'note ss-gone' }, icon('info', 'ico--sm'), ' The account from the audit log entry has been removed since.') : null;

  async function refreshAccounts() {
    await loadAccounts();
    if (!ctx.alive()) return;
    accountsTable.setRows(S.accounts);
  }

  function carPicker(current) {
    const all = field({ label: 'All cars, also cars that appear later', name: 'all_cars', type: 'checkbox', value: current === 'all' });
    const chosen = new Set(current === 'all' ? [] : current || []);
    const first = [...chosen];
    let soldOpen = false;
    const search = h('input', { class: 'input', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car' });
    const list = h('div', { class: 'ss-carlist', role: 'group', 'aria-label': 'Cars this account sees' });
    const count = h('p', { class: 'form-hint' });
    const paint = () => {
      const q = search.value.trim().toLowerCase();
      const shown = S.cars.filter((c) => !q || c.name.toLowerCase().includes(q) || (c.model || '').toLowerCase().includes(q));
      // sold cars wait behind "Sold cars (n)" unless this account sees them already or they are searched for
      const { main, sold } = soldParts(shown, [...first, ...chosen], { search: !!q });
      const row = (c) => h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: chosen.has(c.id), onchange: (e) => { if (e.target.checked) chosen.add(c.id); else chosen.delete(c.id); paintCount(); } }),
        h('span', null, c.name, c.model ? h('span', { class: 'faint' }, ` · ${c.model}`) : null), state.isSold(c) ? h('span', { class: 'tag tag--plain' }, 'Sold') : null);
      replace(list, shown.length ? [main.map(row), soldToggle(sold.length, soldOpen, () => { soldOpen = !soldOpen; paint(); }), soldOpen ? sold.map(row) : null] : h('p', { class: 'faint' }, S.cars.length ? 'No car matches.' : 'No cars yet.'));
    };
    const paintCount = () => { count.textContent = all.value() ? 'Sees every car, now and later.' : `${format(chosen.size, 'count')} of ${format(S.cars.length, 'count')} cars chosen.`; };
    const box = h('div', { class: 'ss-carpick' }, search, list);
    const toggle = () => { box.hidden = all.value(); paintCount(); };
    all.input.addEventListener('change', toggle);
    search.addEventListener('input', paint);
    paint(); toggle();
    const err = h('p', { class: 'form-error', hidden: true });
    return {
      el: h('div', { class: 'form-row' }, h('span', { class: 'form-label' }, 'Cars it sees'), all.el, box, count, err),
      value: () => (all.value() ? 'all' : [...chosen]),
      setError: (t) => { err.hidden = !t; if (t) replace(err, icon('warn'), t); else err.replaceChildren(); },
    };
  }

  function editAccount(a) {
    const isNew = !a;
    const name = field({ label: 'Name', name: 'name', value: a?.name || '', required: true, hint: 'What they type to sign in. 1 to 64 characters.', attrs: { maxlength: 64, autocomplete: 'off' } });
    const password = field({ label: isNew ? 'Password' : 'New password', name: 'password', type: 'password', required: isNew,
      hint: isNew ? 'Tell it to them in person or over a channel you trust.' : 'Leave empty to keep the current password.',
      attrs: { autocomplete: 'new-password' } });
    const role = field({ label: 'Role', name: 'role', type: 'select', value: a?.role || 'watch',
      options: [['watch', `Watch-only: ${ROLE_TEXT.watch}`], ['admin', `Admin: ${ROLE_TEXT.admin}`]] });
    const two = field({ label: 'Require two-step sign-in', name: 'two_step_required', type: 'checkbox', value: !!a?.two_step_required,
      hint: 'After the password, a six-digit code from an authenticator app. Recommended for every admin.' });
    const cars = carPicker(a ? a.cars : []);
    const carsField = { name: 'cars', path: 'cars', input: { checkValidity: () => true, getAttribute: () => null, focus() {} }, setError: cars.setError, value: cars.value, el: cars.el };
    const f = form({
      fields: [name, password, role, carsField, two], submit: isNew ? 'Add account' : 'Save changes',
      secondary: h('button', { class: 'btn btn--ghost', type: 'button', onclick: (e) => e.target.closest('dialog')?.close() }, 'Cancel'),
      async onSubmit(v) {
        const body = { name: v.name.trim(), role: v.role, cars: v.cars, two_step_required: v.two_step_required };
        if (v.password) {
          if (!(await passwordOk(body.name, v.password, password.input))) return;
          body.password = v.password;
        }
        try {
          if (isNew) {
            await ctx.api.post('accounts', body);
            flash(`${body.name} can sign in now, as ${ROLE[body.role].toLowerCase()}.`);
          } else {
            await ctx.api.patch(`accounts/${enc(a.id)}`, { version: a.version, ...body });
            flash(`${body.name} saved.`);
          }
        } catch (e) {
          if (e.status === 409 && e.code === 'stale_version') {
            await refreshAccounts();
            f.setMessage('Someone else changed this account meanwhile. Close this, look at their change, and try again.');
            return;
          }
          if (e.code === 'name_taken') { name.setError('That name is taken.'); return; }
          if (e.code === 'last_admin') { role.setError('This is the last admin. Make another account an admin first.'); return; }
          throw e;
        }
        f.el.closest('dialog')?.close();
        await refreshAccounts();
      },
    });
    openDialog({ title: isNew ? 'Add an account' : `Edit ${a.name}`, body: f.el, actions: [], wide: true });
    name.input.focus();
  }

  async function removeAccount(a) {
    const ok = await confirmDialog(`${a.name} can no longer sign in, and every session of theirs ends now. Their changes stay in the audit log.`, { title: `Remove ${a.name}?`, ok: 'Remove account', danger: true });
    if (!ok) return;
    try {
      await ctx.api.del(`accounts/${enc(a.id)}`);
      flash(`${a.name} was removed.`);
      await refreshAccounts();
    } catch (e) {
      flash(e.code === 'last_admin' ? 'This is the last admin. Make another account an admin first.' : `Could not remove ${a.name}: ${errText(e)}`, { kind: 'error' });
    }
  }

  /* ---------------------------------------------------------------- your sessions */
  const focusSession = query.session || null;
  const sessionsTable = dataTable(ctx, {
    title: 'Your sessions', icon: 'monitor', rows: S.sessions, label: 'Where you are signed in', csvName: 'sessions',
    note: admin
      ? `Every browser where your account is signed in; times are in ${zoneText}. End one you do not recognise, then give your account a new password: Edit your account under Accounts above.`
      : `Every browser where your account is signed in; times are in ${zoneText}. End one you do not recognise, then ask an admin to set a new password for your account.`,
    side: h('button', { class: 'btn btn--sm', type: 'button', onclick: () => endOthers() }, icon('signout', 'ico--sm'), 'Sign out everywhere else'),
    sort: { key: 'last_seen_ms', dir: 'desc' },
    columns: [
      { key: 'device', label: 'Browser', sort: true, render: (s) => h('span', { class: 'row ss-acc' }, s.device || 'Not reported',
        s.this_one ? tag('this browser', 'live') : null, s.id === focusSession ? tag('from the audit log', 'warm') : null) },
      { key: 'created_ms', label: `Signed in (${zone})`, sort: true, render: (s) => dateTime(s.created_ms, tz), csv: timeColumn(`Signed in (${isoNote})`, 'created_ms', tz) },
      { key: 'last_seen_ms', label: 'Last used', sort: true, render: (s) => (s.this_one ? 'Now' : agoEl(s.last_seen_ms, tz)), csv: timeColumn(`Last used (${isoNote})`, 'last_seen_ms', tz) },
      { key: 'act', label: 'End it', csv: false, render: (s) => h('button', { class: 'btn btn--sm', type: 'button', onclick: () => endSession(s) },
        icon('signout', 'ico--sm'), s.this_one ? 'Sign out here' : 'Sign out') },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No sessions', text: 'Only this browser is signed in.' }),
  });
  const sessionGone = focusSession && !S.sessions.some((s) => s.id === focusSession)
    ? h('p', { class: 'note ss-gone' }, icon('info', 'ico--sm'), ' The session from the audit log entry has ended since.') : null;

  async function refreshSessions() { await loadSessions(); if (ctx.alive()) sessionsTable.setRows(S.sessions); }
  async function endSession(s) {
    if (s.this_one) {
      try { await signOut(); flash('Signed out.', { next: true }); state.navigate('/sign-in', { replace: true }); } catch (e) { flash(`Could not sign out: ${errText(e)}`, { kind: 'error' }); }
      return;
    }
    try { await ctx.api.del(`me/sessions/${enc(s.id)}`); flash(`Signed out ${s.device || 'that browser'}.`); await refreshSessions(); } catch (e) { flash(`Could not end the session: ${errText(e)}`, { kind: 'error' }); }
  }
  async function endOthers() {
    const others = S.sessions.filter((s) => !s.this_one).length;
    if (!others) { flash('No other browser is signed in.', { kind: 'info' }); return; }
    const ok = await confirmDialog(`${format(others, 'count')} other ${others === 1 ? 'browser is' : 'browsers are'} signed out now. This browser stays signed in.`, { title: 'Sign out everywhere else?', ok: 'Sign out the others' });
    if (!ok) return;
    try { const r = await ctx.api.del('me/sessions'); flash(`Signed out ${format(r?.ended ?? others, 'count')} other ${(r?.ended ?? others) === 1 ? 'session' : 'sessions'}.`); await refreshSessions(); } catch (e) { flash(`Could not sign out the others: ${errText(e)}`, { kind: 'error' }); }
  }

  /* ---------------------------------------------------------------- audit log (admin) */
  const accountName = (aid) => S.accounts.find((a) => a.id === aid)?.name;
  // the changed item in words (shared/audit.js), a link where it still exists
  const itemCtx = {
    carName: (cid) => carName.get(cid),
    accountName,
    trip: (tid) => S.trips.find((x) => x.id === tid),
  };
  function itemEl(e) {
    const it = auditItem(e, itemCtx);
    if (!it) return h('span', { class: 'faint' }, DASH);
    let href = null;
    if (it.route) { try { href = state.buildUrl(it.route[0], it.route[1]) + (it.hash || ''); } catch { href = null; } }
    return href ? h('a', { class: 'link', href }, it.text) : h('span', { class: it.faint ? 'faint' : null }, it.text);
  }
  // the filters: What (a group) and Who (an account, or the server itself); each change loads the first page again
  const SERVER_WHO = '*server';
  const whatPick = h('select', { class: 'select ss-audit-pick', 'aria-label': 'What' },
    h('option', { value: '' }, 'Everything'), AUDIT_GROUPS.map((g) => h('option', { value: g.id }, g.label)));
  const whoPick = h('select', { class: 'select ss-audit-pick', 'aria-label': 'Who' },
    h('option', { value: '' }, 'Everyone'),
    [...S.accounts].sort((x, y) => String(x.name).localeCompare(String(y.name))).map((a) => h('option', { value: a.name }, a.name)),
    h('option', { value: SERVER_WHO }, 'The server'));
  const auditFilters = h('span', { class: 'ss-audit-filters' },
    h('label', { class: 'ss-audit-filter' }, h('span', { class: 'faint' }, 'What'), whatPick),
    h('label', { class: 'ss-audit-filter' }, h('span', { class: 'faint' }, 'Who'), whoPick));
  const filtered = () => !!(whatPick.value || whoPick.value);
  const refilter = () => auditTable?.setParams({ group: whatPick.value || null, who: whoPick.value || null });
  ctx.on(whatPick, 'change', refilter);
  ctx.on(whoPick, 'change', refilter);
  const auditTable = admin ? dataTable(ctx, {
    title: 'Audit log', icon: 'book', path: 'audit', label: 'Who changed what', csvName: 'audit-log',
    limit: 10, autoMore: true, moreLabel: 'Load more', cls: 'ss-audit', side: auditFilters,
    note: `Every change and every sign-in, newest first; more load as you scroll. The item links to what was changed, where it still exists. Times are in ${zoneText}.`,
    columns: [
      { key: 'at_ms', label: `When (${zone})`, cls: 'ss-audit-when', render: (e) => dateTime(e.at_ms, tz) },
      { key: 'account', label: 'Who', render: (e) => (e.account ? h('b', null, e.account) : h('span', { class: 'faint' }, 'The server')) },
      { key: 'action', label: 'Action', render: (e) => h('span', { class: 'ss-action-label', title: e.action }, actionLabel(e.action)) },
      { key: 'target', label: 'Item', render: itemEl },
      { key: 'summary', label: 'What happened', cls: 'ss-audit-summary', render: (e) => e.summary || DASH },
    ],
    empty: () => (filtered()
      ? emptyState({ icon: 'book', title: 'Nothing matches', text: 'No entry fits this What and Who. Choose Everything or Everyone to see more.' })
      : emptyState({ icon: 'book', title: 'Nothing recorded yet', text: 'Each sign-in and each change appears here as it happens.' })),
  }) : null;

  /* ---------------------------------------------------------------- moving to another server (admin) */
  const move = admin ? movePanel(ctx, { tz, zone: zoneText }) : null;

  /* ---------------------------------------------------------------- the page */
  paintHosting();
  paintNames();
  const hostPanel = panel({ title: 'How this server is reached', icon: 'globe', pad: true, cover: 'web.shell.hosting' },
    h('p', { class: 'ss-addr' }, 'This page: ', h('span', { class: 'mono' }, `${loc.protocol}//${loc.host}/`)),
    hostingBox);
  const namesPanel = panel({ title: 'Host names for the certificate', icon: 'server', pad: true, cover: 'docs.env.web.virtual-host' },
    setting('Names on the certificate', 'The names (or addresses) people and devices use to reach this server. The server puts them in the certificate it makes, so browsers accept those names. They are used when a certificate is made: to put a change in the running certificate, make a new one (see below).', namesBox));
  if (certOwn()) {
    namesPanel.append(h('p', { class: 'note ss-gap' }, 'The server runs with your own certificate, so these names go only into a certificate it makes, not into yours. To have browsers accept another name, get a certificate that names it, put its ', mono('cert.pem'), ' and ', mono('key.pem'), ' in ', mono('data/tls'), ' and start the container again.'));
  }

  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'settings-stack ss' },
      h('div', { class: 'ss-two' }, hostPanel, h('div', { class: 'ss-col' }, namesPanel, portsPanel)),
      h('div', { class: 'ss-two' }, accessPanel, admin ? safety : null),
      move ? move.el : null,
      accountsTable ? [goneNote, accountsTable.el] : null,
      sessionGone, sessionsTable.el,
      auditTable ? auditTable.el : null));
  if (auditTable) await auditTable.ready;
  if (move) await move.ready;
  if (!ctx.alive()) return;
  ctx.ready();
}
