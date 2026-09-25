#!/usr/bin/env bash
# Checks every program's package metadata, and - the point of this script -
# fails when a program's description has fallen behind the program itself.
#
# Each program that is packaged carries a `<program>/package-metadata.sh`
# holding the synopsis and description used for every packaging format, plus a
# PKG_METADATA_REVIEWED_FOR marker recording the major.minor version that text
# was last checked against. A minor or major version bump means behaviour
# changed, so the description may now be wrong: this script refuses to build
# until someone re-reads it and moves the marker. Patch versions are ignored,
# since a patch release changes no behaviour by definition.
#
# The marker can of course be bumped without reading anything - it guarantees a
# decision was made at the right moment, not that the prose is good. That is
# the realistic ceiling here, short of generating package text from the README
# (rejected: a Debian synopsis has length/style rules that prose does not obey,
# and FreeBSD builds are cross-compiled so the binary cannot be run to extract
# anything). See NOTES.md.
#
# Programs are discovered exactly as the release pipeline discovers them, by
# globbing `*/ci-deb.sh` and `*/ci-freebsd.sh`, so adding a program needs no
# change here and a FreeBSD-only program is not skipped.
#
# Usage: check-package-metadata.sh [repo-root]
# Exits 0 if every program's metadata is present, well-formed and current.
set -euo pipefail

root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"

# Debian policy: a synopsis is a short phrase, not a sentence.
readonly MAX_SYNOPSIS=72

problems=0

# Reports one problem and keeps going, so a single run lists everything wrong
# rather than one thing at a time.
fail() {
    echo "error: $*" >&2
    problems=$((problems + 1))
}

# Prints the program's current version, or nothing if the language's build file
# is not one this script knows how to read.
program_version() {
    local dir="$1"

    if [[ -f "$dir/meson.build" ]]; then
        grep -m1 "version:" "$dir/meson.build" \
            | sed -E "s/.*version:[[:space:]]*'([^']+)'.*/\1/"
    elif [[ -f "$dir/Cargo.toml" ]]; then
        grep -m1 '^version' "$dir/Cargo.toml" \
            | sed -E 's/.*"([^"]+)".*/\1/'
    fi
}

# Checks one program directory.
check_program() {
    local dir="$1"
    local name
    name="$(basename "$dir")"

    local metadata="$dir/package-metadata.sh"

    if [[ ! -f "$metadata" ]]; then
        fail "$name has no package-metadata.sh (see gnome-next-meeting's as a template)"
        return
    fi

    # Sourced in a subshell so one program's values can never leak into
    # another's check.
    local synopsis description reviewed
    synopsis="$(. "$metadata"; printf '%s' "${PKG_SYNOPSIS:-}")"
    description="$(. "$metadata"; printf '%s' "${PKG_DESCRIPTION:-}")"
    reviewed="$(. "$metadata"; printf '%s' "${PKG_METADATA_REVIEWED_FOR:-}")"

    if [[ -z "$synopsis" ]]; then
        fail "$name: PKG_SYNOPSIS is empty"
    else
        if (( ${#synopsis} > MAX_SYNOPSIS )); then
            fail "$name: PKG_SYNOPSIS is ${#synopsis} characters, the limit is $MAX_SYNOPSIS"
        fi
        if [[ "$synopsis" == *. ]]; then
            fail "$name: PKG_SYNOPSIS ends with a full stop; it is a phrase, not a sentence"
        fi
        if [[ "$synopsis" == [Aa]" "* || "$synopsis" == [Aa]n" "* || "$synopsis" == [Tt]he" "* ]]; then
            fail "$name: PKG_SYNOPSIS starts with an article; drop it (Debian policy style)"
        fi
    fi

    if [[ -z "$description" ]]; then
        fail "$name: PKG_DESCRIPTION is empty"
    fi

    local version
    version="$(program_version "$dir")"

    if [[ -z "$version" ]]; then
        echo "warning: $name: no readable version (no meson.build or Cargo.toml)," \
             "skipping the staleness check" >&2
        return
    fi

    if [[ -z "$reviewed" ]]; then
        fail "$name: PKG_METADATA_REVIEWED_FOR is not set (expected \"${version%.*}\")"
        return
    fi

    # Compare major.minor only.
    local current_series="${version%.*}"

    if [[ "$reviewed" != "$current_series" ]]; then
        fail "$name is at $version but its package description was last reviewed for $reviewed -
       re-read $name/package-metadata.sh, correct the text if the program's
       behaviour has changed, then set PKG_METADATA_REVIEWED_FOR=\"$current_series\""
    fi
}

shopt -s nullglob

declare -A seen=()
dirs=()

for script in "$root"/*/ci-deb.sh "$root"/*/ci-freebsd.sh; do
    dir="$(dirname "$script")"
    if [[ -z "${seen[$dir]:-}" ]]; then
        seen[$dir]=1
        dirs+=("$dir")
    fi
done

if [[ ${#dirs[@]} -eq 0 ]]; then
    echo "error: no packaged programs found under $root (no */ci-deb.sh or */ci-freebsd.sh)" >&2
    exit 1
fi

for dir in "${dirs[@]}"; do
    check_program "$dir"
done

if (( problems > 0 )); then
    echo "package metadata check failed: $problems problem(s)" >&2
    exit 1
fi

echo "package metadata OK: ${#dirs[@]} program(s)"
