use crate::consts::{LOCK_WAIT, PLUGIN_ID};
use crate::control::{self, Spawn};
use crate::herdr::{self, Rpc, SocketRpc};
use crate::store::StateDir;

/// Turn the global keep-awake monitor on or off. Decided by whether a watcher
/// is actually running, not by the `enabled` file: a watcher that died can
/// leave `enabled` set (and possibly a stale tab bar line), and one press must
/// turn it back on.
pub fn toggle() -> i32 {
    let dir = StateDir::from_env();
    // Held for the whole stop-or-spawn, so a second press (or startup) waits
    // instead of racing; released before any toast, which can be slow.
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

    if let Some(pid) = control::live_watcher(&dir) {
        if let Err(e) = dir.set_enabled(false) {
            eprintln!("herdr-espresso: clear enabled failed: {e}");
        }
        control::stop_watcher(&lock, &dir, pid);
        drop(lock);
        notify("espresso off", "No longer keeping the Mac awake.");
        println!("espresso: off");
        return 0;
    }

    if let Err(e) = dir.set_enabled(true) {
        eprintln!("herdr-espresso: set enabled failed: {e}");
        return 1;
    }
    match control::spawn_watcher(&lock, &dir) {
        Spawn::Started(_) => {
            drop(lock);
            notify(
                "espresso on",
                "Keeping the Mac awake while any agent works.",
            );
            if !crate::espresso::daemon_installed() {
                notify(
                    &format!("{PLUGIN_ID}: lid-closed not active"),
                    "Run `espresso daemon install` for lid-closed keep-awake.",
                );
            }
            println!("espresso: on");
            0
        }
        Spawn::Failed => {
            let _ = dir.set_enabled(false);
            drop(lock);
            eprintln!(
                "herdr-espresso: watcher failed to start, see {}",
                dir.log_path().display()
            );
            1
        }
    }
}

/// Best-effort toast; toggling works the same when herdr cannot show it.
fn notify(title: &str, body: &str) {
    let Some(sock) = herdr::socket_path() else {
        return;
    };
    match SocketRpc::connect(&sock) {
        Ok(mut rpc) => {
            if let Err(e) = rpc.notify(title, body) {
                eprintln!("herdr-espresso: notify failed: {e:?}");
            }
        }
        Err(e) => eprintln!("herdr-espresso: notify failed: {e:?}"),
    }
}
