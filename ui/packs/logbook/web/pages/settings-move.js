// Sentry Mate, "Logbook" UI pack: Settings: Server and access, "Move to another server" (admins only).
//
// Moving needs no shell. Two ways, side by side, each saying when to use it:
//   - A move code (GET, POST, DELETE /system/move): the code is shown once, big, with Copy and when it
//     stops working; then the new server's copy is followed live (once a second), and afterwards this
//     server says where its data went, with "Undo the move" (POST /system/move/undo).
//   - An export file (/system/exports): made in a few seconds, then downloaded as one file that opens with
//     the recovery key on the new server's first page.
// Making a code, undoing a move and making an export ask for the password again (and the two-step code
// when the account has it). The rules are in docs/ui-api.md ("Moving and first-run setup").

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { section, button, flag, ago } from '../core/ui.js';
import { field, form } from '../core/form.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { dateTime, count } from '../core/format.js';
import { bytes, bigCode, copyButton, downloadUrl, stepList, progressBar, callout, PROBLEM } from './move-parts.js';

const OLD_STEPS = [
  ['copying', 'The new server copies everything', 'This server keeps taking uploads meanwhile, so nothing is missed.'],
  ['catching_up', 'It catches up', 'Copying what changed here while the first round ran.'],
  ['paused', 'Last round', 'Uploads wait here for a few seconds while the last changes go over. The CAN Bridge keeps them and sends them again.'],
  ['moved', 'The new server switches over', 'Keep this server on until it says it took over.'],
  ['done', 'Done', null],
];
const WHY_CANCELLED = (w) => (w ? w.replace(/^./, (c) => c.toUpperCase()) : 'Cancelled');

/**
 * Ask for the password again, then run(creds, values) inside the dialog: a wrong password stays in the
 * dialog under its field. extra: more fields (elements) shown above the password. Resolves with run's
 * result, or null when closed.
 */
function askPassword(ctx, { title, text, yes, danger = false, extra = [], run }) {
  let twoStepShown = !!(ctx.me && ctx.me.two_step_required);
  const pass = field({ label: 'Your password', name: 'password', type: 'password', hint: 'The one you sign in with. It is asked again because this hands over all the data.', attrs: { autocomplete: 'current-password', required: true } });
  const two = field({ label: 'Two-step code', name: 'two_step', hint: 'The 6 digits from your authenticator app.', attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', maxlength: 6, spellcheck: 'false' } });
  two.hidden = !twoStepShown;
  let result = null;
  return openDialog({
    title, actions: [], initialFocus: 'input[name=password]',
    body: (close) => [
      text ? (typeof text === 'string' ? h('p', { class: 'dialog-text' }, text) : text) : null,
      form({
        fields: [...extra, pass, two], submitLabel: yes, busyLabel: 'Working…', cls: danger ? 'mv-form-danger' : null,
        onsubmit: async (v) => {
          if (!v.password) throw Object.assign(new Error('Type your password.'), { fields: { password: 'Type your own password.' } });
          const creds = { password: v.password };
          if (twoStepShown && v.two_step) creds.two_step = String(v.two_step).trim();
          try {
            result = await run(creds, v);
          } catch (err) {
            if (err && err.code === 'wrong_two_step' && !twoStepShown) {
              twoStepShown = true; two.hidden = false;
              throw Object.assign(new Error('Your account asks for a two-step code as well.'), { fields: { two_step: 'Type the 6-digit code from your authenticator app.' } });
            }
            if (err && err.code === 'wrong_password') throw Object.assign(new Error(err.message), { fields: { password: 'That is not your password. Type it again.' } });
            if (err && err.code === 'wrong_two_step') throw Object.assign(new Error(err.message), { fields: { two_step: 'Not the right code. Type the one your app shows now.' } });
            if (err && err.status === 429) throw new Error(`Too many wrong tries. Wait ${Math.ceil((err.retryAfterMs || 60000) / 1000)} seconds, then try again.`);
            throw err;
          }
          if (result === false) return; // run kept the dialog open on purpose
          close('done');
        },
      }),
    ],
  }).then(() => result);
}

/**
 * The block: { el, load() }. network is GET /network (for this server's address), or null.
 */
export function moveBlock(ctx, { network = null } = {}) {
  const tz = ctx.tz;
  const myAddress = (() => {
    try { const u = new URL(network && network.upload_address); return u.port && u.port !== '10100' ? u.host : u.hostname; } catch { return location.hostname; }
  })();
  let move = null;      // GET /system/move
  let shown = null;     // { id, text }: the code made on this page, shown until it is used or cancelled
  let exp = null;       // the ExportFile, or null
  let fetchOnReady = null; // the export made on this page: downloaded once it is ready
  let moveTimer = 0;
  let expTimer = 0;
  ctx.onCleanup(() => { clearTimeout(moveTimer); clearTimeout(expTimer); });

  const statusBox = h('div', { class: 'mv-status', 'aria-live': 'polite' });
  const codeBox = h('div', { class: 'mv-way-body' });
  const exportBox = h('div', { class: 'mv-way-body', 'aria-live': 'polite' });

  /* ---------------------------------------------------------------- the move code */
  async function refreshMove() {
    clearTimeout(moveTimer);
    try { move = await ctx.api.get('system/move'); } catch (err) {
      if (api.isAbort(err)) return;
      replace(statusBox, callout('warn', 'The state of the move could not be read.', h('p', null, `${err.message || 'No answer.'} The page asks again in a few seconds.`)));
      moveTimer = setTimeout(refreshMove, 5000);
      return;
    }
    drawMove();
  }
  function schedule() {
    clearTimeout(moveTimer);
    const s = move && move.state;
    const ms = s === 'copying' || s === 'paused' || s === 'moved' ? 1000 : s === 'offered' ? 3000 : 0;
    if (ms) moveTimer = setTimeout(refreshMove, ms);
  }

  async function makeCode({ replaceIt = false } = {}) {
    let anywhere = false;
    const where = h('div', { class: 'field', dataset: { field: 'anywhere' } },
      h('label', { class: 'mv-tick' }, h('input', { type: 'checkbox', name: 'anywhere', onchange: (e) => { anywhere = e.target.checked; } }),
        h('span', null, h('strong', null, 'From anywhere. '), 'Leave it off when both servers are on your home network. Tick it only for a new server elsewhere (another house, a rented server).')),
      h('p', { class: 'field-error', hidden: true }));
    const out = await askPassword(ctx, {
      title: 'Make a move code',
      text: h('div', { class: 'dialog-text' },
        h('p', null, 'The code lets one new server copy everything here: the logs, the accounts, the cars, the upload tokens and the HTTPS key.'),
        h('p', null, 'It works once, for 24 hours. A code still waiting stops working.')),
      yes: 'Make the code', extra: [where],
      run: async (creds) => {
        try {
          return await ctx.api.post('system/move', { ...creds, anywhere, replace: replaceIt || undefined });
        } catch (err) {
          if (err && err.code === 'move_under_way') {
            const ok = await confirmDialog({ title: 'A move is under way. End it?', text: `${(move && move.peer && move.peer.address) || 'A new server'} is copying this server now. A new code ends that copy there (its copy so far is removed). This server’s data is not changed.`, yes: 'End it and make a new code', danger: true });
            if (!ok) return false;
            return ctx.api.post('system/move', { ...creds, anywhere, replace: true });
          }
          if (err && err.code === 'moved') throw new Error('This server has moved already. Undo the move first if you want to move it again.');
          throw err;
        }
      },
    });
    if (!out) return;
    if (out.move_code && out.code) shown = { id: out.code.id, text: out.move_code };
    move = out;
    ctx.flash('Move code made. It is shown below this once.');
    drawMove();
    statusBox.closest('.block')?.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }

  async function cancelMove() {
    const copying = move && (move.state === 'copying' || move.state === 'paused');
    const ok = await confirmDialog({
      title: copying ? 'Cancel the move?' : 'Cancel the move code?',
      text: copying
        ? `${(move.peer && move.peer.address) || 'The new server'} stops copying, and its copy so far is removed. This server goes on as before: its data is never changed by a move.`
        : 'The code stops working at once. This server goes on as before. Make a new code whenever you are ready.',
      yes: copying ? 'Cancel the move' : 'Cancel the code', danger: true,
    });
    if (!ok) return;
    try {
      move = await ctx.api.del('system/move');
      shown = null;
      ctx.flash(move.cancelled === 'ended' ? 'The move was cancelled. This server goes on as before.' : move.cancelled === 'revoked' ? 'The move code was cancelled. It no longer works.' : 'There was nothing to cancel.');
    } catch (err) {
      if (err && err.code === 'finished') ctx.flash('Too late to cancel: the move has finished. Undo the move instead.', { kind: 'error' });
      else ctx.flash(`Not cancelled: ${err.message}`, { kind: 'error' });
      await refreshMove();
      return;
    }
    drawMove();
  }

  async function undoMove() {
    const sure = h('input', { type: 'checkbox', name: 'stopped' });
    const sureField = h('div', { class: 'field', dataset: { field: 'stopped' } },
      h('label', { class: 'mv-tick' }, sure, h('span', null, 'I have stopped the new server', move && move.moved && move.moved.to ? [' at ', h('strong', null, move.moved.to)] : null, '.')),
      h('p', { class: 'field-error', hidden: true }));
    const out = await askPassword(ctx, {
      title: 'Undo the move?',
      text: h('div', { class: 'dialog-text' },
        h('p', null, 'This server takes uploads and changes again at once, with the data it had when it moved.'),
        h('p', null, h('strong', null, 'Stop the new server first. '), 'If both take uploads, they drift apart, and the new one’s uploads since the move are not here.')),
      yes: 'Undo the move', danger: true, extra: [sureField],
      run: async (creds) => {
        if (!sure.checked) { throw Object.assign(new Error('Tick the box first.'), { fields: { stopped: 'Stop the new server first, then tick this.' } }); }
        try { return await ctx.api.post('system/move/undo', { ...creds, confirm: true }); } catch (err) {
          if (err && err.code === 'not_moved') { await refreshMove(); return null; }
          throw err;
        }
      },
    });
    if (!out) return;
    move = out;
    shown = null;
    ctx.flash('The move was undone. This server takes uploads again.');
    drawMove();
  }

  function drawMove() {
    const m = move || { state: 'none' };
    const s = m.state;
    const status = [];
    const body = [];
    if (shown && (s !== 'offered' || !m.code || m.code.id !== shown.id)) shown = null;
    const peer = m.peer;

    // The state of the move, at the top of the block
    if (s === 'copying' || s === 'paused' || s === 'moved') {
      const cur = s === 'copying' ? ((m.progress && m.progress.round) >= 2 ? 'catching_up' : 'copying') : s;
      status.push(callout('info', s === 'moved' ? `The data moved to ${(m.moved && m.moved.to) || (peer && peer.address)}. It is switching over.` : `${(peer && peer.address) || 'A new server'} is copying this server.`,
        h('p', null, s === 'moved' ? 'Keep this server on until the new one says it took over: a few seconds. This server already takes no uploads and no changes.'
          : 'Keep this server on. It goes on taking uploads while the new server copies, and the new server takes over by itself at the end.'),
        peer ? h('p', { class: 'mv-quiet' }, [peer.app ? `Sentry Mate ${peer.app}` : null, peer.claimed_ms ? `started ${dateTime(peer.claimed_ms, tz)}` : null].filter(Boolean).join(', '), peer.last_seen_ms ? ['; last heard ', ago(peer.last_seen_ms)] : null, '.') : null),
      stepList(OLD_STEPS, cur),
      m.progress ? progressBar(m.progress, { label: 'Copied to the new server' }) : null,
      m.problem ? problemView(m.problem) : null,
      s !== 'moved' ? h('div', { class: 'mv-actions' }, button('Cancel the move', { kind: 'danger', onclick: cancelMove })) : null);
    } else if (s === 'done') {
      const to = (m.moved && m.moved.to) || (peer && peer.address) || 'the new server';
      status.push(callout('ok', `This server has moved to ${to}.`,
        h('p', null, 'The new server took over', m.moved && (m.moved.switched_ms || m.moved.at_ms) ? [' ', dateTime(m.moved.switched_ms || m.moved.at_ms, tz)] : null, '. This server takes no more uploads and no changes; reading still works. ', h('strong', null, 'You can turn it off.')),
        h('p', null, 'Each CAN Bridge keeps its token and certificate SHA-256. If the new server has another address, change only the server address in the app, for example ', h('code', { class: 'mono mv-mono' }, `https://${to.replace(/:\d+$/, '')}:10100/api/can/logs`), '.'),
        h('p', { class: 'mv-quiet' }, 'Keep this server’s folders for a while as a backup, then delete them.')),
      h('div', { class: 'mv-actions' }, button('Undo the move', { onclick: undoMove }), h('span', { class: 'mv-quiet' }, 'Brings the data back here. Stop the new server first.')));
    }
    if (m.skipped && m.skipped.length && s !== 'none') status.push(h('details', { class: 'mv-skipped' }, h('summary', null, `${count(m.skipped.length, 'file')} on this server ${m.skipped.length === 1 ? 'was' : 'were'} left out`),
      h('p', { class: 'mv-quiet' }, 'This server cannot read them, so they do not go along. Give them the server’s user as owner if you need them: the next round copies them.'),
      h('ul', { class: 'mv-list' }, m.skipped.map((x) => h('li', null, h('code', { class: 'mono mv-mono' }, x.path), ` (${x.why})`)))));
    replace(statusBox, status);

    // The move code panel
    if (s === 'offered') {
      const until = m.code ? dateTime(m.code.expires_ms, tz) : null;
      const reach = m.code && m.code.anywhere ? 'from anywhere' : 'from your home network only';
      if (shown) {
        body.push(bigCode(shown.text, { label: 'Move code' }),
          h('p', { class: 'mv-text' }, h('strong', null, 'Shown only now: copy it. '), `It works once, until ${until}, ${reach}.`));
      } else {
        body.push(callout('info', 'A move code is waiting.',
          h('p', null, `Made ${m.code ? dateTime(m.code.made_ms, tz) : ''}; it works until ${until}, ${reach}.`),
          h('p', null, 'The code itself is shown only when it is made. If you no longer have it, make a new one: this one then stops working.')));
      }
      if (m.code && m.code.wrong_tries) body.push(callout('warn', `Someone typed a wrong code ${count(m.code.wrong_tries, 'time')}.`, h('p', null, 'If that was not you, cancel the code now and make a new one.')));
      body.push(h('h4', { class: 'mv-h4' }, 'Then, on the new server'),
        h('ol', { class: 'mv-list mv-list-num' },
          h('li', null, 'Install Sentry Mate there and open its address. Type the set-up code from its log.'),
          h('li', null, 'Choose ', h('strong', null, 'Move here from another server'), '.'),
          h('li', null, 'Type this server’s address, ', h('code', { class: 'mono mv-mono' }, myAddress), ' ', copyButton(() => myAddress, { cls: 'button-small' }), ', and the code.'),
          h('li', null, 'Keep both servers on. This page shows the copy as it goes.')),
        h('div', { class: 'mv-actions' }, button('Cancel the code', { kind: 'danger', onclick: cancelMove }), button('Make a new code', { onclick: () => makeCode() })));
    } else if (s === 'copying' || s === 'paused' || s === 'moved') {
      body.push(h('p', { class: 'mv-text' }, 'The code was used: the move is shown above.'));
    } else if (s === 'done') {
      body.push(h('p', { class: 'mv-text' }, 'This server has moved. To move it again, undo the move first.'));
    } else {
      if (s === 'expired') body.push(callout('info', 'The last move code expired unused.', h('p', null, `It was made ${m.code ? dateTime(m.code.made_ms, tz) : ''}. Make a new one when the new server is ready.`)));
      body.push(h('ol', { class: 'mv-list mv-list-num' },
        h('li', null, 'Press ', h('strong', null, 'Make a move code'), ' and type your password again.'),
        h('li', null, 'On the new server’s first page, choose ', h('strong', null, 'Move here from another server'), ' and type this server’s address (', h('code', { class: 'mono mv-mono' }, myAddress), ') and the code.'),
        h('li', null, 'Watch it here. At the end this server says it has moved, and you can turn it off.')),
      h('div', { class: 'mv-actions' }, button('Make a move code', { kind: 'primary', onclick: () => makeCode() })));
    }
    if (m.last_cancelled && s !== 'copying' && s !== 'paused') body.push(h('p', { class: 'mv-quiet' }, `${WHY_CANCELLED(m.last_cancelled.why)}: `, dateTime(m.last_cancelled.at_ms, tz), '.'));
    if (m.last_undone_ms && s === 'none') body.push(h('p', { class: 'mv-quiet' }, 'The last move was undone ', dateTime(m.last_undone_ms, tz), '.'));
    replace(codeBox, body);
    schedule();
  }

  function problemView(p) {
    const w = PROBLEM[p.code] || [p.message || 'The move waits.', ''];
    return callout('warn', w[0], w[1] ? h('p', null, w[1]) : null, p.message ? h('p', { class: 'mv-quiet' }, 'The server says: ', h('q', null, p.message)) : null);
  }

  /* ---------------------------------------------------------------- the export file */
  async function refreshExport() {
    clearTimeout(expTimer);
    try {
      if (exp && exp.id) exp = await ctx.api.get(api.apiPath`system/exports/${exp.id}`);
      else { const r = await ctx.api.get('system/exports'); exp = (r.items || [])[0] || null; }
    } catch (err) {
      if (api.isAbort(err)) return;
      if (err && (err.code === 'no_such_export' || err.status === 404)) exp = null;
      else { replace(exportBox, callout('warn', 'The export file could not be read.', h('p', null, err.message || 'No answer.'))); return; }
    }
    drawExport();
  }

  async function makeExport() {
    const out = await askPassword(ctx, {
      title: 'Make an export file',
      text: h('div', { class: 'dialog-text' },
        h('p', null, 'One file with everything on this server. It takes a few seconds; uploads wait meanwhile (the CAN Bridge keeps them and sends them again).'),
        h('p', null, 'It opens only with this server’s recovery key, and never holds the key file. A file made before is replaced.')),
      yes: 'Make the file',
      run: async (creds) => {
        try { return await ctx.api.post('system/exports', creds); } catch (err) {
          if (err && err.code === 'export_preparing') { const r = await ctx.api.get('system/exports'); return (r.items || [])[0] || null; }
          throw err;
        }
      },
    });
    if (!out) return;
    exp = out;
    fetchOnReady = out.id;
    drawExport();
  }

  async function deleteExport() {
    const ok = await confirmDialog({ title: 'Delete the export file?', text: 'The file is removed from this server. A copy you already downloaded is not touched.', yes: 'Delete the file', danger: true });
    if (!ok) return;
    try { await ctx.api.del(api.apiPath`system/exports/${exp.id}`); ctx.flash('Export file deleted.'); }
    catch (err) { if (!(err && err.status === 404)) { ctx.flash(`Not deleted: ${err.message}`, { kind: 'error' }); return; } }
    exp = null;
    drawExport();
  }

  function drawExport() {
    clearTimeout(expTimer);
    const e = exp;
    const out = [];
    if (!e || e.state === 'expired') {
      if (e) out.push(callout('info', 'The last export file has expired.', h('p', null, `It was made ${dateTime(e.made_ms, tz)}. Make a new one.`)));
      out.push(h('ol', { class: 'mv-list mv-list-num' },
        h('li', null, 'Press ', h('strong', null, 'Download export file'), ' and type your password again. The file is ready in a few seconds.'),
        h('li', null, 'Take the file to the new server (or keep it as a backup).'),
        h('li', null, 'On the new server’s first page, choose ', h('strong', null, 'Restore from an export file'), ', choose the file and type this server’s recovery key.')),
      h('div', { class: 'mv-actions' }, button('Download export file', { kind: 'primary', iconName: 'download', onclick: makeExport })));
    } else if (e.state === 'preparing') {
      out.push(callout('info', 'Making the export file…', h('p', null, 'A few seconds. Uploads wait meanwhile; the download starts by itself when it is ready.')),
        h('div', { class: 'mv-bar mv-bar-busy', role: 'progressbar', 'aria-label': 'Making the export file' }, h('span', { class: 'mv-bar-fill' })));
      expTimer = setTimeout(refreshExport, 1000);
    } else {
      if (fetchOnReady === e.id && e.url) { fetchOnReady = null; downloadUrl(e.url); ctx.flash(`The download of ${e.file_name} has started.`); }
      out.push(h('div', { class: 'mv-file' },
        h('p', { class: 'mv-file-name' }, e.file_name),
        h('p', { class: 'mv-file-facts' }, [bytes(e.size), e.files != null ? count(e.files, 'file') : null, `made ${dateTime(e.made_ms, tz)}`].filter(Boolean).join(' · ')),
        h('p', { class: 'mv-quiet' }, 'Kept on this server until ', dateTime(e.expires_ms, tz), ' (a day after the last download). A download that stops carries on.')),
      h('div', { class: 'mv-actions' },
        h('a', { class: 'button', href: e.url, download: e.file_name }, 'Download', ''),
        button('Make a new one', { onclick: makeExport }),
        button('Delete it', { kind: 'danger', onclick: deleteExport })),
      e.skipped && e.skipped.length ? h('p', { class: 'mv-quiet' }, `Left out (this server cannot read ${e.skipped.length === 1 ? 'it' : 'them'}): `, e.skipped.map((x, i) => [i ? ', ' : null, h('code', { class: 'mono mv-mono' }, x.path)])) : null);
    }
    out.push(h('p', { class: 'mv-quiet' }, 'Without the recovery key the file is useless to anyone who takes it. Delete it once the new server runs.'));
    replace(exportBox, out);
  }

  /* ---------------------------------------------------------------- put together */
  const el = section({ title: 'Move to another server', note: 'no shell, no commands', id: 'move', cls: 'mv-block' },
    h('p', { class: 'mv-lede' }, 'Everything goes along: the CAN logs, uploads still arriving, the accounts, the cars, their upload tokens and the HTTPS key. The CAN Bridge keeps its token and certificate SHA-256; at most its server address changes. This server’s data is never changed by a move, so stopping half way is always safe.'),
    statusBox,
    h('div', { class: 'mv-ways' },
      h('section', { class: 'mv-way', 'aria-labelledby': 'mv-way-code' },
        h('h3', { class: 'mv-way-title', id: 'mv-way-code' }, 'With a move code', flag('Usual')),
        h('p', { class: 'mv-way-when' }, h('strong', null, 'When: '), 'both servers are on at the same time, and the new one can reach this one (the same home network). This server keeps taking uploads while the new one copies, and the new one takes over by itself.'),
        codeBox),
      h('section', { class: 'mv-way', 'aria-labelledby': 'mv-way-file' },
        h('h3', { class: 'mv-way-title', id: 'mv-way-file' }, 'With an export file'),
        h('p', { class: 'mv-way-when' }, h('strong', null, 'When: '), 'the servers cannot reach each other, this one will be off, or you want a copy to keep as a backup. One file with everything, which opens only with the recovery key.'),
        exportBox)),
    h('details', { class: 'mv-more' },
      h('summary', null, 'If it stops half way'),
      h('ul', { class: 'mv-list' },
        h('li', null, 'A dropped connection, or a restart of either server: the new server waits and carries on where it stopped. Nothing to do.'),
        h('li', null, 'Cancel the code or the move here at any time before the switch: the new server’s copy is removed, and this server goes on as before.'),
        h('li', null, 'If this server dies part way, the new server can finish with this server’s recovery key (the last changes may be missing).'),
        h('li', null, 'After the switch, this server takes no more uploads. Undo the move here to take them back (stop the new server first).'))));

  return {
    el,
    async load() {
      await Promise.all([refreshMove(), refreshExport()]);
    },
  };
}
