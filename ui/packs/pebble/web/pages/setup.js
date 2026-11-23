// Sentry Mate, "Pebble" UI pack: the set-up screen (/setup), the first run.
//
// A carousel of cards, one step per card, with dots for where you are and Back and Next buttons (it never needs
// a swipe). While no account exists (GET /api/v1/setup says needs_setup) every address leads here.
//   1. The set-up code from the server's log (POST /setup/claim): whoever reaches a fresh server first can do
//      nothing without it. The browser then holds the claim for an hour from its last use.
//   2. One of three ways to start:
//      - Start fresh (POST /setup/fresh): the first admin account, then the recovery key, shown once, with Copy,
//        Download and a tick "I have saved it" (POST /system/recovery-key/saved).
//      - Move here from another server (POST /setup/move-here): the old server's address and its move code; the
//        copy is followed live (GET /setup/job, once a second) until this server takes over.
//      - Restore from an export file (POST /setup/restore, then PATCH pieces): the file goes up in pieces with its
//        progress, opened with the old server's recovery key; then the checks, live.
//   After a move or a restore the old server's accounts are here: the person signs in with theirs.
// The rules are in docs/ui-api.md ("Moving and first-run setup") and docs/moving.md.

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { formatDateTime } from '../shared/time.js';
import { field, form } from '../core/form.js';
import { confirmSheet, passwordOk } from '../core/sheet.js';
import { btn, linkBtn, icon, chip } from '../core/ui.js';
import { toast } from '../core/toast.js';
import { scrollToTop } from '../core/display.js';
import { authCard, httpsNote } from './sign-in.js';
import { bytes, bigCode, copyButton, downloadText, stepCards, progressBar, callout, PROBLEM } from './move-parts.js';

export const id = 'setup';

// the browser's time zone goes with the first admin: new cars start in it (Settings: Display and units changes it)
const browserZone = () => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || undefined; } catch { return undefined; } };
export const title = () => 'Set up Sentry Mate';

const TZ = (() => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'; } catch { return 'UTC'; } })();
const HEADER_BYTES = 65536;
const CHUNK_DEFAULT = 1 << 20;
const mono = (text) => h('code', { class: 'mono' }, text);
const secs = (ms) => Math.max(1, Math.ceil((ms || 0) / 1000));
const when = (ms) => formatDateTime(ms, TZ);
const count = (n, one) => `${n} ${n === 1 ? one : `${one}s`}`;

/** The cards of each way, for the dots. */
const PATHS = {
  none: ['Set-up code', 'How to start', 'Details', 'Done'],
  fresh: ['Set-up code', 'How to start', 'Your account', 'Recovery key'],
  move: ['Set-up code', 'How to start', 'Old server', 'Copy', 'Done'],
  restore: ['Set-up code', 'How to start', 'File and key', 'Upload and check', 'Done'],
};

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
const RESUMABLE = new Set(['cancelled', 'bad_session', 'not_proven']);

/** What each refusal of a move here means, and what to do. */
const MOVE_REFUSED = {
  old_server_unreachable: ['from', 'Nothing answers at that address.', 'Check the address (the one the CAN Bridge sends to) and that the old server is on. Nothing was started.'],
  move_code_wrong: ['code', 'The old server did not take this code.', 'Check it against the one the old server shows, or make a new one there.'],
  move_code_expired: ['code', 'This move code has expired.', 'A code works for 24 hours. Make a new one on the old server: Settings, Server, Move to another server.'],
  move_code_used: ['code', 'This move code was used already.', 'If that was not this server, cancel the code on the old server now. Then make a new one there.'],
  move_too_old: [null, 'The old server runs a newer Sentry Mate than this one.', 'Update this server first, then try again. The code still works.'],
  move_not_home_network: ['code', 'This code works from the old server’s home network only.', 'Make a new code on the old server with “from anywhere” ticked, or set this server up on the same network.'],
  move_not_offered: ['code', 'No move code waits at that address.', 'Make one there first: Settings, Server, Move to another server.'],
  setup_busy: [null, 'A move or a restore is already under way here.', 'Reload the page to follow it, or give it up first.'],
};
const codeShape = (c) => /^\s*SMMV[-\s]*/i.test(c) && c.replace(/[^0-9A-Za-z]/g, '').length === 34;

export async function mount(ctx) {
  const box = h('div', { class: 'su' });
  ctx.view.append(box);
  let timer = 0;
  let leaveGuard = null;
  let shownAt = -1;
  const stopPoll = () => { clearTimeout(timer); timer = 0; };
  ctx.onCleanup(() => { stopPoll(); if (leaveGuard) window.removeEventListener('beforeunload', leaveGuard); });
  const up = { file: null, key: null, active: false, error: null, sent: 0 };
  let dismissed = null;

  /**
   * One card of the carousel: card({ path, at, titleText, lede, back, next }, ...children). The dots show card
   * `at` of the way `path`; back is () => void (a Back button), next is a button or form submit already inside.
   */
  function card({ path = 'none', at = 0, titleText, lede = null, back = null, nav = null, wide = false }, ...children) {
    stopPoll();
    const steps = PATHS[path];
    const dir = shownAt < 0 ? null : at > shownAt ? 'from-right' : at < shownAt ? 'from-left' : null;
    shownAt = at;
    const dots = h('ol', { class: 'dots', 'aria-label': `Step ${at + 1} of ${steps.length}: ${steps[at]}` },
      steps.map((name, i) => h('li', { class: ['dot-step', { 'is-done': i < at, 'is-now': i === at }], title: name, 'aria-current': i === at ? 'step' : null },
        h('span', { class: 'sr-only' }, `${name}${i < at ? ' (done)' : i === at ? ' (now)' : ''}`))));
    const navRow = back || nav ? h('div', { class: 'su-nav' },
      back ? btn('Back', { kind: 'soft', icon: 'chevronLeft', onclick: back }) : h('span'),
      nav || h('span')) : null;
    replace(box, authCard(
      h('div', { class: 'su-top' }, dots, h('p', { class: 'su-count' }, `${at + 1} / ${steps.length}`)),
      h('div', { class: ['su-slide', dir, { 'is-wide': wide }] },
        h('p', { class: 'eyebrow' }, steps[at]),
        h('h1', { class: 'auth-title', id: 'auth-title' }, titleText),
        lede ? h('p', { class: 'auth-text' }, lede) : null,
        children),
      navRow));
    box.firstElementChild.classList.toggle('is-wide', wide);
    scrollToTop();
  }

  /** Where things stand: the server's answer decides the card. */
  async function route(noteText = null) {
    let s;
    try { s = await ctx.api.get('setup'); } catch (err) { if (api.isAbort(err)) return; showUnreachable(err, () => route()); return; }
    if (s.job && (s.job.state === 'done' || (s.needs_setup && s.claimed && !(s.job.state === 'failed' && s.job.id === dismissed)))) { showJob(s.job); return; }
    if (!s.needs_setup) { await showSetUp(); return; }
    if (!s.claimed) { showClaim(noteText); return; }
    showChoices(noteText);
  }

  function showUnreachable(err, retry) {
    card({ titleText: 'The server does not answer', lede: err && err.status ? (err.message || 'It answered with an error.') : 'This page cannot reach Sentry Mate. It may be starting or restarting.',
      nav: btn('Try again', { kind: 'primary', icon: 'reset', onclick: retry }) });
  }

  /* ---------------------------------------------------------------- already set up */
  async function showSetUp() {
    let me = null;
    try { me = await ctx.api.get('me'); } catch (err) { if (api.isAbort(err)) return; }
    if (!me) {
      card({ at: 3, titleText: 'Set-up is done', lede: 'Sentry Mate already has its accounts. Sign in to use it.', nav: linkBtn('Sign in', '/sign-in', { kind: 'primary', icon: 'arrowRight' }) });
      return;
    }
    let keyUnsaved = false;
    if (me.role === 'admin') {
      try { const n = await ctx.api.get('notices'); keyUnsaved = ((n && n.items) || []).some((x) => x.code === 'recovery_key_unsaved'); } catch (err) { if (api.isAbort(err)) return; }
    }
    card({ path: 'fresh', at: 3, titleText: 'Set-up is done', lede: `Sentry Mate already has its accounts, and you are signed in as “${me.name}”.`,
      nav: keyUnsaved ? linkBtn('Save the recovery key', '/settings/server#recovery-key', { kind: 'primary', icon: 'key' }) : linkBtn('Go to Sentry Mate', '/', { kind: 'primary', icon: 'arrowRight' }) },
    keyUnsaved ? callout('warn', 'The recovery key is not saved yet.',
      h('p', null, 'Nobody ticked “I have saved it”, so the server still keeps it in its keys folder. Settings, Server shows it once more after your password.')) : null,
    keyUnsaved ? h('p', { class: 'auth-foot' }, h('a', { href: '/' }, 'Go to Sentry Mate')) : null);
  }

  /* ---------------------------------------------------------------- 1. the set-up code */
  function showClaim(noteText = null) {
    const code = field({ label: 'Set-up code', name: 'setup_code', hint: 'It looks like SETUP-7KQ2-9XW4-M3TD. Case, spaces and dashes do not matter.',
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', autocapitalize: 'characters', class: 'input-code' } });
    const f = form({
      fields: [code], submitLabel: 'Next', busyLabel: 'Checking…', submitIcon: 'arrowRight', cls: 'su-form',
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
    card({ at: 0, titleText: 'Set up Sentry Mate', lede: 'This server is new: no accounts and no data yet. First show that it is yours, with the set-up code from its log.' },
      httpsNote(),
      noteText ? callout('info', null, h('p', null, noteText)) : null,
      h('div', { class: 'su-help' },
        h('p', { class: 'su-help-title' }, icon('info'), h('span', null, 'Where to find the code')),
        h('ol', { class: 'su-list' },
          h('li', null, 'On TrueNAS: open Apps, choose the Sentry Mate app and press its Logs button.'),
          h('li', null, 'Find the line ', mono('first-run set-up code: SETUP-…'), ' and copy the code.'),
          h('li', null, 'With plain Docker: ', mono('docker logs sentry-mate'), '.')),
        h('p', { class: 'hint' }, 'Only someone who can read the server’s log has the code, so a stranger who reaches this page first cannot take the server over.')),
      f);
    code.control.focus();
  }

  /* ---------------------------------------------------------------- 2. the three ways */
  function showChoices(noteText = null) {
    let picked = null;
    const next = btn('Next', { kind: 'primary', icon: 'arrowRight', disabled: true, cls: 'btn-end', onclick: () => ({ fresh: showFresh, move: showMoveForm, restore: () => showRestoreForm() })[picked]() });
    const choice = (key, ic, name, what, need) => h('button', { type: 'button', role: 'radio', 'aria-checked': 'false', class: 'su-choice', dataset: { key },
      onclick: (e) => {
        picked = key;
        for (const b of e.currentTarget.parentElement.children) b.setAttribute('aria-checked', String(b.dataset.key === key));
        next.disabled = false;
      },
      ondblclick: () => next.click(),
    },
    h('span', { class: 'row-ic tone-coral', 'aria-hidden': 'true' }, icon(ic)),
    h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, name), h('span', { class: 'row-sub' }, what), h('span', { class: 'su-need' }, 'You need: ', need)),
    h('span', { class: 'row-check', 'aria-hidden': 'true' }, icon('check')));
    card({ at: 1, titleText: 'How should this server start?', lede: 'Pick one, then Next. Nothing changes until you confirm on the next card, and Back always comes here.', nav: next },
      noteText ? callout('info', null, h('p', null, noteText)) : null,
      h('div', { class: 'su-choices', role: 'radiogroup', 'aria-label': 'How to start' },
        choice('fresh', 'spark', 'Start fresh', 'An empty Sentry Mate. You make the first admin account; cars appear once their CAN Bridge uploads.', 'a name and a password.'),
        choice('move', 'move', 'Move here from another server', 'Copies everything from your old server over the network. The old one keeps taking uploads until this one takes over.', 'its address and a move code made there.'),
        choice('restore', 'file', 'Restore from an export file', 'Upload the one file the old server made, opened with its recovery key. For an old server that is off or gone.', 'the .smexport file and the recovery key (SMRK-…).')),
      h('p', { class: 'hint' }, 'This browser holds the set-up for an hour from its last step. Another device that types the set-up code takes it over.'));
  }

  const backToChoices = () => route();

  function claimLost(err) {
    if (err && err.code === 'not_claimed') { route('Another device typed the set-up code, or an hour passed: type the code again to go on here.'); return true; }
    if (err && err.code === 'setup_done') { route(); return true; }
    return false;
  }

  /* ---------------------------------------------------------------- start fresh */
  function showFresh() {
    const name = field({ label: 'Your name', name: 'name', hint: 'The name you sign in with. Admins can add more people later.', attrs: { autocomplete: 'username', required: true, maxlength: 64 } });
    const password = field({ label: 'Password', name: 'password', type: 'password', hint: 'A few words in a row are easy to remember and hard to guess.', attrs: { autocomplete: 'new-password', required: true } });
    const again = field({ label: 'Password again', name: 'again', type: 'password', attrs: { autocomplete: 'new-password', required: true } });
    const f = form({
      fields: [name, password, again], submitLabel: 'Make the account', busyLabel: 'Setting up…', submitIcon: 'arrowRight', cls: 'su-form',
      onsubmit: async (v) => {
        const n = String(v.name || '').trim();
        if (!n) throw Object.assign(new Error('Type a name.'), { fields: { name: 'Type the name you will sign in with.' } });
        if (v.password !== v.again) throw Object.assign(new Error('The two passwords differ.'), { fields: { again: 'This is not the same as the password above.' } });
        if (!(await passwordOk(n, v.password, password.control))) return;
        let res;
        try {
          res = await ctx.api.post('setup/fresh', { name: n, password: v.password, time_zone: browserZone() });
        } catch (err) {
          if (claimLost(err)) return;
          if (err && err.code === 'setup_busy') throw new Error('A move or a restore into this server is under way. Go back, follow it or give it up, then start fresh.');
          throw err;
        }
        showKey(res.recovery_key, (res.me && res.me.name) || n);
      },
    });
    card({ path: 'fresh', at: 2, titleText: 'Make the admin account', lede: 'You will be the first admin: you see every car and every setting, and you may add more people.', back: backToChoices },
      httpsNote(), f);
    name.control.focus();
  }

  function showKey(key, who) {
    const tick = h('input', { type: 'checkbox', id: 'su-saved', class: 'tick-input' });
    const go = btn('Go to Sentry Mate', { kind: 'primary', icon: 'arrowRight', disabled: true, cls: 'btn-end' });
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
      toast(`Welcome, ${who}. Next: make an upload token for your car in Settings, Cars.`, { next: true });
      location.assign('/');
    });
    const fileText = [
      'Sentry Mate recovery key', '', key, '',
      `Saved ${new Date(api.now()).toISOString().slice(0, 10)} from ${location.host}.`,
      'Keep it away from the server and its backups: in a password manager, or on paper.',
      'It opens this server\'s data and its export files. Lose it together with the keys folder, and nobody can.', '',
    ].join('\n');
    card({ path: 'fresh', at: 3, titleText: 'Save the recovery key', lede: `Your account “${who}” is ready and you are signed in. One last thing, and it matters.`, nav: go },
      bigCode(key, { label: 'Recovery key' }),
      h('div', { class: 'form-actions' }, btn('Download as a file', { kind: 'soft', icon: 'download', onclick: () => downloadText('sentry-mate-recovery-key.txt', fileText) })),
      callout('warn', 'This is the only time it is shown.',
        h('p', null, 'Everything Sentry Mate stores is encrypted. The keys folder on the server opens it day to day; the recovery key opens it when that folder is lost, and opens export files when you move.')),
      h('label', { class: 'tick', for: 'su-saved' }, tick, h('span', { class: 'tick-box', 'aria-hidden': 'true' }, icon('check')), h('span', null, 'I have saved the recovery key somewhere safe, away from this server.')),
      err,
      h('p', { class: 'hint' }, 'Until you tick it, the server keeps a copy in its keys folder. If this page closes first, sign in and open Settings, Server: it shows the key once more after your password.'));
  }

  /* ---------------------------------------------------------------- move here */
  function showMoveForm() {
    const from = field({ label: 'Old server’s address', name: 'from', hint: 'The address the CAN Bridge sends to, such as 192.168.1.20 or nas.local:10100.',
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', inputmode: 'url', class: 'input-mono' } });
    const code = field({ label: 'Move code', name: 'code', hint: 'SMMV- and six groups of five, from the old server. Case and spaces do not matter.',
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', autocapitalize: 'characters', class: 'input-code' } });
    const f = form({
      fields: [from, code], submitLabel: 'Start the move', busyLabel: 'Reaching the old server…', submitIcon: 'arrowRight', cls: 'su-form',
      onsubmit: async (v) => {
        const a = String(v.from || '').trim();
        const c = String(v.code || '').trim();
        const bad = {};
        if (!a) bad.from = 'Type the old server’s address.';
        else if (/^http:\/\//i.test(a)) bad.from = 'The old server’s upload address is https://, not http://. Or type just its name or IP address.';
        if (!c) bad.code = 'Type the move code.';
        else if (!codeShape(c)) bad.code = 'A move code is SMMV- and six groups of five letters and digits. Check for a missing group.';
        if (Object.keys(bad).length) throw Object.assign(new Error('Check the marked fields.'), { fields: bad });
        let job;
        try {
          job = await ctx.api.post('setup/move-here', { from: a, code: c });
        } catch (err) {
          if (claimLost(err)) return;
          const r = err && MOVE_REFUSED[err.code];
          if (r) { const words = `${r[1]} ${r[2]}`; throw r[0] ? Object.assign(new Error(words), { fields: { [r[0]]: words } }) : new Error(words); }
          throw err;
        }
        showJob(job);
      },
    });
    card({ path: 'move', at: 2, titleText: 'Move here from another server', back: backToChoices,
      lede: 'This server copies everything from the old one: the CAN logs, the accounts, the cars, their upload tokens and the HTTPS key. The old server’s data is never changed.' },
    h('div', { class: 'su-help' },
      h('p', { class: 'su-help-title' }, icon('info'), h('span', null, 'First, on the old server')),
      h('ol', { class: 'su-list' },
        h('li', null, 'Sign in as an admin and open Settings, Server, Move to another server.'),
        h('li', null, 'Press Make a move code. It works once, for 24 hours, from your home network.'),
        h('li', null, 'Copy the code and the server’s address here.'))),
    f,
    h('details', { class: 'su-more' }, h('summary', null, 'What happens next, and if it stops half way'),
      h('ul', { class: 'su-list' },
        h('li', null, 'The copy runs on the server, not in this browser: you can close the page and come back.'),
        h('li', null, 'If the connection drops or either server restarts, it waits and carries on where it stopped.'),
        h('li', null, 'Give up at any time before the switch: what was copied is removed, and the old server goes on as before.'),
        h('li', null, 'At the end, sign in here with your account from the old server.'))));
    from.control.focus();
  }

  /* ---------------------------------------------------------------- restore from an export file */
  function showRestoreForm({ again = null } = {}) {
    const fileField = field({ label: 'Export file', name: 'file', type: 'file', hint: again
      ? `Choose ${again.file_name} (${bytes(again.size)}) again: the ${bytes(again.received)} already here are not sent twice.`
      : 'The .smexport file the old server made (Settings, Server, Download export file).',
    attrs: { accept: '.smexport,application/octet-stream', required: true } });
    const key = field({ label: 'The old server’s recovery key', name: 'recovery_key', hint: 'SMRK- and 13 groups of 4, saved when the old server was set up.',
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', autocapitalize: 'characters', class: 'input-code' } });
    const f = form({
      fields: [fileField, key], submitLabel: again ? 'Carry on with the upload' : 'Check and upload', busyLabel: 'Checking the file and the key…', submitIcon: 'upload', cls: 'su-form',
      onsubmit: async (v) => {
        const file = fileField.control.files && fileField.control.files[0];
        const k = String(v.recovery_key || '').trim();
        const bad = {};
        if (!file) bad.file = 'Choose the export file.';
        else if (again && (file.name !== again.file_name || file.size !== again.size)) bad.file = `That is another file. To carry on, choose ${again.file_name} (${bytes(again.size)}); or give up and start again with this one.`;
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
    card({ path: 'restore', at: 2, titleText: again ? 'Carry on with the restore' : 'Restore from an export file', back: again ? null : backToChoices,
      lede: again ? `The upload of ${again.file_name} stopped at ${bytes(again.received)} of ${bytes(again.size)}.` : 'The file holds everything the old server had. It is checked with the recovery key before the upload starts, and again before anything is used.',
      nav: again ? giveUpButton({ kind: 'restore', state: 'uploading' }) : null },
    f,
    h('details', { class: 'su-more' }, h('summary', null, 'What happens next, and if it stops half way'),
      h('ul', { class: 'su-list' },
        h('li', null, 'The file goes up in pieces. Keep this page open until the upload is done; the checks after it run on the server.'),
        h('li', null, 'If the upload stops, choose the same file again: it carries on where it stopped.'),
        h('li', null, 'A damaged or cut-short file is refused, and nothing of it is kept.'),
        h('li', null, 'At the end, sign in here with your account from the old server.'))));
  }

  async function base64Of(blob) {
    const buf = new Uint8Array(await blob.arrayBuffer());
    let s = '';
    for (let i = 0; i < buf.length; i += 0x8000) s += String.fromCharCode.apply(null, buf.subarray(i, i + 0x8000));
    return btoa(s);
  }
  const hex = (b) => [...new Uint8Array(b)].map((x) => x.toString(16).padStart(2, '0')).join('');

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

  /* ---------------------------------------------------------------- following a move or a restore */
  let jobBox = null;

  function giveUpButton(job) {
    return btn(job.kind === 'restore' ? 'Give up the restore' : 'Give up the move', { kind: 'soft', icon: 'close', cls: 'btn-end', onclick: async () => {
      const ok = await confirmSheet({
        title: job.kind === 'restore' ? 'Give up the restore?' : 'Give up the move?',
        text: job.kind === 'restore'
          ? 'What was uploaded is removed. The export file itself is not changed: you can restore it again later.'
          : 'What was copied here is removed, and the old server is told. The old server goes on as before, with all its data.',
        yes: 'Give up', danger: true,
      });
      if (!ok) return;
      try { await ctx.api.del('setup/job'); } catch (err) {
        if (claimLost(err)) return;
        if (err && err.code === 'finished') { toast('It is switching over already: it finishes by itself.', { kind: 'info' }); return; }
        if (!(err && err.code === 'no_such_job')) { toast(`Not given up: ${err.message}`, { kind: 'error' }); return; }
      }
      up.file = null; up.key = null; up.error = null;
      route(job.kind === 'restore' ? 'The restore was given up. Nothing of it is kept.' : 'The move was given up. The old server goes on as before.');
    } });
  }

  function showJob(job) {
    const restore = job.kind === 'restore';
    jobBox = h('div', { class: 'su-job', 'aria-live': 'polite' });
    const done = job.state === 'done';
    card({ path: restore ? 'restore' : 'move', at: done ? 4 : 3, titleText: done ? (restore ? 'Restored' : 'Moved here') : restore ? 'Restoring from the file' : 'Moving here' }, jobBox);
    drawJob(job);
  }

  function drawJob(job) {
    if (!jobBox) return;
    const restore = job.kind === 'restore';
    const st = job.state;
    const p = job.progress || null;
    const prob = job.problem;
    if (st === 'done') { stopPoll(); drawDone(job); return; }
    const parts = [];
    parts.push(h('p', { class: 'su-from' }, restore
      ? ['From the file ', h('strong', { class: 'break' }, job.file_name || 'the export file'), job.size ? ` (${bytes(job.size)})` : '']
      : ['From ', h('strong', { class: 'mono break' }, job.from || 'the old server'), job.started_ms ? `, started ${when(job.started_ms)}` : '']));
    const steps = restore ? RESTORE_STEPS : MOVE_STEPS;
    let current = st;
    if (st === 'waiting' || st === 'stopped') current = ROUND_STEP[Math.min(3, (p && p.round) || 0)];
    if (st === 'failed') current = 'uploading';
    const uploadStalled = restore && st === 'uploading' && !up.active;

    // the output of the current step: its progress and what to do
    const out = [];
    if (p && st !== 'connecting' && st !== 'failed') out.push(progressBar(restore && st === 'uploading' ? { ...p, bytes_done: job.received, bytes_total: job.size } : p, { label: restore ? 'Uploaded' : 'Copied' }));
    if (st === 'waiting') {
      const w = PROBLEM[prob && prob.code] || [prob ? prob.message : 'It waits.', 'It tries again by itself.'];
      out.push(callout('warn', w[0], h('p', null, w[1]),
        prob && prob.message ? h('p', { class: 'mono-line' }, 'The server says: ', h('q', null, prob.message)) : null,
        prob && prob.retry_ms ? h('p', { class: 'hint' }, `Next try in about ${secs(prob.retry_ms)} seconds.`) : null));
    } else if (st === 'stopped') {
      const resumable = !restore && RESUMABLE.has(prob && prob.code);
      const w = PROBLEM[prob && prob.code] || [prob ? prob.message : 'The move stopped.', 'It waits for you.'];
      out.push(callout('bad', w[0], resumable ? null : h('p', null, w[1]),
        prob && prob.message && prob.code !== 'cancelled' ? h('p', { class: 'mono-line' }, 'The server says: ', h('q', null, prob.message)) : null,
        h('p', null, resumable ? 'What was copied so far stays here. Make a new move code on the old server and type it below: the move carries on from where it stopped.'
          : 'To move after all: make a new move code on the old server, give up here, then choose Move here again with the new code.')));
      if (resumable) out.push(carryOn(job));
    } else if (st === 'failed') {
      const w = PROBLEM[prob && prob.code] || [prob ? prob.message : 'The restore failed.', 'Nothing of it was kept.'];
      out.push(callout('bad', w[0], h('p', null, w[1]), prob && prob.message ? h('p', { class: 'mono-line' }, 'The server says: ', h('q', null, prob.message)) : null));
    } else if (restore && st === 'uploading') {
      if (up.active) out.push(h('p', { class: 'hint' }, 'Keep this page open until the upload is done.'));
      else if (up.error) out.push(callout('warn', 'The upload stopped.', h('p', null, up.error)));
      else out.push(callout('info', 'The upload stopped part way.', h('p', null, `The server has ${bytes(job.received)} of ${bytes(job.size)}. Choose the same file again to carry on from there.`)));
    } else if (st === 'switching') {
      out.push(h('p', { class: 'hint' }, 'It finishes by itself in a few seconds. The page may lose the server for a moment: it keeps asking.'));
    } else if (!restore) {
      out.push(h('p', { class: 'hint' }, 'The copy runs on the server: you can close this page and come back. The old server keeps taking uploads until the switch.'));
    }
    if (job._net) out.push(callout('info', 'The server does not answer this page right now.', h('p', null, 'It may be restarting. The page keeps asking every few seconds.')));
    // the one clear button of the current step
    if (restore && st === 'uploading' && !up.active) {
      out.push(h('div', { class: 'form-actions' }, up.file && up.key
        ? btn('Carry on with the upload', { kind: 'primary', icon: 'upload', onclick: () => upload(job) })
        : btn('Choose the file again', { kind: 'primary', icon: 'file', onclick: () => showRestoreForm({ again: job }) })));
    }
    if (st === 'failed') {
      dismissed = job.id;
      out.push(h('div', { class: 'form-actions' }, btn('Choose a file again', { kind: 'primary', icon: 'file', onclick: () => { up.file = null; showRestoreForm(); } })));
    }
    parts.push(stepCards(steps, current, {
      problem: st === 'waiting' || st === 'stopped' || uploadStalled,
      failed: st === 'failed' ? 'The restore stopped here. Nothing of the file was kept.' : null,
      output: out,
    }));
    if (!restore && (st === 'waiting' || st === 'stopped') && p && p.bytes_done) parts.push(finishWithKey(job));
    const nav = h('div', { class: 'su-nav' },
      st === 'failed' ? btn('Back', { kind: 'soft', icon: 'chevronLeft', onclick: backToChoices }) : h('span'),
      !['switching', 'done', 'failed'].includes(st) ? giveUpButton(job) : null);
    parts.push(nav);
    // keep a form the person is typing in across a redraw
    const carry = jobBox.querySelector('.su-carry');
    if (carry) { const fresh = out.find((x) => x.classList && x.classList.contains('su-carry')); if (fresh) fresh.replaceWith(carry); }
    const open = jobBox.querySelector('.su-finish');
    if (open && open.open) { const k = parts.findIndex((x) => x.classList && x.classList.contains('su-finish')); if (k >= 0) parts[k] = open; }
    replace(jobBox, parts);
    const serverWorks = !(restore && st === 'uploading') && st !== 'failed' && st !== 'done';
    if (serverWorks || (restore && st === 'uploading' && !up.active)) schedule(job, serverWorks ? 1000 : 5000);
  }

  function schedule(job, ms) {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      if (!ctx.alive() || up.active) return;
      try {
        drawJob(await ctx.api.get('setup/job'));
      } catch (err) {
        if (api.isAbort(err) || claimLost(err)) return;
        if (err && err.code === 'no_such_job') { route('Nothing is under way any more: it was given up, perhaps from another device.'); return; }
        drawJob({ ...job, _net: true });
        clearTimeout(timer);
        timer = setTimeout(() => schedule(job, 0), 3000);
      }
    }, ms);
  }

  function carryOn(job) {
    const code = field({ label: 'New move code', name: 'code', hint: `Make it on ${job.from || 'the old server'}: Settings, Server, Move to another server, Make a move code.`,
      attrs: { autocomplete: 'off', required: true, spellcheck: 'false', autocapitalize: 'characters', class: 'input-code' } });
    const f = form({
      fields: [code], submitLabel: 'Carry on', busyLabel: 'Reaching the old server…', submitIcon: 'arrowRight',
      onsubmit: async (v) => {
        const c = String(v.code || '').trim();
        if (!c) throw Object.assign(new Error('Type the new move code.'), { fields: { code: 'Type the move code.' } });
        if (!codeShape(c)) throw Object.assign(new Error('Check the code.'), { fields: { code: 'A move code is SMMV- and six groups of five letters and digits. Check for a missing group.' } });
        let next;
        try {
          next = await ctx.api.post('setup/move-here', { from: job.from, code: c });
        } catch (err) {
          if (claimLost(err)) return;
          if (err && err.code === 'setup_busy') { toast('The move is not stopped any more (perhaps another device carried it on).'); drawJob(await ctx.api.get('setup/job')); return; }
          const r = err && MOVE_REFUSED[err.code];
          if (r) throw Object.assign(new Error(`${r[1]} ${r[2]}`), { fields: r[0] === 'code' ? { code: `${r[1]} ${r[2]}` } : {} });
          throw err;
        }
        toast('Carrying on with the move: what was copied before is kept.');
        jobBox.querySelector('.su-carry')?.remove();
        drawJob(next);
      },
    });
    return h('div', { class: 'su-help su-carry' },
      h('p', { class: 'su-help-title' }, icon('key'), h('span', null, 'Carry on with a new move code')),
      h('p', { class: 'hint' }, `What was copied here (${bytes(job.progress && job.progress.bytes_done)}) is kept: nothing is copied twice.`),
      f);
  }

  function finishWithKey(job) {
    const key = field({ label: 'The old server’s recovery key', name: 'recovery_key', hint: 'SMRK- and 13 groups of 4.', attrs: { autocomplete: 'off', required: true, spellcheck: 'false', class: 'input-code' } });
    const sure = h('input', { type: 'checkbox', name: 'confirm', id: 'su-sure', class: 'tick-input' });
    const sureField = h('div', { class: 'field', dataset: { field: 'confirm' } },
      h('label', { class: 'tick', for: 'su-sure' }, sure, h('span', { class: 'tick-box', 'aria-hidden': 'true' }, icon('check')), h('span', null, 'The old server is gone for good. I will never start it again with the CAN Bridge pointed at it.')),
      h('p', { class: 'field-error', hidden: true }));
    const f = form({
      fields: [key, sureField], submitLabel: 'Finish with what was copied', busyLabel: 'Checking the key…', submitIcon: 'key',
      onsubmit: async (v) => {
        if (!sure.checked) throw Object.assign(new Error('Tick the box first.'), { fields: { confirm: 'Tick this only if the old server is really gone.' } });
        try {
          drawJob(await ctx.api.post('setup/move-here/finish', { recovery_key: String(v.recovery_key || '').trim(), confirm: true }));
        } catch (err) {
          if (claimLost(err)) return;
          if (err && err.code === 'wrong_recovery_key') throw Object.assign(new Error(err.message), { fields: { recovery_key: 'This key does not open the old server’s data. Use the one it showed when it was set up.' } });
          if (err && err.code === 'not_ready') throw new Error('Not now: this works only while the copy waits or has stopped part way. Reload the page to see where it stands.');
          if (err && err.status === 429) throw new Error(`Too many wrong keys. Wait ${secs(err.retryAfterMs)} seconds, then try again.`);
          throw err;
        }
      },
    });
    return h('details', { class: 'su-more su-finish' },
      h('summary', null, 'The old server is gone for good?'),
      h('p', { class: 'hint' }, `If its disk died, this server can open what was copied (${bytes(job.progress && job.progress.bytes_done)}) with the old server’s recovery key. The last changes may be missing. Use this only as a last resort.`),
      f);
  }

  function drawDone(job) {
    const r = job.result || {};
    const restore = job.kind === 'restore';
    const host = location.hostname.includes(':') ? `[${location.hostname}]` : location.hostname;
    const newAddress = `https://${host}:10100/api/can/logs`;
    replace(jobBox,
      callout('ok', restore ? 'Restored. Sentry Mate is set up.' : 'Moved. Sentry Mate is set up.',
        h('p', null, 'Everything from ', h('strong', { class: 'break' }, restore ? (r.file_name || job.file_name || 'the export file') : (r.from || job.from || 'the old server')), ' is here: ',
          count(r.accounts || 0, 'account'), ', ', count(r.cars || 0, 'car'), r.at_ms ? `, as it was at ${when(r.at_ms)}` : '', '.'),
        r.forced ? h('p', null, chip('Finished with the recovery key', { tone: 'sun' }), ' The last changes on the old server may be missing.') : null),
      h('div', { class: 'su-help' },
        h('p', { class: 'su-help-title' }, icon('person'), h('span', null, 'Next: sign in')),
        h('p', null, 'Use your name and password from the old server: its accounts came along. Nobody is signed in yet.')),
      h('div', { class: 'su-help' },
        h('p', { class: 'su-help-title' }, icon('plug'), h('span', null, 'The CAN Bridge')),
        r.same_machine
          ? h('p', null, 'The old server ran on this machine. Stop the old app, then give this app port 10100 back. The CAN Bridge then needs no change at all.')
          : [h('p', null, 'Each CAN Bridge keeps its token and its certificate SHA-256. If this server has a new address, change only the server address in the app:'),
            h('p', { class: 'su-addr' }, mono(newAddress), copyButton(() => newAddress)),
            h('p', { class: 'hint' }, 'Nothing is lost meanwhile: the bridge keeps its logs and sends them once it reaches this server.')],
        r.cert_sha256 ? h('p', { class: 'hint break' }, 'The certificate SHA-256 is unchanged: ', mono(r.cert_sha256)) : null),
      !restore ? h('p', { class: 'hint' }, 'The old server now says its data has moved, and takes no more uploads. Keep its folders for a while as a backup.') : null,
      h('div', { class: 'su-nav' }, h('span'), linkBtn('Sign in', '/sign-in', { kind: 'primary', icon: 'arrowRight' })));
  }

  await route();
  ctx.ready();
  return {};
}
