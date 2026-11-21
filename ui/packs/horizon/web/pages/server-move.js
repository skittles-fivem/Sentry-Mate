// Server and access: moving to another server, and the recovery key (admins only; part of
// pages/settings-server.js). No shell is needed for any of it.
//
//   - A move code (POST /system/move): both servers are on; the new one copies over the network while this one
//     keeps taking uploads, then takes over by itself. The code is shown once on an LCD face with Copy. While
//     a move runs the panel reads GET /system/move once a second (twice a second is never needed) and shows the
//     steps as lamps and the copy as a bar. A move ended part way keeps what the new server copied: a new code
//     from this server carries it on. Afterwards it says where the data went, with Undo (POST /system/move/undo).
//   - An export file (POST /system/exports): one file with everything, for servers that can't reach each
//     other, or to keep. It opens only with the recovery key and never holds the key file.
//   - The recovery key: show it again, make a new one, or say it is saved (POST /system/recovery-key/...).
// Making a code, undoing a move, making an export and the recovery key ask for the person's password again.

import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { dayKey } from '../shared/time.js';
import { icon, hud, btn, tag, bar, callout, note } from '../core/ui.js';
import { field, form } from '../core/form.js';
import { openDialog, confirmDialog, toast } from '../core/layer.js';
import { dateTime, dur, ago, DASH } from '../core/fmt.js';
import { lcd, copyBtn, lampSteps, errText, downloadText, code } from '../core/setting.js';

const enc = encodeURIComponent;
const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const count = (n) => (n == null ? DASH : format(n, 'count'));
const share = (a, b) => (a != null && b ? Math.max(0, Math.min(1, a / b)) : 0);
const eta = (s) => (s == null ? null : s < 1 ? 'almost done' : `about ${dur(s * 1000)} to go`);
const LIVE = new Set(['offered', 'copying', 'paused', 'moved']);
const STAY = Symbol('stay');

/**
 * The person's own password again (and the two-step code when the account has one) before a big action.
 * run(credentials, values) does the call; a wrong password keeps the dialog open. Resolves with run's answer,
 * or null when closed.
 */
export function askPassword(ctx, { title, intro, submit, danger = false, extra = [], run }) {
  const me = ctx.me || {};
  const pass = field({ label: 'Your password', name: 'password', type: 'password', required: true, hint: `The password you sign in with as ${me.name || 'this account'}.`, attrs: { autocomplete: 'current-password' } });
  const two = field({ label: 'Code from your authenticator app', name: 'two_step', hint: 'Six digits; they change every 30 seconds.', attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', maxlength: '8' } });
  two.el.hidden = !me.two_step_required;
  let result = null;
  return new Promise((resolve) => {
    let d = null;
    const f = form({
      fields: [...extra, pass, two], submit, danger,
      secondary: btn('Cancel', () => d?.close(), { kind: 'ghost' }),
      async onSubmit(v) {
        const cred = { password: v.password };
        if (!two.el.hidden && v.two_step) cred.two_step = String(v.two_step).replace(/\s+/g, '');
        try {
          const r = await run(cred, v);
          if (r === STAY) return;
          result = r;
        } catch (e) {
          if (e.code === 'wrong_password') { pass.setError('That is not your password. Type the one you sign in with.'); pass.input.select(); return; }
          if (e.code === 'wrong_two_step') { two.el.hidden = false; two.setError(v.two_step ? 'That code is not right. Type the one your app shows now.' : 'Your account has two-step sign-in: type the code from your app too.'); two.input.focus(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong passwords. Wait ${e.retryAfterMs ? Math.ceil(e.retryAfterMs / 1000) : 60} s, then try again.`); return; }
          throw e;
        }
        d?.close();
      },
    });
    d = openDialog({ title, body: [intro, f.el], onClose: () => resolve(result) });
    pass.input.focus();
  });
}

/** The recovery key as a text file that names this server and the full date. */
function keyFile(key, ms, tz, why) {
  const day = dayKey(ms, tz);
  const host = location.hostname.replace(/[^A-Za-z0-9.-]+/g, '_') || 'server';
  downloadText(`sentry-mate-recovery-key-${host}-${day}.txt`,
    `Sentry Mate recovery key\r\n\r\n${key}\r\n\r\nFor the server at ${location.host}, ${why} ${dateTime(ms, tz, { year: true, month: 'long', weekday: 'long' })} (${day}).\r\n`
    + 'It opens this server\'s data if its key file is lost, and the export files it makes.\r\nKeep it away from the server: in a password manager, or on paper.\r\n');
}

/** The key on an LCD face, with Copy, Download and the tick "I have saved it". Resolves true once saved. */
function keyDialog(ctx, key, { title, intro, ms, tz, why }) {
  const tick = h('input', { type: 'checkbox', id: 'hz-key-saved' });
  const err = h('p', { class: 'field-error', role: 'alert', hidden: true });
  const say = (t) => { replace(err, icon('warn', 'i--sm'), h('span', null, t)); err.hidden = false; };
  tick.addEventListener('change', () => { if (tick.checked) err.hidden = true; });
  let ok = false;
  return new Promise((resolve) => {
    const done = btn('Done: delete the server\'s copy', async () => {
      if (!tick.checked) { say('Save the key first (Copy or Download), then tick "I have saved it".'); tick.focus(); return; }
      done.disabled = true;
      try { await ctx.api.post('system/recovery-key/saved', {}); ok = true; d.close(); } catch (e) { say(`The server could not delete its copy (${errText(e)}). Press Done again; until it works the copy stays.`); } finally { done.disabled = false; }
    }, { kind: 'go', icon: 'check' });
    const d = openDialog({
      title, cls: 'dlg--wide',
      body: [intro, lcd(key, { label: 'Recovery key', big: true }),
        h('div', { class: 'row' }, copyBtn(key, 'Copy', { sm: false, kind: 'go' }), btn('Download as a file', () => keyFile(key, ms, tz, why), { icon: 'download' })),
        h('label', { class: 'check sm-tick', for: 'hz-key-saved' }, tick, h('span', null, 'I have saved it somewhere safe, away from this server')),
        note('Closed without the tick? Nothing is lost: the server keeps its copy, and this page can show the key again.'), err],
      actions: [btn('Close', () => d.close(), { kind: 'ghost' }), done],
      onClose: () => resolve(ok),
    });
  });
}

/** Show the key still waiting in RECOVERY-KEY.txt. Resolves true once saved (or already gone). */
export async function showKey(ctx, { tz }) {
  const r = await askPassword(ctx, {
    title: 'Show the recovery key again', submit: 'Show the key',
    intro: h('p', null, 'The server keeps the key it made at set-up until you say it is saved. Type your password to see it once more.'),
    run: (cred) => ctx.api.post('system/recovery-key/show', cred).catch((e) => {
      if (e.code !== 'recovery_key_saved') throw e;
      toast('The server deleted its copy already: the key was saved. If it is lost, make a new one.', { kind: 'info' });
      return { gone: true };
    }),
  });
  if (!r || r.gone || !ctx.alive()) return !!r?.gone;
  return keyDialog(ctx, r.recovery_key, { title: 'Your recovery key', ms: ctx.api.now(), tz, why: 'shown again',
    intro: h('p', null, 'Keep it in a password manager or on paper, ', h('b', null, 'away from the server'), '. Then tick below: the server deletes its copy and nobody can show the key again.') });
}

/** A new recovery key: the old one stops working. Resolves true (saved), 'unsaved' or false (none made). */
export async function newKey(ctx, { tz }) {
  const r = await askPassword(ctx, {
    title: 'Make a new recovery key', submit: 'Make a new key', danger: true,
    intro: h('div', { class: 'stack' },
      h('p', null, 'For a key that was lost, or seen by someone who should not have it. ', h('b', null, 'The old key stops working at once.')),
      h('p', null, 'Export files and copies of the data folder made before still open only with the old key: keep it as long as you keep those.')),
    run: (cred) => ctx.api.post('system/recovery-key/new', cred),
  });
  if (!r || !ctx.alive()) return false;
  toast('A new recovery key was made. The old one no longer works.');
  return (await keyDialog(ctx, r.recovery_key, { title: 'Your new recovery key', ms: r.made_ms || ctx.api.now(), tz, why: 'new key made',
    intro: h('p', null, h('b', null, 'Shown only now.'), ' Keep it in a password manager or on paper, away from the server, then tick below.') })) || 'unsaved';
}

/** "I have saved it" without showing the key. */
export async function keySaved(ctx) {
  if (!(await confirmDialog('Only if the SMRK- key is in a password manager or on paper, away from the server. The server then deletes its copy (RECOVERY-KEY.txt), and nobody can show the key again.', { title: 'Have you saved the recovery key?', ok: 'Yes, delete the server\'s copy' }))) return false;
  try { await ctx.api.post('system/recovery-key/saved', {}); } catch (e) { toast(`The server could not delete its copy: ${errText(e)}`, { kind: 'error' }); return false; }
  toast('Saved. The server deleted its copy of the recovery key.');
  return true;
}

/* ====================================================================== the move panel */

const STEPS = [['offered', 'Code made'], ['copying', 'Copying'], ['paused', 'Last round'], ['moved', 'Switching'], ['done', 'Done']];

export function movePanel(ctx, { tz, zoneText }) {
  const S = { move: null, codeText: null, codeId: null, exp: null, err: null, expErr: null, wantDownload: false };
  const moveBox = h('div', { class: 'mv-box', 'aria-live': 'polite' });
  const expBox = h('div', { class: 'mv-box', 'aria-live': 'polite' });
  const at = (ms) => (ms == null ? DASH : dateTime(ms, tz));
  let timer = 0, expTimer = 0;
  ctx.onCleanup(() => { clearTimeout(timer); clearTimeout(expTimer); });

  function schedule() { clearTimeout(timer); if (S.move && LIVE.has(S.move.state)) timer = setTimeout(refresh, S.move.state === 'offered' ? 2000 : 1000); }
  async function refresh() {
    try { const m = await ctx.api.get('system/move'); if (!ctx.alive()) return; setMove(m); S.err = null; } catch (e) { if (!ctx.alive()) return; S.err = e; }
    paintMove(); schedule();
  }
  function setMove(m) {
    if (m.state === 'none' || m.state === 'expired' || (m.code && S.codeId && m.code.id !== S.codeId)) { S.codeText = null; S.codeId = null; }
    S.move = m;
  }

  async function makeCode() {
    const busy = S.move && (S.move.state === 'copying' || S.move.state === 'paused');
    if (busy && !(await confirmDialog(`${S.move.peer?.address || 'A new server'} is copying this server now. A new code ends that move: the copy there stops, and nothing here changes.`, { title: 'End the move under way?', ok: 'End it and make a new code', danger: true }))) return;
    const anywhere = field({ label: 'From anywhere, not only from this home network', name: 'anywhere', type: 'checkbox', value: false,
      hint: 'Off for a new server in the same house. On for one somewhere else (another house, a rented server); it then needs to reach this server\'s upload port.' });
    const r = await askPassword(ctx, {
      title: 'Make a move code', submit: 'Make the code', extra: [anywhere],
      intro: h('p', null, 'The code lets one new Sentry Mate server copy everything from this one: logs, cars, accounts, upload tokens and the certificate. It works once, for 24 hours. ',
        S.move?.state === 'offered' ? h('b', null, 'The code waiting now stops working.') : null),
      run: (cred, v) => ctx.api.post('system/move', { ...cred, anywhere: !!v.anywhere, ...(busy ? { replace: true } : {}) }),
    });
    if (!r || !ctx.alive()) return;
    S.codeText = r.move_code; S.codeId = r.code?.id || null; S.move = r;
    paintMove(); schedule();
    toast('Move code made. Type it on the new server\'s first page.');
  }
  async function cancel() {
    const copying = S.move && (S.move.state === 'copying' || S.move.state === 'paused');
    if (!(await confirmDialog(copying
      ? `The move to ${S.move.peer?.address || 'the new server'} ends now. Nothing here changes; this server keeps taking uploads. What the new server copied stays there: a new code from this server carries it on.`
      : 'The code stops working at once. Nothing here changes. You can make a new code at any time.',
    { title: copying ? 'End the move?' : 'Cancel the move code?', ok: copying ? 'End the move' : 'Cancel the code', danger: true }))) return;
    try {
      const r = await ctx.api.del('system/move');
      if (!ctx.alive()) return;
      setMove(r);
      toast(r.cancelled === 'ended' ? 'The move was ended. This server keeps taking uploads.' : r.cancelled === 'revoked' ? 'The move code was cancelled: it no longer works.' : 'There was nothing to cancel.', { kind: 'info' });
    } catch (e) {
      toast(e.code === 'finished' ? 'Too late to cancel: the move finished. Use Undo the move instead.' : `Could not cancel: ${errText(e)}`, { kind: 'error' });
      await refresh();
      return;
    }
    paintMove(); schedule();
  }
  async function undo() {
    const sure = field({ label: 'The new server is stopped, or I will stop it now', name: 'confirm', type: 'checkbox', value: false, hint: 'If both servers take uploads, they drift apart: each gets only some of the logs.' });
    const r = await askPassword(ctx, {
      title: 'Undo the move', submit: 'Undo the move', danger: true, extra: [sure],
      intro: h('div', { class: 'stack' },
        h('p', null, 'This server takes uploads again at once, as before the move. Its data is as it was when the move finished: what reached the new server since is not here.'),
        h('p', null, h('b', null, 'Stop the new server first'), ' (on TrueNAS: Apps, its Sentry Mate app, Stop).')),
      run: (cred, v) => {
        if (!v.confirm) { sure.setError('Tick this first: stop the new server before you undo.'); sure.input.focus(); return STAY; }
        return ctx.api.post('system/move/undo', { ...cred, confirm: true });
      },
    });
    if (!r || !ctx.alive()) return;
    setMove(r);
    paintMove(); schedule();
    toast('The move was undone. This server takes uploads again.');
  }

  const progress = (p) => (p ? h('div', { class: 'mv-prog' },
    bar(share(p.bytes_done, p.bytes_total), { label: 'Copied' }),
    h('p', { class: 'mv-prog-text' }, h('b', null, bytes(p.bytes_done)), ` of ${bytes(p.bytes_total)} copied (${format(share(p.bytes_done, p.bytes_total) * 100, 'number', { digits: 0 })} %)`,
      ` · ${count(p.files_done)} of ${count(p.files_total)} files`,
      p.cars_total ? ` · ${count(p.cars_done)} of ${count(p.cars_total)} cars` : null,
      p.round > 1 ? ` · round ${p.round}` : null,
      p.bytes_per_s ? ` · ${bytes(p.bytes_per_s)} a second` : null,
      p.eta_s != null && p.bytes_per_s ? ` · ${eta(p.eta_s)}` : null)) : null);
  const skipped = (list) => (list?.length ? h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, `Not carried, because this server can't read ${list.length === 1 ? 'it' : 'them'}: `, list.map((s, i) => [i ? ', ' : '', code(s.path)]), '.')) : null);
  const howto = (head, ...items) => h('div', { class: 'mv-howto' }, h('p', { class: 'mv-howto-head' }, head), h('ol', { class: 'mv-howto-list' }, items.map((x, i) => h('li', null, h('span', { class: 'mv-howto-n' }, String(i + 1)), h('span', null, x)))));
  const stepsFor = (s) => lampSteps(STEPS, STEPS.findIndex(([k]) => k === s), { done: s === 'done', label: 'Steps of the move' });

  function paintMove() {
    const m = S.move;
    if (!m) { replace(moveBox, h('p', { class: 'faint small' }, S.err ? `Could not read the move: ${errText(S.err)}` : 'Reading the move…')); return; }
    const c = m.code;
    const where = c?.anywhere ? 'from anywhere' : 'only from this home network';
    const parts = [];
    if (S.err) parts.push(callout('warn', 'The page lost touch with the server', `${errText(S.err)} It keeps trying; the move itself goes on.`));
    if (m.problem) parts.push(callout('warn', 'The move has a problem', m.problem.message || m.problem.code));
    switch (m.state) {
      case 'none':
      case 'expired': {
        const ended = m.last_cancelled && !m.last_undone_ms;
        if (m.state === 'expired') parts.push(callout('info', 'The last move code expired unused', `It stopped working ${at(c?.expires_ms)}. Make a new one when the new server is ready.`));
        else if (ended) {
          parts.push(h('div', { class: 'mv-carry', cover: null },
            callout('warn', 'A move stopped part way', `It was ended ${at(m.last_cancelled.at_ms)} (${m.last_cancelled.why}). Nothing on this server changed.`),
            howto('Carry it on', 'Make a new move code here.',
              h('span', null, 'On the new server, under ', h('b', null, 'Carry on with a new move code'), ', type the same address as before and the new code.'),
              'It goes on from where it stopped: what was copied is kept, nothing is copied twice.')));
        } else parts.push(callout('info', 'No move code waits', 'Make one when the new server is installed and shows its first page. Nothing changes here until a new server uses it.'));
        if (m.last_undone_ms) parts.push(note(`A move was undone ${at(m.last_undone_ms)}; this server takes uploads.`));
        parts.push(h('div', { class: 'row' }, btn(ended ? 'Make a new code to carry on' : 'Make a move code', makeCode, { kind: 'go', icon: 'share' })));
        break;
      }
      case 'offered':
        parts.push(stepsFor('offered'));
        parts.push(S.codeText
          ? h('div', { class: 'stack' }, lcd(S.codeText, { label: 'Move code', big: true, note: `Shown only now. Works once, until ${at(c?.expires_ms)} (24 hours), ${where}.` }), copyBtn(S.codeText, 'Copy the code', { sm: false, kind: 'go' }))
          : callout('info', `A move code is waiting (code ${c?.id || DASH})`, `Made ${at(c?.made_ms)}, works until ${at(c?.expires_ms)}, ${where}. The code itself is shown only when made: without it, make a new one (this one then stops working).`));
        parts.push(howto('On the new server',
          h('span', null, 'Open its address. On its first page type its set-up code (from its log), then choose ', h('b', null, 'Move here from another server'), '.'),
          'Type this server\'s address, the one the CAN Bridge sends to, and the move code.',
          'Keep this server on. This panel shows the copy as it goes.'));
        if (c?.wrong_tries) parts.push(callout('warn', `${count(c.wrong_tries)} wrong ${c.wrong_tries === 1 ? 'try' : 'tries'} with a code`, 'If that was not you, cancel the code.'));
        parts.push(h('div', { class: 'row' }, btn('Cancel the code', cancel, { icon: 'x' }), btn('Make a new code', makeCode, { kind: 'ghost', icon: 'reset' })));
        break;
      case 'copying':
      case 'paused':
        parts.push(stepsFor(m.state));
        parts.push(callout('ok', m.state === 'paused' ? `Last round: ${m.peer?.address || 'the new server'} takes the last changes` : `${m.peer?.address || 'The new server'} is copying this server`,
          m.state === 'paused' ? 'Uploads wait a few seconds so nothing is missed; the CAN Bridge sends them again by itself.' : 'This server keeps taking uploads; the new server copies what arrives too.',
          ` Started ${at(m.peer?.claimed_ms)}, last heard from ${m.peer?.last_seen_ms ? ago(m.peer.last_seen_ms) : DASH}${m.peer?.app ? `, Sentry Mate ${m.peer.app}` : ''}.`));
        parts.push(progress(m.progress));
        parts.push(note(h('b', null, 'If it stops half way: '), 'nothing here changes. The new server tries again by itself and carries on where it stopped. Keep this server on until the move says Done.'));
        parts.push(skipped(m.skipped));
        parts.push(h('div', { class: 'row' }, btn('End the move', cancel, { icon: 'x' })));
        break;
      case 'moved':
      case 'done':
        parts.push(stepsFor(m.state));
        parts.push(m.state === 'done'
          ? callout('ok', `Moved to ${m.moved?.to || 'the new server'}: you can turn this server off`, `The new server took over ${at(m.moved?.switched_ms || m.moved?.at_ms)}. This server takes no uploads or changes any more; reading still works.`)
          : callout('warn', `Moving to ${m.moved?.to || 'the new server'}: keep this server on`, 'The new server is switching to the copy. This server takes no uploads or changes now. It says Done in a moment.'));
        parts.push(howto('What is left to do',
          'Sign in on the new server with your account from here: accounts came along.',
          h('span', null, 'In the CAN Bridge app, under Second Sentry Six Manager, change only the server address to ', code(`https://${m.moved?.to || 'the-new-server'}:10100/api/can/logs`, { long: true }), '. The token and the certificate SHA-256 came along.'),
          m.state === 'done' ? 'Turn this server off (on TrueNAS: Apps, this Sentry Mate app, Stop). Keep its folders until the new server has run a few days.' : 'Wait for Done before you turn this server off.'));
        parts.push(skipped(m.skipped));
        parts.push(h('div', { class: 'mv-undo' }, h('p', null, h('b', null, 'Changed your mind? '), 'Undo the move: this server takes uploads again. Stop the new server first.'), btn('Undo the move', undo, { kind: 'danger', icon: 'reset' })));
        break;
      default:
        parts.push(callout('info', `State: ${m.state}`));
    }
    replace(moveBox, parts);
  }

  /* ---------- export file */
  function scheduleExp() { clearTimeout(expTimer); if (S.exp?.state === 'preparing') expTimer = setTimeout(refreshExp, 1000); }
  async function refreshExp() {
    try {
      const e = S.exp ? await ctx.api.get(`system/exports/${enc(S.exp.id)}`) : null;
      if (!ctx.alive()) return;
      const was = S.exp?.state;
      S.exp = e;
      if (e && was === 'preparing' && e.state === 'ready' && S.wantDownload) { S.wantDownload = false; download(e); }
    } catch (err) { if (!ctx.alive()) return; if (err.status === 404) S.exp = null; else S.expErr = err; }
    paintExp(); scheduleExp();
  }
  function download(e) {
    if (!e?.url) return;
    const a = h('a', { href: e.url, download: e.file_name || '', hidden: true });
    document.body.append(a); a.click(); a.remove();
    toast(`Downloading ${e.file_name}. Your browser shows where it goes.`);
  }
  async function makeExport() {
    const r = await askPassword(ctx, {
      title: 'Download an export file', submit: 'Make the file',
      intro: h('div', { class: 'stack' },
        h('p', null, 'The server makes one file with everything, in a few seconds (uploads wait for those seconds), then your browser downloads it.'),
        h('p', null, 'It opens only with this server\'s ', h('b', null, 'recovery key'), ' (the SMRK- line saved at set-up). It never holds the key file, so without the recovery key it is useless to anyone who takes it.')),
      run: (cred) => ctx.api.post('system/exports', cred).catch(async (e) => {
        if (e.code !== 'export_preparing') throw e;
        return (await ctx.api.get('system/exports')).items?.[0] || null;
      }),
    });
    if (!r || !ctx.alive()) return;
    S.exp = r; S.expErr = null; S.wantDownload = true;
    paintExp(); scheduleExp();
  }
  async function removeExport() {
    if (!(await confirmDialog('The file is removed from this server. A copy you downloaded stays where you put it.', { title: 'Remove the export file?', ok: 'Remove it', danger: true }))) return;
    try { await ctx.api.del(`system/exports/${enc(S.exp.id)}`); S.exp = null; toast('The export file was removed from the server.'); } catch (e) { toast(`Could not remove it: ${errText(e)}`, { kind: 'error' }); }
    if (ctx.alive()) paintExp();
  }
  function paintExp() {
    const e = S.exp;
    const parts = [];
    if (S.expErr) parts.push(callout('warn', 'Could not read the export', errText(S.expErr)));
    if (!e || e.state === 'expired') {
      if (e?.state === 'expired') parts.push(note(`The last export file (${e.file_name}) was removed ${at(e.expires_ms)}. Make a new one.`));
      else parts.push(callout('info', 'No export file on the server', 'Make one when you need it; it is kept on the server for a while, then removed by itself.'));
      parts.push(h('div', { class: 'row' }, btn('Download export file', makeExport, { kind: 'go', icon: 'download' })));
    } else if (e.state === 'preparing') {
      parts.push(h('div', { class: 'mv-making' }, h('span', { class: 'mv-spin', 'aria-hidden': 'true' }), h('p', null, h('b', null, 'Making the file… '), 'a few seconds. The download starts by itself.')));
    } else {
      parts.push(h('div', { class: 'mv-file' },
        h('p', { class: 'mv-file-name' }, icon('download', 'i--sm'), code(e.file_name, { long: true })),
        h('p', { class: 'mv-file-meta' }, `${bytes(e.size ?? e.bytes)} · ${count(e.files)} files · made ${at(e.made_ms)} · kept until ${at(e.expires_ms)}`, e.sent_bytes ? ` · ${bytes(e.sent_bytes)} downloaded so far` : null),
        h('div', { class: 'row' },
          h('a', { class: 'btn btn--go', href: e.url, download: e.file_name }, icon('download'), h('span', null, 'Download')),
          btn('Make a new one', makeExport, { kind: 'ghost', icon: 'reset' }),
          btn('Remove from the server', removeExport, { kind: 'ghost', icon: 'x' })),
        skipped(e.skipped)));
    }
    parts.push(note(h('b', null, 'On the new server: '), 'choose Restore from an export file on its first page, choose this file and type the recovery key. A cut download carries on when you press Download again.'));
    replace(expBox, parts);
  }

  const el = hud({ title: 'Move to another server', icon: 'share', id: 'ss-move', tone: 'orange', side: tag('Admins', 'mag') },
    h('div', { class: 'stack' },
      h('p', { class: 'chan-text' }, 'Take everything to a new Sentry Mate server: logs, cars, accounts, upload tokens and the certificate. No shell and no commands: start the new server with empty folders, then use one of these. The CAN Bridge keeps its token and its certificate SHA-256; at most its server address changes.'),
      h('div', { class: 'mv-ways' },
        h('section', { class: 'mv-way', 'aria-labelledby': 'mv-a' },
          h('h3', { class: 'mv-way-head', id: 'mv-a' }, h('span', { class: 'mv-way-n' }, 'A'), h('span', null, 'With a move code')),
          h('p', { class: 'mv-when' }, h('b', null, 'When: '), 'both servers are on at the same time and the new one can reach this one. This server keeps taking uploads while the new one copies, and the new one takes over at the end.'),
          moveBox),
        h('section', { class: 'mv-way', 'aria-labelledby': 'mv-b' },
          h('h3', { class: 'mv-way-head', id: 'mv-b' }, h('span', { class: 'mv-way-n' }, 'B'), h('span', null, 'With an export file')),
          h('p', { class: 'mv-when' }, h('b', null, 'When: '), 'the two servers can\'t reach each other, this one will be off before the new one starts, or you want a copy to keep. Uploads that arrive after it was made are not in it.'),
          expBox)),
      h('p', { class: 'note' }, icon('lock', 'i--sm'), h('span', null, `Neither way changes this server's data. Codes, passwords and recovery keys travel only over HTTPS. Times are in ${zoneText}.`))));

  async function load() {
    try {
      const [m, ex] = await Promise.all([ctx.api.get('system/move'), ctx.api.get('system/exports').catch(() => null)]);
      if (!ctx.alive()) return;
      setMove(m);
      S.exp = ex?.items?.[0] || null;
    } catch (e) { if (!ctx.alive()) return; S.err = e; }
    paintMove(); paintExp(); schedule(); scheduleExp();
  }
  paintMove(); paintExp();
  return { el, ready: load() };
}
