pub mod events;
pub mod rpc;

pub use events::{subscribe_request, PollWaiter, SocketEvents, Waiter, Wake, LIFECYCLE_EVENTS};
pub use rpc::{HerdrError, Rpc, Snapshot, SocketRpc};

pub fn socket_path() -> Option<String> {
    std::env::var("HERDR_SOCKET_PATH").ok()
}
