// The move's log (the `log` of GET /system/move and of the move-here SetupJob): what the move did, step by step,
// newest last, each line with its time. Shown on the old server's move panel (pages/settings-server-move.js) and on
// the new server's "Moving here" page (pages/setup.js). The server keeps the last 500 lines with the move's state, so
// a reload shows them again; the pages' own polling brings the new ones. Lines are only added, never changed: a
// new line is appended in place, and the list keeps to its bottom unless the person scrolled up to read.
// "Download log" saves the lines as plain text. Nothing secret is in them (the server leaves codes, keys, tokens
// and paths out).

import { h, replace } from '../shared/dom.js';
import { icon } from '../core/icons.js';

const pad = (n) => String(n).padStart(2, '0');

/** `2026-11-17 14:02:05 UTC`, as the server's own plain text writes it. */
export function utcStamp(ms) {
  const d = new Date(ms);
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())} UTC`;
}

/** The log as plain text: one line each, its time in UTC, two spaces, its words. */
export function plainText(lines) {
  return (lines || []).map((l) => `${utcStamp(l.at_ms)}  ${l.text}\n`).join('');
}

function clock(ms, tz) {
  try {
    return new Intl.DateTimeFormat(undefined, { timeZone: tz || undefined, hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' }).format(ms);
  } catch {
    const d = new Date(ms);
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }
}

function day(ms, tz) {
  try { return new Intl.DateTimeFormat(undefined, { timeZone: tz || undefined, dateStyle: 'medium', timeStyle: 'medium' }).format(ms); } catch { return utcStamp(ms); }
}

/**
 * The log's box: `update(lines)` shows the lines given (the whole log, oldest first), adding only the new ones.
 * `file`: the downloaded file's name without `.txt`; `look`: the pages' class prefix.
 */
export function moveLog({ tz = null, file = 'sentry-mate-move-log', label = 'Move log' } = {}) {
  let lines = [];
  const list = h('ol', { class: 'mlog-list', role: 'log', 'aria-live': 'polite', 'aria-label': label, tabindex: '0' });
  const count = h('span', { class: 'mlog-count' });
  const dl = h('button', { type: 'button', class: 'btn btn--ghost btn--sm mlog-dl', disabled: true }, icon('download', 'ico--sm'), 'Download log');
  dl.addEventListener('click', () => {
    const blob = new Blob([plainText(lines)], { type: 'text/plain;charset=utf-8' });
    const href = URL.createObjectURL(blob);
    const a = h('a', { href, download: `${file}.txt`, hidden: true });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(href), 30000);
  });
  const empty = h('p', { class: 'mlog-empty faint' }, 'Nothing yet: the steps show here as the move goes.');
  const el = h('section', { class: 'mlog', 'aria-label': label },
    h('div', { class: 'mlog-head' }, h('h3', { class: 'mlog-title' }, icon('logs', 'ico--sm'), label), count, dl),
    empty, list);
  const row = (l) => h('li', { class: 'mlog-line' },
    h('time', { class: 'mlog-time', datetime: new Date(l.at_ms).toISOString(), title: day(l.at_ms, tz) }, clock(l.at_ms, tz)),
    h('span', { class: 'mlog-text' }, l.text));

  function update(next) {
    next = Array.isArray(next) ? next : [];
    const atBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 8;
    // the same log with lines added keeps what is drawn; another (a new code, the oldest dropped) is drawn anew
    const same = lines.length && next.length >= lines.length
      && next[0].at_ms === lines[0].at_ms && next[0].text === lines[0].text
      && next[lines.length - 1].at_ms === lines[lines.length - 1].at_ms;
    if (same) { if (next.length > lines.length) list.append(...next.slice(lines.length).map(row)); }
    else replace(list, next.map(row));
    lines = next.slice();
    empty.hidden = lines.length > 0;
    list.hidden = lines.length === 0;
    dl.disabled = lines.length === 0;
    count.textContent = lines.length ? `${lines.length} ${lines.length === 1 ? 'line' : 'lines'}` : '';
    if (atBottom || !same) list.scrollTop = list.scrollHeight;
  }
  update([]);
  return { el, update, lines: () => lines };
}
