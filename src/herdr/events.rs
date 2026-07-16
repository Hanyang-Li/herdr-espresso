use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::json;
use super::rpc::{request_line, HerdrError};

pub enum NextLine { Line, Timeout, Eof }

pub trait Events {
    fn next_line(&mut self, timeout: Option<Duration>) -> NextLine;
}

/// Remove and return one '\n'-terminated line from the front of `buf`.
/// Returns None if no complete line is buffered yet — partial bytes are left
/// intact so they are not lost across reads.
fn take_line(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let pos = buf.iter().position(|&b| b == b'\n')?;
    Some(buf.drain(..=pos).collect())
}

pub struct SocketEvents {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl SocketEvents {
    pub fn subscribe(path: &str, pane_id: &str) -> Result<Self, HerdrError> {
        let mut stream = UnixStream::connect(path)?;
        let sub = request_line("s1", "events.subscribe", json!({"subscriptions":[
            {"type":"pane.agent_status_changed","pane_id": pane_id},
            {"type":"pane.closed","pane_id": pane_id}
        ]}));
        stream.write_all(sub.as_bytes())?;
        stream.flush()?;
        let mut me = Self { stream, buf: Vec::new() };
        // Consume the subscription ack line, bounded so a dead daemon can't
        // hang startup. Any bytes past the ack stay in `buf` for next_line.
        me.stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut chunk = [0u8; 4096];
        loop {
            if take_line(&mut me.buf).is_some() {
                break;
            }
            match me.stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => me.buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        Ok(me)
    }
}

impl Events for SocketEvents {
    fn next_line(&mut self, timeout: Option<Duration>) -> NextLine {
        if take_line(&mut self.buf).is_some() {
            return NextLine::Line;
        }
        let _ = self.stream.set_read_timeout(timeout);
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return NextLine::Eof,
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    if take_line(&mut self.buf).is_some() {
                        return NextLine::Line;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => return NextLine::Timeout,
                Err(_) => return NextLine::Eof,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::take_line;

    #[test]
    fn take_line_returns_none_on_partial_and_retains_bytes() {
        let mut buf = b"{\"partial\":".to_vec();
        assert!(take_line(&mut buf).is_none());
        assert_eq!(buf, b"{\"partial\":");
    }

    #[test]
    fn take_line_extracts_one_line_and_keeps_the_rest() {
        let mut buf = b"line1\nline2-partial".to_vec();
        assert_eq!(take_line(&mut buf), Some(b"line1\n".to_vec()));
        assert_eq!(buf, b"line2-partial");
        assert!(take_line(&mut buf).is_none());
    }

    #[test]
    fn take_line_handles_multiple_buffered_lines() {
        let mut buf = b"a\nb\n".to_vec();
        assert_eq!(take_line(&mut buf), Some(b"a\n".to_vec()));
        assert_eq!(take_line(&mut buf), Some(b"b\n".to_vec()));
        assert!(take_line(&mut buf).is_none());
    }
}
