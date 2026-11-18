// First run: no account exists yet. The one-time set-up code (printed by the server when it starts)
// creates the first admin (POST /api/v1/setup), who is then signed in and goes to Fleet.

import { h, replace } from '../shared/dom.js';
import { brandMark, icon } from '../core/icons.js';
import { field, form } from '../core/form.js';
import * as session from '../core/session.js';
import { flash } from '../core/flash.js';

export const id = 'setup';
export const title = () => 'Set up';

export async function mount(ctx) {
  const card = h('section', { class: 'panel auth-card', 'aria-labelledby': 'auth-title' });
  replace(ctx.view, h('div', { class: 'auth' }, card));

  const code = field({ label: 'Set-up code', name: 'code', required: true, hint: 'Printed in the server log when Sentry Mate starts for the first time.', attrs: { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'characters' } });
  const name = field({ label: 'Your name for signing in', name: 'name', required: true, attrs: { autocomplete: 'username', autocapitalize: 'none', spellcheck: 'false', maxlength: '64' } });
  const pass = field({ label: 'Password', name: 'password', type: 'password', required: true, hint: 'At least 10 characters.', attrs: { autocomplete: 'new-password', minlength: '10' } });
  const again = field({ label: 'Password again', name: 'again', type: 'password', required: true, attrs: { autocomplete: 'new-password' } });
  const f = form({
    fields: [code, name, pass, again], submit: 'Create the admin account',
    async onSubmit(v) {
      if (v.password !== v.again) { again.setError('The two passwords are not the same.'); again.input.focus(); return; }
      try {
        await ctx.api.post('setup', { code: v.code.trim(), name: v.name.trim(), password: v.password });
      } catch (err) {
        if (err.code === 'wrong_setup_code') { code.setError('That set-up code is not right. Copy it from the server log.'); return; }
        if (err.code === 'setup_done') { flash('Set-up is already done. Sign in instead.', { kind: 'info', next: true }); ctx.navigate('/sign-in', { replace: true }); return; }
        throw err;
      }
      await session.signedIn();
      flash('Welcome. Your admin account is ready.', { next: true });
      ctx.navigate('/', { replace: true });
    },
  });
  replace(card,
    h('div', { class: 'auth-brand' }, brandMark(), h('span', { class: 'brand-name' }, 'Sentry Mate')),
    h('h1', { id: 'auth-title' }, 'Set up Sentry Mate'),
    h('p', null, 'Create the first admin account. Admins add other accounts later, in Settings: Server and access.'),
    f.el,
    h('p', { class: 'auth-note' }, icon('lock', 'ico--sm'), ' The set-up code works once. Everything Sentry Mate stores is encrypted.'));
  code.input.focus();
  ctx.ready();
}
