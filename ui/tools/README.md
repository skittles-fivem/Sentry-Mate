# UI tools

| Tool | What it does |
|---|---|
| `sync_shared.py` | Copies the shared browser modules (`ui/shared/*.js`) into every pack's `web/shared/`. |
| `check.js` | The page checker: opens a pack's pages in Chromium on the dev API server (or a real server) and reports what is wrong. |
| `pixel.js`, `stable.js` | The in-page parts of `check.js --pixel` and `check.js --stable`. |
| `map-tiles.test.js` | `node --test ui/tools/map-tiles.test.js`: in all five packs, a page loaded with tiles offline shows OpenStreetMap tiles once an admin turns them on and the person moves to a map inside the app (the page loads itself again once, for the server's new policy), the same after a switch to OpenTopoMap, and every tile request carries only the page origin as Referer; and Settings: Display and units at 360 px with a long zone name (America/Argentina/Buenos_Aires) keeps its zone button and tile terms inside the window. The tile servers are answered inside the browser; nothing leaves the machine. Skipped without Playwright. |
| `settings-server.test.js` | `node --test ui/tools/settings-server.test.js`: Settings: Server and access of both built-in packs in Chromium at phone size on the dev server: the CAN Bridge upload address breaks only after a slash or a colon, the certificate's names start with `localhost`, and with `--own-certificate` no "Make a new certificate" is offered. Skipped without Playwright. |

## The page checker

```bash
node ui/tools/check.js --pack instrument                       # every page, at 1440x900 and 3840x2160
node ui/tools/check.js --pack instrument --pages fleet,drives --sizes 1280x800
node ui/tools/check.js --pack instrument --allow-missing --json > report.json
node ui/tools/check.js --pack instrument --pixel --sizes 1440x900,3840x2160,390x844@3   # pixel centred and correct?
node ui/tools/check.js --pack instrument --stable --sizes 1440x900,390x844             # still while live updates come in?
node ui/tools/check.js --self-test                             # checks the checker
```

It starts the dev API server (`ui/devserver/serve.py --pack <pack> --port 0`) and opens each page in a new
Chromium window of each size, at device scale 1 unless the size names another (`390x844@3`). When it is done it stops the server and the browser,
also on Ctrl-C and on errors. It needs Node and the Playwright that is already installed; nothing else.

| Option | Default | What it does |
|---|---|---|
| `--pack NAME\|DIR` | (required) | A pack in `ui/packs` (`instrument`, `logbook`, `horizon`, `console`, `pebble`) or any folder with `web/`. |
| `--pages LIST\|all` | `all` | Page ids, comma separated. `all` is every page of the matrix plus `sign-in` and `setup`. |
| `--sizes WxH[@S],...` | `1440x900,3840x2160` | Window sizes. `@S` is the device scale (1 to 4): `390x844@3` is a phone, `1440x900@2` a laptop screen at scale 2. |
| `--out DIR` | `UI_CHECK_OUT`, else a new private folder in the system temp folder (`sentry-mate-ui-check-XXXXXX`, made for the run) | Screenshots go to `DIR/<pack>/`. |
| `--server-args "..."` | none | More dev server flags, e.g. `"--cars 200 --latency-ms 150 --role watch"`. |
| `--json` | off | Print one JSON object instead of the table. Progress lines still go to stderr. |
| `--allow-missing` | off | Missing items only warn; the page can still pass. |
| `--full` | off | Also a full-page screenshot at the first size. |
| `--pixel` | off | Also the pixel checks (below): fails a page that is not pixel centred and correct, and saves a screenshot with the problems outlined. |
| `--stable [SECONDS]` | off (`30` when given without a number) | Also the stability checks (below): stays on each ready page that long while the dev server sends live updates, and fails a page whose layout shifts, whose blocks move, or whose elements and loading texts come and go. |
| `--stable-fast` | off | With `--stable`: the dev server sends live values every 0.5 s and list changes every 3 s instead of every 2 s and 10 s. |
| `--fixture-matrix FILE` | `docs/parity/coverage-matrix.json` | Another coverage matrix (the fixtures use `fixtures/matrix.json`). |
| `--ready-timeout-ms N` | `20000` | How long to wait for the page to say it is ready. |
| `--idle-timeout-ms N` | `10000` | How long to wait after that for the network to go quiet. |
| `--jobs N` | `2` | Pages checked at the same time. |
| `--car`, `--drive`, `--charge ID` | from the API | Open these instead. |
| `--base URL` | none | Check against a running real server (`https://127.0.0.1:PORT`) instead of the dev server; see below. |
| `--user NAME` | none | With `--base`: the account to sign in with. |
| `--password PW`, `--password-file FILE` | `UI_CHECK_PASSWORD` | With `--base`: that account's password. |

Exit code: `0` when every checked page passes, `1` when one fails, `2` for a usage error or when the dev
server or the browser does not start.

### Against the real server

With `--base` the checker starts no dev server. It signs in to a real `sentry-mate serve` and checks the pages
the server itself serves (the built-in packs embedded in the binary: rebuild it after changing a page).

```bash
node ui/tools/check.js --pack instrument --base https://127.0.0.1:PORT --user check-instrument --password-file pw.txt
```

- Use it only on a throwaway server with made-up data (`sentry-mate dev seed`, a build with the `dev-seed`
  feature). The server's self-signed certificate is accepted for that origin and that run only: the Node requests
  of the run trust it, and the browser windows ignore certificate errors while every other host stays blocked.
- It signs in with `POST /api/v1/session` (an account without two-step sign-in), puts the session cookie in each
  browser window, and makes the pack being checked that account's own UI pack (`PATCH /api/v1/me/preferences`
  `ui_pack`). Two checkers can run at once with an account each (one per pack).
- `sign-in` is opened without the session. It can only be checked for the pack the server shows signed-out
  browsers (the pack chosen for everyone, else `instrument`); for another pack it fails with that reason.
  `setup` is not in `all` and cannot be named: the server is set up already (check it on the dev server).
- `--server-args` is for the dev server and is refused with `--base`.

A script that builds the binary, seeds a fresh data folder, starts `serve`, makes the accounts and stops it all
again does this:

1. `cargo build -p sentry-mate --features dev-seed`, and a copy of the binary.
2. `sentry-mate init`, then `sentry-mate dev seed --days 3 --cars 2`, with `SENTRY_MATE_DATA`, `SENTRY_MATE_KEYS`
   (two folders), `SENTRY_MATE_REQUIRE_MOUNTS=0` and free `SENTRY_MATE_WEB_PORT` / `SENTRY_MATE_UPLOAD_PORT`.
3. `sentry-mate serve --bind 127.0.0.1`, waiting for `GET /health`.
4. `POST /api/v1/setup` with the code in `keys/SETUP-CODE.txt`, then `POST /api/v1/accounts` for an admin per
   pack and a watch account (header `X-Sentry-Mate: 1` on every change).
5. Waits until each car's `/health` has `logs_waiting` 0 and the car has drives and charges, prints the base URL,
   the accounts and the password, and stops the server (and removes the folder) on Ctrl-C.

### Which pages it opens

Page ids and routes are the ones in `docs/parity/coverage-matrix.json` ("pages"), for example `drives` is
`/cars/{car}/drives`. `{car}` is the first car of `GET /api/v1/cars`; `{drive}` and `{charge}` are that car's
newest drive and charge (the first item of `GET /api/v1/cars/{car}/drives` and `.../charges`). With
`--server-args "--cars 0"` there is no car, and car pages fail with "no car to open".

- **health** has two routes, `/cars/{car}/health` and `/system`, and both are opened. Its own items count
  when they are on either route; the frame items must be on each.
- **sign-in** and **setup** are screens of the app frame, not pages of the matrix. Each gets its own dev
  server, started with `--signed-out` or `--needs-setup`. They need no items, and a `401` or `404` from
  `/api` is expected there.

### The checks

Each page and size is checked in a fresh browser window, then gets a screenshot.

| Column | Fails when |
|---|---|
| **ready** | The page did not set `document.documentElement.dataset.ready` to its own page id within the timeout (`no`), set it to `error:<id>` (`error`), or the page could not be opened (`no load`). A value of another page id that stays for 2 s ends the wait early. After the page is ready the checker waits until no request has been open for 0.5 s; event streams do not count. |
| **missing** | An item of this page in the matrix, or an item of the frame, has no element with `data-cover="<id>"`. Hidden elements count (an item in a closed dialog is marked on the element that opens it). With `--allow-missing` this only warns. |
| **foreign** | A `data-cover` id belongs to another page in the matrix (or to `not-applicable`). |
| **unknown** | A `data-cover` id is not in the matrix at all (a typo, or an item that was removed). |
| **console** | A console error. `Failed to load resource` lines for `/api` and for other hosts are left out; they are in **api** and **outside**. |
| **page err** | An uncaught exception or rejected promise in the page. |
| **csp** | A Content Security Policy violation: an inline style attribute, a script or font from another host, a blocked fetch. Taken from the page's `securitypolicyviolation` events, from workers, and from CSP console messages. |
| **api** | A request to `/api/` on the dev server answered `400` or more, or failed on the network. The error code from the answer is shown (`501 not_built`). Requests cancelled on purpose (`ERR_ABORTED`) are not counted. |
| **outside** | A request to a host other than the dev server, for example a popup or a link opened by script. It is blocked and listed. Map tiles from `tile.openstreetmap.org` are answered with a blank 256 x 256 PNG and are not counted. Most outside requests are already stopped by the CSP and show up there instead. |
| **overflow** | The page is wider than the window (`scrollWidth > clientWidth + 1`), or an element is cut by the left or right edge of the window. An element inside a box that hides or scrolls its overflow only counts if that box is cut. |
| **small text** | Visible text is smaller than 10 CSS px: the computed font size, scaled by an SVG `viewBox` or a shrinking transform. The 5 smallest are listed with a selector. Hidden, transparent and off-page text is left out. |

| **pixel** | Only with `--pixel`: a pixel check found a problem (see "Pixel checks"). |
| **shift**, **moved**, **flicker** | Only with `--stable`: the layout shift score after ready (fails above 0.01), the blocks that moved, and the elements that came and went more than once (fails on any); see "Stability checks". |

Under the table, lines explain each problem; a line shared by every size is printed once as `all sizes:`.
`note:` lines are warnings that do not fail the page: the network still busy at the end, or a script, style
sheet, font or image that came back as the pack's `index.html` (a missing file or a relative path that the
dev server's client-side routing answered).

Status per page: **PASS**, **WARN** (passes, with missing items under `--allow-missing` or with notes), or
**FAIL**. The last line counts the pages that pass.

### JSON

```text
{ pack, packDir, matrix, server: { origin, args }, opened: { car, drive, charge }, sizes, out, allowMissing,
  pass,
  pages: [{ id, url, route, pass,
    sizes: [{ size, pass, fails: [kind],
      ready: { ok, value, ms, landed, reason: null | "error" | "other_page" | "timeout" | "not_loaded" },
      loadError, missing: [id], foreign: [{ id, page }], unknown: [id],
      console: [{ text, url, line }], pageErrors: [{ message, stack }],
      csp: [{ directive, blocked, source, line, sample, worker?, message? }],
      apiFailures: [{ method, url, status, code, error? }], outside: [{ url, method, type }],
      overflow: { document, scrollWidth, clientWidth, count, elements: [{ selector, left, right, width }] },
      smallText: { count, smallest: [{ px, selector, text }] },
      pixel: null | { count, dpr, elements, texts, ms, shot,
        findings: [{ n, kind, selector, what, by, box: { x, y, w, h } }] },
      stable: null | { error } | { watchedMs, firstPaintMs, readyMs, shift, shiftBeforeReady, shiftWithInput,
        shiftSources: [{ selector, times, px }], moved: [{ selector, px, times, atMs: [ms], cause, box }], movedCount,
        toggling: [{ selector, times, first, atMs: [ms], text? }], togglingCount, late: [{ selector, h, atMs, pushed }],
        events: { now, rows, ... }, eventCount, apiRequests, cost, before, after, outlined },
      shot, fullShot, tiles, covered, notes: [text], ms, dropped? }] }] }
```

`fails` names the checks that failed (`load`, `ready`, `missing`, `foreign`, `unknown`, `console`,
`pageErrors`, `csp`, `apiFailures`, `outside`, `overflow`, `smallText`, `pixel` with `--pixel`, and `stable` with
`--stable`). The report also has `pixel: true|false` and `stable: null | { seconds, pace, maxShift }`. `pixel` per
size is `null` without `--pixel`, `stable` per size `null` without `--stable` (see "Stability checks" for its shape). `covered` is the number of distinct
`data-cover` ids found. Lists keep 100 entries; `dropped` counts the rest.

### What it cannot see

- Text drawn on a canvas (chart axes, map labels) is not measured.
- A CSP violation in the very first line of a worker can be missed; the worker is hooked once it exists.
- The overflow check follows parent elements; an absolutely positioned element that escapes a clipping
  parent may still be reported as cut.
- Chromium never finishes a request whose body the page does not read, so such a request stops counting
  as open 1.5 s after its answer arrives.

### Pixel checks (`--pixel`)

With `--pixel` the checker measures each page in the browser (`pixel.js`), after the other checks, and lists
every problem with the element (a selector), what is wrong and by how much in CSS px (`by`). A page with any
problem fails. Measuring takes a few milliseconds on a normal page and about 2 s on a page of 40,000 elements.
Then it outlines each listed problem on the page, with its number and kind, takes a full-page screenshot
(`<page>-<size>-pixel.png`, in `pixel.shot`) and removes the outlines again. The readable report shows the first
15 problems per size; `--json` has up to 100 (`count` has them all).

"Off" means more than 0.5 CSS px. Layout works in 1/64 px, so 0.5 itself passes.

| Kind | Reported when |
|---|---|
| `centre` | Text or an icon is off the centre of the box that centres it. Boxes that centre: a box with `text-align: center` that holds only inline content (buttons, badges, chips, table cells, empty states), checked line by line across; a flex box with `justify-content: center` (the items together, along the row) or `align-items` / `align-self: center` (each item, across the row); a grid of one cell with one child centred by `justify-*` / `align-*` / `place-*`. The box is the inside of the borders when you can see it (a background, a border all round or a shadow), else the content box. Up and down is only checked for a box you can see with one line in it, at most about twice the text's height. |
| `row` | Items in one row (flex row items, grid items, table cells, inline blocks and form fields side by side) whose tops, centres, bottoms and baselines all miss the item before by more than 0.5 px and at least one by no more than 3 px. More than 3 px is taken as meant. Table cells compare what is in them. |
| `column` | Blocks stacked one under the other in one parent whose left edges differ by more than 0.5 and up to 3 px, when their centres and right edges do not line up either (centred or right-aligned blocks are meant). |
| `fraction` | A border, or a 1 px line (a box at most 1.5 px thick with a background), has an edge or a width off a whole device pixel by more than 0.05 device px: `getBoundingClientRect() * devicePixelRatio`. Checked at the scale of the size; add a size at `@2` to check scale 2. Turned (rotated) boxes are left out. |
| `clip` | Text cut off by a box that hides its overflow (`hidden` or `clip`), partly: text cut off completely is taken as hidden on purpose. Cut with `text-overflow: ellipsis` or `-webkit-line-clamp` is meant and not reported; nor is text in a 1 px box or under `clip` / `clip-path` (screen reader text), nor text in a box that scrolls. |
| `overflow` | Text sticks out of its own box (its nearest block, inline-block, flex item or cell) when that box lets it show. |
| `overlap` | Text on other text, or on an icon (`svg`, `img` or `canvas` up to 64 x 64 px), by more than 1 x 1 px. Text in a fixed or sticky bar (a dock, a top bar) over the page is not counted: the page scrolls under it. |
| `hidden` | Text under another element: `elementFromPoint` at three points of each line finds something on top that you can see (a background at least half opaque, an image, an icon, a form field). Only in the window. |
| `scroll` | The page scrolls sideways. |

How text is measured:

- Text is the box of its words from `Range.getClientRects` (the text box of the font: ascent to descent), with
  the spaces left out, so a space at a line end never counts. Inline elements with a background, border or
  side padding count with those.
- Up and down, text is centred when its text box is centred in the box. CSS puts the text box in the middle of
  its line, but Chromium rounds the space above it down to a whole pixel: with a line height whose difference
  from the text box is odd, text sits 0.5 px high (passes); with a line height like `1.2` on an 11 px font it can
  sit about 0.6 to 1 px high, and is reported. Whole-pixel line heights avoid it.
- A text box taller than its line height counts as the line height high for overlaps; for cutting and
  sticking out up and down its extra height is slack (the glyphs still fit the line).
- Baselines (for `row`) are the bottom of the first word's text box less the font's descent (from canvas
  `measureText`, rounded to device pixels like the layout).
- An icon is the box of the element (`svg`, `img`, an inline block without text).

What it cannot see: a `::before` or `::after` in the flow (a box with one is not checked for centring), text drawn
on a canvas or in an SVG, the inside of an icon's drawing, anything under an element with `pointer-events: none`
or text with it (for `hidden`), a bar that is `fixed` or `sticky` on top of text (the page scrolls under it; not
reported), and where a designer moved something on purpose by a pixel or two to look right.

### Stability checks (`--stable`)

The owner's rule: a page you are on must not jump around, and nothing may show up, go away and come back while
live values and list changes arrive. With `--stable [SECONDS]` (default 30) the checker stays on each page for that
long after it is ready (`data-ready`) and measures in the page (`stable.js`, installed before any page script):

| Measure | How | Fails when |
|---|---|---|
| **shift** | A `PerformanceObserver` for `layout-shift` entries (what Chromium's CLS is made of): the sum of the values without recent input, from the frame after the one that painted the ready page. The nodes each entry moved are listed (`shiftSources`, with the px when the node was in the window before and after). | The sum is above 0.01. |
| **moved** | A cross-check that also sees below the window: every 250 ms the layout top (`offsetTop` up the `offsetParent` chain, so a turning spinner or a transform animation does not count) of every block-level element of at least 40 x 8 px. A block whose top moves by more than 1 px without input is listed once with the largest move, how often and when, and what came before it in the page in that step (`cause`: a block that appeared, grew, shrank or went away). Only the outermost of blocks that moved together is listed; fixed and sticky ones are left out. | A block moved. |
| **flicker** | A `MutationObserver` marks the page changed; at the next animation frame (the frame that gets painted) the checker takes the set of visible elements (by their place in the page: tag, id or first class, place among their kind) and of placeholder texts (`—`, `…`, `Loading…`, `Updating…`, `No …`, `None`, `unknown` and the like). Each element or placeholder that appears or goes away counts one change. An element that appeared once (a new row) or went away once is fine. Only the outermost of elements that changed together is listed. | Anything changed more than once: it went away and came back (data, then a loading text or an empty state, then data again), or it appeared and went away again (a spinner or `Loading…` coming back), hidden and shown again (`hidden`, `display`, `visibility`, `opacity: 0`). |
| **late** | Blocks that appeared after ready and pushed others down (the `cause` of a move). | (Shown with the moves they caused.) |
| before ready | The shift between the first paint and ready, and both times. | Never: shown on the summary line. |
| updates | The events the page's `EventSource` got during the watch, by type, and its `/api` requests. | Never; a `note:` when nothing arrived at all. |

Left out on purpose: whatever is inside `svg`, `canvas` and a MapLibre map (`.maplibregl-map`), which redraw by design,
and an element marked `data-stable-ignore`. Text that only changes its value (a speed, "3 min ago") is not a flicker.
Moved blocks and flicker are watched in the window and the three window heights below it (what you see and scroll
to soon), so a page of thousands of rows costs no more than its first screens. The watch must not slow the page
down: a look that took `c` ms waits `3c` ms (flicker) or `4c` ms (moves, at least 250 ms) before the next one, and a
`note:` says so when a big page was looked at less often (`cost` in `--json` has the times).

**The pace of updates.** With `--stable` the dev server is started with `--rows-every-s 10`: besides the live values it
always sends every 2 s (`now` events), every car gets a `rows` event every 10 s as if a drive or charge had started
or ended, so the pages load their lists again during the watch. `--stable-fast` makes it `--sse-interval-ms 500
--rows-every-s 3`. `--server-args` come after these, so they can change them; `--freeze-clock` is refused (nothing
would change). Against a real server (`--base`) the pace is the server's own.

**Screenshots.** Per page and size: `<page>-<size>-stable-before.png` (at ready), `-stable-after.png` (at the end of
the watch) and, when something moved or flickered, `-stable-outlined.png`: the whole page with the moved blocks (red,
`moved`) and the toggling elements that are on the page at the end (purple, `flicker`) outlined and numbered.

The readable report adds the columns **shift**, **moved** and **flicker**, a `stable:` line per problem (up to 8 of
each kind per size; all of them, up to 100, in `--json`) and a summary line per size: how long it watched, the events
and requests that arrived, the first paint, ready, and the shift before ready.

```text
FAIL  drive  /cars/c1/drives/9007200254742142
  size       ready  ...  shift  moved  flicker
  1280x800   yes    ...  0.000  0      5
  all sizes: stable: flicker main#view > section.card > div.box > p.loading appeared and went away again (27 changes, at 0.1, 0.4, 0.6 s)
  all sizes: stable: flicker main#view > section.card > p.badge went away and came back (27 changes, at 0.1, 0.4, 0.6 s)
  1280x800: watched 7 s: 29 now, 6 rows, 0 /api requests; first paint 104 ms, ready 233 ms, shift before ready 0.000
```

What it cannot see: a flash shorter than one frame (never painted, so never seen), a change inside a canvas or svg,
a block that moves and comes back between two 250 ms samples (the layout shift entries still see it when it was in
the window), and movement inside a box that scrolls (its scroll position is not counted).

## Fixtures and the self-test

`fixtures/pack-ok`, `fixtures/pack-bad` and `fixtures/pack-pixel` are tiny packs on the dev server, with their
own small matrix (`fixtures/matrix.json`). The ok pack passes every check, with a map tile, an open event stream and an
expected `401` on the sign-in page. The bad pack breaks each check on purpose (its `app.js` lists which page
breaks which). Check one by hand:

```bash
node ui/tools/check.js --pack ui/tools/fixtures/pack-bad --fixture-matrix ui/tools/fixtures/matrix.json \
  --pages fleet,drives,drive,charge,health,sign-in,setup --ready-timeout-ms 3000
```

`fixtures/pack-stable` is for `--stable`: on `fleet` live values change in place, a list is drawn again in one go
on `rows` events, a spinner turns and a map pin and a chart redraw, and nothing may be reported; on `drives` a block
comes in above the list 1 s after ready and grows with every event (layout shift, moved, late); on `drive` every
event swaps the list for `Loading…` for 300 ms and hides a badge (flicker, nothing moved).

```bash
node ui/tools/check.js --pack ui/tools/fixtures/pack-stable --fixture-matrix ui/tools/fixtures/matrix.json \
  --pages fleet,drives,drive --sizes 1280x800,390x844@3 --stable 6 --stable-fast
```

`fixtures/pack-pixel` has the same small cases twice: right on `fleet`, off on purpose (mostly by 1 px) on
`drives`; its `app.js` and `app.css` say which case breaks what. Every size in it is a whole pixel.

```bash
node ui/tools/check.js --pack ui/tools/fixtures/pack-pixel --fixture-matrix ui/tools/fixtures/matrix.json \
  --pages fleet,drives --sizes 1280x800,390x844@3 --pixel
```

`node ui/tools/check.js --self-test` checks the ok and bad packs at two sizes and asserts that the ok pack passes
and the bad one reports every kind of failure, then that `--allow-missing` turns a missing item into a
warning, then checks the pixel pack with `--pixel` at 1280x800 and at 390x844@3 and asserts that the right
cases report nothing and each case off by 1 px is found, with how much, then checks the stable pack with `--stable 6
--stable-fast` and asserts that live updates arrived, the still page passes, and the jumping and the flickering
pages fail with what moved (how far, why, the late block) and what came and went (how often). It prints one line per assertion and exits `0` when all hold.
