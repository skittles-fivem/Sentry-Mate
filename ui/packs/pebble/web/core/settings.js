// Sentry Mate, "Pebble" UI pack: parts the settings pages share.
//
// Grouped rounded list rows (iOS-like): group() holds rows; infoRow() is a row that only shows; pickRow() is a
// row with the current choice on its right that opens a sheet of choices (a picker never drops down in place);
// askPassword() asks for the person's own password again (and the two-step code when the account has it) in a
// sheet before a dangerous action, keeping a wrong password under its field; keySheet() shows a recovery key with
// Copy, Download and the tick "I have saved it". Styles: app.css (.set-*).

import { h } from '../shared/dom.js';
import { createSheet, openSheet, sheetRow } from './sheet.js';
import { field, form, setFieldError } from './form.js';
import { icon, btn } from './ui.js';
import { bigCode, downloadText } from '../pages/move-parts.js';

/** A group of rows with a small heading: group('Units', [rows], { cover, note }). */
export function group(name, rows, { cover = null, note = null, id = null } = {}) {
  return h('section', { class: 'rows set-group', cover, id },
    name ? h('h3', { class: 'rows-name' }, name) : null,
    h('div', { class: 'rows-list' }, rows.flat().filter(Boolean)),
    note ? h('p', { class: 'set-note' }, note) : null);
}

/** A row that only shows: infoRow({ icon, name, sub, side, tone, cover }). */
export function infoRow({ icon: ic = null, name, sub = null, side = null, tone = null, cover = null, cls = null } = {}) {
  return h('div', { class: ['row', 'row-static', cls], cover },
    ic ? h('span', { class: ['row-ic', tone && `tone-${tone}`], 'aria-hidden': 'true' }, icon(ic)) : null,
    h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, name), sub ? h('span', { class: 'row-sub' }, sub) : null),
    side != null ? h('span', { class: 'row-side' }, side) : null);
}

/**
 * A picker row: the name on the left, the choice on the right, a sheet of choices when pressed.
 * pickRow({ name, sub, value, options: [[value, text, sub?]], onpick(v) -> Promise, cover, disabled }). el.set(v).
 */
export function pickRow({ name, sub = null, value, options, onpick, cover = null, disabled = false, sheetTitle = null, sheetNote = null } = {}) {
  let cur = value;
  const textOf = (v) => (options.find(([o]) => String(o) === String(v)) || [null, String(v ?? '—')])[1];
  const shown = h('span', { class: 'set-value' }, textOf(cur));
  const el = h('button', {
    type: 'button', class: 'row set-pick', cover, disabled, 'aria-haspopup': 'dialog',
    onclick: (e) => open(e.currentTarget),
  },
  h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, name), sub ? h('span', { class: 'row-sub' }, sub) : null),
  h('span', { class: 'row-side' }, shown, icon('chevronRight', 'row-go')));
  el.setAttribute('aria-label', `${name}: ${textOf(cur)}. Change`);
  function set(v) { cur = v; shown.textContent = textOf(v); el.setAttribute('aria-label', `${name}: ${textOf(v)}. Change`); }
  function open(opener) {
    const s = createSheet({ title: sheetTitle || name, cls: 'pick-sheet', onClose: () => setTimeout(() => s.el.remove(), 240) });
    const rows = options.map(([v, t, sb]) => sheetRow({
      name: t, sub: sb || null, role: 'radio', checked: String(v) === String(cur),
      onclick: async () => {
        s.close();
        if (String(v) === String(cur)) return;
        const before = cur;
        set(v);
        try { await onpick?.(v); } catch { set(before); }
      },
    }));
    s.body.append(h('div', { class: 'rows' }, h('div', { class: 'rows-list', role: 'radiogroup', 'aria-label': name }, rows)),
      sheetNote ? h('p', { class: 'sheet-note' }, sheetNote) : null);
    s.open(opener);
  }
  el.set = set;
  return el;
}

/** A row with a control on the right (a segmented choice, a button): ctlRow({ name, sub, control, cover }). */
export function ctlRow({ name, sub = null, control, cover = null, stack = false } = {}) {
  return h('div', { class: ['row', 'row-static', 'set-ctl', { 'is-stack': stack }], cover },
    h('span', { class: 'row-text' }, h('span', { class: 'row-name' }, name), sub ? h('span', { class: 'row-sub' }, sub) : null),
    h('span', { class: 'set-ctl-box' }, control));
}

/** A line saying what happened, with a mark: said('ok' | 'warn' | 'bad' | 'info', text). */
export function said(kind, ...text) {
  return h('p', { class: ['set-said', `is-${kind}`] }, icon(kind === 'ok' ? 'check' : kind === 'info' ? 'info' : 'warning'), h('span', null, ...text));
}

/** "Not saved: …" with the server's own reasons, not only its summary. */
export const notSaved = (err) => `Not saved: ${err && err.fields ? Object.values(err.fields).join(' ') : (err && err.message) || 'the server did not answer'}`;

/**
 * Ask for the password again, then run(creds, values) inside the sheet: a wrong password stays under its field.
 * extra: more fields shown above the password. Resolves with run's result, or null when closed. run may return
 * false to keep the sheet open.
 */
export function askPassword(ctx, { title, text = null, yes, danger = false, extra = [], hint = 'It is asked again because this hands over all the data.', run }) {
  let twoShown = !!(ctx.me && ctx.me.two_step_required);
  const pass = field({ label: 'Your password', name: 'password', type: 'password', hint: `The one you sign in with. ${hint}`, attrs: { autocomplete: 'current-password', required: true, 'data-autofocus': '' } });
  const two = field({ label: 'Two-step code', name: 'two_step', hint: 'The 6 digits your authenticator app shows now.', attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', maxlength: 8, spellcheck: 'false' } });
  two.hidden = !twoShown;
  let result = null;
  return openSheet({
    title,
    body: (close) => {
      const f = form({
        fields: [...extra, pass, two], submitLabel: yes, busyLabel: 'Working…', cls: danger ? 'set-danger-form' : null,
        onsubmit: async (v) => {
          if (!v.password) throw Object.assign(new Error('Type your password.'), { fields: { password: 'Type your own password.' } });
          const creds = { password: v.password };
          if (twoShown && !String(v.two_step || '').trim()) throw Object.assign(new Error('Type the two-step code as well.'), { fields: { two_step: 'Type the 6 digits your authenticator app shows now.' } });
          if (twoShown && v.two_step) creds.two_step = String(v.two_step).replace(/\s+/g, '');
          let r;
          try {
            r = await run(creds, v);
          } catch (err) {
            if (err && err.code === 'wrong_two_step' && !twoShown) {
              twoShown = true; two.hidden = false;
              throw Object.assign(new Error('Your account asks for a two-step code as well.'), { fields: { two_step: 'Type the 6-digit code from your authenticator app.' } });
            }
            if (err && err.code === 'wrong_password') throw Object.assign(new Error('Not done: the password is not right.'), { fields: { password: 'That is not your password. Type it again.' } });
            if (err && err.code === 'wrong_two_step') throw Object.assign(new Error('Not done: the code is not right.'), { fields: { two_step: 'Not the right code. Type the one your app shows now.' } });
            if (err && err.status === 429) throw new Error(`Too many wrong tries. Wait ${Math.ceil((err.retryAfterMs || 60000) / 1000)} seconds, then try again.`);
            throw err;
          }
          if (r === false) return;
          result = r;
          close('done');
        },
      });
      if (danger) f.submitButton.classList.replace('btn-primary', 'btn-danger');
      return [text ? (typeof text === 'string' ? h('p', { class: 'sheet-text' }, text) : text) : null, h('div', { class: 'set-sheet-form' }, f)];
    },
  }).then(() => result);
}

/** A tick box with its words: tick({ id, text }). el.input is the checkbox. */
export function tick(text, { name = null } = {}) {
  const id = `tk-${Math.random().toString(36).slice(2, 9)}`;
  const input = h('input', { type: 'checkbox', id, name, class: 'tick-input' });
  const el = h('label', { class: 'tick', for: id }, input, h('span', { class: 'tick-box', 'aria-hidden': 'true' }, icon('check')), h('span', null, text));
  el.input = input;
  return el;
}

/**
 * The recovery key in a sheet: big, with Copy, Download, and the tick "I have saved it" that calls
 * POST /system/recovery-key/saved. Resolves true once saved, false when closed without it.
 */
export function keySheet(ctx, key, { title = 'Your recovery key', lede = null, why = 'shown again' } = {}) {
  let ok = false;
  const host = location.host;
  const fileText = ['Sentry Mate recovery key', '', key, '', `For the server at ${host}, ${why} ${new Date(ctx.api.now()).toISOString().slice(0, 10)}.`,
    'It opens this server’s data if its key file is lost, and the export files it makes.', 'Keep it away from the server: in a password manager, or on paper.', ''].join('\r\n');
  return openSheet({
    title,
    body: (close) => {
      const t = tick('I have saved it somewhere safe, away from this server.');
      const err = h('p', { class: 'form-error', role: 'alert', hidden: true });
      const done = btn('Done: the server deletes its copy', { kind: 'primary', icon: 'check', onclick: async () => {
        err.hidden = true;
        if (!t.input.checked) { err.textContent = 'Tick the box first: save the key (Copy or Download), then tick it.'; err.hidden = false; t.input.focus(); return; }
        done.disabled = true;
        try { await ctx.api.post('system/recovery-key/saved', {}); ok = true; close(true); } catch (e) {
          err.textContent = `The server could not delete its copy (${(e && e.message) || 'no answer'}). Press Done again; until then the copy stays on the server.`;
          err.hidden = false;
        } finally { done.disabled = false; }
      } });
      return [
        lede ? h('p', { class: 'sheet-text' }, lede) : null,
        h('div', { class: 'set-key' },
          bigCode(key, { label: 'Recovery key' }),
          h('div', { class: 'form-actions' }, btn('Download as a file', { kind: 'soft', icon: 'download', onclick: () => downloadText(`sentry-mate-recovery-key-${location.hostname.replace(/[^A-Za-z0-9.-]+/g, '_') || 'server'}.txt`, fileText) })),
          t,
          h('p', { class: 'hint' }, 'Closed without the tick? Nothing is lost: the server keeps its copy, and this page can show the key again.'),
          err,
          h('div', { class: 'sheet-actions' }, btn('Close', { kind: 'soft', onclick: () => close(false) }), done)),
      ];
    },
  }).then(() => ok);
}

export { setFieldError };

/** Keep the page's tiles in their order (no backfilling of holes) while it is shown. */
export function inOrder(ctx) {
  ctx.view.classList.add('in-order');
  ctx.onCleanup(() => ctx.view.classList.remove('in-order'));
}
