use crate::consts::LOCK_WAIT;
use crate::control::{self, Spawn};
use crate::store::StateDir;

/// The manifest's `[[startup]]` hook: herdr runs it once the server is up,
/// including after a live handoff. Restores the switch, in its mode, from
/// `enabled`.
pub fn startup() -> i32 {
    let dir = StateDir::from_env();
    let lock = match dir.lock(LOCK_WAIT) {
        Ok(l) => l,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::TimedOut {
                eprintln!("herdr-espresso: another toggle/startup is still running ({e})");
            } else {
                eprintln!("herdr-espresso: cannot take the control lock: {e}");
            }
            return 1;
        }
    };
    control::sweep_legacy(&dir);

    // Replace rather than reuse a live watcher: after a live handoff the
    // outgoing one can still look alive, then exit on its dropped stream and
    // leave none running.
    if let Some(pid) = control::live_watcher(&dir) {
        control::stop_watcher(&lock, &dir, pid);
    }

    let Some(mode) = dir.mode() else {
        // Leftovers from a watcher that was killed hard.
        dir.remove_statusline();
        dir.remove_pid();
        return 0;
    };
    match control::spawn_watcher(&lock, &dir, mode) {
        Spawn::Started(_) => 0,
        Spawn::Failed => {
            eprintln!(
                "herdr-espresso: watcher failed to start, see {}",
                dir.log_path().display()
            );
            1
        }
    }
}
