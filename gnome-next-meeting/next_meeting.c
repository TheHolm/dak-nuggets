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
