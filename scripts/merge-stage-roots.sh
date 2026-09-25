#!/usr/bin/env bash
# Merge multiple programs' staged install trees into one, for building the
# combined "all programs" package (see AGENTS.md's Releases & packaging
# convention).
#
# Usage: merge-stage-roots.sh --output <merged-dir> <stage-root>...
#
# Each <stage-root> must be a directory produced by some program's
# `make install DESTDIR=<stage-root>` (the root Makefile's convention), built
# for the same target (e.g. all Debian trixie, or all FreeBSD) - never mix
# stage-roots built for different targets into one merge. Every regular file
# from every stage-root is copied into --output at the same relative path.
# Two stage-roots installing to the same path is treated as an error
# (dak-nuggets programs must never collide on an install path) rather than
# silently letting the later one win.
set -euo pipefail

output=""
roots=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --output)
            output="$2"; shift 2 ;;
        *)
            roots+=("$1"); shift ;;
    esac
done

if [[ -z "$output" || ${#roots[@]} -eq 0 ]]; then
    echo "usage: $(basename "$0") --output <merged-dir> <stage-root>..." >&2
    exit 1
fi

mkdir -p "$output"

for root in "${roots[@]}"; do
    if [[ ! -d "$root" ]]; then
        echo "error: stage-root '$root' does not exist" >&2
        exit 1
    fi
    while IFS= read -r -d '' file; do
        rel="${file#"$root"/}"
        dest="$output/$rel"
        if [[ -e "$dest" ]]; then
            echo "error: '$rel' is installed by more than one program (colliding stage-root: '$root')" >&2
            exit 1
        fi
        mkdir -p "$(dirname "$dest")"
        cp -a "$file" "$dest"
    done < <(find "$root" -type f -print0)
done

echo "merged ${#roots[@]} stage-root(s) into $output"
