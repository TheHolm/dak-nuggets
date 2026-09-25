# Makefile for dak-nuggets
#
# Thin POSIX-compatible orchestrator. It has no GNU-isms on purpose, so it
# runs under both BSD make (FreeBSD's default make) and GNU make (Linux).
#
# Each program lives in its own subdirectory and provides its own build
# entry points via a per-program Makefile. The top-level targets simply
# recurse into every program directory listed in PROGRAMS.

# opencode-podman-status is Linux-only; its own Makefile short-circuits every
# target on other systems, so listing it here is safe on FreeBSD too.
PROGRAMS = gnome-next-meeting opencode-podman-status

PREFIX  ?= /usr/local
DESTDIR ?=

.PHONY: all build test coverage check-metadata install clean $(PROGRAMS)

all: build

build: $(PROGRAMS)

test: $(PROGRAMS:%=%.test)

coverage: $(PROGRAMS:%=%.coverage)

# Checks every program's package synopsis/description, and fails if a program's
# version has moved on to a new feature series without its description being
# re-reviewed. Run by the release pipeline too - see NOTES.md.
check-metadata:
	./scripts/check-package-metadata.sh

install: $(PROGRAMS:%=%.install)

clean: $(PROGRAMS:%=%.clean)

# --- per-program dispatch -------------------------------------------------
#
# Each program directory contains a Makefile exposing the standard targets
# (build, test, coverage, install, clean). The root Makefile forwards the
# request, passing PREFIX/DESTDIR through to install.

$(PROGRAMS):
	+$(MAKE) -C $@ build PREFIX=$(PREFIX)

$(PROGRAMS:%=%.test):
	+$(MAKE) -C $(@:%.test=%) test PREFIX=$(PREFIX)

$(PROGRAMS:%=%.coverage):
	+$(MAKE) -C $(@:%.coverage=%) coverage

$(PROGRAMS:%=%.install):
	+$(MAKE) -C $(@:%.install=%) install PREFIX=$(PREFIX) DESTDIR=$(DESTDIR)

$(PROGRAMS:%=%.clean):
	+$(MAKE) -C $(@:%.clean=%) clean
