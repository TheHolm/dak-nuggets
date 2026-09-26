# Release Notes

Very high-level history of tagged dak-nuggets releases. Each entry covers a
single collection-wide release (one date-based tag, e.g. `v2026.09.22`,
covering every program's packages built at that point - see AGENTS.md's
"Branching & releases" convention). Detail belongs in each program's own
`RELEASE_NOTES.md`; this file only ever lists which programs changed and
points there.

A heading must repeat the tag **verbatim**, including a same-day `-N` suffix
(`## v2026.09.25-1 — …`), because `scripts/extract-release-notes.sh` looks the
entry up by exact tag string.

Each entry has a **User-facing changes** summary (what also appears in the
tagged merge commit's own description) and a **Details** section with
anything that doesn't fit a one-line pointer. `scripts/extract-release-notes.sh`
parses this structure to build each GitHub Release's body, printing from a
`## vTAG` heading up to (not including) the entry's `### Details`.

No releases yet - this file gains its first `## vYYYY.MM.DD` entry when the
first collection-wide tag is cut.

## v2026.09.26-2 — opencode-podman-status 0.2.1

### User-facing changes
- Fixed: enabling `opencode-podman-status`'s status plugin could add ~15-20 s
  to opencode's own startup. The plugin no longer waits for its own startup
  log message (a call back into opencode's own API) before letting opencode
  continue starting up. No other behaviour changes.

### Details
- See `opencode-podman-status/RELEASE_NOTES.md` (v0.2.1) and `NOTES.md` (§6b).
- `gnome-next-meeting` is unchanged and re-released at 0.3.3.

## v2026.09.26-1 — gnome-next-meeting 0.3.3

### User-facing changes
- `gnome-next-meeting` 0.3.3: its FreeBSD `.pkg` no longer declares a pile of
  runtime dependencies it does not need - installing it used to transitively
  pull in the entire WebKitGTK browser engine and GTK4 (both only used by an
  optional OAuth2 account-sign-in feature of its calendar backend that
  `gnome-next-meeting` never exercises), plus OpenLDAP and a few smaller
  unused pieces. The program itself is unchanged.

### Details
- See `gnome-next-meeting/RELEASE_NOTES.md` (v0.3.3) and `NOTES.md`.
- `opencode-podman-status` is unchanged and re-released at 0.2.0.

## v2026.09.26 — opencode-podman-status 0.2.0

### User-facing changes
- `opencode-podman-status` 0.2.0 ships a read-only opencode status plugin, so
  containers no longer need `opencode --port`. `--port` exposes opencode's full
  remote-control API, which lets anything that can reach it, including the
  agent itself, skip every human approval check. Other changes: a new `Error`
  state, password support for password-protected opencode servers, and
  security hardening of the helper.

### Details
- See `opencode-podman-status/RELEASE_NOTES.md` (v0.2.0).
- `gnome-next-meeting` is unchanged and re-released at 0.3.2.

## v2026.09.25-3 — gnome-next-meeting 0.3.2, opencode-podman-status 0.1.1

### User-facing changes
- Fixed a warning `dpkg`/`apt` printed when removing the `dak-nuggets` bundle
  package or either program's own `.deb`: `unable to remove directory
  '/usr/local' ... may be a mount point?`. `gnome-next-meeting` 0.3.2 and
  `opencode-podman-status` 0.1.1 now install their binary under `/usr/bin`,
  per Debian policy, instead of `/usr/local/bin`.

### Details
- See `gnome-next-meeting/RELEASE_NOTES.md` (v0.3.2) and
  `opencode-podman-status/RELEASE_NOTES.md` (v0.1.1).
- Repo-wide: no file is committed with the executable bit set any more
  (previously an inconsistent mix). CI's existing `chmod +x` remains the sole
  source of executability at run time; the one call site outside CI
  (`make check-metadata`) now uses an explicit `bash` invocation instead. No
  packaged output is affected by this part. See `NOTES.md`.
- Re-release of `v2026.09.25-2`, whose Debian and Ubuntu builds failed: the CI
  containers lacked `ca-certificates`, so cargo could not fetch crates for
  `opencode-podman-status`. Same program code and versions; only the CI
  install line changed.

## v2026.09.25-1 — opencode-podman-status 0.1.0

### User-facing changes
- New program `opencode-podman-status` 0.1.0 (**Linux only**): reports what
  each opencode instance running in a rootless podman container is doing, as
  three lines for a DAK button — `run: 3`, `wait:1`, `done:5` — counting
  instances that are working, waiting for an answer from you, and idle.
  `--instance N` shows one container's name, state and time in that state
  (printing nothing for a slot that does not exist), and `--list` explains any
  container it cannot reach. Each container must run `opencode --port 4096`;
  setting `server.port` in `opencode.json` does not work.

### Details
- Reaches each instance by entering its container's user and network
  namespaces, so no ports are published and no privileges are needed beyond
  being the user who started the containers. opencode's port is identified by
  socket ownership, so other servers in the container are never contacted.
- Not built for FreeBSD, where rootless podman does not exist; the top-level
  README's new Platforms column records this.
- `Cargo.lock` is now committed for Rust programs. See `NOTES.md` and
  `opencode-podman-status/RELEASE_NOTES.md`.

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

