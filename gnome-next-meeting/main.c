#include <stdio.h>
#include <time.h>

#include <libecal/libecal.h>
#include <libedataserver/libedataserver.h>

#include "next_meeting.h"


/*
 * Called once for every calendar event instance in the requested
 * time range.
 */
static gboolean
instance_cb(ICalComponent *component,
            ICalTime *instance_start,
            ICalTime *instance_end,
            gpointer user_data,
            GCancellable *cancellable,
            GError **error)
{
    NextMeeting *nm = user_data;

    (void)component;
    (void)instance_end;
    (void)cancellable;
    (void)error;

    /*
     * All-day events have no time-of-day component, so a HH:MM
     * countdown does not apply to them, and a DATE-only ICalTime
     * does not carry a reliable timezone for conversion to time_t.
     * Let nm_consider() ignore them; do not convert their start.
     */
    gboolean is_date = i_cal_time_is_date(instance_start);

    time_t start = 0;

    if (!is_date) {
        start =
            i_cal_time_as_timet_with_zone(
                instance_start,
                i_cal_time_get_timezone(instance_start));
    }

    nm_consider(nm, is_date, start);

    return TRUE;
}


int
main(void)
{
    GError *error = NULL;

    /*
     * Current time.
     */
    time_t now = time(NULL);

    /*
     * EDS knows the system timezone and libical timezone database.
     */
    ICalTimezone *timezone = e_cal_util_get_system_timezone();

    if (timezone == NULL) {
        fprintf(stderr, "Unable to determine system timezone\n");
        return 1;
    }

    /*
     * Calculate the beginning and end of today in the local timezone.
     */
    ICalTime *local_now =
        i_cal_time_new_from_timet_with_zone(now, FALSE, timezone);

    ICalTime *today_start = i_cal_time_clone(local_now);
    i_cal_time_set_hour(today_start, 0);
    i_cal_time_set_minute(today_start, 0);
    i_cal_time_set_second(today_start, 0);

    ICalTime *tomorrow_start = i_cal_time_clone(today_start);
    i_cal_time_adjust(tomorrow_start, 1, 0, 0, 0);

    time_t day_end =
        i_cal_time_as_timet_with_zone(tomorrow_start, timezone);

    g_object_unref(local_now);
    g_object_unref(today_start);
    g_object_unref(tomorrow_start);

    /*
     * Connect to Evolution Data Server's source registry.
     */
    ESourceRegistry *registry =
        e_source_registry_new_sync(NULL, &error);

    if (registry == NULL) {
        fprintf(stderr, "Unable to connect to EDS: %s\n",
                error ? error->message : "unknown error");
        g_clear_error(&error);
        return 1;
    }

    /*
     * Get all enabled calendar sources.
     */
    GList *sources =
        e_source_registry_list_enabled(
            registry,
            E_SOURCE_EXTENSION_CALENDAR);

    NextMeeting nm;
    nm_init(&nm, now);

    /*
     * Process every calendar.
     */
    for (GList *link = sources; link != NULL; link = link->next) {
        ESource *source = E_SOURCE(link->data);

        /*
         * e_cal_client_connect_sync() returns the generic EClient
         * base type; it must be cast to ECalClient before use with
         * the calendar-specific calls below.
         */
        EClient *client =
            e_cal_client_connect_sync(
                source,
                E_CAL_CLIENT_SOURCE_TYPE_EVENTS,
                10,
                NULL,
                &error);

        if (client == NULL) {
            /*
             * One broken/offline calendar should not prevent us
             * from checking the others.
             */
            g_clear_error(&error);
            continue;
        }

        ECalClient *cal_client = E_CAL_CLIENT(client);

        /*
         * Tell EDS which timezone to use for floating/date values.
         */
        e_cal_client_set_default_timezone(cal_client, timezone);

        /*
         * Generate actual event instances, including occurrences
         * of recurring meetings, for today.
         */
        e_cal_client_generate_instances_sync(
            cal_client,
            now,
            day_end,
            NULL,
            instance_cb,
            &nm);

        g_object_unref(client);
    }

    g_list_free_full(sources, g_object_unref);
    g_object_unref(registry);

    gchar *out = nm_format(&nm);

    printf("%s\n", out);

    g_free(out);

    return 0;
}
