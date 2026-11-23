// Supercharger invoices (docs/ui-api.md, "Charges" and "Integrations: Supercharger invoices"): what every pack's
// Charges list, charge page and Settings: Integrations share. The words of a charge's cost source, of an invoice's
// fees, the invoice an applied cost came from, and the links of the downloads (one PDF, the zip, a period's CSV).
//
// Pure functions, no DOM: each pack draws its own cells and blocks and calls these.

import { t } from './i18n.js';
import { startOf, addPeriod, dateFormat } from './time.js';

export const INVOICES_BASE = 'integrations/invoices';

/** The cost sources the server sends (`Charge.cost.source`), in words (the Charges list's cost cell title). */
export const COST_SOURCE_LABELS = Object.freeze({
  tariff: 'Place price',
  manual: 'Entered by an admin',
  free_supercharging: 'Free Supercharging',
  teslamate: 'From TeslaMate',
  tesla_invoice: 'From Tesla invoice',
  teslafi: 'From TeslaFi',
});

/** The cost source in words ("From Tesla invoice"); '' for none or an unknown source. */
export function costSourceLabel(source) {
  const s = String(source ?? '');
  return Object.prototype.hasOwnProperty.call(COST_SOURCE_LABELS, s) ? t(COST_SOURCE_LABELS[s]) : '';
}

/** True for a cost that came from an invoice (Tesla's, or a cost TeslaFi gave). */
export const isInvoiceSource = (source) => source === 'tesla_invoice' || source === 'teslafi';

/** The short words after a cost taken from an invoice: "from Tesla invoice", "from TeslaFi"; '' otherwise. */
export function costFromWords(source) {
  if (source === 'tesla_invoice') return t('from Tesla invoice');
  if (source === 'teslafi') return t('from TeslaFi');
  return '';
}

/** An invoice's source in words: "Tesla invoice", "TeslaFi". */
export const invoiceSourceLabel = (source) => (source === 'teslafi' ? t('TeslaFi') : t('Tesla invoice'));

const FEE_LABELS = { charging: 'Charging', idle: 'Idle fee', other: 'Other fee' };
/** A fee's kind in words: "Charging", "Idle fee", "Other fee". */
export const feeLabel = (kind) => t(FEE_LABELS[kind] || FEE_LABELS.other);

/** The invoice whose total is the charge's cost (`applied`), else null. */
export function appliedInvoice(charge) {
  const list = Array.isArray(charge?.invoices) ? charge.invoices : [];
  return list.find((i) => i && i.applied) || null;
}

/** The invoices kept but not used because an entered cost wins (`applied: false`). */
export function unusedInvoices(charge) {
  const list = Array.isArray(charge?.invoices) ? charge.invoices : [];
  return list.filter((i) => i && !i.applied);
}

const enc = encodeURIComponent;
const qs = (params) => {
  const q = Object.entries(params).filter(([, v]) => v !== null && v !== undefined && v !== '')
    .map(([k, v]) => `${enc(k)}=${enc(String(v))}`).join('&');
  return q ? `?${q}` : '';
};

/** The link of one invoice's PDF on the charge page (same origin, `<a href download>`). */
export const chargeInvoicePdfUrl = (car, charge, invoice) =>
  `/api/v1/cars/${enc(car)}/charges/${enc(charge)}/invoices/${enc(invoice)}/pdf`;

/** The link of one invoice's PDF from the list (admins). */
export const invoicePdfUrl = (invoice) => `/api/v1/${INVOICES_BASE}/${enc(invoice)}/pdf`;

/** The link of the zip of every kept PDF ("Download all invoices"); from_ms/to_ms optional. */
export const invoicesZipUrl = (source = 'tesla', { from_ms, to_ms } = {}) =>
  `/api/v1/${INVOICES_BASE}/zip${qs({ source, from_ms, to_ms })}`;

/** The link of a period's CSV of invoices. */
export const invoicesCsvUrl = (source, { from_ms, to_ms } = {}) =>
  `/api/v1/${INVOICES_BASE}${qs({ source, from_ms, to_ms, format: 'csv' })}`;

/** The invoices block's backfill state in words. */
export function backfillLabel(state) {
  if (state === 'running') return t('Reading older invoices');
  if (state === 'done') return t('Older invoices read');
  return t('Older invoices not read');
}

/**
 * The months for a per-period CSV picker, newest first: [{ key: '2026-11', label: 'November 2026', from_ms, to_ms }],
 * `n` months ending with the month `nowMs` is in, in the zone `tz`. The current month ends at its end (not now).
 */
export function invoiceMonths(nowMs, tz, n = 12) {
  const out = [];
  let start = startOf(nowMs, 'month', tz);
  const fmt = dateFormat(tz, { month: 'long', year: 'numeric' });
  const keyFmt = dateFormat(tz, { year: 'numeric', month: '2-digit' });
  for (let i = 0; i < n; i++) {
    const end = addPeriod(start, 'month', 1, tz);
    const p = Object.fromEntries(keyFmt.formatToParts(start).map((x) => [x.type, x.value]));
    out.push({ key: `${p.year}-${p.month}`, label: fmt.format(start), from_ms: start, to_ms: end });
    start = addPeriod(start, 'month', -1, tz);
  }
  return out;
}
