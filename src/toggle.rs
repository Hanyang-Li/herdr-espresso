use crate::consts::PLUGIN_ID;
use crate::control::{self, Spawn};
use crate::herdr::{self, Rpc, SocketRpc};
use crate::store::StateDir;

/// Turn the global keep-awake monitor on or off. Decided by whether a watcher
/// is actually running, not by the `enabled` file: a watcher that died leaves
/// the tab bar empty (looks off), and one press must turn it back on.
pub fn toggle() -> i32 {
    let dir = StateDir::from_env();
    control::sweep_legacy(&dir);

    if let Some(pid) = control::live_watcher(&dir) {
        if let Err(e) = dir.set_enabled(false) {
            eprintln!("herdr-espresso: clear enabled failed: {e}");
        }
        control::stop_watcher(&dir, pid);
        notify("espresso off", "No longer keeping the Mac awake.");
        println!("espresso: off");
        return 0;
    }

    if let Err(e) = dir.set_enabled(true) {
        eprintln!("herdr-espresso: set enabled failed: {e}");
        return 1;
    }
    match control::spawn_watcher(&dir) {
        Spawn::Started(_) => {
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
        Spawn::AlreadyRunning => {
            println!("espresso: already on");
            0
        }
        Spawn::Failed => {
            let _ = dir.set_enabled(false);
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
