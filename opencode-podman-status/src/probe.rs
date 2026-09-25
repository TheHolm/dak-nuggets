//! The probe: what runs *inside* a container's namespaces.
//!
//! The parent process forks, moves the child into the target container's user
//! and network namespaces, then re-executes this program with `__probe`. At that
//! point `127.0.0.1` is the container's loopback, so opencode's server is
//! reachable exactly as it would be from inside the container.
//!
//! The child finds the port opencode itself is listening on - by socket
//! ownership, never by trying ports, see [`crate::sockets`] - queries opencode,
//! and writes a single JSON line to stdout for the parent to collect.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::http;
use crate::sockets::{self, Discovery};
use crate::status::{Observation, State};

/// Most busy sessions to interrogate for a turn start time.
///
/// One is the overwhelmingly common case; the cap stops a container with many
/// concurrent subagents from turning one probe into dozens of requests.
const MAX_BUSY_SESSIONS_SAMPLED: usize = 4;

/// What the child reports back to the parent.
#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    /// The port opencode was found on, when it was found at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The instance's state, or `None` if it could not be determined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// When the instance entered that state, in Unix milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<i64>,
    /// Why the probe failed, for `--list`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Report {
    /// A failed probe, carrying the reason for `--list` to display.
    pub fn failed(reason: impl Into<String>) -> Self {
        Report { port: None, state: None, since_ms: None, reason: Some(reason.into()) }
    }

    /// Parses the state word back into a [`State`].
    ///
    /// Unknown words are treated as "not determined" rather than guessed at.
    pub fn parsed_state(&self) -> Option<State> {
        match self.state.as_deref() {
            Some("run") => Some(State::Run),
            Some("wait") => Some(State::Wait),
            Some("done") => Some(State::Done),
            _ => None,
        }
    }
}

/// A session as returned by `GET /session`. Only the timing matters here.
#[derive(Debug, Deserialize)]
struct SessionRecord {
    #[serde(default)]
    time: SessionTime,
}

/// The `time` object on a session.
#[derive(Debug, Default, Deserialize)]
struct SessionTime {
    #[serde(default)]
    created: Option<i64>,
    #[serde(default)]
    updated: Option<i64>,
}

/// A message list entry.
///
/// opencode has returned both a bare message and a `{info, parts}` wrapper
/// across versions, so both shapes are accepted and whichever carries a time is
/// used. Guessing wrong would mean a wrong duration, so an absent time is
/// reported as unknown instead.
#[derive(Debug, Deserialize)]
struct MessageEntry {
    #[serde(default)]
    info: Option<MessageInfo>,
    #[serde(default)]
    time: Option<MessageTime>,
}

/// The nested message object of a `{info, parts}` entry.
#[derive(Debug, Deserialize)]
struct MessageInfo {
    #[serde(default)]
    time: Option<MessageTime>,
}

/// Timing of a single message.
#[derive(Debug, Deserialize)]
struct MessageTime {
    #[serde(default)]
    created: Option<i64>,
}

impl MessageEntry {
    /// The creation time of this message, from whichever shape was returned.
    fn created_ms(&self) -> Option<i64> {
        self.info
            .as_ref()
            .and_then(|i| i.time.as_ref())
            .and_then(|t| t.created)
            .or_else(|| self.time.as_ref().and_then(|t| t.created))
    }
}

/// Runs the probe against `port`, or finds opencode's own port if `port` is `None`.
///
/// Always returns a [`Report`]: failures are reported, not raised, because the
/// parent needs to render something for every container.
///
/// Without an explicit port, only ports held by an opencode process are ever
/// connected to. Other servers in the container are never contacted - an earlier
/// version sent each of them a health check and they logged errors about it.
pub fn run(port: Option<u16>, timeout: Duration, now_ms: i64) -> Report {
    let candidates = match port {
        Some(p) => vec![p],
        None => match sockets::discover() {
            Discovery::Found(ports) => ports,
            other => return Report::failed(other.reason()),
        },
    };

    // Normally exactly one. Several means opencode holds more than one listener,
    // all of them its own, so asking each is safe.
    let mut last_error = None;
    for candidate in candidates {
        match probe_one(candidate, timeout, now_ms) {
            Ok(report) => return report,
            Err(e) => last_error = Some(e),
        }
    }
    Report::failed(last_error.unwrap_or_else(|| "opencode did not answer".to_string()))
}

/// Queries one candidate port, confirming it is opencode before trusting it.
fn probe_one(port: u16, timeout: Duration, now_ms: i64) -> Result<Report, String> {
    // A cheap liveness check before the real requests. The port is already known
    // to be opencode's, so this only guards against it still starting up.
    http::get(port, "/global/health", timeout).map_err(|e| format!("port {port}: {e}"))?;

    let statuses: HashMap<String, crate::status::SessionStatus> =
        fetch_json(port, "/session/status", timeout)?;
    let questions: Vec<crate::status::PendingRequest> = fetch_json(port, "/question", timeout)?;
    let permissions: Vec<crate::status::PendingRequest> =
        fetch_json(port, "/permission", timeout)?;

    let observation = Observation { statuses, questions, permissions };
    let state = observation.state();

    let since_ms = match state {
        // Free: the oldest pending request's ID encodes when it was created.
        State::Wait => observation.wait_since_ms(now_ms),
        State::Run => busy_since_ms(port, &observation, timeout, now_ms),
        State::Done => idle_since_ms(port, timeout, now_ms),
    };

    Ok(Report {
        port: Some(port),
        state: Some(state.word().to_string()),
        since_ms,
        reason: None,
    })
}

/// Fetches and deserialises one endpoint.
fn fetch_json<T: serde::de::DeserializeOwned>(
    port: u16,
    path: &str,
    timeout: Duration,
) -> Result<T, String> {
    let body = http::get(port, path, timeout).map_err(|e| format!("GET {path}: {e}"))?;
    serde_json::from_slice(&body).map_err(|e| format!("GET {path}: bad JSON: {e}"))
}

/// When the earliest still-running turn started.
///
/// Asks each busy session for its most recent message and takes the oldest such
/// start, so the figure means "how long has this container been busy".
fn busy_since_ms(
    port: u16,
    observation: &Observation,
    timeout: Duration,
    now_ms: i64,
) -> Option<i64> {
    let mut busy: Vec<&String> = observation
        .statuses
        .iter()
        .filter(|(_, s)| matches!(s.kind.as_str(), "busy" | "retry"))
        .map(|(id, _)| id)
        .collect();
    // Deterministic sampling order, so repeated runs agree when capped.
    busy.sort();

    busy.into_iter()
        .take(MAX_BUSY_SESSIONS_SAMPLED)
        .filter_map(|id| latest_message_ms(port, id, timeout, now_ms))
        .min()
}

/// Creation time of a session's most recent message.
///
/// `limit=1` returns the *newest* message: verified against opencode 1.18.32 by
/// creating a message in an existing session and seeing only that one come back,
/// far newer than the session's own `time.created`. The result is sanity-checked
/// against the clock regardless, so a future change in ordering would show an
/// unknown duration rather than a fabricated one.
fn latest_message_ms(port: u16, session: &str, timeout: Duration, now_ms: i64) -> Option<i64> {
    let path = format!("/session/{session}/message?limit=1");
    let entries: Vec<MessageEntry> = fetch_json(port, &path, timeout).ok()?;
    let created = entries.first()?.created_ms()?;
    plausible_past(created, now_ms)
}

/// When the instance last did anything, for an idle instance.
fn idle_since_ms(port: u16, timeout: Duration, now_ms: i64) -> Option<i64> {
    let sessions: Vec<SessionRecord> = fetch_json(port, "/session", timeout).ok()?;
    sessions
        .iter()
        .filter_map(|s| s.time.updated.or(s.time.created))
        .filter_map(|ms| plausible_past(ms, now_ms))
        .max()
}

/// Accepts a timestamp only if it is a sane point in the recent past.
///
/// Guards against both clock skew and a misread field turning into a wildly
/// wrong duration on the button.
fn plausible_past(ms: i64, now_ms: i64) -> Option<i64> {
    const MAX_AGE_MS: i64 = 180 * 24 * 60 * 60 * 1000;
    const MAX_SKEW_MS: i64 = 60 * 1000;
    if ms <= now_ms + MAX_SKEW_MS && ms >= now_ms - MAX_AGE_MS {
        Some(ms)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real `{info, parts}` entry captured from opencode 1.18.32 for
    /// `GET /session/{id}/message?limit=1`, trimmed of the parts payload.
    #[test]
    fn reads_time_from_real_captured_message() {
        let body = r#"[{"info":{"parentID":"msg_0d6c75e870015YKD4THNNa5tYs","mode":"build",
          "agent":"build","cost":0,"path":{"cwd":"/tmp/ocfix","root":"/"},
          "time":{"created":1790309785251},"role":"assistant",
          "tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},
          "modelID":"example-model","providerID":"example-provider",
          "id":"msg_0d6c75ea3001xwhRotcmNTvdqj",
          "sessionID":"ses_f29457234ffejFzn9L4UNG7Hz7"},"parts":[]}]"#;
        let entries: Vec<MessageEntry> = serde_json::from_str(body).expect("should parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].created_ms(), Some(1790309785251));
    }

    /// A bare-message entry exposes its creation time.
    #[test]
    fn reads_time_from_bare_message_shape() {
        let entry: MessageEntry =
            serde_json::from_str(r#"{"time":{"created":1790308726787}}"#).unwrap();
        assert_eq!(entry.created_ms(), Some(1790308726787));
    }

    /// So does the `{info, parts}` wrapper shape.
    #[test]
    fn reads_time_from_wrapped_message_shape() {
        let entry: MessageEntry =
            serde_json::from_str(r#"{"info":{"time":{"created":1790308726787}},"parts":[]}"#)
                .unwrap();
        assert_eq!(entry.created_ms(), Some(1790308726787));
    }

    /// A message with no usable time reports none rather than defaulting.
    #[test]
    fn reports_no_time_for_timeless_message() {
        let entry: MessageEntry = serde_json::from_str(r#"{"info":{}}"#).unwrap();
        assert_eq!(entry.created_ms(), None);
        let entry: MessageEntry = serde_json::from_str("{}").unwrap();
        assert_eq!(entry.created_ms(), None);
    }

    /// The real `/session` fixture captured from opencode 1.18.32 parses, and
    /// its newest `time.updated` is the one that would be reported.
    #[test]
    fn parses_real_session_list() {
        let body = r#"[
          {"id":"ses_f29457234ffejFzn9L4UNG7Hz7","time":{"created":1790308945355,"updated":1790308945355}},
          {"id":"ses_f2948c7fdffe1r7f03mBfBRLsy","time":{"created":1790308726787,"updated":1790308726787}}
        ]"#;
        let sessions: Vec<SessionRecord> = serde_json::from_str(body).unwrap();
        let newest = sessions.iter().filter_map(|s| s.time.updated).max();
        assert_eq!(newest, Some(1790308945355));
    }

    /// Timestamps outside a believable window are rejected, so a misread field
    /// cannot become a nonsense age on the button.
    #[test]
    fn rejects_implausible_timestamps() {
        let now = 1790308945355;
        assert_eq!(plausible_past(now - 1_000, now), Some(now - 1_000));
        assert_eq!(plausible_past(now, now), Some(now));
        assert_eq!(plausible_past(0, now), None);
        assert_eq!(plausible_past(now + 3_600_000, now), None);
        // Small skew is tolerated: IDs and clocks are not perfectly ordered.
        assert_eq!(plausible_past(now + 1_000, now), Some(now + 1_000));
    }

    /// A failed report round-trips through JSON carrying its reason.
    #[test]
    fn failed_report_round_trips() {
        let report = Report::failed("no listening socket");
        let json = serde_json::to_string(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reason.as_deref(), Some("no listening socket"));
        assert_eq!(back.parsed_state(), None);
        assert_eq!(back.port, None);
    }

    /// A successful report round-trips, and its state word parses back.
    #[test]
    fn successful_report_round_trips() {
        for (word, want) in [
            ("run", State::Run),
            ("wait", State::Wait),
            ("done", State::Done),
        ] {
            let report = Report {
                port: Some(4096),
                state: Some(word.to_string()),
                since_ms: Some(1790308726787),
                reason: None,
            };
            let json = serde_json::to_string(&report).unwrap();
            let back: Report = serde_json::from_str(&json).unwrap();
            assert_eq!(back.parsed_state(), Some(want));
            assert_eq!(back.port, Some(4096));
            assert_eq!(back.since_ms, Some(1790308726787));
        }
    }

    /// An unrecognised state word is treated as undetermined, so a future
    /// opencode state cannot be silently miscounted.
    #[test]
    fn unknown_state_word_does_not_parse() {
        let report = Report {
            port: Some(4096),
            state: Some("hibernating".to_string()),
            since_ms: None,
            reason: None,
        };
        assert_eq!(report.parsed_state(), None);
    }
}
