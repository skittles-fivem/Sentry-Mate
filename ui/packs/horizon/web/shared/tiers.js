// Tiered (block) electricity prices, the same in every pack: the price per kWh steps with the kWh used in a
// billing cycle (from the household's own use, if given, and the earlier charges at the place, in time order). A
// charge that crosses a step pays each part at its own price; a time-of-use rate still sets the price for its hours.
//
// The API shape (docs/ui-api.md, Places): price.tiers = { steps: [{ from_kwh, per_kwh }], cycle_day, base_kwh },
// the first step from 0 kWh, each next higher; cycle_day 1 (the calendar month) to 28; base_kwh 0 or more. The
// charge's cost_breakdown parts carry from_kwh and to_kwh for a step, and cost_breakdown.cycle the cycle.

import { formatNumber, t } from './i18n.js';
import { format } from './units.js';

export const MAX_TIERS = 8;
export const MAX_CYCLE_DAY = 28;
export const MAX_TIER_KWH = 1_000_000;

/** The line under the household field: why a meter that counts the house changes where the car starts. */
export const BASE_USE_NOTE = 'If the meter also counts the house, enter its usual use per cycle: it is counted first, so the car’s kWh may start in a higher step.';

/** Steps for a new tiered price: the made-up example of 1,000 kWh at one price and the rest at another. */
export function newTiers(perKwh = null) {
  return { steps: [{ from_kwh: 0, per_kwh: perKwh }, { from_kwh: 1000, per_kwh: null }], cycle_day: 1, base_kwh: 0 };
}

/** A copy of the API's tiers for an editor (null stays null). */
export function copyTiers(tiers) {
  if (!tiers) return null;
  return { steps: (tiers.steps || []).map((s) => ({ from_kwh: s.from_kwh, per_kwh: s.per_kwh })), cycle_day: tiers.cycle_day || 1, base_kwh: tiers.base_kwh || 0 };
}

/** The body to send: steps sorted as typed, numbers or null, cycle day and household use with their defaults. */
export function tiersBody(tiers) {
  return {
    steps: tiers.steps.map((s, i) => ({ from_kwh: i === 0 ? 0 : s.from_kwh, per_kwh: s.per_kwh })),
    cycle_day: tiers.cycle_day || 1,
    base_kwh: tiers.base_kwh || 0,
  };
}

const kwhText = (v) => `${formatNumber(v, { maximumFractionDigits: 1 })} kWh`;
const priceText = (v, currency) => format({ amount: v, currency }, 'price_energy');

/** A step's range in words: "Up to 1,000 kWh", "1,000 to 2,000 kWh", "Above 2,000 kWh" ("Every kWh" alone). */
export function stepLabel(from, to, kwh = kwhText) {
  if (!from && to == null) return t('Every kWh');
  if (!from) return t('Up to {x}', { x: kwh(to) });
  if (to == null) return t('Above {x}', { x: kwh(from) });
  return t('{a} to {b}', { a: formatNumber(from, { maximumFractionDigits: 1 }), b: kwh(to) });
}

/** The range of step `i` of `steps`: { from, to } (to null for the last). */
export function stepRange(steps, i) {
  return { from: steps[i]?.from_kwh ?? 0, to: steps[i + 1]?.from_kwh ?? null };
}

/** "Calendar month" for day 1, else "From the 15th of each month". */
export function cycleWords(day) {
  const d = Number(day) || 1;
  return d === 1 ? t('Calendar month (from the 1st)') : t('From the {n} of each month', { n: ordinal(d) });
}

/** 1st, 2nd, 3rd, 4th ... 21st, 22nd, 23rd, 28th. */
export function ordinal(n) {
  const v = n % 100;
  const s = v >= 11 && v <= 13 ? 'th' : ({ 1: 'st', 2: 'nd', 3: 'rd' }[n % 10] || 'th');
  return `${n}${s}`;
}

/** The choices of a cycle-day select: [value, label] for 1 to 28. */
export function cycleChoices() {
  return Array.from({ length: MAX_CYCLE_DAY }, (_, i) => [String(i + 1), i === 0 ? t('Calendar month (1st)') : t('The {n}', { n: ordinal(i + 1) })]);
}

/**
 * `kwh` used from `before` kWh of the cycle on, split over the steps (sorted by from_kwh, as the server keeps
 * them): [{ step, from_kwh, to_kwh, per_kwh, kwh, cost }], only the steps used. A step without a price costs 0.
 */
export function splitKwh(steps, before, kwh) {
  const out = [];
  let at = Math.max(0, Number(before) || 0);
  let left = Math.max(0, Number(kwh) || 0);
  if (!steps?.length) return out;
  for (let guard = 0; left > 1e-9 && guard < 100; guard++) {
    let i = 0;
    while (i + 1 < steps.length && steps[i + 1].from_kwh <= at) i++;
    const to = steps[i + 1]?.from_kwh ?? null;
    const take = to == null || to <= at ? left : Math.min(left, to - at);
    const last = out[out.length - 1];
    const per = Number(steps[i].per_kwh) || 0;
    if (last && last.step === i) { last.kwh += take; last.cost += take * per; }
    else out.push({ step: i, from_kwh: steps[i].from_kwh, to_kwh: to, per_kwh: steps[i].per_kwh, kwh: take, cost: take * per });
    at += take;
    left -= take;
  }
  return out;
}

/** "612 kWh at $0.16/kWh + 38 kWh at $0.10/kWh" for parts with kwh and per_kwh (a charge's, or splitKwh's). */
export function splitText(parts, currency, { kwh = kwhText, price = priceText } = {}) {
  return parts.filter((p) => p.kwh > 0).map((p) => t('{kwh} at {price}', { kwh: kwh(p.kwh), price: p.per_kwh != null ? price(p.per_kwh, currency) : '—' })).join(' + ');
}

/** The step parts of a charge's cost_breakdown (those with from_kwh), in order. */
export function stepParts(breakdown) {
  return (breakdown?.parts || []).filter((p) => p.from_kwh != null);
}

/**
 * A worked example for the editor: a made-up 50 kWh charge that starts 20 kWh before the second step (or at the
 * household's use, if that is later), so it shows a split. { before, kwh, parts, energy, total }.
 */
export function workedExample(tiers, sessionFee = null, kwh = 50) {
  const steps = tiers?.steps || [];
  const base = Math.max(0, Number(tiers?.base_kwh) || 0);
  const second = steps[1]?.from_kwh;
  const before = Number.isFinite(second) && second - 20 > base ? second - 20 : base;
  const parts = splitKwh(steps, before, kwh);
  const energy = parts.reduce((s, p) => s + p.cost, 0);
  return { before, kwh, parts, energy, total: energy + (Number(sessionFee) || 0) };
}

/** Field errors by API path (`price.tiers.steps.N.from_kwh` ...), the server's rules said first. */
export function tierErrors(tiers) {
  const e = {};
  const steps = tiers?.steps || [];
  if (!steps.length || steps.length > MAX_TIERS) e['price.tiers.steps'] = t('Give 1 to {n} steps.', { n: MAX_TIERS });
  let last = -1;
  steps.forEach((s, i) => {
    const n = i + 1;
    const f = Number(s.from_kwh);
    if (i > 0 && (s.from_kwh == null || !Number.isFinite(f) || f <= last || f > MAX_TIER_KWH)) e[`price.tiers.steps.${i}.from_kwh`] = t('Step {n}: start it above the step before (up to 1,000,000 kWh).', { n });
    if (Number.isFinite(f)) last = i === 0 ? 0 : f;
    const p = s.per_kwh;
    if (p == null || p === '') e[`price.tiers.steps.${i}.per_kwh`] = t('Step {n} needs a price per kWh.', { n });
    else if (!Number.isFinite(Number(p)) || p < 0 || p > 10) e[`price.tiers.steps.${i}.per_kwh`] = t('Step {n}: a price from 0 to 10 per kWh.', { n });
  });
  const d = Number(tiers?.cycle_day ?? 1);
  if (!Number.isInteger(d) || d < 1 || d > MAX_CYCLE_DAY) e['price.tiers.cycle_day'] = t('A start day from 1 to 28.');
  const b = tiers?.base_kwh;
  if (b != null && b !== '' && (!Number.isFinite(Number(b)) || b < 0 || b > MAX_TIER_KWH)) e['price.tiers.base_kwh'] = t('Household use: 0 to 1,000,000 kWh per cycle.');
  return e;
}

/** A short summary of a place's tiers for a list: "3 kWh steps from $0.16/kWh". */
export function tiersSummary(tiers, currency) {
  const steps = tiers?.steps || [];
  if (!steps.length) return '';
  return steps.length === 1 ? t('one step at {p}', { p: priceText(steps[0].per_kwh, currency) })
    : t('{n} kWh steps from {p}', { n: steps.length, p: priceText(steps[0].per_kwh, currency) });
}
