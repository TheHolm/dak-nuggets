project('gnome-next-meeting', 'c',
  version: '0.4.0',
  license: 'AGPL-3.0-or-later',
  default_options: ['c_std=c99', 'warning_level=2'])

glib_dep = dependency('glib-2.0')
eds_dep = dependency('libedataserver-1.2')
ecal_dep = dependency('libecal-2.0')

# Pure decision/formatting logic, split out from main.c so it can be tested
# without a live Evolution Data Server. See next_meeting.h.
next_meeting = static_library('next_meeting',
  'next_meeting.c',
  dependencies: glib_dep)

gnome_next_meeting = executable('gnome-next-meeting',
  'main.c',
  link_with: next_meeting,
  dependencies: [glib_dep, eds_dep, ecal_dep],
  # The version reaches --help from the project version above, so the two can
  # never disagree and the CI packaging scripts keep a single source of truth.
  c_args: ['-DGNM_VERSION="' + meson.project_version() + '"'],
  install: true)

test_next_meeting = executable('test-next-meeting',
  'test_next_meeting.c',
  link_with: next_meeting,
  dependencies: glib_dep)

test('next-meeting', test_next_meeting)

# Covers main.c's own argument validation (--fmt/--soon/--ending/--lines and
# GLib's own option-parsing errors) by running the real built binary and
# checking its exit status and stderr - see test-cli.sh. This is the one
# slice of main.c that can be exercised without a live EDS session; the rest
# (registry connection, calendar enumeration, i_cal_time_* conversion) stays
# manually verified, per NOTES.md "Known gaps".
test('cli-argument-validation',
  find_program('test-cli.sh'),
  args: [gnome_next_meeting])
