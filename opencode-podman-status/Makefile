# opencode-podman-status build wrapper.
#
# The program itself is built with Cargo. This Makefile wraps it so the
# top-level dak-nuggets orchestrator can drive every helper through the same
# build/test/coverage/install/clean entry points. It is POSIX-compatible
# (BSD make and GNU make).
#
# This program is Linux-only: it works by joining a rootless podman
# container's user and network namespaces (to create sockets there), and rootless podman does not exist
# on FreeBSD (see README.markdown and NOTES.md). Rather than fail the
# collection-wide `make` on FreeBSD, every target below short-circuits with a
# clear message there. The program also ships no ci-freebsd.sh, which is what
# keeps it out of FreeBSD packaging.

PREFIX  ?= /usr/local
DESTDIR ?=
BIN = target/release/opencode-podman-status
PLUGIN = plugin/opencode-podman-status.js
BUN ?= bun

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

# The status plugin's tests need Bun (https://bun.sh). Without it they are
# skipped with a message rather than failing the build: nothing in the build or
# packaging needs Bun, only running these tests does.
test:
	@$(LINUX_ONLY); cargo test
	@$(LINUX_ONLY); \
	if command -v $(BUN) >/dev/null 2>&1; then \
		cd plugin && $(BUN) test; \
	else \
		echo "opencode-podman-status: bun not found, skipping plugin tests (install Bun to run them)"; \
	fi

# Coverage summary. discover.rs and main.rs are excluded from the table because
# they need podman, which unit tests cannot provide; they are covered by hand -
# see NOTES.md. ns_socket.rs is kept in: its nested-namespace test runs wherever
# unprivileged user namespaces are permitted (and says so when they are not), and
# tests/namespace-entry.sh covers it end to end. Needs cargo-llvm-cov.
coverage:
	@$(LINUX_ONLY); \
	if ! cargo llvm-cov --version >/dev/null 2>&1; then \
		echo "opencode-podman-status: cargo-llvm-cov not installed" >&2; \
		echo "  install with: cargo install cargo-llvm-cov" >&2; \
		exit 1; \
	fi; \
	cargo llvm-cov --summary-only --ignore-filename-regex '(discover|main)\.rs'

install: build
	@$(LINUX_ONLY); \
	mkdir -p "$(DESTDIR)$(PREFIX)/bin" "$(DESTDIR)$(PREFIX)/share/opencode-podman-status"; \
	install -m 755 $(BIN) "$(DESTDIR)$(PREFIX)/bin/opencode-podman-status"; \
	install -m 644 $(PLUGIN) "$(DESTDIR)$(PREFIX)/share/opencode-podman-status/opencode-podman-status.js"

clean:
	@$(LINUX_ONLY); cargo clean
