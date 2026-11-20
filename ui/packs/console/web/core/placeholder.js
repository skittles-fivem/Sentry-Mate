// A page that a later batch builds: its header line, what it will show, and the data-ready signal.
//
//   import { placeholder } from '../core/placeholder.js';
//   export const id = 'battery';
//   export const mount = placeholder();

import { h } from '../shared/dom.js';
import { head, paint, section, kv } from './ui.js';
import { tag } from './fmt.js';

export function placeholder() {
  return async function mount(ctx) {
    paint(ctx,
      head(ctx),
      section({ title: 'this page' },
        kv([
          ['page', ctx.id],
          ['shows', ctx.info.desc],
          ['status', [tag('todo', 'warn'), ' being built']],
          ctx.car ? ['car', ctx.car.name || ctx.car.id] : null,
          ctx.period ? ['period', ctx.period.label] : null,
        ], { cols: 1 }),
        h('p', { class: 'note' }, 'Every other page works already: press ctrl k (or /) for the command list, or ? for the keys.')));
    ctx.ready();
  };
}
