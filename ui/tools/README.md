# UI tools

| Tool | What it does |
|---|---|
| `sync_shared.py` | Copies the shared browser modules (`ui/shared/*.js`) into every pack's `web/shared/` (the built-in packs of `ui/packs`: Instrument; not the archived ones of `ui/archived`). |
| `builtin-packs.js` | The built-in packs (every folder of `ui/packs` with `ui.json` and `web/index.html`, as the server embeds them: Instrument alone). The browser tests that run on every pack loop over this list, so an archived pack drops out of them. |
| `check.js` | The page checker: opens a pack's pages in Chromium on the dev API server (or a real server) and reports what is wrong. |
| `perf/` | Delivery measurement on the real server (release build, seeded made-up cars): each pack's car, drives, drive, charges, battery, statistics, settings and settings-integrations pages, cold and warm, on an emulated phone with 4x CPU slowdown and a Fast 3G-like network (or an unthrottled desktop, `--desktop`), and in-app navigation between them (`--nav`); bytes, requests, protocol, first paint, time to usable, long tasks, duplicate API calls. `perf/README.txt` says how to seed and run; `perf/compare.js` puts runs side by side. Results: [docs/plans/performance.md](../../docs/plans/performance.md). |
| `bundle/` | The release build's bundler: `npm ci --prefix ui/tools/bundle && node ui/tools/bundle/bundle.mjs OUT` writes each built-in pack's scripts bundled and minified (esbuild, pinned by `bundle/package-lock.json`; each page its own entry at `pages/<id>.js`, the code pages share in `core/c-<hash>.js` chunks) and its style sheets and `boot.js` minified. `crates/web/build.rs` embeds them with `SENTRY_MATE_UI_BUNDLE=OUT` and the server serves them in place of the sources' files of the same path; a pack's download and an uploaded pack laid over a built-in one keep the sources. `.github/build-binary.sh` (the Image workflow) runs it; tests, a plain `cargo build` and the dev server serve the sources as they are. |
| `pixel.js`, `stable.js`, `hover.js` | The in-page parts of `check.js --pixel`, `check.js --stable` and `check.js --hover`. |
| `map-tiles.test.js` | `node --test ui/tools/map-tiles.test.js`: in every built-in pack, a page loaded with tiles offline shows OpenStreetMap tiles once an admin turns them on and the person moves to a map inside the app (the page loads itself again once, for the server's new policy), the same after a switch to OpenTopoMap, and every tile request carries only the page origin as Referer; and Settings: Maps and addresses and Settings: Cars at 360 px with a long zone name (America/Argentina/Buenos_Aires) keep the tile terms and the zone button for new cars inside the window. The tile servers are answered inside the browser; nothing leaves the machine. Skipped without Playwright. |
| `server-update.test.js` | `node --test ui/tools/server-update.test.js`: a tab open across a server update in every built-in pack, in Chromium on the dev server: after `POST /__dev/new-build` and the tab coming back into view, a link to Data health loads that address in full and the page loads; a page module that fails once is loaded again with a full load; one that keeps failing says "Sentry Mate was updated or a file could not load" (`page_failed`, never "No answer from the server") with a Reload button, and the tab title and the trail name Data health, not the page before. `PACKS=a,b` runs only those. Skipped without Playwright. |
| `battery-figures.test.js` | `node --test ui/tools/battery-figures.test.js`: the Battery page of every built-in pack in Chromium on the dev server, given a `/battery` answer the made-up cars do not have: a capacity that reads above the value when new (no loss, the note `reading_above_new`) and logged charges of no known kind (`lifetime.unknown_kwh`). Every pack says why no loss is shown and shows those charges as their own part of the energy into the pack, not as AC. Skipped without Playwright. |
| `scroll.js`, `scroll.test.js` | The scroll check: scrolls each pack's key pages (Data health, car, drives, charges, statistics, Settings: Server, logbook) with the mouse wheel on a 4K screen while a rebuild runs on the dev server (`--rebuild-running`), and fails a page with a frame over 50 ms; frame times, long tasks and a DevTools trace of each scroll. `node --test ui/tools/scroll.test.js` runs it at 1920x1080 at scale 2 (`SCROLL_PACKS`, `SCROLL_SIZES` to change). Skipped without Playwright. [Below](#the-scroll-check-scrolljs). |
| `car-map-scroll.test.js` | `node --test ui/tools/car-map-scroll.test.js`: each built-in pack's car page in Chromium on the dev server with a car that is driving (`--sse-interval-ms 500`): the car's marker follows the car while the page rests, is not moved (the map is not drawn again) while the page is being scrolled or the map is out of view, and catches up once the scroll has rested with the map in view. Skipped without Playwright. |
| `logbook-scroll-snap.test.js` | `node --test ui/tools/logbook-scroll-snap.test.js`: each built-in pack's logbook in Chromium on the dev server while a rebuild runs (`--rebuild-running --rows-every-s 1`): while its long list is scrolled without a rest the page is not set on whole pixels again frame after frame (the list's table changes height as its window moves; that waits for the scroll to rest), and the panels above the list are made again only when their figures changed. Skipped without Playwright. |
| `print-virtual-list.test.js` | `node --test ui/tools/print-virtual-list.test.js`: the logbook's long list of every built-in pack in Chromium on the dev server (`--drives 600`): on the screen only a window of rows is in the page, also after a scroll; on `beforeprint` and in print media every row is drawn (the print style sheet still gives the box its full height, no sideways overflow, no menu rail), and after `afterprint` or the screen media the list is windowed again at the place it was scrolled to. Skipped without Playwright. |
| `jumps.js`, `jumps.test.js` | The jump check: every page of every pack loaded and scrolled through on an iPhone 15 and an iPhone 13 mini in WebKit, a 390x844 phone in Chromium's mobile emulation and a 1440x900 desktop, with scroll anchoring off in WebKit as in Safari on iOS; fails a page that moves what is on screen (jump score or layout shift over 0.02), resizes a block above the screen while it is scrolled, or scrolls by itself and moves what is on screen. `node --test ui/tools/jumps.test.js` runs all of it, `check.js --pack NAME --jumps` one pack. Skipped without Playwright; the WebKit devices need `npx playwright install webkit`. [Below](#the-jump-check-jumpsjs). |
| `check-jumps-flag.test.js` | `node --test ui/tools/check-jumps-flag.test.js`: `check.js --pack <pack> --jumps` reaches the jump check's own options (check.js and jumps.js need each other; check.js exports before it runs), and a page that is none is a usage error, not a crash. Needs no browser. |
| `health-totals.test.js` | `node --test ui/tools/health-totals.test.js`: each built-in pack's Data health in Chromium on the dev server with `--health-list-max 3` (a car with more chain gaps, dropped frames and unknown spans than its health lists; the real server lists the newest 500): the page's counts are the answer's `*_total`, not the length of the lists. Skipped without Playwright. |
| `health-gap-reasons.test.js` | `node --test ui/tools/health-gap-reasons.test.js`: each built-in pack's Data health and Timeline in Chromium on the dev server say each reason a stretch of time is unknown in its own words: a stored log that could not be decoded (`unreadable`) and a log that names another car's VIN (`other_vin`) are not "the log failed its check", and no reason is shown as its code. Skipped without Playwright. |
| `live-rows.test.js` | `node --test ui/tools/live-rows.test.js`: each built-in pack's Charges page in Chromium on the dev server (`--rows-every-s 2`) loads its list again on a `rows` event and draws a charge that was not there before, without loading the page again; the same while an event comes every second, as during a rebuild (the load comes a moment after the first event and is not put off by the next ones). The event stream is held until the page is ready, so the one list load before ready is counted exactly on a busy machine too. Skipped without Playwright. |
| `overview-links.test.js` | `node --test ui/tools/overview-links.test.js`: in every built-in pack on the dev server, the connections overview's rows for the online map tiles, the address lookup and the release check link to `/settings/display#maps`, `#addresses` and `#release` (what the real server sends), and each opens the section where it lives now, drawn and scrolled to: `#map-tiles` and `#address-lookup` of Settings: Maps and addresses, `#release-check` of Settings: Server and access. `PACKS=a,b` runs only those. Skipped without Playwright. |
| `settings-server.test.js` | `node --test ui/tools/settings-server.test.js`: Settings: Server and access of every built-in pack in Chromium at phone size on the dev server: the CAN Bridge upload address breaks only after a slash or a colon, the certificate's names start with `localhost`, and with `--own-certificate` no "Make a new certificate" is offered; and the Storage panel of every built-in pack at desktop size: the categories grouped by kind, largest first, the CAN logs group opened to its categories by short name, with what compaction saved, "Compact now" with its progress and what it freed, a failed pass with the server's error, no panel for a watch account, and no more reads of the detail once the page is left. Skipped without Playwright. |
| `rebuild_panel.test.js` | `node --test ui/tools/rebuild_panel.test.js`: Data health in Chromium on the dev server. A full rebuild paused after an earlier one was cancelled (`--full-rebuild paused`) is one state in the Rebuild row, the Rebuild all data panel (State, no step, no speed, no time left) and its buttons; Start now and Pause move the row and the panel together. The GPS layout fit (`--gps-fit running`) says how far it is by the work (272 of 22,402 logs is 1 %) with a time left. Past fits (`--gps-fit history`): newest first with the best layout, its errors in metres, good or weak and whether applied; a row opens to the full result; Apply asks the password again; Remove asks first; the layouts used over time; nothing wider than a phone; a watch account sees none. Skipped without Playwright. |
| `account-password.test.js` | `node --test ui/tools/account-password.test.js`: the account forms of Settings: Server and access in every built-in pack in Chromium on the dev server: Add an account and Edit have a "Your password" field and send it as `confirm_password` (the server asks for the admin's own password again before every account change), a wrong one is said under that field and nothing changes, and Remove asks for it in its dialog and sends it with the DELETE. And "Change your password" beside the sessions: a wrong current password is said under its field, the right one sends `PUT /me/password`, and the new password signs in. |
| `fleet-attention.test.js` | `node --test ui/tools/fleet-attention.test.js`: "Need a look" on the Fleet page of every built-in pack in Chromium on the dev server: a silent car's row says how old its newest data is (`since_ms`), an upload that came since with older logs is said apart (never "No upload for 4 min" for data 38 hours old), and a car that never uploaded reads "for a long time". |
| `import-report.test.js` | `node --test ui/tools/import-report.test.js`: the Import page's report of a TeslaFi file in every built-in pack in Chromium on the dev server: the same file a second time shows its car in "Rows per car" with 0 to import and every poll skipped, as the car's own `tables.positions` says (about 40 s: the dev server's check and import take their seconds). |
| `keyboard-focus.test.js` | `node --test ui/tools/keyboard-focus.test.js`: keyboard focus in every built-in pack built on Instrument in Chromium on the dev server: a dialog opened by a button that is disabled while its request runs gives the focus back to that button when it closes, the car overview's live updates leave the focused control in place, the account menu takes the focus when it opens, keeps Tab inside and gives it back when it closes, the car switcher's key (Ctrl+K) opens no second dialog over the page finder or over the switcher, and opening the place editor, saving a place and saving a drive's purpose in the Logbook leave the focus on the page (the purpose's button in the list drawn again), never on `<body>`; one Esc closes the page finder with text typed in it; a save in a dialog that the server refuses (Settings: Server and access, Add account) leaves the focus on the dialog's button, or on the field the answer names; and each step of the first-run set-up (`--fresh`) takes the focus: a wrong code's field, the next step's heading or its first field. `PACKS=a,b` runs only those. Skipped without Playwright. |
| `page-faults.test.js` | `node --test ui/tools/page-faults.test.js`: every built-in pack built on Instrument when something is wrong or missing, in Chromium on the dev server: a latitude or longitude typed out of range in the place editor throws nothing; a car name of one long word stays inside a phone's window; an address that is no page or no car has one `h1` and a tab title that say Not found; the empty note of a table wider than its box stays in the part in view and breaks into lines inside its own width; with no car (`--cars 0`), or before any car was opened, a menu item of a car's page opens the car switcher (which says there is no car yet, or opens that page for the car chosen) and Charges and Statistics ask for no car; Places with 200 cars asks for the drives per car of only the cars that have them, and says which car's could not be read, with Try again; and a side read answered `500` (the overview's charges, a drive's route, a charge's curve and its place's price, the logbook's rules, the cars that need a look, the drives' place filters, the trip's readings and saved trips, the place suggestions) is said as not loaded, never shown as "there is none", and so are the trip's other cars ("Only one car"), a purpose rule's place whose name could not be read (no bare id), the Battery page's consumption factor and its logged totals, and the name of a car that needs a look, while Charges and Statistics for every car show the page's error with Try again when the cars cannot be read, never "no car yet"; in Horizon and Manager the old short address `/charging` opens Charges and each section's short address opens one of its pages; and (Console too) a `#fragment` that is not valid percent-encoding throws nothing; in Settings: Cars a number its field cannot read (`1e`, `-.`) is said under the field and nothing is sent, so the stored value stays; saving a place with only a new name asks nothing about past charges, while a new price still does, and a price per kWh below 0 or above 10 (the server's limits) is said under its field and not sent; with a car and a place renamed to one long word (64 letters) no page is wider than the window, on a phone and on a computer (the name breaks, or is cut with "…" in a button, a chip or a choice); and Charges and Statistics opened for cars that are not there (`?cars=zzz`) say "None of these cars is yours to see" with a way to every car, and ask the server for none of them. `PACKS=a,b` runs only those. Skipped without Playwright. |
| `interface-size.test.js` | `node --test ui/tools/interface-size.test.js`: every built-in pack built on Instrument at the Interface sizes a device can pick, in Chromium on the dev server: on a phone at 110 and 125 the export file's three steps and its card's lines and buttons stay inside the card and off one another; on a phone at 80 the Travel map's legend ends inside the map (it scrolls there) and its switch can still be reached; on a desktop at 80 no unit and no code text of Logbook, Software and Data health is under 10 px; on a phone at 125 Charges does not scroll sideways (its "Follows the period" button takes a second line) and the Logbook's purpose counts and the Drives' pattern figures stay in their boxes; on a phone at 80 the first-start panel's labels are not cut, the charge level labels stay in their column and no unit of the car's overview is under 10 px; on a phone at 100, 80 and 125 the charge page's "Download CSV" stands under "Power against battery %", not on it; and on a phone at 80 and 90 a tag is as high as its text, the charging week's hour and weekday labels are whole and Travel's ranks are inside their column; and on a 1280 and a 1366 px laptop at 125 Efficiency's temperature figures stay in their columns and Software's table titles are whole. `PACKS=a,b` runs only those. Skipped without Playwright. On a phone 320 to 430 px wide at every Interface size (80 to 125) nothing on the car overview's map lies over the car's marker: not the "Outside map links off" chip, the full screen button, the place card, the Map / Satellite switch, the zoom buttons or the credit, and the marker is whole inside the map. |
| `move-log.test.js` | `node --test ui/tools/move-log.test.js`: the move's log in Chromium on the dev server, on the new server's "Moving here" page (`--fresh`) and on the old server's move panel (`--move-state copying`): lines come in as the move goes, newest last and in view, each with a time; a reload shows them again; "Download log" saves them as plain text (`YYYY-MM-DD HH:MM:SS UTC`, two spaces, the words); no line holds a code, key or path; nothing is wider than a 390 px phone. `MOVE_LOG_SHOTS=DIR` also saves screenshots at 1440x900 and 390x844. Skipped without Playwright. |
| `sign-in.test.js` | `node --test ui/tools/sign-in.test.js`: signing in, in Chromium on the dev server (`--signed-out`): the sign-in screen of every built-in pack built on Instrument on a computer shows its card alone, no section links, car switcher or account menu; and in every built-in pack a signed-out visit to `/help#licences` comes back to that address after signing in, section and all (scrolled there where Help has that section), and the same when the session ends while the page is open. `PACKS=a,b` runs only those. Skipped without Playwright. |
| `contrast.test.js` | `node --test ui/tools/contrast.test.js`: Instrument in Chromium on the dev server, in the light and the dark theme: the words of a tag (Preferred, driving now, estimated, Ongoing), of the AC and DC pills of the Charges list, of an unnamed version in the Software page's strip, of a Logbook purpose's total without drives, of a warning in amber (drives without a purpose, stale cars, an estimate's mark, a middling efficiency), of a problem in red and of a link or a good word in teal reach 4.5:1 against the background drawn behind them (measured on a screenshot of the element: its most common colour against the text's colour with every opacity above it). So do the labels on a fill or on the map's glass (a route's A and B pins, the primary button's label, also while pointed at), and, in a sweep, every text of every page and of the sign-in and setup screens: the page is drawn once more with its glyphs not painted, and each text's colour must reach 4.5:1 on nine tenths of the pixels behind its glyph boxes, gradients, tiles and glass included. Not swept: text of a control that is turned off, text that is covered or clipped away, an ornament hidden from screen readers (the trail's "/"), text drawn in a canvas, and menus and dialogs that are not open. `PACKS=a,b` runs only those. Skipped without Playwright. |
| `page-edges.test.js` | `node --test ui/tools/page-edges.test.js`: Instrument at the edges of what a page can be asked, in Chromium on the dev server: a link to a section of the page that is open (Help's "Questions and answers below", the footer's licences and data credits) lands below the sticky top bar; Settings: Cars refuses a sold date after today and a default price in a currency that is none (`DOL`) under their fields and sends neither; Trip and Efficiency for a period still to come are drawn empty with no request the server refuses, a period before 1970 in the address gives the usual period, and the period picker takes no day before 1970; a search of the Logbook or of Data health's signals for one word of 300 letters leaves the page as wide as the window, on a phone and a computer; Battery of a car whose pack has reported nothing (`--cars 200`, car c4) has no "(— readings)" and no "— driving, — while parked"; the car switcher of 200 cars counts the cars not sold (the header's number, from `/fleet/summary`) and its "Sold cars (n)" says where the sold ones are when they are not among the cars listed; Statistics on a server without a car has no "across 0 cars"; a Map or Satellite pick whose save the server refuses shows the server's reason and stays for the page; a page opened for the first time while the dev server is stopped says "No answer from the server" inside the app (not the browser's own error page), and Try again opens it once the server runs again. Skipped without Playwright. |

## The page checker

```bash
node ui/tools/check.js --pack instrument                       # every page, at 1440x900 and 3840x2160
node ui/tools/check.js --pack instrument --pages fleet,drives --sizes 1280x800
node ui/tools/check.js --pack instrument --allow-missing --json > report.json
node ui/tools/check.js --pack instrument --pixel --sizes 1440x900,3840x2160,390x844@3   # pixel centred and correct?
node ui/tools/check.js --pack instrument --stable --sizes 1440x900,390x844             # still while live updates come in?
node ui/tools/check.js --pack instrument --hover                                       # still while the mouse moves?
node ui/tools/check.js --self-test                             # checks the checker
```

It starts the dev API server (`ui/devserver/serve.py --pack <pack> --port 0`) and opens each page in a new
Chromium window of each size, at device scale 1 unless the size names another (`390x844@3`). When it is done it stops the server and the browser,
also on Ctrl-C and on errors. It needs Node and the Playwright that is already installed; nothing else.

| Option | Default | What it does |
|---|---|---|
| `--pack NAME\|DIR` | (required) | A pack in `ui/packs` (`instrument`; the archived packs of `ui/archived` are not checked) or any folder with `web/`. |
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
| `--hover` | off | Also the hover checks (below): moves the mouse over a grid of points, every interactive element and every chart, focuses every element that takes focus, and fails a page where anything outside what is hovered moves or changes size by more than 0.5 px. Starts the dev server with `--freeze-clock` (unless `--stable` is given too) and the browser with its scrollbars shown. |
| `--device-size N` | `auto` | The Interface size on this device (`80`, `90`, `100`, `110` or `125`): put in each window's storage before the page starts, as if picked in Settings: Display and units on that device ([`ui/shared/devicesize.js`](../shared/devicesize.js)). |
| `--fixture-matrix FILE` | `docs/parity/coverage-matrix.json` | Another coverage matrix (the fixtures use `fixtures/matrix.json`). |
| `--ready-timeout-ms N` | `20000` | How long to wait for the page to say it is ready. |
| `--idle-timeout-ms N` | `10000` | How long to wait after that for the network to go quiet. |
| `--jobs N` | `2` | Pages checked at the same time. |
| `--car`, `--drive`, `--charge ID` | from the API | Open these instead. |
| `--place ID` | none | The `places` page opens that place's editor (`/places?place=ID`) instead of the list; the list's items then count as missing (add `--allow-missing`). |
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

Each page and size is checked in a fresh browser window, then gets a screenshot. Before the checks below (after
ready, and after the `--stable` watch) it scrolls the page down about a screen at a time to its end and back (in the
window, else in the page's tallest box that scrolls), and waits for the network to go quiet again: pages draw charts,
maps and lists below the fold only when they come near the screen (`ui/shared/lazy.js`), so this way they are all
there to be measured and photographed.

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
| **hover** | Only with `--hover`: the hovers and focuses that moved something (see "Hover checks"). |
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
        events: { now, rows, ... }, eventCount, apiRequests, cost, before, after, outlined, scrollMoved?, scrollMovedCount? },
      scrolled: null | { steps, moved: [{ selector, px }], movedCount },
      hover: null | { error } | { targets, points, charts, focused, groups, count, noise, ms, pageMs, shot,
        findings: [{ n, kind, target, x, y, count, scrolled, scrollbar, stays, moved: [{ selector, dx, dy, dw, dh, box }] }] },
      shot, fullShot, tiles, covered, notes: [text], ms, dropped? }] }] }
```

`fails` names the checks that failed (`load`, `ready`, `missing`, `foreign`, `unknown`, `console`,
`pageErrors`, `csp`, `apiFailures`, `outside`, `overflow`, `smallText`, `pixel` with `--pixel`, `stable` with
`--stable`, and `hover` with `--hover`). The report also has `pixel: true|false`, `stable: null | { seconds, pace, maxShift }` and `hover: null | { tol, grid }`. `pixel` per
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
| scrolled | After the watch, the layout top of every block of the whole page before and after the page is scrolled through (see "The checks"): a part drawn on the way when it came near the screen that had no room kept for it moves what is under it (`scrollMoved`, `scrollMovedCount`). | A block moved (a `stable: moved ... while the page was scrolled through` line). |
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

### Hover checks (`--hover`)

The owner's rule: moving the mouse over a page must not move the page. Hover and focus may change only colour,
opacity, shadow and transform, never a width, height, margin, padding, border width or font weight; a tooltip or a
chart's crosshair label sits in an overlay (absolute or fixed, out of the flow) and never takes room; a scrollbar
never comes or goes because of a hover (the page's scroller has `scrollbar-gutter: stable`). With `--hover` the
checker, on each ready page (`hover.js`):

1. moves the mouse outside the window and takes the box of every element in the window and half a window above
   and below it (`getBoundingClientRect`; inside `svg`, `canvas` and a MapLibre map only their own box);
2. moves the real mouse (Playwright) to each target: every interactive element whose middle is in the window and
   not under something else (links, buttons, fields, `summary`, `label`, rows and header cells, anything with a
   `title`, a `tabindex`, a role such as `button` or `tab`, or a pointer cursor; at most 4 of one kind, such as a
   row or the cells of a column), 10 points across each chart (`svg`, `canvas`, uPlot, map) at 35 % and 70 % of
   its height and the marks of an svg chart that react to the mouse, and a grid of 10 x 6 points over the window;
   then focuses each target that takes focus (`focus({ focusVisible: true })`), and lets it go again;
3. after each move compares every box with step 1 and lists the elements that moved or changed size by more than
   0.5 CSS px, only the outermost of those that moved together;
4. when something moved, moves the mouse away again: the page must be back as it was in step 1 (else the page
   changed by itself, and step 1 is taken again), and the same hover must move it again. Only then it counts.
   A page that is not back as it was after three tries in a row counts too (`stays`: the hover moved it for good,
   for example by scrolling it a pixel each time);
5. scrolls down by 0.9 of a window and does it again, up to 6 windows down. The scroller is the document, or the
   box that fills most of the window when the document does not scroll (Pebble's `.scroller`).

| Not counted | Why |
|---|---|
| The hovered (or focused) element and what is inside it | It may change its own look; what is around it may not move. |
| An element hidden (opacity 0 or visibility hidden) with the mouse away or on it, or out of the flow and hidden with the mouse away, with `pointer-events: none`, with `role="tooltip"` or marked `data-hover-overlay` (and what is inside it) | An overlay: it takes no room. |
| An element under a transform that the hover changed | Transforms are allowed. |
| An element whose text the hover changed (a readout) | Its own box follows its text; the elements around it still count, so a readout needs a box of its own size (a fixed width, or `flex: 1` with an ellipsis). |

The page scrolling, or the page's scroller (or the window) changing its inner width or height, is reported too ("a
page scrollbar came or went"). While it runs, transitions end at once (so the hover's last state is measured) and
what only paints is left out (blur behind glass, filters, shadows, gradients: at 4K they made each mouse move wait a
quarter of a second); both come back afterwards. Then, when something moved, a full-page screenshot with the moved
boxes outlined and numbered (`<page>-<size>-hover.png`, in `hover.shot`).

Playwright hides the scrollbars of the browser it starts; with `--hover` they are shown (15 px), as in a desktop
browser, so a scrollbar that a hover brings in takes room as it would for a person.

```text
FAIL  drives  /cars/c1/drives
  size      ready  ...  hover
  1280x800  yes    ...  35
  hover: 1. hover on main#view > section.card.bad > div.row:nth-of-type(1) > button.btn.bold:nth-of-type(1) (71, 143) moved main#view > section.card.bad > div.row:nth-of-type(1) > button.btn:nth-of-type(2) (x +1.09 px)
  hover: 6. hover on main#view > section.card.bad > p:nth-of-type(2) > span.low (97, 576) moved body > header.frame (width -15 px), main#view (width -15 px); a page scrollbar came or went
```

`hover` in `--json`: `null` without `--hover`, `{ error }` when it could not run, else `{ targets, points, charts,
focused, groups, count, noise, ms, pageMs, truncated?, shot, findings: [{ n, kind: element | point | chart | focus,
target, x, y, count, scrolled, scrollbar, stays, moved: [{ selector, dx, dy, dw, dh, box }] }] }`. `count` is every hover
that moved something; `findings` keeps one per kind of target and what it moved (up to 100). A page stops being
hovered after 240 s (a `note:` says so).

What it cannot see: a hover that only acts after a delay longer than one frame, what is drawn inside a canvas or
svg, an element out of the window or inside a box that scrolls it out of view, elements more than half a window
away from the window, and a non-interactive element that changes on hover where no grid point falls.

## The jump check (`jumps.js`)

A page must not move under the reader: not while it loads (its parts coming in one by one, the notices bar, the
footer), not while it is scrolled (charts, maps and lists drawn as they come near, live updates of what is above the
screen), and not when it is opened again from kept answers. On a phone this is what makes a page unusable.

```bash
node ui/tools/jumps.js                                   # every pack, page and device
node ui/tools/jumps.js --packs instrument --pages car,drives --devices iphone-15
node ui/tools/check.js --pack instrument --jumps            # the same for one pack
node --test ui/tools/jumps.test.js                       # as a test (JUMPS_PACKS, JUMPS_PAGES, JUMPS_DEVICES)
```

It starts the dev server per pack (live values every 2 s, a `rows` event every 10 s, every answer 60 ms late) and opens
every page of the matrix in a fresh window of each device: `iphone-15` and `iphone-13-mini` (WebKit, Playwright's
device descriptors: Safari's engine and a touch phone's viewport), `phone-chromium` (390x844 at scale 3, mobile
emulation) and `desktop` (1440x900). In WebKit scroll anchoring is off (`overflow-anchor: none`): Safari on iOS has
none, so a block above the screen that changes its height moves what is on screen (Chrome keeps its own, as on Android
and desktops). In each window it:

1. loads the page and waits until it says it is ready, then 1.5 s more (live updates arrive);
2. scrolls down a screen at a time (0.7 of it, 300 ms apart) to the end, and back up;
3. opens Help inside the app and comes back (the page is drawn from kept answers, then from the fresh ones), and
   scrolls down again.

From before the page's first script to the end it watches each animation frame: every block of the page (by its path
from `<body>`, so a block drawn again in its place is the same block), where it is on screen and its height, and where
the box that scrolls (the window, or a pack's own scrolling box) is scrolled and how much of that the check did itself.
Blocks that show nothing (no text, background, border, picture or control of their own), blocks as tall as the screen
that cover it before and after, and what moves within 500 ms of the check's own taps do not count, as in the layout
shift score. A page fails when:

| Fails | When |
|---|---|
| **score** | The jump score is over 0.02: for each frame, the part of the screen whose blocks moved on screen (less what the check scrolled), times how far they moved as a share of the screen's height, summed over the visit (how the layout shift score is made). In Chromium the browser's own layout shift score (every `layout-shift` entry without recent input) counts too. |
| **above** | A block wholly above the screen changed its height while the page was scrolled. |
| **scrolled** | The box scrolled by itself, by more than 1 px, and what is on screen moved with it (a list cut back to its first page, a part emptied and drawn again, a `scrollIntoView`). A scroll that keeps what is on screen in place, such as the page's own scroll anchoring, is not a jump. |

The report names, for each move, the block that moved, by how much, in which step, and what came in, grew, shrank or
went away above it in the same frame. Exit code 0 only when every visit passes.

How a page stays still:

- **The first draw in one go.** The router builds a page drawn for the first time in an invisible box while the view
  shows the loading skeleton, and puts it in place when the page says it is ready (at most 3 s), as it already does for
  a live refresh: its parts come in one by one off screen, not under the reader.
- **The notices bar first.** The first page of a visit waits for the notices (at most 1.5 s) before it shows its
  skeleton, so the bar never pushes a page down.
- **The footer below the fold.** The view is at least a screen high (`100svh`), so the footer never starts on screen.
- **Room kept.** A chart, a map or a list drawn when it comes near has its final height from the start (CSS), and a
  value that changes keeps its width and height (tabular figures, a reserved line).
- **Lists keep their length.** A live refresh asks for as many rows as are shown; a window list swaps its rows in
  place.
- **Scroll anchoring of the page's own.** Safari on iOS has none, and can lay a page out in the middle of a swap and
  scroll it up to fit: `shared/dom.js` `holdScroll()` (called by `replace()` and the routers' and tables' swaps) notes
  the block in the middle of the screen before a part on the page is swapped, and scrolls by as much as it moved, before
  anything is painted.
- **Icons as shapes, not `<use>`.** Safari lays the page out while a part with `<use>` icons is taken out of it.
- **The whole-pixel snap on phones** leaves column widths, and the widths of boxes whose edges do not show, as the CSS
  gives them: a later snap that rounded them by a fraction of a pixel re-wrapped a line of text after the page was
  painted.
- **Small viewport units.** Heights in `svh` (or `lvh`), never `dvh`: `dvh` changes while Safari's toolbars come and
  go as the page scrolls.

## The scroll check (`scroll.js`)

```bash
node ui/tools/scroll.js                                  # every pack, the key pages, at 3840x2160@1 and 1920x1080@2
node ui/tools/scroll.js --packs instrument --pages health --sizes 3840x2160@1
node ui/tools/scroll.js --packs instrument --add-style '.map{display:none}'   # which rule makes painting slow?
node ui/tools/scroll.js --json > scroll.json
node --test ui/tools/scroll.test.js                      # the same as a test, at 1920x1080@2
```

Made for a report of a page that scrolled with only its scroll bar moving, then jumped: the browser had no time to
draw while it scrolled. For each pack it starts the dev server with `--rebuild-running --sse-interval-ms 500
--rows-every-s 1` (a rebuild that never ends: Data health says "Drives and charges are being rebuilt" and reads
itself again, every featured car gets a `rows` event every second, live values come every 500 ms), opens each page,
waits for it to be ready, then turns the wheel down the page and back up (`--steps` 30 each way, `--step-px` 100,
`--step-ms` 50 apart). It records in the page the time between animation frames and the long tasks
(PerformanceObserver `longtask`), and a DevTools trace of the scroll: the longest main-thread task and what took its
time, and the time spent painting and rastering.

| Option | Default | What it does |
|---|---|---|
| `--packs LIST\|all` | `all` | The packs. |
| `--pages LIST` | `health,car,drives,charges,statistics,settings-server,logbook` | Page ids, as in `check.js`. |
| `--sizes WxH@S,...` | `3840x2160@1,1920x1080@2` | Window sizes in CSS px at device scale S: both defaults are a 4K screen (at 100 % and at 200 %). |
| `--max-frame-ms N` | `50` | A page fails with a frame longer than this (half a ms of slack: three 60 Hz frames read 50.1 ms). |
| `--quiet-server` | off | No rebuild and normal live values (the dev server's defaults). |
| `--server-args "..."` | none | More dev server flags. |
| `--add-style CSS` | none | Adds CSS to each page before the scroll (the page's Content Security Policy is set aside for it). |
| `--trace DIR` | none | Saves each scroll's trace (open it in the DevTools Performance panel). |
| `--json` | off | One JSON object with every page's numbers. |

The browser draws in software here (no GPU), so painting costs more than on most desktops: a page that keeps its
frames under 50 ms here has room to spare on a graphics card. Results: [docs/plans/performance.md](../../docs/plans/performance.md#scrolling).

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

`fixtures/pack-hover` is for `--hover`: on `fleet` hover and focus change only colour, shadow, transform and
opacity, a chart's tooltip is an overlay and its readout has a fixed width, and nothing may be reported; on `drives`
a button turns bold, a row's padding grows, an edit icon takes room, a chart puts its tooltip in the flow, a box gets
a scrollbar, a tooltip makes the page longer than the window (a page scrollbar) and a field's border grows on focus,
and each must be reported.

```bash
node ui/tools/check.js --pack ui/tools/fixtures/pack-hover --fixture-matrix ui/tools/fixtures/matrix.json \
  --pages fleet,drives --sizes 1280x800 --hover
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
pages fail with what moved (how far, why, the late block) and what came and went (how often), then checks the hover
pack with `--hover` and asserts that the still page passes and each case on the moving page is found with what moved. It prints one line per assertion and exits `0` when all hold.
