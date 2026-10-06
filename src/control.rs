//! Watcher process control shared by `toggle` and `startup`.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::consts::STOP_WAIT;
use crate::store::{pid_alive, read_pid_at, Reserve, StateDir};

const EXE_NAME: &str = "herdr-espresso";

/// Path of the executable running as `pid` (macOS `proc_pidpath`).
fn exe_path(pid: i32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(PathBuf::from(OsString::from_vec(buf)))
}

pub fn is_named(pid: i32, name: &str) -> bool {
    pid_alive(pid) && exe_path(pid).is_some_and(|p| p.file_name().is_some_and(|n| n == name))
}

/// A live herdr-espresso process. Liveness alone is not enough: after a reboot
/// a stale pidfile's pid can belong to any process, which we must not signal.
pub fn is_ours(pid: i32) -> bool {
    is_named(pid, EXE_NAME)
}

pub fn live_watcher(dir: &StateDir) -> Option<i32> {
    dir.read_pid().filter(|&pid| is_ours(pid))
}

/// SIGTERM `pid`, wait up to `wait` for it to exit, then SIGKILL it. Returns
/// whether it exited on SIGTERM (i.e. ran its own cleanup).
pub fn terminate(pid: i32, wait: Duration) -> bool {
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    false
}

pub fn stop_watcher(dir: &StateDir, pid: i32) {
    if !terminate(pid, STOP_WAIT) {
        // Killed hard, so its own cleanup never ran. Its espresso lease still
        // expires on its own within LEASE.
        dir.remove_statusline();
    }
    dir.remove_pid_if(pid);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Spawn {
    Started(i32),
    AlreadyRunning,
    Failed,
}

/// Start the detached global watcher, unless one is already running.
pub fn spawn_watcher(dir: &StateDir) -> Spawn {
    match dir.try_reserve_pid(is_ours) {
        Reserve::Reserved => {}
        Reserve::Busy => return Spawn::AlreadyRunning,
        Reserve::Failed => return Spawn::Failed,
    }
    let failed = || {
        dir.remove_pid();
        Spawn::Failed
    };
    let Ok(exe) = std::env::current_exe() else {
        return failed();
    };

    // Default both streams to null: a detached watcher must NEVER inherit the
    // caller's tty (Command's default for an unset stream is inherit()).
    // Upgrade to the log file, truncated per start, when it is available.
    let mut cmd = Command::new(exe);
    cmd.arg("watch")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(dir.log_path())
    {
        if let Ok(f2) = f.try_clone() {
            cmd.stderr(Stdio::from(f2));
        }
        cmd.stdout(Stdio::from(f));
    }
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else {
        return failed();
    };
    // A watcher that cannot reach herdr exits at once; catch that here rather
    // than report "on" with nothing running. try_wait (not a liveness probe):
    // our exited child stays a zombie until reaped, and kill(0) succeeds on it.
    std::thread::sleep(Duration::from_millis(100));
    if !matches!(child.try_wait(), Ok(None)) {
        return failed();
    }
    let pid = child.id() as i32;
    if dir.write_pid(pid).is_err() {
        return failed();
    }
    Spawn::Started(pid)
}

/// Stop 0.1.x per-pane watchers and drop their files. On SIGTERM they clear
/// their own `$espresso` pane token, so no marker is left behind.
pub fn sweep_legacy(dir: &StateDir) {
    for path in dir.legacy_pidfiles() {
        if let Some(pid) = read_pid_at(&path) {
            if is_ours(pid) {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("log"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn own_exe_name() -> String {
        std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn identifies_a_process_by_executable_name() {
        let me = std::process::id() as i32;
        assert!(is_named(me, &own_exe_name()));
        assert!(!is_named(me, "definitely-not-this"));
        // The test binary is not herdr-espresso, and a dead pid is nobody.
        assert!(!is_ours(me));
        assert!(!is_ours(999_999_999));
    }

    /// Spawn `sh -c script` and reap it on a thread, so the pid really
    /// disappears once it exits (an unreaped child stays a zombie).
    fn spawn_reaped(script: &str) -> i32 {
        let mut child = Command::new("sh").arg("-c").arg(script).spawn().unwrap();
        let pid = child.id() as i32;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        pid
    }

    #[test]
    fn terminate_stops_a_cooperative_process() {
        let pid = spawn_reaped("exec sleep 60");
        assert!(terminate(pid, Duration::from_secs(2)));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!pid_alive(pid));
    }

    #[test]
    fn terminate_kills_a_process_that_ignores_sigterm() {
        // An ignored disposition survives exec, so `sleep` ignores SIGTERM.
        let pid = spawn_reaped("trap '' TERM; exec sleep 60");
        std::thread::sleep(Duration::from_millis(100)); // let the trap install
        assert!(!terminate(pid, Duration::from_millis(200)));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!pid_alive(pid));
    }

    #[test]
    fn sweep_legacy_removes_old_pidfiles_and_logs_only() {
        let root = std::env::temp_dir().join(format!("he-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = StateDir::new(&root);
        std::fs::write(root.join("w1%3Ap1.pid"), "999999999").unwrap();
        std::fs::write(root.join("w1%3Ap1.log"), "old").unwrap();
        dir.write_pid(999_999_998).unwrap();
        sweep_legacy(&dir);
        assert!(!root.join("w1%3Ap1.pid").exists());
        assert!(!root.join("w1%3Ap1.log").exists());
        assert_eq!(dir.read_pid(), Some(999_999_998));
    }
}
