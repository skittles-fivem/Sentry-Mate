# Instrument pack: how to build a page

This is the guide for writing pages in the Instrument UI pack. Read it with the page plan
([`docs/parity/ui-parity.md`](../../../../../docs/parity/ui-parity.md)) and the API
([`docs/ui-api.md`](../../../../../docs/ui-api.md)). Everything a page needs is here: the module shape, the
`ctx` it gets, every core helper, the design tokens and classes, coverage marks, readiness, and the rules.

## Run it

```bash
python3 ui/devserver/serve.py --pack ui/packs/instrument --port 8000      # then open http://127.0.0.1:8000/
node ui/tools/check.js --pack instrument --pages drives,drive --allow-missing   # check pages
```

The dev server serves `web/` at `/` and the made-up `/api/v1` (see `ui/devserver/README.md`). Any path that is
not a file gets `index.html`, so every route works on reload. Paths in `index.html` are absolute (`/app.css`,
`/core/app.js`), because pages live at deep URLs like `/cars/c1/drives/123`; paths inside the JS and CSS are relative
to their own file (`url('fonts/x.woff2')`, `new URL('../pages/drives.css', import.meta.url)`), so they follow the
server's versioned prefix (`/_v/<version>/...`) that `index.html` is rewritten to.

## Files

```
index.html          shell markup only: the icon sprite, #sidebar, #notices, #topbar, #view, #footer, #flash
boot.js             runs in <head> before the first paint: Interface size (this device's own size from
                    sentrymate.device-size when set, else the account's) and theme from this browser's copy
app.css             tokens, light theme, root size, base, frame, shared components, tiers
manifest.webmanifest, icons/   the app can be installed (home screen, standalone window)
core/               the frame and the helpers (this folder)
pages/<id>.js       one module per page id (and sign-in, setup); pages/<id>.css when the page needs its own rules
shared/             copies of ui/shared/*.js (edit those in ui/shared, then python3 ui/tools/sync_shared.py)
vendor/             MapLibre GL JS and uPlot, loaded on first use
```

## A page module

`pages/<id>.js`, where `<id>` is a page id of `coverage-matrix.json` (`drives`, `settings-cars`, ...). The router
imports it the first time the page is opened.

```js
import { h, replace } from '../shared/dom.js';
import { pageHead, panel, metric } from '../core/page.js';
import { dataTable } from '../core/table.js';

export const id = 'logbook';        // the page id
export const css = true;            // optional: link /pages/logbook.css while this page is shown
export const title = (ctx) => 'Logbook';   // optional: the tab title; default is the menu title

export async function mount(ctx) {
  const data = await ctx.api.get(`cars/${encodeURIComponent(ctx.car.id)}/logbook`, ctx.params);
  if (!ctx.alive()) return;          // the person left while it loaded
  replace(ctx.view,
    pageHead({ eyebrow: ctx.car.name, title: 'Logbook', sub: [ctx.period.label] }),
    panel({ title: 'Totals', icon: 'book', cover: 'dt.export.csv' }, metric('Distance', data.km, 'distance')));
  ctx.ready();                       // first render with data is done
  return { unmount() {} };           // optional: extra clean-up (most pages need none, see ctx.onCleanup)
}
```

A page that is not built yet is three lines (`core/placeholder.js` draws its title, purpose, the car and the period):

```js
import { placeholder } from '../core/placeholder.js';
export const id = 'battery';
export const mount = placeholder();
```

Registering a page: its id, title, menu group, icon, one-line description, finder words, whether it needs a car
and its default period are in `core/pages.js` (`PAGES`). The route is in `shared/state.js` (`ROUTES`). All 22
page ids and `sign-in` and `setup` are already registered; a new page only needs its module.

When a page is mounted again: another page, car, drive, charge or period builds it again from scratch. A change of
the query that the page made itself (`ctx.setQuery`) does not; nor does a `ctx.setQuery` that changes nothing. Back,
forward or a link to other filters of the same page build it again, unless the page exports `onQuery(query, ctx)`:
then that takes the new query (`ctx.query` is already updated), and the page is ready again when it resolves. A
page that was left cannot change the address any more (`ctx.setQuery` does nothing then).

Fast page changes: a page may export `prefetch(ctx)`. The router calls it when a link to the page is under the
pointer for 50 ms, touched or focused, with the `ctx` that `mount` will get (car, period, params, query, state,
me; `ctx.prefetching` is true), but its `ctx.api` only warms answers (`prefetchApi` in `shared/prefetch.js`). It must
make the same calls as the page's first reads, so build them in one function both use (`firstReads(ctx)` in
`pages/battery.js`): a call with other parameters warms an answer nobody asks for. The router also loads the page's
module and style sheet then, and once the first page is drawn the main sections' modules load while the browser
is idle. A page (not the settings forms) takes kept answers at once for the reads it makes before `ready()`; each is
checked at the same time, `<html data-stale>` dims the page while one is on screen, and a page whose answers
changed is built again in place, like a live refresh. On the first page after a reload the service worker's kept
answers can come first the same way.

## `ctx`

| Field | What it is |
|---|---|
| `id`, `info` | The page id and its `core/pages.js` entry (`title`, `purpose`, `sections`, `period`, ...). |
| `view` | The `<main>` to draw into. Start with `replace(ctx.view, ...)`; the router shows a loading skeleton until then. |
| `state`, `query` | The address (`shared/state.js`): `page, car, drive, charge, period, query`. `query` holds page filters as text. |
| `car`, `carId` | The car (`GET /cars/{car}`: list item plus `config`, `time_zone`) on car pages, else `null`. An unknown or hidden car never reaches the page: the router shows "not found". |
| `tz` | The time zone for days: the car's, else the last car's, else the browser's. |
| `period`, `params` | The period resolved in `tz`: `{ preset, from_ms, to_ms, label }` (the address's, else this tab's, else the page's default), and `{ from_ms, to_ms }` for the API. `null` on pages without a period. |
| `api` | A scoped API client (`shared/api.js` `scope()`): `get, page, list, pager, post, put, patch, del, events, csvUrl, url, now`. Everything it started is cancelled when the page is left. |
| `me`, `prefs`, `admin` | The account (`/me`), the preferences, and whether it is an admin. |
| `alive()` | False once the page was left. Check it after every `await`. |
| `ready()` | Sets `<html data-ready="<id>">`. Call once, after the first render with data. |
| `fail(err, { retry })` | Shows the error state (cause and a way out) and sets `data-ready="error:<id>"`. `retry: null` hides Try again. |
| `onCleanup(fn)`, `on(target, type, fn)`, `every(ms, fn)`, `events(types, fn)` | Clean-up, listeners, timers and live events that end with the page. |
| `setTitle(...parts)` | The tab title: `setTitle('Drive Nov 15, 12:17 PM')` gives `Drive Nov 15, 12:17 PM · Comet · Sentry Mate`. |
| `setCrumbs([{ label, href }])` | The breadcrumb in the top bar (the router sets a usual one). |
| `frame.carChanged(car)` | (`core/frame.js`) After a page renamed a car or changed its time zone: the sidebar car card and the top-bar clock follow at once. |
| `link(page, params)` | A link that carries the car and period (`state.linkTo`). |
| `setQuery(patch)` | Puts filters in the address (`null` removes one) without rebuilding the page. |
| `navigate(href)`, `reload()` | Go somewhere; build this page again (keeps the scroll). |
| `refresh()` | Live data changed (a `rows` event): build the page again off screen, in an invisible box of the view's size, and swap it in whole once it is ready, drawn and its requests are done. The page on screen stays until then, so nothing goes away, shows a skeleton and comes back. While the page on screen is scrolled (and for half a second after) its snap and the swap wait: a new page puts its maps and charts on screen in one task, longer than a frame on a big screen. Asked before the first `ready()`, it waits for it. |
| `#id` in a link | After the page mounts, the router scrolls to the element with that id in the view (below the top bar): `/cars/c1/charges?…#habits`. Give a section an `id` for links to land on it. |

Live data: `ctx.events('now', (ev) => ...)` gets `now` deltas (`ev.data = { car, now }`, merge field by field);
`ctx.events('rows', ...)` fires when a drive, charge, parked spell or gap starts or ends (reload the page's lists).

Stillness (the owner's rule, checked by `node ui/tools/check.js --pack instrument --stable`): a page must not jump,
and nothing may show up, go away and come back, on first load or while live data arrives. So: call `ctx.ready()`
only after every section has its data and its charts (load uPlot with the data, not after ready); on a `rows` event
reload tables (`t.reload()` keeps the rows on screen until the new ones are in) or call `ctx.refresh()`, never
paint skeletons or "loading" again; a part with charts that is drawn again uses `offstage(slot, build)`
(`core/stage.js`), which builds it off screen and swaps it in at once; reserve the space of text that arrives later.
Never poll on a timer for data.

Below the fold: `mountPlot` builds a chart and `createMap(el, ctx, { lazy: true })` makes a map (and loads MapLibre) only when its box comes
within a screen of view (`shared/lazy.js` `whenNear`), so the first paint draws only what is on screen. The box has
its final height from CSS from the start, so nothing moves when it is drawn; `plot.u` is null until then and
`setData` keeps the newest data. `createMap` waits until then, so a page that waits for its map before `ready()` does
so only when `nearNow(el)` (`shared/lazy.js`) says it is near; ready never waits for a part below the fold.

## Core helpers

### `core/page.js`: building blocks

| Helper | Use |
|---|---|
| `pageHead({ eyebrow, title, titleExtra, sub, purpose, actions, back, cls })` | The page head. `sub` is text, an element or a list (dots between). `back: { href, label }`. |
| `panel({ title, icon, side, pad, cls, cover, id }, ...children)` | A frosted panel with a head. `side`: elements at the right of the head. |
| `panelLink(href, text)` | A small "Timeline ›" link for a panel head. |
| `metric(label, value, kind, { explain, size: 'sm' \| 'lg', why, digits, sign, cover })` | A figure with its unit and a line that says how it was worked out. A null value shows a dash and `why`. `kind` null: `value` is text or an element. |
| `figures([...])`, `grid(...)`, `span(n, ...)` | A wrapping row of figures; the 12-column grid. |
| `csvButton({ href, filename } \| { onClick })` | The Download CSV button: a server CSV link, or CSV built in the browser. |
| `appLink(page, text, params)`, `note(...)`, `pill(text, { tone, ic, lg })`, `tag(text, tone)`, `kv(rows)` | Small pieces. `kv([[label, value, { icon, cover, title }], ...])` is a key-value list. |

### `core/table.js`: tables

```js
const t = dataTable(ctx, {
  title: 'Drives', icon: 'route', path: `cars/${id}/drives`, params: ctx.params,   // or rows: [...] for local data
  columns: [
    { key: 'start', label: 'Start', sort: 'start', render: (d) => formatTime(d.start_ms, ctx.tz), cover: 'drives.table.date', csv: (d) => d.start_ms },
    { key: 'distance', label: 'Distance', num: true, sort: 'distance', render: (d) => format(d.distance_km, 'distance') },
  ],
  sort: { key: 'start', dir: 'desc' }, rowHref: (d) => buildUrl('drive', { car: id, drive: d.id }),
  empty: () => emptyState({ title: 'No drives', text: 'Say which filter removed everything.' }),
  onPage: (pg) => paintSummary(pg.summary), note: 'One line that explains the table.', cover: 'drives.table',
});
ctx.view.append(t.el); await t.ready;     // t.reload(params), t.setParams(patch), t.setRows(rows), t.rows()
// t.reload() keeps the rows (or the empty state) on screen until the new first page is in; { quiet: false } shows the skeleton
```

An API list is sorted by the server (`sort` is the API's sort name) and paged by cursor with a "Show more" button;
local `rows` sort here (`sort: true`, `value(row)` for the sort value). Every table has a Download CSV button: the
server's CSV for API lists (same filters and sort), else CSV built from the rows (`csv: false` leaves a column out;
`csv: column('Distance', 'distance_km', 'distance')` from `shared/csv.js` names the unit in the header).
An API list that should download readable columns instead of the server's file passes `csv: { all: true }`
(every page of the list, built from the columns' `csv` specs; `csv: {}` takes only the rows shown).
`autoMore: true` also loads the next page as the foot scrolls into view. `box: true` (or the box's accessible name)
puts a long log-like list in a box of a header row and five rows that scrolls by itself (`scrollBox` from
`shared/scrollbox.js`: a focusable region, `data-below` and a fade while rows are hidden under its bottom edge, the
column header sticky at its top). `app.css` gives `.scroll-box` its height from the start, so the page never grows
while rows load; with `autoMore` the next page loads as the end of the box comes near, and Show more stays under
the box for the keyboard. Local rows, and an API list whose first page holds all of it, get `.scroll-box--fit`:
only as high as the rows, up to five. A list that is not a table uses `scrollBox({ cls, label }, list)` itself and
sets `--box-row` (and `--box-head: 0px`) when its rows are not one table row tall. The CSV still has every row.
Boxed: the update and firmware histories (Software), the gaps, unknown time and storage by day (Data health), the
problems of the Connections overview, the broker log and recent events (Settings: Integrations), parked drain
(Battery), the storage categories and the audit log (Settings: Server and access). Drives, Charges and Logbook
stay full-page lists.

`windowTable(ctx, { title, path, params, columns, sort, firstSort, search, jump, csv, onPage, empty })` is for a list
of thousands of rows (the Logbook): the rows sit in a box ten rows high (`.vl-box`, its height from `app.css`) that
scrolls under a header that stays, only the rows in view and a few more are in the page (`shared/vlist.js`), and they
load from the API a chunk at a time as the box reaches them (by cursor when the chunk goes on from one already in, else
by `offset`; Jump to date asks with `at_ms`). Every row has the same height: each cell holds at most two lines that do
not wrap. Sorting is the server's (`sort` is the API's sort name), the search field sends `q`, the Download CSV link
is the server's with the same filters (every row they leave), and every answer goes to `onPage` (the Logbook's totals
come with each). It keeps the table form on a phone (`.tbl--keep`), so rows stay one height there too.

### `core/chart.js`: charts on uPlot

`await loadUPlot()` once, then `mountPlot(el, make(width, height) => uPlotOptions, data, ctx, onBuild)`. The box `el`
is sized by CSS in rem (`.chart { height: 9rem }`); the chart follows it, and is built again when the pixel ratio,
the Interface size or the theme changes; a chart mounted again into the same box replaces the one there. Helpers for the options: `timeAxis(tz, { show, spanMs })`,
`valueAxis({ digits, size })`, `colors()` (theme colours now), `areaFill(color)`, `hexA(color, a)`, `chartFont()`,
`toUnits(values, kind)`. Ready-made: `timeChart(el, ctx, { tz, t_ms, series: [{ label, values, kind, color: '--c-soc', area }], range, sync, zoom(fromMs, toMs), onCursor(i) })`
and `barChart(el, ctx, { t_ms, values, kind, color, tz, onCursor })`.

Rules: convert to the person's units before plotting (`kind`); keep `null` (lines break where nothing is known; only
asleep time may carry a value across, see Car overview); size everything uPlot draws with `rpx()`; charts on one page
share a cursor with `cursor.sync.key`; a legend that hides series is a row of `.legend-chip` buttons (see
`pages/car.js`); every chart gets a CSV button.

### `core/map.js`: maps on MapLibre

```js
const m = await createMap(el, ctx, { center: [lon, lat], zoom: 15 });   // or { bounds }
if (!m) return; await m.ready;
addRoute(m, coords);  routePin(m, coords[0], 'A');  carMarker(m, [lon, lat], { tone: 'live', heading });
m.addLayer({ id, type: 'circle', source, paint }, { 'circle-radius': () => rpx(4) });   // sizes follow the root size
m.frame(() => m.map.fitBounds(boundsOf(coords), { padding: padClear(el, card), duration: 0 }));
```

Tiles are offline first (`GET /maps/config`): Sentry Mate's own tiles, else the online source an admin turned on,
else none (the map shows its layers on a plain background with a note). Wrap `createMap` in try/catch and show
`mapFallback()` when it throws (no WebGL). Put cards on the map with `.map-overlay--tl|tr|bl|br`.

### `core/form.js`: forms with field errors

```js
const name = field({ label: 'Name', name: 'name', required: true, hint: 'Shown in every list.', path: 'name' });
const f = form({ fields: [name], submit: 'Save', async onSubmit(values) { await ctx.api.patch(url, values); flash('Saved.'); } });
```

`field()` types: text, password, number, email, date, time, select (`options: [[value, label]]`), checkbox, textarea.
The browser's own checks run first. An `ApiError` thrown from `onSubmit` puts each `error.fields[path]` message under
its field (`path` is the API's field path, `units.distance`) and the rest in the form's message line
(`showApiError(f, err)`). Settings that save at once (no Save button) call the API on `change` and show a flash note.

### States, notes, dialogs

| Helper | Use |
|---|---|
| `core/states.js` `loadingPage(n)`, `skel(kind, { width })` | Loading skeletons (the router shows `loadingPage` before `mount`). |
| `emptyState({ icon, title, text, action, warn })` | Empty: always say exactly why it is empty and how to change that. |
| `errorState(err, { retry, home, compact })`, `errorWords(err)`, `statePanel(child)` | Errors: the cause in words and a way out. Prefer `ctx.fail(err)` for a page that cannot load. |
| `core/flash.js` `flash(text, { kind: 'ok' \| 'info' \| 'error', next })` | A short note after an action. `next: true` shows it on the page you navigate to. |
| `core/morph.js` `morphChildren(slot, nodes)`, `morph(el, to)`, `keepEl(el, name)` | Live figures in place: build the part again from new data and only the texts and attributes that changed are written (nothing moves). With `shared/live.js` `follow()`: live events, polling only while the live stream is down. |
| `core/dialog.js` `openDialog({ title, body, actions })`, `confirmDialog(text, { danger, ok })` | Native `<dialog>`: focus moves in and back, Esc closes. |

### Formatting: `core/fmt.js` (on `shared/units.js` and `shared/time.js`)

`q(value, kind)` (element: number and unit that never part), `qt(value, kind)` (text), `unit(kind)`, `num(v)`,
`money(list)` (never adds currencies), `val(pair)` (the value of a live `{ value, at_ms }`), `time(ms, tz)`,
`date`, `dateTime`, `when(ms, tz)` ('Today, 5:30 PM'), `dur(ms)`, `ago(ms)` (against the server's clock),
`agoEl(ms, tz)` (refreshed by the frame every 15 s), `statePill(state, { stale, lg })`, `stateLabel`, `STATES`,
`TYRES`, `placeName(p)`, `carMeta(car)`.

Every number shows its unit: use `format(value, kind)` from `shared/units.js` (text) or `quantity(value, kind)` from
`shared/dom.js` (an element) with the kind
named in the API field (`distance`, `energy`, `efficiency`, `pressure`, ...). The API is metric; never convert by hand.
To read a typed value back (a filter in the person's units) use `toMetric(value, kind)`.

Times are in `ctx.tz` (the car's zone), days are the car's days, weeks are ISO weeks; use `shared/time.js` for
buckets and ranges, and `ctx.api.now()` for "now" (the server's clock, not the browser's).

### The period, the car, links

The period picker in the top bar is shared by every page that has a period (`info.period` in `core/pages.js` is its
default). A page never keeps its own period: it reads `ctx.period` and the router builds it again when it changes. A
button on the page can open the same picker: `openPeriodPicker(button)` from `core/period.js`. `openCarSwitcher()`
from `core/carswitch.js` opens the car switcher, which keeps the page.

Links inside the app are plain `<a href>`: the router turns clicks into navigation. Build them with `ctx.link(page)`
(carries car and period) or `buildUrl(page, { car, drive, charge, period, query })` from `shared/state.js`.
A place cell links to the place editor: `buildUrl('places', { query: { place: id } })`, or
`{ query: { lat, lon, new: 1 } }` for a spot that is not a place yet.

### The frame (for reference)

`core/app.js` starts it; `core/router.js` mounts pages; `core/session.js` decides where the home address leads
(set-up first, then sign-in with `?next=`, then the page); `core/frame.js` draws the sidebar (menu grouped by task,
car switcher, account menu), the top bar (menu button, breadcrumb, period, live status, the car's clock), the notices
bar (`core/notices.js`) and the footer; `core/finder.js` is the full menu with type-to-find; `core/account.js` the
Interface size, Follow screen size and theme controls; `core/prefs.js` the account and preferences
(`getMe`, `isAdmin`, `getPrefs`, `savePrefs(patch)`, `onPrefs`). Pages do not touch the frame except through `ctx`.

## Design tokens and classes

Colours are CSS custom properties on `:root` (dark) and `:root[data-theme="light"]`; never write a colour into a
page's CSS or JS, read the token (`cssVar('--live')` in JS).

| Tokens | For |
|---|---|
| `--bg-0..2`, `--panel-solid`, `--glass*`, `--line`, `--line-2`, `--line-3`, `--surface-*` | Backgrounds, panels, lines |
| `--text`, `--text-2`, `--text-3` | Text: main, secondary, faint |
| `--live`, `--live-2`, `--live-3`, `--live-soft`, `--live-line` | The cool accent: live data, links, focus |
| `--warm`, `--warm-soft`, `--warm-line`, `--danger`, `--danger-soft` | Charging and warnings; errors |
| `--st-driving`, `--st-charging`, `--st-parked`, `--st-asleep`, `--st-unknown`, `--asleep-fill`, `--nodata-fill` | Car states (always with a label); asleep is hatched, unknown grey |
| `--c-speed`, `--c-power`, `--c-regen`, `--c-soc`, `--c-elev`, `--c-charge`, `--chart-grid`, `--chart-zero` | Chart series |
| `--radius`, `--radius-sm`, `--gap`, `--gutter`, `--fs-floor` | Shape, spacing, the 10 px text floor |

Classes to use: `.panel .panel-pad .panel-head .panel-title .panel-link` (panels), `.page-head .page-title .page-sub
.eyebrow .page-actions .back` (head), `.metric .metric-label .metric-value .explain .figures` (figures), `.btn
.btn--primary .btn--ghost .btn--sm .btn--icon .btn--danger` (buttons), `.seg .seg-btn[aria-pressed]` (segmented
choices), `.input .select .field .field--search .toolbar .filter-bar` (inputs), `.pill .tag .key-chip .age` (labels),
`.tbl` (via `dataTable`), `.kv` (key-value), `.bar > i` with `--w` (a bar), `.hbars` (a bar list), `.legend-chip`
(chart legend), `.tl-*` (timeline rows, `timelineRow()` from `pages/timeline.js`), `.map-panel .map .map-overlay
.glass-card .place-card` (maps), `.empty .state-panel` (states), `.note .muted .faint .nowrap .mono .warn .ok`.
Icons: `icon('route', 'ico--sm')` from `core/icons.js` (ids are the `<symbol id="i-...">` in `index.html`).

Layout: the 12-column `.grid` with `.span-N`, or the page's own grid in `pages/<id>.css`. No max width: wide screens
get more columns, not a narrow column in the middle.

## Sizes, tiers and 4K

- The root font size is browser default x Auto x Interface size (set only in `app.css` "root size" and `boot.js`).
  Auto follows the screen, not the window
  (`screen.width` x `screen.height` in CSS px): 1 up to 1440 x 810, rising in a straight line to 1.45 at
  3840 x 2160 and capped there; the long and the short side both count (a portrait 4K screen grows like a
  landscape one), and in Firefox, which reports the screen scaled by its page zoom, Auto is 1. Follow screen
  size off makes Auto 1. Interface size is 75 to 200 % (whole numbers). This rule is the same in every pack. Both are the person's
  preferences (`interface_size`, `follow_screen`), cached in this browser only for the first paint.
- Every length is in rem (1 px hairlines aside). The smallest text is `max(<rem>, var(--fs-floor))`: never under 10 px.
- Breakpoints are container queries on the page in rem, never px media queries: up to 50rem tight (200 % browser
  zoom on a 1280 to 1440 px laptop: one panel per row), up to 76rem compact (icon sidebar), up to 85.375rem narrow,
  the base design, from 108rem wide, from 140rem xwide. Put a page's tier rules in its own CSS with
  `@container page (max-width: 85.375rem) { ... }`. Check the tight tier with `--sizes 640x400,720x450`.
- Canvases draw at `devicePixelRatio`: use `mountPlot` and `createMap`, and size what they draw with `rpx()`.
- A table wider than its panel scrolls inside `.tbl-scroll`; nothing may make the page itself wider than the window.

## Phones

The phone layout follows Sentry Manager's: one column, a compact bar of the main sections instead of the
sidebar, tables as cards, 44 px tap targets, 16 px fields. `boot.js` sets `<html data-phone>` before the
first paint, and again when the window changes:

| `data-phone` | When | Frame |
|---|---|---|
| `tall` | the window is at most 48rem wide (an upright phone, a small tablet, a laptop at 200 %) | the dock at the bottom holds the period bar (pages with a period) and the tab bar |
| `short` | at most 30rem high and 60rem wide (a phone on its side) | the period stays in the top bar; a lower tab bar |
| (none) | anything else | the desktop frame |

- The sidebar is hidden; the top bar holds the car (opens the switcher), the live dot and the account. The tab
  bar has All cars, Overview, Drives, Charges and Menu (the finder: every page). `window` gets a
  `phonelayout` event when the attribute changes.
- Tokens on a phone: `--fs-floor` 14px (text never below 14 px), `--tap` 44px, `--gutter` 16px (or the
  safe-area inset), `--dock-h` (the dock's height, the page keeps that much room at the bottom).
- Buttons, fields, segmented choices, panel links, legend chips, the back link and the map's zoom buttons are
  44 px tall; fields use 16 px text so iPhones do not zoom into them.
- Dialogs, the menu, the car switcher, popovers and the account menu are sheets from the bottom, as wide as
  the screen, and fit above the keyboard (`interactive-widget=resizes-content` in `index.html`). The notch and
  the home bar are kept clear with `env(safe-area-inset-*)` (`viewport-fit=cover`). No pull-to-refresh.
- `dataTable()` rows become cards: the first cell is the card's title, the others have their column's name
  over them (`td[data-label]`; a column's `cardLabel` overrides it), and a select in the table's bar picks
  the order. A table a page builds itself with class `tbl` gets its captions from `cardLabels(table)`; a
  matrix of numbers that must stay a table gets `tbl--keep` (scrolls sideways in its panel, first column held).
- A page's own phone rules go in its `pages/<id>.css` under `:root[data-phone]` (they never change the
  desktop). Check with `node ui/tools/check.js --pack instrument --sizes 390x844@3,360x780@3,844x390@3,1440x900 --pixel`.

## Coverage marks and readiness

- The element that does what an inventory item needs carries `data-cover="<item id>"`: `h('div', { cover: ['a', 'b'] })`
  or `cover(el, 'a')`. Item ids for a page are in `docs/parity/coverage-matrix.json`; their details in the page plan.
- Mark an item only when the page really does it. Data the car does not send is said plainly (a note that names what
  is missing is how the plan covers "not on CAN" items). An item in a closed dialog or tab is marked on what opens
  it. An element that only shows in some states (an empty state) may stay in the page hidden, and be marked.
- Frame items are marked by the frame; a page marks only its own page's items.
- `ctx.ready()` after the first render with data; `ctx.fail(err)` when the page cannot load. Charts and maps may
  finish after `ready()`.
- Check with `node ui/tools/check.js --pack instrument --pages <id> --sizes 1440x900,3840x2160`: it reports missing
  items, console and page errors, CSP violations, failed API calls, outside requests, overflow and small text, and
  saves screenshots. Look at the screenshots.

## Do not

- No inline style attributes, `innerHTML`, `insertAdjacentHTML`, string event handlers or `javascript:` links: build
  with `h()`; set dynamic sizes as CSS custom properties (`vars: { '--w': '40%' }`), which goes through CSSOM.
- No `fetch` or `EventSource` outside `shared/api.js`; every change goes through `ctx.api` (it adds the CSRF header).
- No outside requests: no fonts, scripts, styles or images from other hosts. Map tiles come only through
  `core/map.js`; links to other sites open only on the viewer's click, with `rel="noopener noreferrer external"`.
- No unit conversion or date maths by hand; no `new Date().getHours()` (that is the browser's zone, not the car's).
- No colours, px sizes or media queries in pages; no copied TeslaMate text, code or layout.
- No timers that poll the API: use the live events.

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
  snapped grid or table changes size (not when the run itself changed it) and when the fonts have loaded. A
  whole run takes about 1 s on a page of 40,000 elements (it reads all sizes before it writes any), so after a
  change of content only the boxes that hold it are snapped again (a card drawn again, a table's new rows),
  unless one of them changed size other than by whole pixels: a page that reloads its cards while it scrolls
  (a rebuild sends `rows` every second) stays under a frame. Tables that load again keep unchanged rows
  (`reuseRows`) and the page parts that live values draw again keep what did not change, so most reloads
  change nothing at all.
  A long list (`shared/vlist.js`) is snapped once its scroll rests (150 ms without a change): the rows that
  come into and go out of its window, and its table's height, which changes as the window moves over rows of
  other heights (a run over the whole page for each such change took most of every frame of a scroll).
- Mark a box the snap must leave alone with `data-nosnap` (maps and charts are left alone already).
- A box laid over the page (absolute or fixed, taking no room) that comes and goes: `data-snap-over` as well, so adding
  or taking it away does not snap the page again (the router's copy of the loading skeleton).
- A box that sits in a line of text (an inline-flex pill or button) is a source of half pixels: in a table cell
  give it `vertical-align: top` or make it a block; in a flex row with `align-items: baseline`, let the words
  give the baseline and centre the icon with `align-self: center`.
