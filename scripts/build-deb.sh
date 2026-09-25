#!/usr/bin/env bash
# Build a Debian/Ubuntu .deb package from a staged install tree.
#
# Language-agnostic replacement for `cargo-deb`: dak-nuggets is a
# multi-language monorepo (see AGENTS.md), so packaging can't depend on a
# Rust-specific tool. Any program that exposes `make install DESTDIR=<dir>`
# (the root Makefile's convention) can be packaged with this script,
# regardless of what built it.
#
# Usage:
#   build-deb.sh --name gnome-next-meeting --version 0.1.0 --revision 1~trixie \
#     --arch amd64 --stage-root ./stage-root --output dist/gnome-next-meeting_0.1.0-1~trixie_amd64.deb \
#     --synopsis "Time until the next calendar meeting" \
#     [--description "<longer paragraph>"] \
#     [--depends "libecal-2.0-3, libedataserver-1.2-27t64"] \
#     [--maintainer "..."] [--section utils] [--priority optional]
#
# `--stage-root` must contain the package's files at their final absolute
# install paths (e.g. <stage-root>/usr/local/bin/<name>), i.e. exactly what
# `make install DESTDIR=<stage-root>` produces. `dpkg-deb --root-owner-group`
# normalizes ownership to root:root in the built archive without needing to
# actually chown the staged tree.
set -euo pipefail

name=""
version=""
revision="1"
arch=""
stage_root=""
output=""
synopsis=""
description=""
depends=""
maintainer="TheHolm <theholm@github.com>"
section="utils"
priority="optional"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --name) name="$2"; shift 2 ;;
        --version) version="$2"; shift 2 ;;
        --revision) revision="$2"; shift 2 ;;
        --arch) arch="$2"; shift 2 ;;
        --stage-root) stage_root="$2"; shift 2 ;;
        --output) output="$2"; shift 2 ;;
        --synopsis) synopsis="$2"; shift 2 ;;
        --description) description="$2"; shift 2 ;;
        --depends) depends="$2"; shift 2 ;;
        --maintainer) maintainer="$2"; shift 2 ;;
        --section) section="$2"; shift 2 ;;
        --priority) priority="$2"; shift 2 ;;
        *)
            echo "error: unknown argument '$1'" >&2
            exit 1
            ;;
    esac
done

missing=()
[[ -z "$name" ]] && missing+=(--name)
[[ -z "$version" ]] && missing+=(--version)
[[ -z "$arch" ]] && missing+=(--arch)
[[ -z "$stage_root" ]] && missing+=(--stage-root)
[[ -z "$output" ]] && missing+=(--output)
[[ -z "$synopsis" ]] && missing+=(--synopsis)
if [[ ${#missing[@]} -gt 0 ]]; then
    echo "error: missing required argument(s): ${missing[*]}" >&2
    exit 1
fi

if [[ ! -d "$stage_root" ]]; then
    echo "error: stage-root '$stage_root' does not exist" >&2
    exit 1
fi

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

pkg_dir="$work_dir/pkg"
mkdir -p "$pkg_dir/DEBIAN"
cp -a "$stage_root/." "$pkg_dir/"

installed_size="$(du -sk "$stage_root" | cut -f1)"

{
    echo "Package: $name"
    echo "Version: ${version}-${revision}"
    echo "Section: $section"
    echo "Priority: $priority"
    echo "Architecture: $arch"
    echo "Installed-Size: $installed_size"
    if [[ -n "$depends" ]]; then
        echo "Depends: $depends"
    fi
    echo "Maintainer: $maintainer"
    echo "Description: $synopsis"
    if [[ -n "$description" ]]; then
        # Debian control file continuation-line format: every line of the
        # long description is indented with exactly one leading space, and a
        # literal blank line is written as a lone "." so it survives (a truly
        # empty line would end the paragraph and terminate the field).
        while IFS= read -r line; do
            if [[ -z "$line" ]]; then
                echo " ."
            else
                echo " $line"
            fi
        done <<< "$description"
    fi
} > "$pkg_dir/DEBIAN/control"

mkdir -p "$(dirname "$output")"
dpkg-deb --build --root-owner-group "$pkg_dir" "$output"
echo "wrote $output"
