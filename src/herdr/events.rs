use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::rpc::{request_line, HerdrError};
use serde_json::json;

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
}

/// A subscribed event stream for one pane (one long-lived connection).
pub struct SocketEvents {
    stream: UnixStream,
}

impl SocketEvents {
    pub fn subscribe(path: &str, pane_id: &str) -> Result<Self, HerdrError> {
        let mut stream = UnixStream::connect(path)?;
        let sub = request_line(
            "s1",
            "events.subscribe",
            json!({"subscriptions":[
                {"type":"pane.agent_status_changed","pane_id": pane_id},
                {"type":"pane.closed","pane_id": pane_id}
            ]}),
        );
        stream.write_all(sub.as_bytes())?;
        stream.flush()?;
        // We never parse event payloads — any pushed bytes just mean
        // "re-snapshot" — so switch to non-blocking and let poll()+drain handle
        // the ack and every subsequent event uniformly. Non-blocking also means
        // subscription setup can never hang startup.
        stream.set_nonblocking(true)?;
        Ok(Self { stream })
    }

    fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Consume all currently-available bytes (ack and/or events) so `poll()`
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

/// Real `Waiter`: `poll()`s the herdr event socket and a self-pipe fed by the
/// SIGTERM handler. Woken only by an actual event, the stop signal, or the
/// deadline — no busy loop, no periodic wakeups.
pub struct PollWaiter {
    events: SocketEvents,
    /// Read end of the self-pipe; SIGTERM writes a byte to its write end.
    sig_read: UnixStream,
}

impl PollWaiter {
    pub fn new(events: SocketEvents, sig_read: UnixStream) -> Self {
        Self { events, sig_read }
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
        let mut fds = [
            libc::pollfd {
                fd: self.events.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.sig_read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if rc <= 0 {
            // rc == 0: timeout. rc < 0: EINTR/error — re-evaluate on the next
            // loop (a SIGTERM that interrupted poll shows up as the self-pipe
            // being readable on the very next poll).
            return Wake::Timeout;
        }
        // Stop takes priority over events.
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            return Wake::Stop;
        }
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            if self.events.drain() {
                Wake::Event
            } else {
                Wake::Eof
            }
        } else {
            Wake::Timeout
        }
    }
}
