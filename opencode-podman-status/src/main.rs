//! opencode-podman-status: report what every opencode container is doing.
//!
//! See README.markdown for usage and NOTES.md for why it works the way it does.

mod auth;
mod discover;
mod http;
mod ident;
mod ns_socket;
mod probe;
mod render;
mod sockets;
mod status;

use std::process::ExitCode;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discover::{Container, DiscoverError};
use probe::{Report, Source};
use sockets::Discovery;
use status::{Counts, State};

/// Program name used in messages.
const PROGRAM: &str = "opencode-podman-status";

/// Default time budget for probing one container.
///
/// DAK re-invokes this program on a timer, so a wedged container must never hold
/// it up. This is a hard deadline for *everything* one container costs -
/// entering its namespace and every request and response - not a per-read
/// timeout, so a server that drips bytes cannot stretch it. Measured against
/// opencode 1.18.32: a warm request costs 1.5-5 ms, and the first
/// `/session/status` after startup around 360 ms - but an instance probed the
/// moment `/global/health` starts answering can be slower still, which an
/// earlier 1500 ms default was observed to trip over. 2500 ms leaves a wide
/// margin while staying under any sensible DAK refresh interval; containers are
/// probed in parallel, so this also bounds the whole run.
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2500);

/// Hard limit on one whole run, podman included.
///
/// DAK kills a `text_exec` that has not finished after 5 s and draws "Error" on
/// its button (`EXEC_TIMEOUT` in DAK's `src/actions.rs`), so every run must end
/// well before that. Everything below is fitted inside this: podman discovery,
/// then the probes, whose `--timeout` is cut short if less than that remains.
/// The last second is left for process start-up, output and scheduling slack.
const RUN_BUDGET: Duration = Duration::from_millis(4000);

/// Part of [`RUN_BUDGET`] that podman discovery may not eat into.
///
/// podman can stall for seconds behind a container lock while a container is
/// being stopped or removed. Discovery gives up this long before the run budget
/// ends, so probing always has at least this much left.
const PROBE_RESERVE: Duration = Duration::from_millis(1000);

/// Most bytes read from one container across all its responses.
///
/// A handful of requests of a few KiB each is normal; this only stops a hostile
/// server making a nine-container run cost gigabytes.
const BYTE_BUDGET: usize = 4 * http::MAX_RESPONSE;

/// Sockets fetched from each container's namespace, which is also the most
/// requests one container can cost: health, three status endpoints, up to four
/// busy sessions' latest messages or the session list, with room for a second
/// candidate port.
const SOCKETS_PER_CONTAINER: usize = 12;

/// What the user asked for.
#[derive(Debug)]
enum Mode {
    /// Aggregate counts across all containers.
    Counts,
    /// Detail for one container, identified by slot number or name.
    Instance(String),
    /// Human-readable diagnostic table.
    List,
    /// Probe one process's namespaces directly, bypassing podman discovery.
    ///
    /// A diagnostic: it answers "can this container be reached at all, and what
    /// does it say" without depending on podman listing it. Also what the
    /// namespace-entry integration test drives.
    Pid(i32),
    /// Usage text.
    Help,
    /// Version string.
    Version,
}

/// Where the server password comes from.
#[derive(Clone, PartialEq, Eq)]
enum PasswordSource {
    /// Given directly with `--password` (visible in `ps`).
    Argument(String),
    /// Read from the file named with `--password-file`.
    File(String),
}

impl std::fmt::Debug for PasswordSource {
    /// Never shows a command-line password; a file path is harmless to show.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasswordSource::Argument(_) => f.write_str("Argument(<redacted>)"),
            PasswordSource::File(path) => f.debug_tuple("File").field(path).finish(),
        }
    }
}

/// Parsed command line.
#[derive(Debug)]
struct Options {
    mode: Mode,
    /// Skip port discovery and use this port.
    port: Option<u16>,
    /// Which kind of server to take the state from.
    source: Source,
    /// Port the status plugin listens on inside each container.
    plugin_port: u16,
    timeout: Duration,
    /// Password for opencode's server, if it was started with one.
    password: Option<PasswordSource>,
    /// Username to go with the password; `opencode` if not given.
    username: Option<String>,
    /// The resolved credentials, filled in by [`resolve_credentials`].
    credentials: Option<auth::Credentials>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Counts,
            port: None,
            source: Source::Auto,
            plugin_port: probe::DEFAULT_PLUGIN_PORT,
            timeout: DEFAULT_TIMEOUT,
            password: None,
            username: None,
            credentials: None,
        }
    }
}

/// Usage text, also the answer to `--help`.
const USAGE: &str = "\
Usage: opencode-podman-status [options]

Reports what each opencode instance running in a rootless podman container is
doing. With no options, prints three six-character lines for a DAK button:

    run: 3      instances working
    wait:1      instances waiting for you: a question, a permission
                prompt, or (status plugin only) a failed turn
    done:5      instances idle

Options:
  --instance <slot|name>  Detail for one container: name, state (run, wait,
                          done, or Error), time in state. Prints nothing at
                          all if that slot does not exist.
  --list                  Diagnostic table of every container (not for DAK).
  --pid <n>               Diagnostic: probe this process's namespaces directly,
                          bypassing podman, and print the raw JSON report.
  --source <auto|plugin|api>
                          Take the state from the status plugin, from
                          opencode's own API, or (auto, the default) from the
                          plugin when present and the API otherwise.
  --plugin-port <n>       Port the status plugin listens on (default 4097;
                          OPENCODE_STATUS_PORT in the container changes it).
  --port <n>              Use this port instead of discovering it.
  --timeout <ms>          Time budget per container, all requests included
                          (default 2500). Every run also ends within 4 s in
                          total, podman included, to stay inside DAK's 5 s.
  --password-file <path>  Password for opencode servers started with
                          OPENCODE_SERVER_PASSWORD; one for all containers.
                          The file should be mode 600.
  --password <pw>         The same, given directly. Visible to every local
                          user via ps(1) - prefer --password-file.
  --username <name>       Username for the above (default opencode).
  -h, --help              This text.
  -V, --version           Version.

Recommended: enable the status plugin in each container's opencode.json and
run opencode WITHOUT --port. `opencode --port` exposes opencode's full
remote-control API, through which anything reaching the port - including the
agent's own tools in the container - can approve its own permission prompts
and run commands without any human check. See README.markdown.

Linux only: it works by creating sockets in rootless podman containers'
network namespaces.
";

/// Parses arguments, hand-rolled to avoid a dependency for six flags.
fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;

    // Takes the value belonging to a flag, or reports the flag as incomplete.
    fn value<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str, String> {
        *index += 1;
        args.get(*index)
            .map(|s| s.as_str())
            .ok_or_else(|| format!("{flag} needs a value"))
    }

    while index < args.len() {
        match args[index].as_str() {
            "--instance" => {
                let v = value(args, &mut index, "--instance")?.to_string();
                options.mode = Mode::Instance(v);
            }
            "--list" => options.mode = Mode::List,
            "--pid" => {
                let v = value(args, &mut index, "--pid")?;
                let pid: i32 = v.parse().map_err(|_| format!("invalid pid: {v}"))?;
                if pid <= 0 {
                    return Err(format!("invalid pid: {v}"));
                }
                options.mode = Mode::Pid(pid);
            }
            "--source" => {
                let v = value(args, &mut index, "--source")?;
                options.source = Source::parse(v).ok_or_else(|| format!("invalid source: {v}"))?;
            }
            "--plugin-port" => {
                let v = value(args, &mut index, "--plugin-port")?;
                options.plugin_port = match v.parse() {
                    Ok(p) if p != 0 => p,
                    _ => return Err(format!("invalid plugin port: {v}")),
                };
            }
            "--port" => {
                let v = value(args, &mut index, "--port")?;
                options.port = Some(v.parse().map_err(|_| format!("invalid port: {v}"))?);
            }
            "--timeout" => {
                let v = value(args, &mut index, "--timeout")?;
                let ms: u64 = v.parse().map_err(|_| format!("invalid timeout: {v}"))?;
                options.timeout = Duration::from_millis(ms);
            }
            "--password" => {
                let v = value(args, &mut index, "--password")?;
                set_password(&mut options, PasswordSource::Argument(v.to_string()))?;
            }
            "--password-file" => {
                let v = value(args, &mut index, "--password-file")?;
                set_password(&mut options, PasswordSource::File(v.to_string()))?;
            }
            "--username" => {
                options.username = Some(value(args, &mut index, "--username")?.to_string());
            }
            "-h" | "--help" => options.mode = Mode::Help,
            "-V" | "--version" => options.mode = Mode::Version,
            other => return Err(format!("unknown argument: {other}")),
        }
        index += 1;
    }
    if options.username.is_some() && options.password.is_none() {
        return Err("--username needs --password or --password-file".to_string());
    }
    Ok(options)
}

/// Records the password source, refusing a second one.
fn set_password(options: &mut Options, source: PasswordSource) -> Result<(), String> {
    if options.password.is_some() {
        return Err("give only one of --password and --password-file, once".to_string());
    }
    options.password = Some(source);
    Ok(())
}

/// Turns the password options into credentials, reading the file if one was named.
///
/// Returns any warning (a password file others can read) for the caller to show.
fn resolve_credentials(options: &mut Options) -> Result<Option<String>, String> {
    let (password, warning) = match &options.password {
        None => return Ok(None),
        Some(PasswordSource::Argument(pw)) => (pw.clone(), None),
        Some(PasswordSource::File(path)) => auth::read_password_file(path)?,
    };
    let username = options
        .username
        .as_deref()
        .unwrap_or(auth::DEFAULT_USERNAME);
    options.credentials = Some(auth::Credentials::new(username, &password)?);
    Ok(warning)
}

/// Current time in Unix milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Probes one container, entirely from outside it.
///
/// 1. Finds opencode's port by socket ownership, reading the container's
///    `/proc` tables from the host (unless `--port` names it).
/// 2. Has a short-lived child create sockets inside the container's network
///    namespace and hand them back (see [`ns_socket`]).
/// 3. Talks HTTP over those sockets from this process, within one deadline.
///
/// Everything happens before `deadline`. Returns a [`Report`] in every case,
/// including failure, so the caller always has something to render.
fn probe_container(container: &Container, options: &Options, deadline: Instant) -> Report {
    let pid = match container.pid {
        Some(pid) => pid,
        None => return Report::failed("container has no running process"),
    };

    if ns_socket::shares_our_namespace(pid) {
        return Report::failed("shares our network namespace (are we inside it?)");
    }

    let ports = match options.port {
        Some(port) => vec![port],
        None => match sockets::discover(pid) {
            Discovery::Found(ports) => {
                probe::order_candidates(&ports, options.plugin_port, options.source)
            }
            other => return Report::failed(other.reason()),
        },
    };

    let namespaces = match ns_socket::Namespaces::open(pid) {
        Ok(ns) => ns,
        Err(e) => {
            return Report::failed(format!(
                "cannot open namespaces of pid {pid}: {e} \
                 (does opencode run as root inside the container?)"
            ))
        }
    };
    let sockets = match namespaces.sockets(SOCKETS_PER_CONTAINER, deadline) {
        Ok(sockets) => sockets,
        Err(e) => return Report::failed(format!("cannot enter namespaces of pid {pid}: {e}")),
    };

    let mut client = http::Client::new(http::Pool(sockets), deadline, BYTE_BUDGET)
        .with_credentials(options.credentials.as_ref());
    probe::run(&mut client, &ports, options.source, now_ms())
}

/// The deadline for one container's probe: `--timeout` from now, but never
/// later than the end of the whole run.
fn probe_deadline(now: Instant, timeout: Duration, run_deadline: Instant) -> Instant {
    (now + timeout).min(run_deadline)
}

/// The deadline for podman discovery: early enough to leave [`PROBE_RESERVE`]
/// for the probes.
fn discovery_deadline(run_deadline: Instant) -> Instant {
    run_deadline
        .checked_sub(PROBE_RESERVE)
        .unwrap_or(run_deadline)
}

/// Probes every container, in parallel, and pairs each with its report.
///
/// One thread per container, so nine containers cost roughly one probe's
/// latency, not nine. Each thread's namespace work happens in a child it forks,
/// which starts single-threaded whatever the parent is - that is what `setns`
/// requires.
///
/// Each probe is bounded by its own deadline, but some of what it does - reading
/// `/proc` files of processes that are in the middle of exiting, for one - cannot
/// be interrupted. So this does not rely on the probes keeping to their
/// deadlines: it stops waiting at `run_deadline`, reports any probe still
/// running as failed, and leaves that thread behind (it dies when the process
/// exits). Results keep the order of `containers`.
fn probe_all(
    containers: &[Container],
    options: &Arc<Options>,
    run_deadline: Instant,
) -> Vec<(Container, Report)> {
    probe_all_with(containers, run_deadline, |container| {
        let options = Arc::clone(options);
        move || {
            let deadline = probe_deadline(Instant::now(), options.timeout, run_deadline);
            probe_container(&container, &options, deadline)
        }
    })
}

/// [`probe_all`], with the per-container probe supplied, so tests can make one hang.
///
/// `make_probe` is called once per container, on this thread, and returns the
/// work to run on that container's thread.
fn probe_all_with<F, P>(
    containers: &[Container],
    run_deadline: Instant,
    make_probe: F,
) -> Vec<(Container, Report)>
where
    F: Fn(Container) -> P,
    P: FnOnce() -> Report + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    for (index, container) in containers.iter().enumerate() {
        let probe = make_probe(container.clone());
        let tx = tx.clone();
        // A failure to spawn leaves that container's slot empty, reported below.
        let _ = std::thread::Builder::new().spawn(move || {
            let _ = tx.send((index, probe()));
        });
    }
    drop(tx);

    let mut reports: Vec<Option<Report>> = containers.iter().map(|_| None).collect();
    let mut outstanding = containers.len();
    while outstanding > 0 {
        let remaining = run_deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok((index, report)) => {
                reports[index] = Some(report);
                outstanding -= 1;
            }
            // Out of time, or every thread gone (one panicked or never started).
            Err(_) => break,
        }
    }

    containers
        .iter()
        .cloned()
        .zip(reports)
        .map(|(container, report)| {
            let report = report.unwrap_or_else(|| Report::failed("probe did not finish in time"));
            (container, report)
        })
        .collect()
}

/// Finds the container a `--instance` argument refers to.
///
/// Accepts a slot number, a full container name, or the shortened name as
/// displayed. Returns `None` if nothing matches, which the caller renders as no
/// output at all.
fn select_instance<'a>(
    results: &'a [(Container, Report)],
    wanted: &str,
) -> Option<&'a (Container, Report)> {
    if let Ok(slot) = wanted.parse::<usize>() {
        return results.iter().find(|(c, _)| c.slot == slot);
    }
    results.iter().find(|(c, _)| c.name == wanted).or_else(|| {
        results
            .iter()
            .find(|(c, _)| render::short_name(&c.name) == wanted)
    })
}

/// Renders the diagnostic table.
///
/// Names and reasons are passed through [`render::printable`]: a reason can quote
/// text a container's server sent back, and this goes to a terminal.
fn render_list(results: &[(Container, Report)], now: i64) -> String {
    let mut out = String::new();
    for (container, report) in results {
        let state = report.parsed_state().map_or("unknown", State::word);
        let age = match (report.parsed_state(), report.since_ms) {
            (Some(_), Some(since)) => render::hhmm((now - since) / 1000),
            _ => "--:--".to_string(),
        };
        let port = report
            .port
            .map_or_else(|| "-".to_string(), |p| p.to_string());
        let source = render::printable(report.source.as_deref().unwrap_or("-"));
        let note = render::printable(report.reason.as_deref().unwrap_or(""));
        out.push_str(&format!(
            "{:>2}  {:<24} {:<8} {:<6} pid={:<8} port={:<6} via={:<7} {}\n",
            container.slot,
            render::printable(&container.name),
            state,
            age,
            container.pid.unwrap_or(0),
            port,
            source,
            note
        ));
    }
    out
}

/// What a successful run prints.
#[derive(Debug, PartialEq, Eq)]
struct Printed {
    /// For stdout: what DAK puts on the button.
    stdout: String,
    /// For stderr, when something is worth saying despite success. DAK does not
    /// read stderr; this is for whoever runs the program by hand.
    note: Option<String>,
}

impl Printed {
    /// Output with nothing to add on stderr.
    fn plain(stdout: String) -> Self {
        Printed { stdout, note: None }
    }
}

/// Runs the requested mode, returning what to print.
fn run(options: Options) -> Result<Printed, String> {
    run_with(options, &["podman"])
}

/// [`run`], with the podman command given, so tests can substitute one.
///
/// When podman does not answer in time, the DAK-facing modes print placeholders
/// and succeed rather than fail: podman is then stuck behind a container lock
/// (a container is being removed), which clears by itself within seconds, and a
/// non-zero exit would make DAK draw "Error" on the button for all that time.
/// Any other podman failure, and every failure in `--list`, is still an error.
fn run_with(options: Options, podman: &[&str]) -> Result<Printed, String> {
    let now = now_ms();
    let run_deadline = Instant::now() + RUN_BUDGET;
    let options = Arc::new(options);
    let discover = || discover::discover_with(podman, discovery_deadline(run_deadline));
    // A podman timeout becomes the given placeholder output; anything else fails.
    let or_placeholder = |e: DiscoverError, placeholder: String| match e {
        DiscoverError::TimedOut(reason) => Ok(Printed {
            stdout: placeholder,
            note: Some(reason),
        }),
        DiscoverError::Failed(reason) => Err(reason),
    };

    match &options.mode {
        Mode::Help => Ok(Printed::plain(USAGE.to_string())),
        Mode::Version => Ok(Printed::plain(format!(
            "{PROGRAM} {}\n",
            env!("CARGO_PKG_VERSION")
        ))),

        Mode::Counts => {
            let containers = match discover() {
                Ok(containers) => containers,
                Err(e) => return or_placeholder(e, render::counts_unknown()),
            };
            let results = probe_all(&containers, &options, run_deadline);
            let counts = Counts::tally(results.iter().map(|(_, r)| r.parsed_state()));
            Ok(Printed::plain(render::counts(counts)))
        }

        Mode::Instance(wanted) => {
            let containers = match discover() {
                Ok(containers) => containers,
                Err(e) => return or_placeholder(e, render::instance_unknown()),
            };
            let results = probe_all(&containers, &options, run_deadline);
            let stdout = match select_instance(&results, wanted) {
                // A slot that does not exist prints nothing at all, so an unused
                // DAK button stays blank.
                None => String::new(),
                Some((container, report)) => {
                    render::instance(&container.name, report.parsed_state(), report.since_ms, now)
                }
            };
            Ok(Printed::plain(stdout))
        }

        Mode::List => {
            let containers = discover().map_err(|e| e.to_string())?;
            let results = probe_all(&containers, &options, run_deadline);
            Ok(Printed::plain(render_list(&results, now)))
        }

        Mode::Pid(pid) => {
            // A synthetic container: only the PID is real, which is all the
            // namespace-entry path needs.
            let container = Container {
                slot: 1,
                name: format!("pid-{pid}"),
                id: String::new(),
                created: 0,
                pid: Some(*pid),
            };
            let results = probe_all(std::slice::from_ref(&container), &options, run_deadline);
            let report = &results[0].1;
            let json = serde_json::to_string(&report)
                .map_err(|e| format!("cannot serialise report: {e}"))?;
            Ok(Printed::plain(format!("{json}\n")))
        }
    }
}

/// Entry point. Errors go to stderr with a non-zero exit; everything else prints
/// to stdout and exits 0, including the "nothing to report" cases.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut options = match parse_args(&args) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("{PROGRAM}: {e}");
            eprint!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match resolve_credentials(&mut options) {
        Ok(Some(warning)) => eprintln!("{PROGRAM}: {warning}"),
        Ok(None) => {}
        Err(e) => {
            eprintln!("{PROGRAM}: {e}");
            return ExitCode::FAILURE;
        }
    }

    match run(options) {
        Ok(output) => {
            if let Some(note) = &output.note {
                eprintln!("{PROGRAM}: {note}");
            }
            print!("{}", output.stdout);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{PROGRAM}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turns string literals into the argument vector shape `parse_args` takes.
    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// No arguments means the aggregate count view.
    #[test]
    fn defaults_to_counts_mode() {
        let options = parse_args(&args(&[])).unwrap();
        assert!(matches!(options.mode, Mode::Counts));
        assert_eq!(options.port, None);
        assert_eq!(options.timeout, DEFAULT_TIMEOUT);
    }

    /// `--instance` captures its argument, whether a slot or a name.
    #[test]
    fn parses_instance_mode() {
        match parse_args(&args(&["--instance", "3"])).unwrap().mode {
            Mode::Instance(v) => assert_eq!(v, "3"),
            other => panic!("expected instance mode, got {other:?}"),
        }
        match parse_args(&args(&["--instance", "opencode-web"]))
            .unwrap()
            .mode
        {
            Mode::Instance(v) => assert_eq!(v, "opencode-web"),
            other => panic!("expected instance mode, got {other:?}"),
        }
    }

    /// The remaining modes parse.
    #[test]
    fn parses_other_modes() {
        assert!(matches!(
            parse_args(&args(&["--list"])).unwrap().mode,
            Mode::List
        ));
        // The internal re-exec mode of earlier versions is gone for good.
        assert!(parse_args(&args(&["__probe"])).is_err());
        assert!(matches!(
            parse_args(&args(&["--help"])).unwrap().mode,
            Mode::Help
        ));
        assert!(matches!(
            parse_args(&args(&["-h"])).unwrap().mode,
            Mode::Help
        ));
        assert!(matches!(
            parse_args(&args(&["--version"])).unwrap().mode,
            Mode::Version
        ));
        assert!(matches!(
            parse_args(&args(&["-V"])).unwrap().mode,
            Mode::Version
        ));
    }

    /// The diagnostic `--pid` mode parses a positive PID.
    #[test]
    fn parses_pid_mode() {
        match parse_args(&args(&["--pid", "1234"])).unwrap().mode {
            Mode::Pid(pid) => assert_eq!(pid, 1234),
            other => panic!("expected pid mode, got {other:?}"),
        }
    }

    /// PIDs that cannot name a process are rejected rather than probed.
    #[test]
    fn rejects_invalid_pids() {
        assert!(parse_args(&args(&["--pid", "0"])).is_err());
        assert!(parse_args(&args(&["--pid", "-5"])).is_err());
        assert!(parse_args(&args(&["--pid", "abc"])).is_err());
        assert!(parse_args(&args(&["--pid"])).is_err());
    }

    /// Port and timeout are parsed as numbers.
    #[test]
    fn parses_port_and_timeout() {
        let options = parse_args(&args(&["--port", "4096", "--timeout", "250"])).unwrap();
        assert_eq!(options.port, Some(4096));
        assert_eq!(options.timeout, Duration::from_millis(250));
    }

    /// Bad values are rejected with a message rather than silently defaulted.
    #[test]
    fn rejects_bad_arguments() {
        assert!(parse_args(&args(&["--port", "not-a-port"])).is_err());
        assert!(parse_args(&args(&["--port", "99999999"])).is_err());
        assert!(parse_args(&args(&["--timeout", "soon"])).is_err());
        assert!(parse_args(&args(&["--port"])).is_err());
        assert!(parse_args(&args(&["--instance"])).is_err());
        assert!(parse_args(&args(&["--nonsense"])).is_err());
    }

    /// Either password option is accepted, with an optional username.
    #[test]
    fn parses_password_options() {
        let o = parse_args(&args(&["--password", "pw"])).unwrap();
        assert_eq!(o.password, Some(PasswordSource::Argument("pw".into())));
        let o = parse_args(&args(&["--password-file", "/p", "--username", "me"])).unwrap();
        assert_eq!(o.password, Some(PasswordSource::File("/p".into())));
        assert_eq!(o.username.as_deref(), Some("me"));
    }

    /// Conflicting or incomplete password options are refused.
    #[test]
    fn rejects_bad_password_options() {
        assert!(parse_args(&args(&["--password", "a", "--password-file", "/p"])).is_err());
        assert!(parse_args(&args(&["--password", "a", "--password", "b"])).is_err());
        assert!(parse_args(&args(&["--username", "me"])).is_err());
        assert!(parse_args(&args(&["--password"])).is_err());
        assert!(parse_args(&args(&["--password-file"])).is_err());
    }

    /// A command-line password never shows up in debug output of the options.
    #[test]
    fn options_debug_redacts_password() {
        let mut o = parse_args(&args(&["--password", "hunter2"])).unwrap();
        resolve_credentials(&mut o).unwrap();
        let shown = format!("{o:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    /// Credentials resolve with the default username, or the one given.
    #[test]
    fn resolves_credentials() {
        let mut o = parse_args(&args(&["--password", "pw"])).unwrap();
        assert_eq!(resolve_credentials(&mut o), Ok(None));
        assert_eq!(
            o.credentials,
            Some(auth::Credentials::new("opencode", "pw").unwrap())
        );

        let mut o = parse_args(&args(&["--password", "pw", "--username", "me"])).unwrap();
        resolve_credentials(&mut o).unwrap();
        assert_eq!(
            o.credentials,
            Some(auth::Credentials::new("me", "pw").unwrap())
        );

        let mut o = parse_args(&args(&[])).unwrap();
        assert_eq!(resolve_credentials(&mut o), Ok(None));
        assert_eq!(o.credentials, None);

        let mut o = parse_args(&args(&["--password", ""])).unwrap();
        assert!(resolve_credentials(&mut o).is_err());
        let mut o = parse_args(&args(&["--password-file", "/nonexistent/pw"])).unwrap();
        assert!(resolve_credentials(&mut o).is_err());
    }

    /// Builds a container/report pair for selection tests.
    fn pair(slot: usize, name: &str, state: Option<&str>) -> (Container, Report) {
        (
            Container {
                slot,
                name: name.to_string(),
                id: format!("id{slot}"),
                created: slot as i64,
                pid: Some(1000 + slot as i32),
            },
            Report {
                port: Some(4096),
                source: Some("plugin".to_string()),
                state: state.map(|s| s.to_string()),
                since_ms: Some(1790308945355),
                reason: None,
            },
        )
    }

    /// A slot number selects the matching container.
    #[test]
    fn selects_instance_by_slot() {
        let results = vec![
            pair(1, "opencode-web", Some("run")),
            pair(2, "opencode-api", Some("done")),
        ];
        let found = select_instance(&results, "2").expect("should find slot 2");
        assert_eq!(found.0.name, "opencode-api");
    }

    /// A full container name selects it too.
    #[test]
    fn selects_instance_by_full_name() {
        let results = vec![pair(1, "opencode-web", Some("run"))];
        assert_eq!(select_instance(&results, "opencode-web").unwrap().0.slot, 1);
    }

    /// So does the shortened, displayed name.
    #[test]
    fn selects_instance_by_short_name() {
        let results = vec![pair(1, "opencode-web", Some("run"))];
        assert_eq!(select_instance(&results, "web").unwrap().0.slot, 1);
    }

    /// A slot that does not exist selects nothing, which the caller turns into
    /// empty output so unused buttons stay blank.
    #[test]
    fn selects_nothing_for_unknown_instance() {
        let results = vec![pair(1, "opencode-web", Some("run"))];
        assert!(select_instance(&results, "7").is_none());
        assert!(select_instance(&results, "opencode-nope").is_none());
        assert!(select_instance(&results, "").is_none());
    }

    /// Selecting from no containers at all finds nothing rather than panicking.
    #[test]
    fn selects_nothing_when_no_containers() {
        assert!(select_instance(&[], "1").is_none());
    }

    /// The diagnostic table has one line per container and names the problem for
    /// an unreachable one.
    #[test]
    fn list_reports_one_line_per_container() {
        let now = 1790308945355;
        let mut results = vec![pair(1, "opencode-web", Some("run"))];
        let mut broken = pair(2, "opencode-api", None);
        broken.1 = Report::failed("no listening socket");
        results.push(broken);

        let out = render_list(&results, now);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("opencode-web"));
        assert!(lines[0].contains("run"));
        assert!(lines[1].contains("opencode-api"));
        assert!(lines[1].contains("unknown"));
        assert!(lines[1].contains("no listening socket"));
    }

    /// Escape sequences in a name or reason cannot reach the terminal.
    #[test]
    fn list_neutralises_control_characters() {
        let mut row = pair(1, "opencode-\u{1b}[2J", None);
        row.1 = Report::failed("bad \u{1b}]0;title\u{7}");
        let out = render_list(&[row], 1790308945355);
        assert!(!out.contains('\u{1b}') && !out.contains('\u{7}'), "{out:?}");
    }

    /// An empty table is empty output, not a header with nothing under it.
    #[test]
    fn list_of_nothing_is_empty() {
        assert_eq!(render_list(&[], 1790308945355), "");
    }

    /// `--help` and `--version` need no podman and produce output.
    #[test]
    fn help_and_version_need_no_containers() {
        let help = run(Options {
            mode: Mode::Help,
            ..Default::default()
        })
        .unwrap()
        .stdout;
        assert!(help.contains("Usage:"));
        assert!(help.contains("--instance"));
        let version = run(Options {
            mode: Mode::Version,
            ..Default::default()
        })
        .unwrap()
        .stdout;
        assert!(version.contains(env!("CARGO_PKG_VERSION")));
        assert!(version.contains(PROGRAM));
    }

    /// The usage text recommends the plugin and warns what `--port` gives away.
    #[test]
    fn usage_warns_about_port_and_recommends_plugin() {
        assert!(USAGE.contains("status plugin"));
        assert!(USAGE.contains("WITHOUT --port"));
        assert!(USAGE.contains("without any human check"));
    }

    /// `--source` and `--plugin-port` parse, defaulting to auto and 4097.
    #[test]
    fn parses_source_options() {
        let o = parse_args(&args(&[])).unwrap();
        assert_eq!(o.source, Source::Auto);
        assert_eq!(o.plugin_port, 4097);
        let o = parse_args(&args(&["--source", "api", "--plugin-port", "5000"])).unwrap();
        assert_eq!(o.source, Source::Api);
        assert_eq!(o.plugin_port, 5000);
        assert!(parse_args(&args(&["--source", "both"])).is_err());
        assert!(parse_args(&args(&["--plugin-port", "0"])).is_err());
        assert!(parse_args(&args(&["--plugin-port", "x"])).is_err());
        assert!(parse_args(&args(&["--source"])).is_err());
    }

    /// An errored instance is listed as `error`, with what answered.
    #[test]
    fn list_shows_error_and_source() {
        let out = render_list(&[pair(1, "opencode-web", Some("error"))], 1790308945355);
        assert!(out.contains(" error "), "{out}");
        assert!(out.contains("via=plugin"), "{out}");
    }

    /// A probe gets its full `--timeout` when the run has time for it, and is
    /// cut short at the end of the run otherwise.
    #[test]
    fn probe_deadline_is_capped_by_the_run() {
        let now = Instant::now();
        let run_end = now + Duration::from_millis(4000);
        assert_eq!(
            probe_deadline(now, Duration::from_millis(2500), run_end),
            now + Duration::from_millis(2500)
        );
        assert_eq!(
            probe_deadline(now, Duration::from_secs(60), run_end),
            run_end
        );
        // Discovery ran long: only what is left of the run remains.
        let late = now + Duration::from_millis(3500);
        assert_eq!(
            probe_deadline(late, Duration::from_millis(2500), run_end),
            run_end
        );
    }

    /// Discovery always leaves the probe reserve, and the whole budget stays
    /// clear of DAK's 5 s kill.
    #[test]
    fn budget_fits_inside_dak_exec_timeout() {
        let now = Instant::now();
        let run_end = now + RUN_BUDGET;
        assert_eq!(discovery_deadline(run_end), run_end - PROBE_RESERVE);
        assert!(RUN_BUDGET + Duration::from_millis(500) <= Duration::from_secs(5));
        assert!(PROBE_RESERVE < RUN_BUDGET);
    }

    /// Builds a bare container for the probe-scheduling tests.
    fn bare(slot: usize) -> Container {
        Container {
            slot,
            name: format!("opencode-{slot}"),
            id: format!("id{slot}"),
            created: slot as i64,
            pid: Some(1),
        }
    }

    /// A report that says which container produced it, via its port field.
    fn marked(slot: usize) -> Report {
        Report {
            port: Some(slot as u16),
            source: None,
            state: Some("done".into()),
            since_ms: None,
            reason: None,
        }
    }

    /// Results come back in container order, whatever order the probes finish in.
    #[test]
    fn probe_all_keeps_container_order() {
        let containers: Vec<Container> = (1..=4).map(bare).collect();
        let results = probe_all_with(&containers, Instant::now() + Duration::from_secs(5), |c| {
            move || {
                // Later slots finish first.
                std::thread::sleep(Duration::from_millis(40 * (5 - c.slot as u64)));
                marked(c.slot)
            }
        });
        let slots: Vec<usize> = results.iter().map(|(c, _)| c.slot).collect();
        assert_eq!(slots, vec![1, 2, 3, 4]);
        for (c, r) in &results {
            assert_eq!(r.port, Some(c.slot as u16));
        }
    }

    /// A probe stuck somewhere its own deadline cannot reach does not hold the
    /// run: at the run deadline it is reported as failed and the rest are kept.
    #[test]
    fn probe_all_abandons_a_hung_probe_at_the_run_deadline() {
        let containers = vec![bare(1), bare(2)];
        let start = Instant::now();
        let results = probe_all_with(&containers, start + Duration::from_millis(300), |c| {
            move || {
                if c.slot == 1 {
                    std::thread::sleep(Duration::from_secs(30));
                }
                marked(c.slot)
            }
        });
        assert!(
            start.elapsed() < Duration::from_millis(1500),
            "took {:?}",
            start.elapsed()
        );
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].1.reason.as_deref(),
            Some("probe did not finish in time")
        );
        assert_eq!(results[1].1.port, Some(2));
    }

    /// A probe that panics is reported as failed rather than taking the run down
    /// or making it wait for the deadline.
    #[test]
    fn probe_all_survives_a_panicking_probe() {
        let containers = vec![bare(1), bare(2)];
        let start = Instant::now();
        let results = probe_all_with(&containers, start + Duration::from_secs(10), |c| {
            move || {
                if c.slot == 2 {
                    panic!("deliberate test panic");
                }
                marked(c.slot)
            }
        });
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(results[0].1.port, Some(1));
        assert!(results[1].1.reason.is_some());
    }

    /// No containers means no threads and no waiting.
    #[test]
    fn probe_all_of_nothing_returns_at_once() {
        let start = Instant::now();
        let results = probe_all_with(&[], start + Duration::from_secs(10), |_| || marked(0));
        assert!(results.is_empty());
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    /// A throwaway `/bin/sh` script standing in for podman, removed on drop.
    struct FakePodman {
        dir: std::path::PathBuf,
        script: String,
    }

    impl FakePodman {
        /// Writes `body` as the script.
        fn new(body: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "opencode-podman-status-main-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let script = dir.join("podman");
            std::fs::write(&script, format!("{body}\n")).unwrap();
            let script = script.to_str().unwrap().to_string();
            FakePodman { dir, script }
        }

        /// The command running it: read by `/bin/sh`, never exec'd, which
        /// avoids `ETXTBSY` from concurrent test forks.
        fn cmd(&self) -> [&str; 2] {
            ["/bin/sh", &self.script]
        }
    }

    impl Drop for FakePodman {
        /// Removes the scratch directory.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Options for a given mode, everything else default.
    fn mode(mode: Mode) -> Options {
        Options {
            mode,
            ..Default::default()
        }
    }

    /// The reported bug: podman stuck behind a container being removed. The
    /// summary shows dashes and succeeds - so DAK draws no "Error" - well before
    /// DAK's 5 s kill, and says why on stderr.
    #[test]
    fn stuck_podman_gives_placeholder_counts() {
        let fake = FakePodman::new("sleep 30");
        let start = Instant::now();
        let out = run_with(mode(Mode::Counts), &fake.cmd()).unwrap();
        let took = start.elapsed();
        assert_eq!(out.stdout, "run: -\nwait:-\ndone:-\n");
        assert!(out.note.unwrap().contains("did not answer in time"));
        assert!(
            took >= RUN_BUDGET - PROBE_RESERVE - Duration::from_millis(100),
            "took {took:?}"
        );
        assert!(took < RUN_BUDGET, "took {took:?}");
    }

    /// The same for a detail button: placeholders, success.
    #[test]
    fn stuck_podman_gives_placeholder_instance() {
        let fake = FakePodman::new("sleep 30");
        let out = run_with(mode(Mode::Instance("1".into())), &fake.cmd()).unwrap();
        assert_eq!(out.stdout, "------\n????\n--:--\n");
        assert!(out.note.is_some());
    }

    /// `--list` is a diagnostic, so a stuck podman is reported as the error it is.
    #[test]
    fn stuck_podman_is_an_error_in_list() {
        let fake = FakePodman::new("sleep 30");
        let e = run_with(mode(Mode::List), &fake.cmd()).unwrap_err();
        assert!(e.contains("did not answer in time"), "{e}");
    }

    /// podman failing outright is a real problem and stays an error everywhere,
    /// so a broken setup shows up as "Error" on the button.
    #[test]
    fn failing_podman_is_still_an_error() {
        let fake = FakePodman::new("echo 'Error: broken' >&2; exit 125");
        for m in [Mode::Counts, Mode::Instance("1".into()), Mode::List] {
            let e = run_with(mode(m), &fake.cmd()).unwrap_err();
            assert!(e.contains("broken"), "{e}");
        }
        let e = run_with(mode(Mode::Counts), &["/nonexistent/podman"]).unwrap_err();
        assert!(e.contains("cannot run"), "{e}");
    }

    /// A podman that answers normally, with no opencode containers, gives zero
    /// counts and a blank detail button, with nothing on stderr.
    #[test]
    fn answering_podman_gives_real_output() {
        let fake = FakePodman::new("echo '[]'");
        let out = run_with(mode(Mode::Counts), &fake.cmd()).unwrap();
        assert_eq!(out, Printed::plain("run: 0\nwait:0\ndone:0\n".to_string()));
        let out = run_with(mode(Mode::Instance("1".into())), &fake.cmd()).unwrap();
        assert_eq!(out, Printed::plain(String::new()));
    }
}
