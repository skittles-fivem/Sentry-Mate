// Sentry Mate, "Pebble" UI pack: the full rebuild tile (the words: shared/fullrebuild.js).
//
// What it does and does not touch, one button (Cancel while one runs; Start now, Tonight and Pause beside it then)
// and a progress well of fixed size: the bar, the eight status rows (the last says why it runs, with a link to the
// change while release-notes links are allowed), three rows for problems and the recent rebuilds, all there from
// the start and their text changed in place,
// so the tile never grows or shrinks; only the texts that changed are written. Follows the `job` events of kind
// full_rebuild (live events, polling only while the live stream is down: every 2 s while one runs, every 30 s
// otherwise). Used by Health (one car: cars/{car}/full-rebuild) and Settings: Server and access (every car:
// system/full-rebuild).
// Styles: app.css (.fr-*).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as fr from '../shared/fullrebuild.js';
import { tile, btn, note } from './ui.js';
import { confirmSheet } from './sheet.js';
import { askPassword, said } from './settings.js';
import { toast } from './toast.js';
import { errorWords } from './states.js';
import { follow, jobOf, setText, setAttr } from '../shared/live.js';

const ERRORS = 3;

/**
 * fullRebuildTile(ctx, { car, carName, after, label, bare, onStatus }) -> { el, ready, status() }
 * car: the car of the Health page (null: every car). carName(id): a car's name (the cars are read when not
 * given). after: more rows at the end of the tile (Settings: the automatic switch). label: the button's words.
 * bare: the content only, for a tile of the page's own (Health: the keys tile). onStatus(s): after every read.
 */
export function fullRebuildTile(ctx, { car = null, carName = null, after = null, label: word = null, bare = false, onStatus = null } = {}) {
  const path = car ? api.apiPath`cars/${car.id}/full-rebuild` : 'system/full-rebuild';
  const label = word || (car ? fr.BUTTON : fr.BUTTON_ALL);
  let names = carName || ((x) => x);
  let s = null, busy = false;

  const staleText = h('span');
  const stale = said('warn', staleText);
  stale.hidden = true;
  const go = btn(label, { kind: 'soft', icon: 'reset', onclick: () => act() });
  const goText = go.querySelector('.btn-text');
  const other = h('p', { class: 'note fr-other' });
  const fill = h('span', { class: 'mv-bar-fill' });
  const bar = h('div', { class: 'mv-bar', role: 'progressbar', 'aria-label': 'Full rebuild', 'aria-valuemin': '0', 'aria-valuemax': '100' }, fill);
  const pct = h('span', { class: 'fr-pct' });
  // the why row: its words and the change explained (a link, only while release-notes links are allowed)
  const whyText = h('span', { class: 'fr-why' });
  const whyA = h('a', { class: 'link fr-why-link', target: '_blank', rel: 'noopener noreferrer', hidden: true }, fr.WHY_LINK);
  let linksOn = false;
  const rows = fr.lines(null).map((l) => {
    const k = h('span', { class: 'fr-k' }, l.label);
    const v = l.key === 'why' ? h('span', { class: 'fr-v' }, whyText, ' ', whyA) : h('span', { class: 'fr-v' });
    return { key: l.key, k, v, el: h('div', { class: 'fr-row' }, k, v) };
  });
  const errs = Array.from({ length: ERRORS }, () => h('p', { class: 'fr-err' }));
  const hist = Array.from({ length: fr.HISTORY_ROWS }, () => h('p', { class: 'fr-err' }));
  const whenBtns = fr.WHEN.map((w) => { const b = btn(w.label, { kind: 'soft', onclick: () => defer(w.when) }); b.hidden = true; b.title = w.title; return b; });
  const well = h('div', { class: 'fr-well', role: 'status', 'aria-label': 'Full rebuild progress' },
    h('div', { class: 'fr-bar' }, bar, pct), h('div', { class: 'fr-rows' }, rows.map((r) => r.el)), h('div', { class: 'fr-errs' }, errs),
    h('div', { class: 'fr-errs fr-hist' }, h('p', { class: 'fr-k' }, fr.HISTORY_TITLE), hist));
  const parts = [fr.EXPLAIN.map((p) => note(p)), stale, h('div', { class: 'fr-go' }, go, whenBtns, other), well, after];
  const el = bare ? h('div', { class: 'fr-tile fr-bare', id: 'full-rebuild' }, parts)
    : tile({ w: 2, title: 'Full rebuild', icon: 'reset', id: 'full-rebuild', cls: 'fr-tile' }, parts);

  let shownStale = null;
  function paint() {
    const now = api.now();
    const run = fr.running(s);
    const tone = fr.tone(s, now);
    setText(goText, run ? fr.CANCEL : label);
    setAttr(go, 'class', ['btn', run ? 'btn-danger' : 'btn-soft'].join(' '));
    if (go.disabled !== busy) go.disabled = busy;
    // a full rebuild of other cars shows here too, and can be cancelled here
    setText(other, run && car && s && !s.this_car ? 'Running for other cars; this car keeps its history.' : '');
    const list = s?.version?.stale_cars || [];
    const st = car ? fr.staleText(s) : list.length ? `Made with an older way of decoding the logs: ${list.map(names).join(', ')}.` : '';
    if (shownStale !== !!st) { shownStale = !!st; stale.hidden = !st; }
    setText(staleText, st);
    // the bar's fill is the same percent as the figure beside it and the rows' counts
    const p = fr.percent(s);
    const w = String(fr.fraction(s));
    if (fill.style.getPropertyValue('--w') !== w) fill.style.setProperty('--w', w);
    setAttr(bar, 'aria-valuenow', p == null ? null : String(p));
    setText(pct, p == null ? '' : `${p} %`);
    setAttr(well, 'class', `fr-well is-${tone}`);
    fr.WHEN.forEach((w, i) => { const show = fr.whenShown(s, w.when); if (whenBtns[i].hidden !== !show) whenBtns[i].hidden = !show; if (whenBtns[i].disabled !== busy) whenBtns[i].disabled = busy; });
    for (const [i, l] of fr.lines(s, now, names).entries()) {
      setText(rows[i].k, l.label);
      if (l.key === 'why') {
        setText(whyText, l.text);
        const href = linksOn ? l.link : '';
        setAttr(whyA, 'href', href || null);
        if (whyA.hidden !== !href) whyA.hidden = !href;
      } else setText(rows[i].v, l.text);
      setAttr(rows[i].v, 'title', l.text);
    }
    const lines = fr.errorLines(s, ERRORS, names);
    if (!lines.length && s?.error) lines.push(s.error);
    for (const [i, e] of errs.entries()) { setText(e, lines[i] || ''); setAttr(e, 'title', lines[i] || ''); }
    for (const [i, l] of fr.historyLines(s, now, names).entries()) { setText(hist[i], l.text); setAttr(hist[i], 'title', l.title); }
  }

  // GET once at the start and after a reconnect, then the `job` events (a car's own read adds this_car and
  // car_stale, which an event does not carry: they stay from the last read)
  const shown = (next) => {
    s = next;
    paint();
    onStatus?.(s, fr.lines(s, api.now(), names));
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
      title: fr.CONFIRM_TITLE, text: fr.confirmText(car?.name || null), yes: label, danger: true,
      hint: 'It is asked again because this rebuilds the history.',
      run: (creds) => ctx.api.post(path, fr.body(creds.password, creds.two_step)).catch((e) => {
        if (e?.code === 'password_needed' || e?.code === 'wrong_password') throw Object.assign(e, { code: 'wrong_password' });
        if (e?.code === 'wrong_code') throw Object.assign(e, { code: 'wrong_two_step' });
        if (e?.status === 429) throw e;
        throw new Error(fr.errorText(e));   // shown in the sheet
      }),
    });
    if (!r || !ctx.alive()) return;
    shown(r);
    toast('Full rebuild started. Pages show the old history until it is done.');
    read();
  }

  async function cancel() {
    if (!await confirmSheet({ title: 'Cancel the full rebuild?', text: 'It stops now. Cars not switched yet keep their old history; nothing is lost.', yes: fr.CANCEL, no: 'Keep it running', danger: true })) return;
    if (!ctx.alive()) return;
    busy = true; paint();
    try { s = await ctx.api.send('DELETE', 'system/full-rebuild', {}); } catch (e) { if (ctx.alive() && !api.isAbort(e)) toast(e?.code ? fr.errorText(e) : errorWords(e).text, { kind: 'error' }); }
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
    } catch (e) { if (ctx.alive() && !api.isAbort(e)) toast(e?.code ? fr.errorText(e) : errorWords(e).text, { kind: 'error' }); }
    busy = false;
    if (!ctx.alive()) return;
    paint();
    read();
  }

  async function start() {
    // whether the why row may link to the change (the owner's choice about outside links; off when unknown)
    ctx.api.get('settings', null, { maxAgeMs: 300000 }).then((x) => { linksOn = !!x?.outside_links?.release_notes; if (ctx.alive()) paint(); }).catch(() => {});
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
