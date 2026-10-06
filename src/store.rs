//! The plugin's on-disk state: a few fixed-name files in herdr's per-plugin
//! state dir. Paths go through a `StateDir` value rather than a global so each
//! test can use its own temp dir.

use std::io::ErrorKind;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

const ENABLED: &str = "enabled";
const PIDFILE: &str = "watcher.pid";
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

#[derive(Debug, PartialEq, Eq)]
pub enum Reserve {
    Reserved,
    Busy,
    Failed,
}

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
        std::fs::write(self.path(PIDFILE), pid.to_string())
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

    /// Atomically claim the pidfile (holding our own pid as a placeholder) so
    /// two concurrent toggles cannot both start a watcher. A pidfile whose pid
    /// fails `is_live` is stale and is taken over. Reservers are serialized by
    /// an exclusive flock on `watcher.lock`, so check-and-claim is atomic.
    pub fn try_reserve_pid(&self, is_live: impl Fn(i32) -> bool) -> Reserve {
        if std::fs::create_dir_all(&self.root).is_err() {
            return Reserve::Failed;
        }

        // Open and lock the lockfile to serialize check-and-claim.
        let lock_path = self.path(LOCKFILE);
        let lock_file = match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(f) => f,
            Err(_) => return Reserve::Failed,
        };

        // Acquire exclusive lock (released when lock_file is dropped).
        if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Reserve::Failed;
        }

        // Check-and-claim is now atomic (while holding the lock).
        if self.read_pid().is_some_and(&is_live) {
            return Reserve::Busy;
        }

        // Pidfile is either missing or stale; claim it.
        match std::fs::write(self.path(PIDFILE), std::process::id().to_string()) {
            Ok(()) => Reserve::Reserved,
            Err(_) => Reserve::Failed,
        }
        // Lock is released when lock_file is dropped.
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
    fn reserve_then_busy_then_stale_takeover() {
        let dir = temp_dir("reserve");
        assert_eq!(dir.try_reserve_pid(|_| true), Reserve::Reserved);
        assert_eq!(dir.read_pid(), Some(std::process::id() as i32));
        // Held by a live owner -> Busy.
        assert_eq!(dir.try_reserve_pid(|_| true), Reserve::Busy);
        // Owner no longer live -> stale, taken over.
        assert_eq!(dir.try_reserve_pid(|_| false), Reserve::Reserved);
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

    #[test]
    fn concurrent_reserve_serialized_by_lock() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = Arc::new(temp_dir("concurrent"));

        // Write a stale pidfile.
        dir.write_pid(999_999_999).unwrap();

        let reserved_count = Arc::new(AtomicUsize::new(0));
        let busy_count = Arc::new(AtomicUsize::new(0));

        // Spawn 8 threads that race to reserve.
        std::thread::scope(|s| {
            for _ in 0..8 {
                let dir = Arc::clone(&dir);
                let reserved_count = Arc::clone(&reserved_count);
                let busy_count = Arc::clone(&busy_count);

                s.spawn(move || {
                    match dir.try_reserve_pid(pid_alive) {
                        Reserve::Reserved => reserved_count.fetch_add(1, Ordering::SeqCst),
                        Reserve::Busy => busy_count.fetch_add(1, Ordering::SeqCst),
                        result => panic!("Unexpected result: {:?}", result),
                    };
                });
            }
        });

        // Exactly one thread should have reserved, the rest should see busy.
        assert_eq!(reserved_count.load(Ordering::SeqCst), 1);
        assert_eq!(busy_count.load(Ordering::SeqCst), 7);

        // The pidfile should contain the current process's pid.
        assert_eq!(dir.read_pid(), Some(std::process::id() as i32));
    }
}
