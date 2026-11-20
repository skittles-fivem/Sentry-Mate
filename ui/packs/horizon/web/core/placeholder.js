// A page that a later build fills in: its plate, what it will hold, and the car and period it opened with,
// so the frame, the routes and the shared pickers can be tried on every page now.
//
//   import { placeholder } from '../core/placeholder.js';
//   export const id = 'battery';
//   export const mount = placeholder();

import { h, replace } from '../shared/dom.js';
import { icon, hud, plate, readout, readouts } from './ui.js';
import { band, fullName } from './stations.js';
import { carLine, carStatus, dateTime, zone } from './fmt.js';

export function placeholder() {
  return async function mount(ctx) {
    const s = ctx.info;
    const b = band(s.band) || band(ctx.info.parent ? 'driving' : 'cars');
    replace(ctx.view,
      plate({ kicker: [b?.full || b?.name, ctx.car?.name].filter(Boolean).join(' · '), title: fullName(s), aim: s.aim }),
      hud({ title: 'Coming to this station', icon: s.icon, tone: 'cyan' },
        h('ul', { class: 'ph-parts' }, s.parts.map((p) => h('li', { class: 'ph-part' }, icon('chev-r', 'i--sm'), h('span', null, p)))),
        readouts(
          ctx.car ? readout('Car', ctx.car.name, null, { how: `${carLine(ctx.car)} · ${carStatus(ctx.car)}` }) : null,
          ctx.period ? readout('Period', ctx.period.label || ctx.period.preset, null, { how: `${dateTime(ctx.period.from_ms, ctx.tz)} to ${ctx.period.to_ms ? dateTime(ctx.period.to_ms, ctx.tz) : 'now'} (${zone(ctx.tz)})` }) : null)));
    ctx.ready();
  };
}
