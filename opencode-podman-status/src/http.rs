//! A deliberately minimal HTTP/1.1 GET client.
//!
//! Every request this program makes is a plain GET to `127.0.0.1` *inside* a
//! container's network namespace. That makes almost everything a general HTTP
//! client handles irrelevant: no TLS, no redirects, no proxies, no
//! authentication, no keep-alive pooling.
//!
//! Sending `Connection: close` means the server closes the socket when the body
//! is complete, so the body is simply "everything until EOF". That removes the
//! need to implement chunked transfer-encoding or `Content-Length` handling
//! entirely. Verified against opencode 1.18.32, which honours it and replies
//! with `Content-Length` and no chunking - see NOTES.md.
//!
//! Pulling in a full HTTP client crate would have added eight transitive
//! dependencies to do less than this file does.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::time::Duration;

/// Largest response body accepted, as a guard against a runaway peer.
///
/// Session lists on a busy instance are the biggest thing we fetch and are
/// nowhere near this.
const MAX_BODY: usize = 8 * 1024 * 1024;

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

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

/// Performs `GET path` against `127.0.0.1:port` and returns the response body.
///
/// `timeout` bounds connect, read and write independently, so a wedged instance
/// cannot stall the caller: DAK re-invokes this program on a timer and must never
/// be left waiting.
///
/// Fails if the status line is not 200, since every endpoint used here returns
/// 200 on success and there is nothing useful to do with another code.
pub fn get(port: u16, path: &str, timeout: Duration) -> Result<Vec<u8>, Error> {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let mut stream = TcpStream::connect_timeout(&addr.into(), timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_nodelay(true)?;

    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Accept: application/json\r\n\
         User-Agent: opencode-podman-status\r\n\
         Connection: close\r\n\
         \r\n"
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut raw = Vec::new();
    // Bound the read so a peer that never closes cannot exhaust memory.
    let mut limited = stream.take(MAX_BODY as u64);
    limited.read_to_end(&mut raw)?;

    split_response(&raw)
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
            (&b"HTTP/1.1 401 Unauthorized\r\n\r\n"[..], "HTTP 401"),
            (&b"HTTP/1.1 500 Internal Server Error\r\n\r\n"[..], "HTTP 500"),
        ] {
            let err = split_response(raw).expect_err("should reject");
            assert_eq!(err.to_string(), want);
        }
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

    /// End-to-end against a real socket: confirms the request line, the headers
    /// we promise to send, and that the body comes back intact.
    #[test]
    fn performs_a_real_request_over_tcp() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            // Read the request head so we can assert on it.
            let mut reader = std::io::BufReader::new(sock.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            sock.write_all(REAL_HEALTH_RESPONSE).unwrap();
            // Closing is what signals end-of-body to the client.
            drop(sock);
            head
        });

        let body = get(port, "/global/health", Duration::from_secs(5)).expect("request");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "{\"healthy\":true,\"version\":\"1.18.32\"}"
        );

        let head = server.join().expect("server thread");
        assert!(head.starts_with("GET /global/health HTTP/1.1\r\n"), "head: {head:?}");
        assert!(head.contains("Connection: close\r\n"), "head: {head:?}");
        assert!(head.contains(&format!("Host: 127.0.0.1:{port}\r\n")), "head: {head:?}");
    }

    /// Connecting to a port with nothing on it fails rather than hanging.
    #[test]
    fn fails_when_nothing_is_listening() {
        // Bind then drop, so the port is almost certainly free and unowned.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(get(port, "/global/health", Duration::from_millis(500)).is_err());
    }
}
