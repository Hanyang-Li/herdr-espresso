use herdr_espresso::espresso::{EspressoCtl, EspressoError};
use herdr_espresso::herdr::{Events, HerdrError, NextLine, Rpc};
use herdr_espresso::watcher::run_loop;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

// Scripted event source: yields a sequence of NextLine values, then Eof.
struct FakeEvents {
    script: Vec<NextLine>,
    i: usize,
}
impl Events for FakeEvents {
    fn next_line(&mut self, _t: Option<Duration>) -> NextLine {
        let n = self
            .script
            .get(self.i)
            .map(clone_nl)
            .unwrap_or(NextLine::Eof);
        self.i += 1;
        n
    }
}
fn clone_nl(n: &NextLine) -> NextLine {
    match n {
        NextLine::Line => NextLine::Line,
        NextLine::Timeout => NextLine::Timeout,
        NextLine::Eof => NextLine::Eof,
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
        self.up = false;
        self.kills += 1;
    }
    fn is_up(&self) -> bool {
        self.up
    }
}

#[test]
fn working_then_pane_closed_opens_then_cleans_up() {
    // Seed consumes first status (working -> rotate). First Line event's status read returns next scripted status (None) -> break.
    let mut events = FakeEvents {
        script: vec![NextLine::Line, NextLine::Line],
        i: 0,
    };
    let mut rpc = FakeRpc {
        statuses: vec![Some("working".into()), None],
        i: 0,
        marker_cleared: false,
    };
    let mut esp = FakeEsp::default();
    let stop = AtomicBool::new(false);
    let t0 = Instant::now();
    run_loop("w1:p1", &mut rpc, &mut events, &mut esp, move || t0, &stop);
    assert_eq!(esp.rotates, 1); // opened on working
    assert_eq!(esp.kills, 1); // killed during cleanup
    assert!(rpc.marker_cleared); // marker cleared on cleanup
}

#[test]
fn seeded_active_status_opens_lease_without_any_event() {
    let mut events = FakeEvents {
        script: vec![],
        i: 0,
    }; // -> Eof immediately
    let mut rpc = FakeRpc {
        statuses: vec![Some("working".into())],
        i: 0,
        marker_cleared: false,
    };
    let mut esp = FakeEsp::default();
    let stop = AtomicBool::new(false);
    let t0 = Instant::now();
    run_loop("w1:p1", &mut rpc, &mut events, &mut esp, move || t0, &stop);
    assert_eq!(esp.rotates, 1); // opened from the seed, no event needed
    assert_eq!(esp.kills, 1); // cleanup
    assert!(rpc.marker_cleared);
}
