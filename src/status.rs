use crate::state;

pub fn status() -> i32 {
    let mut any = false;
    for (sanitized, pid) in state::list_monitored() {
        if state::pid_alive(pid) {
            println!("monitoring: {sanitized} (watcher pid {pid})");
            any = true;
        } else {
            state::remove_pidfile_sanitized(&sanitized); // reconcile stale
        }
    }
    if !any {
        println!("no panes monitored");
    }
    0
}
