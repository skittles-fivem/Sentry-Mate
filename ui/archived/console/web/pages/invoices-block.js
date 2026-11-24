// settings/integrations, the supercharger invoices part of the tesla account block and of the teslafi block: an
// opt-in switch (off by default: nothing is fetched until an admin turns it on), what was found, the downloads
// (every PDF as one zip from Tesla, a month as CSV) and the sessions not matched to a charge yet.
// The switch asks for the admin's password again, like the pace: PATCH integrations/<source> { version, invoices }.
// API: the overview's `invoices`, GET integrations/invoices?source=&matched=false (the unmatched list),
// GET integrations/invoices/zip?source=tesla, GET integrations/invoices?source=&from_ms=&to_ms=&format=csv.
// Live: `job` events of kind invoices (id the source) change its figures in place (live events, polling
// GET integrations/<source> only while the live stream is down).

import { h } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { formatMoney } from '../shared/units.js';
import { startOf, addPeriod, dateFormat } from '../shared/time.js';
import { invoicesZipUrl, invoicesCsvUrl, backfillLabel } from '../shared/invoices.js';
import { kv, note } from '../core/ui.js';
import { tag, date, time, ago } from '../core/fmt.js';
import * as api from '../shared/api.js';
import { follow, jobOf, setText, setAttr } from '../shared/live.js';
import { morphChildren } from '../core/morph.js';

const BACKFILL_TONE = { running: 'accent', done: 'ok', off: 'muted' };
const muted = (t) => h('span', { class: 'muted' }, t);
const cover = (source) => `inv.settings.${source}`;
// while the live stream is down: read again this often (sooner while older invoices are fetched)
const pollMs = (inv) => (inv?.backfill === 'running' ? 5000 : 30000);

/** The unmatched sessions to show (a few), or null: call in the block's load, before painting. */
export function loadUnmatched(ctx, source, ov) {
  if (!ov?.invoices?.enabled || !ov.invoices.unmatched) return Promise.resolve(null);
  return ctx.api.get('integrations/invoices', { source, matched: 'false', limit: 5 }).then((r) => r?.items || null).catch(() => null);
}

/** An empty stand-in (for a reader who is not an admin, or an overview without invoices): the item stays on the page. */
export const invoicesSlot = (source) => h('div', { hidden: true, cover: cover(source) });

/**
 * The invoices part. ov: the overview (GET integrations/<source>); unmatched: from loadUnmatched;
 * setOn(on): asks the password and saves, resolving once the block has been read again; fk(el, key) marks a
 * button for the focus to come back to after a repaint.
 */
export function invoicesBlock(ctx, { source, ov, unmatched = null, setOn, fk = (el) => el }) {
  const inv = ov?.invoices;
  if (!inv) return invoicesSlot(source);
  const tz = ctx.tz;
  const tesla = source === 'tesla';
  let on = !!inv.enabled;
  const sw = h('div', { class: 'keys', role: 'group', 'aria-label': 'Supercharger invoices' },
    [[false, 'off'], [true, 'on']].map(([v, l]) => fk(h('button', { class: 'key', type: 'button', 'aria-pressed': String(on === v), disabled: !inv.available && v,
      onclick: () => (on === v ? null : setOn(v)) }, l), `inv-${source}:${l}`)));

  // the month for the CSV: this month and the eleven before it, in the zone of the page
  const now = api.now();
  const first = startOf(now, 'month', tz);
  const months = Array.from({ length: 12 }, (_, i) => addPeriod(first, 'month', -i, tz));
  const fmt = dateFormat(tz, { month: 'short', year: 'numeric' });
  const sel = h('select', { class: 'in in--date', 'aria-label': 'Month of the CSV' }, months.map((m, i) => h('option', { value: String(i), selected: i === 0 }, fmt.format(m).toLowerCase())));
  const csvHref = (i) => invoicesCsvUrl(source, { from_ms: months[i], to_ms: i ? months[i - 1] : addPeriod(first, 'month', 1, tz) });
  const csv = h('a', { class: 'btn', href: csvHref(0), download: true }, 'download csv');
  sel.addEventListener('change', () => { csv.href = csvHref(Number(sel.value) || 0); });

  const rowsOf = (inv) => {
    const on = !!inv.enabled;
    const err = inv.last_error;
    return [
      ['state', on ? [tag('on', 'ok'), muted(` read from ${tesla ? 'tesla’s charging history' : 'teslafi'}`)] : [tag('off', 'muted'), muted(' nothing is fetched until an admin turns it on')]],
      !inv.available ? ['available', [tag('no', 'warn'), muted(` this ${tesla ? 'tesla account' : 'source'} gives no invoices`)], { tone: 'warn' }] : null,
      ['sessions found', [formatNumber(inv.sessions ?? 0), muted(` ${formatNumber(inv.matched ?? 0)} matched to a charge, ${formatNumber(inv.unmatched ?? 0)} not`)]],
      inv.gives_pdfs ? ['invoices (pdf)', [formatNumber(inv.pdfs ?? 0), muted(inv.pdfs_waiting ? ` downloaded, ${formatNumber(inv.pdfs_waiting)} still to download` : ' downloaded')]] : null,
      ['older invoices', [tag(backfillLabel(inv.backfill).toLowerCase(), BACKFILL_TONE[inv.backfill] || 'muted')]],
      ['last check', inv.last_check_ms ? h('span', { title: date(inv.last_check_ms, tz) }, ago(inv.last_check_ms)) : muted('none yet')],
      ['next check', inv.next_check_ms && on ? time(inv.next_check_ms, tz) : muted('not planned')],
      ['requests today', formatNumber(inv.requests_today ?? 0)],
      ['last error', err ? [err.message || err.code, err.at_ms ? muted(`, ${ago(err.at_ms)}`) : null] : muted('none'), { tone: err ? 'warn' : null }],
    ].filter(Boolean);
  };
  const figs = kv(rowsOf(inv), { cols: 1 });
  const list = unmatched?.length ? h('ul', { class: 'ln-list' }, unmatched.map((i) => h('li', null, h('span', null, date(i.start_ms, tz)),
    h('span', { class: 'ln-list-t' }, i.site || 'a supercharger', muted(` ${formatMoney(i.total, i.currency)}`))))) : null;

  const listHead = list ? h('p', { class: 'sub-title' }, `not matched to a charge (${formatNumber(inv.unmatched)})`) : null;
  const el = h('div', { cover: cover(source) },
    h('p', { class: 'sub-title' }, 'supercharger invoices'),
    sw,
    note(inv.note || (tesla ? 'Reads each Supercharger session from Tesla’s charging history: its cost becomes the charge’s cost (a typed cost still wins) and its invoice (PDF) is kept. Off by default.'
      : 'Reads the cost TeslaFi gives for each Supercharger session; it becomes the charge’s cost (a typed cost still wins). Off by default.')),
    figs,
    h('div', { class: 'carline' }, muted('csv'), sel, csv,
      tesla ? h('a', { class: 'btn', href: invoicesZipUrl('tesla'), download: true }, 'download all invoices') : null),
    list ? [listHead, list] : null);

  // the figures follow the source's `job` events, written in place; the block's own answer is the first read. A
  // block painted over by a newer one stops following.
  let firstRead = true, seen = false;
  const live = follow(ctx.api, {
    type: 'job', match: jobOf('invoices', source), pick: (d) => d.status,
    load: () => {
      if (firstRead) { firstRead = false; return Promise.resolve(inv); }
      return ctx.api.get(`integrations/${source}`).then((o) => o?.invoices || inv);
    },
    pollMs,
    onValue: (next) => {
      if (!el.isConnected) { if (seen) live.stop(); return; }
      seen = true;
      if (!next || !ctx.alive()) return;
      morphChildren(figs, [...kv(rowsOf(next), { cols: 1 }).childNodes]);
      on = !!next.enabled;
      for (const b of sw.children) setAttr(b, 'aria-pressed', String((b.textContent === 'on') === on));
      if (listHead) setText(listHead, `not matched to a charge (${formatNumber(next.unmatched ?? 0)})`);
    },
  });
  return el;
}
