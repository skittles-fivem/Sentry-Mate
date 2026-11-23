// The full rebuild block (admin only; used by pages/health.js for one car and by pages/settings-server.js for
// every car): what it does and does not touch, the button (Full rebuild, or Cancel while one runs) and a progress
// panel of fixed size: the bar, the seven lines of shared/fullrebuild.js and three lines for problems. Built once
// and changed in place on every read, so nothing on the page moves.
// API: GET cars/{car}/full-rebuild or system/full-rebuild (every POLL_MS while one runs, IDLE_POLL_MS otherwise),
// POST the same with the password asked again, DELETE system/full-rebuild to cancel.

import { h, replace, setVars } from '../shared/dom.js';
import * as fr from '../shared/fullrebuild.js';
import { icon } from '../core/icons.js';
import { flash } from '../core/flash.js';
import { confirmDialog } from '../core/dialog.js';
import { errorWords } from '../core/states.js';
import { askPassword } from './settings-server-move.js';

const enc = encodeURIComponent;
const ERRORS_SHOWN = 3;
const TONE_BAR = { warn: 'bar--warm', error: 'bar--low', off: 'bar--muted' };

/**
 * fullRebuild(ctx, { car, carName, label, onStatus, onEnded }) -> { el, ready, status(), repaint() }.
 * car: the car ({ id, name }) for one car, null for every car. carName(id): a car's name in the lines.
 * label: the button's words while none runs (BUTTON or BUTTON_ALL by default).
 * onStatus(status) after every read; onEnded(status) once when a run seen running has ended.
 */
export function fullRebuild(ctx, { car = null, carName = (id) => id, label = null, onStatus = null, onEnded = null } = {}) {
  const start = label || (car ? fr.BUTTON : fr.BUTTON_ALL);
  const path = car ? `cars/${enc(car.id)}/full-rebuild` : 'system/full-rebuild';
  let S = null;

  // the button: its words and icon change in place
  const btn = h('button', { class: 'btn', type: 'button', onclick: () => press() });
  const stale = h('p', { class: 'fr-stale', hidden: true });

  // the panel: every row is there from the start, an empty one keeps its room
  const bar = h('div', { class: 'bar fr-bar', role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-label': 'Full rebuild progress' }, h('i'));
  const pct = h('span', { class: 'fr-pct' });
  const rows = fr.lines(null).map((l) => {
    const dt = h('dt', null, l.label);
    const dd = h('dd', null);
    return { key: l.key, dt, dd, el: h('div', { class: 'fr-line', 'data-key': l.key }, dt, dd) };
  });
  const errs = Array.from({ length: ERRORS_SHOWN }, () => h('li', null));
  const panelEl = h('div', { class: 'fr-panel', 'aria-live': 'polite' },
    h('div', { class: 'fr-bar-row' }, bar, pct),
    h('dl', { class: 'fr-lines' }, rows.map((r) => r.el)),
    h('div', { class: 'fr-errors-box' },
      h('p', { class: 'fr-errors-head' }, 'Problems'),
      h('ul', { class: 'fr-errors' }, errs)));

  const el = h('div', { class: 'fr' },
    h('div', { class: 'fr-about' },
      fr.EXPLAIN.map((t) => h('p', { class: 'fr-explain' }, t)),
      stale,
      h('div', { class: 'row fr-acts' }, btn)),
    panelEl);

  function paint() {
    const now = ctx.api.now();
    const on = fr.running(S);
    replace(btn, icon(on ? 'x' : 'reset', 'ico--sm'), on ? fr.CANCEL : start);
    btn.classList.toggle('btn--danger', on);
    btn.title = on ? 'Stop the full rebuild: cars not switched yet keep their old history' : null;
    const st = car ? fr.staleText(S) : '';
    stale.textContent = st;
    stale.hidden = !st;

    const p = fr.percent(S);
    setVars(bar, { '--w': `${(fr.fraction(S) * 100).toFixed(1)}%` });
    bar.setAttribute('aria-valuenow', String(p ?? 0));
    bar.classList.remove('bar--warm', 'bar--low', 'bar--muted');
    const tb = TONE_BAR[fr.tone(S, now)];
    if (tb) bar.classList.add(tb);
    pct.textContent = p == null ? '' : `${p} %`;

    const ls = fr.lines(S, now, carName);
    ls.forEach((l, i) => {
      const r = rows[i];
      r.dt.textContent = l.label;
      r.dd.textContent = l.text;
      r.dd.title = l.text || '';
      r.el.dataset.tone = l.key === 'state' ? fr.tone(S, now) : '';
    });

    const n = Array.isArray(S?.errors) ? S.errors.length : 0;
    const el3 = fr.errorLines(S, n > ERRORS_SHOWN ? ERRORS_SHOWN - 1 : ERRORS_SHOWN, carName);
    errs.forEach((li, i) => { li.textContent = el3[i] || ''; li.title = el3[i] || ''; });
    if (!n && S && S.state !== 'idle') errs[0].textContent = 'None so far.';
    panelEl.dataset.tone = fr.tone(S, now);
  }

  function set(s) {
    const was = fr.running(S);
    S = s;
    paint();
    onStatus?.(s);
    if (was && !fr.running(s)) onEnded?.(s);
  }

  /* ---------------------------------------------------------------- reading */
  let timer = null;
  ctx.onCleanup(() => clearTimeout(timer));
  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(refresh, fr.running(S) ? fr.POLL_MS : fr.IDLE_POLL_MS);
  }
  async function refresh() {
    clearTimeout(timer);
    try {
      const s = await ctx.api.get(path);
      if (!ctx.alive()) return;
      set(s);
    } catch {
      if (!ctx.alive()) return;
      // the last status stays; the next read tries again
    }
    schedule();
  }

  /* ---------------------------------------------------------------- the button */
  async function press() {
    if (fr.running(S)) return cancel();
    const r = await askPassword(ctx, {
      title: fr.CONFIRM_TITLE,
      intro: h('p', { class: 'fr-intro' }, fr.confirmText(car?.name || null)),
      submit: start, danger: true,
      run: async (cred) => {
        try {
          return await ctx.api.post(path, fr.body(cred.password, cred.two_step));
        } catch (e) {
          if (!e || e.name === 'AbortError') throw e;
          // the dialog's own words for the password and the code; ours for anything else
          if (e.code === 'password_needed') e.code = 'wrong_password';
          else if (e.code === 'wrong_code') e.code = 'wrong_two_step';
          else e.message = fr.errorText(e);
          if (e.fields) { delete e.fields.confirm_password; delete e.fields.code; if (!Object.keys(e.fields).length) e.fields = null; }
          throw e;
        }
      },
    });
    if (!ctx.alive()) return;
    if (r) { set(r); flash('Full rebuild started. Pages show the old history until the new one is ready.'); }
    refresh();
  }

  async function cancel() {
    const ok = await confirmDialog('Stop the full rebuild? Cars already switched keep their new history; the car under way and the ones after it keep their old one.',
      { title: 'Cancel the full rebuild?', ok: fr.CANCEL, danger: true });
    if (!ok || !ctx.alive()) return;
    btn.disabled = true;
    try {
      set(await ctx.api.send('DELETE', 'system/full-rebuild', {}));
      flash('Full rebuild cancelled. The old history was kept.', { kind: 'info' });
    } catch (e) {
      if (ctx.alive()) flash(e?.code ? fr.errorText(e) : errorWords(e).text, { kind: 'error' });
    } finally { btn.disabled = false; }
    if (ctx.alive()) refresh();
  }

  paint();
  const ready = refresh();
  return { el, ready, status: () => S, repaint: paint };
}
