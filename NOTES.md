# Notes

Agent-to-agent knowledge base for dak-nuggets' cross-program CI and
packaging. Keep it updated as you learn more; don't let it go stale. Per-
program build quirks belong in that program's own `NOTES.md` instead - this
file is only for things that span the whole repo (the release pipeline, the
packaging scripts under `scripts/`, and the checklist for adding a new
program to both).

## Release model

One date-based tag (e.g. `v2026.09.22`) triggers `.woodpecker/release.yaml`,
which rebuilds and republishes **every** program's packages at their current
versions, plus one "bundle" package per target containing every program
together. Tags are always created manually, never by CI - see AGENTS.md's
"Branching & releases" convention for when to cut one (a branch that bumped
at least one program's version, merged to master).

## Packaging scripts (`scripts/`)

- `build-deb.sh` - language-agnostic `.deb` builder. Takes a staged install
  tree (whatever a program's `make install DESTDIR=<dir>` produces) plus
  metadata flags, writes a `DEBIAN/control`, runs `dpkg-deb --build`. Exists
  because `cargo-deb` (what TheHolm/dak and TheHolm/md_timesheet use) is
  Rust-only, and dak-nuggets is multi-language.
- `merge-stage-roots.sh` - combines several programs' staged trees into one,
  for building the bundle package. Errors loudly on a path collision between
  two programs rather than letting one silently overwrite the other.
- `build-freebsd-pkg.py` - writes a FreeBSD `.pkg` (zstd tar + JSON manifest)
  directly from a staged tree, no `pkg` binary or FreeBSD host needed. Ported
  near-verbatim from TheHolm/dak / TheHolm/md_timesheet (already fully
  generic); every metadata flag is required here rather than defaulted to one
  product, since this script packages more than one program. See
  TheHolm/dak's `NOTES.md` section 2 for the on-disk format itself.
- `fetch-freebsd-deps.py` - populates a FreeBSD cross-compilation sysroot
  with a program's *native* dependency closure (e.g. `evolution-data-server`
  for gnome-next-meeting), resolved live against `pkg.freebsd.org`. Ported
  from TheHolm/md_timesheet's `fetch-freebsd-gtk.py`, generalized to take
  `--root`/`--pkg-config-module`/`--pkg-config-check` instead of hardcoding
  gtk4/libadwaita. This is on top of, not instead of, the base-system sysroot
  pieces (libc/CRT/headers) extracted from `base.txz` - see below.
- `extract-release-notes.sh`, `check-target-freshness.sh` - reused verbatim
  from TheHolm/dak; see that repo's own `NOTES.md`/`AGENTS.md` for background.

## FreeBSD cross-compilation: base sysroot gotchas beyond TheHolm/dak's list

TheHolm/dak's `NOTES.md` section 1.4 documents the base `base.txz` file list
for a **Rust** binary. dak-nuggets also has C programs, which hit two things
Rust never does - both cost real debugging time to find, so they're recorded
here in full:

1. **`usr/include` (the whole tree, ~34 MB extracted).** A C source directly
   `#include`s libc headers (`stdio.h`, `time.h`, ...); Rust never touches
   these at all (`libc`-crate bindings are self-contained). Forgetting this
   fails immediately and obviously (`fatal error: 'stdio.h' file not found`),
   but it's easy to not think of if you're starting from a Rust-shaped file
   list.
2. **`usr/lib/libgcc.a`, `usr/lib/libgcc_eh.a`, `usr/lib/libcompiler_rt.a`.**
   A plain `clang ... -o prog` link line for the FreeBSD target
   unconditionally appends `-lgcc --as-needed -lgcc_s --no-as-needed` to the
   linker invocation - this is clang's own hardcoded FreeBSD driver spec, not
   anything project-specific, and `-rtlib=compiler-rt` does **not** suppress
   it for this target (confirmed: the flag is silently accepted but the
   `-lgcc` still gets emitted). Rust never triggers this because rustc
   controls its own link line and never asks clang for `-lgcc` in the first
   place (its `compiler_builtins` crate is statically linked into every
   binary instead). The fix is exactly what a real FreeBSD system has:
   `base.txz` ships `usr/lib/libgcc.a` as a symlink to `usr/lib/
   libcompiler_rt.a` (FreeBSD's own base system is built with clang/compiler-rt,
   not GCC, despite the traditional name) - extract both, in that order
   doesn't matter but both must be present or the symlink dangles and `ar`/
   the linker can't read through it.

   Do **not** substitute the host's own `libclang_rt.builtins*.a` (e.g. from
   `/usr/lib/llvm-19/lib/clang/19/lib/linux/...`) for this - it's built for
   the *host* target (Linux) and the fact that it happens to satisfy the
   linker for simple cases is not something to rely on; always take the real
   `libcompiler_rt.a` for the *target* (FreeBSD) from `base.txz` itself.

Both were found and fixed by actually running the full cross-compile and
`.pkg`-build pipeline end to end (network permitting - `pkg.freebsd.org` and
`download.freebsd.org` reachable), not by inspection alone; the resulting
binary was verified as genuine FreeBSD ELF (`readelf -h` reporting `OS/ABI:
UNIX - FreeBSD`) and the built `.pkg` was verified to contain it. If a future
program's cross-compile fails in a way that looks unrelated to its own
dependency closure, re-check these two first.

## `ar`/`strip` in the Meson cross-file

Use plain `ar`/`strip` (from `binutils`), not `llvm-ar`/`llvm-strip`: the
versioned Debian package name for the latter (`llvm-ar-19`, tied to whichever
LLVM version `clang` currently resolves to) is a needless extra thing to keep
in sync with the CI image's clang version. Plain GNU `ar`/`strip` work fine
on FreeBSD-target objects - archiving and stripping aren't OS-ABI-sensitive
operations the way linking/compiling are.

## `evolution-data-server`'s real dependency closure is large

`--root evolution-data-server` pulls in **186 packages** (~737 MB downloaded,
~2.1 GB unpacked) from `pkg.freebsd.org` at time of writing - including, non-
obviously, `webkit2-gtk`, `gtk4`, `mesa-libs`, `llvm19`, and even
`py312-numpy`/`openblas`. This is comparable to or larger than
TheHolm/md_timesheet's gtk4+libadwaita closure, so `gnome-next-meeting-freebsd-pkg`
is (as expected, and as its own comment in `release.yaml` says) the slowest
step in the pipeline. This is expected and not a sign anything is wrong;
`failure: ignore` exists precisely so this doesn't block the Linux releases
regardless.

## Ubuntu 26.04 package names: verified once, may need re-checking

`gnome-next-meeting`'s Ubuntu 26.04 `.deb` step assumes the same runtime
package names/versions as Debian trixie (`libecal-2.0-3`,
`libedataserver-1.2-27t64`) since Ubuntu derives its GNOME stack packaging
from Debian's. This was not independently verified against a real Ubuntu
26.04 archive (no local Docker to pull `ubuntu:26.04` and no reachable
package-search endpoint at the time this was written - `scripts/
check-target-freshness.sh` does confirm 26.04 is the current LTS, just not
its exact package names/suffixes). Re-verify with `apt-cache policy
libecal-2.0-3 libedataserver-1.2-27t64` inside a real `ubuntu:26.04` container
before trusting a release built by that step, and update `release.yaml`'s
`--depends` if the suffix differs.

## Adding a new program's release steps

Woodpecker YAML cannot loop over repo contents dynamically, so adding a new
program to `.woodpecker/release.yaml` means by hand:

1. Add `<program>-deb-trixie` and `<program>-deb-ubuntu2604` steps
   (`depends_on: []`): install that program's own build dependencies, `make
   install DESTDIR=<stage-dir>`, then `scripts/build-deb.sh` on the staged
   tree. Verify the built `.deb` actually contains the program's files
   (`dpkg-deb -c ... | grep -qF ...`) before finishing the step.
2. Decide if a FreeBSD `.pkg` is worth attempting for this program (it always
   is worth *attempting*, since the step is `failure: ignore` either way -
   the only cost is CI time). Add a `<program>-freebsd-pkg` step: base
   sysroot from `base.txz` (reuse the same file list - see above for what a C
   program needs beyond TheHolm/dak's original Rust-shaped list; a future
   Rust program in this repo would NOT need `usr/include` or the
   `libgcc*`/`libcompiler_rt` trio), `scripts/fetch-freebsd-deps.py --root
   <this program's FreeBSD package name>` for anything beyond the base
   system, then the program's own cross-compile invocation (a Meson
   cross-file for C, `RUSTFLAGS`+`CARGO_TARGET_..._LINKER` for Rust, `GOOS=
   freebsd GOARCH=amd64` for Go - each language needs its own recipe, this
   part isn't reusable across languages the way the packaging scripts are),
   then `scripts/build-freebsd-pkg.py`.
3. Extend every `bundle-*` step's `scripts/merge-stage-roots.sh` invocation
   to also list the new program's staged directory for that target, and add
   the new program's step names to that bundle step's `depends_on:`.
4. Add the new program's step names to `publish-github-release`'s
   `depends_on:`.
5. If the new program is FreeBSD-packageable, confirm its own
   dependency-closure root package name actually exists in the FreeBSD ports
   tree first (check `Mk/Uses/*.mk` or a category `Makefile` in
   github.com/freebsd/freebsd-ports for the real port/package name - do not
   guess; `evolution-data-server`'s real FreeBSD package name, `databases/
   evolution-data-server`, was confirmed exactly this way rather than
   assumed).

## Woodpecker secret requirement

Same as TheHolm/dak/TheHolm/md_timesheet: a `github_token` secret (a GitHub
PAT scoped to Contents: read/write, restricted to the "tag" event) must exist
on this repo in Woodpecker's project settings before `publish-github-release`
can work. This is a manual, server-side setup step - nothing in this repo
configures it.
