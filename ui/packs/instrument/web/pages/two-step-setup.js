// Set up two-step sign-in: what an account without it is shown, at every address, while the server asks every account for
// it (GET /me says two_step_setup_only; every other call answers 403 two_step_setup). It says why, offers the turn-on
// flow of Settings (pages/two-step-turn-on.js) and Sign out; once it is on, the app is loaded again as it is for
// everyone else.

import { h, replace } from '../shared/dom.js';
import { brandMark, icon } from '../core/icons.js';
import * as session from '../core/session.js';
import { flash } from '../core/flash.js';
import { confirmTwoStep, recoveryDialog } from './two-step-turn-on.js';

export const id = 'two-step-setup';
export const css = true;
export const title = () => 'Set up two-step sign-in';

export async function mount(ctx) {
  const me = ctx.me || {};
  const note = h('p', { class: 'form-error', hidden: true, role: 'alert' });
  const turnOn = h('button', { class: 'btn btn--primary', type: 'button' }, 'Turn on two-step sign-in');
  const out = h('button', { class: 'btn btn--ghost', type: 'button' }, 'Sign out');
  const card = h('section', { class: 'panel auth-card', 'aria-labelledby': 'auth-title' },
    h('div', { class: 'auth-brand' }, brandMark(), h('span', { class: 'brand-name' }, 'Sentry Mate')),
    h('h1', { id: 'auth-title' }, 'Set up two-step sign-in'),
    h('p', null, `This server asks every account for two-step sign-in: after the password, a six-digit code from an authenticator app.${me.name ? ` ${me.name} has none yet, so` : ' This account has none yet, so'} nothing else is open until it is set up.`),
    h('p', null, 'It takes a minute: add the key to an authenticator app, type the code it shows, and keep the recovery codes you get.'),
    note,
    h('div', { class: 'row su2-acts' }, turnOn, out),
    h('p', { class: 'auth-note' }, icon('lock', 'ico--sm'), ' An admin can change this rule in Settings: Server and access.'));
  replace(ctx.view, h('div', { class: 'auth' }, card));

  turnOn.addEventListener('click', async () => {
    turnOn.disabled = true;
    try {
      const r = await confirmTwoStep(ctx, me);
      if (r && ctx.alive()) {
        if (r.codes) await recoveryDialog(r.codes, 'Your recovery codes');
        // the account is let in now: the whole app is built again, at the address that was asked for
        globalThis.location.reload();
        return;
      }
    } catch (e) {
      note.hidden = false;
      note.textContent = `Could not set it up: ${e?.message || 'try again'}.`;
    }
    turnOn.disabled = false;
  });
  out.addEventListener('click', async () => {
    out.disabled = true;
    try {
      await session.signOut();
      flash('Signed out.', { next: true });
      ctx.navigate('/sign-in', { replace: true });
    } catch (e) {
      out.disabled = false;
      flash(`Could not sign out: ${e?.message || 'try again'}`, { kind: 'error' });
    }
  });
  turnOn.focus({ preventScroll: true });
  ctx.ready();
}
