// Checker fixture: stable. A tiny pack for `check.js --stable` (see ui/tools/README.md, "Stability checks"):
//   fleet    stays still while live values arrive: values change in place, the list is drawn again in one go
//            on `rows` events, a spinner turns, a map pin and a chart redraw (both are left out)
//   drives   jumps: a block appears above the list 1 s after ready, and every `now` event adds a line to it,
//            pushing the list down
//   drive    flickers: every `now` event swaps the list for "Loading…" for 300 ms and then shows it again, and
//            hides a badge for that time (visibility); the box has a fixed height, so nothing moves
// Other pages: as pack-ok. Made-up data only (the dev server's).

const root = document.documentElement;
const view = document.getElementById('view');
const title = document.querySelector('.title');

async function api(path) {
  const r = await fetch(path, { headers: { Accept: 'application/json' }, credentials: 'same-origin' });
  if (!r.ok) throw new Error(`${path}: ${r.status}`);
  return r.json();
}

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

const km = (v) => (v == null ? 'unknown' : `${v.toFixed(1)} km`);
const kmh = (v) => (v == null ? '0 km/h' : `${Math.round(v)} km/h`);

function stream(on) {
  const es = new EventSource('/api/v1/events');
  for (const [type, f] of Object.entries(on)) es.addEventListener(type, (ev) => f(JSON.parse(ev.data)));
  return es;
}

function driveLines(d) {
  return d.items.map((x) => `${x.start.label} to ${x.end.label}: ${km(x.distance_km)}`);
}

const pages = {
  async fleet() {
    const cars = await api('/api/v1/cars?limit=10');
    const sum = await api('/api/v1/fleet/summary');
    const list = card('fx.fleet.list', 'Cars', []);
    const values = new Map();
    for (const c of cars.items) {
      const p = el('p', {}, `${c.name}: `);
      const v = el('span', { class: 'value' }, kmh(c.now.speed_kph));
      p.append(v);
      list.append(p);
      values.set(c.id, v);
    }
    view.append(list);
    view.append(card('fx.fleet.summary', 'Summary', [`${sum.cars} cars`]));
    const drives = card('', 'Newest drives', []);
    const drawDrives = async () => {
      const d = await api('/api/v1/cars/c1/drives?limit=3');
      // drawn again in one go: the old lines go and the new ones come in the same frame
      drives.replaceChildren(el('h2', {}, 'Newest drives'), ...driveLines(d).map((l) => el('p', {}, l)));
    };
    await drawDrives();
    view.append(drives);
    const extras = card('', 'Live', []);
    const map = el('div', { class: 'maplibregl-map' });
    const pin = el('div', { class: 'pin' });
    map.append(pin);
    const svgNs = 'http://www.w3.org/2000/svg';
    const chart = document.createElementNS(svgNs, 'svg');
    chart.setAttribute('class', 'chart');
    chart.setAttribute('viewBox', '0 0 240 60');
    extras.append(el('div', { class: 'spinner', role: 'img', 'aria-label': 'Live' }), map, chart);
    view.append(extras);
    let n = 0;
    stream({
      now(ev) {
        const s = ev.now && ev.now.speed_kph;
        if (s && values.has(ev.car)) values.get(ev.car).textContent = kmh(s.value);
        n++;
        // the pin comes and goes, the chart is drawn again: both are inside parts that are left out
        if (n % 2) pin.remove(); else map.append(pin);
        const line = document.createElementNS(svgNs, 'polyline');
        line.setAttribute('points', `0,${30 + (n % 20)} 120,${n % 60} 240,30`);
        line.setAttribute('fill', 'none');
        line.setAttribute('stroke', '#2c6fbb');
        chart.replaceChildren(line);
      },
      rows() { drawDrives().catch(() => {}); },
    });
    return 'Fleet';
  },
  async drives(car) {
    const d = await api(`/api/v1/cars/${car}/drives?limit=8`);
    const list = card('fx.drives.table', 'Drives', driveLines(d));
    view.append(list);
    let banner = null;
    setTimeout(() => {
      // a block that comes late, above the list
      banner = el('div', { class: 'banner late' }, 'Live values are coming in.');
      view.prepend(banner);
    }, 1000);
    stream({
      now() {
        // and grows with every event: the list moves down each time
        if (banner) banner.append(el('p', {}, 'Another value arrived.'));
      },
    });
    return 'Drives';
  },
  async drive(car, id) {
    const d = await api(`/api/v1/cars/${car}/drives/${id}`);
    const box = el('div', { class: 'box' });
    const lines = [`Distance ${km(d.distance_km)}`, `From ${d.start.label}`, `To ${d.end.label}`];
    const show = () => box.replaceChildren(...lines.map((l) => el('p', {}, l)));
    show();
    const c = card('fx.drive.figures', 'Drive', []);
    const badge = el('p', { class: 'badge' }, 'Finished');
    c.append(badge, box);
    view.append(c);
    stream({
      now() {
        // the list goes for a moment and comes back: flicker without moving anything
        box.replaceChildren(el('p', { class: 'loading' }, 'Loading…'));
        badge.classList.add('off');
        setTimeout(() => { show(); badge.classList.remove('off'); }, 300);
      },
    });
    return 'Drive';
  },
  async charge(car, id) {
    const c = await api(`/api/v1/cars/${car}/charges/${id}`);
    view.append(card('fx.charge.figures', 'Charge', [`Added ${c.energy_added_kwh == null ? 'unknown' : c.energy_added_kwh.toFixed(2)} kWh`]));
    return 'Charge';
  },
};

function match(path) {
  const p = path.split('/').filter(Boolean).map(decodeURIComponent);
  if (p.length === 0) return ['fleet'];
  if (p[0] === 'cars' && p[2] === 'drives' && p.length === 3) return ['drives', p[1]];
  if (p[0] === 'cars' && p[2] === 'drives' && p.length === 4) return ['drive', p[1], p[3]];
  if (p[0] === 'cars' && p[2] === 'charges' && p.length === 4) return ['charge', p[1], p[3]];
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
    view.append(card('', 'Could not load this page', [String(e.message)]));
    root.dataset.ready = `error:${id}`;
  }
}

main();
