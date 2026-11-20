// First start (/setup): the server has no account yet. Everything is done here, no shell needed.
//
//   1. Code: the one-time set-up code from the server's log (POST /setup/claim). Only someone who can read the
//      log has it, so a stranger who reaches a new server first can do nothing. The claim is a cookie that
//      lasts an hour from its last use, so a reload keeps it.
//   2. Route: one of three ways.
//      - Start fresh (POST /setup/fresh): the first admin; then the recovery key, shown this once with Copy and
//        Download, and "I have saved it" (POST /system/recovery-key/saved) before going on.
//      - Move here (POST /setup/move-here): the old server's address and its move code; the copy is followed
//        with GET /setup/job once a second. Carry on with a new code after a stop; as a last resort, finish
//        with what was copied and the old server's recovery key (POST /setup/move-here/finish).
//      - Restore from an export file (POST /setup/restore, then PATCH chunks): the file and the recovery key are
//        checked before the upload; then upload, checks and switch. A stopped upload carries on from `received`.
//   3. Drive: the job under way, step by step, with progress; at the end, sign in (the accounts came along).

import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { dayKey } from '../shared/time.js';
import { icon, hud, btn, callout, bar } from '../core/ui.js';
import { field, form } from '../core/form.js';
import { toast, confirmDialog } from '../core/layer.js';
import { errorWords } from '../core/states.js';
import { dateTime, dur, DASH } from '../core/fmt.js';
import * as session from '../core/session.js';
import { gateBrand } from './sign-in.js';

export const id = 'setup';
export const css = true;
export const title = () => 'Set up';

const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const count = (n) => (n == null ? DASH : format(n, 'count'));
const share = (a, b) => (a != null && b ? Math.max(0, Math.min(1, a / b)) : 0);
const pctText = (a, b) => `${format(share(a, b) * 100, 'number', { digits: 0 })} %`;
const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : e?.message || errorWords(e).text);
const waitText = (e) => `Wait ${e?.retryAfterMs ? Math.ceil(e.retryAfterMs / 1000) : 60} s, then try again.`;
const eta = (s) => (s == null ? null : s < 1 ? 'almost done' : `about ${dur(s * 1000)} to go`);
const cap = (t) => (t ? `${t[0].toUpperCase()}${t.slice(1)}` : t);
const FINAL = new Set(['done', 'failed']);
const CARRY_ON = new Set(['cancelled', 'bad_session', 'not_proven']);   // a new code from the same server carries on

const MOVE = [['connecting', 'Contact'], ['copying', 'Copy'], ['catching_up', 'Catch up'], ['finishing', 'Last round'], ['switching', 'Switch'], ['done', 'Done']];
const RESTORE = [['uploading', 'Upload'], ['checking', 'Check'], ['switching', 'Switch'], ['done', 'Done']];

function base64(buf) {
  const b = new Uint8Array(buf);
  let s = '';
  for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
  return btoa(s);
}

/** The recovery key in groups, with Copy and Download as a text file that names this server and the date. */
function keyBox(key, ms, tz) {
  const groups = String(key).split('-');
  const copy = btn('Copy', async () => {
    try { await navigator.clipboard.writeText(key); toast('Copied. Paste it somewhere safe, away from this server.'); } catch { toast('This browser would not copy. Select the key and copy it by hand.', { kind: 'error' }); }
  }, { kind: 'go', icon: 'check' });
  const save = btn('Download as a file', () => {
    const day = dayKey(ms, tz);
    const host = location.hostname.replace(/[^A-Za-z0-9.-]+/g, '_') || 'server';
    const text = `Sentry Mate recovery key\r\n\r\n${key}\r\n\r\nFor the server at ${location.host}, set up ${dateTime(ms, tz, { year: true, month: 'long', weekday: 'long' })} (${day}).\r\n`
      + 'It opens this server\'s data if its key file is lost, and the export files it makes.\r\nKeep it away from the server: in a password manager, or on paper.\r\n';
    const href = URL.createObjectURL(new Blob([text], { type: 'text/plain;charset=utf-8' }));
    const a = h('a', { href, download: `sentry-mate-recovery-key-${host}-${day}.txt`, hidden: true });
    document.body.append(a); a.click(); a.remove();
    setTimeout(() => URL.revokeObjectURL(href), 30000);
  }, { icon: 'download' });
  return h('div', { class: 'su-key' },
    h('p', { class: 'su-key-label' }, 'Recovery key'),
    h('p', { class: 'su-key-code', 'aria-label': `Recovery key: ${key}` }, groups.map((g, i) => h('span', { class: 'su-key-g' }, i ? `-${g}` : g))),
    h('div', { class: 'row' }, copy, save));
}

export async function mount(ctx) {
  const tz = ctx.tz;
  const card = hud({ cls: ['gate', 'gate--wide', 'su'], pad: false, label: 'Set up Sentry Mate' });
  replace(ctx.view, card);
  const S = { setup: null, job: null, up: { file: null, active: false, error: null, t0: 0, b0: 0 }, chunk: null };
  let timer = null;
  const stop = () => { clearTimeout(timer); timer = null; };
  ctx.onCleanup(stop);

  // the three lamps at the top: Code, Route, Drive
  function lamps(at, third = 'Go') {
    return h('ol', { class: 'su-lamps', 'aria-label': 'Set-up steps' }, [['Code'], ['Route'], [third]].map(([t], i) => h('li', {
      class: ['su-lamp', i < at && 'is-done', i === at && 'is-now'], 'aria-current': i === at ? 'step' : null,
    }, h('span', { class: 'su-lamp-n' }, i < at ? icon('check', 'i--sm') : String(i + 1)), h('span', { class: 'su-lamp-t' }, t))));
  }
  function show(...children) {
    stop();
    replace(card, gateBrand(), children);
    globalThis.scrollTo?.(0, 0);
  }
  const back = () => btn('Back', () => route(), { kind: 'ghost', icon: 'arrow-l' });

  // answers that mean the claim is gone or the server is set up meanwhile
  function common(e) {
    if (e?.code === 'not_claimed') { codeStep('Another browser typed the set-up code, or this one waited over an hour. Type the code again: whatever is under way goes on meanwhile.'); return true; }
    if (e?.code === 'setup_done') { toSignIn('This server is set up already. Sign in.'); return true; }
    return false;
  }
  async function toSignIn(text) {
    stop();
    session.reset();
    try { await session.ask(); } catch { /* the sign-in page asks again */ }
    if (text) toast(text, { kind: 'info', next: true });
    ctx.navigate('/sign-in', { replace: true });
  }

  /* ---------------------------------------------------------------- 1. the code */
  function codeStep(why = null) {
    const code = field({ label: 'Set-up code', name: 'setup_code', required: true, hint: "It starts with SETUP-. Case, spaces and dashes don't matter.",
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SETUP-…', class: 'input mono' } });
    const f = form({
      fields: [code], submit: 'Go on',
      async onSubmit(v) {
        try { S.setup = await ctx.api.post('setup/claim', { setup_code: v.setup_code.trim() }); } catch (e) {
          if (e.code === 'wrong_setup_code') { code.setError("That is not this server's code. Copy it again from the newest line of the log."); code.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong codes from this device. ${waitText(e)}`); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        S.job = S.setup.job;
        if (S.job) jobStep(); else route();
      },
    });
    show(lamps(0),
      h('h1', { class: 'gate-title chrome' }, 'Start the engine'),
      h('p', { class: 'gate-lead' }, "This server is new and has no account yet. Show that it is yours with the set-up code from its log. Only someone who can read the log has it, so a stranger who finds the server first can't take it."),
      why ? callout('info', why) : null,
      f.el,
      h('div', { class: 'su-help' },
        h('p', { class: 'su-help-head' }, icon('question', 'i--sm'), h('span', null, 'Where is the log?')),
        h('ul', { class: 'su-list' },
          h('li', null, h('b', null, 'TrueNAS: '), 'Apps, the Sentry Mate app, then its ', h('b', null, 'Logs'), ' button.'),
          h('li', null, h('b', null, 'Docker: '), h('span', { class: 'mono' }, 'docker logs sentry-mate'), '.'),
          h('li', null, 'Look for ', h('span', { class: 'mono' }, 'first-run set-up code: SETUP-…'), '. It is printed again on every start until the server is set up.'))),
      h('p', { class: 'gate-foot' }, icon('lock', 'i--sm'), h('span', null, 'Codes and passwords only travel over HTTPS. The code stops working once the server is set up.')));
    code.input.focus();
  }

  /* ---------------------------------------------------------------- 2. the three routes */
  function route(why = null) {
    const way = (ic, tone, head, text, need, go) => h('li', null, h('button', { class: ['su-way', `su-way--${tone}`], type: 'button', onclick: go },
      h('span', { class: 'su-way-ic' }, icon(ic, 'i--lg')),
      h('span', { class: 'su-way-body' }, h('span', { class: 'su-way-head' }, head), h('span', { class: 'su-way-text' }, text), h('span', { class: 'su-way-need' }, h('b', null, 'Pick this when '), need)),
      h('span', { class: 'su-way-go' }, icon('chev-r'))));
    show(lamps(1),
      h('h1', { class: 'gate-title chrome' }, 'Pick a route'),
      h('p', { class: 'gate-lead' }, 'Three ways to start. Nothing is decided until you go on, and each has a Back button to here.'),
      why ? callout('info', why) : null,
      h('ul', { class: 'su-ways' },
        way('plus', 'mag', 'Start fresh', 'A new, empty server. You make the admin account; cars appear when a CAN Bridge sends its first log.',
          'this is your first Sentry Mate server.', freshStep),
        way('share', 'cyan', 'Move here from another server', 'Copy everything from your old server over the network: logs, cars, accounts, upload tokens and its certificate.',
          'the old server is on and this one can reach it. You need a move code from it.', moveStep),
        way('upload', 'org', 'Restore from an export file', 'Read an export file another Sentry Mate server made, with everything in it.',
          "the old server is off or out of reach, or you bring back a backup. You need the file and the old server's recovery key.", () => restoreStep())),
      h('p', { class: 'gate-foot' }, icon('info', 'i--sm'), h('span', null, 'Closed this page by mistake? Open the address again and type the code: you come back here, or to what is under way.')));
  }

  /* ---------------------------------------------------------------- start fresh */
  function freshStep() {
    const name = field({ label: 'Your sign-in name', name: 'name', required: true, hint: '1 to 64 characters. You type it each time you sign in.',
      attrs: { autocomplete: 'username', autocapitalize: 'none', spellcheck: 'false', maxlength: '64' } });
    const pass = field({ label: 'Password', name: 'password', type: 'password', required: true, hint: 'At least 10 characters. A few words in a row are easy to remember and hard to guess.',
      attrs: { autocomplete: 'new-password', minlength: '10' } });
    const again = field({ label: 'Password again', name: 'again', type: 'password', required: true, attrs: { autocomplete: 'new-password' } });
    const f = form({
      fields: [name, pass, again], submit: 'Make the admin account', secondary: back(),
      async onSubmit(v) {
        if (v.password !== v.again) { again.setError('The two passwords are not the same.'); again.input.focus(); return; }
        let r;
        try { r = await ctx.api.post('setup/fresh', { name: v.name.trim(), password: v.password }); } catch (e) {
          if (e.code === 'setup_busy') { toast('A move or a restore into this server is under way. Give it up first.', { kind: 'info' }); await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        keyStep(r.recovery_key, r.me);
      },
    });
    show(lamps(2, 'Start fresh'),
      h('h1', { class: 'gate-title chrome' }, 'The admin account'),
      h('p', { class: 'gate-lead' }, 'The first admin sees every car and every setting. Add more accounts later in Server and access.'),
      f.el);
    name.input.focus();
  }

  function keyStep(key, me) {
    const made = ctx.api.now();
    const tick = h('input', { type: 'checkbox', id: 'su-saved' });
    const go = btn('Go on to Sentry Mate', null, { kind: 'go', icon: 'arrow-r', disabled: true });
    const anyway = btn('Go on anyway', null, { kind: 'ghost' });
    anyway.hidden = true;
    const err = h('p', { class: 'form-msg', role: 'alert', hidden: true });
    tick.addEventListener('change', () => { go.disabled = !tick.checked; });
    const enter = async () => {
      await session.signedIn();
      if (!ctx.alive()) return;
      toast(`Welcome, ${me?.name || 'admin'}. Next: install the CAN Bridge and make an upload token in Car settings.`, { next: true });
      ctx.navigate('/', { replace: true });
    };
    go.addEventListener('click', async () => {
      go.disabled = true; err.hidden = true;
      try { await ctx.api.post('system/recovery-key/saved', {}); } catch (e) {
        if (!ctx.alive()) return;
        replace(err, icon('warn', 'i--sm'), h('span', null, `The server could not delete its copy of the key (${errText(e)}). Press the button again, or go on anyway: Server and access then offers "I have saved it" for later.`));
        err.hidden = false; go.disabled = !tick.checked; anyway.hidden = false;
        return;
      }
      await enter();
    });
    anyway.addEventListener('click', enter);
    show(lamps(3, 'Start fresh'),
      h('h1', { class: 'gate-title chrome' }, 'Save your recovery key'),
      h('p', { class: 'gate-lead' }, h('b', null, 'This is the only time it is shown. '), "Your account is made. Everything this server keeps is encrypted: the recovery key opens it if the server's key file is ever lost, and it opens this server's export files when you move."),
      keyBox(key, made, tz),
      h('ul', { class: 'su-list' },
        h('li', null, 'Keep it in a password manager or on paper, ', h('b', null, 'away from the server'), ': a backup with the key beside it opens for anyone.'),
        h('li', null, 'Closed the page before ticking? Nothing is lost: until you tick, the server keeps a copy, and Server and access has ', h('b', null, 'Show the key again'), ' (with your password).'),
        h('li', null, 'Ticking deletes the server\'s copy; then nobody can show this key again. A lost key is replaced with ', h('b', null, 'Make a new recovery key'), ' in the same place.')),
      h('label', { class: 'check su-tick', for: 'su-saved' }, tick, h('span', null, 'I have saved it somewhere safe, away from this server')),
      err,
      h('div', { class: 'row' }, go, anyway));
  }

  /* ---------------------------------------------------------------- move here */
  function moveStep() {
    const from = field({ label: "The old server's address", name: 'from', required: true,
      hint: 'The address the CAN Bridge sends to, such as 192.168.1.20 or nas.local. Port 10100 unless you add another; the whole https://…/api/can/logs address works too.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'none', inputmode: 'url', placeholder: '192.168.1.20' } });
    const code = field({ label: 'Move code', name: 'code', required: true, hint: "SMMV- and six groups of five. Case and spaces don't matter; a typo is caught at once.",
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMMV-…', class: 'input mono' } });
    const f = form({
      fields: [from, code], submit: 'Start the move', secondary: back(),
      async onSubmit(v) {
        try { S.job = await ctx.api.post('setup/move-here', { from: v.from.trim(), code: v.code.trim() }); } catch (e) {
          const onField = {
            old_server_unreachable: [from, 'Nothing answers at that address. Check it is the one the CAN Bridge sends to, and that the old server is on. Nothing was started.'],
            move_code_wrong: [code, 'The old server did not take this code. Check it letter by letter, or make a new one there.'],
            move_code_expired: [code, 'This code has run out (a code works for 24 hours). Make a new one on the old server.'],
            move_code_used: [code, e.message || 'This code was used already. Make a new one on the old server.'],
          }[e.code];
          if (onField) { onField[0].setError(onField[1]); onField[0].input.focus(); return; }
          const msg = {
            move_too_old: 'The old server runs a newer Sentry Mate than this one. Update this server first (on TrueNAS: Apps, Update), then try again: the code still works.',
            move_not_home_network: 'This code only works from the old server\'s home network. On the old server, make a new code with "From anywhere" ticked.',
            move_not_offered: 'No move code waits at that address. Make one there first (Server and access, Move to another server), or check the address.',
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
    show(lamps(2, 'Move here'),
      h('h1', { class: 'gate-title chrome' }, 'Move here'),
      h('div', { class: 'su-help' },
        h('p', { class: 'su-help-head' }, icon('info', 'i--sm'), h('span', null, 'First, on the old server')),
        h('ol', { class: 'su-list su-list--n' },
          h('li', null, 'Sign in as an admin and open ', h('b', null, 'Server and access'), '.'),
          h('li', null, 'Under ', h('b', null, 'Move to another server'), ', press ', h('b', null, 'Make a move code'), '. It works once, for 24 hours.'))),
      f.el,
      h('ul', { class: 'su-list' },
        h('li', null, 'Both servers stay on while it copies. The old one keeps taking uploads; this one takes over by itself at the end.'),
        h('li', null, 'Nothing on the old server is changed. If the copy stops half way, it tries again by itself and carries on where it stopped.')));
    from.input.focus();
  }

  /* ---------------------------------------------------------------- restore */
  function restoreStep(resume = null) {
    let file = null;
    const fileErr = h('p', { class: 'field-error', hidden: true });
    const fileLine = h('span', { class: 'su-drop-sub' }, resume ? `Choose ${resume.file_name} (${bytes(resume.size)}) again.` : 'The .smexport file the old server made.');
    const input = h('input', { class: 'su-file', type: 'file', accept: '.smexport,application/octet-stream', 'aria-label': 'Export file', onchange: () => { const x = input.files?.[0]; if (x) pick(x); } });
    const drop = h('label', { class: 'su-drop' }, input, h('span', { class: 'su-drop-ic' }, icon('upload', 'i--lg')),
      h('span', { class: 'su-drop-text' }, h('span', { class: 'su-drop-main' }, 'Choose the export file, or drop it here'), fileLine));
    const fileError = (t) => { fileErr.hidden = !t; if (t) replace(fileErr, icon('warn', 'i--sm'), h('span', null, t)); };
    function pick(x) {
      file = x;
      fileError(null);
      replace(fileLine, h('b', { class: 'mono' }, x.name), ` · ${bytes(x.size)}`);
      drop.classList.add('is-chosen');
    }
    ctx.on(drop, 'dragover', (e) => { e.preventDefault(); drop.classList.add('is-over'); });
    ctx.on(drop, 'dragleave', () => drop.classList.remove('is-over'));
    ctx.on(drop, 'drop', (e) => { e.preventDefault(); drop.classList.remove('is-over'); const x = e.dataTransfer?.files?.[0]; if (x) pick(x); });
    const key = field({ label: "The old server's recovery key", name: 'recovery_key', required: true,
      hint: 'The SMRK- line saved when the old server was set up. It is checked with the file before the upload starts.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMRK-…', class: 'input mono' } });
    const f = form({
      fields: [key], submit: resume ? 'Check and carry on' : 'Check and upload',
      secondary: resume ? btn('Give up this upload', () => giveUp(resume), { kind: 'ghost', icon: 'x' }) : back(),
      extra: null,
      async onSubmit(v) {
        if (!file) { fileError('Choose the export file first.'); return; }
        if (resume && (file.name !== resume.file_name || file.size !== resume.size)) {
          fileError(`That is not the file this upload began with (${resume.file_name}, ${bytes(resume.size)}). Choose that one to carry on, or give this upload up to start with another.`);
          return;
        }
        let r;
        try {
          const head = await file.slice(0, Math.min(65536, file.size)).arrayBuffer();
          r = await ctx.api.post('setup/restore', { name: file.name, size: file.size, recovery_key: v.recovery_key.trim(), header: base64(head) });
        } catch (e) {
          if (e.code === 'not_an_export') { fileError('This is not a Sentry Mate export file. Choose the .smexport file the old server made (Server and access, Download export file).'); return; }
          if (e.code === 'export_too_new') { fileError('A newer Sentry Mate made this file. Update this server first (on TrueNAS: Apps, Update), then choose it again.'); return; }
          if (e.code === 'export_cut_short') { fileError('This file is shorter than it says: it was cut off while it was copied. Copy or download it again.'); return; }
          if (e.code === 'wrong_recovery_key') { key.setError('This key does not open this file. Use the recovery key of the server that made it.'); key.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong keys from this device. ${waitText(e)}`); return; }
          if (e.status === 503) { f.setMessage(`The server is busy checking another key. ${waitText(e)}`); return; }
          if (e.status === 400 && e.fields?.recovery_key) { key.setError('This does not read as a recovery key: SMRK- and 13 groups of 4. Look for a missing group.'); return; }
          if (e.code === 'not_empty') { f.setMessage('This server holds data already, so it cannot take an export file.'); return; }
          if (e.code === 'setup_busy') { f.setMessage('A move or a restore into this server is under way already, perhaps from another browser. Give it up first, or wait for it.'); await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        S.job = r;
        S.up.file = file;
        S.chunk = r.chunk_max_bytes;
        jobStep();
        upload();
      },
    });
    f.el.prepend(h('div', { class: 'field' }, h('span', { class: 'field-label' }, 'Export file'), drop, fileErr));
    show(lamps(2, 'Restore'),
      h('h1', { class: 'gate-title chrome' }, resume ? 'Carry on with the upload' : 'Restore from a file'),
      resume
        ? callout('info', `The upload of ${resume.file_name} stopped at ${bytes(resume.received)} of ${bytes(resume.size)} (${pctText(resume.received, resume.size)}).`,
          'The part sent is kept. Choose the same file and type the recovery key again: it carries on from there.')
        : h('div', { class: 'su-help' },
          h('p', { class: 'su-help-head' }, icon('info', 'i--sm'), h('span', null, 'Where the file comes from')),
          h('p', { class: 'su-help-text' }, 'On the old server: ', h('b', null, 'Server and access'), ', ', h('b', null, 'Download export file'), '. It ends in .smexport and never holds the key file: only the recovery key opens it.')),
      f.el,
      h('ul', { class: 'su-list' },
        h('li', null, 'Your browser sends the file, so keep this page open until the upload is done. The checks after it run on the server.'),
        h('li', null, 'If the upload stops (page closed, network gone), choose the same file again: it carries on where it stopped.'),
        h('li', null, 'Nothing is kept from a file that fails its checks.')));
  }

  async function upload() {
    const up = S.up;
    if (up.active || !up.file) return;
    up.active = true; up.error = null; up.t0 = Date.now(); up.b0 = S.job.received || 0;
    const file = up.file;
    const max = S.chunk || S.job.chunk_max_bytes || (1024 * 1024 - 1024);
    let off = S.job.received || 0, tries = 0;
    while (off < file.size && ctx.alive() && up.active) {
      try {
        const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
        const r = await ctx.api.patch('setup/restore', buf, { headers: { 'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size) } });
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
          try { const j = await ctx.api.get('setup/job'); if (!ctx.alive()) return; S.job = j; off = j.received; if (j.state !== 'uploading') break; } catch { /* send the chunk again */ }
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
      ? 'What was copied here so far is removed and the old server is told. Nothing on the old server changes: it keeps taking uploads. You go back to the three routes.'
      : 'The part of the file uploaded so far is removed. You go back to the three routes.',
    { title: move ? 'Give up the move?' : 'Give up the restore?', ok: 'Give it up', danger: true });
    if (!ok || !ctx.alive()) return;
    S.up.active = false; S.up.file = null;
    try { await ctx.api.del('setup/job'); } catch (e) {
      if (e.code === 'finished') { toast('Too late: it is switching over already, and finishes by itself.', { kind: 'info' }); poll(); return; }
      if (e.code !== 'no_such_job') { if (!common(e)) toast(`Could not give it up: ${errText(e)}`, { kind: 'error' }); return; }
    }
    S.job = null;
    route('Given up. Nothing was kept; pick a route again.');
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
    J.head = h('div', { class: 'su-jobhead' });
    J.track = h('ol', { class: 'su-track', 'aria-label': 'Steps' });
    J.prog = h('div', { class: 'su-prog', 'aria-live': 'polite' });
    J.now = h('div', { class: 'su-now', 'aria-live': 'polite' });
    J.result = h('div', { class: 'su-result' });
    J.acts = h('div', { class: 'su-acts' });
    J.actsKey = null;
    show(lamps(2, j.kind === 'move_here' ? 'Move here' : 'Restore'), J.head, J.track, J.prog, J.now, J.result, J.acts);
    paintJob();
    poll();
  }

  function paintJob() {
    const j = S.job;
    if (!j || !J.head) return;
    const move = j.kind === 'move_here';
    const steps = move ? MOVE : RESTORE;
    const trouble = j.state === 'waiting' || j.state === 'stopped' || j.state === 'failed';
    const shownAs = j.state === 'waiting' || j.state === 'stopped' ? (j.progress?.round > 1 ? 'catching_up' : 'copying') : j.state === 'failed' ? 'uploading' : j.state;
    const at = steps.findIndex(([k]) => k === shownAs);
    replace(J.head,
      h('h1', { class: 'gate-title chrome' }, j.state === 'done' ? (move ? 'Moved here' : 'Restored') : move ? 'Moving here' : 'Restoring'),
      h('p', { class: 'gate-lead' }, move ? ['From ', h('b', { class: 'mono' }, j.from || DASH), j.started_ms ? `, started ${dateTime(j.started_ms, tz)}.` : '.']
        : ['From the file ', h('b', { class: 'mono' }, j.file_name || DASH), ` (${bytes(j.size)}).`]));
    replace(J.track, steps.map(([, label], i) => {
      const st = j.state === 'done' || i < at ? 'done' : i === at ? (trouble ? 'stuck' : 'now') : 'later';
      return h('li', { class: ['su-stop', `is-${st}`], 'aria-current': st === 'now' || st === 'stuck' ? 'step' : null },
        h('span', { class: 'su-stop-dot' }, st === 'done' ? icon('check', 'i--sm') : st === 'stuck' ? icon('warn', 'i--sm') : null),
        h('span', { class: 'su-stop-t' }, label));
    }));

    const p = j.progress;
    let done = p?.bytes_done, total = p?.bytes_total, rate = p?.bytes_per_s, left = p?.eta_s;
    if (!move && j.state === 'uploading') {
      done = j.received; total = j.size;
      const secs = (Date.now() - S.up.t0) / 1000;
      rate = S.up.active && secs > 1 ? (done - S.up.b0) / secs : null;
      left = rate ? (total - done) / rate : null;
    }
    if (j.state === 'done' || j.state === 'connecting' || (p == null && j.state !== 'uploading')) replace(J.prog);
    else {
      replace(J.prog, bar(share(done, total), { label: 'Progress', warm: trouble }),
        h('p', { class: 'su-prog-text' }, h('b', null, bytes(done)), ` of ${bytes(total)} ${move ? 'copied' : 'uploaded'} (${pctText(done, total)})`,
          p?.files_total ? ` · ${count(p.files_done)} of ${count(p.files_total)} files` : null,
          rate ? ` · ${bytes(rate)} a second` : null,
          left != null && rate ? ` · ${eta(left)}` : null));
    }

    const pr = j.problem;
    const say = {
      connecting: () => callout('info', `Reaching ${j.from}…`, 'It checks the move code with the old server.'),
      copying: () => callout('info', 'Copying everything from the old server', 'The old server keeps taking uploads; what comes in meanwhile is copied in the next round. You can close this page: the move goes on.'),
      catching_up: () => callout('info', 'Catching up', 'Copying what reached the old server during the first round.'),
      finishing: () => callout('info', 'The last round', 'Uploads on the old server wait a few seconds so nothing is missed. The CAN Bridge sends them again by itself.'),
      switching: () => callout('info', 'Switching over', "Almost done. This can't be given up any more; it finishes by itself."),
      uploading: () => callout('info', S.up.active ? 'Uploading the file' : 'The upload is paused', S.up.active ? 'Keep this page open. If it stops, choose the same file again: it carries on.' : 'Choose the same file again to carry on.'),
      checking: () => callout('info', 'Checking the file', 'Every part is checked before the server uses it. You can close this page now: it goes on.'),
      waiting: () => callout('warn', 'Waiting: it tries again by itself', `${pr?.message ? `${cap(pr.message)}.` : 'A problem stopped the copy for now.'}${pr?.retry_ms ? ` Next try in about ${dur(Math.max(1000, pr.retry_ms))}.` : ''} Nothing to do unless the old server is gone for good. ${fixFor(pr?.code)}`),
      stopped: () => callout('warn', 'Stopped: it waits for you', `${pr?.message ? cap(pr.message) : 'The copy stopped.'} ${fixFor(pr?.code)}`),
      failed: () => callout('warn', 'The file failed its checks: nothing from it was kept', pr?.message ? `${cap(pr.message)}.` : 'The file is damaged. Copy or download it again, or make a new export on the old server.'),
      done: () => null,
    }[j.state];
    replace(J.now, say ? say() : callout('info', j.state));
    replace(J.result, j.state === 'done' ? result(j) : null);

    // the buttons are built again only when what they offer changes, so a half-typed code stays
    const canForce = move && (j.state === 'waiting' || j.state === 'stopped') && (p?.bytes_done || 0) > 0;
    const canCarry = move && j.state === 'stopped' && CARRY_ON.has(pr?.code);
    const k = j.state === 'done' ? 'done' : j.state === 'failed' ? 'failed' : j.state === 'switching' ? 'none'
      : canCarry ? (canForce ? 'carry+force' : 'carry') : canForce ? 'force' : (!move && j.state === 'uploading' && !S.up.active) ? 'resume' : 'giveup';
    if (k === J.actsKey) return;
    J.actsKey = k;
    const halfway = h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, move
      ? 'If it stops half way, nothing on the old server changes. A lost connection is tried again by itself and it carries on where it stopped. Closing this page does not stop it: open the address again and type the code to follow it.'
      : 'Nothing is used until every part is checked. An upload that stopped carries on when you choose the same file again.'));
    const giveBtn = btn(move ? 'Give up the move' : 'Give up the restore', () => giveUp(S.job), { kind: 'ghost', icon: 'x' });
    const acts = {
      done: () => [h('div', { class: 'row' }, btn('Sign in', () => toSignIn(null), { kind: 'go', icon: 'user' }))],
      failed: () => [h('div', { class: 'row' }, btn('Choose another file', () => clearFailed(() => restoreStep()), { kind: 'go', icon: 'upload' }), btn('Back to the three routes', () => clearFailed(() => route()), { kind: 'ghost', icon: 'arrow-l' }))],
      none: () => [halfway],
      resume: () => [h('div', { class: 'row' }, btn('Choose the file again', () => restoreStep(S.job), { kind: 'go', icon: 'upload' }), giveBtn), halfway],
      carry: () => [carryOn(j), h('div', { class: 'row' }, giveBtn), halfway],
      'carry+force': () => [carryOn(j), forceBox(), h('div', { class: 'row' }, giveBtn), halfway],
      force: () => [forceBox(), h('div', { class: 'row' }, giveBtn), halfway],
      giveup: () => [h('div', { class: 'row' }, giveBtn), halfway],
    }[k];
    replace(J.acts, acts());
  }

  function fixFor(code) {
    return {
      unreachable: 'Check that the old server is on and on the network.',
      no_space: "This server's disk is full: make room (on TrueNAS, a bigger dataset); it carries on by itself.",
      disk: "This server's disk reported a problem. Check it; then it carries on.",
      slow_down: 'The old server asked it to go slower; it carries on by itself.',
      changing_fast: 'Something big is running on the old server; it carries on once that calms down.',
      cancelled: 'Make a new code on the old server and type it below.',
      bad_session: 'Make a new move code on the old server and type it below: it carries on from where it stopped.',
      not_proven: 'The old server could not prove who it is. Make a new move code there and type it below.',
      disk_times: "This server's disk keeps file times only roughly: give the app a dataset on ZFS, ext4, xfs or btrfs, and start again.",
    }[code] || '';
  }

  function carryOn(j) {
    const code = field({ label: 'A new move code from the old server', name: 'code', required: true,
      hint: `Make it on ${j.from || 'the old server'}: Server and access, Move to another server, Make a move code.`,
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMMV-…', class: 'input mono' } });
    const f = form({
      fields: [code], submit: 'Carry on',
      async onSubmit(v) {
        try { S.job = await ctx.api.post('setup/move-here', { from: j.from, code: v.code.trim() }); } catch (e) {
          const onCode = {
            bad_input: e.fields?.code ? 'This code has a typo: check it against the one the old server shows (SMMV- and six groups of five).' : null,
            invalid: e.fields?.code ? 'A newer Sentry Mate made this code. Update this server first, then type it again.' : null,
            move_code_wrong: 'The old server did not take this code. Check it letter by letter, or make a new one there.',
            move_code_expired: 'This code has run out (a code works for 24 hours). Make a new one on the old server.',
            move_code_used: e.message || 'This code was used already. Make a new one on the old server.',
          }[e.code];
          if (onCode) { code.setError(onCode); code.input.focus(); return; }
          const msg = {
            old_server_unreachable: `Nothing answers at ${j.from}. Turn the old server on, check it is on the network, then press Carry on again: the code still works.`,
            move_too_old: 'The old server runs a newer Sentry Mate than this one. Update this server first, then press Carry on again.',
            move_not_home_network: 'This code only works from the old server\'s home network. On the old server, make a new code with "From anywhere" ticked.',
            move_not_offered: 'No move code waits on the old server. Make one there first, then type it here.',
            setup_busy: 'The move is not stopped any more (perhaps another browser carried it on). This page shows it again.',
          }[e.code];
          if (msg) { f.setMessage(msg); if (e.code === 'setup_busy') await loadJob(); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        toast('Carrying on with the move: what was copied before is kept.', { kind: 'info' });
        J.actsKey = null;
        paintJob();
        poll();
      },
    });
    return h('div', { class: 'su-help' },
      h('p', { class: 'su-help-head' }, icon('share', 'i--sm'), h('span', null, 'Carry on with a new move code')),
      h('p', { class: 'su-help-text' }, `What was copied here (${bytes(j.progress?.bytes_done)}) is kept. With a new code from the same server, ${j.from || 'the old server'}, the move goes on from there: nothing is copied twice.`),
      f.el);
  }

  function forceBox() {
    const key = field({ label: "The old server's recovery key", name: 'recovery_key', required: true, hint: 'The SMRK- line saved when the old server was set up.',
      attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters', placeholder: 'SMRK-…', class: 'input mono' } });
    const sure = field({ label: 'The old server is gone for good. Its last changes may be missing, and it must never take uploads again.', name: 'confirm', type: 'checkbox', required: true });
    const f = form({
      fields: [key, sure], submit: 'Finish with what was copied', danger: true,
      async onSubmit(v) {
        if (!v.confirm) { sure.setError('Tick this only when the old server is gone for good. If it is only off, turn it on instead.'); sure.input.focus(); return; }
        try { S.job = await ctx.api.post('setup/move-here/finish', { recovery_key: v.recovery_key.trim(), confirm: v.confirm }); } catch (e) {
          if (e.code === 'wrong_recovery_key') { key.setError("This key does not open the old server's data. Use the recovery key saved when the old server was set up."); key.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong keys from this device. ${waitText(e)}`); return; }
          if (e.status === 503) { f.setMessage(`The server is busy checking another key. ${waitText(e)}`); return; }
          if (e.status === 400 && e.fields?.recovery_key) { key.setError('This does not read as a recovery key: SMRK- and 13 groups of 4. Look for a missing group.'); return; }
          if (e.code === 'not_ready') { f.setMessage('This only works while the copy is stopped part way. It is moving again: wait for it.'); return; }
          if (common(e)) return;
          throw e;
        }
        if (!ctx.alive()) return;
        paintJob();
        poll();
      },
    });
    return h('details', { class: 'su-force' },
      h('summary', null, icon('warn', 'i--sm'), h('span', null, 'Is the old server gone for good?')),
      h('div', { class: 'su-force-body' },
        h('p', null, "Only if its disk died or it can never come back: this server can finish with what was copied so far, opened with the old server's recovery key. The last changes before it stopped may be missing. If the old server is only off, turn it on instead: the move carries on by itself."),
        f.el));
  }

  function result(j) {
    const r = j.result || {};
    const move = j.kind === 'move_here';
    return [
      callout('ok', move ? `Everything from ${r.from || j.from} is here` : `Everything from ${r.file_name || j.file_name} is here`,
        `${count(r.accounts)} ${r.accounts === 1 ? 'account' : 'accounts'} and ${count(r.cars)} ${r.cars === 1 ? 'car' : 'cars'}, finished ${dateTime(r.at_ms, tz)}.`),
      r.forced ? callout('warn', 'Finished with the recovery key', "The old server's last changes before it stopped may be missing. Never start the old server again with uploads on.") : null,
      j.skipped?.length ? h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, `Not carried, because the old server could not read ${j.skipped.length === 1 ? 'it' : 'them'}: `, j.skipped.map((s, i) => [i ? ', ' : '', h('span', { class: 'mono' }, s.path)]), '.')) : null,
      h('div', { class: 'su-help' },
        h('p', { class: 'su-help-head' }, icon('check', 'i--sm'), h('span', null, 'What is left to do')),
        h('ol', { class: 'su-list su-list--n' },
          h('li', null, h('b', null, 'Sign in'), ' with your account from the old server: its accounts and passwords came along. Nobody is signed in yet.'),
          r.same_machine
            ? h('li', null, h('b', null, 'Same machine: '), 'the old server ran here too. Stop it, then give this app port 10100 back (on TrueNAS: edit the app, port 10100:10100). The CAN Bridge then needs no change.')
            : h('li', null, h('b', null, 'The CAN Bridge: '), 'in the app, under Second Sentry Six Manager, change only the server address to ',
              h('span', { class: 'mono' }, `https://${location.hostname}:10100/api/can/logs`), '. Keep the token and the certificate SHA-256: they came along',
              r.cert_sha256 ? [' (', h('span', { class: 'mono su-sha' }, r.cert_sha256), ')'] : null, '.'),
          move ? h('li', null, 'The old server knows it moved. Turn it off when you are happy here; keep its folders a few days.')
            : h('li', null, 'Delete the export file once this server runs: it holds everything, and the recovery key opens it.'))),
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
        if (e.code === 'no_such_job') { S.job = null; route('Nothing is under way any more: it was given up, perhaps in another browser. Pick a route again.'); return; }
        if (!J.lost) { J.lost = true; toast('This page lost touch with the server for a moment. It keeps asking; whatever is under way goes on.', { kind: 'info' }); }
      }
      poll();
    }, 1000);
  }

  async function loadJob() {
    try { S.job = await ctx.api.get('setup/job'); } catch (e) {
      if (common(e)) return;
      if (e.code === 'no_such_job') { S.job = null; route(); return; }
      throw e;
    }
    if (ctx.alive()) jobStep();
  }

  /* ---------------------------------------------------------------- start */
  try { S.setup = await ctx.api.get('setup'); } catch (e) { if (ctx.alive()) ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const s = S.setup;
  if (!s.needs_setup && !(s.claimed && s.job)) { await toSignIn('This server is set up already. Sign in.'); return; }
  if (s.claimed && s.job) { S.job = s.job; jobStep(); }
  else if (s.claimed) route();
  else codeStep(s.state === 'moving_here' ? 'A move into this server is under way, started in another browser. Type the set-up code to follow it here.'
    : s.state === 'restoring' ? 'A restore into this server is under way, started in another browser. Type the set-up code to follow it here.' : null);
  ctx.ready();
}
