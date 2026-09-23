# Release notes — gnome-next-meeting

## v0.1.0

- Initial version: prints the time remaining until the next calendar event
  of the day (`HH:MM`, or `----` when none remain) by reading the enabled
  calendars from Evolution Data Server.
- Fixed a build error against current EDS (`e_cal_client_connect_sync`
  returns `EClient *`, not `ECalClient *`; needs an `E_CAL_CLIENT()` cast)
  and removed an unused variable, so the program now compiles cleanly with
  Meson and runs successfully against a live EDS session.
- All-day events are now ignored when computing the next meeting: a `HH:MM`
  countdown doesn't apply to them, and their DATE-only value has no reliable
  timezone to convert from.
