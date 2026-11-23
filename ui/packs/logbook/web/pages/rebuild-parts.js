// Sentry Mate, "Logbook" UI pack: the full rebuild block, shared by Data health (one car:
// cars/{car}/full-rebuild) and Settings: Server and access (every car: system/full-rebuild). The words
// are shared/fullrebuild.js: what it does and does not touch, one button (Cancel while one runs) and a
// progress panel of fixed size: the bar, the seven status lines and three lines for problems, all there
// from the start, only the texts that changed written again, so the block never grows or shrinks.
// Follows the `job` events of kind full_rebuild (live events, polling only while the live stream is
// down: every 2 s while one runs, every 30 s otherwise). Styles: app.css (.fr-*).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as fr from '../shared/fullrebuild.js';
import { button, icon, note, errorWords } from '../core/ui.js';
import { confirmDialog } from '../core/dialog.js';
import { askPassword } from './settings-move.js';
import { follow, jobOf, setText, setAttr } from '../shared/live.js';

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
  let s = null, busy = false;

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

  let shownStale = null;
  function paint() {
    const now = api.now();
    const run = fr.running(s);
    setText(goText, run ? fr.CANCEL : label);
    if (go.classList.contains('button-danger') !== run) { go.classList.toggle('button-danger', run); go.classList.toggle('button-quiet', !run); }
    if (go.disabled !== busy) go.disabled = busy;
    // a full rebuild of other cars shows here too, and can be cancelled here
    setText(other, run && car && s && !s.this_car ? 'Running for other cars; this car keeps its history.' : '');
    const list = s?.version?.stale_cars || [];
    const st = car ? fr.staleText(s) : list.length ? `Made with an older way of decoding the logs: ${list.map(names).join(', ')}.` : '';
    if (shownStale !== !!st) { shownStale = !!st; stale.hidden = !st; }
    setText(staleText, st);
    // the bar's fill is the same percent as the figure beside it and the lines' counts
    const p = fr.percent(s);
    const w = `${(fr.fraction(s) * 100).toFixed(1)}%`;
    if (fill.style.getPropertyValue('--w') !== w) fill.style.setProperty('--w', w);
    setAttr(bar, 'aria-valuenow', p == null ? null : String(p));
    setText(pct, p == null ? '' : `${p} %`);
    setAttr(panel, 'class', `fr-panel is-${fr.tone(s, now)}`);
    for (const [i, l] of fr.lines(s, now, names).entries()) {
      setText(rows[i].k, l.label);
      setText(rows[i].v, l.text);
      setAttr(rows[i].v, 'title', l.text);
    }
    const lines = fr.errorLines(s, ERRORS, names);
    if (!lines.length && s?.error) lines.push(s.error);
    for (const [i, e] of errs.entries()) { setText(e, lines[i] || ''); setAttr(e, 'title', lines[i] || ''); }
  }

  // GET once at the start and after a reconnect, then the `job` events (a car's own read adds this_car and
  // car_stale, which an event does not carry: they stay from the last read)
  const shown = (next) => {
    s = next;
    paint();
    if (onStatus) onStatus(s, fr.lines(s, api.now(), names));
  };
  let live = null;
  const read = () => live?.refresh();
  function follows() {
    let first;
    const ready = new Promise((res) => { first = res; });
    live = follow(ctx.api, {
      type: 'job', match: jobOf('full_rebuild'), pick: (d) => d.status,
      load: () => ctx.api.get(path),
      pollMs: (v) => (fr.running(v) ? fr.POLL_MS : fr.IDLE_POLL_MS),
      onValue: (v, how) => {
        if (!ctx.alive()) return;
        const before = s;
        shown(how === 'event' && car && v ? { ...v, this_car: before?.this_car ?? false, car_stale: before?.car_stale ?? false } : v);
        first();
        // a run that started or ended changes this car's part in it: its own status is read once
        if (car && how === 'event' && fr.running(v) !== fr.running(before)) live.refresh();
      },
      onError: () => first(),   // the last status stays; the next read tries again
    });
    // the time since the last progress (and the stall warning) follow the clock between events
    const tick = setInterval(() => { if (ctx.alive() && fr.running(s)) paint(); }, 10000);
    ctx.onCleanup(() => clearInterval(tick));
    return ready;
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
    shown(r);
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
    await follows();
  }
  paint();
  return { el, ready: start(), status: () => s };
}
