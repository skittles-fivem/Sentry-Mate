// Sentry Mate, "Logbook" UI pack: Settings: Integrations, the Supercharger invoices of the Tesla account part
// and of the TeslaFi part (admins only).
//
// An opt-in switch (off by default: nothing is fetched until an admin turns it on), what was found (sessions, how
// many matched a charge, the invoice PDFs kept, older invoices, the last check and the last error), the downloads
// (every PDF as one zip, Tesla only; one month as CSV) and the few sessions not matched to a charge yet.
// The switch asks for the admin's password again, like the pace: PATCH integrations/<source> { version, invoices }.
//
// API: the overview's `invoices`, GET integrations/invoices?source=&matched=false (the unmatched sessions),
// GET integrations/invoices/zip?source=tesla, GET integrations/invoices?source=&from_ms=&to_ms=&format=csv.
// Live: `job` events of kind invoices (id the source) change its figures in place (live events, polling
// GET integrations/<source> only while the live stream is down).

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { formatDate, formatTime, startOf, addPeriod, dateFormat } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { formatMoney } from '../shared/units.js';
import { invoicesZipUrl, invoicesCsvUrl, backfillLabel } from '../shared/invoices.js';
import { figure, icon, ago } from '../core/ui.js';
import { field, switchField } from '../core/form.js';
import { follow, jobOf } from '../shared/live.js';
import { morph } from '../core/morph.js';

const coverOf = (source) => `inv.settings.${source}`;

/** The unmatched sessions to show (a few), or null: read with the part, before it draws. */
export function loadUnmatched(ctx, source, over) {
  const inv = over && over.invoices;
  if (!inv || !inv.enabled || !inv.unmatched) return Promise.resolve(null);
  return ctx.api.get('integrations/invoices', { source, matched: 'false', limit: 5 })
    .then((r) => (r && r.items) || null).catch((e) => { if (api.isAbort(e)) throw e; return null; });
}

/** An empty stand-in (a watch account, a part that could not load): the item stays on the page. */
export const invoicesSlot = (source) => h('div', { hidden: true, cover: coverOf(source) });

/**
 * The invoices block of a part. over: the overview (GET integrations/<source>); unmatched: from loadUnmatched;
 * ask(opts): the part's password dialog (askAgain); again(focusKey): read the part again and draw it.
 */
export function invoicesPart(ctx, { source, over, unmatched = null, ask, again }) {
  const inv = over && over.invoices;
  if (!inv) return invoicesSlot(source);
  const tz = ctx.tz;
  const tesla = source === 'tesla';
  const on = !!inv.enabled;
  const name = tesla ? 'Tesla' : 'TeslaFi';
  const key = `inv-${source}`;

  const sw = switchField({
    label: `Fetch them from ${name}`, checked: on,
    hint: inv.note || (tesla ? 'Reads each Supercharger session from Tesla’s charging history: its cost becomes the charge’s cost (an entered cost still wins) and its invoice (PDF) is kept.'
      : 'Reads the cost TeslaFi gives for each Supercharger session: it becomes the charge’s cost (an entered cost still wins).'),
    onchange: async (v) => {
      const out = await ask({
        title: v ? 'Turn Supercharger invoices on?' : 'Turn Supercharger invoices off?',
        text: v ? (tesla ? 'Sentry Mate reads each Supercharger session from Tesla’s charging history, older ones first, and keeps its invoice (PDF). Its cost becomes the charge’s cost; an entered cost still wins.'
          : 'Sentry Mate reads the cost TeslaFi gives for each Supercharger session. It becomes the charge’s cost; an entered cost still wins. TeslaFi gives no PDFs.')
          : 'No more invoices are fetched. The costs and invoices already kept stay.',
        yes: v ? 'Turn them on' : 'Turn them off',
        run: (creds) => ctx.api.patch(`integrations/${source}`, { version: over.version, invoices: v, ...creds }),
      });
      if (out) ctx.flash(v ? `Supercharger invoices from ${name} are on.` : `Supercharger invoices from ${name} are off.`); else sw.set(!v);
      await again(key);
    },
  });
  sw.input.dataset.tf = key;
  if (!inv.available && !on) sw.input.disabled = true;

  // one month as CSV: this month and the eleven before it, in the page's zone
  const first = startOf(api.now(), 'month', tz);
  const months = Array.from({ length: 12 }, (_, i) => addPeriod(first, 'month', -i, tz));
  const fmt = dateFormat(tz, { month: 'long', year: 'numeric' });
  const csvHref = (i) => invoicesCsvUrl(source, { from_ms: months[i], to_ms: i ? months[i - 1] : addPeriod(first, 'month', 1, tz) });
  const month = field({ label: 'Month', name: `${key}-month`, type: 'select', value: '0', options: months.map((m, i) => [String(i), fmt.format(m)]), cls: 'inv-month' });
  const csv = h('a', { class: 'link-arrow', href: csvHref(0), download: true }, icon('download'), 'Download the month as CSV');
  month.control.addEventListener('change', () => { csv.href = csvHref(Number(month.control.value) || 0); });

  const figsOf = (inv) => {
    const err = inv.last_error;
    return h('div', { class: 'figs tesla-figs inv-figs' },
      figure('Sessions found', formatNumber(inv.sessions || 0), `${formatNumber(inv.matched || 0)} matched to a charge, ${formatNumber(inv.unmatched || 0)} not`),
      inv.gives_pdfs ? figure('Invoices (PDF)', formatNumber(inv.pdfs || 0), inv.pdfs_waiting ? `downloaded; ${formatNumber(inv.pdfs_waiting)} still to download` : 'downloaded') : null,
      figure('Last check', inv.last_check_ms ? ago(inv.last_check_ms) : 'Not yet', [backfillLabel(inv.backfill), inv.next_check_ms && inv.enabled ? `; next ${formatTime(inv.next_check_ms, tz)}` : '', `; ${formatNumber(inv.requests_today || 0)} requests today`]),
      figure('Last error', err ? err.message || err.code : 'None', err ? (err.at_ms ? ago(err.at_ms) : null) : `no error from ${name}`));
  };
  let figs = figsOf(inv);
  const el = h('div', { class: 'inv-part', cover: coverOf(source) },
    h('h3', { class: 'si-sub' }, 'Supercharger invoices', h('span', { class: 'label-note' }, on ? 'on' : 'off, nothing is fetched')),
    sw,
    !inv.available ? h('p', { class: 'si-quiet' }, tesla ? 'This Tesla account gives no invoices (Tesla’s China accounts do not).' : 'TeslaFi gives no costs for this account.') : null,
    figs,
    h('div', { class: 'inv-downloads' },
      tesla ? h('a', { class: 'link-arrow', href: invoicesZipUrl('tesla'), download: true }, icon('download'), 'Download all invoices') : null,
      h('div', { class: 'inv-csv' }, month, csv)),
    unmatched && unmatched.length ? h('div', { class: 'inv-unmatched' },
      h('p', { class: 'si-quiet' }, `Not matched to a charge (${formatNumber(inv.unmatched)}):`),
      h('ul', { class: 'inv-list' }, unmatched.map((i) => h('li', null, h('span', null, i.site || 'A Supercharger'),
        h('span', { class: 'si-quiet' }, ` ${formatDate(i.start_ms, tz, { weekday: false })}, ${formatMoney(i.total, i.currency)}`))))) : null);

  // the figures follow the source's `job` events, written in place; the part's own answer is the first read. A
  // part drawn over by a newer one stops following.
  let firstRead = true, seen = false;
  const live = follow(ctx.api, {
    type: 'job', match: jobOf('invoices', source), pick: (d) => d.status,
    load: () => {
      if (firstRead) { firstRead = false; return Promise.resolve(inv); }
      return ctx.api.get(`integrations/${source}`).then((o) => (o && o.invoices) || inv);
    },
    pollMs: (v) => (v && v.backfill === 'running' ? 5000 : 30000),
    onValue: (next) => {
      if (!el.isConnected) { if (seen) live.stop(); return; }
      seen = true;
      if (next && ctx.alive()) figs = morph(figs, figsOf(next));
    },
  });
  return el;
}
