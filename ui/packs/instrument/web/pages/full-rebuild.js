// The full rebuild block (admin only; used by pages/health.js for one car and by pages/settings-server.js for
// every car): what it does and does not touch, the button (Full rebuild, or Cancel while one runs) and a progress
// panel of fixed size: the bar, the eight lines of shared/fullrebuild.js (the last says why it runs, with a link to
// the change while release-notes links are allowed), three lines for problems and the recent rebuilds; beside the
// button, Start now, Tonight and Pause while one runs. Built once and changed in place on every value (only the
// texts that changed are written), so nothing on the page moves.
// API: GET cars/{car}/full-rebuild or system/full-rebuild, then `job` events of kind full_rebuild (live events,
// polling only while the live stream is down: every POLL_MS while one runs, IDLE_POLL_MS otherwise); POST the
// same with the password asked again, PATCH system/full-rebuild { when } to start it now, put it off or pause it,
// DELETE system/full-rebuild to cancel; GET settings once (whether links to release notes are allowed).

import { h, replace, setVars } from '../shared/dom.js';
import * as fr from '../shared/fullrebuild.js';
import { follow, jobOf, setText, setAttr } from '../shared/live.js';
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
  // the why line: its words, and the change explained (a link, only while release-notes links are allowed)
  const whyText = h('span', { class: 'fr-why' });
  const whyA = h('a', { class: 'link fr-why-link', target: '_blank', rel: 'noopener noreferrer', hidden: true }, fr.WHY_LINK);
  let linksOn = false;
  const rows = fr.lines(null).map((l) => {
    const dt = h('dt', null, l.label);
    const dd = l.key === 'why' ? h('dd', null, whyText, ' ', whyA) : h('dd', null);
    return { key: l.key, dt, dd, el: h('div', { class: 'fr-line', 'data-key': l.key }, dt, dd) };
  });
  const whenBtns = fr.WHEN.map((w) => h('button', { class: 'btn btn--sm', type: 'button', hidden: true, title: w.title, onclick: () => defer(w.when) }, w.label));
  const hist = Array.from({ length: fr.HISTORY_ROWS }, () => h('li', null));
  const errs = Array.from({ length: ERRORS_SHOWN }, () => h('li', null));
  const panelEl = h('div', { class: 'fr-panel', 'aria-live': 'polite' },
    h('div', { class: 'fr-bar-row' }, bar, pct),
    h('dl', { class: 'fr-lines' }, rows.map((r) => r.el)),
    h('div', { class: 'fr-errors-box' },
      h('p', { class: 'fr-errors-head' }, 'Problems'),
      h('ul', { class: 'fr-errors' }, errs)),
    h('div', { class: 'fr-errors-box fr-history-box' },
      h('p', { class: 'fr-errors-head' }, fr.HISTORY_TITLE),
      h('ul', { class: 'fr-history' }, hist)));

  const el = h('div', { class: 'fr' },
    h('div', { class: 'fr-about' },
      fr.EXPLAIN.map((t) => h('p', { class: 'fr-explain' }, t)),
      stale,
      h('div', { class: 'row fr-acts' }, btn, whenBtns)),
    panelEl);

  // only what changed is written (a job event can come twice a second): the button only when it flips
  let btnOn = null;
  function paint() {
    const now = ctx.api.now();
    const on = fr.running(S);
    const stopping = fr.cancelling(S);
    if (`${on}${stopping}` !== btnOn) {
      btnOn = `${on}${stopping}`;
      replace(btn, icon(on ? 'x' : 'reset', 'ico--sm'), stopping ? 'Cancelling…' : on ? fr.CANCEL : start);
      btn.disabled = stopping;
      btn.classList.toggle('btn--danger', on);
      setAttr(btn, 'title', on ? 'Stop the full rebuild: cars not switched yet keep their old history' : null);
    }
    const st = car ? fr.staleText(S) : '';
    setText(stale, st);
    if (stale.hidden !== !st) stale.hidden = !st;

    // the bar's fill is the same percent as the figure beside it and the lines' counts
    const p = fr.percent(S);
    const w = `${(fr.fraction(S) * 100).toFixed(1)}%`;
    if (bar.style.getPropertyValue('--w') !== w) setVars(bar, { '--w': w });
    setAttr(bar, 'aria-valuenow', String(p ?? 0));
    const tone = fr.tone(S, now);
    const tb = TONE_BAR[tone];
    for (const c of Object.values(TONE_BAR)) if (c !== tb && bar.classList.contains(c)) bar.classList.remove(c);
    if (tb && !bar.classList.contains(tb)) bar.classList.add(tb);
    setText(pct, p == null ? '' : `${p} %`);

    fr.WHEN.forEach((w, i) => { const show = fr.whenShown(S, w.when); if (whenBtns[i].hidden !== !show) whenBtns[i].hidden = !show; });
    const ls = fr.lines(S, now, carName);
    ls.forEach((l, i) => {
      const r = rows[i];
      setText(r.dt, l.label);
      if (l.key === 'why') {
        setText(whyText, l.text);
        const href = linksOn ? l.link : '';
        setAttr(whyA, 'href', href || null);
        if (whyA.hidden !== !href) whyA.hidden = !href;
      } else setText(r.dd, l.text);
      setAttr(r.dd, 'title', l.text || null);
      setAttr(r.el, 'data-tone', l.key === 'state' ? tone : '');
    });
    fr.historyLines(S, now, carName).forEach((l, i) => { setText(hist[i], l.text); setAttr(hist[i], 'title', l.title || null); });

    const n = Array.isArray(S?.errors) ? S.errors.length : 0;
    const el3 = fr.errorLines(S, n > ERRORS_SHOWN ? ERRORS_SHOWN - 1 : ERRORS_SHOWN, carName);
    if (!n && S && S.state !== 'idle') el3[0] = 'None so far.';
    errs.forEach((li, i) => { setText(li, el3[i] || ''); setAttr(li, 'title', el3[i] || null); });
    setAttr(panelEl, 'data-tone', tone);
  }

  function set(s) {
    const was = fr.running(S);
    S = s;
    paint();
    onStatus?.(s);
    if (was && !fr.running(s)) onEnded?.(s);
  }

  /* ---------------------------------------------------------------- following */
  // `job` events of kind full_rebuild carry the status of GET system/full-rebuild; a car's own read adds this_car
  // and car_stale, which an event does not change: they stay from the last read
  const fromEvent = (s) => (car && s ? { ...s, this_car: S?.this_car ?? false, car_stale: S?.car_stale ?? false } : s);
  let first = null;
  const firstRead = new Promise((res) => { first = res; });
  const live = follow(ctx.api, {
    type: 'job', match: jobOf('full_rebuild'), pick: (d) => d.status,
    load: () => ctx.api.get(path),
    pollMs: (s) => (fr.running(s) ? fr.POLL_MS : fr.IDLE_POLL_MS),
    onValue: (s, how) => {
      if (!ctx.alive()) return;
      const before = S;
      set(how === 'event' ? fromEvent(s) : s);
      first();
      // a run that started or ended changes this car's part in it: the car's own status is read once
      if (car && how === 'event' && (s?.id !== before?.id || fr.running(s) !== fr.running(before))) live.refresh();
    },
    // a failed read keeps the last status; the next one tries again
    onError: () => first(),
  });
  const refresh = () => live.refresh();
  // the time since the last progress (and the stall warning) follow the clock between events, with no read
  ctx.every(10000, () => { if (fr.running(S)) paint(); });

  // whether the why line may link to the change (the owner's choice about outside links; off when unknown)
  ctx.api.get('settings', null, { maxAgeMs: 300000 }).then((x) => {
    linksOn = !!x?.outside_links?.release_notes;
    if (ctx.alive()) paint();
  }).catch(() => {});

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

  // start now, put off until tonight, pause (the one under way, whatever started it)
  async function defer(when) {
    whenBtns.forEach((b) => { b.disabled = true; });
    try {
      set(fromEvent(await ctx.api.send('PATCH', 'system/full-rebuild', fr.whenBody(when))));
      flash(fr.whenDone(when), { kind: 'info' });
    } catch (e) {
      if (ctx.alive()) flash(e?.code ? fr.errorText(e) : errorWords(e).text, { kind: 'error' });
    } finally { whenBtns.forEach((b) => { b.disabled = false; }); }
    if (ctx.alive()) refresh();
  }

  paint();
  const ready = firstRead;
  return { el, ready, status: () => S, repaint: paint };
}
