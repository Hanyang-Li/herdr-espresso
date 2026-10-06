use std::os::unix::net::UnixStream;
use std::time::Instant;

use crate::consts::{RESUBSCRIBE_MIN_GAP, STATUS_AWAKE, STATUS_IDLE, SYNC_ROUNDS};
use crate::espresso::{EspressoCtl, EspressoError};
use crate::herdr::{HerdrError, Rpc, Waiter, Wake};
use crate::policy::{Action, Machine};
use crate::store::StateDir;

/// Where the watcher publishes the tab bar line.
pub trait StatusSink {
    fn show(&mut self, line: &str);
    fn clear(&mut self);
}

/// Publishes to `statusline` in the state dir, which the user's
/// `ui.tab_bar_right` command entry reads.
pub struct FileStatus(pub StateDir);

impl StatusSink for FileStatus {
    fn show(&mut self, line: &str) {
        if let Err(e) = self.0.write_statusline(line) {
            eprintln!("herdr-espresso: write statusline failed: {e}");
        }
    }
    fn clear(&mut self) {
        self.0.remove_statusline();
    }
}

/// Outcome of bringing the subscription and the status up to date.
enum Synced {
    /// Apply this "any agent active" value.
    Active(bool),
    /// Nothing reliable to apply; keep the current state.
    Keep,
    /// herdr is gone (or we could never subscribe): end the watcher.
    Stop,
}

/// Global event loop. Blocks in the `Waiter` (kernel `poll()`) until herdr
/// pushes an event, the stop signal fires, or a timer deadline elapses — no
/// periodic polling. Every event triggers a fresh `pane.list` snapshot.
pub fn run_loop<R: Rpc, W: Waiter, C: EspressoCtl, S: StatusSink>(
    rpc: &mut R,
    waiter: &mut W,
    esp: &mut C,
    sink: &mut S,
    mut now: impl FnMut() -> Instant,
) {
    let mut machine = Machine::new();
    let mut warned_missing = false;
    let mut shown: Option<&'static str> = None;
    let mut subscribed: Option<Vec<String>> = None;
    let mut last_eof: Option<Instant> = None;

    let mut running = match sync(rpc, waiter, &mut subscribed) {
        Synced::Active(active) => {
            let n = now();
            let acts = machine.on_status(active, n);
            apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
            true
        }
        Synced::Keep => true,
        Synced::Stop => false,
    };
    if running {
        render(esp, sink, &mut shown);
    }

    while running {
        let timeout = machine
            .next_deadline()
            .map(|d| d.saturating_duration_since(now()));
        let outcome = match waiter.wait(timeout) {
            Wake::Stop => Synced::Stop,
            Wake::Timeout => Synced::Keep,
            Wake::Event => sync(rpc, waiter, &mut subscribed),
            Wake::Eof => {
                // The stream is gone: herdr exited, or it closed a stream that
                // fell behind (`events_lost`). Resubscribe unless this keeps
                // happening.
                let n = now();
                if last_eof.is_some_and(|t| n.saturating_duration_since(t) < RESUBSCRIBE_MIN_GAP) {
                    Synced::Stop
                } else {
                    last_eof = Some(n);
                    subscribed = None;
                    sync(rpc, waiter, &mut subscribed)
                }
            }
        };
        match outcome {
            Synced::Stop => running = false,
            Synced::Active(active) => {
                let n = now();
                let acts = machine.on_status(active, n);
                apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
            }
            Synced::Keep => {}
        }
        if running {
            // Fire any timers now due (renew / stop-grace). Cheap and a no-op
            // unless a deadline has actually passed.
            let n = now();
            let acts = machine.on_timer(n);
            apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
            render(esp, sink, &mut shown);
        }
    }

    // Cleanup — every exit path (toggle-off/SIGTERM, server gone). Releasing
    // espresso here never waits on the stop-grace, so toggle-off is instant.
    esp.kill();
    sink.clear();
}

/// Snapshot the panes and make sure the subscription covers exactly them. The
/// snapshot that counts is one taken while the matching subscription is live:
/// herdr does not replay events from before a subscription, so a change made
/// between an earlier snapshot and the subscribe would otherwise be missed.
fn sync<R: Rpc, W: Waiter>(
    rpc: &mut R,
    waiter: &mut W,
    subscribed: &mut Option<Vec<String>>,
) -> Synced {
    let mut latest = Synced::Keep;
    for _ in 0..SYNC_ROUNDS {
        let snap = match rpc.snapshot() {
            Ok(s) => s,
            Err(HerdrError::Io(e)) => {
                eprintln!("herdr-espresso: pane.list failed, herdr unreachable: {e}");
                return Synced::Stop;
            }
            Err(e) => {
                eprintln!("herdr-espresso: pane.list rejected: {e:?}");
                break;
            }
        };
        if subscribed.as_deref() == Some(snap.pane_ids.as_slice()) {
            return Synced::Active(snap.any_active);
        }
        latest = Synced::Active(snap.any_active);
        match waiter.subscribe(&snap.pane_ids) {
            Ok(()) => *subscribed = Some(snap.pane_ids),
            Err(HerdrError::Io(e)) => {
                eprintln!("herdr-espresso: subscribe failed, herdr unreachable: {e}");
                return Synced::Stop;
            }
            // Usually a pane that closed since the snapshot: list again.
            Err(e) => eprintln!("herdr-espresso: subscribe rejected: {e:?}"),
        }
    }
    if subscribed.is_none() {
        // Without a stream nothing would ever wake us.
        return Synced::Stop;
    }
    latest
}

fn render<C: EspressoCtl, S: StatusSink>(esp: &C, sink: &mut S, shown: &mut Option<&'static str>) {
    let want = if esp.is_up() {
        STATUS_AWAKE
    } else {
        STATUS_IDLE
    };
    if *shown != Some(want) {
        sink.show(want);
        *shown = Some(want);
    }
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

pub fn watch() -> i32 {
    let dir = StateDir::from_env();
    let code = run_watcher(&dir);
    // Only our own pidfile: a replacement watcher may already have claimed it.
    dir.remove_pid_if(std::process::id() as i32);
    code
}

fn run_watcher(dir: &StateDir) -> i32 {
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
        eprintln!("herdr-espresso: HERDR_SOCKET_PATH not set");
        return 1;
    };
    let mut rpc = match crate::herdr::SocketRpc::connect(&sock) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("herdr-espresso: cannot reach herdr: {e:?}");
            return 1;
        }
    };
    let mut waiter = crate::herdr::PollWaiter::new(&sock, sig_read);
    let mut esp = crate::espresso::Lease::default();
    let mut sink = FileStatus(dir.clone());

    run_loop(&mut rpc, &mut waiter, &mut esp, &mut sink, Instant::now);
    0
}
