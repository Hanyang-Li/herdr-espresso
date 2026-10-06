pub mod events;
pub mod rpc;

pub use events::{subscribe_request, PollWaiter, SocketEvents, Waiter, Wake, LIFECYCLE_EVENTS};
pub use rpc::{HerdrError, Rpc, Snapshot, SocketRpc};

pub fn socket_path() -> Option<String> {
    std::env::var("HERDR_SOCKET_PATH").ok()
}

/// Treat EINVAL from `set_*_timeout` as success: macOS returns it when the
/// peer has already closed the socket (e.g. a stub that replied and hung up),
/// and the following read then reports the EOF or the reply itself.
pub(crate) fn tolerate_einval(r: std::io::Result<()>) -> std::io::Result<()> {
    match r {
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        other => other,
    }
}
