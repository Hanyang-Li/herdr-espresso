use std::time::Duration;

pub const PLUGIN_ID: &str = "espresso";

// Tab bar status line (Nerd Font: md-coffee U+F0176, md-coffee_outline U+F06CA,
// md-coffee_to_go U+F0177). Shown while the switch is on; with the switch off
// there is no line at all. herdr strips styling from tab bar output, so modes
// differ by glyph and text, not color.
pub const STATUS_AWAKE: &str = "\u{F0176} awake";
pub const STATUS_IDLE: &str = "\u{F06CA} idle";
pub const STATUS_FORCED: &str = "\u{F0177} forced";

pub const LEASE: Duration = Duration::from_secs(90);
pub const RENEW_INTERVAL: Duration = Duration::from_secs(60);
pub const STOP_GRACE: Duration = Duration::from_secs(5);
pub const RENEW_RETRY: Duration = Duration::from_secs(10);

// How long `events.subscribe` may take to acknowledge before we give up.
pub const SUBSCRIBE_ACK_TIMEOUT: Duration = Duration::from_secs(2);

// Read/write bound for a one-shot request/response call. herdr normally
// answers at once; without a bound, a herdr that accepts but never replies
// would block the watcher loop forever and stop lease renewal.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(5);

// Two stream EOFs closer together than this end the watcher instead of
// resubscribing, so a server that keeps closing the stream cannot spin us.
pub const RESUBSCRIBE_MIN_GAP: Duration = Duration::from_secs(5);
// Snapshot/subscribe rounds per sync before settling for the latest snapshot.
pub const SYNC_ROUNDS: usize = 3;

// How long toggle-off / startup wait for a watcher to exit on SIGTERM before
// SIGKILLing it.
pub const STOP_WAIT: Duration = Duration::from_secs(3);

// How long a freshly spawned watcher gets to subscribe and write its pidfile
// (>= SYNC_ROUNDS * SUBSCRIBE_ACK_TIMEOUT + slack) before it is given up on.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);
pub const READY_POLL: Duration = Duration::from_millis(25);
// How long toggle/startup wait for the control lock: the longest holder is
// startup stopping a watcher and then waiting for the new one to be ready.
pub const LOCK_WAIT: Duration =
    Duration::from_secs(STOP_WAIT.as_secs() + READY_TIMEOUT.as_secs() + 2);
