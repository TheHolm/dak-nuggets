# Makefile for dak-nuggets
#
# Thin POSIX-compatible orchestrator. It has no GNU-isms on purpose, so it
# runs under both BSD make (FreeBSD's default make) and GNU make (Linux).
#
# Each program lives in its own subdirectory and provides its own build
# entry points via a per-program Makefile. The top-level targets simply
# recurse into every program directory listed in PROGRAMS.

PROGRAMS = gnome-next-meeting

PREFIX  ?= /usr/local
DESTDIR ?=

.PHONY: all build test install clean $(PROGRAMS)

all: build

build: $(PROGRAMS)

test: $(PROGRAMS:%=%.test)

install: $(PROGRAMS:%=%.install)

clean: $(PROGRAMS:%=%.clean)

# --- per-program dispatch -------------------------------------------------
#
# Each program directory contains a Makefile exposing the standard targets
# (build, test, install, clean). The root Makefile forwards the request,
# passing PREFIX/DESTDIR through to install.

$(PROGRAMS):
	+$(MAKE) -C $@ build PREFIX=$(PREFIX)

$(PROGRAMS:%=%.test):
	+$(MAKE) -C $(@:%.test=%) test PREFIX=$(PREFIX)

$(PROGRAMS:%=%.install):
	+$(MAKE) -C $(@:%.install=%) install PREFIX=$(PREFIX) DESTDIR=$(DESTDIR)

$(PROGRAMS:%=%.clean):
	+$(MAKE) -C $(@:%.clean=%) clean
