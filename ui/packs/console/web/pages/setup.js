// First run (/setup): the server has no account yet. A step log prints each finished step as '[ok] ...'
// and the current step as a form below it. Nothing here needs a shell.
//   1. the set-up code from the server's log (POST /setup/claim); the claim is a cookie, so a reload keeps it
//   2. one of three ways:
//      start fresh      POST /setup/fresh: the first admin, then the recovery key shown once (Copy, Download,
//                       "I have saved it": POST /system/recovery-key/saved)
//      move here        POST /setup/move-here { from, code }, followed with GET /setup/job once a second; when
//                       the old server is gone for good: POST /setup/move-here/finish with its recovery key
//      restore          POST /setup/restore (file head and recovery key checked first), then PATCH the chunks
//   3. after a move or a restore nobody is signed in: sign in with an account of the old server.
// Every form refuses to send over plain HTTP.

import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { dayKey } from '../shared/time.js';
import { field, form } from '../core/form.js';
import * as session from '../core/session.js';
import { toast } from '../core/toast.js';
import { confirmDialog, passwordOk } from '../core/dialog.js';
import { errorWords } from '../core/states.js';
import { tag, dateTime, dur, DASH } from '../core/fmt.js';
import { textBar } from '../core/ui.js';
import { bareScreen, canSendSecrets, plainHttpLine } from '../core/bare.js';

export const id = 'setup';
export const title = () => 'Set up';

const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const count = (n) => (n == null ? DASH : format(n, 'count'));
const pct = (a, b) => (a != null && b ? Math.max(0, Math.min(100, (a / b) * 100)) : 0);
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : e?.message || errorWords(e).text);
const wait = (e) => `Wait ${e?.retryAfterMs ? Math.ceil(e.retryAfterMs / 1000) : 60} s, then try again.`;
const eta = (s) => (s == null ? null : s < 1 ? 'almost done' : `about ${dur(s * 1000)} to go`);
const FINAL = new Set(['done', 'failed']);
const RESUMABLE = new Set(['cancelled', 'bad_session', 'not_proven']);
const MOVE_STEPS = [['connecting', 'connect', 'check the move code'], ['copying', 'copy', 'everything, while uploads go on'],
  ['catching_up', 'catch up', 'what arrived meanwhile'], ['finishing', 'last round', 'a few seconds'], ['switching', 'switch', 'take over'], ['done', 'done', 'sign in']];
const RESTORE_STEPS = [['uploading', 'upload', 'from this browser'], ['checking', 'check', 'every part of the file'], ['switching', 'switch', 'take the data'], ['done', 'done', 'sign in']];

function toBase64(buf) {
  const b = new Uint8Array(buf);
  let s = '';
  for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
  return btoa(s);
}

/** The recovery key as a text file named after this server and the day. */
function downloadKey(key, ms, tz) {
  const day = dayKey(ms, tz);
  const host = location.hostname.replace(/[^A-Za-z0-9.-]+/g, '_') || 'server';
  const text = `Sentry Mate recovery key\r\n\r\n${key}\r\n\r\nFor the server at ${location.host}, set up ${dateTime(ms, tz, { year: true })} (${day}).\r\n`
    + 'It opens this server\'s data if its key file is lost, and the export files it makes.\r\nKeep it away from the server: in a password manager, or on paper.\r\n';
  const href = URL.createObjectURL(new Blob([text], { type: 'text/plain;charset=utf-8' }));
  const a = h('a', { href, download: `sentry-mate-recovery-key-${host}-${day}.txt`, hidden: true });
  document.body.append(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(href), 30000);
}

export async function mount(ctx) {
  const tz = ctx.tz;
  const scr = bareScreen(ctx, { wide: true, label: 'setup' });
  const S = { setup: null, job: null, log: [], up: { file: null, active: false, error: null, t0: 0, b0: 0 }, chunkMax: null };
  let timer = null;
  const stop = () => { clearTimeout(timer); timer = null; };
  ctx.onCleanup(stop);
  const secure = canSendSecrets();

  const btn = (label, onclick, kind = '') => h('button', { class: ['btn', kind && `btn--${kind}`], type: 'button', onclick }, label);
  const logged = (tone, text) => { S.log.push([tone, text]); };
  const logEl = () => (S.log.length ? h('ol', { class: 'slog', 'aria-label': 'Steps done' }, S.log.map(([tone, text]) =>
    h('li', { class: 'slog-line' }, tag(tone, tone), h('span', { class: 'slog-text' }, text)))) : null);
  const line = (tone, text) => h('p', { class: 'slog-line' }, tag(tone, tone), h('span', { class: 'slog-text' }, text));
  const guard = (f) => { if (!secure) f.submitButton.disabled = true; return f; };
  function paint(heading, ...children) {
    stop();
    scr.paint(h('h1', { id: 'bare-title', class: 'bare-title' }, heading), logEl(), secure ? null : plainHttpLine(), children);
  }
  const howto = (...items) => h('ol', { class: 'howto' }, items.map((x, i) => h('li', null, h('span', { class: 'howto-n' }, `${i + 1}.`), h('span', null, x))));

  function common(e) {
    if (e?.code === 'not_claimed') { S.log = []; codeStep('Another browser typed the set-up code, or this one waited over an hour, so this browser let go. Type the code again: whatever was under way goes on.'); return true; }
    if (e?.code === 'setup_done') { goSignIn('This server is set up already. Sign in.'); return true; }
    return false;
  }
  async function goSignIn(text) {
    stop();
    try { await session.check(); } catch { /* the sign-in page asks again */ }
    if (text) toast(text, { kind: 'info', next: true });
    ctx.navigate('/sign-in', { replace: true });
  }

  /* ---------------------------------------------------------------- 1. the set-up code */
  function codeStep(why = null) {
    const code = field({ label: 'code', name: 'setup_code', required: true, hint: 'It starts with SETUP-. Case, spaces and dashes do not matter.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SETUP-...' } });
    const f = guard(form({
      fields: [code], submit: 'go on',
      async onSubmit(v) {
        try {
          S.setup = await ctx.api.post('setup/claim', { setup_code: v.setup_code.trim() });
        } catch (e) {
          if (e.code === 'wrong_setup_code') { code.setError('That is not this server\'s set-up code. Copy it again from the newest line in the log.'); code.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong codes from this device. ${wait(e)}`); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        logged('ok', 'set-up code accepted: this browser holds the claim for an hour from its last use');
        S.job = S.setup.job;
        if (S.job) jobStep(); else choose();
      },
    }));
    paint('set up this server',
      h('p', { class: 'bare-text' }, 'This server is new: it has no account yet. Show that it is yours with the set-up code from its log. Only someone who can read the log has it.'),
      why ? line('info', why) : null,
      h('div', { class: 'box' }, h('p', { class: 'muted' }, 'where the log is:'),
        howto(['On TrueNAS: Apps, the Sentry Mate app, then ', h('b', null, 'Logs'), '.'],
          ['Find the line ', h('span', { class: 'prompt' }, 'first-run set-up code: SETUP-...'), ' and copy the code. It is printed again on every start until the server is set up.'],
          ['With plain Docker: ', h('span', { class: 'prompt' }, 'docker logs sentry-mate'), '.'])),
      f.el,
      h('p', { class: 'bare-note' }, 'https only: codes and passwords are never sent over plain HTTP. The code stops working once the server is set up.'));
    if (secure) code.input.focus();
  }

  /* ---------------------------------------------------------------- 2. three ways */
  function choose(why = null) {
    const choice = (n, head, text, when, go) => h('li', { class: 'choice' }, h('span', { class: 'choice-n' }, `${n})`),
      h('div', { class: 'choice-body' }, h('p', { class: 'choice-head' }, head), h('p', { class: 'choice-text' }, text),
        h('p', { class: 'choice-text' }, h('span', { class: 'prompt' }, 'when: '), when), btn(`choose ${n}`, go, n === 1 ? 'primary' : '')));
    paint('how should this server start?',
      why ? line('info', why) : null,
      h('ol', { class: 'choices' },
        choice(1, 'start fresh', 'A new, empty server. You make the admin account; cars appear when a CAN Bridge sends its first log.', 'this is your first Sentry Mate server.', freshStep),
        choice(2, 'move here from another server', 'Copy everything from your old server over the network: logs, cars, accounts, upload tokens and its certificate.', 'the old server is on and this one can reach it. You need a move code from it.', moveStep),
        choice(3, 'restore from an export file', 'Open an export file another Sentry Mate server made.', 'the old server is off or out of reach, or you bring back a backup. You need the file and the old server\'s recovery key.', () => restoreStep())),
      h('p', { class: 'bare-note' }, 'Nothing is decided until you go on; each way has a back button to this list. Closed the page? Open the address again and type the code.'));
  }
  const back = () => btn('back', () => choose());

  /* ---------------------------------------------------------------- start fresh */
  function freshStep() {
    const name = field({ label: 'user', name: 'name', required: true, hint: '1 to 64 characters; you type it every time you sign in.', attrs: { autocomplete: 'username', autocapitalize: 'none', spellcheck: 'false', maxlength: '64' } });
    const pass = field({ label: 'password', name: 'password', type: 'password', required: true, hint: 'A few words in a row are easy to remember and hard to guess.', attrs: { autocomplete: 'new-password' } });
    const again = field({ label: 'again', name: 'again', type: 'password', required: true, attrs: { autocomplete: 'new-password' } });
    const f = guard(form({
      fields: [name, pass, again], submit: 'make the admin account', secondary: back(),
      async onSubmit(v) {
        if (v.password !== v.again) { again.setError('The two passwords are not the same.'); again.input.focus(); return; }
        if (!(await passwordOk(v.name.trim(), v.password, pass.input))) return;
        let r;
        try {
          r = await ctx.api.post('setup/fresh', { name: v.name.trim(), password: v.password });
        } catch (e) {
          if (e.code === 'setup_busy') { toast('A move or a restore into this server is under way. Give it up first.', { kind: 'info' }); await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        logged('ok', `chose: start fresh`);
        logged('ok', `admin account "${r.me?.name || v.name.trim()}" made and signed in`);
        keyStep(r.recovery_key, r.me);
      },
    }));
    paint('make the admin account',
      h('p', { class: 'bare-text' }, 'The first admin sees every car and every setting. Add other accounts later in settings/server.'),
      f.el);
    if (secure) name.input.focus();
  }

  function keyStep(key, me) {
    const made = ctx.api.now();
    const saved = h('input', { class: 'chk', type: 'checkbox', id: 'su-saved' });
    const go = h('button', { class: 'btn btn--primary', type: 'button', disabled: true }, 'go on to sentry-mate');
    const anyway = h('button', { class: 'btn', type: 'button', hidden: true }, 'go on anyway');
    const err = h('p', { class: 'frm-msg', hidden: true, role: 'alert' });
    const copied = h('span', { class: 'muted', 'aria-live': 'polite' });
    saved.addEventListener('change', () => { go.disabled = !saved.checked; });
    const enter = async () => {
      await session.signedIn();
      if (!ctx.alive()) return;
      toast(`Welcome, ${me?.name || 'admin'}. Next: install the CAN Bridge and make an upload token in settings/cars.`, { next: true });
      ctx.navigate('/', { replace: true });
    };
    go.addEventListener('click', async () => {
      go.disabled = true; err.hidden = true;
      try { await ctx.api.post('system/recovery-key/saved', {}); } catch (e) {
        if (!ctx.alive()) return;
        replace(err, `The server could not delete its copy of the key (${errText(e)}). Press the button again, or go on anyway: settings/server then offers "I have saved it already".`);
        err.hidden = false; go.disabled = !saved.checked; anyway.hidden = false;
        return;
      }
      await enter();
    });
    anyway.addEventListener('click', enter);
    paint('save your recovery key',
      line('warn', 'It is shown only now, once.'),
      h('p', { class: 'bare-text' }, 'Everything this server stores is encrypted. The recovery key opens it if the key file is ever lost, and opens this server\'s export files when you move.'),
      h('div', { class: 'box' }, h('p', { class: 'muted' }, 'recovery key:'), h('p', { class: 'code-out', 'aria-label': key.split('').join(' ') }, key),
        h('div', { class: 'acts' },
          btn('copy', async () => { try { await navigator.clipboard.writeText(key); copied.textContent = 'copied'; } catch { copied.textContent = 'this browser did not let the page copy: select the key and copy it'; } }, 'primary'),
          btn('download as a file', () => downloadKey(key, made, tz)), copied)),
      howto(['Keep it in a password manager or on paper, away from the server: a backup with the key next to it opens for anyone.'],
        ['Page closed before the tick? Until you tick, the server keeps a copy; settings/server has "show the key again".'],
        ['Ticking deletes the server\'s copy. If the key is lost later, an admin makes a new one in settings/server.']),
      h('label', { class: 'fld-checkline', for: 'su-saved' }, saved, h('span', null, 'I have saved it somewhere safe, away from this server')),
      err,
      h('div', { class: 'acts' }, go, anyway));
  }

  /* ---------------------------------------------------------------- move here */
  function moveStep() {
    const from = field({ label: 'old server', name: 'from', required: true, hint: 'The address the CAN Bridge sends to, such as 192.168.1.20 or nas.local. Port 10100 unless you add another.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'none', inputmode: 'url', placeholder: '192.168.1.20' } });
    const code = field({ label: 'move code', name: 'code', required: true, hint: 'SMMV- and six groups of five. Case and spaces do not matter; a typo is caught at once.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMMV-...' } });
    const f = guard(form({
      fields: [from, code], submit: 'start the move', secondary: back(),
      async onSubmit(v) {
        try {
          S.job = await ctx.api.post('setup/move-here', { from: v.from.trim(), code: v.code.trim() });
        } catch (e) {
          const say = {
            old_server_unreachable: [from, 'Nothing answers at that address. Check it is the one the CAN Bridge sends to, and that the old server is on. Nothing was started.'],
            move_code_wrong: [code, 'The old server did not take this code. Check it letter by letter, or make a new one there.'],
            move_code_expired: [code, 'This code expired (a code works for 24 hours). Make a new one on the old server.'],
            move_code_used: [code, e.message || 'This code was used already.'],
          }[e.code];
          if (say) { say[0].setError(say[1]); say[0].input.focus(); return; }
          const msg = {
            move_too_old: 'The old server runs a newer Sentry Mate. Update this server first, then try again: the code still works.',
            move_not_home_network: 'The code works only from the old server\'s home network. On the old server, make a new code with "from anywhere" ticked.',
            move_not_offered: 'No move code waits at that address. Make one there first (settings/server, move to another server), or check the address.',
          }[e.code];
          if (msg) { f.setMessage(msg); return; }
          if (e.code === 'setup_busy') { await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        logged('ok', `chose: move here from ${v.from.trim()}`);
        jobStep();
      },
    }));
    paint('move here from another server',
      h('div', { class: 'box' }, h('p', { class: 'muted' }, 'first, on the old server:'),
        howto(['Sign in as an admin and open settings/server.'], ['Under "move to another server", press "make a move code". It works once, for 24 hours.'])),
      f.el,
      h('p', { class: 'bare-note' }, 'Both servers stay on while it copies; the old one keeps taking uploads and nothing on it changes. If the copy stops half way it tries again and carries on where it stopped.'));
    if (secure) from.input.focus();
  }

  /* ---------------------------------------------------------------- restore */
  function restoreStep(resume = null) {
    let file = null;
    const fileErr = h('p', { class: 'fld-err', hidden: true, role: 'alert' });
    const chosen = h('span', { class: 'muted' }, resume ? `choose ${resume.file_name} (${bytes(resume.size)}) again` : 'no file chosen: the .smexport file the old server made');
    const input = h('input', { type: 'file', accept: '.smexport,application/octet-stream', id: 'su-file', 'aria-label': 'Export file',
      onchange: () => { const x = input.files?.[0]; if (x) pick(x); } });
    const pickBox = h('div', { class: 'file-pick' }, input, h('label', { class: 'btn', for: 'su-file' }, 'choose file'), chosen);
    function pick(x) { file = x; fileErr.hidden = true; replace(chosen, h('span', { class: 'prompt' }, x.name), h('span', { class: 'muted' }, ` ${bytes(x.size)}`)); }
    const setFileErr = (t) => { fileErr.hidden = !t; replace(fileErr, t || ''); };
    ctx.on(pickBox, 'dragover', (e) => { e.preventDefault(); pickBox.classList.add('is-over'); });
    ctx.on(pickBox, 'dragleave', () => pickBox.classList.remove('is-over'));
    ctx.on(pickBox, 'drop', (e) => { e.preventDefault(); pickBox.classList.remove('is-over'); const x = e.dataTransfer?.files?.[0]; if (x) pick(x); });
    const key = field({ label: 'recovery key', name: 'recovery_key', required: true, hint: 'The SMRK- line saved when the old server was set up. It is checked with the file before the upload starts.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMRK-...' } });
    const f = guard(form({
      fields: [key], submit: resume ? 'check and carry on' : 'check and upload',
      secondary: resume ? btn('give up this upload', () => giveUp(resume), 'danger') : back(),
      async onSubmit(v) {
        if (!file) { setFileErr('Choose the export file first.'); return; }
        if (resume && (file.name !== resume.file_name || file.size !== resume.size)) {
          setFileErr(`That is not the file this upload started with (${resume.file_name}, ${bytes(resume.size)}). Choose that one, or give this upload up.`);
          return;
        }
        let r;
        try {
          const head = await file.slice(0, Math.min(65536, file.size)).arrayBuffer();
          r = await ctx.api.post('setup/restore', { name: file.name, size: file.size, recovery_key: v.recovery_key.trim(), header: toBase64(head) });
        } catch (e) {
          if (e.code === 'not_an_export') { setFileErr('This is not a Sentry Mate export file. Choose the .smexport file the old server made (settings/server, download export file).'); return; }
          if (e.code === 'export_too_new') { setFileErr('A newer Sentry Mate made this file. Update this server first, then choose it again.'); return; }
          if (e.code === 'wrong_recovery_key') { key.setError('This key does not open this file. Use the recovery key of the server that made it.'); key.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong keys from this device. ${wait(e)}`); return; }
          if (e.status === 400 && e.fields?.recovery_key) { key.setError('This does not read as a recovery key: SMRK- and 13 groups of 4.'); return; }
          if (e.code === 'setup_busy') { f.setMessage('A move or a restore into this server is under way already. Give it up first, or wait for it.'); await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        if (!resume) logged('ok', `chose: restore from ${file.name}`);
        logged('ok', 'file and recovery key checked');
        S.job = r; S.up.file = file; S.chunkMax = r.chunk_max_bytes;
        jobStep();
        upload();
      },
    }));
    f.el.prepend(h('div', { class: 'fld' }, h('span', { class: 'fld-label' }, 'file:'), h('div', { class: 'fld-box' }, pickBox, fileErr)));
    paint(resume ? 'carry on with the upload' : 'restore from an export file',
      resume ? line('info', `The upload of ${resume.file_name} stopped at ${bytes(resume.received)} of ${bytes(resume.size)}. The part sent is kept: choose the same file and type the key again.`)
        : h('div', { class: 'box' }, h('p', { class: 'muted' }, 'where the file comes from:'),
          howto(['On the old server: settings/server, "download export file". It ends in .smexport.'], ['It never holds the key file: it opens only with the recovery key.'])),
      f.el,
      h('p', { class: 'bare-note' }, 'Your browser sends the file: keep this page open until the upload is done. If it stops, choose the same file again and it carries on. Nothing is kept from a file that fails its checks.'));
  }

  async function upload() {
    const up = S.up;
    if (up.active || !up.file) return;
    up.active = true; up.error = null; up.t0 = Date.now(); up.b0 = S.job.received || 0;
    const file = up.file;
    const max = S.chunkMax || S.job.chunk_max_bytes || (1024 * 1024 - 1024);
    let off = S.job.received || 0, tries = 0;
    while (off < file.size && ctx.alive() && up.active) {
      try {
        const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
        const r = await ctx.api.patch('setup/restore', buf, { headers: { 'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size) } });
        if (!ctx.alive()) return;
        tries = 0; S.job = r; off = r.received;
        paintJob();
        if (r.state !== 'uploading') break;
      } catch (e) {
        if (!ctx.alive()) return;
        if (['wrong_offset', 'network', 'timeout'].includes(e?.code) && tries < 3) {
          tries++;
          try { const j = await ctx.api.get('setup/job'); if (!ctx.alive()) return; S.job = j; off = j.received; if (j.state !== 'uploading') break; } catch { /* try the chunk again */ }
          continue;
        }
        if (common(e)) { up.active = false; return; }
        up.error = e?.code === 'not_uploading' ? null : e;
        break;
      }
    }
    up.active = false;
    if (off >= file.size) up.file = null;
    if (up.error) { const e = up.error; restoreStep(S.job); toast(`The upload stopped: ${errText(e)} Choose the file again to carry on.`, { kind: 'error' }); return; }
    poll();
  }

  /* ---------------------------------------------------------------- a move or a restore under way */
  const J = {};
  async function giveUp(job) {
    const move = job.kind === 'move_here';
    const ok = await confirmDialog(move
      ? 'What was copied here so far is removed, and the old server is told. Nothing on the old server changes. You go back to the three ways.'
      : 'The part of the file uploaded so far is removed. You go back to the three ways.', { title: move ? 'give up the move?' : 'give up the restore?', ok: 'give it up', danger: true });
    if (!ok) return;
    S.up.active = false; S.up.file = null;
    try { await ctx.api.del('setup/job'); } catch (e) {
      if (e.code === 'finished') { toast('Too late: it is switching already and finishes by itself.', { kind: 'info' }); poll(); return; }
      if (e.code !== 'no_such_job' && !common(e)) { toast(`Could not give it up: ${errText(e)}`, { kind: 'error' }); return; }
      if (e.code !== 'no_such_job') return;
    }
    S.job = null;
    logged('info', move ? 'the move was given up; nothing was kept' : 'the restore was given up; nothing was kept');
    choose();
  }
  async function clearFailed(then) {
    try { await ctx.api.del('setup/job'); } catch (e) { if (common(e)) return; }
    if (!ctx.alive()) return;
    S.job = null;
    then();
  }

  function jobStep() {
    const j = S.job;
    if (j.kind === 'restore' && j.state === 'uploading' && !S.up.active && !S.up.file) { restoreStep(j); return; }
    J.steps = h('ol', { class: 'steps', 'aria-label': 'Steps' });
    J.prog = h('p', { class: 'progress', 'aria-live': 'polite' });
    J.state = h('div', { 'aria-live': 'polite' });
    J.result = h('div');
    J.acts = h('div', { class: 'frm' });
    J.actsKey = null;
    J.sub = h('p', { class: 'bare-text' });
    paint(j.kind === 'move_here' ? 'moving here' : 'restoring', J.sub, J.steps, J.prog, J.state, J.result, J.acts);
    paintJob();
    poll();
  }

  function paintJob() {
    const j = S.job;
    if (!j || !J.steps) return;
    const move = j.kind === 'move_here';
    const list = move ? MOVE_STEPS : RESTORE_STEPS;
    const shown = j.state === 'waiting' || j.state === 'stopped' ? (j.progress?.round > 1 ? 'catching_up' : 'copying') : j.state === 'failed' ? 'uploading' : j.state;
    const at = list.findIndex(([k]) => k === shown);
    const stuck = j.state === 'waiting' || j.state === 'stopped' || j.state === 'failed';
    scr.body.querySelector('.bare-title').textContent = j.state === 'done' ? (move ? 'moved here' : 'restored') : move ? 'moving here' : 'restoring';
    replace(J.sub, move ? ['from ', h('span', { class: 'prompt' }, j.from || DASH), j.started_ms ? `, started ${dateTime(j.started_ms, tz)}` : '']
      : ['from the file ', h('span', { class: 'prompt' }, j.file_name || DASH), ` (${bytes(j.size)})`]);
    replace(J.steps, list.map(([k, l, sub], i) => {
      const st = j.state === 'done' || i < at ? 'ok' : i === at ? (stuck ? (j.state === 'failed' ? 'bad' : 'wait') : 'running') : 'todo';
      return h('li', { class: ['step', i === at && 'step--now'], 'aria-current': i === at ? 'step' : null },
        h('span', { class: 'step-n' }, `${i + 1}.`), tag(st, st === 'wait' ? 'warn' : st), h('span', { class: 'step-t' }, `${l}: `, h('span', { class: 'muted' }, sub)));
    }));

    const p = j.progress;
    let done = p?.bytes_done, total = p?.bytes_total, rate = p?.bytes_per_s, left = p?.eta_s;
    if (!move && j.state === 'uploading') {
      done = j.received; total = j.size;
      const secs = (Date.now() - S.up.t0) / 1000;
      rate = S.up.active && secs > 1 ? (done - S.up.b0) / secs : null;
      left = rate ? (total - done) / rate : null;
    }
    if (j.state === 'done' || (p == null && j.state !== 'uploading') || j.state === 'connecting') replace(J.prog);
    else {
      replace(J.prog, textBar(pct(done, total), { cells: 30, tone: stuck ? 'warn' : 'accent' }), ` ${format(pct(done, total), 'number', { digits: 0 })} %\n`,
        `${bytes(done)} of ${bytes(total)} ${move ? 'copied' : 'uploaded'}`,
        p?.files_total ? ` · ${count(p.files_done)} of ${count(p.files_total)} files` : '',
        rate ? ` · ${bytes(rate)}/s` : '', left != null && rate ? ` · ${eta(left)}` : '');
    }

    const pr = j.problem;
    const say = (s) => (s ? `${s[0].toUpperCase()}${s.slice(1)}${/[.!?]$/.test(s) ? '' : '.'}` : '');
    const st = {
      connecting: ['running', `Reaching ${j.from}: it checks the move code with the old server.`],
      copying: ['running', 'Copying everything. The old server keeps taking uploads; what arrives meanwhile is copied next round. You can close this page: the move goes on.'],
      catching_up: ['running', 'Catching up with what arrived on the old server during the first round.'],
      finishing: ['running', 'The last round: uploads on the old server wait a few seconds so nothing is missed.'],
      switching: ['running', 'Switching to the data. This cannot be given up any more; it finishes by itself.'],
      uploading: S.up.active ? ['running', 'Uploading the file. Keep this page open.'] : ['wait', 'The upload is paused: choose the same file again to carry on.'],
      checking: ['running', 'Checking every part of the file before the server uses it. You can close this page now.'],
      waiting: ['wait', `${say(pr?.message) || 'A problem stopped the copy for now.'}${pr?.retry_ms ? ` Next try in about ${dur(Math.max(1000, pr.retry_ms))}.` : ''} It tries again by itself. ${problemHelp(pr?.code)}`],
      stopped: ['wait', `Stopped: it waits for you. ${say(pr?.message)} ${problemHelp(pr?.code)}`],
      failed: ['bad', `The file failed its checks; nothing from it was kept. ${say(pr?.message) || 'Copy or download it again, or make a new export on the old server.'}`],
    }[j.state];
    replace(J.state, st ? line(st[0] === 'wait' ? 'warn' : st[0], st[1]) : null);
    replace(J.result, j.state === 'done' ? result(j) : null);

    const canForce = move && (j.state === 'waiting' || j.state === 'stopped') && (p?.bytes_done || 0) > 0;
    const canResume = move && j.state === 'stopped' && RESUMABLE.has(pr?.code);
    const k = j.state === 'done' ? 'done' : j.state === 'failed' ? 'failed' : j.state === 'switching' ? 'none'
      : canResume ? (canForce ? 'carry+force' : 'carry') : canForce ? 'force' : (!move && j.state === 'uploading' && !S.up.active) ? 'resume' : 'giveup';
    if (k === J.actsKey) return;
    J.actsKey = k;
    const giveBtn = btn(move ? 'give up the move' : 'give up the restore', () => giveUp(S.job), 'danger');
    const acts = (...b) => h('div', { class: 'acts' }, b);
    if (k === 'done') replace(J.acts, acts(btn('sign in', () => goSignIn(null), 'primary')));
    else if (k === 'failed') replace(J.acts, acts(btn('choose another file', () => clearFailed(() => restoreStep()), 'primary'), btn('back to the three ways', () => clearFailed(() => choose()))));
    else if (k === 'none') replace(J.acts);
    else if (k === 'resume') replace(J.acts, acts(btn('choose the file again', () => restoreStep(S.job), 'primary'), giveBtn));
    else if (k === 'carry' || k === 'carry+force') replace(J.acts, carryOn(j), k === 'carry+force' ? forceBox() : null, acts(giveBtn));
    else if (k === 'force') replace(J.acts, forceBox(), acts(giveBtn));
    else replace(J.acts, acts(giveBtn));
  }

  function problemHelp(code) {
    return {
      unreachable: 'Check that the old server is on and on the network.',
      no_space: 'This server\'s disk is full: make room; it carries on by itself.',
      disk: 'This server\'s disk reported a problem. Check it, then it carries on.',
      slow_down: 'The old server asked it to go slower; it carries on by itself.',
      changing_fast: 'Something big runs on the old server; it carries on once that calms down.',
      cancelled: 'Make a new code on the old server and type it below.',
      bad_session: 'Make a new move code on the old server and type it below: it carries on from where it stopped.',
      not_proven: 'The old server could not prove who it is. Make a new move code there and type it below.',
      disk_times: 'This server\'s disk keeps file times only roughly: use a dataset on ZFS, ext4, xfs or btrfs, and start again.',
    }[code] || '';
  }

  function carryOn(j) {
    const code = field({ label: 'new move code', name: 'code', required: true, hint: `Make it on ${j.from || 'the old server'}: settings/server, move to another server.`,
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMMV-...' } });
    const f = guard(form({
      fields: [code], submit: 'carry on',
      async onSubmit(v) {
        try {
          S.job = await ctx.api.post('setup/move-here', { from: j.from, code: v.code.trim() });
        } catch (e) {
          const say = {
            bad_input: e.fields?.code ? 'This code has a typo: SMMV- and six groups of five.' : null,
            invalid: e.fields?.code ? 'A newer Sentry Mate made this code. Update this server first.' : null,
            move_code_wrong: 'The old server did not take this code. Check it letter by letter, or make a new one there.',
            move_code_expired: 'This code expired (a code works for 24 hours). Make a new one on the old server.',
            move_code_used: e.message || 'This code was used already. Make a new one.',
          }[e.code];
          if (say) { code.setError(say); code.input.focus(); return; }
          const msg = {
            old_server_unreachable: `Nothing answers at ${j.from}. Turn the old server on, then press carry on again: the code still works.`,
            move_too_old: 'The old server runs a newer Sentry Mate. Update this server first, then press carry on again.',
            move_not_home_network: 'The code works only from the old server\'s home network. Make a new code there with "from anywhere" ticked.',
            move_not_offered: 'No move code waits on the old server. Make one there first.',
            setup_busy: 'The move is not stopped any more (perhaps another browser carried it on).',
          }[e.code];
          if (msg) { f.setMessage(msg); if (e.code === 'setup_busy') await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        logged('ok', 'carried on with a new move code; what was copied before is kept');
        jobStep();
      },
    }));
    return h('div', { class: 'box' }, h('p', { class: 'muted' }, `carry on: what was copied here (${bytes(j.progress?.bytes_done)}) is kept. With a new code from ${j.from || 'the same server'} the move goes on from there.`), f.el);
  }

  function forceBox() {
    const key = field({ label: 'recovery key', name: 'recovery_key', required: true, hint: 'The old server\'s SMRK- line, saved when it was set up.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMRK-...' } });
    const sure = field({ label: 'The old server is gone for good. Its last changes may be missing, and it must never take uploads again.', name: 'confirm', type: 'checkbox', required: true });
    const f = guard(form({
      fields: [key, sure], submit: 'finish with what was copied', danger: true,
      async onSubmit(v) {
        if (!v.confirm) { sure.setError('Tick this first: only when the old server is gone for good. If it is only off, turn it on instead.'); return; }
        try {
          S.job = await ctx.api.post('setup/move-here/finish', { recovery_key: v.recovery_key.trim(), confirm: v.confirm });
        } catch (e) {
          if (e.code === 'wrong_recovery_key') { key.setError('This key does not open the old server\'s data.'); key.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong keys from this device. ${wait(e)}`); return; }
          if (e.status === 400 && e.fields?.recovery_key) { key.setError('This does not read as a recovery key: SMRK- and 13 groups of 4.'); return; }
          if (e.code === 'not_ready') { f.setMessage('It can finish this way only while the copy is stopped part way. It is moving again: wait for it.'); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        logged('warn', 'finished with the recovery key: the old server\'s last changes may be missing');
        jobStep();
      },
    }));
    return h('details', { class: 'danger-zone' }, h('summary', null, 'is the old server gone for good?'),
      h('div', { class: 'danger-body' }, h('p', null, 'Only if its disk died or it can never come back: this server can finish with what was copied so far, opened with the old server\'s recovery key. If the old server is only off, turn it on instead: the move carries on by itself.'), f.el));
  }

  function result(j) {
    const r = j.result || {};
    const move = j.kind === 'move_here';
    return [
      line('ok', `everything from ${move ? r.from || j.from : r.file_name || j.file_name} is here: ${count(r.accounts)} ${r.accounts === 1 ? 'account' : 'accounts'} and ${count(r.cars)} ${r.cars === 1 ? 'car' : 'cars'}, finished ${dateTime(r.at_ms, tz)}`),
      r.forced ? line('warn', 'finished with the recovery key: the old server\'s last changes may be missing. Never start it again with uploads on.') : null,
      j.skipped?.length ? line('info', `not carried (the old server could not read ${j.skipped.length === 1 ? 'it' : 'them'}): ${j.skipped.map((s) => s.path).join(', ')}`) : null,
      h('div', { class: 'box' }, h('p', { class: 'muted' }, 'left to do:'),
        howto(['Sign in with your account from the old server: its accounts and passwords came along.'],
          r.same_machine ? ['Same machine: stop the old server, then give this app port 10100 back. The CAN Bridge needs no change.']
            : ['The CAN Bridge: change only the server address to ', h('span', { class: 'prompt' }, `https://${location.hostname}:10100/api/can/logs`), '. Keep the token and the certificate SHA-256: they came along.'],
          move ? ['The old server says it moved. Turn it off when you are happy here; keep its folders a few days.'] : ['Delete the export file once this server runs: it holds everything.'])),
    ];
  }

  function poll() {
    stop();
    const j = S.job;
    if (!j || FINAL.has(j.state) || (j.kind === 'restore' && j.state === 'uploading')) return;
    timer = setTimeout(async () => {
      try {
        const n = await ctx.api.get('setup/job');
        if (!ctx.alive()) return;
        S.job = n; J.lost = false;
        paintJob();
      } catch (e) {
        if (!ctx.alive()) return;
        if (common(e)) return;
        if (e.code === 'no_such_job') { S.job = null; choose('Nothing is under way any more: it was given up, perhaps in another browser. Choose again.'); return; }
        if (!J.lost) { J.lost = true; toast('The page lost touch with this server for a moment. It keeps asking; whatever is under way goes on.', { kind: 'info' }); }
      }
      poll();
    }, 1000);
  }

  async function loadJob() {
    try { S.job = await ctx.api.get('setup/job'); } catch (e) {
      if (common(e)) return;
      if (e.code === 'no_such_job') { S.job = null; choose(); return; }
      throw e;
    }
    if (ctx.alive()) jobStep();
  }

  /* ---------------------------------------------------------------- start */
  try { S.setup = await ctx.api.get('setup'); } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const s = S.setup;
  if (!s.needs_setup && !(s.claimed && s.job)) { await goSignIn('This server is set up already. Sign in.'); return; }
  if (s.claimed) logged('ok', 'set-up code accepted earlier in this browser');
  if (s.claimed && s.job) { S.job = s.job; jobStep(); }
  else if (s.claimed) choose();
  else codeStep(s.state === 'moving_here' ? 'A move into this server is under way, started in another browser. Type the set-up code to follow it here.'
    : s.state === 'restoring' ? 'A restore into this server is under way, started in another browser. Type the set-up code to follow it here.' : null);
  ctx.ready();
}
