#include "next_meeting.h"

#include <stdio.h>
#include <string.h>

G_DEFINE_QUARK(nm-error, nm_error)

/*
 * Field separator mixed into nm_key() between the UID, the recurrence
 * discriminator and the summary, so that moving characters across a field
 * boundary still changes the hash.
 */
#define NM_KEY_SEPARATOR 0x1f

/*
 * 32-bit FNV-1a parameters. Spelled out here rather than using g_str_hash()
 * so the ordering of simultaneous events is pinned to a documented algorithm
 * which cannot change under us.
 */
#define NM_FNV_OFFSET_BASIS 2166136261u
#define NM_FNV_PRIME 16777619u

/*
 * Widest countdown the six-character line format can hold.
 */
#define NM_MAX_HOURS 99
#define NM_MAX_MINUTES 59

void
nm_init(NextMeeting *nm, time_t now)
{
    nm->now = now;
    nm->events = g_array_new(FALSE, FALSE, sizeof(NmEvent));
}

void
nm_clear(NextMeeting *nm)
{
    if (nm->events != NULL) {
        g_array_free(nm->events, TRUE);
        nm->events = NULL;
    }
}

void
nm_formats_init(NmFormats *fmt)
{
    fmt->start = NULL;
    fmt->soon = NULL;
    fmt->end = NULL;
    fmt->ending = NULL;
    fmt->none = NULL;
    fmt->soon_seconds = (gint64)NM_DEFAULT_SOON_MINUTES * 60;
    fmt->ending_seconds = (gint64)NM_DEFAULT_ENDING_MINUTES * 60;
}

void
nm_formats_clear(NmFormats *fmt)
{
    g_free(fmt->start);
    g_free(fmt->soon);
    g_free(fmt->end);
    g_free(fmt->ending);
    g_free(fmt->none);

    fmt->start = NULL;
    fmt->soon = NULL;
    fmt->end = NULL;
    fmt->ending = NULL;
    fmt->none = NULL;
}

gboolean
nm_formats_is_empty(const NmFormats *fmt)
{
    return fmt == NULL ||
           (fmt->start == NULL && fmt->soon == NULL && fmt->end == NULL &&
            fmt->ending == NULL && fmt->none == NULL);
}

gboolean
nm_is_valid_format_value(const char *value)
{
    if (value == NULL) {
        return FALSE;
    }

    const char *p = value;

    while (*p != '\0') {
        if (*p != '#') {
            return FALSE;
        }

        p++;

        if (*p != '[') {
            return FALSE;
        }

        p++;

        gboolean closed = FALSE;

        while (*p != '\0') {
            if (*p == ']') {
                closed = TRUE;
                p++;
                break;
            }

            /*
             * ASCII control characters (including newline) are never valid
             * inside a tag; UTF-8 continuation/lead bytes are all >= 0x80
             * and so never match this check.
             */
            if ((guchar)*p < 0x20 || (guchar)*p == 0x7f) {
                return FALSE;
            }

            p++;
        }

        if (!closed) {
            return FALSE;
        }
    }

    return TRUE;
}

/*
 * Points *target at a copy of tags, freeing whatever it pointed at before.
 * Used by nm_apply_fmt() once per key named in a KEYS=TAGS argument.
 */
static void
nm_set_tag(gchar **target, const char *tags)
{
    g_free(*target);
    *target = g_strdup(tags);
}

gboolean
nm_apply_fmt(NmFormats *fmt, const char *spec, GError **error)
{
    const char *eq = strchr(spec, '=');

    if (eq == NULL || eq == spec) {
        g_set_error(error, NM_ERROR, NM_ERROR_BAD_ARGUMENT,
                    "--fmt: expected KEYS=TAGS, got '%s'", spec);
        return FALSE;
    }

    const char *tags = eq + 1;

    if (!nm_is_valid_format_value(tags)) {
        g_set_error(error, NM_ERROR, NM_ERROR_BAD_ARGUMENT,
                    "--fmt: value must be made only of #[...] tags: '%s'",
                    tags);
        return FALSE;
    }

    g_autofree gchar *keys_part = g_strndup(spec, (gsize)(eq - spec));
    g_auto(GStrv) keys = g_strsplit(keys_part, ",", -1);

    for (gsize i = 0; keys[i] != NULL; i++) {
        if (g_strcmp0(keys[i], "start") == 0) {
            nm_set_tag(&fmt->start, tags);
        } else if (g_strcmp0(keys[i], "soon") == 0) {
            nm_set_tag(&fmt->soon, tags);
        } else if (g_strcmp0(keys[i], "end") == 0) {
            nm_set_tag(&fmt->end, tags);
        } else if (g_strcmp0(keys[i], "ending") == 0) {
            nm_set_tag(&fmt->ending, tags);
        } else if (g_strcmp0(keys[i], "none") == 0) {
            nm_set_tag(&fmt->none, tags);
        } else {
            g_set_error(error, NM_ERROR, NM_ERROR_BAD_ARGUMENT,
                        "--fmt: unknown key '%s'", keys[i]);
            return FALSE;
        }
    }

    return TRUE;
}

gboolean
nm_threshold_seconds(const char *flag,
                     gint minutes,
                     gint64 *out_seconds,
                     GError **error)
{
    if (minutes < 1) {
        g_set_error(error, NM_ERROR, NM_ERROR_BAD_ARGUMENT,
                    "%s must be at least 1", flag);
        return FALSE;
    }

    *out_seconds = (gint64)minutes * 60;

    return TRUE;
}

/*
 * Folds one byte into an in-progress FNV-1a hash.
 */
static void
nm_key_feed_byte(guint32 *hash, guchar byte)
{
    *hash ^= byte;
    *hash *= NM_FNV_PRIME;
}

/*
 * Folds one field, followed by a separator, into an in-progress FNV-1a hash.
 * A NULL field contributes nothing but its separator, so it is equivalent to
 * an empty one.
 */
static void
nm_key_feed_field(guint32 *hash, const char *field)
{
    if (field != NULL) {
        for (const char *p = field; *p != '\0'; p++) {
            nm_key_feed_byte(hash, (guchar)*p);
        }
    }

    nm_key_feed_byte(hash, NM_KEY_SEPARATOR);
}

guint32
nm_key(const char *uid, const char *recurrence_id, const char *summary)
{
    guint32 hash = NM_FNV_OFFSET_BASIS;

    nm_key_feed_field(&hash, uid);
    nm_key_feed_field(&hash, recurrence_id);
    nm_key_feed_field(&hash, summary);

    return hash;
}

gboolean
nm_consider(NextMeeting *nm,
            gboolean is_date,
            time_t start,
            time_t end,
            guint32 key)
{
    /*
     * All-day events have no meaningful HH:MM countdown, and a DATE-only
     * value does not carry a reliable timezone, so skip them entirely.
     */
    if (is_date) {
        return FALSE;
    }

    /*
     * Defend against a malformed instance whose end precedes its start by
     * treating it as a zero-length meeting.
     */
    if (end < start) {
        end = start;
    }

    /*
     * Anything already over is of no interest.
     */
    if (end <= nm->now) {
        return FALSE;
    }

    NmEvent event;

    event.key = key;

    if (start > nm->now) {
        /*
         * Still to come: count down to when it begins.
         */
        event.kind = NM_EVENT_START;
        event.when = start;
    } else {
        /*
         * Already under way: count down to when it frees up, even if that
         * falls after midnight.
         */
        event.kind = NM_EVENT_END;
        event.when = end;
    }

    g_array_append_val(nm->events, event);

    return TRUE;
}

/*
 * Orders two events soonest-first, falling back to the meetings' identity
 * hashes and then to the event kind so that the result is total and does not
 * depend on the order the calendars were read in.
 */
static gint
nm_event_compare(gconstpointer a, gconstpointer b)
{
    const NmEvent *left = a;
    const NmEvent *right = b;

    if (left->when != right->when) {
        return left->when < right->when ? -1 : 1;
    }

    if (left->key != right->key) {
        return left->key < right->key ? -1 : 1;
    }

    if (left->kind != right->kind) {
        return left->kind < right->kind ? -1 : 1;
    }

    return 0;
}

void
nm_sort(NextMeeting *nm)
{
    g_array_sort(nm->events, nm_event_compare);

    /*
     * Collapse exact duplicates, which arise when the same meeting is
     * subscribed in more than one enabled calendar: the copies share a UID, so
     * they land on the same time, identity hash and kind, and would otherwise
     * spend two lines of a three-line display saying the same thing. Walking
     * backwards keeps the surviving indices stable as entries are removed.
     */
    for (guint i = nm->events->len; i > 1; i--) {
        const NmEvent *previous = &g_array_index(nm->events, NmEvent, i - 2);
        const NmEvent *current = &g_array_index(nm->events, NmEvent, i - 1);

        if (nm_event_compare(previous, current) == 0) {
            g_array_remove_index(nm->events, i - 1);
        }
    }
}

/*
 * Renders one event as the six-character countdown line it occupies,
 * optionally preceded by the DAK tag fmt selects for it.
 */
static gchar *
nm_format_event(const NextMeeting *nm, const NmEvent *event,
                const NmFormats *fmt)
{
    /*
     * Calculate remaining time, truncating seconds. The un-clamped value is
     * also what a --soon/--ending threshold is measured against, so an
     * absurdly distant event compares correctly even once its displayed
     * digits are clamped below.
     */
    time_t remaining = event->when - nm->now;

    if (remaining < 0) {
        remaining = 0;
    }

    long hours = (long)(remaining / 3600);
    long minutes = (long)((remaining % 3600) / 60);

    /*
     * Keep the line six characters wide even for an absurdly distant end.
     */
    if (hours > NM_MAX_HOURS) {
        hours = NM_MAX_HOURS;
        minutes = NM_MAX_MINUTES;
    }

    /*
     * A leading "-" marks a countdown to the end of a meeting in progress; a
     * leading space marks a countdown to one which has yet to start, so both
     * kinds line up in the same columns.
     */
    char marker = event->kind == NM_EVENT_END ? '-' : ' ';

    gchar *body = g_strdup_printf("%c%02ld:%02ld", marker, hours, minutes);

    if (nm_formats_is_empty(fmt)) {
        return body;
    }

    /*
     * Each line picks its own key - never a fallback to another key - so a
     * key left unset by the caller renders as NM_DEFAULT_TAG regardless of
     * what any other key was set to.
     */
    const gchar *tag;

    if (event->kind == NM_EVENT_END) {
        tag = remaining < fmt->ending_seconds ? fmt->ending : fmt->end;
    } else {
        tag = remaining < fmt->soon_seconds ? fmt->soon : fmt->start;
    }

    gchar *tagged = g_strconcat(tag != NULL ? tag : NM_DEFAULT_TAG, body, NULL);

    g_free(body);

    return tagged;
}

gchar *
nm_format(NextMeeting *nm, guint max_lines, const NmFormats *fmt)
{
    /*
     * Nothing left to count down to.
     */
    if (nm->events->len == 0) {
        if (nm_formats_is_empty(fmt)) {
            return g_strdup("----");
        }

        return g_strconcat(fmt->none != NULL ? fmt->none : NM_DEFAULT_TAG,
                           "----", NULL);
    }

    if (max_lines < 1) {
        max_lines = 1;
    }

    nm_sort(nm);

    guint count = MIN(max_lines, nm->events->len);

    GString *out = g_string_new(NULL);

    for (guint i = 0; i < count; i++) {
        const NmEvent *event = &g_array_index(nm->events, NmEvent, i);

        if (i > 0) {
            g_string_append_c(out, '\n');
        }

        gchar *line = nm_format_event(nm, event, fmt);

        g_string_append(out, line);

        g_free(line);
    }

    return g_string_free(out, FALSE);
}

gchar *
nm_expand_escapes(const char *text)
{
    if (text == NULL) {
        return NULL;
    }

    GString *out = g_string_new(NULL);

    for (const char *p = text; *p != '\0'; p++) {
        if (*p != '\\') {
            g_string_append_c(out, *p);
            continue;
        }

        /*
         * A backslash introduces an escape. Recognised ones are replaced;
         * anything else (including a trailing backslash) is kept verbatim so
         * arbitrary text survives untouched.
         */
        switch (p[1]) {
        case 'n':
            g_string_append_c(out, '\n');
            p++;
            break;
        case 't':
            g_string_append_c(out, '\t');
            p++;
            break;
        case '\\':
            g_string_append_c(out, '\\');
            p++;
            break;
        default:
            g_string_append_c(out, '\\');
            break;
        }
    }

    return g_string_free(out, FALSE);
}

gchar *
nm_decorate(NextMeeting *nm,
            guint max_lines,
            const char *before,
            const char *after,
            const NmFormats *fmt)
{
    gchar *prefix = nm_expand_escapes(before);
    gchar *body = nm_format(nm, max_lines, fmt);
    gchar *suffix = nm_expand_escapes(after);

    gchar *out =
        g_strconcat(prefix ? prefix : "",
                    body,
                    suffix ? suffix : "",
                    NULL);

    g_free(prefix);
    g_free(body);
    g_free(suffix);

    return out;
}

gchar *
nm_help_summary(const char *version)
{
    return g_strdup_printf(
        "gnome-next-meeting %s - countdowns to your calendar meetings",
        version != NULL ? version : "(unknown version)");
}
