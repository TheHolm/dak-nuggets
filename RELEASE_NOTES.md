# Release Notes

Very high-level history of tagged dak-nuggets releases. Each entry covers a
single collection-wide release (one date-based tag, e.g. `v2026.09.22`,
covering every program's packages built at that point - see AGENTS.md's
"Branching & releases" convention). Detail belongs in each program's own
`RELEASE_NOTES.md`; this file only ever lists which programs changed and
points there.

Each entry has a **User-facing changes** summary (what also appears in the
tagged merge commit's own description) and a **Details** section with
anything that doesn't fit a one-line pointer. `scripts/extract-release-notes.sh`
parses this structure to build each GitHub Release's body, printing from a
`## vTAG` heading up to (not including) the entry's `### Details`.

No releases yet - this file gains its first `## vYYYY.MM.DD` entry when the
first collection-wide tag is cut.

## v2026.09.25 — gnome-next-meeting 0.3.1

### User-facing changes
- `gnome-next-meeting` 0.3.1: the package synopsis and description now describe
  what the program actually does — both the `.deb` and the FreeBSD `.pkg` still
  advertised the pre-0.3.0 behaviour of printing a single `HH:MM` countdown to
  the next event.

### Details
- The packaging text now lives in one `package-metadata.sh` per program instead
  of being duplicated per packaging format, and a new metadata check fails the
  release when a program's description falls behind a feature bump. See
  `NOTES.md` and `gnome-next-meeting/RELEASE_NOTES.md`.

## v2026.09.24 — gnome-next-meeting 0.3.0

### User-facing changes
- `gnome-next-meeting` 0.3.0: instead of a single countdown to the next
  meeting, every remaining meeting of the day now gets its own line, soonest
  first — a leading space counts down to a meeting's start, a leading `-`
  counts down to the end of the meeting you are in right now, which the
  program could not report at all before. Overlapping meetings each keep their
  own line, a meeting running past midnight keeps counting down to its end,
  the same meeting subscribed in two calendars is shown once, the new
  `--lines N` option caps how many lines are printed (default 3, matching a DAK
  button LCD), and `--help` now shows the program version.

### Details
- See `gnome-next-meeting/RELEASE_NOTES.md`.

## v2026.09.23.1 — gnome-next-meeting 0.2.0

### User-facing changes
- `gnome-next-meeting` 0.2.0: the countdown output can now be decorated with
  text before and after the time via the new `--before` / `--after` options,
  including embedded newlines.

### Details
- See `gnome-next-meeting/RELEASE_NOTES.md`.

## v2026.09.23 — Initial release: gnome-next-meeting

### User-facing changes
- First release of dak-nuggets, a collection of helper programs for use with
  DAK (Dynamic Ajazz Keyboard).
- Added `gnome-next-meeting`: prints the time remaining until your next
  calendar meeting, for display on a DAK button.

### Details
- Packaged as individual `.deb` (Debian trixie, Ubuntu 26.04) and
  best-effort FreeBSD `.pkg`, plus a combined "all programs" bundle package
  per target - see `NOTES.md` and `gnome-next-meeting/RELEASE_NOTES.md`.

