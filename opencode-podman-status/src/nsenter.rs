//! Entering a rootless container's user and network namespaces.
//!
//! This is the one place the program uses `unsafe`, and the reason it works at
//! all is worth stating precisely. From `setns(2)`:
//!
//! > A process reassociating itself with a user namespace must have the
//! > `CAP_SYS_ADMIN` capability in the target user namespace. (This necessarily
//! > implies that it is only possible to join a descendant user namespace.) Upon
//! > successfully joining a user namespace, a process is granted all
//! > capabilities in that namespace [...]
//!
//! > In order to reassociate itself with a new network [...] namespace, the
//! > caller must have the `CAP_SYS_ADMIN` capability both in its own user
//! > namespace and in the user namespace that owns the target namespace.
//!
//! Rootless podman creates the container's user namespace as the invoking user,
//! and a process whose effective UID owns a user namespace holds all capabilities
//! in it. So running as that same user is sufficient - no root, no setuid, no
//! file capabilities. This is the same mechanism `podman unshare` and
//! `nsenter -U -n -t` rely on.
//!
//! Three constraints from the same manual page shape the design, and all three
//! are satisfied by doing the work in a freshly forked child:
//!
//! 1. *"A multithreaded process may not change user namespace with `setns()`."*
//!    A child immediately after `fork` has exactly one thread - only the calling
//!    thread is carried over - so this holds even though the parent overlaps
//!    several probes on threads of its own.
//! 2. *"a process can't join a new user namespace if it is sharing
//!    filesystem-related attributes (`CLONE_FS`) with another process."* `fork`
//!    copies those attributes, so the child has its own; threads would share them,
//!    which is why the `setns` calls must happen post-fork rather than on a thread.
//! 3. *"It is not permitted to use `setns()` to reenter the caller's current user
//!    namespace."* Detected up front by [`shares_our_namespace`] and reported,
//!    rather than being attempted and failing.
//!
//! **Order matters.** The user namespace must be joined first; attempting the
//! network namespace on its own fails with `EPERM`. This was verified
//! empirically - see NOTES.md.

use std::ffi::CString;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to a process's user namespace magic link.
fn userns_path(pid: i32) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}/ns/user"))
}

/// Path to a process's network namespace magic link.
fn netns_path(pid: i32) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}/ns/net"))
}

/// Reads the namespace identity a magic link points at, e.g. `net:[4026534881]`.
///
/// Two processes share a namespace exactly when these strings match. This is how
/// the program establishes which namespace a container is in: it is read from the
/// kernel, never inferred from podman's network topology.
pub fn namespace_id(pid: i32, kind: &str) -> io::Result<String> {
    let link = std::fs::read_link(format!("/proc/{pid}/ns/{kind}"))?;
    Ok(link.to_string_lossy().into_owned())
}

/// Opens a namespace magic link, returning a raw descriptor.
///
/// The descriptor is deliberately not wrapped in a type that closes on drop: it
/// is used from `pre_exec`, after which the child execs and the kernel closes it.
fn open_ns(path: &Path) -> io::Result<i32> {
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: `c_path` is a valid NUL-terminated string for the duration of the
    // call, and O_RDONLY|O_CLOEXEC on a /proc magic link has no side effects.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Builds a command that will run inside `pid`'s user and network namespaces.
///
/// The namespace descriptors are opened in the parent, because the child cannot
/// allocate or report errors usefully between `fork` and `exec`. The `pre_exec`
/// closure then performs only two syscalls, which is safe in that context: it
/// neither allocates nor takes locks.
///
/// Returns an error if the namespaces cannot be opened at all - usually because
/// the container's process runs as a UID that does not map back to us, which
/// happens when opencode runs as a non-root user inside the container.
pub fn command_in_namespaces(pid: i32, program: &str) -> io::Result<Command> {
    let userns = open_ns(&userns_path(pid))?;
    let netns = open_ns(&netns_path(pid))?;

    let mut command = Command::new(program);

    // SAFETY: the closure runs in the child between fork and exec. It calls only
    // `setns` and `_exit`-free error returns, performing no allocation, taking no
    // locks, and touching no shared state - the constraints that make pre_exec
    // sound. The child is single-threaded here, which is precisely what
    // setns(CLONE_NEWUSER) requires.
    unsafe {
        command.pre_exec(move || {
            // User namespace first: joining the network namespace requires
            // CAP_SYS_ADMIN in the user namespace that owns it, which we only
            // gain by entering that user namespace. Reversing these two fails
            // with EPERM.
            if libc::setns(userns, libc::CLONE_NEWUSER) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setns(netns, libc::CLONE_NEWNET) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    Ok(command)
}

/// True if `pid` is in the same network namespace as this process.
///
/// Such a container cannot be probed: `setns` refuses to re-enter the caller's
/// own user namespace, and there would be nothing to gain anyway. In practice
/// this means the caller is itself inside the container.
pub fn shares_our_namespace(pid: i32) -> bool {
    match (namespace_id(pid, "net"), namespace_id(std::process::id() as i32, "net")) {
        (Ok(theirs), Ok(ours)) => theirs == ours,
        // If it cannot be determined, assume not shared and let the probe fail
        // with a more specific message.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The namespace link paths are the ones the kernel exposes.
    #[test]
    fn builds_expected_namespace_paths() {
        assert_eq!(userns_path(1234), PathBuf::from("/proc/1234/ns/user"));
        assert_eq!(netns_path(1234), PathBuf::from("/proc/1234/ns/net"));
    }

    /// Our own namespace identity is readable and looks like the kernel's format.
    #[test]
    fn reads_own_namespace_id() {
        let pid = std::process::id() as i32;
        let id = namespace_id(pid, "net").expect("should read own netns");
        assert!(id.starts_with("net:["), "unexpected format: {id}");
        assert!(id.ends_with(']'), "unexpected format: {id}");
    }

    /// The user namespace link is readable in the same way.
    #[test]
    fn reads_own_user_namespace_id() {
        let pid = std::process::id() as i32;
        let id = namespace_id(pid, "user").expect("should read own userns");
        assert!(id.starts_with("user:["), "unexpected format: {id}");
    }

    /// A nonexistent PID is an error, not a panic.
    #[test]
    fn missing_pid_is_an_error() {
        // PID 0 is never a real process in /proc.
        assert!(namespace_id(0, "net").is_err());
    }

    /// This process trivially shares its own network namespace, which is the
    /// check that stops the program probing the container it is running in.
    #[test]
    fn detects_sharing_our_own_namespace() {
        assert!(shares_our_namespace(std::process::id() as i32));
    }

    /// Opening namespaces for a nonexistent PID fails rather than producing a
    /// command that would misbehave later.
    #[test]
    fn command_creation_fails_for_missing_pid() {
        assert!(command_in_namespaces(0, "/bin/true").is_err());
    }
}
