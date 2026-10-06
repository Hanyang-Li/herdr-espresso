use std::time::Duration;

pub const PLUGIN_ID: &str = "espresso";
pub const METADATA_SOURCE: &str = "espresso";
// Sidebar marker, reported as a pane metadata token. herdr renders a
// plugin token only where a sidebar row template names it as `$espresso`
// (see README) — the token name must match `^[A-Za-z0-9_-]{1,32}$`.
pub const MARKER_TOKEN: &str = "espresso";
pub const MARKER: &str = "espresso"; // the token's displayed text

// Tab bar status line (Nerd Font: md-coffee U+F0176, md-coffee_outline U+F06CA).
// Shown while the switch is on; with the switch off there is no line at all.
pub const STATUS_AWAKE: &str = "\u{F0176} awake";
pub const STATUS_IDLE: &str = "\u{F06CA} idle";

pub const LEASE: Duration = Duration::from_secs(90);
pub const RENEW_INTERVAL: Duration = Duration::from_secs(60);
pub const STOP_GRACE: Duration = Duration::from_secs(5);
pub const RENEW_RETRY: Duration = Duration::from_secs(10);

// How long `events.subscribe` may take to acknowledge before we give up.
pub const SUBSCRIBE_ACK_TIMEOUT: Duration = Duration::from_secs(2);

// Two stream EOFs closer together than this end the watcher instead of
// resubscribing, so a server that keeps closing the stream cannot spin us.
pub const RESUBSCRIBE_MIN_GAP: Duration = Duration::from_secs(5);
// Snapshot/subscribe rounds per sync before settling for the latest snapshot.
pub const SYNC_ROUNDS: usize = 3;
