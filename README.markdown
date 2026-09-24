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
