// Sentry Mate, "Logbook" UI pack: the set-up screen (/setup), the first run.
//
// While no account exists (GET /api/v1/setup says needs_setup), every address leads here. Nothing on this
// screen needs a shell:
//   1. The set-up code. The person types the one-time code the server prints in its log
//      (POST /setup/claim): whoever reaches a fresh server first can do nothing without it. The browser then
//      holds the claim in a cookie for an hour from its last use.
//   2. One of three choices:
//      - Start fresh (POST /setup/fresh): the first admin account; then the recovery key, shown once, with
//        Copy, Download and a tick "I have saved it" (POST /system/recovery-key/saved).
//      - Move here from another server (POST /setup/move-here): the old server's address and its move code;
//        the copy is followed live (GET /setup/job, once a second) until this server takes over.
//      - Restore from an export file (POST /setup/restore, then PATCH chunks): the file goes up in chunks
//        with its progress, opened with the old server's recovery key; then the checks, live.
//   After a move or a restore the old server's accounts are here: the person signs in with theirs.
// The rules are in docs/ui-api.md ("Moving and first-run setup") and docs/moving.md.

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { field, form } from '../core/form.js';
import { confirmDialog, passwordOk } from '../core/dialog.js';
import { button, icon, flag } from '../core/ui.js';
import { flash } from '../core/flash.js';
import { dateTime, browserZone, count } from '../core/format.js';
import { bytes, bigCode, copyButton, downloadText, stepList, progressBar, callout, PROBLEM } from './move-parts.js';

export const id = 'setup';
export const css = true;
export const title = () => 'Set up Sentry Mate';

const TZ = browserZone;
const HEADER_BYTES = 65536;
const CHUNK_DEFAULT = 1 << 20;
const isLoopback = (s) => s === 'localhost' || s === '[::1]' || s === '::1' || /^127\./.test(s);
const mono = (text) => h('code', { class: 'mono su-code' }, text);
const secs = (ms) => Math.max(1, Math.ceil((ms || 0) / 1000));

const MOVE_STEPS = [
  ['connecting', 'Reach the old server', 'Checking the address and the move code.'],
  ['copying', 'Copy everything', 'The first round copies all the data. The old server keeps taking uploads meanwhile, so nothing is missed.'],
  ['catching_up', 'Catch up', 'Copying what changed on the old server while the first round ran.'],
  ['finishing', 'Last round', 'The old server holds new uploads for a few seconds while the last changes come over.'],
  ['switching', 'Switch over', 'This server opens the copied data and takes over. The old server stops taking uploads.'],
  ['done', 'Done', null],
];
const RESTORE_STEPS = [
  ['uploading', 'Upload the file', 'Keep this page open until the upload is done. If it stops, choose the same file again and it carries on.'],
  ['checking', 'Check the file', 'Every part of the file is checked against the recovery key before anything is used.'],
  ['switching', 'Switch over', 'This server opens the restored data.'],
  ['done', 'Done', null],
];
const ROUND_STEP = ['connecting', 'copying', 'catching_up', 'finishing'];
/** A move stopped for one of these carries on with a new code from the same old server (the copy so far kept). */
const RESUMABLE = new Set(['cancelled', 'bad_session', 'not_proven']);

/** What each refusal of a move here means, and what to do (the server's own words follow). */
const MOVE_REFUSED = {
  old_server_unreachable: ['from', 'Nothing answers at that address.', 'Check the address (the one the CAN Bridge sends to) and that the old server is on. Nothing was started.'],
  move_code_wrong: ['code', 'The old server did not take this code.', 'Check it against the one the old server shows, or make a new one there.'],
  move_code_expired: ['code', 'This move code has expired.', 'A code works for 24 hours. Make a new one on the old server: Settings, Server and access, Move to another server.'],
  move_code_used: ['code', 'This move code was used already.', 'If that was not this server, cancel the code on the old server now. Then make a new one there.'],
  move_too_old: [null, 'The old server runs a newer Sentry Mate than this one.', 'Update this server first, then try again. The code still works.'],
  move_not_home_network: ['code', 'This code works from the old server’s home network only.', 'Make a new code on the old server with “from anywhere” ticked, or set this server up on the same network.'],
  move_not_offered: ['code', 'No move code waits at that address.', 'Make one there first: Settings, Server and access, Move to another server.'],
  setup_busy: [null, 'A move or a restore is already under way here.', 'Reload the page to follow it, or give it up first.'],
};

/* ================================================================== the screen */

export async function mount(ctx) {
  const box = h('div', { class: 'su' });
  ctx.main.append(box);
  let timer = 0;
  let leaveGuard = null;
  const stopPoll = () => { clearTimeout(timer); timer = 0; };
  ctx.onCleanup(() => { stopPoll(); if (leaveGuard) window.removeEventListener('beforeunload', leaveGuard); });
  // The file chosen for a restore, kept while this page is open (an upload that stopped carries on).
  const up = { file: null, key: null, active: false, error: null, sent: 0 };
  let dismissed = null; // a failed restore the person has seen: the choices come back

  /** One screen: the brand, an eyebrow with the step, the title, the lede, then the content. */
  function screen({ step = null, titleText, lede = null, wide = false, cls = null }, ...children) {
    stopPoll();
    replace(box, h('section', { class: ['auth', 'su-card', { 'su-wide': wide }, cls], 'aria-labelledby': 'auth-title' },
      h('a', { class: 'brand auth-brand', href: '/', 'aria-label': 'Sentry Mate' },
        h('img', { class: 'brand-mark', src: new URL('../icon.svg', import.meta.url).href, alt: '' }), h('span', { class: 'brand-name' }, 'Sentry Mate')),
      step ? h('p', { class: 'eyebrow su-step' }, step) : null,
      h('h1', { class: 'auth-title', id: 'auth-title' }, titleText),
      lede ? h('p', { class: 'auth-text' }, lede) : null,
      children));
    window.scrollTo(0, 0);
  }

  const httpWarning = () => (location.protocol === 'http:' && !isLoopback(location.hostname)
    ? callout('bad', 'This page came over plain HTTP.',
      h('p', null, 'Sentry Mate answers HTTPS only, so something between you and the server removed the encryption. Do not type codes or passwords here.'),
      h('p', null, 'Open ', mono(`https://${location.host}/setup`), ' instead.'))
    : null);

  /** Where things stand: the server's answer decides the screen. */
  async function route(note = null) {
    let s;
    try { s = await ctx.api.get('setup'); } catch (err) { if (api.isAbort(err)) return; showUnreachable(err, () => route()); return; }
    if (s.job && (s.job.state === 'done' || (s.needs_setup && s.claimed && !(s.job.state === 'failed' && s.job.id === dismissed)))) { showJob(s.job); return; }
    if (!s.needs_setup) { showSetUp(); return; }
    if (!s.claimed) { showClaim(note); return; }
    showChoices(note);
  }

  function showUnreachable(err, retry) {
    screen({ titleText: 'The server does not answer' },
      h('p', { class: 'auth-text' }, err && err.status ? (err.message || 'It answered with an error.') : 'This page cannot reach Sentry Mate. It may be starting or restarting.'),
      h('p', { class: 'auth-actions' }, button('Try again', { kind: 'primary', iconName: 'reset', onclick: retry })));
  }

  /* ---------------------------------------------------------------- already set up */
  async function showSetUp() {
    // Signed in already (an admin who reloaded the key page, or any account opening /setup): no "Sign in".
    let me = null;
    try { me = await ctx.api.get('me'); } catch (err) { if (api.isAbort(err)) return; }
    if (!me) {
      screen({ titleText: 'Set-up is done', lede: 'Sentry Mate already has its accounts. Sign in to use it.' },
        h('p', { class: 'auth-actions' }, h('a', { class: 'button', href: '/sign-in' }, 'Sign in')));
      return;
    }
    let keyUnsaved = false;
    if (me.role === 'admin') {
      try {
        const n = await ctx.api.get('notices');
        keyUnsaved = ((n && n.items) || []).some((x) => x.code === 'recovery_key_unsaved');
      } catch (err) { if (api.isAbort(err)) return; }
    }
    screen({ titleText: 'Set-up is done', lede: `Sentry Mate already has its accounts, and you are signed in as “${me.name}”.` },
      keyUnsaved ? callout('warn', 'The recovery key is not saved yet.',
        h('p', null, 'Nobody ticked “I have saved it”, so the server still keeps it in its keys folder. Settings, Server and access shows it once more after your password, with Copy and Download.')) : null,
      h('p', { class: 'auth-actions su-actions' },
        keyUnsaved ? h('a', { class: 'button', href: '/settings/server#recovery-key' }, 'Save the recovery key') : null,
        h('a', { class: keyUnsaved ? 'button button-quiet' : 'button', href: '/' }, 'Go to Sentry Mate')));
  }

  /* ---------------------------------------------------------------- 1. the set-up code */
  function showClaim(note = null) {
    const code = field({ label: 'Set-up code', name: 'setup_code', hint: 'It looks like SETUP-7KQ2-9XW4-M3TD. Case, spaces and dashes do not matter.',
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', autocapitalize: 'characters', class: 'field-input mono su-code-input' } });
    const f = form({
      fields: [code], submitLabel: 'Go on', busyLabel: 'Checking…',
      onsubmit: async (v) => {
        const text = String(v.setup_code || '').trim();
        if (!text) throw Object.assign(new Error('Type the set-up code.'), { fields: { setup_code: 'Type the code from the server’s log.' } });
        try {
          await ctx.api.post('setup/claim', { setup_code: text });
        } catch (err) {
          if (err && err.code === 'wrong_setup_code') throw Object.assign(new Error(err.message), { fields: { setup_code: 'That is not the code. Copy it again from the newest line of the server’s log: it is the same on every start.' } });
          if (err && err.status === 429) throw new Error(`Too many wrong codes from this device. Wait ${secs(err.retryAfterMs)} seconds, then try again.`);
          if (err && err.code === 'setup_done') { route(); return; }
          throw err;
        }
        route();
      },
    });
    screen({ step: 'Set-up · step 1 of 2', titleText: 'Set up Sentry Mate', lede: 'This server is new: it has no accounts and no data yet. First, show that it is yours with the set-up code from its log.' },
      httpWarning(),
      note ? callout('info', null, h('p', null, note)) : null,
      h('div', { class: 'su-where' },
        h('h2', { class: 'su-h2' }, 'Where to find the code'),
        h('ol', { class: 'su-list' },
          h('li', null, h('strong', null, 'On TrueNAS: '), 'open ', h('strong', null, 'Apps'), ', choose the Sentry Mate app, and press its ', h('strong', null, 'Logs'), ' button.'),
          h('li', null, 'Look for the line ', mono('first-run set-up code: SETUP-…'), ' and copy the code.')),
        h('p', { class: 'su-quiet' }, 'Elsewhere, the code is in the container’s log too (with plain Docker: ', mono('docker logs sentry-mate'), '). The server prints it on every start until set-up is done.'),
        h('p', { class: 'su-quiet' }, 'Why: only someone who can read the server’s log has the code, so a stranger who reaches this page first cannot take the server over.')),
      f);
    code.control.focus();
  }

  /* ---------------------------------------------------------------- 2. the three choices */
  function showChoices(note = null) {
    const choiceCard = (key, name, what, when, need, onclick) => h('li', { class: 'su-choice' },
      h('button', { type: 'button', class: 'su-choice-btn', onclick, 'aria-describedby': `su-c-${key}` },
        h('span', { class: 'su-choice-name' }, name, icon('arrowRight')),
        h('span', { class: 'su-choice-what', id: `su-c-${key}` }, what)),
      h('dl', { class: 'su-choice-facts' },
        h('dt', null, 'Choose it when'), h('dd', null, when),
        h('dt', null, 'You need'), h('dd', null, need)));
    screen({ step: 'Set-up · step 2 of 2', titleText: 'How should this server start?', wide: true,
      lede: 'Pick one. Nothing is changed until you confirm the next step, and you can come back here from each of them.' },
    note ? callout('info', null, h('p', null, note)) : null,
    h('ol', { class: 'su-choices' },
      choiceCard('fresh', 'Start fresh', 'An empty Sentry Mate. You make the first admin account, and cars appear once their CAN Bridge uploads.',
        'This is your first Sentry Mate, or you want to begin with nothing.', 'A name and a password for yourself.', () => showFresh()),
      choiceCard('move', 'Move here from another server', 'This server copies everything from your old one over the network: logs, accounts, cars, upload tokens. The old one keeps taking uploads until this one takes over by itself.',
        'Both servers are on now and can reach each other.', 'The old server’s address and a move code made there (Settings, Server and access, Move to another server).', () => showMoveForm()),
      choiceCard('restore', 'Restore from an export file', 'You upload the one file the old server made, and open it with the old server’s recovery key.',
        'The old server is off or out of reach, or you are restoring a copy you kept.', 'The .smexport file and the old server’s recovery key (SMRK-…).', () => showRestoreForm())),
    h('p', { class: 'su-quiet su-claim-note' }, 'This browser holds the set-up for an hour from its last step. Another device that types the set-up code takes it over; anything already under way goes on.'));
  }

  const backLink = () => button('Back to the three choices', { cls: 'su-back', iconName: 'chevronLeft', onclick: () => route() });

  /** A refusal that means the claim is gone (another device typed the code) or set-up ended elsewhere. */
  function claimLost(err) {
    if (err && err.code === 'not_claimed') { route('Another device typed the set-up code, or an hour passed: type the code again to go on here.'); return true; }
    if (err && err.code === 'setup_done') { route(); return true; }
    return false;
  }

  /* ---------------------------------------------------------------- start fresh */
  function showFresh() {
    const name = field({ label: 'Your name', name: 'name', hint: 'The name you sign in with. Admins can add more people later, each with their own list of cars.', attrs: { autocomplete: 'username', required: true, maxlength: 64 } });
    const password = field({ label: 'Password', name: 'password', type: 'password', hint: 'A sentence of a few words is easy to remember and hard to guess.', attrs: { autocomplete: 'new-password', required: true } });
    const again = field({ label: 'Password again', name: 'again', type: 'password', attrs: { autocomplete: 'new-password', required: true } });
    const f = form({
      fields: [name, password, again], submitLabel: 'Make the account', busyLabel: 'Setting up…',
      check: async (v) => {
        const n = String(v.name || '').trim();
        if (!n) throw Object.assign(new Error('Type a name.'), { fields: { name: 'Type the name you will sign in with.' } });
        if (!v.password) throw Object.assign(new Error('Check the password.'), { fields: { password: 'Type a password.' } });
        if (v.password !== v.again) throw Object.assign(new Error('The two passwords differ.'), { fields: { again: 'This is not the same as the password above.' } });
        return passwordOk(n, v.password, password.control);
      },
      onsubmit: async (v) => {
        const n = String(v.name || '').trim();
        let res;
        try {
          res = await ctx.api.post('setup/fresh', { name: n, password: v.password, time_zone: browserZone }); // new cars start in this zone (Settings: Cars changes it)
        } catch (err) {
          if (claimLost(err)) return;
          if (err && err.code === 'setup_busy') throw new Error('A move or a restore into this server is under way. Go back, follow it or give it up, then start fresh.');
          throw err;
        }
        showKey(res.recovery_key, (res.me && res.me.name) || n);
      },
    });
    screen({ step: 'Start fresh', titleText: 'Make the admin account', lede: 'You will be the first admin: you see every car and every setting, and you may add more people.' },
      f, h('p', { class: 'su-foot' }, backLink()));
    name.control.focus();
  }

  /** The recovery key, shown this once. */
  function showKey(key, who) {
    const tick = h('input', { type: 'checkbox', id: 'su-saved', class: 'su-tick-box' });
    const go = button('Go on to Sentry Mate', { kind: 'primary', disabled: true, cls: 'su-go' });
    const err = h('p', { class: 'form-error', role: 'alert', hidden: true });
    tick.addEventListener('change', () => { go.disabled = !tick.checked; });
    leaveGuard = (e) => { e.preventDefault(); e.returnValue = ''; };
    window.addEventListener('beforeunload', leaveGuard);
    go.addEventListener('click', async () => {
      go.disabled = true;
      err.hidden = true;
      try {
        await ctx.api.post('system/recovery-key/saved', {});
      } catch (e) {
        err.textContent = `The server did not hear it: ${e.message || 'no answer'}. Try again in a moment. The key stays valid either way.`;
        err.hidden = false;
        go.disabled = false;
        return;
      }
      window.removeEventListener('beforeunload', leaveGuard);
      leaveGuard = null;
      flash(`Welcome, ${who}. Your account is ready. Next: make an upload token for your car in Settings, Cars.`, { next: true });
      location.assign('/');
    });
    const fileText = [
      'Sentry Mate recovery key',
      '',
      key,
      '',
      `Saved ${new Date(api.now()).toISOString().slice(0, 10)} from ${location.host}.`,
      'Keep it away from the server and its backups: in a password manager, or on paper.',
      'It opens this server\'s data and its export files. Lose it together with the keys folder, and nobody can.',
      '',
    ].join('\n');
    screen({ step: 'Start fresh · last step', titleText: 'Save the recovery key', lede: `Your account “${who}” is ready, and you are signed in. One last thing, and it matters.` },
      h('div', { class: 'su-key' },
        bigCode(key, { label: 'Recovery key' }),
        h('p', { class: 'su-key-actions' }, button('Download as a file', { iconName: 'download', onclick: () => downloadText('sentry-mate-recovery-key.txt', fileText) }))),
      callout('warn', 'This is the only time it is shown.',
        h('p', null, 'Everything Sentry Mate stores is encrypted. The key folder on the server opens it day to day; the recovery key opens it when that folder is lost, and it opens export files when you move to another server.'),
        h('p', null, 'Keep it in a password manager or on paper, away from the server and its backups. Lose the key folder and this key together, and nobody can read the data, us included.')),
      h('label', { class: 'su-tick', for: 'su-saved' }, tick, h('span', null, 'I have saved the recovery key somewhere safe, away from this server.')),
      err,
      h('p', { class: 'su-go-row' }, go),
      h('p', { class: 'su-quiet' }, 'Until you tick it, the server keeps a copy in its keys folder. If this page closes first, sign in and open Settings, Server and access: it shows the key once more (after your password) with the same Copy, Download and tick. Ticking it deletes the server’s copy.'));
  }

  /* ---------------------------------------------------------------- move here */
  function showMoveForm() {
    const from = field({ label: 'Old server’s address', name: 'from', hint: 'The address the CAN Bridge sends to, such as 192.168.1.20 or nas.local:10100. Its upload port (10100) is used unless you give another.',
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', inputmode: 'url', class: 'field-input mono' } });
    const code = field({ label: 'Move code', name: 'code', hint: 'SMMV- and six groups of five, from the old server. Case and spaces do not matter.',
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', autocapitalize: 'characters', class: 'field-input mono su-code-input' } });
    const f = form({
      fields: [from, code], submitLabel: 'Start the move', busyLabel: 'Reaching the old server…',
      onsubmit: async (v) => {
        const a = String(v.from || '').trim();
        const c = String(v.code || '').trim();
        const bad = {};
        if (!a) bad.from = 'Type the old server’s address.';
        else if (/^http:\/\//i.test(a)) bad.from = 'The old server’s upload address is https://, not http://. Or type just its name or IP address.';
        if (!c) bad.code = 'Type the move code.';
        else if (!/^\s*SMMV[-\s]*/i.test(c) || c.replace(/[^0-9A-Za-z]/g, '').length !== 34) bad.code = 'A move code is SMMV- and six groups of five letters and digits. Check for a missing group.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Check the marked fields.'), { fields: bad });
        let job;
        try {
          job = await ctx.api.post('setup/move-here', { from: a, code: c });
        } catch (err) {
          if (claimLost(err)) return;
          const r = err && MOVE_REFUSED[err.code];
          if (r) {
            const words = `${r[1]} ${r[2]}`;
            if (r[0]) throw Object.assign(new Error(words), { fields: { [r[0]]: words } });
            throw new Error(words);
          }
          if (err && err.code === 'invalid' && err.fields && err.fields.code) throw err;
          throw err;
        }
        showJob(job);
      },
    });
    screen({ step: 'Move here', titleText: 'Move here from another server',
      lede: 'This server copies everything from the old one: the CAN logs, the accounts, the cars, their upload tokens and the HTTPS key. The old server’s data is never changed.' },
    h('div', { class: 'su-where' },
      h('h2', { class: 'su-h2' }, 'First, on the old server'),
      h('ol', { class: 'su-list' },
        h('li', null, 'Sign in as an admin and open ', h('strong', null, 'Settings → Server and access → Move to another server'), '.'),
        h('li', null, 'Press ', h('strong', null, 'Make a move code'), '. It works once, for 24 hours, from your home network.'),
        h('li', null, 'Copy the code and its server’s address here.'))),
    f,
    h('details', { class: 'su-more' },
      h('summary', null, 'What happens next, and if it stops half way'),
      h('ul', { class: 'su-list' },
        h('li', null, 'The copy runs on the server, not in this browser: you can close the page and come back.'),
        h('li', null, 'If the connection drops, or either server restarts, it waits and carries on where it stopped.'),
        h('li', null, 'If the old server is gone for good part way, you can finish with its recovery key (the last changes may be missing).'),
        h('li', null, 'Give up at any time before the switch: what was copied is removed, and the old server goes on as before.'),
        h('li', null, 'At the end, sign in here with your account from the old server. The CAN Bridge keeps its token; at most its server address changes.'))),
    h('p', { class: 'su-foot' }, backLink()));
    from.control.focus();
  }

  /* ---------------------------------------------------------------- restore from an export file */
  function showRestoreForm({ again = null } = {}) {
    const fileField = field({ label: 'Export file', name: 'file', type: 'file', hint: again
      ? `Choose ${again.file_name} (${bytes(again.size)}) again: the ${bytes(again.received)} already here are not sent twice.`
      : 'The .smexport file the old server made (Settings, Server and access, Download export file).',
    attrs: { accept: '.smexport,application/octet-stream', required: true, class: 'field-input su-file' } });
    const key = field({ label: 'The old server’s recovery key', name: 'recovery_key', hint: 'SMRK- and 13 groups of 4, saved when the old server was set up. The file opens with no other key.',
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', autocapitalize: 'characters', class: 'field-input mono' } });
    const f = form({
      fields: [fileField, key], submitLabel: again ? 'Carry on with the upload' : 'Check and upload', busyLabel: 'Checking the file and the key…',
      onsubmit: async (v) => {
        const file = fileField.control.files && fileField.control.files[0];
        const k = String(v.recovery_key || '').trim();
        const bad = {};
        if (!file) bad.file = 'Choose the export file.';
        else if (again && (file.name !== again.file_name || file.size !== again.size)) bad.file = `That is another file. To carry on, choose ${again.file_name} (${bytes(again.size)}); or give up below and start again with this one.`;
        if (!k) bad.recovery_key = 'Type the recovery key.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Check the marked fields.'), { fields: bad });
        let job;
        try {
          const header = await base64Of(file.slice(0, Math.min(HEADER_BYTES, file.size)));
          job = await ctx.api.post('setup/restore', { name: file.name, size: file.size, recovery_key: k, header });
        } catch (err) {
          if (claimLost(err)) return;
          const fields = {};
          if (err && err.code === 'not_an_export') fields.file = 'This is not a Sentry Mate export file. Choose the .smexport file the old server made.';
          else if (err && err.code === 'export_too_new') fields.file = 'A newer Sentry Mate made this file. Update this server first, then try again.';
          else if (err && err.code === 'wrong_recovery_key') fields.recovery_key = 'This key does not open the file. Use the recovery key the old server had when it made the export.';
          else if (err && err.status === 429) throw new Error(`Too many wrong keys from this device. Wait ${secs(err.retryAfterMs)} seconds, then try again.`);
          else if (err && err.code === 'setup_busy') throw new Error(MOVE_REFUSED.setup_busy.slice(1).join(' '));
          else if (err && err.status === 400 && err.fields && err.fields.recovery_key) fields.recovery_key = `${err.fields.recovery_key}`;
          if (Object.keys(fields).length) throw Object.assign(new Error(Object.values(fields).join(' ')), { fields });
          throw err;
        }
        up.file = file; up.key = k; up.error = null;
        showJob(job);
        upload(job);
      },
    });
    screen({ step: 'Restore', titleText: again ? 'Carry on with the restore' : 'Restore from an export file',
      lede: again ? `The upload of ${again.file_name} stopped at ${bytes(again.received)} of ${bytes(again.size)}.` : 'The file holds everything the old server had. It is checked with the recovery key before the upload starts, and again before anything is used.' },
    f,
    h('details', { class: 'su-more' },
      h('summary', null, 'What happens next, and if it stops half way'),
      h('ul', { class: 'su-list' },
        h('li', null, 'The file goes up in pieces. Keep this page open until the upload is done; the checks after it run on the server.'),
        h('li', null, 'If the upload stops (the page closed, the network dropped), choose the same file again: it carries on where it stopped.'),
        h('li', null, 'A damaged or cut-short file is refused, and nothing of it is kept: download or copy it again.'),
        h('li', null, 'At the end, sign in here with your account from the old server.'))),
    again ? h('p', { class: 'su-foot' }, giveUpButton({ kind: 'restore', state: 'uploading' })) : h('p', { class: 'su-foot' }, backLink()));
  }

  async function base64Of(blob) {
    const buf = new Uint8Array(await blob.arrayBuffer());
    let s = '';
    for (let i = 0; i < buf.length; i += 0x8000) s += String.fromCharCode.apply(null, buf.subarray(i, i + 0x8000));
    return btoa(s);
  }

  /** Send the file from where the server has it. */
  async function upload(job) {
    if (up.active || !up.file) return;
    up.active = true;
    up.error = null;
    const file = up.file;
    const max = Math.max(1, Math.min(job.chunk_max_bytes || CHUNK_DEFAULT, 4 << 20));
    let off = job.received || 0;
    let last = job;
    try {
      while (off < file.size && ctx.alive()) {
        const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
        const headers = { 'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size) };
        if (globalThis.crypto && crypto.subtle) headers['Upload-SHA256'] = hex(await crypto.subtle.digest('SHA-256', buf));
        try {
          last = await ctx.api.patch('setup/restore', buf, { headers });
        } catch (err) {
          if (err && err.code === 'wrong_offset') { last = await ctx.api.get('setup/job'); off = last.received || 0; continue; }
          throw err;
        }
        off = last.received != null ? last.received : off + buf.byteLength;
        up.sent = off;
        drawJob(last);
        if (last.state !== 'uploading') break;
      }
    } catch (err) {
      if (api.isAbort(err) || !ctx.alive()) { up.active = false; return; }
      if (claimLost(err)) { up.active = false; return; }
      up.error = err && err.code === 'checksum_mismatch' ? 'A piece kept arriving damaged. Check the connection, then carry on: it goes on where it stopped.'
        : err && err.status === 0 ? 'The connection dropped. Carry on when the server answers again: the upload goes on where it stopped.'
          : `The upload stopped: ${err.message || 'the server refused a piece'}.`;
    }
    up.active = false;
    try { last = await ctx.api.get('setup/job'); } catch { /* drawn from the last answer */ }
    showJob(last);
  }
  const hex = (b) => [...new Uint8Array(b)].map((x) => x.toString(16).padStart(2, '0')).join('');

  /* ---------------------------------------------------------------- following a move or a restore */
  let jobBox = null;

  function giveUpButton(job) {
    return button(job.kind === 'restore' ? 'Give up the restore' : 'Give up the move', { kind: 'quiet', onclick: async () => {
      const ok = await confirmDialog({
        title: job.kind === 'restore' ? 'Give up the restore?' : 'Give up the move?',
        text: job.kind === 'restore'
          ? 'What was uploaded is removed. The export file itself is not changed: you can restore it again later.'
          : 'What was copied here is removed, and the old server is told. The old server goes on as before, with all its data.',
        yes: 'Give up', danger: true,
      });
      if (!ok) return;
      try { await ctx.api.del('setup/job'); }
      catch (err) {
        if (claimLost(err)) return;
        if (err && err.code === 'finished') { flash('It is switching over already: it finishes by itself.', { kind: 'info' }); return; }
        if (!(err && err.code === 'no_such_job')) { flash(`Not given up: ${err.message}`, { kind: 'error' }); return; }
      }
      up.file = null; up.key = null; up.error = null;
      route(job.kind === 'restore' ? 'The restore was given up. Nothing of it is kept.' : 'The move was given up. The old server goes on as before.');
    } });
  }

  function showJob(job) {
    const restore = job.kind === 'restore';
    jobBox = h('div', { class: 'su-job', 'aria-live': 'polite' });
    screen({ step: restore ? 'Restore' : 'Move here', titleText: restore ? 'Restoring from the export file' : 'Moving here', wide: false }, jobBox);
    drawJob(job);
  }

  function drawJob(job) {
    if (!jobBox) return;
    const restore = job.kind === 'restore';
    const st = job.state;
    const p = job.progress || null;
    const prob = job.problem;
    const parts = [];
    if (st === 'done') {
      stopPoll();
      drawDone(job);
      return;
    }
    // what it is moving from
    parts.push(h('p', { class: 'su-from' }, restore
      ? ['From the file ', h('strong', { class: 'su-break' }, job.file_name || 'the export file'), job.size ? ` (${bytes(job.size)})` : '']
      : ['From ', h('strong', { class: 'mono su-break' }, job.from || 'the old server'), job.started_ms ? [', started ', dateTime(job.started_ms, TZ)] : '']));
    const steps = restore ? RESTORE_STEPS : MOVE_STEPS;
    let current = st;
    if (st === 'waiting' || st === 'stopped') current = ROUND_STEP[Math.min(3, (p && p.round) || 0)];
    if (st === 'failed') current = 'uploading';
    const uploadStalled = restore && st === 'uploading' && !up.active;
    parts.push(stepList(steps, current, {
      problem: st === 'waiting' || st === 'stopped' || uploadStalled,
      // a restore that failed for good: no "it carries on" under the step, the box below says what to do
      failed: st === 'failed' ? 'The restore stopped here. Nothing of the file was kept.' : null,
    }));
    if (p && st !== 'connecting' && st !== 'failed') parts.push(progressBar(restore && st === 'uploading' ? { ...p, bytes_done: job.received, bytes_total: job.size } : p, { label: restore ? 'Uploaded' : 'Copied' }));

    // problems, and what to do
    if (st === 'waiting') {
      const w = PROBLEM[prob && prob.code] || [prob ? prob.message : 'It waits.', 'It tries again by itself.'];
      parts.push(callout('warn', w[0], h('p', null, w[1]),
        prob && prob.message ? h('p', { class: 'su-log' }, 'The server says: ', h('q', null, prob.message)) : null,
        prob && prob.retry_ms ? h('p', { class: 'su-quiet' }, `Next try in about ${secs(prob.retry_ms)} seconds.`) : null));
    } else if (st === 'stopped') {
      const resumable = !restore && RESUMABLE.has(prob && prob.code);
      const w = PROBLEM[prob && prob.code] || [prob ? prob.message : 'The move stopped.', 'It waits for you.'];
      parts.push(callout('bad', w[0], resumable ? null : h('p', null, w[1]),
        prob && prob.message && prob.code !== 'cancelled' ? h('p', { class: 'su-log' }, 'The server says: ', h('q', null, prob.message)) : null,
        resumable
          ? h('p', null, 'What was copied so far stays here. Make a new move code on the old server and type it below: the move carries on from where it stopped.')
          : h('p', null, 'To move after all: make a new move code on the old server, give up here, then choose “Move here” again with the new code.')));
      if (resumable) parts.push(carryOn(job));
    } else if (st === 'failed') {
      const w = PROBLEM[prob && prob.code] || [prob ? prob.message : 'The restore failed.', 'Nothing of it was kept.'];
      parts.push(callout('bad', w[0], h('p', null, w[1]), prob && prob.message ? h('p', { class: 'su-log' }, 'The server says: ', h('q', null, prob.message)) : null));
    } else if (restore && st === 'uploading') {
      if (up.active) parts.push(h('p', { class: 'su-quiet' }, 'Keep this page open until the upload is done.'));
      else if (up.error) parts.push(callout('warn', 'The upload stopped.', h('p', null, up.error)));
      else parts.push(callout('info', 'The upload stopped part way.', h('p', null, `The server has ${bytes(job.received)} of ${bytes(job.size)}. Choose the same file again to carry on from there.`)));
    } else if (st === 'switching') {
      parts.push(h('p', { class: 'su-quiet' }, 'It finishes by itself in a few seconds. The page may lose the server for a moment: it keeps asking.'));
    } else if (!restore) {
      parts.push(h('p', { class: 'su-quiet' }, 'The copy runs on the server: you can close this page and come back. The old server keeps taking uploads until the switch.'));
    }
    if (job._net) parts.push(callout('info', 'The server does not answer this page right now.', h('p', null, 'It may be restarting. The page keeps asking every few seconds.')));

    // actions
    const acts = [];
    if (restore && st === 'uploading' && !up.active) {
      acts.push(up.file && up.key
        ? button('Carry on with the upload', { kind: 'primary', onclick: () => upload(job) })
        : button('Choose the file again', { kind: 'primary', onclick: () => showRestoreForm({ again: job }) }));
    }
    if (st === 'failed') {
      dismissed = job.id;
      acts.push(button('Choose a file again', { kind: 'primary', onclick: () => { up.file = null; showRestoreForm(); } }), backLink());
    }
    if (!['switching', 'done', 'failed'].includes(st)) acts.push(giveUpButton(job));
    if (acts.length) parts.push(h('div', { class: 'su-actions' }, acts));

    // the last resort: the old server is gone while the copy waits
    if (!restore && (st === 'waiting' || st === 'stopped') && p && p.bytes_done) parts.push(finishWithKey(job));

    if (!restore) {
      parts.push(h('details', { class: 'su-more' },
        h('summary', null, 'If it stops half way'),
        h('ul', { class: 'su-list' },
          h('li', null, 'Nothing of the old server is changed by a move: it keeps all its data and goes on taking uploads until the switch.'),
          h('li', null, 'A dropped connection or a restart of either server: it waits, then carries on where it stopped.'),
          h('li', null, 'Give up: what was copied here is removed, and the page comes back to the three choices.'),
          h('li', null, 'After the switch, the old server takes no more uploads and says where its data went. Undo the move there if you need it back.'))));
    }
    // keep a form the person is typing in (the new code, the last-resort key) across a redraw
    const carry = jobBox.querySelector('.su-carry');
    if (carry && parts.some((x) => x.classList && x.classList.contains('su-carry'))) {
      parts[parts.findIndex((x) => x.classList && x.classList.contains('su-carry'))] = carry;
    }
    const open = jobBox.querySelector('.su-finish');
    if (open && open.open && parts.some((x) => x.classList && x.classList.contains('su-finish'))) {
      const i = parts.findIndex((x) => x.classList && x.classList.contains('su-finish'));
      parts[i] = open;
    }
    replace(jobBox, parts);
    // poll while the server works
    const serverWorks = !(restore && st === 'uploading') && st !== 'failed' && st !== 'done';
    if (serverWorks || (restore && st === 'uploading' && !up.active)) schedule(job, serverWorks ? 1000 : 5000);
  }

  function schedule(job, ms) {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      if (!ctx.alive()) return;
      if (up.active) return;
      try {
        const next = await ctx.api.get('setup/job');
        drawJob(next);
      } catch (err) {
        if (api.isAbort(err)) return;
        if (claimLost(err)) return;
        if (err && err.code === 'no_such_job') { route('Nothing is under way any more: it was given up, perhaps from another device.'); return; }
        drawJob({ ...job, _net: true });
        clearTimeout(timer);
        timer = setTimeout(() => schedule(job, 0), 3000);
      }
    }, ms);
  }

  /** A move stopped for the person (cancelled on the old server, its session ended): a new code carries it on. */
  function carryOn(job) {
    const code = field({ label: 'New move code', name: 'code', hint: `Make it on ${job.from || 'the old server'}: Settings, Server and access, Move to another server, Make a move code. Case and spaces do not matter.`,
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', autocapitalize: 'characters', class: 'field-input mono su-code-input' } });
    const f = form({
      fields: [code], submitLabel: 'Carry on', busyLabel: 'Reaching the old server…',
      onsubmit: async (v) => {
        const c = String(v.code || '').trim();
        if (!c) throw Object.assign(new Error('Type the new move code.'), { fields: { code: 'Type the move code.' } });
        if (!/^\s*SMMV[-\s]*/i.test(c) || c.replace(/[^0-9A-Za-z]/g, '').length !== 34) throw Object.assign(new Error('Check the code.'), { fields: { code: 'A move code is SMMV- and six groups of five letters and digits. Check for a missing group.' } });
        let next;
        try {
          next = await ctx.api.post('setup/move-here', { from: job.from, code: c });
        } catch (err) {
          if (claimLost(err)) return;
          if (err && err.code === 'setup_busy') { flash('The move is not stopped any more (perhaps another device carried it on).'); drawJob(await ctx.api.get('setup/job')); return; }
          const r = err && MOVE_REFUSED[err.code];
          if (r) throw Object.assign(new Error(`${r[1]} ${r[2]}`), { fields: r[0] === 'code' ? { code: `${r[1]} ${r[2]}` } : {} });
          throw err;
        }
        flash('Carrying on with the move: what was copied before is kept.');
        jobBox.querySelector('.su-carry')?.remove();
        drawJob(next);
      },
    });
    return h('div', { class: ['su-where', 'su-carry'] },
      h('h2', { class: 'su-h2' }, 'Carry on with a new move code'),
      h('p', { class: 'su-quiet' }, `What was copied here (${bytes(job.progress && job.progress.bytes_done)}) is kept. With a new code from the same server, the move goes on from there: nothing is copied twice.`),
      f);
  }

  function finishWithKey(job) {
    const key = field({ label: 'The old server’s recovery key', name: 'recovery_key', hint: 'SMRK- and 13 groups of 4.', attrs: { autocomplete: 'off', required: true, spellcheck: 'false', class: 'field-input mono' } });
    const sure = h('input', { type: 'checkbox', name: 'confirm', id: 'su-sure' });
    const sureField = h('div', { class: 'field', dataset: { field: 'confirm' } },
      h('label', { class: 'su-tick su-tick-small', for: 'su-sure' }, sure, h('span', null, 'The old server is gone for good. I will never start it again with the CAN Bridge pointed at it.')),
      h('p', { class: 'field-error', hidden: true }));
    const f = form({
      fields: [key, sureField], submitLabel: 'Finish with what was copied', busyLabel: 'Checking the key…',
      onsubmit: async (v) => {
        if (!sure.checked) throw Object.assign(new Error('Tick the box first.'), { fields: { confirm: 'Tick this only if the old server is really gone.' } });
        try {
          const next = await ctx.api.post('setup/move-here/finish', { recovery_key: String(v.recovery_key || '').trim(), confirm: true });
          drawJob(next);
        } catch (err) {
          if (claimLost(err)) return;
          if (err && err.code === 'wrong_recovery_key') throw Object.assign(new Error(err.message), { fields: { recovery_key: 'This key does not open the old server’s data. Use the one it showed when it was set up.' } });
          if (err && err.code === 'not_ready') throw new Error('Not now: this works only while the copy waits or has stopped part way. Reload the page to see where it stands.');
          if (err && err.status === 429) throw new Error(`Too many wrong keys. Wait ${secs(err.retryAfterMs)} seconds, then try again.`);
          throw err;
        }
      },
    });
    return h('details', { class: ['su-more', 'su-finish'] },
      h('summary', null, 'The old server is gone for good?'),
      h('p', { class: 'su-quiet' }, `If its disk died, this server can open what was copied (${bytes(job.progress && job.progress.bytes_done)}) with the old server’s recovery key. The last changes may be missing. Use this only as a last resort.`),
      f);
  }

  function drawDone(job) {
    const r = job.result || {};
    const restore = job.kind === 'restore';
    const host = location.hostname.includes(':') ? `[${location.hostname}]` : location.hostname;
    const newAddress = `https://${host}:10100/api/can/logs`;
    const t = document.getElementById('auth-title');
    if (t) t.textContent = restore ? 'Restored' : 'Moved here';
    replace(jobBox,
      callout('ok', restore ? 'Restored. Sentry Mate is set up.' : 'Moved. Sentry Mate is set up.',
        h('p', null, restore
          ? ['Everything from ', h('strong', { class: 'su-break' }, r.file_name || job.file_name || 'the export file'), ' is here: ']
          : ['Everything from ', h('strong', { class: 'mono su-break' }, r.from || job.from || 'the old server'), ' is here: '],
        count(r.accounts || 0, 'account'), ', ', count(r.cars || 0, 'car'), r.at_ms ? [', as it was at ', dateTime(r.at_ms, TZ)] : null, '.'),
        r.forced ? h('p', null, flag('Finished with the recovery key', { kind: 'warn' }), ' The last changes on the old server may be missing.') : null),
      h('h2', { class: 'su-h2' }, 'Next: sign in'),
      h('p', { class: 'su-text' }, 'Use your name and password from the old server: its accounts came along. Nobody is signed in yet.'),
      h('p', { class: 'su-actions' }, h('a', { class: 'button su-go', href: '/sign-in' }, 'Sign in')),
      h('h2', { class: 'su-h2' }, 'The CAN Bridge'),
      r.same_machine
        ? h('p', { class: 'su-text' }, 'The old server ran on this machine. Stop the old app, then give this app port 10100 back (on TrueNAS: edit the app and set its upload port to 10100). The CAN Bridge then needs no change at all.')
        : h('div', { class: 'su-text' },
          h('p', null, 'Each CAN Bridge keeps its token and its certificate SHA-256. If this server has a new address, change only the server address in the app’s “Second Sentry Six Manager” section:'),
          h('p', { class: 'su-addr' }, mono(newAddress), copyButton(() => newAddress, { cls: 'button-small' })),
          h('p', { class: 'su-quiet' }, 'Nothing is lost meanwhile: the bridge keeps its logs and sends them once it reaches this server.')),
      r.cert_sha256 ? h('div', { class: 'su-quiet su-cert' }, h('p', null, 'The certificate SHA-256 is unchanged:'), h('p', { class: 'mono su-break' }, r.cert_sha256)) : null,
      !restore ? h('p', { class: 'su-quiet' }, 'The old server now says its data has moved, and takes no more uploads. Keep its folders for a while as a backup.') : null);
  }

  await route();
  ctx.ready();
  return {};
}

