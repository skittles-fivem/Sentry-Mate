// Sentry Mate, "Pebble" UI pack: Settings: Server and access (/settings/server).
//
// Who can sign in, what they may see, how the server is reached, and moving it, as rounded tiles:
// - Everyone: how this address is reached (HTTPS, host name, port), Server updates (this build and the release
//   check's last answer: about, release; Check now and the release check's switch for an admin, release/check,
//   settings release_check), how access works, and their own sessions.
// - Admins also: the certificate (its names changed in a sheet: Change names) and the recovery key (show it again,
//   make a new one, say it is saved), the move to another server as numbered step cards (with a move code, or
//   with an export file: the current card holds the one clear button and what it printed), the accounts (an edit
//   sheet each), the audit log, and Storage: the space on disk by category and what compaction saved, with a
//   "Compact now" button.
// A move that stopped on the new server (a code cancelled here, say) carries on: a new code from this same server
// lets the new server go on with what it copied. Data: settings, network, notices, accounts, me/sessions, audit,
// system/move (+ /undo), system/exports, system/recovery-key/{show,new,saved}, system/storage/detail and
// system/storage/compact. Polls system/move once a second while a move runs (it has no event); the export file
// follows `job` events of kind export, compaction those of kind compaction and the free space `storage` events
// (live events, polling only while the live stream is down: the export while it is made, the detail every 2 s
// while a pass runs). A full rebuild of every car (system/full-rebuild,
// its own tile: core/fullrebuild.js) with the switch for the automatic one (settings full_rebuild.auto).

import { h } from '../shared/dom.js';
import { downloadCsv } from '../shared/csv.js';
import { storageGroups, shortName, shareText, groupWord } from '../shared/storage.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatDateTime, formatDate } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { tile, btn, linkBtn, chip, note, icon, ago } from '../core/ui.js';
import { openSheet, confirmSheet, passwordOk } from '../core/sheet.js';
import { field, form, segmented } from '../core/form.js';
import { toggle } from '../core/form.js';
import { checkRow, sectionHead, moreButton, listBox } from '../core/bits.js';
import { group, infoRow, said, notSaved, askPassword, keySheet, tick } from '../core/settings.js';
import { inOrder } from '../core/settings.js';
import { AUDIT_GROUPS, actionLabel, auditItem } from '../shared/audit.js';
import { fullRebuildTile } from '../core/fullrebuild.js';
import { serverUpdates, RELEASE_OFF_TEXT } from '../shared/release.js';
import { compactionProgress, laterCompaction } from '../shared/compaction.js';
import { bytes, bigCode, copyButton, downloadUrl, stepCards, progressBar, callout, PROBLEM } from './move-parts.js';
import { exportModel, exportPollMs } from '../shared/exportjob.js';
import { follow, jobOf, setText, setAttr } from '../shared/live.js';
import { morph, morphChildren } from '../core/morph.js';

export const id = 'settings-server';
export const shape = [[2, 'h2'], [2, 'h2'], [2, 'h3'], [2, 'h3']];
export const title = () => 'Server and access';
export const css = true;

const ROLE = { admin: 'Admin', watch: 'Watch only' };
const ROLE_TEXT = {
  admin: 'Sees every car and every setting, and can change everything.',
  watch: 'Sees only the cars on its list, and changes only its own preferences and sessions.',
};
/** Text that may break only after a slash, a colon or a dot (addresses, a SHA-256). */
const breakable = (t) => String(t).split(/(?<=[/:.])/).map((part, i) => [i ? h('wbr') : null, part]);

export async function mount(ctx) {
  inOrder(ctx);
  const admin = ctx.admin;
  const me = ctx.me || {};
  const tz = ctx.tz;
  const when = (ms) => (ms == null ? '—' : formatDateTime(ms, tz, { now: api.now() }));
  let [settings, network, notices, cars, accounts, sessions] = await Promise.all([
    ctx.api.get('settings'),
    admin ? ctx.api.get('network').catch(() => null) : null,
    ctx.api.get('notices', null, { maxAgeMs: 5000 }).then((r) => r.items || []).catch(() => []), // the frame's answer of this load
    ctx.api.list('cars', { sort: 'custom' }).all({ max: 500 }).catch(() => []),
    admin ? ctx.api.list('accounts', {}).all({ max: 2000 }) : [],
    ctx.api.list('me/sessions', {}).all({ max: 500 }),
  ]);
  let [about, release] = await Promise.all([ctx.api.get('about', null, { maxAgeMs: 600000 }).catch(() => null), ctx.api.get('release').catch(() => null)]);
  if (!ctx.alive()) return {};
  const carName = new Map(cars.map((c) => [c.id, c.name]));

  /* ================================================================ how this server is reached */
  const here = { https: location.protocol === 'https:', host: location.hostname.toLowerCase(), port: Number(location.port || (location.protocol === 'https:' ? 443 : 80)) };
  const webIn = network && network.inbound ? network.inbound.find((x) => x.purpose === 'web') : null;
  const certNames = network && network.certificate && Array.isArray(network.certificate.names) ? network.certificate.names.map((n) => n.toLowerCase()) : null;
  const portRow = (o) => { const el = checkRow(o); el.dataset.cover = 'docs.env.web.port'; return el; };
  function hostingTile() {
    const saved = (settings.host_names || []).map((n) => n.toLowerCase());
    const names = certNames || saved;
    const named = names.includes(here.host);
    return tile({ w: 2, title: 'This address', icon: 'globe', cover: 'web.shell.hosting', side: chip(`${location.protocol}//${location.host}/`, { tone: here.https ? 'teal' : 'sun', icon: here.https ? 'lock' : 'warning' }) },
      h('ul', { class: 'checks' },
        here.https ? checkRow({ name: 'HTTPS', sub: 'This page came encrypted, so your password never travels in plain text.', word: 'Yes', level: 'ok' })
          : checkRow({ name: 'Plain HTTP', sub: 'Sentry Mate itself answers only HTTPS; something else serves this page (a development server or a proxy). Never type a password on a plain HTTP page over a network.', word: 'No', level: 'warn' }),
        named ? checkRow({ name: `Host name ${here.host}`, sub: 'It is in the certificate, so the browser trusts it once it trusts the certificate.', word: 'Named', level: 'ok' })
          : checkRow({ name: `Host name ${here.host}`, sub: `The certificate names ${names.join(', ') || 'no host'}, so browsers warn. The CAN Bridge pins the certificate’s SHA-256 and is not affected.${saved.includes(here.host) ? ' The name is saved and goes into the next certificate.' : ''}`, word: 'Not named', level: 'warn',
            side: admin && !saved.includes(here.host) ? btn(`Add ${here.host}`, { kind: 'soft', icon: 'plus', cls: 'btn-sm', onclick: () => saveNames([...(settings.host_names || []), here.host]) }) : null }),
        webIn ? portRow({ name: `Port ${here.port}`, sub: here.port === webIn.port ? 'The same port the server listens on.' : `The server listens on ${webIn.port} in its container; a port mapping such as ${here.port}:${webIn.port} joins the two.`, word: here.port === webIn.port ? 'Same' : 'Mapped', level: 'ok' })
          : portRow({ name: `Port ${here.port}`, sub: 'An admin sees the ports the server listens on.', word: 'Yours', level: 'off' }),
        checkRow({ name: 'At the root of its address', sub: 'Sentry Mate always lives at / on its own name or port: behind a shared proxy, give it its own name rather than a path.', word: 'Yes', level: 'ok' }),
        checkRow({ name: 'Works from any address', sub: 'No list of allowed origins: the session cookie is never sent by other sites, and every change carries a header other sites cannot set.', word: 'Yes', level: 'ok' })));
  }

  /* ================================================================ host names (Change names, in a sheet) */
  let draft = [...(settings.host_names || [])];
  async function saveNames(list) {
    try {
      settings = await ctx.api.patch('settings', { version: settings.version, host_names: list });
      draft = [...settings.host_names];
      ctx.toast('Host names saved. They go into the next certificate the server makes.');
      if (edPaint) edPaint();
    } catch (e) {
      if (e && e.status === 409) { settings = await ctx.api.get('settings'); draft = [...settings.host_names]; ctx.toast('Someone else changed the server settings; this page shows theirs now.', { kind: 'info' }); if (edPaint) edPaint(); } else ctx.toast(notSaved(e), { kind: 'error' });
    }
    if (ctx.alive()) draw();
  }
  let edPaint = null;
  function namesEditor() {
    const box = h('div', { class: 'names-ed' });
    const paint = () => {
      const input = field({ label: 'Add a name', name: 'host', hint: 'Such as sentry-mate.local, or an address such as 192.168.1.20. Up to 10 names.', attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: 'sentry-mate.local' } });
      const err = h('p', { class: 'form-error', hidden: true, role: 'alert' });
      const add = () => {
        const v = input.control.value.trim().toLowerCase();
        err.hidden = true;
        if (!v) return;
        if (v.length > 253 || !/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/.test(v)) { err.textContent = 'A host name has letters, digits, hyphens and dots.'; err.hidden = false; return; }
        if (draft.length >= 10) { err.textContent = 'The certificate takes up to 10 names.'; err.hidden = false; return; }
        if (!draft.includes(v)) draft.push(v);
        paint();
        box.querySelector('input')?.focus();
      };
      input.control.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); add(); } });
      const changed = JSON.stringify(draft) !== JSON.stringify(settings.host_names || []);
      box.replaceChildren(
        group(null, draft.map((n, i) => infoRow({ icon: 'globe', name: h('span', { class: 'set-mono' }, n), sub: n === here.host ? 'This address' : null,
          side: btn(`Remove ${n}`, { kind: 'ghost', icon: 'close', iconOnly: true, cls: 'btn-sm', disabled: draft.length <= 1, onclick: () => { draft = draft.filter((_, j) => j !== i); paint(); } }) }))),
        h('div', { class: 'form' }, input, err),
        h('div', { class: 'form-actions' }, btn('Add', { kind: 'soft', icon: 'plus', onclick: add }),
          btn('Save host names', { kind: 'primary', icon: 'check', disabled: !changed, onclick: () => saveNames(draft) }),
          changed ? btn('Undo changes', { kind: 'ghost', onclick: () => { draft = [...settings.host_names]; paint(); } }) : null));
    };
    paint();
    return { box, paint };
  }
  async function changeNames() {
    draft = [...(settings.host_names || [])];
    const ed = namesEditor();
    edPaint = ed.paint;
    const v = await openSheet({ title: 'Names for the certificate', cls: 'names-sheet',
      body: [h('p', { class: 'sheet-text' }, 'The names people and devices use to reach this server. The server puts them in the certificate it makes, so browsers accept them. A saved change goes into the next certificate.'),
        ed.box,
        certOwn() ? note('The server runs with your own certificate, so these names go only into a certificate it makes. For another name, get a certificate that names it, put its cert.pem and key.pem in data/tls and start the container again.') : null],
      actions: certOwn() ? [{ label: 'Close', value: null }] : [{ label: 'Close', value: null }, { label: 'Make a new certificate', value: 'cert', kind: 'primary' }] });
    edPaint = null;
    if (v === 'cert' && ctx.alive()) makeCertificate();
  }

  /* ================================================================ server updates (everyone; Check now: admins) */
  let updChecking = false, updTimer = null;
  ctx.onCleanup?.(() => clearTimeout(updTimer));
  async function updCheckNow() {
    updChecking = true; draw();
    try {
      release = await ctx.api.post('release/check', {});
      if (release && release.status === 'error') ctx.toast(`Could not check: ${(release.error && release.error.message) || 'no answer'}`, { kind: 'error' });
    } catch (e) {
      if (!ctx.alive()) return;
      ctx.toast(e && e.code === 'release_check_off' ? 'Not checked: the release check is off.' : notSaved(e).replace(/^Not saved/, 'Not checked'), { kind: 'error' });
      release = await ctx.api.get('release').catch(() => release);
    }
    updChecking = false;
    if (ctx.alive()) draw();
  }
  const UPD_TONE = { newer: 'sun', ok: 'teal', error: 'coral', wait: 'muted', off: 'muted' };
  const UPD_ICON = { this: 'server', built: 'calendar', newest: 'upload', checked: 'clock' };
  // the release check's switch (#release-check, PATCH /settings release_check with the settings' version; an
  // admin's, read only for others): made once, kept across the page's redraws
  const relSw = toggle({ label: 'Check for new releases', sub: 'Once a day the server asks GitHub for the newest published build and tells admins. Only that question goes out: no token, no car, no account. It never downloads or installs anything.',
    checked: !!(settings.release_check || {}).enabled, disabled: !admin,
    onchange: async (on) => {
      try {
        settings = await ctx.api.patch('settings', { version: settings.version, release_check: { enabled: on } });
        ctx.toast(on ? 'Release check on. Saved for everyone on this server.' : 'Release check off. Saved for everyone on this server.');
      } catch (e) {
        if (e && (e.status === 409 || e.code === 'stale_version')) {
          settings = await ctx.api.get('settings').catch(() => settings);
          ctx.toast('Someone changed these settings meanwhile; they are shown fresh now. Make your change again.', { kind: 'info' });
        } else ctx.toast(notSaved(e), { kind: 'error' });
      }
      if (!ctx.alive()) return;
      relSw.set(!!(settings.release_check || {}).enabled);
      release = await ctx.api.get('release').catch(() => release);
      if (ctx.alive()) draw();
    } });
  relSw.id = 'release-check';
  function updatesTile() {
    const u = serverUpdates(about, release, api.now());
    const busy = updChecking || u.busy;
    if (release && release.status === 'checking' && !updChecking) {
      clearTimeout(updTimer);
      updTimer = setTimeout(async () => { release = await ctx.api.get('release').catch(() => release); if (ctx.alive()) draw(); }, 4000);
    }
    return tile({ w: 2, title: 'Server updates', icon: 'upload', id: 'updates', side: chip(u.on ? 'Check on' : 'Check off', { tone: u.on ? 'teal' : 'muted' }) },
      group(null, [relSw]),
      group(null, u.rows.map((r) => infoRow({ icon: UPD_ICON[r.key], name: r.label,
        side: h('span', { class: ['set-value', 'upd-val', r.tone && `is-${r.tone}`], title: r.title || null }, r.text, r.badge ? chip(r.badge, { tone: UPD_TONE[r.tone] }) : null) }))),
      u.on ? null : said('info', `The release check is off. ${RELEASE_OFF_TEXT} `, admin ? 'Turn it on with the switch above.' : 'An admin can turn it on.'),
      note(`${u.how} Steps for Docker and TrueNAS: `, u.doc.href ? h('a', { class: 'place-link', href: u.doc.href, target: '_blank', rel: 'noopener noreferrer external' }, u.doc.text) : h('code', { class: 'set-mono' }, u.doc.path), '.'),
      admin ? h('div', { class: 'form-actions' }, btn(busy ? 'Checking…' : 'Check now', { kind: 'soft', icon: 'refresh', disabled: !u.on || busy, title: u.on ? null : 'Turn the release check on first', onclick: updCheckNow })) : null);
  }

  /* ================================================================ the recovery key */
  let keyUnsaved = notices.some((n) => n.code === 'recovery_key_unsaved');
  const cert = notices.find((n) => n.code === 'certificate_expiring');
  async function showKey() {
    const r = await askPassword(ctx, {
      title: 'Show the recovery key again', yes: 'Show the key', hint: 'It is asked again because the key opens all the data.',
      text: 'The server still keeps the key it made when it was set up, until you say it is saved.',
      run: (creds) => ctx.api.post('system/recovery-key/show', creds).catch((e) => { if (e && e.code === 'recovery_key_saved') return { gone: true }; throw e; }),
    });
    if (!r || !ctx.alive()) return;
    if (r.gone) { keyUnsaved = false; ctx.toast('The server deleted its copy already: the key was saved. If it is lost, make a new one.', { kind: 'info' }); draw(); return; }
    if (await keySheet(ctx, r.recovery_key, { lede: 'Keep it in a password manager or on paper, away from the server. Then tick below: the server deletes its copy, and nobody can show the key again.' })) { keyUnsaved = false; ctx.toast('Saved. The server deleted its copy of the recovery key.'); }
    if (ctx.alive()) draw();
  }
  async function newKey() {
    const r = await askPassword(ctx, {
      title: 'Make a new recovery key', yes: 'Make a new key', danger: true, hint: 'It is asked again because the old key stops working.',
      text: 'For a key that was lost, or seen by someone it should not have been. The old key stops working at once; export files and backups made before still need the old one.',
      run: (creds) => ctx.api.post('system/recovery-key/new', creds),
    });
    if (!r || !ctx.alive()) return;
    keyUnsaved = true;
    ctx.toast('A new recovery key was made. The old one no longer works.');
    if (await keySheet(ctx, r.recovery_key, { title: 'Your new recovery key', why: 'new key made', lede: 'Shown only now. Keep it away from the server, then tick below.' })) keyUnsaved = false;
    if (ctx.alive()) draw();
  }
  async function markSaved() {
    if (!await confirmSheet({ title: 'Have you saved the recovery key?', text: 'Only if the SMRK- key is in a password manager or on paper, away from the server. The server then deletes its copy, and nobody can show it again.', yes: 'Yes, delete the copy' })) return;
    try { await ctx.api.post('system/recovery-key/saved', {}); keyUnsaved = false; ctx.toast('Saved. The server deleted its copy of the recovery key.'); } catch (e) { ctx.toast(notSaved(e), { kind: 'error' }); }
    draw();
  }
  function keyTile() {
    return tile({ w: 2, title: 'Certificate and recovery key', icon: 'key', id: 'recovery-key', side: chip(keyUnsaved ? 'Key not saved yet' : 'Key saved', { tone: keyUnsaved ? 'sun' : 'teal' }) },
      h('ul', { class: 'checks' },
        cert ? checkRow({ name: 'Certificate', sub: `Expires ${cert.detail && cert.detail.expires_ms ? formatDate(cert.detail.expires_ms, tz, { now: api.now() }) : 'soon'}. Stop the container, delete data/tls/cert.pem (or put your own there) and start it again; then enter the new SHA-256 in each CAN Bridge.`, word: cert.detail && cert.detail.days != null ? `${cert.detail.days} days` : 'Soon', level: 'warn' })
          : checkRow({ name: 'Certificate', sub: 'No problem reported. It keeps the same SHA-256 across restarts, updates and moves.', word: 'OK', level: 'ok' }),
        keyUnsaved ? checkRow({ name: 'Recovery key', sub: 'Until you say it is saved, the server keeps a copy in its keys folder. Show it again, keep it away from the server, then tick “I have saved it”.', word: 'Not saved', level: 'warn' })
          : checkRow({ name: 'Recovery key', sub: 'Saved away from the server; the server keeps no copy. Lost, or seen? Make a new one.', word: 'Saved', level: 'ok' }),
        checkRow({ name: 'Encrypted at rest', sub: 'Logs, the database, tokens and the certificate’s key are sealed. A backup opens only with the key file or the recovery key: keep those out of the same backup.', word: 'Yes', level: 'ok' })),
      h('div', { class: 'form-actions' },
        keyUnsaved ? [btn('Show the key again', { kind: 'primary', icon: 'key', onclick: showKey }), btn('I saved it already', { kind: 'soft', icon: 'check', onclick: markSaved })] : null,
        btn('Make a new recovery key', { kind: keyUnsaved ? 'ghost' : 'soft', icon: 'refresh', onclick: newKey })));
  }

  /* ================================================================ the certificate in use (admins) */
  const certOwn = () => !!(network && network.certificate && network.certificate.own);
  function certTile() {
    const c = (network && network.certificate) || null;
    const up = network && network.upload_address;
    const val = (t) => h('code', { class: 'set-mono cert-val' }, breakable(t));
    return tile({ w: 2, title: 'Certificate', icon: 'shield', id: 'certificate', side: chip(certOwn() ? 'Your own' : 'Made by the server', { tone: certOwn() ? 'sky' : 'muted' }) },
      c || up ? h('dl', { class: 'facts cert-facts' },
        c && c.sha256 ? h('div', { class: 'fact' }, h('dt', null, 'SHA-256'), h('dd', null, val(c.sha256), copyButton(() => c.sha256))) : null,
        c && c.names && c.names.length ? h('div', { class: 'fact', cover: 'docs.env.web.virtual-host' }, h('dt', null, 'Names in it'), h('dd', null, h('ul', { class: 'cert-names' }, c.names.map((n) => h('li', null, h('code', { class: 'set-mono' }, n)))),
          btn('Change names', { kind: 'ghost', icon: 'edit', cls: 'btn-sm names-btn', onclick: changeNames }))) : null,
        up ? h('div', { class: 'fact' }, h('dt', null, 'CAN Bridge upload address'), h('dd', null, val(up), copyButton(() => up))) : null)
        : said('info', 'The server did not say which certificate it runs with.'),
      c && c.sha256 ? said('info', 'The CAN Bridge app needs this SHA-256 and the upload address in its upload settings.') : null,
      certOwn()
        ? note('This is your own certificate (the cert.pem in data/tls), so the server never replaces it. To change it, put the new cert.pem and key.pem there and start the container again. For a certificate the server makes instead, delete data/tls/cert.pem and start it again.')
        : h('div', { class: 'form-actions' }, btn('Make a new certificate', { kind: 'soft', icon: 'refresh', onclick: makeCertificate })));
  }
  async function makeCertificate() {
    const names = ['localhost', ...(settings.host_names || []).filter((n) => n.toLowerCase() !== 'localhost')];
    const out = await askPassword(ctx, {
      title: 'Make a new certificate', yes: 'Make a new certificate', danger: true, hint: 'It is asked again because every CAN Bridge must be told the new SHA-256.',
      text: h('div', { class: 'sheet-text' },
        h('p', null, 'The server makes a new certificate and uses it at once on both ports, without a restart. Before you go on:'),
        h('ul', { class: 'set-list' },
          h('li', null, h('b', null, 'The SHA-256 changes. '), 'The CAN Bridge app’s uploads are refused until you enter the new SHA-256 in its upload settings.'),
          h('li', null, h('b', null, 'Browsers ask again '), 'to accept the new certificate, on every device that opens these pages.'),
          h('li', null, h('b', null, 'The names in it: '), names.map((n, i) => [i ? ', ' : '', h('code', { class: 'set-mono' }, n)]), '. Change them first with Change names, if one is missing.'))),
      run: (creds) => ctx.api.post('network/certificate', creds).catch((e) => {
        if (e && e.code === 'own_certificate') throw Object.assign(new Error('The server runs with your own certificate, so it makes no new one.'), { fields: null });
        if (e && (e.code === 'setup_busy' || e.code === 'move_under_way' || e.code === 'moved')) throw Object.assign(new Error(e.code === 'moved' ? 'The data moved to another server, so this one makes no new certificate.' : 'Not now: a move is under way. The new server pins this certificate’s SHA-256 until it is done.'), { fields: null });
        throw e;
      }),
    });
    if (!out || !ctx.alive()) return;
    try { network = await ctx.api.get('network'); } catch { network = { ...(network || {}), certificate: { names: out.names, sha256: out.sha256 }, upload_address: out.upload_address }; }
    if (!ctx.alive()) return;
    draw();
    await openSheet({
      title: 'The new certificate is in use',
      body: [h('p', { class: 'sheet-text' }, 'Enter this SHA-256 in each CAN Bridge app’s upload settings now: until then it refuses this server.'),
        h('div', { class: 'set-key' }, h('code', { class: 'set-mono cert-val cert-big' }, breakable(out.sha256)), copyButton(() => out.sha256, { kind: 'primary' }),
          h('p', { class: 'hint' }, 'Reload this page to accept the new certificate in this browser.'))],
      actions: [{ label: 'Done', value: true, kind: 'primary' }],
    });
  }

  /* ================================================================ how access works */
  function accessTile() {
    const nAdmin = accounts.filter((a) => a.role === 'admin').length;
    return tile({ w: 2, title: 'How access works', icon: 'lock', cover: 'web.shell.access-model', side: chip(ROLE[me.role] || me.role || '—', { tone: me.role === 'admin' ? 'coral' : 'muted' }) },
      h('p', { class: 'lede' }, `You are ${me.name || 'signed in'}, and see ${me.cars === 'all' ? 'every car' : `${formatNumber(Array.isArray(me.cars) ? me.cars.length : 0)} ${Array.isArray(me.cars) && me.cars.length === 1 ? 'car' : 'cars'}`}.`),
      admin ? h('p', { class: 'note' }, `${formatNumber(accounts.length)} accounts: ${formatNumber(nAdmin)} admin, ${formatNumber(accounts.length - nAdmin)} watch only, ${formatNumber(accounts.filter((a) => a.two_step_required).length)} with two-step sign-in.`) : null,
      group(null, [
        infoRow({ icon: 'lock', name: 'Nothing without signing in', sub: 'Every page and every API call needs a session.' }),
        infoRow({ icon: 'person', name: 'Two roles', sub: `Admin: ${ROLE_TEXT.admin} Watch only: ${ROLE_TEXT.watch}` }),
        infoRow({ icon: 'car', name: 'A car list per account', sub: 'A car not on an account’s list looks exactly like a car that does not exist.' }),
        infoRow({ icon: 'shield', name: 'Two-step sign-in and slow guessing', sub: 'A six-digit code after the password can be required; wrong passwords slow sign-in down.' }),
        infoRow({ icon: 'plug', name: 'Cars never sign in', sub: ['A CAN Bridge uploads with its car’s upload token, made in ', h('a', { href: state.buildUrl('settings-cars'), class: 'place-link' }, 'Settings: Cars'), '.'] }),
        infoRow({ icon: 'book', name: 'Every change is recorded', sub: admin ? 'The audit log below says who changed what, and when.' : 'Admins can read who changed what, and when.' }),
      ]));
  }

  /* ================================================================ moving (admins) */
  let move = null;      // GET /system/move
  let shown = null;     // { id, text }: the code made on this page, shown until it is used or cancelled
  let exp = null;       // the newest ExportFile
  let fetchOnReady = null;
  let moveTimer = 0;
  ctx.onCleanup(() => clearTimeout(moveTimer));
  const moveBox = h('div', { class: 'contents' });
  const myAddress = (() => { try { const u = new URL(network && network.upload_address); return u.port && u.port !== '10100' ? u.host : u.hostname; } catch { return location.hostname; } })();

  async function readMove() {
    clearTimeout(moveTimer);
    try { move = await ctx.api.get('system/move'); } catch (e) { if (api.isAbort(e)) return; move = move || { state: 'none' }; }
    if (ctx.alive()) drawMove();
  }
  let expErr = null;    // the last read of the export failed (the export goes on on the server)
  // the newest export file: read at the start and after a reconnect, then its `job` events (one made elsewhere
  // too, when none is shown)
  let expLive = null;
  function followExport() {
    let first;
    const ready = new Promise((res) => { first = res; });
    expLive = follow(ctx.api, {
      type: 'job', match: (d) => jobOf('export')(d) && (!exp || d.id === exp.id), pick: (d) => d.status,
      load: async () => ((await ctx.api.get('system/exports')).items || [])[0] || null,
      pollMs: () => exportPollMs(exp),
      onValue: (e) => { exp = e; expErr = null; if (ctx.alive() && moveBox.firstChild) drawMove(); first(); },
      onError: (e) => { expErr = e; if (ctx.alive() && moveBox.firstChild) drawMove(); first(); },
    });
    return ready;
  }
  function schedule() {
    clearTimeout(moveTimer);
    const s = move && move.state;
    const ms = s === 'copying' || s === 'paused' || s === 'moved' ? 1000 : s === 'offered' ? 3000 : 0;
    if (ms) moveTimer = setTimeout(readMove, ms);
  }

  async function makeCode() {
    const anywhere = tick('From anywhere. Leave it off when both servers are on your home network.', { name: 'anywhere' });
    const out = await askPassword(ctx, {
      title: 'Make a move code', yes: 'Make the code', extra: [anywhere],
      text: 'The code lets one new server copy everything here: logs, accounts, cars, upload tokens and the HTTPS key. It works once, for 24 hours; a code still waiting stops working.',
      run: async (creds) => {
        const bodyOf = (replace) => ({ ...creds, anywhere: anywhere.input.checked, replace: replace || undefined });
        try { return await ctx.api.post('system/move', bodyOf(false)); } catch (e) {
          if (e && e.code === 'move_under_way') {
            if (!await confirmSheet({ title: 'A move is under way. End it?', text: `${(move && move.peer && move.peer.address) || 'A new server'} is copying this server now. A new code ends that copy there. This server’s data is not changed.`, yes: 'End it and make a new code', danger: true })) return false;
            return ctx.api.post('system/move', bodyOf(true));
          }
          if (e && e.code === 'moved') throw new Error('This server has moved already. Undo the move first to move it again.');
          throw e;
        }
      },
    });
    if (!out) return;
    if (out.move_code && out.code) shown = { id: out.code.id, text: out.move_code };
    move = out;
    ctx.toast('Move code made. It is shown this once.');
    drawMove();
  }
  async function cancelMove() {
    const copying = move && (move.state === 'copying' || move.state === 'paused');
    if (!await confirmSheet({ title: copying ? 'Cancel the move?' : 'Cancel the move code?', danger: true, yes: copying ? 'Cancel the move' : 'Cancel the code',
      text: copying ? 'The new server stops copying and keeps what it has; a new code from this server lets it carry on. This server goes on as before.' : 'The code stops working at once. This server goes on as before.' })) return;
    try {
      move = await ctx.api.del('system/move');
      shown = null;
      ctx.toast(move.cancelled === 'ended' ? 'The move was cancelled. This server goes on as before.' : move.cancelled === 'revoked' ? 'The move code was cancelled.' : 'There was nothing to cancel.');
    } catch (e) { ctx.toast(e && e.code === 'finished' ? 'Too late to cancel: the move has finished. Undo it instead.' : notSaved(e), { kind: 'error' }); await readMove(); return; }
    drawMove();
  }
  async function undoMove() {
    const sure = tick(`I have stopped the new server${move && move.moved && move.moved.to ? ` at ${move.moved.to}` : ''}.`, { name: 'stopped' });
    const out = await askPassword(ctx, {
      title: 'Undo the move?', yes: 'Undo the move', danger: true, extra: [sure], hint: 'It is asked again because this changes where the uploads go.',
      text: 'This server takes uploads and changes again at once, with the data it had when it moved. Stop the new server first: if both take uploads, they drift apart.',
      run: async (creds) => {
        if (!sure.input.checked) throw new Error('Stop the new server first, then tick the box.');
        try { return await ctx.api.post('system/move/undo', { ...creds, confirm: true }); } catch (e) { if (e && e.code === 'not_moved') { await readMove(); return null; } throw e; }
      },
    });
    if (!out) return;
    move = out; shown = null;
    ctx.toast('The move was undone. This server takes uploads again.');
    drawMove();
  }
  async function makeExport() {
    const out = await askPassword(ctx, {
      title: 'Make an export file', yes: 'Make the file',
      text: 'One file with everything on this server, made in three steps: it counts what goes in, waits for the writes under way, then takes a snapshot (uploads wait for that step; the CAN Bridge sends them again). You can close the page meanwhile. It opens only with this server’s recovery key. A file made before is replaced.',
      run: async (creds) => { try { return await ctx.api.post('system/exports', creds); } catch (e) { if (e && e.code === 'export_preparing') return ((await ctx.api.get('system/exports')).items || [])[0] || null; throw e; } },
    });
    if (!out) return;
    exp = out; fetchOnReady = out.id;
    drawMove();
    expLive?.refresh();
  }
  async function cancelExport() {
    if (!exp || !await confirmSheet({ title: 'Stop making the export file?', text: 'The server stops and keeps nothing of it. Uploads go on. You can make a new one at any time.', yes: 'Stop making it', danger: true })) return;
    try { await ctx.api.del(api.apiPath`system/exports/${exp.id}`); ctx.toast('The export file was cancelled.'); } catch (e) { if (!(e && e.status === 404)) { ctx.toast(notSaved(e), { kind: 'error' }); return; } }
    exp = null; fetchOnReady = null;
    drawMove();
  }
  async function deleteExport() {
    if (!await confirmSheet({ title: 'Delete the export file?', text: 'The file is removed from this server. A copy you downloaded is not touched.', yes: 'Delete', danger: true })) return;
    try { await ctx.api.del(api.apiPath`system/exports/${exp.id}`); ctx.toast('Export file deleted.'); } catch (e) { if (!(e && e.status === 404)) { ctx.toast(notSaved(e), { kind: 'error' }); return; } }
    exp = null;
    drawMove();
  }
  const problemBox = (p) => { const w = PROBLEM[p.code] || [p.message || 'The move waits.', '']; return callout('warn', w[0], w[1] ? h('p', null, w[1]) : null, p.message ? h('p', { class: 'muted' }, 'The server says: ', p.message) : null); };

  function codeTile() {
    const m = move || { state: 'none' };
    const s = m.state;
    if (shown && (s !== 'offered' || !m.code || m.code.id !== shown.id)) shown = null;
    const peer = m.peer;
    const steps = [
      ['make', 'Make a move code', 'Your password again. The code works once, for 24 hours.'],
      ['give', 'Type it on the new server', 'On the new server’s first page, choose “Move here from another server” and type this address and the code.'],
      ['copy', 'The new server copies', 'This server keeps taking uploads meanwhile, so nothing is missed. Keep both on.'],
      ['switch', 'It switches over', 'The last round: uploads wait a few seconds, then the new server takes over by itself.'],
      ['done', 'Turn this server off', 'The new server has taken over.'],
    ];
    const cur = s === 'offered' ? 'give' : s === 'copying' || s === 'paused' ? (s === 'paused' ? 'switch' : 'copy') : s === 'moved' ? 'switch' : s === 'done' ? 'done' : 'make';
    let out;
    if (cur === 'make') {
      out = [
        s === 'expired' ? callout('info', 'The last move code expired unused.', h('p', null, `Made ${when(m.code && m.code.made_ms)}. Make a new one when the new server is ready.`)) : null,
        m.last_cancelled ? callout('info', 'A move stopped here can carry on.', h('p', null, `It was ${m.last_cancelled.why || 'cancelled'} ${when(m.last_cancelled.at_ms)}. Make a new code and type it on the same new server with this address: it keeps what it copied and goes on from there.`)) : null,
        m.last_undone_ms ? h('p', { class: 'note' }, `The last move was undone ${when(m.last_undone_ms)}.`) : null,
        h('div', { class: 'form-actions' }, btn('Make a move code', { kind: 'primary', icon: 'key', onclick: makeCode })),
      ];
    } else if (cur === 'give') {
      const reach = m.code && m.code.anywhere ? 'from anywhere' : 'from your home network only';
      out = [
        shown ? [bigCode(shown.text, { label: 'Move code' }), said('warn', `Shown only now: copy it. It works once, until ${when(m.code && m.code.expires_ms)}, ${reach}.`)]
          : callout('info', 'A move code is waiting.', h('p', null, `Made ${when(m.code && m.code.made_ms)}; it works until ${when(m.code && m.code.expires_ms)}, ${reach}. The code is shown only when it is made: if you lost it, make a new one.`)),
        m.code && m.code.wrong_tries ? callout('warn', `Someone typed a wrong code ${m.code.wrong_tries} times.`, h('p', null, 'If that was not you, cancel the code and make a new one.')) : null,
        h('div', { class: 'set-line' }, h('span', { class: 'note' }, 'This server’s address:'), h('code', { class: 'set-mono' }, myAddress), copyButton(() => myAddress)),
        h('div', { class: 'form-actions' }, btn('Cancel the code', { kind: 'danger', icon: 'close', onclick: cancelMove }), btn('Make a new code', { kind: 'ghost', icon: 'refresh', onclick: makeCode })),
      ];
    } else if (cur === 'copy' || cur === 'switch') {
      out = [
        h('p', { class: 'note' }, `${(peer && peer.address) || 'A new server'} ${s === 'moved' ? 'has the data and is switching over. Keep this server on until it says it took over; it takes no uploads or changes now.' : 'is copying this server. Keep this server on.'}`, peer && peer.last_seen_ms ? [' Last heard ', ago(peer.last_seen_ms), '.'] : null),
        m.progress ? progressBar(m.progress, { label: 'Copied to the new server' }) : null,
        m.problem ? problemBox(m.problem) : null,
        s !== 'moved' ? h('div', { class: 'form-actions' }, btn('Cancel the move', { kind: 'danger', icon: 'close', onclick: cancelMove })) : null,
      ];
    } else {
      const to = (m.moved && m.moved.to) || (peer && peer.address) || 'the new server';
      out = [
        callout('ok', `This server has moved to ${to}.`, h('p', null, `It took over ${when(m.moved && (m.moved.switched_ms || m.moved.at_ms))}. This server takes no more uploads or changes; reading still works. You can turn it off.`),
          h('p', null, 'Each CAN Bridge keeps its token and certificate SHA-256. If the new server has another address, change only the server address in the app.')),
        h('div', { class: 'form-actions' }, btn('Undo the move', { kind: 'soft', icon: 'reset', onclick: undoMove }), h('span', { class: 'note' }, 'Brings the data back here. Stop the new server first.')),
      ];
    }
    const skipped = (m.skipped || []).length && s !== 'none' ? said('warn', `${m.skipped.length} ${m.skipped.length === 1 ? 'file' : 'files'} this server cannot read stay behind: `, m.skipped.slice(0, 5).map((x) => x.path).join(', ')) : null;
    return tile({ w: 2, title: 'With a move code', icon: 'move', side: chip('Usual', { tone: 'coral' }) },
      note('When both servers are on at once and the new one can reach this one. This server keeps taking uploads while the new one copies.'),
      stepCards(steps, cur, { problem: !!m.problem && (cur === 'copy' || cur === 'switch'), output: h('div', { class: 'set-steps' }, out) }),
      skipped);
  }

  // The export file's status: one card with the same rows in every state (none, making, stalled, failed, ready), so
  // the tile keeps its size as the export goes on. The words come from shared/exportjob.js.
  function exportTile() {
    const e = exp;
    const m = exportModel(e);
    const ready = m.kind === 'ready';
    if (ready && fetchOnReady === e.id && e.url) { fetchOnReady = null; downloadUrl(e.url); ctx.toast(`The download of ${e.file_name} has started.`); }
    const tone = m.tone === 'error' ? 'bad' : m.tone === 'warn' ? 'warn' : m.tone === 'ok' ? 'ok' : 'info';
    const fill = h('span', { class: 'mv-bar-fill' });
    fill.style.setProperty('--w', m.percent == null ? '0' : String(m.percent / 100));
    let l1 = m.amount && m.files ? `${m.amount} · ${m.files}` : m.amount || m.files;
    let l2 = m.speed;
    if (ready) { l1 = h('span', { class: 'set-mono' }, e.file_name); l2 = `Kept until ${when(e.expires_ms)} · ${e.sent_bytes ? `${bytes(e.sent_bytes)} downloaded so far` : 'not downloaded yet'}`; }
    else if (m.kind === 'failed') { l1 = `Code: ${m.problem.code}`; l2 = ''; }
    let say = null;
    if (expErr) say = [h('b', null, 'This page lost touch with the server. '), 'It keeps trying; the export goes on there.'];
    else if (m.warn) say = m.warn.text;
    else if (m.problem) say = [h('b', null, 'What to do: '), m.problem.todo];
    else if (m.sha256) say = [h('b', null, 'SHA-256 '), h('span', { class: 'set-mono xp-sha' }, m.sha256)];
    else if (m.checksum) say = m.checksum;
    else if (m.kind === 'busy') say = 'You can close this page: the server goes on, and shows it here again when you come back.';
    else say = 'Seconds to minutes, most of it for the database. Uploads wait only for the last step.';
    const acts = m.kind === 'busy' ? [btn('Cancel', { kind: 'soft', icon: 'close', onclick: cancelExport })]
      : ready ? [linkBtn('Download', e.url, { kind: 'primary', icon: 'download' }), btn('Make new', { kind: 'soft', icon: 'refresh', onclick: makeExport }), btn('Delete', { kind: 'ghost', icon: 'trash', onclick: deleteExport })]
        : [btn(m.kind === 'failed' ? 'Make a new one' : 'Make an export file', { kind: 'primary', icon: 'download', onclick: makeExport })];
    const panel = h('div', { class: ['xp', `is-${tone}`], role: 'group', 'aria-label': 'Export file status' },
      h('div', { class: 'xp-top' },
        h('div', { class: 'xp-titles' }, h('p', { class: 'xp-title' }, m.head), h('p', { class: 'xp-sub' }, m.sub)),
        h('span', { class: 'xp-pct' }, m.percentText)),
      h('ol', { class: 'xp-steps', 'aria-label': 'Steps of making the file' }, m.steps.map((st) =>
        h('li', { class: ['xp-step', `is-${st.state}`], 'aria-current': st.state === 'now' ? 'step' : null },
          h('span', { class: 'xp-mark', 'aria-hidden': 'true' }, st.state === 'done' ? icon('check') : String(st.n)), st.label))),
      h('div', { class: ['mv-bar', 'xp-bar', `is-${tone}`], role: 'progressbar', 'aria-label': 'Export file', 'aria-valuemin': '0', 'aria-valuemax': '100',
        'aria-valuenow': m.percent == null ? null : String(Math.round(m.percent)), 'aria-valuetext': m.percentText || m.head }, fill),
      h('p', { class: 'xp-line' }, l1),
      h('p', { class: 'xp-line' }, l2),
      h('div', { class: ['xp-say', `is-${tone}`] }, h('p', null, say)),
      h('div', { class: 'form-actions xp-acts' }, acts));
    return tile({ w: 2, title: 'With an export file', icon: 'file' },
      note('When the servers cannot reach each other, this one will be off, or you want a copy to keep. Without the recovery key the file is useless to anyone who takes it.'),
      panel,
      note('On the new server: its first page, “Restore from an export file”, this file and this server’s recovery key. A download that stops carries on.'));
  }
  // only the texts that changed are written (an export's job event can come twice a second)
  function drawMove() {
    morphChildren(moveBox, [
      sectionHead('Move to another server', 'No shell, no commands. Everything goes along: logs, uploads still arriving, accounts, cars, tokens and the HTTPS key. A move never changes this server’s data, so stopping half way is always safe.'),
      codeTile(), exportTile()]);
    schedule();
  }

  /* ================================================================ storage and compaction (admins) */
  // Two half tiles side by side: Storage (on disk, free, saved by compaction, the last pass, how compaction stands
  // with what it does on hover, Compact now and the CSV) and the space by kind (shared/storage.js): one row per
  // group with its size, a bar and its share; a group opens to its categories by short name, at most five rows
  // tall (the CSV has every category with the server's own label). A pass (also one the server started by itself)
  // follows its `job` events and the free space the `storage` events; when it ends the detail is read once and a
  // note says what was freed.
  let store = null;       // GET /system/storage/detail
  let storeErr = null;    // why the last read failed
  const storeBox = h('div', { class: 'contents' });
  const storeOpen = new Set();   // the groups opened (all closed at first, so the two tiles are about as tall)
  const LOSSLESS = 'Compaction packs data that is no longer written, losslessly: every value stays readable exactly, and the logs’ signed originals are kept byte for byte.';
  // the pass: GET the detail at the start and after a reconnect, then the `job` events; a pass that ended reads the
  // detail once (what it packed changed the groups); only while the live stream is down, the detail every 2 s
  let storeLive = null;
  let fresh = null;   // the detail of the last read, put in with its status
  function setCompaction(c, how) {
    const ended = store?.compaction?.status === 'running' && c?.status !== 'running';
    if (ended && how === 'event') { storeLive.refresh(); return; }
    if (fresh) { store = fresh; fresh = null; }
    store = { ...(store || {}), compaction: c };
    storeErr = null;
    if (ended && c) {
      if (c.status === 'failed') ctx.toast(`Compaction failed: ${c.error || 'the server gave no reason.'}`, { kind: 'error' });
      else ctx.toast(c.last_saved_bytes ? `Compaction is done: ${bytes(c.last_saved_bytes)} freed on disk.` : 'Compaction is done: nothing was left to pack.');
    }
    if (storeBox.firstChild) drawStorage();
  }
  async function readStorage() {
    try { store = await ctx.api.get('system/storage/detail'); storeErr = null; } catch (e) { if (api.isAbort(e)) return; storeErr = e; }
  }
  function followStorage() {
    let firstRead = true;
    storeLive = follow(ctx.api, {
      type: 'job', match: jobOf('compaction'), pick: (d) => laterCompaction(store?.compaction, d.status), // (a failed pass keeps its reason)
      load: async () => {
        // the page's own read of the detail stands for the first one
        if (firstRead && store) { firstRead = false; return store.compaction || null; }
        firstRead = false;
        fresh = await ctx.api.get('system/storage/detail');
        return fresh.compaction || null;
      },
      pollMs: (c) => (c?.status === 'running' ? 2000 : null),
      onValue: (c, how) => { if (ctx.alive()) setCompaction(c, how); },
      onError: (e) => { storeErr = e; if (ctx.alive() && storeBox.firstChild) drawStorage(); },
    });
    // the free space on disk, from the `storage` events (no read of their own)
    follow(ctx.api, {
      type: 'storage',
      onValue: (st) => {
        if (!ctx.alive() || !st || st.free_bytes === store?.free_bytes) return;
        store = { ...(store || {}), free_bytes: st.free_bytes ?? null };
        if (storeBox.firstChild) drawStorage();
      },
    });
  }
  async function compactNow() {
    try {
      const r = await ctx.api.post('system/storage/compact');
      if (!ctx.alive()) return;
      store = { ...(store || {}), compaction: r.compaction };
      storeErr = null;
    } catch (e) { if (api.isAbort(e) || !ctx.alive()) return; ctx.toast(`Compaction not started: ${(e && e.message) || 'the server did not answer'}`, { kind: 'error' }); }
    drawStorage();
    storeLive?.refresh();
  }
  // The compaction tile is built once and changed in place on every read (2 s while a pass runs), so its
  // ring keeps turning and nothing in it moves. The progress row has the same room idle or running: a line
  // of words with the ring, a bar, a line of figures and a warning line, each cut with an ellipsis (the whole
  // text in its title) rather than wrapped (two lines of figures on a phone).
  const cmp = {
    chip: h('span'), disk: h('span'), free: h('span'), saved: h('span'), last: h('span'),
    spin: h('span', { class: 'spin', 'aria-hidden': 'true' }), head: h('span', { class: 'cmp-head-t' }), count: h('span', { class: 'cmp-count' }),
    fill: h('span', { class: 'mv-bar-fill' }), facts: h('p', { class: 'cmp-facts' }), warn: h('p', { class: 'cmp-warn' }),
    go: btn('Compact now', { kind: 'primary', icon: 'refresh', onclick: () => compactNow() }),
  };
  cmp.bar = h('span', { class: 'mv-bar cmp-bar', role: 'progressbar', 'aria-label': 'Compaction progress', 'aria-valuemin': '0', 'aria-valuemax': '100' }, cmp.fill);
  const tip = h('span', { class: 'st-tip', title: LOSSLESS, 'aria-label': LOSSLESS, role: 'img' }, icon('info'));
  cmp.row = h('div', { class: 'cmp', 'aria-live': 'polite' }, h('p', { class: 'cmp-head' }, cmp.spin, cmp.head, cmp.count, tip), cmp.bar, cmp.facts, cmp.warn);
  const num = (k, v, title) => [h('span', { class: 'hero-k', title }, k), h('span', { class: 'st-num', title }, v)];
  const cmpTile = tile({ w: 2, title: 'Compaction', icon: 'refresh', id: 'storage', side: cmp.chip },
    h('div', { class: 'tile-nums st-nums' },
      h('div', null, num('On disk', cmp.disk, 'Everything on the right, as it lies on disk.')),
      h('div', null, num('Free', cmp.free, 'Room left on the disk for new data.')),
      h('div', null, num('Saved', cmp.saved, 'What compaction has freed in all.')),
      h('div', null, num('Last pass', cmp.last, null))),
    cmp.row,
    h('div', { class: 'form-actions' }, cmp.go,
      btn('Download CSV', { kind: 'ghost', icon: 'download', onclick: () => downloadCsv('sentry-mate-storage-on-disk.csv', (store && store.categories) || [], [
        { label: 'What', value: (x) => x.label }, { label: 'Kind', value: (x) => groupWord(x.group) }, { label: 'Key', value: (x) => x.key },
        { label: 'Files', value: (x) => x.files }, { label: 'Size (bytes)', value: (x) => x.bytes }, { label: 'On disk (bytes)', value: (x) => x.disk_bytes }]) })));
  const setLine = (el, ...kids) => { morphChildren(el, kids); setAttr(el, 'title', el.textContent.trim()); };
  function compactionTile() {
    const c = (store && store.compaction) || null;
    const running = !!c && c.status === 'running';
    const failed = !!c && c.status === 'failed';
    const p = compactionProgress(c, { now: api.now(), bytes });
    morphChildren(cmp.chip, chip(running ? 'Packing' : failed ? 'Failed' : c ? 'Idle' : 'Not known', { tone: running ? 'sky' : failed ? 'sun' : c ? 'teal' : 'muted' }));
    setText(cmp.disk, bytes(store ? store.total_disk_bytes : null));
    setText(cmp.free, store && store.free_bytes != null ? bytes(store.free_bytes) : 'Not known');
    setText(cmp.saved, bytes(c ? c.saved_bytes : null));
    setText(cmp.last, !c || c.finished_ms == null ? 'Not yet' : c.last_saved_bytes ? `${bytes(c.last_saved_bytes)} freed` : 'Nothing left to pack');
    setAttr(cmp.last, 'title', c && c.finished_ms != null ? `Finished ${when(c.finished_ms)}.` : 'No pass has finished yet.');
    if (cmp.go.disabled !== (running || !c)) cmp.go.disabled = running || !c;
    cmp.row.classList.toggle('is-running', running);
    cmp.row.classList.toggle('is-failed', failed);
    cmp.spin.style.visibility = running ? '' : 'hidden';
    cmp.bar.style.visibility = running ? '' : 'hidden';
    cmp.bar.classList.toggle('is-stalled', p.stalled);
    const w = String((p.pct ?? 0) / 100);
    if (cmp.fill.style.getPropertyValue('--w') !== w) cmp.fill.style.setProperty('--w', w);
    setAttr(cmp.bar, 'aria-valuenow', p.pct == null ? null : String(Math.round(p.pct)));
    const pollWords = storeErr && store ? `The progress could not be read just now: ${(storeErr && storeErr.message) || 'the server did not answer'}` : null;
    if (running) {
      setLine(cmp.head, h('b', null, 'Packing…'), ' ', [p.stepOf, p.step].filter(Boolean).join(' · ') || 'getting ready');
      setLine(cmp.count, p.count);
      setLine(cmp.facts, [p.saved, p.rate, p.stalled ? null : p.eta, p.stalled ? null : p.last].filter(Boolean).join(' · '));
      setLine(cmp.warn, (p.stalled ? p.stall : p.errors || pollWords) || '');
    } else {
      setLine(cmp.head, ...(failed ? [h('b', null, 'The last pass failed: '), c.error || 'the server gave no reason.']
        : [!c ? 'The server did not say how compaction stands.'
          : c.automatic ? 'Compaction runs by itself when the server is idle. Compact now starts a pass at once.'
            : 'Compaction does not run by itself on this server. Compact now starts a pass.']));
      setLine(cmp.count, '');
      setLine(cmp.facts, '');
      setLine(cmp.warn, pollWords || '');
    }
    return cmpTile;
  }
  // the space by kind: a row per group, opening to its categories (five rows at most, the rest scroll)
  function spaceTile() {
    const groups = storageGroups(store && store.categories);
    return tile({ w: 2, title: 'Space by kind', icon: 'bars', side: chip(store && store.total_disk_bytes != null ? `${bytes(store.total_disk_bytes)} in all` : 'Not known', { tone: 'muted' }) },
      groups.length
        ? h('ul', { class: 'rows-list st-groups', 'aria-label': 'Space on disk by kind' }, groups.map((g) => {
          const open = storeOpen.has(g.group);
          const fill = h('span', { class: 'st-bar-fill' });
          fill.style.setProperty('--w', String(Math.max(g.share > 0 ? 0.01 : 0, g.share)));
          return h('li', { class: 'st-group' },
            h('button', { class: 'row st-group-head', type: 'button', 'aria-expanded': String(open), onclick: () => {
              if (storeOpen.has(g.group)) storeOpen.delete(g.group); else storeOpen.add(g.group);
              drawStorage();
            } },
            icon(open ? 'chevronDown' : 'chevronRight', 'row-go'),
            h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, g.name), h('span', { class: 'st-bar', 'aria-hidden': 'true' }, fill)),
            h('span', { class: 'row-side st-group-side' }, h('span', { class: 'set-value' }, bytes(g.disk_bytes)), h('span', { class: 'st-share' }, shareText(g.share)))),
            h('ul', { class: 'st-rows', tabindex: '0', 'aria-label': g.name, hidden: !open },
              g.rows.map((x) => h('li', { class: 'st-row', title: x.label }, h('span', { class: 'st-row-name' }, shortName(x)), h('span', { class: 'st-row-size' }, bytes(x.disk_bytes))))));
        }))
        : said('warn', 'The server did not say what takes the space', storeErr && storeErr.message ? `: ${storeErr.message}` : '.'));
  }
  const storeHead = sectionHead('Storage');
  let spaceEl = null;   // the space tile on the page: later ones are made like it in place
  function drawStorage() {
    spaceEl = spaceEl ? morph(spaceEl, spaceTile()) : spaceTile();
    const want = [storeHead, compactionTile(), spaceEl];
    if (want.some((el, i) => storeBox.children[i] !== el) || storeBox.children.length !== want.length) storeBox.replaceChildren(...want);
  }

  /* ================================================================ full rebuild (admins) */
  // every car's history made again from the stored data; the switch saves at once, as the other server settings
  let fullT = null;
  if (admin) {
    const auto = toggle({ label: 'Automatic full rebuild', sub: 'Rebuild automatically when a new server version decodes the stored logs differently: once for that version, 10 minutes after the start unless you start it now, put it off or pause it. Off: a notice offers it instead.',
      checked: !settings.full_rebuild || settings.full_rebuild.auto !== false,
      onchange: async (on) => {
        try {
          settings = await ctx.api.patch('settings', { version: settings.version, full_rebuild: { auto: on } });
          ctx.toast(on ? 'Saved. A new server version rebuilds what it decodes differently.' : 'Saved. A notice offers a full rebuild instead.');
        } catch (e) {
          if (e && e.status === 409) { settings = await ctx.api.get('settings').catch(() => settings); ctx.toast('Someone else changed the server settings; this page shows theirs now.', { kind: 'info' }); } else ctx.toast(notSaved(e), { kind: 'error' });
          if (ctx.alive()) auto.set(!settings.full_rebuild || settings.full_rebuild.auto !== false);
        }
      } });
    fullT = fullRebuildTile(ctx, { carName: (c) => carName.get(c) || c, after: group(null, [auto]) });
  }

  /* ================================================================ accounts (admins) */
  function carsPicker(current) {
    const all = toggle({ label: 'Every car, also cars that come later', checked: current === 'all' });
    const chosen = new Set(current === 'all' ? [] : current || []);
    const list = h('div', { class: 'rows-list' }, cars.map((c) => {
      const t = tick(c.name);
      t.input.checked = chosen.has(c.id);
      t.input.addEventListener('change', () => { if (t.input.checked) chosen.add(c.id); else chosen.delete(c.id); });
      return h('div', { class: 'row row-static' }, t);
    }));
    const box = h('div', { class: 'rows' }, list);
    const sync = () => { box.hidden = all.input.checked; };
    all.input.addEventListener('change', sync);
    sync();
    const el = h('div', { class: 'field' }, h('span', { class: 'label' }, 'Cars it sees'), h('div', { class: 'rows-list' }, all), box);
    el.value = () => (all.input.checked ? 'all' : [...chosen]);
    return el;
  }
  function editAccount(a) {
    const isNew = !a;
    openSheet({
      title: isNew ? 'Add an account' : `Edit ${a.name}`,
      body: (close) => {
        const name = field({ label: 'Name', name: 'name', value: a ? a.name : '', hint: 'What they type to sign in. 1 to 64 characters.', attrs: { maxlength: '64', autocomplete: 'off', 'data-autofocus': '' } });
        const pw = field({ label: isNew ? 'Password' : 'New password', name: 'password', type: 'password', hint: isNew ? 'Tell it to them in person.' : 'Leave empty to keep it.', attrs: { autocomplete: 'new-password' } });
        const role = segmented({ label: 'Role', name: 'role', value: a ? a.role : 'watch', options: [['watch', 'Watch only'], ['admin', 'Admin']] });
        let roleNow = a ? a.role : 'watch';
        role.addEventListener('click', () => { roleNow = role.querySelector('[aria-checked="true"]')?.dataset.value || roleNow; });
        role.addEventListener('keydown', () => setTimeout(() => { roleNow = role.querySelector('[aria-checked="true"]')?.dataset.value || roleNow; }));
        const two = toggle({ label: 'Require two-step sign-in', sub: 'A six-digit code from an authenticator app after the password. Good for every admin.', checked: !!(a && a.two_step_required) });
        const pick = carsPicker(a ? a.cars : []);
        const confirm = field({ label: 'Your own password', name: 'confirm_password', type: 'password', hint: 'Account changes ask for your password again.', attrs: { autocomplete: 'current-password' } });
        const code = me.two_step_required ? field({ label: 'Your two-step code', name: 'code', hint: 'From your authenticator app.', attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', maxlength: 8 } }) : null;
        const f = form({
          fields: [name, pw, h('div', { class: 'field' }, role, h('p', { class: 'hint' }, `Admin: ${ROLE_TEXT.admin} Watch only: ${ROLE_TEXT.watch}`)), pick, h('div', { class: 'rows-list' }, two), confirm, code].filter(Boolean),
          submitLabel: isNew ? 'Add account' : 'Save changes',
          onsubmit: async (v) => {
            const body = { name: (v.name || '').trim(), role: roleNow, cars: pick.value(), two_step_required: two.input.checked, confirm_password: v.confirm_password };
            if (v.password) {
              if (!(await passwordOk(body.name, v.password, pw.control))) return;
              body.password = v.password;
            }
            if (v.code) body.code = v.code;
            try {
              if (isNew) await ctx.api.post('accounts', body); else await ctx.api.patch(api.apiPath`accounts/${a.id}`, { version: a.version, ...body });
            } catch (e) {
              if (e && e.code === 'name_taken') throw Object.assign(new Error('Not saved.'), { fields: { name: 'That name is taken.' } });
              if (e && e.code === 'last_admin') throw new Error('This is the last admin. Make another account an admin first.');
              if (e && e.code === 'wrong_password') throw Object.assign(new Error('Not saved.'), { fields: { confirm_password: 'That is not your password.' } });
              throw e;
            }
            ctx.toast(isNew ? `${body.name} can sign in now.` : `${body.name} saved.`);
            close(true);
            accounts = await ctx.api.list('accounts', {}).all({ max: 2000 });
            if (ctx.alive()) draw();
          },
        });
        return h('div', { class: 'set-sheet-form' }, f);
      },
    });
  }
  async function removeAccount(a) {
    const r = await askPassword(ctx, { title: `Remove ${a.name}?`, yes: 'Remove the account', danger: true, hint: 'Account changes ask for it again.',
      text: `${a.name} can no longer sign in, and every session of theirs ends now. Their changes stay in the audit log.`,
      run: (creds) => ctx.api.send('DELETE', api.apiPath`accounts/${a.id}`, { confirm_password: creds.password, ...(creds.two_step ? { code: creds.two_step } : {}) }).then(() => true) });
    if (!r) return;
    ctx.toast(`${a.name} was removed.`);
    accounts = await ctx.api.list('accounts', {}).all({ max: 2000 });
    if (ctx.alive()) draw();
  }
  const carsText = (c) => (c === 'all' ? 'Every car' : !Array.isArray(c) || !c.length ? 'No cars' : c.map((x) => carName.get(x) || x).join(', '));
  function accountsTile() {
    return tile({ w: 2, title: 'Accounts', icon: 'person', side: btn('Add', { kind: 'primary', icon: 'plus', cls: 'btn-sm', onclick: () => editAccount(null) }) },
      group(null, [...accounts].sort((x, y) => x.name.localeCompare(y.name)).map((a) => infoRow({ icon: 'person', tone: a.role === 'admin' ? 'coral' : null, name: [a.name, a.id === me.id ? ' (you)' : ''],
        sub: `${ROLE[a.role] || a.role} · ${carsText(a.cars)} · ${a.two_step_required ? 'two-step sign-in' : 'password only'}`,
        side: [btn(`Edit ${a.name}`, { kind: 'soft', icon: 'edit', iconOnly: true, cls: 'btn-sm', onclick: () => editAccount(a) }),
          btn(`Remove ${a.name}`, { kind: 'ghost', icon: 'trash', iconOnly: true, cls: 'btn-sm', disabled: a.id === me.id, onclick: () => removeAccount(a) })] }))),
      note('Everyone who can sign in, with what they may see. There is always at least one admin.'));
  }

  /* ================================================================ sessions */
  function sessionsTile() {
    const end = async (s) => {
      if (s.this_one) { try { await ctx.api.del('session'); } catch { /* signed out either way */ } location.assign('/sign-in'); return; }
      try { await ctx.api.del(api.apiPath`me/sessions/${s.id}`); ctx.toast(`Signed out ${s.device || 'that browser'}.`); } catch (e) { ctx.toast(notSaved(e), { kind: 'error' }); }
      sessions = await ctx.api.list('me/sessions', {}).all({ max: 500 });
      if (ctx.alive()) draw();
    };
    const others = sessions.filter((s) => !s.this_one).length;
    const endOthers = async () => {
      if (!others) { ctx.toast('No other browser is signed in.', { kind: 'info' }); return; }
      if (!await confirmSheet({ title: 'Sign out everywhere else?', text: `${others} other ${others === 1 ? 'browser is' : 'browsers are'} signed out now. This browser stays signed in.`, yes: 'Sign out the others' })) return;
      try { await ctx.api.del('me/sessions'); ctx.toast('Signed out every other browser.'); } catch (e) { ctx.toast(notSaved(e), { kind: 'error' }); }
      sessions = await ctx.api.list('me/sessions', {}).all({ max: 500 });
      if (ctx.alive()) draw();
    };
    const list = [...sessions].sort((a, b) => (b.this_one - a.this_one) || (b.last_seen_ms - a.last_seen_ms));
    return tile({ w: 2, title: 'Your sessions', icon: 'signout', side: btn('Sign out others', { kind: 'soft', cls: 'btn-sm', onclick: endOthers }) },
      group(null, list.slice(0, 12).map((s) => infoRow({ icon: 'globe', tone: s.this_one ? 'teal' : null, name: [s.device || 'Browser', s.this_one ? ' (this one)' : ''], sub: [`Signed in ${when(s.created_ms)} · last used `, s.this_one ? 'now' : ago(s.last_seen_ms)],
        side: btn(s.this_one ? 'Sign out here' : 'Sign out', { kind: 'ghost', cls: 'btn-sm', onclick: () => end(s) }) }))),
      list.length > 12 ? note(`And ${list.length - 12} more. “Sign out others” ends them all.`) : null,
      note(admin ? 'End one you do not recognise, then give your account a new password under Accounts.' : 'End one you do not recognise, then ask an admin for a new password.'));
  }

  /* ================================================================ audit log (admins) */
  // Each entry in words (shared/audit.js): what was done (its code on hover), who, when, what happened and the
  // item it changed, linked where it still exists. Filters: what (a group of actions) and who. The list scrolls
  // in a box about five entries high (its height set from the start); ten entries first, the next ten as the
  // end of the box comes near, or with Load more. New entries go under the ones on screen, which stay as they are.
  let auditT = null;
  async function auditTile() {
    const trips = new Map((await ctx.api.list('trips', {}).all({ max: 2000 }).catch(() => [])).map((t) => [String(t.id), t]));
    const itemCtx = { carName: (c) => carName.get(c), accountName: (a) => (accounts.find((x) => x.id === a) || {}).name, trip: (t) => trips.get(String(t)) };
    const F = { group: '', who: '' };
    const params = () => ({ ...(F.group ? { group: F.group } : {}), ...(F.who ? { who: F.who } : {}) });
    const names = [...new Set(accounts.map((a) => a.name))].sort((a, b) => a.localeCompare(b));
    const what = field({ label: 'What', name: 'audit-group', type: 'select', value: '', options: [['', 'All'], ...AUDIT_GROUPS.map((g) => [g.id, g.label])] });
    const who = field({ label: 'Who', name: 'audit-who', type: 'select', value: '', options: [['', 'All'], ...names.map((n) => [n, n]), ['*server', 'The server']] });
    const itemEl = (e) => {
      const it = auditItem(e, itemCtx);
      if (!it) return null;
      if (!it.route || it.faint) return h('span', { class: 'au-faint' }, it.text);
      return h('a', { class: 'au-item', href: state.buildUrl(it.route[0], it.route[1] || {}) + (it.hash || '') }, it.text);
    };
    const rowOf = (e) => {
      const item = itemEl(e);
      return h('div', { class: 'row row-static au-row', role: 'listitem' },
        h('span', { class: 'row-text' },
          h('span', { class: 'row-name au-act', title: e.action }, actionLabel(e.action)),
          e.summary ? h('span', { class: 'row-sub au-sum' }, e.summary) : null,
          h('span', { class: 'row-sub au-meta' }, h('span', { class: 'au-when' }, when(e.at_ms)), item ? h('span', { class: 'au-it' }, h('span', { class: 'au-dot', 'aria-hidden': 'true' }, '·'), item) : null)),
        h('span', { class: 'row-side au-who' }, e.account || h('span', { class: 'au-faint' }, 'The server')));
    };
    const list = h('div', { class: 'rows-list au-list', role: 'list', 'aria-label': 'Audit log entries' });
    const status = h('div', { class: 'au-status' });
    let pager = null, gen = 0;
    // Load more under the box; as the end of the box comes near it loads too (core/bits.js listBox)
    const moreRow = moreButton(async () => {
      const g = gen;
      try {
        const got = await pager.more();
        if (g !== gen || !ctx.alive()) return false;
        list.append(...got.map(rowOf));
        status.replaceChildren(list.children.length ? '' : said('info', F.group || F.who ? 'No entry matches this what and who.' : 'Nothing recorded yet.'));
        list.hidden = !list.children.length;
      } catch (e) {
        if (g !== gen || !ctx.alive() || api.isAbort(e)) return false;
        status.replaceChildren(said('warn', `The log did not load: ${(e && e.message) || 'the server did not answer'}`));
        return true;
      } finally { if (g === gen) shown(); }
      return !pager.done;
    }, { text: 'Load more' });
    const box = listBox(ctx, { label: 'Audit log entries', cls: 'au-box', more: moreRow }, status, list);
    const csv = linkBtn('Download CSV', ctx.api.csvUrl('audit', params()), { kind: 'ghost', icon: 'download', cls: 'btn-sm' });
    csv.setAttribute('download', '');
    function shown() {
      moreRow.hidden = !pager || pager.done;
      csv.href = ctx.api.csvUrl('audit', params());
      csv.hidden = !list.children.length;
    }
    async function first() {
      gen++;
      pager = ctx.api.pager('audit', params(), { limit: 10 });
      const g = gen;
      const got = await pager.more().catch((e) => { if (g === gen && ctx.alive()) status.replaceChildren(said('warn', `The log did not load: ${(e && e.message) || 'the server did not answer'}`)); return null; });
      if (g !== gen || !ctx.alive() || !got) return;
      list.replaceChildren(...got.map(rowOf));
      box.top();   // new filters: the list starts at its top
      list.hidden = !got.length;
      status.replaceChildren(got.length ? '' : said('info', F.group || F.who ? 'No entry matches this what and who.' : 'Nothing recorded yet.'));
      shown();
      box.recheck();
    }
    const refilter = () => { F.group = what.control.value; F.who = who.control.value; first(); };
    what.control.addEventListener('change', refilter);
    who.control.addEventListener('change', refilter);
    await first();
    return tile({ w: 'full', title: 'Audit log', icon: 'book', side: csv, cls: 'au-tile' },
      note(`Every change and every sign-in, newest first. Times in ${tz.replace(/_/g, ' ')}. An item opens where it still exists.`),
      h('div', { class: 'au-filters' }, what, who),
      box.el, moreRow);
  }

  /* ================================================================ put together */
  function draw() {
    const tiles = [hostingTile(), updatesTile(), accessTile()];
    if (admin) tiles.push(certTile(), keyTile());
    tiles.push(sessionsTile());
    if (admin) tiles.push(accountsTile(), moveBox, storeBox, fullT.el, auditT);
    ctx.view.replaceChildren(...tiles.filter(Boolean));
  }
  if (admin) {
    await Promise.all([readMove(), followExport(), readStorage(), fullT.ready]);
    auditT = await auditTile();
    if (!ctx.alive()) return {};
    drawMove();
    drawStorage();
    followStorage();
  }
  draw();
  ctx.ready();
  return {};
}
