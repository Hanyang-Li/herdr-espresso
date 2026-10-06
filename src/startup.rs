use crate::control::{self, Spawn};
use crate::store::StateDir;

/// The manifest's `[[startup]]` hook: herdr runs it once the server is up,
/// including after a live handoff. Restores the switch from `enabled`.
pub fn startup() -> i32 {
    let dir = StateDir::from_env();
    control::sweep_legacy(&dir);

    // Replace rather than reuse a live watcher: after a live handoff the
    // outgoing one can still look alive, then exit on its dropped stream and
    // leave none running.
    if let Some(pid) = control::live_watcher(&dir) {
        control::stop_watcher(&dir, pid);
    }

    if !dir.enabled() {
        // Leftovers from a watcher that was killed hard.
        dir.remove_statusline();
        dir.remove_pid();
        return 0;
    }
    match control::spawn_watcher(&dir) {
        Spawn::Started(_) | Spawn::AlreadyRunning => 0,
        Spawn::Failed => {
            dir.remove_statusline();
            eprintln!(
                "herdr-espresso: watcher failed to start, see {}",
                dir.log_path().display()
            );
            1
        }
    }
}
