use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use crate::consts::{MARKER, MARKER_TOKEN, METADATA_SOURCE};
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
/// treats a rejected request as done — how a marker that never once reached the
/// sidebar looked healthy for weeks.
pub fn reply_error(reply: &Value) -> Result<(), HerdrError> {
    let Some(err) = reply.get("error") else {
        return Ok(());
    };
    let code = err.get("code").and_then(Value::as_str).unwrap_or("error");
    let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
    Err(HerdrError::Protocol(format!("{code}: {msg}")))
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
    fn pane_state(&mut self, pane_id: &str) -> Result<PaneState, HerdrError>;
    fn set_marker(&mut self, pane_id: &str) -> Result<(), HerdrError>;
    fn clear_marker(&mut self, pane_id: &str) -> Result<(), HerdrError>;
    fn notify(&mut self, title: &str, body: &str) -> Result<(), HerdrError>;
    fn snapshot(&mut self) -> Result<Snapshot, HerdrError>;
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
        let reply = self.call(
            "pane.report_metadata",
            json!({
                "pane_id": pane_id, "source": METADATA_SOURCE,
                "tokens": { MARKER_TOKEN: MARKER }
            }),
        )?;
        reply_error(&reply)
    }
    fn clear_marker(&mut self, pane_id: &str) -> Result<(), HerdrError> {
        // A null value removes only our token, leaving tokens reported by other
        // sources on the pane alone.
        let reply = self.call(
            "pane.report_metadata",
            json!({
                "pane_id": pane_id, "source": METADATA_SOURCE,
                "tokens": { MARKER_TOKEN: Value::Null }
            }),
        )?;
        reply_error(&reply)
    }
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
