// Checker fixture: ok. A tiny pack that passes every check of ui/tools/check.js.
// It routes by path like a real pack, reads the dev API, marks its fixture items with data-cover
// and sets data-ready when the first render is done. Made-up data only (the dev server's).

const root = document.documentElement;
const view = document.getElementById('view');
const title = document.querySelector('.title');

async function api(path, { allow = [] } = {}) {
  const r = await fetch(path, { headers: { Accept: 'application/json' }, credentials: 'same-origin' });
  if (!r.ok && !allow.includes(r.status)) throw new Error(`${path}: ${r.status}`);
  return { status: r.status, body: r.headers.get('content-type')?.includes('json') ? await r.json() : null };
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

function frame(on) {
  document.querySelector('.nav').dataset.cover = on ? 'fx.frame.nav' : '';
  title.dataset.cover = on ? 'fx.frame.title' : '';
  if (!on) document.querySelector('.nav').hidden = true;
}

const km = (v) => (v == null ? 'unknown' : `${v.toFixed(1)} km`);
const kwh = (v) => (v == null ? 'unknown' : `${v.toFixed(2)} kWh`);
const pct = (v) => (v == null ? 'unknown' : `${Math.round(v)} %`);

const pages = {
  async fleet() {
    const cars = (await api('/api/v1/cars?limit=10')).body;
    const sum = (await api('/api/v1/fleet/summary')).body;
    view.append(card('fx.fleet.list', 'Cars', cars.items.map((c) => `${c.name}: ${c.now.state}, ${pct(c.now.soc_pct)}`)));
    view.append(card('fx.fleet.summary', 'Summary', [`${sum.cars} cars`]));
    const tile = el('img', { class: 'tile', alt: 'Map tile', src: 'https://tile.openstreetmap.org/1/0/0.png' });
    view.append(tile);
    view.append(el('p', { class: 'hidden-hint' }, 'Hidden text may be small: it is not shown.'));
    // a live stream stays open; the checker must not wait for it to end
    new EventSource('/api/v1/events');
    await tile.decode().catch(() => {});
    return 'Fleet';
  },
  async drives(car) {
    const d = (await api(`/api/v1/cars/${car}/drives?limit=5`)).body;
    view.append(card('fx.drives.table', 'Drives', d.items.map((x) => `${x.start.label} to ${x.end.label}: ${km(x.distance_km)}`)));
    return 'Drives';
  },
  async drive(car, id) {
    const d = (await api(`/api/v1/cars/${car}/drives/${id}`)).body;
    view.append(card('fx.drive.figures', 'Drive', [`Distance ${km(d.distance_km)}`, `Energy ${kwh(d.energy_kwh)}`]));
    return 'Drive';
  },
  async charge(car, id) {
    const c = (await api(`/api/v1/cars/${car}/charges/${id}`)).body;
    view.append(card('fx.charge.figures', 'Charge', [`Added ${kwh(c.energy_added_kwh)}`]));
    return 'Charge';
  },
  async health(car) {
    if (car) {
      const c = (await api(`/api/v1/cars/${car}`)).body;
      view.append(card('fx.health.car', 'Car health', [`${c.name}: last activity recorded`]));
    } else {
      await fetch('/health');
      view.append(card('fx.health.server', 'Server health', ['The health address answers.']));
    }
    return 'Data health';
  },
  async 'sign-in'() {
    frame(false);
    const me = await api('/api/v1/me', { allow: [401] }); // 401 is expected here
    view.append(card('', 'Sign in', [me.status === 401 ? 'You are signed out.' : 'You are signed in.']));
    return 'Sign in';
  },
  async setup() {
    frame(false);
    const s = (await api('/api/v1/setup')).body;
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
    title.textContent = await pages[id](...args.map((a) => (a == null ? a : encodeURIComponent(a))));
    root.dataset.ready = id;
  } catch (e) {
    view.append(card('', 'Could not load this page', [String(e.message)]));
    root.dataset.ready = `error:${id}`;
  }
}

main();
