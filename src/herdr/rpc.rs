use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use crate::consts::RPC_TIMEOUT;
use crate::herdr::tolerate_einval;
use crate::policy::active;

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

/// Turn an `error` reply into an `Err`. The socket call itself succeeds when
/// herdr rejects a request, so a caller that only checks transport errors
/// treats a rejected request as done.
pub fn reply_error(reply: &Value) -> Result<(), HerdrError> {
    let Some(err) = reply.get("error") else {
        return Ok(());
    };
    let code = err.get("code").and_then(Value::as_str).unwrap_or("error");
    let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
    Err(HerdrError::Protocol(format!("{code}: {msg}")))
}

/// What the watcher needs from one `pane.list`: whether any agent is active,
/// and every pane id (to subscribe to each pane's status events).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub any_active: bool,
    /// Sorted, so snapshots of the same panes compare equal.
    pub pane_ids: Vec<String>,
}

pub fn parse_snapshot(reply: &Value) -> Result<Snapshot, HerdrError> {
    reply_error(reply)?;
    let panes = reply
        .get("result")
        .and_then(|r| r.get("panes"))
        .and_then(Value::as_array)
        .ok_or_else(|| HerdrError::Protocol("pane.list: no result.panes".into()))?;
    let any_active = panes.iter().any(|p| {
        let has_agent = p
            .get("agent")
            .and_then(Value::as_str)
            .is_some_and(|a| !a.is_empty());
        has_agent
            && p.get("agent_status")
                .and_then(Value::as_str)
                .is_some_and(active)
    });
    let mut pane_ids: Vec<String> = panes
        .iter()
        .filter_map(|p| p.get("pane_id")?.as_str().map(str::to_string))
        .collect();
    pane_ids.sort();
    Ok(Snapshot {
        any_active,
        pane_ids,
    })
}

pub trait Rpc {
    fn snapshot(&mut self) -> Result<Snapshot, HerdrError>;
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
        // Bound both directions: a herdr that accepts but never answers must
        // surface as an Io error, not block the caller forever. A timed-out
        // read maps to HerdrError::Io via `?` (watcher: stop; notify: best effort).
        tolerate_einval(stream.set_read_timeout(Some(RPC_TIMEOUT)))?;
        tolerate_einval(stream.set_write_timeout(Some(RPC_TIMEOUT)))?;
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
    fn notify(&mut self, title: &str, body: &str) -> Result<(), HerdrError> {
        let reply = self.call("notification.show", json!({"title": title, "body": body}))?;
        reply_error(&reply)
    }
    fn snapshot(&mut self) -> Result<Snapshot, HerdrError> {
        let reply = self.call("pane.list", json!({}))?;
        parse_snapshot(&reply)
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

    fn list(panes: serde_json::Value) -> serde_json::Value {
        json!({"id":"r1","result":{"type":"pane_list","panes": panes}})
    }

    #[test]
    fn snapshot_no_panes_is_inactive() {
        let s = parse_snapshot(&list(json!([]))).unwrap();
        assert_eq!(
            s,
            Snapshot {
                any_active: false,
                pane_ids: vec![]
            }
        );
    }

    #[test]
    fn snapshot_shells_only_is_inactive_but_lists_every_pane() {
        let s = parse_snapshot(&list(json!([
            {"pane_id":"w1:p2","agent_status":"unknown"},
            {"pane_id":"w1:p1","agent_status":"unknown"}
        ])))
        .unwrap();
        assert!(!s.any_active);
        // Sorted, so two snapshots of the same panes compare equal.
        assert_eq!(s.pane_ids, vec!["w1:p1", "w1:p2"]);
    }

    #[test]
    fn snapshot_idle_agent_is_inactive() {
        let s = parse_snapshot(&list(json!([
            {"pane_id":"w1:p1","agent":"claude","agent_status":"idle"}
        ])))
        .unwrap();
        assert!(!s.any_active);
    }

    #[test]
    fn snapshot_working_or_blocked_agent_is_active() {
        for status in ["working", "blocked"] {
            let s = parse_snapshot(&list(json!([
                {"pane_id":"w1:p1","agent":"claude","agent_status":"idle"},
                {"pane_id":"w2:p1","agent":"codex","agent_status": status}
            ])))
            .unwrap();
            assert!(s.any_active, "{status} should count as active");
        }
    }

    #[test]
    fn snapshot_status_without_agent_is_inactive() {
        // herdr only reports a real agent through `agent`; a status on a pane
        // without one never counts.
        let s = parse_snapshot(&list(json!([
            {"pane_id":"w1:p1","agent_status":"working"}
        ])))
        .unwrap();
        assert!(!s.any_active);
    }

    #[test]
    fn snapshot_error_reply_is_a_protocol_error() {
        let err = parse_snapshot(&json!({"id":"r1","error":{"code":"internal","message":"x"}}))
            .expect_err("error reply");
        assert!(matches!(err, HerdrError::Protocol(m) if m.contains("internal")));
    }

    #[test]
    fn snapshot_missing_panes_is_a_protocol_error() {
        let err = parse_snapshot(&json!({"id":"r1","result":{}})).expect_err("malformed");
        assert!(matches!(err, HerdrError::Protocol(_)));
    }
}
