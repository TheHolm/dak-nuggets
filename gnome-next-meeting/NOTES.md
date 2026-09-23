# Notes — gnome-next-meeting

Agent-to-agent knowledge base for this program. Keep it updated as you learn
more; don't let it go stale.

## Purpose

Prints `HH:MM` until the next event that starts later today, or `----` when
there is none. Intended to be run by DAK via `text_exec`. The countdown can
optionally be wrapped with `--before` / `--after` text.

## Dependencies / build

- Uses Evolution Data Server (EDS): `libedataserver` (`ESourceRegistry`,
  `ESource`, `e_cal_util_get_system_timezone`) and `libecal` (`ECalClient`).
- pkg-config modules in `meson.build`: `glib-2.0`, `libedataserver-1.2`,
  `libecal-2.0`.
- Builds with Meson; the directory `Makefile` is a thin wrapper so the root
  orchestrator's `make build`/`install`/`test`/`clean` work unchanged.

## Test layout

The decision/formatting logic lives in `next_meeting.c` / `next_meeting.h`
(the `NextMeeting` state, `nm_consider()`, `nm_format()`, plus the
`nm_expand_escapes()` / `nm_decorate()` decoration helpers), deliberately
split out from `main.c` so it can be tested with plain `time_t` values and no
Evolution Data Server, D-Bus session, or `ICalTime` objects. `main.c` keeps
only the EDS glue (registry, calendars, `i_cal_time_*` conversion), the
`instance_cb` adapter that translates an `ICalTime` instance into a call to
`nm_consider()`, and the GLib `GOptionContext` command-line parsing.

`test_next_meeting.c` uses GLib's `GTest` framework and covers: the `----`
marker, future-meeting formatting, sub-minute truncation, ignoring
already-started meetings, ignoring all-day events, nearest-wins ordering
(including that a later meeting does not displace a nearer one already
found), multi-hour zero-padding, re-init clearing state, escape expansion
(`\n`, `\t`, `\\`, passthrough of unrecognised/trailing backslashes, NULL),
and decoration (both/one/no sides, and the `----` marker). Run them with
`make test` (or `meson test -C build`).

## Behaviour / quirks

- Only events starting **later today** are considered (`start > now`), and the
  query window is `[now, start of tomorrow)` computed in the system timezone.
- Recurring events are expanded via `e_cal_client_generate_instances_sync`.
- All-day (DATE-only) events are skipped via `i_cal_time_is_date()`: a
  `HH:MM` countdown doesn't mean anything for them, and libical does not
  attach a reliable timezone to a DATE-only `ICalTime` for conversion to
  `time_t`, so they are excluded before any time math runs.
- One broken/offline calendar does not abort the run: it is skipped and the
  error cleared, so the other calendars are still checked.
- The nearest future start time wins; ties are not specially handled.
- Output is zero-padded `%02ld:%02ld`, so it is always exactly five characters
  (`HH:MM`) or four (`----`).
- `--before` / `--after` wrap the rendered time (or the `----` marker); the
  program appends a single trailing newline after the decoration. Escapes
  `\n`, `\t` and `\\` in the supplied text are expanded by
  `nm_expand_escapes()`; any other backslash sequence (including a trailing
  one) is passed through unchanged. Option parsing uses GLib's
  `GOptionContext`, which also provides `--help` and unknown-option handling
  for free.

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

Going further (creating a real local calendar `ESource` + `.ics` with a
`VEVENT` to test the instance-generation/formatting path) needs a
`local-stub` collection source that Evolution normally provisions
automatically; doing this by hand in a headless container is fragile and
not worth codifying here. If a real regression in that path is suspected,
test interactively on a machine with a running desktop session instead.

## Known gaps

- The unit tests cover the pure decision/formatting logic (`next_meeting.c`)
  only. The EDS glue in `main.c` (registry connection, calendar enumeration,
  `i_cal_time_*` conversion, the `instance_cb` adapter) is not covered by
  automated tests, since exercising it needs a live EDS session.
- Only the empty-calendar path has been manually verified end to end (see
  above); the event-instance/expansion path has not been exercised against a
  real `VEVENT`.
