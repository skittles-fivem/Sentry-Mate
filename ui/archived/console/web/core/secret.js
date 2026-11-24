// Codes, keys and tokens shown once: a copy button, a text file to download, and the person's own password
// again before a dangerous action. Used by settings/server (move code, export file, recovery key) and
// settings/cars (upload tokens).

import { h, replace } from '../shared/dom.js';
import { dayKey } from '../shared/time.js';
import { field, form } from './form.js';
import { openDialog } from './dialog.js';
import { dateTime } from './fmt.js';

/** [copy] with its own 'copied' word after it. */
export function copyBtn(text, label = 'copy', { primary = false } = {}) {
  const said = h('span', { class: 'muted', 'aria-live': 'polite' });
  const b = h('button', { class: ['btn', primary && 'btn--primary'], type: 'button', onclick: async () => {
    try { await navigator.clipboard.writeText(typeof text === 'function' ? text() : text); said.textContent = 'copied'; } catch { said.textContent = 'this browser did not let the page copy: select it and copy by hand'; }
  } }, label);
  return [b, said];
}

/** A code in the accent, big, read out character by character. */
export const codeOut = (text) => h('p', { class: 'code-out', 'aria-label': String(text).split('').join(' ') }, text);

/** Save text as a file in the browser. */
export function downloadText(name, text) {
  const href = URL.createObjectURL(new Blob([text], { type: 'text/plain;charset=utf-8' }));
  const a = h('a', { href, download: name, hidden: true });
  document.body.append(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(href), 30000);
}

/** The recovery key as a file named after this server and the day, the full date in it. */
export function downloadKey(key, { ms, tz, why = 'shown' }) {
  const day = dayKey(ms, tz);
  const host = location.hostname.replace(/[^A-Za-z0-9.-]+/g, '_') || 'server';
  downloadText(`sentry-mate-recovery-key-${host}-${day}.txt`,
    `Sentry Mate recovery key\r\n\r\n${key}\r\n\r\nFor the server at ${location.host}, ${why} ${dateTime(ms, tz, { year: true })} (${day}).\r\n`
    + 'It opens this server\'s data if its key file is lost, and the export files it makes.\r\nKeep it away from the server: in a password manager, or on paper.\r\n');
}

export const STAY = Symbol('stay');   // run() answers this to keep the dialog open

/**
 * The person's own password again (and the two-step code when the account has it) before a dangerous action.
 * run(credentials, values) makes the call; a wrong password keeps the dialog open. Resolves the answer, or null.
 * focus: the element to start on instead of the password field (for a dialog whose first step comes before it);
 * the dialog then opens at its top. onOpen(dialog, form) runs once it is open.
 */
export function askPassword(ctx, { title, intro, submit, danger = false, extra = [], run, focus = null, onOpen = null }) {
  const me = ctx.me || {};
  const pass = field({ label: 'password', name: 'password', type: 'password', required: true,
    hint: `The password you sign in with as ${me.name || 'this account'}.`, attrs: { autocomplete: 'current-password' } });
  const two = field({ label: 'code', name: 'two_step', hint: 'The six digits your authenticator app shows now.',
    attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', maxlength: '8' } });
  two.el.hidden = !me.two_step_required;
  let result = null;
  return new Promise((resolve) => {
    let d = null;
    const f = form({
      fields: [...extra, pass, two], submit, danger,
      secondary: h('button', { class: 'btn', type: 'button', onclick: () => d.close() }, 'cancel'),
      async onSubmit(v) {
        const cred = { password: v.password };
        if (!two.el.hidden && v.two_step) cred.two_step = v.two_step.replace(/\s+/g, '');
        try {
          const r = await run(cred, v);
          if (r === STAY) return;
          result = r;
        } catch (e) {
          if (e.code === 'wrong_password') { pass.setError(v.password ? 'That is not your password.' : 'Type your password to go on.'); pass.input.select(); return; }
          if (e.code === 'wrong_two_step') { two.el.hidden = false; two.setError(v.two_step ? 'That code is not right. Type the one the app shows now.' : 'Your account has two-step sign-in: type the code as well.'); two.input.focus(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong passwords. Wait ${e.retryAfterMs ? Math.ceil(e.retryAfterMs / 1000) : 60} s, then try again.`); return; }
          throw e;
        }
        d.close();
      },
    });
    d = openDialog({ title, body: [intro, f.el], onClose: () => resolve(result) });
    if (focus) { focus.focus({ preventScroll: true }); d.el.scrollTop = 0; } else pass.input.focus();
    onOpen?.(d, f);
  });
}

/** A dialog that shows a secret once: its lines, then 'I have saved it' or 'done'. */
export function showOnce({ title, body, okLabel = 'done', onOk = null }) {
  return new Promise((resolve) => {
    const err = h('p', { class: 'frm-msg', role: 'alert', hidden: true });
    let answer = false;
    const ok = h('button', { class: 'btn btn--primary', type: 'button', onclick: async () => {
      if (onOk) {
        const why = await onOk();
        if (why) { replace(err, why); err.hidden = false; return; }
      }
      answer = true; d.close();
    } }, okLabel);
    const d = openDialog({ title, body: [body, err], actions: [ok, h('button', { class: 'btn', type: 'button', onclick: () => d.close() }, 'close')], onClose: () => resolve(answer) });
  });
}
