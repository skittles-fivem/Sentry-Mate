// Checker fixture: hover. A tiny pack for `check.js --hover` (see ui/tools/README.md, "Hover checks"):
//   fleet    nothing moves: hover changes colour, shadow and transform, a row's background, an edit icon's
//            opacity; a chart's tooltip is an overlay (absolute, no pointer events); its readout has a fixed width
//   drives   each hover or focus moves something, on purpose: a button that turns bold, a row whose padding
//            grows, an edit icon that takes room, a chart tooltip in the flow, a box that gets a scrollbar, a
//            tooltip that makes the page longer than the window (a page scrollbar), a field whose border grows
// Other pages: as pack-ok. Made-up data only (the dev server's).

const root = document.documentElement;
const view = document.getElementById('view');
const title = document.querySelector('.title');
const svgNs = 'http://www.w3.org/2000/svg';

async function api(path) {
  const r = await fetch(path, { headers: { Accept: 'application/json' }, credentials: 'same-origin' });
  if (!r.ok) throw new Error(`${path}: ${r.status}`);
  return r.json();
}

function el(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  for (const k of kids) e.append(k);
  return e;
}

function chart() {
  const c = document.createElementNS(svgNs, 'svg');
  c.setAttribute('class', 'chart');
  c.setAttribute('viewBox', '0 0 320 80');
  const line = document.createElementNS(svgNs, 'polyline');
  line.setAttribute('points', '0,60 80,20 160,50 240,10 320,40');
  line.setAttribute('fill', 'none');
  line.setAttribute('stroke', '#2c6fbb');
  c.append(line);
  return c;
}

const table = (cls, rows) => el('table', {}, el('tbody', {}, ...rows.map((r) => el('tr', { class: cls }, ...r.map((x) => el('td', {}, x))))));

const pages = {
  async fleet() {
    const d = await api('/api/v1/cars/c1/drives?limit=3');
    const card = el('section', { class: 'card ok', 'data-cover': 'fx.fleet.list' }, el('h2', {}, 'Cars'));
    card.append(el('div', { class: 'row' }, el('button', { class: 'btn', type: 'button' }, 'Open'), el('button', { class: 'btn', type: 'button' }, 'Share'), el('button', { class: 'btn', type: 'button' }, 'Export')));
    card.append(table('', d.items.map((x) => [x.start.label, x.end.label])));
    card.append(el('div', { class: 'row' }, el('a', { href: '/?edit=cost' }, 'No price'), el('span', { class: 'edit-ok' }, '✎')));
    const c = chart();
    const tip = el('div', { class: 'tip' });
    const read = el('span', { class: 'read' }, 'none');
    c.addEventListener('mousemove', (e) => {
      const r = card.getBoundingClientRect();
      tip.textContent = `${Math.round(e.offsetX)} km`;
      tip.classList.add('on');
      tip.style.transform = `translate(${Math.round(e.clientX - r.left + 8)}px, ${Math.round(e.clientY - r.top - 24)}px)`;
      read.textContent = `${Math.round(e.offsetX)} km`;
    });
    c.addEventListener('mouseleave', () => { tip.classList.remove('on'); read.textContent = 'none'; });
    card.append(c, el('div', { class: 'row' }, 'At the mouse: ', read), tip);
    card.append(el('div', { class: 'row' }, el('input', { class: 'input', 'aria-label': 'Search', title: 'Search the drives' })));
    view.append(card, el('section', { class: 'card', 'data-cover': 'fx.fleet.summary' }, el('h2', {}, 'Summary'), `${d.items.length} drives`));
    return 'Fleet';
  },
  async drives(car) {
    const d = await api(`/api/v1/cars/${car}/drives?limit=3`);
    const card = el('section', { class: 'card bad', 'data-cover': 'fx.drives.table' }, el('h2', {}, 'Drives'));
    card.append(el('div', { class: 'row' }, el('button', { class: 'btn bold', type: 'button' }, 'Bold'), el('button', { class: 'btn', type: 'button' }, 'Next to it')));
    card.append(table('pad', d.items.map((x) => [x.start.label, x.end.label])));
    card.append(el('div', { class: 'row icon' }, el('a', { href: '/cars/c1/drives?edit=cost' }, 'No price'), el('span', { class: 'edit' }, '✎'), el('span', {}, 'after the icon')));
    const c = chart();
    const tip = el('p', { class: 'tip-flow' });
    c.addEventListener('mousemove', (e) => { tip.textContent = `${Math.round(e.offsetX)} km`; if (!tip.isConnected) c.after(tip); });
    c.addEventListener('mouseleave', () => tip.remove());
    card.append(c, el('p', {}, 'Under the chart'));
    card.append(el('div', { class: 'scroll' }, el('div', { class: 'scroll-host' }, el('p', {}, el('a', { href: '/cars/c1/drives?box=1' }, 'Hover me')), el('p', {}, 'and a scrollbar comes'), el('div', { class: 'scroll-tip' }, 'A tip in the box'))));
    card.append(el('div', { class: 'row' }, el('input', { class: 'input thick', 'aria-label': 'Search' }), el('span', {}, 'beside the field')));
    card.append(el('p', {}, el('span', { class: 'low', tabindex: '-1' }, 'A tooltip below', el('span', { class: 'low-tip' }, 'very long'))));
    view.append(card);
    return 'Drives';
  },
};

function match(path) {
  const p = path.split('/').filter(Boolean).map(decodeURIComponent);
  if (p.length === 0) return ['fleet'];
  if (p[0] === 'cars' && p[2] === 'drives' && p.length === 3) return ['drives', p[1]];
  return [null];
}

async function main() {
  delete root.dataset.ready;
  const [id, ...args] = match(location.pathname);
  if (!id) { root.dataset.ready = 'error:unknown'; return; }
  document.querySelector('.nav').dataset.cover = 'fx.frame.nav';
  title.dataset.cover = 'fx.frame.title';
  try {
    title.textContent = await pages[id](...args.map((a) => encodeURIComponent(a)));
    root.dataset.ready = id;
  } catch (e) {
    view.append(el('section', { class: 'card' }, String(e.message)));
    root.dataset.ready = `error:${id}`;
  }
}

main();
