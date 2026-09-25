# Package synopsis and description for opencode-podman-status, used by every
# target's packaging.
#
# This file is *sourced* by ci-deb.sh, never executed, so it needs no executable
# bit. It exists so the text lives in exactly one place instead of being
# duplicated per packaging format.
#
# Note there is no ci-freebsd.sh for this program: it is Linux-only by design
# (rootless podman does not exist on FreeBSD), so it is absent from FreeBSD
# packaging altogether. See README.markdown and NOTES.md.
#
# Re-read it whenever the program's behaviour changes, and record that you did
# by updating PKG_METADATA_REVIEWED_FOR below.
# scripts/check-package-metadata.sh fails the build when a minor or major
# version bump leaves that marker behind - see the top-level NOTES.md.

# One line, Debian-policy style: at most 72 characters, no trailing full stop,
# and no leading article.
PKG_SYNOPSIS="opencode container status for DAK"

# Longer description. Blank lines are allowed: scripts/build-deb.sh reflows
# this into Debian's control-file continuation format.
PKG_DESCRIPTION="Reports what each opencode instance running in a rootless podman container is
doing, as three six-character lines sized for a DAK button LCD: how many
instances are working, how many are waiting for an answer, and how many are
idle. With --instance it reports one container's name, state and how long it
has been in that state instead.

opencode's terminal interface listens only on its container's loopback address,
which the host cannot reach. This program therefore enters each container's user
and network namespaces to query it, needing no privileges beyond running as the
user who started the containers, and no published ports, bind mounts or plugins.
Each container must run opencode with an explicit --port.

Linux only: rootless podman does not exist on FreeBSD.

Intended for DAK's text_exec setup type, but usable from any script."

# The major.minor this text was last checked against. Patch digits are
# deliberately ignored: a patch release changes no behaviour by definition, so
# it cannot invalidate the description.
PKG_METADATA_REVIEWED_FOR="0.1"
