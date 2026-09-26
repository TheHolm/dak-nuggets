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

### What actually enters the namespace (since 0.2.0): one socket-making child

A socket belongs for life to the network namespace it was **created** in, whoever
later holds it. So only `socket(2)` has to happen inside the container. The
mechanism (`src/ns_socket.rs`):

1. the parent opens `/proc/<pid>/ns/{user,net}` and a `SOCK_SEQPACKET`
   socketpair, and allocates the `SCM_RIGHTS` control buffer - all before `fork`;
2. the child: `prctl(PR_SET_DUMPABLE, 0)`, `setns(user)`, `setns(net)`, 12 ×
   `socket(AF_INET)`, one `sendmsg` carrying `[stage, errno]` plus the fds,
   `_exit`. No allocation, no `exec`, nothing but raw syscalls - required because
   the parent is multithreaded, so only async-signal-safe calls are sound;
3. the parent polls the socketpair against the container's deadline, wraps every
   received fd immediately (so nothing leaks on any error path), kills the child
   if it overran, and always reaps it (`Reaper`'s `Drop`);
4. the parent `connect()`s those sockets to `127.0.0.1:<port>` non-blockingly
   against the same deadline (`http::connect_socket`) and does all HTTP/JSON
   itself. It never changes namespace.

Twelve sockets is also the **cap on requests per container** - there is no
second trip. The child reports *which step* failed, so `--list` says "joining
the user namespace failed: …" rather than a bare errno.

Before 0.2.0 the child instead re-`exec`ed this whole program with `__probe`
inside the namespaces, reporting back JSON on stdout. Replaced because: the full
HTTP + JSON stack then ran holding the container's user-namespace credentials
(container root has every capability over such a process); it inherited DAK's
environment and cwd; it depended on `/proc/self/exe` still existing, which a
package upgrade breaks (`(deleted)`); and the parent waited on `output()` with no
bound, so a wedged child hung the run. `__probe` is now rejected as an unknown
argument.

Residual exposure, stated honestly: for the microseconds between `setns` and
`_exit` the child is a copy of the parent in the container's user namespace. It is
non-dumpable, which stops same-UID processes attaching, but a process with
`CAP_SYS_PTRACE` *in that user namespace* (container root) is not stopped by that.
It would also need to name the child's PID, and the child is not in the container's
PID namespace, so container root cannot see it in its `/proc`. Any secret the
parent holds in memory is therefore exposed, at most, through this window and
these conditions.

### Three constraints that shaped the code

1. **Single-threaded.** `setns(CLONE_NEWUSER)` refuses a multithreaded process. A
   child immediately after `fork` has one thread — only the calling thread is
   carried over — so the parent *may* use threads to overlap probes, as
   `probe_all` does. The `setns` calls must nonetheless happen post-fork.
2. **No `CLONE_FS` sharing.** `fork` copies fs attributes; threads share them. So
   the work cannot be done on a thread of the parent, only in a forked child.
3. **Cannot re-enter your own userns** (`EINVAL`). `shares_our_namespace` checks
   this up front, which also stops the program probing the container it is itself
   running in. The unit test `reports_kernel_refusal_from_the_child` relies on
   exactly this refusal to exercise the fork/report/reap path without a second
   namespace.

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

Re-validated for the 0.2.0 mechanism, again as uid 1000 with no capabilities,
against `unshare -Urn` holding a listener renamed `opencode`:

```
/proc/<pid>/net/tcp read from the host:     ['0100007F:B927']       (its table, not ours)
/proc/<pid>/fd read from the host:          socket:[...] links visible
child setns(user,net) + socket + SCM_RIGHTS: exit 0
parent connect() on the received socket:    b'HTTP/1.1 200 OK ... hello-from-ns'
parent's own netns afterwards:              unchanged
```

`tests/namespace-entry.sh` reproduces the same shape through this program's own
code (isolation, `--pid` entry, port discovery by socket ownership, and a decoy
server that must receive no requests), and passes both as root and via
`setpriv --reuid=1000 --regid=1000 --clear-groups`. It is **not** part of
`make test`: it needs to create a nested user namespace, which CI containers
commonly forbid. It exits 2 to mean "skipped". The unit test
`creates_sockets_inside_a_nested_namespace` covers the socket path in-process
(checking each socket's namespace with `SIOCGSKNS`) and passes vacuously, saying
so, where nesting is forbidden.

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

**Now** (`src/sockets.rs`), entirely from the host (since 0.2.0; before that the
same scan ran inside the namespaces, in the re-executed child):

1. read listeners from `/proc/<container-pid>/net/tcp` and `…/tcp6` - these are
   the tables of *that process's* network namespace - field 10 is the socket
   **inode**;
2. scan `/proc/*` for processes whose `/proc/<pid>/ns/net` equals the container's
   (i.e. are in this container; host PIDs) and that are opencode: `comm == "opencode"` or `basename(argv[0]) ==
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
- **Permission to read `/proc/<pid>/fd`** needs ptrace-read access. The host-side
  process has it because it has the same host UID as the container's root (the
  mapped UID). This is the same condition as opening `/proc/<pid>/ns/*` - see the
  known limitation in §2.
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

**Correction (0.2.0): that `EAGAIN` was most likely not slowness at all.** On a
cold instance opencode 1.18.32 sends the complete `/session/status` response,
`Content-Length` included, and then **leaves the socket open** despite our
`Connection: close`. The old client read to EOF, so it sat there until the read
timeout fired (`EAGAIN`) - with any timeout. Caught under strace: the 123-byte
response arrived at +0.1 ms, the next `recvfrom` returned `EAGAIN` 2.4 s later.
Warm instances do close. The client now stops at `Content-Length` when present
(`http::expected_total_len`, pinned by
`stops_at_content_length_when_server_keeps_socket_open`) and only reads to EOF
without one.

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

**Password support (0.2.0).** Originally rejected for the reasons in the next
paragraph, and those still hold; it is supported now because a user asked to run
opencode with `OPENCODE_SERVER_PASSWORD` set, and the helper then gets 401. The
helper takes one password for every container, via `--password-file` (warned
about if group/other-readable, one trailing newline stripped, max 4 KiB) or
`--password` (visible in `ps` / `/proc/<pid>/cmdline` to every local user on
every DAK refresh, and stored in DAK's `config.json`), plus `--username`
(default `opencode`, matching `OPENCODE_SERVER_USERNAME`'s default). Sent as
`Authorization: Basic` with hand-rolled base64 (RFC 4648 vectors in the tests).
Only the parent sends requests, so the credentials never enter a container's
context, and they are redacted from every `Debug` impl. Verified against a real
1.18.32 `serve --port` with the variable set: no password and a wrong one both
give "HTTP 401: authentication required or wrong password", the right one works.

Sharing one password across containers adds no exposure: each container's agent
already holds it (inherited environment, below), and a container's API is
reachable only from inside its own network namespace, so knowing the password
lets no container reach another.

**Why the password protects little.** The server reads it only from
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

## 6a. The status plugin (0.2.0)

`plugin/opencode-podman-status.js` lets opencode be monitored **without `--port`**,
i.e. without exposing the full remote-control API (§6). It serves GET-only
`/global/health` (with `source:"opencode-podman-status-plugin"`),
`/session/status`, `/question`, `/permission` - opencode's own shapes plus
`since_ms`, nothing else - on `127.0.0.1:${OPENCODE_STATUS_PORT:-4097}`. Everything
else is 404, other methods 405. With `OPENCODE_SERVER_PASSWORD` set it requires the
same Basic credentials (SHA-256 + `timingSafeEqual`), checked before routing.

Measured against 1.18.32 (TUI and `serve`, with a mock OpenAI-compatible
provider driving real permission/question/error turns):

- An absolute path in `opencode.json`'s `plugin` array loads the file. Plugin
  context keys: `client, project, worktree, directory, experimental_workspace,
  serverUrl, $`.
- `Bun.serve` inside the plugin works in the TUI **without `--port`**, and the
  listener is held by the main `opencode` process (the plugin runs on the TUI's
  worker thread, same fd table), so §3a's ownership discovery finds it unchanged.
- **Every export of a plugin module is treated as a plugin and must be a function**
  (`TypeError("Plugin export is not a function")`). Hence one export, with test
  internals hung off it as `.internals`; a test pins this.
- Events: `permission.asked`/`question.asked` carry `id`+`sessionID`; replies carry
  `requestID` (`question.rejected` too). `session.status` busy repeats many times
  per turn, so `since_ms` is stamped only on a real change (busy<->retry keeps it).
- **`session.error` arrives before the turn's `idle`**, so error persists through
  that idle and clears on the next busy.
- **User abort emits `session.error` `MessageAbortedError`** - ignored, the operator
  was present.
- **Aborting with a permission open emits no reply event, and opencode's own
  `GET /permission` keeps listing it indefinitely** - so API mode can show a stale
  `wait`. The plugin drops a session's pending requests when it goes idle.
- A freshly started instance with no events yet reports `done` with no
  `since_ms` (shown `--:--`); there is nothing to date it by. A startup snapshot
  via `client` was considered and dropped: no session of a new instance can be
  active before the plugin loads.
- State lives on a `globalThis` symbol so several plugin initialisations in one
  process (one per project instance) share one tracker and one listener instead of
  fighting over the port. A port already in use is logged via `client.app.log` and
  opencode carries on.
- Tracked entries are capped (256 sessions/requests; only the newest idle kept).

Helper side: `--source auto|plugin|api` (auto prefers `--plugin-port`, default
4097, among *owned* listeners; the health marker, not the port number, decides
what a port is), and `State::Error` - counted in `wait:` in the aggregate,
`Error` on `--instance`, `error` in `--list`, which also gains `via=plugin|api`.
Tests: `bun test` in `plugin/` (Bun needed only for that; `make test` skips with a
message without it). Installed to `/usr/share/opencode-podman-status/` by the
`.deb`, `$PREFIX/share/opencode-podman-status/` by `make install`.

Dev-environment gotcha: **never `pkill` by the name `opencode`** here - the agent
doing the work is itself an `opencode` process. Kill test instances by verified
PID only (e.g. one whose netns differs from ours).

## 7. Rejected approaches, and why

| Approach | Why not |
|---|---|
| **Publish ports** (`-p` + `--hostname 0.0.0.0`) | Needs a per-container host port and exposes the unauthenticated API to every container on the network and to the host. Also hits the rootlesskit/pasta trap: forwarded traffic arrives at the container's interface address, not loopback, so a `127.0.0.1` bind silently never receives it. |
| **`podman exec` + curl** | Needs an HTTP client in every image; 100–300 ms per exec, so nine containers get slow. |
| **Plugin writing status files to a bind mount** | Push-based, but DAK polls on a timer, so observable freshness is identical — the advantage evaporates. Costs a bind mount per container, and gives the container a writable path into the host. *Reconsidered in 0.2.0:* a plugin that instead **serves** a read-only status API on the container's loopback (§6a) needs no mount, reuses the whole namespace transport unchanged, and removes the need for `--port` - which turned out to be the real security problem (§6). |
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
- **The namespace child must stay async-signal-safe.** It is forked from a
  multithreaded parent, so another thread may hold the allocator lock at fork
  time. Do not add anything to `child_main` that allocates, formats, logs, or
  touches `std` I/O - only raw `libc` calls on memory prepared before `fork`.
  (Before 0.2.0 this was sidestepped by `exec`ing; see §2 for why that went.)
- **All HTTP is in-process and testable without a namespace**: `http::Client` is
  generic over `Connect`, and tests use the `#[cfg(test)]` `Loopback` connector
  against plain `TcpListener`s. `probe.rs` tests drive the whole classification
  that way, including that a hostile session ID never reaches a request path.
- **One deadline per container, not per read.** `http::Client` re-applies the
  remaining time before every read and write. Per-read timeouts alone let a
  server that drips a byte every second hold a probe (and so DAK's invocation)
  for as long as it likes; `slow_drip_server_hits_the_overall_deadline` pins it.
- **Why there is still no HTTP library.** Responses end at `Content-Length`
  (opencode always sends one, and no chunking), or at EOF without one;
  `Connection: close` is sent but **not reliable** on a cold instance (§5). That
  is the whole of the framing needed. A client crate would have added eight
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
- **Coverage** (`make coverage`) leaves `discover.rs` and `main.rs` out of the
  table, since both need podman. Everything else, including `ns_socket.rs` and
  `probe.rs`, is now exercised in-process (the namespace test where nesting is
  permitted).
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

1. ~~`busy_since_ms` against a genuinely busy session~~ — verified in 0.2.0 with a
   mock provider holding a real turn open: API mode reported `run` with a
   `since_ms` 2 ms from the plugin's own stamp.
2. **`podman ps --format json` field availability** on the target podman version:
   whether `Created` is always numeric seconds and whether `.Pid` is present
   (the code uses a batched `podman inspect` for PIDs and accepts several spellings
   of `Id`/`Names`/`Created`). No podman is available in the dev container.
3. **Nine concurrent containers** — behaviour and load with the real thing;
   two podman invocations total per run regardless of count, but this is untested
   above one.
4. **A container up but not yet listening** — expected to render `----`/`--:--`,
   exercised only by unit tests so far. (Deliberately the same rendering as "no
   opencode in the container" and "no plugin and no `--port`"; `--list` tells
   them apart. Kept as-is on request.)
5. **The plugin under `opencode web` / desktop** — verified only in the TUI and
   `opencode serve`.
6. **The plugin across opencode upgrades** — it depends on bus event names and
   payload fields (§6a), which are not a documented stable interface. If a
   future opencode renames them the plugin degrades to "everything done"; re-run
   the event capture described in §6a after upgrading opencode.
