//! opencode-podman-status: report what every opencode container is doing.
//!
//! See README.markdown for usage and NOTES.md for why it works the way it does.

mod discover;
mod http;
mod ident;
mod nsenter;
mod probe;
mod render;
mod sockets;
mod status;

use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use discover::Container;
use probe::Report;
use status::{Counts, State};

/// Program name used in messages.
const PROGRAM: &str = "opencode-podman-status";

/// Argument that puts the program into its in-namespace probe mode.
///
/// Double-underscored because it is internal: the parent re-executes itself with
/// it after entering a container's namespaces. Not documented for users.
const PROBE_ARG: &str = "__probe";

/// Default per-request timeout.
///
/// DAK re-invokes this program on a timer, so a wedged container must never hold
/// it up. Measured against opencode 1.18.32: a warm request costs 1.5-5 ms, and
/// the first `/session/status` after startup around 360 ms - but an instance
/// probed the moment `/global/health` starts answering can be slower still, which
/// an earlier 1500 ms default was observed to trip over. 2500 ms leaves a wide
/// margin while staying under any sensible DAK refresh interval; probes run in
/// parallel, so this bounds the whole run, not each container.
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2500);

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
    /// In-namespace probe of a single instance.
    Probe,
    /// Usage text.
    Help,
    /// Version string.
    Version,
}

/// Parsed command line.
#[derive(Debug)]
struct Options {
    mode: Mode,
    /// Skip port discovery and use this port.
    port: Option<u16>,
    timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options { mode: Mode::Counts, port: None, timeout: DEFAULT_TIMEOUT }
    }
}

/// Usage text, also the answer to `--help`.
const USAGE: &str = "\
Usage: opencode-podman-status [options]

Reports what each opencode instance running in a rootless podman container is
doing. With no options, prints three six-character lines for a DAK button:

    run: 3      instances working
    wait:1      instances waiting for an answer from you
    done:5      instances idle

Options:
  --instance <slot|name>  Detail for one container: name, state, time in state.
                          Prints nothing at all if that slot does not exist.
  --list                  Diagnostic table of every container (not for DAK).
  --pid <n>               Diagnostic: probe this process's namespaces directly,
                          bypassing podman, and print the raw JSON report.
  --port <n>              Use this port instead of discovering it.
  --timeout <ms>          Per-request timeout (default 2500).
  -h, --help              This text.
  -V, --version           Version.

Each container must run opencode with an explicit --port, for example
`opencode --port 4096`; the same port in every container is fine. Setting
server.port in opencode.json does NOT work - see README.markdown.

Linux only: it works by entering a rootless podman container's namespaces.
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
            PROBE_ARG => options.mode = Mode::Probe,
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
            "--port" => {
                let v = value(args, &mut index, "--port")?;
                options.port = Some(v.parse().map_err(|_| format!("invalid port: {v}"))?);
            }
            "--timeout" => {
                let v = value(args, &mut index, "--timeout")?;
                let ms: u64 = v.parse().map_err(|_| format!("invalid timeout: {v}"))?;
                options.timeout = Duration::from_millis(ms);
            }
            "-h" | "--help" => options.mode = Mode::Help,
            "-V" | "--version" => options.mode = Mode::Version,
            other => return Err(format!("unknown argument: {other}")),
        }
        index += 1;
    }
    Ok(options)
}

/// Current time in Unix milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Probes one container by entering its namespaces and re-executing ourselves.
///
/// Returns a [`Report`] in every case, including failure, so the caller always has
/// something to render.
fn probe_container(container: &Container, options: &Options) -> Report {
    let pid = match container.pid {
        Some(pid) => pid,
        None => return Report::failed("container has no running process"),
    };

    if nsenter::shares_our_namespace(pid) {
        return Report::failed("shares our network namespace (are we inside it?)");
    }

    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => return Report::failed(format!("cannot find own executable: {e}")),
    };

    let mut command = match nsenter::command_in_namespaces(pid, &exe.to_string_lossy()) {
        Ok(command) => command,
        Err(e) => {
            return Report::failed(format!(
                "cannot open namespaces of pid {pid}: {e} \
                 (does opencode run as root inside the container?)"
            ))
        }
    };

    command.arg(PROBE_ARG);
    command.arg("--timeout").arg(options.timeout.as_millis().to_string());
    if let Some(port) = options.port {
        command.arg("--port").arg(port.to_string());
    }

    let output = match command.output() {
        Ok(output) => output,
        Err(e) => return Report::failed(format!("probe failed to run: {e}")),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() { "no output".to_string() } else { stderr };
        return Report::failed(format!("probe exited unsuccessfully: {detail}"));
    }

    match serde_json::from_slice::<Report>(&output.stdout) {
        Ok(report) => report,
        Err(e) => Report::failed(format!("unreadable probe output: {e}")),
    }
}

/// Probes every container, in parallel, and pairs each with its report.
///
/// Parallel by process rather than by thread: each probe is a child that must be
/// single-threaded to call `setns`, so they are all spawned, then all collected.
/// Nine containers therefore cost roughly one probe's latency, not nine.
fn probe_all(containers: &[Container], options: &Options) -> Vec<(Container, Report)> {
    // Spawning is cheap and the work is in waiting, so a plain sequential
    // spawn-then-collect would serialise. Use scoped threads purely to overlap
    // the waiting; each thread's child is still forked single-threaded by the
    // kernel, which is what setns cares about.
    std::thread::scope(|scope| {
        let handles: Vec<_> = containers
            .iter()
            .map(|container| scope.spawn(move || (container.clone(), probe_container(container, options))))
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .collect()
    })
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
    results
        .iter()
        .find(|(c, _)| c.name == wanted)
        .or_else(|| results.iter().find(|(c, _)| render::short_name(&c.name) == wanted))
}

/// Renders the diagnostic table.
fn render_list(results: &[(Container, Report)], now: i64) -> String {
    let mut out = String::new();
    for (container, report) in results {
        let state = report.parsed_state().map_or("unknown", State::word);
        let age = match (report.parsed_state(), report.since_ms) {
            (Some(_), Some(since)) => render::hhmm((now - since) / 1000),
            _ => "--:--".to_string(),
        };
        let port = report.port.map_or_else(|| "-".to_string(), |p| p.to_string());
        let note = report.reason.as_deref().unwrap_or("");
        out.push_str(&format!(
            "{:>2}  {:<24} {:<8} {:<6} pid={:<8} port={:<6} {}\n",
            container.slot,
            container.name,
            state,
            age,
            container.pid.unwrap_or(0),
            port,
            note
        ));
    }
    out
}

/// Runs the requested mode, returning what to print and the exit code.
fn run(options: Options) -> Result<String, String> {
    let now = now_ms();

    match &options.mode {
        Mode::Help => Ok(USAGE.to_string()),
        Mode::Version => Ok(format!("{PROGRAM} {}\n", env!("CARGO_PKG_VERSION"))),

        Mode::Probe => {
            let report = probe::run(options.port, options.timeout, now);
            let json = serde_json::to_string(&report)
                .map_err(|e| format!("cannot serialise report: {e}"))?;
            Ok(format!("{json}\n"))
        }

        Mode::Counts => {
            let containers = discover::discover()?;
            let results = probe_all(&containers, &options);
            let counts = Counts::tally(results.iter().map(|(_, r)| r.parsed_state()));
            Ok(render::counts(counts))
        }

        Mode::Instance(wanted) => {
            let containers = discover::discover()?;
            let results = probe_all(&containers, &options);
            match select_instance(&results, wanted) {
                // A slot that does not exist prints nothing at all, so an unused
                // DAK button stays blank.
                None => Ok(String::new()),
                Some((container, report)) => Ok(render::instance(
                    &container.name,
                    report.parsed_state(),
                    report.since_ms,
                    now,
                )),
            }
        }

        Mode::List => {
            let containers = discover::discover()?;
            let results = probe_all(&containers, &options);
            Ok(render_list(&results, now))
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
            let report = probe_container(&container, &options);
            let json = serde_json::to_string(&report)
                .map_err(|e| format!("cannot serialise report: {e}"))?;
            Ok(format!("{json}\n"))
        }
    }
}

/// Entry point. Errors go to stderr with a non-zero exit; everything else prints
/// to stdout and exits 0, including the "nothing to report" cases.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let options = match parse_args(&args) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("{PROGRAM}: {e}");
            eprint!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match run(options) {
        Ok(output) => {
            print!("{output}");
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
        match parse_args(&args(&["--instance", "opencode-web"])).unwrap().mode {
            Mode::Instance(v) => assert_eq!(v, "opencode-web"),
            other => panic!("expected instance mode, got {other:?}"),
        }
    }

    /// The remaining modes parse.
    #[test]
    fn parses_other_modes() {
        assert!(matches!(parse_args(&args(&["--list"])).unwrap().mode, Mode::List));
        assert!(matches!(parse_args(&args(&["__probe"])).unwrap().mode, Mode::Probe));
        assert!(matches!(parse_args(&args(&["--help"])).unwrap().mode, Mode::Help));
        assert!(matches!(parse_args(&args(&["-h"])).unwrap().mode, Mode::Help));
        assert!(matches!(parse_args(&args(&["--version"])).unwrap().mode, Mode::Version));
        assert!(matches!(parse_args(&args(&["-V"])).unwrap().mode, Mode::Version));
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
        assert_eq!(
            select_instance(&results, "opencode-web").unwrap().0.slot,
            1
        );
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

    /// An empty table is empty output, not a header with nothing under it.
    #[test]
    fn list_of_nothing_is_empty() {
        assert_eq!(render_list(&[], 1790308945355), "");
    }

    /// `--help` and `--version` need no podman and produce output.
    #[test]
    fn help_and_version_need_no_containers() {
        let help = run(Options { mode: Mode::Help, ..Default::default() }).unwrap();
        assert!(help.contains("Usage:"));
        assert!(help.contains("--instance"));
        let version = run(Options { mode: Mode::Version, ..Default::default() }).unwrap();
        assert!(version.contains(env!("CARGO_PKG_VERSION")));
        assert!(version.contains(PROGRAM));
    }

    /// The usage text documents the --port requirement, which is the single most
    /// common reason the program reports nothing useful.
    #[test]
    fn usage_documents_the_port_requirement() {
        assert!(USAGE.contains("--port 4096"));
        assert!(USAGE.contains("server.port"));
    }
}
