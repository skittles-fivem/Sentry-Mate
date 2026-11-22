// Sign in: user and password (POST /api/v1/session), then a two-step code when the account needs one
// (POST /api/v1/session/two-step). The answer is the same for an unknown name and a wrong password. The
// form refuses to send over plain HTTP. Afterwards the person goes back to ?next= (a path of this app).

import { h } from '../shared/dom.js';
import { field, form } from '../core/form.js';
import * as session from '../core/session.js';
import { toast } from '../core/toast.js';
import { bareScreen, canSendSecrets, plainHttpLine } from '../core/bare.js';

export const id = 'sign-in';
export const title = () => 'Sign in';

export async function mount(ctx) {
  const next = session.safeNext(ctx.query.next);
  const scr = bareScreen(ctx, { label: 'login' });
  const secure = canSendSecrets();
  const wait = (e) => (e?.retryAfterMs ? ` Wait ${Math.ceil(e.retryAfterMs / 1000)} s, then try again.` : '');

  const done = async () => {
    await session.signedIn();
    toast('Signed in.', { next: true });
    ctx.navigate(next, { replace: true });
  };

  function passwordStep() {
    const user = field({ label: 'user', name: 'name', required: true, attrs: { autocomplete: 'username', autocapitalize: 'none', spellcheck: 'false' } });
    const pass = field({ label: 'password', name: 'password', type: 'password', required: true, attrs: { autocomplete: 'current-password' } });
    const f = form({
      fields: [user, pass], submit: 'sign in',
      async onSubmit(v) {
        if (!canSendSecrets()) { f.setMessage('Not sent: this page is plain HTTP. Open it over HTTPS.'); return; }
        try {
          const r = await ctx.api.post('session', { name: v.name.trim(), password: v.password });
          if (r.status === 'two_step_pending') { codeStep(r.pending); return; }
          await done();
        } catch (e) {
          if (e.code === 'wrong_credentials') { f.setMessage('That user and password do not match an account.'); pass.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many tries for this user.${wait(e)}`); return; }
          throw e;
        }
      },
    });
    if (!secure) f.submitButton.disabled = true;
    scr.paint(
      h('h1', { id: 'bare-title', class: 'bare-title' }, 'sign in'),
      h('p', { class: 'bare-text' }, 'Sign in to see your cars.'),
      secure ? null : plainHttpLine(),
      f.el,
      h('p', { class: 'bare-note' }, 'https only: passwords are never sent over plain HTTP. Accounts are made by an admin in settings/server.'));
    if (secure) user.input.focus();
  }

  function codeStep(pending) {
    const code = field({ label: 'code', name: 'code', required: true, hint: 'From your authenticator app. Codes change every 30 seconds.',
      attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', pattern: '[0-9 ]{6,8}', maxlength: '8' } });
    const f = form({
      fields: [code], submit: 'confirm',
      secondary: h('button', { class: 'btn', type: 'button', onclick: passwordStep }, 'start again'),
      async onSubmit(v) {
        try {
          await ctx.api.post('session/two-step', { pending, code: v.code.replace(/\s+/g, '') });
          await done();
        } catch (e) {
          if (e.code === 'wrong_code') { code.setError('That code is not right. Type the one the app shows now.'); return; }
          if (e.code === 'sign_in_expired') { toast('That sign-in took too long. Start again.', { kind: 'info' }); passwordStep(); return; }
          throw e;
        }
      },
    });
    scr.paint(
      h('h1', { id: 'bare-title', class: 'bare-title' }, 'two-step sign-in'),
      h('p', { class: 'bare-text' }, 'This account asks for a code as well as the password.'),
      f.el);
    code.input.focus();
  }

  passwordStep();
  ctx.ready();
}
