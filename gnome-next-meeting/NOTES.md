# Notes — gnome-next-meeting

Agent-to-agent knowledge base for this program. Keep it updated as you learn
more; don't let it go stale.

## Purpose

Prints one six-character countdown per line for the rest of today's meetings
— `" HH:MM"` until a meeting starts, `"-HH:MM"` until a meeting already in
progress ends — or `----` when nothing is left. Intended to be run by DAK via
`text_exec`, whose LCD shows the first six characters of the first three
lines, which is why the default line limit is three. The block can optionally
be wrapped with `--before` / `--after` text.

## Dependencies / build

- Uses Evolution Data Server (EDS): `libedataserver` (`ESourceRegistry`,
  `ESource`, `e_cal_util_get_system_timezone`) and `libecal` (`ECalClient`).
- pkg-config modules in `meson.build`: `glib-2.0`, `libedataserver-1.2`,
  `libecal-2.0`.
- Builds with Meson; the directory `Makefile` is a thin wrapper so the root
  orchestrator's `make build`/`install`/`test`/`clean` work unchanged.
- The package synopsis/description shipped in the `.deb` and the FreeBSD
  `.pkg` are **not** written in `ci-deb.sh`/`ci-freebsd.sh`: both source
  `package-metadata.sh`, so the two formats cannot describe the program
  differently. Change behaviour, re-read that file, and bump its
  `PKG_METADATA_REVIEWED_FOR` marker - `make check-metadata` and the release
  pipeline fail on a minor/major bump until you do. See the root `NOTES.md`.
- The version lives **only** in `meson.build`'s `project(version: …)`. It
  reaches the `--help` summary through `-DGNM_VERSION`, set from
  `meson.project_version()` in the executable's `c_args`, so the help text
  cannot drift from the packaged version; `ci-deb.sh` / `ci-freebsd.sh` keep
  scraping the same line. `main.c` falls back to `"unknown"` if the define is
  missing, so it still builds outside Meson.

## Test layout

The decision/formatting logic lives in `next_meeting.c` / `next_meeting.h`
(the `NextMeeting` collection, `nm_consider()`, `nm_sort()`, `nm_format()`,
`nm_key()`, plus the `nm_expand_escapes()` / `nm_decorate()` decoration
helpers and `nm_help_summary()`), deliberately split out from `main.c` so it
can be tested with plain `time_t` values and no Evolution Data Server, D-Bus
session, or `ICalTime` objects. `main.c` keeps only the EDS glue (registry,
calendars, `i_cal_time_*` conversion), the `instance_cb` adapter that
translates an `ICalTime` instance into a call to `nm_consider()`, and the GLib
`GOptionContext` command-line parsing.

`test_next_meeting.c` uses GLib's `GTest` framework (39 cases) and covers: the
`----` marker; start and end line formatting including the space/`-` markers;
sub-minute truncation; a meeting starting exactly now counting as in progress;
a meeting that began yesterday, and one spanning the whole of today from both
sides; ignoring finished meetings and all-day events; zero-length and malformed
(end-before-start) instances; time ordering independent of arrival order; the
`--lines` limit, its clamping below 1 and its behaviour above the event count;
the tie-break of simultaneous events by `nm_key()` hash and then by
`NmEventKind`, including that insertion order does not affect it; the pinned
FNV-1a hash values and field-boundary sensitivity; multi-hour zero-padding;
ends past midnight (`-27:15`) and the `99:59` clamp; `nm_clear()` being
idempotent and re-init clearing state; a full clashing-meetings scenario;
escape expansion (`\n`, `\t`, `\\`, passthrough of unrecognised/trailing
backslashes, NULL); decoration (both/one/no sides, the `----` marker, and
wrapping a multi-line block once); and the help summary with and without a
version. Run them with `make test` (or `meson test -C build`).

### Coverage

`make coverage` configures a throwaway `build-coverage/` with
`-Db_coverage=true`, runs the suite and prints a `gcovr` line and branch report
(`gcovr` is an optional dev dependency: Debian `gcovr`, FreeBSD `devel/gcovr`).
It is always reconfigured from scratch, because stale `.gcda` counters from an
earlier run silently inflate the figures.

The report is filtered to `next_meeting.c` on purpose. `main.c` is the EDS glue
that no unit test can reach — it is verified by hand instead (see below) — so
including it would only dilute the number into meaninglessness.

As of v0.3.0 that file is at **100% line coverage (115/115)** and 92% branch
coverage (65/70). The five unhit branches are all inside GLib's
`g_string_append_c()`, which is an always-inline function whose
`G_UNLIKELY (gstring == NULL)` / `val == NULL` guards cannot be reached from
here — so every branch of this program's own logic is covered. Two branches
worth knowing about are only reachable through deliberate tests rather than
normal use: `nm_event_compare()`'s `return 0` (needs an exact duplicate event)
and `nm_format_event()`'s `remaining < 0` clamp (needs `nm->now` to be moved
forward after collection, i.e. a clock jump).

## Behaviour / quirks

- The query window is `[start of today, start of tomorrow)` in the system
  timezone. It deliberately starts at **midnight, not now**, because a meeting
  already under way still has a countdown to its end;
  `e_cal_client_generate_instances_sync()` returns every instance overlapping
  the range, so meetings which began before midnight are included too. Both
  directions of that overlap are verified live (see below): a meeting starting
  two hours before today's local midnight, and one starting three days ago, are
  both returned by the unwidened window, with their **true start reported
  unclipped** (`start=…T180047Z (-319 min)` for a window beginning 199 minutes
  ago), and both render as an end countdown. Widening the lower bound by seven
  days changes nothing, so it is deliberately left at midnight.
  `nm_consider()` discards whatever has already ended.
- A meeting with `start > now` contributes a start countdown; one with
  `start <= now < end` contributes an end countdown. Never both: the end of an
  upcoming meeting is not shown. A meeting whose end has passed contributes
  nothing. How long ago a running meeting began makes no difference — a meeting
  that started yesterday and ends in 1h45m with nothing following it prints a
  single `-01:45` line.
- Meetings which start after midnight are tomorrow's and are not shown, but an
  end countdown may legitimately exceed 24 hours, so the hours field is not
  capped at 24 — only clamped at `99:59` to keep the six-character width.
- Recurring events are expanded via `e_cal_client_generate_instances_sync`,
  and an occurrence moved out of its slot (a detached instance carrying
  RECURRENCE-ID) is honoured at its new time — both verified live.
- An event with DTSTART and no DTEND/DURATION never reaches `main.c` as a null
  end: EDS synthesises `end == start`, so the `instance_end == NULL ||
  i_cal_time_is_null_time()` guard there is purely defensive. What actually
  applies is `nm_consider()`'s zero-length handling — such an event shows its
  start countdown and then disappears once reached, rather than sticking at
  `-00:00`.
- All-day (DATE-only) events are skipped via `i_cal_time_is_date()`: a
  `HH:MM` countdown doesn't mean anything for them, and libical does not
  attach a reliable timezone to a DATE-only `ICalTime` for conversion to
  `time_t`, so they are excluded before any time math runs.
- One broken/offline calendar does not abort the run: it is skipped and the
  error cleared, so the other calendars are still checked.
- Events are sorted by time, then by `nm_key()`, then by `NmEventKind` (so a
  start precedes an end landing on the same second). Events matching in all
  three are the same occurrence counted twice — which happens when one meeting
  is subscribed in two enabled calendars, since the copies share a UID — and
  `nm_sort()` collapses them, so duplicates cannot eat the three available
  lines. De-duplication happens before the line limit is applied, and requires
  equality to the second: two copies whose start times differ at all are
  distinct events, which is the desired behaviour. Because
  identity is a 32-bit hash, two genuinely different meetings starting at the
  same second could in theory collide and be collapsed; carrying full identity
  strings on every event to rule that out is not worth it for a three-line
  display, but it is a known (astronomically unlikely) failure mode.
- `nm_key()` is a 32-bit FNV-1a over UID + recurrence discriminator + SUMMARY
  joined by a `0x1f` separator, implemented locally rather than using
  `g_str_hash()` so the ordering of simultaneous meetings is pinned to a
  documented algorithm and cannot shift with a GLib version; a unit test
  asserts known values. The recurrence discriminator is the component's
  RECURRENCE-ID when present, otherwise the instance's own start time.
  Measured live: EDS stamps a distinct RECURRENCE-ID on **every** expanded
  occurrence of a recurring event, so the first branch is what actually runs
  for recurrences and each occurrence gets a distinct key — which is precisely
  what stops de-duplication from collapsing sibling occurrences. The
  instance-start fallback runs for plain non-recurring events, where
  RECURRENCE-ID comes back as `(none)`.
- Output is `%c%02ld:%02ld`, so every countdown line is exactly six characters,
  and the no-meeting marker is four (`----`). Lines are joined with `\n` and
  the program appends a single trailing newline.
- Clashes need no marker: overlapping meetings each occupy their own line, so
  an overlap shows up as a start falling before the previous meeting's end.
- `--lines N` caps the number of lines (default `NM_DEFAULT_LINES` = 3);
  `main.c` rejects `N < 1` with exit status 1 before touching EDS, while
  `nm_format()` itself clamps a 0 to 1 so the library cannot be asked for
  nothing.
- `--before` / `--after` wrap the whole block once, not each line, and apply to
  the `----` marker too. Escapes `\n`, `\t` and `\\` in the supplied text are
  expanded by `nm_expand_escapes()`; any other backslash sequence (including a
  trailing one) is passed through unchanged. Option parsing uses GLib's
  `GOptionContext`, which also provides `--help` (carrying the version via
  `g_option_context_set_summary()`) and unknown-option handling for free.

## API gotcha: `e_cal_client_connect_sync` return type

`e_cal_client_connect_sync()` returns the generic `EClient *` base type, not
`ECalClient *`, in the EDS version packaged for Debian trixie (3.56.2) — and
per the upstream header (`libecal/e-cal-client.h`) this has been its
signature for a long time; assigning it straight to an `ECalClient *`
variable is a pointer-type mismatch (`-Wincompatible-pointer-types`, a hard
error under `-Werror`/newer GCC defaults). Store it as `EClient *` and cast
with `E_CAL_CLIENT(client)` before calling calendar-specific functions
(`e_cal_client_set_default_timezone`, `e_cal_client_generate_instances_sync`).

## Manually testing against a live EDS session

There is no display server in a typical container/CI shell, and EDS's
registry daemon is auto-launched over the **session** D-Bus bus, so running
the binary directly fails with `Cannot autolaunch D-Bus without X11
$DISPLAY`. To exercise the real code path without a full desktop:

1. Install the runtime daemon, not just the dev libs: `apt install
   evolution-data-server dbus-x11` (the dev packages alone only give you
   headers/pkg-config files — `evolution-source-registry` and friends live in
   `evolution-data-server` itself).
2. Start a session bus and export its address: `export $(dbus-launch)`.
3. Run the binary. With no calendar sources configured it correctly prints
   `----` and exits `0` — confirming the registry connection, the (empty)
   enumeration loop, and the fallback path all work.
4. Kill the bus when done: `kill $DBUS_SESSION_BUS_PID`.

### Seeding real events (works headlessly)

The earlier note here claimed a real calendar was too fragile to set up
headlessly. It is not: EDS provisions a writable local calendar ("Personal",
backed by `~/.local/share/evolution/calendar/system/calendar.ics`) on first
run of the registry, and `e_source_registry_ref_default_calendar()` returns
it. A throwaway seeding program (keep it in `/tmp`, never in the repo) can
then create events with

```c
ESource *source = e_source_registry_ref_default_calendar(registry);
EClient *client = e_cal_client_connect_sync(source, E_CAL_CLIENT_SOURCE_TYPE_EVENTS, 10, NULL, &error);
ICalComponent *comp = i_cal_component_new(I_CAL_VEVENT_COMPONENT);
i_cal_component_set_uid(comp, "…");
i_cal_component_set_summary(comp, "…");
i_cal_component_set_dtstart(comp, i_cal_time_new_from_timet_with_zone(t, FALSE, utc));
i_cal_component_set_dtend(comp, i_cal_time_new_from_timet_with_zone(t2, FALSE, utc));
e_cal_client_create_object_sync(E_CAL_CLIENT(client), comp, E_CAL_OPERATION_FLAG_NONE, &out_uid, NULL, &error);
```

Gotchas found doing this:

- Set the `is_date` argument of `i_cal_time_new_from_timet_with_zone()` to
  `TRUE` (with a NULL zone) to seed an all-day event and check it is ignored.
- `e_cal_util_get_system_timezone()` returns NULL for an `/etc/localtime`
  pointing at a zone libical cannot name, e.g. `Etc/GMT-14`, and the program
  then exits 1 with "Unable to determine system timezone". Use a real city
  zone (`Pacific/Kiritimati` for UTC+14) when shifting the clock to move
  "now" into the middle of the local day — worth doing, because with a
  container clock near 23:00 UTC most test events land in tomorrow's window.
- Let `dbus-launch`'s daemon inherit a detached stdout (redirect the whole
  block to a file and run it from a script) — if it inherits the agent
  shell's pipe, the shell blocks waiting for EOF long after the command
  finishes.
- Kill the factories (`pkill -f evolution-`) before deleting
  `calendar.ics`, otherwise the running `evolution-calendar-factory` rewrites
  it from memory.
- A recurring event needs only
  `i_cal_component_take_property(comp, i_cal_property_new_rrule(i_cal_recurrence_new_from_string("FREQ=HOURLY;COUNT=4")))`.
  Hourly is far more useful than daily here, because several occurrences then
  land inside today's window.
- To move a single occurrence, fetch the master with
  `e_cal_client_get_object_sync()`, clone it, set RECURRENCE-ID to the original
  occurrence start, remove the RRULE property from the clone, give it the new
  DTSTART/DTEND, and save with
  `e_cal_client_modify_object_sync(..., E_CAL_OBJ_MOD_THIS, ...)`.
- A **second** local calendar can be created headlessly after all, despite the
  older note in this file claiming otherwise: `e_source_new_with_uid()`,
  `e_source_set_parent(source, "local-stub")`, set the
  `E_SOURCE_EXTENSION_CALENDAR` backend name to `"local"`, then
  `e_source_registry_commit_source_sync()`. Give the registry a couple of
  seconds to settle and re-`ref` the source before connecting to it, and allow
  a generous timeout on the run which creates it — the registry is busy enough
  that a 30-second budget can expire. This is how de-duplication across
  calendars was verified.
- When seeding "the same" meeting into two calendars, compute one reference
  `now` and pass absolute times to both inserts. Calling `time(NULL)` inside
  each insert lets a second tick between them, the two copies then differ by
  one second, and they are (correctly) *not* de-duplicated — which looks like a
  bug in the program until you notice it in the harness.

### Probing what EDS actually returns for a window

To settle a question about EDS's range semantics rather than inferring it,
write a second throwaway program which computes the same window `main.c` does
and simply prints what comes back, one line per instance:

```c
e_cal_client_generate_instances_sync(cal, day_start, day_end, NULL, cb, NULL);
/* in cb: i_cal_time_as_ical_string(instance_start/end), plus the offset in
   minutes from now, and i_cal_component_get_summary() */
```

Take the lower bound's offset as an argument so a widened window can be
compared against the real one in the same run. This is how the overlap
semantics above were established: seed a meeting starting before local
midnight (or days earlier), then compare `back_days=0` against `back_days=7`
output. Shift the clock with a real city zone so local "now" lands in the
early morning, otherwise "yesterday" is not reachable — `Asia/Dubai` works for
a container clock near 23:00 UTC. Compute the seeded start from
`localtime_r()` + `mktime()` rather than hardcoding offsets, so the scenario
stays valid whatever the clock says, and refuse to seed if local midnight is
not actually in the past.

## Known gaps

- The unit tests cover the pure decision/formatting logic (`next_meeting.c`)
  only, at 100% line coverage. The EDS glue in `main.c` (registry connection,
  calendar enumeration, `i_cal_time_*` conversion, the `instance_cb` adapter)
  is not covered by automated tests, since exercising it needs a live EDS
  session — but it *has* now been verified manually end to end with real
  `VEVENT`s, using the seeding and probing recipes above. Cases confirmed: an
  empty calendar (`----`), a meeting in progress, two clashing meetings each
  keeping a line, the three-line default truncation, an all-day event being
  ignored, an already-finished event being ignored, a meeting running past
  midnight (`-27:15`), a meeting that began two hours before today's local
  midnight (`-01:44`), one that began three days ago (`-01:29`), a recurring
  event expanded into four occurrences (three shown, ` 00:29` / ` 01:29` /
  ` 02:29`), a single occurrence moved 15 minutes later via a detached
  RECURRENCE-ID instance (` 01:44` in place of ` 01:29`), an event with no
  DTEND, and the same meeting (same UID) in two separate calendars — which EDS
  duly returns twice and the program prints once, alongside an event unique to
  the second calendar, so the multi-calendar enumeration loop is exercised too.
- Not yet exercised: the "one broken/offline calendar is skipped" path, which
  needs a deliberately unreachable source (e.g. a CalDAV URL that does not
  resolve).
