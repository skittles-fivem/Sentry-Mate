// The frame of the sign-in and set-up screens: one box, centred both ways on whole pixels, styled as a
// login prompt. bareScreen(ctx, { wide }) -> { box, head, body, paint(...children) }.

import { h, replace } from '../shared/dom.js';
import { centreUpDown } from './ui.js';

export function bareScreen(ctx, { wide = false, label = 'login' } = {}) {
  const head = h('div', { class: 'bare-head' }, h('a', { class: 'cmd-home', href: '/' }, 'sentry-mate'), h('span', { class: 'muted' }, label));
  const body = h('div', { class: 'bare-body' });
  const box = h('section', { class: ['bare-box', wide && 'bare-box--wide'], 'aria-labelledby': 'bare-title' }, head, body);
  const host = h('div', { class: 'bare' }, box);
  replace(ctx.view, host);
  const stop = centreUpDown(host, box);
  ctx.onCleanup(stop);
  return {
    box, head, body,
    paint(...children) { replace(body, children); stop.update(); },
    setWide(on) { box.classList.toggle('bare-box--wide', !!on); stop.update(); },
  };
}

/** True when this page may send a password: HTTPS, or this machine itself (the browser treats both as secure). */
export const canSendSecrets = () => window.isSecureContext === true;

/** The line that says the form will not send over plain HTTP, with the HTTPS address. */
export function plainHttpLine() {
  const https = `https://${location.host}${location.pathname}${location.search}`;
  return h('p', { class: 'frm-msg', role: 'alert' }, 'This page came over plain HTTP, so it will not send a password. Open ',
    h('a', { class: 'lnk', href: https }, https), ' instead.');
}
