# Rebuilds: what starts them, and why

[← Back to the README](../README.md)

- [What starts a rebuild](#what-starts-a-rebuild)
- [The rules every rebuild follows](#the-rules-every-rebuild-follows)
- [Version changes](#version-changes)
- [Adding a version change](#adding-a-version-change)

The server keeps the raw inputs (CAN logs, their signatures, the APIs' samples, imported rows) and works out the
rest from them: drives, charges, states, visits, positions, battery and efficiency figures. When the way it works
something out changes, or a setting it depends on does, what was worked out is made again. This page lists every
thing that does that, how far each one reaches, and the server updates that changed how logs are read.

## What starts a rebuild

| Trigger | What is worked out again | When it runs | Can it fire again needlessly? |
|---|---|---|---|
| **A server update that changed the decoding** (`decode-N`, `rules-N` or the DBC files: see [below](#version-changes)) | A **full rebuild** of the cars whose history was made with the old decoding, beside the history in use | Planned once per version at the first look after the start, then waits a grace of 10 minutes (Settings → Server: **Start now**, **Tonight** or **Pause**) | No: planned once per version (`engine` / `full-auto`); a car's new version is noted only when its switch is done; a plain restart plans nothing; a restart in the middle carries the same run on; an update during a run takes the new cars into that run; one that failed is offered by a notice, not retried |
| **An update that changed the CAN log check** (`trust-N`) | Only the logs that **failed** their check under the older rules are checked again, per car key; those whose verdict changes are decoded again | Once per key, two minutes after the start | No: each key's new rules version is written with its results, all at once |
| **An admin's full rebuild** (Data health, Settings → Server) | A full rebuild of the car or of every car | At once | No: an ask while one runs joins that run (its cars added) |
| **An admin's rebuild of a car or a period** (`POST /cars/{car}/rebuild`) | That car's drives, charges, states and visits from its logs | At once (refused while a full rebuild runs) | No: one job per car at a time; asks meanwhile join one job after it |
| **The car's GPS layout** (`PUT /cars/{car}/gps-layout`) | **Positions only**: the logs whose positions came from the Screenmate's own GPS frames are decoded again; the others read the same whatever the layout and are left | After a 3 s settle | No: saves in a row are one job; noted on the car first, so a restart before it ran does it then |
| **The car's source order** (Settings → the car → Data sources) | **The inputs joined again**: every window built again from what is stored, nothing decoded again | After a 3 s settle | No: saves in a row are one job, the same as above |
| **The preferred CAN key of a car** | The car's history from its logs (each second's source follows the key) | After a 3 s settle | No: saves in a row are one job |
| **A CAN key given to another car** | The logs of that key are worked on for their new car; the old car's windows they were in are built again | At once | No: only that key's logs |
| **Imported rows moved between cars** (an import's lots placed, the start-up re-homing of an older import) | The cars the rows moved to or from | After the move | No: the start-up re-homing runs once per re-homing version |
| **A log that arrives late, or is judged again** | The window of the timeline the log changes (back and forward to the nearest settled point) | As it arrives | No |
| **Start-up recovery** | Logs told of while the server was down, windows a crash left dirty, a rebuild a restart cut off (carried on, not started over), a narrow rebuild asked for and not done | At the start | No: logs recorded the same way are never decoded again |
| **Place changes** (a place moved, renamed, a price changed) | Nothing: places are matched when read; a charge keeps its cost once it ended, a price reaches past charges only through **Reprice** | — | — |
| **Gap filling, time zone, the car's other settings** | Nothing: worked out when read | — | — |
| **The missing-log scan** | Nothing: the cars are asked to send the logs again; each one that comes is decoded as it arrives | A while after the start, then once a day | — |
| **Compaction** | Nothing: values are packed as they are, never changed | When idle | — |
| **The background passes** (the long periods' day parts, the pack's day readings) | Only what is not kept yet (each day's part kept is read, not made again) | Once idle after the start, then now and then | No |

## The rules every rebuild follows

- **At most once per version.** A version-triggered full rebuild is planned once per version. The version a car's
  history was made with is written when its switch is done; a run cut off by a restart carries on where it was.
- **Never stacked.** One full rebuild at a time: an admin's ask while one runs, or a new version found while one runs,
  joins it. One rebuild job per car at a time: asks for a car whose job has not started join it; asks while it runs
  are one job after it.
- **Settled.** A rebuild a saved setting asks for waits 3 s (each later save moves that on), so a few saves in a row
  are one rebuild.
- **Narrow.** A change that only touches one thing rebuilds only that, and only for the car it is about: a GPS layout
  change decodes again only the logs whose positions it changes; a source order change joins the inputs again
  without decoding. Each narrow rebuild is proven to give the history a whole rebuild of the car gives
  (`crates/engine/tests/tesla_input.rs`: `a_gps_layout_change_rebuilds_positions_only`,
  `a_source_order_change_joins_the_inputs_again_as_a_whole_rebuild_would`), except that a drive keeps the id it had.
- **Ids kept.** A drive or charge made again keeps the id it has in the history in use (the ids come from a row's
  start when it is first made and stay when a late log moves the start), so what points at it stays with it: a
  cost typed or kept, a drive's purpose and notes. A full rebuild, which makes the history from scratch beside the
  one in use, gives its rows those ids at the switch.
- **Said why.** Every rebuild carries its causes: the full rebuild's status and its notice name them (for a server
  update with a link to the change below), and Data health shows the last rebuilds with their cause and how long
  they took.
- **Put off when you choose.** A new version's full rebuild starts after its grace by itself, unless an admin starts
  it now, puts it off until tonight (02:00, the server's time zone) or pauses it (its progress kept); any full
  rebuild can also be cancelled.

## When a stored file can't be read

A rebuild makes the history again from what is stored, so a stored file that no longer opens (a disk fault, a copy
cut short) is where it can lose something. It never does so quietly:

- **A stored CAN log that can't be read** (its file, or the day pack it is in, is damaged). A **full rebuild** whose
  car has such a log that the history in use was made from **fails and keeps the old history**: that one still has
  what the log held, a new one would not. Its error names the log; put the file back from a backup and start it
  again. A log that could not be read before either (the history in use never had it) is listed with the rebuild's
  errors and left out. A **rebuild of a car or a period** clears first, so it can only say so: it ends `failed`
  with how many logs it could not read (`crates/engine/tests/pipeline.rs`:
  `a_full_rebuild_keeps_the_old_history_when_a_stored_log_cant_be_read`,
  `a_rebuild_that_cant_read_a_stored_log_ends_failed_and_says_so`).
- **A damaged day of decoded samples** (`db/cars/<car>/seg/samples.can` or `charge.can`: a day's file or a
  month's pack). Everything in it is made again from the stored logs, so a rebuild that meets it sets the file aside
  (`<name>.damaged` beside it), widens its window over the time the file covered and decodes the logs of that time
  again, where it used to clear the car's drives and charges and then stop at the file
  (`a_rebuild_mends_a_damaged_day_of_decoded_samples`). A full rebuild never reads those files: it makes them all.
- **A damaged table of a car** (`drives.snap` and the like), or a damaged series of an import or an API: these
  also hold what no log gives (imported rows, typed costs and purposes), so no rebuild replaces them by itself. A
  rebuild or a full rebuild that meets one fails and names the file, and the admins get the notice `data_damaged`
  with what to do: put the car's folder back from a backup with the server stopped (the logs that came since are
  decoded again at the start). This is how it stays: no rebuild and no full rebuild starts such a table empty,
  because that would lose its imported rows and typed values without a word. The steps are in
  [A car's history file is damaged](install.md#a-cars-history-file-is-damaged).
- **A rebuild that fails is said**: `500 rebuild_failed` with why, an audit entry that says it failed, and the
  notice `rebuild_failed` (`crates/web/tests/system.rs`).
- **A full rebuild carried on after a restart** goes on with the logs left, counts the car's logs from the start
  and rebuilds its windows once per run of logs as before the restart, to the very history an uninterrupted one
  gives (`crates/sentry-mate/src/window_batch.rs`:
  `a_full_rebuild_carried_on_after_a_restart_gives_the_same_timeline_per_run_of_logs`).
- **A rebuild of a period** rebuilds its windows once per run of logs too where no drive or charge goes on across
  the period's end, to the very history a window per log gives, ids included
  (`a_rebuild_of_a_period_gives_the_same_timeline_per_run_of_logs`). A period whose end cuts a drive or a charge
  gets a window per log: the part after the end is kept, and per run the whole drive or charge could keep the id of
  that part instead of its start's.

## Version changes

A version is bumped only when what is stored would really come out otherwise, and each bump names the test that
shows the output changing (`engine::versions::BUMPS`; the test `versions_bumped_have_a_proof` fails otherwise).

<a id="decode-2"></a>
### decode-2: a failed log's hole is a few minutes

A log that failed its check leaves a hole (unknown, never asleep) of a few minutes only around its name's start,
whatever its unsigned sidecar claims. Before, a sidecar could make a long stretch unknown.
Proof: `failed_logs_are_unknown_for_their_own_minutes_whatever_their_sidecar_claims`.

<a id="decode-3"></a>
### decode-3: firmware fingerprints

Each log's firmware fingerprints are kept, and software updates are found from them. Older logs are read again for
them. Proof: `a_flapping_install_is_one_update_and_stored_ones_regroup_with_their_name`.

<a id="rules-2"></a>
### rules-2: positions are worked out differently

A fix is one input's latitude and longitude, and fixes that cannot be real (a placeholder, or a jump faster than a
car drives from the fixes around it) are left out, so no drive starts or ends where the car never was.
Proof: `bad_fixes_at_the_start_or_end_and_placeholders_are_left_out`.

<a id="rules-3"></a>
### rules-3: charging efficiency over one span

A charge's energy added and grid energy are measured over one span, or the charge says they are not (its
efficiency is then not shown). Proof: `a_charge_whose_energies_cover_different_spans_shows_no_efficiency_and_says_why`.

<a id="rules-4"></a>
### rules-4: energy and distance are summed differently

A charge's grid energy counts every phase in use. A reading counts in an energy sum until the next second that has
it (an API's for 150 s at most), so readings far apart leave no energy out. A drive whose odometer was first read
late or last read early has the GPS distance of the rest. Proof: `grid_energy_counts_every_phase`.

<a id="rules-5"></a>
### rules-5: drives and charges are worked out differently

A drive an API read (readings seconds to minutes apart) runs up to the first reading that sees the car parked, and
ends at that reading's odometer and position: the stretch after its last reading in D is no longer left out. A last
odometer reading further from the first than the car can have driven in the time between them (one bad value) is
passed over for the one before it. With no odometer, a GPS step that ends where the car stands is left out only when
it is short (5 s at most) or starts standing too, so fixes far apart lose no distance before a stop. Charging shorter
than a minute that added less than 0.05 kWh (a scheduled charge that starts and stops at plug-in, a failed
handshake) is no charge. Proof: `api_only_drive_keeps_its_last_leg`.

<a id="trust-2"></a>
### trust-2: a chain break fails only the log that breaks it

A break in a car key's chain of logs fails only the log that breaks it, not those after it. The logs that failed
under the older rules are checked again once. Proof: `a_chain_with_one_bad_log_fails_that_log_alone`.

## Adding a version change

1. Write the test that shows the stored output changing (it fails on the old rules, passes on the new).
2. Bump the version (`crates/engine/src/records.rs` `DECODE_VERSION`, `crates/timeline/src/timeline.rs`
   `RULES_VERSION`, or `crates/trust/src/lib.rs` `RULES_VERSION`).
3. Add a `Bump` to `crates/engine/src/versions.rs` naming that test, and a heading here with the anchor
   `<part>-<version>`.

`cargo test -p engine versions` fails until all three are there.
