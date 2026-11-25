// How old TeslaFi's data is and whose problem it is (docs/ui-api.md: `/cars/{car}/now` `inputs`, the notices
// `teslafi_no_data` and `teslafi_time_offset`), in the same words on every pack. TeslaFi's polls carry TeslaFi's own
// time of its data from the car (`last_ms` of its input); `received_ms` is when this server read it. A connection that
// polls without errors says nothing about that data, so a car page says it:
//
//  - no new data: TeslaFi's newest data is 2 h or more old (by when it came) while the car is not asleep: the car is
//    not sending TeslaFi anything newer, the problem is on TeslaFi's side. A sleeping car is silent as expected.
//  - times in the wrong zone: the data came in the last 10 min but is stamped an hour or more before (or after) the
//    moment it came: the account's time zone is not TeslaFi's own.
//
// The thresholds are the server's (crates/teslafi/src/health.rs). Pure functions: a page renders them in its own look.

/** No new data from an awake car for this long (2 h). */
export const NO_DATA_AFTER_MS = 2 * 3_600_000;
/** Data that came this recently is "still coming" (10 min). */
export const FRESH_MS = 10 * 60_000;
/** Data stamped this far from when it came is read in the wrong zone (1 h). */
export const TIME_OFFSET_MIN_MS = 3_600_000;

/** A span in words: "45 min", "6 h", "2 h 30 min", "3 days". */
export function spanText(ms) {
  const h = Math.floor(ms / 3_600_000), m = Math.floor(ms / 60_000) % 60;
  if (h >= 48) return `${Math.floor(h / 24)} days`;
  if (h === 0) return `${m} min`;
  if (m === 0 || h >= 10) return `${h} h`;
  return `${h} h ${m} min`;
}

/**
 * What the age of TeslaFi's data says, from the car's Now: `inputs` is `now.inputs`, `state` the car's state word
 * (`asleep` is silent), `nowMs` this computer's clock corrected to the server's. Returns `null` (nothing to say) or
 * { kind: 'no_data' | 'time_offset', ms, text } where `ms` is the age or the offset.
 */
export function dataNote(inputs, state, nowMs) {
  const fi = (inputs || []).find((i) => i && i.kind === 'teslafi_api');
  if (!fi || fi.last_ms == null) return null;
  const came = fi.received_ms ?? fi.last_ms;
  if (nowMs - came <= FRESH_MS) {
    const skew = Math.abs(came - fi.last_ms);
    if (skew < TIME_OFFSET_MIN_MS) return null;
    const behind = came >= fi.last_ms;
    return {
      kind: 'time_offset',
      ms: skew,
      text: `TeslaFi's newest data is stamped ${spanText(skew)} ${behind ? 'before' : 'after'} the moment this server read it, so its values look ${behind ? 'old' : 'to come'}. The account's time zone is probably not TeslaFi's own: check it in Settings, TeslaFi.`,
    };
  }
  const age = nowMs - came;
  if (age >= NO_DATA_AFTER_MS && state !== 'asleep') {
    return {
      kind: 'no_data',
      ms: age,
      text: `TeslaFi has had no new data from the car for ${spanText(age)}. The server reads TeslaFi without errors, so the car is not sending TeslaFi anything newer: the problem is on TeslaFi's side, not here.`,
    };
  }
  return null;
}
