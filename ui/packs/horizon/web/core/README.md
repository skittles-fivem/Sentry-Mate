# Horizon pack: how it is built

Horizon is a synthwave dashboard: neon HUD panels over a dusk sky, a striped setting sun and a grid floor that
runs to the horizon. Pages are tuned like a car radio. Read this with the page plan
([`docs/parity/ui-parity.md`](../../../../../docs/parity/ui-parity.md)) and the API
([`docs/ui-api.md`](../../../../../docs/ui-api.md)).

## Run it

```bash
python3 ui/devserver/serve.py --pack ui/packs/horizon --port 8000          # then open http://127.0.0.1:8000/
node ui/tools/check.js --pack horizon --pages drives --sizes 1440x900,3840x2160,390x844@3 --pixel
```

## Files

```
index.html        the shell: the icon sprite, #sky, #dash, #tuner, #console, #notices, #view, #foot, #toasts
boot.js           runs before the first paint: root size (Auto x Interface size), look, Background choice
app.css           tokens (Dusk and Daybreak), base, components: HUD panels, plate, readouts, buttons, fields,
                  choices, tags, states, dialogs, popovers, toasts, gauges
frame.css         the sky, the dashboard strip and radio, band menus, the console row, notices, footer,
                  the phone tuner and Quick knob, the sign-in and set-up screens, the tiers
kit.css           parts of data pages: control deck, track lists, figures, counters, bars, strip, charts, maps
core/             the frame and the helpers (below)
pages/<id>.js     one module per page id, plus sign-in and setup; pages/<id>.css when a page needs its own rules
shared/           copies of ui/shared/*.js (edit those, then python3 ui/tools/sync_shared.py)
fonts/            Orbitron (display), Chakra Petch (text), JetBrains Mono (digits), with their licences
vendor/           MapLibre GL JS and uPlot, loaded only by pages that draw maps or charts
```

| Module | What it does |
|---|---|
| `core/main.js` | Entry: the background, the frame, the router; a lost session goes to sign-in and back |
| `core/stations.js` | Every page in dial order, the six bands, `tune(id, step)` and `channel(id)` |
| `core/router.js` | Address to page: admission, the car, the period, the module, `ctx` |
| `core/session.js` | Set-up first, then sign-in, then the page; `signedIn()`, `signOut()` |
| `core/prefs.js` | The account and preferences; `savePrefs(patch)` uses a change at once, then saves it |
| `core/display.js` | Interface size, Follow screen size, the look, `rpx()`, `onRedraw()` for canvases and maps |
| `core/sky.js` | The background; `setSky('auto' \| 'full' \| 'calm' \| 'off')`, `holdSky(reason, on)` |
| `core/frame.js` | The strip, radio, band menus, tuner, Quick sheet, console row, footer, live events, keys |
| `core/period.js` | The period dial; `openPeriod(anchor)` opens the same picker from a page |
| `core/pickers.js` | `openCarPicker()` (keeps the page) and `openScan()` (find a page) |
| `core/account.js` | Size, follow, look and background controls; used in the account menu and the Quick sheet |
| `core/notices.js` | Server notices with their fix links, and the offline line |
| `core/layer.js` | `openDialog`, `confirmDialog`, `popover`, `toast` |
| `core/form.js` | `field()`, `form()`, `showApiError()` |
| `core/ui.js` | `icon`, `hud`, `plate`, `readout`, `readouts`, `btn`, `linkBtn`, `seg`, `tag`, `lamp`, `kv`, `callout`, `note`, `bar`, `place` |
| `core/states.js` | `loadingDeck`, `state`, `emptyState`, `errorState`, `errorWords`, `offlineState`, `signedOutState`, `statePanel` |
| `core/gauges.js` | Cluster parts: `dial` (speedometer style), `fuel` (fuel style), `odo` (trip counter digits), `lights` (warning lights) |
| `core/fmt.js` | `q`, `qt`, `num`, `money`, `val`, `time`, `date`, `dateTime`, `when`, `dur`, `ago`, `agoEl`, `zone`, `stateWord`, `carLine`, `carStatus`, `placeName` |
| `core/kit.js` | Parts of data pages: `controls`/`slot` (the control deck), `periodBtn`, `carBtn`, `select`, `search`, `csvBtn`, `placeLabel`/`placeHref`/`placeLink`/`route`, `rack`/`stat` (figures), `counters` (trip counters), `eq` (equaliser bars), `spark`, `band` (numbered section head), `sourceTag`, `plural` |
| `core/tracks.js` | `tracks(ctx, opts)`: the track list (table) from an API list or local rows, sort, Load more, CSV; cards on phones |
| `core/strip.js` | `strip(items, from, to, opts)`: the activity strip (one SVG), with its marks and key |
| `core/chart.js` | uPlot: `scope()` (traces on a time axis, synced cursor, drag to zoom), `columns()` (bars per bucket), `well()` |
| `core/map.js` | MapLibre: `createMap()`, `addTrace()` (neon route), `pin()`, `boundsOf()`, `pad()`, `mapFallback()` |
| `core/placeholder.js` | The page shown until a page is built |

## The frame

- **Computers:** the dashboard strip. Brand at the left; the bands Cars, Driving, Charging at the left of the
  radio and Energy, Reports, Set-up at its right; each band button opens its menu (a line per page and its
  channel). The radio display in the exact middle shows the channel and band, the page name and the car; its
  arrows tune to the previous and next page (`[` and `]`), the face opens the scan box (`/`). The account knob at
  the right holds the size, look, background and sign out. Ctrl/Cmd+K opens the car picker.
- **Under the strip:** the console row: the car cartridge (car picker), the period dial (only on pages with a
  period) and the car's clock; then the server notices; the footer has the version, licences and release notice.
- **Phones** (below 52rem): the radio fills the strip (swipe it to tune), the tuner bar under it scrolls through
  every page (the one tuned to sits in the middle), and the round Quick knob opens a sheet with find, car,
  All cars, settings and the account controls.
- **Tiers** are container queries on `#app` and `#view` in rem, so the Interface size and browser zoom move them:
  brand words go below 76rem, band labels below 64rem, phone below 52rem. Pages use `@container view (...)`.
- The dash is sticky when the window is at least 560 px high.

## A page module

```js
import { h, replace } from '../shared/dom.js';
import { plate, hud, readout, readouts } from '../core/ui.js';

export const id = 'battery';
export const css = true;                     // optional: /pages/battery.css while the page is open
export const title = (ctx) => 'Battery';     // optional tab title; default: the station's full name

export async function mount(ctx) {
  const now = await ctx.api.get(`cars/${encodeURIComponent(ctx.car.id)}/now`);
  if (!ctx.alive()) return;
  replace(ctx.view,
    plate({ kicker: `Energy · ${ctx.car.name}`, title: 'Battery', aim: ctx.info.aim }),
    hud({ title: 'Now', icon: 'battery', cover: 'battery.now' }, readouts(readout('Charge', now.soc_pct?.value, 'percent'))));
  ctx.ready();
}
```

`ctx`: `id`, `info` (the station), `view`, `state`, `query`, `car`, `carId`, `tz` (the car's zone, else the last
car's, else the browser's), `period` and `params` (`{ from_ms, to_ms }`), `api` (a scoped client: everything it
started is cancelled when the page is left), `me`, `prefs`, `admin`, `alive()`, `ready()`, `fail(err, { retry })`,
`onCleanup(fn)`, `on(target, type, fn)`, `every(ms, fn)`, `events(types, fn)` (live `now` and `rows` events; never
poll), `setTitle(...)`, `link(page, params)`, `setQuery(patch)`, `navigate(href)`, `reload()`, `carChanged(car)`.
A page that exports `onQuery(query, ctx)` takes back and forward between its own filters without being built again.

## The pages built so far

| Page | Its picture |
|---|---|
| All cars | The garage: warning lights per state that filter it, the cars that need a look, a cartridge per car with its own charge gauge |
| Cluster | The car now: speedometer, battery dial with fuel gauge and ranges, power meter, odometer, warning lights, map, tyres, charging; then the period |
| Timeline | State meters, the activity strip, and the tape of entries with a side per day |
| Drives | The playlist: drives as numbered tracks, summary counters, driving patterns, drives with missing data |
| Drive | The replay: night map, trip computer, synced traces with drag to zoom, speed bands, GPX and GeoJSON |
| Trip | The trip computer for any window, a second car riding along, time and energy, route, traces, drives and charges |
| Logbook | The pit log: numbered drives with purposes, totals per purpose, rules, CSV and PDF |
| Travel | The world tour: counters, map, cities and regions ranked like a chart show, countries, cities, days in a city, places, stops |
| Places | The preset bank (the list) and the tuner (the editor) with map, overlaps, prices, effect on history and repricing |
| Charges | The charging log: trip counters, sessions, missing data; costs, AC and DC, a battery-level heat grid and start-to-end bars on one cursor, level bands, week grid, places map, fast-charge curve |
| Charge | The charger display: battery fuel gauge from start to end, charge computer, map, synced traces with zoom, the supply, power against battery %, the cost meter and its editor |
| Battery | The pack cluster: fuel gauge with the everyday band, health dial, capacities and range, lifetime odometer, reference values, capacity history; charge level, projected range and parked drain for the period |
| Efficiency | Economy dials for net and gross, the energy factor, groups by outside temperature with their drives, another car alongside; temperatures, history, charges and each day |
| Statistics | Rows per day, week, month or year with sum counters, odometer and distance charts, each car, and milestones as lamps |
| Software | The version on an LCD face, update counters, the update history (admins name a version) and every car's version |
| Data health | A car's warning lights, fixes, keys, gaps, what is recorded, storage and signals; on /system the server's storage, cars, health and API errors |

## Looks and tokens

Two looks: Dusk (dark, `:root`) and Daybreak (light, `:root[data-theme="light"]`); the person's `theme` preference
picks one, `device` follows the system. Never write a colour in a page: use the tokens.

| Tokens | For |
|---|---|
| `--bg-0..2`, `--glass`, `--glass-2`, `--glass-solid`, `--well`, `--ring`, `--ring-2`, `--ring-hot`, `--rule` | Backgrounds, panels, wells, rings, rules |
| `--ink`, `--ink-2`, `--ink-3` | Text: main, second, faint (all AA on the glass) |
| `--mag`, `--org`, `--cyan`, `--yel`, `--grn`, `--red`, `--violet` and `--*-ink` | Neon accents; `-ink` versions for text |
| `--hot`, `--on-hot`, `--chrome`, `--radio-face` | The magenta to orange gradient and text on it; chrome headings; the radio glass |
| `--st-driving`, `--st-charging`, `--st-parked`, `--st-asleep`, `--st-unknown` | Car states (always with a word) |
| `--c-speed`, `--c-power`, `--c-regen`, `--c-soc`, `--c-elev`, `--c-charge`, `--c-temp`, `--chart-grid`, `--chart-zero` | Chart series |
| `--sans`, `--display`, `--mono`, `--fs-xs`, `--fs-sm`, `--fs`, `--fs-lg`, `--fs-floor` | Type; the smallest text is never under 10 px |
| `--r`, `--gap`, `--gutter`, `--ctl`, `--ctl-sm` | Shape, spacing, control heights |

Classes: `.hud .hud--cyan .hud--orange .hud-head .hud-title .hud-body` (panels), `.plate*` (page title),
`.deck` with `.span-3..9` (the 12-column page grid), `.readout*`, `.kv`, `.btn .btn--go .btn--ghost .btn--danger
.btn--sm`, `.seg .seg-btn`, `.input .field .field-label .field-hint`, `.tag .tag--cyan|mag|org|grn|red`, `.lamp`,
`.callout .callout--warn|ok`, `.note`, `.bar`, `.state`, `.skel`, `.chrome` (chrome text), and for gauges `.gauge*`,
`.fuel*`, `.odo*`, `.lights .light`.

## Pixel rules

- Every length is in rem; the root size is set only in `app.css` and `boot.js`.
- **Outlines are inset rings** (`box-shadow: inset 0 0 0 1px ...`), never borders, and hairlines are shadows:
  they stay crisp at any pixel ratio and never shift the layout.
- Centre with flex or grid (`align-items: center`), not with padding guesses. An icon in front of text that may
  wrap is one line high (`height: 1lh`), so its top meets the text's top and it sits on the first line.
- Line heights keep clear of each font's own height (Chakra Petch 1.30em, Orbitron 1.25em, JetBrains Mono
  1.32em): use 1.1 to 1.2 for one-line labels or 1.4 and up for text; never within 0.1 of the font's height, or
  clipped and centred text lands half a pixel off.
- Text that can be cut uses `text-overflow: ellipsis` (`.ell`).
- Check with `--pixel` at `1440x900,3840x2160,390x844@3` and look at the screenshots.

## Coverage and readiness

The element that does what an item needs carries `data-cover="<item id>"` (`cover` in `h()`); frame items are
marked by the frame (brand, band buttons, Quick knob, footer, title, toasts, manifest). Call `ctx.ready()` after the
first render with data, `ctx.fail(err)` when the page cannot load.

## Do not

- No inline styles, `innerHTML` or string handlers; dynamic sizes go through `vars: { '--w': '40%' }`.
- No `fetch` outside `shared/api.js`; no outside requests (map tiles only through the map helper).
- No unit or date maths by hand; no colours or px sizes in pages; no timers that poll the API.
- No code, text or look copied from other packs or from TeslaMate.
