# Security and privacy

[← Back to the README](../README.md)

- [Security](#security)
- [The Tesla API input](#the-tesla-api-input)
- [The TeslaFi input](#the-teslafi-input)
- [Supercharger invoices](#supercharger-invoices)
- [The local TeslaMate input](#the-local-teslamate-input)
- [No way back without a backup](#no-way-back-without-a-backup)
- [Privacy](#privacy)
- [When a log fails its check](#when-a-log-fails-its-check)
- [Pairing and the owner's word](#pairing-and-the-owners-word)
- [A new signing key](#a-new-signing-key)

## Security

- TLS 1.3 only on the upload port; plain HTTP and older TLS are refused. The certificate is self-signed and made on
  first start, its private key sealed in the vault; the app pins it by SHA-256. An admin can make a new one from the
  web pages (Settings → Server and access, with the password again and the two-step code): it is written beside the
  old one and renamed into place, so a power cut never leaves none, and it is never made over the owner's own
  certificate or while a move is under way. Each CAN Bridge then needs the new SHA-256.
- The upload port can't be filled without a token: connections are counted per address (an IPv6 /64 is one), the
  TLS handshake and the headers must come within 10 s each, and a connection that has not shown a valid token is
  closed, oldest first, to make room for a new one, so the CAN Bridge's uploads still get in.
- A log can't unpack to more than it is worth: at most 200 times its packed size (at least 1 MiB), 4 GB of candump
  text, a columns body of 64 MiB and 8 million frames. Real logs unpack to under 10 times their size, so a few
  kilobytes crafted to unpack to gigabytes are cut or refused (a refused log is a hole, never decoded again).
- An import's upload (a TeslaMate backup, TeslaFi's files) is held to the same idea as it is read: past 16 MiB a
  gzipped file or a zip's entry may unpack to 500 times its packed size and an xz'd one to 4000 times (a CAN export
  zip's entries to 100 times), else the import fails as `unreadable`. Rows that hardly differ pack far better than
  logs do (a TeslaFi month in which the car stood asleep: 100 to 200 times, with xz up to some 1,300), so the limits
  sit above that and below what runs of one byte reach (about 1,030 with deflate, 7,000 with xz).
- Pages that decode stored logs while they answer (signals; the battery pages and efficiency only for a day whose pack
  values the server has not worked out ahead in time) take turns: at most 3 run at once for the whole server and 4 run or wait per account (`503 busy`, `429 throttled` beyond), a request
  that goes away stops its decoding, and one answer decodes at most 2 GiB of log lines. A watch account can't keep
  every core busy, or starve uploads and the engine.
- Tokens are write-only: they can upload and ask which uploads arrived, nothing else. They are stored only as their
  SHA-256 and tied to one car key on the first upload. That first upload never takes a car key in use already
  (another token is tied to it, or its logs are here), unless the token was made for that key's car (on the car's
  page, or `token new --key`): car keys are in every log name, so they are not secret, and a token handed out for one
  car could otherwise upload into another car's history, block its log names or pair its own signing key with it. Wrong tokens are rate limited per address; a right
  token is never refused because of wrong ones.
- Every log's ECDSA P-256 signature is checked against the key the car key is paired with. A log signed by another
  key is kept and marked failed, and not used until the owner trusts that key (`keys trust`, after a `yes`). Only
  a key that signed some of the car key's logs can be trusted. Deleted, swapped or reordered logs show in the
  chain. A log without a signature from a paired car is not used either, unless an admin says so for that log,
  their password asked again, never for a badly signed one ([Pairing and the owner's word](#pairing-and-the-owners-word)).
- **Encryption at rest, in Rust.** One random data key seals the contents of everything stored (logs, sidecars,
  uploads still arriving, trust records, tokens, the TLS key) with XChaCha20-Poly1305, in 64 KiB chunks, each file
  with its own key (HKDF-SHA256) and its path bound in, so a file that is changed, cut short, or moved under another
  name doesn't open. Files that grow (uploads still arriving, journals) give every piece its own random nonce, so a
  piece written again after a crash never reuses one, and bind each piece to the one before it, so pieces can't be
  reordered, dropped from the middle or mixed with those of an older copy of the file (an older copy put back whole,
  or pieces missing at the end, can't be told from a crash). The data key is kept only wrapped: by the key file in
  `/keys` (a separate volume, refused inside the data folder) for restarts with nothing to type, and by the recovery
  key through Argon2id at 1 GiB. Keys are wiped from memory when no longer needed and core dumps are off. This
  protects a stolen or copied disk or backup; it can't protect a server taken over while it runs. File and folder
  names, sizes and times are not sealed (see [Privacy](#privacy)).
- Pure Rust where it matters (`unsafe` forbidden in this code, the xz reader included); files from the network are
  read with fixed limits. The image is one static binary on `scratch`.
- **How the image is built.** The workflow that builds and publishes it (`.github/workflows/image.yml`, the only
  one with a token that can push `latest`) runs each GitHub Action at a full commit id, never a tag or branch the
  action's owner could move, and the Dockerfile builds on the Rust image pinned by digest. A taken-over action or
  base image tag upstream can't change what goes into the image. Dependabot proposes new pins as changes to review,
  and a test (`crates/sentry-mate/tests/repo_hygiene.rs`) fails on an unpinned action or image.
- **The container.** The image runs as 1000:1000; the TrueNAS file runs it as TrueNAS's apps user, 568:568. The
  compose and TrueNAS files drop every capability, make the root read-only and set `no-new-privileges`, so it can
  write only to its two mounted folders.
- **Folders it can't keep are refused**, before anything is written: a data or keys folder that is not mounted (it
  would be lost with the container, the key file with it), a keys folder inside the data folder or the same folder
  at both (the key file would be in the data's backups), and a folder it can't write to (most often root's).
- **A new vault is never made over an existing key file.** An empty or wrong data folder next to a key file stops it
  instead, so the key file that opens your data is never replaced.
- Nothing leaves the server unless an admin turns it on. These are all the outgoing connections the server makes,
  and the switch for each (Settings → Integrations lists them, `GET /api/v1/network`):
  - **MQTT**: to the broker an admin sets up, while MQTT is on.
  - **The Tesla API**: to Tesla's own hosts only, once an admin adds a Tesla account. See
    [The Tesla API input](#the-tesla-api-input).
  - **TeslaFi**: to `www.teslafi.com` only, once an admin adds a TeslaFi token. See
    [The TeslaFi input](#the-teslafi-input).
  - **Supercharger invoices**: to `ownership.tesla.com` (Tesla) and TeslaFi's charge log, only once an admin turns
    them on in Settings → Integrations. See [Supercharger invoices](#supercharger-invoices).
  - **Local TeslaMate**: to the owner's own TeslaMate MQTT broker (`teslamate_mqtt`) and, when turned on, its
    database (`teslamate_db`), at the host and port an admin typed, once an admin adds them. See
    [The local TeslaMate input](#the-local-teslamate-input).
  - **A move**: a new server, during a move you started, connects to the old server's upload port, and nowhere else.

  With none of these turned on, Sentry Mate makes no outgoing connection at all.
- The viewers' browsers load nothing from outside either. Map tiles are offline until an admin picks online tiles in
  Settings; until then the pages' Content-Security-Policy names no outside host and every map draws the track on a
  plain background. Online tiles send each viewer's browser to that one tile server, which then sees the viewer's
  address and the tiles asked for (where the car drove and parked), so it stays off unless the owner chooses it.
  See [Map tiles](#map-tiles).
- **The move code** ([moving.md](moving.md)) is 125 random bits, works once, for 24 hours, and only from the home
  network unless made with `--anywhere`. Logs name it by a short id. Only the old server shows the code itself,
  when asked (`move code`, or in its log with `SENTRY_MATE_OFFER_MOVE`). The new server proves the code over the TLS
  connection itself, so a fake server or someone in between learns nothing from it. The data key travels last,
  wrapped with a key from the code, inside TLS 1.3. The key file never leaves its folder: the new server writes a
  key file of its own, and the recovery key stays the same. Treat the code like a password until it is used, and
  run `move cancel` on the old server if someone else may have used it.
- **An export file never holds the key file.** It opens only with the recovery key (Argon2id at 1 GiB), so a
  copied export file is useless without it. It still holds everything, sealed as stored: keep it private, and delete
  it once the new server runs.
- A move or an import goes only into a new server: empty data and keys folders, or only what a first start made
  there. A key file that opens other data is never replaced.
- **The audit log names what changed.** Moving a CAN Bridge key between cars (it decides whose history a car's signed
  logs count for), marking it preferred, and making or revoking an upload token are each recorded with the key or
  token id and the car (both cars for a move), so another admin can see which car's history was redirected. A
  change that is not worded on its own is recorded with the ids of its request. A watch account can't fill the
  log: the places preview writes nothing, and the same preferences change within 10 minutes is one entry.
- A watch account's set-up status speaks only of its own cars: it never shows a CAN Bridge key waiting for a car,
  nor counts the tokens made for a new car.
- **Signing in to the web UI.** Passwords are argon2id hashes; failed tries wait longer each time, per address and
  per account (home and outside counted apart); right passwords too are limited (20 sign-ins a minute per account,
  one check at a time per account), so a loop of sign-ins can't keep the few password checks busy for everyone else.
  A password change signs out every other session, even a sign-in that was still being checked with the old
  password at that moment. With the "two-step from outside" rule, a right password of an account without two-step
  sign-in gets, from outside, the answer a wrong one gets, so the internet can't learn that a guess was right.
- **Passwords are the person's choice.** The server refuses only an empty password and one over 1,024 bytes, for
  every account, the first admin included: no length rule, nothing about the account name. The pages check a new
  password first (shorter than 12 characters, close to the account name, one of the most common, repeating itself)
  and, when one of those holds, say why in a pop-up with "Use it anyway" and "Change it". A weak password is still
  hard to use from outside: argon2id makes each guess slow, the throttles above stop a guessing loop after a few
  tries, the web UI is HTTPS only (a password never crosses the network in the clear), and two-step sign-in (or the
  "two-step from outside" rule) puts a code from an app after the password.
- **Changes ask again.** Every account and two-step change (a new account, a role, a password, a car list, turning
  someone's two-step sign-in off, new recovery codes, the two-step rule), and each server change that can lock a
  device out or hand data on (a move code, an export file, the recovery key, a new certificate), asks for the acting
  account's password and, when it has two-step sign-in on, a code from the app or a recovery code: a browser left
  open plus a known password isn't enough. When the rule changes, open live streams of accounts it cuts off end at once.
- **What counts as the home network** is decided from the connection's address: loopback, the private ranges and
  link-local. Behind Docker's published ports that address is only right for IPv4: a connection to the host's
  **IPv6** address on a network without IPv6 (the compose file's default) is passed on by Docker's proxy from the
  bridge's gateway (for example `172.18.0.1`), so every IPv6 visitor from the internet looks like the home network,
  the "two-step from outside" rule doesn't hold for them, and their failed tries share one address. If the host has a
  public IPv6 address, don't open the ports to it in the firewall, or turn IPv6 on for the compose network
  (`enable_ipv6: true` with an IPv6 subnet) so the real address arrives. The same goes for a reverse proxy on the
  same host: it is the address the server sees.
- Sealing the key file to the host's TPM is optional, later.

## Map tiles

Online tiles are off by default; Settings → Maps and addresses (an admin's page) turns them on and picks the
source: **OpenStreetMap standard** (`tile.openstreetmap.org`, the default), **Satellite** (Esri World Imagery,
`server.arcgisonline.com`, with Esri's transparent place names, borders and roads layers from the same host on top
unless "Place names and roads" is off), **Satellite, US only** (USGS The National Map imagery,
`basemap.nationalmap.gov`, public domain, zoom 16 at most), **OpenTopoMap** (`tile.opentopomap.org`) or your own
`https://` address with `{z}`, `{x}` and `{y}`. The ready-made sources are free, need no key and allow a web app to
show their tiles with light use and their credit on the map (Esri: "Imagery © Esri, Maxar, Earthstar Geographics, and
the GIS User Community"); each map shows the credit in its corner, never folded away. A server whose admin never
picked a source (including one still on the old OpenStreetMap default) uses OpenStreetMap standard; a source an admin
picked is kept.

- **Viewers can switch Map / Satellite** on a map page, among the sources the admin allows ("Maps people can switch
  to", OpenStreetMap and Satellite by default); each person's pick is remembered for them (`map_style`). Every host
  of the chosen source and of each allowed one is listed in Settings → Integrations → Network ("Every connection",
  the Outbound "Map pictures" row) and allowed in the pages' Content-Security-Policy (`img-src` and `connect-src`),
  and no other.
- **The browser asks the tile server itself**, as these sources' terms expect. OpenStreetMap's
  [tile usage policy](https://operations.osmfoundation.org/policies/tiles/) asks browsers to send a valid `Referer`
  and blocks tile requests without one ("403 Access blocked"), and it advises against putting a caching proxy in
  front of its servers. So while online tiles are on, the pack pages say `Referrer-Policy: strict-origin`: the tile
  server gets the page's origin (`https://sentry.example:10101`), never a path, a car, a drive or a query. API
  answers still say `no-referrer`, and with online tiles off every page does. The Content-Security-Policy lets the
  pages reach only those tile servers, and links to other sites carry `noreferrer`.
- **A page keeps the policy it was loaded with.** After an admin turns online tiles on or picks another source, a
  page that was already open still blocks the new tile server. Its maps notice the first blocked tile and load the
  page once more to get the new policy (not again for the same server within two minutes).
- **What the tile server sees**: the viewer's address, the server's origin and the tiles asked for (the area the
  map shows). Nothing else leaves; the server itself downloads no tiles.
- OpenTopoMap's terms are on [opentopomap.org/about](https://opentopomap.org/about) (light use, no bulk
  downloads, the credit "Map data: © OpenStreetMap contributors, SRTM | Map style: © OpenTopoMap (CC-BY-SA)").
- OpenFreeMap is not offered: it serves vector tiles only, and the packs draw raster tiles.
- **A map with no tiles says so.** With offline tiles and no map file (a new server), every map shows "No map tiles
  yet" over its plain background: an admin gets "Use the online map", which turns the default source (OpenStreetMap standard) on
  only after a confirm that says what the tile server then sees (each viewer's IP address, this server's
  address and the map area), and "Add an offline map file"; anyone else is told to ask an admin. Tiles that fail
  (403 blocked, 429, unreachable) show a short note with the cause: maps draw tiles as images, which say nothing
  about why they failed, so a map asks the same server for the same tile once more to read its status.
- **Dark maps are dimmed, not inverted**: in the dark theme raster tiles are drawn darker so streets, labels and
  water stay readable. Each person can pick "original colours" instead (Settings → Display and units, Map colours).
  Satellite pictures are never dimmed (photos would turn to mud); the car, routes and place outlines get a dark
  edge so they stay clear on them.

### Offline map file

An offline map file keeps every tile request on the server: the browsers ask Sentry Mate for tiles
(`/api/v1/maps/tiles/{z}/{x}/{y}`, signed-in accounts only), which reads them out of a
[PMTiles](https://github.com/protomaps/PMTiles) file in the data folder's `maps/` folder. Nothing leaves the server,
and the pages' policy names no outside host.

- **Sentry Mate never downloads a map.** An admin makes the file (for example a Protomaps extract of their area,
  made on their own computer with the `pmtiles` tool: [install.md](install.md#offline-map)) and copies it into
  `data/maps/`. Any `*.pmtiles` there counts; the largest is used unless an admin picks another in Settings →
  Maps and addresses (`tiles.offline_file`, a plain file name: no folders, no `..`).
- The file is only read: its header and directories are checked (version 3, plain or gzip directories, sizes
  bounded) before a tile is served, and a damaged file is named in Settings instead of served.
- A Protomaps (vector) file is drawn in a plain style without names (the server serves no fonts); a raster
  `.pmtiles` shows its pictures as made.

## The Tesla API input

The server can read your cars from Tesla's Owner API (the unofficial API Tesla's own app uses), beside or instead of
SMO. How to add an account is in [install.md](install.md#add-a-tesla-account).

- **Off until an admin adds a Tesla account.** Before that the server starts nothing for it, makes no connection
  and no DNS lookup for any Tesla host. Removing the last account stops it again.
- **Only Tesla's own hosts, over TLS.** The hosts are fixed in the program, never taken from a setting or an answer:
  `auth.tesla.com`, `owner-api.teslamotors.com` and `streaming.vn.teslamotors.com`, or for a China account
  `auth.tesla.cn` and `owner-api.vn.cloud.tesla.cn`. TLS 1.3 with rustls and the public web roots; redirects are
  not followed. Through the owner's HTTP proxy only when one is set in Settings (`outbound.proxy`).
- **What is sent, and nothing else:** the token requests (a refresh of the access token, or the code from your
  sign-in), the list of the account's cars, one car's data, and, while a car drives, a subscription to Tesla's live
  stream for it. The requests carry the access token and a plain `SentryMate/<version> (self-hosted)` User-Agent.
  Nothing about this server, its other cars, its accounts or its owner is sent.
- **Read-only.** There is no code that wakes a car or sends it a command. A car that is asleep is left asleep: only
  the car list is read then, which does not wake it. An awake car with nothing going on is left alone for a while
  so it can fall asleep. Requests go one at a time per account, at the intervals an admin set (never faster than
  driving every 2.5 s); there is no cap of its own: when Tesla answers 429 the server waits as long as Tesla asks,
  and says so.
- **The sign-in is on Tesla's own page,** in your browser: Sentry Mate never sees your Tesla password, your
  two-step code or a captcha. You paste back the address the page ends on; the code in it works once and only for
  that sign-in (it is bound to a secret that never leaves the server).
- **Tokens are sealed in the vault** (`tesla/accounts/<id>.json` in the data folder, encrypted with the vault key
  like everything else), held in memory only while used, and wiped after. No page, API answer, audit entry or log
  line shows a token. A move or an export carries the account to the new server, still sealed.
- **No full VIN is kept:** the VIN prefix, as for SMO, and a keyed hash to tell two cars with the same prefix apart.
  The access token names the account's e-mail inside; the server never reads it, and the e-mail is kept nowhere.
- **Admins only.** Every page and call about Tesla accounts is for admins, and every change writes an audit entry.
  Adding, replacing or removing an account or its token (and the poll pace) asks for the admin's password again (and
  the two-step code when it is on); matching a Tesla vehicle to a car here takes the admin's session alone, as it
  stores and shows no token. When Tesla stops taking an
  account's token, the server stops asking (no retries against Tesla's sign-in), writes an audit entry and shows a
  notice to admins until someone signs in again.
- **Removing an account** stops reading at once and deletes its sealed file. Sign the server out at Tesla too
  (your Tesla account's security page), so the token stops working there as well.
- Data from the Tesla API is not signed like SMO's logs. Every row says where it came from (`tesla_api`), so it is
  never mistaken for checked CAN data.

## The TeslaFi input

For owners who pay for TeslaFi, the server can read TeslaFi's own polls of the car from TeslaFi's API, beside or
instead of SMO and the Tesla API. TeslaFi is a third party: it already polls your car with the Tesla access it was
given; this server only reads what TeslaFi stored. How to add the token is in
[install.md](install.md#add-a-teslafi-token).

- **Off until an admin adds a TeslaFi token.** Before that the server starts nothing for it, makes no connection and
  no DNS lookup for TeslaFi. Removing the last account stops it again (`/network` shows its `teslafi` row off).
- **Only TeslaFi's API host, over TLS.** The host is fixed in the program, never taken from a setting or an
  answer: `www.teslafi.com` on port 443, with rustls and the public web roots. TLS 1.3 whenever TeslaFi's host
  offers it (rustls refuses a downgrade from it); TLS 1.2 is allowed for this one host only, in case TeslaFi's edge
  stops offering 1.3, and the TeslaFi status says which one was used (`tls`). Plain `http://` is never used,
  redirects are never followed. Through the owner's HTTP proxy only when one is set in Settings (`outbound.proxy`).
- **What is sent, and nothing else:** `GET /feed.php` (TeslaFi's latest poll of the car) and
  `GET /feed.php?command=lastGood` (its last poll of the car awake), with the token in the `Authorization` header
  (never in an address, where proxies and logs keep it) and a plain `SentryMate/<version> (self-hosted)`
  User-Agent. These two reads are fixed in the program: no other command can be built, so no wake (`wake_up`,
  `wake`, a `wake=` parameter), no command to the car, no change to TeslaFi's logging is ever sent. They are served
  from TeslaFi's database and do not wake the car.
- **Polite.** One read a minute at most while the car drives or charges (every 15 s with the `often` pace, about as
  often as TeslaFi itself refreshes), less while it is parked or asleep (never less because SMO or the Tesla API is live for it);
  never two requests within 10 s; at most 1,500 requests a day per account (6,000 with `often`); waits after errors
  and whatever TeslaFi asks for (429, `Retry-After`), whatever the pace.
- **The token is sealed in the vault** (`teslafi/accounts/<id>.json` in the data folder, encrypted with the vault
  key), held in memory only while used and wiped after. No page, API answer, audit entry, error or log line shows
  it. A move or an export carries the account to the new server, still sealed.
- **No full VIN is kept:** the VIN prefix (11 characters at most) and a keyed hash to tell two cars apart. A car
  TeslaFi names is not read into any car here until an admin confirms which car it is.
- **Admins only.** Every page and call about TeslaFi is for admins, and every change writes an audit entry. Adding,
  replacing or removing an account or its token (and the poll pace) asks for the admin's password again (and the
  two-step code when it is on); matching TeslaFi's car to a car here takes the admin's session alone, as it stores
  and shows no token. When TeslaFi refuses the token (a wrong
  token, or the subscription ended), the server stops asking, writes an audit entry and shows a notice to admins
  until someone gives a new token.
- **Removing an account** stops reading at once and deletes its sealed file. Make a new token on TeslaFi's API page
  too, so the old one stops working there as well.
- Data from TeslaFi is not signed like SMO's logs. Every row and value says where it came from (`teslafi_api`, "Data
  from TeslaFi"), and SMO's and the Tesla API's data win over it where they cover the same time.

## Supercharger invoices

The server can fetch your paid Supercharger sessions, their costs and their invoices (PDF), to price your DC charges
with what Tesla billed. Each input has its own switch (Settings → Integrations, "Supercharger invoices"), **off until
an admin turns it on** (the password asked again, an audit entry written): before that nothing below is fetched.

- **From Tesla**, with the Tesla account's own Owner API tokens (no new sign-in), from one more of Tesla's hosts,
  fixed in the program: `ownership.tesla.com` (the host Tesla's app reads its charging history from; global accounts
  only: no public source names one for China, where Tesla's charging history is not open yet). Two reads, both
  `GET`, nothing else:
  - a page of a car's charging history: `/mobile-app/charging/history?vin=<the car's VIN>&deviceLanguage=en&
    deviceCountry=US&operationName=getChargingHistoryV2&pageNo=N&pageSize=50`. The VIN goes to Tesla because Tesla
    asks for it by VIN; it is held in memory only (from the car list) and never kept or logged here;
  - an invoice's PDF: `/mobile-app/charging/invoice/<Tesla's id of it>` (with the same language, country and VIN).
  The records are the shape Tesla documents for its Fleet API's charging history (`sessionId`, `siteLocationName`,
  start and stop times, `fees` with their currency, rates, amounts billed and owed, `invoices` with `contentId`).
  Tesla does not document this route for Owner API tokens; if Tesla refuses it, the status says so and the cars go
  on being read as before.
- **From TeslaFi**: TeslaFi's documented API (`feed.php`) has no command for charges, costs or invoices, and TeslaFi
  documents no way to download the Supercharger invoices it fetches for its own pages. The server reads TeslaFi's
  charge log, `GET /history.php?command=charges&dateFrom=<date>&dateTo=<date>` (the path made of two dates only,
  the token in the `Authorization` header as for the polls), and takes the cost TeslaFi lists for each charge. No
  PDFs come from TeslaFi.
- **Paced.** One request at a time, after the car's own reads (never instead of one). TeslaFi: in its own pace and
  gap between requests, counted in its daily cap, never more than 60 a day or into the last 200 requests of the
  cap. Tesla: no cap of this server's own (Tesla does the limiting; its 429 is waited out as it asks). The first time, the whole history is read page by
  page (resumable: a restart goes on where it was); then once a day, and soon after a DC charge (TeslaFi: a charge)
  ends. An error waits half an hour, doubling up to a day; Tesla's or TeslaFi's `Retry-After` is followed.
- **Kept sealed, once.** Each session (one per Tesla session id or TeslaFi charge id) and each PDF (at most 4 MiB,
  only a file that is a PDF) is kept in the server's database, sealed with the vault like everything else, and moves
  and is backed up with it. It is kept as it came: rebuilding the cars' history never changes it, and a full rebuild
  matches it again.
- **Who sees it.** A charge's invoice, and its PDF, are shown to whoever may see that car; the list of every session,
  its CSV and the zip of all PDFs are for admins (the zip writes an audit entry).
- **What it changes:** a session is matched to the car's DC charge it overlaps in time (with ten minutes of slack;
  and within 400 m where TeslaFi gives where it was), and that charge then costs what the invoice says. A cost an
  admin typed always wins over it. A session with no charge is listed, never dropped.

## The local TeslaMate input

For owners who run their own TeslaMate, the server can read it on the home network: its MQTT broker for live values,
and optionally its PostgreSQL database to catch up on what MQTT missed.

- **Off until an admin adds it.** Before that the server starts nothing for it and makes no connection. Removing it
  stops reading at once and deletes its sealed settings file; the history already stored stays.
- **Only the two hosts the admin typed**, shown in `/network` as `teslamate_mqtt` and `teslamate_db` (`host:port` as
  saved; the database row only while the catch-up is on). Nothing else is looked up or connected to for it.
- **Read only.** On MQTT the server subscribes to `<prefix>/cars/+/#` at QoS 0 and to nothing else; it never
  publishes (not even a test message), sets no last will and asks for a clean session, so the broker keeps nothing
  of it. The database is read in a `READ ONLY` transaction, only the tables and columns the TeslaMate import reads
  (never `private.*` nor any tokens table), and only the first 11 characters of a VIN.
- **No password in the clear unless the owner says so.** A password (typed now or kept) with TLS off is refused
  (`plaintext_password`) unless the admin ticks "Allow unencrypted (local network only)". Without a password, plain
  TCP is allowed. With TLS, the server is trusted by the public web roots and its name, or by the SHA-256 pin of its
  certificate (for a self-signed one). A kept password goes only to the host it was typed for: changing the host,
  port or TLS needs the password again (`password_resend`).
- **The passwords are sealed in the vault** (`teslamate-local/config.json` in the data folder). No page, API answer
  (it says only `password_set`), audit entry, error or log line shows them. A catch-up's dump is written next to it
  while it is stored and deleted right after.
- **Admins only.** Every page and call about it is for admins (reads included), and every change writes an audit
  entry. Adding, replacing, pausing or removing its settings (which hold the passwords) asks for the admin's password
  again (and the two-step code when it is on); a connection test and matching TeslaMate's cars take the admin's
  session alone, as they store and show no password. A TeslaMate car is read
  into no car here until an admin says which car it is.
- Data from TeslaMate is not signed like SMO's logs. Every value says where it came from (`teslamate_local`, "Data
  from local TeslaMate"); SMO's, the Tesla API's and TeslaFi's data win over it where they cover the same time.

## The release check

The server can tell its admins when a newer image is published (Settings → Server and access, Server updates,
"Check for new releases"). It only says so: it never pulls, downloads or installs anything; updating stays `docker compose pull`.

- **Off by default; an admin's switch.** Until it is on, the server makes no connection and no DNS lookup for it.
  Turning it off stops it and drops what it learnt.
- **When:** once at the switch (and at each start while on), then once a day, 24 hours after the last check (later
  when GitHub's rate limit says to wait: no request goes before the time GitHub gave), and when an admin presses
  "Check now".
- **Only GitHub's API host, over TLS.** `api.github.com` on port 443, fixed in the program, with rustls and the
  public web roots (TLS 1.3, or 1.2 if GitHub's edge offers only that); plain `http://` is never used, redirects are
  never followed, answers are read up to 16 MB. Through the owner's HTTP proxy only when one is set in Settings
  (`outbound.proxy`).
- **What is sent, and nothing else:** `GET /repos/skittles-fivem/Sentry-Mate/actions/workflows/image.yml/runs?branch=main&status=success&per_page=1`
  (the newest published image) and, when that is not this build,
  `GET /repos/skittles-fivem/Sentry-Mate/compare/<this build's commit>...<the newest's commit>` (what changed),
  with the headers `User-Agent: sentry-mate`, `Accept: application/vnd.github+json`, `X-GitHub-Api-Version` and
  `If-None-Match` (the ETag of the last answer). No token, no cookie, no body, and no id of the server, a car or an
  account. GitHub sees the server's (or the proxy's) address and this build's commit, which is public. A local build
  (no commit) asks nothing.
- `/network` lists it (`release_check`, `api.github.com`, on or off, last used).

## No way back without a backup

This version writes growing files in a newer format (format 2) as soon as it opens them to add to, from its first
start. Older versions can't open that format, so once it has run on a data folder, going back to an older image
needs a backup of the data folder made before the upgrade ([Backups](install.md#backups)).

## Privacy

- Logs hold the car's GPS position. Treat the data folder as private, and anything you export from it
  ([Logs](commands.md#logs)).
- Contents are sealed, names are not. A copied disk shows each car key, and each log's UTC start time (it is in the
  log's name), so it shows when each car was recording.
- The certificate (`tls/cert.pem`) is in the clear. It is public anyway.
- At the default log level (`info`) the server writes no log line per upload. With `RUST_LOG=debug`, the TLS
  library logs a few lines per connection (no names or addresses).

## When a log fails its check

Every log is kept as uploaded, whatever its check says. Each gets one of these results:

| Result | When |
|---|---|
| verified | Signed by the key the car key is paired with. The first signed log pairs it. A key trusted before keeps the logs that came while it was trusted. |
| unsigned | Not signed, and the car key isn't paired yet. |
| could not sign | Not signed, from a paired car key, and the bridge said why it couldn't sign. Not used unless the owner says so ([below](#pairing-and-the-owners-word)). |
| failed | A bad signature or sidecar, a changed file, not signed although the car key is paired, or signed by another key. Only the one that failed for being unsigned can be used on the owner's word. |
| waiting | The sidecar hasn't come. After 10 minutes the log is judged without it. |

- `log-checks --car c1` counts every log of a car as the app is told it, per `check_reason`, with when the failed
  runs were, naming no log or key ([Log checks](commands.md#log-checks)); Data health shows the same counts. `chain`
  shows the paired key's failed logs and the gaps ([The chain report](commands.md#the-chain-report)). `log decode`
  says one log's result.
- A log fails for what is wrong with it, never for an earlier log. A changed, cut, unsigned or wrongly signed log
  fails, and so does a log whose chain link contradicts the verified log before it. A log after a failed or missing
  one, whose own file and signature are fine, is verified and starts a new segment of the chain: the break shows
  once, as that log's failure or as a gap in the chain report and on Data health, and a missing log is asked of the
  car again ([Did each log arrive intact](cars-and-bridge.md#did-each-log-arrive-intact)).
- `log decode` decodes only `verified` and `unsigned` logs (and those an admin chose to use, below), unless `--any`.
- Every log without a signature says why (`unsigned_kind` of `GET /cars/{car}/logs`, and Data health): no sidecar
  came (`no_sidecar`), its sidecar has no `integrity` object, as an app from before signing writes
  (`no_integrity`), its sidecar says `signed: false` (`signed_false`), or the bridge tried and could not sign it,
  with its reason (`could_not_sign`). Records judged before the kind was kept get it when their failed logs are
  checked again by the newer rules (once, after the update); an unsigned log of a car not yet paired then that was
  never failed says `unknown`.
- A log signed by another key is marked failed. `keys` lists that key. Check it in the app, then trust it with
  `keys trust` ([A new signing key](#a-new-signing-key)): its logs are verified from then on.
- The app asks which logs arrived intact (`check` of `POST /api/can/logs/status`) and keeps a failed one. Its
  **Resend all** sends it again under its name: that copy takes the failed one's place, the failed copy is kept in
  `quarantine/` until the new one verifies, and the log's time stops being unknown once it does. A verified or
  waiting log is never replaced ([Did each log arrive intact](cars-and-bridge.md#did-each-log-arrive-intact)).
- A reinstalled bridge app or a new Screenmate has a new car key. Its uploads with the old token get
  `403 wrong_car`, so give it a new token. Its logs are kept under the new car key, which is paired by its own first
  signed log. Linking several car keys to one car is planned.

## Pairing and the owner's word

Pairing is what makes a log's data trustworthy: once a car key's first signed log has pinned its signing key, a log
without that key's signature is not used, because anyone with the car's upload token could have made it. Only the
owner can say otherwise, and only for logs that failed **for being unsigned and nothing else**:

| Kind | Sidecar | Result | Can be used on the owner's word |
|---|---|---|---|
| no sidecar | none came (judged without one after 10 minutes) | failed: not signed, but this car is paired | yes |
| no integrity | no `integrity` object (an app from before signing) | failed: not signed, but this car is paired | yes |
| signed false | `integrity.signed` is not `true`, no `error` | failed: not signed, but this car is paired | yes |
| could not sign | `integrity.signed: false` with an `error` | could not sign (the bridge's reason) | yes |
| a bad signature, another key's, another car key's, a changed or cut file, bad chain fields, a wrong name | | failed | **never** |

On Data health an admin's **Use unsigned logs** (`POST /cars/{car}/logs/use-unsigned`) asks the admin's password
(and two-step code) again, says how many logs of which kinds, and sets "use it anyway" on those logs only: a list
of names with any other log among them is refused whole (`422`, naming each and why). **Stop using**
(`DELETE` on the same route) takes it back. Each is an audit entry on the car (`can-logs-unsigned-used`,
`can-logs-unsigned-stopped`) with how many logs of which kinds; the logs are decoded (or left out) again and the
windows they are in rebuilt, so their drives appear (or go). The choice is kept with the log's record: judging it
again keeps it while the log still fails only for being unsigned, and drops it otherwise. A log in use is not
replaced by another copy sent under its name (a copy the owner never saw would take their word). The CAN Bridge app
is still told such a log failed (`check_reason: unsigned`): it is not signed.

**A log's time is no reason to use it by itself.** A log without a signature whose time is before the car key's
first signed log looks like one made before the app signed logs, but that time comes from the log's name, and
whoever holds the car's upload token chooses the name: counting such logs as usable on their own would let a
backdated log through, which is what pairing refuses. So nothing is used without the owner's word; the logs from
before the first signed log are only offered together, preselected, in the dialog ("only the N logs from before
this car's first signed log", `before_first_signed: true`), for an owner who knows their app signed nothing then.

## A new signing key

The CAN Bridge signs every log with a key kept in the Screenmate. The first signed log of a car key pairs it with
that key. A log signed by another key is kept, marked failed, and not used. That happens when the Screenmate's
keystore makes a new key, or when someone else, who has the car's upload token, uploads with it.

See the keys of a car. The car key is the last column of `token list`:

```bash
docker exec sentry-mate /sentry-mate keys <car key>
```

Each key shows as the CAN Bridge app shows it under **Log signing key**, with how many logs it signed, when its
first and last log came, and whether it is trusted. A new key says `not trusted: its logs are kept, not used`.

Then open the CAN Bridge app and look at **Log signing key**:

- **It is the new key.** Trust it. Copy the key from the list:

  ```bash
  docker exec -it sentry-mate /sentry-mate keys trust <car key> <signing key>
  ```

  It shows what will change and asks you to type `yes` and press Enter (`-it` lets you answer; `--yes` skips the
  question). Then the logs that key signed are used, and so are its later logs. The key trusted before stays trusted
  for the logs that came before; its later logs fail. It works with Sentry Mate running. Only a key that signed at
  least one of the car key's logs can be trusted.

- **It is not the new key.** Don't trust it: someone else signed those logs. Revoke the car's token, make a new one
  and put it in the app ([Tokens](commands.md#tokens), [The CAN Bridge app](cars-and-bridge.md#the-can-bridge-app)).
