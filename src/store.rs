//! The plugin's on-disk state: a few fixed-name files in herdr's per-plugin
//! state dir. Paths go through a `StateDir` value rather than a global so each
//! test can use its own temp dir.

use std::io::ErrorKind;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ENABLED: &str = "enabled";
const PIDFILE: &str = "watcher.pid";
const PIDFILE_TMP: &str = ".watcher.pid.tmp";
const LOCKFILE: &str = "watcher.lock";
const STATUSLINE: &str = "statusline";
const STATUSLINE_TMP: &str = ".statusline.tmp";
const LOG: &str = "watcher.log";

pub fn pid_alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) == 0 }
}

pub fn read_pid_at(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn remove_quietly(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Exclusive hold on `watcher.lock`, taken by toggle/startup around a whole
/// stop-or-spawn operation so two of them never interleave. Released on drop.
#[must_use]
pub struct ControlLock(#[allow(dead_code)] std::fs::File); // held for its flock

#[derive(Debug, Clone)]
pub struct StateDir {
    root: PathBuf,
}

impl StateDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// herdr's per-plugin state dir (`HERDR_PLUGIN_STATE_DIR`), or a temp dir
    /// when run outside herdr.
    pub fn from_env() -> Self {
        match std::env::var("HERDR_PLUGIN_STATE_DIR") {
            Ok(dir) if !dir.is_empty() => Self::new(dir),
            _ => Self::new(std::env::temp_dir().join("herdr-espresso")),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn log_path(&self) -> PathBuf {
        self.path(LOG)
    }

    /// The persisted switch: present = the user turned keep-awake on.
    pub fn enabled(&self) -> bool {
        self.path(ENABLED).exists()
    }

    pub fn set_enabled(&self, on: bool) -> std::io::Result<()> {
        if on {
            std::fs::create_dir_all(&self.root)?;
            std::fs::write(self.path(ENABLED), b"")
        } else {
            match std::fs::remove_file(self.path(ENABLED)) {
                Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }

    pub fn read_pid(&self) -> Option<i32> {
        read_pid_at(&self.path(PIDFILE))
    }

    pub fn write_pid(&self, pid: i32) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        // Atomic, so a poller never reads a half-written pid.
        let tmp = self.path(PIDFILE_TMP);
        std::fs::write(&tmp, pid.to_string())?;
        std::fs::rename(&tmp, self.path(PIDFILE))
    }

    pub fn remove_pid(&self) {
        remove_quietly(&self.path(PIDFILE));
    }

    /// Remove the pidfile only while it still names `pid`, so an exiting
    /// watcher never deletes the pidfile of the watcher that replaced it.
    pub fn remove_pid_if(&self, pid: i32) {
        if self.read_pid() == Some(pid) {
            self.remove_pid();
        }
    }

    /// Take the exclusive control lock, retrying every 50 ms for up to `wait`
    /// (`TimedOut` after that). The watcher never holds it, so a stuck holder
    /// can only be another toggle/startup, which is itself bounded.
    pub fn lock(&self, wait: Duration) -> std::io::Result<ControlLock> {
        std::fs::create_dir_all(&self.root)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.path(LOCKFILE))?;
        let deadline = Instant::now() + wait;
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(ControlLock(file));
            }
            let err = std::io::Error::last_os_error();
            match err.kind() {
                ErrorKind::Interrupted => continue,
                ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(ErrorKind::TimedOut.into());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => return Err(err),
            }
        }
    }

    /// Replace the tab bar line atomically (write + rename), so the tab bar's
    /// `cat` never reads a half-written file.
    pub fn write_statusline(&self, line: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let tmp = self.path(STATUSLINE_TMP);
        std::fs::write(&tmp, format!("{line}\n"))?;
        std::fs::rename(&tmp, self.path(STATUSLINE))
    }

    pub fn read_statusline(&self) -> Option<String> {
        let s = std::fs::read_to_string(self.path(STATUSLINE)).ok()?;
        Some(s.trim_end().to_string())
    }

    pub fn remove_statusline(&self) {
        remove_quietly(&self.path(STATUSLINE));
    }

    /// Pidfiles left by 0.1.x per-pane watchers (`<pane>.pid`).
    pub fn legacy_pidfiles(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "pid")
                    && p.file_name().is_some_and(|n| n != PIDFILE)
            })
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> StateDir {
        let root = std::env::temp_dir().join(format!("he-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        StateDir::new(root)
    }

    #[test]
    fn enabled_flag_roundtrip() {
        let dir = temp_dir("enabled");
        assert!(!dir.enabled());
        dir.set_enabled(true).unwrap();
        assert!(dir.enabled());
        dir.set_enabled(false).unwrap();
        assert!(!dir.enabled());
        // Turning off twice is not an error.
        dir.set_enabled(false).unwrap();
    }

    #[test]
    fn statusline_is_replaced_atomically_and_removed() {
        let dir = temp_dir("statusline");
        assert_eq!(dir.read_statusline(), None);
        dir.write_statusline("a").unwrap();
        assert_eq!(dir.read_statusline().as_deref(), Some("a"));
        dir.write_statusline("b").unwrap();
        assert_eq!(dir.read_statusline().as_deref(), Some("b"));
        // The temp file used for the atomic swap never lingers.
        assert!(!dir.root().join(".statusline.tmp").exists());
        dir.remove_statusline();
        assert_eq!(dir.read_statusline(), None);
    }

    #[test]
    fn lock_is_exclusive_and_bounded() {
        let dir = temp_dir("lock");
        let held = dir.lock(Duration::from_secs(1)).unwrap();
        let err = dir.lock(Duration::from_millis(200)).err().expect("busy");
        assert_eq!(err.kind(), ErrorKind::TimedOut);
        drop(held);
        let _again = dir.lock(Duration::from_millis(200)).unwrap();
    }

    #[test]
    fn lock_waits_for_a_release() {
        let dir = temp_dir("lock-wait");
        let held = dir.lock(Duration::from_secs(1)).unwrap();
        std::thread::scope(|s| {
            let waiter = s.spawn(|| dir.lock(Duration::from_secs(5)).is_ok());
            std::thread::sleep(Duration::from_millis(150));
            drop(held);
            assert!(waiter.join().unwrap());
        });
    }

    #[test]
    fn write_pid_leaves_no_temp_file() {
        let dir = temp_dir("pid-tmp");
        dir.write_pid(5).unwrap();
        assert_eq!(dir.read_pid(), Some(5));
        assert!(!dir.root().join(".watcher.pid.tmp").exists());
    }

    #[test]
    fn remove_pid_if_only_removes_its_own_pid() {
        let dir = temp_dir("remove-if");
        dir.write_pid(42).unwrap();
        dir.remove_pid_if(7);
        assert_eq!(dir.read_pid(), Some(42));
        dir.remove_pid_if(42);
        assert_eq!(dir.read_pid(), None);
    }

    #[test]
    fn legacy_pidfiles_skip_the_global_pidfile() {
        let dir = temp_dir("legacy");
        std::fs::create_dir_all(dir.root()).unwrap();
        for name in ["w1%3Ap1.pid", "watcher.pid", "w1%3Ap1.log", "statusline"] {
            std::fs::write(dir.root().join(name), "1").unwrap();
        }
        assert_eq!(dir.legacy_pidfiles(), vec![dir.root().join("w1%3Ap1.pid")]);
    }

    #[test]
    fn pid_alive_for_self_only() {
        assert!(pid_alive(std::process::id() as i32));
        assert!(!pid_alive(999_999_999));
    }

    #[test]
    fn pid_alive_guards_against_invalid_pids() {
        assert!(!pid_alive(0));
        assert!(!pid_alive(-1));
    }
}
