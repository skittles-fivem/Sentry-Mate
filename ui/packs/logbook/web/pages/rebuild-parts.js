// Sentry Mate, "Logbook" UI pack: the full rebuild block, shared by Data health (one car:
// cars/{car}/full-rebuild) and Settings: Server and access (every car: system/full-rebuild). The words
// are shared/fullrebuild.js: what it does and does not touch, one button (Cancel while one runs) and a
// progress panel of fixed size: the bar, the seven status lines and three lines for problems, all there
// from the start, their text changed in place, so the block never grows or shrinks. Read again every
// 2 s while one runs, every 30 s otherwise. Styles: app.css (.fr-*).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as fr from '../shared/fullrebuild.js';
import { button, icon, note, errorWords } from '../core/ui.js';
import { confirmDialog } from '../core/dialog.js';
import { askPassword } from './settings-move.js';

const ERRORS = 3;

/**
 * fullRebuild(ctx, { car, carName, after, label, onStatus }) -> { el, ready, status() }: the block's
 * content. car: the car of Data health (null: every car). carName(id): a car's name (the cars are read
 * when not given). after: more at the end (Settings: the automatic switch). label: the button's words.
 * onStatus(s): after every read (Data health shows the step on its Rebuild line).
 */
export function fullRebuild(ctx, { car = null, carName = null, after = null, label: word = null, onStatus = null } = {}) {
  const path = car ? api.apiPath`cars/${car.id}/full-rebuild` : 'system/full-rebuild';
  const label = word || (car ? fr.BUTTON : fr.BUTTON_ALL);
  let names = carName || ((x) => x);
  let s = null, timer = 0, busy = false;
  ctx.onCleanup(() => clearTimeout(timer));

  const staleText = h('span');
  const stale = h('p', { class: 'fr-stale', hidden: true }, icon('warning'), staleText);
  const goText = h('span', null, label);
  const go = button('', { iconName: 'reset', cls: 'button-small', onclick: () => act() });
  go.append(goText);
  const other = h('p', { class: 'fr-other' });
  const fill = h('span', { class: 'mv-bar-fill' });
  const bar = h('div', { class: 'mv-bar', role: 'progressbar', 'aria-label': 'Full rebuild', 'aria-valuemin': '0', 'aria-valuemax': '100' }, fill);
  const pct = h('span', { class: 'fr-pct' });
  const rows = fr.lines(null).map((l) => {
    const k = h('dt', { class: 'fr-k' }, l.label);
    const v = h('dd', { class: 'fr-v' });
    return { k, v, el: h('div', { class: 'fr-line' }, k, v) };
  });
  const errs = Array.from({ length: ERRORS }, () => h('p', { class: 'fr-err' }));
  const panel = h('div', { class: 'fr-panel', role: 'status', 'aria-label': 'Full rebuild progress' },
    h('div', { class: 'fr-bar' }, bar, pct), h('dl', { class: 'fr-lines' }, rows.map((r) => r.el)), h('div', { class: 'fr-errs' }, errs));
  const el = h('div', { class: 'fr' },
    h('div', { class: 'fr-text' }, fr.EXPLAIN.map((p) => note(p)), stale),
    h('div', { class: 'fr-go' }, go, other), panel, after);

  function paint() {
    const now = api.now();
    const run = fr.running(s);
    goText.textContent = run ? fr.CANCEL : label;
    go.classList.toggle('button-danger', run);
    go.classList.toggle('button-quiet', !run);
    go.disabled = busy;
    // a full rebuild of other cars shows here too, and can be cancelled here
    other.textContent = run && car && s && !s.this_car ? 'Running for other cars; this car keeps its history.' : '';
    const list = s?.version?.stale_cars || [];
    const st = car ? fr.staleText(s) : list.length ? `Made with an older way of decoding the logs: ${list.map(names).join(', ')}.` : '';
    stale.hidden = !st;
    staleText.textContent = st;
    const p = fr.percent(s);
    fill.style.setProperty('--w', `${(fr.fraction(s) * 100).toFixed(1)}%`);
    if (p == null) bar.removeAttribute('aria-valuenow'); else bar.setAttribute('aria-valuenow', String(p));
    pct.textContent = p == null ? '' : `${p} %`;
    panel.className = `fr-panel is-${fr.tone(s, now)}`;
    for (const [i, l] of fr.lines(s, now, names).entries()) {
      rows[i].k.textContent = l.label;
      rows[i].v.textContent = l.text;
      rows[i].v.title = l.text;
    }
    const lines = fr.errorLines(s, ERRORS, names);
    if (!lines.length && s?.error) lines.push(s.error);
    for (const [i, e] of errs.entries()) { e.textContent = lines[i] || ''; e.title = lines[i] || ''; }
  }

  async function read() {
    clearTimeout(timer);
    try {
      const next = await ctx.api.get(path);
      if (!ctx.alive()) return;
      s = next;
      paint();
      if (onStatus) onStatus(s, fr.lines(s, api.now(), names));
    } catch (e) { if (!ctx.alive() || api.isAbort(e)) return; }
    // while one runs, and once more after it ended
    timer = setTimeout(() => { if (ctx.alive()) read(); }, fr.running(s) ? fr.POLL_MS : fr.IDLE_POLL_MS);
  }

  async function act() {
    if (busy) return;
    if (fr.running(s)) { await cancel(); return; }
    const r = await askPassword(ctx, {
      title: fr.CONFIRM_TITLE, text: fr.confirmText(car ? car.name || car.id : null), yes: label, danger: true,
      hint: 'It is asked again because this makes the history again.',
      run: (creds) => ctx.api.post(path, fr.body(creds.password, creds.two_step)).catch((e) => {
        if (e && (e.code === 'password_needed' || e.code === 'wrong_password')) throw Object.assign(new Error(fr.errorText(e)), { code: 'wrong_password' });
        if (e && e.code === 'wrong_code') throw Object.assign(new Error(fr.errorText(e)), { code: 'wrong_two_step' });
        if (e && e.status === 429) throw e;
        throw new Error(fr.errorText(e));   // shown in the dialog
      }),
    });
    if (!r || !ctx.alive()) return;
    s = r; paint();
    if (onStatus) onStatus(s, fr.lines(s, api.now(), names));
    ctx.flash('Full rebuild started. Pages show the old history until it is done.');
    read();
  }

  async function cancel() {
    const ok = await confirmDialog({ title: 'Cancel the full rebuild?', text: 'It stops now. Cars not switched yet keep their old history; nothing is lost.', yes: fr.CANCEL, no: 'Keep it running', danger: true });
    if (!ok || !ctx.alive()) return;
    busy = true; paint();
    try { s = await ctx.api.send('DELETE', 'system/full-rebuild', {}); } catch (e) {
      if (ctx.alive() && !api.isAbort(e)) ctx.flash(`Not cancelled: ${e && e.code ? fr.errorText(e) : errorWords(e).text}`, { kind: 'error' });
    }
    busy = false;
    if (!ctx.alive()) return;
    paint();
    read();
  }

  async function start() {
    if (!carName) {
      const cars = await ctx.api.list('cars', {}).all({ max: 500 }).catch(() => []);
      const m = new Map(cars.map((c) => [c.id, c.name]));
      names = (id) => m.get(id) || id;
    }
    await read();
  }
  paint();
  return { el, ready: start(), status: () => s };
}
