// Sentry Mate, "Pebble" UI pack: the Supercharger invoices tile, in the Tesla account and in TeslaFi on
// Settings: Integrations. An opt-in switch (off by default: nothing is fetched until an admin turns it on), what
// was found (sessions, matched, invoice PDFs, older invoices, the last check, the last error), the downloads (every
// PDF as one zip, Tesla only; one month as CSV) and a few sessions not matched to a charge yet.
// The switch asks for the admin's password again, like the pace: PATCH integrations/<source> { version, invoices }.
//
// Data: the overview's `invoices`, GET integrations/invoices?source=&matched=false (the unmatched sessions),
// GET integrations/invoices/zip?source=tesla, GET integrations/invoices?source=&from_ms=&to_ms=&format=csv.
// Live: `job` events of kind invoices (id the source) change its checks in place (live events, polling
// GET integrations/<source> only while the live stream is down).

import { h } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { formatMoney } from '../shared/units.js';
import { formatDate, formatTime, startOf, addPeriod, dateFormat } from '../shared/time.js';
import { invoicesZipUrl, invoicesCsvUrl, backfillLabel } from '../shared/invoices.js';
import * as api from '../shared/api.js';
import { tile, chip, linkBtn, ago } from '../core/ui.js';
import { toggle } from '../core/form.js';
import { group, infoRow, pickRow, ctlRow } from '../core/settings.js';
import { follow, jobOf } from '../shared/live.js';
import { morph } from '../core/morph.js';

const coverOf = (source) => `inv.settings.${source}`;
// while the live stream is down: read again this often (sooner while older invoices are fetched)
const pollMs = (inv) => (inv?.backfill === 'running' ? 5000 : 30000);

/** The unmatched sessions to show (a few), or null: read in the tiles' load, before they paint. */
export function loadUnmatched(ctx, source, ov) {
  if (!ov?.invoices?.enabled || !ov.invoices.unmatched) return Promise.resolve(null);
  return ctx.api.get('integrations/invoices', { source, matched: 'false', limit: 5 }).then((r) => r?.items || null).catch(() => null);
}

/** An empty stand-in (not an admin, or an overview without invoices): the item stays on the page. */
export const invoicesSlot = (source) => h('div', { hidden: true, cover: coverOf(source) });

const download = (el) => { el.setAttribute('download', ''); return el; };

/**
 * The tile. ov: the overview (GET integrations/<source>); unmatched: from loadUnmatched; setOn(on, sw): asks
 * for the password and saves (sw.set() puts the switch back when it is not saved).
 */
export function invoicesTile(ctx, { source, ov, unmatched = null, setOn }) {
  const inv = ov?.invoices;
  if (!inv) return invoicesSlot(source);
  const tz = ctx.tz;
  const tesla = source === 'tesla';
  const on = !!inv.enabled;
  const sw = toggle({ label: 'Supercharger invoices', checked: on, disabled: !inv.available && !on,
    sub: inv.note || (tesla ? 'Reads each Supercharger session from Tesla’s charging history: its cost becomes the charge’s cost (an entered cost still wins) and its invoice (PDF) is kept.'
      : 'Reads the cost TeslaFi gives for each Supercharger session: it becomes the charge’s cost (an entered cost still wins).'),
    onchange: (v) => setOn(v, sw) });

  const check = (level, name, sub, word) => h('li', { class: ['check', `is-${level}`] }, h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
    h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, name), sub ? h('span', { class: 'check-sub' }, sub) : null), h('span', { class: 'check-word' }, word));
  const checksOf = (inv) => {
    const on = !!inv.enabled;
    const le = inv.last_error;
    return h('ul', { class: 'checks' },
      !inv.available ? check('warn', 'Not available', tesla ? 'This Tesla account gives no invoices.' : 'TeslaFi gives no costs for this account.', 'No') : null,
      check(inv.sessions ? 'ok' : 'off', 'Sessions found', `${formatNumber(inv.matched ?? 0)} matched to a charge, ${formatNumber(inv.unmatched ?? 0)} not.`, formatNumber(inv.sessions ?? 0)),
      inv.gives_pdfs ? check(inv.pdfs_waiting ? 'warn' : inv.pdfs ? 'ok' : 'off', 'Invoices (PDF) downloaded', inv.pdfs_waiting ? `${formatNumber(inv.pdfs_waiting)} still to download.` : 'Every invoice found is kept.', formatNumber(inv.pdfs ?? 0)) : null,
      check(inv.backfill === 'running' ? 'warn' : inv.backfill === 'done' ? 'ok' : 'off', backfillLabel(inv.backfill), inv.backfill === 'running' ? 'Older sessions are read a few at a time.' : null, inv.backfill === 'done' ? 'Done' : inv.backfill === 'running' ? 'Running' : 'Off'),
      check(inv.last_check_ms ? 'ok' : 'off', 'Last check', inv.next_check_ms && on ? `Next at ${formatTime(inv.next_check_ms, tz)}. ${formatNumber(inv.requests_today ?? 0)} requests today.` : `${formatNumber(inv.requests_today ?? 0)} requests today.`, inv.last_check_ms ? ago(inv.last_check_ms) : 'None yet'),
      check(le ? 'warn' : 'ok', 'Last error', le ? `${le.message || le.code}${le.at_ms ? `, ${formatDate(le.at_ms, tz, { weekday: false })}` : ''}` : 'None.', le ? 'Yes' : 'None'));
  };
  let checks = checksOf(inv);

  // one month as CSV: this month and the eleven before it
  const first = startOf(api.now(), 'month', tz);
  const months = Array.from({ length: 12 }, (_, i) => addPeriod(first, 'month', -i, tz));
  const fmt = dateFormat(tz, { month: 'long', year: 'numeric' });
  const csvHref = (i) => invoicesCsvUrl(source, { from_ms: months[i], to_ms: i ? months[i - 1] : addPeriod(first, 'month', 1, tz) });
  const csv = download(linkBtn('CSV', csvHref(0), { kind: 'soft', icon: 'download' }));
  const month = pickRow({ name: 'Month', value: 0, options: months.map((m, i) => [i, fmt.format(m)]), sheetTitle: 'Month of the CSV',
    onpick: (i) => { csv.href = csvHref(Number(i) || 0); } });

  const el = tile({ w: 2, title: 'Supercharger invoices', icon: 'coin', cover: coverOf(source),
    side: on ? chip('On', { tone: 'teal', icon: 'check' }) : chip('Off', { tone: 'muted', icon: 'minus' }) },
  group(null, [sw]),
  checks,
  group('Downloads', [
    tesla ? ctlRow({ name: 'Download all invoices', sub: 'Every invoice PDF kept, as one zip.', control: download(linkBtn('Zip', invoicesZipUrl('tesla'), { kind: 'soft', icon: 'download' })) }) : null,
    month,
    ctlRow({ name: 'That month as CSV', sub: 'One line per session: site, time, energy, fees and total.', control: csv }),
  ]),
  unmatched?.length ? group(`Not matched to a charge (${formatNumber(inv.unmatched)})`, unmatched.map((i) => infoRow({ icon: 'bolt', name: i.site || 'A Supercharger',
    sub: formatDate(i.start_ms, tz, { weekday: false }), side: formatMoney(i.total, i.currency) }))) : null);

  // the checks follow the source's `job` events, written in place; the tile's own answer is the first read. A tile
  // painted over by a newer one stops following.
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
      if (next && ctx.alive()) checks = morph(checks, checksOf(next));
    },
  });
  return el;
}
