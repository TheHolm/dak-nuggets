# opencode-podman-status

Reports what each [opencode](https://opencode.ai) instance running in a rootless
[podman](https://podman.io) container on this machine is doing: how many are
working, how many are waiting for an answer from you, and how many are idle.

**Linux only.** It works by entering a rootless podman container's user and
network namespaces, and rootless podman does not exist on FreeBSD — see
[Platform support](#platform-support).

## Output

With no arguments it prints three six-character lines, sized for a DAK button LCD:

```
run: 3
wait:1
done:5
```

- **`run`** — instances with at least one session working (`busy` or `retry`)
- **`wait`** — instances with a question or permission prompt waiting on you
- **`done`** — instances reachable and fully idle

Each container counts in exactly one bucket, with precedence **wait > run >
done**: something needing a human answer is always the more useful thing to
surface. Counts are clamped at 9.

Containers that are running but whose opencode API cannot be reached count in
*none* of the three, so the numbers can legitimately sum to less than the number
of containers. `--list` shows why.

### One container at a time

`--instance` takes a slot number or a container name and prints three lines:

```
$ opencode-podman-status --instance 2
api        # container name, "opencode-" stripped, truncated to 6 characters
wait       # run | wait | done | ----
00:12      # hh:mm in that state
```

A container that exists but cannot be reached shows `----` and `--:--`, keeping
the misconfiguration visible. By far the most common cause is opencode having
been started without `--port`; `--list` says so explicitly. A slot that **does not exist prints nothing at
all**, so unused DAK buttons stay blank.

Slots are numbered from 1 by container creation time, oldest first. That is
stable across runs, which is what a fixed button needs. Terminating a container
does renumber the ones created after it.

## Requirements

Each container must run opencode with an **explicit `--port`**:

```
opencode --port 4096
```

The same port in every container is correct and intended: each container has its
own network namespace, so there is no conflict, and nothing needs publishing to
the host.

> **Setting `server.port` in `opencode.json` does not work.** opencode's own
> schema documents that block as *"Server configuration for opencode serve and
> web commands"* — the TUI resolves its network options without consulting the
> config, so a plain `opencode` opens no listening socket at all and talks to its
> own server in-process. This is by design, not a bug. If you cannot change the
> launch command, wrap it in the image: `exec opencode --port 4096 "$@"`.

Nothing else is required. No published ports (`-p`), no `--hostname 0.0.0.0`, no
podman labels, no bind mounts, no plugins, and no `OPENCODE_SERVER_PASSWORD`.

The helper must run **as the user who started the containers**, and opencode must
run as a UID inside the container that maps back to that user — which is the
default. See [How it works](#how-it-works).

## Usage

```
opencode-podman-status [options]

  --instance <slot|name>  Detail for one container: name, state, time in state.
                          Prints nothing at all if that slot does not exist.
  --list                  Diagnostic table of every container (not for DAK).
  --pid <n>               Diagnostic: probe this process's namespaces directly,
                          bypassing podman, and print the raw JSON report.
  --port <n>              Use this port instead of discovering it.
  --timeout <ms>          Per-request timeout (default 2500).
  -h, --help              Usage.
  -V, --version           Version.
```

Containers are matched by name: exactly `opencode`, or `opencode-` followed by
anything. Lookalikes such as `openconnect` or `my-opencode` are never probed.

Exit status is 0 whenever the situation could be reported, including "no
containers at all" (which prints `run: 0` / `wait:0` / `done:0`) and "slot does
not exist" (which prints nothing). Exit 1 with a message on stderr is reserved
for a bad command line or podman being unavailable.

### Diagnosing

`--list` prints one line per container with its slot, name, state, age, PID,
discovered port and any failure reason:

```
$ opencode-podman-status --list
 1  opencode-web             run      00:03  pid=41233    port=4096
 2  opencode-api             unknown  --:--  pid=41890    port=-      opencode is running without --port, so it has no API socket
```

## DAK integration

Show the aggregate on one button:

```json
{
  "type": "text_exec",
  "params": "opencode-podman-status",
  "refresh": 5
}
```

Or give each container its own button:

```json
{
  "type": "text_exec",
  "params": "opencode-podman-status --instance 1",
  "refresh": 5
}
```

DAK renders only the first six characters of the first three lines, which is
exactly what this program emits. Keep `refresh` above `--timeout` (default
2500 ms) so a slow container cannot cause overlapping invocations.

## How it works

opencode's TUI runs an HTTP server, and `--port` makes it listen on the
container's `127.0.0.1`. Loopback is per network namespace, so that socket is
reachable only from inside the container — and in rootless podman the host cannot
route to container addresses at all.

So the helper goes to the socket instead of the other way round. For each
container it forks, moves the child into the container's user and network
namespaces, and re-executes itself there; `127.0.0.1` is then the container's
loopback and opencode answers normally. The child reports back as JSON and the
parent aggregates.

This needs no privileges beyond being the right user. From `setns(2)`:

> A process reassociating itself with a user namespace must have the
> `CAP_SYS_ADMIN` capability in the target user namespace. […] Upon successfully
> joining a user namespace, a process is granted all capabilities in that
> namespace […]

Rootless podman creates the container's user namespace as the invoking user, and
a process whose effective UID owns a user namespace holds all capabilities in it.
This is the same mechanism `podman unshare` and `nsenter -U -n -t` use — no root,
no setuid, no file capabilities.

A container's namespaces are identified by reading `/proc/<pid>/ns/{user,net}`,
never by reasoning about podman's network topology.

The port is found by **socket ownership**, never by trying ports. Inside the
container, the helper looks for processes named `opencode`, collects the socket
inodes they hold from `/proc/<pid>/fd`, and keeps only the listening sockets in
the container's `/proc/net/tcp` with a matching inode. So nothing depends on a
port convention, and **other servers in the same container are never
contacted** — not even to ask whether they are opencode.

Three GETs per container — `/session/status`, `/question` and `/permission` —
determine the state. Only `--instance` pays for the extra request needed to work
out how long the instance has been in that state.

`NOTES.md` covers the reverse-engineering behind all of this, including opencode's
timestamp-bearing ID format and what was measured rather than assumed.

## Security

The port this program talks to is an **unauthenticated remote-control API**: it
can run shell commands, drive the agent, open a PTY, read files, answer the very
permission prompts that are meant to gate dangerous tool calls, and stream the
whole conversation. opencode only requires a password if
`OPENCODE_SERVER_PASSWORD` is set, and the TUI does not warn when it is not.

What keeps that acceptable here is that `--port` alone binds **`127.0.0.1` only**
(opencode's default `hostname`), and loopback is per network namespace. So the
API is reachable from inside that container, and from a process able to enter its
namespaces — which means the same user, who could already `podman exec` into it.
It is **not** reachable from other containers, from the host's network, or from
the LAN.

Two things would change that, and this program needs neither: `--hostname
0.0.0.0` (also implied by `--mdns`) and publishing the port with `podman run -p`.

A password was considered and deliberately rejected: opencode reads it only from
`OPENCODE_SERVER_PASSWORD`, there is no command-line alternative for the server
side, and every process opencode spawns — the bash tool, LSP servers, MCP servers,
formatters — inherits its environment. It would therefore be readable by exactly
the in-container code it would be defending against, while protecting nothing that
namespace isolation does not already protect.

## Building

```
make build     # cargo build --release
make test      # cargo test
make coverage  # needs cargo-llvm-cov
make install   # PREFIX/DESTDIR honoured
make clean
```

Or with Cargo directly. Dependencies are `serde`, `serde_json` and `libc`; HTTP is
hand-rolled, since every request is a plain loopback GET.

The namespace-entry mechanism has its own opt-in integration test, kept out of
`make test` because it needs to create a nested user namespace, which many CI
containers forbid:

```
sh tests/namespace-entry.sh
```

## Platform support

Linux only, and deliberately so:

- **Rootless podman does not exist on FreeBSD.** Podman there requires root;
  rootless mode is not supported.
- Podman on FreeBSD is experimental, built on jails with VNET, CNI and `pf`
  rather than namespaces.
- On FreeBSD, published ports are not reachable from the host to itself, so the
  approach an alternative design would have used is broken there too.

The program therefore ships no `ci-freebsd.sh` and is absent from FreeBSD
packaging, and its `Makefile` targets short-circuit with a message on non-Linux
systems so a collection-wide `make` still succeeds.

## License

GNU Affero General Public License v3 or later — see [LICENSE](../LICENSE).
