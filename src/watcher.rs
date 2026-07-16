use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::consts::PLUGIN_ID;
use crate::espresso::{EspressoCtl, EspressoError};
use crate::herdr::{Events, NextLine, Rpc};
use crate::policy::{active, Action, Machine};
use crate::state;

pub fn run_loop<R: Rpc, E: Events, C: EspressoCtl>(
    pane_id: &str,
    rpc: &mut R,
    events: &mut E,
    esp: &mut C,
    mut now: impl FnMut() -> Instant,
    stop: &AtomicBool,
) {
    let mut machine = Machine::new();
    let mut warned_missing = false;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let timeout = machine
            .next_deadline()
            .map(|d| d.saturating_duration_since(now()));
        match events.next_line(timeout) {
            NextLine::Line => match rpc.pane_status(pane_id) {
                Ok(Some(s)) => {
                    let n = now();
                    let acts = machine.on_status(active(&s), n);
                    apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
                }
                Ok(None) | Err(_) => break, // pane gone or unreachable
            },
            NextLine::Timeout => {
                let n = now();
                let acts = machine.on_timer(n);
                apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
            }
            NextLine::Eof => break,
        }
    }

    // Cleanup (covers SIGTERM, pane close, and server gone).
    esp.kill();
    let _ = rpc.clear_marker(pane_id);
    state::remove_pidfile(pane_id);
}

fn apply<R: Rpc, C: EspressoCtl>(
    actions: &[Action],
    esp: &mut C,
    rpc: &mut R,
    machine: &mut Machine,
    now: Instant,
    warned_missing: &mut bool,
) {
    for act in actions {
        match act {
            Action::RotateLease => match esp.rotate() {
                Ok(()) => {}
                Err(EspressoError::NotFound) => {
                    // espresso missing: warn once, and retry sooner than the
                    // normal renew cadence in case it appears on PATH.
                    if !*warned_missing {
                        *warned_missing = true;
                        let _ = rpc.notify(
                            "espresso not found",
                            "Install espresso and keep it on PATH.",
                        );
                    }
                    machine.note_rotate_failed(now);
                }
                Err(EspressoError::Spawn(_)) => machine.note_rotate_failed(now),
            },
            Action::KillEspresso => esp.kill(),
        }
    }
}

pub fn watch(pane_id: &str) -> i32 {
    let Some(sock) = crate::herdr::socket_path() else {
        eprintln!("HERDR_SOCKET_PATH not set");
        return 1;
    };
    let mut rpc = match crate::herdr::SocketRpc::connect(&sock) {
        Ok(r) => r,
        Err(_) => return 1,
    };
    let mut events = match crate::herdr::SocketEvents::subscribe(&sock, pane_id) {
        Ok(e) => e,
        Err(_) => return 1,
    };
    let mut esp = crate::espresso::Lease::default();

    // Marker on; one-time helper warning.
    let _ = rpc.set_marker(pane_id);
    if !crate::espresso::daemon_installed() {
        let _ = rpc.notify(
            &format!("{PLUGIN_ID}: lid-closed not active"),
            "Run `espresso daemon install` for lid-closed keep-awake.",
        );
    }

    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone());

    run_loop(pane_id, &mut rpc, &mut events, &mut esp, Instant::now, &stop);
    0
}
