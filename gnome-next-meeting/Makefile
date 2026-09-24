# gnome-next-meeting build wrapper.
#
# The program itself is built with Meson (idiomatic for C/GLib + Evolution
# Data Server). This Makefile wraps Meson so the top-level dak-nuggets
# orchestrator can drive every helper through the same build/test/install/
# clean entry points. It is POSIX-compatible (BSD make and GNU make).

PREFIX  ?= /usr/local
DESTDIR ?=
BUILDDIR = build
COVDIR = build-coverage

.PHONY: all build test coverage install clean

all: build

build:
	meson setup $(BUILDDIR) --prefix=$(PREFIX)
	meson compile -C $(BUILDDIR)

test: build
	meson test -C $(BUILDDIR)

# Branch/line coverage of the unit-tested logic layer. Always configured from
# scratch, so stale .gcda counters from an earlier run cannot skew the figures.
# The report is filtered to next_meeting.c deliberately: main.c is the EDS glue,
# which no unit test can reach and which is verified by hand (see NOTES.md), so
# including it would only dilute the number. Needs gcovr installed.
coverage:
	rm -rf $(COVDIR)
	meson setup $(COVDIR) -Db_coverage=true
	meson test -C $(COVDIR)
	gcovr --root . --filter 'next_meeting\.c' --txt
	gcovr --root . --filter 'next_meeting\.c' --txt --txt-metric branch

install: build
	DESTDIR=$(DESTDIR) meson install -C $(BUILDDIR)

clean:
	rm -rf $(BUILDDIR) $(COVDIR)
