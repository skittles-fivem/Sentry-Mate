# Shared browser modules

Plain ES modules every UI pack uses, so pages look different but count, convert and fetch the same way. No
build step, no dependencies, safe under the strict Content-Security-Policy (no `eval`, no inline styles, no
`innerHTML` with data).

**Edit them here, in `ui/shared/`.** Each pack serves a copy from `ui/packs/<name>/web/shared/`, written by:

```bash
python3 ui/tools/sync_shared.py            # copy into every pack
python3 ui/tools/sync_shared.py --check    # change nothing; fail if a copy is out of date
```

Pages import them relative to `web/`: `import * as api from './shared/api.js';`. Pages never call `fetch`
themselves.

Tests (Node 22, no packages; the browser test uses the pre-installed Playwright and is skipped without it):

```bash
node --test ui/shared/tests/
```

## Modules

| Module | What it is for |
|---|---|
| [`api.js`](api.js) | The one client for `/api/v1`: reads, cursor paging, changes with the CSRF header, errors, sign-in prompts, live events, cancelling, the server's clock |
| [`units.js`](units.js) | The person's units applied to the API's metric values; every number with its unit |
| [`time.js`](time.js) | Dates and times in the car's time zone, ISO weeks, buckets, the period presets |
| [`state.js`](state.js) | Page, car and period in the URL; the shared store; links that carry the car and period; the person's car when the URL names none (`setCarPrefs(prefs)`, `preferredCar(cars)`: main car, else last car not sold, else first pinned, else first not sold; `rememberCar(id, { sold, explicit })` saves `last_car`, debounced) |
| [`i18n.js`](i18n.js) | `t()` with the English text as key; the locale for numbers and dates; `<html lang>` |
| [`csv.js`](csv.js) | CSV built in the browser, for chart data that has no list resource |
| [`dom.js`](dom.js) | `h()` and other safe DOM helpers; coverage marks; the readiness signal |
| [`packs.js`](packs.js) | `packChoices(client, { fallback, current })`: the UI packs a person can choose (`GET /ui-packs`), with a short description each, the page's own list if the request fails |
| [`tyres.js`](tyres.js) | `tyreView(now)`: the Now's four tyres against the recommended pressures (each against its axle's when the sensors' wheels are known, else against the lower one, as the server judges `tyre_low`), and `recommendedLine(now)`: "Recommended 2.90 bar (from CAN)." with where it comes from (CAN, the door placard for the car's wheel size, typed) |
| [`password.js`](password.js) | `passwordAdvice(name, password)`: why a new password is easy to guess, in the server's words; pages warn with it and let the person go on |
| [`maptiles.js`](maptiles.js) | Map tiles for every map: which tiles to draw (`GET /maps/config`: the offline map file, the online source, or none and why), raster paint for the theme and the person's map colours (dimmed, never inverted, in the dark theme), a plain style for an offline Protomaps vector file, the "No map tiles yet" note with an admin's one-click OpenStreetMap switch, the note with the cause when tiles fail, and the words of the Settings map rows |
| [`fullrebuild.js`](fullrebuild.js) | The full rebuild's shared words and rules for every pack's Data health and Settings: Server and access: what it does and does not touch (`EXPLAIN`), the button and password prompt words, the fixed seven lines of the progress panel (`lines(status)`: state, step, logs, days, data, speed and time left, last progress with the stall warning), `errorLines`, `percent`/`fraction` for the bar, `body(password, code)`, `errorText(e)`, the poll intervals |
| [`teslamate.js`](teslamate.js) | The local TeslaMate input's shared rules for every pack's Settings: Integrations section: state and error words, the drafts of the MQTT and database forms, the server's checks said first (the plain text rule, a kept password, the pin, the topic prefix), the PUT body and the changed car matches, the TLS tick's port switch (`tmTlsPort`: 1883 and 8883, only from the other default) and the Host and pin hints; and its notices for every pack's notices bar (`isTmNotice`, `tmNoticeFix`: the link to Settings: Integrations #teslamate, `TM_NOTICE_ACTION`: the link's words) |

### api.js

`get(path, params, opts)`, `page(path, params, { after, limit })`, `list(path, params, opts)` (async iterable of
items, plus `.pages()` and `.all({ max })`), `pager(path, params, { limit })` (`more()`, `items`, `done`,
`last`, `reset()`), `send(method, path, body, opts)` and `post` / `put` / `patch` / `del`, `csvUrl(path,
params)`, `url(path, params)`, `apiPath` (tagged template that encodes ids), `events(onEvent, { types,
onStatus, signal })` and `reconnectEvents()`, `scope()` (a client for one page; `abort()` cancels all it
started), `on('unauthorized' | 'network' | 'events', fn)`, `now()` (the server's clock), `invalidate(prefix)`,
`network()`, `eventsState()`, `ApiError`, `isAbort`, `createClient(options)` (for tests).

- Every call takes `{ signal }`. The same GET asked for twice while the first is under way is sent once; a
  caller that leaves only stops waiting, and the request is cancelled when every caller has left.
- `opts.maxAgeMs` reuses a result that young (for `me`, `me/preferences`); any change drops kept results.
  `opts.keep` keeps an answer for later `maxAgeMs` callers without reusing an older one itself: the frame reads
  `notices` with it, and a page loaded at the same moment asks with a short `maxAgeMs` instead of sending it again.
- Errors are `ApiError { status, code, message, fields, retryAfterMs }`; `status` 0 is no answer (`code`
  `network` or `timeout`). A 401 also tells `on('unauthorized')` listeners. A read answered 429 with a short
  `Retry-After` (10 s or less) waits and tries once more by itself; changes are never repeated.
- `events()` shares one EventSource between callers, closes it when the last one stops, and after a drop
  tries again after 1, 2, 4 ... up to 60 s (with jitter), checking first that the session is still there.
- `now()` estimates the server's clock from each response's `X-Sentry-Mate-Now-Ms` header, or its standard
  `Date` header (to the second). Until the first answer it is the browser's clock.

```js
import * as api from './shared/api.js';
const page = api.scope();                       // call page.abort() when the page is left
const car = await page.get(api.apiPath`cars/${carId}`);
for await (const d of page.list(api.apiPath`cars/${carId}/drives`, { from_ms, to_ms }, { max: 200 })) rows.push(d);
api.on('unauthorized', () => showSignIn());
```

### units.js

`setUnits(prefs)` (takes `/api/v1/me/preferences` or a flat `{ distance, temperature, pressure, range }`),
`getUnits()`, `onUnitsChange(fn)`, `format(value, kind, opts)`, `parts(value, kind, opts)`, `formatField(obj,
field)`, `kindOf(field)`, `convert(value, kind)`, `toMetric(value, kind)`, `unitLabel(kind)`,
`digitsFor(kind, value)`, `formatMoney(amount, currency)`, `formatMoneyList(list)`, `setMissingText(fn)`,
`rangeField(field)`, `rangeValue(obj, field)`, `rangeLabel()`, `KIND_NAMES`.

Kinds and their API unit: `distance`, `odometer`, `range` (km), `length` (m: elevation, accuracy, radius),
`speed` (km/h), `range_rate` (km of range per hour), `temperature`, `temperature_delta` (°C), `pressure`
(kPa), `energy` (kWh), `power` (kW), `power_w` (W), `efficiency` (Wh/km), `percent`, `voltage`, `current`,
`degrees`, `count`, `number`, `duration` (ms), `bytes`, `money`, `price_energy` (per kWh), `price_time` (per
minute), `money_per_distance` (per 100 km).

Precision per kind: distance 1 decimal under 100, else none; odometer, range, speed, efficiency, length
none; temperature 1 decimal; pressure 2 decimals in bar, none in psi; energy 2 decimals under 1 kWh, 1
under 1000; power 1 decimal under 10 kW; percent none. `opts.digits` overrides. A value that rounds up past a
step drops the decimal (99.97 mi is `100 mi`). Number and unit are joined by a no-break space. `null` is
`—`; `parts()` also gives `why`, from `opts.why` or the `setMissingText()` hook, for the page to show.
Currencies are never added together.

```js
import { setUnits, format, parts } from './shared/units.js';
setUnits(await api.get('me/preferences', null, { maxAgeMs: 60000 }));
format(drive.distance_km, 'distance');            // '7.6 mi'
format(now.outside_c?.value, 'temperature');       // '68.0 °F'
parts(null, 'pressure', { why: 'Tyre sensors not paired yet' }).why;
```

### time.js

Zones: `zonedParts(ms, tz)`, `zonedToMs(parts, tz, { disambiguation })`, `zoneOffset`, `zoneName`,
`isValidZone`. Days and weeks: `dayKey`, `dayStart`, `addDays`, `daysBetween`, `isoWeek`, `isoWeekOf`,
`isoWeeksInYear`. Calendar: `startOf(ms, unit, tz)`, `endOf`, `addPeriod(ms, unit, n, tz)`, `bucketKey`,
`buckets(from, to, unit, tz)`, `groupByBucket` (units: hour, day, week, month, year). Formatting:
`formatDate`, `formatTime`, `formatDateTime`, `formatWhen` ('Today, 5:30 PM'), `formatDayKey`,
`formatDateRange`, `formatRelative` ('5 min ago'), `formatDuration` ('2 h 05 min'), `toIsoZoned`,
`dateFormat`. Periods: `PRESETS`, `presetLabel`, `normalizePeriod`, `resolvePeriod`, `periodParams`,
`periodLabel`, `shiftPeriod`, `dayRange`, `periodDays`.

A day is the wall-clock day in the zone (23 or 25 hours on clock-change days); weeks are ISO weeks. Presets:
`24h` the last 24 hours; `7d` today and the 6 days before; `30d` today and the 29 days before; `year` since
1 January; `all` since the oldest data (or no lower limit); custom `{ from, to }`. All end now.

```js
import { resolvePeriod, periodParams, buckets } from './shared/time.js';
const period = resolvePeriod(state.current().period ?? '7d', { now: api.now(), tz: car.time_zone });
const drives = await page.get(api.apiPath`cars/${car.id}/drives`, periodParams(period));
const days = buckets(period.from_ms, period.to_ms, 'day', car.time_zone);
```

### state.js

`ROUTES`, `PAGE_IDS`, `PERIOD_PAGES`, `CAR_PAGES`, `parseUrl(url)`, `buildUrl(page, { car, drive, charge,
period, query })`, `parsePeriod(search)`, `periodQuery(period)`, `createStore(initial)`, `store`, `current()`,
`subscribe(fn)`, `syncFromLocation()`, `sameRoute(url)`, `navigate(href, { replace })`, `linkTo(page, params)`,
`linkForCar(car)`, `setPeriod(period, { push })`, `setQuery(patch, { push })`, `startRouting()`,
`tabPeriod()`, `lastCar()`, `rememberCar(id)`, `forgetCar(id)`.

The car is in the path, the period in the query (`?preset=7d` or `?from=<ms>&to=<ms>`). A URL without a
period uses the one chosen last in this tab; a page with neither uses its own default (`period` is null).
`navigate()` removes `data-ready`. `startRouting()` reads the address, follows back and forward, and turns
clicks on in-app links into `navigate()`; subscribe first, it notifies at once. A step that only moves the
`#hash` on the page shown (an in-page anchor, back / forward between anchors, `navigate()` to the same URL
with another hash) is no new page: `data-ready` stays, the store does not change, the router scrolls to the
element and the page can open the part holding it on `hashchange`.

```js
import * as state from './shared/state.js';
state.subscribe((s) => render(s.page, s));
state.startRouting();
h('a', { href: state.linkTo('charges') }, 'Charges');   // '/cars/c1/charges?preset=7d'
state.setPeriod({ preset: '30d' });
```

### i18n.js

`t(text, vars)`, `tn(n, one, other, vars)`, `addCatalog(lang, messages)`, `setLocale(tag | 'auto')`,
`getLocale()`, `getTextLanguage()`, `resolveLocale({ choice, browser })`, `onLocaleChange(fn)`,
`isSupportedLocale`, `formatNumber(v, opts)`, `numberParts`, `numberFormat(opts)`, `formatList(items)`,
`compareText(a, b)`.

The locale is the person's choice, else the browser's languages in order, else `en-US`. `<html lang>` names
the language the text is really in: the locale when a catalog for its language exists, otherwise `en`.

```js
import { t, tn, setLocale } from './shared/i18n.js';
setLocale(prefs.language || 'auto');
t('{n} drives in {city}', { n: 12, city: place.label });
tn(count, '{n} charge', '{n} charges');
```

### csv.js

`toCsv(rows, columns, { header, eol, bom })`, `csvCell(value)`, `column(label, key, kind)`, `timeColumn(label,
key, tz)`, `rowsFromColumns(series)`, `csvFilename(...parts)`, `downloadCsv(filename, rows, columns)`.

RFC 4180 with CRLF; numbers with a dot, never in the locale; text starting with `=`, `+`, `-` or `@` gets a
leading apostrophe so spreadsheets do not run it. Unit columns convert to the person's units and name the
unit in the header. Lists have server CSV (`api.csvUrl`); this is for chart data.

```js
import { downloadCsv, rowsFromColumns, column, timeColumn, csvFilename } from './shared/csv.js';
const rows = rowsFromColumns(series);
downloadCsv(csvFilename(car.name, 'battery-level'), rows, [timeColumn('Time', 't_ms', car.time_zone), column('Battery', 'soc_pct', 'percent')]);
```

### dom.js

`h(tag, attrs, ...children)`, `svg(tag, attrs, ...children)`, `append`, `text`, `frag`, `clear`, `replace`,
`on(target, type, fn)`, `esc`, `safeUrl`, `setVars(el, vars)`, `cssVar(name)`, `cover(el, ...ids)`,
`ready(pageId)`, `unready()`, `failed(pageId)`, `readyPage()`, `setTitle(...parts)`, `quantity(value, kind,
opts)`, `styleSheet(href, { first })` (adds a style sheet once, after the others or with `first` before them, and resolves when it has loaded: maps and charts await it).

`h()` attributes: `class` (text, array or `{ name: on }`), `dataset`, `vars` (CSS custom properties, set through
CSSOM), `style` (an object, CSSOM), `cover` (inventory item ids), `on<event>` functions, `value` / `checked`
as properties; `href` and `src` go through `safeUrl` (no `javascript:`). Strings are always text. `ready()`
sets `<html data-ready="<page id>">` after the first render with data; `failed()` sets `error:<page id>`.

```js
import { h, cover, ready, quantity } from './shared/dom.js';
const tile = h('div', { class: 'tile', vars: { '--fill': `${soc}%` }, cover: 'shell.nav.home' },
  h('span', { class: 'label' }, 'Battery'), quantity(now.soc_pct?.value, 'percent'));
ready('car');
```
