use std::os::unix::net::UnixStream;
use std::time::Instant;

use crate::consts::PLUGIN_ID;
use crate::espresso::{EspressoCtl, EspressoError};
use crate::herdr::{PaneState, Rpc, Waiter, Wake};
use crate::policy::{active, Action, Machine};
use crate::state;

/// Per-pane event loop. Fully event-driven: it blocks in the `Waiter` (kernel
/// `poll()`) until herdr pushes an event, the stop signal fires, or a timer
/// deadline elapses — no periodic status polling. Status is re-read only when
/// herdr pushes an event.
pub fn run_loop<R: Rpc, W: Waiter, C: EspressoCtl>(
    pane_id: &str,
    rpc: &mut R,
    waiter: &mut W,
    esp: &mut C,
    mut now: impl FnMut() -> Instant,
) {
    let mut machine = Machine::new();
    let mut warned_missing = false;

    // Seed the initial status: herdr sends no state snapshot on subscribe, so
    // without this a pane already working/blocked at toggle-on would not
    // acquire a lease until its next status transition.
    let mut enter = true;
    match rpc.pane_state(pane_id) {
        Ok(PaneState::Agent(s)) => {
            let n = now();
            let acts = machine.on_status(active(&s), n);
            apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
        }
        // No agent / pane gone / unreachable at start — nothing to monitor.
        _ => enter = false,
    }

    if enter {
        loop {
            let timeout = machine
                .next_deadline()
                .map(|d| d.saturating_duration_since(now()));
            match waiter.wait(timeout) {
                Wake::Stop | Wake::Eof => break,
                Wake::Event => match rpc.pane_state(pane_id) {
                    Ok(PaneState::Agent(s)) => {
                        let n = now();
                        let acts = machine.on_status(active(&s), n);
                        apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
                    }
                    // Agent exited (NoAgent), pane closed (Gone), or the socket
                    // failed — stop monitoring. Cleanup below releases espresso
                    // and clears the marker even though the pane stays open.
                    Ok(PaneState::NoAgent) | Ok(PaneState::Gone) | Err(_) => break,
                },
                Wake::Timeout => {}
            }
            // Fire any timers now due (renew / stop-grace). Cheap and a no-op
            // unless a deadline has actually passed.
            let n = now();
            let acts = machine.on_timer(n);
            apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
        }
    }

    // Cleanup — runs on every exit path (toggle-off/SIGTERM, pane close, server
    // gone). Closing espresso here is immediate and never waits on the
    // stop-grace, so toggle-off reacts at once.
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
    // Self-pipe for instant toggle-off: the SIGTERM handler writes a byte to
    // `sig_write`; the watcher's poll() wakes on `sig_read` and cleans up at
    // once — no periodic flag-polling, no wakeup latency.
    let (sig_read, sig_write) = match UnixStream::pair() {
        Ok(pair) => pair,
        Err(_) => return 1,
    };
    if signal_hook::low_level::pipe::register(signal_hook::consts::SIGTERM, sig_write).is_err() {
        eprintln!("herdr-espresso: failed to register SIGTERM handler");
    }

    let Some(sock) = crate::herdr::socket_path() else {
        eprintln!("HERDR_SOCKET_PATH not set");
        return 1;
    };
    let mut rpc = match crate::herdr::SocketRpc::connect(&sock) {
        Ok(r) => r,
        Err(_) => return 1,
    };
    let events = match crate::herdr::SocketEvents::subscribe(&sock, pane_id) {
        Ok(e) => e,
        Err(_) => return 1,
    };
    let mut waiter = crate::herdr::PollWaiter::new(events, sig_read);
    let mut esp = crate::espresso::Lease::default();

    // Marker on; one-time helper warning if lid-closed isn't installed.
    let _ = rpc.set_marker(pane_id);
    if !crate::espresso::daemon_installed() {
        let _ = rpc.notify(
            &format!("{PLUGIN_ID}: lid-closed not active"),
            "Run `espresso daemon install` for lid-closed keep-awake.",
        );
    }

    run_loop(pane_id, &mut rpc, &mut waiter, &mut esp, Instant::now);
    0
}
