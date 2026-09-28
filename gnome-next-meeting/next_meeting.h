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
 * NM_DEFAULT_SOON_MINUTES:
 *
 * Default value of --soon: a not-yet-started meeting counts as "soon" when
 * fewer than this many minutes remain until it starts.
 */
#define NM_DEFAULT_SOON_MINUTES 10

/**
 * NM_DEFAULT_ENDING_MINUTES:
 *
 * Default value of --ending: a meeting in progress counts as "ending" when
 * fewer than this many minutes remain until it ends.
 */
#define NM_DEFAULT_ENDING_MINUTES 10

/**
 * NM_DEFAULT_TAG:
 *
 * The DAK tag applied to a countdown line whose own --fmt key was never set,
 * once at least one --fmt key has been set at all (see
 * nm_formats_is_empty()). With no --fmt given, no tag is added anywhere and
 * output stays exactly the pre-0.4.0 plain text.
 */
#define NM_DEFAULT_TAG "#[default]"

/**
 * NM_ERROR:
 *
 * Domain for GError values raised by nm_apply_fmt() and
 * nm_threshold_seconds(). The specific error code carries no meaning beyond
 * "this domain"; callers only ever print error->message.
 */
#define NM_ERROR (nm_error_quark())

/**
 * nm_error_quark:
 *
 * Registers (once) and returns the #NM_ERROR domain's #GQuark. Declared so
 * NM_ERROR can be used from any file that includes this header; defined with
 * G_DEFINE_QUARK() in next_meeting.c.
 *
 * Returns: the #NM_ERROR quark.
 */
GQuark nm_error_quark(void);

/**
 * NmError:
 * @NM_ERROR_BAD_ARGUMENT: a --fmt/--soon/--ending argument was malformed
 *
 * Error codes for #NM_ERROR. There is only one: every error this program can
 * raise here is a bad command-line argument, reported via error->message.
 */
typedef enum {
    NM_ERROR_BAD_ARGUMENT
} NmError;

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
 * NmFormats:
 * @start: (nullable): DAK tag for a not-yet-started meeting whose start is
 *         --soon minutes away or more
 * @soon: (nullable): DAK tag for a not-yet-started meeting whose start is
 *        fewer than --soon minutes away
 * @end: (nullable): DAK tag for a meeting in progress whose end is --ending
 *       minutes away or more
 * @ending: (nullable): DAK tag for a meeting in progress whose end is fewer
 *          than --ending minutes away
 * @none: (nullable): DAK tag for the "----" no-meeting marker
 * @soon_seconds: threshold, in seconds, below which a not-yet-started
 *                meeting's remaining time makes it use @soon rather than
 *                @start
 * @ending_seconds: threshold, in seconds, below which a meeting in
 *                  progress's remaining time makes it use @ending rather
 *                  than @end
 *
 * DAK #[...] tag overrides for every kind of countdown line, built up by
 * repeated --fmt KEYS=TAGS command-line arguments (nm_apply_fmt()) and the
 * --soon/--ending thresholds (nm_threshold_seconds()).
 *
 * Every field covers its own lines only: there is no fallback between
 * fields, so a line whose own key was never set renders with
 * #NM_DEFAULT_TAG regardless of what any other key was set to. A freshly
 * nm_formats_init()'d value (no --fmt given at all) renders exactly the old
 * plain text with no tags anywhere; see nm_formats_is_empty().
 */
typedef struct {
    gchar *start;
    gchar *soon;
    gchar *end;
    gchar *ending;
    gchar *none;
    gint64 soon_seconds;
    gint64 ending_seconds;
} NmFormats;

/**
 * nm_formats_init:
 * @fmt: the value to initialise
 *
 * Resets @fmt to "no --fmt given": every tag NULL, and both thresholds at
 * their documented defaults (#NM_DEFAULT_SOON_MINUTES,
 * #NM_DEFAULT_ENDING_MINUTES). Release the result with nm_formats_clear().
 */
void nm_formats_init(NmFormats *fmt);

/**
 * nm_formats_clear:
 * @fmt: the value to release
 *
 * Frees every tag nm_apply_fmt() allocated and leaves @fmt safe to
 * nm_formats_init() again. Calling it twice in a row is harmless. The
 * threshold fields are plain integers and need no release.
 */
void nm_formats_clear(NmFormats *fmt);

/**
 * nm_formats_is_empty:
 * @fmt: (nullable): the value to check
 *
 * TRUE when no --fmt key at all has been set (or @fmt is NULL), in which
 * case rendering must stay byte-identical to the plain, untagged output.
 * The threshold fields do not affect this: --soon/--ending alone, with no
 * matching --fmt key, changes nothing about the output.
 *
 * Returns: TRUE if @fmt has every tag field NULL, or is NULL itself.
 */
gboolean nm_formats_is_empty(const NmFormats *fmt);

/**
 * nm_is_valid_format_value:
 * @value: the value to check
 *
 * Checks that a --fmt value (the TAGS half of KEYS=TAGS) is made only of
 * DAK #[...] tags: no bare text, no control characters, and every "#["
 * closed by a "]" in the same value. An empty string is valid (equivalent to
 * not setting that key). Keeping this strict is what keeps every rendered
 * line six characters wide, since tags themselves add no visible width but
 * arbitrary text would.
 *
 * Returns: TRUE if @value is valid.
 */
gboolean nm_is_valid_format_value(const char *value);

/**
 * nm_apply_fmt:
 * @fmt: the value to update in place
 * @spec: one --fmt argument, shaped "KEYS=TAGS"
 * @error: (nullable): set on failure
 *
 * Applies one --fmt KEYS=TAGS argument to @fmt. KEYS is one or more
 * comma-separated keys from start/soon/end/ending/none; TAGS is applied to
 * every named key, replacing (and freeing) any value that key already had.
 * A later call for the same key overrides an earlier one.
 *
 * Fails if @spec has no "=", KEYS is empty, a key is not one of the five
 * above, or TAGS fails nm_is_valid_format_value().
 *
 * Returns: TRUE on success.
 */
gboolean nm_apply_fmt(NmFormats *fmt, const char *spec, GError **error);

/**
 * nm_threshold_seconds:
 * @flag: the option's own name (e.g. "--soon"), used only in the error
 *        message
 * @minutes: the value given to that option
 * @out_seconds: (out): set to @minutes converted to seconds on success
 * @error: (nullable): set on failure
 *
 * Validates and converts a --soon/--ending argument. @minutes must be at
 * least 1: unlike a --fmt key being left unset (which is how "I don't want
 * this styled" is spelled), a threshold of zero or less has no sensible
 * meaning, so it is rejected rather than silently clamped.
 *
 * Returns: TRUE on success.
 */
gboolean nm_threshold_seconds(const char *flag,
                              gint minutes,
                              gint64 *out_seconds,
                              GError **error);

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
 * @fmt: (nullable): --fmt/--soon/--ending overrides; NULL is equivalent to
 *       an nm_formats_init()'d value with no key set
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
 * With @fmt empty (see nm_formats_is_empty()), every line is exactly as
 * above with no tag added. Otherwise every line - including "----" - gets a
 * leading DAK tag: the one configured for that line's key
 * (start/soon/end/ending/none), or #NM_DEFAULT_TAG if that particular key
 * was not set, so no style can carry from one line into the next.
 *
 * Returns: (transfer full): a newly allocated string; free with g_free().
 */
gchar *nm_format(NextMeeting *nm, guint max_lines, const NmFormats *fmt);

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
 * @fmt: (nullable): --fmt/--soon/--ending overrides, as for nm_format()
 *
 * Renders @nm like nm_format(), with the optional @before and @after
 * decoration wrapped once around the whole block of countdown lines rather
 * than around each line. Escapes in @before and @after are expanded with
 * nm_expand_escapes(), so newlines and tabs can be embedded from the command
 * line. The decoration applies equally to the "----" no-meeting marker. The
 * caller is responsible for any trailing newline.
 *
 * @before and @after are never tagged, even with @fmt set: only the
 * countdown lines nm_format() itself renders get a tag. If @after is placed
 * right after a tagged countdown line, it inherits that line's style until
 * DAK's markup parser sees something else - callers who need @after left
 * untouched can put "#[default]" in it themselves.
 *
 * Returns: (transfer full): a newly allocated string; free with g_free().
 */
gchar *nm_decorate(NextMeeting *nm,
                   guint max_lines,
                   const char *before,
                   const char *after,
                   const NmFormats *fmt);

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
