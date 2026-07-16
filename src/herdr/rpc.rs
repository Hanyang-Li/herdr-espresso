use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use crate::consts::{MARKER, METADATA_SOURCE};

#[derive(Debug)]
pub enum HerdrError {
    Io(std::io::Error),
    Protocol(String),
}
impl From<std::io::Error> for HerdrError {
    fn from(e: std::io::Error) -> Self {
        HerdrError::Io(e)
    }
}

pub fn request_line(id: &str, method: &str, params: Value) -> String {
    let mut s = serde_json::to_string(&json!({
        "id": id, "method": method, "params": params
    }))
    .expect("serialize request");
    s.push('\n');
    s
}

pub fn parse_pane_status(reply: &Value) -> Option<String> {
    reply
        .get("result")?
        .get("pane")?
        .get("agent_status")?
        .as_str()
        .map(str::to_string)
}

pub trait Rpc {
    fn pane_status(&mut self, pane_id: &str) -> Result<Option<String>, HerdrError>;
    fn set_marker(&mut self, pane_id: &str) -> Result<(), HerdrError>;
    fn clear_marker(&mut self, pane_id: &str) -> Result<(), HerdrError>;
    fn notify(&mut self, title: &str, body: &str) -> Result<(), HerdrError>;
}

pub struct SocketRpc {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    seq: u64,
}

impl SocketRpc {
    pub fn connect(path: &str) -> Result<Self, HerdrError> {
        let stream = UnixStream::connect(path)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            reader,
            writer: stream,
            seq: 0,
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, HerdrError> {
        self.seq += 1;
        let id = format!("r{}", self.seq);
        self.writer
            .write_all(request_line(&id, method, params).as_bytes())?;
        self.writer.flush()?;
        // One-shot connection: the next line is our reply.
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(HerdrError::Protocol("eof".into()));
        }
        serde_json::from_str(&line).map_err(|e| HerdrError::Protocol(e.to_string()))
    }
}

impl Rpc for SocketRpc {
    fn pane_status(&mut self, pane_id: &str) -> Result<Option<String>, HerdrError> {
        let reply = self.call("pane.get", json!({"pane_id": pane_id}))?;
        Ok(parse_pane_status(&reply))
    }
    fn set_marker(&mut self, pane_id: &str) -> Result<(), HerdrError> {
        self.call(
            "pane.report_metadata",
            json!({
                "pane_id": pane_id, "source": METADATA_SOURCE, "custom_status": MARKER
            }),
        )?;
        Ok(())
    }
    fn clear_marker(&mut self, pane_id: &str) -> Result<(), HerdrError> {
        self.call(
            "pane.report_metadata",
            json!({
                "pane_id": pane_id, "source": METADATA_SOURCE, "clear_custom_status": true
            }),
        )?;
        Ok(())
    }
    fn notify(&mut self, title: &str, body: &str) -> Result<(), HerdrError> {
        self.call("notification.show", json!({"title": title, "body": body}))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_line_is_one_json_line() {
        let line = request_line("r1", "pane.get", json!({"pane_id":"w1:p1"}));
        assert!(line.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["method"], "pane.get");
        assert_eq!(v["params"]["pane_id"], "w1:p1");
        assert_eq!(v["id"], "r1");
    }

    #[test]
    fn parse_status_reads_agent_status() {
        let reply = json!({"id":"r1","result":{"type":"pane_info",
            "pane":{"pane_id":"w1:p1","agent_status":"working"}}});
        assert_eq!(parse_pane_status(&reply), Some("working".to_string()));
    }

    #[test]
    fn parse_status_none_when_error_or_missing() {
        let err = json!({"id":"r1","error":{"code":"not_found"}});
        assert_eq!(parse_pane_status(&err), None);
        let no_agent = json!({"id":"r1","result":{"type":"pane_info",
            "pane":{"pane_id":"w1:p1"}}});
        assert_eq!(parse_pane_status(&no_agent), None);
    }
}
