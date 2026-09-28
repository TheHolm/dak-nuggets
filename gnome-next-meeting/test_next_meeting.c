#include <glib.h>
#include <string.h>

#include "next_meeting.h"

/*
 * Fixed reference time so the tests never depend on the wall clock.
 * 2023-11-14 22:13:20 UTC - the actual value is irrelevant, only that
 * everything is expressed relative to it.
 */
#define NOW ((time_t)1700000000)

/*
 * Arbitrary but distinct identity hashes, ordered KEY_LOW < KEY_HIGH, for
 * exercising the tie-break between events falling at the same second.
 */
#define KEY_LOW ((guint32)100)
#define KEY_HIGH ((guint32)200)

/*
 * A minute and an hour in seconds, to keep the test arithmetic readable.
 */
#define MINUTES(n) ((time_t)((n) * 60))
#define HOURS(n) ((time_t)((n) * 3600))

/**
 * With no instances offered, the output is the "no meeting" marker.
 */
static void
test_no_meeting(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "----");

    nm_clear(&nm);
}

/**
 * A meeting that has not started yet renders as a space and the time
 * remaining until it begins.
 */
static void
test_future_meeting(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + MINUTES(90),
                              NOW + MINUTES(150), KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 01:30");

    nm_clear(&nm);
}

/**
 * A meeting already under way renders as a "-" and the time remaining until
 * it ends, not until it started.
 */
static void
test_meeting_in_progress(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW - MINUTES(35),
                              NOW + MINUTES(25), KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "-00:25");

    nm_clear(&nm);
}

/**
 * A meeting which began yesterday and is still running counts down to its end
 * like any other meeting in progress: how long ago it started is irrelevant.
 * With nothing else left, that countdown is the only line printed. (Verified
 * live too: EDS returns such an instance even though its start precedes the
 * queried window - see NOTES.md.)
 */
static void
test_meeting_started_yesterday(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW - HOURS(29),
                              NOW + MINUTES(105), KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "-01:45");

    nm_clear(&nm);
}

/**
 * A meeting which began yesterday and does not end until tomorrow - covering
 * the whole of today from both sides - still counts down to its end, with the
 * hours field free to exceed 24.
 */
static void
test_meeting_spanning_whole_day(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW - HOURS(29), NOW + HOURS(26),
                              KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "-26:00");

    nm_clear(&nm);
}

/**
 * A meeting starting exactly now counts as in progress, so it counts down to
 * its end.
 */
static void
test_meeting_starting_exactly_now(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW, NOW + MINUTES(60), KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "-01:00");

    nm_clear(&nm);
}

/**
 * A meeting a single second away still renders as "00:00", since seconds are
 * truncated rather than rounded.
 */
static void
test_meeting_one_second_away(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + 1, NOW + MINUTES(60),
                              KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 00:00");

    nm_clear(&nm);
}

/**
 * Just under a minute away is still "00:00": seconds never round up.
 */
static void
test_seconds_are_truncated(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + 59, NOW + MINUTES(60),
                              KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 00:00");

    nm_clear(&nm);
}

/**
 * Meetings which have already finished contribute nothing, including one
 * ending at exactly the reference time.
 */
static void
test_finished_meetings_ignored(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_false(nm_consider(&nm, FALSE, NOW - HOURS(2), NOW - HOURS(1),
                               KEY_LOW));
    g_assert_false(nm_consider(&nm, FALSE, NOW - HOURS(1), NOW, KEY_HIGH));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "----");

    nm_clear(&nm);
}

/**
 * A meeting ending one second from now is still in progress, and renders as
 * "-00:00" rather than disappearing.
 */
static void
test_meeting_ending_one_second_away(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW - HOURS(1), NOW + 1, KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "-00:00");

    nm_clear(&nm);
}

/**
 * All-day events are ignored whether they are still to come or notionally in
 * progress, since a HH:MM countdown does not apply to them.
 */
static void
test_all_day_ignored(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_false(nm_consider(&nm, TRUE, NOW + HOURS(1), NOW + HOURS(2),
                               KEY_LOW));
    g_assert_false(nm_consider(&nm, TRUE, NOW - HOURS(1), NOW + HOURS(1),
                               KEY_HIGH));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "----");

    nm_clear(&nm);
}

/**
 * A zero-length meeting still to come contributes its start; the same meeting
 * once reached contributes nothing, as there is no time left in it.
 */
static void
test_zero_length_meeting(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + MINUTES(30),
                              NOW + MINUTES(30), KEY_LOW));
    g_assert_false(nm_consider(&nm, FALSE, NOW, NOW, KEY_HIGH));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 00:30");

    nm_clear(&nm);
}

/**
 * A malformed instance whose end precedes its start is treated as
 * zero-length: still usable while it is in the future, dropped once reached.
 */
static void
test_end_before_start_treated_as_zero_length(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + MINUTES(30),
                              NOW + MINUTES(10), KEY_LOW));
    g_assert_false(nm_consider(&nm, FALSE, NOW - MINUTES(10),
                               NOW - MINUTES(30), KEY_HIGH));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 00:30");

    nm_clear(&nm);
}

/**
 * Several meetings are rendered one per line, soonest first, regardless of
 * the order they were offered in.
 */
static void
test_events_sorted_soonest_first(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    /* Offered latest-first to prove the output is sorted, not appended. */
    g_assert_true(nm_consider(&nm, FALSE, NOW + HOURS(2), NOW + HOURS(3),
                              KEY_LOW));
    g_assert_true(nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1),
                              KEY_LOW));
    g_assert_true(nm_consider(&nm, FALSE, NOW - MINUTES(5), NOW + MINUTES(10),
                              KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "-00:10\n 00:30\n 02:00");

    nm_clear(&nm);
}

/**
 * Only the requested number of lines is printed, keeping the most imminent
 * countdowns.
 */
static void
test_max_lines_truncates(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    for (int i = 1; i <= 5; i++) {
        g_assert_true(nm_consider(&nm, FALSE, NOW + HOURS(i),
                                  NOW + HOURS(i) + MINUTES(30), KEY_LOW));
    }

    g_autofree gchar *two = nm_format(&nm, 2, NULL);
    g_assert_cmpstr(two, ==, " 01:00\n 02:00");

    g_autofree gchar *one = nm_format(&nm, 1, NULL);
    g_assert_cmpstr(one, ==, " 01:00");

    nm_clear(&nm);
}

/**
 * Asking for more lines than there are events prints only the events
 * available, and a limit below one is clamped to a single line.
 */
static void
test_max_lines_bounds(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + HOURS(1), NOW + HOURS(2),
                              KEY_LOW));
    g_assert_true(nm_consider(&nm, FALSE, NOW + HOURS(3), NOW + HOURS(4),
                              KEY_LOW));

    g_autofree gchar *plenty = nm_format(&nm, 10, NULL);
    g_assert_cmpstr(plenty, ==, " 01:00\n 03:00");

    g_autofree gchar *none = nm_format(&nm, 0, NULL);
    g_assert_cmpstr(none, ==, " 01:00");

    nm_clear(&nm);
}

/**
 * Two meetings starting at the same second are ordered by identity hash, and
 * the order does not depend on which was offered first - so consecutive runs
 * of the program agree.
 */
static void
test_simultaneous_events_ordered_by_key(void)
{
    NextMeeting first;
    nm_init(&first, NOW);

    nm_consider(&first, FALSE, NOW + HOURS(1), NOW + HOURS(2), KEY_HIGH);
    nm_consider(&first, FALSE, NOW + HOURS(1), NOW + HOURS(3), KEY_LOW);

    NextMeeting second;
    nm_init(&second, NOW);

    nm_consider(&second, FALSE, NOW + HOURS(1), NOW + HOURS(3), KEY_LOW);
    nm_consider(&second, FALSE, NOW + HOURS(1), NOW + HOURS(2), KEY_HIGH);

    g_autofree gchar *out_first = nm_format(&first, NM_DEFAULT_LINES, NULL);
    g_autofree gchar *out_second = nm_format(&second, NM_DEFAULT_LINES, NULL);

    g_assert_cmpstr(out_first, ==, " 01:00\n 01:00");
    g_assert_cmpstr(out_first, ==, out_second);

    nm_clear(&first);
    nm_clear(&second);
}

/**
 * When time and identity hash are both equal, the event kind decides, so a
 * start is printed before an end landing on the same second.
 */
static void
test_simultaneous_events_ordered_by_kind(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    /* An end and a start which coincide, both attributed to the same key. */
    nm_consider(&nm, FALSE, NOW - HOURS(1), NOW + HOURS(1), KEY_LOW);
    nm_consider(&nm, FALSE, NOW + HOURS(1), NOW + HOURS(2), KEY_LOW);

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 01:00\n-01:00");

    nm_clear(&nm);
}

/**
 * The identity hash depends on every field, treats NULL as empty, and
 * respects field boundaries so that moving a character between fields
 * changes the result.
 */
static void
test_key_distinguishes_fields(void)
{
    g_assert_cmpuint(nm_key("uid", "rid", "summary"), ==,
                     nm_key("uid", "rid", "summary"));

    g_assert_cmpuint(nm_key("uid", "rid", "summary"), !=,
                     nm_key("other", "rid", "summary"));
    g_assert_cmpuint(nm_key("uid", "rid", "summary"), !=,
                     nm_key("uid", "other", "summary"));
    g_assert_cmpuint(nm_key("uid", "rid", "summary"), !=,
                     nm_key("uid", "rid", "other"));

    g_assert_cmpuint(nm_key(NULL, NULL, NULL), ==, nm_key("", "", ""));
    g_assert_cmpuint(nm_key("uid", NULL, NULL), ==, nm_key("uid", "", ""));

    g_assert_cmpuint(nm_key("ab", "c", ""), !=, nm_key("a", "bc", ""));
}

/**
 * The identity hash is pinned to documented 32-bit FNV-1a output, so the
 * ordering of simultaneous meetings cannot silently change between releases.
 */
static void
test_key_is_pinned(void)
{
    g_assert_cmpuint(nm_key("uid-1", "20231114T220000Z", "Standup"), ==,
                     26726279u);
    g_assert_cmpuint(nm_key(NULL, NULL, NULL), ==, 4272243572u);
}

/**
 * Countdowns of more than one hour are rendered with both fields.
 */
static void
test_multi_hour_formatting(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE,
                              NOW + HOURS(10) + MINUTES(5),
                              NOW + HOURS(11), KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 10:05");

    nm_clear(&nm);
}

/**
 * A meeting in progress which runs past midnight still counts down to its
 * end, so the hours field is free to exceed 24.
 */
static void
test_end_past_midnight(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW - HOURS(1),
                              NOW + HOURS(27) + MINUTES(15), KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "-27:15");

    nm_clear(&nm);
}

/**
 * An absurdly long meeting is clamped to "99:59" so the line stays six
 * characters wide.
 */
static void
test_long_countdown_clamped(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW - HOURS(1), NOW + HOURS(500),
                              KEY_LOW));

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "-99:59");

    nm_clear(&nm);
}

/**
 * Re-initialising discards everything collected previously.
 */
static void
test_reinit_clears_state(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_consider(&nm, FALSE, NOW + HOURS(1), NOW + HOURS(2), KEY_LOW);

    nm_clear(&nm);
    nm_init(&nm, NOW);

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "----");

    nm_clear(&nm);
}

/**
 * Clearing an already-cleared state is harmless.
 */
static void
test_clear_is_idempotent(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_clear(&nm);
    nm_clear(&nm);

    g_assert_null(nm.events);
}

/**
 * Overlapping meetings each keep their own line: the meeting in progress
 * counts down to its end while the two clashing ones count down to their
 * starts, all in time order.
 */
static void
test_clashing_meetings(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    /* In progress, ending in 10 minutes. */
    nm_consider(&nm, FALSE, NOW - MINUTES(50), NOW + MINUTES(10), KEY_LOW);
    /* Starts in 10 minutes, runs an hour. */
    nm_consider(&nm, FALSE, NOW + MINUTES(10), NOW + MINUTES(70), KEY_HIGH);
    /* Clashes with the previous one, starting half an hour into it. */
    nm_consider(&nm, FALSE, NOW + MINUTES(40), NOW + MINUTES(100), KEY_LOW);
    /* Later still, and so pushed off the end of a three-line display. */
    nm_consider(&nm, FALSE, NOW + HOURS(8), NOW + HOURS(9), KEY_LOW);

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, "-00:10\n 00:10\n 00:40");

    nm_clear(&nm);
}

/**
 * The same meeting occurrence offered twice - as happens when one meeting is
 * subscribed in two enabled calendars - is collapsed into a single countdown,
 * both in the collected events and in the output.
 */
static void
test_duplicate_event_collapses(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_assert_true(nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1),
                              KEY_LOW));
    g_assert_true(nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1),
                              KEY_LOW));

    g_assert_cmpuint(nm.events->len, ==, 2);

    nm_sort(&nm);

    g_assert_cmpuint(nm.events->len, ==, 1);

    /* Sorting again must not disturb the result. */
    nm_sort(&nm);
    g_assert_cmpuint(nm.events->len, ==, 1);

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 00:30");

    nm_clear(&nm);
}

/**
 * A duplicate is collapsed before the line limit is applied, so it never costs
 * a distinct meeting its place in the output.
 */
static void
test_duplicate_does_not_consume_a_line(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1), KEY_LOW);
    nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1), KEY_LOW);
    nm_consider(&nm, FALSE, NOW + HOURS(2), NOW + HOURS(3), KEY_HIGH);
    nm_consider(&nm, FALSE, NOW + HOURS(4), NOW + HOURS(5), KEY_HIGH);

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 00:30\n 02:00\n 04:00");

    nm_clear(&nm);
}

/**
 * If the reference time is moved past an event that was already collected -
 * the clock jumping forward between collection and rendering - the countdown
 * bottoms out at zero instead of going negative and breaking the fixed line
 * width.
 */
static void
test_clock_moved_forward(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1), KEY_LOW);

    nm.now = NOW + HOURS(2);

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, NULL);
    g_assert_cmpstr(out, ==, " 00:00");

    nm_clear(&nm);
}

/**
 * Escapes in decoration text are expanded.
 */
static void
test_expand_escapes(void)
{
    g_autofree gchar *out = nm_expand_escapes("a\\nb\\tc\\\\d");

    g_assert_cmpstr(out, ==, "a\nb\tc\\d");
}

/**
 * Unrecognised escapes and a trailing backslash survive unchanged.
 */
static void
test_expand_escapes_passthrough(void)
{
    g_autofree gchar *out = nm_expand_escapes("100% \\q \\");

    g_assert_cmpstr(out, ==, "100% \\q \\");
}

/**
 * No decoration text at all yields no string.
 */
static void
test_expand_escapes_null(void)
{
    gchar *out = nm_expand_escapes(NULL);

    g_assert_null(out);
}

/**
 * Decoration is placed either side of the countdown, with escapes expanded.
 */
static void
test_decorate_both_sides(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_consider(&nm, FALSE, NOW + MINUTES(90), NOW + HOURS(2), KEY_LOW);

    g_autofree gchar *out =
        nm_decorate(&nm, NM_DEFAULT_LINES, "In\\n", " to go", NULL);
    g_assert_cmpstr(out, ==, "In\n 01:30 to go");

    nm_clear(&nm);
}

/**
 * Without decoration the output is just the countdown.
 */
static void
test_decorate_no_decoration(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_consider(&nm, FALSE, NOW + MINUTES(1), NOW + HOURS(1), KEY_LOW);

    g_autofree gchar *out = nm_decorate(&nm, NM_DEFAULT_LINES, NULL, NULL, NULL);
    g_assert_cmpstr(out, ==, " 00:01");

    nm_clear(&nm);
}

/**
 * Decoration applies to the "no meeting" marker too.
 */
static void
test_decorate_no_meeting(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    g_autofree gchar *out = nm_decorate(&nm, NM_DEFAULT_LINES, "[", "]", NULL);
    g_assert_cmpstr(out, ==, "[----]");

    nm_clear(&nm);
}

/**
 * Decorating only one side leaves the other untouched.
 */
static void
test_decorate_one_side(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1), KEY_LOW);

    g_autofree gchar *before_only =
        nm_decorate(&nm, NM_DEFAULT_LINES, "<", NULL, NULL);
    g_assert_cmpstr(before_only, ==, "< 00:30");

    g_autofree gchar *after_only =
        nm_decorate(&nm, NM_DEFAULT_LINES, NULL, ">", NULL);
    g_assert_cmpstr(after_only, ==, " 00:30>");

    nm_clear(&nm);
}

/**
 * Decoration wraps the whole block of countdowns once, rather than repeating
 * around every line.
 */
static void
test_decorate_wraps_whole_block(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_consider(&nm, FALSE, NOW - MINUTES(5), NOW + MINUTES(5), KEY_LOW);
    nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1), KEY_LOW);

    g_autofree gchar *out = nm_decorate(&nm, NM_DEFAULT_LINES, "[", "]", NULL);
    g_assert_cmpstr(out, ==, "[-00:05\n 00:30]");

    nm_clear(&nm);
}

/**
 * A --fmt value must be made only of #[...] tags: no bare text, no unclosed
 * tag, no control character, but an empty value is fine (equivalent to
 * never setting that key).
 */
static void
test_validates_format_values(void)
{
    g_assert_true(nm_is_valid_format_value(""));
    g_assert_true(nm_is_valid_format_value("#[fg=red]"));
    g_assert_true(nm_is_valid_format_value("#[fg=red,bold]"));
    g_assert_true(nm_is_valid_format_value("#[fg=red]#[bold]"));
    g_assert_false(nm_is_valid_format_value("plain text"));
    g_assert_false(nm_is_valid_format_value("#[fg=red] extra"));
    g_assert_false(nm_is_valid_format_value("#[fg=red"));
    g_assert_false(nm_is_valid_format_value("#[fg=red]\n"));
    /* "#" not followed by "[" at all. */
    g_assert_false(nm_is_valid_format_value("#nope"));
    /* A control character inside an otherwise well-formed tag: 0x01 hits
     * the "< 0x20" side of the check, 0x7f (DEL) hits the "== 0x7f" side. */
    g_assert_false(nm_is_valid_format_value("#[fg=\x01red]"));
    g_assert_false(nm_is_valid_format_value("#[fg=\x7fred]"));
    g_assert_false(nm_is_valid_format_value(NULL));
}

/**
 * nm_formats_is_empty() is true for NULL and for a freshly nm_formats_init()'d
 * value, and false as soon as any single tag field is set - individually,
 * so no field is skipped by a short-circuited check.
 */
static void
test_formats_is_empty(void)
{
    g_assert_true(nm_formats_is_empty(NULL));

    NmFormats fmt;
    nm_formats_init(&fmt);
    g_assert_true(nm_formats_is_empty(&fmt));

    fmt.start = g_strdup("#[fg=red]");
    g_assert_false(nm_formats_is_empty(&fmt));
    g_free(fmt.start);
    fmt.start = NULL;

    fmt.soon = g_strdup("#[fg=red]");
    g_assert_false(nm_formats_is_empty(&fmt));
    g_free(fmt.soon);
    fmt.soon = NULL;

    fmt.end = g_strdup("#[fg=red]");
    g_assert_false(nm_formats_is_empty(&fmt));
    g_free(fmt.end);
    fmt.end = NULL;

    fmt.ending = g_strdup("#[fg=red]");
    g_assert_false(nm_formats_is_empty(&fmt));
    g_free(fmt.ending);
    fmt.ending = NULL;

    fmt.none = g_strdup("#[fg=red]");
    g_assert_false(nm_formats_is_empty(&fmt));
    g_free(fmt.none);
    fmt.none = NULL;

    g_assert_true(nm_formats_is_empty(&fmt));
}

/**
 * One --fmt KEYS=TAGS sets every key in a comma-separated list to the same
 * tag; a later call for the same key overrides the earlier value, and other
 * keys set alongside it are left alone.
 */
static void
test_apply_fmt_sets_multiple_keys(void)
{
    NmFormats fmt;
    nm_formats_init(&fmt);

    GError *error = NULL;

    g_assert_true(nm_apply_fmt(&fmt, "start,end,none=#[fg=gray]", &error));
    g_assert_no_error(error);
    g_assert_cmpstr(fmt.start, ==, "#[fg=gray]");
    g_assert_cmpstr(fmt.end, ==, "#[fg=gray]");
    g_assert_cmpstr(fmt.none, ==, "#[fg=gray]");
    g_assert_null(fmt.soon);
    g_assert_null(fmt.ending);

    g_assert_true(nm_apply_fmt(&fmt, "soon,ending=#[fg=red,bold]", &error));
    g_assert_no_error(error);
    g_assert_cmpstr(fmt.soon, ==, "#[fg=red,bold]");
    g_assert_cmpstr(fmt.ending, ==, "#[fg=red,bold]");

    g_assert_true(nm_apply_fmt(&fmt, "end=#[fg=red,bold]", &error));
    g_assert_no_error(error);
    g_assert_cmpstr(fmt.start, ==, "#[fg=gray]");
    g_assert_cmpstr(fmt.end, ==, "#[fg=red,bold]");

    nm_formats_clear(&fmt);
}

/**
 * A bad --fmt argument - no "=", an empty key list, an unknown key or a
 * value that is not made only of #[...] tags - is rejected, reports a
 * message, and leaves the formats untouched.
 */
static void
test_apply_fmt_rejects_bad_specs(void)
{
    NmFormats fmt;
    nm_formats_init(&fmt);

    GError *error = NULL;

    g_assert_false(nm_apply_fmt(&fmt, "start", &error));
    g_assert_nonnull(error);
    g_clear_error(&error);

    g_assert_false(nm_apply_fmt(&fmt, "=#[fg=red]", &error));
    g_assert_nonnull(error);
    g_clear_error(&error);

    g_assert_false(nm_apply_fmt(&fmt, "nope=#[fg=red]", &error));
    g_assert_nonnull(error);
    g_clear_error(&error);

    g_assert_false(nm_apply_fmt(&fmt, "start=plain text", &error));
    g_assert_nonnull(error);
    g_clear_error(&error);

    g_assert_true(nm_formats_is_empty(&fmt));

    nm_formats_clear(&fmt);
}

/**
 * --soon/--ending must be at least 1; a valid value converts cleanly to
 * seconds, and the error message names the flag that was given.
 */
static void
test_threshold_seconds(void)
{
    gint64 seconds = 0;
    GError *error = NULL;

    g_assert_true(nm_threshold_seconds("--soon", 10, &seconds, &error));
    g_assert_no_error(error);
    g_assert_cmpint(seconds, ==, 600);

    g_assert_false(nm_threshold_seconds("--soon", 0, &seconds, &error));
    g_assert_nonnull(error);
    g_assert_nonnull(strstr(error->message, "--soon"));
    g_clear_error(&error);

    g_assert_false(nm_threshold_seconds("--ending", -1, &seconds, &error));
    g_assert_nonnull(error);
    g_assert_nonnull(strstr(error->message, "--ending"));
    g_clear_error(&error);
}

/**
 * With no --fmt key set at all (NULL, or a freshly nm_formats_init()'d
 * value), rendering is byte-identical to the plain, untagged output.
 */
static void
test_format_stays_plain_without_fmt(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1), KEY_LOW);

    NmFormats fmt;
    nm_formats_init(&fmt);

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, &fmt);
    g_assert_cmpstr(out, ==, " 00:30");

    nm_formats_clear(&fmt);
    nm_clear(&nm);
}

/**
 * Once any --fmt key is set, a start line at or beyond the --soon threshold
 * uses the "start" tag, and an end line at or beyond the --ending threshold
 * uses the "end" tag.
 */
static void
test_format_applies_start_and_end_tags(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    /* 30 minutes to start, 40 minutes left to run: both at/above the 10
     * minute defaults, so neither counts as "soon"/"ending". */
    nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + MINUTES(70), KEY_LOW);
    nm_consider(&nm, FALSE, NOW - MINUTES(20), NOW + MINUTES(40), KEY_HIGH);

    NmFormats fmt;
    nm_formats_init(&fmt);
    fmt.start = g_strdup("#[fg=blue]");
    fmt.end = g_strdup("#[fg=green]");

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, &fmt);
    g_assert_cmpstr(out, ==, "#[fg=blue] 00:30\n#[fg=green]-00:40");

    nm_formats_clear(&fmt);
    nm_clear(&nm);
}

/**
 * A start within --soon minutes uses "soon" instead of "start", and an end
 * within --ending minutes uses "ending" instead of "end". The two
 * thresholds are independent: changing --soon does not affect the --ending
 * boundary or vice versa.
 */
static void
test_format_applies_soon_and_ending_tags(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    /* 5 minutes to start (soon), 3 minutes left to run (ending). */
    nm_consider(&nm, FALSE, NOW + MINUTES(5), NOW + MINUTES(45), KEY_LOW);
    nm_consider(&nm, FALSE, NOW - MINUTES(57), NOW + MINUTES(3), KEY_HIGH);

    NmFormats fmt;
    nm_formats_init(&fmt);
    fmt.start = g_strdup("#[fg=blue]");
    fmt.soon = g_strdup("#[fg=yellow,bold]");
    fmt.end = g_strdup("#[fg=green]");
    fmt.ending = g_strdup("#[fg=red,bold]");

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, &fmt);
    g_assert_cmpstr(out, ==,
                    "#[fg=red,bold]-00:03\n#[fg=yellow,bold] 00:05");

    nm_formats_clear(&fmt);
    nm_clear(&nm);
}

/**
 * The --soon/--ending boundary is exclusive on the "soon"/"ending" side: a
 * remaining time exactly equal to the threshold still counts as
 * "start"/"end", and one second less already counts as "soon"/"ending".
 */
static void
test_soon_and_ending_boundary(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    /* Exactly 10 minutes to start: not yet "soon" at the default threshold. */
    nm_consider(&nm, FALSE, NOW + MINUTES(10), NOW + MINUTES(70), KEY_LOW);

    NmFormats fmt;
    nm_formats_init(&fmt);
    fmt.start = g_strdup("#[fg=blue]");
    fmt.soon = g_strdup("#[fg=yellow]");

    g_autofree gchar *at_threshold = nm_format(&nm, NM_DEFAULT_LINES, &fmt);
    g_assert_cmpstr(at_threshold, ==, "#[fg=blue] 00:10");

    nm_clear(&nm);
    nm_init(&nm, NOW);

    /* One second under 10 minutes: now "soon". */
    nm_consider(&nm, FALSE, NOW + MINUTES(10) - 1, NOW + MINUTES(70),
               KEY_LOW);

    g_autofree gchar *under_threshold =
        nm_format(&nm, NM_DEFAULT_LINES, &fmt);
    g_assert_cmpstr(under_threshold, ==, "#[fg=yellow] 00:09");

    nm_formats_clear(&fmt);
    nm_clear(&nm);
}

/**
 * Once any --fmt key is set, a line whose own key was never set renders
 * with the plain #NM_DEFAULT_TAG - it never borrows another key's tag, even
 * a closely related one such as "start" for an unset "soon".
 */
static void
test_format_unset_key_falls_back_to_default_tag(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    /* Within the "soon" window, but only "start" is configured. */
    nm_consider(&nm, FALSE, NOW + MINUTES(1), NOW + MINUTES(30), KEY_LOW);

    NmFormats fmt;
    nm_formats_init(&fmt);
    fmt.start = g_strdup("#[fg=blue]");

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, &fmt);
    g_assert_cmpstr(out, ==, NM_DEFAULT_TAG " 00:01");

    nm_formats_clear(&fmt);
    nm_clear(&nm);
}

/**
 * With no meetings left, the "----" marker gets the "none" tag if set, or
 * #NM_DEFAULT_TAG if any other --fmt key was set but not "none".
 */
static void
test_format_none_tag(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    NmFormats fmt;
    nm_formats_init(&fmt);
    fmt.none = g_strdup("#[fg=green]");

    g_autofree gchar *out = nm_format(&nm, NM_DEFAULT_LINES, &fmt);
    g_assert_cmpstr(out, ==, "#[fg=green]----");

    nm_formats_clear(&fmt);
    nm_formats_init(&fmt);
    fmt.start = g_strdup("#[fg=blue]");

    g_autofree gchar *fallback = nm_format(&nm, NM_DEFAULT_LINES, &fmt);
    g_assert_cmpstr(fallback, ==, NM_DEFAULT_TAG "----");

    nm_formats_clear(&fmt);
    nm_clear(&nm);
}

/**
 * --before/--after text is never tagged, even with --fmt active: only the
 * countdown lines nm_format() itself renders get a leading tag.
 */
static void
test_decorate_does_not_tag_before_after(void)
{
    NextMeeting nm;
    nm_init(&nm, NOW);

    nm_consider(&nm, FALSE, NOW + MINUTES(30), NOW + HOURS(1), KEY_LOW);

    NmFormats fmt;
    nm_formats_init(&fmt);
    fmt.start = g_strdup("#[fg=blue]");

    g_autofree gchar *out =
        nm_decorate(&nm, NM_DEFAULT_LINES, "Next: ", " to go", &fmt);
    g_assert_cmpstr(out, ==, "Next: #[fg=blue] 00:30 to go");

    nm_formats_clear(&fmt);
    nm_clear(&nm);
}


static void
test_help_summary(void)
{
    g_autofree gchar *out = nm_help_summary("1.2.3");

    g_assert_nonnull(strstr(out, "gnome-next-meeting"));
    g_assert_nonnull(strstr(out, "1.2.3"));
}

/**
 * A missing version is reported rather than producing a malformed summary.
 */
static void
test_help_summary_without_version(void)
{
    g_autofree gchar *out = nm_help_summary(NULL);

    g_assert_nonnull(strstr(out, "gnome-next-meeting"));
    g_assert_nonnull(strstr(out, "unknown"));
}

int
main(int argc, char *argv[])
{
    g_test_init(&argc, &argv, NULL);

    g_test_add_func("/next-meeting/no-meeting", test_no_meeting);
    g_test_add_func("/next-meeting/future-meeting", test_future_meeting);
    g_test_add_func("/next-meeting/in-progress", test_meeting_in_progress);
    g_test_add_func("/next-meeting/started-yesterday",
                    test_meeting_started_yesterday);
    g_test_add_func("/next-meeting/spanning-whole-day",
                    test_meeting_spanning_whole_day);
    g_test_add_func("/next-meeting/starting-exactly-now",
                    test_meeting_starting_exactly_now);
    g_test_add_func("/next-meeting/one-second-away",
                    test_meeting_one_second_away);
    g_test_add_func("/next-meeting/seconds-truncated",
                    test_seconds_are_truncated);
    g_test_add_func("/next-meeting/finished-ignored",
                    test_finished_meetings_ignored);
    g_test_add_func("/next-meeting/ending-one-second-away",
                    test_meeting_ending_one_second_away);
    g_test_add_func("/next-meeting/all-day-ignored", test_all_day_ignored);
    g_test_add_func("/next-meeting/zero-length", test_zero_length_meeting);
    g_test_add_func("/next-meeting/end-before-start",
                    test_end_before_start_treated_as_zero_length);
    g_test_add_func("/next-meeting/sorted-soonest-first",
                    test_events_sorted_soonest_first);
    g_test_add_func("/next-meeting/max-lines-truncates",
                    test_max_lines_truncates);
    g_test_add_func("/next-meeting/max-lines-bounds", test_max_lines_bounds);
    g_test_add_func("/next-meeting/simultaneous-by-key",
                    test_simultaneous_events_ordered_by_key);
    g_test_add_func("/next-meeting/simultaneous-by-kind",
                    test_simultaneous_events_ordered_by_kind);
    g_test_add_func("/next-meeting/key-distinguishes-fields",
                    test_key_distinguishes_fields);
    g_test_add_func("/next-meeting/key-pinned", test_key_is_pinned);
    g_test_add_func("/next-meeting/multi-hour", test_multi_hour_formatting);
    g_test_add_func("/next-meeting/end-past-midnight", test_end_past_midnight);
    g_test_add_func("/next-meeting/long-countdown-clamped",
                    test_long_countdown_clamped);
    g_test_add_func("/next-meeting/reinit-clears-state",
                    test_reinit_clears_state);
    g_test_add_func("/next-meeting/clear-idempotent",
                    test_clear_is_idempotent);
    g_test_add_func("/next-meeting/clashing-meetings", test_clashing_meetings);
    g_test_add_func("/next-meeting/duplicate-collapses",
                    test_duplicate_event_collapses);
    g_test_add_func("/next-meeting/duplicate-keeps-line",
                    test_duplicate_does_not_consume_a_line);
    g_test_add_func("/next-meeting/clock-moved-forward",
                    test_clock_moved_forward);
    g_test_add_func("/next-meeting/expand-escapes", test_expand_escapes);
    g_test_add_func("/next-meeting/expand-escapes-passthrough",
                    test_expand_escapes_passthrough);
    g_test_add_func("/next-meeting/expand-escapes-null",
                    test_expand_escapes_null);
    g_test_add_func("/next-meeting/decorate-both-sides",
                    test_decorate_both_sides);
    g_test_add_func("/next-meeting/decorate-no-decoration",
                    test_decorate_no_decoration);
    g_test_add_func("/next-meeting/decorate-no-meeting",
                    test_decorate_no_meeting);
    g_test_add_func("/next-meeting/decorate-one-side", test_decorate_one_side);
    g_test_add_func("/next-meeting/decorate-wraps-whole-block",
                    test_decorate_wraps_whole_block);
    g_test_add_func("/next-meeting/validates-format-values",
                    test_validates_format_values);
    g_test_add_func("/next-meeting/formats-is-empty", test_formats_is_empty);
    g_test_add_func("/next-meeting/apply-fmt-sets-multiple-keys",
                    test_apply_fmt_sets_multiple_keys);
    g_test_add_func("/next-meeting/apply-fmt-rejects-bad-specs",
                    test_apply_fmt_rejects_bad_specs);
    g_test_add_func("/next-meeting/threshold-seconds", test_threshold_seconds);
    g_test_add_func("/next-meeting/format-stays-plain-without-fmt",
                    test_format_stays_plain_without_fmt);
    g_test_add_func("/next-meeting/format-start-and-end-tags",
                    test_format_applies_start_and_end_tags);
    g_test_add_func("/next-meeting/format-soon-and-ending-tags",
                    test_format_applies_soon_and_ending_tags);
    g_test_add_func("/next-meeting/soon-and-ending-boundary",
                    test_soon_and_ending_boundary);
    g_test_add_func("/next-meeting/format-unset-key-falls-back-to-default",
                    test_format_unset_key_falls_back_to_default_tag);
    g_test_add_func("/next-meeting/format-none-tag", test_format_none_tag);
    g_test_add_func("/next-meeting/decorate-does-not-tag-before-after",
                    test_decorate_does_not_tag_before_after);
    g_test_add_func("/next-meeting/help-summary", test_help_summary);
    g_test_add_func("/next-meeting/help-summary-without-version",
                    test_help_summary_without_version);

    return g_test_run();
}
