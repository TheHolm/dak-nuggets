# Release notes — opencode-podman-status

## v0.2.1

- **Fixed: the status plugin could add ~15-20 s to opencode's own startup.**
  Enabling the plugin sometimes made opencode itself slow to start, with no
  visible cause. The plugin's startup logged its own "serving on ..." message
  by calling back into opencode's own HTTP API, and waited for that call to
  finish before letting opencode continue starting up - but opencode's API is
  not necessarily reachable yet that early in its own startup, and opencode
  waits for every plugin to finish before continuing. The plugin no longer
  waits for that log call (or the two failure ones - invalid port, bind
  failure) to complete; opencode's startup is no longer able to depend on it
  either way. No other behaviour changes: same routes, same auth, same
  tracked state.

## v0.2.0

- **New: read-only status plugin.** Enable
  `/usr/share/opencode-podman-status/opencode-podman-status.js` in
  `opencode.json` and run opencode **without `--port`**. `--port` exposes
  opencode's full remote-control API, through which anything that can reach it
  can answer permission prompts and run commands with no human check. That
  includes the agent itself, from inside its own container. The plugin serves
  only state and timestamps and can change nothing. The README's Security
  section explains this in full.
- **New: `Error` state** (plugin only). A turn that failed is shown as `Error`
  on `--instance` and `error` in `--list`, and is counted on the `wait:` line
  of the summary. A user abort is not an error.
- **New: password support** for opencode started with
  `OPENCODE_SERVER_PASSWORD`, via `--password-file` (recommended) or
  `--password`, plus `--username`. One password is used for all containers.
- **New:** `--source auto|plugin|api` and `--plugin-port`. `--list` shows which
  server answered (`via=`).
- **Security hardening:** the helper no longer runs any of its own logic inside
  containers. `--timeout` is now a hard budget for everything one container
  costs. Responses and request counts are capped. Server-supplied IDs are
  validated before they are used in requests, and control characters are never
  printed.
- **Fixed:** probing a freshly started opencode could time out, because it
  keeps the connection open after a complete response.

### Implementation detail

- **Namespace transport rewritten** (`src/ns_socket.rs` replaces
  `src/nsenter.rs`).
  - Before: a forked child re-executed the program with `__probe` inside the
    container's user and network namespaces, and reported back JSON. So the
    whole HTTP and JSON stack ran with the container's user-namespace
    credentials; it inherited DAK's environment and working directory; it
    broke when a package upgrade replaced `/proc/self/exe`; and the parent
    waited on it with no bound.
  - Now the child makes only raw async-signal-safe syscalls, in order:
    `PR_SET_DUMPABLE=0`, `setns(user)`, `setns(net)`, 12 × `socket()`, then one
    `sendmsg` carrying the descriptors via `SCM_RIGHTS` plus a `[stage, errno]`
    report, then `_exit`. Buffers are prepared before `fork`. The parent owns
    received descriptors immediately, kills the child on the deadline, and
    always reaps it.
  - A socket stays in the namespace it was created in, so the parent connects
    it (non-blocking, against the deadline) to the container's loopback, and
    does all HTTP itself without ever changing namespace.
  - `__probe` is now an unknown argument.
- **Port discovery** by socket ownership now runs from the host, reading
  `/proc/<container-pid>/net/tcp{,6}`. It was re-validated as an unprivileged
  UID.
- **HTTP client**, `http::Client`, generic over a `Connect` trait:
  - one absolute deadline, re-applied before every read and write, so a server
    that drips bytes can't outlast it;
  - a 1 MiB per-response cap (was 8 MiB, silently truncated) and a 4 MiB byte
    budget per container;
  - request paths must be printable ASCII with no spaces;
  - session IDs must match `[A-Za-z0-9_]{1,64}` before they go into a path, so
    a crafted `/session/status` can't inject CRLF or steer requests to other
    routes.
- **Content-Length framing.** Cold opencode 1.18.32 sends a complete
  `Content-Length` response and then ignores `Connection: close`. The old
  "read to EOF" therefore hung until the timeout; this is very likely what the
  earlier "1500 ms is too short" observation actually was. Malformed,
  conflicting or over-cap lengths are errors.
- **Passwords** (`src/auth.rs`):
  - HTTP Basic, with hand-rolled RFC 4648 base64;
  - validation: no empty password, no `:` in the username, no control
    characters;
  - a warning for group- or other-readable files, a 4 KiB limit, and exactly
    one trailing newline stripped;
  - redacted from every `Debug` impl;
  - HTTP 401 has an explanatory message.
- **Plugin** (`plugin/opencode-podman-status.js`):
  - `Bun.serve` on `127.0.0.1` only, port `OPENCODE_STATUS_PORT` (default
    4097);
  - GET-only, four routes, with opencode's shapes reduced to type and
    `since_ms`;
  - Basic auth mirroring `OPENCODE_SERVER_PASSWORD`, compared in constant time
    and checked before routing;
  - state is built from bus events, as measured on 1.18.32. `since_ms` is set
    only on a real change. An error persists through the idle that follows it.
    `MessageAbortedError` is ignored. Pending prompts are dropped when their
    session goes idle, because opencode's own `/permission` keeps aborted
    prompts forever;
  - memory is bounded, state is shared per process through `globalThis`, and
    there is exactly one export (opencode rejects non-function exports);
  - installed at `/usr/share/opencode-podman-status/` by the `.deb` and under
    `$PREFIX/share/...` by `make install`;
  - `make test` runs `bun test` when Bun is present.
- **Verification:**
  - tested end to end against real opencode 1.18.32 inside `unshare -Urn`,
    both the TUI without `--port` and `serve --port` with a password, including
    permission, question, error, slow-turn and abort sequences;
  - 170 Rust tests and 24 plugin tests pass;
  - `tests/namespace-entry.sh` passes both as root and as an unprivileged UID.

## v0.1.1

- Low-level: the `.deb` installed the binary under `/usr/local/bin` instead of
  `/usr/bin`. `/usr/local` is reserved for the local admin by Debian policy and
  is never owned by any other package, so `dak-nuggets` ended up the sole
  claimant of that directory; removing the package then tried to `rmdir
  /usr/local`, which failed loudly ("Device or resource busy - directory may
  be a mount point?") whenever `/usr/local` happened to be a separate mount -
  a legitimate, policy-sanctioned setup (e.g. a shared/NFS `/usr/local`).
  `ci-deb.sh` now stages the binary at `usr/bin/opencode-podman-status`,
  matching Debian convention. This program has no FreeBSD packaging (Linux
  only), so there is no `.pkg` counterpart affected.

## v0.1.0

First release.

Reports what each opencode instance running in a rootless podman container on this
machine is doing. With no arguments it prints three six-character lines sized for a
DAK button — `run: 3`, `wait:1`, `done:5` — counting instances that are working,
that are waiting for an answer from you, and that are idle. `--instance` gives one
container's name, state and how long it has been in that state; `--list` prints a
diagnostic table.

Each container needs `opencode --port 4096` (the same port everywhere is correct).
Nothing else: no published ports, no bind mounts, no labels, no plugins, no
password.

Linux only, and deliberately so — it works by entering a rootless podman
container's namespaces, and rootless podman does not exist on FreeBSD.

### Implementation detail

- **Transport.** opencode's TUI listens on the container's `127.0.0.1`, which is
  unreachable from the host: loopback is per network namespace and rootless podman
  gives the host no route to container addresses. So for each container the program
  forks, moves the child into the container's user and network namespaces with
  `setns(2)`, and re-executes itself there. This needs no privileges beyond being
  the user who started the containers, because that user owns the container's user
  namespace and therefore holds `CAP_SYS_ADMIN` in it — the same mechanism
  `podman unshare` uses. The user namespace must be joined before the network
  namespace; the reverse fails `EPERM`.
- **`--port` is mandatory in the container, and config cannot replace it.** A plain
  `opencode` opens no socket at all and talks to its own server in-process via a
  `fetch` shim. The TUI resolves its network options without consulting the config,
  so the `server` block is inert for it — documented behaviour, since the schema
  scopes that block to `serve` and `web`.
- **Namespace identity is read, not inferred.** `/proc/<pid>/ns/{user,net}` is
  authoritative; nothing reasons about podman's network topology.
- **opencode's port is found by socket ownership, never by trying ports.** Inside
  the container the probe finds the processes named `opencode`, collects the socket
  inodes they hold from `/proc/<pid>/fd`, and keeps only listeners in the container's
  `/proc/net/tcp` with a matching inode. Other servers sharing the container are
  never contacted. A pre-release build sent a health check to every listening port
  instead, so neighbouring servers logged errors about requests they never asked
  for - found in first real use, and covered now by a regression test with a decoy
  server. It also lets the probe say precisely *why* a container is unreachable:
  opencode not running, or running without `--port`.
- **Three GETs per container** — `/session/status`, `/question`, `/permission`.
  `/session/status` omits idle sessions (verified with idle sessions present), so an
  empty map means nothing is working. Subagents are deliberately not filtered out:
  if a subagent is working the container is working.
- **Durations come from opencode's IDs.** IDs embed the low 48 bits of
  `unix_millis * 4096 + counter`, in one of two variants — ascending, or
  complemented so IDs sort newest-first. `ses_` is descending while `msg_` is
  ascending, so the variant is looked up per prefix rather than guessed; an early
  attempt to guess it by plausibility was wrong near the ~795-day wrap boundary and
  was caught by unit tests. Unknown prefixes yield no timestamp rather than a
  fabricated one.
- **Errors are not representable.** `session.error` is an event, not a status, so a
  polling design cannot see it — hence three buckets, not four.
- **No HTTP dependency.** Every request is a loopback GET with `Connection: close`,
  so the body is everything until EOF: no chunked encoding or `Content-Length`
  handling. `serde_json` is kept, because that is where a hand-rolled bug would
  hide. Dependencies are `serde`, `serde_json` and `libc`.
- **Default timeout 2500 ms.** Warm requests measure 1.5–5 ms and a cold first
  `/session/status` about 360 ms, but an earlier 1500 ms default was observed to
  time out when probing the instant `/global/health` began answering. Probes run in
  parallel, so this bounds the whole run rather than each container.
- **Slots are creation order, oldest first, with container ID as a tiebreaker** so
  same-second creations cannot swap between runs. Podman labels were considered and
  rejected: they are immutable after container creation, so the program could not
  assign them itself.
- **Security.** The port is an unauthenticated full-control API, so the program
  relies on `--port` binding `127.0.0.1` only and on loopback being per namespace.
  A password was rejected deliberately: opencode reads it only from the environment,
  and everything it spawns inherits that environment, so it would be readable by
  the in-container code it would defend against.
- 113 unit tests, plus an opt-in `tests/namespace-entry.sh` covering namespace entry
  against a fake opencode in a nested user namespace, next to a decoy server that
  must receive no requests. That one is kept out of
  `make test` because creating a nested user namespace is commonly forbidden in CI.
