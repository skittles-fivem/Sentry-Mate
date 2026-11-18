// Help and about (/help): version, licences, data credits, and short answers to common questions.
// GET /api/v1/about for the versions, licences and credits; GET /api/v1/notices for a release notice
// (only there when the owner turned the release check on).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, kv } from '../core/page.js';
import { getNotices } from '../core/notices.js';

export const id = 'help';
export const css = true;

// the third-party parts this pack ships, with their licence files (served from the pack itself)
const BUNDLED = [
  ['MapLibre GL JS', 'BSD-3-Clause', '/vendor/maplibre-gl/LICENSE.txt'],
  ['uPlot', 'MIT', '/vendor/uplot/LICENSE.txt'],
  ['Inter (font)', 'SIL Open Font License 1.1', '/fonts/LICENSE-Inter.txt'],
  ['JetBrains Mono (font)', 'SIL Open Font License 1.1', '/fonts/LICENSE-JetBrainsMono.txt'],
];

export async function mount(ctx) {
  let about = null, notices = getNotices();
  try {
    [about, notices] = await Promise.all([ctx.api.get('about', null, { maxAgeMs: 600000 }), ctx.api.get('notices').then((r) => r.items).catch(() => notices)]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const release = (notices || []).find((n) => n.code === 'release_available');

  const qa = [
    ['Why is a car missing?', 'A car appears when its CAN Bridge uploads its first log. If it does not, check the upload token in the CAN Bridge app, and look for a key waiting to be assigned.', 'settings-cars', 'Settings: Cars'],
    ['Why is the data a minute or more late?', 'The CAN Bridge sends finished logs, about a minute after the car records them. With no network in the car, logs wait on the device and arrive later; the pages fill in by themselves.', 'health', 'Data health'],
    ['What does "unknown" mean?', 'No log covers that time: the CAN Bridge was off, recording was paused, or a log failed its check. Unknown is never counted as asleep.', 'timeline', 'Timeline'],
    ['How does pairing work?', 'Make an upload token for the car in Settings: Cars and enter it in the CAN Bridge app. The CAN Bridge signs every log with its own key; the first key is tied to the car, and other keys wait until you assign them.', 'settings-cars', 'Settings: Cars'],
    ['Does Sentry Mate wake the car?', 'No. It only listens to the car’s own messages, so it never wakes the car or keeps it awake.', 'car', 'Car overview'],
  ];

  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'help-grid' },
      panel({ title: 'About', icon: 'info', id: 'about' },
        kv([
          ['Sentry Mate', about.version, { cover: 'web.settings.about.version' }],
          ['API version', about.api_version],
          ['UI pack', `${about.ui_pack?.name || '—'} ${about.ui_pack?.version || ''}`.trim()],
          ['Newer version', release ? h('span', { class: 'ok' }, release.message) : h('span', { class: 'faint' }, 'Release check is off, or this is the newest'), { cover: 'web.settings.about.update-available', title: 'Shown only when the owner turned the release check on (Settings: Server and access).' }],
          ['Source code', about.links?.source ? h('a', { class: 'link', href: about.links.source, target: '_blank', rel: 'noopener noreferrer external' }, about.links.source) : h('span', { class: 'faint' }, 'Not published yet'), { cover: 'web.settings.about.source-link' }],
          ['Documentation', h('a', { class: 'link', href: about.links?.docs || '/help' }, 'Help and answers'), { cover: 'web.settings.about.docs-link' }],
        ])),
      panel({ title: 'Licences and notices', icon: 'book', id: 'licences', cover: ['web.legal.notice', 'web.legal.license'] },
        h('p', { class: 'note' }, 'Each part keeps its own licence. The full texts open as plain files.'),
        h('ul', { class: 'licence-list' },
          BUNDLED.map(([name, lic, href]) => h('li', null, h('b', null, name), h('span', null, lic), h('a', { class: 'link', href, target: '_blank', rel: 'noopener' }, 'Licence text'))),
          (about.licences || []).map((l) => h('li', null, h('b', null, l.name), h('span', null, l.licence))))),
      panel({ title: 'Data credits', icon: 'globe', id: 'credits' },
        h('ul', { class: 'licence-list' }, (about.data_credits || []).map((c) => h('li', null, h('b', null, c.name), h('span', null, `${c.text}${c.licence ? ` (${c.licence})` : ''}`)))))),
    panel({ title: 'Questions and answers', icon: 'question', cls: 'qa-panel' },
      h('dl', { class: 'qa' }, qa.map(([q, a, page, label]) => [h('dt', null, q), h('dd', null, a, ' ', h('a', { class: 'link', href: state.linkTo(page) }, `Open ${label}`))]))));
  ctx.ready();
  if (location.hash) document.getElementById(location.hash.slice(1))?.scrollIntoView();
}
