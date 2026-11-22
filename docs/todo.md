# To do

What is being built now, what comes next, and the open questions. What already works: [Progress](status.md). Back to
the [README](../README.md).

## In progress now

- **Every design shows the Tesla API and where data came from.** A Tesla account page in Settings: Integrations (sign
  in on Tesla's page or paste a refresh token, match cars, pause, remove) and "Data from SMO / the Tesla API /
  Imported from TeslaMate" on every page. The server side is done; until the pages have it, a Tesla account can't be
  added from the web UI.
- **Horizon, Console and Pebble catch up:** the certificate SHA-256 with Copy and "Make a new certificate", the
  Sentry Six Manager CAN export import, and the weak-password warning (Instrument and Logbook have them).
- **Logbook pixel clean** at laptop, 4K and phone sizes (Instrument is).
- **The Import page in each design shows TeslaFi.** The server takes TeslaFi's data download (`kind: "teslafi"`:
  its report, the time zone to confirm and the units to Start, `docs/ui-api.md`, "TeslaFi's data") and the dev
  server serves it with a made-up file (`ui/devserver/mockapi/data/TeslaFi32023.csv`); a later step does the pages:
  the time zone and units on the check's page, the report's rows, drives and charges, and "Imported from TeslaFi".
- **CI in about 5 minutes** per push to main (tests in parallel shards, a build cache), down from 30-45.

## Next

1. **Car firmware panel and update marker in each design:** the "Car firmware" panel (each module's fingerprints and
   when they were first seen, from `/cars/{car}/firmware`), and the "Tesla update installed" marker on the timeline
   and the Software page with the changed modules and a box to type the version (the server side is done).
2. Vehicle info on each design's car page (the server's `vehicle` facts with their sources, disagreements and unknown
   codes), and check the gateway configuration (`0x7FF`) and the LFP brick count on a real log (until then the brick count is not read for the chemistry).
3. Check the timeline by hand against real drives and charges.
4. Every page in all five designs, checked item by item against TeslaMate, on desktop and phone.
5. A live connection to TeslaMate's database for the import.
6. First release: the docs, a licence and the final name.

Open questions: two battery % signals disagree (0x33A and 0x292) and need checking against the car's screen.

Answered: Tesla's version number (2025.x) is not on CAN, but the modules' firmware fingerprints are (CRCs and git
hashes in 0x300, 0x301, 0x335 and 0x656 on the vehicle bus): the server decodes them and finds the updates from them
(done, server side; the version is what the owner types, and names later updates to the same fingerprints by itself).
Still to check on a real car: which fields change at a real update.

## Not built yet

| Part | State |
|---|---|
| A command that lists stored logs (a car's logs and their check results are in the web UI), deleting logs, a backup command | not built |
| Passkeys | planned |
| `cargo deny` and `cargo audit`, fuzzing in CI | planned |
| Sealing the key file to the host's TPM | optional, later |
