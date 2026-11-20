// Sign in: name and password (POST /api/v1/session), then the code from an authenticator app when the account
// asks for one (POST /api/v1/session/two-step). An unknown name and a wrong password get the same answer.
// Afterwards the person goes on to the page they asked for (?next=, a page of this app only).

import { h, replace } from '../shared/dom.js';
import { icon, logo, hud, btn } from '../core/ui.js';
import { field, form } from '../core/form.js';
import { toast } from '../core/layer.js';
import * as session from '../core/session.js';

export const id = 'sign-in';
export const title = () => 'Sign in';

export function gateBrand() {
  return h('div', { class: 'gate-brand' }, logo(),
    h('span', { class: 'gate-brand-text' }, h('span', { class: 'gate-brand-name chrome' }, 'Sentry Mate'), h('span', { class: 'gate-brand-sub' }, 'Horizon')));
}

export async function mount(ctx) {
  const next = session.safeNext(ctx.query.next);
  const card = hud({ cls: 'gate', pad: false, label: 'Sign in' });
  replace(ctx.view, card);
  const waitText = (e) => (e?.retryAfterMs ? ` Wait ${Math.ceil(e.retryAfterMs / 1000)} s, then try again.` : ' Wait a minute, then try again.');

  async function inside() {
    await session.signedIn();
    toast('Signed in.', { next: true });
    ctx.navigate(next, { replace: true });
  }

  function passwordStep() {
    const name = field({ label: 'Name', name: 'name', required: true, attrs: { autocomplete: 'username', autocapitalize: 'none', spellcheck: 'false' } });
    const pass = field({ label: 'Password', name: 'password', type: 'password', required: true, attrs: { autocomplete: 'current-password' } });
    const f = form({
      fields: [name, pass], submit: 'Sign in',
      async onSubmit(v) {
        try {
          const r = await ctx.api.post('session', { name: v.name.trim(), password: v.password });
          if (r.status === 'two_step_pending') { codeStep(r.pending); return; }
          await inside();
        } catch (e) {
          if (e.code === 'wrong_credentials') { f.setMessage('That name and password do not match an account here.'); pass.input.select(); return; }
          if (e.status === 429) { f.setMessage(`Too many tries for this name.${waitText(e)}`); return; }
          throw e;
        }
      },
    });
    f.submitButton.classList.add('btn--wide');
    replace(card,
      gateBrand(),
      h('h1', { class: 'gate-title chrome' }, 'Sign in'),
      h('p', { class: 'gate-lead' }, 'Your cars are waiting on the other side of the horizon.'),
      f.el,
      h('p', { class: 'gate-foot' }, icon('lock', 'i--sm'), h('span', null, 'An admin makes accounts in Server and access. Passwords only travel over HTTPS.')));
    name.input.focus();
  }

  function codeStep(pending) {
    const code = field({ label: 'Code from your authenticator app', name: 'code', required: true, hint: 'Six digits. A new code comes every 30 seconds.',
      attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', pattern: '[0-9 ]{6,8}', maxlength: '8' } });
    const f = form({
      fields: [code], submit: 'Confirm',
      secondary: btn('Start again', passwordStep, { kind: 'ghost', icon: 'arrow-l' }),
      async onSubmit(v) {
        try {
          await ctx.api.post('session/two-step', { pending, code: v.code.replace(/\s+/g, '') });
          await inside();
        } catch (e) {
          if (e.code === 'wrong_code') { code.setError('That code is not right. Type the one the app shows now.'); code.input.select(); return; }
          if (e.code === 'sign_in_expired') { toast('That sign-in took too long. Start again.', { kind: 'info' }); passwordStep(); return; }
          if (e.status === 429) { f.setMessage(`Too many wrong codes.${waitText(e)}`); return; }
          throw e;
        }
      },
    });
    replace(card,
      gateBrand(),
      h('h1', { class: 'gate-title chrome' }, 'Two-step sign-in'),
      h('p', { class: 'gate-lead' }, 'This account asks for a code as well as the password.'),
      f.el);
    code.input.focus();
  }

  passwordStep();
  ctx.ready();
}
