use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// 由打开的文件描述符持有跨渠道实例锁；字段关闭时内核自动释放锁。
/// Holds the cross-channel instance lock through an open file descriptor; the kernel releases
/// it automatically when the descriptor closes.
pub(crate) struct InstanceGuard {
    _file: File,
}

#[derive(Debug)]
pub(crate) enum AcquireError {
    AlreadyRunning,
    Io(io::Error),
}

impl fmt::Display for AcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => f.write_str("another Oh-My-Tab instance is already running"),
            Self::Io(error) => write!(f, "cannot acquire the instance lock: {error}"),
        }
    }
}

pub(crate) fn acquire() -> Result<InstanceGuard, AcquireError> {
    acquire_at(&lock_path())
}

fn lock_path() -> PathBuf {
    lock_path_in(&std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn lock_path_in(home: &str) -> PathBuf {
    PathBuf::from(home).join(".config/oh-my-tab/instance.lock")
}

fn acquire_at(path: &Path) -> Result<InstanceGuard, AcquireError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(AcquireError::Io)?;
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(AcquireError::Io)?;

    // flock 锁与打开的文件描述符绑定，不会像仅创建锁文件那样在崩溃后留下陈旧状态。
    // flock is tied to the open file descriptor, so a crash cannot leave the stale state caused
    // by using file existence alone as the lock.
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::WouldBlock {
            Err(AcquireError::AlreadyRunning)
        } else {
            Err(AcquireError::Io(error))
        };
    }

    file.set_len(0).map_err(AcquireError::Io)?;
    writeln!(file, "{}", std::process::id()).map_err(AcquireError::Io)?;
    file.flush().map_err(AcquireError::Io)?;

    Ok(InstanceGuard { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_is_shared_by_all_bundle_channels() {
        assert_eq!(
            lock_path_in("/Users/tester"),
            PathBuf::from("/Users/tester/.config/oh-my-tab/instance.lock")
        );
    }

    #[test]
    fn lock_rejects_a_second_holder_and_recovers_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance.lock");

        let first = acquire_at(&path).unwrap();
        assert!(matches!(
            acquire_at(&path),
            Err(AcquireError::AlreadyRunning)
        ));

        drop(first);
        assert!(acquire_at(&path).is_ok());
    }
}
