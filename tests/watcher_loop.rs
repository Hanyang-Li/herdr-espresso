use herdr_espresso::espresso::{EspressoCtl, EspressoError};
use herdr_espresso::herdr::{HerdrError, Rpc, Waiter, Wake};
use herdr_espresso::watcher::run_loop;
use std::time::{Duration, Instant};

// Scripted waiter: yields a sequence of Wake values, then Eof forever.
struct FakeWaiter {
    script: Vec<Wake>,
    i: usize,
}
impl Waiter for FakeWaiter {
    fn wait(&mut self, _t: Option<Duration>) -> Wake {
        let w = self.script.get(self.i).copied().unwrap_or(Wake::Eof);
        self.i += 1;
        w
    }
}

struct FakeRpc {
    statuses: Vec<Option<String>>,
    i: usize,
    marker_cleared: bool,
}
impl Rpc for FakeRpc {
    fn pane_status(&mut self, _p: &str) -> Result<Option<String>, HerdrError> {
        let s = self.statuses.get(self.i).cloned().unwrap_or(None);
        self.i += 1;
        Ok(s)
    }
    fn set_marker(&mut self, _p: &str) -> Result<(), HerdrError> {
        Ok(())
    }
    fn clear_marker(&mut self, _p: &str) -> Result<(), HerdrError> {
        self.marker_cleared = true;
        Ok(())
    }
    fn notify(&mut self, _t: &str, _b: &str) -> Result<(), HerdrError> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeEsp {
    up: bool,
    rotates: u32,
    kills: u32,
}
impl EspressoCtl for FakeEsp {
    fn rotate(&mut self) -> Result<(), EspressoError> {
        self.up = true;
        self.rotates += 1;
        Ok(())
    }
    fn kill(&mut self) {
        // Idempotent: only counts a kill that actually stops a running hold, so
        // the final cleanup kill is a no-op when espresso is already down.
        if self.up {
            self.up = false;
            self.kills += 1;
        }
    }
    fn is_up(&self) -> bool {
        self.up
    }
}

#[test]
fn seeded_working_opens_then_pane_closed_cleans_up() {
    // Seed reads working -> rotate. First event's status read returns None
    // (pane gone) -> break -> cleanup.
    let mut waiter = FakeWaiter {
        script: vec![Wake::Event],
        i: 0,
    };
    let mut rpc = FakeRpc {
        statuses: vec![Some("working".into()), None],
        i: 0,
        marker_cleared: false,
    };
    let mut esp = FakeEsp::default();
    let t0 = Instant::now();
    run_loop("w1:p1", &mut rpc, &mut waiter, &mut esp, move || t0);
    assert_eq!(esp.rotates, 1); // opened from the seed
    assert_eq!(esp.kills, 1); // killed during cleanup
    assert!(rpc.marker_cleared);
}

#[test]
fn seeded_active_opens_without_any_event() {
    // No events at all (Waiter yields Eof immediately). A pane already working
    // at toggle-on must acquire the lease from the seed alone.
    let mut waiter = FakeWaiter {
        script: vec![],
        i: 0,
    };
    let mut rpc = FakeRpc {
        statuses: vec![Some("working".into())],
        i: 0,
        marker_cleared: false,
    };
    let mut esp = FakeEsp::default();
    let t0 = Instant::now();
    run_loop("w1:p1", &mut rpc, &mut waiter, &mut esp, move || t0);
    assert_eq!(esp.rotates, 1);
    assert_eq!(esp.kills, 1); // cleanup
    assert!(rpc.marker_cleared);
}

#[test]
fn stop_signal_closes_immediately() {
    // Toggle-off: the Stop wake breaks the loop at once and cleanup kills
    // espresso — no stop-grace involved.
    let mut waiter = FakeWaiter {
        script: vec![Wake::Stop],
        i: 0,
    };
    let mut rpc = FakeRpc {
        statuses: vec![Some("working".into())],
        i: 0,
        marker_cleared: false,
    };
    let mut esp = FakeEsp::default();
    let t0 = Instant::now();
    run_loop("w1:p1", &mut rpc, &mut waiter, &mut esp, move || t0);
    assert_eq!(esp.rotates, 1); // opened from seed
    assert_eq!(esp.kills, 1); // closed by cleanup on stop
    assert!(rpc.marker_cleared);
}

#[test]
fn idle_event_then_stop_grace_timer_closes() {
    // Working -> an idle event arms the 5s stop-grace (no immediate close);
    // a later timer wake (deadline elapsed) fires the close via on_timer, not
    // via the final cleanup.
    let mut waiter = FakeWaiter {
        script: vec![Wake::Event, Wake::Timeout],
        i: 0,
    };
    let mut rpc = FakeRpc {
        statuses: vec![Some("working".into()), Some("idle".into())],
        i: 0,
        marker_cleared: false,
    };
    let mut esp = FakeEsp::default();
    // Advancing clock (+10s per call) so the 5s stop-grace elapses between the
    // idle event and the next timer wake.
    let start = Instant::now();
    let mut ticks = 0u64;
    let clock = move || {
        let t = start + Duration::from_secs(ticks * 10);
        ticks += 1;
        t
    };
    run_loop("w1:p1", &mut rpc, &mut waiter, &mut esp, clock);
    assert_eq!(esp.rotates, 1); // opened from seed (working)
                                // Closed by the stop-grace timer; cleanup kill is then a no-op, so a
                                // count of exactly 1 proves the timer (not cleanup) did the close.
    assert_eq!(esp.kills, 1);
}
