// Tests for invoices.js: the cost sources in words, fee words, the applied invoice and the download links.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as I from '../invoices.js';
import { setLocale } from '../i18n.js';

test('every cost source in words', () => {
  setLocale('en-US');
  assert.equal(I.costSourceLabel('tariff'), 'Place price');
  assert.equal(I.costSourceLabel('manual'), 'Entered by an admin');
  assert.equal(I.costSourceLabel('free_supercharging'), 'Free Supercharging');
  assert.equal(I.costSourceLabel('teslamate'), 'From TeslaMate');
  assert.equal(I.costSourceLabel('tesla_invoice'), 'From Tesla invoice');
  assert.equal(I.costSourceLabel('teslafi'), 'From TeslaFi');
  assert.equal(I.costSourceLabel(null), '');
  assert.equal(I.costSourceLabel('toString'), '', 'no inherited keys');
  assert.equal(I.costSourceLabel('something-new'), '');
});

test('invoice sources and their short words', () => {
  assert.ok(I.isInvoiceSource('tesla_invoice'));
  assert.ok(I.isInvoiceSource('teslafi'));
  assert.ok(!I.isInvoiceSource('manual'));
  assert.equal(I.costFromWords('tesla_invoice'), 'from Tesla invoice');
  assert.equal(I.costFromWords('teslafi'), 'from TeslaFi');
  assert.equal(I.costFromWords('tariff'), '');
  assert.equal(I.invoiceSourceLabel('tesla'), 'Tesla invoice');
  assert.equal(I.invoiceSourceLabel('teslafi'), 'TeslaFi');
});

test('fee words', () => {
  assert.equal(I.feeLabel('charging'), 'Charging');
  assert.equal(I.feeLabel('idle'), 'Idle fee');
  assert.equal(I.feeLabel('other'), 'Other fee');
  assert.equal(I.feeLabel(undefined), 'Other fee');
});

test('the applied invoice and the unused ones', () => {
  const a = { id: 'tesla-1', applied: true };
  const b = { id: 'tesla-2', applied: false };
  assert.equal(I.appliedInvoice({ invoices: [b, a] }), a);
  assert.equal(I.appliedInvoice({ invoices: [b] }), null);
  assert.equal(I.appliedInvoice({}), null);
  assert.deepEqual(I.unusedInvoices({ invoices: [a, b] }), [b]);
  assert.deepEqual(I.unusedInvoices(null), []);
});

test('download links', () => {
  assert.equal(I.chargeInvoicePdfUrl('c1', 'ch 3', 'tesla-1000123'), '/api/v1/cars/c1/charges/ch%203/invoices/tesla-1000123/pdf');
  assert.equal(I.invoicePdfUrl('tesla-1000123'), '/api/v1/integrations/invoices/tesla-1000123/pdf');
  assert.equal(I.invoicesZipUrl(), '/api/v1/integrations/invoices/zip?source=tesla');
  assert.equal(I.invoicesZipUrl('tesla', { from_ms: 1, to_ms: 2 }), '/api/v1/integrations/invoices/zip?source=tesla&from_ms=1&to_ms=2');
  assert.equal(I.invoicesCsvUrl('teslafi', { from_ms: 10, to_ms: 20 }), '/api/v1/integrations/invoices?source=teslafi&from_ms=10&to_ms=20&format=csv');
  assert.equal(I.invoicesCsvUrl('tesla'), '/api/v1/integrations/invoices?source=tesla&format=csv');
});

test('backfill words', () => {
  assert.equal(I.backfillLabel('running'), 'Reading older invoices');
  assert.equal(I.backfillLabel('done'), 'Older invoices read');
  assert.equal(I.backfillLabel('off'), 'Older invoices not read');
});

test('months for the per-period CSV', () => {
  setLocale('en-US');
  const now = Date.UTC(2026, 10, 16, 17, 30); // 2026-11-16 17:30 UTC
  const m = I.invoiceMonths(now, 'America/Los_Angeles', 3);
  assert.deepEqual(m.map((x) => x.key), ['2026-11', '2026-10', '2026-09']);
  assert.equal(m[0].label, 'November 2026');
  assert.equal(m[0].from_ms, Date.UTC(2026, 10, 1, 7));   // midnight Pacific (PST)
  assert.equal(m[0].to_ms, Date.UTC(2026, 11, 1, 8));
  assert.equal(m[1].to_ms, m[0].from_ms);
  assert.equal(m[1].from_ms, Date.UTC(2026, 9, 1, 7));    // PDT
});

test('the audit log names the invoice changes', async () => {
  const A = await import('../audit.js');
  assert.equal(A.actionLabel('tesla-invoices-changed'), 'Turned Tesla invoices on or off');
  assert.equal(A.actionGroup('invoices-downloaded'), 'integrations');
  const it = A.auditItem({ target: 'settings:tesla-invoices' });
  assert.equal(it.text, 'Tesla Supercharger invoices');
  assert.equal(it.hash, '#tesla');
});
