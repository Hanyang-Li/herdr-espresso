use std::time::Duration;

pub const PLUGIN_ID: &str = "espresso";
pub const METADATA_SOURCE: &str = "espresso";
// Sidebar marker, reported as a pane metadata token. herdr renders a
// plugin token only where a sidebar row template names it as `$espresso`
// (see README) — the token name must match `^[A-Za-z0-9_-]{1,32}$`.
pub const MARKER_TOKEN: &str = "espresso";
pub const MARKER: &str = "espresso"; // the token's displayed text

pub const LEASE: Duration = Duration::from_secs(90);
pub const RENEW_INTERVAL: Duration = Duration::from_secs(60);
pub const STOP_GRACE: Duration = Duration::from_secs(5);
pub const RENEW_RETRY: Duration = Duration::from_secs(10);
