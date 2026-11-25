// Settings: Server and access, "Move to another server" (admin only; part of pages/settings-server.js).
// Two ways to take everything to a new Sentry Mate server, with no shell:
//   - a move code (POST /system/move): both servers are on at once; the new one copies over the network,
//     this one keeps taking uploads, and the new one takes over by itself. Shown once, big, with Copy.
//     While it runs this panel polls GET /system/move once a second: who copies, how much, which step, and the
//     move's log underneath (its `log`, newest last, kept by the server: a reload shows it again; pages/move-log.js).
//     Afterwards it says where the data went, with "Undo the move" (POST /system/move/undo).
//   - an export file (POST /system/exports): one file with everything, for servers that can't reach each
//     other, or to keep as a backup. It opens only with the recovery key and never holds the key file.
//     Its progress follows `job` events of kind export (live events, polling GET /system/exports only while the
//     live stream is down), painted in place.
// Making a code, undoing a move and making an export ask for the person's own password again.

import { h, replace, setVars } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { icon } from '../core/icons.js';
import { panel, pill } from '../core/page.js';
import { field, form } from '../core/form.js';
import { codeAttrs, codeValue, CODE_HINT } from '../core/code.js';
import { errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { dateTime, dur, ago, DASH } from '../core/fmt.js';
import { dayKey } from '../shared/time.js';
import { exportModel, exportPollMs } from '../shared/exportjob.js';
import { follow, jobOf } from '../shared/live.js';
import { morphChildren } from '../core/morph.js';
import { moveLog } from './move-log.js';

const enc = encodeURIComponent;
const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const count = (n) => (n == null ? DASH : format(n, 'count'));
const pct = (a, b) => (a != null && b ? Math.max(0, Math.min(100, (a / b) * 100)) : 0);
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : e?.message || errorWords(e).text);
export const STAY = Symbol('stay');   // run() answers this to keep the dialog open
const LIVE = new Set(['offered', 'copying', 'paused', 'moved']);

/** A code split into its groups, so it wraps between groups and never inside one. */
export function codeGroups(text, cls = 'mv-code') {
  return h('p', { class: cls, 'aria-label': text.split('').join(' ') },
    String(text).split('-').map((g, i, all) => h('span', { class: 'mv-group' }, g, i < all.length - 1 ? '-' : '')));
}

/** A Copy button for a text, with its own "Copied." line. */
export function copyButton(text, label = 'Copy', { primary = false } = {}) {
  const said = h('span', { class: 'mv-copied', 'aria-live': 'polite' });
  const btn = h('button', { class: ['btn', primary && 'btn--primary'], type: 'button', onclick: async () => {
    try { await navigator.clipboard.writeText(text); said.textContent = 'Copied.'; } catch { said.textContent = 'This browser did not let the page copy: select the text and copy it by hand.'; }
  } }, icon('check', 'ico--sm'), label);
  return h('span', { class: 'mv-copy' }, btn, said);
}

/** "about 2 min to go", or "almost done" under a second (never "about 0 s"). */
export const etaText = (s) => (s == null ? null : s < 1 ? 'almost done' : `about ${dur(s * 1000)} to go`);

/**
 * The recovery key as a text file to keep for years: the server and the full date (with the year) in the file
 * and in its name, so two servers' keys never look alike.
 */
export function downloadKey(key, { ms, tz, why = 'set up' }) {
  const host = globalThis.location.host;
  const day = dayKey(ms, tz);
  const safeHost = globalThis.location.hostname.replace(/[^A-Za-z0-9.-]+/g, '_') || 'server';
  const text = `Sentry Mate recovery key\r\n\r\n${key}\r\n\r\nFor the server at ${host}, ${why} ${dateTime(ms, tz, { year: true, month: 'long', weekday: 'long' })} (${day}).\r\n`
    + 'It opens this server\'s data if its key file is lost, and the export files it makes.\r\n'
    + 'Keep it away from the server: in a password manager, or on paper.\r\n';
  const href = URL.createObjectURL(new Blob([text], { type: 'text/plain;charset=utf-8' }));
  const a = h('a', { href, download: `sentry-mate-recovery-key-${safeHost}-${day}.txt`, hidden: true });
  document.body.append(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(href), 30000);
}

/** The key, big and in groups, with Copy and Download. look: 'su' on the set-up page, 'mv' in Settings. */
export function keyBox(key, file, look = 'su') {
  const su = look === 'su';
  return h('div', { class: su ? 'su-keybox' : 'mv-codebox' },
    h('p', { class: su ? 'su-keybox-head' : 'mv-codebox-head' }, 'Recovery key'),
    codeGroups(key, su ? 'mv-code su-key' : 'mv-code mv-key'),
    h('div', { class: ['row', su ? 'su-acts' : 'mv-acts'] }, copyButton(key, 'Copy', { primary: true }),
      h('button', { class: 'btn', type: 'button', onclick: () => downloadKey(key, file) }, icon('download', 'ico--sm'), 'Download as a file')));
}

/**
 * The person's own password again (and the two-step code when the account asks for one) before a dangerous
 * action. run(credentials) does the call and returns its answer; the dialog stays open on a wrong password.
 */
export function askPassword(ctx, { title, intro, submit, danger = false, extra = [], run }) {
  const me = ctx.me || {};
  const pass = field({ label: 'Your password', name: 'password', type: 'password', required: true,
    hint: `The password you sign in with as ${me.name || 'this account'}.`, attrs: { autocomplete: 'current-password' } });
  const two = field({ label: 'Code from your authenticator app', name: 'two_step', hint: CODE_HINT,
    attrs: codeAttrs() });
  two.el.hidden = !(me.two_step || me.two_step_required);
  let result = null;
  const f = form({
    fields: [...extra, pass, two], submit,
    secondary: h('button', { class: 'btn btn--ghost', type: 'button', onclick: (e) => e.target.closest('dialog')?.close() }, 'Cancel'),
    async onSubmit(v) {
      const cred = { password: v.password };
      if (!two.el.hidden && v.two_step) cred.two_step = codeValue(v.two_step);
      try {
        const r = await run(cred, v);
        if (r === STAY) return;
        result = r;
      } catch (e) {
        if (e.code === 'wrong_password') { pass.setError(v.password ? 'That is not your password. Type the one you sign in with.' : 'Type your password to go on.'); pass.input.select(); return; }
        if (e.code === 'wrong_two_step') { two.el.hidden = false; two.setError(v.two_step ? 'That code is not right. Type the one your authenticator app shows now.' : 'Your account has two-step sign-in: type the code from your authenticator app as well.'); two.input.focus(); return; }
        if (e.status === 429) { f.setMessage(`Too many wrong passwords. Wait ${e.retryAfterMs ? Math.ceil(e.retryAfterMs / 1000) : 60} s, then try again.`); return; }
        throw e;
      }
      f.el.closest('dialog')?.close();
    },
  });
  if (danger) f.el.querySelector('button[type="submit"]')?.classList.replace('btn--primary', 'btn--danger');
  const done = openDialog({ title, body: [intro, f.el], actions: [] });
  // focused without scrolling: a long intro (steps to follow) stays in view from its first line
  pass.input.focus({ preventScroll: true });
  return done.then(() => result);
}

/* ---------------------------------------------------------------- the recovery key, from Settings (admin) */

/** The key in a dialog, with Copy, Download and the tick "I have saved it" (POST /system/recovery-key/saved). */
async function keyDialog(ctx, key, { title, intro, file }) {
  const saved = h('input', { type: 'checkbox', id: 'mv-key-saved' });
  const err = h('p', { class: 'form-error', hidden: true, role: 'alert' });
  const say = (t) => { replace(err, icon('warn'), t); err.hidden = false; };
  saved.addEventListener('change', () => { if (saved.checked) err.hidden = true; });
  let ok = false;
  await openDialog({
    title, wide: true,
    body: h('div', { class: 'mv-keydlg' }, intro, keyBox(key, file, 'mv'),
      h('label', { class: 'check mv-saved', for: 'mv-key-saved' }, saved, h('span', null, 'I have saved it somewhere safe, away from this server')),
      h('p', { class: 'note' }, 'Closed without the tick? Nothing is lost: the server keeps its copy, and this page can show the key again.'),
      err),
    actions: [
      { label: 'Close', value: false },
      { label: 'Done: delete the server\'s copy', kind: 'primary', onClick: async () => {
        if (!saved.checked) { say('Tick this first: save the key (Copy or Download), then tick "I have saved it".'); saved.focus(); return false; }
        try { await ctx.api.post('system/recovery-key/saved', {}); } catch (e) {
          say(`The server could not delete its copy (${errText(e)}). Press Done again; until it works the copy stays on the server.`);
          return false;
        }
        ok = true;
        return true;
      } },
    ],
  });
  return ok;
}

/** "Show the key again": the key still waiting in RECOVERY-KEY.txt (POST /system/recovery-key/show). Resolves true once saved. */
export async function showRecoveryKey(ctx, { tz }) {
  const r = await askPassword(ctx, {
    title: 'Show the recovery key again', submit: 'Show the key',
    intro: h('p', { class: 'mv-intro' }, 'The server still keeps the key it made when it was set up, until you say it is saved. Type your password to see it once more.'),
    run: (cred) => ctx.api.post('system/recovery-key/show', cred).catch((e) => {
      if (e.code !== 'recovery_key_saved') throw e;
      flash('The server deleted its copy already: the key was saved. If it is lost, make a new one.', { kind: 'info' });
      return { gone: true };
    }),
  });
  if (!r || r.gone || !ctx.alive()) return !!r?.gone;
  return keyDialog(ctx, r.recovery_key, {
    title: 'Your recovery key', file: { ms: ctx.api.now(), tz, why: 'shown again' },
    intro: h('div', { class: 'mv-intro' }, h('p', null, 'Keep it in a password manager or on paper, ', h('b', null, 'away from the server'), '. Then tick below: the server deletes its copy, and nobody can show the key again.')),
  });
}

/** "Make a new recovery key" (POST /system/recovery-key/new): the old one stops working. Resolves true once
 * saved, 'unsaved' when the dialog closed without the tick, false when nothing was made. */
export async function newRecoveryKey(ctx, { tz }) {
  const r = await askPassword(ctx, {
    title: 'Make a new recovery key', submit: 'Make a new key', danger: true,
    intro: h('div', { class: 'mv-intro' },
      h('p', null, 'For a key that was lost, or seen by someone it should not have been. ', h('b', null, 'The old key stops working at once.')),
      h('p', null, 'Export files and copies of the data folder made before still open only with the old key: keep the old one as long as you keep those.')),
    run: (cred) => ctx.api.post('system/recovery-key/new', cred),
  });
  if (!r || !ctx.alive()) return false;
  flash('A new recovery key was made. The old one no longer works.');
  // not ticked: the server keeps the new key's file, and Settings shows "Show the key again"
  return (await keyDialog(ctx, r.recovery_key, {
    title: 'Your new recovery key', file: { ms: r.made_ms || ctx.api.now(), tz, why: 'new key made' },
    intro: h('div', { class: 'mv-intro' }, h('p', null, h('b', null, 'Shown only now.'), ' Keep it in a password manager or on paper, away from the server, then tick below.')),
  })) || 'unsaved';
}

/** "I have saved it" without showing the key: for someone who saved it on the set-up page but never ticked. */
export async function markKeySaved(ctx) {
  const ok = await confirmDialog('Only if the SMRK- key is in a password manager or on paper, away from the server. The server then deletes its copy (RECOVERY-KEY.txt), and nobody can show the key again.',
    { title: 'Have you saved the recovery key?', ok: 'Yes, delete the server\'s copy' });
  if (!ok) return false;
  try { await ctx.api.post('system/recovery-key/saved', {}); } catch (e) { flash(`The server could not delete its copy: ${errText(e)} Try again.`, { kind: 'error' }); return false; }
  flash('Saved. The server deleted its copy of the recovery key.');
  return true;
}

/** The panel. tz: the zone for times on the page. */
export function movePanel(ctx, { tz, zone }) {
  const S = { move: null, code: null, codeText: null, exp: null, err: null };
  const moveBox = h('div', { class: 'mv-state', 'aria-live': 'polite' });
  // outside moveBox, which is drawn anew each time: the list keeps its scroll and only gains lines
  const log = moveLog({ tz, file: 'sentry-mate-move-log' });
  log.el.hidden = true;
  const expBox = h('div', { class: 'mv-export', 'aria-live': 'polite' });
  const t = (ms) => (ms == null ? DASH : dateTime(ms, tz));

  /* ---------------------------------------------------------------- polling */
  // the move has no event: its state is read every second or two while one is under way
  let timer = null;
  ctx.onCleanup(() => clearTimeout(timer));
  function schedule() {
    clearTimeout(timer);
    if (!S.move || !LIVE.has(S.move.state)) return;
    timer = setTimeout(refresh, S.move.state === 'offered' ? 2000 : 1000);
  }
  async function refresh() {
    try {
      const m = await ctx.api.get('system/move');
      if (!ctx.alive()) return;
      setMove(m);
      S.err = null;
    } catch (e) {
      if (!ctx.alive()) return;
      S.err = e;
    }
    paintMove();
    schedule();
  }
  function setMove(m) {
    if (m.state === 'none' || m.state === 'expired' || (m.code && S.code && m.code.id !== S.code)) { S.codeText = null; S.code = null; }
    S.move = m;
  }

  /* ---------------------------------------------------------------- actions */
  async function makeCode() {
    const busy = S.move && (S.move.state === 'copying' || S.move.state === 'paused');
    if (busy && !(await confirmDialog(`${S.move.peer?.address || 'A new server'} is copying this server now. A new code ends that move: the copy there stops, and this server's data is not changed.`, { title: 'End the move under way?', ok: 'End it and make a new code', danger: true }))) return;
    const anywhere = field({ label: 'From anywhere, not only from this home network', name: 'anywhere', type: 'checkbox', value: false,
      hint: 'Leave it off when the new server is in the same house. Tick it for a new server somewhere else (another house, a rented server); it then needs to reach this server\'s upload port.' });
    const r = await askPassword(ctx, {
      title: 'Make a move code', submit: 'Make the code', extra: [anywhere],
      intro: h('p', { class: 'mv-intro' }, 'The code lets one new Sentry Mate server copy everything from this one: logs, cars, accounts, upload tokens and the certificate. It works once, for 24 hours. ',
        S.move?.state === 'offered' ? h('b', null, 'The code waiting now stops working.') : null),
      run: (cred, v) => ctx.api.post('system/move', { ...cred, anywhere: !!v.anywhere, ...(busy ? { replace: true } : {}) }),
    });
    if (!r || !ctx.alive()) return;
    S.codeText = r.move_code;
    S.code = r.code?.id || null;
    S.move = r;
    paintMove(); schedule();
    flash('Move code made. Type it on the new server\'s first page.');
  }

  async function cancelCode() {
    const copying = S.move && (S.move.state === 'copying' || S.move.state === 'paused');
    const ok = await confirmDialog(copying
      ? `The move to ${S.move.peer?.address || 'the new server'} ends now. Nothing on this server changes; it keeps taking uploads. What the new server copied stays there: with a new code from this server it carries on from there.`
      : 'The code stops working at once. Nothing on this server changes. You can make a new code at any time.',
    { title: copying ? 'End the move?' : 'Cancel the move code?', ok: copying ? 'End the move' : 'Cancel the code', danger: true });
    if (!ok) return;
    try {
      const r = await ctx.api.del('system/move');
      if (!ctx.alive()) return;
      setMove(r);
      flash(r.cancelled === 'ended' ? 'The move was ended. This server keeps taking uploads.' : r.cancelled === 'revoked' ? 'The move code was cancelled: it no longer works.' : 'There was nothing to cancel.', { kind: 'info' });
    } catch (e) {
      flash(e.code === 'finished' ? 'Too late to cancel: the move finished, the new server holds the data. Use Undo the move instead.' : `Could not cancel: ${errText(e)}`, { kind: 'error' });
      await refresh();
      return;
    }
    paintMove(); schedule();
  }

  async function undo() {
    const sure = field({ label: 'The new server is stopped, or I will stop it now', name: 'confirm', type: 'checkbox', value: false,
      hint: 'If both servers take uploads, they drift apart: each gets only some of the logs.' });
    const r = await askPassword(ctx, {
      title: 'Undo the move', submit: 'Undo the move', danger: true, extra: [sure],
      intro: h('div', { class: 'mv-intro' },
        h('p', null, 'This server takes uploads again at once, as before the move. Its data is as it was when the move finished: what reached the new server since then is not here.'),
        h('p', null, h('b', null, 'Stop the new server first'), ' (on TrueNAS: Apps, its Sentry Mate app, Stop).')),
      run: (cred, v) => {
        if (!v.confirm) { sure.setError('Tick this first: stop the new server before you undo.'); sure.input.focus(); return STAY; }
        return ctx.api.post('system/move/undo', { ...cred, confirm: true });
      },
    });
    if (!r || !ctx.alive()) return;
    setMove(r);
    paintMove(); schedule();
    flash('The move was undone. This server takes uploads again.');
  }

  /* ---------------------------------------------------------------- painting the move */
  const STEPS = [
    ['offered', 'Code made', 'Waits for it'],
    ['copying', 'Copying', 'Uploads go on'],
    ['paused', 'Last round', 'Seconds'],
    ['moved', 'Switching', 'Keep it on'],
    ['done', 'Done', 'Turn it off'],
  ];
  function steps(state) {
    const at = STEPS.findIndex(([k]) => k === state);
    return h('ol', { class: 'mv-steps', 'aria-label': 'Steps of the move' }, STEPS.map(([k, l, sub], i) => {
      const st = i < at || state === 'done' ? 'done' : i === at ? 'now' : 'later';
      return h('li', { class: ['mv-step', `mv-step--${st}`], 'aria-current': st === 'now' ? 'step' : null },
        h('span', { class: 'mv-step-n' }, st === 'done' ? icon('check', 'ico--sm') : String(i + 1)),
        h('span', { class: 'mv-step-t' }, h('b', null, l), h('span', null, sub)));
    }));
  }
  function progress(p, { label = 'Copied' } = {}) {
    if (!p) return null;
    const bar = h('div', { class: 'bar mv-bar', role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': String(Math.round(pct(p.bytes_done, p.bytes_total))) }, h('i'));
    setVars(bar, { '--w': `${pct(p.bytes_done, p.bytes_total).toFixed(1)}%` });
    return h('div', { class: 'mv-progress' }, bar,
      h('p', { class: 'mv-bar-text' }, h('b', null, bytes(p.bytes_done)), ` of ${bytes(p.bytes_total)} ${label.toLowerCase()} (${format(pct(p.bytes_done, p.bytes_total), 'number', { digits: 0 })} %)`,
        ` · ${count(p.files_done)} of ${count(p.files_total)} files`,
        p.round > 1 ? ` · round ${p.round}` : null,
        p.bytes_per_s ? ` · ${bytes(p.bytes_per_s)} per second` : null,
        p.eta_s != null && p.bytes_per_s ? ` · ${etaText(p.eta_s)}` : null));
  }
  const line = (tone, head, ...text) => h('div', { class: ['mv-line', `mv-line--${tone}`] },
    h('span', { class: 'mv-line-ic' }, icon(tone === 'ok' ? 'check' : tone === 'warn' ? 'warn' : 'info')),
    h('div', null, h('p', { class: 'mv-line-head' }, head), text.length ? h('p', { class: 'mv-line-text' }, text) : null));
  const skippedNote = (list) => (list?.length ? h('p', { class: 'note' }, icon('info', 'ico--sm'), ` Not carried, because this server can't read ${list.length === 1 ? 'it' : 'them'}: `,
    list.map((s, i) => [i ? ', ' : '', h('span', { class: 'mono' }, s.path)]), '.') : null);
  const btn = (label, onclick, { kind = '', ic = null } = {}) => h('button', { class: ['btn', kind && `btn--${kind}`], type: 'button', onclick }, ic ? icon(ic, 'ico--sm') : null, label);

  function paintMove() {
    const m = S.move;
    if (!m) { replace(moveBox, h('p', { class: 'faint' }, S.err ? `Could not read the move: ${errText(S.err)}` : 'Loading…')); return; }
    const parts = [];
    const c = m.code;
    const where = c?.anywhere ? 'from anywhere' : 'only from this home network';
    if (S.err) parts.push(h('p', { class: 'form-error' }, icon('warn'), `The page lost touch with the server (${errText(S.err)}). It keeps trying; the move itself goes on.`));
    switch (m.state) {
      case 'none':
      case 'expired':
        parts.push(m.state === 'expired'
          ? line('info', 'The last move code expired unused', `It stopped working ${t(c?.expires_ms)}. Make a new one when the new server is ready.`)
          : line('info', 'No move code waits', 'Make one when the new server is installed and shows its first page. Nothing changes here until a new server uses the code.'));
        if (m.last_undone_ms) parts.push(h('p', { class: 'note' }, `A move was undone ${t(m.last_undone_ms)}; this server takes uploads.`));
        else if (m.last_cancelled) parts.push(h('p', { class: 'note' }, `The last move was cancelled ${t(m.last_cancelled.at_ms)} (${m.last_cancelled.why}).`));
        parts.push(h('div', { class: 'row mv-acts' }, btn('Make a move code', makeCode, { kind: 'primary', ic: 'share' })));
        break;
      case 'offered':
        parts.push(steps('offered'));
        if (S.codeText) {
          parts.push(h('div', { class: 'mv-codebox' },
            h('p', { class: 'mv-codebox-head' }, 'The move code'),
            codeGroups(S.codeText),
            h('div', { class: 'row mv-acts' }, copyButton(S.codeText, 'Copy the code', { primary: true })),
            h('p', { class: 'mv-codebox-note' }, h('b', null, 'Shown only now.'), ` Works once, until ${t(c?.expires_ms)} (24 hours), ${where}. If you lose it, make a new one: this one then stops working.`)));
        } else {
          parts.push(line('info', `A move code is waiting (code ${c?.id || DASH})`,
            `Made ${t(c?.made_ms)}, works until ${t(c?.expires_ms)}, ${where}. The code itself is shown only when it is made: if you don't have it any more, make a new one (this one then stops working).`));
        }
        parts.push(h('div', { class: 'mv-next' },
          h('p', { class: 'mv-next-head' }, 'On the new server'),
          h('ol', { class: 'mv-howto' },
            h('li', null, 'Open its address. On the first page type its set-up code (from its log), then choose ', h('b', null, 'Move here from another server'), '.'),
            h('li', null, 'Type this server\'s address, the one the CAN Bridge sends to, and the move code.'),
            h('li', null, 'Keep this server on. This page shows the copy as it goes.'))),
          c?.wrong_tries ? h('p', { class: 'form-error' }, icon('warn'), `${count(c.wrong_tries)} wrong ${c.wrong_tries === 1 ? 'try' : 'tries'} with a code against this server. If that was not you, cancel the code.`) : null);
        parts.push(h('div', { class: 'row mv-acts' }, btn('Cancel the code', cancelCode, { ic: 'x' }), btn('Make a new code', makeCode, { kind: 'ghost', ic: 'reset' })));
        break;
      case 'copying':
      case 'paused':
        parts.push(steps(m.state));
        parts.push(line('ok', m.state === 'paused' ? `Last round: ${m.peer?.address || 'the new server'} takes the last changes` : `${m.peer?.address || 'The new server'} is copying this server`,
          m.state === 'paused' ? 'Uploads wait a few seconds so nothing is missed; the CAN Bridge sends them again by itself.' : 'This server keeps taking uploads meanwhile; the new server copies what arrives too.',
          ` Started ${t(m.peer?.claimed_ms)}, last heard from ${m.peer?.last_seen_ms ? ago(m.peer.last_seen_ms) : DASH}${m.peer?.app ? `, Sentry Mate ${m.peer.app}` : ''}.`));
        parts.push(progress(m.progress));
        parts.push(h('p', { class: 'note' }, h('b', null, 'If it stops half way:'), ' nothing on this server changes. The new server tries again by itself and carries on where it stopped. Keep this server on until the move says Done.'));
        parts.push(skippedNote(m.skipped));
        parts.push(h('div', { class: 'row mv-acts' }, btn('End the move', cancelCode, { ic: 'x' })));
        break;
      case 'moved':
      case 'done':
        parts.push(steps(m.state));
        parts.push(m.state === 'done'
          ? line('ok', `Moved to ${m.moved?.to || 'the new server'}: you can turn this server off`, `The new server took over ${t(m.moved?.switched_ms || m.moved?.at_ms)}. This server takes no uploads and no changes any more; reading still works.`)
          : line('warn', `Moving to ${m.moved?.to || 'the new server'}: keep this server on`, 'The new server is switching to the copy. This server takes no uploads or changes now. It says Done in a moment.'));
        parts.push(h('div', { class: 'mv-next' },
          h('p', { class: 'mv-next-head' }, 'What is left to do'),
          h('ol', { class: 'mv-howto' },
            h('li', null, 'Sign in on the new server with your account from here: accounts came along.'),
            h('li', null, 'In the CAN Bridge app, under Second Sentry Six Manager, change only the server address to ', h('span', { class: 'mono' }, `https://${m.moved?.to || 'the-new-server'}:10100/api/can/logs`), '. Keep the token and the certificate SHA-256: they came along.'),
            h('li', null, m.state === 'done' ? 'Turn this server off (on TrueNAS: Apps, this Sentry Mate app, Stop). Keep its folders until the new server has run a few days.' : 'Wait for Done before you turn this server off.'))));
        parts.push(skippedNote(m.skipped));
        parts.push(h('div', { class: 'mv-undo' },
          h('p', null, h('b', null, 'Changed your mind?'), ' Undo the move: this server takes uploads again. Stop the new server first.'),
          btn('Undo the move', undo, { kind: 'danger', ic: 'reset' })));
        break;
      default:
        parts.push(line('info', `State: ${m.state}`));
    }
    replace(moveBox, parts);
    const lines = m.log || [];
    log.el.hidden = lines.length === 0;
    log.update(lines);
  }

  /* ---------------------------------------------------------------- export file */
  // The status panel has the same rows in every state (none, making, stalled, failed, ready), each always there,
  // so the box keeps its size as the export goes on. The words come from shared/exportjob.js.
  // `job` events of kind export carry the ExportFile; GET /system/exports (at most one) is read at the start, after
  // a reconnect and, only while the live stream is down, as often as exportPollMs says
  function takeExp(e) {
    if (!ctx.alive()) return;
    const was = S.exp;
    S.exp = e || null; S.expErr = null;
    if (e && was?.id === e.id && was.state === 'preparing' && e.state === 'ready' && S.wantDownload) { S.wantDownload = false; download(e); }
    paintExp();
  }
  let expFirst = null;
  const expReady = new Promise((res) => { expFirst = res; });
  const expLive = follow(ctx.api, {
    type: 'job', match: jobOf('export'), pick: (d) => d.status,
    load: () => ctx.api.get('system/exports').then((x) => x?.items?.[0] || null),
    pollMs: () => exportPollMs(S.exp),
    onValue: (e) => { takeExp(e); expFirst(); },
    onError: (err) => { if (ctx.alive()) { S.expErr = err; paintExp(); } expFirst(); },
  });
  function download(e) {
    if (!e?.url) return;
    const a = h('a', { href: e.url, download: e.file_name || '', hidden: true });
    document.body.append(a); a.click(); a.remove();
    flash(`Downloading ${e.file_name}. Your browser shows where it goes.`);
  }
  async function makeExport() {
    const r = await askPassword(ctx, {
      title: 'Download an export file', submit: 'Make the file',
      intro: h('div', { class: 'mv-intro' },
        h('p', null, 'The server makes one file with everything in it, in three steps: it counts what goes in, waits for the writes under way, then takes a snapshot (uploads wait for that step). This page shows how far it got; you can close it and come back. Then your browser downloads the file.'),
        h('p', null, 'The file opens only with this server\'s ', h('b', null, 'recovery key'), ' (the SMRK- line you saved when it was set up). It never holds the key file, so without the recovery key it is useless to anyone who takes it.')),
      // one being made already (another tab, another admin): follow that one
      run: (cred) => ctx.api.post('system/exports', cred).catch(async (e) => {
        if (e.code !== 'export_preparing') throw e;
        return (await ctx.api.get('system/exports')).items?.[0] || null;
      }),
    });
    if (!r || !ctx.alive()) return;
    S.exp = r; S.expErr = null; S.wantDownload = true;
    paintExp(); expLive.refresh();
  }
  async function cancelExport() {
    if (!S.exp || !(await confirmDialog('The server stops making the file and keeps nothing of it. Uploads go on. You can make a new one at any time.', { title: 'Stop making the export file?', ok: 'Stop making it', danger: true }))) return;
    try {
      await ctx.api.del(`system/exports/${enc(S.exp.id)}`);
      S.exp = null; S.wantDownload = false;
      flash('The export file was cancelled.', { kind: 'info' });
    } catch (e) { flash(`Could not cancel it: ${errText(e)}`, { kind: 'error' }); }
    if (ctx.alive()) { paintExp(); expLive.refresh(); }
  }
  async function removeExport() {
    if (!(await confirmDialog('The file is removed from this server. A copy you downloaded stays where you put it.', { title: 'Remove the export file?', ok: 'Remove it', danger: true }))) return;
    try { await ctx.api.del(`system/exports/${enc(S.exp.id)}`); S.exp = null; flash('The export file was removed from the server.'); } catch (e) { flash(`Could not remove it: ${errText(e)}`, { kind: 'error' }); }
    if (ctx.alive()) paintExp();
  }
  function expPanel(e) {
    const m = exportModel(e);
    const ready = m.kind === 'ready';
    const ic = { busy: 'download', warn: 'warn', ok: 'check', error: 'warn', info: 'download' }[m.tone] || 'info';
    const bar = h('div', { class: ['bar', 'xp-bar', m.percent == null && m.kind === 'busy' && 'xp-bar--unknown', m.tone === 'error' && 'bar--low', m.tone === 'warn' && 'bar--warm'],
      role: 'progressbar', 'aria-label': 'Export file', 'aria-valuemin': '0', 'aria-valuemax': '100',
      'aria-valuenow': m.percent == null ? null : String(Math.round(m.percent)), 'aria-valuetext': m.percentText || m.head }, h('i'));
    setVars(bar, { '--w': `${(m.percent ?? 0).toFixed(1)}%` });
    let line1 = m.amount && m.files ? `${m.amount} · ${m.files}` : m.amount || m.files;
    let line2 = m.speed;
    if (ready) {
      line1 = h('span', { class: 'mono' }, e.file_name);
      line2 = `Kept until ${t(e.expires_ms)} · ${e.sent_bytes ? `${bytes(e.sent_bytes)} downloaded so far` : 'not downloaded yet'}`;
    } else if (m.kind === 'failed') {
      line1 = `Code: ${m.problem.code}`;
      line2 = '';
    }
    let note = null;
    if (S.expErr) note = h('p', { class: 'xp-note-text' }, h('b', null, 'The page lost touch with the server. '), `It keeps trying; the export goes on there. (${errText(S.expErr)})`);
    else if (m.warn) note = h('p', { class: 'xp-note-text' }, m.warn.text);
    else if (m.problem) note = h('p', { class: 'xp-note-text' }, h('b', null, 'What to do: '), m.problem.todo);
    else if (m.sha256) note = h('p', { class: 'xp-note-text' }, h('b', null, 'SHA-256 '), h('span', { class: 'mono xp-sha' }, m.sha256));
    else if (m.checksum) note = h('p', { class: 'xp-note-text' }, m.checksum);
    else if (m.kind === 'busy') note = h('p', { class: 'xp-note-text' }, 'You can close this page: the server goes on, and the page shows it again when you come back.');
    else if (m.kind === 'none' || m.kind === 'expired') note = h('p', { class: 'xp-note-text' }, 'It takes seconds to minutes, most of it for the database. Uploads wait only for the last step.');
    const acts = [];
    if (m.kind === 'busy') acts.push(btn('Cancel', cancelExport, { ic: 'x' }));
    if (ready) {
      acts.push(h('a', { class: 'btn btn--primary', href: e.url, download: e.file_name }, icon('download', 'ico--sm'), 'Download'));
      acts.push(btn('Make new', makeExport, { kind: 'ghost', ic: 'reset' }));
      acts.push(btn('Remove', removeExport, { kind: 'ghost', ic: 'x' }));
    }
    if (m.kind === 'failed') acts.push(btn('Make a new one', makeExport, { kind: 'primary', ic: 'reset' }));
    if (m.kind === 'none' || m.kind === 'expired') acts.push(btn('Download export file', makeExport, { kind: 'primary', ic: 'download' }));
    return h('div', { class: ['xp', `xp--${m.kind}`, `xp--${m.tone}`], role: 'group', 'aria-label': 'Export file status' },
      h('div', { class: 'xp-head' },
        h('span', { class: 'xp-ic' }, icon(ic)),
        h('div', { class: 'xp-titles' },
          h('p', { class: 'xp-title' }, m.head),
          h('p', { class: 'xp-sub' }, m.sub)),
        h('span', { class: 'xp-pct' }, m.percentText)),
      h('ol', { class: 'xp-steps', 'aria-label': 'Steps of making the file' }, m.steps.map((st) =>
        h('li', { class: ['xp-step', `xp-step--${st.state}`], 'aria-current': st.state === 'now' ? 'step' : null },
          h('span', { class: 'xp-step-n' }, st.state === 'done' ? icon('check', 'ico--sm') : String(st.n)), h('span', null, st.label)))),
      bar,
      h('p', { class: 'xp-line' }, line1),
      h('p', { class: 'xp-line xp-line--2' }, line2),
      h('div', { class: ['xp-note', m.tone === 'warn' && 'xp-note--warn', m.tone === 'error' && 'xp-note--error'] }, note),
      h('div', { class: 'row xp-acts' }, acts));
  }
  function paintExp() {
    const e = S.exp;
    // only the words and figures that changed are written (an event can come twice a second)
    morphChildren(expBox, [
      expPanel(e),
      e?.state === 'ready' ? skippedNote(e.skipped) : null,
      h('p', { class: 'note' }, h('b', null, 'On the new server:'), ' choose Restore from an export file on its first page, choose this file and type the recovery key. A cut download carries on when you press Download again.'),
    ]);
  }

  /* ---------------------------------------------------------------- the panel */
  const el = panel({ title: 'Move to another server', icon: 'share', pad: true, id: 'move', cls: 'mv',
    side: h('span', { class: 'faint' }, 'Admins only') },
  h('p', { class: 'mv-lead' }, 'Take everything to a new Sentry Mate server: logs, cars, accounts, upload tokens and the certificate. No shell and no commands: start the new server with empty folders, then use one of these. ',
    'The CAN Bridge keeps its token and its certificate SHA-256; at most its server address changes.'),
  h('div', { class: 'mv-ways' },
    h('section', { class: 'mv-way', 'aria-labelledby': 'mv-code-title' },
      h('h3', { id: 'mv-code-title' }, icon('share', 'ico--sm'), 'With a move code'),
      h('p', { class: 'mv-when' }, h('b', null, 'When:'), ' both servers are on at the same time and the new one can reach this one (the same home network, or tick from anywhere). This server keeps taking uploads while the new one copies, and the new one takes over by itself at the end.'),
      moveBox, log.el),
    h('section', { class: 'mv-way', 'aria-labelledby': 'mv-file-title' },
      h('h3', { id: 'mv-file-title' }, icon('download', 'ico--sm'), 'With an export file'),
      h('p', { class: 'mv-when' }, h('b', null, 'When:'), ' the two servers can\'t reach each other, this one will be off before the new one starts, or you want a copy to keep as a backup. One file with everything; uploads that arrive after it was made are not in it.'),
      expBox)),
  h('p', { class: 'note mv-safe' }, icon('lock', 'ico--sm'), ' Neither way changes this server\'s data. Codes, passwords and recovery keys travel only over HTTPS. Times are in ', zone, '.'));

  async function load() {
    try {
      const m = await ctx.api.get('system/move');
      if (!ctx.alive()) return;
      setMove(m);
    } catch (e) { if (!ctx.alive()) return; S.err = e; }
    paintMove(); schedule();
  }
  paintMove(); paintExp();
  return { el, ready: Promise.all([load(), expReady]) };
}
