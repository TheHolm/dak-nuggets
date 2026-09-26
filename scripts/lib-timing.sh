# Shared timing helper, sourced (never executed) by build scripts that want
# to report how long each of their steps took - see NOTES.md.
#
# Uses bash's built-in `SECONDS` rather than shelling out to `date`: it is
# already ~0 at the start of any freshly exec'd script (each ci-*.sh and
# build-target-*.sh runs as its own process), and assigning to it resets the
# count, so timing a sequence of steps needs no extra bookkeeping - just call
# `step_done` once after each step.
#
# Usage:
#   . "$root/scripts/lib-timing.sh"
#   ... do step 1 ...
#   step_done "label for step 1"       # prints "== label for step 1: XmYs =="
#   ... do step 2 ...
#   step_done "label for step 2"
#
# 1-second resolution (SECONDS increments once per second), which is fine for
# the steps this is used for - builds/downloads/packaging taking seconds to
# minutes - and not intended for anything sub-second.

# Print how long has elapsed since the last `step_done` call (or since the
# script started, for the first call) as "MmSs", then reset the clock for the
# next step.
step_done() {
    local label="$1"
    local total="$SECONDS"
    echo "== $label: $((total / 60))m$((total % 60))s =="
    SECONDS=0
}
