# AGENTS.md

## Repository visibility

**This repository is pushed to a public GitHub repo.** Before committing,
pushing, or writing anything into a tracked file (code, docs, `NOTES.md`,
scripts, test fixtures, commit messages), make sure it contains no sensitive
or identifying information: no real hostnames/IPs, credentials/passwords/API
keys/private keys, serial numbers of specific physical devices, personal
file paths, or other details tied to a particular person's or machine's
identity. Genuinely throwaway material (e.g. ad hoc test scripts/VM
connection details written for a single session) belongs outside the repo
entirely (e.g. under `/tmp`), never committed - see the "Never commit
changes unless the user explicitly asks to commit" rule below, which exists
partly for this reason.

## Project overview

**dak-nuggets** is a collection of small, independent helper programs used
alongside [DAK](https://github.com/TheHolm/dak) (**D**ynamic **A**jazz
**K**eyboard), the Rust tool for controlling Ajazz/Mirabox USB macro keypads.
DAK drives its runtime behaviour from `config.json`; helpers plug into it by
printing text or images to stdout (consumed by DAK's `text_exec` /
`image_exec` setup types), by being launched detached via `launch`, or by
being called inside `$(command)` variable substitutions.

Each helper is a self-contained program living in its own subdirectory, with
its own language, build files, README, release notes and notes. The repo is
not a single binary or library: there is deliberately no shared runtime or
common dependency between the programs, so they can each use whatever
language and tooling fits the job.

## Stack

Languages are chosen per program; there is no repo-wide language mandate. In
practice:

- C with GLib / Evolution Data Server (`libecal` / `libedataserver`) for
  programs that need calendar data (`gnome-next-meeting`)
- Rust, Go, shell, or anything else where it fits a future helper

## Target platforms

Linux and FreeBSD, mirroring DAK's own target platforms. Helpers are CLI
tools with no GUI. Code should compile and run on both platforms unless a
program is explicitly documented otherwise.

Where a program *is* documented otherwise, its platform support is declared in
its own `README.markdown` and summarised in the **Platforms** column of the
top-level `README.markdown` table, so the exception is visible without opening
anything. Such a program simply omits the `ci-*.sh` for the target it does not
support (that is what excludes it from packaging) and guards its own `Makefile`
targets with `uname -s` so a collection-wide `make` still succeeds everywhere.
`opencode-podman-status` is Linux-only on this basis — see `NOTES.md`.

## Layout

One subdirectory per helper program. Each program directory is self-contained
and carries, at minimum:

- `README.markdown` — what the program does, its CLI/output, and how it
  integrates with DAK's `config.json`
- `RELEASE_NOTES.md` — the program's own change history (the top-level
  `RELEASE_NOTES.md` only ever lists high-level, cross-program changes)
- `NOTES.md` — agent-to-agent knowledge base for that program's build quirks,
  dependencies, and non-obvious behaviour; keep it updated, don't let it go
  stale
- `package-metadata.sh` — the synopsis/description shipped in every package
  format, in one place (sourced by the program's `ci-*.sh`), plus the
  `PKG_METADATA_REVIEWED_FOR` marker that keeps it from going stale
- its own build files (Meson for C/GLib, `Cargo.toml` for Rust, etc.)

Top level:

- `README.markdown` — general overview of the collection and one section per
  included program
- `RELEASE_NOTES.md` — very high-level notes about what changed; details go
  in each program's own `RELEASE_NOTES.md`
- `NOTES.md` — agent-to-agent knowledge base for cross-program concerns: the
  release pipeline, the packaging scripts under `scripts/`, and the checklist
  for wiring a new program into both; keep it updated, don't let it go stale
- `Makefile` — thin POSIX-compatible orchestrator (see Build below)
- `scripts/` — packaging and release tooling shared by every program (see
  Releases & packaging below); nothing here is specific to one program
- `.woodpecker/` — CI: `release.yaml` (tag-triggered packaging/publishing) and
  `check-target-freshness.yaml` (monthly staleness check of the pinned OS
  versions)
- `AGENTS.md` — this file
- `LICENSE` — GNU Affero GPL v3 or later (`AGPL-3.0-or-later`)

## Build

There is no single build system. Each program builds with its own native
tooling; the top-level `Makefile` is a thin POSIX-compatible orchestrator
that recurses into each program directory and dispatches to whatever that
program uses. It is deliberately free of GNU-isms so it runs under both BSD
make (FreeBSD's default `make`) and GNU make (Linux).

Top-level targets:

- `make build` — build every helper
- `make test` — run every helper's tests
- `make install` — install every helper (respects `PREFIX`/`DESTDIR`)
- `make clean` — clean build artifacts for every helper

When adding a new program, add a corresponding hook to the root `Makefile`
and document its build in the program's own `README.markdown`.

## Commands

There are no repo-wide build or test commands; use the root `make` targets
above, or build an individual program from its own directory. Each program's
`README.markdown` documents its specific commands and dependencies.

## Releases & packaging

Every program is versioned independently (see Conventions), but releases are
collection-wide: a single date-based tag (e.g. `v2026.09.22`, or
`v2026.09.22-1` for a second release on the same day) triggers
`.woodpecker/release.yaml`, which rebuilds and republishes **every**
program's packages at their current versions, plus one additional "bundle"
package per target (Debian trixie, Ubuntu LTS, FreeBSD) containing every
program together - not just a metapackage depending on the others, an
actual combined package.

The pipeline has exactly one build step per target platform (not per
program): each sets up its target's environment once, then builds every
program and the bundle. A program opts into a target by providing a
`ci-deb.sh` (and/or `ci-freebsd.sh`) of its own; the target orchestrators
under `scripts/` discover programs by those files, so adding a program needs
no change to the pipeline's step list. Individual and bundle packages are
built with the generic scripts in `scripts/` (`build-deb.sh`,
`build-freebsd-pkg.py`, `merge-stage-roots.sh`) rather than a
language-specific tool like `cargo-deb`, since packages here can come from
any language.

FreeBSD packaging never uses a real FreeBSD host: the base system comes from
selectively extracting the official `base.txz` release set, and each
program's own native dependency closure is resolved live against the real
FreeBSD package repository (`scripts/fetch-freebsd-deps.py`). That live
repository layout is not a stable public interface, so the FreeBSD step is
best-effort (`failure: ignore` in CI) - a break there never blocks the Linux
releases. See `NOTES.md` for the low-level detail (sysroot gotchas, adding a
new program, the shared-CI-workspace constraints, etc.).

## Conventions

- Each program follows its own language's standard formatter and idioms
  (`clang-format` or GNU style for C, `cargo fmt` for Rust, `gofmt` for Go,
  `shellcheck` for shell, etc.) — there is no single repo-wide formatter
- Every function and every test is documented with a `///`-style doc comment
  (the language's equivalent) describing its purpose and any non-obvious
  behaviour
- All new code must be covered by tests where the language/tooling supports
  it; never add production code without accompanying tests
- Commits include a detailed description of what changed and why
- Any change that can affect a built package (source, build files, packaging
  scripts) must be made on a branch, never committed directly to master.
  Master accepts direct commits only for changes that cannot affect any
  package: documentation, `NOTES.md`, tests-only changes, or CI/tooling
  changes that don't alter packaged output
- When merging a branch to master that will **not** be tagged as a release,
  summarise all changes in the code since the branch started (or the last
  merge to master) and use that summary as the merge description
- When merging a branch to master that **will** be tagged as a release, the
  merge commit description contains only user-affecting changes (new
  features, bug fixes, changed behavior) — no low-level implementation
  detail. Add a new entry to the affected program's own `RELEASE_NOTES.md`
  with that same user-facing summary plus all the low-level detail that
  would otherwise have gone in the merge commit description, and add a
  corresponding high-level line to the top-level `RELEASE_NOTES.md`
- A release is exactly a branch that bumped at least one program's version,
  merged to master; branches merged without any version bump (docs, tests,
  non-packaging tooling) are never tagged. Tag format is a single date-based
  tag for the whole collection, `vYYYY.MM.DD`. When more than one release is
  cut on the same day, suffix the second and subsequent ones `-1`, `-2`, … —
  e.g. `v2026.09.25`, then `v2026.09.25-1`, then `v2026.09.25-2`. The first
  release of a day carries no suffix. (`v2026.09.23.1` predates this
  convention; don't copy its dotted style.) The suffix never reaches a package
  version: the release scripts fold the hyphen into the date, so
  `v2026.09.25-1` packages as `2026.09.25.1` — see `NOTES.md`
- Each program version follows `X.Y.Z`: `X` (major) only when explicitly
  asked for; `Y` (minor) for a new feature; `Z` (patch) for bugfixes and
  other changes that don't add, remove, or change functionality. Release
  notes live in the program's own `RELEASE_NOTES.md`
- A minor or major bump means behaviour changed, so it also requires re-reading
  the things that *describe* that behaviour to users: the program's
  `package-metadata.sh` (the synopsis/description shipped in every package) and
  the opening of its `README.markdown`. Record the review by setting
  `PKG_METADATA_REVIEWED_FOR` to the new `major.minor`; `make check-metadata`
  and the release pipeline both fail until you do
- When starting work on each new branch, ask the user whether to bump a
  version number (and if so, to what value) before writing any code
- Never commit changes unless the user explicitly asks to commit
- Never create or push a git tag unless the user explicitly asks - same rule
  as commits, and for the same reason: a tag triggers a real release build
  and publish
