// The full rebuild block (shared/fullrebuild.js has the words): what it does, the button (cancel while one runs)
// and a progress box of fixed size: a text bar, the seven status lines and three error lines, all there from the
// start, their text changed in place. Read again every 2 s while one runs, every 30 s otherwise.
// Used by health (one car: cars/{car}/full-rebuild) and settings/server (every car: system/full-rebuild).

import { h } from '../shared/dom.js';
import * as fr from '../shared/fullrebuild.js';
import { btn, note, textBar } from './ui.js';
import { tag } from './fmt.js';
import { confirmDialog } from './dialog.js';
import { askPassword } from './secret.js';
import { toast } from './toast.js';
import { errorWords } from './states.js';

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
  let s = null, timer = 0, busy = false;
  ctx.onCleanup(() => clearTimeout(timer));

  const stale = h('p', { class: 'note fr-stale', hidden: true }, tag('stale', 'warn'), ' ', h('span'));
  const go = btn(word, () => act());
  const said = h('span', { class: 'muted fr-said' });
  const bar = h('span', { class: 'fr-bar' });
  const pct = h('span', { class: 'fr-pct' });
  const rows = fr.lines(null).map((l) => {
    const k = h('span', { class: 'fr-k' }, low(l.label));
    const v = h('span', { class: 'fr-v' });
    return { key: l.key, k, v, el: h('div', { class: 'fr-line' }, k, v) };
  });
  const errs = Array.from({ length: ERRORS }, () => h('div', { class: 'fr-err' }));
  const box = h('div', { class: 'mono-box fr-box', role: 'status', 'aria-label': 'Full rebuild progress' },
    h('div', { class: 'fr-line fr-line--bar' }, bar, pct), rows.map((r) => r.el), h('div', { class: 'fr-errs' }, errs));
  const el = h('div', { class: 'fr' },
    fr.EXPLAIN.map((p) => note(p)), stale,
    h('div', { class: 'acts fr-acts' }, go, said), box);

  function paint() {
    const now = ctx.api.now();
    const tone = fr.tone(s, now);
    const run = fr.running(s);
    go.textContent = run ? low(fr.CANCEL) : word;
    go.className = ['btn', run && 'btn--danger'].filter(Boolean).join(' ');
    go.disabled = busy;
    // a full rebuild of other cars: this car's page shows it too, and can cancel it
    said.textContent = run && car && s && !s.this_car ? 'running for other cars; this car keeps its history' : '';
    // the car made with an older decoding (health), or every such car by name (settings/server)
    const list = s?.version?.stale_cars || [];
    const st = car ? fr.staleText(s) : list.length ? `made with an older way of decoding the logs: ${list.map(names || ((x) => x)).join(', ')}.` : '';
    stale.hidden = !st;
    stale.lastChild.textContent = st;
    const p = fr.percent(s);
    bar.replaceChildren(textBar(fr.fraction(s) * 100, { tone: BAR[tone], label: p == null ? 'no full rebuild under way' : `${p} percent` }));
    pct.textContent = p == null ? '' : ` ${String(p).padStart(3)} %`;
    for (const [i, l] of fr.lines(s, now, names || ((x) => x)).entries()) {
      const r = rows[i];
      r.k.textContent = low(l.label);
      r.v.textContent = l.text;
      r.v.title = l.text;
      if (l.key === 'state') r.v.className = `fr-v tag--${TONE[tone]}`;
    }
    const lines = fr.errorLines(s, ERRORS, names || ((x) => x));
    if (!lines.length && s?.error) lines.push(s.error);
    for (const [i, e] of errs.entries()) {
      const t = lines[i] || '';
      e.textContent = t;
      e.title = t;
    }
  }

  async function read() {
    clearTimeout(timer);
    try {
      const next = await ctx.api.get(path);
      if (!ctx.alive()) return;
      s = next;
      paint();
      onStatus?.(s, fr.lines(s, ctx.api.now(), names || ((x) => x)));
    } catch (e) { if (!ctx.alive()) return; }
    // while one runs, and once more after it ended
    timer = setTimeout(() => { if (ctx.alive()) read(); }, fr.running(s) ? fr.POLL_MS : fr.IDLE_POLL_MS);
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
    s = r; paint(); onStatus?.(s, fr.lines(s, ctx.api.now(), names || ((x) => x)));
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

  async function start() {
    if (!names) {
      try {
        const cars = await ctx.api.list('cars', { limit: 500 }).all({ max: 5000 });
        const m = new Map(cars.map((c) => [c.id, c.name]));
        names = (id) => m.get(id) || id;
      } catch { names = (id) => id; }
    }
    paint();
    await read();
  }
  paint();
  return { el, ready: start(), status: () => s };
}
