// The build this server runs and the release check's answer (docs/ui-api.md: /about `build`, /release), in the
// same words on every pack: "0.1.0 · build 41 · Nov 22", "Up to date", "A newer build is ready: build 42,
// Nov 21, 4 changes", "Could not check: ...". Builds are named by their number (the Image workflow's run); the
// commit is only in a hover title (buildTitle, newestTitle) and the change list. Plus the per-browser dismissal of the release notice, keyed by the
// newer build's commit (a later build shows again). Pure functions apart from the dismissal (localStorage, every
// access wrapped: a private window or blocked storage just never remembers).

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** "Nov 22", with the year when it is not `nowMs`'s year ("Nov 22, 2025"). UTC: a build has no car's zone. */
export function buildDate(ms, nowMs = Date.now()) {
  if (!Number.isFinite(ms)) return '';
  const d = new Date(ms);
  const day = `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}`;
  return d.getUTCFullYear() === new Date(nowMs).getUTCFullYear() ? day : `${day}, ${d.getUTCFullYear()}`;
}

const buildNumber = (n) => (Number.isSafeInteger(n) && n >= 0 ? n : null);

/** "0.1.0 · build 41 · Nov 22"; a build with no number (a local one): "0.1.0 · dev build". `about` is GET /about. */
export function buildLabel(about, nowMs = Date.now()) {
  const b = about?.build;
  const version = b?.version || about?.version || '';
  const n = b && !b.dev ? buildNumber(b.number) : null;
  if (n == null) return `${version} · dev build`.replace(/^ · /, '');
  const parts = [version, `build ${n}`];
  if (Number.isFinite(b.built_ms)) parts.push(buildDate(b.built_ms, nowMs));
  return parts.filter(Boolean).join(' · ');
}

const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

/** "4 changes" / "1 change". */
export const changesText = (n) => plural(Number(n) || 0, 'change', 'changes');

/** The hover title of this build: "commit 2f44a52" (empty for a local build). Put it on the element showing buildLabel. */
export function buildTitle(about) {
  const b = about?.build;
  return b && !b.dev && b.short ? `commit ${b.short}` : '';
}

/** "build 42, Nov 21, 4 changes" for GET /release's `newest` (no number known: "Nov 21, 4 changes"). */
export function newestText(newest, nowMs = Date.now()) {
  if (!newest) return '';
  const n = buildNumber(newest.build);
  return [n == null ? '' : `build ${n}`, buildDate(newest.date_ms, nowMs), changesText(newest.changes)].filter(Boolean).join(', ');
}

/** The hover title of a newer build: "commit 9c1e7d3". */
export const newestTitle = (newest) => (newest?.short ? `commit ${newest.short}` : '');

/**
 * The one-line status of GET /release: { tone: 'off'|'wait'|'ok'|'newer'|'error', text }.
 *   off: "Off" · never: "Not checked yet" · checking: "Checking…" · up_to_date: "Up to date"
 *   newer: "A newer build is ready: build 42, Nov 21, 4 changes" · error: "Could not check: <message>"
 */
export function releaseStatus(r, nowMs = Date.now()) {
  switch (r?.status) {
    case 'up_to_date': return { tone: 'ok', text: 'Up to date' };
    case 'newer': return { tone: 'newer', text: `A newer build is ready: ${newestText(r.newest, nowMs)}` };
    case 'error': return { tone: 'error', text: `Could not check: ${r.error?.message || 'no answer'}` };
    case 'checking': return { tone: 'wait', text: 'Checking…' };
    case 'never': return { tone: 'wait', text: 'Not checked yet' };
    default: return { tone: 'off', text: 'Off' };
  }
}

/** "checked 2 h ago" (or "" when never checked). */
export function checkedAgo(r, nowMs = Date.now()) {
  if (!Number.isFinite(r?.checked_ms)) return '';
  const s = Math.max(0, Math.round((nowMs - r.checked_ms) / 1000));
  if (s < 60) return 'checked just now';
  if (s < 3600) return `checked ${Math.round(s / 60)} min ago`;
  if (s < 2 * 86400) return `checked ${Math.round(s / 3600)} h ago`;
  return `checked ${Math.round(s / 86400)} days ago`;
}

/** The titles to list beside a newer build, plus "and 3 more" when the server kept only some. */
export function changeLines(newest) {
  const lines = Array.isArray(newest?.titles) ? newest.titles.map(String) : [];
  if (newest?.more > 0) lines.push(`and ${plural(newest.more, 'more change', 'more changes')}`);
  return lines;
}

const DISMISS_KEY = 'sentry-mate.release-dismissed';

/** Whether this browser dismissed the release notice of that build (`notice.detail.commit`). */
export function releaseDismissed(notice) {
  const c = notice?.detail?.commit || notice?.detail?.version;
  if (!c) return false;
  try { return globalThis.localStorage?.getItem(DISMISS_KEY) === c; } catch { return false; }
}

/** Remembers, in this browser only, that the release notice of that build was dismissed. */
export function dismissRelease(notice) {
  const c = notice?.detail?.commit || notice?.detail?.version;
  if (!c) return;
  try { globalThis.localStorage?.setItem(DISMISS_KEY, c); } catch { /* storage blocked: shown again next time */ }
}
