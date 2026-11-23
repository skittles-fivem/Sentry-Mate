# Pebble pack: how it is built

Pebble is soft and touch first: rounded tiles in a bento grid, a floating dock at the bottom of every screen,
sheets that slide up, Manrope type, light by default and dark when asked. Read this with the page plan
([`docs/parity/ui-parity.md`](../../../../../docs/parity/ui-parity.md)) and the API
([`docs/ui-api.md`](../../../../../docs/ui-api.md)).

## Run and check it

```bash
python3 ui/devserver/serve.py --pack ui/packs/pebble --port 8000          # then open http://127.0.0.1:8000/
node ui/tools/check.js --pack pebble --pages drives,drive --sizes 1440x900,3840x2160,390x844 --pixel
```

Look at the screenshots, light and dark, desktop, 4K and phone. `--pixel` must report nothing.

## Files

```
index.html     the shell: #scroller > .shell > #head, #notices, #main (the bento grid), #foot; #dock; #toasts
boot.js        before the first paint: Auto x Interface size (or this device's own size from sentrymate.device-size),
               --dp (one device pixel) and the theme
app.css        fonts, size tokens, colours (light, dark), the frame and every shared part
core/          the frame and its helpers (below)
pages/<id>.js  one module per page id, plus sign-in, setup and move-parts (shared with Settings: Server)
shared/        copies of ui/shared/*.js: edit ui/shared, then run python3 ui/tools/sync_shared.py
fonts/         Manrope 500/700/800 and JetBrains Mono (SIL OFL 1.1, licences next to them)
vendor/        MapLibre GL JS (BSD-3-Clause) and uPlot (MIT), for the map and chart pages
```

| Module | What it does |
|---|---|
| `start.js` | Home routing: set-up first, then sign-in (`?next=`), then the page. Loads preferences and starts the frame and the router. |
| `router.js` | Leaves the old page, shows grey tiles of the page's shape, loads the car and the period, calls `mount(ctx)`. The grey tiles stay until the page puts its own content in, and the view keeps their height until the page is ready, so a page never collapses and grows again while it loads. |
| `frame.js` | The page header (brand, a clock with its time zone, account button, title, car pill, date-range pill, page buttons), the dock, the More sheet, notices, footer, offline chip. |
| `pages.js` | The catalogue: title, dock and sheet label, icon, blurb, purpose, default period, car need. `DOCK` and `SHEET` order the navigation. |
| `display.js` | Interface size (50 to 200 %), Follow screen size, `--dp`, and the bento grid in whole device pixels. `display.grid`, `display.onGrid(fn)`, `display.onChange(fn)`, `snap(px)`, `scrollToTop()`. |
| `prefs.js` | Preferences: units, theme (Auto, Light, Dark), language, size. `savePref(patch)` shows a change at once and puts it back if the server says no. `onTheme(fn)`. |
| `sheet.js` | Bottom sheets on a modal `<dialog>` (focus stays inside, Esc closes, 220 ms slide): `createSheet`, `openSheet`, `confirmSheet`, `sheetRow`, `sheetGroup`. |
| `switcher.js`, `period.js`, `account.js` | The car switcher (also Ctrl K), the period sheet and `resolveFor()`, the account sheet. |
| `toast.js` | `toast(text, { kind: 'ok' \| 'info' \| 'error', next })`, above the dock. |
| `states.js` | `loadingTiles(shape)`, `emptyTile()`, `errorTile()`, `errorWords()`; `ghostTile(w, h)` (one grey tile that keeps a block's room until its own data is in: `slot.replaceWith(tile)`) and `plotGhost()` (a chart's room inside a tile). |
| `form.js` | `field()`, `form()`, `toggle()` (an on / off row), `segmented()`. |
| `ui.js` | Icons, `btn`, `linkBtn`, `chip`, `stateTag`, `num` (number and unit on one baseline), `ring` (SOC ring), `tile`, `note`, `ago`. |
| `map.js` | `mapTile(ctx, box, opts)`: MapLibre in a rounded box, tiles from `/maps/config` (offline first), routes in the series colours, round pins, a place circle. With `lazy: true` a map below the fold is made (and MapLibre loaded) when it comes near: a page waits for it before `ctx.ready()` only when `nearNow(box)` (`shared/lazy.js`) says it is near. |
| `chart.js` | `timeChart(ctx, body, opts)`: a uPlot chart with one or two lines, a legend of keys that show the value under the cursor and hide a line when pressed; charts with one `sync` key share cursor and zoom. `bars()` for plain histograms. A chart below the fold is built when it comes within a screen of view (`shared/lazy.js`); its `.plot` box has its height from CSS, so nothing moves. |
| `source.js` | Where data came from: `srcPill(row)` (the row's `source_label` as a soft pill, one dot per input: SMO, Tesla API, TeslaFi, imported), `estPill` (Tesla-only energy is estimated), `viaMark` (a live value the Tesla API sent), `inputsLine` (the car's live inputs from `/now`), the source filter words. |
| `firmware.js` | The car's firmware fingerprints and the "Tesla update installed" marker: module names, `hexCode` (lowercase hex, monospace), `changesList` (old hex -> new hex per module), `aroundDrives` / `betweenLine` (the drive before and after), `namedLine`, `nameBox` (the version name and who gave it; admins open a box with Edit to type or clear it, Escape or Cancel closes it). Used by Software and Timeline. |
| `morph.js` | Live figures painted in place: `morphChildren(slot, nodes)` / `morph(el, next)` make what is on the page look like a part built again from new data, writing only the texts and attributes that changed; `keepEl(el, name)` keeps a part with its own state (a progress panel) as it is. |
| `bits.js` | Parts the data pages share: `filterBar` (car and period pills), day groups and `dayHead`, `statTile`, `facts`, `socBar`, `strip` (the activity strip), `pills`, `moreButton` (its row's `load()` is what a click does, never two at once), `listBox(ctx, { label, cls, fit, more }, ...children)`: a long log-like list in a box about five rows high that scrolls by itself (shared/scrollbox.js, `.scroll-box` in app.css; `--box-row` / `--box-rows` per list, about three on a phone; `fit` for rows in hand, a box at most that high; `more`, a moreButton row under the box, loads as the end of the box comes near). Used by the audit log, Space on disk, the fingerprints, the gaps log and the connection problems. Place words and links. |

## A page module

```js
import { h } from '../shared/dom.js';
import { tile, num, btn } from '../core/ui.js';
import { emptyTile } from '../core/states.js';

export const id = 'drives';
export const css = true;                                  // optional: /pages/drives.css while shown
export const shape = [['full', 'h1'], [2, 'h2'], [2, 'h2']]; // optional: grey tiles while it loads
export const title = (ctx) => 'Drives';                    // optional: the tab title

export async function mount(ctx) {
  const pg = await ctx.api.page(`cars/${encodeURIComponent(ctx.car.id)}/drives`, ctx.params, { limit: 50 });
  if (!ctx.alive()) return;
  ctx.head({ actions: [btn('Export', { icon: 'download', kind: 'soft' })] });
  ctx.view.replaceChildren(
    pg.items.length ? tile({ w: 1, title: 'Distance', icon: 'route' }, num(12.3, 'distance', { size: 'l' }))
      : emptyTile({ icon: 'route', title: 'No drives this week', text: 'Pick a longer period above.' }));
  ctx.ready();                                            // after tiles and charts have drawn
  return { unmount() {}, update(ctx, why) {} };           // both optional
}
```

`ctx`: `id`, `info` (its `pages.js` entry), `state`, `query`, `car`, `carId`, `tz` (the car's zone), `period`
and `params` (`{ from_ms, to_ms }`), `api` (scoped: cancelled when the page is left), `me`, `admin`, `view` (the
grid), `alive()`, `ready()`, `fail(err, { retry })`, `head({ title, actions })`, `title(...)`, `setQuery(patch)`,
`link(page, params)`, `navigate(href)`, `reload()`, `events(types, fn)`, `on(target, type, fn)`, `every(ms, fn)`,
`onCleanup(fn)`, `toast`. A page exporting `onQuery(query, ctx)` takes back and forward on its own filters;
`update(ctx, { period | prefs })` takes a new period or new units without a remount.

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

## Layout: the bento grid

`#main` is the grid. Tiles go straight into it: `tile({ w })` with `w` 1, 2, 3, 4 or `'full'` columns. The grid has
2 to 8 columns of 17.5 to 22.5 rem (4 at 1280, 6 at 1920, 8 at most), so 4K shows more tiles, not bigger ones,
and it is centred with equal margins. On a phone (under 600 px) it has 2 columns: big tiles use `w: 2`, small
number tiles `w: 1`. `w: 3` and `w: 4` shrink to what the grid has. Tiles in a row share their height.

The page scrolls in `#scroller`, which ends above the dock's zone, so the dock never covers a tile.

## Pixel care

- Every size is a token rounded to two device pixels (`--s1`..`--s12` spacing, `--h-btn` 44, `--h-sm` 36,
  `--h-input`, `--ic` 24, `--ic-sm` 20, `--av`, `--r-tile` 24, line heights `--l13`..`--l72`). Use them, not
  raw rem: even sizes keep centred things on whole pixels at every Interface size and zoom.
- Text: `--f13` (logs), `--f14`, `--f16`, `--f20`, `--f28`, `--f44`, `--f72`, never under 10 px. Give every
  text its matching line height token. A unit next to a big number uses `num()` (45 % size, one baseline).
- Borders only on boxes whose place is whole pixels (tiles, sheets, the dock, inputs). Pills and chips that
  follow text widths have no border: a fill instead.
- Centre with flex or grid (`place-items: center`), never by nudging. Icons are drawn on a 24 px grid.
- The dock is centred with `left: 50%` and a shift rounded to the pixel (`frame.js` `placeDock()`).

## Colours and type

Tokens on `:root` (light) and for dark (`[data-theme="dark"]`, or the device when the theme is Auto): `--bg`,
`--card`, `--card-alt`, `--well`, `--text`, `--muted`, `--coral` (text) and `--coral-fill` (fills), `--teal`,
`--sky`, `--sun`, `--bad`, each with a `-soft` tint; `--edge` (the 1 px edge of dark cards), `--shadow`.
States: Driving coral, Charging teal, Parked sky, Asleep muted, Offline bad (`stateTag()`). Chart series:
`--c1` coral, `--c2` sky (also dashed), `--c3` teal. Never write a colour in a page; read the token.

## Coverage, readiness, rules

- Mark what does an inventory item with `cover: '<item id>'`; frame items are marked by the frame.
- `ctx.ready()` once tiles and charts have drawn; `ctx.fail(err)` when the page cannot load.
- Empty states say why and what to do ("No drives this week"); loading shows grey tiles of the final size.
- Start every call at once and let each tile paint when its own answer is in (a `ghostTile` in its place until
  then); `ctx.ready()` once they all have. Never hold the whole page back for its slowest call.
- Pages stay still (`ui/tools/check.js --stable`): live `now` events are merged into what is shown (a `{ value, at_ms }` pair sent as only `{ at_ms }` keeps its value), a live reload keeps the old content until the new answers are in, and a page goes in with its tiles already filled where they would push the rest down.
- Motion (tile press, the dock pill, ring fill, sheets) is off with `prefers-reduced-motion`.
- No inline styles, `innerHTML`, `fetch` or outside requests; units and dates only through `shared/`.
