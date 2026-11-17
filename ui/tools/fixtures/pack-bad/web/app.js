// Checker fixture: bad. Each page breaks one or more checks of ui/tools/check.js on purpose, so the
// self-test can see that every kind of failure is reported:
//   fleet    a missing item, an item of another page, an unknown item, a console error, a CSP
//            violation by an inline style attribute, document overflow, 8 px text
//   drives   a page error, a failed /api request, an outside request, a CSP violation in a worker
//   drive    data-ready set to error:drive
//   charge   data-ready never set
//   health   /cars/{car}/health reports another page's id; /system leaves a health item unmarked
//   sign-in  an /api answer the sign-in page does not expect (403; its 401 is expected)
//   setup    nothing wrong: one page that passes in a pack that fails

const root = document.documentElement;
const view = document.getElementById('view');
const title = document.querySelector('.title');

function el(tag, attrs = {}, text = '') {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text) e.textContent = text;
  return e;
}

function card(cover, heading, lines) {
  const c = el('section', { class: 'card', 'data-cover': cover });
  c.append(el('h2', {}, heading));
  for (const l of lines) c.append(el('p', {}, l));
  return c;
}

function frame(on) {
  document.querySelector('.nav').dataset.cover = on ? 'fx.frame.nav' : '';
  title.dataset.cover = on ? 'fx.frame.title' : '';
  if (!on) document.querySelector('.nav').hidden = true;
}

const later = (ms) => new Promise((r) => setTimeout(r, ms));

const pages = {
  async fleet() {
    // fx.fleet.summary is missing; fx.drives.table belongs to Drives; fx.nope.unknown is in no matrix
    view.append(card('fx.fleet.list fx.drives.table fx.nope.unknown', 'Cars', ['Comet', 'Maple', 'Atlas']));
    console.error('fixture console error');
    const styled = el('p', {}, 'This paragraph tries an inline style attribute.');
    styled.setAttribute('style', 'color: red'); // blocked by style-src 'self'
    view.append(styled);
    view.append(el('p', { class: 'tiny' }, 'Text at 8 px is too small to read.'));
    view.append(el('div', { class: 'wide' }, 'A strip wider than the window'));
    return 'Fleet';
  },
  async drives() {
    view.append(card('fx.drives.table', 'Drives', ['No drives shown']));
    setTimeout(() => { throw new Error('fixture page error'); }, 0);
    await fetch('/api/v1/cars/c1/no-such-thing'); // 404
    window.open('https://outside.invalid/fixture', '_blank'); // a host other than the dev server
    const code = "setTimeout(() => fetch('https://example.invalid/from-worker').catch(() => {}), 300);";
    new Worker(URL.createObjectURL(new Blob([code], { type: 'text/javascript' })));
    await later(600);
    return 'Drives';
  },
  async drive() {
    view.append(card('fx.drive.figures', 'Drive', ['This drive could not be loaded.']));
    throw new Error('fixture load error');
  },
  async charge() {
    view.append(card('fx.charge.figures', 'Charge', ['Still loading, for ever.']));
    return new Promise(() => {}); // never ready
  },
  async health(car) {
    if (car) {
      view.append(card('fx.health.car', 'Car health', ['Marks the car part.']));
      return { title: 'Data health', ready: 'fleet' }; // the wrong page id
    }
    view.append(card('', 'Server health', ['Does not mark fx.health.server.']));
    return 'Data health';
  },
  async 'sign-in'() {
    frame(false);
    await fetch('/api/v1/me'); // 401: expected on the sign-in page
    await fetch('/api/v1/session', { method: 'POST', body: '{}' }).then((r) => r.text()); // no CSRF header: 403, not expected
    view.append(card('', 'Sign in', ['You are signed out.']));
    return 'Sign in';
  },
  async setup() {
    frame(false);
    const r = await fetch('/api/v1/setup');
    const s = await r.json();
    view.append(card('', 'Set up', [s.needs_setup ? 'Create the first account.' : 'Set-up is done.']));
    return 'Set up';
  },
};

function match(path) {
  const p = path.split('/').filter(Boolean).map(decodeURIComponent);
  if (p.length === 0) return ['fleet'];
  if (p[0] === 'cars' && p[2] === 'drives' && p.length === 3) return ['drives', p[1]];
  if (p[0] === 'cars' && p[2] === 'drives' && p.length === 4) return ['drive', p[1], p[3]];
  if (p[0] === 'cars' && p[2] === 'charges' && p.length === 4) return ['charge', p[1], p[3]];
  if (p[0] === 'cars' && p[2] === 'health') return ['health', p[1]];
  if (p[0] === 'system') return ['health', null];
  if (p[0] === 'sign-in' || p[0] === 'setup') return [p[0]];
  return [null];
}

async function main() {
  delete root.dataset.ready;
  const [id, ...args] = match(location.pathname);
  if (!id) { root.dataset.ready = 'error:unknown'; return; }
  frame(true);
  try {
    const r = await pages[id](...args);
    title.textContent = typeof r === 'string' ? r : r.title;
    root.dataset.ready = typeof r === 'string' ? id : r.ready;
  } catch (e) {
    view.append(card('', 'Could not load this page', [String(e.message)]));
    root.dataset.ready = `error:${id}`;
  }
}

main();
