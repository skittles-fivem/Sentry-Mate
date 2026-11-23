// settings/server: who can sign in and how the server is reached, and moving it.
// Everyone: the check of this address (https, host name, port), how access works, their own sessions.
// Admins also: host names as a 'key = value' line, ports, the certificate and the recovery key (show again,
// make new, say it is saved), moving to another server as numbered steps with a state tag and a run button
// each (a move code, or an export file; carry on a stopped move; undo a move), output in a mono box; the
// accounts, and the audit log.
// The move and the export file are read once a second while something runs (GET /system/move,
// GET /system/exports/{id}); every dangerous action asks for the person's own password again.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { head, paint, section, note, link, btn, kv, textBar } from '../core/ui.js';
import { carKeys } from '../core/carpick.js';
import { table } from '../core/table.js';
import { field, form } from '../core/form.js';
import { editLines } from '../core/kvedit.js';
import { empty, errorWords } from '../core/states.js';
import { tag, dateTime, date, ago, dur, DASH } from '../core/fmt.js';
import { toast } from '../core/toast.js';
import { openDialog, confirmDialog, passwordOk } from '../core/dialog.js';
import { signOut } from '../core/session.js';
import { askPassword, copyBtn, codeOut, downloadKey, showOnce, STAY } from '../core/secret.js';
import { AUDIT_GROUPS, actionLabel, auditItem } from '../shared/audit.js';

export const id = 'settings-server';
export const title = () => 'Settings: server';

const enc = encodeURIComponent;
const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const count = (n) => (n == null ? DASH : format(n, 'count'));
const pct = (a, b) => (a != null && b ? Math.max(0, Math.min(100, (a / b) * 100)) : 0);
// the acting admin's own password (and two-step code) for an account change: the headers the API takes
// (X-Sentry-Mate-Password, X-Sentry-Mate-Code), or confirm_password and code in the body for a password a header
// cannot hold (characters outside printable ASCII). -> [body, { headers }]
const ASCII = /^[\x20-\x7e]*$/;
const withCred = (cred, body) => (ASCII.test(cred.password) && ASCII.test(cred.two_step || '')
  ? [body, { headers: { 'X-Sentry-Mate-Password': cred.password, ...(cred.two_step ? { 'X-Sentry-Mate-Code': cred.two_step } : {}) } }]
  : [{ ...(body || {}), confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) }, {}]);
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : e?.message || errorWords(e).text);
const ROLE = { admin: 'admin', watch: 'watch' };
const PURPOSE = { web: 'web and api', upload: 'uploads' };
const HOST_RE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/;
const LIVE = new Set(['offered', 'copying', 'paused', 'moved']);

/** A list of '[tag] text' lines. */
const lines = (rows, attrs = {}) => h('ul', { class: 'ln-list', ...attrs }, rows.filter(Boolean).map(([t, text, sub]) =>
  h('li', null, t, h('span', { class: 'ln-list-t' }, text, sub ? h('span', { class: 'muted' }, sub) : null))));

/** Numbered steps: [state, head, text, actions]; state 'ok' | 'todo' | 'running' | 'later' | 'bad' | 'wait'. */
function runs(steps, attrs = {}) {
  const T = { ok: ['ok', 'ok'], todo: ['todo', 'warn'], running: ['running', 'running'], later: ['later', 'muted'], bad: ['bad', 'bad'], wait: ['wait', 'warn'], off: ['-', 'muted'] };
  return h('ol', { class: 'runs', ...attrs }, steps.filter(Boolean).map(([st, headText, text, acts, out], i) =>
    h('li', { class: ['run', st === 'todo' || st === 'running' ? 'run--now' : st === 'later' ? 'run--later' : null], dataset: { row: '' } },
      h('span', { class: 'run-n' }, `${i + 1}.`), h('span', { class: 'run-tag' }, tag(...T[st] || [st, 'muted'])),
      h('div', { class: 'run-body' }, h('p', { class: 'run-head' }, headText), text ? h('p', { class: 'run-text' }, text) : null, out || null),
      h('div', { class: 'run-acts' }, acts || null))));
}

export async function mount(ctx) {
  const admin = ctx.admin;
  const me = ctx.me || {};
  const S = { settings: null, network: null, notices: [], accounts: [], cars: [], sessions: [] };
  const loadAccounts = async () => { S.accounts = admin ? await ctx.api.list('accounts', { limit: 500 }).all({ max: 5000 }) : []; };
  const loadSessions = async () => { S.sessions = await ctx.api.list('me/sessions', { limit: 500 }).all({ max: 5000 }); };
  const [settings, network, notices, cars, trips] = await Promise.all([
    ctx.api.get('settings').catch(() => null),
    admin ? ctx.api.get('network').catch(() => null) : null,
    ctx.api.get('notices', null, { maxAgeMs: 5000 }).catch(() => null), // the frame's answer of this load
    ctx.api.list('cars', { sort: 'custom', limit: 500 }).all({ max: 5000 }).catch(() => []),
    admin ? ctx.api.list('trips', { limit: 500 }).all({ max: 2000 }).catch(() => []) : [],
    loadAccounts(), loadSessions(),
  ]);
  if (!ctx.alive()) return;
  Object.assign(S, { settings, network, notices: notices?.items || [], cars });
  const carName = new Map(S.cars.map((c) => [c.id, c.name]));
  const clock = S.cars.find((c) => c.id === state.preferredCar(S.cars)) || null;
  const tz = clock?.time_zone || ctx.tz;
  const t = (ms) => (ms == null ? DASH : dateTime(ms, tz));

  /* ---------------------------------------------------------------- this address */
  const loc = globalThis.location;
  const here = { https: loc.protocol === 'https:', host: loc.hostname.toLowerCase(), port: Number(loc.port || (loc.protocol === 'https:' ? 443 : 80)) };
  const webIn = S.network?.inbound?.find((x) => x.purpose === 'web') || null;
  const upIn = S.network?.inbound?.find((x) => x.purpose === 'upload') || null;
  let certNames = null;
  const readCert = () => { certNames = Array.isArray(S.network?.certificate?.names) ? S.network.certificate.names.map((n) => n.toLowerCase()) : null; };
  readCert();
  // the owner's own certificate (a cert.pem and key.pem put in data/tls): the server never makes one in its place
  const certOwn = () => !!S.network?.certificate?.own;
  const hostSlot = h('div');
  function paintHosting() {
    const saved = (S.settings?.host_names || []).map((n) => n.toLowerCase());
    const names = certNames || saved;
    const named = names.includes(here.host);
    replace(hostSlot, lines([
      here.https ? [tag('ok', 'ok'), 'https: this page came encrypted, so a password never travels in plain text.']
        : [tag('warn', 'warn'), 'plain http: Sentry Mate itself answers only https (TLS 1.3), so something else serves this page, such as a proxy or a dev server. Never type a password here over a network.'],
      named ? [tag('ok', 'ok'), `host name ${here.host} is on the certificate, so the browser accepts the address once it trusts the certificate.`]
        : saved.includes(here.host) ? [tag('info', 'muted'), `host name ${here.host} is saved but not yet in the certificate in use (${names.join(', ') || 'no name'}). Browsers warn until a new certificate is made.`]
          : [tag('warn', 'warn'), `host name ${here.host} is not on the certificate (${names.join(', ') || 'no name'}). Browsers warn about it. The CAN Bridge pins the certificate's SHA-256 and does not check names.`],
      webIn ? (here.port === webIn.port ? [tag('ok', 'ok'), `port ${here.port}: the port the server listens on for web pages.`]
        : [tag('info', 'muted'), `port ${here.port} here, ${webIn.port} in the container: a port mapping such as ${here.port}:${webIn.port} joins the two. Fine; it matters only when you type the address into another device.`])
        : [tag('info', 'muted'), `port ${here.port}. An admin sees the ports the server listens on.`],
      [tag('ok', 'ok'), 'at the root of its address: Sentry Mate lives at / on its own name or port, never under a path prefix. Behind a shared proxy, give it its own name.'],
      [tag('ok', 'ok'), 'works under any of its names: the session cookie is never sent by other sites (SameSite=Strict) and every change carries a header other sites cannot set.'],
    ]));
  }

  /* ---------------------------------------------------------------- host names, ports */
  const names = S.settings ? editLines([
    { key: 'host_names', id: 'certificate', type: 'text', value: (S.settings.host_names || []).join(', '), readOnly: !admin, cover: 'docs.env.web.virtual-host',
      help: 'The names or addresses people and devices use to reach this server, comma between them, at most 10. They go into the next certificate the server makes; the one in use keeps its names until then.',
      parse: (x) => {
        const list = [...new Set(x.split(/[\s,]+/).map((n) => n.trim().toLowerCase()).filter(Boolean))];
        if (!list.length) return { error: 'the certificate needs at least one name' };
        if (list.length > 10) return { error: 'at most 10 names' };
        const bad = list.find((n) => !HOST_RE.test(n) || n.length > 253);
        return bad ? { error: `${bad} is not a host name: letters, digits, hyphens and dots, such as sentry-mate.local or 192.168.1.20` } : { value: list };
      },
      toText: (v) => (Array.isArray(v) ? v.join(', ') : v),
      save: async (v) => {
        try { S.settings = await ctx.api.patch('settings', { version: S.settings.version, host_names: v }); } catch (e) {
          if (e.status === 409) { S.settings = await ctx.api.get('settings'); throw new Error('Someone else changed the server settings first. Look again and save once more.'); }
          throw e;
        }
        paintHosting();
      } },
  ]) : null;
  const ports = section({ title: 'ports', cover: 'docs.env.web.port' },
    S.network?.inbound?.length ? kv(S.network.inbound.map((p) => [PURPOSE[p.purpose] || p.purpose, [String(p.port), ' ', p.tls ? tag('https, tls 1.3', 'ok') : tag('no tls', 'warn')]]), { cols: 1 })
      : kv([['your address', String(here.port)]], { cols: 1 }),
    note(webIn ? ['To use another port on the host, change only the left number of the mapping in the compose or TrueNAS file, such as ', h('span', { class: 'prompt' }, `${here.port !== webIn.port ? here.port : 10443}:${webIn.port}`),
      `, and use that number in the address.${upIn ? ` The upload port maps the same way (${upIn.port}:${upIn.port}); a CAN Bridge uses the left number. It takes only uploads with a car's token, and stays closed while uploads are off.` : ''} Both speak only https; there is no plain http port.`]
      : 'An admin sees which ports the server listens on. Change only the left number of a port mapping (host:container) to use another host port.'));

  /* ---------------------------------------------------------------- certificate and recovery key */
  const cert = S.notices.find((n) => n.code === 'certificate_expiring');
  let keyUnsaved = !!S.notices.find((n) => n.code === 'recovery_key_unsaved');
  const keySlot = h('div');
  async function keyDialog(key, ms, why) {
    const tick = h('input', { class: 'chk', type: 'checkbox', id: 'ss-key-saved' });
    const ok = await showOnce({
      title: 'recovery key', okLabel: 'done: delete the server\'s copy',
      body: h('div', { class: 'hc' },
        h('p', null, tag('shown once', 'warn'), ' Keep it in a password manager or on paper, away from the server. Then tick and press done: the server deletes its copy and nobody can show it again.'),
        h('div', { class: 'box' }, codeOut(key), h('div', { class: 'acts' }, copyBtn(key, 'copy', { primary: true }), btn('download as a file', () => downloadKey(key, { ms, tz, why })))),
        h('label', { class: 'fld-checkline', for: 'ss-key-saved' }, tick, h('span', null, 'I have saved it somewhere safe, away from this server')),
        note('Closed without the tick? Nothing is lost: the server keeps its copy and this page shows it again.')),
      onOk: async () => {
        if (!tick.checked) return 'Save the key first (copy or download), then tick the box.';
        try { await ctx.api.post('system/recovery-key/saved', {}); return null; } catch (e) { return `The server could not delete its copy (${errText(e)}). Press done again.`; }
      },
    });
    keyUnsaved = !ok;
    if (ok) toast('Saved. The server deleted its copy of the recovery key.');
    paintKey();
  }
  async function showKey() {
    const r = await askPassword(ctx, { title: 'show the recovery key again', submit: 'show the key',
      intro: note('The server still keeps the key it made at set-up, until you say it is saved. Type your password to see it once more.'),
      run: (cred) => ctx.api.post('system/recovery-key/show', cred).catch((e) => { if (e.code === 'recovery_key_saved') return { gone: true }; throw e; }) });
    if (!r || !ctx.alive()) return;
    if (r.gone) { keyUnsaved = false; paintKey(); toast('The server deleted its copy already: the key was saved. If it is lost, make a new one.', { kind: 'info' }); return; }
    await keyDialog(r.recovery_key, ctx.api.now(), 'shown again');
  }
  async function newKey() {
    const r = await askPassword(ctx, { title: 'make a new recovery key', submit: 'make a new key', danger: true,
      intro: h('div', { class: 'hc' }, h('p', null, 'For a key that was lost or seen. ', h('b', null, 'The old key stops working at once.')),
        note('Export files and copies of the data folder made before still open only with the old key: keep it as long as you keep those.')),
      run: (cred) => ctx.api.post('system/recovery-key/new', cred) });
    if (!r || !ctx.alive()) return;
    toast('A new recovery key was made. The old one no longer works.');
    await keyDialog(r.recovery_key, r.made_ms || ctx.api.now(), 'new key made');
  }
  async function markSaved() {
    if (!(await confirmDialog('Only if the key is in a password manager or on paper, away from the server. The server then deletes its copy and nobody can show the key again.', { title: 'have you saved the recovery key?', ok: 'yes, delete the server\'s copy' }))) return;
    try { await ctx.api.post('system/recovery-key/saved', {}); keyUnsaved = false; paintKey(); toast('Saved. The server deleted its copy of the recovery key.'); } catch (e) { toast(`The server could not delete its copy: ${errText(e)}`, { kind: 'error' }); }
  }
  function paintKey() {
    replace(keySlot, runs([
      cert ? ['todo', `certificate: expires ${cert.detail?.expires_ms ? date(cert.detail.expires_ms, tz) : 'soon'}${cert.detail?.days != null ? ` (in ${count(cert.detail.days)} days)` : ''}`,
        certOwn() ? 'It is your own certificate, so the server cannot make a new one: renew it where you got it, put the new cert.pem and key.pem in data/tls and start the container again. A new certificate has a new SHA-256: type it into each CAN Bridge again.'
          : 'Make a new one before then (above). A new certificate has a new SHA-256: type it into each CAN Bridge again.']
        : ['ok', 'certificate', 'No problem reported. It keeps the same SHA-256 across restarts, updates and moves, until a new one is made.'],
      keyUnsaved
        ? ['todo', 'recovery key: not marked as saved', 'Until you say it is saved, the server keeps a copy in its keys folder. Show it again, keep it away from the server, then tick "I have saved it".',
          admin ? [btn('show again', showKey, { kind: 'primary' }), btn('saved already', markSaved), btn('make new', newKey, { kind: 'quiet' })] : null]
        : ['ok', 'recovery key: saved away from the server', 'The server keeps no copy. Lost it, or someone saw it? Make a new one: the old one stops working at once.', admin ? btn('make new', newKey) : null],
      ['ok', 'encrypted at rest', 'Logs, the database, tokens and the certificate\'s key are sealed. A backup of the data folder opens only with the key file (kept apart in /keys) or the recovery key: keep those out of the same backup.'],
    ]));
  }
  paintKey();

  // the certificate in use: its SHA-256, its names and the CAN Bridge's upload address, each to copy, and a new
  // one made here (no shell, no restart); never for the owner's own certificate
  const certSlot = h('div');
  const shaText = (v) => String(v || '').toUpperCase();
  // a SHA-256 or an address that wraps only after a colon or a slash, never inside a hex pair or a port
  const breakAfter = (t) => String(t).split(/(?<=[:/])/).flatMap((part, i) => (i ? [h('wbr'), part] : [part]));
  // the move state: while a move code waits, a new server copies from here, or the data moved away, the server
  // makes no new certificate (a new server pins this one), so the button is not offered then
  let moveState = null;
  const CERT_HELD = { offered: 'A move code is waiting: the new server pins this certificate.', copying: 'A new server copies from here and pins this certificate.',
    paused: 'A stopped move pins this certificate until it is ended or carried on.', moved: 'The data moved to another server: this one makes no new certificate.',
    done: 'The data moved to another server: this one makes no new certificate.' };
  function paintCert() {
    const c = S.network?.certificate || null;
    if (!admin) { replace(certSlot, note('An admin sees the certificate\'s SHA-256 and the CAN Bridge upload address, and can make a new certificate.')); return; }
    replace(certSlot,
      c || S.network?.upload_address ? kv([
        c?.sha256 ? ['sha-256', h('span', { class: 'hc' }, h('span', { class: 'cert-sha' }, breakAfter(shaText(c.sha256))), h('span', { class: 'acts' }, copyBtn(shaText(c.sha256))))] : null,
        c?.names?.length ? ['names in it', c.names.join(', ')] : null,
        S.network?.upload_address ? ['upload address', h('span', { class: 'hc' }, h('span', { class: 'cert-sha' }, breakAfter(S.network.upload_address)), h('span', { class: 'acts' }, copyBtn(S.network.upload_address)))] : null,
        ['made by', certOwn() ? [tag('own', 'muted'), ' you: the cert.pem and key.pem in data/tls'] : [tag('server', 'muted'), ' this server']],
      ], { cols: 1 }) : note('The server did not say which certificate it runs with.'),
      c?.sha256 ? note('The CAN Bridge app needs this SHA-256 (and the upload address) in its upload settings.') : null,
      certOwn()
        ? note('This is your own certificate, so the server never replaces it. To change it, put the new cert.pem and key.pem in data/tls and start the container again. For a certificate the server makes instead, delete data/tls/cert.pem and start it again.')
        : CERT_HELD[moveState]
          ? h('p', { class: 'note' }, tag('held', 'muted'), ` ${CERT_HELD[moveState]} `, moveState === 'moved' || moveState === 'done' ? null : ['Finish or end the move (', link(`${state.buildUrl('settings-server')}#move`, 'move to another server'), ') to make a new certificate.'])
          : h('div', { class: 'acts cert-acts' }, btn('make a new certificate', makeCertificate)));
  }
  const certMove = (st) => { const was = !!CERT_HELD[moveState]; moveState = st || null; if (was !== !!CERT_HELD[moveState]) paintCert(); };
  const nextNames = () => ['localhost', ...((S.settings?.host_names) || []).filter((n) => n.toLowerCase() !== 'localhost')];
  async function makeCertificate() {
    const out = await askPassword(ctx, {
      title: 'make a new certificate', submit: 'make a new certificate', danger: true,
      intro: h('div', { class: 'hc' },
        h('p', null, 'The server makes a new certificate and uses it at once on both ports, without a restart. Before you go on:'),
        lines([
          [tag('sha-256', 'warn'), 'It changes. Each CAN Bridge app refuses this server until you type the new SHA-256 into its upload settings.'],
          [tag('browsers', 'warn'), 'They ask again to accept the new certificate, on every device that opens these pages.'],
          [tag('names', 'muted'), [nextNames().join(', '), h('span', { class: 'muted' }, 'Change them under host names first, if one is missing.')]],
        ])),
      run: (cred) => ctx.api.post('network/certificate', cred).catch((e) => {
        if (e.status === 409) e.message = { own_certificate: 'This server runs with your own certificate; it never replaces it.', move_under_way: 'A move is under way: a new server pins this certificate. Finish or end the move first.', setup_busy: 'A move or a restore into this server runs now. Try again when it is done.', moved: 'The data moved to another server; this one makes no new certificate.' }[e.code] || e.message;
        throw e;
      }),
    });
    if (!out || !ctx.alive()) return;
    try { S.network = await ctx.api.get('network'); } catch { S.network = { ...(S.network || {}), certificate: { names: out.names, sha256: out.sha256, own: false }, upload_address: out.upload_address }; }
    if (!ctx.alive()) return;
    readCert(); paintCert(); paintHosting();
    // the button that opened this was painted again: the focus goes to the new one (and back to it after this)
    certSlot.querySelector('.cert-acts .btn')?.focus({ preventScroll: true });
    await showOnce({
      title: 'the new certificate is in use',
      body: h('div', { class: 'hc' },
        h('p', null, 'Type this SHA-256 into each CAN Bridge app\'s upload settings now: until then it refuses this server.'),
        h('div', { class: 'box' }, h('p', { class: 'cert-sha' }, breakAfter(shaText(out.sha256))), h('div', { class: 'acts' }, copyBtn(shaText(out.sha256), 'copy', { primary: true }))),
        note('Reload this page to accept the new certificate in this browser.')),
    });
  }
  paintCert();

  /* ---------------------------------------------------------------- moving (admin) */
  const M = { move: null, codeText: null, codeId: null, err: null, exp: null, expErr: null, want: false };
  const moveSlot = h('div', { 'aria-live': 'polite' });
  const expSlot = h('div', { 'aria-live': 'polite' });
  let mt = null, et = null;
  ctx.onCleanup(() => { clearTimeout(mt); clearTimeout(et); });
  function schedule() { clearTimeout(mt); if (M.move && LIVE.has(M.move.state)) mt = setTimeout(refreshMove, M.move.state === 'offered' ? 2000 : 1000); }
  async function refreshMove() {
    try { const m = await ctx.api.get('system/move'); if (!ctx.alive()) return; setMove(m); M.err = null; } catch (e) { if (!ctx.alive()) return; M.err = e; }
    paintMove(); schedule();
  }
  function setMove(m) {
    if (m.state === 'none' || m.state === 'expired' || (m.code && M.codeId && m.code.id !== M.codeId)) { M.codeText = null; M.codeId = null; }
    M.move = m;
    certMove(m?.state);
  }
  async function makeCode() {
    const busy = M.move && (M.move.state === 'copying' || M.move.state === 'paused');
    if (busy && !(await confirmDialog(`${M.move.peer?.address || 'A new server'} is copying now. A new code ends that move; this server's data does not change.`, { title: 'end the move under way?', ok: 'end it, make a new code', danger: true }))) return;
    const anywhere = field({ label: 'from anywhere, not only this home network', name: 'anywhere', type: 'checkbox', value: false,
      hint: 'Off when the new server is in the same house. On for one somewhere else; it then needs to reach this server\'s upload port.' });
    const r = await askPassword(ctx, { title: 'make a move code', submit: 'make the code', extra: [anywhere],
      intro: note('The code lets one new Sentry Mate server copy everything from this one: logs, cars, accounts, upload tokens and the certificate. It works once, for 24 hours.', M.move?.state === 'offered' ? ' The code waiting now stops working.' : ''),
      run: (cred, v) => ctx.api.post('system/move', { ...cred, anywhere: !!v.anywhere, ...(busy ? { replace: true } : {}) }) });
    if (!r || !ctx.alive()) return;
    M.codeText = r.move_code; M.codeId = r.code?.id || null; M.move = r; certMove(r.state);
    paintMove(); schedule();
    toast('Move code made. Type it on the new server\'s first page.');
  }
  async function cancelMove() {
    const copying = M.move && (M.move.state === 'copying' || M.move.state === 'paused');
    if (!(await confirmDialog(copying ? `The move to ${M.move.peer?.address || 'the new server'} ends now. Nothing here changes; this server keeps taking uploads. What the new server copied stays there: with a new code from this server it carries on from there.`
      : 'The code stops working at once. Nothing on this server changes.', { title: copying ? 'end the move?' : 'cancel the code?', ok: copying ? 'end the move' : 'cancel the code', danger: true }))) return;
    try { const r = await ctx.api.del('system/move'); if (!ctx.alive()) return; setMove(r); toast(r.cancelled === 'ended' ? 'The move was ended. This server keeps taking uploads.' : r.cancelled === 'revoked' ? 'The code was cancelled.' : 'There was nothing to cancel.', { kind: 'info' }); }
    catch (e) { toast(e.code === 'finished' ? 'Too late: the move finished. Use undo instead.' : `Could not cancel: ${errText(e)}`, { kind: 'error' }); await refreshMove(); return; }
    paintMove(); schedule();
  }
  async function undoMove() {
    const sure = field({ label: 'the new server is stopped, or I stop it now', name: 'confirm', type: 'checkbox', value: false, hint: 'If both servers take uploads, each gets only some of the logs.' });
    const r = await askPassword(ctx, { title: 'undo the move', submit: 'undo the move', danger: true, extra: [sure],
      intro: h('div', { class: 'hc' }, h('p', null, 'This server takes uploads again at once. Its data is as it was when the move finished: what reached the new server since is not here.'),
        note('Stop the new server first (on TrueNAS: Apps, its Sentry Mate app, Stop).')),
      run: (cred, v) => { if (!v.confirm) { sure.setError('Tick this first: stop the new server before you undo.'); return STAY; } return ctx.api.post('system/move/undo', { ...cred, confirm: true }); } });
    if (!r || !ctx.alive()) return;
    setMove(r); paintMove(); schedule();
    toast('The move was undone. This server takes uploads again.');
  }
  function progressBox(p) {
    if (!p) return null;
    const pc = pct(p.bytes_done, p.bytes_total);
    return h('pre', { class: 'mono-box out-box' },
      textBar(pc, { label: `${Math.round(pc)} percent copied` }), ` ${format(pc, 'number', { digits: 0 })} %\n`,
      `copied  ${bytes(p.bytes_done)} of ${bytes(p.bytes_total)}\nfiles   ${count(p.files_done)} of ${count(p.files_total)}${p.round > 1 ? `\nround   ${p.round}` : ''}`,
      p.bytes_per_s ? `\nspeed   ${bytes(p.bytes_per_s)}/s` : '', p.eta_s != null && p.bytes_per_s ? `\nleft    ${p.eta_s < 1 ? 'almost done' : `about ${dur(p.eta_s * 1000)}`}` : '');
  }
  const skipped = (list) => (list?.length ? note(`Not carried (this server can't read ${list.length === 1 ? 'it' : 'them'}): `, list.map((s) => s.path).join(', '), '.') : null);
  function paintMove() {
    const m = M.move;
    if (!m) { replace(moveSlot, M.err ? h('p', { class: 'errbox-line' }, tag('bad', 'bad'), ` could not read the move: ${errText(M.err)}`) : null); return; }
    const s = m.state, c = m.code;
    const where = c?.anywhere ? 'from anywhere' : 'only from this home network';
    const at = { none: 0, expired: 0, offered: 1, copying: 2, paused: 3, moved: 3, done: 4 }[s] ?? 0;
    const st = (i) => (at > i || s === 'done' ? 'ok' : at === i ? (i === 0 ? 'todo' : 'running') : 'later');
    const resumable = (s === 'none' || s === 'expired') && m.last_cancelled && !m.last_undone_ms;
    const codeBox = M.codeText ? h('div', { class: 'box out-box' }, h('p', { class: 'muted' }, 'move code:'), codeOut(M.codeText),
      h('div', { class: 'acts' }, copyBtn(M.codeText, 'copy the code', { primary: true })),
      h('p', { class: 'fld-hint' }, tag('shown once', 'warn'), ` works once, until ${t(c?.expires_ms)}, ${where}. Lost it? Make a new one; this one then stops working.`)) : null;
    const out = [
      M.err ? `! lost touch with the server (${errText(M.err)}); it keeps trying. The move itself goes on.` : null,
      `state   ${s}`,
      c ? `code    ${c.id}, made ${t(c.made_ms)}, until ${t(c.expires_ms)}, ${where}${c.wrong_tries ? `, ${c.wrong_tries} wrong tries` : ''}` : null,
      m.peer ? `peer    ${m.peer.address || DASH}${m.peer.app ? `, Sentry Mate ${m.peer.app}` : ''}, last heard ${m.peer.last_seen_ms ? ago(m.peer.last_seen_ms) : DASH}` : null,
      m.moved ? `moved   to ${m.moved.to || DASH} ${t(m.moved.switched_ms || m.moved.at_ms)}` : null,
      m.last_cancelled ? `stopped ${t(m.last_cancelled.at_ms)} (${m.last_cancelled.why})` : null,
      m.last_undone_ms ? `undone  ${t(m.last_undone_ms)}` : null,
    ].filter(Boolean).join('\n');
    replace(moveSlot,
      runs([
        resumable
          ? ['todo', 'carry on the stopped move', 'The last move stopped part way. The new server kept what it copied: make a new code here and type it there with the same address, and it carries on from where it stopped.', btn('make a new code', makeCode, { kind: 'primary' })]
          : [st(0), s === 'expired' ? 'make a move code: the last one expired unused' : 'make a move code', 'When the new server is installed and shows its first page. Asks your password.',
            s === 'none' || s === 'expired' ? btn('run', makeCode, { kind: 'primary' }) : s === 'offered' ? [btn('new code', makeCode), btn('cancel', cancelMove, { kind: 'quiet' })] : null, s === 'offered' ? codeBox : null],
        [st(1), 'type it on the new server', ['On its first page: its set-up code (from its log), then ', h('b', null, 'move here from another server'), ', then this server\'s address (the one the CAN Bridge sends to) and the code.'], null],
        [st(2), s === 'paused' ? 'last round: uploads wait a few seconds' : 'the new server copies', 'This server keeps taking uploads meanwhile. If it stops half way nothing here changes; the new server tries again and carries on.',
          s === 'copying' || s === 'paused' ? btn('end the move', cancelMove, { kind: 'danger' }) : null, s === 'copying' || s === 'paused' ? progressBox(m.progress) : null],
        [s === 'moved' ? 'running' : st(3), s === 'moved' ? 'switching: keep this server on' : 'the new server takes over', 'Then this server takes no uploads or changes; reading still works.', null],
        [s === 'done' ? 'ok' : 'later', 'turn this server off', s === 'done' ? `Moved to ${m.moved?.to || 'the new server'}. Sign in there with your account from here. In the CAN Bridge app change only the server address to https://${m.moved?.to || 'the-new-server'}:10100/api/can/logs; token and certificate SHA-256 came along. Keep this server's folders a few days.` : 'Wait until the step above says ok.', null],
        s === 'moved' || s === 'done' ? ['todo', 'changed your mind? undo the move', 'This server takes uploads again. Stop the new server first. Asks your password.', btn('undo', undoMove, { kind: 'danger' })] : null,
      ], { cover: null }),
      skipped(m.skipped),
      h('pre', { class: 'mono-box out-box', 'aria-label': 'Move output' }, out));
  }

  /* ---------------------------------------------------------------- export file */
  function scheduleExp() { clearTimeout(et); if (M.exp?.state === 'preparing') et = setTimeout(refreshExp, 1000); }
  async function refreshExp() {
    try {
      const e = M.exp ? await ctx.api.get(`system/exports/${enc(M.exp.id)}`) : null;
      if (!ctx.alive()) return;
      const was = M.exp?.state;
      M.exp = e; M.expErr = null;
      if (e && was === 'preparing' && e.state === 'ready' && M.want) { M.want = false; download(e); }
    } catch (err) { if (!ctx.alive()) return; if (err.status === 404) M.exp = null; else M.expErr = err; }
    paintExp(); scheduleExp();
  }
  function download(e) {
    if (!e?.url) return;
    const a = h('a', { href: e.url, download: e.file_name || '', hidden: true });
    document.body.append(a); a.click(); a.remove();
    toast(`Downloading ${e.file_name}.`);
  }
  async function makeExport() {
    const r = await askPassword(ctx, { title: 'make an export file', submit: 'make the file',
      intro: h('div', { class: 'hc' }, h('p', null, 'One file with everything, made in a few seconds (uploads wait for those seconds); then your browser downloads it.'),
        note('It opens only with this server\'s recovery key and never holds the key file, so without the key it is useless to anyone who takes it.')),
      run: (cred) => ctx.api.post('system/exports', cred).catch(async (e) => { if (e.code !== 'export_preparing') throw e; return (await ctx.api.get('system/exports')).items?.[0] || null; }) });
    if (!r || !ctx.alive()) return;
    M.exp = r; M.expErr = null; M.want = true;
    paintExp(); scheduleExp();
  }
  async function removeExport() {
    if (!(await confirmDialog('The file is removed from this server. A copy you downloaded stays where you put it.', { title: 'remove the export file?', ok: 'remove', danger: true }))) return;
    try { await ctx.api.del(`system/exports/${enc(M.exp.id)}`); M.exp = null; toast('The export file was removed from the server.'); } catch (e) { toast(`Could not remove it: ${errText(e)}`, { kind: 'error' }); }
    if (ctx.alive()) paintExp();
  }
  function paintExp() {
    const e = M.exp;
    const s = !e || e.state === 'expired' ? 'none' : e.state;
    const out = [
      M.expErr ? `! could not read the export: ${errText(M.expErr)}` : null,
      e ? `file    ${e.file_name}` : 'file    none yet',
      e ? `state   ${e.state}` : null,
      e?.state === 'ready' ? `size    ${bytes(e.size)}, ${count(e.files)} files\nmade    ${t(e.made_ms)}\nkept    until ${t(e.expires_ms)}${e.sent_bytes ? `\nsent    ${bytes(e.sent_bytes)}` : ''}` : null,
      e?.state === 'expired' ? `removed ${t(e.expires_ms)}: make a new one` : null,
      e?.state === 'failed' ? `error   ${e.error || 'it could not be made'}` : null,
    ].filter(Boolean).join('\n');
    replace(expSlot,
      runs([
        [s === 'none' || s === 'failed' ? 'todo' : s === 'preparing' ? 'running' : 'ok', 'make the export file', 'A snapshot of the whole server. Asks your password. A new one replaces the one there was.',
          s === 'preparing' ? null : btn(s === 'ready' ? 'make a new one' : 'run', makeExport, { kind: s === 'ready' ? '' : 'primary' })],
        [s === 'ready' ? (e.sent_bytes ? 'ok' : 'todo') : 'later', 'download it', 'A cut download carries on when you press download again. Kept a day after the last download, a week at most.',
          s === 'ready' ? [h('a', { class: 'btn btn--primary', href: e.url, download: e.file_name }, 'download'), btn('remove', removeExport, { kind: 'quiet' })] : null],
        [s === 'ready' && e.sent_bytes ? 'todo' : 'later', 'restore it on the new server', ['On its first page choose ', h('b', null, 'restore from an export file'), ', choose this file and type the recovery key.'], null],
      ]),
      skipped(e?.skipped),
      h('pre', { class: 'mono-box out-box', 'aria-label': 'Export output' }, out));
  }

  /* ---------------------------------------------------------------- access, accounts, sessions, audit */
  const nAdmins = S.accounts.filter((a) => a.role === 'admin').length;
  const access = section({ title: 'how access works', cover: 'web.shell.access-model' },
    kv([
      ['you', [me.name || DASH, ' ', tag(ROLE[me.role] || me.role || '?', me.role === 'admin' ? 'accent' : 'muted'), me.cars === 'all' ? ' every car' : ` ${count(Array.isArray(me.cars) ? me.cars.length : 0)} cars`]],
      admin ? ['accounts', `${count(S.accounts.length)} (${count(nAdmins)} admin, ${count(S.accounts.length - nAdmins)} watch)`] : null,
      admin ? ['two-step required', count(S.accounts.filter((a) => a.two_step_required).length)] : null,
    ].filter(Boolean)),
    lines([
      [tag('rule', 'muted'), 'nothing without signing in: every page and every api call needs a session.'],
      [tag('rule', 'muted'), 'two roles: admin sees and changes everything; watch sees only the cars on its list and changes only its own preferences and sessions.'],
      [tag('rule', 'muted'), 'a car not on an account\'s list looks exactly like a car that does not exist.'],
      [tag('rule', 'muted'), 'two-step sign-in: an account can be made to give a six-digit code after its password.'],
      [tag('rule', 'muted'), 'guessing is slowed: five wrong passwords for one name in a minute, or 30 from one address, and sign-in waits a minute.'],
      [tag('rule', 'muted'), ['cars never sign in: a CAN Bridge uploads with its car\'s token on the upload port, made in ', link(state.buildUrl('settings-cars'), 'settings/cars'), '.']],
      [tag('rule', 'muted'), admin ? 'every change is recorded in the audit log below.' : 'every change is recorded; admins can read who changed what.'],
    ]));

  const carsText = (cs) => (cs === 'all' ? 'all cars' : !Array.isArray(cs) || !cs.length ? 'no cars' : cs.map((c) => carName.get(c) || c).join(', '));
  const accounts = admin ? table(ctx, {
    title: 'accounts', id: 'accounts', rows: S.accounts, csv: false, label: 'Accounts that can sign in',
    side: btn('add account', () => editAccount(null), { kind: 'primary' }),
    sort: { key: 'name', dir: 'asc' },
    columns: [
      { key: 'name', label: 'name', sort: true, ph: 'main', render: (a) => [a.name, a.id === me.id ? [' ', tag('you', 'accent')] : null] },
      { key: 'role', label: 'role', sort: true, ph: 'num', render: (a) => tag(ROLE[a.role] || a.role, a.role === 'admin' ? 'accent' : 'muted') },
      { key: 'cars', label: 'cars it sees', ph: 'sub', grow: true, render: (a) => carsText(a.cars) },
      { key: 'two', label: 'two-step', render: (a) => (a.two_step_required ? tag('required', 'ok') : h('span', { class: 'muted' }, 'password only')) },
      { key: 'created_ms', label: 'made', sort: true, render: (a) => date(a.created_ms, tz) },
      { key: 'act', label: '', render: (a) => h('div', { class: 'acts' }, btn('edit', () => editAccount(a)), btn('remove', () => removeAccount(a), { kind: 'quiet', disabled: a.id === me.id })) },
    ],
    empty: () => empty('no accounts'),
  }) : null;
  async function refreshAccounts() { await loadAccounts(); if (ctx.alive()) accounts.setRows(S.accounts); }
  function editAccount(a) {
    const isNew = !a;
    const name = field({ label: 'name', name: 'name', value: a?.name || '', required: true, hint: 'What they type to sign in. 1 to 64 characters.', attrs: { maxlength: 64, autocomplete: 'off' } });
    const password = field({ label: isNew ? 'password' : 'new password', name: 'password', type: 'password', required: isNew, hint: isNew ? 'Tell it to them in person or over a channel you trust.' : 'Empty keeps the password.', attrs: { autocomplete: 'new-password' } });
    const role = field({ label: 'role', name: 'role', type: 'select', value: a?.role || 'watch', options: [['watch', 'watch: sees its cars, changes nothing shared'], ['admin', 'admin: every car and setting']] });
    const two = field({ label: 'require two-step sign-in', name: 'two_step_required', type: 'checkbox', value: !!a?.two_step_required });
    const allCars = field({ label: 'every car, also cars that appear later', name: 'all_cars', type: 'checkbox', value: a?.cars === 'all' });
    const chosen = new Set(Array.isArray(a?.cars) ? a.cars : []);
    const list = h('div', { class: 'hc' }, carKeys(S.cars, (c) => h('label', { class: 'fld-checkline' },
      h('input', { class: 'chk', type: 'checkbox', checked: chosen.has(c.id), onchange: (e) => (e.target.checked ? chosen.add(c.id) : chosen.delete(c.id)) }), h('span', null, c.name)), { keep: [...chosen] }));
    const syncList = () => { list.hidden = allCars.input.checked; };
    allCars.input.addEventListener('change', syncList); syncList();
    let d = null;
    const f = form({
      fields: [name, password, role, two, allCars], submit: isNew ? 'add account' : 'save',
      secondary: h('button', { class: 'btn', type: 'button', onclick: () => d.close() }, 'cancel'),
      async onSubmit(v) {
        const body = { name: v.name.trim(), role: v.role, cars: v.all_cars ? 'all' : [...chosen], two_step_required: v.two_step_required };
        if (v.password) {
          if (!(await passwordOk(body.name, v.password, password.input))) return;
          body.password = v.password;
        }
        // the acting admin's own password again (and the code with two-step sign-in), as every account change asks
        const out = await askPassword(ctx, {
          title: isNew ? `add ${body.name}` : `save ${a.name}`, submit: isNew ? 'add account' : 'save',
          intro: note(isNew ? `${body.name} can sign in once it is added. Account changes ask for your own password again.` : 'Account changes ask for your own password again.'),
          run: async (cred) => {
            try {
              if (isNew) await ctx.api.post('accounts', ...withCred(cred, body));
              else await ctx.api.patch(`accounts/${enc(a.id)}`, ...withCred(cred, { version: a.version, ...body }));
              return { ok: true };
            } catch (e) {
              if (['wrong_password', 'password_needed'].includes(e.code)) { e.code = 'wrong_password'; throw e; }
              if (e.code === 'wrong_code' || e.code === 'wrong_two_step') { e.code = 'wrong_two_step'; throw e; }
              if (e.status === 429) throw e;
              return { err: e };   // shown in the account form, under its field
            }
          },
        });
        if (!out || !ctx.alive()) return;
        const e = out.err;
        if (e) {
          if (e.code === 'name_taken') { name.setError('That name is taken.'); name.input.focus(); return; }
          if (e.code === 'last_admin') { role.setError('This is the last admin. Make another account an admin first.'); role.input.focus(); return; }
          if (e.status === 409) { await refreshAccounts(); f.setMessage('Someone else changed this account first. Close, look at their change, and try again.'); return; }
          throw e;
        }
        toast(isNew ? `${body.name} can sign in now.` : `${body.name} saved.`);
        d.close();
        await refreshAccounts();
      },
    });
    d = openDialog({ title: isNew ? 'add an account' : `edit ${a.name}`, body: [f.el, h('p', { class: 'sub-title' }, 'cars it sees'), list] });
    name.input.focus();
  }
  async function removeAccount(a) {
    const out = await askPassword(ctx, {
      title: `remove ${a.name}?`, submit: 'remove', danger: true,
      intro: note(`${a.name} can no longer sign in and every session of theirs ends now. Their changes stay in the audit log.`),
      run: async (cred) => {
        try { await ctx.api.send('DELETE', `accounts/${enc(a.id)}`, ...withCred(cred, undefined)); return { ok: true }; } catch (e) {
          if (['wrong_password', 'password_needed'].includes(e.code)) { e.code = 'wrong_password'; throw e; }
          if (e.code === 'wrong_code' || e.code === 'wrong_two_step') { e.code = 'wrong_two_step'; throw e; }
          if (e.status === 429) throw e;
          return { err: e };
        }
      },
    });
    if (!out || !ctx.alive()) return;
    if (out.err) { toast(out.err.code === 'last_admin' ? 'This is the last admin. Make another account an admin first.' : `Could not remove: ${errText(out.err)}`, { kind: 'error' }); return; }
    toast(`${a.name} was removed.`); await refreshAccounts();
  }

  const sessions = table(ctx, {
    title: 'your sessions', rows: S.sessions, csv: false, label: 'Where you are signed in',
    note: `Every browser where you are signed in; times in ${tz}. End one you do not recognise, then change your password.`,
    side: btn('sign out everywhere else', () => endOthers()),
    sort: { key: 'last_seen_ms', dir: 'desc' },
    columns: [
      { key: 'device', label: 'browser', ph: 'main', grow: true, render: (s) => [s.device || 'not reported', s.this_one ? [' ', tag('this one', 'accent')] : null] },
      { key: 'created_ms', label: 'signed in', sort: true, ph: 'sub', render: (s) => dateTime(s.created_ms, tz) },
      { key: 'last_seen_ms', label: 'last used', sort: true, render: (s) => (s.this_one ? 'now' : ago(s.last_seen_ms)) },
      { key: 'act', label: '', ph: 'num', render: (s) => btn(s.this_one ? 'sign out here' : 'sign out', () => endSession(s)) },
    ],
    empty: () => empty('only this browser is signed in'),
  });
  async function refreshSessions() { await loadSessions(); if (ctx.alive()) sessions.setRows(S.sessions); }
  async function endSession(s) {
    if (s.this_one) { try { await signOut(); toast('Signed out.', { next: true, kind: 'info' }); state.navigate('/sign-in', { replace: true }); } catch (e) { toast(`Could not sign out: ${errText(e)}`, { kind: 'error' }); } return; }
    try { await ctx.api.del(`me/sessions/${enc(s.id)}`); toast(`Signed out ${s.device || 'that browser'}.`); await refreshSessions(); } catch (e) { toast(`Could not end it: ${errText(e)}`, { kind: 'error' }); }
  }
  async function endOthers() {
    const n = S.sessions.filter((s) => !s.this_one).length;
    if (!n) { toast('No other browser is signed in.', { kind: 'info' }); return; }
    if (!(await confirmDialog(`${count(n)} other ${n === 1 ? 'browser is' : 'browsers are'} signed out now. This one stays.`, { title: 'sign out everywhere else?', ok: 'sign out the others' }))) return;
    try { await ctx.api.del('me/sessions'); toast('The other sessions were ended.'); await refreshSessions(); } catch (e) { toast(`Could not: ${errText(e)}`, { kind: 'error' }); }
  }

  // the audit log in words (shared/audit.js): the action (its code on hover), the item it changed, linked where
  // it still exists; filtered by what (a group of actions) and who; ten entries, more as the end scrolls in
  const lower1 = (t) => (t ? t[0].toLowerCase() + t.slice(1) : t);
  const tripOf = new Map((trips || []).map((x) => [String(x.id), x]));
  const itemCtx = { carName: (c) => carName.get(c), accountName: (a) => S.accounts.find((x) => x.id === a)?.name, trip: (x) => tripOf.get(String(x)) };
  function target(e) {
    const it = auditItem(e, itemCtx);
    if (!it) return null;
    if (!it.route || it.faint) return h('span', { class: 'muted' }, it.text);
    return link(state.buildUrl(it.route[0], it.route[1] || {}) + (it.hash || ''), it.text);
  }
  const auditF = { group: '', who: '' };
  const auditParams = () => ({ ...(auditF.group ? { group: auditF.group } : {}), ...(auditF.who ? { who: auditF.who } : {}) });
  const whatSel = h('select', { class: 'in', name: 'audit-group', 'aria-label': 'What' },
    h('option', { value: '' }, 'what: all'), AUDIT_GROUPS.map((g) => h('option', { value: g.id }, `what: ${lower1(g.label)}`)));
  const whoSel = h('select', { class: 'in', name: 'audit-who', 'aria-label': 'Who' },
    h('option', { value: '' }, 'who: all'), [...new Set(S.accounts.map((a) => a.name))].sort((x, y) => x.localeCompare(y)).map((n) => h('option', { value: n }, `who: ${n}`)),
    h('option', { value: '*server' }, 'who: the server'));
  const refilter = () => { auditF.group = whatSel.value; auditF.who = whoSel.value; audit.reload(auditParams()); };
  whatSel.addEventListener('change', refilter);
  whoSel.addEventListener('change', refilter);
  const audit = admin ? table(ctx, {
    title: 'audit log', path: 'audit', limit: 10, csvName: 'audit-log', label: 'Who changed what', wrapCls: 'audit-sec',
    note: `Every change and sign-in, newest first; five at a time, more as you scroll the list. Times in ${tz}.`,
    filters: h('div', { class: 'filt filt--audit' }, h('span', { class: 'filt-p' }, 'filter>'), h('span', { class: 'filt-sel' }, whatSel), h('span', { class: 'filt-sel' }, whoSel)),
    autoMore: true, moreLabel: 'load more', box: 'Audit log entries',
    columns: [
      { key: 'at_ms', label: 'when', ph: 'sub', cls: 'nowrap', render: (e) => dateTime(e.at_ms, tz) },
      { key: 'account', label: 'who', ph: 'num', cls: 'nowrap', render: (e) => e.account || h('span', { class: 'muted' }, 'the server') },
      { key: 'action', label: 'action', ph: 'main', render: (e) => h('span', { title: e.action }, lower1(actionLabel(e.action))) },
      { key: 'target', label: 'item', ph: 'sub', render: (e) => target(e) },
      { key: 'summary', label: 'what happened', grow: true, ph: 'sub', render: (e) => e.summary || null },
    ],
    empty: () => empty(auditF.group || auditF.who ? 'no entry matches this what and who' : 'nothing recorded yet'),
  }) : null;

  /* ---------------------------------------------------------------- the page */
  paintHosting();
  const moving = admin ? [
    section({ title: 'move to another server', id: 'move', side: h('span', { class: 'muted' }, 'with a move code') },
      note('Both servers on at once, and the new one can reach this one (same home network, or tick from anywhere). This server keeps taking uploads while the new one copies; the new one takes over by itself. The CAN Bridge keeps its token and certificate SHA-256; at most its server address changes.'),
      moveSlot),
    section({ title: 'export file', id: 'export', side: h('span', { class: 'muted' }, 'or with one file') },
      note('For servers that can\'t reach each other, one that will be off before the new one starts, or a copy to keep. Uploads that arrive after it was made are not in it.'),
      expSlot,
      note('Neither way changes this server\'s data. Codes, passwords and keys travel only over https.')),
  ] : null;
  paint(ctx, head(ctx),
    section({ title: 'this address', cover: 'web.shell.hosting', side: h('span', { class: 'muted' }, `${loc.protocol}//${loc.host}/`) }, hostSlot),
    names ? section({ title: 'host names' }, names.el) : null,
    ports,
    section({ title: 'certificate and recovery key', id: 'certificate-in-use' }, certSlot, keySlot),
    access,
    moving,
    accounts?.el,
    sessions.el,
    audit?.el);
  if (admin) {
    try {
      const [m, ex] = await Promise.all([ctx.api.get('system/move'), ctx.api.get('system/exports').catch(() => null)]);
      if (!ctx.alive()) return;
      setMove(m); M.exp = ex?.items?.[0] || null;
    } catch (e) { if (!ctx.alive()) return; M.err = e; }
    paintMove(); paintExp(); schedule(); scheduleExp();
  }
  await Promise.all([accounts?.ready, sessions.ready, audit?.ready].filter(Boolean)).catch(() => {});
  if (ctx.alive()) ctx.ready();
}
