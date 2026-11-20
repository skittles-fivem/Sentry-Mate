// Sentry Mate, "Pebble" UI pack: a page that is not built yet.
//
// placeholder() gives a page module ({ title, mount }) that shows what the page is for, the car and the period
// it would show (the frame works as on a finished page: the car switcher keeps the page, the period sheet
// changes it), in the pack's own tiles. It sets data-ready like any page, so the checker can open it. A finished
// page replaces the import with its own module.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { periodLabel } from '../shared/time.js';
import { tile, ring, stateTag, note } from './ui.js';
import { rangeText } from './period.js';

function render(ctx) {
  const tiles = [
    tile({ w: 2, title: ctx.info.title, icon: ctx.info.icon, cls: 'tile-soon' },
      h('p', { class: 'lede' }, ctx.info.purpose),
      note('This page comes in a later step of the Pebble pack. The dock, the car switcher and the period already work here.')),
  ];
  if (ctx.car) {
    const n = ctx.car.now || {};
    tiles.push(tile({ w: 1, title: 'Car', icon: 'car', cls: 'tile-center' },
      ring(n.soc_pct, { label: 'Battery', size: 's' }),
      h('p', { class: 'tile-name' }, ctx.car.name),
      stateTag(n.state || 'unknown', { stale: !!n.stale })));
  }
  if (ctx.period) {
    tiles.push(tile({ w: 1, title: 'Period', icon: 'calendar' },
      h('p', { class: 'tile-big' }, periodLabel(ctx.period, ctx.tz, api.now())),
      ctx.period.preset !== 'custom' ? h('p', { class: 'tile-line' }, rangeText(ctx.period, ctx.tz)) : null,
      note(`Days in ${ctx.tz.replace(/_/g, ' ')} time.`)));
  }
  ctx.view.replaceChildren(...tiles);
  ctx.ready();
}

/** The module of a page still to be built. A new period draws it again. */
export function placeholder() {
  return { mount(ctx) { render(ctx); return { update: render }; } };
}
