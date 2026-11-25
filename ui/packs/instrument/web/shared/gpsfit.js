// Sentry Mate shared module: gpsfit.js (Data health's "Find GPS layout").
// Canonical source: ui/shared/gpsfit.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// The car's Screenmate GPS layout fit (docs/ui-api.md: /cars/{car}/gps-layout/fit, admin), in the same words on
// every pack, next to the gps_inputs_disagree notice of Data health: the button, a block of fixed size with the
// progress bar and one step line while it runs, then the best layout of latitude and longitude with their errors
// in metres and how many frames were paired, and Apply (PUT /cars/{car}/gps-layout). No position is ever shown:
// the server answers none.
//
// fitModel(status) -> every slot of the block as text (always strings, so the block draws the same rows in every
// state). gpsFit(api, carId, { onChange }) -> { load(), start(days), apply(), stop(), status() }: reads the status
// again every POLL_MS while a fit runs. gpsFitBlock({ api, car, ... }) -> { el, ready, stop() }: the block itself,
// built once and changed in place (classes gf-*, styled by each pack; its buttons take the pack's classes).
//
// gpsLayoutSection({ api, car, admin, ... }) -> { el, ready, load(), stop() }: the "GPS layout" line Data health
// always shows for a car with SMO (CAN) data (hasCanData): the layout its Screenmate GPS frames are read with now
// ("standard (DBC)" or the car's own, with when it was set; GET /cars/{car}/gps-layout, read by everyone), and for
// admins "Find GPS layout" (the fit block opens under the line, and stays open) and "Reset to standard" while the
// car's own layout is set. layoutModel(answer, when) -> the line's slots as text.

import { formatNumber } from './i18n.js';
import { h, setVars } from './dom.js';

export const BUTTON = 'Find GPS layout';
export const AGAIN = 'Find again';
export const RUNNING = 'Finding…';
export const APPLY = 'Apply';
export const RESET = 'Reset to standard';
export const LABEL = 'GPS layout';
export const STANDARD = 'standard (DBC)';
export const EXPLAIN = 'Works out where this car’s Screenmate GPS frame keeps latitude and longitude, from its stored logs of the last 30 days and the positions its other inputs reported at the same moments. It only reads; the result shows layouts and errors in metres, never a position.';
/** How often the status is read again while a fit runs. */
export const POLL_MS = 1500;
export const DAYS = 30;

const enc = encodeURIComponent;
export const fitPath = (car) => `cars/${enc(car)}/gps-layout/fit`;
export const layoutPath = (car) => `cars/${enc(car)}/gps-layout`;

const PHASES = {
  queued: 'Waiting for another car’s fit',
  positions: 'Reading the other inputs’ positions',
  logs: 'Reading the logs’ GPS frames',
  fitting: 'Trying the layouts',
};
const UNITS = { positions: ['day', 'days'], logs: ['log', 'logs'] };
const INPUTS = {
  'teslamate-local': 'local TeslaMate',
  'tesla-api': 'Tesla API',
  'teslafi-api': 'TeslaFi',
  teslamate: 'TeslaMate import',
  teslafi: 'TeslaFi import',
};

const plural = (n, [one, other]) => (n === 1 ? one : other);
const count = (n, unit) => `${formatNumber(n)} ${plural(n, unit)}`;

/** An error in metres as shown: `4 m`, `1.4 km`, `over 10 km` (null: the server only says it is over 10 km). */
export function metres(m) {
  if (m == null || !Number.isFinite(m)) return 'over 10 km';
  if (m < 1000) return `${formatNumber(Math.round(m))} m`;
  return `${formatNumber(m / 1000, { maximumFractionDigits: 1 })} km`;
}

/** Whether a fit runs. */
export const running = (s) => s?.state === 'running';

/** The PUT body of Apply: the layout found, or null when it is the DBC's own (the car's own layout dropped). */
export function applyBody(s) {
  const r = s?.result;
  return { layout: r?.is_dbc ? null : (r?.layout ?? null) };
}

/**
 * Every slot of the block, as text:
 * { state, running, button, buttonDisabled, pct (0..100), bar (0..1), step, headline, tone ('off'|'wait'|'ok'|'warn'|'error'),
 *   rows: [{ label, layout, median, p95 }] (latitude and longitude, always two), pairs, notes, canApply, applied,
 *   applyTitle }
 */
export function fitModel(s) {
  const state = s?.state || 'idle';
  const on = state === 'running';
  const out = {
    state, running: on,
    button: on ? RUNNING : (state === 'idle' ? BUTTON : AGAIN),
    buttonDisabled: on,
    pct: 0, bar: 0, step: '', headline: '', tone: 'off',
    rows: [{ label: 'Latitude', layout: '', median: '', p95: '' }, { label: 'Longitude', layout: '', median: '', p95: '' }],
    pairs: '', notes: '', canApply: false, applied: false, applyTitle: '',
  };
  if (state === 'idle') {
    out.headline = 'Not run yet.';
    return out;
  }
  out.pct = Math.max(0, Math.min(100, Math.round(s.percent ?? 0)));
  out.bar = out.pct / 100;
  if (on) {
    out.tone = 'wait';
    const unit = UNITS[s.phase];
    const what = PHASES[s.phase] || 'Starting';
    out.step = unit && s.total ? `${what}: ${formatNumber(s.done ?? 0)} of ${count(s.total, unit)}` : what;
    out.headline = `Finding the layout (${out.pct} %)`;
    if (s.server_busy) out.notes = `The server is also busy: ${s.server_busy}.`;
    // (a rebuild runs: the fit waits, its progress kept, and goes on once it ends)
    if (s.waiting) {
      out.headline = `Waiting (${out.pct} %)`;
      out.notes = `Waiting: ${s.waiting}.`;
    }
    return out;
  }
  if (state === 'failed') {
    out.tone = 'error';
    out.headline = `It failed: ${s.error || 'no reason given'}. Try again.`;
    return out;
  }
  const r = s.result || {};
  const from = (r.sources || []).filter((x) => x.positions > 0).map((x) => `${INPUTS[x.input] || x.input} ${formatNumber(x.positions)}`);
  out.pairs = `${count(r.pairs ?? 0, ['frame', 'frames'])} from ${count(r.logs ?? 0, ['log', 'logs'])} paired with positions (${from.length ? from.join(', ') : 'none'}) over ${count(r.days ?? DAYS, ['day', 'days'])}`;
  const notes = [];
  if (r.days_skipped) notes.push(`${count(r.days_skipped, ['day', 'days'])} of positions left out: the server changed them while they were read.`);
  if (r.logs_skipped) notes.push(`${count(r.logs_skipped, ['log', 'logs'])} left out: they would not read.`);
  if (!r.enough) {
    out.tone = 'warn';
    out.headline = 'Too few pairs to tell layouts apart: let the car drive with the Screenmate and another input on for a day, then try again.';
    out.notes = notes.join(' ');
    return out;
  }
  [r.lat, r.lon].forEach((a, i) => {
    const b = a?.best;
    out.rows[i].layout = b?.layout || 'none within 5 km';
    out.rows[i].median = b ? metres(b.median_m) : '';
    out.rows[i].p95 = b ? metres(b.p95_m) : '';
  });
  if (r.weak) notes.push('The car moved little in these days, so the fit is weak.');
  out.notes = notes.join(' ');
  if (!r.layout) {
    out.tone = 'warn';
    out.headline = 'No layout comes within 5 km of the other inputs’ positions.';
    return out;
  }
  const err = `${metres(r.median_m)} median, ${metres(r.p95_m)} at the 95th percentile`;
  out.applied = !!s.applied;
  if (r.is_dbc) {
    out.tone = r.good ? 'ok' : 'warn';
    out.headline = `The DBC’s own layout fits: ${err}.`;
  } else {
    out.tone = r.good ? 'ok' : 'warn';
    out.headline = r.good ? `Found a layout that fits: ${err}.` : `Best layout found, but it is far off: ${err}. A layout that fits is off by tens of metres.`;
  }
  out.canApply = !out.applied;
  out.applyTitle = out.applied
    ? 'This car’s frames are read with it already.'
    : 'Read this car’s Screenmate GPS frames with this layout. Its history is rebuilt from the stored logs.';
  return out;
}

/** The fit of one car, read from the server: load(), start(days), apply(), stop(), status(). */
export function gpsFit(api, car, { onChange = () => {} } = {}) {
  let S = null;
  let timer = null;
  let alive = true;
  const set = (s) => { S = s; if (alive) onChange(s); };
  function schedule() {
    clearTimeout(timer);
    if (alive && running(S)) timer = setTimeout(() => { load().catch(() => schedule()); }, POLL_MS);
  }
  async function load() {
    const s = await api.get(fitPath(car));
    set(s);
    schedule();
    return s;
  }
  async function start(days = DAYS) {
    const s = await api.post(fitPath(car), { days });
    set(s);
    schedule();
    return s;
  }
  async function apply() {
    const r = await api.put(layoutPath(car), applyBody(S));
    await load();
    return r;
  }
  function stop() { alive = false; clearTimeout(timer); }
  return { load, start, apply, stop, status: () => S };
}

/**
 * The block, built once and changed in place on every read (its rows keep their room in every state):
 * gpsFitBlock({ api, car, alive, confirm, flash, errorText, onApplied, btnClass, applyClass, icon })
 *   api: the page's API client; car: the car's id; alive(): the page is still shown;
 *   confirm(text, opts) -> Promise<boolean>: the pack's confirm dialog (Apply rebuilds the car's history);
 *   flash(text, { kind }): the pack's toast; errorText(e): an error in words;
 *   onApplied(): after Apply (the page reads its health again); btnClass / applyClass: the buttons' classes;
 *   barClass: the pack's progress bar class (the bar is a div holding an <i> as wide as --w);
 *   icon(name): the pack's icon element for the buttons (or null);
 *   inline: the Find button is left out of el (the caller places `find`, see gpsLayoutSection);
 *   onStatus(status): after every read.
 * -> { el, find, ready (the first read), status(), load(), stop() }
 */
export function gpsFitBlock({ api, car, alive = () => true, confirm = async () => true, flash = () => {},
  errorText = (e) => e?.message || String(e), onApplied = () => {}, btnClass = 'btn btn--sm', applyClass = 'btn btn--sm',
  barClass = '', icon = () => null, inline = false, onStatus = () => {} } = {}) {
  const find = h('button', { class: [btnClass, 'gf-find'], type: 'button', title: inline ? EXPLAIN : null, onclick: () => press() });
  const apply = h('button', { class: [applyClass, 'gf-apply'], type: 'button', onclick: () => applyIt() });
  const bar = h('div', { class: [barClass, 'gf-bar'], role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-label': 'GPS layout fit progress' }, h('i'));
  const pct = h('span', { class: 'gf-pct' });
  const head = h('p', { class: 'gf-head' });
  const step = h('p', { class: 'gf-step' });
  const rows = [0, 1].map(() => {
    const dt = h('dt');
    const layout = h('dd', { class: 'gf-layout' });
    const err = h('dd', { class: 'gf-err' });
    return { dt, layout, err, el: h('div', { class: 'gf-row' }, dt, layout, err) };
  });
  const notes = h('p', { class: 'gf-notes' });
  const el = h('div', { class: 'gf', 'aria-live': 'polite' },
    h('p', { class: 'gf-explain' }, EXPLAIN),
    h('div', { class: 'gf-acts' }, inline ? null : find, apply),
    h('div', { class: 'gf-bar-row' }, bar, pct),
    head, step,
    h('dl', { class: 'gf-rows' }, rows.map((r) => r.el)),
    notes);

  let busy = false;
  function paint(s) {
    const m = fitModel(s);
    el.dataset.state = m.state;
    el.dataset.tone = m.tone;
    find.replaceChildren(...[icon(m.running ? 'clock' : 'search'), m.button].filter(Boolean));
    find.disabled = m.buttonDisabled || busy;
    apply.replaceChildren(...[icon('check'), m.applied ? 'Applied' : APPLY].filter(Boolean));
    apply.disabled = !m.canApply || busy;
    apply.title = m.applyTitle;
    setVars(bar, { '--w': `${(m.bar * 100).toFixed(1)}%` });
    bar.setAttribute('aria-valuenow', String(m.pct));
    pct.textContent = m.state === 'idle' ? '' : `${m.pct} %`;
    head.textContent = m.headline;
    step.textContent = m.running ? m.step : m.pairs;
    step.title = step.textContent;
    m.rows.forEach((r, i) => {
      rows[i].dt.textContent = r.label;
      rows[i].layout.textContent = r.layout;
      rows[i].layout.title = r.layout;
      rows[i].err.textContent = r.median ? `${r.median} median · ${r.p95} 95th pct` : '';
    });
    notes.textContent = m.notes;
  }

  const fit = gpsFit(api, car, { onChange: (s) => { if (alive()) { paint(s); onStatus(s); } } });
  async function press() {
    busy = true;
    paint(fit.status());
    try { await fit.start(); } catch (e) { if (alive()) flash(errorText(e), { kind: 'error' }); }
    busy = false;
    if (alive()) paint(fit.status());
  }
  async function applyIt() {
    const s = fit.status();
    const r = s?.result;
    if (!r?.layout) return;
    const text = r.is_dbc
      ? 'Read this car’s Screenmate GPS frames as the DBC says again? Its history is rebuilt from the stored logs, which can take a while.'
      : `Read this car’s Screenmate GPS frames with ${r.layout}? Its history is rebuilt from the stored logs, which can take a while.`;
    if (!(await confirm(text, { title: 'Apply the GPS layout?', ok: APPLY }))) return;
    busy = true;
    paint(s);
    try {
      await fit.apply();
      if (alive()) { flash('GPS layout applied. The car’s history is being rebuilt with it.'); onApplied(); }
    } catch (e) { if (alive()) flash(errorText(e), { kind: 'error' }); }
    busy = false;
    if (alive()) paint(fit.status());
  }
  paint(null);
  const ready = fit.load().catch(() => {});
  return { el, find, ready, status: () => fit.status(), load: () => fit.load(), stop: () => fit.stop() };
}

/** Whether the car has SMO (CAN) data: an SMO input among its sources, or a CAN Bridge key tied to it. */
export function hasCanData(car, health) {
  const kinds = (car?.sources || []).map((x) => x?.kind);
  return kinds.includes('smo') || (health?.keys || []).length > 0;
}

/**
 * The GPS layout line's slots, from GET /cars/{car}/gps-layout ({ layout, set_ms }; null: not read yet), as text:
 * { custom, value, when, title }. when(ms) -> the pack's words for a moment.
 */
export function layoutModel(a, when = (ms) => new Date(ms).toISOString()) {
  if (!a) return { custom: false, value: '…', when: '', title: '' };
  const custom = typeof a.layout === 'string' && a.layout !== '';
  const at = a.set_ms != null ? when(a.set_ms) : '';
  return {
    custom,
    value: custom ? `custom: ${a.layout}` : STANDARD,
    when: at ? `${custom ? 'set' : 'since'} ${at}` : '',
    title: custom
      ? `This car’s Screenmate GPS frames are read with its own layout: ${a.layout}.`
      : 'This car’s Screenmate GPS frames are read as the DBC says.',
  };
}

/**
 * The always-present "GPS layout" section of Data health, built once and changed in place:
 * gpsLayoutSection({ api, car, admin, when, ...gpsFitBlock's options, resetClass })
 *   admin: the buttons are there (others see the line read-only); when(ms): the pack's words for a moment;
 *   onApplied(): after Apply or Reset (the page reads its health again).
 * -> { el, ready (the first reads), load() (the layout read again), stop() }
 */
export function gpsLayoutSection({ api, car, admin = false, when, alive = () => true, confirm = async () => true,
  flash = () => {}, errorText = (e) => e?.message || String(e), onApplied = () => {}, btnClass = 'btn btn--sm',
  applyClass = 'btn btn--sm', resetClass = btnClass, barClass = '', icon = () => null } = {}) {
  let A = null;
  let open = false;
  let busy = false;
  const val = h('span', { class: 'gl-val' });
  const at = h('span', { class: 'gl-when' });
  const reset = admin ? h('button', { class: [resetClass, 'gl-reset'], type: 'button', onclick: () => resetIt() }) : null;
  const fitHere = h('div', { class: 'gl-fit' });
  async function load() {
    const a = await api.get(layoutPath(car));
    A = a;
    if (alive()) paint();
    return a;
  }
  const block = admin ? gpsFitBlock({
    api, car, alive, confirm, flash, errorText, btnClass, applyClass, barClass, icon, inline: true,
    onApplied: () => { load().catch(() => {}); onApplied(); },
    onStatus: (s) => { if (s && s.state !== 'idle') open = true; paint(); },
  }) : null;
  if (block) fitHere.append(block.el);
  const el = h('div', { class: 'gl', id: 'gps-layout' },
    h('div', { class: 'gl-line' },
      h('span', { class: 'gl-label' }, LABEL),
      // its width follows the line (the packs' pixel snap leaves it alone, so the buttons stay on the line)
      h('span', { class: 'gl-text', 'data-nosnap': '' }, val, at),
      admin ? h('span', { class: 'gl-acts' }, block.find, reset) : null),
    admin ? fitHere : null);

  function paint() {
    const m = layoutModel(A, when);
    el.dataset.custom = m.custom ? 'true' : 'false';
    val.textContent = m.value;
    val.title = m.title;
    at.textContent = m.when ? ` · ${m.when}` : '';
    fitHere.hidden = !open;
    el.dataset.open = open ? 'true' : 'false';
    if (reset) {
      reset.replaceChildren(...[icon('reset'), RESET].filter(Boolean));
      reset.hidden = !m.custom;
      reset.disabled = busy || running(block?.status());
      reset.title = 'Read this car’s Screenmate GPS frames as the DBC says again. Its history is rebuilt from the stored logs.';
    }
  }
  if (block) block.find.addEventListener('click', () => { open = true; paint(); });

  async function resetIt() {
    if (!(await confirm('Read this car’s Screenmate GPS frames as the DBC says again? Its history is rebuilt from the stored logs, which can take a while.',
      { title: 'Reset the GPS layout?', ok: 'Reset' }))) return;
    busy = true;
    paint();
    try {
      A = await api.put(layoutPath(car), { layout: null });
      if (alive()) { flash('GPS layout reset to standard. The car’s history is being rebuilt.'); onApplied(); }
      block?.load().catch(() => {});
    } catch (e) { if (alive()) flash(errorText(e), { kind: 'error' }); }
    busy = false;
    if (alive()) paint();
  }

  paint();
  const ready = Promise.all([load().catch(() => {}), block?.ready]).then(() => {});
  return { el, ready, load, stop: () => block?.stop() };
}
