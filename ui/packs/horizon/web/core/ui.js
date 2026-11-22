// Building blocks every Horizon page uses: icons, HUD panels, the title plate, readouts, buttons, choices,
// tags, notes. Everything is built with h() from shared/dom.js: no innerHTML, no inline styles (dynamic
// sizes go through CSS custom properties, `vars`).

import { h, svg, quantity, cover as markCover } from '../shared/dom.js';

/** A sprite icon: icon('bolt', 'i--sm'). Ids are the <symbol id="i-..."> in index.html. */
export function icon(id, cls = '') {
  return svg('svg', { class: `i i-${id}${cls ? ` ${cls}` : ''}`, 'aria-hidden': 'true', focusable: 'false' }, svg('use', { href: `#i-${id}` }));
}

/** The brand mark (the sun over the grid). */
export const logo = () => icon('logo');

/**
 * A HUD panel: dense dark glass, a thin neon ring and corner brackets.
 * hud({ title, icon, side, tone: 'cyan' | 'orange' | 'plain', pad, cls, id, cover, label }, ...children)
 * With a title it gets a head row (title at the left, `side` at the right) and a body.
 */
export function hud(opts = {}, ...children) {
  const { title, icon: ic, side, tone, pad = true, cls, id, cover, label } = opts;
  const head = title ? h('div', { class: 'hud-head' },
    h('h2', { class: 'hud-title' }, ic ? icon(ic) : null, h('span', { class: 'ell' }, title)),
    side ? h('div', { class: 'hud-side' }, side) : null) : null;
  const body = title ? h('div', { class: pad ? 'hud-body' : null }, children) : children;
  return h('section', {
    class: ['hud', tone && `hud--${tone}`, !title && pad && 'hud-pad', cls], id, cover,
    'aria-label': label || (typeof title === 'string' ? title : null),
  }, head, body);
}

/** The title plate at the top of a page: kicker (band and car), title, aim, tools at the right. */
export function plate({ kicker, title, aim, tools, extra, cover } = {}) {
  return h('header', { class: 'hud plate', cover },
    h('div', { class: 'plate-top' },
      h('div', { class: 'plate-words' },
        kicker ? h('p', { class: 'plate-kicker' }, kicker) : null,
        h('h1', { class: 'plate-title chrome' }, title)),
      tools ? h('div', { class: 'plate-tools' }, tools) : null),
    aim ? h('p', { class: 'plate-aim' }, aim) : null,
    extra || null);
}

/**
 * A readout: a label, the value with its unit (in the person's units), and a line on how it was worked out.
 * readout('Range', car.now.range_km, 'range', { how, why, size: 'sm' | 'lg', digits, cover })
 * kind null: value is text or an element as it is.
 */
export function readout(label, value, kind, opts = {}) {
  const v = kind ? quantity(value, kind, { why: opts.why, digits: opts.digits, sign: opts.sign }) : (value ?? '—');
  return h('div', { class: ['readout', opts.size && `readout--${opts.size}`], cover: opts.cover },
    h('span', { class: 'readout-label' }, label),
    h('span', { class: 'readout-value' }, v),
    opts.how ? h('span', { class: 'readout-how' }, opts.how) : null);
}

/** A wrapping row of readouts. */
export const readouts = (...items) => h('div', { class: 'readouts' }, items);

/** A button: btn('Save', onClick, { kind: 'go' | 'ghost' | 'danger', icon, sm, type, label, cover, disabled }). */
export function btn(text, onclick, o = {}) {
  return h('button', {
    class: ['btn', o.kind && `btn--${o.kind}`, o.sm && 'btn--sm', !text && 'btn--icon', o.cls],
    type: o.type || 'button', onclick, 'aria-label': o.label || null, title: o.title || o.label || null, cover: o.cover, disabled: !!o.disabled,
  }, o.icon ? icon(o.icon, o.sm ? 'i--sm' : '') : null, text ? h('span', null, text) : null);
}

/** A link that looks like a button. */
export function linkBtn(text, href, o = {}) {
  return h('a', { class: ['btn', o.kind && `btn--${o.kind}`, o.sm && 'btn--sm', o.cls], href, cover: o.cover }, o.icon ? icon(o.icon, o.sm ? 'i--sm' : '') : null, h('span', null, text));
}

/**
 * Preset buttons, one pressed: seg([['full', 'Full'], ...], now, (v) => ..., { label }). Returns { el, set(v) }.
 */
export function seg(options, value, onPick, { label, cls, cover } = {}) {
  const el = h('div', { class: ['seg', cls], role: 'group', 'aria-label': label || null, cover });
  const set = (v) => { for (const b of el.children) b.setAttribute('aria-pressed', String(b.dataset.value === String(v))); };
  for (const [v, text, ic] of options) {
    el.append(h('button', { class: 'seg-btn', type: 'button', dataset: { value: String(v) }, 'aria-pressed': String(String(v) === String(value)), onclick: () => { set(v); onPick(v); } },
      ic ? icon(ic, 'i--sm') : null, text));
  }
  return { el, set };
}

/** A small tag: tag('Imported', 'org'). */
export const tag = (text, tone) => h('span', { class: ['tag', tone && `tag--${tone}`] }, text);

/** A state lamp: lamp('driving', { stale }). */
export const lamp = (state, { stale = false } = {}) => h('span', { class: ['lamp', stale && 'is-stale'], dataset: { state: state || 'unknown' }, 'aria-hidden': 'true' });

/** A key-value list: kv([[label, value, { cover }], ...]). */
export function kv(rows) {
  return h('dl', { class: 'kv' }, rows.filter(Boolean).map(([k, v, o = {}]) => [h('dt', { cover: o.cover }, k), h('dd', null, v ?? '—')]));
}

/** A boxed line: callout('warn' | 'ok' | 'info', head, ...text). */
export function callout(tone, head, ...text) {
  return h('div', { class: ['callout', tone !== 'info' && `callout--${tone}`], role: tone === 'warn' ? 'alert' : null },
    icon(tone === 'ok' ? 'check' : tone === 'warn' ? 'warn' : 'info'),
    h('div', null, h('p', { class: 'callout-head' }, head), text.length ? h('p', { class: 'callout-text' }, text) : null));
}

/** A short note with an icon. */
export const note = (...text) => h('p', { class: 'note' }, icon('info', 'i--sm'), h('span', null, text));

/** A progress bar: bar(0.4) (0..1), with an aria label. */
export function bar(fraction, { label = 'Progress', warm = false } = {}) {
  const pct = Math.max(0, Math.min(100, (Number(fraction) || 0) * 100));
  return h('div', { class: ['bar', warm && 'bar--warm'], role: 'progressbar', 'aria-label': label, 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': String(Math.round(pct)) },
    h('i', { vars: { '--w': `${pct.toFixed(1)}%` } }));
}

/** Mark an element with coverage ids (re-exported for pages). */
export const cover = markCover;

/** Place a popover next to its anchor, inside the window: below (or above) and aligned to its start or end. */
export function place(pop, anchor, { side = 'below', align = 'start' } = {}) {
  const a = anchor.getBoundingClientRect();
  const p = pop.getBoundingClientRect();
  const m = 8;
  const vw = document.documentElement.clientWidth, vh = document.documentElement.clientHeight;
  let x = align === 'end' ? a.right - p.width : align === 'centre' ? a.left + (a.width - p.width) / 2 : a.left;
  x = Math.max(m, Math.min(x, vw - p.width - m));
  let y = side === 'above' ? a.top - p.height - 6 : a.bottom + 6;
  if (side === 'below' && y + p.height > vh - m) y = Math.max(m, a.top - p.height - 6);
  if (side === 'above' && y < m) y = Math.min(vh - p.height - m, a.bottom + 6);
  pop.style.setProperty('--pop-x', `${Math.round(x)}px`);
  pop.style.setProperty('--pop-y', `${Math.round(Math.max(m, y))}px`);
}
