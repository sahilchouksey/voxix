//! Advisory instance lock (`service.lock`, SDK plan §3.2): one listener per
//! data directory; released automatically on crash (kernel advisory lock) and
//! on graceful shutdown. Implemented over fcntl F_SETLK — no extra crates.

use libc::{flock, LOCK_EX, LOCK_NB, LOCK_UN};
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::Mutex;

pub struct InstanceLock {
    _file: Mutex<Option<std::fs::File>>,
}

#[derive(Debug)]
pub enum LockAcquisition {
    Acquired,
}

pub fn acquire(state_dir: &Path) -> Result<InstanceLock, String> {
    let path = state_dir.join("service.lock");
    let _ = std::fs::create_dir_all(state_dir);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(&path)
        .map_err(|_| format!("cannot open {}", path.display()))?;
    unsafe {
        if flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) != 0 {
            return Err(format!(
                "another voxix serve holds {} (one listener per data directory; exit code 73)",
                path.display()
            ));
        }
    }
    Ok(InstanceLock {
        _file: Mutex::new(Some(file)),
    })
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        if let Some(file) = self._file.lock().unwrap_or_else(|e| e.into_inner()).take() {
            unsafe {
                let _ = flock(file.as_raw_fd(), LOCK_UN);
            }
        }
    }
}
