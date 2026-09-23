project('gnome-next-meeting', 'c',
  version: '0.1.0',
  license: 'AGPL-3.0-or-later',
  default_options: ['c_std=c99', 'warning_level=2'])

glib_dep = dependency('glib-2.0')
eds_dep = dependency('libedataserver-1.2')
ecal_dep = dependency('libecal-2.0')

executable('gnome-next-meeting',
  'main.c',
  dependencies: [glib_dep, eds_dep, ecal_dep],
  install: true)
