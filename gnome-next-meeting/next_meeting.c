#include "next_meeting.h"

#include <stdio.h>

void
nm_init(NextMeeting *nm, time_t now)
{
    nm->now = now;
    nm->found = FALSE;
    nm->start = 0;
}

gboolean
nm_consider(NextMeeting *nm, gboolean is_date, time_t start)
{
    /*
     * All-day events have no meaningful HH:MM countdown, and a DATE-only
     * value does not carry a reliable timezone, so skip them entirely.
     */
    if (is_date) {
        return FALSE;
    }

    /*
     * Only meetings which have not started yet count.
     */
    if (start <= nm->now) {
        return FALSE;
    }

    /*
     * Keep whichever future meeting starts soonest.
     */
    if (!nm->found || start < nm->start) {
        nm->found = TRUE;
        nm->start = start;
        return TRUE;
    }

    return FALSE;
}

gchar *
nm_format(const NextMeeting *nm)
{
    /*
     * Nothing later today.
     */
    if (!nm->found) {
        return g_strdup("----");
    }

    /*
     * Calculate remaining time, truncating seconds.
     */
    time_t remaining = nm->start - nm->now;

    long hours = remaining / 3600;
    long minutes = (remaining % 3600) / 60;

    return g_strdup_printf("%02ld:%02ld", hours, minutes);
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
nm_decorate(const NextMeeting *nm,
            const char *before,
            const char *after)
{
    gchar *prefix = nm_expand_escapes(before);
    gchar *body = nm_format(nm);
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
