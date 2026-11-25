// Settings: Server and access (/settings/server): who can sign in, what each account may see, and how
// the server is reached. Every visitor gets: Server updates (this build and the release check's last answer:
// GET /about, GET /release; Check now and the release check's switch for an admin, POST /release/check, PATCH /settings release_check), the check of the address in this browser against the
// server's host names and port, how access works, and their own sessions (sign out one, all others, or
// here). Admins also get the accounts (roles, car lists, two-step sign-in), the audit log (each entry
// links to the item it changed), where the certificate (its names: Change names), the recovery key and
// backups stand, the move to another server (a move code or an export file:
// pages/settings-server-move.js) with Storage beside it: the space on disk by kind and what compaction saved
// (GET /system/storage/detail), with "Compact now" (POST /system/storage/compact; a pass's progress follows
// `job` events of kind compaction and `storage` events: live events, polling only while the live stream is
// down), and the full rebuild of every car with its automatic run on a new server
// version (pages/full-rebuild.js). Changes are saved through the API with the object's version; a short note
// confirms each one.

import { h, replace, setVars } from '../shared/dom.js';
import { compactionProgress, laterCompaction } from '../shared/compaction.js';
import * as state from '../shared/state.js';
import { format } from '../shared/units.js';
import { icon } from '../core/icons.js';
import { soldParts, soldToggle } from '../core/carswitch.js';
import { pageHead, panel, kv, tag, pill, metric, figures, csvButton } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { field, form, once } from '../core/form.js';
import { codeAttrs, codeValue, CODE_HINT } from '../core/code.js';
import { cancelBtn as twoCancelBtn, ownPassword as twoOwnPassword, ownCode, ownWrong, recoveryDialog, confirmTwoStep } from './two-step-turn-on.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { openDialog, confirmDialog, passwordOk } from '../core/dialog.js';
import { signOut } from '../core/session.js';
import { dateTime, date, agoEl, tzName, DASH } from '../core/fmt.js';
import { timeColumn, downloadCsv, csvFilename } from '../shared/csv.js';
import { storageGroups, shortName, shareText, groupWord } from '../shared/storage.js';
import { AUDIT_GROUPS, actionLabel, auditItem } from '../shared/audit.js';
import { serverUpdates, RELEASE_OFF_TEXT } from '../shared/release.js';
import { cover } from '../shared/dom.js';
import { movePanel, showRecoveryKey, newRecoveryKey, markKeySaved, askPassword } from './settings-server-move.js';
import { fullRebuild } from './full-rebuild.js';
import { follow, jobOf } from '../shared/live.js';
import { morphChildren } from '../core/morph.js';

export const id = 'settings-server';
export const css = true;

const enc = encodeURIComponent;
const ROLE = { admin: 'Admin', watch: 'Watch-only' };
const ROLE_TEXT = {
  admin: 'Sees every car and every setting, and can change everything: accounts, cars, places, integrations, imports.',
  watch: 'Sees only the cars on its list and can change only its own preferences and sessions.',
};
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);
const bytes = (v) => format(v, 'bytes');

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
  const S = { twoStep: null, policy: null, settings: null, network: null, notices: [], accounts: [], cars: [], sessions: [], trips: [], storage: null, storageErr: null, about: null, release: null };

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
      ctx.api.get('about', null, { maxAgeMs: 600000 }).then((a) => { S.about = a; }).catch(() => {}),
      ctx.api.get('release').then((r) => { S.release = r; }).catch(() => {}),
      ctx.api.get('me/two-step').then((t) => { S.twoStep = t; }).catch(() => {}),
      admin ? ctx.api.get('two-step-policy').then((p) => { S.policy = p; }).catch(() => {}) : null,
    ]);
    Object.assign(S, { settings, network, notices: notices?.items || [], cars });
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const carName = new Map(S.cars.map((c) => [c.id, c.name]));
  // the zone the times on this page are in, named with them: the frame clock's (the preferred car's:
  // Main car, else the last one opened), else this browser's
  const clockId = state.preferredCar(S.cars);
  const clockCar = S.cars.find((c) => c.id === clockId) || null;
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
            `The name is on the server's list of names, but the certificate the server uses now is your own and names ${names.join(', ') || 'no host'}. Browsers keep warning about ${here.host} until your certificate names it too.`,
            admin ? ownSteps() : null)
          : checkRow('info', `Host name ${here.host} is saved, not yet in the certificate`,
            `The name is on the server's list of names, but the certificate the server uses now names ${names.join(', ') || 'no host'}. Browsers keep warning about ${here.host} until you make a new certificate: the button is here and under Certificate, recovery key and backups.`,
            admin ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => makeCertificate() }, icon('reset', 'ico--sm'), 'Make a new certificate') : null))
        : checkRow('warn', `Host name ${here.host} is not on the certificate`,
          `You opened Sentry Mate as ${here.host}, but the certificate names ${names.join(', ') || 'no host'}. Browsers warn about a name that is not on it. The CAN Bridge is not affected: it pins the certificate's SHA-256 and does not check names.`,
          admin ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => addName(here.host) }, icon('plus', 'ico--sm'), `Add ${here.host} to the names`) : null));
    const portRow = (el) => cover(el, 'docs.env.web.port');
    if (webIn) {
      rows.push(portRow(here.port === webIn.port
        ? checkRow('ok', `Port ${here.port}`, `The same port the server listens on for the web pages (${webIn.port}).`)
        : checkRow('info', `Port ${here.port}, server port ${webIn.port}`, `Your address uses port ${here.port}; the server listens on ${webIn.port} inside its container. A port mapping such as "${here.port}:${webIn.port}" in the compose file (or a proxy) joins the two. That is fine; it only matters when you write the address into another device.`)));
    } else {
      rows.push(portRow(checkRow('info', `Port ${here.port}`, 'The port in your address. An admin sees the ports the server itself listens on.')));
    }
    rows.push(checkRow('ok', 'At the root of its address', `Sentry Mate always lives at / on its own host name or port, so links never need a path prefix. Behind a shared proxy, give it its own name (such as ${names[0] || 'sentry-mate.local'}) rather than a path.`));
    rows.push(checkRow('ok', 'Works from any address', 'There is no list of allowed origins to keep in step: the session cookie is never sent by other sites (SameSite=Strict) and every change carries a header other sites cannot set, so the page works under each of its names.'));
    replace(hostingBox, h('ul', { class: 'ss-checks' }, rows));
  }

  /* ---------------------------------------------------------------- host names for the certificate (Change names, in a dialog) */
  const namesBox = h('div', { class: 'ss-names' });
  let draft = [...(S.settings.host_names || [])];
  const nameInput = h('input', { class: 'input ss-name-input', type: 'text', placeholder: 'sentry-mate.local', 'aria-label': 'Host name to add', autocomplete: 'off', spellcheck: 'false', disabled: !admin,
    onkeydown: (e) => { if (e.key === 'Enter') { e.preventDefault(); addDraft(); } } });
  const nameErr = h('p', { class: 'form-error', hidden: true });
  const saveNames = h('button', { class: 'btn btn--primary btn--sm', type: 'button', disabled: true, onclick: once(() => putNames(draft)) }, 'Save host names');
  function paintNames() {
    const changed = JSON.stringify(draft) !== JSON.stringify(S.settings.host_names || []);
    saveNames.disabled = !admin || !changed || !draft.length;
    replace(namesBox,
      h('ul', { class: 'ss-name-list', 'aria-label': 'Names for the certificate' }, draft.map((n, i) => h('li', { class: 'ss-name' },
        h('span', { class: 'mono' }, n),
        n.toLowerCase() === here.host ? tag('this address', 'live') : null,
        admin ? h('button', { class: 'btn btn--ghost btn--icon btn--sm', type: 'button', 'aria-label': `Remove ${n}`, disabled: draft.length <= 1,
          title: draft.length <= 1 ? 'The certificate needs at least one name' : null,
          onclick: () => { if (savingNames) return; draft = draft.filter((_, j) => j !== i); paintNames(); } }, icon('x', 'ico--sm')) : null))),
      admin ? h('div', { class: 'row ss-name-add' }, nameInput,
        h('button', { class: 'btn btn--sm', type: 'button', onclick: () => addDraft() }, icon('plus', 'ico--sm'), 'Add')) : null,
      nameErr,
      admin ? h('div', { class: 'row' }, saveNames, changed ? h('button', { class: 'btn btn--ghost btn--sm', type: 'button', onclick: () => { if (savingNames) return; draft = [...S.settings.host_names]; paintNames(); } }, 'Undo changes') : null,
        changed ? h('span', { class: 'setting-note' }, 'Not saved yet.') : null) : h('p', { class: 'setting-note' }, 'Only an admin can change them.'));
  }
  function addDraft() {
    if (savingNames) return;
    const v = nameInput.value.trim().toLowerCase();
    nameErr.hidden = true;
    if (!v) return;
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/.test(v) || v.length > 253) {
      replace(nameErr, icon('warn'), 'A host name has letters, digits, hyphens and dots, such as sentry-mate.local or an address such as 192.0.2.20.');
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
  // one save at a time: a second press (or Add, Undo, a removal) while one runs would send a stale version (409)
  let savingNames = false;
  async function putNames(list, { fromDraft = true } = {}) {
    if (savingNames) return;
    savingNames = true;
    try { await putNamesNow(list, { fromDraft }); } finally { savingNames = false; }
  }
  async function putNamesNow(list, { fromDraft }) {
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
    if (saved.length >= 10) { flash('The certificate takes up to 10 names. Remove one with Change names under Certificate first.', { kind: 'error' }); return; }
    putNames([...saved, n], { fromDraft: false });
  }

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
      rows.push(h('div', { class: 'ss-fact', cover: 'docs.env.web.virtual-host' }, h('dt', null, 'Names in it'),
        h('dd', null, h('ul', { class: 'ss-cert-names' }, c.names.map((n) => h('li', null, h('code', { class: 'mono ss-value' }, n)))),
          admin ? h('button', { class: 'btn btn--ghost btn--sm ss-names-btn', type: 'button', onclick: () => changeNames() }, icon('settings', 'ico--sm'), 'Change names') : null)));
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
          h('li', null, h('b', null, 'The names in it: '), h('span', { class: 'ss-newcert-names' }, names.map((n, i) => [i ? ', ' : '', h('code', { class: 'mono' }, n)])), '. Change them first with Change names, if one is missing.'))),
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
      metric('Two-step on', S.accounts.filter((a) => a.two_step).length, 'count', { explain: 'Accounts that give a code after the password.', size: 'sm' }),
    ], 'ss-figs') : null,
    h('ul', { class: 'ss-rules' },
      h('li', null, h('b', null, 'Nothing without signing in.'), ' Every page and every API call needs a session; there is no open view and no open API.'),
      h('li', null, h('b', null, 'Two roles.'), ` Admin: ${ROLE_TEXT.admin} Watch-only: ${ROLE_TEXT.watch}`),
      h('li', null, h('b', null, 'A car list per account.'), ' A car that is not on an account\'s list looks exactly like a car that does not exist: it is in no list, total, map or export.'),
      h('li', null, h('b', null, 'Two-step sign-in.'), ' Each person turns it on for their own account: after the password, a six-digit code from an authenticator app. An admin can require it for every account, or from outside the home network.'),
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
    note: 'Everyone who can sign in, with what they may see. Edit an account to change its role, its cars or its password. Two-step sign-in is turned on by each person for their own account (below, under Two-step sign-in); an admin can require it for everyone or from outside the home network.',
    side: h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: () => editAccount(null) }, icon('plus', 'ico--sm'), 'Add account'),
    sort: { key: 'name', dir: 'asc' },
    columns: [
      { key: 'name', label: 'Name', sort: true, render: (a) => h('span', { class: 'row ss-acc' }, h('b', null, a.name),
        a.id === me.id ? tag('you', 'live') : null, a.id === focusAccount ? tag('from the audit log', 'warm') : null) },
      { key: 'role', label: 'Role', sort: true, render: (a) => pill(ROLE[a.role] || a.role, { tone: a.role === 'admin' ? 'warm' : 'plain' }), csv: (a) => ROLE[a.role] || a.role },
      { key: 'cars', label: 'Cars it sees', value: (a) => (a.cars === 'all' ? -1 : a.cars.length), sort: true, render: (a) => carsText(a.cars), csv: (a) => (a.cars === 'all' ? 'all' : a.cars.map((c) => carName.get(c) || c).join('; ')) },
      { key: 'two_step', label: 'Two-step sign-in', sort: true, render: (a) => (a.two_step ? h('span', { class: 'ok' }, 'On') : h('span', { class: 'faint' }, 'Password only')), csv: (a) => (a.two_step ? 'on' : 'no') },
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
    // the server asks for the acting admin's own password again before every account change (and the code, with
    // two-step sign-in on): sent as confirm_password and code
    const own = field({ label: 'Your password', name: 'confirm_password', type: 'password', required: true,
      hint: `The password you sign in with as ${me.name || 'this account'}: an account change asks for it again.`, attrs: { autocomplete: 'current-password' } });
    const code = field({ label: 'Code from your authenticator app', name: 'code', hint: CODE_HINT,
      attrs: codeAttrs() });
    code.el.hidden = !(me.two_step || me.two_step_required);
    const cars = carPicker(a ? a.cars : []);
    const carsField = { name: 'cars', path: 'cars', input: { checkValidity: () => true, getAttribute: () => null, focus() {} }, setError: cars.setError, value: cars.value, el: cars.el };
    const f = form({
      fields: [name, password, role, carsField, own, code], submit: isNew ? 'Add account' : 'Save changes',
      secondary: h('button', { class: 'btn btn--ghost', type: 'button', onclick: (e) => e.target.closest('dialog')?.close() }, 'Cancel'),
      async onSubmit(v) {
        const body = { name: v.name.trim(), role: v.role, cars: v.cars };
        if (v.password) {
          if (!(await passwordOk(body.name, v.password, password.input))) return;
          body.password = v.password;
        }
        body.confirm_password = v.confirm_password;
        if (!code.el.hidden && v.code) body.code = codeValue(v.code);
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
          if (e.code === 'wrong_password' || e.code === 'password_needed') { own.setError(v.confirm_password ? 'That is not your password. Type the one you sign in with.' : 'Type your password to go on.'); own.input.focus(); own.input.select(); return; }
          if (e.code === 'wrong_two_step' || e.code === 'wrong_code') { code.el.hidden = false; code.setError(v.code ? 'That code is not right. Type the one your authenticator app shows now.' : 'Your account has two-step sign-in: type the code from your authenticator app as well.'); code.input.focus(); return; }
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
    const ok = await askPassword(ctx, {
      title: `Remove ${a.name}?`,
      intro: h('p', null, `${a.name} can no longer sign in, and every session of theirs ends now. Their changes stay in the audit log.`),
      submit: 'Remove account', danger: true,
      run: async (cred) => {
        try {
          await ctx.api.send('DELETE', `accounts/${enc(a.id)}`, { confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
        } catch (e) {
          if (e.code === 'wrong_password' || e.code === 'wrong_two_step' || e.status === 429) throw e;
          if (e.code === 'wrong_code') { e.code = 'wrong_two_step'; throw e; }
          flash(e.code === 'last_admin' ? 'This is the last admin. Make another account an admin first.' : `Could not remove ${a.name}: ${errText(e)}`, { kind: 'error' });
          return false;
        }
        return true;
      },
    });
    if (!ok || !ctx.alive()) return;
    flash(`${a.name} was removed.`);
    await refreshAccounts();
  }


  /* ---------------------------------------------------------------- two-step sign-in (everyone), and the rule (admin) */
  // One's own two-step sign-in: GET /me/two-step, POST /me/two-step (the secret and its otpauth address),
  // POST /me/two-step/confirm (password and the first code: the recovery codes, shown once), DELETE /me/two-step and
  // POST /me/two-step/recovery-codes (password and a code). The rule (admin): GET and PUT /two-step-policy.
  const tsBody = h('div', { class: 'ss-twostep' });
  const cancelBtn = twoCancelBtn;
  const ownPassword = () => twoOwnPassword(me);
  async function loadTwoStep() {
    try { S.twoStep = await ctx.api.get('me/two-step'); } catch { /* the panel keeps what it had */ }
    if (S.twoStep) me.two_step = !!S.twoStep.enabled;
  }
  async function turnOnTwoStep() {
    const r = await confirmTwoStep(ctx, me);
    if (r?.already) { await loadTwoStep(); paintTwoStep(); return; }
    if (!r || !ctx.alive()) return;
    me.two_step = true;
    await loadTwoStep();
    paintTwoStep();
    if (admin) refreshAccounts();
    flash('Two-step sign-in is on.');
    await recoveryDialog(r.codes, 'Your recovery codes');
  }
  // the password and a code again, then run({ password, code }); the dialog stays open on a wrong one
  function ownAgain({ title, intro, submit, danger = false, run }) {
    const password = ownPassword();
    const code = ownCode('Code from your authenticator app');
    let result = null;
    const f = form({
      fields: [password, code], submit, secondary: cancelBtn(),
      async onSubmit(v) {
        try { result = await run({ password: v.password, code: codeValue(v.code) }); } catch (e) {
          if (ownWrong(e, password, code, v)) return;
          if (e.code === 'two_step_kept') { f.setMessage('The rule asks every account for two-step sign-in, so it cannot be turned off. An admin can change the rule.'); return; }
          throw e;
        }
        f.el.closest('dialog')?.close();
      },
    });
    if (danger) f.el.querySelector('button[type="submit"]')?.classList.replace('btn--primary', 'btn--danger');
    const done = openDialog({ title, body: [intro, f.el], actions: [] });
    password.input.focus({ preventScroll: true });
    return done.then(() => result);
  }
  async function newRecoveryCodes() {
    const out = await ownAgain({
      title: 'New recovery codes',
      intro: h('p', null, 'The recovery codes you have now stop working. You get ten new ones, shown once.'),
      submit: 'Make new codes', run: (cred) => ctx.api.post('me/two-step/recovery-codes', cred),
    });
    if (!out || !ctx.alive()) return;
    await loadTwoStep();
    paintTwoStep();
    await recoveryDialog(out.recovery_codes || [], 'Your new recovery codes');
  }
  async function turnOffTwoStep() {
    const out = await ownAgain({
      title: 'Turn off two-step sign-in?',
      intro: h('p', null, 'After this your password alone signs in to your account, and your recovery codes stop working.'),
      submit: 'Turn off', danger: true, run: (cred) => ctx.api.send('DELETE', 'me/two-step', cred),
    });
    if (!out || !ctx.alive()) return;
    me.two_step = false;
    await loadTwoStep();
    paintTwoStep();
    if (admin) refreshAccounts();
    flash('Two-step sign-in is off.');
  }
  async function saveRule(key, box) {
    const want = box.checked;
    // the switch is drawn again from what the server has when the dialog closes: back as it was if cancelled
    const next = { everyone: !!S.policy?.everyone, outside: !!S.policy?.outside, [key]: want };
    const risky = key === 'everyone' && want && !me.two_step;
    const out = await askPassword(ctx, {
      title: want ? 'Require two-step sign-in?' : 'Stop requiring two-step sign-in?',
      intro: h('div', null,
        h('p', null, key === 'everyone'
          ? (want ? 'Every account will need a code from an authenticator app after its password. An account without it can only set it up until it has.' : 'Accounts that have two-step sign-in on keep it; the others can sign in with a password alone again.')
          : (want ? 'Signing in from outside the home network will need a code from an authenticator app after the password. An account without two-step sign-in cannot sign in from outside.' : 'Signing in from outside the home network needs no code any more, unless the rule for every account is on.')),
        risky ? h('p', { class: 'form-error ss-warn' }, icon('warn'), ' Your own account has no two-step sign-in yet: after this you must set it up next, before you can use anything else.') : null),
      submit: 'Save the rule',
      run: async (cred) => {
        try {
          return await ctx.api.put('two-step-policy', { ...next, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
        } catch (e) {
          if (e.code === 'wrong_code') e.code = 'wrong_two_step';
          throw e;
        }
      },
    });
    if (out && ctx.alive()) {
      S.policy = { everyone: !!out.everyone, outside: !!out.outside };
      if (S.twoStep) S.twoStep.required = S.policy;
      flash('Saved the two-step rule.');
    }
    if (ctx.alive()) paintTwoStep();
  }
  function paintTwoStep() {
    const t = S.twoStep;
    const on = !!t?.enabled;
    const left = t?.recovery_codes_left;
    const own = !t
      ? h('p', { class: 'note' }, 'This page could not read your two-step sign-in. Reload it to try again.')
      : setting('Your two-step sign-in',
        on
          ? [pill('On', { tone: 'warm' }), ` After your password, a code from your authenticator app. ${left == null ? '' : `${format(left, 'count')} recovery ${left === 1 ? 'code' : 'codes'} left.`}`]
          : [pill('Off', { tone: 'plain' }), ' Your password alone signs in. Turn it on to add a six-digit code from an authenticator app.'],
        on
          ? h('div', { class: 'row ss-acts' },
            h('button', { class: 'btn btn--sm', type: 'button', onclick: () => newRecoveryCodes() }, 'New recovery codes'),
            h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => turnOffTwoStep() }, 'Turn off'))
          : h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: () => turnOnTwoStep() }, 'Turn on'));
    const rule = admin && S.policy ? setting('Where it is required', 'The rule for every account, not for one: set here by an admin.',
      h('div', { class: 'ss-rule' },
        h('label', { class: 'check' }, h('input', { type: 'checkbox', name: 'rule-everyone', checked: !!S.policy.everyone, onchange: (e) => saveRule('everyone', e.target) }), h('span', null, 'Require it for every account')),
        h('label', { class: 'check' }, h('input', { type: 'checkbox', name: 'rule-outside', checked: !!S.policy.outside, onchange: (e) => saveRule('outside', e.target) }), h('span', null, 'Require it from outside the home network')),
        !me.two_step ? h('p', { class: 'note ss-rule-note' }, icon('info', 'ico--sm'), ' Your own account has no two-step sign-in yet: if you turn on "Require it for every account", you must set it up next.') : null)) : null;
    replace(tsBody, own, rule);
  }
  paintTwoStep();
  const twoPanel = panel({ title: 'Two-step sign-in', icon: 'lock', pad: true }, tsBody);

  /* ---------------------------------------------------------------- your sessions */
  const focusSession = query.session || null;
  const sessionsTable = dataTable(ctx, {
    title: 'Your sessions', icon: 'monitor', rows: S.sessions, label: 'Where you are signed in', csvName: 'sessions',
    note: admin
      ? `Every browser where your account is signed in; times are in ${zoneText}. End one you do not recognise, then give your account a new password with Change your password.`
      : `Every browser where your account is signed in; times are in ${zoneText}. End one you do not recognise, then give your account a new password with Change your password.`,
    side: [h('button', { class: 'btn btn--sm', type: 'button', onclick: () => changePassword() }, icon('lock', 'ico--sm'), 'Change your password'),
      h('button', { class: 'btn btn--sm', type: 'button', onclick: () => endOthers() }, icon('signout', 'ico--sm'), 'Sign out everywhere else')],
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
  // anyone's own password (PUT /me/password): the current one again, the new one, and the code with two-step
  // sign-in on; the account's other sessions end
  function changePassword() {
    const current = field({ label: 'Current password', name: 'password', type: 'password', required: true,
      hint: `The password you sign in with as ${me.name || 'this account'}.`, attrs: { autocomplete: 'current-password' } });
    const next = field({ label: 'New password', name: 'new_password', type: 'password', required: true,
      hint: 'Every other browser where your account is signed in is signed out; this one stays.', attrs: { autocomplete: 'new-password' } });
    const code = field({ label: 'Code from your authenticator app', name: 'code', hint: CODE_HINT,
      attrs: codeAttrs() });
    code.el.hidden = !(me.two_step || me.two_step_required);
    const f = form({
      fields: [current, next, code], submit: 'Change password',
      secondary: h('button', { class: 'btn btn--ghost', type: 'button', onclick: (e) => e.target.closest('dialog')?.close() }, 'Cancel'),
      async onSubmit(v) {
        if (!(await passwordOk(me.name || '', v.new_password, next.input))) return;
        let out;
        try {
          out = await ctx.api.put('me/password', { password: v.password, new_password: v.new_password, ...(!code.el.hidden && v.code ? { code: codeValue(v.code) } : {}) });
        } catch (e) {
          if (e.code === 'wrong_password') { current.setError('That is not your password. Type the one you sign in with.'); current.input.focus(); current.input.select(); return; }
          if (e.code === 'wrong_code' || e.code === 'wrong_two_step') { code.el.hidden = false; code.setError(v.code ? 'That code is not right. Type the one your authenticator app shows now.' : 'Your account has two-step sign-in: type the code from your authenticator app as well.'); code.input.focus(); return; }
          throw e;
        }
        f.el.closest('dialog')?.close();
        const n = out?.signed_out_others || 0;
        flash(n ? `Password changed. ${format(n, 'count')} other ${n === 1 ? 'browser was' : 'browsers were'} signed out.` : 'Password changed.');
        await refreshSessions();
      },
    });
    openDialog({ title: 'Change your password', body: f.el, actions: [] });
    current.input.focus();
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
    limit: 10, autoMore: true, moreLabel: 'Load more', cls: 'ss-audit', side: auditFilters, box: 'Audit log entries',
    note: `Every change and every sign-in, newest first; five show at a time, and more load as you scroll the list. The item links to what was changed, where it still exists. Times are in ${zoneText}.`,
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

  /* ---------------------------------------------------------------- storage and compaction (admin) */
  // A compact panel: one row of figures (on disk, free, saved by compaction, the last pass) and Compact now, then
  // the space by kind (shared/storage.js): one row per group with its size, a bar and its share; a group opens to
  // its categories by short name, at most five rows tall (the CSV has every category with the server's own
  // label). "Compact now" starts a pass; while one runs (also one the server started by itself) its progress comes
  // as `job` events of kind compaction (and the free space as `storage` events), changed in place; the detail is
  // read at the start, after a reconnect, when a pass ends (the groups are drawn again and a note says what was
  // freed) and, only while the live stream is down, every 2 s while a pass runs (30 s otherwise).
  const compactBtn = h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: () => compactNow() }, icon('reset', 'ico--sm'), 'Compact now');
  const storFigs = h('dl', { class: 'st-figs' });
  const storList = h('ul', { class: 'st-groups', 'aria-label': 'Space on disk by kind' });
  const storOpen = new Set();
  let storOpenSet = false;
  const storCsv = csvButton({ onClick: () => downloadCsv(csvFilename('sentry-mate', 'storage on disk'), S.storage?.categories || [], [
    { label: 'What', value: (c) => c.label }, { label: 'Kind', value: (c) => groupWord(c.group) }, { label: 'Key', value: (c) => c.key },
    { label: 'Files', value: (c) => c.files }, { label: 'Size (bytes)', value: (c) => c.bytes }, { label: 'On disk (bytes)', value: (c) => c.disk_bytes }]) });
  const LOSSLESS = 'Compaction packs data that is no longer written, losslessly: every value stays readable exactly, and the logs\' signed originals are kept byte for byte.';
  const storTip = h('span', { class: 'st-tip', title: LOSSLESS, 'aria-label': LOSSLESS, role: 'img' }, icon('info', 'ico--sm'));
  // the progress row: one box of fixed size, the same idle or running (nothing below it moves); its parts
  // stay and only their words change
  const prog = {
    lead: h('span', { class: 'ss-prog-lead' }),
    head: h('span', { class: 'ss-prog-text' }),
    bar: h('div', { class: 'bar ss-prog-bar', role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-label': 'Progress of this step' }, h('i')),
    count: h('span', { class: 'ss-prog-count' }),
    info: h('p', { class: 'ss-prog-info' }),
    err: h('p', { class: 'ss-prog-err warn' }),
  };
  const storLine = h('div', { class: 'ss-prog' },
    h('p', { class: 'ss-prog-head', 'aria-live': 'polite' }, prog.lead, prog.head, storTip),
    h('div', { class: 'ss-prog-barrow' }, prog.bar, prog.count), prog.info, prog.err);
  const say = (el, t) => { const s = t || ''; if (el.textContent !== s) el.textContent = s; el.title = s; };
  let leadKind = null;
  function lead(kind) {
    if (leadKind === kind) return;
    leadKind = kind;
    replace(prog.lead, kind === 'run' ? h('span', { class: 'mv-spin', 'aria-hidden': 'true' }) : icon(kind === 'bad' ? 'warn' : 'info', 'ico--sm'));
  }
  const storagePanel = admin ? panel({ title: 'Storage', icon: 'server', pad: true, cls: 'st', id: 'storage', side: h('span', { class: 'row st-side' }, storCsv, compactBtn) },
    storFigs, storLine, storList) : null;
  const fig = (label, value, title = null) => h('div', { class: 'st-fig', title }, h('dt', null, label), h('dd', null, value));
  let groupsSig = null;
  function paintGroups(force = true) {
    // a read of the detail that did not change the categories leaves the list as it is (opened groups, scroll)
    const sig = JSON.stringify(S.storage?.categories || null) + (S.storageErr ? errText(S.storageErr) : '');
    if (!force && sig === groupsSig) return;
    groupsSig = sig;
    const groups = storageGroups(S.storage?.categories);
    if (!storOpenSet && groups.length) { storOpen.add(groups[0].group); storOpenSet = true; }  // the largest, open at first
    if (!groups.length) {
      replace(storList, h('li', { class: 'st-none' }, `The server did not say what takes the space${S.storageErr ? `: ${errText(S.storageErr)}` : '.'}`));
      return;
    }
    // the list is built again: a head or a scrolling list that had the focus gets it back
    const kept = storList.contains(document.activeElement) ? document.activeElement.getAttribute('data-fk') : null;
    replace(storList, groups.map((g) => {
      const open = storOpen.has(g.group);
      const bar = h('span', { class: 'bar st-bar', 'aria-hidden': 'true' }, h('i'));
      setVars(bar, { '--w': `${Math.max(g.share > 0 ? 1 : 0, g.share * 100).toFixed(1)}%` });
      const rows = h('ul', { class: 'st-rows', tabindex: '0', 'data-fk': `rows:${g.group}`, 'aria-label': g.name, hidden: !open },
        g.rows.map((c) => h('li', { class: 'st-row', title: c.label },
          h('span', { class: 'st-row-name' }, shortName(c)), h('span', { class: 'st-row-size num' }, bytes(c.disk_bytes)))));
      const head = h('button', { class: 'st-group-head', type: 'button', 'data-fk': `head:${g.group}`, 'aria-expanded': String(open), onclick: () => {
        if (storOpen.has(g.group)) storOpen.delete(g.group); else storOpen.add(g.group);
        paintGroups();
      } }, icon(open ? 'chev-d' : 'chev-r', 'ico--sm'), h('span', { class: 'st-group-name' }, g.name), bar,
      h('span', { class: 'st-group-size num' }, bytes(g.disk_bytes)), h('span', { class: 'st-group-share num' }, shareText(g.share)));
      return h('li', { class: ['st-group', open && 'is-open'] }, head, rows);
    }));
    if (kept) [...storList.querySelectorAll('[data-fk]')].find((el) => el.getAttribute('data-fk') === kept)?.focus();
  }
  let storPollErr = null;
  function paintStorage() {
    const d = S.storage;
    const c = d?.compaction || null;
    const running = c?.status === 'running';
    compactBtn.disabled = running || !c;
    const last = !c || c.finished_ms == null ? 'Not yet' : c.last_saved_bytes ? `${bytes(c.last_saved_bytes)} freed` : 'Nothing left to pack';
    const num = (v) => (v == null ? DASH : bytes(v));
    morphChildren(storFigs, [
      fig('On disk', num(d?.total_disk_bytes), 'Everything below, as it lies on disk.'),
      fig('Free', num(d?.free_bytes), 'Room left on the disk for new data.'),
      fig('Saved', num(c?.saved_bytes), 'What compaction has freed in all.'),
      fig('Last pass', last, c?.finished_ms != null ? `Finished ${dateTime(c.finished_ms, tz)} (${zone}).` : 'No pass has finished yet.')]);
    const p = compactionProgress(c, { now: ctx.api.now?.() ?? Date.now(), bytes });
    if (storLine.classList.contains('is-running') !== running) storLine.classList.toggle('is-running', running);
    const bad = !running && c?.status === 'failed';
    if (storLine.classList.contains('is-bad') !== bad) storLine.classList.toggle('is-bad', bad);
    if (running) {
      lead('run');
      say(prog.head, [p.stepOf, p.step].filter(Boolean).join(' · ') || 'Packing…');
      if (prog.bar.classList.contains('is-counting') !== !!p.counting) prog.bar.classList.toggle('is-counting', !!p.counting);
      const w = p.pct == null ? '0%' : `${p.pct.toFixed(1)}%`;
      if (prog.bar.style.getPropertyValue('--w') !== w) setVars(prog.bar, { '--w': w });
      const now = p.pct == null ? null : String(Math.round(p.pct));
      if (prog.bar.getAttribute('aria-valuenow') !== now) { if (now == null) prog.bar.removeAttribute('aria-valuenow'); else prog.bar.setAttribute('aria-valuenow', now); }
      say(prog.count, p.count);
      say(prog.info, [p.saved, p.rate, p.stalled ? null : p.eta, p.last].filter(Boolean).join(' · '));
      // the stall warning takes the estimate's place; the units left as they were, and a progress read that
      // failed, go on the warn line (cut with an ellipsis, never growing the row)
      say(prog.err, [p.stall, p.errors, storPollErr ? `Could not read the progress just now: ${errText(storPollErr)}` : null].filter(Boolean).join(' '));
    } else {
      lead(c?.status === 'failed' ? 'bad' : 'idle');
      if (c?.status === 'failed') morphChildren(prog.head, [h('b', null, 'The last pass failed: '), c.error || 'the server gave no reason.']);
      else say(prog.head, !c ? 'The server did not say how compaction stands.'
        : c.automatic ? 'Compaction runs by itself when the server is idle; Compact now starts a pass at once.'
          : 'Compaction does not run by itself on this server; Compact now starts a pass.');
      prog.head.title = prog.head.textContent;
      say(prog.count, ''); say(prog.info, ''); say(prog.err, '');
    }
  }
  // the pass's state: from a read of the detail, a `job` event (its compaction object) or a `storage` event
  let storRunning = false;
  function takeCompaction(c, how) {
    const ended = storRunning && c?.status !== 'running';
    storRunning = c?.status === 'running';
    paintStorage();
    if (!ended) return;
    const endNote = () => {
      if (!ctx.alive()) return;
      if (c?.status === 'failed') flash(`Compaction failed: ${c.error || 'the server gave no reason.'}`, { kind: 'error' });
      else flash(c?.last_saved_bytes ? `Compaction is done: ${bytes(c.last_saved_bytes)} freed on disk.` : 'Compaction is done: nothing was left to pack.');
    };
    // what it freed changed the sizes: the detail is read again for them (not sent on the stream), then the note
    if (how === 'event' && storLive) storLive.refresh().then(endNote, endNote);
    else endNote();
  }
  let storLive = null;
  let storFirst = null;
  const storReady = admin ? new Promise((res) => { storFirst = res; }) : null;
  if (storagePanel) {
    storLive = follow(ctx.api, {
      type: 'job', match: jobOf('compaction'), pick: (d) => laterCompaction(S.storage?.compaction, d.status), // (a failed pass keeps its reason)
      load: () => ctx.api.get('system/storage/detail'),
      // (a `job` event that comes in while the detail is read — one comes with the stream's first words — has the pass
      // only, the detail the categories too: the read is kept, the pass as the event says it unless the read has how it failed)
      overtaken: (detail, pass) => ({ ...detail, compaction: laterCompaction(detail.compaction, pass) }),
      pollMs: () => (S.storage?.compaction?.status === 'running' ? 2000 : 30000),
      onValue: (v, how) => {
        if (!ctx.alive()) return;
        if (how === 'load') { S.storage = v; S.storageErr = null; storPollErr = null; paintGroups(false); } else S.storage = { ...(S.storage || {}), compaction: v };
        takeCompaction(S.storage?.compaction, how);
        if (how === 'load') storFirst(); // (the page is ready with the detail read, not with the first word of the stream)
      },
      onError: (e) => {
        if (!ctx.alive()) return;
        if (S.storage) storPollErr = e; else { S.storageErr = e; paintGroups(false); }
        paintStorage(); storFirst();
      },
    });
    // the free space and the pass's state also come with the server's storage figures (no categories)
    follow(ctx.api, {
      type: 'storage',
      onValue: (d) => {
        if (!ctx.alive() || !d) return;
        S.storage = { ...(S.storage || {}), free_bytes: d.free_bytes ?? S.storage?.free_bytes ?? null, compaction: d.compaction ? laterCompaction(S.storage?.compaction, d.compaction) : S.storage?.compaction || null };
        takeCompaction(S.storage.compaction, 'event');
      },
    });
  }
  async function compactNow() {
    compactBtn.disabled = true;
    try {
      const r = await ctx.api.post('system/storage/compact');
      if (!ctx.alive()) return;
      S.storage = { ...(S.storage || {}), compaction: r.compaction };
      storPollErr = null;
    } catch (e) {
      if (!ctx.alive()) return;
      flash(`Could not start compaction: ${errText(e)}`, { kind: 'error' });
    }
    takeCompaction(S.storage?.compaction, 'post');
    // with the live stream down, the fallback reads start now (2 s apart while the pass runs)
    if (storLive && !storLive.live) storLive.refresh();
  }
  if (storagePanel) { paintGroups(); paintStorage(); }

  /* ---------------------------------------------------------------- moving to another server (admin) */
  const move = admin ? movePanel(ctx, { tz, zone: zoneText }) : null;

  /* ---------------------------------------------------------------- the full rebuild (admin) */
  let fullPanel = null, full = null;
  if (admin) {
    // the cars whose history was made with another way of decoding (from the status), named
    const staleLine = h('p', { class: 'ss-fr-stale', hidden: true });
    const paintStale = (st) => {
      const ids = st?.version?.stale_cars || [];
      staleLine.textContent = ids.length
        ? `Made with an older way of decoding the logs: ${ids.map((c) => carName.get(c) || c).join(', ')}. A full rebuild makes ${ids.length === 1 ? 'its history' : 'their histories'} again with this server's.`
        : '';
      staleLine.hidden = !ids.length;
    };
    full = fullRebuild(ctx, { carName: (c) => carName.get(c) || c, onStatus: paintStale });
    // full_rebuild.auto: saved at once with the settings' version
    const autoOn = () => S.settings.full_rebuild?.auto ?? true;
    const autoState = h('label', { for: 'ss-fr-auto' }, autoOn() ? 'On' : 'Off');
    const autoInput = h('input', { type: 'checkbox', id: 'ss-fr-auto', role: 'switch', checked: autoOn(),
      'aria-label': 'Rebuild automatically when a new server version decodes the stored logs differently',
      onchange: async () => {
        const want = autoInput.checked;
        autoInput.disabled = true;
        try {
          S.settings = await ctx.api.patch('settings', { version: S.settings.version, full_rebuild: { auto: want } });
          flash(want ? 'Automatic full rebuild on.' : 'Automatic full rebuild off: a notice offers it instead.');
        } catch (e) {
          if (e.status === 409) {
            S.settings = await ctx.api.get('settings').catch(() => S.settings);
            flash('Someone else changed the server settings meanwhile; this page shows theirs now. Try again.', { kind: 'info' });
          } else flash(`Could not save: ${errText(e)}`, { kind: 'error' });
        } finally { autoInput.disabled = false; }
        if (!ctx.alive()) return;
        autoInput.checked = autoOn();
        autoState.textContent = autoOn() ? 'On' : 'Off';
      } });
    fullPanel = panel({ title: 'Full rebuild', icon: 'reset', pad: true, id: 'full-rebuild' },
      full.el, staleLine,
      setting('Rebuild automatically', 'When a new server version decodes the stored logs differently, the cars whose history was made the old way are rebuilt by themselves, once for that version: 10 minutes after the start, unless you start it now, put it off until tonight or pause it. Off, a notice offers it instead.',
        h('div', { class: 'switch-row switch-row--inline' }, h('span', { class: 'switch' }, autoInput, h('i', { 'aria-hidden': 'true' })), autoState)));
  }

  // Change names: the list for the next certificate, saved here; then, if the server makes its own, a new one
  async function changeNames() {
    draft = [...(S.settings.host_names || [])];
    nameInput.value = ''; nameErr.hidden = true;
    paintNames();
    const go = await openDialog({
      title: 'Names for the certificate', wide: true,
      body: h('div', { class: 'ss-names-dlg' },
        h('p', null, 'The names (or addresses) people and devices use to reach this server. The server puts them in the certificate it makes, so browsers accept those names. A saved change goes into the next certificate it makes.'),
        namesBox,
        certOwn() ? h('p', { class: 'note' }, 'The server runs with your own certificate, so these names go only into a certificate it makes, not into yours. To have browsers accept another name, get a certificate that names it, put its ', mono('cert.pem'), ' and ', mono('key.pem'), ' in ', mono('data/tls'), ' and start the container again.') : null),
      actions: certOwn() ? [{ label: 'Close', value: null }]
        : [{ label: 'Close', value: null }, { label: 'Make a new certificate', value: 'cert', kind: 'primary' }],
    });
    if (go === 'cert' && ctx.alive()) makeCertificate();
  }

  /* ---------------------------------------------------------------- server updates (every visitor; Check now: admins) */
  const updBox = h('div', { class: 'ss-upd' });
  let updChecking = false, updTimer = null;
  ctx.onCleanup(() => clearTimeout(updTimer));
  async function updCheckNow() {
    updChecking = true; paintUpdates();
    try {
      S.release = await ctx.api.post('release/check', {});
      if (S.release?.status === 'error') flash(S.release.error?.message ? `Could not check: ${S.release.error.message}` : 'Could not check.', { kind: 'error' });
    } catch (e) {
      if (!ctx.alive()) return;
      flash(e?.code === 'release_check_off' ? 'Not checked: the release check is off.' : `Not checked: ${errText(e)}`, { kind: 'error' });
      S.release = await ctx.api.get('release').catch(() => S.release);
    }
    updChecking = false;
    if (ctx.alive()) paintUpdates();
  }
  function paintUpdates() {
    const u = serverUpdates(S.about, S.release, ctx.api.now());
    const busy = updChecking || u.busy;
    replace(updBox,
      kv(u.rows.map((r) => [r.label, h('span', { class: ['ss-upd-val', `ss-upd-val--${r.tone || 'plain'}`] },
        h('span', { class: r.key === 'this' ? 'mono' : null, title: r.title || null }, r.text),
        r.badge ? pill(r.badge, { tone: r.tone === 'newer' ? 'warm' : 'live', ic: r.tone === 'newer' ? 'upload' : 'check' }) : null)])),
      h('div', { class: 'ss-upd-check' },
        h('p', { class: 'ss-upd-on' }, 'Release check: ', h('b', null, u.on ? 'On' : 'Off'), u.on ? '. Once a day the server asks GitHub whether a newer image is published.' : `. ${RELEASE_OFF_TEXT}`,
          !u.on ? [' ', admin ? 'Turn it on with the switch above.' : 'An admin can turn it on.'] : null),
        admin ? h('button', { class: 'btn btn--sm', type: 'button', disabled: !u.on || busy, title: u.on ? null : 'Turn the release check on first', onclick: updCheckNow },
          icon('reset', 'ico--sm'), busy ? 'Checking…' : 'Check now') : null),
      h('p', { class: 'note ss-upd-how' }, `${u.how} Steps for Docker and TrueNAS: `,
        u.doc.href ? h('a', { class: 'link', href: u.doc.href, target: '_blank', rel: 'noopener noreferrer external' }, u.doc.text) : mono(u.doc.path), '.'));
    if (S.release?.status === 'checking' && !updChecking) {
      clearTimeout(updTimer);
      updTimer = setTimeout(async () => { S.release = await ctx.api.get('release').catch(() => S.release); if (ctx.alive()) paintUpdates(); }, 4000);
    }
  }
  // the release check's switch (#release-check, PATCH /settings release_check with the settings' version; an
  // admin's, read only for others): it lives here, beside what the check answered
  const relOn = () => !!S.settings?.release_check?.enabled;
  const relState = h('label', { for: 'ss-release' }, relOn() ? 'On' : 'Off');
  const relInput = h('input', { type: 'checkbox', id: 'ss-release', role: 'switch', 'aria-label': 'Check for new releases', checked: relOn(), disabled: !admin || !S.settings,
    onchange: async () => {
      const want = relInput.checked;
      relInput.disabled = true;
      try {
        S.settings = await ctx.api.patch('settings', { version: S.settings.version, release_check: { enabled: want } });
        flash(want ? 'Release check on.' : 'Release check off.');
      } catch (e) {
        if (e.status === 409) {
          S.settings = await ctx.api.get('settings').catch(() => S.settings);
          flash('Someone else changed the server settings meanwhile; this page shows theirs now. Try again.', { kind: 'info' });
        } else flash(`Could not save: ${errText(e)}`, { kind: 'error' });
      } finally { relInput.disabled = false; }
      if (!ctx.alive()) return;
      relInput.checked = relOn();
      relState.textContent = relOn() ? 'On' : 'Off';
      S.release = await ctx.api.get('release').catch(() => S.release);
      if (ctx.alive()) paintUpdates();
    } });
  const relRow = setting('Check for new releases', 'Once a day the server asks GitHub whether a newer Sentry Mate image is published, and tells admins. Only that question goes out: no token, no car, no account. It never downloads or installs anything.',
    h('div', { class: 'switch-row switch-row--inline' }, h('span', { class: 'switch' }, relInput, h('i', { 'aria-hidden': 'true' })), relState));
  relRow.id = 'release-check';
  paintUpdates();
  const updatesPanel = panel({ title: 'Server updates', icon: 'upload', pad: true, id: 'updates' }, relRow, updBox);

  /* ---------------------------------------------------------------- the page */
  paintHosting();
  paintNames();
  const hostPanel = panel({ title: 'How this server is reached', icon: 'globe', pad: true, cover: 'web.shell.hosting' },
    h('p', { class: 'ss-addr' }, 'This page: ', h('span', { class: 'mono' }, `${loc.protocol}//${loc.host}/`)),
    hostingBox);
  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'settings-stack ss' },
      h('div', { class: 'ss-two' }, hostPanel, h('div', { class: 'ss-col' }, updatesPanel, accessPanel)),
      admin ? safety : null,
      fullPanel,
      // the move and Storage side by side (one above the other on a phone or a narrow window)
      move || storagePanel ? h('div', { class: 'ss-pair' }, move ? move.el : null, storagePanel) : null,
      accountsTable ? [goneNote, accountsTable.el] : null,
      twoPanel,
      sessionGone, sessionsTable.el,
      auditTable ? auditTable.el : null));
  if (auditTable) await auditTable.ready;
  if (move) await move.ready;
  if (full) await full.ready;
  if (storReady) await storReady;
  if (!ctx.alive()) return;
  ctx.ready();
}
