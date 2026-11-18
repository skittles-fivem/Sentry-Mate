// Sentry Mate, "Logbook" UI pack: the sign-in screen (/sign-in?next=<where to go after>).
//
// Name and password (POST /api/v1/session), then the two-step code when the account has one
// (POST /api/v1/session/two-step). Shown without the frame. After signing in the page loads the address
// it came from, so the frame starts afresh with the new session. Passwords only ever go over HTTPS on a
// real server; the dev server is plain HTTP on 127.0.0.1 only.

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { field, form } from '../core/form.js';
import { flash } from '../core/flash.js';

export const id = 'sign-in';
export const title = () => 'Sign in';

/**
 * Only an address inside the app: '/cars/c1' yes; '//elsewhere', 'https://…', '/api/…' no. It is read the way
 * the browser reads it ('/\\host' and '/<tab>/host' are other sites to a browser), and only the path, query
 * and hash on this origin are kept.
 */
export function safeNext(v) {
  const s = typeof v === 'string' ? v : '';
  if (!s.startsWith('/')) return '/';
  let u;
  try { u = new URL(s, location.origin); } catch { return '/'; }
  if (u.origin !== location.origin) return '/';
  const path = u.pathname;
  if (path.startsWith('//') || path.startsWith('/api/') || path === '/sign-in' || path === '/setup') return '/';
  return path + u.search + u.hash;
}

function card(...children) {
  return h('section', { class: 'auth', 'aria-labelledby': 'auth-title' },
    h('a', { class: 'brand auth-brand', href: '/', 'aria-label': 'Sentry Mate' },
      h('img', { class: 'brand-mark', src: '/icon.svg', alt: '' }), h('span', { class: 'brand-name' }, 'Sentry Mate')),
    children);
}

export async function mount(ctx) {
  const next = safeNext(ctx.query.next);
  let me = null;
  try { me = await ctx.api.get('me'); } catch (err) { if (!(err && err.status === 401)) throw err; }
  if (me) {
    ctx.main.append(card(
      h('h1', { class: 'auth-title', id: 'auth-title' }, 'You are signed in'),
      h('p', { class: 'auth-text' }, `As ${me.name}.`),
      h('p', { class: 'auth-actions' }, h('a', { class: 'button', href: next }, 'Continue'))));
    ctx.ready();
    return {};
  }

  const done = (who) => {
    flash(`Signed in as ${who.name}.`, { next: true });
    location.assign(next);
  };
  const box = h('div', { class: 'auth-body' });
  const showCode = (pending) => {
    const code = field({ label: 'Code from your app', name: 'code', attrs: { autocomplete: 'one-time-code', inputmode: 'numeric', required: true, maxlength: '10' } });
    replace(box, 
      h('p', { class: 'auth-text' }, 'This account uses two-step sign-in. Type the 6-digit code your authenticator app shows now.'),
      form({
        fields: [code], submitLabel: 'Sign in', busyLabel: 'Checking…',
        onsubmit: async (v) => {
          const res = await api.post('session/two-step', { pending, code: String(v.code || '').trim() });
          done(res.me || { name: '' });
        },
      }),
      h('p', { class: 'auth-foot' }, h('a', { href: location.pathname + location.search }, 'Start again')));
    code.control.focus();
  };
  const name = field({ label: 'Name', name: 'name', attrs: { autocomplete: 'username', required: true, autocapitalize: 'none', spellcheck: 'false' } });
  const password = field({ label: 'Password', name: 'password', type: 'password', attrs: { autocomplete: 'current-password', required: true } });
  box.append(form({
    fields: [name, password], submitLabel: 'Sign in', busyLabel: 'Signing in…',
    onsubmit: async (v) => {
      if (!v.name || !v.password) throw Object.assign(new Error('Type your name and password.'), { fields: { ...(v.name ? {} : { name: 'Type your name.' }), ...(v.password ? {} : { password: 'Type your password.' }) } });
      try {
        const res = await api.post('session', { name: v.name.trim(), password: v.password });
        if (res.status === 'two_step_pending') showCode(res.pending);
        else done(res.me || { name: v.name.trim() });
      } catch (err) {
        if (err && err.status === 429) {
          const s = Math.ceil((err.retryAfterMs || 60000) / 1000);
          throw new Error(`Too many tries. Wait ${s} s, then try again.`);
        }
        if (err && err.code === 'wrong_credentials') throw new Error('That name and password do not match an account.');
        throw err;
      }
    },
  }));
  ctx.main.append(card(
    h('h1', { class: 'auth-title', id: 'auth-title' }, 'Sign in'),
    h('p', { class: 'auth-text' }, 'Sentry Mate keeps your cars’ drives, charges and battery history. Sign in with the account an admin made for you.'),
    box,
    h('p', { class: 'auth-foot' }, 'Forgot the password? An admin can set a new one in Settings: Server and access.')));
  name.control.focus();
  ctx.ready();
  return {};
}
