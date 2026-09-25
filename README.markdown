# dak-nuggets

A collection of small, independent helper programs used alongside
[DAK](https://github.com/TheHolm/dak) (**D**ynamic **A**jazz **K**eyboard),
the Rust tool for controlling Ajazz- and Mirabox-branded USB macro keypads.

DAK drives its runtime behaviour from a `config.json` file. Helpers plug into
it in one of three ways:

- **`text_exec` / `image_exec`** — the helper prints text (or a whole image
  file) to stdout; DAK draws it onto a button LCD.
- **`launch`** — DAK starts the helper detached so it runs on its own.
- **`$(command)`** — the helper's output is captured inline into a DAK
  variable or action string.

There is deliberately no shared runtime or common dependency between the
programs in this repo: each one is self-contained, in whatever language and
build system fits its job, and documents itself in its own subdirectory.

## Included programs

| Program | Platforms |
| --- | --- |
| [`gnome-next-meeting`](#gnome-next-meeting) | Linux, FreeBSD |
| [`opencode-podman-status`](#opencode-podman-status) | **Linux only** |

### `gnome-next-meeting`

Prints the time until the next calendar event for today, in `HH:MM` format
(`----` when nothing remains). Reads the enabled calendars from Evolution
Data Server (EDS), so it works with GNOME Calendar/Evolution.

Typical DAK use — show the countdown on a button:

```json
{
  "type": "text_exec",
  "params": "gnome-next-meeting",
  "refresh": 60
}
```

See [gnome-next-meeting/README.markdown](gnome-next-meeting/README.markdown)
for full details, build instructions and dependencies.

### `opencode-podman-status`

Reports what each [opencode](https://opencode.ai) instance running in a rootless
[podman](https://podman.io) container is doing, as three six-character lines:
`run: 3`, `wait:1`, `done:5` — how many instances are working, how many are
waiting for an answer from you, and how many are idle. `--instance` reports one
container instead.

**Linux only.** It queries each instance by entering that container's user and
network namespaces, and rootless podman does not exist on FreeBSD. It is
therefore absent from FreeBSD packages, and its `make` targets short-circuit
there so a collection-wide build still succeeds.

Typical DAK use — show the summary on a button:

```json
{
  "type": "text_exec",
  "params": "opencode-podman-status",
  "refresh": 5
}
```

Each container must run opencode with an explicit port, e.g.
`opencode --port 4096`; setting `server.port` in `opencode.json` does not work.

See
[opencode-podman-status/README.markdown](opencode-podman-status/README.markdown)
for full details, including the security implications of that port.

## Building

Each program builds with its own native tooling. The top-level `Makefile` is
a thin POSIX-compatible orchestrator:

```
make build     # build every helper
make test      # run every helper's tests
make coverage  # test coverage report for every helper (needs gcovr)
make install   # install every helper (PREFIX/DESTDIR honoured)
make clean     # clean build artifacts
```

Build and test commands for an individual program are documented in that
program's own `README.markdown`.

## License

GNU Affero General Public License v3 or later — see [LICENSE](LICENSE).
