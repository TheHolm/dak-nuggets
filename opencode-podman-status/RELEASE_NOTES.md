# Release notes — opencode-podman-status

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
