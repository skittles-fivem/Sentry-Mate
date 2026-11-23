# Console pack: how it is built

The Console pack is a keyboard-first operator console: flat, sharp, monospace and dense. One full-width column
per page, a command bar with the route as a path, a fuzzy command list, `key: value` blocks and text tables.
Read this with the page plan ([`docs/parity/ui-parity.md`](../../../../../docs/parity/ui-parity.md)), the API
([`docs/ui-api.md`](../../../../../docs/ui-api.md)) and the coverage matrix
([`docs/parity/coverage-matrix.json`](../../../../../docs/parity/coverage-matrix.json)).

## Run and check it

```bash
python3 ui/devserver/serve.py --pack ui/packs/console --port 8000        # then open http://127.0.0.1:8000/
node ui/tools/check.js --pack console --pages drives --sizes 1440x900,3840x2160,390x844@3 --pixel
python3 ui/tools/sync_shared.py --check                                  # the shared modules are up to date
```

`--fresh` (or `--needs-setup`) on the dev server shows the set-up screen, `--signed-out` the sign-in screen.
Paths in `index.html` are absolute (`/app.css`, `/core/app.js`): pages live at deep addresses. Paths inside the JS
and CSS are relative to their own file (`url('fonts/x.woff2')`, `new URL('../vendor/...', import.meta.url)`), so they
follow the server's versioned prefix (`/_v/<version>/...`) that `index.html` is rewritten to.

## Files

```
index.html     the frame's elements only: #top (#cmd, #links), #notices, #view, #foot, #toasts
boot.js        runs before the first paint: Interface size (this device's own size from sentrymate.device-size when
               set, else the account's), Follow screen size and theme from this browser's copy
app.css        sizes, colours, base, frame, components, states, dialogs, bare screens, phone, motion
fonts/         JetBrains Mono and IBM Plex Sans (Latin), with OFL.txt
vendor/        MapLibre GL JS and uPlot (licences next to them), loaded by the pages that draw maps and charts
shared/        copies of ui/shared/*.js (edit those, then python3 ui/tools/sync_shared.py)
core/          the frame and the helpers (this folder)
pages/<id>.js  one module per page id, plus sign-in and setup; pages/<id>.css when a page needs its own rules
```

## Core modules

| Module | What it does |
|---|---|
| `app.js` | The entry: builds the frame, starts the keys and the router; a lost session goes to sign-in. |
| `pages.js` | Every page: `name`, `link` (the word in the link row), `scope` (`car`, `global`, `settings`), `car`, default `period`, `parent`, `go` (the `g <key>` shortcut), `desc`, `words`. |
| `router.js` | The address to a mounted page, with `ctx` (below). Set-up first, then sign-in, then the page. |
| `session.js` | Set-up needed? Signed in? `gate(page)`, `safeNext(next)`, `signedIn()`, `signOut()`. |
| `prefs.js` | The account and its preferences: `getMe`, `isAdmin`, `getPrefs`, `savePrefs(patch)`, `onPrefs`. |
| `display.js` | Interface size, Follow screen size, theme: `applyInterface`, `applyTheme`, `rootPx`, `unit`, `rpx`, `onChange`, `onTheme`. |
| `frame.js` | The command bar, the link row, notices, footer, live status; `navCar()` is the car the link row is for. |
| `palette.js` | The command list (Ctrl/Cmd+K or `/`): pages, cars, recent drives and charges, settings keys (`SETTINGS_KEYS`). |
| `keys.js` | Global keys (`KEYS`), `j`/`k` rows, `g <key>` chords, `?` key list. |
| `carswitch.js`, `period.js`, `account.js` | The car switcher, the period picker and the account menu, all dialogs. |
| `carpick.js` | A page's own car keys and selects: `carKeys(items, key, { keep })`, `carOptions(items, option)`, `pairOptions(pairs, cars, selected)`. Sold cars sit behind a "sold cars (n)" key or an optgroup; a kept (chosen) sold car still shows. |
| `notices.js` | Server notices and the offline line under the command bar. |
| `toast.js` | `toast(text, { kind: 'ok' \| 'info' \| 'error', next })`: bottom-right notes, no motion. |
| `dialog.js` | `openDialog({ title, body, actions })`, `confirmDialog(text, { danger, ok })` on native `<dialog>`. |
| `states.js` | `loadingLine(what)` (a blinking caret, never a spinner; a pass under way shows `.spin`, one character cell turning, the same size everywhere), `empty(text, hint)`, `errorBlock(err, { retry })`, `errorWords(err)`. |
| `form.js` | `field({ label, name, type, ... })`, `form({ fields, submit, onSubmit })`, `showApiError`: `label: [input]` lines. |
| `ui.js` | `head(ctx)` (the header line), `section`, `kv`, `btn`, `actions`, `note`, `link`, `textBar`, `paint`, `centreUpDown`. |
| `fmt.js` | `num(value, kind)` (number with its unit muted), `numText`, `time`, `date`, `dateTime`, `when`, `dur`, `ago`, `range`, `slug`, `stateWord`, `tag`. |
| `bare.js` | The sign-in and set-up box: centred both ways on whole pixels; `canSendSecrets()` and the plain-HTTP line. |
| `stage.js` | `offstage(slot, build)`: a part of a page drawn again on a live update is built in an invisible box of the same width (charts and all) and swapped in at once. |
| `table.js` | `table(ctx, opts)`: dense text tables from an API list (server sort and paging, csv) or local rows, a totals row, `data-row` rows, the two-line phone rows (`ph: 'main' \| 'sub' \| 'num'`); `autoMore`; `box` (and `boxRows`) for a long log-like list that scrolls in a box about five rows high (shared/scrollbox.js, `.scroll-box` in app.css: the head line stays at the top, a fade at the bottom while rows are hidden below; an API list has its height from the start and loads its next page as the end of the box comes near, "more" stays under it for the keyboard and the csv still exports every row; local rows are all drawn in a box at most that high, `.scroll-box--fit`); `cellBar(frac, cells)`. |
| `chart.js` | uPlot: `loadUPlot`, `plot(el, ctx, make, data, { onCursor })`, `line(label, n)` (series n: c1 solid, c2 / c3 dashed), `timeAxis(tz)`, `valueAxis(fmt)`, `cursor(syncKey)`, `readout()`, `chartOrNote`. |
| `map.js` | MapLibre in a flat grey style: `createMap` / `safeMap(el, ctx, opts)` with `line`, `points`, `circle`, `marker`, `fit`, `frame`. Below the fold, `plot` builds a chart and `createMap(el, ctx, { lazy: true })` makes a map (and loads MapLibre) only when its box comes within a screen of view (`shared/lazy.js`); the box has its height from CSS, so nothing moves. A page waits for a map before `ctx.ready()` only when `nearNow(el)` says it is near. |
| `strip.js` | `stateStrip(ctx, items, from, to, tz)`: the state strip, one character per cell. |
| `filterline.js` | `filterLine({ keys, value, onApply })`: 'filter> from:2026-10 min:10km place:home'; `parseFilter`, `dateSpan`, `numberOf`. |
| `kvedit.js` | Settings as `key = value` lines: `editLines(specs)`, `editGroup({ save })` (lines saved together), `saveError`. A value is text until focused; Enter saves, Esc undoes, a choice is a row of keys. |
| `secret.js` | Things shown once: `copyBtn`, `codeOut`, `downloadText`, `downloadKey`, `askPassword(ctx, ...)` (the password again before a dangerous action), `showOnce`. |

## A page module

```js
import { h } from '../shared/dom.js';
import { head, paint, section, kv } from '../core/ui.js';
import { num, stateWord } from '../core/fmt.js';
import { empty } from '../core/states.js';

export const id = 'battery';
export const css = true;                     // optional: link /pages/battery.css while shown
export const title = (ctx) => 'Battery';     // optional: the tab title (default: the page's name)

export async function mount(ctx) {
  const data = await ctx.api.get(`cars/${encodeURIComponent(ctx.car.id)}/battery`, ctx.params);
  if (!ctx.alive()) return;
  paint(ctx, head(ctx), section({ title: 'now', cover: 'bh.filter.car' }, kv([['capacity', num(data.capacity_kwh, 'energy')]])));
  ctx.ready();                               // after every table and chart of the first screen has drawn
}
```

Every page starts with `head(ctx)`: the title on the left, the period and the car on the right (both open their
pickers). Then sections (`## drives`), `key: value` blocks, tables and charts, one column, at most `--maxw` wide.

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

### `ctx`

| Field | What it is |
|---|---|
| `id`, `info` | The page id and its `pages.js` entry. |
| `view` | The `<main>` to draw into; `paint(ctx, ...)` replaces what is there (the router shows a loading line first). |
| `state`, `query` | The address (`shared/state.js`). `query` holds the page's filters as text. |
| `car`, `carId`, `tz` | The car (`GET /cars/{car}`) on car pages, else null; the time zone for days (the car's). |
| `period`, `params` | The resolved period `{ preset, from_ms, to_ms, label }` and `{ from_ms, to_ms }` for the API. |
| `api` | A scoped API client: everything it started is cancelled when the page is left. |
| `me`, `prefs`, `admin` | The account, the preferences, whether it is an admin. |
| `alive()` | False once the page was left: check it after every `await`. |
| `ready()`, `fail(err, { retry })` | The data-ready signal (`<html data-ready="<id>">`), or the error block (`error:<id>`). |
| `onCleanup`, `on`, `every`, `events(types, fn)` | Clean-up, listeners, timers and live events that end with the page. |
| `setTitle`, `link(page, params)`, `setQuery(patch)`, `navigate`, `reload` | Tab title, links that carry car and period, filters in the address. |
| `refresh()` | Live data changed (a `rows` event): the page is built again off screen and put in place in one step once it is ready, drawn and its own requests are done; the old page stays until then. Asked before the first `ready()`, it waits for it. |

Live data: `ctx.events('now', fn)` and `ctx.events('rows', fn)`; never poll on a timer for data.
On a live update nothing may go away and come back: update values in place, keep what is on screen until the
new answer is in (a loading line only the first time), or use `ctx.refresh()` / `offstage()`. `ready()` is marked
on the next frame once the fonts are in; check with `node ui/tools/check.js --pack console --stable`.

## Rows and keys

A row the keys can move to has `data-row` (and `data-href`, or a first link inside). `j`/`k` select it (class
`is-sel`), Enter opens it. Tables, timeline lines and list rows should all have it, so `j`/`k` work on every page.
Keys are off while an input has focus. On a phone rows are tapped instead.

## Sizes: whole pixels at every size

The root size is `round(clamp(13px, 0.55vw + 8px, 20px) x Interface size)` (16 px x Interface size with Follow
screen size off; 15 px on phones). Everything else is a registered custom property in whole px, worked out once
on `:root` (see `app.css` part 2):

| Token | What |
|---|---|
| `--g`, `--g2` | the 8 px grid unit (half the root) and half of it |
| `--fs1..5`, `--lh1..5` | text sizes 13 / 14 / 16 / 20 / 28 at a 16 px root, and their line heights |
| `--a1..5` | the width of one monospace character at each size (a whole px) |
| `--ctl`, `--bar`, `--links`, `--gut`, `--maxw` | control height, command bar, link row, side margin, content width |

Rules that keep it pixel-exact:

- Lengths are `calc(var(--g) * n)` or another token, never rem, em or fractions. 1 px rules only.
- Text is JetBrains Mono at a size where one character is a whole px wide, so text columns line up. Bordered
  boxes around text (`.btn`, `.key`, `.toast`) round their width up with `calc-size()`.
- Tables share no free width out: one column (`grow`, else the first `wrap-cell`) takes it, so column edges stay
  on whole pixels. A field in a row has a width in characters, not `auto`.
- Line heights are the font's text box plus an even number of px: text sits exactly in the middle of a line, and
  a chip or button whose line height equals its height is centred up and down.
- Centring a box uses `round(down, ..., 1px)` (`.wrap`, `.page`, dialogs, `.bare-box`); a box of unknown height
  uses `centreUpDown(host, box)`.
- Text inside a `position: fixed` bar must not have a background of its own (the pixel check stops at the first
  opaque box above the page).

Check with `--pixel` at `1440x900,3840x2160,390x844@3`; for browser zoom, `720x450` is a 1440 laptop at 200 % and
`2880x1800` one at 50 %.

## Colours

Tokens on `:root` (dark, the default) and `:root[data-theme="light"]`: `--bg`, `--panel`, `--panel-2`, `--rule`,
`--rule-2`, `--text`, `--muted`, `--accent`, `--warn`, `--bad`, `--good`, chart series `--c1` (solid), `--c2`
(dashed), `--c3` (dashed), map `--map-land`, `--map-water`, `--map-road`, and `--sel` for the selected row. Never
write a colour in a page; read the token (`cssVar('--c1')` in JS for charts).

State words are text in brackets: `stateWord('driving')` gives `[driving]` in the accent, `[charging]` warn,
`[parked]` text, `[asleep]` muted, `[offline]` bad. Tags: `tag('ok', 'ok')`, `tag('todo', 'warn')`.

## Phone (up to 860 px wide)

The page scrolls inside `#scroller` (notices, view, footer) between the link row at the top and the command bar at
the bottom; neither bar ever covers the page. The command bar is a search field at the bottom above the safe area (with the car and the account), the link row
scrolls sideways, `key: value` blocks are one column, tap targets are 44 px high. Tables must collapse to two-line
rows (main value left, secondary muted under it, number right); charts are full width and 180 px tall. Nothing
may make the page wider than the window.

## Do not

- No inline styles, `innerHTML` or string event handlers: build with `h()`; dynamic sizes go through CSS custom
  properties (`vars`), which use CSSOM.
- No `fetch` outside `shared/api.js`, no outside requests except map tiles, no unit or date maths by hand.
- No blur, gradients, glow, shadows or serif; corners 2 px at most; motion only the command list fade, the caret and the `.spin` cell.
