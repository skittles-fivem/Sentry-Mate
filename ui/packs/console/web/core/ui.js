// Building blocks of console pages: the header line, sections, key: value blocks, buttons, link lines and the
// text bar. Everything is plain elements with the classes in app.css; no inline styles.

import { h, replace, cover as addCover } from '../shared/dom.js';
import { range, DASH } from './fmt.js';
import { openPeriodPicker } from './period.js';
import { openCarSwitcher } from './carswitch.js';

/**
 * The one-line header every page starts with: title on the left, the period and the car on the right.
 * Both right-hand parts are buttons (the period picker, the car switcher) when the page has them.
 * extra: more elements after the title (a count, a tag).
 */
export function head(ctx, { title, extra = null, period = true, car = true } = {}) {
  const right = h('div', { class: 'ph-side' });
  if (period && ctx.period) {
    right.append(h('button', { class: 'ph-btn', type: 'button', title: 'Change the period', onclick: (e) => openPeriodPicker(e.currentTarget) },
      h('span', { class: 'muted' }, 'range '), ctx.period.from_ms != null ? range(ctx.period.from_ms, ctx.period.to_ms - 1, ctx.tz) : ctx.period.label));
  }
  if (car && ctx.car) {
    right.append(h('button', { class: 'ph-btn', type: 'button', title: 'Switch car', onclick: (e) => openCarSwitcher(e.currentTarget) },
      h('span', { class: 'muted' }, 'car '), ctx.car.name || ctx.car.id));
  }
  return h('header', { class: 'ph' }, h('h1', { class: 'ph-title' }, title ?? ctx.info?.name ?? ''), extra, right);
}

/** A section with a rule and a lowercase heading line: '## drives'. */
export function section({ title, side = null, id = null, cover = null, cls = null } = {}, ...children) {
  return h('section', { class: ['sec', cls], id, cover },
    title != null ? h('div', { class: 'sec-head' }, h('h2', { class: 'sec-title' }, title), side ? h('div', { class: 'sec-side' }, side) : null) : null,
    children);
}

/** key: value lines, two columns on wide screens, one on phones. rows: [[key, value, { cover, tone }]] */
export function kv(rows, { cols = 2, cls = null } = {}) {
  return h('dl', { class: ['kv', cols === 1 && 'kv--one', cls] }, rows.filter(Boolean).map(([k, v, o = {}]) =>
    h('div', { class: ['kv-row', o.tone && `kv-row--${o.tone}`], cover: o.cover || null },
      h('dt', { class: 'kv-k' }, k), h('dd', { class: 'kv-v' }, v == null || v === '' ? DASH : v))));
}

/** A button: kind '' | 'primary' | 'danger' | 'quiet'. */
export function btn(label, onclick, { kind = '', type = 'button', title = null, cover = null, disabled = false } = {}) {
  return h('button', { class: ['btn', kind && `btn--${kind}`], type, onclick, title, cover, disabled }, label);
}

/** A row of buttons or links, left to right. */
export const actions = (...children) => h('div', { class: 'acts' }, children);

/** A line of muted text. */
export const note = (...children) => h('p', { class: 'note' }, children);

/** A link that looks like a link (inside the app: the router follows it). */
export const link = (href, text, attrs = {}) => h('a', { class: 'lnk', href, ...attrs }, text);

/**
 * A text bar of `cells` cells for 0..100: '##########..........'. Filled cells in the tone's colour, empty
 * ones muted. Each cell is one character, so the bar is exactly cells characters wide.
 */
export function textBar(pct, { cells = 20, tone = 'accent', label = null } = {}) {
  const v = pct == null || !Number.isFinite(pct) ? null : Math.max(0, Math.min(100, pct));
  const on = v == null ? 0 : Math.round((v / 100) * cells);
  return h('span', { class: ['tbar', `tbar--${tone}`], role: 'img', 'aria-label': label || (v == null ? 'unknown' : `${Math.round(v)} percent`) },
    h('span', { class: 'tbar-on' }, '#'.repeat(on)), h('span', { class: 'tbar-off' }, (v == null ? '?' : '.').repeat(cells - on)));
}

/** Put elements into the page's view, replacing what was there. */
export const paint = (ctx, ...children) => replace(ctx.view, h('div', { class: 'page', dataset: { page: ctx.id } }, children));

/** Mark coverage items on an element (shared/dom.js cover). */
export const cover = addCover;

/**
 * Keep an element whose height is not known centred up and down on whole pixels: sets --cy on host to
 * the rounded-down top offset. Returns a stop function; its update() measures again at once.
 */
export function centreUpDown(host, box) {
  const set = () => {
    // measured without the old offset: a host that grew by it would keep part of it (and the box would jump later)
    host.style.setProperty('--cy', '0px');
    const free = host.clientHeight - box.offsetHeight;
    host.style.setProperty('--cy', `${Math.max(0, Math.floor(free / 2))}px`);
  };
  const ro = new ResizeObserver(set);
  ro.observe(host); ro.observe(box);
  set();
  // stop.update(): measure again now (after the box got its contents), so the first painted frame is already centred
  const stop = () => ro.disconnect();
  stop.update = set;
  return stop;
}
