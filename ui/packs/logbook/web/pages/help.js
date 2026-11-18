// Sentry Mate, "Logbook" UI pack: Help and about (/help).
//
// Version, API version and UI pack (GET /api/v1/about), the opt-in release note (GET /api/v1/notices),
// licences and data credits (the server's, then the files this pack bundles), and short answers.

import { h } from '../shared/dom.js';
import { pageHead, icon } from '../core/ui.js';

export const id = 'help';
export const css = true;
export const title = () => 'Help and about';

/** Third-party files inside this pack, with their licences (the files sit next to them in web/). */
const BUNDLED = [
  ['MapLibre GL JS', 'BSD-3-Clause', '/vendor/maplibre-gl/LICENSE.txt'],
  ['uPlot', 'MIT', '/vendor/uplot/LICENSE'],
  ['Inter (typeface)', 'SIL Open Font License 1.1', '/fonts/LICENSE-Inter.txt'],
  ['Charter (typeface)', 'Bitstream Charter licence', '/fonts/LICENSE-Charter.txt'],
];

const QA = [
  ['Why is a car missing?', 'A car appears when its CAN Bridge uploads its first log. If it does not, check in Settings: Cars that the car has an upload token, that the token was used, and that no CAN Bridge key is waiting to be assigned or trusted. Fleet lists what is waiting.', 'settings-cars'],
  ['Why is the data late?', 'The CAN Bridge sends what it records about a minute later, and again when it has a connection. Each value shows its own time; nothing is hidden when it gets old.', 'car'],
  ['Why is a span unknown?', 'Unknown means no log covers it: the CAN Bridge was off or had no connection, recording was paused, or a log failed its check. Asleep is different: the car went quiet and the log says so. Data health lists every gap with its cause.', 'health'],
  ['How does pairing work?', 'Make an upload token for the car in Settings: Cars and enter it in the CAN Bridge app. The app signs each log with its own key; Sentry Mate checks the signature and the chain of logs before it uses them.', 'settings-cars'],
  ['Does Sentry Mate ever talk to the car or to Tesla?', 'No. It only reads what the CAN Bridge recorded. It never wakes the car, sends commands or signs in to a Tesla account.', null],
];

export async function mount(ctx) {
  const [about, notices] = await Promise.all([ctx.api.get('about'), ctx.api.get('notices').catch(() => ({ items: [] }))]);
  const release = (notices.items || []).find((n) => n.code === 'release_available');
  const links = about.links || {};
  const pack = about.ui_pack || {};
  ctx.main.append(
    pageHead({ eyebrow: 'Set-up', title: 'Help and about', deck: ctx.info.purpose }),
    h('div', { class: 'help-grid' },
      h('section', { class: 'help-block', 'aria-labelledby': 'about-title' },
        h('h2', { class: 'label', id: 'about-title' }, 'About'),
        h('dl', { class: 'about-list' },
          h('div', { cover: 'web.settings.about.version' }, h('dt', null, 'Sentry Mate'), h('dd', { class: 'mono' }, about.version)),
          h('div', null, h('dt', null, 'API version'), h('dd', { class: 'mono' }, String(about.api_version))),
          h('div', null, h('dt', null, 'UI pack'), h('dd', null, `${pack.name || 'Logbook'} ${pack.version || ''}`, pack.api ? h('span', { class: 'about-note' }, ` for API ${pack.api}`) : null)),
          h('div', { cover: 'web.settings.about.update-available' }, h('dt', null, 'Newer release'), h('dd', null, release ? release.message : 'Not checked. An admin can turn on the release check in Settings: Display and units.')),
          h('div', { cover: 'web.settings.about.source-link' }, h('dt', null, 'Source code'), h('dd', null, links.source ? h('a', { href: links.source, target: '_blank', rel: 'external noopener noreferrer' }, 'Sentry Mate’s source code') : 'This server names no source address.')),
          h('div', { cover: 'web.settings.about.docs-link' }, h('dt', null, 'Documentation'), h('dd', null, links.docs ? h('a', { href: links.docs, target: links.docs.startsWith('/') ? null : '_blank', rel: links.docs.startsWith('/') ? null : 'external noopener noreferrer' }, links.docs.startsWith('/') ? 'These help pages' : 'The documentation') : '—')))),
      h('section', { class: 'help-block', id: 'licence', 'aria-labelledby': 'licence-title', cover: ['web.legal.license', 'web.legal.notice'] },
        h('h2', { class: 'label', id: 'licence-title' }, 'Licences and notices'),
        h('p', { class: 'note' }, 'Sentry Mate’s own licence and notice files come with every copy of the program. It uses these parts, each under its own licence:'),
        h('ul', { class: 'licence-list' },
          (about.licences || []).map((l) => h('li', null, h('span', null, l.name), h('span', { class: 'licence-name' }, l.licence))),
          BUNDLED.map(([n, l, href]) => h('li', null, h('span', null, n, ' (in this UI pack)'), h('a', { class: 'licence-name', href, target: '_blank', rel: 'noopener' }, l)))),
        h('h3', { class: 'label credits-title' }, 'Data credits'),
        h('ul', { class: 'licence-list' }, (about.data_credits || []).map((c) => h('li', null, h('span', null, c.text || c.name), h('span', { class: 'licence-name' }, c.licence || ''))))),
      h('section', { class: 'help-block help-qa', 'aria-labelledby': 'qa-title' },
        h('h2', { class: 'label', id: 'qa-title' }, 'Questions and answers'),
        h('dl', { class: 'qa-list' }, QA.map(([qn, a, page]) => h('div', { class: 'qa' },
          h('dt', null, qn),
          h('dd', null, a, page ? [' ', h('a', { class: 'link-arrow', href: ctx.link(page) }, 'Go there', icon('arrowRight'))] : null)))))));
  ctx.ready();
  return {};
}
