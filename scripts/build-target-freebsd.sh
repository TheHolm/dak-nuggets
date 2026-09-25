#!/usr/bin/env bash
# Cross-compile and package every program's FreeBSD .pkg - plus the combined
# "all programs" bundle - in a single pass. Called once from
# .woodpecker/release.yaml; the shared FreeBSD cross-compilation environment
# (sysroot, PKG_CONFIG_* variables, base.txz extraction and the dependency
# closure fetch) is already set up by the caller - see that file and NOTES.md.
#
# Programs are discovered by the presence of a `<program>/ci-freebsd.sh`
# script, so adding a program needs no change here: a program that has no
# ci-freebsd.sh (e.g. one that cannot be cross-compiled) is simply skipped.
#
# Usage: build-target-freebsd.sh <cross-file> <deps-file>
#
#   cross-file  Meson cross-file for the FreeBSD target
#   deps-file   JSON written by scripts/fetch-freebsd-deps.py, recorded as the
#               programs' .pkg runtime dependencies
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $(basename "$0") <cross-file> <deps-file>" >&2
    exit 1
fi

cross_file="$1"
deps_file="$2"

root="$(cd "$(dirname "$0")/.." && pwd)"

if [[ -z "${CI_COMMIT_TAG:-}" ]]; then
    echo "error: CI_COMMIT_TAG must be set (this script only runs in tagged CI)" >&2
    exit 1
fi
version="${CI_COMMIT_TAG#v}"

dist="$root/dist"
mkdir -p "$dist"

# Same metadata gate as the .deb path - see NOTES.md.
"$root/scripts/check-package-metadata.sh" "$root"

stages=()
shopt -s nullglob
for script in "$root"/*/ci-freebsd.sh; do
    program_dir="$(dirname "$script")"
    name="$(basename "$program_dir")"
    stage="$program_dir/stage-root-freebsd"

    echo "== $name: cross-compiling for freebsd =="
    rm -rf "$stage"
    "$script" "$program_dir/build-freebsd" "$stage" "$dist" "$cross_file" "$deps_file"
    stages+=("$stage")
done

if [[ ${#stages[@]} -eq 0 ]]; then
    echo "warning: no <program>/ci-freebsd.sh scripts found under $root" >&2
    exit 0
fi

echo "== bundle: combining ${#stages[@]} program(s) for freebsd =="
bundle_stage="$root/stage-bundle-freebsd"
rm -rf "$bundle_stage"
"$root/scripts/merge-stage-roots.sh" --output "$bundle_stage" "${stages[@]}"

python3 "$root/scripts/build-freebsd-pkg.py" \
    --name dak-nuggets \
    --origin "sysutils/dak-nuggets" \
    --version "$version" \
    --stage-root "$bundle_stage" \
    --comment "All dak-nuggets helper programs, for DAK" \
    --desc "Bundles every dak-nuggets helper program (see README.markdown) into a single package." \
    --www "https://github.com/TheHolm/dak-nuggets" \
    --output "$dist/dak-nuggets-${version}-freebsd-amd64.pkg"

echo "== done: individual + bundle .pkg packages for freebsd =="
