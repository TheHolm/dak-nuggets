//! The probe: querying one opencode instance and classifying it.
//!
//! Runs in the main process, which never changes namespace. The requests travel
//! over sockets that were created inside the container's network namespace (see
//! [`crate::ns_socket`]), so `127.0.0.1` on them is the container's loopback,
//! but every byte of every response is handled out here. The ports asked about
//! are only ever ones established by socket ownership (see [`crate::sockets`])
//! or named explicitly by the user.
//!
//! Two kinds of server can answer, and both speak the same routes and shapes:
//! opencode's own API (only when opencode was started with `--port`) and the
//! read-only status plugin (`plugin/opencode-podman-status.js`). They are told
//! apart by the `source` marker the plugin puts in `/global/health`; with the
//! plugin, every age comes straight from its `since_ms` fields.
//!
//! The server is treated as untrusted: its answers are bounded in time and size
//! by the [`Client`], and nothing taken from one response reaches the next
//! request's path unless it passes [`is_safe_id`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::http::{Client, Connect};
use crate::status::{Observation, State};

/// The `source` value the status plugin reports in `/global/health`.
pub const PLUGIN_MARKER: &str = "opencode-podman-status-plugin";

/// Port the status plugin listens on unless `OPENCODE_STATUS_PORT` says otherwise.
pub const DEFAULT_PLUGIN_PORT: u16 = 4097;

/// Which kind of server the user wants the state from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The status plugin if opencode holds its port, otherwise opencode's API.
    Auto,
    /// Only the status plugin.
    Plugin,
    /// Only opencode's own API (needs `opencode --port`).
    Api,
}

impl Source {
    /// Parses a `--source` value.
    pub fn parse(value: &str) -> Option<Source> {
        match value {
            "auto" => Some(Source::Auto),
            "plugin" => Some(Source::Plugin),
            "api" => Some(Source::Api),
            _ => None,
        }
    }
}

/// Orders candidate ports so the preferred kind of server is tried first.
///
/// Every candidate is already known to belong to opencode (the plugin runs
/// inside opencode's own process, so its listener is opencode's too). For
/// `auto` and `plugin` the plugin port goes first; for `api` it goes last. The
/// `/global/health` marker, not the port number, then decides what each one is.
pub fn order_candidates(ports: &[u16], plugin_port: u16, source: Source) -> Vec<u16> {
    let (plugin, others): (Vec<u16>, Vec<u16>) = ports.iter().partition(|&&p| p == plugin_port);
    match source {
        Source::Auto | Source::Plugin => plugin.into_iter().chain(others).collect(),
        Source::Api => others.into_iter().chain(plugin).collect(),
    }
}

/// The part of `/global/health` that matters: the plugin's marker, if any.
#[derive(Debug, Default, Deserialize)]
struct Health {
    #[serde(default)]
    source: Option<String>,
}

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
    /// What answered: `plugin` or `api`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
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
        Report { port: None, source: None, state: None, since_ms: None, reason: Some(reason.into()) }
    }

    /// Parses the state word back into a [`State`].
    ///
    /// Unknown words are treated as "not determined" rather than guessed at.
    pub fn parsed_state(&self) -> Option<State> {
        self.state.as_deref().and_then(State::from_word)
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
/// `ports` are tried in order (see [`order_candidates`]) and the first one that
/// is the kind of server `source` allows, and answers, wins.
pub fn run<C: Connect>(client: &mut Client<C>, ports: &[u16], source: Source, now_ms: i64) -> Report {
    // Normally one or two (the plugin, and opencode's API if --port was given).
    // All belong to opencode, so asking each is safe.
    let mut last_error = None;
    for &candidate in ports {
        match probe_one(client, candidate, source, now_ms) {
            Ok(report) => return report,
            Err(e) => last_error = Some(e),
        }
    }
    Report::failed(last_error.unwrap_or_else(|| "opencode did not answer".to_string()))
}

/// Queries one candidate port.
fn probe_one<C: Connect>(
    client: &mut Client<C>,
    port: u16,
    source: Source,
    now_ms: i64,
) -> Result<Report, String> {
    // A liveness check that also says which kind of server this is. The port is
    // already known to be opencode's, so asking is safe.
    let body = client.get(port, "/global/health").map_err(|e| format!("port {port}: {e}"))?;
    let health: Health = serde_json::from_slice(&body).unwrap_or_default();
    let is_plugin = health.source.as_deref() == Some(PLUGIN_MARKER);
    match (source, is_plugin) {
        (Source::Plugin, false) => {
            return Err(format!("port {port}: not the status plugin (is it enabled in opencode.json?)"))
        }
        (Source::Api, true) => return Err(format!("port {port}: this is the status plugin, not opencode's API")),
        _ => {}
    }

    let statuses: HashMap<String, crate::status::SessionStatus> =
        fetch_json(client, port, "/session/status")?;
    let questions: Vec<crate::status::PendingRequest> = fetch_json(client, port, "/question")?;
    let permissions: Vec<crate::status::PendingRequest> =
        fetch_json(client, port, "/permission")?;

    let observation = Observation { statuses, questions, permissions };
    let state = observation.state();

    let since_ms = if is_plugin {
        // The plugin stamps every entry itself: no decoding, no extra requests.
        observation.reported_since_ms(state).and_then(|ms| plausible_past(ms, now_ms))
    } else {
        match state {
            // Free: the oldest pending request's ID encodes when it was created.
            State::Wait => observation.wait_since_ms(now_ms),
            State::Run => busy_since_ms(client, port, &observation, now_ms),
            State::Done => idle_since_ms(client, port, now_ms),
            // opencode's API has no error status; this only happens if it gains one.
            State::Error => None,
        }
    };

    Ok(Report {
        port: Some(port),
        source: Some(if is_plugin { "plugin" } else { "api" }.to_string()),
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
        let report = run(&mut client(), &[port], Source::Auto, now);
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
        let report = run(&mut client(), &[port], Source::Auto, 1790308945355);
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
        let report = run(&mut client(), &[port], Source::Auto, now);
        assert_eq!(report.parsed_state(), Some(State::Run));
        assert_eq!(report.since_ms, Some(now - 5_000));
    }

    /// The plugin's routes, as served by plugin/opencode-podman-status.js.
    fn plugin_routes(status: &str, questions: &str) -> Vec<(&'static str, String)> {
        vec![
            (
                "/global/health",
                format!(r#"{{"healthy":true,"version":"0.2.0","source":"{PLUGIN_MARKER}"}}"#),
            ),
            ("/session/status", status.to_string()),
            ("/question", questions.to_string()),
            ("/permission", "[]".to_string()),
        ]
    }

    /// Against the plugin, the age comes from its `since_ms` and costs no
    /// requests beyond the four routes - even for a busy session.
    #[test]
    fn plugin_supplies_ages_without_extra_requests() {
        let now = 1790308945355;
        let (port, seen) = fake_server(plugin_routes(
            &format!(r#"{{"ses_a":{{"type":"busy","since_ms":{}}}}}"#, now - 7_000),
            "[]",
        ));
        let report = run(&mut client(), &[port], Source::Auto, now);
        assert_eq!(report.parsed_state(), Some(State::Run));
        assert_eq!(report.since_ms, Some(now - 7_000));
        assert_eq!(report.source.as_deref(), Some("plugin"));
        assert_eq!(seen.lock().unwrap().len(), 4);
    }

    /// The plugin's error state comes through, with its age.
    #[test]
    fn plugin_reports_error_state() {
        let now = 1790308945355;
        let (port, _) = fake_server(plugin_routes(
            &format!(r#"{{"ses_a":{{"type":"error","since_ms":{}}}}}"#, now - 60_000),
            "[]",
        ));
        let report = run(&mut client(), &[port], Source::Auto, now);
        assert_eq!(report.parsed_state(), Some(State::Error));
        assert_eq!(report.since_ms, Some(now - 60_000));
    }

    /// A plugin timestamp from the future is shown as unknown, not negative.
    #[test]
    fn implausible_plugin_time_is_unknown() {
        let now = 1790308945355;
        let (port, _) = fake_server(plugin_routes(
            &format!(r#"{{"ses_a":{{"type":"busy","since_ms":{}}}}}"#, now + 3_600_000),
            "[]",
        ));
        assert_eq!(run(&mut client(), &[port], Source::Auto, now).since_ms, None);
    }

    /// `--source plugin` refuses opencode's API, and `--source api` the plugin,
    /// each after nothing more than the health check.
    #[test]
    fn source_restricts_what_is_accepted() {
        let (api, api_seen) = fake_server(vec![("/global/health", r#"{"healthy":true}"#.into())]);
        let report = run(&mut client(), &[api], Source::Plugin, 0);
        assert!(report.reason.unwrap().contains("not the status plugin"));
        assert_eq!(*api_seen.lock().unwrap(), vec!["/global/health"]);

        let (plugin, plugin_seen) = fake_server(plugin_routes("{}", "[]"));
        let report = run(&mut client(), &[plugin], Source::Api, 0);
        assert!(report.reason.unwrap().contains("is the status plugin"));
        assert_eq!(*plugin_seen.lock().unwrap(), vec!["/global/health"]);
    }

    /// In auto mode the first candidate that answers wins, whichever kind it is.
    #[test]
    fn auto_falls_back_to_the_api() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let dead = listener.local_addr().unwrap().port();
        drop(listener);
        let (api, _) = fake_server(vec![
            ("/global/health", r#"{"healthy":true}"#.into()),
            ("/session/status", "{}".into()),
            ("/question", "[]".into()),
            ("/permission", "[]".into()),
            ("/session", "[]".into()),
        ]);
        let report = run(&mut client(), &[dead, api], Source::Auto, 0);
        assert_eq!(report.source.as_deref(), Some("api"));
        assert_eq!(report.parsed_state(), Some(State::Done));
    }

    /// The plugin port is tried first, except when the API is asked for.
    #[test]
    fn orders_candidates_by_source() {
        assert_eq!(order_candidates(&[4096, 4097], 4097, Source::Auto), vec![4097, 4096]);
        assert_eq!(order_candidates(&[4096, 4097], 4097, Source::Plugin), vec![4097, 4096]);
        assert_eq!(order_candidates(&[4097, 4096], 4097, Source::Api), vec![4096, 4097]);
        assert_eq!(order_candidates(&[4096], 4097, Source::Auto), vec![4096]);
        assert_eq!(order_candidates(&[], 4097, Source::Auto), Vec::<u16>::new());
    }

    /// `--source` values parse; others do not.
    #[test]
    fn parses_source_values() {
        assert_eq!(Source::parse("auto"), Some(Source::Auto));
        assert_eq!(Source::parse("plugin"), Some(Source::Plugin));
        assert_eq!(Source::parse("api"), Some(Source::Api));
        assert_eq!(Source::parse("both"), None);
    }

    /// An unreachable candidate is reported with its port, not as a panic.
    #[test]
    fn unreachable_port_is_a_failed_report() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let report = run(&mut client(), &[port], Source::Auto, 0);
        assert_eq!(report.parsed_state(), None);
        assert!(report.reason.unwrap().contains(&format!("port {port}")));
    }

    /// With no candidates at all there is still a report, not a panic.
    #[test]
    fn no_candidates_is_a_failed_report() {
        let report = run(&mut client(), &[], Source::Auto, 0);
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
            ("error", State::Error),
        ] {
            let report = Report {
                port: Some(4096),
                source: Some("api".to_string()),
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
            source: None,
            state: Some("hibernating".to_string()),
            since_ms: None,
            reason: None,
        };
        assert_eq!(report.parsed_state(), None);
    }
}
