// Imported rows moved to another car, and the rows that wait for one (docs/ui-api.md, Import: "Moves and rows that
// need a car"), in words every pack shows the same:
//
//   moveSummary(job, carName, num)   "1,840 rows moved to Maple; 620 rows need a car" (or '' when nothing moved)
//   waiting(holds)                   the lots still to give a car (waiting, or left aside)
//   holdWhy(h)                       why a lot waits, in a sentence
//   HOLD_ACTIONS                     the choices besides a car: [value, label]
//   holdSaid(out, carName, num)      what a POST /import-holds/{hold} did, for a flash

const plain = (n) => String(n ?? 0);

/** The short summary of what an import's rows did after the matching: moved to which car, and how many wait. */
export function moveSummary(job, carName = (c) => c, num = plain) {
  const moves = Array.isArray(job?.moves) ? job.moves : [];
  const holds = Array.isArray(job?.needs_car) ? job.needs_car : [];
  const to = new Map();
  for (const m of moves) {
    if (!m || !m.to_car) continue;
    const name = m.to_name || carName(m.to_car);
    to.set(name, (to.get(name) || 0) + (m.rows || 0));
  }
  const parts = [...to].map(([name, rows]) => `${num(rows)} rows moved to ${name}`);
  const wait = holds.filter((h) => h && h.status !== 'assigned');
  const waitRows = wait.filter((h) => h.status === 'waiting').reduce((a, h) => a + (h.rows || 0), 0);
  const asideRows = wait.filter((h) => h.status === 'ignored').reduce((a, h) => a + (h.rows || 0), 0);
  if (waitRows) parts.push(`${num(waitRows)} rows need a car`);
  if (asideRows) parts.push(`${num(asideRows)} rows left aside`);
  return parts.join('; ');
}

/** The lots that still have no car (waiting first, then those left aside), oldest first. */
export function waiting(holds) {
  const list = (Array.isArray(holds) ? holds : []).filter((h) => h && h.status !== 'assigned');
  return list.sort((a, b) => (a.status === b.status ? 0 : a.status === 'waiting' ? -1 : 1));
}

/** Why a lot waits for a car. */
export function holdWhy(h) {
  if (h?.why === 'odometer') return 'Their odometer reads above the car\'s own later drives, so they can\'t be that car\'s.';
  return 'They came without a VIN and their odometer or time does not carry on from any car in the file.';
}

/** `n rows`, `1 drive`, `2 charges`: a count with its word. */
export function counted(n, one, num = plain) {
  return `${num(n)} ${n === 1 ? one : `${one}s`}`;
}

export const HOLD_ACTIONS = Object.freeze([['new', 'A new car'], ['ignore', 'Leave aside (kept)'], ['wait', 'Keep waiting']]);

/** What giving a lot a car (or not) did, in one sentence. */
export function holdSaid(out, carName = (c) => c, num = plain) {
  const m = out?.move;
  if (m) return `${num(m.rows)} rows moved to ${m.to_name || carName(m.to_car)}; it is being rebuilt.`;
  if (out?.hold?.status === 'ignored') return 'Left aside. The rows are kept and can still be given a car.';
  return 'The rows wait for a car again.';
}
