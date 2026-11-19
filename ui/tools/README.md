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
node ui/tools/check.js --self-test                             # checks the checker
```

It starts the dev API server (`ui/devserver/serve.py --pack <pack> --port 0`) and opens each page in a new
Chromium window of each size, at device scale 1. When it is done it stops the server and the browser,
also on Ctrl-C and on errors. It needs Node and the Playwright that is already installed; nothing else.

| Option | Default | What it does |
|---|---|---|
| `--pack NAME\|DIR` | (required) | A pack in `ui/packs` (`instrument`) or any folder with `web/`. |
| `--pages LIST\|all` | `all` | Page ids, comma separated. `all` is every page of the matrix plus `sign-in` and `setup`. |
| `--sizes WxH,...` | `1440x900,3840x2160` | Window sizes. |
| `--out DIR` | `UI_CHECK_OUT`, else a new private folder in the system temp folder (`sentry-mate-ui-check-XXXXXX`, made for the run) | Screenshots go to `DIR/<pack>/`. |
| `--server-args "..."` | none | More dev server flags, e.g. `"--cars 200 --latency-ms 150 --role watch"`. |
| `--json` | off | Print one JSON object instead of the table. Progress lines still go to stderr. |
| `--allow-missing` | off | Missing items only warn; the page can still pass. |
| `--full` | off | Also a full-page screenshot at the first size. |
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
      shot, fullShot, tiles, covered, notes: [text], ms, dropped? }] }] }
```

`fails` names the checks that failed (`load`, `ready`, `missing`, `foreign`, `unknown`, `console`,
`pageErrors`, `csp`, `apiFailures`, `outside`, `overflow`, `smallText`). `covered` is the number of distinct
`data-cover` ids found. Lists keep 100 entries; `dropped` counts the rest.

### What it cannot see

- Text drawn on a canvas (chart axes, map labels) is not measured.
- A CSP violation in the very first line of a worker can be missed; the worker is hooked once it exists.
- The overflow check follows parent elements; an absolutely positioned element that escapes a clipping
  parent may still be reported as cut.
- Chromium never finishes a request whose body the page does not read, so such a request stops counting
  as open 1.5 s after its answer arrives.

## Fixtures and the self-test

`fixtures/pack-ok` and `fixtures/pack-bad` are tiny packs on the dev server, with their own small matrix
(`fixtures/matrix.json`). The ok pack passes every check, with a map tile, an open event stream and an
expected `401` on the sign-in page. The bad pack breaks each check on purpose (its `app.js` lists which page
breaks which). Check one by hand:

```bash
node ui/tools/check.js --pack ui/tools/fixtures/pack-bad --fixture-matrix ui/tools/fixtures/matrix.json \
  --pages fleet,drives,drive,charge,health,sign-in,setup --ready-timeout-ms 3000
```

`node ui/tools/check.js --self-test` checks both packs at two sizes and asserts that the ok pack passes
and the bad one reports every kind of failure, then that `--allow-missing` turns a missing item into a
warning. It prints one line per assertion and exits `0` when all hold.
