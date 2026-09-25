#!/usr/bin/env bash
# Build and package opencode-podman-status as a .deb.
#
# Called by scripts/build-target-debs.sh from CI, once per target platform; the
# target's build environment (Rust toolchain) is already set up by the caller.
# Everything is written to caller-supplied, target-specific paths so several
# targets can build at the same time in CI's shared workspace without ever
# touching the same directory - see the top-level NOTES.md.
#
# There is deliberately no ci-freebsd.sh alongside this: the program is
# Linux-only, and scripts/build-target-freebsd.sh discovers programs by globbing
# */ci-freebsd.sh, so its absence is what keeps this program out of FreeBSD
# packaging. See README.markdown and NOTES.md.
#
# Usage:
#   ci-deb.sh <build-dir> <stage-dir> <dist-dir> <deb-revision> <deb-arch>
#
# <deb-revision> is the Debian revision suffix that distinguishes targets,
# e.g. "1~trixie" or "1~ubuntu2604".
#
# NOTE: unlike the other programs, this one deliberately ignores the caller's
# DEB_DEPENDS. That variable is the *union* of every program's runtime
# dependencies for the target (it exists for the bundle package's Depends field),
# so honouring it here would make this package declare libraries it does not use
# - Rust links its dependencies statically, leaving only the C library, which
# dpkg's shlibs machinery handles. DEB_DEPENDS_OPENCODE_PODMAN_STATUS is
# available as a per-program escape hatch should that ever change.
set -euo pipefail

if [[ $# -ne 5 ]]; then
    echo "usage: $(basename "$0") <build-dir> <stage-dir> <dist-dir> <deb-revision> <deb-arch>" >&2
    exit 1
fi

build_dir="$1"
stage_dir="$2"
dist_dir="$3"
revision="$4"
arch="$5"

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"

# Package synopsis/description live in one place so the text cannot drift from
# the program - see package-metadata.sh. `set -u` above turns a missing variable
# into a build failure rather than an empty control field.
. "$here/package-metadata.sh"

# Rust statically links its own dependencies, so there is no runtime package to
# depend on beyond the C library, which dpkg's shlibs machinery would add anyway.
# The shared DEB_DEPENDS is deliberately NOT consulted - see the header.
depends="${DEB_DEPENDS_OPENCODE_PODMAN_STATUS:-}"

version="$(grep -m1 '^version' "$here/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"

# Always start from a clean target directory: CI's workspace is shared between
# concurrent target steps and may persist between pipeline runs, so a stale
# build from another target must never be reused.
rm -rf "$build_dir"
mkdir -p "$build_dir" "$dist_dir"

# CARGO_TARGET_DIR keeps the build output inside the caller's per-target
# directory rather than the source tree, which is what makes concurrent target
# builds in one workspace safe.
CARGO_TARGET_DIR="$build_dir" cargo build --release --manifest-path "$here/Cargo.toml"

install -D -m 755 \
    "$build_dir/release/opencode-podman-status" \
    "$stage_dir/usr/bin/opencode-podman-status"

deb="$dist_dir/opencode-podman-status_${version}-${revision}_${arch}.deb"

# build-deb.sh omits the Depends field entirely when given an empty value, which
# is what we want here.
"$root/scripts/build-deb.sh" \
    --name opencode-podman-status \
    --version "$version" \
    --revision "$revision" \
    --arch "$arch" \
    --stage-root "$stage_dir" \
    --output "$deb" \
    --synopsis "$PKG_SYNOPSIS" \
    --description "$PKG_DESCRIPTION" \
    --depends "$depends"

# Fail if the binary did not make it into the .deb.
dpkg-deb -c "$deb" | grep -qF 'usr/bin/opencode-podman-status'
