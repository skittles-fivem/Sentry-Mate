// Sentry Mate, "Pebble" UI pack: the release check's parts, shared by Settings: Display and Help (/help#about).
// The words come from shared/release.js so every pack says the same; this file only lays them out the Pebble way.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { releaseStatus, checkedAgo, changeLines } from '../shared/release.js';

const TONE_KIND = { ok: 'ok', newer: 'info', error: 'bad', wait: 'info', off: 'info' };

/** The status words and the "checked 2 h ago" for GET /release's answer: { tone, text, ago, kind }. */
export function relState(r) {
  const now = api.now();
  const s = releaseStatus(r, now);
  return { ...s, ago: checkedAgo(r, now), kind: TONE_KIND[s.tone] || 'info' };
}

/** The newer build's titles and the pull command, or null when there is no newer build. */
export function relNewer(r) {
  if (!r || r.status !== 'newer') return null;
  const lines = changeLines(r.newest);
  return h('div', { class: 'rel-newer' },
    lines.length ? h('ol', { class: 'rel-titles' }, lines.map((t) => h('li', null, t))) : null,
    r.pull ? h('p', { class: 'rel-pull' }, h('span', { class: 'rel-pull-name' }, 'To update, run on the server:'), h('code', { class: 'rel-code' }, r.pull)) : null);
}
