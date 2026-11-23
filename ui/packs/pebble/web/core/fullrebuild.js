// Sentry Mate, "Pebble" UI pack: the full rebuild tile (the words: shared/fullrebuild.js).
//
// What it does and does not touch, one button (Cancel while one runs) and a progress well of fixed size: the bar,
// the seven status rows and three rows for problems, all there from the start and their text changed in place,
// so the tile never grows or shrinks. Read again every 2 s while one runs, every 30 s otherwise. Used by Health
// (one car: cars/{car}/full-rebuild) and Settings: Server and access (every car: system/full-rebuild).
// Styles: app.css (.fr-*).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as fr from '../shared/fullrebuild.js';
import { tile, btn, note } from './ui.js';
import { confirmSheet } from './sheet.js';
import { askPassword, said } from './settings.js';
import { toast } from './toast.js';
import { errorWords } from './states.js';

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
  let s = null, timer = 0, busy = false;
  ctx.onCleanup(() => clearTimeout(timer));

  const staleText = h('span');
  const stale = said('warn', staleText);
  stale.hidden = true;
  const go = btn(label, { kind: 'soft', icon: 'reset', onclick: () => act() });
  const goText = go.querySelector('.btn-text');
  const other = h('p', { class: 'note fr-other' });
  const fill = h('span', { class: 'mv-bar-fill' });
  const bar = h('div', { class: 'mv-bar', role: 'progressbar', 'aria-label': 'Full rebuild', 'aria-valuemin': '0', 'aria-valuemax': '100' }, fill);
  const pct = h('span', { class: 'fr-pct' });
  const rows = fr.lines(null).map((l) => {
    const k = h('span', { class: 'fr-k' }, l.label);
    const v = h('span', { class: 'fr-v' });
    return { k, v, el: h('div', { class: 'fr-row' }, k, v) };
  });
  const errs = Array.from({ length: ERRORS }, () => h('p', { class: 'fr-err' }));
  const well = h('div', { class: 'fr-well', role: 'status', 'aria-label': 'Full rebuild progress' },
    h('div', { class: 'fr-bar' }, bar, pct), h('div', { class: 'fr-rows' }, rows.map((r) => r.el)), h('div', { class: 'fr-errs' }, errs));
  const parts = [fr.EXPLAIN.map((p) => note(p)), stale, h('div', { class: 'fr-go' }, go, other), well, after];
  const el = bare ? h('div', { class: 'fr-tile fr-bare', id: 'full-rebuild' }, parts)
    : tile({ w: 2, title: 'Full rebuild', icon: 'reset', id: 'full-rebuild', cls: 'fr-tile' }, parts);

  function paint() {
    const now = api.now();
    const run = fr.running(s);
    const tone = fr.tone(s, now);
    goText.textContent = run ? fr.CANCEL : label;
    go.className = ['btn', run ? 'btn-danger' : 'btn-soft'].join(' ');
    go.disabled = busy;
    // a full rebuild of other cars shows here too, and can be cancelled here
    other.textContent = run && car && s && !s.this_car ? 'Running for other cars; this car keeps its history.' : '';
    const list = s?.version?.stale_cars || [];
    const st = car ? fr.staleText(s) : list.length ? `Made with an older way of decoding the logs: ${list.map(names).join(', ')}.` : '';
    stale.hidden = !st;
    staleText.textContent = st;
    const p = fr.percent(s);
    fill.style.setProperty('--w', String(fr.fraction(s)));
    if (p == null) bar.removeAttribute('aria-valuenow'); else bar.setAttribute('aria-valuenow', String(p));
    pct.textContent = p == null ? '' : `${p} %`;
    well.className = `fr-well is-${tone}`;
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
      onStatus?.(s, fr.lines(s, api.now(), names));
    } catch (e) { if (!ctx.alive() || api.isAbort(e)) return; }
    // while one runs, and once more after it ended
    timer = setTimeout(() => { if (ctx.alive()) read(); }, fr.running(s) ? fr.POLL_MS : fr.IDLE_POLL_MS);
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
    s = r; paint(); onStatus?.(s, fr.lines(s, api.now(), names));
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
