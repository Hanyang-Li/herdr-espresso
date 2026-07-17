pub mod events;
pub mod rpc;

pub use events::{PollWaiter, SocketEvents, Waiter, Wake};
pub use rpc::{HerdrError, PaneState, Rpc, SocketRpc};

pub fn socket_path() -> Option<String> {
    std::env::var("HERDR_SOCKET_PATH").ok()
}

pub fn focused_pane_id() -> Option<String> {
    if let Ok(id) = std::env::var("HERDR_PANE_ID") {
        if !id.is_empty() {
            return Some(id);
        }
    }
    let ctx = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?;
    let v: serde_json::Value = serde_json::from_str(&ctx).ok()?;
    v.get("focused_pane_id")?.as_str().map(str::to_string)
}
