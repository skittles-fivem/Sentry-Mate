// Sentry Mate shared module: i18n.js (words, locale, number formatting).
// Canonical source: ui/shared/i18n.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// t('English text {name}', { name }) looks the text up in the catalog for the person's language and
// fills in the {placeholders}; the English text itself is the key, so a missing translation shows English.
// The locale (for example 'en-GB' or 'de-DE') sets how numbers, dates and lists are written, even when
// no catalog for that language exists yet. setLocale() also sets <html lang> to the language the text is
// really in. No DOM writes apart from that attribute; no dependencies.

const FALLBACK_LOCALE = 'en-US';

const catalogs = new Map(); // language tag -> { 'English text': 'translation' | { one, other, ... } }
const listeners = new Set();
let locale = FALLBACK_LOCALE;
let textLang = 'en';

/** Is this a locale tag the browser can format with? */
export function isSupportedLocale(tag) {
  if (typeof tag !== 'string' || !tag.trim()) return false;
  try {
    const [canon] = Intl.getCanonicalLocales(tag.trim());
    return Intl.NumberFormat.supportedLocalesOf([canon]).length > 0;
  } catch {
    return false;
  }
}

const canonical = (tag) => Intl.getCanonicalLocales(tag.trim())[0];
const baseOf = (tag) => String(tag).split('-')[0].toLowerCase();

function browserLanguages() {
  const nav = globalThis.navigator;
  if (!nav) return [];
  if (Array.isArray(nav.languages) && nav.languages.length) return [...nav.languages];
  return nav.language ? [nav.language] : [];
}

/**
 * Pick the locale: the person's explicit choice first (preference or ?lang=), then each browser
 * language in its stated order, then the fallback. 'auto', '', null and tags the browser cannot use
 * are skipped, never mapped to some unrelated language.
 */
export function resolveLocale({ choice = null, browser = browserLanguages(), fallback = FALLBACK_LOCALE } = {}) {
  const candidates = [choice, ...(browser || [])];
  for (const c of candidates) {
    if (c == null || c === 'auto' || c === 'device') continue;
    if (isSupportedLocale(c)) return canonical(c);
  }
  return isSupportedLocale(fallback) ? canonical(fallback) : FALLBACK_LOCALE;
}

/** The catalog language that best serves a locale: exact tag, then its base language, else English. */
function catalogLangFor(tag) {
  if (catalogs.has(tag)) return tag;
  const base = baseOf(tag);
  if (catalogs.has(base)) return base;
  for (const k of catalogs.keys()) if (baseOf(k) === base) return k;
  return 'en';
}

function applyDocumentLang() {
  const doc = globalThis.document;
  if (!doc || !doc.documentElement) return;
  // The page declares the language its text is in. Same base language as the locale: use the full
  // locale (en-GB); otherwise the text is in the catalog language (English while none exists).
  doc.documentElement.lang = baseOf(locale) === baseOf(textLang) ? locale : textLang;
}

/**
 * Use this locale from now on. tag: a BCP 47 tag, or 'auto' / null for the browser's languages.
 * Sets <html lang>, tells every onLocaleChange listener, returns the locale in use.
 */
export function setLocale(tag) {
  const next = resolveLocale({ choice: tag });
  const nextLang = catalogLangFor(next);
  const changed = next !== locale || nextLang !== textLang;
  locale = next;
  textLang = nextLang;
  applyDocumentLang();
  if (changed) for (const f of [...listeners]) { try { f(locale); } catch (e) { console.error(e); } }
  return locale;
}

/** The locale numbers and dates are written in, for example 'en-US'. */
export const getLocale = () => locale;
/** The language the interface text is in ('en' while no catalog matches). */
export const getTextLanguage = () => textLang;

/** f(locale) after the locale changed; returns an unsubscribe function. */
export function onLocaleChange(f) {
  listeners.add(f);
  return () => listeners.delete(f);
}

/** Add or extend the translations for one language: { 'English text': 'Translated text' }. */
export function addCatalog(lang, messages) {
  const tag = canonical(lang);
  catalogs.set(tag, { ...(catalogs.get(tag) || {}), ...messages });
  if (catalogLangFor(locale) !== textLang) setLocale(locale);
}

function fill(text, vars) {
  if (!vars) return text;
  return text.replace(/\{([a-zA-Z_][\w]*)\}/g, (all, name) => {
    if (!(name in vars)) return all;
    const v = vars[name];
    if (v == null) return '';
    return typeof v === 'number' ? formatNumber(v) : String(v);
  });
}

/** Translate English text and fill in {placeholders}; numbers are written in the locale. */
export function t(message, vars) {
  const cat = catalogs.get(textLang);
  const hit = cat && typeof cat[message] === 'string' ? cat[message] : message;
  return fill(hit, vars);
}

const pluralRules = new Map();
function pluralOf(n) {
  let r = pluralRules.get(textLang);
  if (!r) { r = new Intl.PluralRules(textLang); pluralRules.set(textLang, r); }
  return r.select(n);
}

/**
 * Count words: tn(3, '{n} drive', '{n} drives') -> '3 drives'. {n} is the count written in the locale.
 * A catalog can hold an object for the plural key: { one: '…', few: '…', other: '…' }.
 */
export function tn(n, one, other, vars = {}) {
  const cat = catalogs.get(textLang);
  const forms = cat && cat[other];
  const rule = pluralOf(Math.abs(n));
  let text;
  if (forms && typeof forms === 'object') text = forms[rule] ?? forms.other ?? other;
  else text = rule === 'one' ? one : other;
  return fill(text, { n, ...vars });
}

const numberFormats = new Map();
/** A cached Intl.NumberFormat for the current locale. */
export function numberFormat(opts = {}) {
  const key = locale + '|' + JSON.stringify(opts);
  let f = numberFormats.get(key);
  if (!f) {
    f = new Intl.NumberFormat(locale, opts);
    if (numberFormats.size > 500) numberFormats.clear();
    numberFormats.set(key, f);
  }
  return f;
}

/**
 * A number in the locale, with a true minus sign (−) and no "-0". null, NaN and Infinity give ''.
 * opts are Intl.NumberFormat options; signDisplay defaults to 'negative'.
 */
export function formatNumber(v, opts = {}) {
  if (v == null || typeof v !== 'number' || !Number.isFinite(v)) return '';
  return numberFormat({ signDisplay: 'negative', ...opts }).format(v).replace(/-/g, '\u2212');
}

/** formatToParts with the same rules as formatNumber. */
export function numberParts(v, opts = {}) {
  if (v == null || typeof v !== 'number' || !Number.isFinite(v)) return [];
  return numberFormat({ signDisplay: 'negative', ...opts }).formatToParts(v)
    .map((p) => (p.type === 'minusSign' ? { ...p, value: '\u2212' } : p));
}

/** 'a, b and c' in the locale. type: 'conjunction' (and) or 'disjunction' (or). */
export function formatList(items, { type = 'conjunction', style = 'long' } = {}) {
  const list = (items || []).filter((x) => x != null && x !== '').map(String);
  if (typeof Intl.ListFormat !== 'function') return list.join(', ');
  return new Intl.ListFormat(locale, { type, style }).format(list);
}

let collator = null, collatorLocale = null;
/** Compare names the way a person sorts them (locale order, numbers by value: 'Car 2' before 'Car 10'). */
export function compareText(a, b) {
  if (collatorLocale !== locale) { collator = new Intl.Collator(locale, { numeric: true, sensitivity: 'base' }); collatorLocale = locale; }
  return collator.compare(String(a ?? ''), String(b ?? ''));
}

// Start with the browser's languages, so formatting is right before preferences arrive.
setLocale(null);
