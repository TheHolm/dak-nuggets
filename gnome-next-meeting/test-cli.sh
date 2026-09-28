#!/bin/sh
# Exercises gnome-next-meeting's own command-line argument handling: every
# bad --fmt/--soon/--ending/--lines argument, plus --help. All of this runs
# and exits before the program ever touches Evolution Data Server, so it
# needs no live EDS session (unlike the rest of main.c: see NOTES.md
# "Known gaps").
#
# Usage: test-cli.sh BINARY
# Exits 0 if every case behaved as expected, non-zero otherwise (printing
# every mismatch found, not just the first).

set -u

if [ "$#" -ne 1 ]; then
    echo "usage: test-cli.sh BINARY" >&2
    exit 2
fi

bin="$1"
failures=0
cases=0

# Runs the binary with the given arguments, then checks its exit status and
# that stderr contains (not necessarily equals) the given substring. stdout
# is discarded; every argument-validation failure path in main.c writes to
# stderr only.
check() {
    description="$1"
    expected_status="$2"
    expected_stderr_substring="$3"
    shift 3

    cases=$((cases + 1))

    actual_stderr=$("$bin" "$@" 2>&1 >/dev/null)
    actual_status=$?

    if [ "$actual_status" -ne "$expected_status" ]; then
        echo "FAIL: $description: expected exit $expected_status, got $actual_status" >&2
        failures=$((failures + 1))
        return
    fi

    case "$actual_stderr" in
        *"$expected_stderr_substring"*) ;;
        *)
            echo "FAIL: $description: expected stderr to contain '$expected_stderr_substring', got '$actual_stderr'" >&2
            failures=$((failures + 1))
            ;;
    esac
}

# --fmt argument shapes rejected by nm_apply_fmt() (next_meeting.c).
check "--fmt with no '='" 1 "expected KEYS=TAGS" --fmt start
check "--fmt with an empty key list" 1 "expected KEYS=TAGS" --fmt '=#[fg=red]'
check "--fmt with an unknown key" 1 "unknown key 'foo'" --fmt 'foo=#[fg=red]'
check "--fmt with a value that is not made of #[...] tags" 1 \
    "must be made only of #[...] tags" --fmt 'start=plain'

# --soon/--ending rejected by nm_threshold_seconds() (next_meeting.c).
check "--soon 0" 1 "--soon must be at least 1" --soon 0
check "--soon negative" 1 "--soon must be at least 1" --soon -1
check "--ending 0" 1 "--ending must be at least 1" --ending 0
check "--ending negative" 1 "--ending must be at least 1" --ending -1

# A valid --fmt does not skip the --soon/--ending check that runs after it.
check "valid --fmt does not bypass a bad --soon" 1 \
    "--soon must be at least 1" --fmt 'start=#[fg=red]' --soon 0

# Pre-existing checks, exercised here for the first time: --lines' own
# validation, and GLib's own option-parsing errors (a non-integer argument,
# an unknown flag), all of which must also exit 1 before EDS is touched.
check "--lines 0" 1 "--lines must be at least 1" --lines 0
check "--soon given a non-integer" 1 "--soon" --soon abc
check "an unknown option" 1 "Unknown option" --does-not-exist

# --help exits 0, prints nothing to stderr, and names the version and the
# 0.4.0 options - so a future rename of one of them fails this test.
help_stderr=$("$bin" --help 2>&1 >/tmp/gnome-next-meeting-test-cli-help.$$)
help_status=$?
help_stdout=$(cat /tmp/gnome-next-meeting-test-cli-help.$$)
rm -f /tmp/gnome-next-meeting-test-cli-help.$$
cases=$((cases + 1))

if [ "$help_status" -ne 0 ]; then
    echo "FAIL: --help: expected exit 0, got $help_status" >&2
    failures=$((failures + 1))
elif [ -n "$help_stderr" ]; then
    echo "FAIL: --help: expected empty stderr, got '$help_stderr'" >&2
    failures=$((failures + 1))
else
    for needle in -- "--fmt" "--soon" "--ending"; do
        case "$needle" in
            --) continue ;;
        esac
        case "$help_stdout" in
            *"$needle"*) ;;
            *)
                echo "FAIL: --help: expected stdout to mention '$needle'" >&2
                failures=$((failures + 1))
                ;;
        esac
    done
fi

if [ "$failures" -eq 0 ]; then
    echo "ok: $cases case(s) passed"
    exit 0
fi

echo "$failures/$cases case(s) failed" >&2
exit 1
