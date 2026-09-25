# Package synopsis and description for gnome-next-meeting, used by every
# target's packaging (.deb, FreeBSD .pkg).
#
# This file is *sourced* by ci-deb.sh and ci-freebsd.sh, never executed, so it
# needs no executable bit. It exists so the text lives in exactly one place
# instead of being duplicated per packaging format, where the copies drifted
# apart and then fell behind the program itself.
#
# Re-read it whenever the program's behaviour changes, and record that you did
# by updating PKG_METADATA_REVIEWED_FOR below.
# scripts/check-package-metadata.sh fails the build when a minor or major
# version bump leaves that marker behind - see NOTES.md.

# One line, Debian-policy style: at most 72 characters, no trailing full stop,
# and no leading article.
PKG_SYNOPSIS="Calendar meeting countdowns for DAK"

# Longer description. Blank lines are allowed: scripts/build-deb.sh reflows
# this into Debian's control-file continuation format, and the FreeBSD packager
# takes it as-is.
PKG_DESCRIPTION="Prints a six-character countdown for each of today's remaining meetings, one
per line and soonest first, by reading the enabled calendars from Evolution
Data Server - the calendar backend behind GNOME Calendar and Evolution.

A leading space counts down to when a meeting starts; a leading '-' counts
down to the end of the meeting that is in progress right now. Overlapping
meetings each keep their own line, and '----' means nothing is left today. At
most three lines are printed unless --lines says otherwise, three being what a
DAK button LCD can display.

Intended for DAK's text_exec setup type, but usable from any script."

# The major.minor this text was last checked against. Patch digits are
# deliberately ignored: a patch release changes no behaviour by definition, so
# it cannot invalidate the description.
PKG_METADATA_REVIEWED_FOR="0.3"
