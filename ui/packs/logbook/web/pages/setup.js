// Sentry Mate, "Logbook" UI pack: the set-up screen (/setup), the first run.
//
// While no account exists (GET /api/v1/setup says needs_setup), every address leads here. The person
// types the one-time set-up code the server printed in its log, and the name and password of the first
// admin account (POST /api/v1/setup); that also signs them in, and Fleet opens.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { field, form } from '../core/form.js';
import { flash } from '../core/flash.js';

export const id = 'setup';
export const title = () => 'Set up Sentry Mate';

function card(...children) {
  return h('section', { class: 'auth auth-wide', 'aria-labelledby': 'auth-title' },
    h('a', { class: 'brand auth-brand', href: '/', 'aria-label': 'Sentry Mate' },
      h('img', { class: 'brand-mark', src: '/icon.svg', alt: '' }), h('span', { class: 'brand-name' }, 'Sentry Mate')),
    children);
}

export async function mount(ctx) {
  const s = await ctx.api.get('setup');
  if (!s.needs_setup) {
    ctx.main.append(card(
      h('h1', { class: 'auth-title', id: 'auth-title' }, 'Set-up is done'),
      h('p', { class: 'auth-text' }, 'Sentry Mate already has an admin account. Sign in to use it.'),
      h('p', { class: 'auth-actions' }, h('a', { class: 'button', href: '/sign-in' }, 'Sign in'))));
    ctx.ready();
    return {};
  }
  const code = field({ label: 'Set-up code', name: 'code', hint: 'Printed once in the server’s log when it started for the first time.', attrs: { autocomplete: 'off', required: true, spellcheck: 'false' } });
  const name = field({ label: 'Your name', name: 'name', hint: 'The name you will sign in with.', attrs: { autocomplete: 'username', required: true } });
  const password = field({ label: 'Password', name: 'password', type: 'password', hint: 'At least 10 characters. A sentence of a few words is easy to remember.', attrs: { autocomplete: 'new-password', required: true } });
  const again = field({ label: 'Password again', name: 'again', type: 'password', attrs: { autocomplete: 'new-password', required: true } });
  ctx.main.append(card(
    h('h1', { class: 'auth-title', id: 'auth-title' }, 'Set up Sentry Mate'),
    h('p', { class: 'auth-text' }, 'Make the first admin account. Admins can add more people later, with their own car lists.'),
    form({
      fields: [code, name, password, again], submitLabel: 'Make the account', busyLabel: 'Setting up…',
      onsubmit: async (v) => {
        if (v.password !== v.again) throw Object.assign(new Error('The two passwords differ.'), { fields: { again: 'This is not the same as the password above.' } });
        try {
          const res = await api.post('setup', { code: String(v.code || '').trim(), name: String(v.name || '').trim(), password: v.password || '' });
          flash(`Welcome, ${(res.me && res.me.name) || v.name}. Your account is ready.`, { next: true });
          location.assign('/');
        } catch (err) {
          if (err && err.code === 'wrong_setup_code') throw Object.assign(new Error(err.message), { fields: { code: 'That code is not right. Look for it in the server’s log.' } });
          throw err;
        }
      },
    })));
  code.control.focus();
  ctx.ready();
  return {};
}
