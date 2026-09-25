#!/usr/bin/env bash
# Build every program's .deb - plus the combined "all programs" bundle - for
# one Debian/Ubuntu target, in a single pass. Called once per target step from
# .woodpecker/release.yaml; the target's build environment is already set up
# by the caller (see that file and NOTES.md).
#
# Programs are discovered by the presence of a `<program>/ci-deb.sh` script,
# so adding a program needs no change here: drop a ci-deb.sh into its
# directory and it is picked up. Each program builds into its own
# target-specific build/ and stage-root/ directories, so this is safe to run
# concurrently with the other target steps in CI's shared workspace.
#
# Usage: build-target-debs.sh <deb-revision> <deb-arch> <target-name>
#
#   deb-revision  e.g. "1~trixie" / "1~ubuntu2604" (distinguishes the targets)
#   deb-arch      e.g. "amd64"
#   target-name   a short name for this target, used in build/stage dir names
#                 e.g. "trixie" / "ubuntu2604" (must be filesystem-safe)
#
# DEB_DEPENDS is forwarded to ci-deb.sh and used for the bundle's own
# Depends: field - it should be the union of every program's runtime
# dependencies for this target.
set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $(basename "$0") <deb-revision> <deb-arch> <target-name>" >&2
    exit 1
fi

revision="$1"
arch="$2"
target="$3"

root="$(cd "$(dirname "$0")/.." && pwd)"

if [[ -z "${CI_COMMIT_TAG:-}" ]]; then
    echo "error: CI_COMMIT_TAG must be set (this script only runs in tagged CI)" >&2
    exit 1
fi
version="${CI_COMMIT_TAG#v}"

dist="$root/dist"
mkdir -p "$dist"

# Refuse to package a program whose description has fallen behind it - see
# scripts/check-package-metadata.sh and NOTES.md.
"$root/scripts/check-package-metadata.sh" "$root"

stages=()
shopt -s nullglob
for script in "$root"/*/ci-deb.sh; do
    program_dir="$(dirname "$script")"
    name="$(basename "$program_dir")"
    stage="$program_dir/stage-root-$target"

    echo "== $name: building for $target =="
    rm -rf "$stage"
    "$script" "$program_dir/build-$target" "$stage" "$dist" "$revision" "$arch"
    stages+=("$stage")
done

if [[ ${#stages[@]} -eq 0 ]]; then
    echo "error: no <program>/ci-deb.sh scripts found under $root" >&2
    exit 1
fi

echo "== bundle: combining ${#stages[@]} program(s) for $target =="
bundle_stage="$root/stage-bundle-$target"
rm -rf "$bundle_stage"
"$root/scripts/merge-stage-roots.sh" --output "$bundle_stage" "${stages[@]}"

bundle_depends=()
if [[ -n "${DEB_DEPENDS:-}" ]]; then
    bundle_depends=(--depends "$DEB_DEPENDS")
fi

"$root/scripts/build-deb.sh" \
    --name dak-nuggets \
    --version "$version" \
    --revision "$revision" \
    --arch "$arch" \
    --stage-root "$bundle_stage" \
    --output "$dist/dak-nuggets_${version}-${revision}_${arch}.deb" \
    --synopsis "All dak-nuggets helper programs, for DAK" \
    --description "Bundles every dak-nuggets helper program (see README.markdown) into a single package." \
    "${bundle_depends[@]}"

echo "== done: individual + bundle .deb packages for $target =="
