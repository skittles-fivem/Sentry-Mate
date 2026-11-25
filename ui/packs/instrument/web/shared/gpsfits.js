// Sentry Mate shared module: gpsfits.js (Data health's "Past fits" of the GPS layout).
// Canonical source: ui/shared/gpsfits.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// Every GPS layout fit the server ran for a car is kept (docs/ui-api.md: GET /cars/{car}/gps-layout/fits, and
// GET, DELETE .../fits/{id}, POST .../fits/{id}/apply with the password asked again, GET .../changes): a run goes
// on in the server when the page is left, and comes back here as a row, running or done. Each row says when it ran,
// the days, logs and pairs, the best layout with its errors in metres, good or weak, and whether it was applied;
// it opens to the same view as the fit block shows, and a past result can be applied or removed. The layouts the
// car was read with over time are listed below (when, by whom, and the layout before). No position, time of day or
// place is ever shown: the server answers none.
//
// rowModel(row, when) and changeModel(change, when) -> the slots of a row as text. gpsFitHistory({ api, car, ... })
// -> { el, ready, load(), stop() }: the block, built once and changed in place (classes gp-*, styled by each pack).

import { formatNumber } from './i18n.js';
import { h } from './dom.js';

export const TITLE = 'Past fits';
export const CHANGES_TITLE = 'Layouts used over time';
export const EMPTY = 'No fit has run yet.';
export const CHANGES_EMPTY = 'The layout has not been changed here yet.';
const STANDARD = 'standard (DBC)';
const PAGE = 5;

const enc = encodeURIComponent;
const base = (car) => `cars/${enc(car)}/gps-layout`;
export const fitsPath = (car) => `${base(car)}/fits`;
export const fitOnePath = (car, id) => `${base(car)}/fits/${enc(id)}`;
export const applyPath = (car, id) => `${base(car)}/fits/${enc(id)}/apply`;
export const changesPath = (car) => `${base(car)}/changes`;

const plural = (n, [one, other]) => `${formatNumber(n)} ${n === 1 ? one : other}`;
const m = (v) => {
  if (v == null || !Number.isFinite(v)) return 'over 10 km';
  if (v < 1000) return `${formatNumber(Math.round(v))} m`;
  return `${formatNumber(v / 1000, { maximumFractionDigits: 1 })} km`;
};

/** The words of Apply's confirmation for a past result. */
export const applyText = (r) => (r.is_dbc
  ? 'Read this car’s Screenmate GPS frames as the DBC says again, as this fit found? Its history is rebuilt from the stored logs, which can take a while.'
  : `Read this car’s Screenmate GPS frames with ${r.layout}, as this fit found? Its history is rebuilt from the stored logs, which can take a while.`);

/** The slots of one row of the list, as text; when(ms) is the pack's words for a moment. */
export function rowModel(r, when = (ms) => new Date(ms).toISOString()) {
  const state = r?.state || 'done';
  let verdict;
  let tone;
  if (state === 'running') { verdict = `Running (${r.percent ?? 0} %)`; tone = 'wait'; }
  else if (state === 'failed') { verdict = 'Failed'; tone = 'error'; }
  else if (!r.enough) { verdict = 'Too few pairs'; tone = 'warn'; }
  else if (!r.layout) { verdict = 'No layout found'; tone = 'warn'; }
  else if (r.weak) { verdict = 'Weak'; tone = 'warn'; }
  else if (r.good) { verdict = 'Good'; tone = 'ok'; }
  else { verdict = 'Far off'; tone = 'warn'; }
  const counts = state === 'running' ? '' : [
    plural(r.days ?? 0, ['day', 'days']),
    r.logs != null ? plural(r.logs, ['log', 'logs']) : '',
    r.pairs != null ? plural(r.pairs, ['pair', 'pairs']) : '',
  ].filter(Boolean).join(' · ');
  let best = '';
  let err = '';
  if (state === 'done' && r.layout) {
    best = r.is_dbc ? STANDARD : r.layout;
    err = `${m(r.median_m)} median · ${m(r.p95_m)} 95th pct`;
  }
  let applied = '';
  if (r.applied_ms != null) applied = `Applied${r.applied_by ? ` by ${r.applied_by}` : ''} ${when(r.applied_ms)}`;
  else if (r.applied) applied = 'In use now';
  return {
    when: r.started_ms != null ? when(r.started_ms) : '',
    counts, best, err, verdict, tone, applied,
    canApply: state === 'done' && !!r.layout && !r.applied,
    canDelete: state !== 'running',
    canOpen: state !== 'running',
  };
}

/** The slots of one change of the layout: { when, by, now, before }. */
export function changeModel(c, when = (ms) => new Date(ms).toISOString()) {
  return {
    when: c.at_ms != null ? when(c.at_ms) : '',
    by: c.by || '',
    now: c.layout || STANDARD,
    before: c.layout == null && c.before == null ? '' : `before: ${c.before || STANDARD}`,
    fromFit: c.fit != null,
  };
}

/**
 * gpsFitHistory({ api, car, when, confirm, password, flash, errorText, alive, model, btnClass, ghostClass, icon,
 *   onApplied })
 *   when(ms): the pack's words for a moment; confirm(text, opts) -> Promise<boolean>: the pack's dialog (a delete);
 *   password({ title, intro, submit, run(cred) }) -> Promise<result|null>: the pack's password dialog (Apply);
 *   model(status): fitModel of gpsfit.js, which draws an opened result; onApplied(): after an Apply.
 */
export function gpsFitHistory({ api, car, when = (ms) => new Date(ms).toISOString(), alive = () => true,
  confirm = async () => true, password = async () => null, flash = () => {}, errorText = (e) => e?.message || String(e),
  model = () => null, btnClass = 'btn btn--sm', ghostClass = btnClass, icon = () => null, onApplied = () => {} } = {}) {
  let fits = [];
  let fitsNext = null;
  let changes = [];
  let changesNext = null;
  let fitsTotal = 0;
  let changesTotal = 0;
  let loaded = false;
  let busy = false;
  const openId = { v: null };
  const detail = new Map();

  const list = h('ul', { class: 'gp-list' });
  const empty = h('p', { class: 'gp-empty' }, EMPTY);
  const more = h('button', { class: ghostClass, type: 'button', hidden: true, onclick: () => loadFits(true) }, 'Show more');
  const clist = h('ul', { class: 'gp-list gp-changes' });
  const cempty = h('p', { class: 'gp-empty' }, CHANGES_EMPTY);
  const cmore = h('button', { class: ghostClass, type: 'button', hidden: true, onclick: () => loadChanges(true) }, 'Show more');
  const el = h('div', { class: 'gp', id: 'gps-fits' },
    h('h4', { class: 'gp-title' }, TITLE),
    empty, list, more,
    h('h4', { class: 'gp-title' }, CHANGES_TITLE),
    cempty, clist, cmore);

  async function loadFits(append = false) {
    const q = append && fitsNext ? `?limit=${PAGE}&after=${enc(fitsNext)}` : `?limit=${PAGE}`;
    const r = await api.get(`${fitsPath(car)}${q}`);
    fits = append ? fits.concat(r.items || []) : (r.items || []);
    fitsNext = r.next || null;
    fitsTotal = r.total ?? fits.length;
    loaded = true;
    if (alive()) paint();
  }
  async function loadChanges(append = false) {
    const q = append && changesNext ? `?limit=${PAGE}&after=${enc(changesNext)}` : `?limit=${PAGE}`;
    const r = await api.get(`${changesPath(car)}${q}`);
    changes = append ? changes.concat(r.items || []) : (r.items || []);
    changesNext = r.next || null;
    changesTotal = r.total ?? changes.length;
    if (alive()) paint();
  }
  async function load() {
    // a page already shown is read again as far as it reaches (a run that ended, one removed)
    await Promise.all([loadFits(false), loadChanges(false)]);
    if (openId.v != null && !fits.some((f) => f.id === openId.v)) openId.v = null;
  }

  async function open(row) {
    if (openId.v === row.id) { openId.v = null; paint(); return; }
    openId.v = row.id;
    paint();
    if (!detail.has(row.id)) {
      try { detail.set(row.id, await api.get(fitOnePath(car, row.id))); } catch (e) { if (alive()) flash(errorText(e), { kind: 'error' }); }
      if (alive()) paint();
    }
  }

  async function applyRow(row) {
    const d = detail.get(row.id) || await api.get(fitOnePath(car, row.id)).catch(() => null);
    const r = d?.result;
    if (!r?.layout) return;
    busy = true;
    paint();
    try {
      const res = await password({
        title: 'Apply the GPS layout?',
        intro: h('p', null, applyText(r)),
        submit: 'Apply',
        run: async (cred) => {
          try {
            return await api.post(applyPath(car, row.id), cred.two_step ? { confirm_password: cred.password, code: cred.two_step } : { confirm_password: cred.password });
          } catch (e) {
            if (!e || e.name === 'AbortError') throw e;
            if (e.code === 'password_needed') e.code = 'wrong_password';
            else if (e.code === 'wrong_code') e.code = 'wrong_two_step';
            else e.message = errorText(e);
            if (e.fields) { delete e.fields.confirm_password; delete e.fields.code; if (!Object.keys(e.fields).length) e.fields = null; }
            throw e;
          }
        },
      });
      if (res && alive()) {
        flash('GPS layout applied. The car’s history is being rebuilt with it.');
        onApplied();
      }
    } catch (e) { if (alive()) flash(errorText(e), { kind: 'error' }); }
    busy = false;
    if (alive()) { await load().catch(() => {}); paint(); }
  }

  async function removeRow(row) {
    const ok = await confirm('Remove this fit from the list? The layout in use stays as it is.', { title: 'Remove this fit?', ok: 'Remove' });
    if (!ok || !alive()) return;
    busy = true;
    paint();
    try {
      await api.send('DELETE', fitOnePath(car, row.id), {});
      detail.delete(row.id);
      flash('Fit removed.', { kind: 'info' });
    } catch (e) { if (alive()) flash(errorText(e), { kind: 'error' }); }
    busy = false;
    if (alive()) { await load().catch(() => {}); paint(); }
  }

  /** The opened result: the fit block's own words (headline, pairs, latitude and longitude with their errors). */
  function resultEl(s) {
    const x = model(s);
    if (!x) return h('p', { class: 'gp-wait' }, 'Reading the fit…');
    return h('div', { class: 'gf gp-result', 'data-state': x.state, 'data-tone': x.tone },
      h('p', { class: 'gp-head' }, x.headline),
      h('p', { class: 'gp-pairs' }, x.pairs),
      h('dl', { class: 'gf-rows' }, x.rows.map((r) => h('div', { class: 'gf-row' },
        h('dt', null, r.label), h('dd', { class: 'gf-layout', title: r.layout }, r.layout),
        h('dd', { class: 'gf-err' }, r.median ? `${r.median} median · ${r.p95} 95th pct` : '')))),
      x.notes ? h('p', { class: 'gp-notes' }, x.notes) : null);
  }

  function rowEl(f) {
    const r = rowModel(f, when);
    const isOpen = openId.v === f.id;
    const open1 = r.canOpen ? h('button', { class: ghostClass, type: 'button', 'aria-expanded': String(isOpen), onclick: () => open(f) }, isOpen ? 'Close' : 'Open') : null;
    const apply = r.canApply ? h('button', { class: btnClass, type: 'button', disabled: busy, onclick: () => applyRow(f) }, ...[icon('check'), 'Apply'].filter(Boolean)) : null;
    const del = r.canDelete ? h('button', { class: ghostClass, type: 'button', disabled: busy, onclick: () => removeRow(f) }, ...[icon('trash'), 'Remove'].filter(Boolean)) : null;
    return h('li', { class: 'gp-row', 'data-id': f.id, 'data-tone': r.tone, 'data-open': String(isOpen) },
      h('div', { class: 'gp-line' },
        h('span', { class: 'gp-when' }, r.when),
        h('span', { class: 'gp-verdict' }, r.verdict),
        h('span', { class: 'gp-counts' }, r.counts),
        h('span', { class: 'gp-best', title: r.best }, r.best),
        h('span', { class: 'gp-err' }, r.err),
        h('span', { class: 'gp-applied' }, r.applied),
        h('span', { class: 'gp-acts' }, open1, apply, del)),
      isOpen ? resultEl(detail.get(f.id)) : null);
  }

  function changeEl(c) {
    const x = changeModel(c, when);
    return h('li', { class: 'gp-row gp-change' },
      h('div', { class: 'gp-line' },
        h('span', { class: 'gp-when' }, x.when),
        h('span', { class: 'gp-by' }, x.by ? `by ${x.by}` : ''),
        h('span', { class: 'gp-best', title: x.now }, x.now),
        h('span', { class: 'gp-before', title: x.before }, x.before),
        h('span', { class: 'gp-applied' }, x.fromFit ? 'from a fit' : '')));
  }

  function paint() {
    empty.hidden = !loaded || fits.length > 0;
    list.replaceChildren(...fits.map(rowEl));
    more.hidden = !fitsNext;
    more.textContent = `Show more (${Math.max(0, fitsTotal - fits.length)})`;
    cempty.hidden = !loaded || changes.length > 0;
    clist.replaceChildren(...changes.map(changeEl));
    cmore.hidden = !changesNext;
    cmore.textContent = `Show more (${Math.max(0, changesTotal - changes.length)})`;
    el.dataset.count = String(fits.length);
  }

  paint();
  const ready = load().catch(() => {});
  return { el, ready, load, stop: () => {}, fits: () => fits };
}
