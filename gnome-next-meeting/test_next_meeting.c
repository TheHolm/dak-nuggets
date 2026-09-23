#include <glib.h>

#include "next_meeting.h"

/*
 * Fixed reference time so the tests never depend on the wall clock.
 * 2023-11-14 22:13:20 UTC - the actual value is irrelevant, only that
 * everything is expressed relative to it.
 */
#define NOW ((time_t)1700000000)

/**
 * With no instances offered, the output is the "no meeting" marker.
 */
static void
test_no_meeting(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_autofree gchar *out = nm_format(&nm);
    g_assert_cmpstr(out, ==, "----");
}

/**
 * A meeting that has not started yet is accepted and rendered as HH:MM
 * remaining.
 */
static void
test_future_meeting(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + 90 * 60));

    g_autofree gchar *out = nm_format(&nm);
    g_assert_cmpstr(out, ==, "01:30");
}

/**
 * A meeting exactly one second away still renders (as "00:00", since the
 * seconds are truncated).
 */
static void
test_meeting_one_second_away(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + 1));

    g_autofree gchar *out = nm_format(&nm);
    g_assert_cmpstr(out, ==, "00:00");
}

/**
 * Sub-minute remaining times truncate down to "00:00" rather than rounding up.
 */
static void
test_seconds_are_truncated(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + 59));

    g_autofree gchar *out = nm_format(&nm);
    g_assert_cmpstr(out, ==, "00:00");
}

/**
 * A meeting that is starting right now, or has already started, is ignored.
 */
static void
test_started_meetings_ignored(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_false(nm_consider(&nm, FALSE, NOW));
    g_assert_false(nm_consider(&nm, FALSE, NOW - 1));

    g_autofree gchar *out = nm_format(&nm);
    g_assert_cmpstr(out, ==, "----");
}

/**
 * All-day (DATE-only) events are ignored, even if their start time would
 * otherwise look like a future meeting.
 */
static void
test_all_day_ignored(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_false(nm_consider(&nm, TRUE, NOW + 3600));

    g_autofree gchar *out = nm_format(&nm);
    g_assert_cmpstr(out, ==, "----");
}

/**
 * When several future meetings are offered, the nearest one wins, regardless
 * of the order they arrive in.
 */
static void
test_nearest_meeting_wins(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + 3600));
    g_assert_true(nm_consider(&nm, FALSE, NOW + 1800));

    /* A later meeting must not displace the nearer one already found. */
    g_assert_false(nm_consider(&nm, FALSE, NOW + 7200));

    g_autofree gchar *out = nm_format(&nm);
    g_assert_cmpstr(out, ==, "00:30");
}

/**
 * Multi-hour remaining times render correctly, with both fields zero-padded.
 */
static void
test_multi_hour_formatting(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + 10 * 3600 + 5 * 60));

    g_autofree gchar *out = nm_format(&nm);
    g_assert_cmpstr(out, ==, "10:05");
}

/**
 * Re-initialising resets any previously found meeting.
 */
static void
test_reinit_clears_state(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);
    g_assert_true(nm_consider(&nm, FALSE, NOW + 60));

    nm_init(&nm, NOW + 120);

    g_autofree gchar *out = nm_format(&nm);
    g_assert_cmpstr(out, ==, "----");
}

/**
 * Escape expansion turns "\n", "\t" and "\\" into their literal characters.
 */
static void
test_expand_escapes(void)
{
    g_autofree gchar *out = nm_expand_escapes("a\\nb\\tc\\\\d");

    g_assert_cmpstr(out, ==, "a\nb\tc\\d");
}

/**
 * Unrecognised escapes and a trailing backslash are preserved verbatim.
 */
static void
test_expand_escapes_passthrough(void)
{
    g_autofree gchar *out = nm_expand_escapes("100% \\q \\");

    g_assert_cmpstr(out, ==, "100% \\q \\");
}

/**
 * A NULL input yields NULL, so optional decoration is a no-op.
 */
static void
test_expand_escapes_null(void)
{
    g_assert_null(nm_expand_escapes(NULL));
}

/**
 * Decoration is placed on either side of the time, with escapes expanded.
 */
static void
test_decorate_both_sides(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);
    g_assert_true(nm_consider(&nm, FALSE, NOW + 90 * 60));

    g_autofree gchar *out = nm_decorate(&nm, "In\\n", " to go");

    g_assert_cmpstr(out, ==, "In\n01:30 to go");
}

/**
 * Missing (NULL) decoration on either side simply yields the bare time.
 */
static void
test_decorate_no_decoration(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);
    g_assert_true(nm_consider(&nm, FALSE, NOW + 60));

    g_autofree gchar *out = nm_decorate(&nm, NULL, NULL);

    g_assert_cmpstr(out, ==, "00:01");
}

/**
 * Decoration also applies to the "----" no-meeting marker.
 */
static void
test_decorate_no_meeting(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_autofree gchar *out = nm_decorate(&nm, "[", "]");

    g_assert_cmpstr(out, ==, "[----]");
}

/**
 * A NULL decoration on only one side still combines correctly.
 */
static void
test_decorate_one_side(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);
    g_assert_true(nm_consider(&nm, FALSE, NOW + 30 * 60));

    g_autofree gchar *before = nm_decorate(&nm, "<", NULL);
    g_autofree gchar *after = nm_decorate(&nm, NULL, ">");

    g_assert_cmpstr(before, ==, "<00:30");
    g_assert_cmpstr(after, ==, "00:30>");
}

int
main(int argc, char *argv[])
{
    g_test_init(&argc, &argv, NULL);

    g_test_add_func("/next-meeting/no-meeting", test_no_meeting);
    g_test_add_func("/next-meeting/future-meeting", test_future_meeting);
    g_test_add_func("/next-meeting/one-second-away", test_meeting_one_second_away);
    g_test_add_func("/next-meeting/seconds-truncated", test_seconds_are_truncated);
    g_test_add_func("/next-meeting/started-ignored", test_started_meetings_ignored);
    g_test_add_func("/next-meeting/all-day-ignored", test_all_day_ignored);
    g_test_add_func("/next-meeting/nearest-wins", test_nearest_meeting_wins);
    g_test_add_func("/next-meeting/multi-hour", test_multi_hour_formatting);
    g_test_add_func("/next-meeting/reinit-clears-state", test_reinit_clears_state);
    g_test_add_func("/next-meeting/expand-escapes", test_expand_escapes);
    g_test_add_func("/next-meeting/expand-escapes-passthrough", test_expand_escapes_passthrough);
    g_test_add_func("/next-meeting/expand-escapes-null", test_expand_escapes_null);
    g_test_add_func("/next-meeting/decorate-both-sides", test_decorate_both_sides);
    g_test_add_func("/next-meeting/decorate-no-decoration", test_decorate_no_decoration);
    g_test_add_func("/next-meeting/decorate-no-meeting", test_decorate_no_meeting);
    g_test_add_func("/next-meeting/decorate-one-side", test_decorate_one_side);

    return g_test_run();
}
