#!/usr/bin/env bash
# Build and package gnome-next-meeting as a .deb.
#
# Called by scripts/build-target-debs.sh from CI, once per target platform;
# the target's build environment (toolchain, -dev packages) is already set up
# by the caller. Everything is written to caller-supplied, target-specific
# paths so several targets can build at the same time in CI's shared
# workspace without ever touching the same directory - see NOTES.md.
#
# Usage:
#   ci-deb.sh <build-dir> <stage-dir> <dist-dir> <deb-revision> <deb-arch>
#
# <deb-revision> is the Debian revision suffix that distinguishes targets,
# e.g. "1~trixie" or "1~ubuntu2604". DEB_DEPENDS may be set by the caller to
# override the runtime dependency list (defaults to the Debian trixie names).
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

# Package synopsis/description live in one place, shared with ci-freebsd.sh, so
# the two packaging formats cannot describe the program differently - see
# package-metadata.sh and NOTES.md. `set -u` above turns a missing variable
# into a build failure rather than an empty control field.
. "$here/package-metadata.sh"

# NOTE: these runtime package names were verified against Debian trixie. If a
# target's names/versions differ, the caller sets DEB_DEPENDS (see
# .woodpecker/release.yaml and NOTES.md).
depends="${DEB_DEPENDS:-libecal-2.0-3, libedataserver-1.2-27t64}"

version="$(grep -m1 "version:" "$here/meson.build" | sed -E "s/.*version:[[:space:]]*'([^']+)'.*/\1/")"

# Always start from a clean build directory: CI's workspace is shared between
# concurrent target steps and may also persist between pipeline runs, so a
# stale Meson build directory from another target or an earlier run must never
# be reused (it can be unreadable by this step's Meson version).
rm -rf "$build_dir"

meson setup "$build_dir" "$here" --prefix=/usr
meson compile -C "$build_dir"
DESTDIR="$stage_dir" meson install -C "$build_dir"

mkdir -p "$dist_dir"
deb="$dist_dir/gnome-next-meeting_${version}-${revision}_${arch}.deb"

"$root/scripts/build-deb.sh" \
    --name gnome-next-meeting \
    --version "$version" \
    --revision "$revision" \
    --arch "$arch" \
    --stage-root "$stage_dir" \
    --output "$deb" \
    --synopsis "$PKG_SYNOPSIS" \
    --description "$PKG_DESCRIPTION" \
    --depends "$depends"

# Fail if the binary did not make it into the .deb.
dpkg-deb -c "$deb" | grep -qF 'usr/bin/gnome-next-meeting'
