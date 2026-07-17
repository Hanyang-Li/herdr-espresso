use std::time::Duration;

pub const PLUGIN_ID: &str = "espresso";
pub const METADATA_SOURCE: &str = "espresso";
pub const MARKER: &str = "espresso"; // sidebar custom-status label (<=32 chars)

pub const LEASE: Duration = Duration::from_secs(90);
pub const RENEW_INTERVAL: Duration = Duration::from_secs(60);
pub const STOP_GRACE: Duration = Duration::from_secs(5);
pub const RENEW_RETRY: Duration = Duration::from_secs(10);
