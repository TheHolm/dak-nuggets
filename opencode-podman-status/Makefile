# opencode-podman-status build wrapper.
#
# The program itself is built with Cargo. This Makefile wraps it so the
# top-level dak-nuggets orchestrator can drive every helper through the same
# build/test/coverage/install/clean entry points. It is POSIX-compatible
# (BSD make and GNU make).
#
# This program is Linux-only: it works by entering a rootless podman
# container's user and network namespaces, and rootless podman does not exist
# on FreeBSD (see README.markdown and NOTES.md). Rather than fail the
# collection-wide `make` on FreeBSD, every target below short-circuits with a
# clear message there. The program also ships no ci-freebsd.sh, which is what
# keeps it out of FreeBSD packaging.

PREFIX  ?= /usr/local
DESTDIR ?=
BIN = target/release/opencode-podman-status

# Prepended to every recipe. Each recipe line runs in its own shell, so the
# `exit 0` abandons just that line - which is the whole recipe here, since the
# guard and the real command are joined with a semicolon. Backticks rather than
# $(...) so make does not try to expand it.
LINUX_ONLY = if [ "`uname -s`" != "Linux" ]; then \
		echo "opencode-podman-status: Linux only, skipping (see README.markdown)"; \
		exit 0; \
	fi

.PHONY: all build test coverage install clean

all: build

build:
	@$(LINUX_ONLY); cargo build --release

test:
	@$(LINUX_ONLY); cargo test

# Coverage of the whole program, deliberately unfiltered. Four modules cannot be
# fully covered by an unprivileged unit test and pull the total down: nsenter.rs
# (needs a second namespace to enter), discover.rs and main.rs (need podman), and
# probe.rs (needs a live opencode). Those paths are covered instead by
# tests/namespace-entry.sh and by hand - see NOTES.md. Filtering them out would
# produce a prettier number that says less, so the per-file table is printed and
# the total left honest. Needs cargo-llvm-cov.
coverage:
	@$(LINUX_ONLY); \
	if ! cargo llvm-cov --version >/dev/null 2>&1; then \
		echo "opencode-podman-status: cargo-llvm-cov not installed" >&2; \
		echo "  install with: cargo install cargo-llvm-cov" >&2; \
		exit 1; \
	fi; \
	cargo llvm-cov --summary-only --ignore-filename-regex '(nsenter|discover|main)\.rs'

install: build
	@$(LINUX_ONLY); \
	mkdir -p "$(DESTDIR)$(PREFIX)/bin"; \
	install -m 755 $(BIN) "$(DESTDIR)$(PREFIX)/bin/opencode-podman-status"

clean:
	@$(LINUX_ONLY); cargo clean
