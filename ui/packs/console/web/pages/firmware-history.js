// ## module firmware history, for the Software page (#firmware-history): every fingerprint each module has had,
// newest first, with the version it came with, and a "versions over time" strip with one lane per module.
// - GET /cars/{car}/firmware/history?module=&limit=&after= (paged; each page also has `modules` (all four, in
//   order), `spans` (every fingerprint, oldest first, not paged) and `range`). Version rows (kind "version", from
//   an imported TeslaMate or TeslaFi history) carry a car version and no fingerprint; a module filter leaves them out.
// - Read-only for everyone (a watch account reads it for its cars): names are typed under ## tesla updates installed.
//
//   firmwareHistory(ctx)   { el, ready, reload({ quiet }) }

import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { section, note } from '../core/ui.js';
import { tag, dateTime, date, dur, DASH } from '../core/fmt.js';
import { table } from '../core/table.js';
import { empty, loadingLine } from '../core/states.js';
import { src, srcText } from '../core/source.js';
import { MODULE_WORDS, modWords, hexEl, versionTag } from './firmware.js';

const enc = encodeURIComponent;
const none = (t) => h('span', { class: 'muted' }, t);
const sig = (x) => JSON.stringify(x);
const modName = (id, name) => MODULE_WORDS[id] || name || id;
/** The row's input as core/source.js reads it ('can' is SMO; an import names its site). */
const srcRow = (r) => ({ sources: [r.source === 'can' || !r.source ? { kind: 'smo', label: r.source_label || 'Data from SMO' } : { kind: 'import', site: r.source, label: r.source_label }] });

export function firmwareHistory(ctx) {
  const cid = ctx.car.id, tz = ctx.tz;
  const path = `cars/${enc(cid)}/firmware/history`;
  const LIMIT = 50;
  let module = null;   // null: all modules (and the version rows)
  let lastHead = null, lastFirst = null;

  const side = h('span', { class: 'muted' });
  const keys = h('div', { class: 'keys fh-keys', role: 'group', 'aria-label': 'Show the history of' });
  const stripSlot = h('div', { class: 'fh-strip' }, loadingLine('loading the versions over time'));

  const span = (r) => [dateTime(r.first_seen_ms, tz), h('span', { class: 'muted' }, ' .. '), r.current ? h('b', null, 'now') : dateTime(r.last_seen_ms, tz)];
  const t = table(ctx, {
    bare: true, path, params: {}, limit: LIMIT, csv: false, label: 'Module firmware history, newest first',
    columns: [
      { key: 'module', label: 'module', ph: 'main',
        render: (r) => [r.kind === 'version' ? [tag('version', 'muted'), ' car version'] : modName(r.module, r.module_name), r.current ? [' ', tag('current', 'accent')] : null] },
      { key: 'fp', label: 'fingerprint', ph: 'sub', render: (r) => (r.kind === 'version' ? false : hexEl(r.fingerprint)) },
      { key: 'when', label: 'from .. to', ph: 'sub', render: span },
      { key: 'took', label: 'ran for', num: true, ph: 'num', render: (r) => dur(r.duration_ms) },
      { key: 'version', label: 'came with', ph: 'sub', grow: true,
        render: (r) => (r.version ? [h('b', null, r.version), r.update?.version_source && versionTag(r.update.version_source) ? [' ', versionTag(r.update.version_source)] : null] : r.kind === 'version' ? none(DASH) : r.update ? none('unnamed') : none('first seen')) },
      { key: 'from', label: 'data', title: 'where it came from: smo (the car\'s logs), imp (an imported history)', render: (r) => src(srcRow(r)), value: (r) => srcText(srcRow(r)) },
    ],
    empty: () => (module
      ? empty(`no fingerprint from the ${modName(module)} yet`, 'it shows once the car\'s logs carry one; an older car may not send it.')
      : empty('no firmware history yet', 'a fingerprint shows once the car\'s logs carry one, a version once a TeslaMate or TeslaFi history is imported.')),
    onPage: (pg, items) => { if (ctx.alive()) paintHead(pg, items); },
    onError: () => { if (lastHead == null) { replace(side, tag('not read', 'bad')); replace(stripSlot); } },
  });

  function pick(id) {
    module = id;
    for (const b of keys.children) b.setAttribute('aria-pressed', String((b.dataset.v || null) === module));
    lastFirst = null;
    t.reload(module ? { module } : {});
  }

  function paintHead(pg, items) {
    if (items.length <= (pg.items || []).length) lastFirst = sig(pg.items);   // the first page: what a live update compares
    const s = sig([pg.modules, pg.spans, pg.range]);
    if (s === lastHead) return;   // nothing new: the strip and the keys stay
    lastHead = s;
    const mods = pg.modules || [];
    const n = mods.reduce((a, m) => a + (m.fingerprints || 0), 0);
    replace(side, n ? `${formatNumber(n)} fingerprint${n === 1 ? '' : 's'}` : 'none seen yet');
    replace(keys,
      h('button', { class: 'key', type: 'button', 'aria-pressed': String(module == null), dataset: { v: '' }, onclick: () => pick(null) }, 'all'),
      mods.map((m) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(module === m.id), dataset: { v: m.id }, title: modName(m.id, m.name),
        onclick: () => pick(m.id) }, modWords([m.id], { short: true }))));
    replace(stripSlot, lanes(mods, pg.spans || [], pg.range || {}));
  }

  /** One lane of characters per module: '=' a fingerprint (two tones, the current one in the accent), '|' a change, '.' not seen. */
  function lanes(mods, spans, rg) {
    if (rg.from_ms == null || !spans.length) return empty('no versions over time yet');
    const from = rg.from_ms, to = Math.max(rg.to_ms ?? from, from + 1);
    const cs = getComputedStyle(document.documentElement);
    const adv = parseFloat(cs.getPropertyValue('--a2')) || 8;
    const width = Math.max(160, ctx.view.clientWidth - 2 * (parseFloat(cs.getPropertyValue('--gut')) || 16));
    const cells = Math.max(16, Math.min(160, Math.floor(width / adv)));
    const step = (to - from) / cells;
    const tip = (sp) => `${sp.fingerprint}  ${sp.version || 'unnamed'}  ${dateTime(sp.from_ms, tz)} .. ${sp.current ? 'now' : dateTime(sp.to_ms, tz)}`;
    return h('div', { class: 'fh-lanes' },
      mods.map((m) => {
        const own = spans.filter((sp) => sp.module === m.id);
        const runs = [];
        for (let i = 0; i < cells; i++) {
          const a = from + i * step, b = a + step;
          // a fingerprint that starts in this cell wins it (a short one still shows its tick), else the one covering it most
          let k = -1, best = 0;
          for (let j = own.length - 1; j >= 0 && k < 0; j--) if (own[j].from_ms >= a && own[j].from_ms < b) k = j;
          if (k < 0) own.forEach((sp, j) => { const o = Math.min(b, sp.to_ms ?? to) - Math.max(a, sp.from_ms); if (o > best) { best = o; k = j; } });
          const tick = k > 0 && own[k].from_ms >= a && own[k].from_ms < b;
          const key = `${k}:${tick}`;
          const last = runs[runs.length - 1];
          if (last && last.key === key) last.n++; else runs.push({ key, k, tick, n: 1 });
        }
        const names = own.map((sp) => sp.version || 'unnamed').filter((v, i, a) => i === 0 || a[i - 1] !== v);
        return h('div', { class: 'fh-lane' },
          h('p', { class: 'fh-lane-k' }, h('span', { class: 'fh-lane-n' }, modName(m.id, m.name)),
            none(own.length ? `  ${formatNumber(own.length)}: ${names.join(' -> ')}` : '  not seen')),
          h('div', { class: 'strip-cells fh-cells', role: 'img', 'aria-label': `${modName(m.id, m.name)}: ${own.length ? own.map(tip).join('; ') : 'no fingerprint seen'}` },
            runs.map((r) => (r.k < 0
              ? h('span', { class: 'fh-none' }, '.'.repeat(r.n))
              : h('span', { class: own[r.k].current ? 'fh-cur' : r.k % 2 ? 'fh-b' : 'fh-a', title: tip(own[r.k]) },
                r.tick ? [h('span', { class: 'fh-tick' }, '|'), '='.repeat(r.n - 1)] : '='.repeat(r.n))))));
      }),
      h('div', { class: 'strip-axis muted' }, h('span', null, date(from, tz)), h('span', null, date(to, tz))),
      h('div', { class: 'strip-key' }, [['fh-a', '=', 'a fingerprint'], ['fh-cur', '=', 'the current one'], ['fh-tick', '|', 'a change'], ['fh-none', '.', 'not seen']]
        .map(([c, ch, w]) => h('span', { class: 'nowrap' }, h('span', { class: c }, ch), ` ${w}`))));
  }

  /** A live update: load again only when the first page changed (the strip comes with it). */
  async function reload({ quiet = true } = {}) {
    if (!quiet || lastFirst == null) return t.reload(null, { quiet });
    let pg;
    try { pg = await ctx.api.get(path, { ...(module ? { module } : {}), limit: LIMIT }); } catch { return; }
    if (!ctx.alive() || sig(pg.items) === lastFirst) return;
    await t.reload(null, { quiet: true });
  }

  const el = section({ title: 'module firmware history', id: 'firmware-history', side },
    note('Every fingerprint each module has had, newest first, from first to last seen, and the version it came with. ',
      'A version row is a car version from an imported history: those carry no fingerprints. Names are typed under tesla updates installed.'),
    h('p', { class: 'sub-title' }, 'versions over time'), stripSlot,
    h('p', { class: 'sub-title' }, 'history'), keys, t.el);
  return { el, ready: t.ready, reload };
}
