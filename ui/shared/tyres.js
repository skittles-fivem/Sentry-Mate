// Sentry Mate shared module: tyres.js (the car's tyre pressures against its recommended ones).
// Canonical source: ui/shared/tyres.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// tyreView(now) reads a /cars/{car}/now answer: the four pressures (kPa), whether they are in wheel order
// (front left, front right, rear left, rear right) or the car's own sensor slots, the recommended pressures
// (front and rear axle) and where they come from, and which tyre is low, by the server's rule: more than
// 10 % under the recommended pressure of its axle, or of the lower axle while which wheel each sensor is is
// not known (so a tyre at the rear's pressure is never called low against a higher front one).
// recommendedLine(now) is the sentence pages show under the tyres: "Recommended 2.9 bar (from CAN)".

import { t } from './i18n.js';
import { format } from './units.js';

/** A tyre is low under this share of its recommended pressure. */
export const LOW_SHARE = 0.9;

const val = (p) => (p != null && typeof p === 'object' && 'value' in p ? p.value : p ?? null);
const num = (x) => (typeof x === 'number' && Number.isFinite(x) ? x : null);

/**
 * The Now's tyres: { kpa: [4], mapped, front, rear (kPa or null), source: 'can' | 'placard' | 'owner' | null,
 * wheelIn, against: [4] (the pressure each tyre is judged against), low: [4] (true, false or null), anyLow }.
 * `anyLow` is the server's `tyre_low` when it gives one.
 */
export function tyreView(now) {
  const n = now || {};
  const kpa = [0, 1, 2, 3].map((i) => num(val((n.tyres_kpa || [])[i])));
  const mapped = n.tyres_slots_mapped === true;
  const front = num(val(n.tyre_recommended_kpa));
  const rear = num(val(n.tyre_recommended_rear_kpa)) ?? front;
  const lower = front != null && rear != null ? Math.min(front, rear) : null;
  const against = [0, 1, 2, 3].map((i) => (mapped ? (i < 2 ? front : rear) : lower));
  const low = kpa.map((p, i) => (p == null || against[i] == null ? null : p < against[i] * LOW_SHARE));
  const server = val(n.tyre_low);
  const anyLow = typeof server === 'boolean' ? server : low.includes(true) ? true : low.every((l) => l === false) ? false : null;
  const source = ['can', 'placard', 'owner'].includes(n.tyre_recommended_source) ? n.tyre_recommended_source : front != null ? 'can' : null;
  return { kpa, mapped, front, rear, source, wheelIn: num(n.tyre_recommended_wheel_in), against, low, anyLow };
}

/** Where the recommended pressure comes from, in words: "from CAN", "from the door placard for 20″ wheels", "typed". */
export function recommendedFrom(view) {
  const v = view || {};
  if (v.source === 'owner') return t('typed');
  if (v.source === 'placard') return v.wheelIn != null ? t('from the door placard for {inch}″ wheels', { inch: v.wheelIn }) : t('from the door placard');
  if (v.source === 'can') return t('from CAN');
  return '';
}

/** The recommended pressures in the person's unit: "2.9 bar", or "2.9 bar front, 3.1 bar rear"; null when none. */
export function recommendedValue(view) {
  const v = view || {};
  if (v.front == null) return null;
  if (v.rear == null || Math.abs(v.rear - v.front) < 0.5) return format(v.front, 'pressure');
  return t('{front} front, {rear} rear', { front: format(v.front, 'pressure'), rear: format(v.rear, 'pressure') });
}

/**
 * The line under the tyres: "Recommended 2.9 bar (from CAN)." or "No recommended pressure known.", and, while
 * which wheel each sensor sits on is not known, a sentence saying so.
 */
export function recommendedLine(now) {
  const v = tyreView(now);
  const value = recommendedValue(v);
  const first = value == null ? t('No recommended pressure known.') : t('Recommended {value} ({from}).', { value, from: recommendedFrom(v) });
  const anyTyre = v.kpa.some((p) => p != null);
  if (!anyTyre || v.mapped) return first;
  const split = v.front != null && v.rear != null && Math.abs(v.rear - v.front) >= 0.5;
  return `${first} ${split ? t('Which wheel each sensor sits on is not known yet, so each tyre is judged against the lower one.') : t('Which wheel each sensor sits on is not known yet.')}`;
}
