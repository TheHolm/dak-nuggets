//! The probe: what runs *inside* a container's namespaces.
//!
//! The parent process forks, moves the child into the target container's user
//! and network namespaces, then re-executes this program with `__probe`. At that
//! point `127.0.0.1` is the container's loopback, so opencode's server is
//! reachable exactly as it would be from inside the container.
//!
//! The child finds the listening port from `/proc/net/tcp` (its own, which is now
//! the container's), queries opencode, and writes a single JSON line to stdout for
//! the parent to collect.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::http;
use crate::status::{Observation, State};

/// `/proc/net/tcp` state value for a listening socket.
const TCP_LISTEN: &str = "0A";

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

/// Extracts ports of listening sockets bound to loopback or the wildcard address.
///
/// Takes the contents of a `/proc/net/tcp`-format table so it can be tested
/// against captured fixtures. Both the loopback and wildcard cases are accepted:
/// opencode binds `127.0.0.1` by default, but someone may have passed
/// `--hostname 0.0.0.0`, and from inside the namespace `127.0.0.1` reaches both.
///
/// Addresses are little-endian hex in this file, so `127.0.0.1` appears as
/// `0100007F`.
pub fn listening_ports(table: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    for line in table.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let _index = fields.next();
        let local = match fields.next() {
            Some(l) => l,
            None => continue,
        };
        let _remote = fields.next();
        if fields.next() != Some(TCP_LISTEN) {
            continue;
        }

        let (addr, port) = match local.split_once(':') {
            Some(parts) => parts,
            None => continue,
        };
        if !is_local_address(addr) {
            continue;
        }
        if let Ok(port) = u16::from_str_radix(port, 16) {
            if port != 0 && !ports.contains(&port) {
                ports.push(port);
            }
        }
    }
    ports
}

/// True if a `/proc/net/tcp` local address is loopback or the wildcard.
///
/// Handles both the 8-character IPv4 form and the 32-character IPv6 form.
fn is_local_address(addr: &str) -> bool {
    // Wildcard: every bit zero, in either address family.
    if !addr.is_empty() && addr.bytes().all(|b| b == b'0') {
        return true;
    }
    match addr.len() {
        // IPv4, little-endian: 127.0.0.1.
        8 => addr.eq_ignore_ascii_case("0100007F"),
        // IPv6 ::1, as four little-endian words.
        32 => addr.eq_ignore_ascii_case("00000000000000000000000001000000"),
        _ => false,
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

/// Runs the probe against `port`, or discovers the port if `port` is `None`.
///
/// Always returns a [`Report`]: failures are reported, not raised, because the
/// parent needs to render something for every container.
pub fn run(port: Option<u16>, timeout: Duration, now_ms: i64) -> Report {
    let candidates = match port {
        Some(p) => vec![p],
        None => match std::fs::read_to_string("/proc/net/tcp") {
            Ok(table) => listening_ports(&table),
            Err(e) => return Report::failed(format!("cannot read /proc/net/tcp: {e}")),
        },
    };

    if candidates.is_empty() {
        return Report::failed("no listening socket (is opencode running with --port?)");
    }

    // With several candidates, ask each whether it is actually opencode. A
    // container may have unrelated services on loopback.
    let mut last_error = None;
    for candidate in candidates {
        match probe_one(candidate, timeout, now_ms) {
            Ok(report) => return report,
            Err(e) => last_error = Some(e),
        }
    }
    Report::failed(
        last_error.unwrap_or_else(|| "no opencode server on any listening port".to_string()),
    )
}

/// Queries one candidate port, confirming it is opencode before trusting it.
fn probe_one(port: u16, timeout: Duration, now_ms: i64) -> Result<Report, String> {
    // /global/health is the cheapest way to establish this is opencode and not
    // some other service that happens to share the namespace.
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

    /// A real `/proc/net/tcp` captured while opencode listened on port 4098
    /// (`0x1002`), alongside an established outbound connection. The addresses of
    /// that connection have been replaced with RFC 5737 documentation addresses;
    /// nothing in the parser depends on their value.
    const REAL_TABLE: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1002 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 257009 1 0000000078043ef6 20 4 26 4 2
   1: 010002C0:E5DE 026433C6:01BB 01 00000000:00000000 00:00000000 00000000     0        0 227599 1 00000000efef65ae 20 4 0 4 2
";

    /// The real table yields exactly the listening port, ignoring the
    /// established connection.
    #[test]
    fn finds_listening_port_in_real_table() {
        assert_eq!(listening_ports(REAL_TABLE), vec![0x1002]);
    }

    /// A table with no listening socket - the state this container was in before
    /// opencode was given `--port` - yields nothing.
    #[test]
    fn finds_nothing_when_only_established_connections() {
        let table = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 010002C0:E5DE 026433C6:01BB 01 00000000:00000000 00:00000000 00000000     0        0 227599 1 0 20 4 26 4 2
";
        assert!(listening_ports(table).is_empty());
    }

    /// A wildcard bind, as `--hostname 0.0.0.0` would produce, is accepted.
    #[test]
    fn accepts_wildcard_bind() {
        let table = "header\n   0: 00000000:1000 00000000:0000 0A 0 0 0 0 0 0 0 0\n";
        assert_eq!(listening_ports(table), vec![4096]);
    }

    /// An IPv6 loopback or wildcard listener is accepted too.
    #[test]
    fn accepts_ipv6_loopback_and_wildcard() {
        let v6_loopback =
            "header\n   0: 00000000000000000000000001000000:1000 00000000000000000000000000000000:0000 0A 0 0 0 0 0 0 0 0\n";
        assert_eq!(listening_ports(v6_loopback), vec![4096]);
        let v6_wildcard =
            "header\n   0: 00000000000000000000000000000000:1000 00000000000000000000000000000000:0000 0A 0 0 0 0 0 0 0 0\n";
        assert_eq!(listening_ports(v6_wildcard), vec![4096]);
    }

    /// A listener on a routable address is not ours to talk to on loopback.
    #[test]
    fn ignores_non_local_listeners() {
        let table = "header\n   0: 010002C0:1000 00000000:0000 0A 0 0 0 0 0 0 0 0\n";
        assert!(listening_ports(table).is_empty());
    }

    /// Several listeners are all reported, in table order, without duplicates.
    #[test]
    fn reports_multiple_candidate_ports_once_each() {
        let table = "header\n\
   0: 0100007F:1000 00000000:0000 0A 0 0 0 0 0 0 0 0\n\
   1: 0100007F:1F90 00000000:0000 0A 0 0 0 0 0 0 0 0\n\
   2: 0100007F:1000 00000000:0000 0A 0 0 0 0 0 0 0 0\n";
        assert_eq!(listening_ports(table), vec![4096, 8080]);
    }

    /// Truncated, empty and malformed tables are survived without panicking.
    #[test]
    fn survives_malformed_tables() {
        assert!(listening_ports("").is_empty());
        assert!(listening_ports("header only\n").is_empty());
        assert!(listening_ports("header\n   0:\n").is_empty());
        assert!(listening_ports("header\n   0: nocolon 00000000:0000 0A\n").is_empty());
        assert!(listening_ports("header\n   0: 0100007F:ZZZZ 0:0 0A\n").is_empty());
        assert!(listening_ports("header\n   0: 0100007F:0000 0:0 0A 0 0\n").is_empty());
    }

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
