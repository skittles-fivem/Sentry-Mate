// Data health: the car's CAN logs without a signature (admins only: the answers are admin reads), each with why it
// has none in plain words and those used on the owner's word marked "unsigned, used by the owner"; with the admin's
// "Use unsigned logs", whose dialog says exactly how many logs of which kinds, that bad or other-key logs are never
// included, and offers only those from before the car's first signed log (preselected), the password asked again;
// and "Stop using" for the ones in use. Watch accounts get none of it (the page does not build it for them).
// API: GET cars/{car}/logs/unsigned; POST and DELETE cars/{car}/logs/use-unsigned { all, before_first_signed?,
// confirm_password, code? } (docs/ui-api.md, docs/security.md "Pairing and the owner's word").

import { h, replace } from '../shared/dom.js';
import * as U from '../shared/unsigned.js';
import { formatNumber } from '../shared/i18n.js';
import { scrollBox } from '../shared/scrollbox.js';
import { icon } from '../core/icons.js';
import { tag } from '../core/page.js';
import { field } from '../core/form.js';
import { flash } from '../core/flash.js';
import { errorWords } from '../core/states.js';
import { askPassword } from './settings-server-move.js';

const enc = encodeURIComponent;

/** unsignedLogs(ctx, { car, when, onChanged }) -> { el, load() }: the block, read again with load(). */
export function unsignedLogs(ctx, { car, when, onChanged = null }) {
  const base = `cars/${enc(car.id)}/logs`;
  const el = h('div', { class: 'hl-unsigned', id: 'unsigned-logs' });
  let last = null;
  let sig = null;
  let box = null;
  ctx.onCleanup(() => box?.destroy());

  async function load() {
    let u;
    try {
      u = await ctx.api.get(`${base}/unsigned`);
    } catch (e) {
      if (!ctx.alive() || e?.name === 'AbortError') return;
      paintError(e);
      return;
    }
    if (!ctx.alive()) return;
    const s = JSON.stringify(u);
    if (s === sig) return;
    sig = s;
    last = u;
    paint(u);
  }

  function paintError(e) {
    sig = null;
    replace(el, h('h3', { class: 'hl-sub' }, 'Logs without a signature'),
      h('p', { class: 'note warn' }, `Could not read them: ${errorWords(e).text}`));
  }

  function row(r) {
    return h('li', { class: 'hl-unsigned-row' },
      h('span', { class: 'cell-text' },
        h('span', { class: 'main' }, when(ctx, r.start_ms),
          r.override_use ? tag(U.USED_BY_OWNER, 'warm') : null),
        h('span', { class: 'sub' }, U.kindText(r), r.before_first_signed ? ' · before the first signed log' : '')));
  }

  function paint(u) {
    box?.destroy(); box = null;
    const head = h('h3', { class: 'hl-sub' }, 'Logs without a signature');
    if (!u.can_use && !u.in_use) {
      replace(el, head, h('p', { class: 'note hl-missing-line' }, icon('check', 'ico--sm'),
        h('span', null, u.never
          ? `None waits for your word. ${formatNumber(u.never)} ${u.never === 1 ? 'log' : 'logs'} failed for something else (a bad signature, another key’s, a changed file) and ${u.never === 1 ? 'is' : 'are'} never used.`
          : 'None: every log of this car is signed, or came before the car was paired.')));
      return;
    }
    const items = u.items || [];
    const more = (u.items_total ?? items.length) - items.length;
    const list = items.length
      ? (box = scrollBox({ cls: 'hl-span-box scroll-box--fit', label: 'Logs without a signature' },
        h('ul', { class: 'hl-unsigned-list' }, items.map(row))))
      : null;
    const buttons = h('div', { class: 'hl-unsigned-actions' },
      u.can_use ? h('button', { class: 'btn btn--sm', type: 'button', onclick: () => use() }, icon('check', 'ico--sm'), U.USE_BUTTON) : null,
      u.in_use ? h('button', { class: 'btn btn--sm btn--ghost', type: 'button', onclick: () => stop() }, U.STOP_BUTTON) : null);
    replace(el, head,
      h('p', { class: 'note' }, U.summaryText(u, formatNumber)),
      list ? list.el : null,
      more > 0 ? h('p', { class: 'note faint' }, `The newest ${formatNumber(items.length)} are listed; ${formatNumber(more)} more are counted above.`) : null,
      buttons,
      h('p', { class: 'note faint' }, 'The CAN Bridge app still shows these as failed: they have no signature. Logs with a bad signature or another key’s are never used.'));
  }

  // the dialog's own words for the password and the code; the server's for anything else
  const send = async (method, b) => {
    try {
      return await ctx.api.send(method, `${base}/use-unsigned`, b);
    } catch (e) {
      if (!e || e.name === 'AbortError') throw e;
      if (e.code === 'password_needed') e.code = 'wrong_password';
      else if (e.code === 'wrong_code') e.code = 'wrong_two_step';
      if (e.fields) { delete e.fields.confirm_password; delete e.fields.code; if (!Object.keys(e.fields).length) e.fields = null; }
      throw e;
    }
  };

  async function use() {
    const u = last;
    if (!u?.can_use) return;
    const c = U.choice(u, formatNumber);
    const only = c ? field({ label: c.label, name: 'only_before', type: 'checkbox', value: c.value, hint: c.hint }) : null;
    const intro = h('div', { class: 'hl-unsigned-intro' });
    const paintIntro = () => replace(intro, U.confirmText(u, car.name, !!only?.value(), formatNumber).map((t) => h('p', null, t)));
    paintIntro();
    only?.input.addEventListener('change', paintIntro);
    const out = await askPassword(ctx, {
      title: U.USE_BUTTON, intro, submit: U.USE_BUTTON, extra: only ? [only] : [],
      run: (cred) => send('POST', U.body({ password: cred.password, code: cred.two_step, onlyBefore: !!only?.value() })),
    });
    if (!ctx.alive() || !out) return;
    flash(U.doneText(out, true, formatNumber));
    sig = null;
    await load();
    onChanged?.();
  }

  async function stop() {
    const u = last;
    if (!u?.in_use) return;
    const out = await askPassword(ctx, {
      title: 'Stop using unsigned logs', intro: h('p', null, U.stopText(u, car.name, formatNumber)), submit: U.STOP_BUTTON, danger: true,
      run: (cred) => send('DELETE', U.body({ password: cred.password, code: cred.two_step })),
    });
    if (!ctx.alive() || !out) return;
    flash(U.doneText(out, false, formatNumber));
    sig = null;
    await load();
    onChanged?.();
  }

  return { el, load };
}
