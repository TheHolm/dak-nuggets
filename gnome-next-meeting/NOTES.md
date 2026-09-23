# Notes — gnome-next-meeting

Agent-to-agent knowledge base for this program. Keep it updated as you learn
more; don't let it go stale.

## Purpose

Prints `HH:MM` until the next event that starts later today, or `----` when
there is none. Intended to be run by DAK via `text_exec`.

## Dependencies / build

- Uses Evolution Data Server (EDS): `libedataserver` (`ESourceRegistry`,
  `ESource`, `e_cal_util_get_system_timezone`) and `libecal` (`ECalClient`).
- pkg-config modules in `meson.build`: `glib-2.0`, `libedataserver-1.2`,
  `libecal-2.0`.
- Builds with Meson; the directory `Makefile` is a thin wrapper so the root
  orchestrator's `make build`/`install`/`test`/`clean` work unchanged.

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

- No automated tests yet. The logic is a single `main` with one static
  callback, so meaningful coverage would require faking EDS sources. If this
  gets refactored into testable units, add tests per the repo convention.
- No test target in `meson.build`; `meson test` succeeds with zero tests.
- Only manually verified against the empty-calendar path (see above); the
  event-detection/instance-expansion logic itself has not been exercised
  against a real event.
