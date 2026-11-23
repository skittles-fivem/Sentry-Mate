// The full rebuild block (shared/fullrebuild.js has the words): what it does, the button (cancel while one runs)
// (start now, tonight and pause beside it while one runs) and a progress box of fixed size: a text bar, the eight
// status lines (the last says why it runs, with a link to the change while release-notes links are allowed), three
// error lines and the recent rebuilds, all there from the start, only the texts that changed written again. Follows the `job` events of kind full_rebuild (live events,
// polling only while the live stream is down: every 2 s while one runs, every 30 s otherwise).
// Used by health (one car: cars/{car}/full-rebuild) and settings/server (every car: system/full-rebuild).

import { h } from '../shared/dom.js';
import * as fr from '../shared/fullrebuild.js';
import { btn, note, textBar } from './ui.js';
import { tag } from './fmt.js';
import { confirmDialog } from './dialog.js';
import { askPassword } from './secret.js';
import { toast } from './toast.js';
import { errorWords } from './states.js';
import { follow, jobOf, setText, setAttr } from '../shared/live.js';

const ERRORS = 3;
const TONE = { off: 'muted', wait: 'accent', ok: 'good', warn: 'warn', error: 'bad' };
const BAR = { off: 'text', wait: 'accent', ok: 'accent', warn: 'warn', error: 'bad' };
const low = (t) => t.toLowerCase();

/**
 * fullRebuild(ctx, { car, carName, label, onStatus }) -> { el, ready, status() }
 * car: the car of the health page (null for every car). carName(id): a car's name (the cars list is read when
 * not given). label: the button's words (health: 'rebuild all data'). onStatus(s): after every read.
 */
export function fullRebuild(ctx, { car = null, carName = null, label = null, onStatus = null } = {}) {
  const word = label || low(car ? fr.BUTTON : fr.BUTTON_ALL);
  const path = car ? `cars/${encodeURIComponent(car.id)}/full-rebuild` : 'system/full-rebuild';
  let names = carName;
  let s = null, busy = false;

  const stale = h('p', { class: 'note fr-stale', hidden: true }, tag('stale', 'warn'), ' ', h('span'));
  const go = btn(word, () => act());
  const said = h('span', { class: 'muted fr-said' });
  // the text bar built once: its cells and tone change in place, its percent is the lines' (shared/fullrebuild.js)
  const tbar = textBar(0, { label: 'no full rebuild under way' });
  tbar.removeAttribute('role');
  tbar.removeAttribute('aria-label');
  tbar.setAttribute('aria-hidden', 'true');
  const [barOn, barOff] = tbar.children;
  const bar = h('span', { class: 'fr-bar', role: 'progressbar', 'aria-label': 'Full rebuild progress', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': '0' }, tbar);
  const CELLS = barOn.textContent.length + barOff.textContent.length;
  const pct = h('span', { class: 'fr-pct' });
  // the why line: its words and the change explained (a link, only while release-notes links are allowed)
  const whyText = h('span', { class: 'fr-why' });
  const whyA = h('a', { class: 'link fr-why-link', target: '_blank', rel: 'noopener noreferrer', hidden: true }, low(fr.WHY_LINK));
  let linksOn = false;
  const rows = fr.lines(null).map((l) => {
    const k = h('span', { class: 'fr-k' }, low(l.label));
    const v = l.key === 'why' ? h('span', { class: 'fr-v' }, whyText, ' ', whyA) : h('span', { class: 'fr-v' });
    return { key: l.key, k, v, el: h('div', { class: 'fr-line' }, k, v) };
  });
  const errs = Array.from({ length: ERRORS }, () => h('div', { class: 'fr-err' }));
  const hist = Array.from({ length: fr.HISTORY_ROWS }, () => h('div', { class: 'fr-err' }));
  const whenBtns = fr.WHEN.map((w) => { const b = btn(low(w.label), () => defer(w.when)); b.hidden = true; b.title = w.title; return b; });
  const box = h('div', { class: 'mono-box fr-box', role: 'status', 'aria-label': 'Full rebuild progress' },
    h('div', { class: 'fr-line fr-line--bar' }, bar, pct), rows.map((r) => r.el), h('div', { class: 'fr-errs' }, errs),
    h('div', { class: 'fr-errs fr-hist' }, h('div', { class: 'fr-line' }, h('span', { class: 'fr-k' }, low(fr.HISTORY_TITLE))), hist));
  const el = h('div', { class: 'fr' },
    fr.EXPLAIN.map((p) => note(p)), stale,
    h('div', { class: 'acts fr-acts' }, go, whenBtns, said), box);

  let shownStale = null;
  function paint() {
    const now = ctx.api.now();
    const tone = fr.tone(s, now);
    const run = fr.running(s);
    setText(go, run ? low(fr.CANCEL) : word);
    setAttr(go, 'class', ['btn', run && 'btn--danger'].filter(Boolean).join(' '));
    if (go.disabled !== busy) go.disabled = busy;
    // a full rebuild of other cars: this car's page shows it too, and can cancel it
    setText(said, run && car && s && !s.this_car ? 'running for other cars; this car keeps its history' : '');
    // the car made with an older decoding (health), or every such car by name (settings/server)
    const list = s?.version?.stale_cars || [];
    const st = car ? fr.staleText(s) : list.length ? `made with an older way of decoding the logs: ${list.map(names || ((x) => x)).join(', ')}.` : '';
    if (shownStale !== !!st) { shownStale = !!st; stale.hidden = !st; }
    setText(stale.lastChild, st);
    const p = fr.percent(s);
    const on = Math.round(fr.fraction(s) * CELLS);
    setText(barOn, '#'.repeat(on));
    setText(barOff, (p == null ? '?' : '.').repeat(CELLS - on));
    setAttr(tbar, 'class', `tbar tbar--${BAR[tone]}`);
    setAttr(bar, 'aria-valuenow', String(p ?? 0));
    setAttr(bar, 'aria-valuetext', p == null ? 'no full rebuild under way' : `${p} percent`);
    setText(pct, p == null ? '' : ` ${String(p).padStart(3)} %`);
    fr.WHEN.forEach((w, i) => { const show = fr.whenShown(s, w.when); if (whenBtns[i].hidden !== !show) whenBtns[i].hidden = !show; if (whenBtns[i].disabled !== busy) whenBtns[i].disabled = busy; });
    for (const [i, l] of fr.lines(s, now, names || ((x) => x)).entries()) {
      const r = rows[i];
      setText(r.k, low(l.label));
      if (l.key === 'why') {
        setText(whyText, l.text);
        const href = linksOn ? l.link : '';
        setAttr(whyA, 'href', href || null);
        if (whyA.hidden !== !href) whyA.hidden = !href;
      } else setText(r.v, l.text);
      setAttr(r.v, 'title', l.text);
      if (l.key === 'state') setAttr(r.v, 'class', `fr-v tag--${TONE[tone]}`);
    }
    const lines = fr.errorLines(s, ERRORS, names || ((x) => x));
    if (!lines.length && s?.error) lines.push(s.error);
    for (const [i, e] of errs.entries()) {
      const t = lines[i] || '';
      setText(e, t);
      setAttr(e, 'title', t);
    }
    for (const [i, l] of fr.historyLines(s, now, names || ((x) => x)).entries()) {
      setText(hist[i], l.text);
      setAttr(hist[i], 'title', l.title);
    }
  }

  // GET once at the start and after a reconnect, then the `job` events (a car's own read adds this_car and
  // car_stale, which an event does not carry: they stay from the last read)
  const shown = (next) => {
    s = next;
    paint();
    onStatus?.(s, fr.lines(s, ctx.api.now(), names || ((x) => x)));
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
    ctx.every(10000, () => { if (fr.running(s)) paint(); });
    return ready;
  }

  async function act() {
    if (busy) return;
    if (fr.running(s)) { await cancel(); return; }
    const r = await askPassword(ctx, {
      title: low(fr.CONFIRM_TITLE), submit: word, danger: true,
      intro: note(fr.confirmText(car?.name || null)),
      run: (cred) => ctx.api.post(path, fr.body(cred.password, cred.two_step)).catch((e) => {
        if (e.code === 'password_needed' || e.code === 'wrong_password') { e.code = 'wrong_password'; throw e; }
        if (e.code === 'wrong_code') { e.code = 'wrong_two_step'; throw e; }
        if (e.status === 429) throw e;
        throw new Error(fr.errorText(e));   // shown in the dialog's message line
      }),
    });
    if (!r || !ctx.alive()) return;
    shown(r);
    toast('Full rebuild started. Pages show the old history until it is done.');
    read();
  }

  async function cancel() {
    if (!(await confirmDialog('The full rebuild stops now. Cars not switched yet keep their old history; nothing is lost.', { title: 'cancel the full rebuild?', ok: 'cancel it', danger: true }))) return;
    if (!ctx.alive()) return;
    busy = true; paint();
    try { s = await ctx.api.send('DELETE', 'system/full-rebuild', {}); } catch (e) { if (ctx.alive()) toast(e.code ? fr.errorText(e) : errorWords(e).text, { kind: 'error' }); }
    busy = false;
    if (!ctx.alive()) return;
    paint();
    read();
  }

  // start now, put off until tonight, pause (the one under way, whatever started it)
  async function defer(when) {
    if (busy) return;
    busy = true; paint();
    try {
      const v = await ctx.api.send('PATCH', 'system/full-rebuild', fr.whenBody(when));
      s = car ? { ...v, this_car: s?.this_car ?? false, car_stale: s?.car_stale ?? false } : v;
      toast(fr.whenDone(when));
    } catch (e) { if (ctx.alive()) toast(e.code ? fr.errorText(e) : errorWords(e).text, { kind: 'error' }); }
    busy = false;
    if (!ctx.alive()) return;
    paint();
    read();
  }

  async function start() {
    // whether the why line may link to the change (the owner's choice about outside links; off when unknown)
    ctx.api.get('settings', null, { maxAgeMs: 300000 }).then((x) => { linksOn = !!x?.outside_links?.release_notes; if (ctx.alive()) paint(); }).catch(() => {});
    if (!names) {
      try {
        const cars = await ctx.api.list('cars', { limit: 500 }).all({ max: 5000 });
        const m = new Map(cars.map((c) => [c.id, c.name]));
        names = (id) => m.get(id) || id;
      } catch { names = (id) => id; }
    }
    paint();
    await follows();
  }
  paint();
  return { el, ready: start(), status: () => s };
}
