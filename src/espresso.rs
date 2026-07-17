use std::process::{Child, Command, Stdio};

use crate::consts::LEASE;

#[derive(Debug)]
pub enum EspressoError {
    NotFound,
    Spawn(std::io::Error),
}

pub fn lease_secs() -> String {
    LEASE.as_secs().to_string()
}

pub fn kill_pid(pid: i32) {
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

pub trait EspressoCtl {
    fn rotate(&mut self) -> Result<(), EspressoError>;
    fn kill(&mut self);
    fn is_up(&self) -> bool;
}

#[derive(Default)]
pub struct Lease {
    current: Option<Child>,
}

impl EspressoCtl for Lease {
    fn rotate(&mut self) -> Result<(), EspressoError> {
        // Hold keep-awake via command mode (`espresso -- sleep <lease>`), NOT
        // `espresso -t <lease>`: the `-t` timer draws a progress bar in raw
        // mode and aborts with "failed to enable terminal raw mode" when run
        // headless (as the watcher always is). Command mode holds the assertion
        // without a TTY. The lease still bounds a dead watcher's hold: the
        // orphaned `sleep` self-exits within `LEASE`, releasing espresso.
        let mut child = match Command::new("espresso")
            .arg("--")
            .arg("sleep")
            .arg(lease_secs())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(EspressoError::NotFound)
            }
            Err(e) => return Err(EspressoError::Spawn(e)),
        };
        // Confirm it did not immediately exit.
        if let Ok(Some(_status)) = child.try_wait() {
            return Err(EspressoError::Spawn(std::io::Error::other(
                "espresso exited immediately",
            )));
        }
        // New child is up: kill the old one, then adopt the new.
        if let Some(old) = self.current.take() {
            kill_pid(old.id() as i32);
            let mut old = old;
            let _ = old.wait();
        }
        self.current = Some(child);
        Ok(())
    }

    fn kill(&mut self) {
        if let Some(child) = self.current.take() {
            kill_pid(child.id() as i32);
            let mut child = child;
            let _ = child.wait();
        }
    }

    fn is_up(&self) -> bool {
        self.current.is_some()
    }
}

pub fn daemon_installed() -> bool {
    // `espresso daemon status` exits 0 and reports installed state; if the
    // binary is missing or the helper is not installed, treat as not installed.
    match Command::new("espresso").args(["daemon", "status"]).output() {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
            out.status.success() && text.contains("installed") && !text.contains("not installed")
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_stops_a_spawned_child() {
        // Use `sleep` as a stand-in child we fully control.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        assert!(crate::state::pid_alive(pid));
        kill_pid(pid);
        // give the kernel a moment
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = child.wait();
        assert!(!crate::state::pid_alive(pid));
    }

    #[test]
    fn lease_secs_is_whole_seconds() {
        assert_eq!(lease_secs(), "90");
    }
}
