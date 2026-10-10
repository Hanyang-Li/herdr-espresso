use herdr_espresso::consts::{STATUS_AWAKE, STATUS_FORCED, STATUS_IDLE};
use herdr_espresso::espresso::{EspressoCtl, EspressoError};
use herdr_espresso::herdr::{HerdrError, Rpc, Snapshot, Waiter, Wake};
use herdr_espresso::watcher::{run_loop, StatusSink};
use std::time::{Duration, Instant};

/// One scripted `pane.list` outcome.
#[derive(Clone)]
enum List {
    Panes(bool, &'static [&'static str]),
    Rejected,
    Unreachable,
}

fn io_err() -> HerdrError {
    HerdrError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
}

struct FakeRpc {
    script: Vec<List>,
    i: usize,
}
impl FakeRpc {
    fn new(script: Vec<List>) -> Self {
        Self { script, i: 0 }
    }
}
impl Rpc for FakeRpc {
    fn snapshot(&mut self) -> Result<Snapshot, HerdrError> {
        let step = self
            .script
            .get(self.i)
            .cloned()
            .unwrap_or(List::Unreachable);
        self.i += 1;
        match step {
            List::Panes(any_active, ids) => Ok(Snapshot {
                any_active,
                pane_ids: ids.iter().map(|s| s.to_string()).collect(),
            }),
            List::Rejected => Err(HerdrError::Protocol("internal".into())),
            List::Unreachable => Err(io_err()),
        }
    }
    fn notify(&mut self, _t: &str, _b: &str) -> Result<(), HerdrError> {
        Ok(())
    }
}

/// One scripted `subscribe` outcome.
#[derive(Clone, Copy)]
enum Sub {
    Ok,
    Rejected,
    Unreachable,
}

/// Yields scripted wakes (then `Stop` forever) and scripted subscribe
/// outcomes (then `Ok`), recording every subscribe attempt.
struct FakeWaiter {
    wakes: Vec<Wake>,
    waited: usize,
    subs: Vec<Sub>,
    attempts: Vec<Vec<String>>,
}
impl FakeWaiter {
    fn new(wakes: Vec<Wake>, subs: Vec<Sub>) -> Self {
        Self {
            wakes,
            waited: 0,
            subs,
            attempts: Vec::new(),
        }
    }
}
impl Waiter for FakeWaiter {
    fn wait(&mut self, _t: Option<Duration>) -> Wake {
        let w = self.wakes.get(self.waited).copied().unwrap_or(Wake::Stop);
        self.waited += 1;
        w
    }
    fn subscribe(&mut self, ids: &[String]) -> Result<(), HerdrError> {
        let outcome = self
            .subs
            .get(self.attempts.len())
            .copied()
            .unwrap_or(Sub::Ok);
        self.attempts.push(ids.to_vec());
        match outcome {
            Sub::Ok => Ok(()),
            Sub::Rejected => Err(HerdrError::Protocol("pane_not_found".into())),
            Sub::Unreachable => Err(io_err()),
        }
    }
}

#[derive(Default)]
struct FakeEsp {
    log: Option<std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>>,
    up: bool,
    rotates: u32,
    kills: u32,
}
impl EspressoCtl for FakeEsp {
    fn rotate(&mut self) -> Result<(), EspressoError> {
        if let Some(log) = &self.log {
            log.borrow_mut().push("rotate");
        }
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

#[derive(Default)]
struct FakeSink {
    shown: Vec<String>,
    cleared: bool,
}
impl StatusSink for FakeSink {
    fn show(&mut self, line: &str) {
        self.shown.push(line.to_string());
        self.cleared = false;
    }
    fn clear(&mut self) {
        self.cleared = true;
    }
}

const A: &[&str] = &["w1:p1"];
const AB: &[&str] = &["w1:p1", "w1:p2"];

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A clock that advances 10 s on every read, so the 5 s stop-grace elapses
/// between consecutive steps.
fn advancing() -> impl FnMut() -> Instant {
    let start = Instant::now();
    let mut ticks = 0u64;
    move || {
        let t = start + Duration::from_secs(ticks * 10);
        ticks += 1;
        t
    }
}

fn run(
    rpc: &mut FakeRpc,
    waiter: &mut FakeWaiter,
    clock: impl FnMut() -> Instant,
) -> (FakeEsp, FakeSink) {
    let (esp, sink, _) = run_ready(rpc, waiter, clock);
    (esp, sink)
}

/// Like `run`, also returning how many times `on_ready` fired.
fn run_ready(
    rpc: &mut FakeRpc,
    waiter: &mut FakeWaiter,
    clock: impl FnMut() -> Instant,
) -> (FakeEsp, FakeSink, u32) {
    run_mode(rpc, waiter, clock, false)
}

/// Like `run`, in forced mode.
fn run_forced(
    rpc: &mut FakeRpc,
    waiter: &mut FakeWaiter,
    clock: impl FnMut() -> Instant,
) -> (FakeEsp, FakeSink, u32) {
    run_mode(rpc, waiter, clock, true)
}

fn run_mode(
    rpc: &mut FakeRpc,
    waiter: &mut FakeWaiter,
    clock: impl FnMut() -> Instant,
    forced: bool,
) -> (FakeEsp, FakeSink, u32) {
    let mut esp = FakeEsp::default();
    let mut sink = FakeSink::default();
    let mut ready = 0;
    run_loop(rpc, waiter, &mut esp, &mut sink, clock, forced, || {
        ready += 1
    });
    (esp, sink, ready)
}

#[test]
fn seeded_active_holds_and_shows_awake_until_stop() {
    // Subscribe first, then the snapshot taken under the live subscription
    // decides — herdr does not replay events from before a subscription.
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A), List::Panes(true, A)]);
    let mut waiter = FakeWaiter::new(vec![Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, sink, ready) = run_ready(&mut rpc, &mut waiter, move || t0);
    assert_eq!(ready, 1);
    assert_eq!(waiter.attempts, vec![ids(A)]);
    assert_eq!(rpc.i, 2);
    assert_eq!(esp.rotates, 1);
    assert_eq!(esp.kills, 1); // cleanup on Stop
    assert_eq!(sink.shown, vec![STATUS_AWAKE]);
    assert!(sink.cleared);
}

#[test]
fn seeded_idle_shows_idle() {
    let mut rpc = FakeRpc::new(vec![List::Panes(false, A), List::Panes(false, A)]);
    let mut waiter = FakeWaiter::new(vec![Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(esp.rotates, 0);
    assert_eq!(sink.shown, vec![STATUS_IDLE]);
    assert!(sink.cleared);
}

#[test]
fn all_idle_releases_after_grace_and_shows_idle() {
    let mut rpc = FakeRpc::new(vec![
        List::Panes(true, A),
        List::Panes(true, A),
        List::Panes(false, A),
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Timeout, Wake::Stop], vec![]);
    let (esp, sink) = run(&mut rpc, &mut waiter, advancing());
    assert_eq!(esp.rotates, 1);
    // Released by the stop-grace timer; the cleanup kill is then a no-op, so
    // exactly 1 proves the timer did it.
    assert_eq!(esp.kills, 1);
    assert_eq!(sink.shown, vec![STATUS_AWAKE, STATUS_IDLE]);
}

#[test]
fn reactivating_within_grace_keeps_holding() {
    let mut rpc = FakeRpc::new(vec![
        List::Panes(true, A),
        List::Panes(true, A),
        List::Panes(false, A),
        List::Panes(true, A),
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Event, Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(esp.rotates, 1);
    assert_eq!(esp.kills, 1); // only the cleanup
    assert_eq!(sink.shown, vec![STATUS_AWAKE]);
}

#[test]
fn rejected_list_keeps_the_previous_state() {
    // If the error were read as "nothing active", the stop-grace timer would
    // release espresso and the line would flip to idle.
    let mut rpc = FakeRpc::new(vec![
        List::Panes(true, A),
        List::Panes(true, A),
        List::Rejected,
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Timeout, Wake::Stop], vec![]);
    let (esp, sink) = run(&mut rpc, &mut waiter, advancing());
    assert_eq!(esp.rotates, 1);
    assert_eq!(esp.kills, 1); // only the cleanup
    assert_eq!(sink.shown, vec![STATUS_AWAKE]);
}

#[test]
fn unreachable_list_exits_and_cleans_up() {
    let mut rpc = FakeRpc::new(vec![
        List::Panes(true, A),
        List::Panes(true, A),
        List::Unreachable,
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Event], vec![]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.waited, 1);
    assert_eq!(esp.kills, 1);
    assert!(sink.cleared);
}

#[test]
fn new_pane_resubscribes_then_resnapshots() {
    // A pane appears (idle), and its agent is already working by the time the
    // new subscription is live: the post-subscribe snapshot must catch it.
    let mut rpc = FakeRpc::new(vec![
        List::Panes(false, A),
        List::Panes(false, A),
        List::Panes(false, AB),
        List::Panes(true, AB),
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.attempts, vec![ids(A), ids(AB)]);
    assert_eq!(rpc.i, 4);
    assert_eq!(esp.rotates, 1);
    assert_eq!(sink.shown, vec![STATUS_IDLE, STATUS_AWAKE]);
}

#[test]
fn rejected_subscription_retries_with_a_fresh_list() {
    // w1:p2 closed between pane.list and events.subscribe.
    let mut rpc = FakeRpc::new(vec![
        List::Panes(false, AB),
        List::Panes(true, A),
        List::Panes(true, A),
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Stop], vec![Sub::Rejected, Sub::Ok]);
    let t0 = Instant::now();
    let (esp, _sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.attempts, vec![ids(AB), ids(A)]);
    assert_eq!(esp.rotates, 1);
}

#[test]
fn never_subscribed_exits_without_waiting() {
    // With no stream nothing would ever wake the loop, so it must not start.
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A); 3]);
    let mut waiter = FakeWaiter::new(
        vec![Wake::Stop],
        vec![Sub::Rejected, Sub::Rejected, Sub::Rejected],
    );
    let t0 = Instant::now();
    let (esp, sink, ready) = run_ready(&mut rpc, &mut waiter, move || t0);
    assert_eq!(ready, 0);
    assert_eq!(waiter.waited, 0);
    assert_eq!(esp.rotates, 0);
    assert!(sink.shown.is_empty());
    assert!(sink.cleared);
}

#[test]
fn eof_resubscribes_and_keeps_watching() {
    // herdr closes a stream that fell behind (`events_lost`); the watcher
    // must resubscribe even though the pane set is unchanged.
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A); 4]);
    let mut waiter = FakeWaiter::new(vec![Wake::Eof, Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, _sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.attempts, vec![ids(A), ids(A)]);
    assert_eq!(waiter.waited, 2);
    assert_eq!(rpc.i, 4);
    assert_eq!(esp.rotates, 1);
}

#[test]
fn eof_with_herdr_gone_exits() {
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A); 3]);
    let mut waiter = FakeWaiter::new(vec![Wake::Eof, Wake::Stop], vec![Sub::Ok, Sub::Unreachable]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.waited, 1);
    assert_eq!(esp.kills, 1);
    assert!(sink.cleared);
}

#[test]
fn rapid_eofs_exit_instead_of_spinning() {
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A); 6]);
    let mut waiter = FakeWaiter::new(vec![Wake::Eof, Wake::Eof, Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (_esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.attempts.len(), 2); // initial + one resubscribe
    assert_eq!(waiter.waited, 2);
    assert!(sink.cleared);
}

#[test]
fn first_unreachable_list_never_reports_ready() {
    let mut rpc = FakeRpc::new(vec![List::Unreachable]);
    let mut waiter = FakeWaiter::new(vec![Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (_esp, sink, ready) = run_ready(&mut rpc, &mut waiter, move || t0);
    assert_eq!(ready, 0);
    assert_eq!(waiter.waited, 0);
    assert!(sink.cleared);
}

#[test]
fn ready_fires_once_and_before_the_first_lease() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A); 4]);
    let mut waiter = FakeWaiter::new(vec![Wake::Eof, Wake::Stop], vec![]);
    let mut esp = FakeEsp {
        log: Some(Rc::clone(&log)),
        ..Default::default()
    };
    let mut sink = FakeSink::default();
    let t0 = Instant::now();
    let l2 = Rc::clone(&log);
    run_loop(
        &mut rpc,
        &mut waiter,
        &mut esp,
        &mut sink,
        move || t0,
        false,
        || l2.borrow_mut().push("ready"),
    );
    assert_eq!(*log.borrow(), vec!["ready", "rotate"]);
}

#[test]
fn forced_holds_while_every_agent_is_idle() {
    let mut rpc = FakeRpc::new(vec![List::Panes(false, A); 3]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Timeout, Wake::Stop], vec![]);
    let (esp, sink, ready) = run_forced(&mut rpc, &mut waiter, advancing());
    assert_eq!(ready, 1);
    assert!(esp.rotates >= 1);
    // Never released by the stop-grace timer: only the cleanup kill.
    assert_eq!(esp.kills, 1);
    assert_eq!(sink.shown, vec![STATUS_FORCED]);
    assert!(sink.cleared);
}

#[test]
fn forced_holds_even_when_the_first_list_is_rejected() {
    let mut rpc = FakeRpc::new(vec![List::Panes(false, A), List::Rejected]);
    let mut waiter = FakeWaiter::new(vec![Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, sink, ready) = run_forced(&mut rpc, &mut waiter, move || t0);
    assert_eq!(ready, 1);
    assert_eq!(esp.rotates, 1);
    assert_eq!(sink.shown, vec![STATUS_FORCED]);
}

#[test]
fn forced_renews_the_lease() {
    // The advancing clock passes the 60 s renew deadline within a few steps.
    let mut rpc = FakeRpc::new(vec![List::Panes(false, A); 2]);
    let wakes = vec![Wake::Timeout; 10];
    let mut waiter = FakeWaiter::new(wakes, vec![]);
    let (esp, _sink, _) = run_forced(&mut rpc, &mut waiter, advancing());
    assert!(esp.rotates >= 2, "rotates = {}", esp.rotates);
}

#[test]
fn forced_still_exits_when_herdr_is_gone() {
    let mut rpc = FakeRpc::new(vec![List::Panes(false, A), List::Panes(false, A)]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event], vec![]);
    let t0 = Instant::now();
    let (esp, sink, _) = run_forced(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.waited, 1);
    assert_eq!(esp.kills, 1);
    assert!(sink.cleared);
}
