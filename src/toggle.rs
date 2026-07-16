use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::{herdr, state};

pub fn toggle() -> i32 {
    let Some(pane_id) = herdr::focused_pane_id() else {
        eprintln!("no focused pane (HERDR_PANE_ID unset)");
        return 1;
    };

    if let Some(pid) = state::read_pidfile(&pane_id) {
        if state::pid_alive(pid) {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            state::remove_pidfile(&pane_id);
            println!("espresso: monitoring off for {pane_id}");
            return 0;
        }
        state::remove_pidfile(&pane_id); // stale
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return 1,
    };
    let _ = std::fs::create_dir_all(state::state_dir());
    let log = state::state_dir().join(format!("{}.log", state::sanitize_pane_id(&pane_id)));

    // Default both streams to null: a detached watcher must NEVER inherit the
    // caller's tty (Command's default for an unset stream is inherit()).
    // Upgrade to the log file only when it is fully available.
    let mut cmd = Command::new(exe);
    cmd.arg("watch").arg(&pane_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        match f.try_clone() {
            Ok(f2) => {
                cmd.stdout(Stdio::from(f));
                cmd.stderr(Stdio::from(f2));
            }
            Err(_) => {
                cmd.stdout(Stdio::from(f)); // stderr stays null
            }
        }
    }
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return 1,
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let pid = child.id() as i32;
    if !state::pid_alive(pid) {
        return 1;
    }
    if state::write_pidfile(&pane_id, pid).is_err() {
        return 1;
    }
    println!("espresso: monitoring on for {pane_id}");
    0
}
