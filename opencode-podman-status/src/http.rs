//! A deliberately minimal HTTP/1.1 GET client.
//!
//! Every request this program makes is a plain GET to `127.0.0.1` *inside* a
//! container's network namespace. That makes almost everything a general HTTP
//! client handles irrelevant: no TLS, no redirects, no proxies, no keep-alive
//! pooling. The one extra is optional Basic authentication (see
//! [`crate::auth`]).
//!
//! Responses end at `Content-Length` when the server sends one, and at EOF
//! otherwise. `Connection: close` is still sent, and opencode usually honours
//! it, but **not always**: measured against 1.18.32, the first `/session/status`
//! on a cold instance arrived complete with `Content-Length` and the socket was
//! then left open, so "read to EOF" hung until the deadline. opencode replies
//! with `Content-Length` and no chunking, so chunked transfer-encoding is still
//! not implemented - see NOTES.md.
//!
//! # The peer is not trusted
//!
//! The server on the other end runs inside a container whose agent can run
//! arbitrary commands, so it may be anything. Hence:
//!
//! * a [`Client`] has one absolute **deadline** shared by all its requests,
//!   checked before every read and write, so a server that drips one byte at a
//!   time cannot hold the caller past it (per-read timeouts alone would allow
//!   that indefinitely);
//! * each response is capped at [`MAX_RESPONSE`] and all of a client's responses
//!   together at its byte budget, so memory use per container is bounded;
//! * request paths are validated ([`validate_path`]) so that data taken from one
//!   response can never smuggle extra request lines or headers into the next.
//!
//! Pulling in a full HTTP client crate would have added eight transitive
//! dependencies to do less than this file does.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use crate::auth::Credentials;

/// Largest single response accepted, headers included.
///
/// Session lists on a busy instance are the biggest thing fetched and are far
/// below this.
pub const MAX_RESPONSE: usize = 1024 * 1024;

/// Why a request failed. Deliberately coarse: the caller only ever turns this
/// into "instance unreachable", but the text reaches `--list` for diagnosis.
#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Error("timed out".into()),
            _ => Error(e.to_string()),
        }
    }
}

/// Something that yields a TCP connection to `127.0.0.1:<port>`.
///
/// The real implementation hands out sockets created inside a container's
/// network namespace (see [`crate::ns_socket`]); tests use [`Loopback`], which
/// connects in this process's own namespace.
pub trait Connect {
    /// Connects to `127.0.0.1:port`, giving up at `deadline`.
    fn connect(&mut self, port: u16, deadline: Instant) -> io::Result<TcpStream>;
}

/// Connects in the caller's own network namespace. Test-only: the program
/// itself always connects through sockets from the container's namespace.
#[cfg(test)]
pub struct Loopback;

#[cfg(test)]
impl Connect for Loopback {
    /// A plain loopback connection, bounded by the deadline.
    fn connect(&mut self, port: u16, deadline: Instant) -> io::Result<TcpStream> {
        let remaining = remaining(deadline)?;
        TcpStream::connect_timeout(&std::net::SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(), remaining)
    }
}

/// Sockets created elsewhere - in a container's namespace - used once each.
///
/// Running out is an error rather than a reason to fetch more: the pool size is
/// the cap on how many requests one container can cost.
pub struct Pool(pub Vec<OwnedFd>);

impl Connect for Pool {
    /// Connects the next unused socket.
    fn connect(&mut self, port: u16, deadline: Instant) -> io::Result<TcpStream> {
        let fd = self
            .0
            .pop()
            .ok_or_else(|| io::Error::other("request budget exhausted"))?;
        connect_socket(fd, port, deadline)
    }
}

/// Time left before `deadline`, or a timeout error if none.
fn remaining(deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
    } else {
        Ok(left)
    }
}

/// Connects an existing, unconnected IPv4 TCP socket to `127.0.0.1:port`.
///
/// Whatever network namespace the socket was created in is the one whose
/// loopback it reaches. The connect is non-blocking and waited on with `poll`
/// so it respects the deadline; the socket is returned to blocking mode after.
pub fn connect_socket(fd: OwnedFd, port: u16, deadline: Instant) -> io::Result<TcpStream> {
    let raw = fd.as_raw_fd();
    set_nonblocking(raw, true)?;

    let addr = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr { s_addr: u32::from(Ipv4Addr::LOCALHOST).to_be() },
        sin_zero: [0; 8],
    };
    // SAFETY: `addr` is a valid sockaddr_in of the length passed.
    let r = unsafe {
        libc::connect(
            raw,
            &addr as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if r != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(e);
        }
        crate::ns_socket::wait_for(raw, libc::POLLOUT, deadline)?;
        let mut err: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: SO_ERROR writes one c_int into `err`.
        let r = unsafe {
            libc::getsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut err as *mut libc::c_int as *mut libc::c_void,
                &mut len,
            )
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err));
        }
    }
    set_nonblocking(raw, false)?;
    Ok(TcpStream::from(fd))
}

/// Sets or clears `O_NONBLOCK` on a descriptor.
fn set_nonblocking(fd: i32, on: bool) -> io::Result<()> {
    // SAFETY: F_GETFL/F_SETFL on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = if on { flags | libc::O_NONBLOCK } else { flags & !libc::O_NONBLOCK };
        if libc::fcntl(fd, libc::F_SETFL, flags) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Checks a request path is safe to put on the request line.
///
/// Only printable ASCII without spaces is allowed, and it must start with `/`.
/// That excludes CR and LF, so nothing can terminate the request line early and
/// inject headers or a second request - which matters because some paths are
/// built from data the (untrusted) server itself returned.
pub fn validate_path(path: &str) -> Result<(), Error> {
    if !path.starts_with('/') {
        return Err(Error(format!("refusing request path not starting with '/': {path:?}")));
    }
    if !path.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(Error(format!("refusing unsafe request path: {path:?}")));
    }
    Ok(())
}

/// Makes GET requests against one opencode instance within a fixed budget.
///
/// All requests share one deadline and one byte budget, so however the peer
/// behaves, one container costs at most that much time and memory.
pub struct Client<C: Connect> {
    connector: C,
    deadline: Instant,
    bytes_left: usize,
    /// Precomputed `Authorization` header value, if credentials were given.
    authorization: Option<String>,
}

impl<C: Connect> Client<C> {
    /// A client that must finish every request by `deadline` and may read at
    /// most `byte_budget` bytes in total.
    pub fn new(connector: C, deadline: Instant, byte_budget: usize) -> Self {
        Client { connector, deadline, bytes_left: byte_budget, authorization: None }
    }

    /// Sends these credentials with every request.
    pub fn with_credentials(mut self, credentials: Option<&Credentials>) -> Self {
        self.authorization = credentials.map(Credentials::header_value);
        self
    }

    /// Performs `GET path` against `127.0.0.1:port` and returns the response body.
    ///
    /// Fails if the status line is not 200, since every endpoint used here returns
    /// 200 on success and there is nothing useful to do with another code.
    pub fn get(&mut self, port: u16, path: &str) -> Result<Vec<u8>, Error> {
        validate_path(path)?;
        let mut stream = self.connector.connect(port, self.deadline)?;
        stream.set_nodelay(true)?;

        let authorization = match &self.authorization {
            Some(value) => format!("Authorization: {value}\r\n"),
            None => String::new(),
        };
        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Accept: application/json\r\n\
             User-Agent: opencode-podman-status\r\n\
             {authorization}\
             Connection: close\r\n\
             \r\n"
        );
        stream.set_write_timeout(Some(remaining(self.deadline)?))?;
        stream.write_all(request.as_bytes())?;
        stream.flush()?;

        let limit = MAX_RESPONSE.min(self.bytes_left);
        let raw = read_bounded(&mut stream, self.deadline, limit)?;
        self.bytes_left -= raw.len();
        split_response(&raw)
    }
}

/// Reads one response: up to its `Content-Length` if it has one, else to EOF.
///
/// Fails at `deadline` or once more than `limit` bytes arrive. The deadline is
/// re-applied before every read, which is what bounds the total time rather than
/// just the gap between packets.
fn read_bounded<R: Read + ReadTimeout>(stream: &mut R, deadline: Instant, limit: usize) -> Result<Vec<u8>, Error> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    // Total length once the headers have been seen and carried a Content-Length.
    let mut expected: Option<usize> = None;
    loop {
        if expected.is_some_and(|total| raw.len() >= total) {
            return Ok(raw);
        }
        stream.set_timeout(remaining(deadline)?)?;
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(raw),
            Ok(n) => {
                if raw.len() + n > limit {
                    return Err(Error(format!("response larger than {limit} bytes")));
                }
                raw.extend_from_slice(&chunk[..n]);
                if expected.is_none() {
                    expected = expected_total_len(&raw)?;
                    if expected.is_some_and(|total| total > limit) {
                        return Err(Error(format!("response larger than {limit} bytes")));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Total response length implied by the headers, once they are complete.
///
/// `Ok(None)` means "headers not complete yet, or no Content-Length: read to
/// EOF". A Content-Length that is not a plain decimal number, or two that
/// disagree, is an error rather than a guess (RFC 9112 §6.3).
fn expected_total_len(raw: &[u8]) -> Result<Option<usize>, Error> {
    let Some(end) = find_header_end(raw) else { return Ok(None) };
    let body_start = end + header_terminator_len(raw, end);
    let head = String::from_utf8_lossy(&raw[..end]);
    let mut length: Option<usize> = None;
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else { continue };
        if !name.trim().eq_ignore_ascii_case("content-length") {
            continue;
        }
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error(format!("malformed Content-Length: {value:?}")));
        }
        let n: usize = value.parse().map_err(|_| Error("Content-Length too large".into()))?;
        if length.is_some_and(|previous| previous != n) {
            return Err(Error("conflicting Content-Length headers".into()));
        }
        length = Some(n);
    }
    Ok(length.map(|n| body_start.saturating_add(n)))
}

/// A readable stream whose read timeout can be set, so [`read_bounded`] can be
/// exercised against both real sockets and test doubles.
trait ReadTimeout {
    /// Sets the timeout for the next read.
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()>;
}

impl ReadTimeout for TcpStream {
    /// Delegates to the socket's own read timeout.
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }
}

/// Splits a raw HTTP response into its body, checking the status line.
///
/// Separated from the socket work so it can be unit-tested without a server.
fn split_response(raw: &[u8]) -> Result<Vec<u8>, Error> {
    let split = find_header_end(raw)
        .ok_or_else(|| Error("malformed response: no header terminator".into()))?;
    let (head, body) = raw.split_at(split);

    let status_line = head
        .split(|&b| b == b'\n')
        .next()
        .ok_or_else(|| Error("malformed response: no status line".into()))?;
    let status_line = String::from_utf8_lossy(status_line).trim_end().to_string();

    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| Error(format!("malformed status line: {status_line:?}")))?;

    if code == 401 {
        // The one failure worth spelling out: it has a fix the user can apply.
        return Err(Error(
            "HTTP 401: authentication required or wrong password \
             (see --password-file / --username)"
                .into(),
        ));
    }
    if code != 200 {
        return Err(Error(format!("HTTP {code}")));
    }

    // Skip the blank line that terminated the headers.
    Ok(body[header_terminator_len(raw, split)..].to_vec())
}

/// Finds the offset of the blank line ending the headers.
///
/// Accepts a bare `\n\n` as well as `\r\n\r\n`: nothing we talk to emits the
/// former, but tolerating it costs nothing and avoids a confusing failure.
fn find_header_end(raw: &[u8]) -> Option<usize> {
    if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some(i);
    }
    raw.windows(2).position(|w| w == b"\n\n")
}

/// Length of the header terminator found at `offset`.
fn header_terminator_len(raw: &[u8], offset: usize) -> usize {
    if raw[offset..].starts_with(b"\r\n\r\n") {
        4
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::thread;

    /// A response captured verbatim from opencode 1.18.32.
    const REAL_HEALTH_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\n\
        Content-Type: application/json\r\n\
        Date: Fri, 25 Sep 2026 04:02:11 GMT\r\n\
        Content-Length: 36\r\n\
        Vary: Origin\r\n\
        \r\n\
        {\"healthy\":true,\"version\":\"1.18.32\"}";

    /// A client over plain loopback with a generous budget, for tests.
    fn client(timeout: Duration) -> Client<Loopback> {
        Client::new(Loopback, Instant::now() + timeout, 4 * MAX_RESPONSE)
    }

    /// Serves one connection with `respond`, returning the request head it read.
    fn serve_once<F>(respond: F) -> (u16, thread::JoinHandle<String>)
    where
        F: FnOnce(&mut TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut reader = std::io::BufReader::new(sock.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            respond(&mut sock);
            head
        });
        (port, handle)
    }

    /// The real captured response parses to exactly its JSON body.
    #[test]
    fn parses_real_opencode_response() {
        let body = split_response(REAL_HEALTH_RESPONSE).expect("should parse");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "{\"healthy\":true,\"version\":\"1.18.32\"}"
        );
    }

    /// An empty body is valid - several endpoints legitimately return `{}`.
    #[test]
    fn parses_empty_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(split_response(raw).unwrap(), Vec::<u8>::new());
    }

    /// A bare-LF header terminator is tolerated.
    #[test]
    fn tolerates_lf_only_headers() {
        let raw = b"HTTP/1.1 200 OK\nContent-Type: application/json\n\n{}";
        assert_eq!(split_response(raw).unwrap(), b"{}".to_vec());
    }

    /// Non-200 responses are errors, reported with the code.
    #[test]
    fn rejects_non_200_status() {
        for (raw, want) in [
            (&b"HTTP/1.1 404 Not Found\r\n\r\n"[..], "HTTP 404"),
            (&b"HTTP/1.1 500 Internal Server Error\r\n\r\n"[..], "HTTP 500"),
        ] {
            let err = split_response(raw).expect_err("should reject");
            assert_eq!(err.to_string(), want);
        }
    }

    /// A 401 says what to do about it.
    #[test]
    fn explains_401() {
        let err = split_response(b"HTTP/1.1 401 Unauthorized\r\n\r\n").unwrap_err();
        assert!(err.to_string().starts_with("HTTP 401: authentication required"), "{err}");
        assert!(err.to_string().contains("--password-file"), "{err}");
    }

    /// Credentials, when given, go out as a Basic `Authorization` header; when
    /// not, no such header is sent at all.
    #[test]
    fn sends_basic_auth_only_when_configured() {
        let creds = Credentials::new("opencode", "open sesame").unwrap();
        let (port, server) = serve_once(|sock| sock.write_all(REAL_HEALTH_RESPONSE).unwrap());
        client(Duration::from_secs(5))
            .with_credentials(Some(&creds))
            .get(port, "/global/health")
            .expect("request");
        let head = server.join().unwrap();
        assert!(
            head.contains(&format!("Authorization: Basic {}\r\n", crate::auth::base64(b"opencode:open sesame"))),
            "head: {head:?}"
        );

        let (port, server) = serve_once(|sock| sock.write_all(REAL_HEALTH_RESPONSE).unwrap());
        client(Duration::from_secs(5)).with_credentials(None).get(port, "/global/health").unwrap();
        assert!(!server.join().unwrap().contains("Authorization"));
    }

    /// Garbage that is not an HTTP response at all is rejected, not guessed at.
    #[test]
    fn rejects_malformed_responses() {
        assert!(split_response(b"").is_err());
        assert!(split_response(b"not http at all").is_err());
        assert!(split_response(b"HTTP/1.1\r\n\r\n").is_err());
        assert!(split_response(b"nonsense\r\n\r\nbody").is_err());
    }

    /// Bodies containing the header terminator sequence are not truncated at it.
    #[test]
    fn does_not_split_on_terminator_inside_body() {
        let raw = b"HTTP/1.1 200 OK\r\n\r\n{\"a\":\"x\\r\\n\\r\\ny\"}";
        let body = split_response(raw).unwrap();
        assert_eq!(String::from_utf8(body).unwrap(), "{\"a\":\"x\\r\\n\\r\\ny\"}");
    }

    /// Ordinary API paths, including query strings, are accepted.
    #[test]
    fn accepts_ordinary_paths() {
        for path in ["/global/health", "/session/status", "/session/ses_abc123/message?limit=1"] {
            assert!(validate_path(path).is_ok(), "{path}");
        }
    }

    /// Anything that could end the request line early or smuggle headers is
    /// refused: CR, LF, spaces, other control characters and non-ASCII.
    #[test]
    fn refuses_injection_in_paths() {
        for path in [
            "/session/x\r\nX-Injected: 1",
            "/session/x\n",
            "/session/x HTTP/1.1",
            "/session/\u{0}",
            "/session/\t",
            "/session/\u{7f}",
            "/session/\u{e9}",
            "session/no-leading-slash",
            "",
        ] {
            assert!(validate_path(path).is_err(), "should refuse {path:?}");
        }
    }

    /// An unsafe path is refused before any connection is made.
    #[test]
    fn get_refuses_unsafe_path_without_connecting() {
        /// A connector that fails the test if used.
        struct MustNotConnect;
        impl Connect for MustNotConnect {
            /// Panics: validation should have stopped the request first.
            fn connect(&mut self, _: u16, _: Instant) -> io::Result<TcpStream> {
                panic!("connected despite an unsafe path");
            }
        }
        let mut c = Client::new(MustNotConnect, Instant::now() + Duration::from_secs(1), 1024);
        assert!(c.get(1, "/x\r\nHost: evil").is_err());
    }

    /// End-to-end against a real socket: confirms the request line, the headers
    /// we promise to send, and that the body comes back intact.
    #[test]
    fn performs_a_real_request_over_tcp() {
        let (port, server) = serve_once(|sock| {
            sock.write_all(REAL_HEALTH_RESPONSE).unwrap();
        });
        let body = client(Duration::from_secs(5)).get(port, "/global/health").expect("request");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "{\"healthy\":true,\"version\":\"1.18.32\"}"
        );
        let head = server.join().expect("server thread");
        assert!(head.starts_with("GET /global/health HTTP/1.1\r\n"), "head: {head:?}");
        assert!(head.contains("Connection: close\r\n"), "head: {head:?}");
        assert!(head.contains(&format!("Host: 127.0.0.1:{port}\r\n")), "head: {head:?}");
    }

    /// A complete response with Content-Length is returned even though the
    /// server keeps the connection open - the behaviour measured on a cold
    /// opencode 1.18.32, which made "read to EOF" hang until the deadline.
    #[test]
    fn stops_at_content_length_when_server_keeps_socket_open() {
        let (port, server) = serve_once(|sock| {
            sock.write_all(REAL_HEALTH_RESPONSE).unwrap();
            // Hold the socket open well past the client's deadline.
            thread::sleep(Duration::from_millis(1500));
        });
        let started = Instant::now();
        let body = client(Duration::from_secs(1)).get(port, "/global/health").expect("no hang");
        assert!(started.elapsed() < Duration::from_millis(900), "took {:?}", started.elapsed());
        assert_eq!(body, br#"{"healthy":true,"version":"1.18.32"}"#.to_vec());
        server.join().unwrap();
    }

    /// Content-Length is found case-insensitively and only once headers end.
    #[test]
    fn computes_expected_length_from_headers() {
        assert_eq!(expected_total_len(b"HTTP/1.1 200 OK\r\nContent-Len").unwrap(), None);
        assert_eq!(expected_total_len(b"HTTP/1.1 200 OK\r\n\r\n").unwrap(), None);
        assert_eq!(
            expected_total_len(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nab").unwrap(),
            Some(43)
        );
        assert_eq!(expected_total_len(REAL_HEALTH_RESPONSE).unwrap(), Some(REAL_HEALTH_RESPONSE.len()));
    }

    /// Malformed or conflicting lengths are refused rather than guessed at.
    #[test]
    fn refuses_malformed_content_length() {
        for raw in [
            &b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n"[..],
            b"HTTP/1.1 200 OK\r\nContent-Length: 1e3\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length:\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999999\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
        ] {
            assert!(expected_total_len(raw).is_err(), "{:?}", String::from_utf8_lossy(raw));
        }
    }

    /// A declared length over the cap fails at once, before the body is read.
    #[test]
    fn refuses_oversized_declared_length_early() {
        let (port, _server) = serve_once(|sock| {
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n");
            thread::sleep(Duration::from_millis(1500));
        });
        let started = Instant::now();
        let err = client(Duration::from_secs(1)).get(port, "/big").expect_err("too big");
        assert!(err.to_string().contains("larger than"), "{err}");
        assert!(started.elapsed() < Duration::from_millis(900));
    }

    /// A server that trickles bytes forever is cut off at the deadline, even
    /// though every individual read succeeds well within any per-read timeout.
    #[test]
    fn slow_drip_server_hits_the_overall_deadline() {
        let (port, _server) = serve_once(|sock| {
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
            for _ in 0..100 {
                if sock.write_all(b" ").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        let err = client(Duration::from_millis(400))
            .get(port, "/global/health")
            .expect_err("should time out");
        assert_eq!(err.to_string(), "timed out");
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
    }

    /// A response over the per-response cap is refused, not truncated and parsed.
    #[test]
    fn oversized_response_is_refused() {
        let (port, _server) = serve_once(|sock| {
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..(MAX_RESPONSE / chunk.len() + 2) {
                if sock.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
        let err = client(Duration::from_secs(5)).get(port, "/big").expect_err("too big");
        assert!(err.to_string().contains("larger than"), "{err}");
    }

    /// The byte budget spans requests: once spent, even a small response fails.
    #[test]
    fn byte_budget_is_shared_across_requests() {
        let respond = |sock: &mut TcpStream| {
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\n\r\n0123456789");
        };
        let mut c = Client::new(Loopback, Instant::now() + Duration::from_secs(5), 40);
        let (port, s) = serve_once(respond);
        assert!(c.get(port, "/a").is_ok());
        s.join().unwrap();
        let (port, s) = serve_once(respond);
        let err = c.get(port, "/b").expect_err("budget spent");
        assert!(err.to_string().contains("larger than"), "{err}");
        let _ = s.join();
    }

    /// Connecting to a port with nothing on it fails rather than hanging.
    #[test]
    fn fails_when_nothing_is_listening() {
        // Bind then drop, so the port is almost certainly free and unowned.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(client(Duration::from_millis(500)).get(port, "/global/health").is_err());
    }

    /// A pre-created socket handed in from elsewhere is connected and used, and
    /// once the pool is empty further requests fail instead of opening more.
    #[test]
    fn pool_uses_each_socket_once() {
        let (port, server) = serve_once(|sock| {
            sock.write_all(REAL_HEALTH_RESPONSE).unwrap();
        });
        // SAFETY: a fresh socket owned by nobody else.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        assert!(fd >= 0);
        // SAFETY: fd was just created.
        let fd = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let mut c = Client::new(Pool(vec![fd]), Instant::now() + Duration::from_secs(5), MAX_RESPONSE);
        assert!(c.get(port, "/global/health").is_ok());
        server.join().unwrap();
        let err = c.get(port, "/global/health").expect_err("pool empty");
        assert!(err.to_string().contains("budget exhausted"), "{err}");
    }

    /// Connecting a pre-created socket to a closed port reports the refusal.
    #[test]
    fn connect_socket_reports_refusal() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        // SAFETY: a fresh socket owned by nobody else.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        // SAFETY: fd was just created.
        let fd = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let err = connect_socket(fd, port, Instant::now() + Duration::from_secs(2)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
    }

    /// An already-expired deadline fails immediately rather than blocking.
    #[test]
    fn expired_deadline_fails_immediately() {
        let mut c = Client::new(Loopback, Instant::now(), MAX_RESPONSE);
        let err = c.get(1, "/global/health").expect_err("expired");
        assert_eq!(err.to_string(), "timed out");
    }
}
