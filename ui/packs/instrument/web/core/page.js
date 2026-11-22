// Building blocks every page uses: the page head, panels, figures, the CSV button, links into the app.
// They only make elements (shared/dom.js h()), so data is always text and the page policy holds.

import { h, cover } from '../shared/dom.js';
import { quantity } from '../shared/dom.js';
import { linkTo } from '../shared/state.js';
import { icon } from './icons.js';

/**
 * The head of a page: eyebrow (small line above), the title, the line under it, actions on the right.
 *   pageHead({ eyebrow: car.name, title: 'Drives', sub: ['Oct 19 – Nov 17', 'times in PST'], actions: [btn] })
 * sub: text, an element, or a list of them (shown with dots between). purpose: the plan's purpose line.
 */
export function pageHead({ eyebrow = null, title, titleExtra = null, sub = null, purpose = null, actions = null, back = null, cls = null } = {}) {
  const subs = sub == null ? [] : (Array.isArray(sub) ? sub : [sub]).filter((x) => x != null && x !== '');
  const subEls = [];
  subs.forEach((s, i) => { if (i) subEls.push(h('span', { class: 'sep', 'aria-hidden': 'true' })); subEls.push(typeof s === 'string' ? h('span', null, s) : s); });
  return h('div', { class: ['page-head', cls] },
    h('div', { class: 'page-head-text' },
      back ? h('a', { class: 'back', href: back.href }, icon('arrow-l', 'ico--sm'), back.label) : null,
      eyebrow ? h('span', { class: 'eyebrow' }, eyebrow) : null,
      h('h1', { class: 'page-title' }, title, titleExtra ? [' ', titleExtra] : null),
      subEls.length ? h('p', { class: 'page-sub' }, subEls) : null,
      purpose ? h('p', { class: 'page-purpose' }, purpose) : null),
    actions ? h('div', { class: 'page-actions' }, actions) : null);
}

/**
 * A panel (frosted card) with an optional head: panel({ title: 'Battery', icon: 'battery', side: link,
 * cover: 'bh.soc.current', pad: true }, ...children). side: elements at the right of the head.
 */
export function panel(opts = {}, ...children) {
  const { title = null, icon: ic = null, side = null, pad = true, cls = null, label = null, id = null } = opts;
  const el = h('section', { class: ['panel', pad && 'panel-pad', cls], 'aria-label': label || (typeof title === 'string' ? title : null), id },
    title ? h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, ic ? icon(ic) : null, title), side) : null,
    children);
  if (opts.cover) cover(el, opts.cover);
  return el;
}

/** A link in a panel head: panelLink(linkTo('timeline'), 'Timeline'). */
export const panelLink = (href, text, attrs = {}) => h('a', { class: 'panel-link', href, ...attrs }, text, icon('chev-r', 'ico--sm'));

/**
 * A figure: a label, the value with its unit, and a line under it that says how it was worked out.
 *   metric('Distance', 12.3, 'distance', { explain: 'By odometer', size: 'sm', cover: 'ds.stat.distance_total' })
 * value may also be an element or text (kind null). A null value shows a dash and opts.why.
 */
export function metric(label, value, kind = null, opts = {}) {
  const { explain = null, size = null, why = null, digits, sign, cls = null } = opts;
  const v = kind && (value == null || typeof value === 'number') ? quantity(value, kind, { why, digits, sign })
    : value == null || value === '' ? quantity(null, 'number', { why }) : value;
  const el = h('div', { class: ['metric', size && `metric--${size}`, cls] },
    h('span', { class: 'metric-label' }, label),
    h('span', { class: 'metric-value' }, v),
    explain ? h('span', { class: 'explain' }, explain) : null);
  if (opts.cover) cover(el, opts.cover);
  return el;
}

/** A row of figures that wraps by itself: figures([metric(...), metric(...)]). */
export const figures = (items, cls = null) => h('div', { class: ['figures', cls] }, items);

/**
 * The Download CSV button. href: a server CSV link (api.csvUrl(path, params)); or onClick for CSV
 * built in the browser (shared/csv.js downloadCsv). Every table and chart has one.
 */
export function csvButton({ href = null, onClick = null, label = 'Download CSV', cover: cv = null, filename = null } = {}) {
  const attrs = { class: 'btn btn--sm csv-btn', cover: cv };
  return href
    ? h('a', { ...attrs, href, download: filename || '' }, icon('download', 'ico--sm'), label)
    : h('button', { ...attrs, type: 'button', onclick: onClick }, icon('download', 'ico--sm'), label);
}

/** A link to another page that carries the car and period: appLink('drives', 'Drives', { car }). */
export const appLink = (page, text, params = {}, cls = 'link') => h('a', { class: cls, href: linkTo(page, params) }, text);

/** A small explanatory line: note('Energy is measured at the pack.'). */
export const note = (...children) => h('p', { class: 'note' }, children);

/** A pill: pill('Plugged in', { tone: 'warm', icon: 'plug' }). tone: live | warm | plain. */
export const pill = (text, { tone = 'plain', ic = null, lg = false } = {}) =>
  h('span', { class: ['pill', `pill--${tone}`, lg && 'pill--lg'] }, ic ? icon(ic) : h('span', { class: 'dot', 'aria-hidden': 'true' }), text);

/** A tag: tag('Estimated', 'warm'). */
export const tag = (text, tone = 'plain') => h('span', { class: ['tag', `tag--${tone}`] }, text);

/** A key-value list: kv([['Odometer', q(...)], ['12 V battery', '13.6 V']], { icons: ['odo', 'battery'] }). */
export function kv(rows) {
  return h('dl', { class: 'kv' }, rows.filter(Boolean).map((r) => {
    const [label, value, opts = {}] = r;
    const row = h('div', { class: 'kv-row', title: opts.title || null },
      h('dt', null, opts.icon ? icon(opts.icon) : null, label), h('dd', null, value));
    if (opts.cover) cover(row, opts.cover);
    return row;
  }));
}

/** The 12-column grid: grid(span(8, a), span(4, b)). */
export const grid = (...children) => h('div', { class: 'grid' }, children);
export const span = (n, ...children) => h('div', { class: `span-${n}` }, children);
