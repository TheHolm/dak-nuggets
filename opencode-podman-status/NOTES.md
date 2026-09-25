# opencode-podman-status — implementation notes

Agent-to-agent notes for this program: the reverse-engineering behind it, what was
measured rather than assumed, the dead ends, and the things that will break.
Keep it current.

For repo-wide concerns (release pipeline, packaging scripts) see the top-level
`NOTES.md`.

---

## 1. The single most important fact: the TUI ignores `server.*` in config

**A plain `opencode` opens no listening socket at all.** The TUI reaches its own
server through an in-process `fetch` shim at the fake URL
`http://opencode.internal`. The switch that makes it bind a real socket is, from
the 1.18.32 binary:

```js
R = du("--port") || du("--hostname") || Q.mdns === true
q = R ? { url: (await z.call("server", Q)).url, fetch: undefined, ... }
      : { url: "http://opencode.internal", fetch: v7(z), events: d7(z) }
```

and `du` / `r6` scan **`process.argv`**, never the config:

```js
function du(D){ return g8().some((u)=> u===D || u.startsWith(D+"=")) }
function r6(D){ return g8().some((u)=> u===D || u===D+"=true" || u===D+"=false" || u==="--no-"+D.slice(2)) }
function g8(){ let D=process.argv.indexOf("--"); return process.argv.slice(2, D===-1?void 0:D) }
```

Worse, the TUI calls the network-option resolver with **one** argument:

```js
Q = K0(D)                     // TUI: u is undefined
K0(D, config)                 // serve / web / acp, via Cli.resolveNetworkOptions
```

Every config lookup inside `K0(D, u)` is `u?.server?.…`, so with `u === undefined`
they all collapse to yargs defaults. **The entire `server` block is inert for the
TUI.** This is documented behaviour, not a bug: the schema annotates it as
*"Server configuration for opencode serve and web commands"*.

Verified empirically, twice:

| Config | Result |
|---|---|
| `"server": {"port":4096,"hostname":"0.0.0.0"}` | `opencode debug config` shows it resolved; **no listener**, `curl` exit 7 |
| `"server": {"mdns":true,"hostname":"127.0.0.1","port":4097}` via `OPENCODE_CONFIG` | **no listener** |
| `opencode --port 4098` (CLI) | `LISTEN 0100007F:1002`, `{"healthy":true,"version":"1.18.32"}` |

**Consequence:** the container must run `opencode --port <n>`. There is no env var
(`OPENCODE_PORT` does not exist) and no config route. A wrapper in the image is the
fallback: `exec opencode --port 4096 "$@"`.

`--port` alone binds `127.0.0.1` — `hostname:{type:"string",describe:"hostname to
listen on",default:"127.0.0.1"}`, and `port` defaults to `0` (random). `--mdns`
forces `0.0.0.0` (*"enable mDNS service discovery (defaults hostname to
0.0.0.0)"*), so do not suggest it.

---

## 2. Why namespace entry, and why it is allowed

Loopback is per network namespace, and in **rootless** podman the host cannot route
to container addresses at all. The options were: publish ports, `podman exec`, a
plugin writing status files, or entering the namespace. Entering won because it
needs the least container-side configuration (one flag, no `-p`, no
`--hostname 0.0.0.0`, no labels, no bind mounts) and keeps the API on loopback.

The permission model, from `setns(2)`:

> **User namespaces**: A process reassociating itself with a user namespace must
> have the `CAP_SYS_ADMIN` capability in the target user namespace. (This
> necessarily implies that it is only possible to join a descendant user
> namespace.) Upon successfully joining a user namespace, a process is granted all
> capabilities in that namespace, regardless of its user and group IDs.
> A multithreaded process may not change user namespace with `setns()`.
> […] a process can't join a new user namespace if it is sharing
> filesystem-related attributes (`CLONE_FS`) with another process.

> **Network namespaces**: the caller must have `CAP_SYS_ADMIN` both in its own user
> namespace and in the user namespace that owns the target namespace.

Rootless podman creates the container's userns as the invoking user, and a process
whose euid owns a userns has all capabilities in it. Hence no root, no setuid.

### Three constraints that shaped the code

1. **Single-threaded.** `setns(CLONE_NEWUSER)` refuses a multithreaded process. A
   child immediately after `fork` has one thread — only the calling thread is
   carried over — so the parent *may* use threads to overlap probes, as
   `probe_all` does. The `setns` calls must nonetheless happen post-fork, which is
   what `Command::pre_exec` gives us.
2. **No `CLONE_FS` sharing.** `fork` copies fs attributes; threads share them. So
   the work cannot be done on a thread of the parent, only in a forked child.
3. **Cannot re-enter your own userns** (`EINVAL`). `shares_our_namespace` checks
   this up front, which also stops the program probing the container it is itself
   running in.

### Order is mandatory

User namespace **first**. Joining the netns alone fails `EPERM`, measured:

```
nsenter --net --target PID -- curl ...
  nsenter: reassociate to namespaces failed: Operation not permitted
```

### Validation performed

As an **unprivileged** user (uid 1000, no capabilities, `/etc/subuid` populated
rootless-style), against a nested userns+netns holding a listener:

```
port discovery from OUTSIDE the namespace:  LISTEN 0100007F:270F
connect WITHOUT entering:                   refused
join userns+netns as unprivileged owner:    {"healthy":true,...}  -> PASS
netns only, without joining userns:         EPERM
```

`tests/namespace-entry.sh` reproduces the same shape through this program's own
code (isolation, `--pid` entry, port discovery by socket ownership, and a decoy
server that must receive no requests). It is **not** part of
`make test`: it needs to create a nested user namespace, which CI containers
commonly forbid. It exits 2 to mean "skipped".

### Known limitation

opencode must run as a UID inside the container that maps back to the invoking
user — i.e. root inside the container, the default. If it runs as a non-root
container user, its host UID is an unowned subuid and opening `/proc/<pid>/ns/*`
fails. The general workaround, not implemented, is two-level entry via
`podman unshare nsenter …`.

---

## 3. Network namespaces are per **container**, not per podman network

A recurring misconception worth recording. By default every container gets its own
netns; a "podman network" is a bridge connecting separate namespaces. Namespaces
are shared only in a **pod** (all containers share the infra container's netns),
with `--network=container:<name>`, or with `--network=host`. Red Hat's
documentation: *"By definition, all containers in a Podman pod share the same
network namespace […] they will have the same IP address, MAC addresses, and port
mappings."*

None of this is inferred by the program. A container's namespace identity is read
from `/proc/<pid>/ns/net`, whose target (`net:[4026534881]`) matches exactly when
two processes share a namespace. If two containers report the same netns inode they
are in a pod, which also means they would collide on the port.

---

## 3a. Finding opencode's port: by socket ownership, never by trying ports

**First real-use bug.** The pre-release probe, when not given `--port`, took every
loopback/wildcard listener in the container's `/proc/net/tcp` and sent each a
`GET /global/health` to see which one was opencode. The reporting user's containers
run other servers next to opencode, and those servers logged errors about requests
they never asked for. A monitor must not touch services it was not pointed at, so
"try it and see" is out entirely - even a single request to the wrong server is the
bug.

Reproduced here with a decoy server that records every request: the old code sent
it `/global/health` whenever opencode itself had no listener (i.e. was started
without `--port`), because the decoy was then the only candidate.

**Now** (`src/sockets.rs`): inside the container's namespaces,

1. read listeners from `/proc/net/tcp` and `/proc/net/tcp6` - field 10 is the
   socket **inode**;
2. scan `/proc/*` for processes whose `/proc/<pid>/ns/net` equals our own (i.e.
   are in this container - `/proc` is still the host's mount, so these are host
   PIDs) and that are opencode: `comm == "opencode"` or `basename(argv[0]) ==
   "opencode"`, both exact;
3. collect their socket inodes from `/proc/<pid>/fd/*` (`socket:[N]` links);
4. use only listeners whose inode is among them.

Verified live: with opencode on 4099 and a decoy on 8765, the ownership scan maps
4099 to the `opencode` process and 8765 to `python3`; the probe reports 4099 and the
decoy receives nothing. With opencode running *without* `--port` it reports
"opencode is running without --port, so it has no API socket" - and the decoy still
receives nothing. That exact message is also what the reporting user's setup should
now show, instead of silently counting nothing.

Details that matter:

- **Exact name match.** This program's own `comm` is `opencode-podman` (15-char
  truncation of `opencode-podman-status`), so a prefix match would find itself.
- **The listening socket is held by the main `opencode` process** (Bun's server
  runs on a worker *thread*, and threads share the fd table), so no child-process
  search is needed.
- **Permission to read `/proc/<pid>/fd`** needs ptrace-read access. The probe child
  has it twice over: same host UID as the container's root, and full capabilities
  in the container's user namespace after `setns`.
- **Three distinct outcomes** are reported: not running, running without `--port`,
  and fds unreadable. "Not listening" is only claimed when the fds *were* readable,
  otherwise it would be a guess.
- An explicit `--port` still bypasses all of this and is connected to as given -
  that is the user naming the port themselves.
- `tests/namespace-entry.sh` now carries a regression test: the fake opencode renames
  itself `opencode` via `prctl(PR_SET_NAME)`, and a decoy in a separate process
  (forked *before* the rename, otherwise it inherits the name and genuinely looks
  like opencode) must record zero requests.

## 4. opencode's ID format carries a timestamp

This is how "how long has it been waiting" is answered with no state files and no
extra requests. IDs look like `ses_f2948c7fdffe1r7f03mBfBRLsy`:

```
<prefix> _ <12 hex chars> <14 random chars>
```

The hex holds the **low 48 bits** of `(unix_millis * 4096 + per_ms_counter)`.
Generator, from the binary:

```js
$ = BigInt(Y)*0x1000n + BigInt(cU)          // Y = Date.now(), cU = counter
A = _ ? ~$ : $                              // descending variant complements
U = 6 bytes of A, big-endian, as 12 hex chars   // low 48 bits only
return U + <14 random chars>
```

Prefixes seen: `job evt ses msg per que prt pty tool wrk`.

### Two variants, and you cannot tell which from the ID

* **Ascending** stores the value directly (sorts oldest-first).
* **Descending** stores `~value`, i.e. `(2^48-1) - value` (sorts newest-first).

Established so far:

| Prefix | Encoding | How established |
|---|---|---|
| `per` | ascending | source: its constructor calls the ascending generator |
| `que` | ascending | assumed — same request machinery as `per` |
| `ses` | **descending** | decoded two real IDs against their recorded `time.created` |
| `msg` | ascending | decoded a real ID against its recorded `time.created` |

`ses` and `msg` genuinely differ, which is why `Encoding::for_prefix` exists rather
than a single assumption. **Unknown prefixes decode to nothing**, never a guess.

### A trap: do not guess the encoding by plausibility

The first implementation decoded both ways and kept whichever landed in a
believable window. That is **wrong** and the unit tests caught it: near a wrap
boundary the incorrect interpretation is also plausible, and can even look more
recent. Two tests failed on exactly this. The encoding must be stated or looked up.

### Only 36 bits of time survive

48 payload bits minus 12 counter bits, so the millisecond timestamp **wraps every
~795 days**. High bits are reconstructed from the current time, stepping back one
wrap if that would land in the future.

### opencode's own decoder is not reusable

```js
function S6(Q){ let $=Q.split("_")[0], K=Q.slice($.length+1,$.length+13);
                return Number(BigInt("0x"+K)/BigInt(4096)) }
```

It assumes ascending and ignores the wrap. Copying it would silently mis-decode
every `ses_` ID.

### Verification data

| ID | recorded `time.created` | decode | delta |
|---|---|---|---|
| `ses_f2948c7fdffe1r7f03mBfBRLsy` | 1790308726787 | descending | 1 ms (ID minted just before) |
| `ses_f29457234ffejFzn9L4UNG7Hz7` | 1790308945355 | descending | 0 ms |
| `msg_0d6c75ea3001xwhRotcmNTvdqj` | 1790309785251 | ascending | 0 ms, counter 1 |

---

## 5. API behaviour that matters

### `/session/status` omits idle sessions

Verified with **two idle sessions present**: the response was still `{}`. So an
empty map means "nothing is working", and the program never needs `GET /session`
to enumerate sessions for classification.

Statuses are `idle` / `busy` / `retry` (the last carries `attempt`, `message`,
`next`). `retry` counts as working. An explicit `idle` entry is still handled in
case the omission changes.

### Errors are not pollable

`session.error` is an **event**, not a status. `/session/status` never reports it.
A polling design therefore cannot surface errors at all — which is why the output
has three buckets and not four. Push via `GET /event` (SSE, all bus events plus a
10 s heartbeat) would be needed.

### Subagents need no filtering

Sessions have an optional `parentID` marking subagents. Deliberately ignored: if a
subagent is working the container is working, and a question needs answering
whichever session raised it. This removes a fourth request and a class of edge
cases.

### Three GETs, and the shapes

`/session/status` → object map; `/question`, `/permission`, `/session` → arrays.
`?directory=` defaults to the opencode process's own cwd, so it is omitted — inside
a container the container *is* the scope.

Neither `QuestionRequest` nor `PermissionRequest` carries a time field (confirmed
from the schema), which is why their IDs must be decoded.

### `?limit=1` returns the newest message

Verified: created a message in an existing session, and `GET
/session/{id}/message?limit=1` returned only that one, far newer than the session's
own `time.created`. The wrapper shape is `[{"info":{…,"time":{"created":…}},"parts":[…]}]`;
a bare-message shape is also accepted since older versions differed.

Note `POST /session/{id}/shell` does **not** mark a session `busy` — so the `run`
classification could not be exercised against a live instance this way. That path
is covered by unit tests only. **Still to verify against a genuinely busy session:
`busy_since_ms`.**

### Measured latencies (1.18.32, loopback)

| | |
|---|---|
| warm `/global/health`, `/session/status`, `/question`, `/permission` | 1.5–3 ms |
| warm `/session` | 5 ms |
| **cold** first `/session/status` | 357 ms |
| `/global/health` first answers | ~3.5 s into boot |

An earlier 1500 ms default timeout was observed to fail with `EAGAIN` on
`/session/status` when probing the instant `/global/health` started answering.
Default is now **2500 ms**. Probes run in parallel, so it bounds the whole run.

---

## 6. Security posture

The port is an **unauthenticated full-control API**. Routes include
`POST /session/{id}/shell`, `POST /session/{id}/prompt_async`, `POST /pty` +
`/pty/{id}/connect` (interactive terminal), `POST /mcp/{name}/connect`,
`POST /global/upgrade`, `GET /file/content`, `GET /find`,
`POST /permission/{id}/reply` and `POST /question/{id}/reply` (answering the very
prompts meant to gate dangerous calls), `GET /event` (whole conversation),
`POST /session/{id}/share` (publish it), and `PUT`/`DELETE /auth/{providerID}`
(replace or delete provider credentials — there is no `GET`, so keys cannot be read
back, but they can be repointed at an attacker's `baseURL`).

Authentication is opt-in and off by default:

```js
required(n) { return isSome(n.password) && n.password.value !== "" }
```

`serve`/`web` at least warn *"OPENCODE_SERVER_PASSWORD is not set; server is
unsecured"*; **the TUI warns about nothing.**

**A password was considered and rejected.** The server reads it only from
`OPENCODE_SERVER_PASSWORD` — `--password`/`--username` are client-side flags for
`--attach`, and nothing passes CLI values into `ServerAuth.Config`. Every process
opencode spawns inherits its environment (`env:{...process.env, ...}` at the shell
tool, PTY, LSP and MCP spawn sites; the only config knobs, `lsp.<id>.env` and
`mcp.<name>.environment`, are *additive*). So the secret would be readable by the
in-container code it would defend against, while protecting nothing that namespace
isolation does not. The `shell` config key could front a scrubbing shim, but it
covers only the bash tool and terminal — not LSP, MCP or formatters.

What actually contains the exposure: `--port` binds `127.0.0.1` only, and loopback
is per netns. Reachable from inside the container, and by a user who can enter its
namespaces — who could already `podman exec` in. Not reachable from other
containers, the host network, or the LAN. Adding `-p` or `--hostname 0.0.0.0` would
break that.

---

## 7. Rejected approaches, and why

| Approach | Why not |
|---|---|
| **Publish ports** (`-p` + `--hostname 0.0.0.0`) | Needs a per-container host port and exposes the unauthenticated API to every container on the network and to the host. Also hits the rootlesskit/pasta trap: forwarded traffic arrives at the container's interface address, not loopback, so a `127.0.0.1` bind silently never receives it. |
| **`podman exec` + curl** | Needs an HTTP client in every image; 100–300 ms per exec, so nine containers get slow. |
| **Plugin writing status files to a bind mount** | Push-based, but DAK polls on a timer, so observable freshness is identical — the advantage evaporates. Costs a second artifact in a third language plus a bind mount per container. |
| **Terminal window title** | opencode does set it, but only to `OpenCode` or `OC | <session title>` — **no status**. Adding status needs a plugin anyway, and reading titles is impossible on Wayland (no protocol to enumerate other clients' windows), collapses to one title if containers share a terminal via tabs, and is rewritten by tmux. |
| **mDNS (`--mdns`)** | Forces `0.0.0.0`, and *skips publishing entirely* when the hostname is loopback. Multicast does not cross slirp4netns/pasta. |
| **Podman labels for slot numbers** | **Labels are immutable after container creation** — there is no `podman container update --label` (open feature request, podman #27815). The program could not assign them itself. Reading them is free (`podman ps --format json` returns `Labels`), so this remains a cheap future option if stable numbering is ever wanted. |
| **Unix socket** | opencode binds host/port only; no socket-path option. |
| **The durable `event` table in `opencode.db`** | Persists message/session events only, not status. |

---

## 8. Gotchas for whoever works on this next

- **Do not use `pkill -f` with a pattern that matches your own command line.** It
  kills the shell running it. Cost a confusing "command produced no output" during
  development.
- **The `--pid` flag is a real diagnostic**, not test-only scaffolding: it probes a
  PID's namespaces directly, bypassing podman discovery, and is the hook
  `tests/namespace-entry.sh` drives.
- **`__probe` is internal.** The parent re-executes itself with it after entering
  namespaces. Deliberately undocumented in `--help`'s option list.
- **Re-exec rather than doing HTTP in the forked child.** The child is a fresh
  process, which sidesteps every fork-safety question about allocators, and makes
  `__probe` independently testable against a plain `TcpListener`.
- **`Connection: close` is why there is no HTTP library.** The server closes the
  socket when the body is done, so the body is "everything until EOF" — no chunked
  encoding, no `Content-Length` parsing. Verified against 1.18.32, which replies
  with `Content-Length` and no chunking. A client crate would have added eight
  transitive dependencies.
- **`serde_json` is kept deliberately.** A hand-rolled JSON parser is where a
  silent correctness bug would hide; hand-rolled HTTP is not.
- **Slot numbering is creation order with container ID as tiebreaker.** The
  tiebreaker matters: without it, two containers created in the same second could
  swap slots between runs, which is the one thing slot numbers must not do.
- **`attach_pids` drops containers without a PID but does not renumber survivors.**
  Renumbering there would defeat the point of stable slots.
- **shellcheck is not installed in the dev container**, so
  `tests/namespace-entry.sh` and `ci-deb.sh` have not been linted. Likewise
  **bmake is unavailable**, so the `Makefile`'s BSD-make compatibility is by
  inspection only (it uses just `?=`, `.PHONY`, backslash continuations and shell
  recipes, all of which both makes accept). Worth confirming both when a suitable
  environment is available.
- **Never find a service by sending it a request.** See §3a: probing every
  listener to see which one answers is how the first real-use bug happened. Identify
  by ownership from `/proc`, then talk only to what was identified.
- **Coverage is reported unfiltered** (`make coverage`), currently ~83% lines.
  The logic layer is essentially complete - `ident.rs` and `render.rs` at 100%,
  `status.rs` and `http.rs` at 99% - while `nsenter.rs` (73%), `probe.rs` (68%)
  and `main.rs` (67%) hold the paths that need a real namespace, a live opencode
  or podman. Filtering those out was considered and rejected: it would produce a
  nicer number that says less, and it would also hide the well-tested pure
  parsing that lives in those same files.
- A new netns starts with `lo` **DOWN**. Anything binding `127.0.0.1` inside one
  must bring it up first; the test harness does it with `SIOCSIFFLAGS` because
  iproute2 is not present.

## 9. Packaging notes specific to this program

- **`DEB_DEPENDS` is deliberately ignored.** The release pipeline sets it per
  *step* as the union of every program's runtime dependencies (it feeds the bundle
  package's `Depends:`), and forwards it to every `ci-deb.sh`. Honouring it here
  would make this package declare Evolution Data Server libraries it never loads.
  `ci-deb.sh` therefore reads `DEB_DEPENDS_OPENCODE_PODMAN_STATUS` instead, empty
  by default because Rust links statically. Verified by building with a polluting
  `DEB_DEPENDS` set and confirming no `Depends` field appears.
- **No `ci-freebsd.sh`**, which is the whole mechanism for staying out of FreeBSD
  packaging - see the top-level `NOTES.md`.
- `CARGO_TARGET_DIR` is pointed at the caller's per-target build directory, not the
  source tree, so concurrent target steps in CI's shared workspace cannot collide.
- Built `.deb` is ~260 KB with no `Depends` field, verified locally with
  `dpkg-deb -c`/`-I`.
- **`Cargo.lock` is committed**, and the repo-wide `.gitignore` was amended to stop
  ignoring it. Every Rust program here is a binary rather than a library, and the
  Rust convention for binaries is to commit the lockfile: it makes package builds
  reproducible and stops a bad upstream crate release entering a build unnoticed.
  13 packages are locked.

## 10. Still to verify

1. **`busy_since_ms` against a genuinely busy session** — needs a real agent turn
   in flight. `POST /shell` does not make a session `busy`.
2. **`podman ps --format json` field availability** on the target podman version:
   whether `Created` is always numeric seconds and whether `.Pid` is present
   (the code uses a batched `podman inspect` for PIDs and accepts several spellings
   of `Id`/`Names`/`Created`). No podman is available in the dev container.
3. **Nine concurrent containers** — behaviour and load with the real thing;
   two podman invocations total per run regardless of count, but this is untested
   above one.
4. **A container up but not yet listening** — expected to render `----`/`--:--`,
   exercised only by unit tests so far.
