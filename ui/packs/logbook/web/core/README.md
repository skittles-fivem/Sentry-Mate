# Logbook pack: building a page

This is the guide for building pages in the Logbook UI pack. Read it with the page plan
([`docs/parity/ui-parity.md`](../../../../../docs/parity/ui-parity.md)), which says what each page shows, and the API
([`docs/ui-api.md`](../../../../../docs/ui-api.md)), which says where the data comes from. Everything a page needs is
described here; you should not need to read the frame's code.

## How the pack is laid out

```
web/
  index.html        the shell only: masthead slots, nav, notices, flash zone, <main>, footer
  boot.js           runs before the first paint: interface size (this device's own size from sentrymate.device-size
                    when set, else the account's) and theme from this browser's copy
  app.css           tokens, base, layout, the frame and every shared component
  manifest.webmanifest, icon*.png, icon.svg   so the app can be added to a home screen
  core/             the frame and the helpers (this folder)
  pages/<id>.js     one module per page id (and sign-in.js, setup.js)
  pages/<id>.css    optional: styles only that page uses
  shared/           the shared modules, copied from ui/shared/ (never edit them here)
  vendor/           MapLibre GL JS and uPlot, loaded only when a page draws a map or a chart
  fonts/            Charter (serif) and Inter (sans)
```

Paths in `index.html` are absolute (`/core/start.js`, `/app.css`), because pages live at addresses such as
`/cars/c1/drives`. Paths inside the JS and CSS are relative to their own file (`url('fonts/x.woff2')`,
`new URL('../vendor/...', import.meta.url)`), so they follow the server's versioned prefix (`/_v/<version>/...`)
that `index.html` is rewritten to.

Run it on the dev API server (made-up data, no outside network):

```bash
python3 ui/devserver/serve.py --pack ui/packs/logbook --port 8000
node ui/tools/check.js --pack logbook --pages drives --sizes 1440x900,3840x2160
```

## Writing a page

A page is `pages/<page id>.js`. The page ids, their routes and their menu group are fixed (contract 3 in the
page plan; the list is `core/pages.js`). The router already knows every page id (`MODULES` in `core/router.js`);
to build a page, replace its placeholder file. Nothing else needs registering.

```js
// pages/charges.js
import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { pageHead, q, figure, figures, empty } from '../core/ui.js';
import { dataTable } from '../core/table.js';

export const id = 'charges';
export const css = true;                 // load pages/charges.css before mount (optional)
export const title = () => 'Charges';    // first part of the tab title; the car's name is added

export async function mount(ctx) {
  const path = ctx.car ? api.apiPath`cars/${ctx.car.id}/charges` : 'charges';
  const table = dataTable(ctx, { path, params: ctx.params, columns: [...], itemName: ['charge', 'charges'] });
  ctx.main.append(pageHead({ eyebrow: ctx.car?.name, title: 'Charges', deck: ctx.info.purpose }), table.el);
  await table.load();                    // throws if the first page fails: the router shows the error state
  ctx.ready();                           // the first render with data is done
  return {
    unmount() {},                        // optional: anything ctx.onCleanup does not cover
    async update(c, why) {               // optional: a new period (why.period) or new units (why.prefs)
      await table.reload(c.params);
      c.ready();                         // update() must call ready() again
    },
  };
}
```

What the router does for every page:

1. Leaves the old page: calls its `unmount()`, cancels every request it made through `ctx.api`, runs its
   `ctx.onCleanup` functions, removes `data-ready`.
2. Shows the loading view in `<main>` (`<main class="wrap main page-<id>">`).
3. Loads the car of the address (`GET /api/v1/cars/{car}`); an unknown or hidden car shows "not found".
4. Resolves the period in the car's time zone, tells the frame (car switcher, period picker, nav, clock),
   sets the tab title, loads `pages/<id>.css` when the module says `css = true`.
5. Empties `<main>` and calls `mount(ctx)`. If `mount` throws (or rejects), the page's error state is shown
   with a "Try again" button and `data-ready` becomes `error:<id>`.

A new period calls `update(ctx, { period: true })` when the page has one, otherwise the page is mounted
again. A change of units, range or language calls `update(ctx, { prefs: true })`, or mounts again. A change of
car, drive or charge always mounts again.

Fast page changes: a page may export `prefetch(ctx)`. The router calls it when a link to the page is under the
pointer for 50 ms, touched or focused, with the `ctx` that `mount` will get (car, period, params, query, state,
me; `ctx.prefetching` is true), but its `ctx.api` only warms answers (`prefetchApi` in `shared/prefetch.js`). It must
make the same calls as the page's first reads, so build them in one function both use (`topReads(ctx)` in
`pages/battery.js`): a call with other parameters warms an answer nobody asks for. The router also loads the page's
module and style sheet then, and once the first page is drawn the main sections' modules load while the browser
is idle. A page (not the settings forms) takes kept answers at once for the reads it makes before `ready()`; each is
checked at the same time, `<html data-stale>` dims the page while one is on screen, and a page whose answers
changed is built again in place, like a live refresh. On the first page after a reload the service worker's kept
answers can come first the same way.

### The page context (`ctx`)

| Field | What it is |
|---|---|
| `id`, `info` | The page id and its entry in `core/pages.js` (`name`, `purpose`, `sections`, `period` default, `car`). |
| `state`, `query` | The URL state (`shared/state.js` `current()`), and the page's own query parameters (filters). |
| `carId`, `driveId`, `chargeId` | From the path. |
| `car` | The car (`GET /api/v1/cars/{car}`, with `config` and `version`), or null on pages without one. |
| `tz` | The car's time zone, else the browser's. Show every time and group every day in it. |
| `period` | `{ preset, from_ms, to_ms, label }` resolved in `tz`, or null when the page shows no period. |
| `params` | `{ from_ms, to_ms }` for the API (no `from_ms` for "All time"). |
| `api` | A client for this page (`shared/api.js` `scope()`): `get`, `page`, `list`, `pager`, `post`, `put`, `patch`, `del`, `csvUrl`, `url`. Everything it starts is cancelled when the page is left. Never use `fetch`. |
| `signal` | Aborted when the page is left. |
| `main` | The `<main>` element to draw into. |
| `me` | The account (`{ id, name, role: 'admin' \| 'watch', cars }`). Hide admin-only actions for `watch`. |
| `ready()` | Call once the first render with data is done (sets `data-ready="<id>"`). |
| `fail(err)` | Show the page's error state now (sets `data-ready="error:<id>"`). Throwing from `mount` does the same. |
| `alive()` | False once the page was left (check it after an `await` that is not through `ctx.api`). |
| `onCleanup(fn)` | Run `fn` when the page is left. |
| `title(...parts)` | Set the tab title (`ctx.title('Costco to Home')` gives `Costco to Home · Comet · Sentry Mate`). |
| `setQuery(patch)` | Put the page's filters in the URL (`{ q: 'home', min: null }`, null removes one). The router does not react: the page has already updated itself. Back, forward or a link to other filters mount the page again, so read your filters from `ctx.query` in `mount`. Once the page was left it does nothing (a late timer cannot change the next page's address); still clear your timers with `ctx.onCleanup`. |
| `link(page, params)` | A link to another page that carries the car and the period (`ctx.link('drives')`). |
| `go(href)` | Go to an address inside the app. |
| `events(types, fn)` | Live events from `/api/v1/events` while the page is shown: `ctx.events(['now'], (ev) => …)`, `ev.data` is the event. |
| `flash(text, opts)` | A short note after an action (see Flash notes). |
| `cover(el, ...ids)` | Mark an element as covering inventory items. |

### Readiness and coverage

- Call `ctx.ready()` after the first render **with data**, not before the data has arrived. Maps and charts
  may still be drawing; await them first when it costs little (`await m.ready` with a timeout).
- An error that stops the whole page: throw it from `mount` (or call `ctx.fail(err)`). An error in one part of
  a page: draw `errorView(err)` in that part and carry on.
- Put `data-cover="<item id>"` on the element that really does what the item describes, with the `cover`
  attribute of `h()` (`h('section', { cover: ['drives.table', 'drives.table.date'] }, …)`) or `cover(el, …)`.
  An item in a closed menu or dialog is marked on what opens it. **Only mark an item when the page really does
  it**; leave it missing otherwise. `node ui/tools/check.js --pack logbook --pages <id>` lists what is missing.
  The frame's 20 items are marked by the frame; never mark them in a page.

## Core helpers

All are plain ES modules; import them relative to the page (`'../core/ui.js'`). Text from the API is always set
as text (`h()` from `shared/dom.js`).

### `core/ui.js`: page parts

| Helper | What it gives |
|---|---|
| `pageHead({ eyebrow, title, deck, aside, cover, cls })` | The page head: small-caps eyebrow (text or a node such as breadcrumbs), the serif title (text or nodes), a deck line, and an aside on the right (buttons, "as of"). |
| `section({ title, note, actions, cover, cls, id }, ...children)` | A block with a small-caps label over a strong rule, and actions at its right. |
| `figure(name, value, note, { cover, cls })` | One figure: name, big value, a line saying how it was worked out or why it is blank. |
| `figures(list, { cls, cover })` | Figures in a ruled row (`.figs`), as many as fit. |
| `q(value, kind, opts)` | A value with its unit, the unit quieter (`q(d.distance_km, 'distance')` → `7.6 mi`). A null value shows `—`, and `opts.why` says why in the open. |
| `plus(node)` | `+15.2 kWh` with a quiet plus sign. |
| `ago(ms, { stale })` | A `<time>` that says "5 min ago" and keeps itself up to date (the frame refreshes every `[data-ago]` every 20 s). |
| `stateMark(state, { stale })`, `stateLabel(state, stale)` | A car state with its coloured dot; `driving`, `charging`, `parked`, `asleep`, `unknown`, `online`. |
| `chip(text, { kind, iconName })`, `flag(text, { kind })` | A rounded label (`kind: plug \| warn \| on`); a small uppercase flag (`kind: warn \| estimated`). |
| `pageLink(text, page, params)` | An arrow link to another page that carries car and period. |
| `button(text, { onclick, kind: 'quiet' \| 'primary' \| 'danger', iconName, disabled })` | A button in the pack's style. |
| `icon(name)` | A line icon: `search chevronRight chevronLeft chevronDown arrowRight close plug reset download menu calendar person warning info check pin star settings`. Decorative; say the meaning in text. |
| `loadingView()`, `loadingBlock(lines)` | The shimmering stand-ins (the router shows `loadingView` for the whole page). |
| `empty({ title, text, action, cover })` | Nothing to show, and why: say which filter removed everything or what to do. Never a bare "No data". |
| `errorView(err, { retry, eyebrow, title, home })`, `errorWords(err)` | The error state of a page or a part, in plain words per HTTP status, with "Try again". For a part, pass `title` (its own words when the error has none more specific) and `home: false` (no "Go to Fleet" link). |
| `batteryBar(soc, { from, limit, cls })` | A thin battery bar (SVG with percentage widths). |
| `routeSketch([[lon, lat], …], { cls })` | A route's shape as a small drawing, start ring and end dot. |
| `plannedSections(list, { title, noteText })` | The page plan's sections for a part not built yet. |

`core/format.js` has text helpers in the car's zone for places where an element is not wanted: `T(value,
kind)`, `dur(ms)`, `when(ms, tz)` ('Today, 5:30 PM'), `dayName(key, tz)`, `timeRange(a, b, tz)`, `zoneLabel(tz)`,
`count(n, 'drive')`, `placeText(p)`, `areaText(p)`.

### `core/table.js`: tables

```js
const table = dataTable(ctx, {
  path: api.apiPath`cars/${car.id}/charges`, params: { ...ctx.params, kind: 'dc' }, limit: 50,
  columns: [
    { key: 'start', label: 'Date', sort: 'start', th: true, cell: (c) => h('a', { href: … }, formatDate(c.start_ms, ctx.tz)) },
    { key: 'energy', label: 'Added', sort: 'energy', num: true, cell: (c) => q(c.energy_added_kwh, 'energy'), value: (c) => c.energy_added_kwh },
  ],
  sort: { key: 'start', dir: 'desc' }, itemName: ['charge', 'charges'],
  empty: () => empty({ title: 'No DC charge in this period.', text: 'Choose AC or all kinds above.' }),
  onRows: (items, page) => drawTotals(page.totals),
  cover: ['charges.table'],
});
ctx.main.append(table.el);
await table.load();
```

- `box: true` (or the box's name): the rows, and the loading, empty or error view, scroll inside a box whose
  height the page's CSS gives `.ledger-box` from the start (a focusable region); with `autoMore` the next page
  loads as the end of the box comes near, and `data-below` marks rows hidden under its bottom edge (for a fade).
  The audit log of Settings: Server and access uses it.
- A list resource (`path`) is paged by cursor: the caption counts the rows, **Show more** loads the next page,
  and the **Download CSV** button is the same resource with `format=csv` and the same filters (every matching
  row, not one page). A column with `sort: '<key>'` is sorted by the server (the resource's `sort` and `dir`);
  `sort: true` sorts the loaded rows here.
- Rows the page already has: `rows: [...]` instead of `path`. Sorting is done here, and the CSV is built in the
  browser from each column's `value(row)` (or pass `csv: { columns: [column(…), timeColumn(…)] }` from
  `shared/csv.js` so the CSV names its units).
- `table.reload(params, { sort: null })` starts again with new filters; `table.redraw()` draws the rows again;
  `table.redrawRow(item)` redraws one row in place (live updates); `table.items` are the rows loaded.
- `onSort({ key, dir })` is called when a column head is clicked, before the rows load again in that order
  (a page with its own order control keeps it, and its URL, in step).
- `num: true` right-aligns figures; `th: true` makes the cell the row's header; `rowAttrs(row)` adds a class or
  `data-` value to a row. The heads stick under the masthead while the rows scroll.
- `windowLedger(ctx, { path, params, columns, sort, firstSort, search, jump, tools, onPage, empty })` is for a list of
  thousands of rows (the log): the rows sit in a box ten rows high (`.vl-box`) that scrolls under its head, only the
  rows in view and a few more are in the page (`shared/vlist.js`), and they load from the API a chunk at a time as the
  box comes to them (by cursor when the chunk goes on from one already in, else by `offset`; Jump to date asks with
  `at_ms`). Every row has the same height (`.vl-ledger`: two lines a cell, no wrapping). Sorting is the server's, the
  search field sends `q`, Download CSV is the server's file with the same filters, and every answer goes to
  `onPage`. `setParams(patch, { sort })` starts again from the top; `refresh()` loads the rows in view again quietly.

### `core/chart.js`: charts (uPlot)

```js
await stackedCharts(ctx, box, {
  tz: ctx.tz, t_ms: s.t_ms, states: s.state, spans: spansFromStates(s.t_ms, s.state, s.bucket_ms),
  rows: [
    { key: 'soc', name: 'Battery', kind: 'percent', values: s.values.soc_pct, colour: '--accent', fill: '--chart-wash', floor: 0 },
    { key: 'power', name: 'Power', kind: 'power', values: s.values.power_kw, signed: true, note: 'use above zero, regen below' },
    { key: 'temp', name: 'Temperatures', kind: 'temperature', values: s.values.outside_c, extra: [{ values: s.values.cabin_c, colour: '--ochre' }] },
  ],
  onHover: (i) => …, onZoom: (from_ms, to_ms) => …,
});
```

- Charts under each other share one time axis in the car's zone, one hover line (`onHover(index)`), drag to
  zoom (`onZoom`), double-click to zoom out, and `resetZoom()`.
- Values come metric from the API and are drawn in the person's units with the unit beside each chart (`kind`).
- `null` is a gap: a line never crosses time with no data. With `states`, a value is held across **asleep**
  buckets (nothing changes while the car sleeps) but never across **unknown** ones. `spans` shade asleep and
  unknown time apart (`--chart-asleep`, `--chart-unknown`).
- Sizes: the chart fills its `.chart-box`, which CSS sizes in rem (`.chart-box { height: 8.125rem }`; give a row
  another height with `.chart-<key> .chart-box`). Fonts, padding and lines scale with the interface size;
  the charts rebuild on a new root size or theme; uPlot draws at the device pixel ratio by itself.
- Below the fold: each chart is built, and `makeMap(ctx, el, { lazy: true })` makes its map (and loads MapLibre), only when its box comes
  within a screen of view (`shared/lazy.js` `whenNear`), so the first paint draws only what is on screen. The box
  has its height from CSS from the start, so nothing moves; a late chart takes the zoom of the others. A page waits
  for a map before `ready()` only when `nearNow(box)` (`shared/lazy.js`) says it is near.
- uPlot is loaded on first use (`loadUplot()`). `niceSplits`, `niceStep`, `chartColours()` are there for a
  page that draws its own uPlot chart; it must then follow the same rules.

### `core/map.js`: maps (MapLibre)

```js
const m = await makeMap(ctx, box, { bounds: boundsOf(coords), padding: 40, maxZoom: 16, fullscreen: true });
if (m) {
  await m.loaded;
  m.addRoute('route', coords);                         // accent line with a paper casing; kind 'faint' or 'ahead'
  m.addPin(coords[0], 'Home', 'pin-start');            // pin-start | pin-end | pin-here | pin-charge
  m.map.addSource('sel', { type: 'geojson', data });   // the MapLibre map itself is m.map
  m.addLine({ id: 'sel-line', source: 'sel' }, 6, '--accent-strong');
}
```

- Tiles follow the server's map settings (`GET /api/v1/maps/config`): offline tiles served by Sentry Mate when
  installed, else the online tile server the owner turned on, else no tiles (the paper background, with the
  lines and pins, and a note saying so). **Never** point a map, an image or a link at another host yourself.
- Line widths are in px at the standard size and scale with the interface size; colours are CSS tokens and
  follow the theme. Use `m.addLine` (or `m.addRoute`), not `m.map.addLayer` with a fixed colour or width.
- The box shimmers until the map has drawn; `m.ready` resolves then (or after 15 s). The map is removed when
  the page is left. Size the box in CSS (`.map-<name> { height: 20rem }`), in rem.
- `coordsText([lon, lat])` gives `37.7508° N, 122.4330° W`.

### `core/journal.js`: the activity strip and timeline rows

- `activityStrip(GET …/activity answer, { tz, link(segment) })`: the state strip with its key; a segment can
  link to its drive or charge.
- `timelineRow(row, { car, tz, full, period })`: one `GET …/timeline` row as a journal entry (inside an
  `<ol class="tl">`, or `tl tl-full` with `full: true` for times, figures and row links).
- `placeLink(point)`: the Places editor for a recorded point (edit its place, or a new place prefilled there).
- `AWAKE_WORDS`, `GAP_WORDS`, `STRIP_NAMES`: the words for the API's codes.

### `core/form.js`: forms and controls

```js
const name = field({ label: 'Name', name: 'name', value: place.name, hint: 'As it shows on drives and charges.', attrs: { required: true } });
const radius = field({ label: `Radius (${unitLabel('length')})`, name: 'radius', type: 'number', value: … });
ctx.main.append(form({
  fields: [name, radius], submitLabel: 'Save the place',
  onsubmit: async (v) => { await ctx.api.patch(api.apiPath`places/${id}`, { version, name: v.name, radius_m: toMetric(v.radius, 'length') }); ctx.flash('Place saved.'); },
}));
```

- `field({ label, name, type, value, hint, options, attrs })`: a labelled control (`text`, `password`,
  `number`, `date`, `select` with `options: [[value, text]]`, `textarea`); the control is `el.control`.
- `form({ fields, onsubmit, submitLabel })`: disables the button while saving; when `onsubmit` throws an
  `ApiError`, each entry of its `fields` (`'units.distance'`, `'name'`) goes under its field and the rest to a
  line at the top. Throw your own `Object.assign(new Error('…'), { fields: { again: '…' } })` for checks in
  the page. `setFieldError(fieldEl, text)` sets one by hand.
- `choice({ label, name, value, options, onchange })`: the pack's segmented toggle (radio buttons; arrow keys
  move and pick); `el.set(value)` updates it.
- `switchField({ label, checked, onchange, hint })`: an on / off switch (a real checkbox); `el.set(on)`.
- Values typed in the person's units go back to metric with `toMetric(value, kind)` from `shared/units.js`.

### `core/dialog.js`: dialogs and menus

- `openDialog({ title, body, actions: [{ label, value, kind, onclick }], wide })` resolves with the value of
  the action that closed it (null for Esc or the close button). An action's `onclick(close)` may return false
  to keep the dialog open.
- `confirmDialog({ title, text, yes, danger })` resolves true or false. Ask before anything that cannot be
  undone, and say what will change.
- `popover(trigger, panel)`: a small menu under its button (Esc, a click outside or leaving it closes it).

### Flash notes (`core/flash.js`)

`ctx.flash('Place saved.')` after a successful action; `ctx.flash('Not saved: …', { kind: 'error' })` stays
until closed; `{ kind: 'info' }` for neutral news; `{ next: true }` shows it on the next page instead (an
action that ends in a navigation).

### Preferences, theme and size

- `currentPrefs()`, `onPrefs(fn)` (`core/prefs.js`); save a person's choice with `savePref(patch)`
  (`core/frame.js`): applied at once, saved to `/api/v1/me/preferences`, put back if the server says no.
- `isDark()`, `onTheme(fn)` for anything drawn in canvas or WebGL; CSS needs nothing (the tokens change).
- `display.rem`, `display.dpr`, `display.scale`, `display.onChange(fn)` (`core/display.js`) for drawings
  in device pixels. `sizeState()`, `setSize({ size, follow })`, `sizeNote()`.

## Units, numbers and time

- The API is metric (`_km`, `_kwh`, `_c`, `_kpa`, `_ms`); show every value through `q()`, `figure()` or
  `format(value, kind)` from `shared/units.js`, which convert to the person's units and attach the unit.
  Every number shows its unit. Kinds: `distance odometer range length speed temperature pressure energy power
  efficiency percent voltage current duration money …` (`shared/README.md`).
- Rated or ideal range: `rangeField('range_km')` names the field to read (`ideal_range_km` when the person
  prefers ideal), `rangeLabel()` its name.
- Money keeps its currency; never add two currencies (`formatMoneyList`).
- Times: `formatTime`, `formatDate`, `formatDateTime`, `formatDuration`, `dayKey`, `formatDayKey` from
  `shared/time.js`, always with `ctx.tz`. "Now" is the server's clock, `api.now()`, never `Date.now()`.
- A blank value says why (`q(null, 'energy', { why: 'not sent by this car' })`); asleep is not unknown.

## The period

The period picker is in the masthead, in the car's time zone: 24 h, 7 days, 30 days, this year, all, or
custom dates, with Earlier and Later. It is in the URL (`?preset=7d` or `?from=&to=`), carried to every page
and remembered by the tab. A page reads `ctx.period` and sends `ctx.params`; it never draws its own period
control. A page's default period is `period` in `core/pages.js` (null: the page shows no period, and the
picker says so). To show a narrower span (a zoom inside a drive) keep it in the page, not in the period.

A link that opens another page on one span (a timeline row's "Trip for this span") uses
`spanHref(page, { car, from, to, query })` from `core/period.js`, not `period:` in `buildUrl`: a period in an
address becomes the tab's period, so Back and every later page would show the row's span. `spanHref` puts the
span in `?span_from=&span_to=` (ms), which the router resolves as that page's `ctx.period` only while the
address names no period of its own; the picker shows it, and choosing another period there replaces it.

## Design tokens and classes

Colours (light paper and night ink, switched by `data-theme` and the device setting): `--paper`,
`--paper-raised`, `--paper-sunk`, `--ink`, `--ink-2`, `--ink-3`, `--ink-4`, `--rule`, `--rule-strong`,
`--accent` (deep green: driving, links, focus), `--accent-strong`, `--accent-wash`, `--ochre` (charging),
`--ochre-wash`, `--slate` (parked), `--dusk` (asleep), `--rust` (warnings, unknown), `--rust-wash`,
`--map-paper`, `--chart-wash`, `--chart-ochre-wash`, `--chart-sunk`, `--chart-asleep`, `--chart-unknown`.

Type: `--serif` (Charter: titles, names, figures), `--sans` (Inter: text, labels, units), `--mono`. Small text
sizes, each never under 10 px: `--micro`, `--micro-2`, `--small`, `--small-2`, `--text-s`, `--text-sm`.

Classes in `app.css` to reuse: `.wrap` (the page column), `.page-head`, `.eyebrow`, `.page-title`, `.deck`,
`.label`, `.label-note`, `.note`, `.note-serif`, `.block`, `.block-head`, `.figs`, `.fig`, `.today-figs`,
`.q`, `.state`, `.state-dot`, `.chip`, `.flag`, `.tag`, `.button` (`-quiet`, `-danger`, `-small`),
`.segmented`, `.toolbar`, `.field`, `.field-input`, `.field-select`, `.field-search`, `.ledger*`,
`.cell-main`, `.cell-sub`, `.tl`, `.tl-full`, `.strip`, `.map`, `.pin*`, `.sketch`, `.bar*`, `.charts`,
`.chart-row`, `.readout`, `.crumbs`, `.step-nav`, `.link-arrow`, `.empty`, `.planned`, `.dialog`, `.frame-line`.

Layout: the page's width is the container `page`. Lay pages out with grid and **container queries in rem**
on these tiers, never media queries on pixels:

| Tier | Container width | Typical |
|---|---|---|
| compact | under 50rem | 200 % size or zoom on a laptop: one column, nothing scrolls sideways |
| narrow | 50 to 72rem | big size on a desktop window: side columns move under the main one, the masthead scrolls away |
| standard | 72 to 112.5rem | the laptop layout (1440 x 900 is 90rem) |
| wide | 112.5rem and up | 1080p to 4K: more columns, bigger maps and charts beside the list, never longer lines of text |
| widest | 150rem and up | tables may add columns |

```css
.page-drives .patterns { display: grid; grid-template-columns: 1.4fr 1fr 0.9fr; }
@container page (width < 72rem) { .page-drives .patterns { grid-template-columns: minmax(0, 1fr); } }
```

Page styles go in `pages/<id>.css` (`export const css = true`); start selectors with the page's own class
names or `.page-<id>` (the router puts it on `<main>`), so they never touch another page.

## Size and 4K

- Effective size = Auto x Interface size. Auto follows the screen, not the window
  (`screen.width` x `screen.height` in CSS px): 1 up to 1440 x 810, rising in a straight line to 1.45 at
  3840 x 2160 and capped there; the long and the short side both count (a portrait 4K screen grows like a
  landscape one), and in Firefox, which reports the screen scaled by its page zoom, Auto is 1. Follow screen
  size off makes Auto 1. Interface size is 75 to 200 % (whole numbers). This rule is the same in every pack. Both live in the person's preferences
  (`interface_size`, `follow_screen`) and in this browser's copy for the first paint (`boot.js` and
  `core/display.js` hold the same formula: change both together).
- So: **rem for every size** (text, padding, boxes, map and chart heights). px only for hairlines (1px rules)
  and the 10 px floor (`max(10px, 0.75rem)`; use the `--small` style tokens).
- SVG drawn in percentages of a box sized in rem scales by itself. Canvas (charts, maps) draws at
  `devicePixelRatio`: use the helpers, or follow `display.onChange` yourself.
- Check every page at 1440x900 and 3840x2160 (and 1280x800, 2560x1440): `node ui/tools/check.js`.

## Live data

Pages never poll. `ctx.events(['now'], …)` gives a car's live values as they arrive (merge them field by field:
a `{ value, at_ms }` pair is sent whole; `mergeNow` in `pages/car.js` does it); `ctx.events(['rows'], …)` says
a drive, charge or parked spell started or ended, so a list can load again. The frame already follows
`notice` and `prefs` events.

## What not to do

- No inline `style` attributes or `<style>` elements (the CSP blocks them): set dynamic values with
  `h(tag, { vars: { '--w': '42%' } })` or `el.style.setProperty()` (CSSOM is allowed).
- No `innerHTML`, `outerHTML` or `insertAdjacentHTML` with data; build with `h()` and `svg()`. Never pass
  null to `el.replaceChildren()` (it prints "null"): use `replace(el, ...children)` from `shared/dom.js`.
- No `fetch` or `EventSource` in a page: `ctx.api` and `ctx.events` only.
- No request to another host: no outside fonts, scripts, images, maps or links that load anything. A link
  the person clicks to another site appears only where the owner allowed outside links
  (`GET /api/v1/settings` `outside_links`), with `target="_blank" rel="external noopener noreferrer"`.
- No `Date.now()` for "now" (use `api.now()`), no times outside the car's zone, no numbers without units.
- No period control of a page's own, no car picker of its own: the frame has them.
- No `data-cover` for something the page does not really do; no frame items in a page.
- No real data in examples or fixtures: made-up VINs end in a zeroed serial (`5YJ3E1EB0MF000002`).

## Whole pixels

Pages are checked with `node ui/tools/check.js --pixel` (see `ui/tools/README.md`). Keep them on whole pixels:

- Lengths are `round(<n>rem, var(--e))`: `--e` is 1 px at the standard size, where the design's sizes are whole
  px already, and 2 px at any other size, so a box centred in another lands on a whole px. Font sizes stay as
  they are.
- Line heights are set as `--LH`, never `line-height`: `--LH: round(calc(1.3 * 1em), 2px)` (a number) or
  `--LH: round(1rem, 2px)` (a length). A rule in `app.css` makes `--LH` each element's line height, so a number
  still follows each element's own font size. The `font` shorthand is followed by `line-height: var(--LH)`.
- Bars, strips and marks placed by a fraction (`left: var(--x)`) round their edges: `round(var(--x), 1px)`.
- `core/snap.js` keeps the page on whole px. Nothing to call from a page. Each run:
  - rounds each grid's `fr` columns (up, the widest give back a pixel each) and each table's columns (the
    same total width);
  - grows a box outside tables whose width or height ends between two device pixels to the next one (a
    `min-width` / `min-height`), so what follows it starts on a whole pixel;
  - makes each table row a whole number of px high, with an even height inside its cells, and moves a cell
    centred up and down by a whole px when its content is a pixel or so off the centre the other cells share;
  - last, moves a box with a border or a 1 px line that still starts between two device pixels by less than
    half a pixel (`translate`, or `position: relative` for an inline box) onto whole ones.
  It first takes back everything the last run set, so the CSS always decides the layout and the snap only
  rounds it. It runs again when the page adds or takes away elements, when text in a table changes, when a
  snapped grid or table changes size (not when the run itself changed it) and when the fonts have loaded. It
  takes about 1 s on a page of 40,000 elements: it reads all sizes before it writes any.
- Mark a box the snap must leave alone with `data-nosnap` (maps, charts and the busy ring `.spin` are left alone already).
- A box that sits in a line of text (an inline-flex pill or button) is a source of half pixels: in a table cell
  give it `vertical-align: top` or make it a block; in a flex row with `align-items: baseline`, let the words
  give the baseline and centre the icon with `align-self: center`.
