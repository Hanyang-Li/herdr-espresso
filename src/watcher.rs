use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::consts::PLUGIN_ID;
use crate::espresso::{EspressoCtl, EspressoError};
use crate::herdr::{Events, NextLine, Rpc};
use crate::policy::{active, Action, Machine};
use crate::state;

// Upper bound on how long a single `next_line` blocks, so the loop rechecks
// `stop` (toggle-off / SIGTERM) promptly even when there is no timer deadline
// (e.g. a pane that is idle at toggle-on: no lease, no pending stop). Without
// this, a toggle-off while idle would leave the marker and pidfile until the
// next pane event. Waking early is harmless: `on_timer` returns no actions
// unless a deadline is actually due.
const POLL_CAP: Duration = Duration::from_secs(1);

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

    // Seed the initial status: herdr sends no state snapshot on subscribe, so
    // without this a pane already working/blocked at toggle-on would not
    // acquire a lease until its next status transition (which may never come).
    let mut enter = true;
    match rpc.pane_status(pane_id) {
        Ok(Some(s)) => {
            let n = now();
            let acts = machine.on_status(active(&s), n);
            apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
        }
        Ok(None) | Err(_) => enter = false, // pane gone/unreachable at start
    }

    // `while enter` triggers clippy::while_immutable_condition (deny-by-default:
    // `enter` is never mutated in the loop body, only before it), so the
    // single-entry gate is expressed as `if enter { loop { ... } }` instead.
    // Every existing `break` and the loop body are otherwise unchanged.
    if enter {
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let timeout = match machine.next_deadline() {
                Some(d) => d.saturating_duration_since(now()).min(POLL_CAP),
                None => POLL_CAP,
            };
            // An event wakes us early; otherwise we wake at POLL_CAP. Either
            // way, re-read the authoritative status EVERY iteration: herdr does
            // not reliably push a `pane.agent_status_changed` event to this
            // subscription for every transition (and events are sparse when
            // other panes are quiet), so relying on events alone made idle
            // detection lag badly (espresso lingered ~30s). Polling each tick
            // notices the change within ~POLL_CAP, then the stop-grace applies.
            if matches!(events.next_line(Some(timeout)), NextLine::Eof) {
                break;
            }
            match rpc.pane_status(pane_id) {
                Ok(Some(s)) => {
                    let n = now();
                    let acts = machine.on_status(active(&s), n);
                    apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
                }
                Ok(None) | Err(_) => break, // pane gone or unreachable
            }
            let n = now();
            let acts = machine.on_timer(n);
            apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
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
    // Register SIGTERM handling FIRST so a toggle-off arriving during startup
    // is caught (flag set) instead of killing us via the default disposition
    // and bypassing cleanup.
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    if signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone()).is_err() {
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

    run_loop(
        pane_id,
        &mut rpc,
        &mut events,
        &mut esp,
        Instant::now,
        &stop,
    );
    0
}
