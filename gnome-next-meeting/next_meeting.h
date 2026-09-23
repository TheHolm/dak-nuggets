#ifndef NEXT_MEETING_H
#define NEXT_MEETING_H

#include <glib.h>
#include <time.h>

/**
 * NextMeeting:
 * @now: reference "current" time, in seconds since the epoch
 * @found: whether any upcoming meeting has been found yet
 * @start: start time of the nearest upcoming meeting (only meaningful
 *         when @found is TRUE)
 *
 * Running state while scanning calendar instances for the next meeting
 * that has not started yet.
 */
typedef struct {
    time_t now;
    gboolean found;
    time_t start;
} NextMeeting;

/**
 * nm_init:
 * @nm: the state to initialise
 * @now: reference "current" time, in seconds since the epoch
 *
 * Resets @nm to "no meeting found yet", relative to @now.
 */
void nm_init(NextMeeting *nm, time_t now);

/**
 * nm_consider:
 * @nm: running search state, updated in place
 * @is_date: TRUE if the instance is an all-day (DATE-only) value
 * @start: the instance's start time, in seconds since the epoch
 *
 * Offers one calendar instance to the search. All-day values and instances
 * that have already started (or are starting right now) are ignored; any
 * other instance becomes the new nearest meeting if it is closer than
 * everything seen so far.
 *
 * Returns: TRUE if this instance became the new nearest meeting.
 */
gboolean nm_consider(NextMeeting *nm, gboolean is_date, time_t start);

/**
 * nm_format:
 * @nm: the search state to render
 *
 * Renders the time remaining until @nm's meeting as zero-padded "HH:MM", or
 * "----" when no upcoming meeting was found. Seconds are truncated, so a
 * meeting less than a minute away renders as "00:00".
 *
 * Returns: (transfer full): a newly allocated string; free with g_free().
 */
gchar *nm_format(const NextMeeting *nm);

/**
 * nm_expand_escapes:
 * @text: (nullable): the text whose backslash escapes should be expanded
 *
 * Expands the escapes "\n", "\t" and "\\" in @text into a newline, a tab and
 * a literal backslash respectively. Any other backslash sequence is copied
 * through unchanged, as is a trailing backslash.
 *
 * Returns: (transfer full) (nullable): a newly allocated string, or NULL if
 *          @text is NULL; free with g_free().
 */
gchar *nm_expand_escapes(const char *text);

/**
 * nm_decorate:
 * @nm: the search state to render
 * @before: (nullable): text to place before the time, or NULL for none
 * @after: (nullable): text to place after the time, or NULL for none
 *
 * Renders @nm like nm_format(), wrapped in the optional @before and @after
 * decoration. Escapes in @before and @after are expanded with
 * nm_expand_escapes(), so newlines and tabs can be embedded from the command
 * line. The decoration applies equally to the "----" no-meeting marker. The
 * caller is responsible for any trailing newline.
 *
 * Returns: (transfer full): a newly allocated string; free with g_free().
 */
gchar *nm_decorate(const NextMeeting *nm,
                   const char *before,
                   const char *after);

#endif /* NEXT_MEETING_H */
