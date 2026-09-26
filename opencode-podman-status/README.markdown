# opencode-podman-status

Reports what each [opencode](https://opencode.ai) instance running in a rootless
[podman](https://podman.io) container on this machine is doing: how many are
working, how many are waiting for you, and how many are idle.

The recommended setup is to enable the bundled **read-only status plugin** in
each container's opencode and to run opencode **without `--port`**. `opencode
--port` exposes opencode's full remote-control API. Anything that can reach that
port can use it to skip every human check, and that includes the agent itself,
from inside its own container. See [Security](#security).

**Linux only.** It works by creating sockets inside rootless podman containers'
network namespaces, and rootless podman does not exist on FreeBSD. See
[Platform support](#platform-support).

## Output

With no arguments it prints three six-character lines, sized for a DAK button LCD:

```
run: 3
wait:1
done:5
```

- **`run`**: instances with at least one session working (`busy` or `retry`)
- **`wait`**: instances waiting for you. That means a question or permission
  prompt, or (status plugin only) a turn that failed with an error
- **`done`**: instances reachable and fully idle

Each container counts in exactly one bucket. The precedence is **wait > error >
run > done**, because something that needs you is always the more useful thing
to surface. Errors are counted on the `wait:` line, since the button has only
three lines and a failed turn needs the operator just as a question does.
Counts are clamped at 9.

Some containers are counted in *none* of the three:

- no opencode process is running in them;
- opencode is running but has neither the plugin nor `--port`;
- opencode is still starting.

So the numbers can legitimately add up to less than the number of containers.
`--list` shows why each one was left out.

While a container is being stopped or removed, podman itself can stop
answering for several seconds (see [When podman is busy](#when-podman-is-busy)).
The counts are then unknown and shown as dashes:

```
run: -
wait:-
done:-
```

### One container at a time

`--instance` takes a slot number or a container name and prints three lines:

```
$ opencode-podman-status --instance 2
api        # container name, "opencode-" stripped, truncated to 6 characters
wait       # run | wait | done | Error | ----
00:12      # hh:mm in that state
```

- `Error` means the instance's last turn failed, for example with a provider or
  API error. Only the status plugin can report it; a user abort (Esc) is not an
  error.
- `----` / `--:--` means the container exists but could not be probed, for any
  of the reasons above. The problem stays visible on the button, and `--list`
  says which reason it is.
- A slot that **does not exist prints nothing at all**, so unused DAK buttons
  stay blank.
- `------` / `????` / `--:--` means podman did not answer in time, so it is not
  even known which container the slot is
  ([When podman is busy](#when-podman-is-busy)).

Slots are numbered from 1 by container creation time, oldest first. That is
stable across runs, which is what a fixed button needs. Terminating a container
does renumber the ones created after it.

## Requirements

Each container needs **one** of these:

1. **The status plugin** (recommended). See [Enabling the status
   plugin](#enabling-the-status-plugin). opencode runs normally, with no
   `--port`.
2. **`opencode --port 4096`**, using opencode's own API. This works, but read
   [Security](#security) first. The same port in every container is fine,
   because each container has its own network namespace. Setting `server.port`
   in `opencode.json` does **not** work: opencode's schema scopes that block to
   `opencode serve` and `web`, and the TUI ignores it.

Neither option needs published ports (`-p`), `--hostname 0.0.0.0`, podman labels
or bind mounts.

The helper must run **as the user who started the containers**. opencode must
run as a UID inside the container that maps back to that user, which is root
inside the container, the default. See [How it works](#how-it-works).

## Enabling the status plugin

The plugin is a single JavaScript file, installed by the package at:

| Installed by | Location |
|---|---|
| `.deb` (Debian, Ubuntu, and the `dak-nuggets` bundle) | `/usr/share/opencode-podman-status/opencode-podman-status.js` |
| `make install` | `$PREFIX/share/opencode-podman-status/opencode-podman-status.js` (default `PREFIX=/usr/local`) |
| FreeBSD `.pkg` | not packaged, since the program is Linux-only |

To enable it, add its path to the `plugin` array of an `opencode.json` that
opencode reads, for example the global `~/.config/opencode/opencode.json` inside
the container:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["/usr/share/opencode-podman-status/opencode-podman-status.js"]
}
```

opencode resolves that path **inside the container**, so the file must exist at
that path there. Any `plugin` entries you already have stay as they are; add
this one to the list. opencode loads plugins at startup without asking, so
restart opencode after the change.

What the plugin does:

- It listens on **`127.0.0.1` only**, on port **4097**. Set
  `OPENCODE_STATUS_PORT` in opencode's environment to change the port, and give
  the helper the matching `--plugin-port`.
- It answers only `GET` on four routes: `/global/health`, `/session/status`,
  `/question` and `/permission`. It uses opencode's own response shapes,
  reduced to session state and `since_ms`. Every other path gets 404 and every
  other method 405.
- Nothing can be changed through it, and it returns no titles, messages, file
  paths or error text.
- If opencode is started with `OPENCODE_SERVER_PASSWORD`, the plugin requires
  the same HTTP Basic credentials (`OPENCODE_SERVER_USERNAME`, default
  `opencode`). Give the helper the same password, as described in
  [Usage](#usage).
- Its data comes from opencode's own events as they happen. A freshly started
  instance therefore shows `done` with an unknown age (`--:--`) until something
  happens in it.

## Usage

```
opencode-podman-status [options]

  --instance <slot|name>  Detail for one container: name, state (run, wait,
                          done, or Error), time in state. Prints nothing at
                          all if that slot does not exist.
  --list                  Diagnostic table of every container (not for DAK).
  --pid <n>               Diagnostic: probe this process's namespaces directly,
                          bypassing podman, and print the raw JSON report.
  --source <auto|plugin|api>
                          Take the state from the status plugin, from
                          opencode's own API, or (auto, the default) from the
                          plugin when present and the API otherwise.
  --plugin-port <n>       Port the status plugin listens on (default 4097;
                          OPENCODE_STATUS_PORT in the container changes it).
  --port <n>              Use this port instead of discovering it.
  --timeout <ms>          Time budget per container, all requests included
                          (default 2500). Every run also ends within 4 s in
                          total, podman included, to stay inside DAK's 5 s.
  --password-file <path>  Password for opencode servers started with
                          OPENCODE_SERVER_PASSWORD; one for all containers.
                          The file should be mode 600.
  --password <pw>         The same, given directly. Visible to every local
                          user via ps(1) - prefer --password-file.
  --username <name>       Username for the above (default opencode).
  -h, --help              Usage.
  -V, --version           Version.
```

Containers are matched by name: exactly `opencode`, or `opencode-` followed by
anything. Lookalikes such as `openconnect` or `my-opencode` are never probed.

Exit status is 0 whenever the situation could be reported. That includes "no
containers at all", which prints `run: 0` / `wait:0` / `done:0`, and "slot does
not exist", which prints nothing. Exit status 1, with a message on stderr, is
reserved for a bad command line, an unreadable password file, or podman being
unavailable.

### Passwords

A single password is used for every container.

- **`--password-file <path>`** is the one to use. One trailing newline is
  stripped. If the file is readable by group or others, the helper warns on
  stderr (and still runs); `chmod 600` it.
- **`--password <pw>`** also works, but **every local user can read it** through
  `ps` and `/proc/<pid>/cmdline`, on every DAK refresh, unless `/proc` is
  mounted with `hidepid`. It is also stored in plain text in DAK's
  `config.json`.

The password is sent only as an HTTP `Authorization` header, from the helper's
own process. It never enters a container. A wrong or missing password shows in
`--list` as `HTTP 401: authentication required or wrong password`.

### Diagnosing

`--list` prints one line per container: slot, name, state, age, PID, port,
which server answered (`via=plugin` or `via=api`), and any failure reason:

```
$ opencode-podman-status --list
 1  opencode-web             run      00:03  pid=41233    port=4097   via=plugin
 2  opencode-api             error    00:41  pid=41560    port=4097   via=plugin
 3  opencode-db              unknown  --:--  pid=41890    port=-      via=-       opencode is running but listens on nothing: enable the status plugin (see README.markdown)
 4  opencode-tmp             unknown  --:--  pid=42011    port=-      via=-       opencode is not running in this container
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

If opencode is password-protected:

```json
{
  "type": "text_exec",
  "params": "opencode-podman-status --password-file /home/me/.config/opencode-podman-status/password",
  "refresh": 5
}
```

DAK renders only the first six characters of the first three lines, which is
exactly what this program emits. Keep `refresh` at 5 s or more: a run can take
up to 4 s (see below), so a shorter interval can overlap invocations.

### When podman is busy

DAK kills a `text_exec` command that has not finished within 5 s and draws a red
"Error" on its button. It does the same for any non-zero exit. So every run of
this program ends within **4 s**, whatever podman and the containers do:
podman discovery gets up to 3 s of that, and each container's probe
(`--timeout`, default 2.5 s) is cut short if less time is left.

The budget matters because of podman, not opencode. While a container is being
removed, including a `--rm` container that has just stopped, podman holds a
lock for as long as it takes to delete the container's storage, and **every**
podman command waits for it (`ps`, `inspect`, even `images`). Measured with a
large write layer, that lasted about 15 s.

During that window the program does not wait for podman. It gives up after
3 s, prints the dashes shown under [Output](#output), exits 0 (so DAK shows no
"Error"), and names the reason on stderr. The next refresh after podman
recovers shows real state again. Any other podman failure, such as podman
missing or `podman ps` exiting with an error, is still reported as an error.
`--list` also reports a busy podman as an error, since it is a diagnostic.

## How it works

Whichever server answers (the plugin, or opencode's API when `--port` is used),
it listens on the container's `127.0.0.1`. Loopback belongs to a network
namespace, so the host cannot reach that address, and rootless podman gives the
host no route to container addresses at all.

**A socket belongs for life to the network namespace it was created in**, and
the helper relies on that. For each container:

1. **Find the port by socket ownership, from the host.** `/proc/<pid>/net/tcp`
   is the TCP table of *that process's* network namespace. The helper:
   - finds the processes named `opencode` in the container's network namespace;
   - collects the socket inodes they hold from `/proc/<pid>/fd`;
   - keeps only the listeners with a matching inode.

   It never tries ports, so **other servers in the same container are never
   contacted**, not even to ask whether they are opencode.
2. **Create sockets inside the container.** A short-lived forked child joins
   the container's user namespace, then its network namespace, creates a few
   unconnected TCP sockets, and hands them to the parent over a Unix socket
   (`SCM_RIGHTS`). Then it exits. It runs none of the program's own logic: no
   HTTP, no JSON, and no `exec` into anything.
3. **Talk HTTP from outside.** The parent, which never changes namespace,
   connects those sockets to `127.0.0.1:<port>`. The connections land on the
   container's loopback. All requests and parsing happen in the parent.

Joining the namespaces needs no privileges beyond being the right user. From
`setns(2)`:

> A process reassociating itself with a user namespace must have the
> `CAP_SYS_ADMIN` capability in the target user namespace. […] Upon successfully
> joining a user namespace, a process is granted all capabilities in that
> namespace […]

Rootless podman creates the container's user namespace as the invoking user, and
a process whose effective UID owns a user namespace holds all capabilities in it.
This is the same mechanism `podman unshare` and `nsenter -U -n -t` use: no root,
no setuid, no file capabilities.

Four GETs per container determine the state: `/global/health`,
`/session/status`, `/question` and `/permission`. With the plugin, every age
comes directly from its `since_ms` fields. With opencode's API it is decoded
from opencode's timestamp-bearing IDs, which may need one or two extra requests.

The container side is treated as untrusted:

- each container gets one hard deadline (`--timeout`) covering everything;
- each response is capped at 1 MiB, and each container at a 4 MiB total and 12
  requests;
- IDs taken from one response must look like plain opencode IDs before they are
  used in the next request;
- control characters are never printed to your terminal.

`NOTES.md` covers the reverse-engineering behind all of this, and what was
measured rather than assumed.

## Security

**`opencode --port` gives any client that can reach that port an easy way to
bypass every human check.** The port serves opencode's full remote-control API.
Through it, a client can:

- **answer the agent's own permission and question prompts**
  (`POST /permission/<id>/reply`, `POST /question/<id>/reply`). The approval
  step that is supposed to gate dangerous tool calls is then skipped;
- **run shell commands** (`POST /session/<id>/shell`) and **open terminals**
  (the PTY routes), which never ask anyone at all;
- send the agent new instructions, change its configuration, read files, and
  stream the whole conversation.

The client that can most easily reach that port is **the agent itself**. Its
bash tool runs inside the same container, where `127.0.0.1:4096` is right
there. A single `curl` lets the agent approve its own permission request, or
skip the request entirely and run the command through the API. Container
isolation doesn't help here, because the agent is already inside the container.

**A password does not fix this.** `OPENCODE_SERVER_PASSWORD` makes the API
require HTTP Basic auth. But opencode takes the password only from its own
environment, and every process it starts inherits that environment: the bash
tool, terminals, LSP servers, MCP servers and formatters. The agent can
therefore read it with `env`. A password does keep out other local processes
that cannot see the container's environment, and nothing more.

What `--port` does *not* do: it binds `127.0.0.1` only, and loopback is per
network namespace. The API is therefore not reachable from other containers, the
host network or the LAN, unless you also add `--hostname 0.0.0.0` (also implied
by `--mdns`) or publish the port with `podman run -p`. Never do either.

**Recommendation:** use the status plugin and run opencode **without `--port`**.
The plugin gives the agent nothing it could use: it is read-only, it only
reports session states and times, and it offers no route that changes anything.
The worst the agent can do to it is stop it loading, which makes that container
unmonitored. It gains no extra control that way. Use `--port` only if you accept
that the agent in that container can approve its own actions.

On the host side:

- The helper never runs its own logic inside a container. Only a socket-creating
  child enters the namespaces, briefly (see [How it works](#how-it-works)).
- `--password` is visible to every local user. Use `--password-file` with mode
  600.

## Building

```
make build     # cargo build --release
make test      # cargo test, plus the plugin's tests if Bun is installed
make coverage  # needs cargo-llvm-cov
make install   # binary and plugin; PREFIX/DESTDIR honoured
make clean
```

Or with Cargo directly. Dependencies are `serde`, `serde_json` and `libc`. HTTP
is hand-rolled, since every request is a plain loopback GET.

**Running the plugin's tests needs [Bun](https://bun.sh)** (`bun test` in
`plugin/`). Nothing else does: the plugin runs inside opencode, which has Bun
built in, and building or packaging needs no Bun at all. Without Bun, `make test`
skips the plugin tests with a message. Set `BUN=/path/to/bun` if it is not on
`PATH`.

The namespace mechanism has its own opt-in integration test. It is kept out of
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
packaging. Its `Makefile` targets short-circuit with a message on non-Linux
systems, so a collection-wide `make` still succeeds.

## License

GNU Affero General Public License v3 or later. See [LICENSE](../LICENSE).
