# gnome-next-meeting

Prints the time remaining until the next calendar event of the day, in
`HH:MM` format, by reading the enabled calendars from Evolution Data Server
(EDS) — the calendar backend behind GNOME Calendar and Evolution.

## Output

- `HH:MM` — hours and minutes until the next event that starts later today.
- `----` — no further events today.

Times are computed in the system timezone. Recurring events are expanded so
recurrences are counted.

## Decoration

The output can be wrapped with text supplied on the command line:

- `--before TEXT` (`-b`) — printed before the time
- `--after TEXT` (`-a`) — printed after the time

Within `TEXT`, the escapes `\n`, `\t` and `\\` are expanded to a newline, a
tab and a literal backslash, so newlines can be embedded. The decoration
applies to the `----` marker as well. `--help` prints the usage summary.

```
$ gnome-next-meeting --before 'Starts in ' --after ' minutes'
Starts in 00:30 minutes

$ gnome-next-meeting --before 'Next:\n' --after '\n'
Next:
00:30
```

## DAK integration

The output is plain text, so it fits DAK's `text_exec` setup type directly.
A typical button showing a countdown refreshed every minute:

```json
{
  "type": "text_exec",
  "params": "gnome-next-meeting",
  "refresh": 60
}
```

Only the first six characters of the first three lines of stdout are shown
on a button LCD, which is more than enough for the `HH:MM` / `----` output.

## Build

Dependencies (build-time dev headers):

- GLib
- Evolution Data Server: `libedataserver` and `libecal`

On Debian/Ubuntu:

```
sudo apt install libecal2.0-dev libedataserver1.2-dev
```

Build with Meson (or use the top-level dak-nuggets `make build`):

```
meson setup build
meson compile -C build
```

The `Makefile` in this directory wraps Meson so the root `make` targets work.

## Test

Run the unit tests (the decision/formatting logic in `next_meeting.c`, no
live EDS needed):

```
make test
```

To verify the full behaviour against a real calendar, run the binary directly
against a live EDS session (see `NOTES.md`):

```
./build/gnome-next-meeting
```

## License

GNU Affero General Public License v3 or later.
