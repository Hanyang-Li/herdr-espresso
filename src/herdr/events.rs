use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::rpc::{reply_error, request_line, HerdrError};
use crate::consts::SUBSCRIBE_ACK_TIMEOUT;
use serde_json::{json, Value};

/// What woke a `Waiter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// herdr pushed one or more events on the subscription — re-snapshot status.
    Event,
    /// The stop signal fired (toggle-off / SIGTERM).
    Stop,
    /// A timer deadline elapsed with nothing else pending.
    Timeout,
    /// The herdr event stream closed (server gone).
    Eof,
}

/// Blocks until something happens. This is the event-driven core: the thread is
/// suspended by the kernel inside `poll()` and consumes no CPU until an event
/// arrives, the stop signal fires, or the deadline elapses — there is no
/// periodic polling.
pub trait Waiter {
    /// Wait until an event / stop / EOF, or until `timeout` elapses. `None`
    /// blocks indefinitely (until event/stop/eof).
    fn wait(&mut self, timeout: Option<Duration>) -> Wake;

    /// Replace the event stream with one covering `pane_ids`. The old stream
    /// is dropped only once the new one is acknowledged. `HerdrError::Io`:
    /// herdr is unreachable; `HerdrError::Protocol`: herdr rejected it (e.g.
    /// a listed pane closed in the meantime).
    fn subscribe(&mut self, pane_ids: &[String]) -> Result<(), HerdrError>;
}

/// Unfiltered pane lifecycle kinds. They announce changes to the set of panes,
/// which is what tells the watcher to rebuild the per-pane status entries.
pub const LIFECYCLE_EVENTS: &[&str] = &[
    "pane.created",
    "pane.closed",
    "pane.exited",
    "pane.agent_detected",
];

/// `pane.agent_status_changed` is the only event herdr emits when an agent's
/// status changes, and it must name a pane — so every pane gets its own entry.
pub fn subscribe_request(pane_ids: &[String]) -> String {
    let mut subs: Vec<Value> = LIFECYCLE_EVENTS
        .iter()
        .map(|t| json!({ "type": t }))
        .collect();
    subs.extend(
        pane_ids
            .iter()
            .map(|id| json!({ "type": "pane.agent_status_changed", "pane_id": id })),
    );
    request_line("s1", "events.subscribe", json!({ "subscriptions": subs }))
}

/// A subscribed event stream (one long-lived connection).
pub struct SocketEvents {
    stream: UnixStream,
}

impl SocketEvents {
    pub fn subscribe(path: &str, pane_ids: &[String]) -> Result<Self, HerdrError> {
        let mut stream = UnixStream::connect(path)?;
        stream.write_all(subscribe_request(pane_ids).as_bytes())?;
        stream.flush()?;
        // Read the ack before going non-blocking: a rejected request must fail
        // here rather than look like a healthy stream that never speaks. Bytes
        // the BufReader reads past the ack are events we drop — harmless, the
        // caller snapshots right after subscribing.
        if let Err(e) = stream.set_read_timeout(Some(SUBSCRIBE_ACK_TIMEOUT)) {
            // Tolerate EINVAL: the peer has already closed (e.g. in tests after sending reply).
            if e.raw_os_error() != Some(libc::EINVAL) {
                return Err(HerdrError::Io(e));
            }
        }
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(HerdrError::Protocol(
                "subscription closed before ack".into(),
            ));
        }
        let ack: Value =
            serde_json::from_str(&line).map_err(|e| HerdrError::Protocol(e.to_string()))?;
        reply_error(&ack)?;
        // We never parse event payloads — any pushed bytes just mean
        // "re-snapshot" — so from here on poll()+drain handle the stream.
        if let Err(e) = stream.set_read_timeout(None) {
            // Tolerate EINVAL: the peer has closed (timeout occurred or connection was reset).
            if e.raw_os_error() != Some(libc::EINVAL) {
                return Err(HerdrError::Io(e));
            }
        }
        stream.set_nonblocking(true)?;
        Ok(Self { stream })
    }

    fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Consume all currently-available bytes (pushed events) so `poll()`
    /// won't immediately re-fire. Returns `false` on EOF (herdr closed).
    fn drain(&mut self) -> bool {
        let mut buf = [0u8; 4096];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => return false, // EOF
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
                Err(_) => return false,
            }
        }
    }
}

/// Real `Waiter`: `poll()`s the herdr event socket (when subscribed) and a
/// self-pipe fed by the SIGTERM handler. Woken only by an actual event, the
/// stop signal, or the deadline — no busy loop, no periodic wakeups.
pub struct PollWaiter {
    path: String,
    events: Option<SocketEvents>,
    /// Read end of the self-pipe; SIGTERM writes a byte to its write end.
    sig_read: UnixStream,
}

impl PollWaiter {
    pub fn new(path: &str, sig_read: UnixStream) -> Self {
        Self {
            path: path.to_string(),
            events: None,
            sig_read,
        }
    }
}

impl Waiter for PollWaiter {
    fn wait(&mut self, timeout: Option<Duration>) -> Wake {
        let timeout_ms: libc::c_int = match timeout {
            Some(d) => {
                let ms = d.as_millis();
                if ms > libc::c_int::MAX as u128 {
                    libc::c_int::MAX
                } else {
                    ms as libc::c_int
                }
            }
            None => -1,
        };
        let mut fds = vec![libc::pollfd {
            fd: self.sig_read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        if let Some(events) = &self.events {
            fds.push(libc::pollfd {
                fd: events.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if rc <= 0 {
            // rc == 0: timeout. rc < 0: EINTR/error — re-evaluate on the next
            // loop (a SIGTERM that interrupted poll shows up as the self-pipe
            // being readable on the very next poll).
            return Wake::Timeout;
        }
        let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR;
        // Stop takes priority over events.
        if fds[0].revents & ready != 0 {
            return Wake::Stop;
        }
        if fds.len() > 1 && fds[1].revents & ready != 0 {
            let open = self.events.as_mut().is_some_and(|e| e.drain());
            if open {
                Wake::Event
            } else {
                // Server gone, or it closed the stream (`events_lost`).
                self.events = None;
                Wake::Eof
            }
        } else {
            Wake::Timeout
        }
    }

    fn subscribe(&mut self, pane_ids: &[String]) -> Result<(), HerdrError> {
        self.events = Some(SocketEvents::subscribe(&self.path, pane_ids)?);
        Ok(())
    }
}
