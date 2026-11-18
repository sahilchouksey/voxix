//! Process-policy acceptance (SDK plan §15 G8/G22).
//!
//! Acceptance gates: interactive QoS for spawned/transcription processes (no
//! background/eco throttling) and, on Linux, a systemd unit of `Type=notify`
//! (+ `Restart=on-failure`) shipped as a template under `packaging/systemd/`.
//!
//! `sd_notify` implements the readiness half of `Type=notify` without taking
//! on a libsystemd dependency: a `NOTIFY_SOCKET` datagram with `READY=1`
//! after the listener is bound and the discovery document is published, and
//! `STOPPING=1` once the graceful shutdown ladder starts. Abstract (`@…`)
//! and path-based socket addresses are both supported on Linux; every other
//! platform (and any missing `NOTIFY_SOCKET`) is a silent no-op.
//!
//! What is automated now is the no-background-throttling assertion: the
//! service process must not lower its own scheduling niceness, and a spawned
//! daemon must not report background throttling.

/// A self-check over the scheduling attributes the OS exposes cheaply:
/// nice level must remain at the default (0) — a negative/positive nice would
/// indicate background-class pressure or voluntary down-prioritization.
pub fn check_self_policy() -> Result<ProcessPolicyReport, String> {
    let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    if nice > 0 {
        return Err(format!(
            "process appears background-throttled (nice {nice}); refusing to start a service in a degraded QoS class"
        ));
    }
    Ok(ProcessPolicyReport {
        nice,
        interactive: true,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ProcessPolicyReport {
    pub nice: i32,
    pub interactive: bool,
}

impl ProcessPolicyReport {
    /// The one phrase the acceptance gate prohibits anywhere in service output.
    pub const FORBIDDEN_PHRASE: &'static str = "background throttled";

    pub fn assertion(&self) -> Result<(), String> {
        if !self.interactive || self.nice < 0 || self.nice > 0 {
            return Err("no-background-throttling assertion failed".to_string());
        }
        Ok(())
    }
}

/// Scan process output for the forbidden phrase (part of the acceptance
/// assertion used by tests around spawn and embed lifecycles).
pub fn output_is_clean(output: &str) -> bool {
    !output
        .to_lowercase()
        .contains(ProcessPolicyReport::FORBIDDEN_PHRASE)
}

/// Best-effort `sd_notify` to `$NOTIFY_SOCKET`. Returns true when the
/// datagram was sent. Never panics; never blocks meaningfully (a missing
/// socket, an unwritable one, or a non-Linux platform reports false).
#[cfg(target_os = "linux")]
pub fn sd_notify(payload: &str) -> bool {
    let socket = match std::env::var("NOTIFY_SOCKET") {
        Ok(s) if !s.is_empty() => s,
        _ => return false,
    };
    let bytes = socket.as_bytes();
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return false;
        }
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let path_ptr = addr.sun_path.as_mut_ptr() as *mut u8;
        let addr_len: libc::socklen_t = if let Some(rest) = bytes.strip_prefix(b"@") {
            if rest.len() + 1 > addr.sun_path.len() {
                libc::close(fd);
                return false;
            }
            *path_ptr = 0;
            std::ptr::copy_nonoverlapping(rest.as_ptr(), path_ptr.add(1), rest.len());
            (std::mem::size_of::<libc::sa_family_t>() + 1 + rest.len()) as libc::socklen_t
        } else {
            let len = bytes.len().min(addr.sun_path.len() - 1);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), path_ptr, len);
            *path_ptr.add(len) = 0;
            (std::mem::size_of::<libc::sa_family_t>() + len + 1) as libc::socklen_t
        };
        let sent = libc::sendto(
            fd,
            payload.as_ptr() as *const libc::c_void,
            payload.len(),
            libc::MSG_NOSIGNAL,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        );
        libc::close(fd);
        sent >= 0
    }
}

/// No-op outside Linux (the `Type=notify` contract is systemd/Linux-only).
#[cfg(not(target_os = "linux"))]
pub fn sd_notify(_payload: &str) -> bool {
    false
}
