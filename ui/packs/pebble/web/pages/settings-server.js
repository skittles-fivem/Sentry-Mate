// Sentry Mate, "Pebble" UI pack: Settings: Server and access (/settings/server).
//
// Who can sign in, what they may see, how the server is reached, and moving it, as rounded tiles:
// - Everyone: how this address is reached (HTTPS, host name, port), how access works, and their own sessions.
// - Admins also: host names for the certificate, the ports, the certificate and the recovery key (show it again,
//   make a new one, say it is saved), the move to another server as numbered step cards (with a move code, or
//   with an export file: the current card holds the one clear button and what it printed), the accounts (an edit
//   sheet each), and the audit log.
// A move that stopped on the new server (a code cancelled here, say) carries on: a new code from this same server
// lets the new server go on with what it copied. Data: settings, network, notices, accounts, me/sessions, audit,
// system/move (+ /undo), system/exports, system/recovery-key/{show,new,saved}. Polls once a second while a move
// or an export file is being made (no event stream for these).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { formatDateTime, formatDate } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { tile, btn, linkBtn, chip, note, icon, ago } from '../core/ui.js';
import { openSheet, confirmSheet, passwordOk } from '../core/sheet.js';
import { field, form, segmented } from '../core/form.js';
import { toggle } from '../core/form.js';
import { checkRow, sectionHead, moreButton } from '../core/bits.js';
import { group, infoRow, said, notSaved, askPassword, keySheet, tick } from '../core/settings.js';
import { inOrder } from '../core/settings.js';
import { bytes, bigCode, copyButton, downloadUrl, stepCards, progressBar, callout, PROBLEM } from './move-parts.js';

export const id = 'settings-server';
export const shape = [[2, 'h2'], [2, 'h2'], [2, 'h3'], [2, 'h3']];
export const title = () => 'Server and access';

const ROLE = { admin: 'Admin', watch: 'Watch only' };
const ROLE_TEXT = {
  admin: 'Sees every car and every setting, and can change everything.',
  watch: 'Sees only the cars on its list, and changes only its own preferences and sessions.',
};
const PURPOSE = { web: 'Web pages and API', upload: 'CAN Bridge uploads' };
// audit actions this page names in words (the entry's own summary says the rest)
const ACTION = {
  'tesla-account-added': 'Tesla account added', 'tesla-account-removed': 'Tesla account removed', 'tesla-account-paused': 'Tesla account paused',
  'tesla-account-resumed': 'Tesla account resumed', 'tesla-account-changed': 'Tesla account changed', 'tesla-token-replaced': 'Tesla token replaced',
  'tesla-car-matched': 'Tesla cars matched', 'tesla-poll-changed': 'Tesla pace changed', 'tesla-sign-in-needed': 'Tesla stopped taking a sign-in',
  'tesla-account-error': 'Tesla account error', 'certificate-made': 'New certificate made',
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
    ctx.api.get('notices').then((r) => r.items || []).catch(() => []),
    ctx.api.list('cars', { sort: 'custom' }).all({ max: 500 }).catch(() => []),
    admin ? ctx.api.list('accounts', {}).all({ max: 2000 }) : [],
    ctx.api.list('me/sessions', {}).all({ max: 500 }),
  ]);
  if (!ctx.alive()) return {};
  const carName = new Map(cars.map((c) => [c.id, c.name]));

  /* ================================================================ how this server is reached */
  const here = { https: location.protocol === 'https:', host: location.hostname.toLowerCase(), port: Number(location.port || (location.protocol === 'https:' ? 443 : 80)) };
  const webIn = network && network.inbound ? network.inbound.find((x) => x.purpose === 'web') : null;
  const certNames = network && network.certificate && Array.isArray(network.certificate.names) ? network.certificate.names.map((n) => n.toLowerCase()) : null;
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
        webIn ? checkRow({ name: `Port ${here.port}`, sub: here.port === webIn.port ? 'The same port the server listens on.' : `The server listens on ${webIn.port} in its container; a port mapping such as ${here.port}:${webIn.port} joins the two.`, word: here.port === webIn.port ? 'Same' : 'Mapped', level: 'ok' })
          : checkRow({ name: `Port ${here.port}`, sub: 'An admin sees the ports the server listens on.', word: 'Yours', level: 'off' }),
        checkRow({ name: 'At the root of its address', sub: 'Sentry Mate always lives at / on its own name or port: behind a shared proxy, give it its own name rather than a path.', word: 'Yes', level: 'ok' }),
        checkRow({ name: 'Works from any address', sub: 'No list of allowed origins: the session cookie is never sent by other sites, and every change carries a header other sites cannot set.', word: 'Yes', level: 'ok' })));
  }

  /* ================================================================ host names and ports */
  let draft = [...(settings.host_names || [])];
  async function saveNames(list) {
    try {
      settings = await ctx.api.patch('settings', { version: settings.version, host_names: list });
      draft = [...settings.host_names];
      ctx.toast('Host names saved. They go into the next certificate the server makes.');
    } catch (e) {
      if (e && e.status === 409) { settings = await ctx.api.get('settings'); draft = [...settings.host_names]; ctx.toast('Someone else changed the server settings; this page shows theirs now.', { kind: 'info' }); } else ctx.toast(notSaved(e), { kind: 'error' });
    }
    if (ctx.alive()) draw();
  }
  function namesTile() {
    const input = field({ label: 'Add a name', name: 'host', hint: 'Such as sentry-mate.local, or an address such as 192.168.1.20. Up to 10 names.', attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: 'sentry-mate.local', disabled: !admin } });
    const err = h('p', { class: 'form-error', hidden: true, role: 'alert' });
    const add = () => {
      const v = input.control.value.trim().toLowerCase();
      err.hidden = true;
      if (!v) return;
      if (v.length > 253 || !/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/.test(v)) { err.textContent = 'A host name has letters, digits, hyphens and dots.'; err.hidden = false; return; }
      if (draft.length >= 10) { err.textContent = 'The certificate takes up to 10 names.'; err.hidden = false; return; }
      if (!draft.includes(v)) draft.push(v);
      draw();
    };
    input.control.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); add(); } });
    const changed = JSON.stringify(draft) !== JSON.stringify(settings.host_names || []);
    return tile({ w: 2, title: 'Host names for the certificate', icon: 'server', cover: 'docs.env.web.virtual-host' },
      note('The names people and devices use to reach this server. The server puts them in the certificate it makes, so browsers accept them. A change goes into the next certificate.'),
      group(null, draft.map((n, i) => infoRow({ icon: 'globe', name: h('span', { class: 'set-mono' }, n), sub: n === here.host ? 'This address' : null,
        side: admin ? btn(`Remove ${n}`, { kind: 'ghost', icon: 'close', iconOnly: true, cls: 'btn-sm', disabled: draft.length <= 1, onclick: () => { draft = draft.filter((_, j) => j !== i); draw(); } }) : null }))),
      admin ? [h('div', { class: 'form' }, input, err), h('div', { class: 'form-actions' }, btn('Add', { kind: 'soft', icon: 'plus', onclick: add }),
        btn('Save host names', { kind: 'primary', icon: 'check', disabled: !changed, onclick: () => saveNames(draft) }),
        changed ? btn('Undo changes', { kind: 'ghost', onclick: () => { draft = [...settings.host_names]; draw(); } }) : null)] : note('Only an admin can change them.'));
  }
  function portsTile() {
    const inb = (network && network.inbound) || [];
    const up = inb.find((x) => x.purpose === 'upload');
    return tile({ w: 2, title: 'Ports', icon: 'plug', cover: 'docs.env.web.port' },
      group(null, inb.length ? inb.map((p) => infoRow({ icon: p.purpose === 'upload' ? 'upload' : 'globe', name: PURPOSE[p.purpose] || p.purpose, sub: p.tls ? 'HTTPS with TLS 1.3' : 'No TLS', side: h('span', { class: 'set-value' }, String(p.port)) }))
        : [infoRow({ name: 'Your address', side: h('span', { class: 'set-value' }, String(here.port)) })]),
      note(network ? `Only HTTPS, no plain HTTP port.${up ? ` The upload port takes only CAN Bridge uploads with a car’s token.` : ''} To use another port on the host, change only the left number of the mapping in the compose or TrueNAS file (host:container), for example ${here.port === (webIn && webIn.port) ? 10443 : here.port}:${webIn ? webIn.port : 443}.` : 'An admin sees which ports the server listens on.'));
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
        c && c.names && c.names.length ? h('div', { class: 'fact' }, h('dt', null, 'Names in it'), h('dd', null, h('ul', { class: 'cert-names' }, c.names.map((n) => h('li', null, h('code', { class: 'set-mono' }, n)))))) : null,
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
          h('li', null, h('b', null, 'The names in it: '), names.map((n, i) => [i ? ', ' : '', h('code', { class: 'set-mono' }, n)]), '. Change them under Host names for the certificate first, if one is missing.'))),
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
  let moveTimer = 0, expTimer = 0;
  ctx.onCleanup(() => { clearTimeout(moveTimer); clearTimeout(expTimer); });
  const moveBox = h('div', { class: 'contents' });
  const myAddress = (() => { try { const u = new URL(network && network.upload_address); return u.port && u.port !== '10100' ? u.host : u.hostname; } catch { return location.hostname; } })();

  async function readMove() {
    clearTimeout(moveTimer);
    try { move = await ctx.api.get('system/move'); } catch (e) { if (api.isAbort(e)) return; move = move || { state: 'none' }; }
    if (ctx.alive()) drawMove();
  }
  async function readExport() {
    clearTimeout(expTimer);
    try {
      if (exp && exp.id) exp = await ctx.api.get(api.apiPath`system/exports/${exp.id}`);
      else exp = ((await ctx.api.get('system/exports')).items || [])[0] || null;
    } catch (e) { if (api.isAbort(e)) return; if (e && e.status === 404) exp = null; }
    if (ctx.alive()) drawMove();
  }
  function schedule() {
    clearTimeout(moveTimer);
    const s = move && move.state;
    const ms = s === 'copying' || s === 'paused' || s === 'moved' ? 1000 : s === 'offered' ? 3000 : 0;
    if (ms) moveTimer = setTimeout(readMove, ms);
    clearTimeout(expTimer);
    if (exp && exp.state === 'preparing') expTimer = setTimeout(readExport, 1000);
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
      text: 'One file with everything on this server, ready in a few seconds (uploads wait meanwhile; the CAN Bridge sends them again). It opens only with this server’s recovery key. A file made before is replaced.',
      run: async (creds) => { try { return await ctx.api.post('system/exports', creds); } catch (e) { if (e && e.code === 'export_preparing') return ((await ctx.api.get('system/exports')).items || [])[0] || null; throw e; } },
    });
    if (!out) return;
    exp = out; fetchOnReady = out.id;
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

  function exportTile() {
    const e = exp;
    const steps = [
      ['make', 'Make the export file', 'Your password again. Ready in a few seconds.'],
      ['ready', 'Download it', 'Take it to the new server, or keep it as a backup.'],
      ['restore', 'Restore it on the new server', 'On its first page, choose “Restore from an export file”, choose the file and type this server’s recovery key.'],
    ];
    let cur = 'make';
    let out;
    if (!e || e.state === 'expired' || e.state === 'failed') {
      out = [
        e && e.state === 'expired' ? callout('info', 'The last export file has expired.', h('p', null, `Made ${when(e.made_ms)}. Make a new one.`)) : null,
        e && e.state === 'failed' ? callout('bad', 'The export file could not be made.', h('p', null, e.error || 'Make a new one.')) : null,
        h('div', { class: 'form-actions' }, btn('Make an export file', { kind: 'primary', icon: 'download', onclick: makeExport })),
      ];
    } else if (e.state === 'preparing') {
      out = [callout('info', 'Making the export file…', h('p', null, 'A few seconds. The download starts by itself when it is ready.')), progressBar({ bytes_done: e.sent_bytes || 0 }, { label: 'Making the export file' })];
    } else {
      cur = 'ready';
      if (fetchOnReady === e.id && e.url) { fetchOnReady = null; downloadUrl(e.url); ctx.toast(`The download of ${e.file_name} has started.`); }
      out = [
        group(null, [infoRow({ icon: 'file', name: h('span', { class: 'set-mono' }, e.file_name), sub: [bytes(e.size), e.files != null ? `${formatNumber(e.files)} files` : null, `made ${when(e.made_ms)}`].filter(Boolean).join(' · ') }),
          infoRow({ icon: 'clock', name: 'Kept until', sub: 'A day after the last download. A download that stops carries on.', side: h('span', { class: 'set-value' }, when(e.expires_ms)) })]),
        h('div', { class: 'form-actions' }, linkBtn('Download', e.url, { kind: 'primary', icon: 'download' }), btn('Make a new one', { kind: 'soft', icon: 'refresh', onclick: makeExport }), btn('Delete it', { kind: 'ghost', icon: 'trash', onclick: deleteExport })),
      ];
    }
    return tile({ w: 2, title: 'With an export file', icon: 'file' },
      note('When the servers cannot reach each other, this one will be off, or you want a copy to keep. Without the recovery key the file is useless to anyone who takes it.'),
      stepCards(steps, cur, { output: h('div', { class: 'set-steps' }, out) }));
  }
  function drawMove() {
    moveBox.replaceChildren(
      sectionHead('Move to another server', 'No shell, no commands. Everything goes along: logs, uploads still arriving, accounts, cars, tokens and the HTTPS key. A move never changes this server’s data, so stopping half way is always safe.'),
      codeTile(), exportTile());
    schedule();
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
      run: (creds) => ctx.api.del(api.apiPath`accounts/${a.id}`, { headers: { 'X-Sentry-Mate-Password': creds.password, ...(creds.two_step ? { 'X-Sentry-Mate-Code': creds.two_step } : {}) } }).then(() => true) });
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
  let auditT = null;
  async function auditTile() {
    const pager = ctx.api.pager('audit', {}, { limit: 25 });
    await pager.more();
    const target = (t, car) => {
      const i = String(t || '').indexOf(':');
      const kind = i < 0 ? t : t.slice(0, i), tid = i < 0 ? '' : t.slice(i + 1);
      if (kind === 'car' && carName.has(tid)) return carName.get(tid);
      if (kind === 'account') return `account ${(accounts.find((a) => a.id === tid) || {}).name || tid}`;
      if (kind === 'tesla') return `Tesla account ${tid}`;
      if (kind === 'server') return tid === 'certificate' ? 'the certificate' : 'the server';
      if (car && carName.has(car)) return `${kind} of ${carName.get(car)}`;
      return kind || '';
    };
    const rowOf = (e) => h('div', { class: 'log-row', role: 'listitem' }, h('span', { class: 'log-k' }, `${when(e.at_ms)}  ${e.account || 'server'}  ${e.summary || ACTION[e.action] || e.action}`), h('span', { class: 'log-v' }, [ACTION[e.action] && e.summary ? ACTION[e.action] : null, target(e.target, e.car)].filter(Boolean).join(' · ')));
    const log = h('div', { class: 'log', role: 'list' }, pager.items.map(rowOf));
    const more = moreButton(async () => { const b = pager.items.length; await pager.more(); log.append(...pager.items.slice(b).map(rowOf)); return !pager.done; }, { text: 'Older entries' });
    more.querySelector('button').hidden = pager.done;
    more.classList.remove('w-full');
    return tile({ w: 'full', title: 'Audit log', icon: 'book' },
      note(`Every change and every sign-in, newest first. Times in ${tz.replace(/_/g, ' ')}.`),
      pager.items.length ? log : said('info', 'Nothing recorded yet.'), more);
  }

  /* ================================================================ put together */
  function draw() {
    const tiles = [hostingTile(), accessTile()];
    if (admin) tiles.push(namesTile(), certTile(), portsTile(), keyTile());
    tiles.push(sessionsTile());
    if (admin) tiles.push(accountsTile(), moveBox, auditT);
    ctx.view.replaceChildren(...tiles.filter(Boolean));
  }
  if (admin) {
    await Promise.all([readMove(), readExport()]);
    auditT = await auditTile();
    if (!ctx.alive()) return {};
    drawMove();
  }
  draw();
  ctx.ready();
  return {};
}
