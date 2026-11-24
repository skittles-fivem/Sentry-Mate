// Turning two-step sign-in on for one's own account, shared by Settings: Server and access (its Two-step panel) and the
// set-up page an account without it is sent to when the server asks every account for it (pages/two-step-setup.js).
//   POST /me/two-step                 the key and its otpauth address
//   POST /me/two-step/confirm         { password, code }: on; the recovery codes, shown once

import { h } from '../shared/dom.js';
import { field, form } from '../core/form.js';
import { codeAttrs, codeValue, CODE_HINT } from '../core/code.js';
import { errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { openDialog } from '../core/dialog.js';
import { codeGroups, copyButton } from './settings-server-move.js';

const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);

export const cancelBtn = () => h('button', { class: 'btn btn--ghost', type: 'button', onclick: (e) => e.target.closest('dialog')?.close() }, 'Cancel');

export const ownPassword = (me) => field({ label: 'Your password', name: 'password', type: 'password', required: true,
  hint: `The password you sign in with as ${me?.name || 'this account'}.`, attrs: { autocomplete: 'current-password' } });

export const ownCode = (label) => field({ label, name: 'code', required: true, hint: CODE_HINT, attrs: codeAttrs() });

/** A wrong password or code is said under its field; true when it was one of those. */
export function ownWrong(e, password, code, v) {
  if (e.code === 'wrong_password') { password.setError(v.password ? 'That is not your password. Type the one you sign in with.' : 'Type your password to go on.'); password.input.focus(); password.input.select(); return true; }
  if (e.code === 'wrong_code') { code.setError(v.code ? 'That code is not right. Type the one your authenticator app shows now.' : 'Type the code from your authenticator app.'); code.input.focus(); return true; }
  return false;
}

/** The recovery codes, shown once. Resolves when the dialog is closed. */
export function recoveryDialog(codes, title) {
  return openDialog({
    title, wide: true,
    body: h('div', { class: 'ss-rdlg' },
      h('p', null, 'Keep these recovery codes somewhere safe, away from your phone (a password manager, or on paper). Each one works once, in place of the app\'s code, if you lose your phone. They are shown only now.'),
      h('div', { class: 'mv-codebox' },
        h('p', { class: 'mv-codebox-head' }, 'Recovery codes'),
        h('ul', { class: 'ss-rcodes', 'aria-label': 'Recovery codes' }, codes.map((c) => h('li', null, h('code', { class: 'mono ss-value' }, c)))),
        h('div', { class: 'row mv-acts' }, copyButton(codes.join('\n'), 'Copy all codes', { primary: true })))),
    actions: [{ label: 'I have kept them', value: true, kind: 'primary', focus: true }],
  });
}

/**
 * Starts the set-up and asks for the password and the first code in a dialog. Resolves { codes } once it is on (the
 * recovery codes, not yet shown), { already: true } when it was on already, or null (closed, or it could not start).
 */
export async function confirmTwoStep(ctx, me) {
  let setup;
  try { setup = await ctx.api.post('me/two-step', {}); } catch (e) {
    if (e.code === 'two_step_on') return { already: true };
    flash(`Could not start: ${errText(e)}`, { kind: 'error' });
    return null;
  }
  const password = ownPassword(me);
  const code = ownCode('Code from your authenticator app');
  let codes = null;
  const f = form({
    fields: [password, code], submit: 'Turn on two-step sign-in', secondary: cancelBtn(),
    async onSubmit(v) {
      try {
        const out = await ctx.api.post('me/two-step/confirm', { password: v.password, code: codeValue(v.code) });
        codes = out.recovery_codes || [];
      } catch (e) {
        if (ownWrong(e, password, code, v)) return;
        if (e.status === 409) { f.setMessage('This set-up ran out or was started again. Close this and press Turn on again.'); return; }
        throw e;
      }
      f.el.closest('dialog')?.close();
    },
  });
  // h() leaves out an href it does not know as safe (otpauth: is none of its schemes): set it by hand, for this scheme only
  const appLink = h('a', { class: 'btn' }, 'Open in your authenticator app');
  if (/^otpauth:\/\/totp\//.test(setup.uri)) appLink.setAttribute('href', setup.uri); else appLink.hidden = true;
  const grouped = String(setup.secret).replace(/\s+/g, '').match(/.{1,4}/g)?.join('-') || setup.secret;
  const done = openDialog({
    title: 'Turn on two-step sign-in', wide: true,
    body: h('div', { class: 'ss-tsdlg' },
      h('ol', { class: 'ss-steps' },
        h('li', null, 'In an authenticator app (any app that makes six-digit codes), add an account with this key:',
          h('div', { class: 'mv-codebox' }, h('p', { class: 'mv-codebox-head' }, 'Key'), codeGroups(grouped, 'mv-code ss-secret'),
            h('div', { class: 'row mv-acts' }, copyButton(setup.secret, 'Copy key'),
              appLink, copyButton(setup.uri, 'Copy link')))),
        h('li', null, 'Type your password and the six-digit code the app shows now.')),
      f.el),
    actions: [],
  });
  password.input.focus({ preventScroll: true });
  await done;
  return codes ? { codes } : null;
}
