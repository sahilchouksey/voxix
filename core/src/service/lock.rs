//! Advisory instance lock (`service.lock`, SDK plan §3.2): one listener per
//! data directory. On Unix it is a `flock(2)` advisory lock — kernel-released
//! on crash and on graceful shutdown. On Windows it is a named `Local\…`
//! mutex, likewise released by the kernel when the process exits.

#[cfg(unix)]
mod unix {
    use libc::{flock, LOCK_EX, LOCK_NB, LOCK_UN};
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    pub struct InstanceLockImpl {
        file: std::sync::Mutex<Option<std::fs::File>>,
    }

    pub fn acquire(state_dir: &Path) -> Result<InstanceLockImpl, String> {
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
        Ok(InstanceLockImpl {
            file: std::sync::Mutex::new(Some(file)),
        })
    }

    impl InstanceLockImpl {
        fn release(&self) {
            if let Some(file) = self.file.lock().unwrap_or_else(|e| e.into_inner()).take() {
                unsafe {
                    let _ = flock(file.as_raw_fd(), LOCK_UN);
                }
            }
        }
    }

    impl Drop for InstanceLockImpl {
        fn drop(&mut self) {
            self.release();
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::path::Path;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

    pub struct InstanceLockImpl {
        handle: std::sync::Mutex<Option<windows::Win32::Foundation::HANDLE>>,
    }

    fn mutex_name_for(state_dir: &Path) -> windows::core::HSTRING {
        // FNV-1a 64 over the canonical (lowercased) path — a stable, unique,
        // path-safe mutex name that survives no-restart crashes.
        let path = state_dir.to_string_lossy().to_lowercase();
        let mut hash = 0xcbf29ce484222325u64;
        for byte in path.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        windows::core::HSTRING::from(format!("Local\\VoxixServiceLock_{hash:016x}"))
    }

    pub fn acquire(state_dir: &Path) -> Result<InstanceLockImpl, String> {
        let _ = std::fs::create_dir_all(state_dir);
        let name = mutex_name_for(state_dir);
        let handle = unsafe {
            CreateMutexW(None, true, &name).map_err(|e| format!("cannot create instance mutex: {e}"))?
        };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // The mutex already existed — check whether it is currently owned.
            // WAIT_TIMEOUT means another voxix serve is alive; WAIT_OBJECT_0
            // means the previous owner died without cleaning up (abandoned
            // mutex) — take ownership of the same name.
            let outcome = unsafe { WaitForSingleObject(handle, 0) };
            if outcome == WAIT_TIMEOUT {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(
                    "another voxix serve holds this instance (one listener per data directory; exit code 73)".to_string(),
                );
            }
        }
        Ok(InstanceLockImpl {
            handle: std::sync::Mutex::new(Some(handle)),
        })
    }

    impl Drop for InstanceLockImpl {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.lock().unwrap_or_else(|e| e.into_inner()).take() {
                unsafe {
                    let _ = CloseHandle(handle);
                }
            }
        }
    }
}

use std::path::Path as StdPath;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

pub struct InstanceLock {
    _inner: platform::InstanceLockImpl,
}

#[derive(Debug)]
pub enum LockAcquisition {
    Acquired,
}

pub fn acquire(state_dir: &StdPath) -> Result<InstanceLock, String> {
    Ok(InstanceLock {
        _inner: platform::acquire(state_dir)?,
    })
}
