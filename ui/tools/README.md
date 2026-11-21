# UI tools

| Tool | What it does |
|---|---|
| `sync_shared.py` | Copies the shared browser modules (`ui/shared/*.js`) into every pack's `web/shared/`. |
| `check.js` | The page checker: opens a pack's pages in Chromium on the dev API server (or a real server) and reports what is wrong. |

## The page checker

```bash
node ui/tools/check.js --pack instrument                       # every page, at 1440x900 and 3840x2160
node ui/tools/check.js --pack instrument --pages fleet,drives --sizes 1280x800
node ui/tools/check.js --pack instrument --allow-missing --json > report.json
node ui/tools/check.js --pack instrument --pixel --sizes 1440x900,3840x2160,390x844@3   # pixel centred and correct?
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
      shot, fullShot, tiles, covered, notes: [text], ms, dropped? }] }] }
```

`fails` names the checks that failed (`load`, `ready`, `missing`, `foreign`, `unknown`, `console`,
`pageErrors`, `csp`, `apiFailures`, `outside`, `overflow`, `smallText`, and `pixel` with `--pixel`). The report
also has `pixel: true|false`. `pixel` per size is `null` without `--pixel`. `covered` is the number of distinct
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
| `overlap` | Text on other text, or on an icon (`svg`, `img` or `canvas` up to 64 x 64 px), by more than 1 x 1 px. |
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

## Fixtures and the self-test

`fixtures/pack-ok`, `fixtures/pack-bad` and `fixtures/pack-pixel` are tiny packs on the dev server, with their
own small matrix (`fixtures/matrix.json`). The ok pack passes every check, with a map tile, an open event stream and an
expected `401` on the sign-in page. The bad pack breaks each check on purpose (its `app.js` lists which page
breaks which). Check one by hand:

```bash
node ui/tools/check.js --pack ui/tools/fixtures/pack-bad --fixture-matrix ui/tools/fixtures/matrix.json \
  --pages fleet,drives,drive,charge,health,sign-in,setup --ready-timeout-ms 3000
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
cases report nothing and each case off by 1 px is found, with how much. It prints one line per assertion and exits `0` when all hold.
