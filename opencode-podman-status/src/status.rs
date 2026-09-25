//! Turning opencode's API responses into a single state per container.
//!
//! An instance is classified from three endpoints:
//!
//! * `GET /session/status` - a map of session ID to status. Verified against
//!   opencode 1.18.32: **idle sessions are omitted entirely**, so an empty `{}`
//!   means everything is idle. Explicit `idle` entries are still tolerated in
//!   case that changes.
//! * `GET /question` - pending questions the agent has asked the user.
//! * `GET /permission` - pending permission prompts.
//!
//! Subagent sessions are deliberately *not* filtered out. If a subagent is
//! working then the container is working, and a question needs answering
//! whichever session raised it - so `GET /session` is not needed at all.

use serde::Deserialize;

use crate::ident;

/// What a single opencode instance is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// At least one session is `busy` or `retry`.
    Run,
    /// At least one question or permission is waiting on the user.
    Wait,
    /// Reachable, with nothing running and nothing waiting.
    Done,
}

impl State {
    /// The six-character-safe word shown on a button for this state.
    pub fn word(self) -> &'static str {
        match self {
            State::Run => "run",
            State::Wait => "wait",
            State::Done => "done",
        }
    }
}

/// One entry of `GET /session/status`.
///
/// Only the discriminant matters here; `retry` also carries `attempt`/`next`,
/// which we ignore because a retrying session is still working.
#[derive(Debug, Deserialize)]
pub struct SessionStatus {
    #[serde(rename = "type")]
    pub kind: String,
}

/// A pending question or permission. Only the ID is needed: it encodes when the
/// request was created (see [`crate::ident`]).
#[derive(Debug, Deserialize)]
pub struct PendingRequest {
    pub id: String,
}

/// Everything one probe observed about a single instance.
#[derive(Debug, Default, Deserialize)]
pub struct Observation {
    /// Session ID to status, as returned by `GET /session/status`.
    #[serde(default)]
    pub statuses: std::collections::HashMap<String, SessionStatus>,
    /// Pending questions, from `GET /question`.
    #[serde(default)]
    pub questions: Vec<PendingRequest>,
    /// Pending permissions, from `GET /permission`.
    #[serde(default)]
    pub permissions: Vec<PendingRequest>,
}

/// True if a `/session/status` discriminant means the session is working.
///
/// `retry` counts as working: the turn has not finished, it is being reattempted.
fn is_working(kind: &str) -> bool {
    matches!(kind, "busy" | "retry")
}

impl Observation {
    /// Classifies the instance.
    ///
    /// Precedence is **wait > run > done**: something needing a human answer is
    /// always the more useful thing to surface, even if another session in the
    /// same container happens to be busy.
    pub fn state(&self) -> State {
        if !self.questions.is_empty() || !self.permissions.is_empty() {
            State::Wait
        } else if self.statuses.values().any(|s| is_working(&s.kind)) {
            State::Run
        } else {
            State::Done
        }
    }

    /// When the instance entered its current state, in Unix milliseconds.
    ///
    /// Only [`State::Wait`] can be answered from these three endpoints alone:
    /// the oldest pending request's ID encodes its creation time, which is
    /// exactly "how long has this been waiting for you". `Run` and `Done` need
    /// an extra request, so the probe supplies them separately.
    ///
    /// Returns `None` when the time cannot be established, so the caller shows
    /// an unknown duration rather than a wrong one.
    pub fn wait_since_ms(&self, now_ms: i64) -> Option<i64> {
        let ids = self
            .questions
            .iter()
            .chain(self.permissions.iter())
            .map(|r| r.id.as_str());
        ident::oldest_timestamp_ms(ids, now_ms)
    }
}

/// How many instances are in each state, for the aggregate output.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub run: usize,
    pub wait: usize,
    pub done: usize,
}

impl Counts {
    /// Tallies one state per instance.
    ///
    /// `None` means the container exists but could not be probed - usually
    /// opencode was started without `--port`, or is still booting. Such
    /// instances are counted in no bucket, so the three numbers can legitimately
    /// sum to less than the number of containers.
    pub fn tally<I: IntoIterator<Item = Option<State>>>(states: I) -> Self {
        let mut counts = Counts::default();
        for state in states {
            match state {
                Some(State::Run) => counts.run += 1,
                Some(State::Wait) => counts.wait += 1,
                Some(State::Done) => counts.done += 1,
                None => {}
            }
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses a probe observation from JSON shaped like the real endpoints.
    fn observe(statuses: &str, questions: &str, permissions: &str) -> Observation {
        let json = format!(
            r#"{{"statuses":{statuses},"questions":{questions},"permissions":{permissions}}}"#
        );
        serde_json::from_str(&json).expect("fixture should parse")
    }

    /// The real empty case captured from opencode 1.18.32: two idle sessions
    /// exist, yet `/session/status` is `{}` and nothing is pending.
    #[test]
    fn empty_everything_is_done() {
        let o = observe("{}", "[]", "[]");
        assert_eq!(o.state(), State::Done);
    }

    /// A busy session means the instance is running.
    #[test]
    fn busy_session_is_run() {
        let o = observe(r#"{"ses_a":{"type":"busy"}}"#, "[]", "[]");
        assert_eq!(o.state(), State::Run);
    }

    /// A retrying session is still working, so it also counts as running.
    #[test]
    fn retry_session_is_run() {
        let o = observe(
            r#"{"ses_a":{"type":"retry","attempt":2,"message":"boom"}}"#,
            "[]",
            "[]",
        );
        assert_eq!(o.state(), State::Run);
    }

    /// An explicit idle entry is not working, even though real servers omit it.
    #[test]
    fn explicit_idle_session_is_done() {
        let o = observe(r#"{"ses_a":{"type":"idle"}}"#, "[]", "[]");
        assert_eq!(o.state(), State::Done);
    }

    /// An unrecognised future status is treated as not working, so a new
    /// discriminant cannot silently inflate the "run" count.
    #[test]
    fn unknown_status_is_not_working() {
        let o = observe(r#"{"ses_a":{"type":"hibernating"}}"#, "[]", "[]");
        assert_eq!(o.state(), State::Done);
    }

    /// A pending question puts the instance in the waiting state.
    #[test]
    fn pending_question_is_wait() {
        let o = observe("{}", r#"[{"id":"que_000000000001x"}]"#, "[]");
        assert_eq!(o.state(), State::Wait);
    }

    /// A pending permission does too.
    #[test]
    fn pending_permission_is_wait() {
        let o = observe("{}", "[]", r#"[{"id":"per_000000000001x"}]"#);
        assert_eq!(o.state(), State::Wait);
    }

    /// Waiting outranks running: a container with both should report wait.
    #[test]
    fn wait_takes_precedence_over_run() {
        let o = observe(
            r#"{"ses_a":{"type":"busy"}}"#,
            r#"[{"id":"que_000000000001x"}]"#,
            "[]",
        );
        assert_eq!(o.state(), State::Wait);
    }

    /// The state words are the ones the renderer and README promise.
    #[test]
    fn state_words_are_stable() {
        assert_eq!(State::Run.word(), "run");
        assert_eq!(State::Wait.word(), "wait");
        assert_eq!(State::Done.word(), "done");
    }

    /// The oldest pending request across both lists determines the wait age.
    #[test]
    fn wait_since_uses_oldest_across_both_lists() {
        let now = 1790308945355;
        // Ascending encoding, as questions and permissions use, truncated to the
        // 48 payload bits the real generator keeps.
        let mk = |ms: i64| {
            let value = ((ms as u64) * 4096 + 1) & ((1u64 << 48) - 1);
            format!("{value:012x}")
        };
        let json_q = format!(r#"[{{"id":"que_{}RAND"}}]"#, mk(now - 10_000));
        let json_p = format!(r#"[{{"id":"per_{}RAND"}}]"#, mk(now - 45_000));
        let o = observe("{}", &json_q, &json_p);
        assert_eq!(o.wait_since_ms(now), Some(now - 45_000));
    }

    /// With nothing pending there is no wait time to report.
    #[test]
    fn wait_since_is_none_when_nothing_pending() {
        let o = observe("{}", "[]", "[]");
        assert_eq!(o.wait_since_ms(1790308945355), None);
    }

    /// An undecodable request ID yields no time rather than a wrong one.
    #[test]
    fn wait_since_is_none_for_undecodable_id() {
        let o = observe("{}", r#"[{"id":"que_notvalidhex"}]"#, "[]");
        assert_eq!(o.wait_since_ms(1790308945355), None);
    }

    /// Tallying counts one instance per bucket.
    #[test]
    fn tally_counts_each_state() {
        let counts = Counts::tally([
            Some(State::Run),
            Some(State::Run),
            Some(State::Wait),
            Some(State::Done),
            Some(State::Done),
            Some(State::Done),
        ]);
        assert_eq!(counts, Counts { run: 2, wait: 1, done: 3 });
    }

    /// Unreachable instances are counted nowhere, so the buckets may under-sum.
    #[test]
    fn tally_ignores_unreachable() {
        let counts = Counts::tally([Some(State::Run), None, None, Some(State::Done)]);
        assert_eq!(counts, Counts { run: 1, wait: 0, done: 1 });
    }

    /// No containers at all is a legitimate all-zero tally.
    #[test]
    fn tally_of_nothing_is_zero() {
        assert_eq!(Counts::tally([]), Counts { run: 0, wait: 0, done: 0 });
    }
}
