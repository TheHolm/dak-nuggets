//! HTTP Basic credentials for an opencode server started with
//! `OPENCODE_SERVER_PASSWORD`.
//!
//! opencode checks `Authorization: Basic base64(username:password)`, with the
//! username defaulting to `opencode` (or `OPENCODE_SERVER_USERNAME`). One set of
//! credentials is used for every container.
//!
//! The password can be given on the command line or in a file. The command line
//! is convenient but **visible to every local user** through `ps` and
//! `/proc/<pid>/cmdline`, on every DAK refresh, and it also sits in DAK's
//! `config.json`; the file is the better choice, and a file that other users can
//! read earns a warning.
//!
//! The credentials never enter a container's namespace: only the parent sends
//! requests (see [`crate::ns_socket`]).

use std::fs;
use std::os::unix::fs::PermissionsExt;

/// Username opencode expects when `OPENCODE_SERVER_USERNAME` is not set.
pub const DEFAULT_USERNAME: &str = "opencode";

/// Longest password file accepted. Passwords are short; this stops a mistaken
/// path (a log, a device) being read into memory wholesale.
const MAX_PASSWORD_FILE: u64 = 4096;

/// A username and password, deliberately not printable with `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    username: String,
    password: String,
}

impl std::fmt::Debug for Credentials {
    /// Shows the username only, so a stray debug print cannot leak the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl Credentials {
    /// Validates and builds credentials.
    ///
    /// An empty password is refused because opencode treats it as "no
    /// authentication" - sending one would be meaningless. A username containing
    /// `:` cannot be expressed in Basic auth (RFC 7617 §2), and control
    /// characters in either part are refused as almost certainly a mistake.
    pub fn new(username: &str, password: &str) -> Result<Self, String> {
        if password.is_empty() {
            return Err("password is empty".to_string());
        }
        if username.is_empty() {
            return Err("username is empty".to_string());
        }
        if username.contains(':') {
            return Err("username must not contain ':'".to_string());
        }
        if username.chars().chain(password.chars()).any(char::is_control) {
            return Err("username and password must not contain control characters".to_string());
        }
        Ok(Credentials { username: username.to_string(), password: password.to_string() })
    }

    /// The value of the `Authorization` header for these credentials.
    ///
    /// Base64 output is `[A-Za-z0-9+/=]` only, so this can never break the header
    /// line whatever the password contains.
    pub fn header_value(&self) -> String {
        format!("Basic {}", base64(format!("{}:{}", self.username, self.password).as_bytes()))
    }
}

/// Standard base64 (RFC 4648 §4) with padding.
///
/// Hand-rolled to avoid a dependency for twenty lines.
pub fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Strips exactly one trailing line ending (`\n` or `\r\n`), as editors and
/// `echo` add one. Any other whitespace is kept: it may be part of the password.
fn strip_one_newline(s: &str) -> &str {
    s.strip_suffix("\r\n").or_else(|| s.strip_suffix('\n')).unwrap_or(s)
}

/// A warning if a file's permission bits let group or others read it.
fn permission_warning(path: &str, mode: u32) -> Option<String> {
    if mode & 0o044 != 0 {
        Some(format!(
            "warning: password file {path} is readable by other users (mode {:o}); \
             consider chmod 600",
            mode & 0o777
        ))
    } else {
        None
    }
}

/// Reads a password from `path`.
///
/// Returns the password and, separately, any warning about the file's
/// permissions, so the caller decides where warnings go.
pub fn read_password_file(path: &str) -> Result<(String, Option<String>), String> {
    let meta = fs::metadata(path).map_err(|e| format!("cannot read password file {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("password file {path} is not a regular file"));
    }
    if meta.len() > MAX_PASSWORD_FILE {
        return Err(format!("password file {path} is larger than {MAX_PASSWORD_FILE} bytes"));
    }
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read password file {path}: {e}"))?;
    let warning = permission_warning(path, meta.permissions().mode());
    Ok((strip_one_newline(&text).to_string(), warning))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The RFC 4648 §10 test vectors.
    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want, "input {input:?}");
        }
    }

    /// All 256 byte values encode to the standard alphabet only, including the
    /// two characters (`+`, `/`) that differ from the URL-safe variant.
    #[test]
    fn base64_uses_the_standard_alphabet() {
        let all: Vec<u8> = (0..=255).collect();
        let encoded = base64(&all);
        assert!(encoded.contains('+') && encoded.contains('/'));
        assert!(encoded.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)));
        assert_eq!(encoded.len(), 344);
    }

    /// The header is the RFC 7617 example's shape.
    #[test]
    fn builds_basic_authorization_header() {
        let c = Credentials::new("Aladdin", "open sesame").unwrap();
        assert_eq!(c.header_value(), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }

    /// A password containing `:` is fine - only the username may not.
    #[test]
    fn password_may_contain_colons() {
        let c = Credentials::new(DEFAULT_USERNAME, "a:b").unwrap();
        assert_eq!(c.header_value(), format!("Basic {}", base64(b"opencode:a:b")));
    }

    /// Credentials opencode could never accept are refused up front.
    #[test]
    fn refuses_unusable_credentials() {
        assert!(Credentials::new("opencode", "").is_err());
        assert!(Credentials::new("", "pw").is_err());
        assert!(Credentials::new("a:b", "pw").is_err());
        assert!(Credentials::new("opencode", "pw\r\nX-Injected: 1").is_err());
        assert!(Credentials::new("op\u{1b}", "pw").is_err());
    }

    /// Debug output never contains the password.
    #[test]
    fn debug_output_redacts_the_password() {
        let c = Credentials::new("opencode", "hunter2").unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("opencode"), "{shown}");
    }

    /// Exactly one trailing line ending is removed; other whitespace is kept.
    #[test]
    fn strips_exactly_one_newline() {
        assert_eq!(strip_one_newline("pw\n"), "pw");
        assert_eq!(strip_one_newline("pw\r\n"), "pw");
        assert_eq!(strip_one_newline("pw\n\n"), "pw\n");
        assert_eq!(strip_one_newline(" pw "), " pw ");
        assert_eq!(strip_one_newline("pw"), "pw");
    }

    /// Group- or world-readable files draw a warning; owner-only ones do not.
    #[test]
    fn warns_about_readable_password_files() {
        assert!(permission_warning("f", 0o100600).is_none());
        assert!(permission_warning("f", 0o100400).is_none());
        assert!(permission_warning("f", 0o100640).unwrap().contains("640"));
        assert!(permission_warning("f", 0o100604).is_some());
    }

    /// Writes `contents` to a fresh temporary file with `mode`, returning its path.
    fn temp_file(name: &str, contents: &[u8], mode: u32) -> String {
        let dir = std::env::temp_dir().join(format!("ops-auth-{}-{name}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("pw");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// A real file is read, its newline stripped, and its mode judged.
    #[test]
    fn reads_password_files() {
        let path = temp_file("ok", b"s3cret\n", 0o600);
        assert_eq!(read_password_file(&path).unwrap(), ("s3cret".to_string(), None));
        let path = temp_file("loose", b"s3cret", 0o644);
        let (pw, warning) = read_password_file(&path).unwrap();
        assert_eq!(pw, "s3cret");
        assert!(warning.is_some());
    }

    /// Missing files, directories and oversized files are errors.
    #[test]
    fn rejects_unusable_password_files() {
        assert!(read_password_file("/nonexistent/password").is_err());
        assert!(read_password_file("/").is_err());
        let path = temp_file("big", &vec![b'x'; 5000], 0o600);
        assert!(read_password_file(&path).unwrap_err().contains("larger than"));
    }
}
