// Checker fixture: pixel. Pages for ui/tools/check.js --pixel. The same small cases twice: on fleet each is
// right, on drives each is off on purpose (mostly by 1 px), so the self-test can see that every kind is found
// and that nothing right is reported. Classes say which case is which (.ok on fleet, .bad on drives):
//   hidden   text under an opaque box (fleet: under a see-through link that covers the card)
//   overlap  two labels on top of each other (fleet: side by side)
//   badge    text 1 px right of centre (padding 9 px left, 7 px right)
//   chip     a button's text 1 px below centre (padding 5 px top, 3 px bottom)
//   iconbtn  a flex-centred icon 1 px low (margin-top 2 px)
//   avatar   flex-centred initials 1 px left (margin-right 2 px)
//   empty    a centred empty state 1 px left (an invisible 2 px spacer at the end)
//   fields   a row of fields whose second field sits 1 px lower
//   grid     a table row whose middle cell sits 1 px lower
//   stack    stacked blocks whose middle one is 2 px in
//   frac     a bordered box and a 1 px line on half pixels
//   clipbox  text cut off by its box (fleet: cut with an ellipsis, which is meant)
//   over     text that sticks out of its box (fleet: it wraps)
//   wide     a strip wider than the window: the page scrolls sideways (drives only)
// Made-up text only.

const root = document.documentElement;
const view = document.getElementById('view');
const title = document.querySelector('.title');
const SVG = 'http://www.w3.org/2000/svg';

function el(tag, cls, text = '', attrs = {}) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text) e.textContent = text;
  return e;
}

function icon(cls) {
  const s = document.createElementNS(SVG, 'svg');
  s.setAttribute('class', cls);
  s.setAttribute('viewBox', '0 0 16 16');
  s.setAttribute('width', '16');
  s.setAttribute('height', '16');
  s.setAttribute('aria-hidden', 'true');
  const c = document.createElementNS(SVG, 'circle');
  c.setAttribute('cx', '8'); c.setAttribute('cy', '8'); c.setAttribute('r', '6');
  s.append(c);
  return s;
}

function cases(q, cover) {
  const box = el('section', 'card', '', cover ? { 'data-cover': cover } : {});
  const add = (name, ...kids) => { const c = el('div', `case case-${name}`); c.append(el('p', 'label', name), ...kids); box.append(c); };

  const cover1 = el('div', `coverbox ${q}`);
  cover1.append(el('p', 'covered', 'Range 312 km'));
  cover1.append(q === 'ok' ? el('a', 'stretch', '', { href: '/', 'aria-label': 'Open' }) : el('div', 'lid'));
  add('hidden', cover1);

  const labels = el('div', `labels ${q}`);
  labels.append(el('span', 'one', 'Start'), el('span', 'two', 'End'));
  add('overlap', labels);

  const row = el('div', 'boxes');
  row.append(el('span', `badge ${q}`, 'New'), el('button', `chip ${q}`, 'Chip', { type: 'button' }));
  const ib = el('button', `iconbtn ${q}`, '', { type: 'button', 'aria-label': 'Locate' });
  ib.append(icon('glyph'));
  const av = el('div', `avatar ${q}`);
  av.append(el('span', 'initials', 'AB'));
  row.append(ib, av);
  add('centre', row);

  const empty = el('p', `empty ${q}`, 'Nothing here yet. Drives show up after the first trip.');
  empty.append(el('span', 'spacer'));
  add('empty', empty);

  const fields = el('div', `fields ${q}`);
  const sel = el('select', 'f-select', '', { 'aria-label': 'Car' });
  sel.append(el('option', '', 'Comet'));
  fields.append(el('input', 'f-input', '', { 'aria-label': 'Name', value: 'Comet' }), sel, el('button', 'f-button', 'Save', { type: 'button' }));
  add('fields', fields);

  const table = el('table', `grid ${q}`);
  const tr = el('tr');
  tr.append(el('td', 'c1', 'Monday'), el('td', 'c2', '42 km'), el('td', 'c3', '6.1 kWh'));
  const tb = el('tbody');
  tb.append(tr);
  table.append(tb);
  add('grid', table);

  const stack = el('div', `stack ${q}`);
  stack.append(el('div', 's1', 'First'), el('div', 's2', 'Second'), el('div', 's3', 'Third'));
  add('stack', stack);

  const fr = el('div', 'frac-wrap');
  fr.append(el('div', `frac ${q}`, 'Bordered'), el('div', `rule ${q}`));
  add('frac', fr);

  const clipbox = el('p', `clipbox ${q}`, 'A long place name that does not fit');
  const tight = el('p', 'tight', 'Tight line height');
  const sr = el('span', 'sr-only', 'Only for screen readers');
  add('clip', clipbox, tight, sr);

  add('over', el('p', `over ${q}`, 'Sticks out of its box'));
  if (q === 'bad') add('wide', el('div', 'wide'));
  return box;
}

function frame() {
  document.querySelector('.nav').dataset.cover = 'fx.frame.nav';
  title.dataset.cover = 'fx.frame.title';
}

const pages = {
  fleet() {
    view.append(cases('ok', 'fx.fleet.list fx.fleet.summary'));
    return 'Fleet';
  },
  drives() {
    view.append(cases('bad', 'fx.drives.table'));
    return 'Drives';
  },
};

async function main() {
  delete root.dataset.ready;
  const p = location.pathname.split('/').filter(Boolean);
  const id = p.length === 0 ? 'fleet' : p[0] === 'cars' && p[2] === 'drives' && p.length === 3 ? 'drives' : null;
  if (!id) { root.dataset.ready = 'error:unknown'; return; }
  frame();
  title.textContent = pages[id]();
  await document.fonts.ready;
  root.dataset.ready = id;
}

main();
