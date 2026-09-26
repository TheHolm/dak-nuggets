//! Finding the port opencode is listening on, without touching anything else.
//!
//! A container may run other servers alongside opencode. An earlier version of
//! this program tried every listening loopback socket in turn and sent it
//! `GET /global/health` to see whether it was opencode - which meant unrelated
//! services received HTTP requests they never asked for and logged errors about
//! them. That was reported from real use and is not acceptable for a monitor.
//!
//! So the port is now established from **ownership**, never by trying it:
//!
//! 1. Every listening socket in `/proc/net/tcp` (and `tcp6`) carries an inode.
//! 2. Every open socket of a process appears in `/proc/<pid>/fd/*` as a link to
//!    `socket:[<inode>]`.
//! 3. So the processes in this network namespace that *are* opencode, and the
//!    listening sockets they hold, identify its port exactly.
//!
//! Only a port found this way is ever connected to. If opencode is running but
//! holds no listening socket, neither the status plugin nor `--port` is in use,
//! and that is reported as such.
//!
//! All of this runs **on the host**, without entering any namespace:
//! `/proc/<pid>/net/tcp` is the TCP table of *that process's* network namespace,
//! so reading it for the container's main PID gives the container's table. PIDs
//! are host PIDs throughout, and processes are matched to the container by
//! comparing network namespace identity. (Verified as an unprivileged user
//! against a rootless-style nested namespace - see NOTES.md.)

use std::collections::HashSet;
use std::fs;

/// `/proc/net/tcp` state value for a listening socket.
const TCP_LISTEN: &str = "0A";

/// The name opencode's processes run under, as seen in `comm` and `argv[0]`.
const OPENCODE: &str = "opencode";

/// A listening socket from a `/proc/net/tcp`-format table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listener {
    pub port: u16,
    pub inode: u64,
}

/// Extracts listening sockets bound to loopback or the wildcard address.
///
/// Takes the contents of a `/proc/net/tcp`-format table so it can be tested
/// against captured fixtures. Both loopback and wildcard binds are kept:
/// opencode binds `127.0.0.1` by default, but someone may have passed
/// `--hostname 0.0.0.0`, and from inside the namespace `127.0.0.1` reaches both.
///
/// Addresses are little-endian hex in this file, so `127.0.0.1` appears as
/// `0100007F`. The inode is the tenth whitespace-separated field.
pub fn parse_listeners(table: &str) -> Vec<Listener> {
    let mut found = Vec::new();
    for line in table.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 || fields[3] != TCP_LISTEN {
            continue;
        }
        let (addr, port) = match fields[1].split_once(':') {
            Some(parts) => parts,
            None => continue,
        };
        if !is_local_address(addr) {
            continue;
        }
        let port = match u16::from_str_radix(port, 16) {
            Ok(p) if p != 0 => p,
            _ => continue,
        };
        let inode = match fields[9].parse::<u64>() {
            Ok(i) if i != 0 => i,
            _ => continue,
        };
        found.push(Listener { port, inode });
    }
    found
}

/// True if a `/proc/net/tcp` local address is loopback or the wildcard.
///
/// Handles both the 8-character IPv4 form and the 32-character IPv6 form.
fn is_local_address(addr: &str) -> bool {
    if !addr.is_empty() && addr.bytes().all(|b| b == b'0') {
        return true;
    }
    match addr.len() {
        8 => addr.eq_ignore_ascii_case("0100007F"),
        32 => addr.eq_ignore_ascii_case("00000000000000000000000001000000"),
        _ => false,
    }
}

/// Extracts the inode from a `/proc/<pid>/fd/*` link target such as
/// `socket:[345568]`. Anything that is not a socket yields `None`.
pub fn socket_inode(link: &str) -> Option<u64> {
    link.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
}

/// True if a process is opencode, judged by its `comm` and its `argv[0]`.
///
/// Either is sufficient: `comm` is what the compiled binary shows, while a
/// wrapper that renames itself may only be recognisable by `argv[0]`. Both must
/// match **exactly** after taking the basename, so this program - whose `comm`
/// is `opencode-podman` - is never mistaken for the thing it monitors.
pub fn is_opencode_process(comm: &str, cmdline: &[u8]) -> bool {
    if comm.trim_end() == OPENCODE {
        return true;
    }
    let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or(&[]);
    let argv0 = String::from_utf8_lossy(argv0);
    argv0.rsplit('/').next() == Some(OPENCODE)
}

/// Picks the ports of the listeners that belong to opencode.
///
/// `owned` is the set of socket inodes held by opencode processes. Ports come
/// back in table order, without duplicates (IPv4 and IPv6 listeners on the same
/// port collapse to one).
pub fn owned_ports(listeners: &[Listener], owned: &HashSet<u64>) -> Vec<u16> {
    let mut ports = Vec::new();
    for listener in listeners {
        if owned.contains(&listener.inode) && !ports.contains(&listener.port) {
            ports.push(listener.port);
        }
    }
    ports
}

/// What the ownership scan established.
#[derive(Debug, PartialEq, Eq)]
pub enum Discovery {
    /// opencode holds these listening ports.
    Found(Vec<u16>),
    /// No opencode process in this network namespace.
    NotRunning,
    /// opencode is running but holds no listening socket: neither the status
    /// plugin nor `--port` is in use.
    NotListening,
    /// opencode is running, but its open files could not be inspected.
    Unreadable(String),
}

impl Discovery {
    /// Human-readable reason for a discovery that found nothing, for `--list`.
    pub fn reason(&self) -> String {
        match self {
            Discovery::Found(_) => String::new(),
            Discovery::NotRunning => "opencode is not running in this container".to_string(),
            Discovery::NotListening => {
                "opencode is running but listens on nothing: enable the status plugin \
                 (see README.markdown)"
                    .to_string()
            }
            Discovery::Unreadable(e) => format!("cannot inspect opencode's sockets: {e}"),
        }
    }
}

/// Reads a process's network namespace identity.
fn netns_of(pid: i32) -> Option<String> {
    fs::read_link(format!("/proc/{pid}/ns/net"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Paths of the IPv4 and IPv6 TCP tables of `pid`'s network namespace.
fn tcp_table_paths(pid: i32) -> (String, String) {
    (format!("/proc/{pid}/net/tcp"), format!("/proc/{pid}/net/tcp6"))
}

/// Reads every listening socket in `pid`'s network namespace.
///
/// `/proc/<pid>/net` resolves to that process's network namespace, so for a
/// container's main PID these are the container's sockets, read from outside.
/// `tcp6` may be absent on a host without IPv6, which is not an error.
fn read_listeners(pid: i32) -> Result<Vec<Listener>, String> {
    let (v4_path, v6_path) = tcp_table_paths(pid);
    let v4 = fs::read_to_string(&v4_path).map_err(|e| format!("cannot read {v4_path}: {e}"))?;
    let mut listeners = parse_listeners(&v4);
    if let Ok(v6) = fs::read_to_string(&v6_path) {
        listeners.extend(parse_listeners(&v6));
    }
    Ok(listeners)
}

/// Collects the socket inodes held by one process.
fn socket_inodes_of(pid: &str, into: &mut HashSet<u64>) -> std::io::Result<()> {
    for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
        let Ok(entry) = entry else { continue };
        if let Ok(target) = fs::read_link(entry.path()) {
            if let Some(inode) = socket_inode(&target.to_string_lossy()) {
                into.insert(inode);
            }
        }
    }
    Ok(())
}

/// Finds opencode's listening ports in the network namespace of `container_pid`.
///
/// Scans `/proc` for processes that share that network namespace and are
/// opencode, gathers the socket inodes they hold, and keeps only the listeners
/// whose inode is among them. Nothing is connected to, and no namespace is
/// entered.
pub fn discover(container_pid: i32) -> Discovery {
    let listeners = match read_listeners(container_pid) {
        Ok(l) => l,
        Err(e) => return Discovery::Unreadable(e),
    };
    let Some(ours) = netns_of(container_pid) else {
        return Discovery::Unreadable(format!(
            "cannot read the network namespace of pid {container_pid}"
        ));
    };
    let entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(e) => return Discovery::Unreadable(format!("cannot read /proc: {e}")),
    };

    let mut seen_opencode = false;
    let mut owned = HashSet::new();
    let mut last_error = None;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name.to_string_lossy();
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // Only processes in this network namespace - i.e. in this container.
        // Processes we may not inspect (other users') simply fail and are skipped.
        match fs::read_link(format!("/proc/{pid}/ns/net")) {
            Ok(ns) if ns.to_string_lossy() == ours => {}
            _ => continue,
        }
        let comm = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if !is_opencode_process(&comm, &cmdline) {
            continue;
        }
        seen_opencode = true;
        if let Err(e) = socket_inodes_of(&pid, &mut owned) {
            last_error = Some(format!("pid {pid}: {e}"));
        }
    }

    if !seen_opencode {
        return Discovery::NotRunning;
    }
    let ports = owned_ports(&listeners, &owned);
    if !ports.is_empty() {
        return Discovery::Found(ports);
    }
    match last_error {
        // Could not see its sockets at all, so "not listening" would be a guess.
        Some(e) if owned.is_empty() => Discovery::Unreadable(e),
        _ => Discovery::NotListening,
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

    /// Builds a one-listener table line, for the cases the real capture lacks.
    fn table(lines: &[(&str, &str, &str)]) -> String {
        let mut t = String::from("header\n");
        for (i, (local, state, inode)) in lines.iter().enumerate() {
            t.push_str(&format!(
                "   {i}: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000 0 0 {inode} 1 0 20 4 0 4 2\n"
            ));
        }
        t
    }

    /// The real table yields the listener with its inode, and ignores the
    /// established connection.
    #[test]
    fn parses_real_table() {
        assert_eq!(
            parse_listeners(REAL_TABLE),
            vec![Listener { port: 0x1002, inode: 257009 }]
        );
    }

    /// Only listening sockets count.
    #[test]
    fn ignores_non_listening_sockets() {
        let t = table(&[("0100007F:1000", "01", "11"), ("0100007F:1001", "06", "12")]);
        assert!(parse_listeners(&t).is_empty());
    }

    /// Wildcard and IPv6 loopback/wildcard binds are accepted.
    #[test]
    fn accepts_wildcard_and_ipv6_local_binds() {
        let t = table(&[
            ("00000000:1000", "0A", "21"),
            ("00000000000000000000000001000000:1001", "0A", "22"),
            ("00000000000000000000000000000000:1002", "0A", "23"),
        ]);
        let ports: Vec<u16> = parse_listeners(&t).iter().map(|l| l.port).collect();
        assert_eq!(ports, vec![0x1000, 0x1001, 0x1002]);
    }

    /// A listener on a routable address is not reachable on loopback.
    #[test]
    fn ignores_non_local_listeners() {
        let t = table(&[("010002C0:1000", "0A", "31")]);
        assert!(parse_listeners(&t).is_empty());
    }

    /// Truncated and malformed tables are survived without panicking.
    #[test]
    fn survives_malformed_tables() {
        assert!(parse_listeners("").is_empty());
        assert!(parse_listeners("header only\n").is_empty());
        assert!(parse_listeners("header\n   0:\n").is_empty());
        assert!(parse_listeners("header\n   0: nocolon 0:0 0A 0 0 0 0 0 0 5\n").is_empty());
        let t = table(&[("0100007F:ZZZZ", "0A", "41"), ("0100007F:0000", "0A", "42")]);
        assert!(parse_listeners(&t).is_empty());
        let t = table(&[("0100007F:1000", "0A", "notanumber"), ("0100007F:1001", "0A", "0")]);
        assert!(parse_listeners(&t).is_empty());
    }

    /// Socket fd links yield their inode; other fd targets do not.
    #[test]
    fn extracts_socket_inodes_from_fd_links() {
        assert_eq!(socket_inode("socket:[345568]"), Some(345568));
        assert_eq!(socket_inode("pipe:[12]"), None);
        assert_eq!(socket_inode("/dev/null"), None);
        assert_eq!(socket_inode("socket:[]"), None);
        assert_eq!(socket_inode("socket:[12"), None);
        assert_eq!(socket_inode("anon_inode:[eventpoll]"), None);
    }

    /// opencode is recognised by `comm` as the compiled binary reports it.
    #[test]
    fn recognises_opencode_by_comm() {
        assert!(is_opencode_process("opencode\n", b"opencode\0--port\x004096\0"));
        assert!(is_opencode_process("opencode", b""));
    }

    /// ...or by `argv[0]`, including a full path, when `comm` differs.
    #[test]
    fn recognises_opencode_by_argv0() {
        assert!(is_opencode_process("node\n", b"/usr/local/bin/opencode\0--port\x004096\0"));
        assert!(is_opencode_process("bun\n", b"opencode\0"));
    }

    /// Neighbours in the container are not opencode - including this program,
    /// whose `comm` is truncated to `opencode-podman`, and other servers.
    #[test]
    fn does_not_mistake_other_processes_for_opencode() {
        assert!(!is_opencode_process("opencode-podman\n", b"/usr/bin/opencode-podman-status\0__probe\0"));
        assert!(!is_opencode_process("python3\n", b"python3\0-m\0http.server\0"));
        assert!(!is_opencode_process("bash\n", b"bash\0-c\0opencode --port 4096\0"));
        assert!(!is_opencode_process("opencodex\n", b"opencodex\0"));
        assert!(!is_opencode_process("", b""));
    }

    /// Only listeners opencode holds are selected - the fix for probing
    /// unrelated servers. Mirrors the reported case: another server on 8765
    /// alongside opencode on 4099.
    #[test]
    fn selects_only_ports_opencode_owns() {
        let listeners = vec![
            Listener { port: 8765, inode: 347358 }, // someone else's server
            Listener { port: 4099, inode: 345568 }, // opencode
        ];
        let owned: HashSet<u64> = [345568, 999].into_iter().collect();
        assert_eq!(owned_ports(&listeners, &owned), vec![4099]);
    }

    /// With opencode holding no listener, nothing is selected - the other
    /// server must not be picked as a fallback.
    #[test]
    fn selects_nothing_when_opencode_holds_no_listener() {
        let listeners = vec![Listener { port: 8765, inode: 347358 }];
        let owned: HashSet<u64> = [111, 222].into_iter().collect();
        assert!(owned_ports(&listeners, &owned).is_empty());
    }

    /// IPv4 and IPv6 listeners on the same port are reported once.
    #[test]
    fn deduplicates_ports_across_address_families() {
        let listeners = vec![
            Listener { port: 4096, inode: 1 },
            Listener { port: 4096, inode: 2 },
        ];
        let owned: HashSet<u64> = [1, 2].into_iter().collect();
        assert_eq!(owned_ports(&listeners, &owned), vec![4096]);
    }

    /// Each non-success discovery explains itself, and the not-listening case
    /// names the fix.
    #[test]
    fn discovery_reasons_are_specific() {
        assert!(Discovery::NotRunning.reason().contains("not running"));
        assert!(Discovery::NotListening.reason().contains("status plugin"));
        assert!(Discovery::Unreadable("boom".into()).reason().contains("boom"));
        assert_eq!(Discovery::Found(vec![4096]).reason(), "");
    }

    /// The TCP tables are read per process, which is what lets discovery run on
    /// the host against a container's PID.
    #[test]
    fn reads_tables_of_the_given_process() {
        assert_eq!(
            tcp_table_paths(1234),
            ("/proc/1234/net/tcp".to_string(), "/proc/1234/net/tcp6".to_string())
        );
    }

    /// The live scan against our own PID runs without panicking. What it finds
    /// depends on the machine (an opencode may well share this namespace), so
    /// only the shape is asserted.
    #[test]
    fn live_discovery_does_not_panic() {
        let _ = discover(std::process::id() as i32);
    }

    /// A PID that does not exist is reported as unreadable, not as "not running".
    #[test]
    fn discovery_of_missing_pid_is_unreadable() {
        assert!(matches!(discover(0), Discovery::Unreadable(_)));
    }
}
