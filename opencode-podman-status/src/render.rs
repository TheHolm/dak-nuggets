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

/// Renders the aggregate three-line summary.
///
/// Counts are clamped to [`MAX_COUNT`]. Instances that could not be probed are
/// already excluded by [`Counts::tally`], so these three numbers may sum to less
/// than the number of containers present.
pub fn counts(counts: Counts) -> String {
    let clamp = |n: usize| n.min(MAX_COUNT);
    format!(
        "run: {}\nwait:{}\ndone:{}\n",
        clamp(counts.run),
        clamp(counts.wait),
        clamp(counts.done)
    )
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
/// `state` is `None` when the container exists but its API could not be reached,
/// typically because opencode started without `--port` or is still booting; that
/// shows as `----`. `since_ms` is when the instance entered its current state, and
/// `None` shows as `--:--`.
///
/// The case of a slot that does not exist at all is *not* handled here: the
/// caller prints nothing, so unused DAK buttons stay blank.
pub fn instance(name: &str, state: Option<State>, since_ms: Option<i64>, now_ms: i64) -> String {
    let state_line = match state {
        Some(s) => s.word(),
        None => UNKNOWN_STATE,
    };
    let time_line = match (state, since_ms) {
        (Some(_), Some(since)) => hhmm((now_ms - since) / 1000),
        _ => UNKNOWN_TIME.to_string(),
    };
    format!("{}\n{}\n{}\n", short_name(name), state_line, time_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line of every rendering fits a DAK button.
    fn assert_fits(rendered: &str) {
        let lines: Vec<&str> = rendered.lines().collect();
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
        let out = counts(Counts { run: 3, wait: 1, done: 5 });
        assert_eq!(out, "run: 3\nwait:1\ndone:5\n");
    }

    /// All three aggregate lines are exactly six characters, which is what makes
    /// the layout line up on the button.
    #[test]
    fn aggregate_lines_are_exactly_six_chars() {
        let out = counts(Counts { run: 3, wait: 1, done: 5 });
        for line in out.lines() {
            assert_eq!(line.chars().count(), 6, "line {line:?}");
        }
        assert_fits(&out);
    }

    /// No containers renders as all zeroes rather than nothing.
    #[test]
    fn renders_zero_counts() {
        assert_eq!(counts(Counts::default()), "run: 0\nwait:0\ndone:0\n");
    }

    /// Counts above nine are clamped so the line cannot overflow the button.
    #[test]
    fn clamps_counts_to_single_digit() {
        let out = counts(Counts { run: 12, wait: 10, done: 99 });
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
        let out = instance("opencode-web", Some(State::Run), Some(now - 12 * 60_000), now);
        assert_eq!(out, "web\nrun\n00:12\n");
        assert_fits(&out);
    }

    /// A container that exists but cannot be probed shows dashes for both state
    /// and age, keeping the misconfiguration visible.
    #[test]
    fn renders_unreachable_instance() {
        let now = 1790308945355;
        let out = instance("opencode-web", None, None, now);
        assert_eq!(out, "web\n----\n--:--\n");
        assert_fits(&out);
    }

    /// A known state with an unknown age still shows the state.
    #[test]
    fn renders_instance_with_unknown_age() {
        let now = 1790308945355;
        let out = instance("opencode-api", Some(State::Wait), None, now);
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

    /// Every state word renders within the button width.
    #[test]
    fn all_states_fit_the_button() {
        let now = 1790308945355;
        for state in [State::Run, State::Wait, State::Done] {
            assert_fits(&instance("opencode-verylongname", Some(state), Some(now), now));
        }
    }
}
