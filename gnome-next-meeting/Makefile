# gnome-next-meeting build wrapper.
#
# The program itself is built with Meson (idiomatic for C/GLib + Evolution
# Data Server). This Makefile wraps Meson so the top-level dak-nuggets
# orchestrator can drive every helper through the same build/test/install/
# clean entry points. It is POSIX-compatible (BSD make and GNU make).

PREFIX  ?= /usr/local
DESTDIR ?=
BUILDDIR = build

.PHONY: all build test install clean

all: build

build:
	meson setup $(BUILDDIR) --prefix=$(PREFIX)
	meson compile -C $(BUILDDIR)

test: build
	meson test -C $(BUILDDIR)

install: build
	DESTDIR=$(DESTDIR) meson install -C $(BUILDDIR)

clean:
	rm -rf $(BUILDDIR)
