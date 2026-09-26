//! Obtaining TCP sockets that live in a rootless container's network namespace,
//! without ever running this program's own code inside the container.
//!
//! # Why sockets, not a process
//!
//! A socket belongs to the network namespace it was *created* in, for its whole
//! life, whichever process later holds it. So the only thing that needs to happen
//! inside the container's namespaces is `socket(2)` itself. A forked child does
//! exactly that - join the namespaces, create the sockets, pass them back over a
//! Unix socketpair with `SCM_RIGHTS`, `_exit` - and the parent, which never
//! changes namespace, then connects them to `127.0.0.1` and does all the HTTP and
//! JSON work itself. Nothing is `exec`ed, and no request or response is ever
//! handled by a process holding the container's credentials.
//!
//! An earlier version re-executed this whole program inside the namespaces. That
//! worked, but it meant the full HTTP and JSON stack ran with the container's
//! user-namespace credentials, inherited the caller's environment, and depended
//! on `/proc/self/exe` still naming a binary that a package upgrade may have
//! replaced. See NOTES.md.
//!
//! # Why it is allowed
//!
//! From `setns(2)`:
//!
//! > A process reassociating itself with a user namespace must have the
//! > `CAP_SYS_ADMIN` capability in the target user namespace. [...] Upon
//! > successfully joining a user namespace, a process is granted all
//! > capabilities in that namespace [...]
//!
//! > In order to reassociate itself with a new network [...] namespace, the
//! > caller must have the `CAP_SYS_ADMIN` capability both in its own user
//! > namespace and in the user namespace that owns the target namespace.
//!
//! Rootless podman creates the container's user namespace as the invoking user,
//! and a process whose effective UID owns a user namespace holds all capabilities
//! in it. So running as that same user is sufficient - no root, no setuid, no file
//! capabilities. **The user namespace must be joined first**; the network
//! namespace alone fails with `EPERM` (verified, see NOTES.md).
//!
//! # Why a forked child
//!
//! *"A multithreaded process may not change user namespace with `setns()`"*, and
//! *"a process can't join a new user namespace if it is sharing filesystem-related
//! attributes (`CLONE_FS`) with another process"*. A child immediately after
//! `fork` has one thread and its own fs attributes, so both hold even though the
//! parent probes containers on several threads.
//!
//! Because the parent *is* multithreaded, the child may only make
//! async-signal-safe calls: it must not allocate, lock, or touch Rust's standard
//! streams. Everything it needs - descriptors, buffers - is prepared before
//! `fork`, and it makes nothing but raw system calls.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Instant;

/// Most sockets fetched from one namespace in a single round trip.
///
/// Also a hard cap on how many requests one container can cost: every request
/// consumes a socket, and there is no second trip.
pub const MAX_SOCKETS: usize = 16;

/// Stages of the child's work, reported back so a failure says what failed.
const STAGE_OK: i32 = 0;
const STAGE_USERNS: i32 = 1;
const STAGE_NETNS: i32 = 2;
const STAGE_SOCKET: i32 = 3;

/// Size of the child's report: `[stage, errno]` as two native `i32`s.
const REPORT_LEN: usize = 2 * std::mem::size_of::<i32>();

/// Reads the namespace identity a magic link points at, e.g. `net:[4026534881]`.
///
/// Two processes share a namespace exactly when these strings match. This is how
/// the program establishes which namespace a container is in: it is read from the
/// kernel, never inferred from podman's network topology.
pub fn namespace_id(pid: i32, kind: &str) -> io::Result<String> {
    let link = std::fs::read_link(format!("/proc/{pid}/ns/{kind}"))?;
    Ok(link.to_string_lossy().into_owned())
}

/// True if `pid` is in the same network namespace as this process.
///
/// Such a container cannot be entered: `setns` refuses to re-enter the caller's
/// own user namespace. In practice this means the caller is itself inside it.
pub fn shares_our_namespace(pid: i32) -> bool {
    match (namespace_id(pid, "net"), namespace_id(std::process::id() as i32, "net")) {
        (Ok(theirs), Ok(ours)) => theirs == ours,
        // If it cannot be determined, assume not shared and let entry fail with
        // a more specific message.
        _ => false,
    }
}

/// Opens a `/proc/<pid>/ns/<kind>` magic link.
fn open_ns(pid: i32, kind: &str) -> io::Result<OwnedFd> {
    let path = std::ffi::CString::new(format!("/proc/{pid}/ns/{kind}"))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: `path` is a valid NUL-terminated string for the duration of the
    // call; O_RDONLY|O_CLOEXEC on a /proc magic link has no side effects.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by open(2) and is owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Human-readable name of a child stage, for error messages.
fn stage_name(stage: i32) -> &'static str {
    match stage {
        STAGE_USERNS => "joining the user namespace",
        STAGE_NETNS => "joining the network namespace",
        STAGE_SOCKET => "creating a socket",
        _ => "an unknown step",
    }
}

/// Turns the child's `[stage, errno]` report into a result.
///
/// Separated out so the error text can be tested without a namespace.
fn interpret_report(stage: i32, errno: i32) -> io::Result<()> {
    if stage == STAGE_OK {
        return Ok(());
    }
    let cause = io::Error::from_raw_os_error(errno);
    Err(io::Error::new(cause.kind(), format!("{} failed: {cause}", stage_name(stage))))
}

/// A container's user and network namespaces, held open by descriptor.
///
/// Opening them is the permission check that matters: it fails with `EACCES`
/// when the container's process runs as a UID that does not map back to us,
/// which happens when opencode runs as a non-root user inside the container.
pub struct Namespaces {
    user: OwnedFd,
    net: OwnedFd,
}

impl Namespaces {
    /// Opens `pid`'s user and network namespaces.
    pub fn open(pid: i32) -> io::Result<Self> {
        Ok(Namespaces { user: open_ns(pid, "user")?, net: open_ns(pid, "net")? })
    }

    /// Creates `count` unconnected IPv4 TCP sockets inside these namespaces.
    ///
    /// Forks a child that joins the user namespace, then the network namespace,
    /// creates the sockets and passes them back. If the child has not answered by
    /// `deadline` it is killed; it is always reaped before this returns.
    pub fn sockets(&self, count: usize, deadline: Instant) -> io::Result<Vec<OwnedFd>> {
        if count == 0 || count > MAX_SOCKETS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("socket count must be 1..={MAX_SOCKETS}"),
            ));
        }

        let (ours, theirs) = socketpair()?;

        // Everything the child touches is allocated here, before fork. `u64`
        // elements give the control buffer the alignment cmsghdr needs.
        // SAFETY: CMSG_SPACE is a pure size computation.
        let space = unsafe { libc::CMSG_SPACE((MAX_SOCKETS * std::mem::size_of::<RawFd>()) as u32) }
            as usize;
        let mut control = vec![0u64; space.div_ceil(8)];

        // SAFETY: fork(2) has no preconditions. In the child only
        // async-signal-safe raw system calls are made (see `child_main`), which
        // is what makes forking a multithreaded parent sound.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            // SAFETY: we are the freshly forked, single-threaded child. The
            // pointers refer to this process's copy-on-write copy of the parent's
            // memory, which nothing else can touch now.
            unsafe {
                child_main(
                    self.user.as_raw_fd(),
                    self.net.as_raw_fd(),
                    theirs.as_raw_fd(),
                    count,
                    control.as_mut_ptr() as *mut u8,
                )
            }
        }

        // Parent. Drop our copy of the child's end, so that if the child dies
        // without writing, our read sees EOF instead of waiting for the deadline.
        drop(theirs);
        let reaper = Reaper(pid);
        let result = receive(&ours, count, deadline, &mut control);
        if result.is_err() {
            reaper.kill();
        }
        drop(reaper);
        result
    }
}

/// Kills and reaps the helper child on every exit path.
struct Reaper(libc::pid_t);

impl Reaper {
    /// Kills the child outright; used when it overran or misbehaved.
    fn kill(&self) {
        // SAFETY: the pid is our own unreaped child, so it cannot have been
        // reused by an unrelated process.
        unsafe { libc::kill(self.0, libc::SIGKILL) };
    }
}

impl Drop for Reaper {
    /// Waits for the child. It exits straight after sending, or has just been
    /// killed, so this never blocks for long.
    fn drop(&mut self) {
        let mut status = 0;
        loop {
            // SAFETY: waiting on our own child with a valid status pointer.
            let r = unsafe { libc::waitpid(self.0, &mut status, 0) };
            if r >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
    }
}

/// Creates a connected pair of close-on-exec Unix datagram-boundary sockets.
fn socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors socketpair(2) writes.
    let r = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created and are owned by nobody else.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Waits for the child's report and takes ownership of every descriptor in it.
///
/// Every received descriptor is wrapped immediately, so none can leak even when
/// the report turns out to be a failure or the wrong size.
fn receive(
    chan: &OwnedFd,
    count: usize,
    deadline: Instant,
    control: &mut [u64],
) -> io::Result<Vec<OwnedFd>> {
    wait_readable(chan.as_raw_fd(), deadline)?;

    let mut report = [0i32; 2];
    let mut iov = libc::iovec {
        iov_base: report.as_mut_ptr() as *mut libc::c_void,
        iov_len: REPORT_LEN,
    };
    // SAFETY: an all-zero msghdr is a valid empty header.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = std::mem::size_of_val(control) as _;

    // SAFETY: `msg` points at live buffers of the sizes it declares.
    let n = unsafe { libc::recvmsg(chan.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut received = Vec::new();
    // SAFETY: walking the control messages the kernel just filled in, within
    // the bounds recvmsg reported in `msg`.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                let bytes = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                for i in 0..bytes / std::mem::size_of::<RawFd>() {
                    received.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i))));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }

    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "namespace helper exited without reporting",
        ));
    }
    if n as usize != REPORT_LEN || msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "malformed report from namespace helper"));
    }
    interpret_report(report[0], report[1])?;
    if received.len() != count {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("namespace helper sent {} sockets, expected {count}", received.len()),
        ));
    }
    Ok(received)
}

/// Blocks until `fd` is readable or `deadline` passes.
pub(crate) fn wait_readable(fd: RawFd, deadline: Instant) -> io::Result<()> {
    wait_for(fd, libc::POLLIN, deadline)
}

/// Blocks until `fd` reports any of `events`, or `deadline` passes.
///
/// Hang-up and error conditions also end the wait; the caller's next operation
/// on the descriptor then reports what happened.
pub(crate) fn wait_for(fd: RawFd, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
        }
        // Round up so a sub-millisecond remainder still waits rather than spins.
        let ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd { fd, events, revents: 0 };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r > 0 {
            return Ok(());
        }
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}

/// The child: join the namespaces, create sockets, send them, exit.
///
/// Runs between `fork` and `_exit` in a child of a multithreaded process, so it
/// makes only raw, async-signal-safe system calls and writes only to memory it
/// was handed. Failure is reported as `[stage, errno]` over `chan`, with no
/// descriptors attached.
///
/// It also marks itself non-dumpable first. That stops anything else running as
/// the same user from ptrace-attaching to it or opening its `/proc/<pid>/mem`
/// while it briefly holds a copy of the parent's memory. (Processes with
/// `CAP_SYS_PTRACE` in the container's user namespace are not stopped by this
/// once the child has joined it; they would also need to see the child's PID,
/// which lives outside the container's PID namespace. See NOTES.md.)
///
/// # Safety
///
/// Must only be called in a freshly forked child. `control` must point at a
/// buffer of at least `CMSG_SPACE(MAX_SOCKETS * size_of::<RawFd>())` bytes,
/// aligned for `cmsghdr`, and `count` must be in `1..=MAX_SOCKETS`.
unsafe fn child_main(user: RawFd, net: RawFd, chan: RawFd, count: usize, control: *mut u8) -> ! {
    libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);

    let mut fds = [-1 as RawFd; MAX_SOCKETS];
    let mut stage = STAGE_OK;
    let mut errno = 0;

    // User namespace first: the network namespace requires CAP_SYS_ADMIN in the
    // user namespace that owns it, which we only gain by entering that one.
    if libc::setns(user, libc::CLONE_NEWUSER) != 0 {
        stage = STAGE_USERNS;
    } else if libc::setns(net, libc::CLONE_NEWNET) != 0 {
        stage = STAGE_NETNS;
    } else {
        for slot in fds.iter_mut().take(count) {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
            if fd < 0 {
                stage = STAGE_SOCKET;
                break;
            }
            *slot = fd;
        }
    }
    if stage != STAGE_OK {
        errno = *libc::__errno_location();
    }

    let mut report = [stage, errno];
    let mut iov = libc::iovec {
        iov_base: report.as_mut_ptr() as *mut libc::c_void,
        iov_len: REPORT_LEN,
    };
    let mut msg: libc::msghdr = std::mem::zeroed();
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;

    if stage == STAGE_OK {
        let payload = (count * std::mem::size_of::<RawFd>()) as u32;
        msg.msg_control = control as *mut libc::c_void;
        msg.msg_controllen = libc::CMSG_SPACE(payload) as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(payload) as _;
        let data = libc::CMSG_DATA(cmsg) as *mut RawFd;
        for (i, fd) in fds.iter().take(count).enumerate() {
            std::ptr::write_unaligned(data.add(i), *fd);
        }
    }

    let sent = libc::sendmsg(chan, &msg, libc::MSG_NOSIGNAL);
    libc::_exit(if sent == REPORT_LEN as isize { 0 } else { 1 });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Our own namespace identity is readable and looks like the kernel's format.
    #[test]
    fn reads_own_namespace_ids() {
        let pid = std::process::id() as i32;
        let net = namespace_id(pid, "net").expect("should read own netns");
        assert!(net.starts_with("net:[") && net.ends_with(']'), "unexpected: {net}");
        let user = namespace_id(pid, "user").expect("should read own userns");
        assert!(user.starts_with("user:["), "unexpected: {user}");
    }

    /// A nonexistent PID is an error, not a panic.
    #[test]
    fn missing_pid_is_an_error() {
        // PID 0 is never a real process in /proc.
        assert!(namespace_id(0, "net").is_err());
        assert!(Namespaces::open(0).is_err());
    }

    /// This process trivially shares its own network namespace, which is the
    /// check that stops the program trying to enter the container it runs in.
    #[test]
    fn detects_sharing_our_own_namespace() {
        assert!(shares_our_namespace(std::process::id() as i32));
    }

    /// A child's failure report names the step that failed and keeps the OS error.
    #[test]
    fn failure_reports_name_the_failing_step() {
        assert!(interpret_report(STAGE_OK, 0).is_ok());
        let e = interpret_report(STAGE_USERNS, libc::EPERM).unwrap_err();
        assert!(e.to_string().starts_with("joining the user namespace failed"), "{e}");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let e = interpret_report(STAGE_NETNS, libc::EPERM).unwrap_err();
        assert!(e.to_string().contains("network namespace"), "{e}");
        let e = interpret_report(STAGE_SOCKET, libc::EMFILE).unwrap_err();
        assert!(e.to_string().contains("creating a socket"), "{e}");
        let e = interpret_report(99, libc::EIO).unwrap_err();
        assert!(e.to_string().contains("unknown step"), "{e}");
    }

    /// Socket counts outside what one round trip carries are refused up front.
    #[test]
    fn rejects_out_of_range_socket_counts() {
        let pid = std::process::id() as i32;
        let ns = Namespaces::open(pid).expect("own namespaces open");
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(ns.sockets(0, deadline).is_err());
        assert!(ns.sockets(MAX_SOCKETS + 1, deadline).is_err());
    }

    /// Entering our *own* user namespace is refused by the kernel (`EINVAL`);
    /// the full fork/report/reap path turns that into a specific error rather
    /// than hanging or leaking a child.
    #[test]
    fn reports_kernel_refusal_from_the_child() {
        let pid = std::process::id() as i32;
        let ns = Namespaces::open(pid).expect("own namespaces open");
        let e = ns
            .sockets(2, Instant::now() + Duration::from_secs(5))
            .expect_err("re-entering our own user namespace must fail");
        assert!(e.to_string().contains("joining the user namespace failed"), "{e}");
    }

    /// End to end against a real nested user+network namespace, when this
    /// environment permits creating one (many CI containers do not; then the
    /// test says so and passes vacuously - tests/namespace-entry.sh is the
    /// authoritative check).
    ///
    /// The sockets must belong to the target network namespace, not ours: that
    /// is asserted by asking the kernel which namespace each socket is in
    /// (`SIOCGSKNS`), and by the parent staying in its own namespace throughout.
    #[test]
    fn creates_sockets_inside_a_nested_namespace() {
        let Some(mut target) = spawn_nested_namespace() else {
            eprintln!("skipping: cannot create a nested user+network namespace here");
            return;
        };
        let pid = target.id() as i32;
        let before = namespace_id(std::process::id() as i32, "net").unwrap();

        let ns = Namespaces::open(pid).expect("open target namespaces");
        let sockets = ns
            .sockets(3, Instant::now() + Duration::from_secs(5))
            .expect("sockets from target namespace");
        assert_eq!(sockets.len(), 3);

        let target_ns = inode_of_path(&format!("/proc/{pid}/ns/net"));
        for socket in &sockets {
            // SAFETY: SIOCGSKNS on a socket returns a new fd for its netns.
            let nsfd = unsafe { libc::ioctl(socket.as_raw_fd(), 0x894C /* SIOCGSKNS */) };
            assert!(nsfd >= 0, "SIOCGSKNS failed: {}", io::Error::last_os_error());
            // SAFETY: nsfd was just returned by the kernel.
            let nsfd = unsafe { OwnedFd::from_raw_fd(nsfd) };
            assert_eq!(inode_of_fd(&nsfd), target_ns, "socket is not in the target namespace");
        }
        assert_eq!(namespace_id(std::process::id() as i32, "net").unwrap(), before);

        let _ = target.kill();
        let _ = target.wait();
    }

    /// Starts `sleep` in a fresh user+network namespace via unshare(1), and
    /// waits until its namespaces have actually changed. `None` if not possible.
    fn spawn_nested_namespace() -> Option<std::process::Child> {
        let ours = namespace_id(std::process::id() as i32, "net").ok()?;
        let mut child = std::process::Command::new("unshare")
            .args(["-Urn", "sleep", "30"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let pid = child.id() as i32;
        for _ in 0..100 {
            if let Ok(Some(_)) = child.try_wait() {
                return None;
            }
            match namespace_id(pid, "net") {
                Ok(theirs) if theirs != ours => return Some(child),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        None
    }

    /// Inode number of the file a path resolves to.
    fn inode_of_path(path: &str) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).expect("stat").ino()
    }

    /// Inode number of an open descriptor.
    fn inode_of_fd(fd: &OwnedFd) -> u64 {
        // SAFETY: an all-zero stat is a valid out-buffer for fstat.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid fd and out-pointer.
        assert_eq!(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) }, 0);
        st.st_ino
    }
}
