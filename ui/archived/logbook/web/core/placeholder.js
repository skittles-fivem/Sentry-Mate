// Sentry Mate, "Logbook" UI pack: a page that is not built yet.
//
// placeholder('trip') gives a page module ({ id, title, mount }) that shows the page's name, the purpose
// line from the page plan, the car and period it would show (the frame works as on a finished page: the
// car switcher keeps it, the period picker changes it) and the sections it will hold. It sets data-ready
// like any page, so the checker can open it. A finished page replaces the import with its own module.

import { h } from '../shared/dom.js';
import { periodLabel } from '../shared/time.js';
import * as api from '../shared/api.js';
import { pageHead, plannedSections } from './ui.js';
import { groupOf } from './pages.js';
import { rangeText } from './period.js';

/** The car and period a page shows, in one quiet line. */
export function frameLine(ctx) {
  const items = [];
  if (ctx.car) items.push(['Car', `${ctx.car.name} (${[ctx.car.year, ctx.car.model].filter(Boolean).join(' ')})`]);
  if (ctx.period) {
    items.push(['Period', periodLabel(ctx.period, ctx.tz, api.now())]);
    if (ctx.period.preset !== 'custom') items.push(['Dates', rangeText(ctx.period, ctx.tz)]);
    items.push(['Time zone', ctx.tz.replace(/_/g, ' ')]);
  }
  if (!items.length) return null;
  return h('dl', { class: 'frame-line' }, items.map(([k, v]) => h('div', null, h('dt', null, k), h('dd', null, v))));
}

/** A page module for a page still to be built. A new period or car draws it again. */
export function placeholder(id) {
  function render(ctx) {
    ctx.main.replaceChildren(
      pageHead({ eyebrow: ctx.car ? ctx.car.name : (groupOf(id) || {}).name, title: ctx.info.name, deck: ctx.info.purpose }),
      frameLine(ctx) || '',
      plannedSections(ctx.info.sections, {
        title: 'What this page will show',
        noteText: 'This page is not built yet. The car and the period above already follow the car switcher and the period picker.',
      }));
    ctx.ready();
  }
  return {
    id,
    title: (ctx) => ctx.info.name,
    mount(ctx) { render(ctx); return { update: render }; },
  };
}
