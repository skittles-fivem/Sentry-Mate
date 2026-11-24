// Sign in: name and password (POST /api/v1/session), then a two-step code when the account needs one
// (POST /api/v1/session/two-step). The answer is the same for an unknown name and a wrong password.
// After signing in, the person goes back to the page they asked for (?next=, a path inside the app).

import { h, replace } from '../shared/dom.js';
import { brandMark, icon } from '../core/icons.js';
import { field, form } from '../core/form.js';
import * as session from '../core/session.js';
import { flash } from '../core/flash.js';

export const id = 'sign-in';
export const css = true;
export const title = () => 'Sign in';

export async function mount(ctx) {
  const next = session.safeNext(ctx.query.next);
  const card = h('section', { class: 'panel auth-card', 'aria-labelledby': 'auth-title' });
  replace(ctx.view, h('div', { class: 'auth' }, card));

  const done = async () => {
    await session.signedIn();
    flash('Signed in.', { next: true });
    ctx.navigate(next, { replace: true });
  };
  const wait = (err) => (err?.retryAfterMs ? ` Wait ${Math.ceil(err.retryAfterMs / 1000)} s, then try again.` : '');

  function passwordStep() {
    const name = field({ label: 'Name', name: 'name', required: true, attrs: { autocomplete: 'username', autocapitalize: 'none', spellcheck: 'false' } });
    const pass = field({ label: 'Password', name: 'password', type: 'password', required: true, attrs: { autocomplete: 'current-password' } });
    const f = form({
      fields: [name, pass], submit: 'Sign in',
      async onSubmit(v) {
        try {
          const r = await ctx.api.post('session', { name: v.name.trim(), password: v.password });
          if (r.status === 'two_step_pending') { codeStep(r.pending); return; }
          await done();
        } catch (err) {
          if (err.code === 'wrong_credentials') { f.setMessage('That name and password do not match an account.'); pass.input.select(); return; }
          if (err.status === 429) { f.setMessage(`Too many tries for this name.${wait(err)}`); return; }
          throw err;
        }
      },
    });
    replace(card,
      h('div', { class: 'auth-brand' }, brandMark(), h('span', { class: 'brand-name' }, 'Sentry Mate')),
      h('h1', { id: 'auth-title' }, 'Sign in'),
      h('p', null, 'Sign in to see your cars.'),
      f.el,
      h('p', { class: 'auth-note' }, icon('lock', 'ico--sm'), ' Accounts are made by an admin in Settings: Server and access. Passwords are only sent over HTTPS.'));
    name.input.focus();
  }

  function codeStep(pending) {
    const code = field({ label: 'Code from your authenticator app', name: 'code', required: true, attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', pattern: '[0-9 ]{6,8}', maxlength: '8' } });
    const f = form({
      fields: [code], submit: 'Confirm',
      secondary: h('button', { class: 'btn btn--ghost', type: 'button', onclick: passwordStep }, 'Start again'),
      async onSubmit(v) {
        try {
          await ctx.api.post('session/two-step', { pending, code: v.code.replace(/\s+/g, '') });
          await done();
        } catch (err) {
          if (err.code === 'wrong_code') { code.setError('That code is not right. Codes change every 30 seconds.'); return; }
          if (err.code === 'sign_in_expired') { flash('That sign-in took too long. Start again.', { kind: 'info' }); passwordStep(); return; }
          throw err;
        }
      },
    });
    replace(card,
      h('div', { class: 'auth-brand' }, brandMark(), h('span', { class: 'brand-name' }, 'Sentry Mate')),
      h('h1', { id: 'auth-title' }, 'Two-step sign-in'),
      h('p', null, 'This account asks for a code as well as the password.'),
      f.el);
    code.input.focus();
  }

  passwordStep();
  ctx.ready();
}
