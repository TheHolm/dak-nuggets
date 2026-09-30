//! Discovering opencode containers and giving them stable slot numbers.
//!
//! Containers are found with `podman ps --format json` and filtered by name:
//! `opencode` or `opencode-<something>`. Their main process ID - the host-visible
//! PID, the handle used to reach the container's namespaces - comes from the same
//! `ps` output, or from `podman inspect` for a podman that does not list it.
//! Every podman invocation is bounded by a deadline (see [`run_bounded`]).
//!
//! Slots are assigned by **creation time, oldest first**. That is stable across
//! repeated runs of this program, which is what matters for a keypad button
//! pointing at a fixed slot. Terminating a container does renumber the ones
//! created after it; that was accepted deliberately. Container ID breaks ties, so
//! two containers created in the same second cannot swap places between runs.
//!
//! Podman labels were considered and rejected: they are immutable after container
//! creation, so this program could not assign them itself even if asked to.

use std::io::{ErrorKind, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// Default pattern for an opencode container name: `opencode` or `opencode-*`.
const NAME_PREFIX: &str = "opencode";

/// One discovered container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// Slot number, 1-based, assigned by creation order.
    pub slot: usize,
    /// Container name as podman reports it, with no leading slash.
    pub name: String,
    /// Container ID, used only as a tiebreaker and for diagnostics.
    pub id: String,
    /// Creation time as reported by podman, in seconds since the epoch.
    pub created: i64,
    /// Host-visible PID of the container's main process, once known.
    pub pid: Option<i32>,
}

/// A container as it appears in `podman ps --format json`.
///
/// Field names vary in case across podman versions, and `Names` is a list. Only
/// what is needed is deserialised, so unrelated schema churn cannot break this.
#[derive(Debug, Deserialize)]
struct PsEntry {
    #[serde(alias = "ID", alias = "Id", alias = "id")]
    id: String,
    #[serde(alias = "Names", alias = "names")]
    names: Vec<String>,
    /// Unix seconds. Podman also emits `CreatedAt` as a string, which is not
    /// used: the numeric field is unambiguous.
    #[serde(alias = "Created", alias = "created")]
    created: i64,
    /// Host PID of the container's main process. Present in podman's `ps`
    /// JSON; 0 for a container that is not running. Optional so that a podman
    /// without it falls back to `inspect` rather than failing to parse.
    #[serde(default, alias = "Pid", alias = "pid")]
    pid: Option<i32>,
}

/// True if a container name identifies an opencode instance.
///
/// Accepts exactly `opencode`, or `opencode-` followed by at least one character.
/// Deliberately does not accept `opencodex` or `my-opencode`, so unrelated
/// containers are never probed.
pub fn is_opencode_name(name: &str) -> bool {
    if name == NAME_PREFIX {
        return true;
    }
    match name.strip_prefix(NAME_PREFIX) {
        Some(rest) => rest.starts_with('-') && rest.len() > 1,
        None => false,
    }
}

/// Parses `podman ps --format json` output into slot-numbered containers.
///
/// Entries whose names do not identify opencode are dropped. The result is sorted
/// by creation time, then by container ID, and numbered from 1.
pub fn parse_ps(json: &str) -> Result<Vec<Container>, String> {
    let entries: Vec<PsEntry> =
        serde_json::from_str(json).map_err(|e| format!("cannot parse podman ps output: {e}"))?;

    let mut found: Vec<Container> = entries
        .into_iter()
        .filter_map(|entry| {
            // Podman reports a list of names; the first is the primary one.
            let name = entry
                .names
                .iter()
                .find(|n| is_opencode_name(n.trim_start_matches('/')))?
                .trim_start_matches('/')
                .to_string();
            Some(Container {
                slot: 0,
                name,
                id: entry.id,
                created: entry.created,
                // PID 0 means not running; treat it as unknown, as inspect does.
                pid: entry.pid.filter(|&p| p > 0),
            })
        })
        .collect();

    // Creation order, with the ID as a deterministic tiebreaker so same-second
    // creations cannot swap between runs.
    found.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    for (index, container) in found.iter_mut().enumerate() {
        container.slot = index + 1;
    }
    Ok(found)
}

/// Parses the batched output of `podman inspect --format '{{.Name}} {{.State.Pid}}'`.
///
/// One `name pid` pair per line. Unparseable lines are skipped: a container that
/// stopped between `ps` and `inspect` should not fail the whole run.
pub fn parse_pids(output: &str) -> Vec<(String, i32)> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?.trim_start_matches('/').to_string();
            let pid: i32 = parts.next()?.parse().ok()?;
            // A stopped container reports PID 0.
            if pid <= 0 {
                return None;
            }
            Some((name, pid))
        })
        .collect()
}

/// Attaches PIDs to containers, dropping any whose PID could not be determined.
///
/// A container that already has a PID (from `ps`) keeps it.
pub fn attach_pids(containers: &mut Vec<Container>, pids: &[(String, i32)]) {
    for container in containers.iter_mut().filter(|c| c.pid.is_none()) {
        container.pid = pids
            .iter()
            .find(|(name, _)| *name == container.name)
            .map(|(_, pid)| *pid);
    }
    containers.retain(|c| c.pid.is_some());
}

/// Why discovery failed.
///
/// The two cases are handled differently: podman that is merely slow - stuck
/// behind a container lock while a container is being removed, which lasts
/// seconds and then clears by itself - is shown on DAK buttons as "unknown for
/// now", while podman that is missing or failing is a real error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoverError {
    /// podman did not answer before the deadline and was killed.
    TimedOut(String),
    /// Anything else: podman missing, failing, or producing unusable output.
    Failed(String),
}

impl std::fmt::Display for DiscoverError {
    /// The message alone; which case it is is the caller's business.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscoverError::TimedOut(m) | DiscoverError::Failed(m) => f.write_str(m),
        }
    }
}

/// How long a finished podman may keep its output pipes open before we stop
/// waiting for EOF.
///
/// podman can leave a long-lived process behind (the rootless pause process, on
/// the first invocation after boot) that may inherit stdout. The output podman
/// itself wrote is complete once podman has exited, so waiting for EOF beyond a
/// short grace period would only turn that into a timeout.
const EXIT_GRACE: Duration = Duration::from_millis(200);

/// How often a running podman is checked for exit.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// What one podman invocation produced.
#[derive(Debug)]
struct Outcome {
    /// Whether podman exited with status 0.
    success: bool,
    /// Everything it wrote to stdout.
    stdout: String,
    /// Everything it wrote to stderr, trimmed.
    stderr: String,
}

/// Collects one pipe into a shared buffer until EOF, on a thread of its own.
///
/// The buffer is shared rather than returned so the caller can take whatever
/// has arrived without waiting for EOF, which a leftover grandchild holding the
/// pipe open could delay indefinitely. The returned flag is set at EOF.
fn collect(mut pipe: impl Read + Send + 'static) -> (Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));
    let (b, d) = (Arc::clone(&buffer), Arc::clone(&done));
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let mut buf = b.lock().unwrap_or_else(|e| e.into_inner());
                    // podman's output here is a few KiB per container; this
                    // only stops a runaway process from eating memory.
                    if buf.len() < MAX_OUTPUT {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        d.store(true, Ordering::Release);
    });
    (buffer, done)
}

/// Most bytes kept from one podman output stream.
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

/// Takes a copy of a collected buffer as text.
fn snapshot(buffer: &Mutex<Vec<u8>>) -> String {
    let buf = buffer.lock().unwrap_or_else(|e| e.into_inner());
    String::from_utf8_lossy(&buf).into_owned()
}

/// Runs `program` with `args`, killing it if it has not exited by `deadline`.
///
/// podman takes a per-container lock for `ps` and `inspect`, and the cleanup
/// that runs when a container stops or is removed holds that lock for as long
/// as it takes - seconds, sometimes. Unbounded, that wait was what made DAK kill
/// this program (DAK allows a `text_exec` 5 s) and draw "Error" whenever a
/// container went away. So podman gets a deadline like everything else.
///
/// podman is started in a process group of its own and the whole group is
/// killed on overrun, since rootless podman re-executes itself into a child.
/// Stdin is closed. Never waits past the deadline, even for pipe EOF.
///
/// `command` is the program and any leading arguments (just `["podman"]`
/// outside tests); `args` follow it.
fn run_bounded(
    command: &[&str],
    args: &[&str],
    deadline: Instant,
) -> Result<Outcome, DiscoverError> {
    let (program, leading) = command.split_first().expect("command names a program");
    let what = || format!("podman {}", args.first().copied().unwrap_or(""));
    let mut child = Command::new(program)
        .args(leading)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| {
            DiscoverError::Failed(format!(
                "cannot run {program}: {e} (is podman installed and on PATH?)"
            ))
        })?;

    let (out, out_done) = collect(child.stdout.take().expect("stdout is piped"));
    let (err, err_done) = collect(child.stderr.take().expect("stderr is piped"));

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_group(&mut child);
                return Err(DiscoverError::Failed(format!(
                    "cannot wait for {}: {e}",
                    what()
                )));
            }
        }
        let now = Instant::now();
        if now >= deadline {
            kill_group(&mut child);
            return Err(DiscoverError::TimedOut(format!(
                "{} did not answer in time (is a container being stopped or removed?)",
                what()
            )));
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline - now));
    };

    // podman has exited; its own output is complete. Wait briefly for EOF so the
    // readers have caught up, but never past the deadline.
    let settle = (Instant::now() + EXIT_GRACE).min(deadline);
    while !(out_done.load(Ordering::Acquire) && err_done.load(Ordering::Acquire))
        && Instant::now() < settle
    {
        std::thread::sleep(POLL_INTERVAL);
    }

    Ok(Outcome {
        success: status.success(),
        stdout: snapshot(&out),
        stderr: snapshot(&err).trim().to_string(),
    })
}

/// Kills a child's whole process group and reaps the child.
fn kill_group(child: &mut Child) {
    // SAFETY: the child was started with `process_group(0)`, so its PID is also
    // its process group ID, and it is still unreaped, so neither can have been
    // reused by anything unrelated.
    unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
    let _ = child.wait();
}

/// Runs podman with a deadline, returning stdout or a descriptive error.
fn podman(command: &[&str], args: &[&str], deadline: Instant) -> Result<String, DiscoverError> {
    let outcome = run_bounded(command, args, deadline)?;
    if !outcome.success {
        return Err(DiscoverError::Failed(format!(
            "podman {} failed: {}",
            args.join(" "),
            outcome.stderr
        )));
    }
    Ok(outcome.stdout)
}

/// Discovers every running opencode container, slot-numbered and with PIDs.
///
/// `command` is how to run podman - `["podman"]`, or a fake in tests. Gives up
/// at `deadline`; see [`run_bounded`] for why podman needs one.
///
/// Usually a single `podman ps`: its JSON carries each container's PID. Only
/// containers listed without one cost a `podman inspect`, batched into one
/// invocation. A container can be removed between the two - that is exactly
/// when this runs slowly - and `inspect` then fails for the whole batch while
/// still printing the others, so its output is used whatever its exit status;
/// only the vanished container is dropped.
pub(crate) fn discover_with(
    command: &[&str],
    deadline: Instant,
) -> Result<Vec<Container>, DiscoverError> {
    let ps = podman(command, &["ps", "--format", "json"], deadline)?;
    let mut containers = parse_ps(&ps).map_err(DiscoverError::Failed)?;
    let missing: Vec<String> = containers
        .iter()
        .filter(|c| c.pid.is_none())
        .map(|c| c.id.clone())
        .collect();
    if missing.is_empty() {
        return Ok(containers);
    }

    let mut args: Vec<&str> = vec!["inspect", "--format", "{{.Name}} {{.State.Pid}}"];
    args.extend(missing.iter().map(String::as_str));
    let inspected = run_bounded(command, &args, deadline)?;
    let pids = parse_pids(&inspected.stdout);
    // A failure with no usable output at all is podman failing, not a
    // container vanishing, and is reported as such.
    if !inspected.success && pids.is_empty() {
        return Err(DiscoverError::Failed(format!(
            "podman inspect failed: {}",
            inspected.stderr
        )));
    }
    attach_pids(&mut containers, &pids);
    Ok(containers)
}

/// Reads a container's hostname, resolved exactly the way the window-title
/// plugin resolves it: the `HOSTNAME` environment variable when it is set and
/// not blank, otherwise the container's system hostname.
///
/// `command` is how to run podman - `["podman"]`, or a fake in tests - and the
/// call obeys the same deadline as discovery. Only used to build the `--title`
/// string, so `name` is a container that discovery already found.
pub(crate) fn hostname(
    command: &[&str],
    name: &str,
    deadline: Instant,
) -> Result<String, DiscoverError> {
    let format = "{{json .Config.Hostname}} {{json .Config.Env}}";
    let out = podman(command, &["inspect", "--format", format, name], deadline)?;
    parse_hostname(&out)
        .map_err(|e| DiscoverError::Failed(format!("cannot read hostname of {name}: {e}")))
}

/// Resolves a hostname the way `plugin/opencode-window-title.js` does.
///
/// The plugin's `HOSTNAME` takes precedence over the system hostname and is used
/// only when it is non-empty after trimming; the fallback is trimmed the same
/// way. Kept character-for-character in step with the plugin so `--title`
/// prints a string that matches the window the plugin labelled.
fn resolve_host(hostname: Option<&str>, env: &[String]) -> String {
    let from_env = env
        .iter()
        .find_map(|entry| entry.strip_prefix("HOSTNAME="))
        .map(str::trim)
        .filter(|h| !h.is_empty());
    let system = hostname.map(str::trim).filter(|h| !h.is_empty());
    from_env.or(system).unwrap_or("").to_string()
}

/// Parses `podman inspect --format '{{json .Config.Hostname}} {{json .Config.Env}}'`
/// output and resolves the hostname from it.
///
/// `Config.Hostname` is a JSON string (or null) and `Config.Env` a JSON array of
/// `KEY=value` strings; any other shape, a missing value, or malformed JSON is
/// an error rather than a silently empty hostname.
fn parse_hostname(output: &str) -> Result<String, String> {
    let mut values = serde_json::Deserializer::from_str(output).into_iter::<serde_json::Value>();
    let hostname = values
        .next()
        .transpose()
        .map_err(|e| format!("invalid hostname JSON: {e}"))?
        .ok_or_else(|| "inspect output has no hostname".to_string())?;
    let env = values
        .next()
        .transpose()
        .map_err(|e| format!("invalid environment JSON: {e}"))?
        .ok_or_else(|| "inspect output has no environment".to_string())?;

    let hostname = match hostname {
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Null => None,
        other => return Err(format!("unexpected hostname value: {other}")),
    };
    let env: Vec<String> = match env {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                serde_json::Value::String(s) => Some(s),
                _ => None,
            })
            .collect(),
        other => return Err(format!("unexpected environment value: {other}")),
    };
    Ok(resolve_host(hostname.as_deref(), &env))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only exact opencode names are recognised.
    #[test]
    fn recognises_opencode_names() {
        assert!(is_opencode_name("opencode"));
        assert!(is_opencode_name("opencode-web"));
        assert!(is_opencode_name("opencode-1"));
        assert!(is_opencode_name("opencode-a-b-c"));
    }

    /// Names that merely resemble opencode are not probed.
    #[test]
    fn rejects_lookalike_names() {
        assert!(!is_opencode_name("opencodex"));
        assert!(!is_opencode_name("my-opencode"));
        assert!(!is_opencode_name("opencode_web"));
        assert!(!is_opencode_name("openconnect"));
        assert!(!is_opencode_name("opencode-"));
        assert!(!is_opencode_name(""));
        assert!(!is_opencode_name("OPENCODE"));
    }

    /// Slots follow creation order, oldest first, regardless of listing order.
    #[test]
    fn numbers_slots_by_creation_time() {
        let json = r#"[
          {"Id":"ccc","Names":["opencode-third"],"Created":300},
          {"Id":"aaa","Names":["opencode-first"],"Created":100},
          {"Id":"bbb","Names":["opencode-second"],"Created":200}
        ]"#;
        let got = parse_ps(json).unwrap();
        let names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["opencode-first", "opencode-second", "opencode-third"]
        );
        assert_eq!(
            got.iter().map(|c| c.slot).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    /// Same-second creations are ordered by container ID, so the numbering cannot
    /// differ between two runs.
    #[test]
    fn breaks_creation_ties_by_id_deterministically() {
        let json = r#"[
          {"Id":"zzz","Names":["opencode-z"],"Created":100},
          {"Id":"aaa","Names":["opencode-a"],"Created":100},
          {"Id":"mmm","Names":["opencode-m"],"Created":100}
        ]"#;
        let first = parse_ps(json).unwrap();
        let names: Vec<&str> = first.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["opencode-a", "opencode-m", "opencode-z"]);

        // Re-parsing a differently ordered listing gives identical slots.
        let reordered = r#"[
          {"Id":"mmm","Names":["opencode-m"],"Created":100},
          {"Id":"zzz","Names":["opencode-z"],"Created":100},
          {"Id":"aaa","Names":["opencode-a"],"Created":100}
        ]"#;
        assert_eq!(parse_ps(reordered).unwrap(), first);
    }

    /// Non-opencode containers are filtered out and do not consume slots.
    #[test]
    fn ignores_unrelated_containers() {
        let json = r#"[
          {"Id":"aaa","Names":["postgres"],"Created":100},
          {"Id":"bbb","Names":["opencode-web"],"Created":200},
          {"Id":"ccc","Names":["openconnect"],"Created":300}
        ]"#;
        let got = parse_ps(json).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "opencode-web");
        assert_eq!(got[0].slot, 1);
    }

    /// Podman's alternative field spellings are all accepted.
    #[test]
    fn accepts_podman_field_name_variants() {
        for json in [
            r#"[{"Id":"a","Names":["opencode"],"Created":1}]"#,
            r#"[{"ID":"a","Names":["opencode"],"Created":1}]"#,
            r#"[{"id":"a","names":["opencode"],"created":1}]"#,
        ] {
            let got = parse_ps(json).unwrap();
            assert_eq!(got.len(), 1, "failed for {json}");
            assert_eq!(got[0].name, "opencode");
        }
    }

    /// Names with podman's leading slash are normalised.
    #[test]
    fn strips_leading_slash_from_names() {
        let json = r#"[{"Id":"a","Names":["/opencode-web"],"Created":1}]"#;
        let got = parse_ps(json).unwrap();
        assert_eq!(got[0].name, "opencode-web");
    }

    /// No containers is a valid, empty result rather than an error.
    #[test]
    fn empty_listing_is_not_an_error() {
        assert_eq!(parse_ps("[]").unwrap(), Vec::new());
    }

    /// Malformed podman output is reported rather than silently treated as empty.
    #[test]
    fn malformed_listing_is_an_error() {
        assert!(parse_ps("").is_err());
        assert!(parse_ps("not json").is_err());
        assert!(parse_ps(r#"{"Id":"a"}"#).is_err());
    }

    /// PID lines parse into name/PID pairs.
    #[test]
    fn parses_inspect_pid_output() {
        let out = "/opencode-web 1234\n/opencode-api 5678\n";
        assert_eq!(
            parse_pids(out),
            vec![
                ("opencode-web".to_string(), 1234),
                ("opencode-api".to_string(), 5678)
            ]
        );
    }

    /// A stopped container reports PID 0 and is skipped.
    #[test]
    fn skips_zero_and_malformed_pids() {
        let out = "/opencode-a 0\n/opencode-b notanumber\nbroken\n\n/opencode-c 42\n";
        assert_eq!(parse_pids(out), vec![("opencode-c".to_string(), 42)]);
    }

    /// PIDs are matched onto containers by name.
    #[test]
    fn attaches_pids_by_name() {
        let mut containers = parse_ps(
            r#"[
              {"Id":"aaa","Names":["opencode-web"],"Created":100},
              {"Id":"bbb","Names":["opencode-api"],"Created":200}
            ]"#,
        )
        .unwrap();
        attach_pids(
            &mut containers,
            &[
                ("opencode-api".to_string(), 22),
                ("opencode-web".to_string(), 11),
            ],
        );
        assert_eq!(containers[0].pid, Some(11));
        assert_eq!(containers[1].pid, Some(22));
    }

    /// A container with no PID is dropped, but the slots already assigned to the
    /// survivors are left alone - renumbering here would defeat the point.
    #[test]
    fn drops_containers_without_a_pid_keeping_slots() {
        let mut containers = parse_ps(
            r#"[
              {"Id":"aaa","Names":["opencode-web"],"Created":100},
              {"Id":"bbb","Names":["opencode-api"],"Created":200},
              {"Id":"ccc","Names":["opencode-db"],"Created":300}
            ]"#,
        )
        .unwrap();
        attach_pids(
            &mut containers,
            &[
                ("opencode-web".to_string(), 11),
                ("opencode-db".to_string(), 33),
            ],
        );
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].slot, 1);
        assert_eq!(containers[1].slot, 3);
    }

    /// `ps` JSON carrying `Pid` fills it in; PID 0 (not running) is unknown.
    #[test]
    fn takes_pids_from_ps_json() {
        let got = parse_ps(
            r#"[
              {"Id":"a","Names":["opencode-a"],"Created":1,"Pid":1234},
              {"Id":"b","Names":["opencode-b"],"Created":2,"Pid":0},
              {"Id":"c","Names":["opencode-c"],"Created":3}
            ]"#,
        )
        .unwrap();
        assert_eq!(
            got.iter().map(|c| c.pid).collect::<Vec<_>>(),
            vec![Some(1234), None, None]
        );
    }

    /// A PID already known from `ps` is not overwritten by `inspect` output.
    #[test]
    fn attach_keeps_pids_from_ps() {
        let mut containers =
            parse_ps(r#"[{"Id":"a","Names":["opencode-a"],"Created":1,"Pid":7}]"#).unwrap();
        attach_pids(&mut containers, &[("opencode-a".to_string(), 99)]);
        assert_eq!(containers[0].pid, Some(7));
    }

    /// A scratch directory holding a fake `podman` script, removed on drop.
    struct FakePodman {
        dir: std::path::PathBuf,
        /// Path of the script inside `dir`.
        script: String,
    }

    impl FakePodman {
        /// Writes `body` as a `/bin/sh` script named `podman`.
        ///
        /// `$DIR` in the body is replaced by the scratch directory, so scripts
        /// can leave evidence (invocation logs, PID files) there.
        fn new(body: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "opencode-podman-status-test-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let script = dir.join("podman");
            let body = body.replace("$DIR", dir.to_str().unwrap());
            std::fs::write(&script, format!("{body}\n")).unwrap();
            let script = script.to_str().unwrap().to_string();
            FakePodman { dir, script }
        }

        /// The command running the script.
        ///
        /// Through `/bin/sh`, so the script is only ever read: exec'ing a file
        /// just written fails with `ETXTBSY` whenever another test thread forks
        /// while the write is still open.
        fn cmd(&self) -> [&str; 2] {
            ["/bin/sh", &self.script]
        }

        /// Reads a file the script wrote, or an empty string.
        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
        }
    }

    impl Drop for FakePodman {
        /// Removes the scratch directory.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// True while a process with this PID exists (and is not a zombie).
    fn alive(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => !stat
                .rsplit(')')
                .next()
                .unwrap_or("")
                .trim_start()
                .starts_with('Z'),
            Err(_) => false,
        }
    }

    /// Output and status of a podman that answers promptly are passed through.
    #[test]
    fn bounded_run_returns_output() {
        let fake = FakePodman::new("echo out; echo err >&2; exit 3");
        let o = run_bounded(
            &fake.cmd(),
            &["ps"],
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        assert!(!o.success);
        assert_eq!(o.stdout, "out\n");
        assert_eq!(o.stderr, "err");
    }

    /// The regression: a podman stuck behind a container lock is killed at the
    /// deadline, together with its children, instead of holding the run past
    /// DAK's 5 s limit.
    #[test]
    fn bounded_run_kills_a_stuck_podman_and_its_children() {
        // The child keeps the output pipes open, like podman's re-executed self.
        let fake = FakePodman::new("sleep 30 & echo $! > $DIR/child; wait");
        let start = Instant::now();
        let e = run_bounded(&fake.cmd(), &["ps"], start + Duration::from_millis(300)).unwrap_err();
        let took = start.elapsed();
        assert!(matches!(e, DiscoverError::TimedOut(_)), "{e:?}");
        assert!(e.to_string().contains("did not answer in time"), "{e}");
        assert!(took < Duration::from_millis(1500), "took {took:?}");

        let child: i32 = fake
            .read("child")
            .trim()
            .parse()
            .expect("script wrote its child's pid");
        // SIGKILL delivery is asynchronous; give the kernel a moment.
        let settle = Instant::now() + Duration::from_secs(2);
        while alive(child) && Instant::now() < settle {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!alive(child), "podman's child {child} survived");
    }

    /// A process left behind in another session holding stdout open does not
    /// turn a finished podman into a timeout: its output is used shortly after
    /// it exits.
    #[test]
    fn bounded_run_does_not_wait_for_a_detached_pipe_holder() {
        if std::process::Command::new("setsid")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("setsid(1) not available; skipping");
            return;
        }
        let fake = FakePodman::new("setsid sleep 3 & echo $! > $DIR/holder; echo '[]'; exit 0");
        let start = Instant::now();
        let o = run_bounded(&fake.cmd(), &["ps"], start + Duration::from_secs(10)).unwrap();
        let took = start.elapsed();
        let holder: i32 = fake.read("holder").trim().parse().unwrap_or(0);
        if holder > 0 {
            // SAFETY: signalling a PID our own script just started.
            unsafe { libc::kill(holder, libc::SIGKILL) };
        }
        assert!(o.success);
        assert_eq!(o.stdout, "[]\n");
        assert!(took < Duration::from_secs(2), "took {took:?}");
    }

    /// A podman that is not installed is a clear error, not a panic.
    #[test]
    fn bounded_run_reports_a_missing_program() {
        let e = run_bounded(
            &["/nonexistent/podman"],
            &["ps"],
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(e, DiscoverError::Failed(_)), "{e:?}");
        assert!(e.to_string().contains("cannot run"), "{e}");
    }

    /// A slow `ps` fails discovery quickly with a message naming the cause.
    #[test]
    fn discovery_gives_up_on_a_slow_ps() {
        let fake = FakePodman::new("sleep 30");
        let start = Instant::now();
        let e = discover_with(&fake.cmd(), start + Duration::from_millis(200)).unwrap_err();
        assert!(matches!(e, DiscoverError::TimedOut(_)), "{e:?}");
        assert!(start.elapsed() < Duration::from_millis(1500));
    }

    /// When `ps` lists every PID, `inspect` is never run.
    #[test]
    fn discovery_uses_ps_pids_without_inspect() {
        let fake = FakePodman::new(
            r#"echo "$1" >> $DIR/calls
case "$1" in
ps) echo '[{"Id":"a","Names":["opencode-a"],"Created":1,"Pid":11}]' ;;
*) exit 99 ;;
esac"#,
        );
        let got = discover_with(&fake.cmd(), Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].pid, Some(11));
        assert_eq!(fake.read("calls"), "ps\n");
    }

    /// Without PIDs in `ps`, one batched `inspect` supplies them.
    #[test]
    fn discovery_falls_back_to_inspect() {
        let fake = FakePodman::new(
            r#"case "$1" in
ps) echo '[{"Id":"a","Names":["opencode-a"],"Created":1},{"Id":"b","Names":["opencode-b"],"Created":2}]' ;;
inspect) echo "/opencode-a 11"; echo "/opencode-b 22" ;;
esac"#,
        );
        let got = discover_with(&fake.cmd(), Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(
            got.iter().map(|c| c.pid).collect::<Vec<_>>(),
            vec![Some(11), Some(22)]
        );
    }

    /// The other regression: a container removed between `ps` and `inspect`
    /// makes `inspect` exit 125 while still printing the rest. Only the vanished
    /// container is dropped; the run does not fail.
    #[test]
    fn discovery_survives_a_container_vanishing_before_inspect() {
        let fake = FakePodman::new(
            r#"case "$1" in
ps) echo '[{"Id":"a","Names":["opencode-a"],"Created":1},{"Id":"b","Names":["opencode-b"],"Created":2}]' ;;
inspect) echo "/opencode-b 22"; echo 'Error: no such object: "a"' >&2; exit 125 ;;
esac"#,
        );
        let got = discover_with(&fake.cmd(), Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "opencode-b");
        assert_eq!(got[0].slot, 2, "survivors keep their slots");
    }

    /// An `inspect` that fails with no usable output is reported.
    #[test]
    fn discovery_reports_a_failed_inspect() {
        let fake = FakePodman::new(
            r#"case "$1" in
ps) echo '[{"Id":"a","Names":["opencode-a"],"Created":1}]' ;;
inspect) echo 'Error: broken' >&2; exit 125 ;;
esac"#,
        );
        let e = discover_with(&fake.cmd(), Instant::now() + Duration::from_secs(5)).unwrap_err();
        assert!(matches!(e, DiscoverError::Failed(_)), "{e:?}");
        let e = e.to_string();
        assert!(e.contains("inspect failed") && e.contains("broken"), "{e}");
    }

    /// A failing `ps` is reported with podman's own message.
    #[test]
    fn discovery_reports_a_failed_ps() {
        let fake = FakePodman::new("echo 'Error: nope' >&2; exit 125");
        let e = discover_with(&fake.cmd(), Instant::now() + Duration::from_secs(5)).unwrap_err();
        assert!(matches!(e, DiscoverError::Failed(_)), "{e:?}");
        assert!(e.to_string().contains("nope"), "{e}");
    }

    /// The container's `HOSTNAME` environment variable wins over its system
    /// hostname, and both are trimmed - the plugin's own precedence.
    #[test]
    fn resolve_host_prefers_the_environment() {
        let env = vec!["PATH=/bin".to_string(), "HOSTNAME=from-env".to_string()];
        assert_eq!(resolve_host(Some("system"), &env), "from-env");
        assert_eq!(resolve_host(Some("system"), &[]), "system");
        assert_eq!(resolve_host(Some(" system "), &[]), "system");
        assert_eq!(resolve_host(None, &[]), "");
    }

    /// A blank `HOSTNAME` (or a blank system hostname) falls back rather than
    /// producing an empty title.
    #[test]
    fn resolve_host_ignores_blank_values() {
        let env = vec!["HOSTNAME=   ".to_string()];
        assert_eq!(resolve_host(Some("system"), &env), "system");
        assert_eq!(resolve_host(Some(""), &env), "");
        assert_eq!(resolve_host(Some("  "), &[]), "");
        assert_eq!(resolve_host(None, &["HOSTNAME=".to_string()]), "");
    }

    /// `inspect` output in the shape podman actually prints is parsed into a
    /// hostname, including the `null` hostname and non-string env entries.
    #[test]
    fn parses_inspect_hostname_output() {
        assert_eq!(
            parse_hostname(r#""syshost" ["HOSTNAME=envhost","PATH=/bin"]"#).unwrap(),
            "envhost"
        );
        assert_eq!(
            parse_hostname(r#""syshost" ["PATH=/bin"]"#).unwrap(),
            "syshost"
        );
        assert_eq!(parse_hostname(r#"null ["PATH=/bin"]"#).unwrap(), "");
        // A second value per line, as podman may wrap, is still parsed.
        assert_eq!(
            parse_hostname("\"syshost\" [\n  \"PATH=/bin\"\n]\n").unwrap(),
            "syshost"
        );
        // A non-string env entry is ignored, not fatal: only HOSTNAME matters.
        assert_eq!(parse_hostname(r#""syshost" ["ok", 5]"#).unwrap(), "syshost");
    }

    /// Malformed or unexpectedly shaped `inspect` output is an error, not an
    /// empty hostname.
    #[test]
    fn rejects_bad_inspect_hostname_output() {
        assert!(parse_hostname("").is_err());
        assert!(parse_hostname(r#""only-hostname""#).is_err());
        assert!(parse_hostname(r#"42 ["HOSTNAME=a"]"#).is_err());
        assert!(parse_hostname(r#""h" "not-an-array""#).is_err());
    }

    /// `hostname` runs exactly one `inspect` with the two JSON fields and
    /// returns the resolved value.
    #[test]
    fn hostname_reads_the_container() {
        let fake = FakePodman::new(
            r#"echo "$1 $3" >> $DIR/calls
echo '"syshost" ["HOSTNAME=envhost"]'"#,
        );
        let got = hostname(
            &fake.cmd(),
            "opencode-web",
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(got, "envhost");
        let calls = fake.read("calls");
        assert!(calls.contains("inspect"), "{calls}");
        assert!(calls.contains("{{json .Config.Hostname}}"), "{calls}");
    }

    /// A failing `inspect` is reported with the container it was for.
    #[test]
    fn hostname_reports_a_failed_inspect() {
        let fake = FakePodman::new("echo 'Error: gone' >&2; exit 125");
        let e = hostname(
            &fake.cmd(),
            "opencode-web",
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(matches!(e, DiscoverError::Failed(_)), "{e:?}");
        let e = e.to_string();
        assert!(e.contains("opencode-web") && e.contains("gone"), "{e}");
    }
}
