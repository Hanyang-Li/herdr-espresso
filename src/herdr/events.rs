use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::json;
use super::rpc::{request_line, HerdrError};

pub enum NextLine { Line, Timeout, Eof }

pub trait Events {
    fn next_line(&mut self, timeout: Option<Duration>) -> NextLine;
}

pub struct SocketEvents {
    reader: BufReader<UnixStream>,
}

impl SocketEvents {
    pub fn subscribe(path: &str, pane_id: &str) -> Result<Self, HerdrError> {
        let mut stream = UnixStream::connect(path)?;
        use std::io::Write;
        let sub = request_line("s1", "events.subscribe", json!({"subscriptions":[
            {"type":"pane.agent_status_changed","pane_id": pane_id},
            {"type":"pane.closed","pane_id": pane_id}
        ]}));
        stream.write_all(sub.as_bytes())?;
        stream.flush()?;
        let mut reader = BufReader::new(stream);
        // Consume the subscription ack line.
        let mut ack = String::new();
        reader.read_line(&mut ack)?;
        Ok(Self { reader })
    }
}

impl Events for SocketEvents {
    fn next_line(&mut self, timeout: Option<Duration>) -> NextLine {
        // SO_RCVTIMEO via the underlying stream.
        let stream = self.reader.get_ref();
        let _ = stream.set_read_timeout(timeout);
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => NextLine::Eof,
            Ok(_) => NextLine::Line,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut => NextLine::Timeout,
            Err(_) => NextLine::Eof,
        }
    }
}
