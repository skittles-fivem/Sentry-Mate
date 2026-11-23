// Settings: Integrations, the "Supercharger invoices" block of the Tesla section (#tesla) and of the TeslaFi
// section (#teslafi): the opt-in switch (PATCH integrations/tesla or /teslafi `invoices`, the password asked again
// like the pace), what was found (sessions, invoices downloaded, matched and not), the last check, the read of
// older sessions and the last error; for Tesla "Download all invoices" (a zip) and for both a month's CSV. A short
// list of the sessions not matched to a charge is read with the block (GET integrations/invoices?matched=false).
//
// The block is drawn from the input's overview (`ov.invoices`) by the section's paint(); its sessions list is read
// by load() before the page is ready, so nothing shows up later.

import { h } from '../shared/dom.js';
import { formatMoney } from '../shared/units.js';
import { formatDate } from '../shared/time.js';
import { backfillLabel, invoicesZipUrl, invoicesCsvUrl, invoiceMonths, INVOICES_BASE } from '../shared/invoices.js';
import { icon } from '../core/icons.js';
import { metric, figures } from '../core/page.js';
import { ago, num, time } from '../core/fmt.js';
import { askPassword } from './settings-server-move.js';

const LOCAL = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
const NAME = { tesla: 'Tesla', teslafi: 'TeslaFi' };

/** The sessions of `source` not matched to a charge (at most 5), for the block; [] when they cannot be read. */
export async function loadUnmatched(ctx, source) {
  try {
    const pg = await ctx.api.get(INVOICES_BASE, { source, matched: 'false', limit: 5 });
    return pg.items || [];
  } catch (e) {
    if (e?.name === 'AbortError') throw e;
    return [];
  }
}

/**
 * The block. ov: the input's overview; base: 'integrations/tesla' or '/teslafi'; guarded, withPw, after: the
 * section's own helpers for the password dialog (so the words and the stale-page rule are the section's).
 */
export function invoicesBlock(ctx, { source, ov, base, unmatched = [], guarded, withPw, after }) {
  const inv = ov?.invoices;
  const id = `${source}-invoices`;
  if (!inv) return h('div', { class: 'panel panel-pad inv-block', id, hidden: true, cover: `inv.settings.${source}` });
  const name = NAME[source];
  const input = h('input', { type: 'checkbox', id: `${id}-on`, checked: !!inv.enabled, disabled: !inv.available });
  input.addEventListener('change', async () => {
    const want = input.checked;
    input.checked = !want; // stays as it was until the server says so
    const r = await askPassword(ctx, {
      title: want ? `Turn ${name} invoices on` : `Turn ${name} invoices off`,
      intro: h('p', { class: 'note' }, want
        ? (source === 'tesla' ? 'The server then reads the Supercharger sessions of this account\'s cars from Tesla\'s charging history, keeps each invoice (PDF), and gives each matched charge the cost on its invoice.'
          : 'The server then takes the cost TeslaFi lists for each of its charges, and gives each matched charge that cost.')
        : 'Nothing more is fetched. What was fetched stays, and so do the costs it gave.'),
      submit: want ? 'Turn on' : 'Turn off',
      run: guarded((cred) => ctx.api.patch(base, withPw(cred, { version: ov.version, invoices: want }))),
    });
    await after(r, `${name} Supercharger invoices: ${want ? 'on' : 'off'}.`);
  });
  const months = invoiceMonths(ctx.api.now(), LOCAL, 12);
  const monthSel = h('select', { class: 'select', 'aria-label': 'Month of the CSV' }, months.map((m) => h('option', { value: m.key }, m.label)));
  const csvLink = h('a', { class: 'btn btn--sm', href: invoicesCsvUrl(source, months[0]), download: '' }, icon('download', 'ico--sm'), 'CSV of the month');
  monthSel.addEventListener('change', () => {
    const m = months.find((x) => x.key === monthSel.value) || months[0];
    csvLink.setAttribute('href', invoicesCsvUrl(source, m));
  });
  const err = inv.last_error;
  const check = inv.last_check_ms ? `${ago(inv.last_check_ms)}` : 'not yet';
  return h('div', { class: 'panel panel-pad inv-block', id, cover: `inv.settings.${source}` },
    h('div', { class: 'panel-head' }, h('h3', { class: 'panel-title' }, icon('book'), 'Invoices')),
    h('div', { class: 'switch-row switch-row--inline' }, h('span', { class: 'switch' }, input, h('i', { 'aria-hidden': 'true' })),
      h('label', { for: input.id }, 'Supercharger invoices', h('span', { class: 'faint' }, inv.enabled ? ': on' : ': off'))),
    h('p', { class: 'note' }, !inv.available ? `${name} gives no invoices for this account.`
      : inv.note || (source === 'tesla' ? 'Off until you turn it on: nothing is fetched before. Each charge then takes the cost on its invoice, unless an admin entered one.' : null)),
    figures([
      metric('Sessions found', inv.sessions, 'number', { size: 'sm', digits: 0, explain: `${num(inv.matched)} matched to a charge, ${num(inv.unmatched)} not.` }),
      source === 'tesla' ? metric('Invoices (PDF)', inv.pdfs, 'number', { size: 'sm', digits: 0, explain: inv.pdfs_waiting ? `${num(inv.pdfs_waiting)} still to download.` : 'All downloaded.' }) : null,
      metric('Last check', check, null, { size: 'sm', explain: `${inv.next_check_ms ? `Next ${time(inv.next_check_ms, LOCAL)}. ` : ''}${backfillLabel(inv.backfill)}. ${num(inv.requests_today)} requests today.` }),
    ].filter(Boolean), 'si-figs'),
    err ? h('p', { class: 'si-last-error warn' }, icon('warn', 'ico--sm'), ' Last error: ', h('b', null, err.message || err.code), err.at_ms ? ` (${ago(err.at_ms)})` : '', '.')
      : h('p', { class: 'si-last-error faint' }, icon('check', 'ico--sm'), ' No error.'),
    h('div', { class: 'inv-downloads' },
      source === 'tesla' ? h('a', { class: 'btn btn--sm', href: invoicesZipUrl('tesla'), download: '' }, icon('download', 'ico--sm'), 'Download all invoices') : null,
      h('span', { class: 'inv-month' }, monthSel, csvLink)),
    unmatched.length ? h('div', { class: 'inv-unmatched' },
      h('span', { class: 'metric-label' }, 'Not matched to a charge'),
      h('ul', { class: 'inv-list' }, unmatched.map((s) => h('li', null,
        h('span', null, s.site), h('span', { class: 'faint' }, formatDate(s.start_ms, LOCAL, { weekday: false, year: true })),
        h('span', { class: 'num' }, formatMoney(s.total, s.currency)))))) : null);
}
