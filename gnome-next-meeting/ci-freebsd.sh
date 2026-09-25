#!/usr/bin/env bash
# Cross-compile and package gnome-next-meeting as a FreeBSD .pkg.
#
# Called by scripts/build-target-freebsd.sh from CI. The caller is
# responsible for setting up the shared FreeBSD cross-compilation environment
# first - the sysroot (/opt/freebsd-sysroot, from base.txz plus this
# program's dependency closure), a Meson cross-file, and the
# PKG_CONFIG_*_/... environment variables - see .woodpecker/release.yaml.
#
# Usage:
#   ci-freebsd.sh <build-dir> <stage-dir> <dist-dir> <cross-file> <deps-file>
#
# <deps-file> is the JSON written by scripts/fetch-freebsd-deps.py, recorded
# as the .pkg's runtime dependencies.
set -euo pipefail

if [[ $# -ne 5 ]]; then
    echo "usage: $(basename "$0") <build-dir> <stage-dir> <dist-dir> <cross-file> <deps-file>" >&2
    exit 1
fi

build_dir="$1"
stage_dir="$2"
dist_dir="$3"
cross_file="$4"
deps_file="$5"

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"

# Same single source of packaging text as ci-deb.sh - see package-metadata.sh.
. "$here/package-metadata.sh"

version="$(grep -m1 "version:" "$here/meson.build" | sed -E "s/.*version:[[:space:]]*'([^']+)'.*/\1/")"

# Clean build directory for the same reason as ci-deb.sh.
rm -rf "$build_dir"

meson setup "$build_dir" "$here" --cross-file "$cross_file" --prefix=/usr/local
meson compile -C "$build_dir"
DESTDIR="$stage_dir" meson install -C "$build_dir"

mkdir -p "$dist_dir"
pkg="$dist_dir/gnome-next-meeting-${version}-freebsd-amd64.pkg"

python3 "$root/scripts/build-freebsd-pkg.py" \
    --name gnome-next-meeting \
    --origin "sysutils/gnome-next-meeting" \
    --version "$version" \
    --stage-root "$stage_dir" \
    --deps-file "$deps_file" \
    --comment "$PKG_SYNOPSIS" \
    --desc "$PKG_DESCRIPTION" \
    --www "https://github.com/TheHolm/dak-nuggets" \
    --output "$pkg"

# Fail if the binary did not make it into the .pkg.
zstd -dc "$pkg" | tar -tf - | grep -qF 'usr/local/bin/gnome-next-meeting'
