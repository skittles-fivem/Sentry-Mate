// Sentry Mate, "Logbook" UI pack: parts that moving a server shares between the first-run set-up screen
// (pages/setup.js) and Settings: Server and access (pages/settings-server.js): a code shown big with a Copy
// button, the progress of a copy (bytes, files, which step), a text file handed to the browser, and the
// words for the problems a move or a restore can meet. Their styles are in app.css (.mv-*).

import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { formatDuration } from '../shared/time.js';
import { button, icon } from '../core/ui.js';

export const bytes = (n) => (n == null ? '—' : format(n, 'bytes'));

/** "about 2 min" for a count of seconds (null: nothing). */
export function etaWords(s) {
  if (s == null || !Number.isFinite(s)) return null;
  if (s < 5) return 'a few seconds';
  return `about ${formatDuration(Math.round(s) * 1000)}`;
}

/** A Copy button and the line that says whether it worked. */
export function copyButton(textFn, { label = 'Copy', kind = 'quiet', cls = null } = {}) {
  const said = h('span', { class: 'mv-copied', 'aria-live': 'polite' });
  const b = button(label, {
    kind, cls,
    onclick: async () => {
      try { await navigator.clipboard.writeText(textFn()); said.textContent = 'Copied.'; } catch { said.textContent = 'This browser did not allow copying: select the text and copy it.'; }
    },
  });
  return h('span', { class: 'mv-copy' }, b, said);
}

/**
 * A secret shown big and readable, in its groups (SMMV-1K3QX-…, SMRK-…), with a Copy button. The text
 * can be selected whole with one click as well.
 */
export function bigCode(text, { label, cover = null, cls = null } = {}) {
  const groups = String(text).split('-');
  return h('div', { class: ['mv-code', cls], cover },
    label ? h('p', { class: 'mv-code-label' }, label) : null,
    h('p', { class: 'mv-code-text', 'aria-label': String(text).split('').join(' ') },
      groups.map((g, i) => h('span', { class: 'mv-code-group' }, g, i < groups.length - 1 ? h('span', { class: 'mv-code-dash' }, '-') : null))),
    copyButton(() => String(text), { label: 'Copy', kind: 'primary' }));
}

/** Hand a text file to the browser as a download. */
export function downloadText(filename, text) {
  const blob = new Blob([text], { type: 'text/plain;charset=utf-8' });
  const href = URL.createObjectURL(blob);
  const a = h('a', { href, download: filename, rel: 'noopener', hidden: true });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(href), 30000);
}

/** Start a download of an address on this server (a plain link: the browser shows it and resumes it). */
export function downloadUrl(href) {
  const a = h('a', { href, download: '', rel: 'noopener', hidden: true });
  document.body.append(a);
  a.click();
  a.remove();
}

/**
 * The steps of a copy, the one now marked: steps is [[key, name, text]], current a key (or null), done
 * true once all of them are done.
 */
export function stepList(steps, current, { done = false, problem = false } = {}) {
  const at = steps.findIndex(([k]) => k === current);
  return h('ol', { class: 'mv-steps' }, steps.map(([k, name, text], i) => {
    const st = done || (at >= 0 && i < at) ? 'done' : i === at ? (problem ? 'stuck' : 'now') : 'later';
    return h('li', { class: ['mv-step', `mv-step-${st}`], 'aria-current': st === 'now' || st === 'stuck' ? 'step' : null },
      h('span', { class: 'mv-step-mark', 'aria-hidden': 'true' }, st === 'done' ? icon('check') : st === 'stuck' ? icon('warning') : String(i + 1)),
      h('span', { class: 'mv-step-body' },
        h('span', { class: 'mv-step-name' }, name, h('span', { class: 'sr-only' }, st === 'done' ? ' (done)' : st === 'now' ? ' (now)' : st === 'stuck' ? ' (waiting)' : '')),
        (st === 'now' || st === 'stuck') && text ? h('span', { class: 'mv-step-text' }, text) : null));
  }));
}

/** A bar with how much is copied: p is the API's progress object. */
export function progressBar(p, { label = 'Copied' } = {}) {
  const done = p && p.bytes_done != null ? p.bytes_done : 0;
  const total = p && p.bytes_total ? p.bytes_total : null;
  const pct = total ? Math.max(0, Math.min(100, (done / total) * 100)) : null;
  const fill = h('span', { class: 'mv-bar-fill' });
  fill.style.setProperty('--w', pct == null ? '0%' : `${pct.toFixed(1)}%`);
  const bits = [];
  if (total) bits.push(`${bytes(done)} of ${bytes(total)}`);
  else if (done) bits.push(bytes(done));
  if (p && p.files_total) bits.push(`${format(p.files_done || 0, 'count')} of ${format(p.files_total, 'count')} files`);
  const speed = p && p.bytes_per_s ? `${bytes(p.bytes_per_s)}/s` : null;
  const eta = p ? etaWords(p.eta_s) : null;
  return h('div', { class: 'mv-progress' },
    h('div', { class: 'mv-bar', role: 'progressbar', 'aria-label': label, 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': pct == null ? null : String(Math.round(pct)) }, fill),
    h('p', { class: 'mv-progress-text' },
      h('span', { class: 'mv-progress-pct' }, pct == null ? 'Working out the size…' : `${Math.floor(pct)} %`),
      bits.length ? h('span', null, bits.join(' · ')) : null,
      speed || eta ? h('span', { class: 'mv-progress-quiet' }, [speed, eta ? `${eta} left` : null].filter(Boolean).join(' · ')) : null));
}

/** A box with a plain message: kind 'ok' | 'warn' | 'info' | 'bad'. */
export function callout(kind, title, ...body) {
  const mark = kind === 'ok' ? 'check' : kind === 'info' ? 'info' : 'warning';
  return h('div', { class: ['mv-callout', `mv-callout-${kind}`], role: kind === 'bad' ? 'alert' : null },
    h('span', { class: 'mv-callout-mark', 'aria-hidden': 'true' }, icon(mark)),
    h('div', { class: 'mv-callout-body' }, title ? h('p', { class: 'mv-callout-title' }, title) : null, body));
}

/** What a problem in a move or a restore means, and what to do. */
export const PROBLEM = {
  unreachable: ['The old server does not answer.', 'Nothing to do: it tries again by itself and carries on where it stopped. If this goes on, check that the old server is on and that its address is right.'],
  no_space: ['This server’s disk is full.', 'Make room on it (delete old files, or give the data folder more space). The copy carries on by itself when there is room.'],
  disk: ['This server could not write to its disk.', 'Check the disk and the data folder’s owner, then wait: it tries again.'],
  slow_down: ['The old server asked to slow down.', 'Nothing to do: it is busy, and the copy goes on more slowly.'],
  changing_fast: ['The old server changes faster than this copy catches up.', 'Something big runs there (an import, say). Let it end first; the copy then catches up.'],
  cancelled: ['The move was cancelled on the old server.', 'What was copied so far is still here.'],
  bad_session: ['The old server ended this move.', 'Make a new move code on the old server, give up here, and move again with the new code.'],
  not_proven: ['The old server could not prove it is the one the code came from.', 'Check the address. If it is right, make a new move code there and start again.'],
  disk_times: ['This server’s disk keeps file times only roughly.', 'Put the data folder on another disk (ZFS, ext4, xfs or btrfs), then start again. The code still works.'],
  export_damaged: ['The export file is damaged.', 'Nothing was kept. Download or copy the file again (or make a new export on the old server), then restore it again.'],
  export_cut_short: ['The export file is cut short.', 'It ends before its last part: the download or the copy stopped early. Download or copy it again, then restore it again.'],
};

/** Replace a box's content (a small helper for drawing a part again). */
export const redraw = (el, ...children) => replace(el, ...children);
