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

1. Check the timeline by hand against real drives and charges.
2. Vehicle info on each design's car page (the server's `vehicle` facts with their sources, disagreements and unknown
   codes), and check the gateway configuration (`0x7FF`) and the LFP brick count on a real log (until then the brick count is not read for the chemistry).
3. Every page in all five designs, checked item by item against TeslaMate, on desktop and phone.
4. A live connection to TeslaMate's database for the import.
5. First release: the docs, a licence and the final name.

Open questions: two battery % signals disagree (0x33A and 0x292) and need checking against the car's screen. A
firmware-version signal for software updates is still to be found.

## Not built yet

| Part | State |
|---|---|
| A command that lists stored logs (a car's logs and their check results are in the web UI), deleting logs, a backup command | not built |
| Passkeys | planned |
| `cargo deny` and `cargo audit`, fuzzing in CI | planned |
| Sealing the key file to the host's TPM | optional, later |
