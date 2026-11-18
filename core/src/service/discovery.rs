//! Discovery document (SDK plan §15 G2/G3).
//!
//! `state/discovery.json` publishes `{port, pid, protocol_version,
//! token_fingerprint}` — never the plaintext token — written 0600 and
//! atomically (temp file → fsync → rename → best-effort directory sync).
//! It is a **locator**, never a lock: single-instance exclusion is owned by
//! `service.lock` (kernel flock). A stale document never blocks startup;
//! callers re-probe (`alive_probe`) before trusting it, and a
//! crashed process leaves a document the GUI can detect and replace.
//! Graceful shutdown removes the document only when it still describes this
//! process (compare-then-delete, so a replacement server's doc is never
//! clobbered).

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DISCOVERY_FILE_NAME: &str = "discovery.json";
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryDocument {
    pub port: u16,
    pub pid: u32,
    pub protocol_version: u32,
    /// First 32 base-16 characters of the SHA-256 digest of the token. Lets
    /// an SDK confirm the token it holds belongs to this daemon without the
    /// plaintext ever touching disk.
    pub token_fingerprint: String,
}

pub fn document_path(state_dir: &Path) -> PathBuf {
    state_dir.join(DISCOVERY_FILE_NAME)
}

/// Best-effort durability for the containing directory.
fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

/// Another pid is alive?
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // Fallback to POSIX kill(0) — a signal-free liveness check.
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0
}

/// Another pid is alive? (Windows)
#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    // tasklist is always present on Windows and exits 0 with the process
    // line on stdout when found; parsing the CSV output is the cheapest
    // dependency-free answer.
    std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("PID eq {pid}"),
            "/NH",
            "/FO",
            "CSV",
        ])
        .output()
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout);
            text.contains(&format!("\"{pid}\""))
        })
        .unwrap_or(false)
}

/// Read the existing document, if parseable and alive-ish.
pub fn read_document(state_dir: &Path) -> Option<DiscoveryDocument> {
    let path = document_path(state_dir);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Publish the document atomically with 0600 permissions.
pub fn publish(state_dir: &Path, doc: &DiscoveryDocument) -> Result<PathBuf, String> {
    let path = document_path(state_dir);
    let temp = state_dir.join(format!(".{DISCOVERY_FILE_NAME}.tmp.{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&temp)
            .map_err(|e| format!("cannot create {}: {e}", temp.display()))?;
        #[cfg(unix)]
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        let body = serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?;
        file.write_all(body.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    sync_parent_dir(state_dir);
    Ok(path)
}

/// Owned-removal on graceful shutdown: delete only if the document on
/// still points at this pid (a replacement server's doc is never clobbered).
pub fn remove_owned(state_dir: &Path, expected_pid: u32) -> bool {
    match read_document(state_dir) {
        Some(doc) if doc.pid == expected_pid => {
            let path = document_path(state_dir);
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    sync_parent_dir(state_dir);
                    true
                }
                Err(_) => false,
            }
        }
        _ => false,
    }
}

/// Liveness probe for the daemon's unauthenticated `GET /alive` route.
/// Connects to 127.0.0.1:port and expects HTTP 200. Distinguishes our daemon
/// from a squatter on the port, and from a stale doc whose pid was recycled.
pub fn alive_probe(port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(b"GET /alive HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 256];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 4096 || buf.windows(2).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let status = head.split_whitespace().nth(1).unwrap_or("");
    status == "200"
}

/// Liveness helper re-exported for tests and SDK-facing parity.
pub fn pid_alive_public(pid: u32) -> bool {
    pid_alive(pid)
}
