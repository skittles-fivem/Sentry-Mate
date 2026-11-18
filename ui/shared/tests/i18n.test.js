// Tests for i18n.js: text lookup, placeholders, plurals, locale choice and <html lang>.
import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { htmlEl } from './fake-browser.js';
import * as I from '../i18n.js';

after(() => I.setLocale('en-US'));

test('English text is the key; placeholders are filled, numbers in the locale', () => {
  I.setLocale('en-US');
  assert.equal(I.t('Drives'), 'Drives');
  assert.equal(I.t('{n} drives in {city}', { n: 1234, city: 'Springfield' }), '1,234 drives in Springfield');
  assert.equal(I.t('Hello {missing}', {}), 'Hello {missing}');
  assert.equal(I.t('A {x} B', { x: null }), 'A  B');
});

test('plurals follow the language rules', () => {
  I.setLocale('en-US');
  assert.equal(I.tn(1, '{n} drive', '{n} drives'), '1 drive');
  assert.equal(I.tn(0, '{n} drive', '{n} drives'), '0 drives');
  assert.equal(I.tn(2500, '{n} drive', '{n} drives'), '2,500 drives');
});

test('a catalog translates; a missing entry stays English; <html lang> names the text language', () => {
  I.addCatalog('de', { Drives: 'Fahrten', '{n} drives': { one: '{n} Fahrt', other: '{n} Fahrten' } });
  I.setLocale('de-AT');
  assert.equal(I.getLocale(), 'de-AT');
  assert.equal(I.getTextLanguage(), 'de');
  assert.equal(htmlEl().lang, 'de-AT');
  assert.equal(I.t('Drives'), 'Fahrten');
  assert.equal(I.t('Charges'), 'Charges');
  assert.equal(I.tn(1, '{n} drive', '{n} drives'), '1 Fahrt');
  assert.equal(I.tn(15, '{n} drive', '{n} drives'), '15 Fahrten');
  I.setLocale('fr-FR');
  assert.equal(I.getTextLanguage(), 'en');
  assert.equal(htmlEl().lang, 'en', 'text is English while no French catalog exists');
  assert.equal(I.formatNumber(1234.5, { maximumFractionDigits: 1 }).replace(/\u202f/g, ' '), '1 234,5');
  I.setLocale('en-GB');
  assert.equal(htmlEl().lang, 'en-GB');
});

test('the locale comes from the choice, then the browser languages in order, then the fallback', () => {
  assert.equal(I.resolveLocale({ choice: 'de-DE', browser: ['en-US'] }), 'de-DE');
  assert.equal(I.resolveLocale({ choice: 'auto', browser: ['nl-NL', 'en-US'] }), 'nl-NL');
  assert.equal(I.resolveLocale({ choice: '!!not a tag', browser: ['qqq', 'sv-SE'] }), 'sv-SE');
  assert.equal(I.resolveLocale({ choice: null, browser: [] }), 'en-US');
  assert.equal(I.resolveLocale({ choice: 'en_us', browser: [] }), 'en-US', 'a malformed tag is skipped');
  assert.equal(I.isSupportedLocale('en-GB'), true);
  assert.equal(I.isSupportedLocale(''), false);
});

test('listeners hear a change of locale once', () => {
  I.setLocale('en-US');
  const seen = [];
  const off = I.onLocaleChange((l) => seen.push(l));
  I.setLocale('en-GB');
  I.setLocale('en-GB');
  off();
  I.setLocale('en-US');
  assert.deepEqual(seen, ['en-GB']);
});

test('numbers: true minus sign, no minus zero, lists, sorting', () => {
  I.setLocale('en-US');
  assert.equal(I.formatNumber(-3.25, { maximumFractionDigits: 1 }), '\u22123.3');
  assert.equal(I.formatNumber(-0.01, { maximumFractionDigits: 1 }), '0');
  assert.equal(I.formatNumber(null), '');
  assert.equal(I.formatNumber(Infinity), '');
  assert.equal(I.formatList(['a', 'b', 'c']), 'a, b, and c');
  assert.deepEqual(['Car 10', 'car 2', 'Car 1'].sort(I.compareText), ['Car 1', 'car 2', 'Car 10']);
});
