// Sentry Mate shared module: tyres.js (the car's tyre pressures against its recommended ones).
// Canonical source: ui/shared/tyres.js. ui/tools/sync_shared.py copies it into each pack's web/shared/;
// edit it here, never in a pack.
//
// tyreView(now) reads a /cars/{car}/now answer: the four pressures (kPa), whether they are in wheel order
// (front left, front right, rear left, rear right) or the car's own sensor slots, the recommended pressures
// (front and rear axle) and where they come from, and which tyre is low, by the server's rule: more than
// 10 % under the recommended pressure of its axle, or of the lower axle while which wheel each sensor is is
// not known (so a tyre at the rear's pressure is never called low against a higher front one).
// recommendedLine(now) is the short sentence pages show under the tyres: "Recommended 2.9 bar (from CAN).";
// wheelGuessLine(now) the nudge to set the wheel size while the placard's is for a size that is only a guess;
// tyresTitle(now) the longer explanation pages put in the line's hover title (how tyres are judged, the sensor
// slots not being known, cold mornings).

import { t } from './i18n.js';
import { format } from './units.js';

/** A tyre is low under this share of its recommended pressure. */
export const LOW_SHARE = 0.9;

const val = (p) => (p != null && typeof p === 'object' && 'value' in p ? p.value : p ?? null);
const num = (x) => (typeof x === 'number' && Number.isFinite(x) ? x : null);

/**
 * The Now's tyres: { kpa: [4], mapped, front, rear (kPa or null), source: 'can' | 'placard' | 'owner' | null,
 * wheelIn, wheelFrom (where that size comes from: 'owner' | 'can' | 'vin' | 'tesla_api' | 'teslamate' | null),
 * wheelGuess (true when that size is not typed and not the car's own wheel setting), against: [4] (the pressure each tyre is judged against), low: [4] (true, false or null), anyLow }.
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
  const wheelIn = source === 'placard' ? num(n.tyre_recommended_wheel_in) : null;
  const wheelFrom = wheelIn != null && typeof n.tyre_recommended_wheel_from === 'string' ? n.tyre_recommended_wheel_from : null;
  const wheelGuess = wheelIn != null && n.tyre_recommended_wheel_guess === true;
  return { kpa, mapped, front, rear, source, wheelIn, wheelFrom, wheelGuess, against, low, anyLow };
}

/** Where the recommended pressure comes from, in words: "from CAN", "from the door placard for 20″ wheels", "typed". */
export function recommendedFrom(view) {
  const v = view || {};
  if (v.source === 'owner') return t('typed');
  if (v.source === 'placard') return v.wheelIn != null ? t('from the door placard for {inch}″ wheels', { inch: v.wheelIn }) : t('from the door placard');
  if (v.source === 'can') return t('from CAN');
  return '';
}

/**
 * The recommended pressures in the person's unit: "2.9 bar", or "2.9 bar front, 3.1 bar rear" when the two read
 * differently in that unit; null when none.
 */
export function recommendedValue(view) {
  const v = view || {};
  if (v.front == null) return null;
  const front = format(v.front, 'pressure');
  if (v.rear == null || Math.abs(v.rear - v.front) < 0.5) return front;
  const rear = format(v.rear, 'pressure');
  return rear === front ? front : t('{front} front, {rear} rear', { front, rear }); // 290 and 293 kPa are both '42 psi'
}

/** The line under the tyres: "Recommended 2.9 bar (from CAN)." or "No recommended pressure known." */
export function recommendedLine(now) {
  const v = tyreView(now);
  const value = recommendedValue(v);
  return value == null ? t('No recommended pressure known.') : t('Recommended {value} ({from}).', { value, from: recommendedFrom(v) });
}

const GUESS_FROM = {
  can: 'Wheel size taken from the factory tyre.',
  vin: 'Wheel size guessed from the VIN.',
  tesla_api: 'Wheel size taken from the Tesla API.',
  teslamate: 'Wheel size taken from the import.',
};

/**
 * While the placard's pressure is for a wheel size that is only a guess: { said: "Wheel size guessed from the
 * VIN.", ask: "Not 21″?", action: "Set the wheel size" } (pages link the action to where an admin types it);
 * else null.
 */
export function wheelGuessLine(now) {
  const v = tyreView(now);
  if (!v.wheelGuess) return null;
  return {
    said: t(GUESS_FROM[v.wheelFrom] || 'Wheel size guessed.'),
    ask: t('Not {inch}″?', { inch: v.wheelIn }),
    action: t('Set the wheel size'),
  };
}

/**
 * The hover title of the tyres line: how a tyre is judged, and, while which wheel each sensor sits on is not known
 * (nothing the owner can change: the car's input does not say), that each is judged against the lower axle's.
 */
export function tyresTitle(now) {
  const v = tyreView(now);
  const out = [];
  if (v.front != null) out.push(t('A tyre more than 10 % under its recommended pressure is flagged; a cold morning alone takes a few percent off every tyre.'));
  if (v.kpa.some((p) => p != null) && !v.mapped) {
    const split = v.front != null && v.rear != null && Math.abs(v.rear - v.front) >= 0.5;
    out.push(split ? t('Which wheel each sensor sits on is not known yet, so each tyre is judged against the lower one.') : t('Which wheel each sensor sits on is not known yet.'));
  }
  return out.join(' ');
}
