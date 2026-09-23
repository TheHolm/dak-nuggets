project('gnome-next-meeting', 'c',
  version: '0.2.0',
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

executable('gnome-next-meeting',
  'main.c',
  link_with: next_meeting,
  dependencies: [glib_dep, eds_dep, ecal_dep],
  install: true)

test_next_meeting = executable('test-next-meeting',
  'test_next_meeting.c',
  link_with: next_meeting,
  dependencies: glib_dep)

test('next-meeting', test_next_meeting)
