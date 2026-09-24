#ifndef NEXT_MEETING_H
#define NEXT_MEETING_H

#include <glib.h>
#include <time.h>

/**
 * NM_DEFAULT_LINES:
 *
 * Default maximum number of countdown lines printed. Three is what a DAK
 * button LCD can display (the first six characters of the first three lines
 * of a helper's stdout).
 */
#define NM_DEFAULT_LINES 3

/**
 * NmEventKind:
 * @NM_EVENT_START: countdown to a meeting which has not begun yet
 * @NM_EVENT_END: countdown to the end of a meeting already in progress
 *
 * The two kinds of countdown a meeting can contribute. A meeting in progress
 * contributes only an end event; a meeting still to come contributes only a
 * start event.
 *
 * The numeric order matters: it is the last tie-break when sorting events, so
 * a start sorts before an end which falls at the very same second.
 */
typedef enum {
    NM_EVENT_START = 0,
    NM_EVENT_END = 1
} NmEventKind;

/**
 * NmEvent:
 * @kind: whether this counts down to a meeting's start or to its end
 * @when: absolute time of the event, in seconds since the epoch
 * @key: stable identity hash of the meeting, used to break ties between
 *       events falling at the same @when, and to recognise the same meeting
 *       arriving from two different calendars
 *
 * One countdown to display.
 */
typedef struct {
    NmEventKind kind;
    time_t when;
    guint32 key;
} NmEvent;

/**
 * NextMeeting:
 * @now: reference "current" time, in seconds since the epoch
 * @events: (element-type NmEvent): the countdowns collected so far, in
 *          arrival order until nm_sort() (or nm_format()) orders them
 *
 * Running state while scanning calendar instances for the countdowns to
 * display.
 */
typedef struct {
    time_t now;
    GArray *events;
} NextMeeting;

/**
 * nm_init:
 * @nm: the state to initialise
 * @now: reference "current" time, in seconds since the epoch
 *
 * Resets @nm to "nothing collected yet", relative to @now. Release the
 * result with nm_clear().
 */
void nm_init(NextMeeting *nm, time_t now);

/**
 * nm_clear:
 * @nm: the state to release
 *
 * Frees everything nm_init() allocated and leaves @nm safe to nm_init()
 * again. Calling it twice in a row is harmless.
 */
void nm_clear(NextMeeting *nm);

/**
 * nm_key:
 * @uid: (nullable): the meeting's iCalendar UID
 * @recurrence_id: (nullable): the occurrence discriminator (RECURRENCE-ID,
 *                 or the instance's own start time for an expanded
 *                 recurrence)
 * @summary: (nullable): the meeting's SUMMARY
 *
 * Computes a stable identity hash for one meeting occurrence, used to order
 * events which fall at the very same second. The hash is a 32-bit FNV-1a
 * over the three fields joined by a separator byte, so it depends only on
 * the arguments: the same occurrence sorts the same way on every run, on
 * every platform, and regardless of the GLib version. NULL fields count as
 * empty.
 *
 * Returns: the identity hash.
 */
guint32 nm_key(const char *uid,
               const char *recurrence_id,
               const char *summary);

/**
 * nm_consider:
 * @nm: running state, updated in place
 * @is_date: TRUE if the instance is an all-day (DATE-only) value
 * @start: the instance's start time, in seconds since the epoch
 * @end: the instance's end time, in seconds since the epoch; an end before
 *       @start is treated as a zero-length meeting
 * @key: the instance's identity hash from nm_key()
 *
 * Offers one calendar instance to the collection. All-day values are ignored
 * (a HH:MM countdown does not apply to them), as are meetings which have
 * already ended. A meeting still to come contributes a countdown to its
 * start; a meeting already in progress contributes a countdown to its end,
 * even if that end falls after midnight.
 *
 * Returns: TRUE if the instance contributed a countdown.
 */
gboolean nm_consider(NextMeeting *nm,
                     gboolean is_date,
                     time_t start,
                     time_t end,
                     guint32 key);

/**
 * nm_sort:
 * @nm: the collected state to order and de-duplicate in place
 *
 * Orders the collected events by time, soonest first. Events falling at the
 * same second are ordered by nm_key() hash and finally by NmEventKind, so
 * the output never depends on the order calendars were read in and stays
 * identical from run to run.
 *
 * Events which match in all three respects describe the same meeting
 * occurrence reached twice — which happens when the same meeting is
 * subscribed in more than one enabled calendar — and are collapsed into one,
 * so duplicates cannot consume the few lines available. Because identity is a
 * 32-bit hash, two genuinely different meetings starting at the same second
 * could in principle collide and be collapsed; the odds make that a better
 * trade than carrying full identity strings for every event.
 *
 * nm_format() does this itself; it is exposed separately so the ordering and
 * de-duplication can be tested directly. Running it twice changes nothing.
 */
void nm_sort(NextMeeting *nm);

/**
 * nm_format:
 * @nm: the collected state to render, ordered in place as a side effect
 * @max_lines: how many countdown lines to print at most; values below 1 are
 *             clamped to 1
 *
 * Renders up to @max_lines countdowns, soonest first, one per line separated
 * by "\n" and with no trailing newline. Exact duplicates are collapsed first
 * (see nm_sort()), so they never take a line from a distinct meeting. Each
 * line is exactly six characters: a space and "HH:MM" until a meeting starts,
 * or a "-" and "HH:MM" until a meeting in progress ends. Seconds are
 * truncated, so an event less than a minute away renders as "00:00".
 * Countdowns of 100 hours or more are clamped to "99:59" to keep the
 * six-character width.
 *
 * When there is nothing left to count down to, the single marker "----" is
 * rendered instead.
 *
 * Returns: (transfer full): a newly allocated string; free with g_free().
 */
gchar *nm_format(NextMeeting *nm, guint max_lines);

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
 * @nm: the collected state to render, ordered in place as a side effect
 * @max_lines: how many countdown lines to print at most, as for nm_format()
 * @before: (nullable): text to place before the countdowns, or NULL for none
 * @after: (nullable): text to place after the countdowns, or NULL for none
 *
 * Renders @nm like nm_format(), with the optional @before and @after
 * decoration wrapped once around the whole block of countdown lines rather
 * than around each line. Escapes in @before and @after are expanded with
 * nm_expand_escapes(), so newlines and tabs can be embedded from the command
 * line. The decoration applies equally to the "----" no-meeting marker. The
 * caller is responsible for any trailing newline.
 *
 * Returns: (transfer full): a newly allocated string; free with g_free().
 */
gchar *nm_decorate(NextMeeting *nm,
                   guint max_lines,
                   const char *before,
                   const char *after);

/**
 * nm_help_summary:
 * @version: (nullable): the program version to advertise
 *
 * Builds the one-line summary shown above the option list in --help output,
 * naming the program and @version. A NULL @version is reported as unknown
 * rather than omitted, so the line always has the same shape.
 *
 * Returns: (transfer full): a newly allocated string; free with g_free().
 */
gchar *nm_help_summary(const char *version);

#endif /* NEXT_MEETING_H */
