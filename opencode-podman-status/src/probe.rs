//! The probe: querying one opencode instance and classifying it.
//!
//! Runs in the main process, which never changes namespace. The requests travel
//! over sockets that were created inside the container's network namespace (see
//! [`crate::ns_socket`]), so `127.0.0.1` on them is the container's loopback,
//! but every byte of every response is handled out here. The ports asked about
//! are only ever ones established by socket ownership (see [`crate::sockets`])
//! or named explicitly by the user.
//!
//! The server is treated as untrusted: its answers are bounded in time and size
//! by the [`Client`], and nothing taken from one response reaches the next
//! request's path unless it passes [`is_safe_id`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::http::{Client, Connect};
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

/// Runs the probe against the given candidate ports, in order.
///
/// Always returns a [`Report`]: failures are reported, not raised, because the
/// caller needs to render something for every container.
///
/// `ports` must already be known to belong to opencode (or be the user's own
/// explicit choice): other servers in the container are never contacted - an
/// earlier version sent each of them a health check and they logged errors.
pub fn run<C: Connect>(client: &mut Client<C>, ports: &[u16], now_ms: i64) -> Report {
    // Normally exactly one. Several means opencode holds more than one listener,
    // all of them its own, so asking each is safe.
    let mut last_error = None;
    for &candidate in ports {
        match probe_one(client, candidate, now_ms) {
            Ok(report) => return report,
            Err(e) => last_error = Some(e),
        }
    }
    Report::failed(last_error.unwrap_or_else(|| "opencode did not answer".to_string()))
}

/// Queries one candidate port.
fn probe_one<C: Connect>(client: &mut Client<C>, port: u16, now_ms: i64) -> Result<Report, String> {
    // A cheap liveness check before the real requests. The port is already known
    // to be opencode's, so this only guards against it still starting up.
    client.get(port, "/global/health").map_err(|e| format!("port {port}: {e}"))?;

    let statuses: HashMap<String, crate::status::SessionStatus> =
        fetch_json(client, port, "/session/status")?;
    let questions: Vec<crate::status::PendingRequest> = fetch_json(client, port, "/question")?;
    let permissions: Vec<crate::status::PendingRequest> =
        fetch_json(client, port, "/permission")?;

    let observation = Observation { statuses, questions, permissions };
    let state = observation.state();

    let since_ms = match state {
        // Free: the oldest pending request's ID encodes when it was created.
        State::Wait => observation.wait_since_ms(now_ms),
        State::Run => busy_since_ms(client, port, &observation, now_ms),
        State::Done => idle_since_ms(client, port, now_ms),
    };

    Ok(Report {
        port: Some(port),
        state: Some(state.word().to_string()),
        since_ms,
        reason: None,
    })
}

/// Fetches and deserialises one endpoint.
fn fetch_json<T: serde::de::DeserializeOwned, C: Connect>(
    client: &mut Client<C>,
    port: u16,
    path: &str,
) -> Result<T, String> {
    let body = client.get(port, path).map_err(|e| format!("GET {path}: {e}"))?;
    serde_json::from_slice(&body).map_err(|e| format!("GET {path}: bad JSON: {e}"))
}

/// When the earliest still-running turn started.
///
/// Asks each busy session for its most recent message and takes the oldest such
/// start, so the figure means "how long has this container been busy".
///
/// Session IDs come from the server's own response and are about to be put into
/// a request path, so any that are not plainly an ID are skipped.
fn busy_since_ms<C: Connect>(
    client: &mut Client<C>,
    port: u16,
    observation: &Observation,
    now_ms: i64,
) -> Option<i64> {
    let mut busy: Vec<&String> = observation
        .statuses
        .iter()
        .filter(|(id, s)| matches!(s.kind.as_str(), "busy" | "retry") && is_safe_id(id))
        .map(|(id, _)| id)
        .collect();
    // Deterministic sampling order, so repeated runs agree when capped.
    busy.sort();

    busy.into_iter()
        .take(MAX_BUSY_SESSIONS_SAMPLED)
        .filter_map(|id| latest_message_ms(client, port, id, now_ms))
        .min()
}

/// True if a server-supplied ID is safe to embed in a request path.
///
/// opencode IDs are a short prefix, an underscore and alphanumerics
/// (`ses_f2948c7fdffe1r7f03mBfBRLsy`). Anything else - above all anything with
/// `/`, `?`, `%`, whitespace or control characters - could redirect the request
/// elsewhere on the API, so it is refused rather than escaped.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Creation time of a session's most recent message.
///
/// `limit=1` returns the *newest* message: verified against opencode 1.18.32 by
/// creating a message in an existing session and seeing only that one come back,
/// far newer than the session's own `time.created`. The result is sanity-checked
/// against the clock regardless, so a future change in ordering would show an
/// unknown duration rather than a fabricated one.
fn latest_message_ms<C: Connect>(
    client: &mut Client<C>,
    port: u16,
    session: &str,
    now_ms: i64,
) -> Option<i64> {
    let path = format!("/session/{session}/message?limit=1");
    let entries: Vec<MessageEntry> = fetch_json(client, port, &path).ok()?;
    let created = entries.first()?.created_ms()?;
    plausible_past(created, now_ms)
}

/// When the instance last did anything, for an idle instance.
fn idle_since_ms<C: Connect>(client: &mut Client<C>, port: u16, now_ms: i64) -> Option<i64> {
    let sessions: Vec<SessionRecord> = fetch_json(client, port, "/session").ok()?;
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
    use crate::http::Loopback;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// A fake opencode on loopback that answers from a route table and records
    /// every request path it receives, for as long as the test runs.
    fn fake_server(routes: Vec<(&'static str, String)>) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { break };
                let mut reader = BufReader::new(sock.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(path.clone());
                let reply = match routes.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => format!("HTTP/1.1 200 OK\r\n\r\n{body}"),
                    None => "HTTP/1.1 404 Not Found\r\n\r\n".to_string(),
                };
                let _ = sock.write_all(reply.as_bytes());
            }
        });
        (port, seen)
    }

    /// A loopback client with the program's normal limits.
    fn client() -> Client<Loopback> {
        Client::new(Loopback, Instant::now() + Duration::from_secs(5), 4 * crate::http::MAX_RESPONSE)
    }

    /// Real opencode IDs are accepted as safe path components.
    #[test]
    fn accepts_real_ids() {
        for id in ["ses_f2948c7fdffe1r7f03mBfBRLsy", "msg_0d6c75ea3001xwhRotcmNTvdqj", "a"] {
            assert!(is_safe_id(id), "{id}");
        }
    }

    /// IDs that could steer a request elsewhere on the API are refused.
    #[test]
    fn refuses_ids_that_could_redirect_a_request() {
        for id in [
            "",
            "../permission",
            "ses_x/../../auth",
            "ses_x?directory=/",
            "ses_x%2F",
            "ses_x\r\nX: y",
            "ses x",
            "ses_\u{e9}",
            &"a".repeat(65),
        ] {
            assert!(!is_safe_id(id), "should refuse {id:?}");
        }
    }

    /// End to end: an idle instance is classified `done`, and its age comes
    /// from the newest session's `updated` time.
    #[test]
    fn classifies_idle_instance_end_to_end() {
        let now = 1790308945355;
        let (port, seen) = fake_server(vec![
            ("/global/health", r#"{"healthy":true}"#.into()),
            ("/session/status", "{}".into()),
            ("/question", "[]".into()),
            ("/permission", "[]".into()),
            ("/session", format!(r#"[{{"time":{{"created":1,"updated":{}}}}}]"#, now - 60_000)),
        ]);
        let report = run(&mut client(), &[port], now);
        assert_eq!(report.parsed_state(), Some(State::Done));
        assert_eq!(report.since_ms, Some(now - 60_000));
        assert_eq!(report.port, Some(port));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["/global/health", "/session/status", "/question", "/permission", "/session"]
        );
    }

    /// A busy session with a hostile ID is not used to build a request path;
    /// the instance is still reported as running, just with an unknown age.
    #[test]
    fn hostile_session_id_never_reaches_a_request() {
        let (port, seen) = fake_server(vec![
            ("/global/health", "{}".into()),
            ("/session/status", r#"{"../../auth/x?":{"type":"busy"}}"#.into()),
            ("/question", "[]".into()),
            ("/permission", "[]".into()),
        ]);
        let report = run(&mut client(), &[port], 1790308945355);
        assert_eq!(report.parsed_state(), Some(State::Run));
        assert_eq!(report.since_ms, None);
        let seen = seen.lock().unwrap();
        assert!(seen.iter().all(|p| !p.contains("auth")), "requests: {seen:?}");
        assert_eq!(seen.len(), 4, "requests: {seen:?}");
    }

    /// A safe busy session's latest message supplies the running age.
    #[test]
    fn busy_age_comes_from_latest_message() {
        let now = 1790308945355;
        let (port, _) = fake_server(vec![
            ("/global/health", "{}".into()),
            ("/session/status", r#"{"ses_abc":{"type":"busy"}}"#.into()),
            ("/question", "[]".into()),
            ("/permission", "[]".into()),
            (
                "/session/ses_abc/message?limit=1",
                format!(r#"[{{"info":{{"time":{{"created":{}}}}},"parts":[]}}]"#, now - 5_000),
            ),
        ]);
        let report = run(&mut client(), &[port], now);
        assert_eq!(report.parsed_state(), Some(State::Run));
        assert_eq!(report.since_ms, Some(now - 5_000));
    }

    /// An unreachable candidate is reported with its port, not as a panic.
    #[test]
    fn unreachable_port_is_a_failed_report() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let report = run(&mut client(), &[port], 0);
        assert_eq!(report.parsed_state(), None);
        assert!(report.reason.unwrap().contains(&format!("port {port}")));
    }

    /// With no candidates at all there is still a report, not a panic.
    #[test]
    fn no_candidates_is_a_failed_report() {
        let report = run(&mut client(), &[], 0);
        assert!(report.reason.is_some());
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
