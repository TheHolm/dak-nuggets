# Release notes — gnome-next-meeting

## v0.3.1

- The package synopsis and description now describe what the program actually
  does. Both the `.deb` and the FreeBSD `.pkg` still advertised the 0.2
  behaviour ("Prints HH:MM until the next calendar event of the day"), which
  has been wrong since 0.3.0 introduced a countdown per meeting and the
  countdown to the end of the meeting in progress.
- Low-level: the synopsis/description moved out of `ci-deb.sh` and
  `ci-freebsd.sh` into a single `package-metadata.sh` that both source, so the
  two packaging formats can no longer drift apart. It also carries a
  `PKG_METADATA_REVIEWED_FOR` marker; the new
  `scripts/check-package-metadata.sh` fails the build when a program's
  `major.minor` moves past the version its description was last reviewed
  against, and also checks Debian synopsis hygiene. It runs from both target
  orchestrators and from the new root `make check-metadata`.

## v0.3.0

- The output is no longer a single countdown to the next meeting: every
  remaining meeting of the day now gets its own six-character line, soonest
  first. A leading space means "time until this meeting starts"; a leading
  `-` means "time until the meeting you are in right now ends", which the
  program previously could not report at all.
- Overlapping ("clashing") meetings are handled: each keeps its own line, so a
  meeting starting before the current one ends is plainly visible.
- The same meeting subscribed in more than one calendar is now shown once
  instead of twice, so duplicates no longer use up the few lines available.
- A meeting under way keeps counting down to its end even when that end falls
  after midnight, so the hours field can exceed 24 (for example `-27:15`).
- New `--lines N` (`-l`) option caps how many countdown lines are printed,
  defaulting to 3 — exactly what a DAK button LCD displays. `N` below 1 is
  rejected.
- `--help` now shows the program version.
- `--before` / `--after` still work and now wrap the whole block of countdowns
  once, rather than each line, and still apply to the `----` marker.
- Countdowns falling at the same second keep a stable order between runs.
- Low-level: `NextMeeting` became a collection of `NmEvent {kind, when, key}`
  records in a `GArray` instead of a single start time; `nm_consider()` takes
  the instance's end time and identity hash as well; new `nm_clear()`,
  `nm_key()`, `nm_sort()` (which also collapses exact duplicates, before the
  line limit is applied) and `nm_help_summary()`; `nm_format()` /
  `nm_decorate()` take a line limit. `main.c` stops discarding
  `instance_end`, derives the identity hash from UID + RECURRENCE-ID (or the
  instance start) + SUMMARY via a locally implemented 32-bit FNV-1a, and moves
  the EDS query's lower bound from `now` back to midnight so meetings already
  under way are returned. The version reaches `--help` via `-DGNM_VERSION`,
  injected from `meson.project_version()`.
- Low-level: new `make coverage` target (root and program level) reporting
  `gcovr` line/branch coverage of the unit-tested logic layer; `gcovr` is an
  optional dev dependency and nothing in CI or packaging depends on it. The
  unit suite grew from 16 to 39 cases and `next_meeting.c` is at 100% line
  coverage, with every branch of this program's own logic exercised.
- Low-level: the EDS path was additionally verified by hand against real
  `VEVENT`s (in-progress, clashing, all-day, finished, overnight, begun
  yesterday, begun three days ago, recurring, a moved occurrence, and one with
  no DTEND) — confirming that `e_cal_client_generate_instances_sync()` returns
  instances overlapping the window with their true start unclipped (so the
  lower bound can stay at midnight), and that EDS stamps a distinct
  RECURRENCE-ID on every expanded occurrence, which is what keeps sibling
  occurrences from being de-duplicated. See `NOTES.md` for the headless
  seeding and probing recipes.

## v0.2.0

- New `--before` / `--after` options (short `-b` / `-a`) wrap the countdown
  output with arbitrary text, and `\n`, `\t` and `\\` in that text are
  expanded, so newlines can be added around the time. The decoration also
  applies to the `----` no-meeting marker.
- `--help` now prints a usage summary of the available options.
- Low-level: option parsing uses GLib's `GOptionContext`; the escape
  expansion and decoration logic lives in the testable `next_meeting.c`
  (`nm_expand_escapes()`, `nm_decorate()`) and is covered by new unit tests.

## v0.1.0

- Initial version: prints the time remaining until the next calendar event
  of the day (`HH:MM`, or `----` when none remain) by reading the enabled
  calendars from Evolution Data Server.
- Fixed a build error against current EDS (`e_cal_client_connect_sync`
  returns `EClient *`, not `ECalClient *`; needs an `E_CAL_CLIENT()` cast)
  and removed an unused variable, so the program now compiles cleanly with
  Meson and runs successfully against a live EDS session.
- All-day events are now ignored when computing the next meeting: a `HH:MM`
  countdown doesn't apply to them, and their DATE-only value has no reliable
  timezone to convert from.
