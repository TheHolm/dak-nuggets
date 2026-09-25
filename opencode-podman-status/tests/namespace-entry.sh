#!/bin/sh
# Opt-in integration test for namespace entry.
#
# Everything else in this program is covered by `cargo test`, but the part that
# actually earns its keep - entering a rootless container's user and network
# namespaces - cannot be tested that way: it needs a second namespace to enter,
# and CI containers often forbid creating one. So this lives here as a script you
# run by hand:
#
#     sh tests/namespace-entry.sh
#
# It stands in a nested user+network namespace for a rootless podman container,
# puts a fake opencode server inside it, and checks that:
#
#   1. the server is NOT reachable from outside the namespace;
#   2. `--pid` reaches it by entering the namespace, and reports the right state;
#   3. the port is discovered by socket ownership rather than needing to be told;
#   4. a second, unrelated server in the same namespace is never contacted.
#
# Point 4 is a regression test: an earlier version tried every listening port,
# so other servers in the container received requests and logged errors.
#
# The same shape as the manual validation recorded in NOTES.md, which additionally
# confirmed this works as an unprivileged user (the case that matters for rootless
# podman) and that joining the network namespace without the user namespace fails
# with EPERM.
#
# Requires: unshare(1) with unprivileged user namespaces permitted, python3, and
# a built binary. Exits 0 on success, 1 on failure, 2 if it cannot run at all.
set -u

here=$(cd "$(dirname "$0")" && pwd)
program_dir=$(cd "$here/.." && pwd)
bin="$program_dir/target/debug/opencode-podman-status"
[ -x "$bin" ] || bin="$program_dir/target/release/opencode-podman-status"

work=$(mktemp -d)
# shellcheck disable=SC2064  # expand $work now, not at trap time
trap "rm -rf '$work'; [ -n \"\${server_pid:-}\" ] && pkill -P \"\$server_pid\" 2>/dev/null; [ -n \"\${server_pid:-}\" ] && kill \"\$server_pid\" 2>/dev/null" EXIT

skip() {
    echo "SKIP: $*" >&2
    exit 2
}

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

[ -x "$bin" ] || skip "no built binary; run 'cargo build' first"
command -v unshare >/dev/null 2>&1 || skip "unshare(1) not available"
command -v python3 >/dev/null 2>&1 || skip "python3 not available"

PORT=47311
DECOY_PORT=47312

# A fake opencode: enough of the API for the probe to classify it. Reports one
# busy session and no pending requests, so the expected verdict is "run".
cat >"$work/fake_opencode.py" <<'PYTHON'
"""Minimal stand-in for opencode's server, inside a fresh network namespace.

A new netns starts with `lo` administratively DOWN, so 127.0.0.1 would be
unusable until it is brought up; there is no dependency on iproute2 here, so
this uses the SIOCSIFFLAGS ioctl directly.
"""
import fcntl
import http.server
import json
import os
import socket
import struct
import sys

SIOCGIFFLAGS, SIOCSIFFLAGS, IFF_UP = 0x8913, 0x8914, 0x1
PR_SET_NAME = 15
PORT = int(sys.argv[1])
STATE_DIR = sys.argv[2]
DECOY_PORT = int(sys.argv[3])


def become_opencode():
    """Rename this process's comm to `opencode`, as the probe identifies opencode
    by process name. Must run on the main thread, whose comm /proc reports."""
    import ctypes
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(PR_SET_NAME, b"opencode", 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "prctl(PR_SET_NAME) failed")


def bring_loopback_up():
    """Set IFF_UP on `lo` in the current network namespace."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        ifr = struct.pack("16sh", b"lo", 0)
        flags = struct.unpack("16sh", fcntl.ioctl(sock, SIOCGIFFLAGS, ifr))[1]
        fcntl.ioctl(sock, SIOCSIFFLAGS, struct.pack("16sh", b"lo", flags | IFF_UP))
    finally:
        sock.close()


ROUTES = {
    "/global/health": {"healthy": True, "version": "1.18.32"},
    # One busy session: the probe should classify this instance as "run".
    "/session/status": {"ses_f29457234ffejFzn9L4UNG7Hz7": {"type": "busy"}},
    "/question": [],
    "/permission": [],
}


class Handler(http.server.BaseHTTPRequestHandler):
    """Serves the handful of endpoints the probe asks for."""

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ROUTES:
            body = json.dumps(ROUTES[path]).encode()
        elif path.endswith("/message"):
            # Shape verified against opencode 1.18.32: a {info, parts} wrapper.
            body = json.dumps(
                [{"info": {"time": {"created": 0}}, "parts": []}]
            ).encode()
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        """Silence the default stderr access log."""


class Decoy(http.server.BaseHTTPRequestHandler):
    """An unrelated server sharing the namespace. Records any request it gets;
    the probe must never send it one."""

    def do_GET(self):
        with open(os.path.join(STATE_DIR, "decoy-hits"), "a") as fh:
            fh.write(self.path + "\n")
        self.send_error(404)

    def log_message(self, *args):
        """Silence the default stderr access log."""


if __name__ == "__main__":
    bring_loopback_up()
    # The decoy runs in a separate process forked *before* the rename, so it is
    # neither named opencode nor holding opencode's sockets - just a neighbour.
    # Renaming first would make the child inherit the name and genuinely look
    # like opencode (a real child of opencode execs, which resets its name).
    if os.fork() == 0:
        decoy = http.server.HTTPServer(("127.0.0.1", DECOY_PORT), Decoy)
        decoy.serve_forever()
    become_opencode()
    server = http.server.HTTPServer(("127.0.0.1", PORT), Handler)
    with open(os.path.join(STATE_DIR, "pid"), "w") as fh:
        fh.write(str(os.getpid()))
    with open(os.path.join(STATE_DIR, "ready"), "w") as fh:
        fh.write("ready")
    server.serve_forever()
PYTHON

# The PID written from inside is globally visible: only the user and network
# namespaces are unshared, not the PID namespace.
unshare -Urn python3 "$work/fake_opencode.py" "$PORT" "$work" "$DECOY_PORT" >"$work/server.log" 2>&1 &

i=0
while [ ! -f "$work/ready" ] && [ "$i" -lt 60 ]; do
    sleep 0.25
    i=$((i + 1))
done
if [ ! -f "$work/ready" ]; then
    sed 's/^/  /' "$work/server.log" >&2
    skip "could not create a nested user+network namespace (not permitted here?)"
fi

server_pid=$(cat "$work/pid")
echo "fake opencode running as pid $server_pid in its own namespaces"

# 1. Isolation: the server must not be reachable from out here. If it were, the
#    whole premise of entering the namespace would be wrong.
if python3 -c "
import socket, sys
try:
    socket.create_connection(('127.0.0.1', $PORT), timeout=2).close()
except OSError:
    sys.exit(1)
sys.exit(0)
"; then
    fail "server is reachable without entering the namespace; isolation assumption is wrong"
fi
echo "ok: unreachable from outside the namespace"

# 2. Namespace entry with an explicit port.
out=$("$bin" --pid "$server_pid" --port "$PORT" 2>&1) || fail "--pid exited non-zero: $out"
echo "$out" | grep -q '"state":"run"' \
    || fail "expected state run with explicit port, got: $out"
echo "ok: reached it via namespace entry, state=run"

# 3. Namespace entry with the port found by socket ownership.
out=$("$bin" --pid "$server_pid" 2>&1) || fail "--pid (auto port) exited non-zero: $out"
echo "$out" | grep -q "\"port\":$PORT" \
    || fail "expected discovered port $PORT, got: $out"
echo "$out" | grep -q '"state":"run"' \
    || fail "expected state run with discovered port, got: $out"
echo "ok: found opencode's own port $PORT by socket ownership, state=run"

# 4. The unrelated server must not have been contacted, by any of the above.
if [ -s "$work/decoy-hits" ]; then
    fail "the unrelated server was contacted: $(tr '\n' ' ' < "$work/decoy-hits")"
fi
echo "ok: the unrelated server in the same namespace received no requests"

echo "PASS"
