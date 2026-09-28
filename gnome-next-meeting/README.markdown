# gnome-next-meeting

Prints countdowns to your calendar meetings for the rest of today, one per
line, by reading the enabled calendars from Evolution Data Server (EDS) — the
calendar backend behind GNOME Calendar and Evolution.

## Output

Each line is exactly six characters wide:

- `" HH:MM"` (leading space) — time until a meeting **starts**.
- `"-HH:MM"` (leading `-`) — time until a meeting already in progress
  **ends**.
- `----` — nothing left today.

The soonest countdown comes first. A meeting under way contributes only the
countdown to its end — however long ago it began, including on a previous day —
and a meeting still to come contributes only the countdown to its start.
Overlapping ("clashing") meetings therefore each keep their own line, so a
clash is visible as two starts falling inside one another.

At most three lines are printed by default, which is exactly what a DAK
button can display; use `--lines N` for more. Given a meeting running until
10:00, one from 10:00 to 11:00 and a clashing one from 10:30 to 11:30, at
09:50 the output is:

```
-00:10
 00:10
 00:40
```

Times are computed in the system timezone. Recurring events are expanded so
recurrences are counted, including single occurrences moved out of their slot.
The same meeting subscribed in two calendars is shown once, not twice. All-day
events are ignored, since a `HH:MM` countdown does not apply to them. A meeting
that is under way keeps counting down to its end even when that end falls after
midnight, so the hours field may exceed 24 (`-27:15`); countdowns of 100 hours
or more are clamped to `99:59` to preserve the six-character width. Meetings
which start after midnight belong to tomorrow and are not shown.

Countdowns falling at the very same second are ordered by a stable hash of
the meeting's identity, so the order never changes between runs.

## Options

- `--lines N` (`-l`) — print at most `N` countdown lines (default 3, minimum 1)
- `--before TEXT` (`-b`) — printed before the countdowns
- `--after TEXT` (`-a`) — printed after the countdowns
- `--fmt KEYS=TAGS` — colour countdown lines with DAK tags; see
  [Colouring buttons](#colouring-buttons)
- `--soon MINUTES` — threshold for the `soon` key (default 10, minimum 1);
  see [Colouring buttons](#colouring-buttons)
- `--ending MINUTES` — threshold for the `ending` key (default 10, minimum 1);
  see [Colouring buttons](#colouring-buttons)
- `--help` (`-h`) — usage summary, including the program version

Within `TEXT`, the escapes `\n`, `\t` and `\\` are expanded to a newline, a
tab and a literal backslash, so newlines can be embedded. The decoration
wraps the whole block of countdowns once — not each line — and applies to the
`----` marker as well.

```
$ gnome-next-meeting --lines 1 --before 'Starts in ' --after ' minutes'
Starts in  00:30 minutes

$ gnome-next-meeting --lines 2 --before 'Next:\n'
Next:
 00:30
 01:15
```

### Colouring buttons

DAK (>= 0.13, with `markup` left at its default `"tmux"`) understands
tmux-style tags such as `#[fg=red,bold]` in button text. `--fmt` wraps this
program's own countdown lines in tags of your choosing, so a button can turn
red when a meeting is about to start and stay grey otherwise. With no `--fmt`
given, output is exactly the plain text shown under [Output](#output) —
nothing changes unless you ask for it.

`--fmt` takes one argument shaped `KEYS=TAGS`: `KEYS` is one or more
comma-separated keys, and `TAGS` is one or more DAK `#[...]` tags applied to
every named key. Give `--fmt` again to set more keys; setting the same key
twice keeps the later value. `TAGS` must be made only of `#[...]` tags — no
plain text, no newlines — since that is what keeps every line six characters
wide.

The keys are:

- `start` — a not-yet-started meeting whose countdown is at or above the
  `--soon` threshold
- `soon` — a not-yet-started meeting whose countdown is below the `--soon`
  threshold (default 10 minutes)
- `end` — a meeting in progress whose countdown is at or above the
  `--ending` threshold
- `ending` — a meeting in progress whose countdown is below the `--ending`
  threshold (default 10 minutes)
- `none` — the `----` no-meetings marker

Once any key is set, every countdown line gets a tag: the one configured for
its own key, or plain `#[default]` for a key you left out. Keys never borrow
from one another — leaving `soon` unset does *not* fall back to `start`'s
tag, so a meeting five minutes out renders in `#[default]` unless `soon`
itself is set. Set both explicitly (`--fmt start,soon=...`) for the same
colour regardless of how soon it is.

```
$ gnome-next-meeting --fmt start,end='#[fg=gray]' \
    --fmt soon='#[fg=yellow,bold]' --soon 10 \
    --fmt ending='#[fg=red,bold]' --ending 5
```

Grey while nothing is imminent, yellow for a meeting starting inside 10
minutes, red for one about to end inside 5.

`--before`/`--after` text is never tagged — only the countdown lines
themselves are. If `--after` text follows a tagged line, it inherits that
line's style until DAK's markup parser sees something else; put your own
`#[default]` in `--after` if you need it left untouched.

## DAK integration

The output is plain text, so it fits DAK's `text_exec` setup type directly.
A typical button showing the countdowns refreshed every minute:

```json
{
  "type": "text_exec",
  "params": "gnome-next-meeting",
  "refresh": 60
}
```

With colour, see [Colouring buttons](#colouring-buttons):

```json
{
  "type": "text_exec",
  "params": "gnome-next-meeting --fmt start,end='#[fg=gray]' --fmt soon,ending='#[fg=red,bold]'",
  "refresh": 60
}
```

Only the first six characters of the first three lines of stdout are shown on
a button LCD, which is exactly the default output: three six-character
countdowns, most imminent first.

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

Run the unit tests (the decision/formatting logic in `next_meeting.c`, plus
the command-line argument validation in `main.c` run against the real built
binary — neither needs a live EDS session):

```
make test
```

For a coverage report of the decision/formatting logic (needs `gcovr`; on
Debian/Ubuntu `sudo apt install gcovr`):

```
make coverage
```

To verify the full behaviour against a real calendar, run the binary directly
against a live EDS session (see `NOTES.md`):

```
./build/gnome-next-meeting
```

## License

GNU Affero General Public License v3 or later.
