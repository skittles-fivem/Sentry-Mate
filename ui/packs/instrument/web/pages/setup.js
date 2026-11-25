// First run (/setup): the server has no account yet. Nothing here needs a shell.
//   1. The set-up code from the server's log (POST /setup/claim): only someone who can read the log has it,
//      so whoever reaches a fresh server first can't take it over. The claim is a cookie (an hour from its
//      last use), so a reload keeps it.
//   2. One of three choices:
//      - Start fresh (POST /setup/fresh): the first admin, then the recovery key shown once with Copy and
//        Download, and "I have saved it" (POST /system/recovery-key/saved) before going on.
//      - Move here from another server (POST /setup/move-here): the old server's address and its move code;
//        the copy is followed by polling GET /setup/job once a second, with the move's log underneath (its
//        `log`, newest last, kept by the server: a reload shows it again; pages/move-log.js). Last resort when
//        the old server is gone for good: POST /setup/move-here/finish with its recovery key.
//      - Restore from an export file (POST /setup/restore, then PATCH chunks): the file and the recovery key
//        are checked before the upload; then the upload, the checks and the switch.
//   3. After a move or a restore the server has the old server's accounts and nobody is signed in: sign in.

import { h, replace, setVars } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { brandMark, icon } from '../core/icons.js';
import { field, form } from '../core/form.js';
import * as session from '../core/session.js';
import { flash } from '../core/flash.js';
import { confirmDialog, passwordOk } from '../core/dialog.js';
import { errorWords } from '../core/states.js';
import { dateTime, dur, DASH } from '../core/fmt.js';
import { keyBox, etaText } from './settings-server-move.js';
import { moveLog } from './move-log.js';

export const id = 'setup';

// the browser's time zone goes with the first admin: new cars start in it (Settings: Cars changes it)
const browserZone = () => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || undefined; } catch { return undefined; } };
export const css = true;
export const title = () => 'Set up';

const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const count = (n) => (n == null ? DASH : format(n, 'count'));
const pct = (a, b) => (a != null && b ? Math.max(0, Math.min(100, (a / b) * 100)) : 0);
const pctText = (a, b) => `${format(pct(a, b), 'number', { digits: 0 })} %`;
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : e?.message || errorWords(e).text);
const wait = (e) => `Wait ${e?.retryAfterMs ? Math.ceil(e.retryAfterMs / 1000) : 60} s, then try again.`;
const FINAL = new Set(['done', 'failed']);
const RESUMABLE = new Set(['cancelled', 'bad_session', 'not_proven']);  // stopped: a new code from the same server carries on

const MOVE_STEPS = [
  ['connecting', 'Connecting', 'Checks the code'],
  ['copying', 'Copying', 'Uploads go on'],
  ['catching_up', 'Catching up', 'New arrivals'],
  ['finishing', 'Last round', 'Seconds'],
  ['switching', 'Switching', 'Takes over'],
  ['done', 'Done', 'Sign in'],
];
const RESTORE_STEPS = [
  ['uploading', 'Uploading', 'From this browser'],
  ['checking', 'Checking', 'Every part'],
  ['switching', 'Switching', 'Takes the data'],
  ['done', 'Done', 'Sign in'],
];

function toBase64(buf) {
  const b = new Uint8Array(buf);
  let s = '';
  for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
  return btoa(s);
}

export async function mount(ctx) {
  const tz = ctx.tz;
  const card = h('section', { class: 'panel su-card', 'aria-labelledby': 'auth-title' });
  replace(ctx.view, h('div', { class: 'auth su' }, card));
  const S = { setup: null, job: null, up: { file: null, active: false, error: null, t0: 0, b0: 0 } };
  let timer = null;
  ctx.onCleanup(() => clearTimeout(timer));
  const stop = () => { clearTimeout(timer); timer = null; };

  const btn = (label, onclick, { kind = '', ic = null, type = 'button' } = {}) =>
    h('button', { class: ['btn', kind && `btn--${kind}`], type, onclick }, ic ? icon(ic, 'ico--sm') : null, label);
  const brand = () => h('div', { class: 'auth-brand' }, brandMark(), h('span', { class: 'brand-name' }, 'Sentry Mate'));
  // where the person is: set-up code, choice, then the chosen way
  function crumbs(at, third = 'Set up') {
    const list = [['Set-up code'], ['Choose'], [third]];
    return h('ol', { class: 'su-crumbs', 'aria-label': 'Set-up steps' }, list.map(([l], i) => h('li', {
      class: ['su-crumb', i < at && 'su-crumb--done', i === at && 'su-crumb--now'], 'aria-current': i === at ? 'step' : null,
    }, h('span', { class: 'su-crumb-n' }, i < at ? icon('check', 'ico--sm') : String(i + 1)), h('span', null, l))));
  }
  const line = (tone, head, ...text) => h('div', { class: ['su-line', `su-line--${tone}`], role: tone === 'warn' ? 'alert' : null },
    h('span', { class: 'su-line-ic' }, icon(tone === 'ok' ? 'check' : tone === 'warn' ? 'warn' : 'info')),
    h('div', null, h('p', { class: 'su-line-head' }, head), text.length ? h('p', { class: 'su-line-text' }, text) : null));
  let painted = false;
  function paint(wide, ...children) {
    stop();
    card.classList.toggle('su-card--wide', !!wide);
    replace(card, brand(), children);
    globalThis.scrollTo?.(0, 0);
    // a step after the first: the control that led here is gone, and the focus with it. The new step's heading
    // takes it (a step with a field to type in moves it there next), so a screen reader says where this is.
    if (painted) takeFocus();
    painted = true;
  }
  function takeFocus() {
    const head = card.querySelector('h1');
    if (head && (!document.activeElement || document.activeElement === document.body)) { head.tabIndex = -1; head.focus({ preventScroll: true }); }
  }

  // anything that says the claim is gone or the server is set up
  function common(e) {
    if (e?.code === 'not_claimed') { codeStep('Another browser typed the set-up code, or this one waited over an hour, so this browser let go. Type the code again to carry on: whatever was under way goes on meanwhile.'); return true; }
    if (e?.code === 'setup_done') { goSignIn('This server is set up already. Sign in.'); return true; }
    return false;
  }
  async function goSignIn(text) {
    stop();
    try { await session.check(); } catch { /* the sign-in page asks again */ }
    if (text) flash(text, { kind: 'info', next: true });
    ctx.navigate('/sign-in', { replace: true });
  }

  /* ================================================================ 1. the set-up code */
  function codeStep(why = null) {
    const code = field({ label: 'Set-up code', name: 'setup_code', required: true, hint: 'It starts with SETUP-. Case, spaces and dashes don\'t matter.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SETUP-…' } });
    const f = form({
      fields: [code], submit: 'Go on',
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
        S.job = S.setup.job;
        if (S.job) jobStep(); else choose();
      },
    });
    paint(false,
      crumbs(0),
      h('h1', { id: 'auth-title' }, 'Set up Sentry Mate'),
      h('p', null, 'This server is new: it has no account yet. First show that it is yours with the set-up code from its log. Only someone who can read the log has it, so whoever reaches a new server first can\'t take it over.'),
      why ? line('info', why) : null,
      f.el,
      h('div', { class: 'su-where' },
        h('p', { class: 'su-where-head' }, icon('question', 'ico--sm'), 'Where is the log?'),
        h('ol', { class: 'su-howto' },
          h('li', null, h('b', null, 'On TrueNAS:'), ' Apps, then the Sentry Mate app, then the ', h('b', null, 'Logs'), ' button on its page.'),
          h('li', null, 'Look for the line ', h('span', { class: 'mono' }, 'first-run set-up code: SETUP-…'), ' and copy the code. It is printed again on every start until the server is set up.')),
        h('p', { class: 'note' }, 'With plain Docker instead, the same line is in ', h('span', { class: 'mono' }, 'docker logs sentry-mate'), '.')),
      h('p', { class: 'auth-note' }, icon('lock', 'ico--sm'), ' Codes and passwords are only sent over HTTPS. The code stops working once the server is set up.'));
    code.input.focus();
  }

  /* ================================================================ 2. the three choices */
  function choose(why = null) {
    const choice = (ic, head, text, when, go) => h('li', null, h('button', { class: 'su-choice', type: 'button', onclick: go },
      h('span', { class: 'su-choice-ic' }, icon(ic)),
      h('span', { class: 'su-choice-body' },
        h('span', { class: 'su-choice-head' }, head),
        h('span', { class: 'su-choice-text' }, text),
        h('span', { class: 'su-choice-when' }, h('b', null, 'Choose this when '), when)),
      h('span', { class: 'su-choice-go' }, icon('chev-r'))));
    paint(true,
      crumbs(1),
      h('h1', { id: 'auth-title' }, 'How should this server start?'),
      h('p', null, 'Pick one. Nothing is decided until you go on, and each way has a Back button to this choice.'),
      why ? line('info', why) : null,
      h('ul', { class: 'su-choices' },
        choice('plus', 'Start fresh', 'A new, empty server. You make the admin account; cars appear when a CAN Bridge sends its first log.',
          'this is your first Sentry Mate server.', freshStep),
        choice('share', 'Move here from another server', 'Copy everything from your old Sentry Mate server over the network: logs, cars, accounts, upload tokens and its certificate.',
          'the old server is on and this one can reach it. You need a move code from it.', moveStep),
        choice('upload', 'Restore from an export file', 'Open an export file another Sentry Mate server made, with everything in it.',
          'the old server is off or can\'t be reached, or you bring back a backup. You need the file and the old server\'s recovery key.', () => restoreStep())),
      h('p', { class: 'auth-note' }, icon('info', 'ico--sm'), ' Closed the page by mistake? Open the address again and type the set-up code: you come back here, or to what is under way.'));
  }
  const back = () => btn('Back', () => choose(), { kind: 'ghost', ic: 'arrow-l' });

  /* ================================================================ start fresh */
  function freshStep() {
    const name = field({ label: 'Your name for signing in', name: 'name', required: true, hint: '1 to 64 characters. You type it every time you sign in.',
      attrs: { autocomplete: 'username', autocapitalize: 'none', spellcheck: 'false', maxlength: '64' } });
    const pass = field({ label: 'Password', name: 'password', type: 'password', required: true, hint: 'A few words in a row are easy to remember and hard to guess.',
      attrs: { autocomplete: 'new-password' } });
    const again = field({ label: 'Password again', name: 'again', type: 'password', required: true, attrs: { autocomplete: 'new-password' } });
    const f = form({
      fields: [name, pass, again], submit: 'Make the admin account', secondary: back(),
      async onSubmit(v) {
        if (v.password !== v.again) { again.setError('The two passwords are not the same.'); again.input.focus(); return; }
        if (!(await passwordOk(v.name.trim(), v.password, pass.input))) return;
        let r;
        try {
          r = await ctx.api.post('setup/fresh', { name: v.name.trim(), password: v.password, time_zone: browserZone() });
        } catch (e) {
          if (e.code === 'setup_busy') { flash('A move or a restore into this server is under way. Give it up first.', { kind: 'info' }); await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        keyStep(r.recovery_key, r.me);
      },
    });
    paint(false,
      crumbs(2, 'Start fresh'),
      h('h1', { id: 'auth-title' }, 'Make the admin account'),
      h('p', null, 'This is the first admin: it sees every car and every setting. Add other accounts later, in Settings: Server and access.'),
      f.el);
    name.input.focus();
  }

  function keyStep(key, me) {
    const madeMs = ctx.api.now();
    const saved = h('input', { type: 'checkbox', id: 'su-saved' });
    const go = h('button', { class: 'btn btn--primary', type: 'button', disabled: true }, 'Go on to Sentry Mate', icon('arrow-r', 'ico--sm'));
    const err = h('p', { class: 'form-error', hidden: true, role: 'alert' });
    saved.addEventListener('change', () => { go.disabled = !saved.checked; });
    const anyway = h('button', { class: 'btn btn--ghost', type: 'button', hidden: true }, 'Go on anyway');
    const enter = async () => {
      await session.signedIn();
      if (!ctx.alive()) return;
      flash(`Welcome, ${me?.name || 'admin'}. Your server is ready: install the CAN Bridge and make an upload token in Settings: Cars.`, { next: true });
      ctx.navigate('/', { replace: true });
    };
    go.addEventListener('click', async () => {
      go.disabled = true; err.hidden = true;
      try { await ctx.api.post('system/recovery-key/saved', {}); } catch (e) {
        if (!ctx.alive()) return;
        // stay on this page so the message is read: try again, or go on and do it in Settings
        replace(err, icon('warn'), `The server could not delete its copy of the key (${errText(e)}). Press the button again, or go on anyway: Settings: Server and access then shows "I have saved it already" to do it later.`);
        err.hidden = false; go.disabled = !saved.checked; anyway.hidden = false;
        return;
      }
      await enter();
    });
    anyway.addEventListener('click', enter);
    paint(true,
      crumbs(3, 'Start fresh'),
      h('h1', { id: 'auth-title' }, 'Save your recovery key'),
      h('p', null, h('b', null, 'It is shown only now, once.'), ' Your account is made. Everything this server stores is encrypted; the recovery key opens it if the server\'s key file is ever lost, and it opens export files of this server when you move.'),
      keyBox(key, { ms: madeMs, tz, why: 'set up' }),
      h('ul', { class: 'su-rules' },
        h('li', null, 'Keep it in a password manager or on paper, ', h('b', null, 'away from the server'), ': a backup with the key next to it opens for anyone. The downloaded file names this server and today\'s date.'),
        h('li', null, 'Page closed before the tick? Nothing is lost: until you tick, the server keeps a copy, and ', h('b', null, 'Settings: Server and access'), ' has ', h('b', null, 'Show the key again'), ' (with your password).'),
        h('li', null, 'Ticking below deletes the server\'s copy; after that nobody can show this key again. If it is lost later, an admin presses ', h('b', null, 'Make a new recovery key'), ' in the same place.')),
      h('label', { class: 'check su-saved', for: 'su-saved' }, saved, h('span', null, 'I have saved it somewhere safe, away from this server')),
      err,
      h('div', { class: 'row su-acts' }, go, anyway));
  }

  /* ================================================================ move here */
  function moveStep() {
    const from = field({ label: 'The old server\'s address', name: 'from', required: true,
      hint: 'The one the CAN Bridge sends to, such as 192.0.2.20 or nas.local. Port 10100 unless you add another; the whole https://…/api/can/logs address works too.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'none', inputmode: 'url', placeholder: '192.0.2.20' } });
    const code = field({ label: 'Move code', name: 'code', required: true, hint: 'SMMV- and six groups of five. Case and spaces don\'t matter; a typo is caught at once.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMMV-…' } });
    const f = form({
      fields: [from, code], submit: 'Start the move', secondary: back(),
      async onSubmit(v) {
        try {
          S.job = await ctx.api.post('setup/move-here', { from: v.from.trim(), code: v.code.trim() });
        } catch (e) {
          const say = {
            old_server_unreachable: [from, 'Nothing answers at that address. Check it is the one the CAN Bridge sends to, and that the old server is on. Nothing was started.'],
            move_code_wrong: [code, 'The old server did not take this code. Check it letter by letter, or make a new one there.'],
            move_code_expired: [code, 'This code expired (a code works for 24 hours). Make a new one on the old server.'],
            move_code_used: [code, `${e.message || 'This code was used already.'}`],
          }[e.code];
          if (say) { say[0].setError(say[1]); say[0].input.focus(); return; }
          const msg = {
            move_too_old: 'The old server runs a newer Sentry Mate than this one. Update this server first (on TrueNAS: Apps, Update on the Sentry Mate app), then try again: the code still works.',
            move_not_home_network: 'The code works only from the old server\'s home network. On the old server, make a new code with "From anywhere" ticked.',
            move_not_offered: 'No move code waits at that address. Make one there first (Settings: Server and access, Move to another server), or check the address.',
          }[e.code];
          if (msg) { f.setMessage(msg); return; }
          if (e.code === 'setup_busy') { await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        jobStep();
      },
    });
    paint(true,
      crumbs(2, 'Move here'),
      h('h1', { id: 'auth-title' }, 'Move here from another server'),
      h('div', { class: 'su-where' },
        h('p', { class: 'su-where-head' }, icon('info', 'ico--sm'), 'First, on the old server'),
        h('ol', { class: 'su-howto' },
          h('li', null, 'Sign in as an admin and open ', h('b', null, 'Settings: Server and access'), '.'),
          h('li', null, 'Under ', h('b', null, 'Move to another server'), ', press ', h('b', null, 'Make a move code'), '. It works once, for 24 hours.'))),
      f.el,
      h('ul', { class: 'su-rules' },
        h('li', null, 'Both servers stay on while it copies. The old one keeps taking uploads; this one takes over by itself at the end.'),
        h('li', null, 'Nothing on the old server is changed. If the copy stops half way, it tries again by itself and carries on where it stopped.')));
    from.input.focus();
  }

  /* ================================================================ restore */
  function restoreStep(resume = null) {
    let file = null;
    const fileErr = h('p', { class: 'form-error', hidden: true });
    const fileInfo = h('span', { class: 'su-drop-sub' }, resume ? `Choose ${resume.file_name} (${bytes(resume.size)}) again.` : 'The .smexport file the old server made.');
    const input = h('input', { class: 'su-file-input', type: 'file', accept: '.smexport,application/octet-stream', 'aria-label': 'Export file',
      onchange: () => { const x = input.files?.[0]; if (x) pick(x); } });
    const drop = h('label', { class: 'su-drop' }, input, h('span', { class: 'su-drop-ic' }, icon('upload')),
      h('span', { class: 'su-drop-main' }, 'Choose the export file, or drop it here'), fileInfo);
    function pick(x) {
      file = x;
      fileErr.hidden = true;
      replace(fileInfo, h('b', { class: 'mono' }, x.name), ` · ${bytes(x.size)}`);
      drop.classList.add('su-drop--chosen');
    }
    ctx.on(drop, 'dragover', (e) => { e.preventDefault(); drop.classList.add('su-drop--over'); });
    ctx.on(drop, 'dragleave', () => drop.classList.remove('su-drop--over'));
    ctx.on(drop, 'drop', (e) => { e.preventDefault(); drop.classList.remove('su-drop--over'); const x = e.dataTransfer?.files?.[0]; if (x) pick(x); });
    const fileErrSet = (t) => { fileErr.hidden = !t; if (t) replace(fileErr, icon('warn'), t); };

    const key = field({ label: 'The old server\'s recovery key', name: 'recovery_key', required: true,
      hint: 'The SMRK- line saved when the old server was set up. It is checked, with the file, before the upload starts.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMRK-…', class: 'input mono' } });
    const f = form({
      fields: [key], submit: resume ? 'Check and carry on' : 'Check and upload',
      extra: null,
      secondary: resume ? btn('Give up this upload', () => giveUp(resume), { kind: 'ghost', ic: 'x' }) : back(),
      async onSubmit(v) {
        if (!file) { fileErrSet('Choose the export file first.'); drop.focus?.(); return; }
        if (resume && (file.name !== resume.file_name || file.size !== resume.size)) {
          fileErrSet(`That is not the file this upload started with (${resume.file_name}, ${bytes(resume.size)}). Choose that one to carry on, or give this upload up to start with another.`);
          return;
        }
        let r;
        try {
          const head = await file.slice(0, Math.min(65536, file.size)).arrayBuffer();
          r = await ctx.api.post('setup/restore', { name: file.name, size: file.size, recovery_key: v.recovery_key.trim(), header: toBase64(head) });
        } catch (e) {
          if (e.code === 'not_an_export') { fileErrSet('This is not a Sentry Mate export file. Choose the .smexport file the old server made (Settings: Server and access, Download export file).'); return; }
          if (e.code === 'export_too_new') { fileErrSet('A newer Sentry Mate made this file. Update this server first (on TrueNAS: Apps, Update), then choose it again.'); return; }
          if (e.code === 'wrong_recovery_key') { key.setError('This key does not open this file. Use the recovery key of the server that made it.'); key.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong keys from this device. ${wait(e)}`); return; }
          if (e.status === 400 && e.fields?.recovery_key) { key.setError('This does not read as a recovery key: SMRK- and 13 groups of 4. Check for a missing group.'); return; }
          if (e.code === 'setup_busy') { f.setMessage('A move or a restore into this server is under way already (perhaps from another browser). Give it up first, or wait for it.'); await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        S.job = r;
        S.up.file = file;
        S.chunkMax = r.chunk_max_bytes;
        jobStep();
        upload();
      },
    });
    f.el.prepend(h('div', { class: 'form-row' }, h('span', { class: 'form-label' }, 'Export file'), drop, fileErr));
    paint(true,
      crumbs(2, 'Restore'),
      h('h1', { id: 'auth-title' }, resume ? 'Carry on with the upload' : 'Restore from an export file'),
      resume
        ? line('info', `The upload of ${resume.file_name} stopped at ${bytes(resume.received)} of ${bytes(resume.size)} (${pctText(resume.received, resume.size)}).`,
          'The part sent is kept. Choose the same file and type the recovery key again: it carries on from there.')
        : h('div', { class: 'su-where' },
          h('p', { class: 'su-where-head' }, icon('info', 'ico--sm'), 'Where the file comes from'),
          h('p', { class: 'su-where-text' }, 'On the old server: ', h('b', null, 'Settings: Server and access'), ', ', h('b', null, 'Download export file'), '. It ends in .smexport and never holds the key file: it opens only with the recovery key.')),
      f.el,
      h('ul', { class: 'su-rules' },
        h('li', null, 'Your browser sends the file, so keep this page open until the upload is done. The checks after it run on the server.'),
        h('li', null, 'If the upload stops (the page closed, the network dropped), choose the same file again: it carries on where it stopped.'),
        h('li', null, 'Nothing is kept from a file that fails its checks.')));
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
        const r = await ctx.api.patch('setup/restore', buf, { headers: {
          'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size) } });
        if (!ctx.alive()) return;
        tries = 0;
        S.job = r;
        off = r.received;
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
    if (up.error) { restoreStepAfterStop(); return; }
    poll();
  }
  function restoreStepAfterStop() {
    const e = S.up.error;
    restoreStep(S.job);
    flash(`The upload stopped: ${errText(e)} Choose the file again to carry on.`, { kind: 'error' });
  }

  /* ================================================================ a move or restore under way */
  const J = {};
  async function giveUp(job) {
    const move = job.kind === 'move_here';
    const ok = await confirmDialog(move
      ? 'What was copied here so far is removed, and the old server is told. Nothing on the old server changes: it keeps taking uploads. You go back to the three choices.'
      : 'The part of the file uploaded so far is removed. You go back to the three choices.',
    { title: move ? 'Give up the move?' : 'Give up the restore?', ok: 'Give it up', danger: true });
    if (!ok) return;
    S.up.active = false; S.up.file = null;
    try { await ctx.api.del('setup/job'); } catch (e) {
      if (e.code === 'finished') { flash('Too late: it is switching to the data already, and finishes by itself.', { kind: 'info' }); poll(); return; }
      if (e.code !== 'no_such_job' && !common(e)) { flash(`Could not give it up: ${errText(e)}`, { kind: 'error' }); return; }
      if (e.code !== 'no_such_job') return;
    }
    S.job = null;
    choose('Given up. Nothing was kept; choose again.');
  }

  // a restore that failed its checks kept nothing: clear it away without asking
  async function clearFailed(then) {
    try { await ctx.api.del('setup/job'); } catch (e) { if (common(e)) return; }
    if (!ctx.alive()) return;
    S.job = null;
    then();
  }

  function jobStep() {
    const j = S.job;
    if (j.kind === 'restore' && j.state === 'uploading' && !S.up.active && !S.up.file) { restoreStep(j); return; }
    J.head = h('div', { class: 'su-jobhead' });
    J.steps = h('ol', { class: 'su-steps', 'aria-label': 'Steps' });
    J.prog = h('div', { class: 'su-progress', 'aria-live': 'polite' });
    J.state = h('div', { class: 'su-jobstate', 'aria-live': 'polite' });
    J.acts = h('div', { class: 'su-jobacts' });
    J.actsKey = null;
    J.result = h('div', { class: 'su-result' });
    J.log = j.kind === 'move_here' ? moveLog({ tz, file: 'sentry-mate-move-here-log' }) : null;
    J.tail = 0;
    paint(true, crumbs(2, j.kind === 'move_here' ? 'Move here' : 'Restore'), J.head, J.steps, J.prog, J.state, J.result, J.log?.el, J.acts);
    paintJob();
    poll();
  }

  function paintJob() {
    const j = S.job;
    if (!j || !J.head) return;
    const move = j.kind === 'move_here';
    const list = move ? MOVE_STEPS : RESTORE_STEPS;
    const shown = j.state === 'waiting' || j.state === 'stopped' ? (j.progress?.round > 1 ? 'catching_up' : 'copying') : j.state === 'failed' ? 'uploading' : j.state;
    const at = list.findIndex(([k]) => k === shown);
    const problemAt = (j.state === 'waiting' || j.state === 'stopped' || j.state === 'failed') ? at : -1;
    replace(J.head,
      h('h1', { id: 'auth-title' }, j.state === 'done' ? (move ? 'Moved here' : 'Restored') : move ? 'Moving here' : 'Restoring'),
      h('p', null, move ? ['From ', h('b', { class: 'mono' }, j.from || DASH), j.started_ms ? `, started ${dateTime(j.started_ms, tz)}.` : '.']
        : ['From the file ', h('b', { class: 'mono' }, j.file_name || DASH), ` (${bytes(j.size)}).`]));
    replace(J.steps, list.map(([k, l, sub], i) => {
      const st = j.state === 'done' || i < at ? 'done' : i === problemAt ? 'stuck' : i === at ? 'now' : 'later';
      return h('li', { class: ['su-step', `su-step--${st}`], 'aria-current': st === 'now' || st === 'stuck' ? 'step' : null },
        h('span', { class: 'su-step-n' }, st === 'done' ? icon('check', 'ico--sm') : st === 'stuck' ? icon('warn', 'ico--sm') : String(i + 1)),
        h('span', { class: 'su-step-t' }, h('b', null, l), h('span', null, sub)));
    }));

    // progress: the copy, or the upload with this browser's own speed
    const p = j.progress;
    let done = p?.bytes_done, total = p?.bytes_total, rate = p?.bytes_per_s, eta = p?.eta_s;
    let label = move ? 'copied' : 'uploaded';
    if (!move && j.state === 'uploading') {
      done = j.received; total = j.size; label = 'uploaded';
      const secs = (Date.now() - S.up.t0) / 1000;
      rate = S.up.active && secs > 1 ? (done - S.up.b0) / secs : null;
      eta = rate ? (total - done) / rate : null;
    }
    if (j.state === 'done' || (p == null && j.state !== 'uploading') || j.state === 'connecting') replace(J.prog);
    else {
      const bar = h('div', { class: ['bar', 'su-bar', (j.state === 'waiting' || j.state === 'stopped' || j.state === 'failed') && 'bar--warm'], role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': String(Math.round(pct(done, total))), 'aria-label': 'Progress' }, h('i'));
      setVars(bar, { '--w': `${pct(done, total).toFixed(1)}%` });
      replace(J.prog, bar, h('p', { class: 'su-bar-text' }, h('b', null, bytes(done)), ` of ${bytes(total)} ${label} (${pctText(done, total)})`,
        p?.files_total ? ` · ${count(p.files_done)} of ${count(p.files_total)} files` : null,
        rate ? ` · ${bytes(rate)} per second` : null,
        eta != null && rate ? ` · ${etaText(eta)}` : null));
    }

    // what is happening, in words
    const pr = j.problem;
    let st;
    switch (j.state) {
      case 'connecting': st = line('info', `Reaching ${j.from}…`, 'It checks the move code with the old server.'); break;
      case 'copying': st = line('info', 'Copying everything from the old server', 'The old server keeps taking uploads; what arrives meanwhile is copied in the next round. You can close this page: the move goes on.'); break;
      case 'catching_up': st = line('info', 'Catching up', 'Copying what arrived on the old server during the first round.'); break;
      case 'finishing': st = line('info', 'The last round', 'Uploads on the old server wait a few seconds so nothing is missed. The CAN Bridge sends them again by itself.'); break;
      case 'switching': st = line('info', 'Switching to the data', 'Almost done. This can\'t be given up any more; it finishes by itself.'); break;
      case 'uploading': st = line('info', S.up.active ? 'Uploading the file' : 'The upload is paused', S.up.active ? 'Keep this page open. If it stops, choose the same file again: it carries on.' : 'Choose the same file again to carry on.'); break;
      case 'checking': st = line('info', 'Checking the file', 'Every part of it is checked before the server uses it. You can close this page now: it goes on.'); break;
      case 'waiting': st = line('warn', 'Waiting: it tries again by itself', `${pr?.message ? `${pr.message[0].toUpperCase()}${pr.message.slice(1)}.` : 'A problem stopped the copy for now.'}${pr?.retry_ms ? ` Next try in about ${dur(Math.max(1000, pr.retry_ms))}.` : ''}`,
        ' Nothing to do unless the old server is gone for good. ', problemHelp(pr?.code)); break;
      case 'stopped': st = line('warn', 'Stopped: it waits for you', pr?.message ? `${pr.message[0].toUpperCase()}${pr.message.slice(1)}` : 'The copy stopped.', ' ', problemHelp(pr?.code)); break;
      case 'failed': st = line('warn', 'The file failed its checks: nothing from it was kept', pr?.message ? `${pr.message[0].toUpperCase()}${pr.message.slice(1)}.` : 'The file is damaged. Copy or download it again, or make a new export on the old server.'); break;
      case 'done': st = null; break;
      default: st = line('info', j.state);
    }
    replace(J.state, st);
    replace(J.result, j.state === 'done' ? result(j) : null);
    J.log?.update(j.log);

    // the buttons: rebuilt only when what they offer changes, so a half-typed key stays
    const canForce = move && (j.state === 'waiting' || j.state === 'stopped') && (p?.bytes_done || 0) > 0;
    const canResume = move && j.state === 'stopped' && RESUMABLE.has(pr?.code);
    const k = j.state === 'done' ? 'done' : j.state === 'failed' ? 'failed' : j.state === 'switching' ? 'none'
      : canResume ? (canForce ? 'carry+force' : 'carry') : canForce ? 'force' : (!move && j.state === 'uploading' && !S.up.active) ? 'resume' : 'giveup';
    if (k !== J.actsKey) {
      J.actsKey = k;
      const halfway = move
        ? h('p', { class: 'note su-halfway' }, h('b', null, 'If it stops half way:'), ' nothing on the old server changes. A lost connection is tried again by itself and it carries on where it stopped. Closing this page does not stop it: open the address again and type the set-up code to follow it.')
        : h('p', { class: 'note su-halfway' }, h('b', null, 'If it stops half way:'), ' nothing is used until every part is checked. An upload that stopped carries on when you choose the same file again.');
      const giveBtn = btn(move ? 'Give up the move' : 'Give up the restore', () => giveUp(S.job), { kind: 'ghost', ic: 'x' });
      if (k === 'done') replace(J.acts, h('div', { class: 'row su-acts' }, btn('Sign in', () => goSignIn(null), { kind: 'primary', ic: 'user' })));
      else if (k === 'failed') replace(J.acts, h('div', { class: 'row su-acts' }, btn('Choose another file', () => clearFailed(() => restoreStep()), { kind: 'primary', ic: 'upload' }), btn('Back to the three choices', () => clearFailed(() => choose()), { kind: 'ghost', ic: 'arrow-l' })));
      else if (k === 'none') replace(J.acts, halfway);
      else if (k === 'resume') replace(J.acts, h('div', { class: 'row su-acts' }, btn('Choose the file again', () => restoreStep(S.job), { kind: 'primary', ic: 'upload' }), giveBtn), halfway);
      else if (k === 'carry' || k === 'carry+force') replace(J.acts, carryOnBox(j), k === 'carry+force' ? forceBox() : null, h('div', { class: 'row su-acts' }, giveBtn), halfway);
      else if (k === 'force') replace(J.acts, forceBox(), h('div', { class: 'row su-acts' }, giveBtn), halfway);
      else replace(J.acts, h('div', { class: 'row su-acts' }, giveBtn), halfway);
    }
  }

  function problemHelp(code) {
    return {
      unreachable: 'Check that the old server is on and on the network.',
      no_space: 'This server\'s disk is full: make room (on TrueNAS, a bigger dataset); it carries on by itself.',
      disk: 'This server\'s disk reported a problem. Check it, then it carries on.',
      slow_down: 'The old server asked it to go slower; it carries on by itself.',
      changing_fast: 'Something big is running on the old server; it carries on once that calms down.',
      cancelled: 'Make a new code on the old server and type it below.',
      bad_session: 'Make a new move code on the old server and type it below: it carries on from where it stopped.',
      not_proven: 'The old server could not prove who it is. Make a new move code there and type it below.',
      disk_times: 'This server\'s disk keeps file times only roughly: give the app a dataset on ZFS, ext4, xfs or btrfs, and start again.',
    }[code] || '';
  }

  // a move that stopped for the person (cancelled on the old server, its session ended): a new code carries it on
  function carryOnBox(j) {
    const code = field({ label: 'New move code from the old server', name: 'code', required: true,
      hint: `Make it on ${j.from || 'the old server'}: Settings: Server and access, Move to another server, Make a move code. Case and spaces don't matter.`,
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMMV-…' } });
    const f = form({
      fields: [code], submit: 'Carry on',
      async onSubmit(v) {
        try {
          S.job = await ctx.api.post('setup/move-here', { from: j.from, code: v.code.trim() });
        } catch (e) {
          const say = {
            bad_input: e.fields?.code ? 'This code has a typo: check it against the one the old server shows (SMMV- and six groups of five).' : null,
            invalid: e.fields?.code ? 'A newer Sentry Mate made this code. Update this server first (on TrueNAS: Apps, Update), then type it again.' : null,
            move_code_wrong: 'The old server did not take this code. Check it letter by letter, or make a new one there.',
            move_code_expired: 'This code expired (a code works for 24 hours). Make a new one on the old server.',
            move_code_used: e.message || 'This code was used already. Make a new one on the old server.',
          }[e.code];
          if (say) { code.setError(say); code.input.focus(); return; }
          const msg = {
            old_server_unreachable: `Nothing answers at ${j.from}. Turn the old server on, check it is on the network, then press Carry on again: the code still works.`,
            move_too_old: 'The old server runs a newer Sentry Mate than this one. Update this server first (on TrueNAS: Apps, Update), then press Carry on again.',
            move_not_home_network: 'The code works only from the old server\'s home network. On the old server, make a new code with "From anywhere" ticked.',
            move_not_offered: 'No move code waits on the old server. Make one there first, then type it here.',
            setup_busy: 'The move is not stopped any more (perhaps another browser carried it on). This page shows it again.',
          }[e.code];
          if (msg) { f.setMessage(msg); if (e.code === 'setup_busy') await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        flash('Carrying on with the move: what was copied before is kept.', { kind: 'info' });
        J.actsKey = null;
        paintJob(); poll();
      },
    });
    return h('div', { class: 'su-where su-carry' },
      h('p', { class: 'su-where-head' }, icon('share', 'ico--sm'), 'Carry on with a new move code'),
      h('p', { class: 'su-where-text' }, `What was copied here (${bytes(j.progress?.bytes_done)}) is kept. With a new code from the same server, ${j.from || 'the old server'}, the move goes on from there: nothing is copied twice.`),
      f.el);
  }

  function forceBox() {
    const key = field({ label: 'The old server\'s recovery key', name: 'recovery_key', required: true, hint: 'The SMRK- line saved when the old server was set up.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMRK-…', class: 'input mono' } });
    const sure = field({ label: 'The old server is gone for good. Its last changes may be missing, and it must never take uploads again.', name: 'confirm', type: 'checkbox', required: true });
    const f = form({
      fields: [key, sure], submit: 'Finish with what was copied',
      async onSubmit(v) {
        // the one step that can't be undone: only with the person's own tick
        if (!v.confirm) { sure.setError('Tick this first: only when the old server is gone for good. If it is only off, turn it on instead.'); sure.input.focus(); return; }
        try {
          S.job = await ctx.api.post('setup/move-here/finish', { recovery_key: v.recovery_key.trim(), confirm: v.confirm });
        } catch (e) {
          if (e.code === 'wrong_recovery_key') { key.setError('This key does not open the old server\'s data. Use the recovery key saved when the old server was set up.'); key.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong keys from this device. ${wait(e)}`); return; }
          if (e.status === 400 && e.fields?.recovery_key) { key.setError('This does not read as a recovery key: SMRK- and 13 groups of 4. Check for a missing group.'); return; }
          if (e.code === 'not_ready') { f.setMessage('It can finish this way only while the copy is stopped part way. It is moving again: wait for it.'); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        paintJob(); poll();
      },
    });
    f.el.querySelector('button[type="submit"]')?.classList.replace('btn--primary', 'btn--danger');
    return h('details', { class: 'su-force' },
      h('summary', null, icon('warn', 'ico--sm'), 'Is the old server gone for good?'),
      h('div', { class: 'su-force-body' },
        h('p', null, 'Only if its disk died or it can never come back: this server can finish with what was copied so far, opened with the old server\'s recovery key. The last changes before it stopped may be missing. If the old server is only off for now, turn it on instead: the move carries on by itself.'),
        f.el));
  }

  function result(j) {
    const r = j.result || {};
    const move = j.kind === 'move_here';
    const here = globalThis.location.hostname;
    return [
      line('ok', move ? `Everything from ${r.from || j.from} is here` : `Everything from ${r.file_name || j.file_name} is here`,
        `${count(r.accounts)} ${r.accounts === 1 ? 'account' : 'accounts'} and ${count(r.cars)} ${r.cars === 1 ? 'car' : 'cars'}, finished ${dateTime(r.at_ms, tz)}.`),
      r.forced ? line('warn', 'Finished with the recovery key', 'The old server\'s last changes before it stopped may be missing. Never start the old server again with uploads on.') : null,
      j.skipped?.length ? h('p', { class: 'note' }, `Not carried, because the old server couldn't read ${j.skipped.length === 1 ? 'it' : 'them'}: `, j.skipped.map((s, i) => [i ? ', ' : '', h('span', { class: 'mono' }, s.path)]), '.') : null,
      h('div', { class: 'su-where' },
        h('p', { class: 'su-where-head' }, icon('check', 'ico--sm'), 'What is left to do'),
        h('ol', { class: 'su-howto' },
          h('li', null, h('b', null, 'Sign in'), ' with your account from the old server: its accounts and passwords came along. Nobody is signed in yet.'),
          r.same_machine
            ? h('li', null, h('b', null, 'Same machine:'), ' the old server ran here too. Stop it, then give this app port 10100 back (on TrueNAS: edit the app, port 10100:10100). The CAN Bridge then needs no change.')
            : h('li', null, h('b', null, 'The CAN Bridge:'), ' in the app, under Second Sentry Six Manager, change only the server address to ',
              h('span', { class: 'mono' }, `https://${here}:10100/api/can/logs`), '. Keep the token and the certificate SHA-256: they came along',
              r.cert_sha256 ? [' (', h('span', { class: 'mono su-sha' }, r.cert_sha256), ')'] : null, '.'),
          move ? h('li', null, 'The old server says it moved. Turn it off when you are happy here; keep its folders a few days.')
            : h('li', null, 'Delete the export file once this server runs: it holds everything, and the recovery key opens it.'))),
    ];
  }

  // once a move is done the new server still says what it started (its first start after the switch): the log is
  // read a little longer, until it says so
  const started = (j) => (j.log || []).some((l) => l.text.startsWith('Inputs started here'));
  function poll() {
    stop();
    const j = S.job;
    const tail = j?.kind === 'move_here' && j.state === 'done' && !started(j) && J.tail++ < 30;
    if (!j || (FINAL.has(j.state) && !tail) || (j.kind === 'restore' && j.state === 'uploading')) return;
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
        if (!J.lost) { J.lost = true; flash('The page lost touch with this server for a moment. It keeps asking; whatever is under way goes on.', { kind: 'info' }); }
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

  /* ================================================================ start */
  try {
    S.setup = await ctx.api.get('setup');
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const s = S.setup;
  if (!s.needs_setup && !(s.claimed && s.job)) { await goSignIn('This server is set up already. Sign in.'); return; }
  if (s.claimed && s.job) { S.job = s.job; jobStep(); }
  else if (s.claimed) choose();
  else codeStep(s.state === 'moving_here' ? 'A move into this server is under way, started in another browser. Type the set-up code to follow it here.'
    : s.state === 'restoring' ? 'A restore into this server is under way, started in another browser. Type the set-up code to follow it here.' : null);
  ctx.ready();
}
