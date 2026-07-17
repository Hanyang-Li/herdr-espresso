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

/// A pane's monitoring-relevant state from `pane.get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneState {
    /// Pane does not exist / unreachable (pane.get returned an error).
    Gone,
    /// Pane exists but has no agent (a plain shell, or the agent has exited).
    NoAgent,
    /// Pane has an agent; carries its `agent_status` (working/blocked/idle/...).
    Agent(String),
}

/// Distinguish agent-present from no-agent using the `agent` field (authoritative
/// "is there an agent"), falling back to `agent_status` for the status string.
pub fn parse_pane_state(reply: &Value) -> PaneState {
    let Some(pane) = reply.get("result").and_then(|r| r.get("pane")) else {
        return PaneState::Gone;
    };
    match pane.get("agent").and_then(Value::as_str) {
        // `agent` present and non-empty -> a real agent occupies the pane.
        Some(agent) if !agent.is_empty() => {
            let status = pane
                .get("agent_status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            PaneState::Agent(status)
        }
        // No `agent` field -> shell / agent exited.
        _ => PaneState::NoAgent,
    }
}

pub trait Rpc {
    fn pane_state(&mut self, pane_id: &str) -> Result<PaneState, HerdrError>;
    fn set_marker(&mut self, pane_id: &str) -> Result<(), HerdrError>;
    fn clear_marker(&mut self, pane_id: &str) -> Result<(), HerdrError>;
    fn notify(&mut self, title: &str, body: &str) -> Result<(), HerdrError>;
}

pub struct SocketRpc {
    path: String,
    seq: u64,
}

impl SocketRpc {
    pub fn connect(path: &str) -> Result<Self, HerdrError> {
        // herdr's API socket serves ONE request per connection and closes it
        // after the response, so we cannot hold a connection open across calls
        // — each `call` opens a fresh one. Probe once here so a bad socket path
        // fails fast (and `watch` can bail before setting the marker).
        UnixStream::connect(path)?;
        Ok(Self {
            path: path.to_string(),
            seq: 0,
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, HerdrError> {
        self.seq += 1;
        let id = format!("r{}", self.seq);
        // Fresh connection per request (herdr closes it after one response).
        let mut stream = UnixStream::connect(&self.path)?;
        stream.write_all(request_line(&id, method, params).as_bytes())?;
        stream.flush()?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(HerdrError::Protocol("eof".into()));
        }
        serde_json::from_str(&line).map_err(|e| HerdrError::Protocol(e.to_string()))
    }
}

impl Rpc for SocketRpc {
    fn pane_state(&mut self, pane_id: &str) -> Result<PaneState, HerdrError> {
        let reply = self.call("pane.get", json!({"pane_id": pane_id}))?;
        Ok(parse_pane_state(&reply))
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
    fn parse_state_agent_present() {
        let reply = json!({"id":"r1","result":{"type":"pane_info",
            "pane":{"pane_id":"w1:p1","agent":"claude","agent_status":"working"}}});
        assert_eq!(
            parse_pane_state(&reply),
            PaneState::Agent("working".to_string())
        );
    }

    #[test]
    fn parse_state_no_agent_when_agent_field_absent_or_null() {
        // Plain shell / agent exited: herdr reports agent_status "unknown" and
        // no `agent` field.
        let shell = json!({"id":"r1","result":{"type":"pane_info",
            "pane":{"pane_id":"w1:p1","agent_status":"unknown"}}});
        assert_eq!(parse_pane_state(&shell), PaneState::NoAgent);
        let null_agent = json!({"id":"r1","result":{"type":"pane_info",
            "pane":{"pane_id":"w1:p1","agent":null,"agent_status":"unknown"}}});
        assert_eq!(parse_pane_state(&null_agent), PaneState::NoAgent);
    }

    #[test]
    fn parse_state_gone_on_error_reply() {
        let err = json!({"id":"r1","error":{"code":"not_found"}});
        assert_eq!(parse_pane_state(&err), PaneState::Gone);
    }
}
