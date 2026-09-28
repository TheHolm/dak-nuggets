//! Formatting output for a DAK button.
//!
//! DAK renders only the first six characters of the first three lines of a
//! helper's stdout, so every line this module produces is at most six characters
//! wide and there are never more than three of them. The aggregate labels are
//! chosen so all three lines are exactly six characters: `run: 3`, `wait:1`,
//! `done:5` - note the extra space after `run:` that squares them up.

use crate::status::{Counts, State};

/// Width of a DAK button's text line, in characters.
const WIDTH: usize = 6;

/// Largest count that fits the layout. Callers are documented as supporting at
/// most nine containers; anything beyond that is clamped rather than widening
/// the line and being truncated mid-number by DAK.
const MAX_COUNT: usize = 9;

/// Shown in place of a state that could not be determined.
const UNKNOWN_STATE: &str = "----";

/// Shown in place of a duration that could not be determined.
const UNKNOWN_TIME: &str = "--:--";

/// Prefix stripped from container names before display.
const NAME_PREFIX: &str = "opencode-";

/// The DAK tag a line gets when nothing more specific was configured for it.
///
/// Used only once any formatting has been requested at all (see
/// [`SummaryFormats::is_empty`]/[`DetailFormats::is_empty`]); with no `--fmt` or
/// `--details-fmt` given, output stays exactly the old plain text.
const DEFAULT_TAG: &str = "#[default]";

/// DAK-tag overrides for the aggregate summary button, set by repeated `--fmt
/// KEYS=TAGS` arguments.
///
/// Each summary line has a "zero" and a "non-zero" format, since that is the
/// distinction DAK buttons most want to highlight (an idle `done:` line looks
/// different from a `wait:` line that needs attention). `unknown` covers the
/// placeholder shown while podman itself could not answer in time; a line with
/// no `unknown` format falls back to its own zero format, and finally to
/// [`DEFAULT_TAG`], since the dashes are not really "zero" or "non-zero".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SummaryFormats {
    pub run0: Option<String>,
    pub run1: Option<String>,
    pub wait0: Option<String>,
    pub wait1: Option<String>,
    pub done0: Option<String>,
    pub done1: Option<String>,
    pub unknown: Option<String>,
}

impl SummaryFormats {
    /// True when no `--fmt` was given at all, in which case rendering stays
    /// byte-identical to the plain, unformatted output.
    pub fn is_empty(&self) -> bool {
        self.run0.is_none()
            && self.run1.is_none()
            && self.wait0.is_none()
            && self.wait1.is_none()
            && self.done0.is_none()
            && self.done1.is_none()
            && self.unknown.is_none()
    }
}

/// DAK-tag overrides for the `--instance` detail button's state line, set by
/// repeated `--details-fmt KEYS=TAGS` arguments.
///
/// Only the state line (`run`/`wait`/`done`/`Error`/`----`) is ever styled; the
/// name and time lines always use [`DEFAULT_TAG`] once any formatting is in
/// effect, so a name or duration is never coloured by accident.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DetailFormats {
    pub run: Option<String>,
    pub wait: Option<String>,
    pub done: Option<String>,
    pub error: Option<String>,
    pub unknown: Option<String>,
}

impl DetailFormats {
    /// True when no `--details-fmt` was given at all.
    pub fn is_empty(&self) -> bool {
        self.run.is_none()
            && self.wait.is_none()
            && self.done.is_none()
            && self.error.is_none()
            && self.unknown.is_none()
    }
}

/// Checks that a `--fmt`/`--details-fmt` value is made only of DAK `#[...]`
/// tags: no bare text, no control characters (including newlines), and every
/// `#[` closed on the same value by a `]`. An empty value is valid (equivalent
/// to not setting that key). Keeping this strict is what keeps every rendered
/// line within DAK's three-line, six-column button, since tags themselves add
/// no visible width but arbitrary text would.
pub fn is_valid_format_value(value: &str) -> bool {
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '#' {
            return false;
        }
        if chars.next() != Some('[') {
            return false;
        }
        let mut closed = false;
        for c2 in chars.by_ref() {
            if c2 == ']' {
                closed = true;
                break;
            }
            if c2.is_control() {
                return false;
            }
        }
        if !closed {
            return false;
        }
    }
    true
}

/// The tag for one line: the configured one, or [`DEFAULT_TAG`].
fn tag(fmt: Option<&str>) -> &str {
    fmt.unwrap_or(DEFAULT_TAG)
}

/// Renders the aggregate three-line summary.
///
/// Counts are clamped to [`MAX_COUNT`]. Instances that could not be probed are
/// already excluded by [`Counts::tally`], so these three numbers may sum to less
/// than the number of containers present.
///
/// With `fmt` empty, output is exactly the old plain text. Otherwise every line
/// gets a leading DAK tag: the configured zero/non-zero format for that line's
/// count, or [`DEFAULT_TAG`] if that particular one was not set.
pub fn counts(counts: Counts, fmt: &SummaryFormats) -> String {
    let clamp = |n: usize| n.min(MAX_COUNT);
    let run = clamp(counts.run);
    let wait = clamp(counts.wait);
    let done = clamp(counts.done);
    if fmt.is_empty() {
        return format!("run: {run}\nwait:{wait}\ndone:{done}\n");
    }
    let run_tag = tag(if run == 0 { fmt.run0.as_deref() } else { fmt.run1.as_deref() });
    let wait_tag = tag(if wait == 0 { fmt.wait0.as_deref() } else { fmt.wait1.as_deref() });
    let done_tag = tag(if done == 0 { fmt.done0.as_deref() } else { fmt.done1.as_deref() });
    format!("{run_tag}run: {run}\n{wait_tag}wait:{wait}\n{done_tag}done:{done}\n")
}

/// Replaces control characters with `?`.
///
/// Container names and failure reasons can carry text that originated inside a
/// container. None of today's sources can contain control characters, but
/// printing one to a terminal could rewrite what the user sees (ANSI escapes),
/// so everything displayed goes through here regardless.
pub fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// Shortens a container name for display.
///
/// Strips a leading `opencode-`, neutralises control characters, and truncates
/// to six characters. A bare
/// `opencode` has no prefix to strip and so renders as `openco`; collisions with
/// names like `opencode-openconnect` are accepted deliberately, as six
/// characters cannot disambiguate everything.
pub fn short_name(container: &str) -> String {
    let stripped = container.strip_prefix(NAME_PREFIX).unwrap_or(container);
    // A name of exactly "opencode-" would strip to nothing; fall back to the
    // original so the line is never blank.
    let base = if stripped.is_empty() { container } else { stripped };
    truncate(&printable(base), WIDTH)
}

/// Truncates to at most `max` characters, respecting character boundaries.
fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => s[..byte_idx].to_string(),
        None => s.to_string(),
    }
}

/// Formats a duration as `hh:mm`.
///
/// Clamped to `99:59`, because a wider field would be cut off by DAK. Sub-minute
/// durations show as `00:00`: minute resolution is all the layout allows.
pub fn hhmm(seconds: i64) -> String {
    if seconds < 0 {
        return UNKNOWN_TIME.to_string();
    }
    let total_minutes = seconds / 60;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if hours > 99 {
        return "99:59".to_string();
    }
    format!("{hours:02}:{minutes:02}")
}

/// Renders the three-line detail view for a single instance.
///
/// `state` is `None` when the container exists but could not be probed,
/// typically because opencode has neither the status plugin nor `--port`, or is
/// still booting; that shows as `----`. An error shows as `Error`. `since_ms` is when the instance entered its current state, and
/// `None` shows as `--:--`.
///
/// The case of a slot that does not exist at all is *not* handled here: the
/// caller prints nothing, so unused DAK buttons stay blank.
///
/// With `fmt` empty, output is exactly the old plain text. Otherwise the name
/// and time lines always get [`DEFAULT_TAG`], and the state line gets the
/// format configured for that state (falling back to `unknown` for a
/// container that could not be probed, and to [`DEFAULT_TAG`] if that was not
/// set either).
pub fn instance(
    name: &str,
    state: Option<State>,
    since_ms: Option<i64>,
    now_ms: i64,
    fmt: &DetailFormats,
) -> String {
    let state_line = match state {
        Some(s) => s.label(),
        None => UNKNOWN_STATE,
    };
    let time_line = match (state, since_ms) {
        (Some(_), Some(since)) => hhmm((now_ms - since) / 1000),
        _ => UNKNOWN_TIME.to_string(),
    };
    if fmt.is_empty() {
        return format!("{}\n{}\n{}\n", short_name(name), state_line, time_line);
    }
    let state_tag = tag(match state {
        Some(State::Run) => fmt.run.as_deref(),
        Some(State::Wait) => fmt.wait.as_deref(),
        Some(State::Done) => fmt.done.as_deref(),
        Some(State::Error) => fmt.error.as_deref(),
        None => fmt.unknown.as_deref(),
    });
    format!(
        "{DEFAULT_TAG}{}\n{state_tag}{}\n{DEFAULT_TAG}{}\n",
        short_name(name),
        state_line,
        time_line
    )
}

/// Shown on the detail view's state line when podman itself could not say
/// which containers exist.
const UNKNOWN_PODMAN_STATE: &str = "????";

/// Shown on the detail view's name line when podman could not say which
/// container the slot refers to.
const UNKNOWN_NAME: &str = "------";

/// The aggregate summary when podman did not answer in time.
///
/// podman stalls for seconds behind a container lock while a container is being
/// removed, and then recovers by itself. The counts are unknown for that
/// moment, which is not an error worth DAK's red "Error" label: dashes in place
/// of the numbers say "no information right now", and the next refresh after
/// podman recovers shows real counts again.
///
/// With `fmt` empty, output is exactly the old plain text. Otherwise each line
/// uses `unknown` if set, else falls back to that line's own zero format (the
/// dashes are closer to "definitely not busy" than to any real count), else
/// [`DEFAULT_TAG`].
pub fn counts_unknown(fmt: &SummaryFormats) -> String {
    if fmt.is_empty() {
        return "run: -\nwait:-\ndone:-\n".to_string();
    }
    let run_tag = tag(fmt.unknown.as_deref().or(fmt.run0.as_deref()));
    let wait_tag = tag(fmt.unknown.as_deref().or(fmt.wait0.as_deref()));
    let done_tag = tag(fmt.unknown.as_deref().or(fmt.done0.as_deref()));
    format!("{run_tag}run: -\n{wait_tag}wait:-\n{done_tag}done:-\n")
}

/// The detail view when podman did not answer in time; see [`counts_unknown`].
///
/// Without podman not even the container's name is known, so the name and time
/// lines are always [`DEFAULT_TAG`]. Distinct from a slot that does not exist,
/// which prints nothing: whether this slot exists cannot be known either.
pub fn instance_unknown(fmt: &DetailFormats) -> String {
    if fmt.is_empty() {
        return format!("{UNKNOWN_NAME}\n{UNKNOWN_PODMAN_STATE}\n{UNKNOWN_TIME}\n");
    }
    let state_tag = tag(fmt.unknown.as_deref());
    format!("{DEFAULT_TAG}{UNKNOWN_NAME}\n{state_tag}{UNKNOWN_PODMAN_STATE}\n{DEFAULT_TAG}{UNKNOWN_TIME}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strips DAK `#[...]` tags, so tests can check line width regardless of
    /// whether formatting was applied.
    fn strip_tags(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '#' && chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2 == ']' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Every line of every rendering fits a DAK button, ignoring any tags.
    fn assert_fits(rendered: &str) {
        let stripped = strip_tags(rendered);
        let lines: Vec<&str> = stripped.lines().collect();
        assert!(lines.len() <= 3, "more than three lines: {rendered:?}");
        for line in lines {
            assert!(
                line.chars().count() <= WIDTH,
                "line {line:?} is wider than {WIDTH} characters"
            );
        }
    }

    /// The aggregate view matches the agreed layout exactly.
    #[test]
    fn renders_counts_in_agreed_format() {
        let out = counts(Counts { run: 3, wait: 1, done: 5 }, &SummaryFormats::default());
        assert_eq!(out, "run: 3\nwait:1\ndone:5\n");
    }

    /// All three aggregate lines are exactly six characters, which is what makes
    /// the layout line up on the button.
    #[test]
    fn aggregate_lines_are_exactly_six_chars() {
        let out = counts(Counts { run: 3, wait: 1, done: 5 }, &SummaryFormats::default());
        for line in out.lines() {
            assert_eq!(line.chars().count(), 6, "line {line:?}");
        }
        assert_fits(&out);
    }

    /// No containers renders as all zeroes rather than nothing.
    #[test]
    fn renders_zero_counts() {
        assert_eq!(
            counts(Counts::default(), &SummaryFormats::default()),
            "run: 0\nwait:0\ndone:0\n"
        );
    }

    /// Counts above nine are clamped so the line cannot overflow the button.
    #[test]
    fn clamps_counts_to_single_digit() {
        let out = counts(Counts { run: 12, wait: 10, done: 99 }, &SummaryFormats::default());
        assert_eq!(out, "run: 9\nwait:9\ndone:9\n");
        assert_fits(&out);
    }

    /// The `opencode-` prefix is stripped and the rest truncated to six.
    #[test]
    fn shortens_prefixed_names() {
        assert_eq!(short_name("opencode-web"), "web");
        assert_eq!(short_name("opencode-api"), "api");
        assert_eq!(short_name("opencode-project1"), "projec");
    }

    /// A bare `opencode` keeps its name and truncates, as agreed.
    #[test]
    fn shortens_bare_opencode_to_openco() {
        assert_eq!(short_name("opencode"), "openco");
    }

    /// A name that strips to nothing falls back to the original.
    #[test]
    fn falls_back_when_stripping_empties_the_name() {
        assert_eq!(short_name("opencode-"), "openco");
    }

    /// Truncation counts characters, not bytes, so multi-byte names cannot be
    /// cut mid-character.
    #[test]
    fn truncates_on_character_boundaries() {
        assert_eq!(short_name("opencode-\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"), "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}");
    }

    /// Durations format as zero-padded hours and minutes.
    #[test]
    fn formats_durations_as_hhmm() {
        assert_eq!(hhmm(0), "00:00");
        assert_eq!(hhmm(59), "00:00");
        assert_eq!(hhmm(60), "00:01");
        assert_eq!(hhmm(12 * 60), "00:12");
        assert_eq!(hhmm(3600), "01:00");
        assert_eq!(hhmm(3600 + 23 * 60), "01:23");
    }

    /// Long durations clamp instead of widening past six characters.
    #[test]
    fn clamps_long_durations() {
        assert_eq!(hhmm(100 * 3600), "99:59");
        assert_eq!(hhmm(i64::MAX / 2), "99:59");
        assert_eq!(hhmm(99 * 3600 + 59 * 60), "99:59");
    }

    /// A negative duration means the clock disagrees with itself; report unknown
    /// rather than a nonsense figure.
    #[test]
    fn negative_duration_is_unknown() {
        assert_eq!(hhmm(-1), UNKNOWN_TIME);
    }

    /// The single-instance view puts name, state and age on three lines.
    #[test]
    fn renders_instance_detail() {
        let now = 1790308945355;
        let out = instance(
            "opencode-web",
            Some(State::Run),
            Some(now - 12 * 60_000),
            now,
            &DetailFormats::default(),
        );
        assert_eq!(out, "web\nrun\n00:12\n");
        assert_fits(&out);
    }

    /// A container that exists but cannot be probed shows dashes for both state
    /// and age, keeping the misconfiguration visible.
    #[test]
    fn renders_unreachable_instance() {
        let now = 1790308945355;
        let out = instance("opencode-web", None, None, now, &DetailFormats::default());
        assert_eq!(out, "web\n----\n--:--\n");
        assert_fits(&out);
    }

    /// A known state with an unknown age still shows the state.
    #[test]
    fn renders_instance_with_unknown_age() {
        let now = 1790308945355;
        let out = instance(
            "opencode-api",
            Some(State::Wait),
            None,
            now,
            &DetailFormats::default(),
        );
        assert_eq!(out, "api\nwait\n--:--\n");
        assert_fits(&out);
    }

    /// Control characters, including the ESC that starts a terminal escape
    /// sequence, never survive into displayed text.
    #[test]
    fn neutralises_control_characters() {
        assert_eq!(printable("ok\u{1b}[2Jgone\r\n\t"), "ok?[2Jgone???");
        assert_eq!(printable("plain \u{e9}"), "plain \u{e9}");
        assert_eq!(short_name("opencode-\u{1b}[31m"), "?[31m");
    }

    /// An errored instance says so, capitalised, on its button.
    #[test]
    fn renders_error_instance() {
        let now = 1790308945355;
        let out = instance(
            "opencode-web",
            Some(State::Error),
            Some(now - 3 * 60_000),
            now,
            &DetailFormats::default(),
        );
        assert_eq!(out, "web\nError\n00:03\n");
        assert_fits(&out);
    }

    /// Every state word renders within the button width.
    #[test]
    fn all_states_fit_the_button() {
        let now = 1790308945355;
        for state in [State::Run, State::Wait, State::Done, State::Error] {
            assert_fits(&instance(
                "opencode-verylongname",
                Some(state),
                Some(now),
                now,
                &DetailFormats::default(),
            ));
        }
    }

    /// With podman unavailable, the summary keeps its labels and shows a dash
    /// in place of every number, in the same fixed width.
    #[test]
    fn renders_unknown_counts() {
        let out = counts_unknown(&SummaryFormats::default());
        assert_eq!(out, "run: -\nwait:-\ndone:-\n");
        assert_fits(&out);
        for line in out.lines() {
            assert_eq!(line.chars().count(), WIDTH, "{line:?}");
        }
    }

    /// With podman unavailable, the detail view is dashes, question marks and
    /// an unknown time.
    #[test]
    fn renders_unknown_instance() {
        let out = instance_unknown(&DetailFormats::default());
        assert_eq!(out, "------\n????\n--:--\n");
        assert_fits(&out);
    }

    /// Values must be made only of `#[...]` tags: no bare text, no unclosed
    /// tag, no control characters, but an empty value is fine.
    #[test]
    fn validates_format_values() {
        assert!(is_valid_format_value(""));
        assert!(is_valid_format_value("#[fg=red]"));
        assert!(is_valid_format_value("#[fg=red,bold]"));
        assert!(is_valid_format_value("#[fg=red]#[bold]"));
        assert!(!is_valid_format_value("plain text"));
        assert!(!is_valid_format_value("#[fg=red] extra"));
        assert!(!is_valid_format_value("#[fg=red"));
        assert!(!is_valid_format_value("#[fg=red]\n"));
        assert!(!is_valid_format_value("#[fg=\u{1b}red]"));
    }

    /// With no `--fmt` given, the summary is byte-identical to the plain,
    /// unformatted output, whatever the counts.
    #[test]
    fn summary_stays_plain_without_fmt() {
        let out = counts(Counts { run: 3, wait: 0, done: 5 }, &SummaryFormats::default());
        assert_eq!(out, "run: 3\nwait:0\ndone:5\n");
    }

    /// Each summary line picks its own zero/non-zero format once any `--fmt`
    /// key is set; a line with no key configured falls back to `#[default]`.
    #[test]
    fn summary_applies_zero_and_nonzero_formats() {
        let fmt = SummaryFormats {
            wait0: Some("#[fg=gray]".to_string()),
            wait1: Some("#[fg=red,bold]".to_string()),
            ..Default::default()
        };
        let idle = counts(Counts { run: 3, wait: 0, done: 5 }, &fmt);
        assert_eq!(idle, "#[default]run: 3\n#[fg=gray]wait:0\n#[default]done:5\n");
        assert_fits(&idle);

        let waiting = counts(Counts { run: 3, wait: 2, done: 5 }, &fmt);
        assert_eq!(
            waiting,
            "#[default]run: 3\n#[fg=red,bold]wait:2\n#[default]done:5\n"
        );
        assert_fits(&waiting);
    }

    /// A shared key list (`wait0,run0,done0=...`) is expressed by setting the
    /// same tag on multiple fields; every configured line uses it.
    #[test]
    fn summary_shared_zero_format_applies_to_every_line() {
        let fmt = SummaryFormats {
            run0: Some("#[fg=gray]".to_string()),
            wait0: Some("#[fg=gray]".to_string()),
            done0: Some("#[fg=gray]".to_string()),
            ..Default::default()
        };
        let out = counts(Counts::default(), &fmt);
        assert_eq!(
            out,
            "#[fg=gray]run: 0\n#[fg=gray]wait:0\n#[fg=gray]done:0\n"
        );
    }

    /// Without an explicit `unknown` format, the busy-podman placeholder falls
    /// back to each line's own zero format, then to `#[default]`.
    #[test]
    fn counts_unknown_falls_back_to_zero_format_then_default() {
        let fmt = SummaryFormats {
            wait0: Some("#[fg=gray]".to_string()),
            ..Default::default()
        };
        let out = counts_unknown(&fmt);
        assert_eq!(out, "#[default]run: -\n#[fg=gray]wait:-\n#[default]done:-\n");
        assert_fits(&out);
    }

    /// An explicit `unknown` format wins over the per-line zero fallback.
    #[test]
    fn counts_unknown_prefers_explicit_unknown_format() {
        let fmt = SummaryFormats {
            wait0: Some("#[fg=gray]".to_string()),
            unknown: Some("#[fg=yellow]".to_string()),
            ..Default::default()
        };
        let out = counts_unknown(&fmt);
        assert_eq!(
            out,
            "#[fg=yellow]run: -\n#[fg=yellow]wait:-\n#[fg=yellow]done:-\n"
        );
    }

    /// With no `--details-fmt` given, the detail view is byte-identical to the
    /// plain, unformatted output.
    #[test]
    fn instance_stays_plain_without_details_fmt() {
        let now = 1790308945355;
        let out = instance(
            "opencode-web",
            Some(State::Run),
            Some(now - 60_000),
            now,
            &DetailFormats::default(),
        );
        assert_eq!(out, "web\nrun\n00:01\n");
    }

    /// Once any `--details-fmt` key is set, the name and time lines always get
    /// `#[default]`, and the state line gets its own configured format.
    #[test]
    fn instance_formats_only_the_state_line() {
        let now = 1790308945355;
        let fmt = DetailFormats {
            wait: Some("#[fg=red,bold]".to_string()),
            ..Default::default()
        };
        let out = instance("opencode-web", Some(State::Wait), Some(now - 60_000), now, &fmt);
        assert_eq!(out, "#[default]web\n#[fg=red,bold]wait\n#[default]00:01\n");
        assert_fits(&out);

        // A state with no configured format falls back to `#[default]`.
        let out = instance("opencode-web", Some(State::Run), Some(now - 60_000), now, &fmt);
        assert_eq!(out, "#[default]web\n#[default]run\n#[default]00:01\n");
    }

    /// The `error` key formats only the `Error` state line.
    #[test]
    fn instance_formats_error_state() {
        let now = 1790308945355;
        let fmt = DetailFormats {
            error: Some("#[fg=red]".to_string()),
            ..Default::default()
        };
        let out = instance("opencode-web", Some(State::Error), Some(now - 60_000), now, &fmt);
        assert_eq!(out, "#[default]web\n#[fg=red]Error\n#[default]00:01\n");
    }

    /// The `unknown` key formats an unprobed container's `----` state line;
    /// without it, that line falls back to `#[default]`.
    #[test]
    fn instance_formats_unknown_state() {
        let now = 1790308945355;
        let fmt = DetailFormats {
            unknown: Some("#[fg=gray]".to_string()),
            ..Default::default()
        };
        let out = instance("opencode-web", None, None, now, &fmt);
        assert_eq!(out, "#[default]web\n#[fg=gray]----\n#[default]--:--\n");

        let out = instance("opencode-web", None, None, now, &DetailFormats {
            wait: Some("#[fg=red]".to_string()),
            ..Default::default()
        });
        assert_eq!(out, "#[default]web\n#[default]----\n#[default]--:--\n");
    }

    /// Once any `--details-fmt` key is set, the busy-podman placeholder's state
    /// line uses `unknown` if set, else `#[default]`; name and time are always
    /// `#[default]`.
    #[test]
    fn instance_unknown_uses_unknown_format() {
        let fmt = DetailFormats {
            unknown: Some("#[fg=yellow]".to_string()),
            ..Default::default()
        };
        let out = instance_unknown(&fmt);
        assert_eq!(out, "#[default]------\n#[fg=yellow]????\n#[default]--:--\n");
        assert_fits(&out);

        let out = instance_unknown(&DetailFormats {
            wait: Some("#[fg=red]".to_string()),
            ..Default::default()
        });
        assert_eq!(out, "#[default]------\n#[default]????\n#[default]--:--\n");
    }
}
