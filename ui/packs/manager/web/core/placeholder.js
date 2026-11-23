// A page that is planned but not built yet: its title, the purpose line from the page plan, what it will
// hold, and the car and period it would show (so the frame's car switcher and period picker can be tried
// on it). Each pages/<id>.js that is not built yet is three lines:
//   import { placeholder } from '../core/placeholder.js';
//   export const id = 'battery';
//   export const mount = placeholder();

import { h, svg, replace } from '../shared/dom.js';
import { formatDateRange, zoneName } from '../shared/time.js';
import * as api from '../shared/api.js';
import { icon } from './icons.js';
import { pageHead, appLink } from './page.js';
import { GROUPS } from './pages.js';

/** mount() for a placeholder page. opts.links: [[page id, text]] to pages that already show some of it. */
export function placeholder({ links = [] } = {}) {
  return async function mount(ctx) {
    const { info, car, period, tz } = ctx;
    const group = GROUPS.find((g) => g.id === info.group);
    const now = api.now();
    const frameRows = [];
    if (car) frameRows.push(['Car', h('span', null, car.name, h('span', { class: 'faint' }, ` · ${car.model} ${car.year}`))]);
    else if (info.car === 'optional') frameRows.push(['Cars', 'Every car you may see (open a car to see one)']);
    if (period) {
      frameRows.push(['Period', h('span', null, period.preset === 'custom' ? 'Custom' : period.label, h('span', { class: 'faint' },
        period.from_ms != null ? ` · ${formatDateRange(period.from_ms, Math.max(period.from_ms, period.to_ms - 1), tz)}` : ' · since the first data'))]);
      frameRows.push(['Time zone', `${tz.replace(/_/g, ' ')} (${zoneName(now, tz)})`]);
    }
    replace(ctx.view,
      pageHead({ eyebrow: car ? car.name : group?.title || null, title: info.title, purpose: info.purpose }),
      h('div', { class: 'soon' },
        h('section', { class: 'panel soon-copy', 'aria-label': 'What this page will show' },
          h('div', { class: 'soon-icon', 'aria-hidden': 'true' }, icon(info.icon)),
          h('h2', null, 'Being built'),
          h('p', null, info.desc),
          h('ul', { class: 'soon-plan' }, (info.sections || []).map((s) => h('li', null, icon('check'), s))),
          links.length ? h('p', { class: 'note' }, 'Meanwhile: ', links.flatMap(([p, t], i) => [i ? ', ' : '', appLink(p, t)]), '.') : null,
          h('p', { class: 'soon-note' }, icon('info', 'ico--sm'), 'This page is planned. Its sections follow the page plan.')),
        h('section', { class: 'panel soon-preview', 'aria-label': 'Car and period' },
          frameRows.length ? h('dl', { class: 'soon-frame' }, frameRows.map(([k, v]) => [h('dt', null, k), h('dd', null, v)])) : h('p', { class: 'note' }, 'This page does not depend on a car or a period.'),
          h('div', { class: 'ph-row', 'aria-hidden': 'true' }, [0, 1, 2, 3].map(() => h('div', { class: 'ph-tile' }))),
          h('div', { class: 'ph-chart', 'aria-hidden': 'true' }, svg('svg', { viewBox: '0 0 600 200', preserveAspectRatio: 'none' },
            svg('path', { class: 'fill', d: 'M0 160 C80 140 120 80 200 90 S320 150 380 120 S500 40 600 70 L600 200 L0 200 Z' }),
            svg('path', { d: 'M0 160 C80 140 120 80 200 90 S320 150 380 120 S500 40 600 70' }))),
          h('div', { class: 'ph-lines', 'aria-hidden': 'true' }, h('div', { class: 'ph-line' }), h('div', { class: 'ph-line' }), h('div', { class: 'ph-line' })))));
    ctx.ready();
  };
}
