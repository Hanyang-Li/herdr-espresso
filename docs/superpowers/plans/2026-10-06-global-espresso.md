# Global keep-awake monitor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace per-pane monitoring with one herdr-wide switch that keeps the Mac awake while any agent is working/blocked, and show its state in herdr's tab bar.

**Architecture:** One detached global watcher (`herdr-espresso watch`) holds a socket subscription covering pane lifecycle events plus one `pane.agent_status_changed` entry per pane; on every wake it takes a `pane.list` snapshot, rebuilds the subscription if the pane set changed, and feeds "is any agent active" into the existing lease state machine (`policy::Machine`). It publishes `󰅶 awake` / `󰛊 idle` to a `statusline` file that the user's `ui.tab_bar_right` command entry `cat`s. `toggle` starts/stops the watcher and persists the switch in an `enabled` file; the manifest's `[[startup]]` hook restores it when herdr starts.

**Tech Stack:** Rust 2021 (`serde_json`, `libc`, `signal-hook`, `clap`), herdr socket API (line-delimited JSON over `$HERDR_SOCKET_PATH`), the `espresso` CLI.

**Spec:** `docs/superpowers/specs/2026-10-06-global-espresso-design.md`

## Global Constraints

- Platform: macOS only (`platforms = ["macos"]`); `libc::proc_pidpath` is used.
- `min_herdr_version = "0.8.2"`; plugin `version = "0.2.0"` in both `herdr-plugin.toml` and `Cargo.toml`.
- Plugin id `espresso`; the only action is `espresso.toggle`.
- Status text, exactly: active `"\u{F0176} awake"` (md-coffee), idle `"\u{F06CA} idle"` (md-coffee_outline). No emoji, no agent count. Off = no `statusline` file.
- "Active" means the pane has a non-empty `agent` and `agent_status` is `working` or `blocked` (existing `policy::active`).
- State dir files, exactly: `enabled`, `watcher.pid`, `statusline`, `watcher.log` (in `HERDR_PLUGIN_STATE_DIR`).
- Lease timings are unchanged: `LEASE` 90 s, `RENEW_INTERVAL` 60 s, `STOP_GRACE` 5 s, `RENEW_RETRY` 10 s.
- Code style: match the existing code — comments explain *why*, `eprintln!("herdr-espresso: ...")` for log lines, no new dependencies.
- Every task ends with `cargo build`, `cargo test` and `cargo fmt --check` all green. Commit messages end with:
  `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`

## File map

| File | After this plan |
|---|---|
| `src/store.rs` (new) | `StateDir`: the fixed-name files, atomic statusline write, pidfile reservation, legacy pidfile listing |
| `src/control.rs` (new) | Watcher process control: identify our processes, spawn, terminate, legacy sweep |
| `src/startup.rs` (new) | `startup` subcommand |
| `src/toggle.rs` | Global toggle |
| `src/watcher.rs` | Global watcher loop (`run_loop`, `StatusSink`, `watch`) |
| `src/herdr/rpc.rs` | `Rpc { snapshot, notify }`, `parse_snapshot`, `Snapshot` |
| `src/herdr/events.rs` | `SocketEvents::subscribe(path, pane_ids)`, `Waiter { wait, subscribe }`, `PollWaiter` |
| `src/consts.rs` | Status strings + timings; marker consts removed |
| `src/state.rs`, `src/status.rs`, `bin/status` | Deleted |
| `bin/startup` (new) | Shell shim for the startup hook |
| `herdr-plugin.toml`, `Cargo.toml` | Version 0.2.0, `[[startup]]`, single toggle action |
| `tests/watcher_loop.rs`, `tests/rpc_wire.rs` | Rewritten for the global model |
| `tests/cli.rs` (new) | Binary-level tests for `startup`/`toggle` failure paths |
| `README.md`, `README.zh-CN.md`, `docs/manual-test.md` | Rewritten for the global model |

---

### Task 1: State dir store

**Files:**
- Create: `src/store.rs`
- Modify: `src/lib.rs`, `src/consts.rs`

**Interfaces:**
- Produces (used by Tasks 4, 5):
  - `pub fn pid_alive(pid: i32) -> bool`
  - `pub fn read_pid_at(path: &Path) -> Option<i32>`
  - `#[derive(Debug, PartialEq, Eq)] pub enum Reserve { Reserved, Busy, Failed }`
  - `#[derive(Debug, Clone)] pub struct StateDir` with `new(root: impl Into<PathBuf>)`, `from_env()`, `root(&self) -> &Path`, `log_path()`, `enabled()`, `set_enabled(bool) -> io::Result<()>`, `read_pid()`, `write_pid(i32) -> io::Result<()>`, `remove_pid()`, `remove_pid_if(i32)`, `try_reserve_pid(impl Fn(i32) -> bool) -> Reserve`, `write_statusline(&str) -> io::Result<()>`, `read_statusline() -> Option<String>`, `remove_statusline()`, `legacy_pidfiles() -> Vec<PathBuf>`
  - `consts::STATUS_AWAKE: &str = "\u{F0176} awake"`, `consts::STATUS_IDLE: &str = "\u{F06CA} idle"`

- [ ] **Step 1: Write the failing tests**

Create `src/store.rs` with only the test module (the code under test comes in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> StateDir {
        let root = std::env::temp_dir().join(format!("he-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        StateDir::new(root)
    }

    #[test]
    fn enabled_flag_roundtrip() {
        let dir = temp_dir("enabled");
        assert!(!dir.enabled());
        dir.set_enabled(true).unwrap();
        assert!(dir.enabled());
        dir.set_enabled(false).unwrap();
        assert!(!dir.enabled());
        // Turning off twice is not an error.
        dir.set_enabled(false).unwrap();
    }

    #[test]
    fn statusline_is_replaced_atomically_and_removed() {
        let dir = temp_dir("statusline");
        assert_eq!(dir.read_statusline(), None);
        dir.write_statusline("a").unwrap();
        assert_eq!(dir.read_statusline().as_deref(), Some("a"));
        dir.write_statusline("b").unwrap();
        assert_eq!(dir.read_statusline().as_deref(), Some("b"));
        // The temp file used for the atomic swap never lingers.
        assert!(!dir.root().join(".statusline.tmp").exists());
        dir.remove_statusline();
        assert_eq!(dir.read_statusline(), None);
    }

    #[test]
    fn reserve_then_busy_then_stale_takeover() {
        let dir = temp_dir("reserve");
        assert_eq!(dir.try_reserve_pid(|_| true), Reserve::Reserved);
        assert_eq!(dir.read_pid(), Some(std::process::id() as i32));
        // Held by a live owner -> Busy.
        assert_eq!(dir.try_reserve_pid(|_| true), Reserve::Busy);
        // Owner no longer live -> stale, taken over.
        assert_eq!(dir.try_reserve_pid(|_| false), Reserve::Reserved);
    }

    #[test]
    fn remove_pid_if_only_removes_its_own_pid() {
        let dir = temp_dir("remove-if");
        dir.write_pid(42).unwrap();
        dir.remove_pid_if(7);
        assert_eq!(dir.read_pid(), Some(42));
        dir.remove_pid_if(42);
        assert_eq!(dir.read_pid(), None);
    }

    #[test]
    fn legacy_pidfiles_skip_the_global_pidfile() {
        let dir = temp_dir("legacy");
        std::fs::create_dir_all(dir.root()).unwrap();
        for name in ["w1%3Ap1.pid", "watcher.pid", "w1%3Ap1.log", "statusline"] {
            std::fs::write(dir.root().join(name), "1").unwrap();
        }
        assert_eq!(dir.legacy_pidfiles(), vec![dir.root().join("w1%3Ap1.pid")]);
    }

    #[test]
    fn pid_alive_for_self_only() {
        assert!(pid_alive(std::process::id() as i32));
        assert!(!pid_alive(999_999_999));
    }
}
```

Add `pub mod store;` to `src/lib.rs` (keep the list alphabetical: after `pub mod status;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib store`
Expected: compile errors — `StateDir`, `Reserve`, `pid_alive` not found.

- [ ] **Step 3: Write the implementation**

Put this above the test module in `src/store.rs`:

```rust
//! The plugin's on-disk state: a few fixed-name files in herdr's per-plugin
//! state dir. Paths go through a `StateDir` value rather than a global so each
//! test can use its own temp dir.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

const ENABLED: &str = "enabled";
const PIDFILE: &str = "watcher.pid";
const STATUSLINE: &str = "statusline";
const STATUSLINE_TMP: &str = ".statusline.tmp";
const LOG: &str = "watcher.log";

pub fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn read_pid_at(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn remove_quietly(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reserve {
    Reserved,
    Busy,
    Failed,
}

#[derive(Debug, Clone)]
pub struct StateDir {
    root: PathBuf,
}

impl StateDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// herdr's per-plugin state dir (`HERDR_PLUGIN_STATE_DIR`), or a temp dir
    /// when run outside herdr.
    pub fn from_env() -> Self {
        match std::env::var("HERDR_PLUGIN_STATE_DIR") {
            Ok(dir) if !dir.is_empty() => Self::new(dir),
            _ => Self::new(std::env::temp_dir().join("herdr-espresso")),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn log_path(&self) -> PathBuf {
        self.path(LOG)
    }

    /// The persisted switch: present = the user turned keep-awake on.
    pub fn enabled(&self) -> bool {
        self.path(ENABLED).exists()
    }

    pub fn set_enabled(&self, on: bool) -> std::io::Result<()> {
        if on {
            std::fs::create_dir_all(&self.root)?;
            std::fs::write(self.path(ENABLED), b"")
        } else {
            match std::fs::remove_file(self.path(ENABLED)) {
                Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }

    pub fn read_pid(&self) -> Option<i32> {
        read_pid_at(&self.path(PIDFILE))
    }

    pub fn write_pid(&self, pid: i32) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(self.path(PIDFILE), pid.to_string())
    }

    pub fn remove_pid(&self) {
        remove_quietly(&self.path(PIDFILE));
    }

    /// Remove the pidfile only while it still names `pid`, so an exiting
    /// watcher never deletes the pidfile of the watcher that replaced it.
    pub fn remove_pid_if(&self, pid: i32) {
        if self.read_pid() == Some(pid) {
            self.remove_pid();
        }
    }

    /// Atomically claim the pidfile (holding our own pid as a placeholder) so
    /// two concurrent toggles cannot both start a watcher. A pidfile whose pid
    /// fails `is_live` is stale and is taken over.
    pub fn try_reserve_pid(&self, is_live: impl Fn(i32) -> bool) -> Reserve {
        if std::fs::create_dir_all(&self.root).is_err() {
            return Reserve::Failed;
        }
        let path = self.path(PIDFILE);
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let _ = write!(f, "{}", std::process::id());
                    return Reserve::Reserved;
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if self.read_pid().is_some_and(&is_live) {
                        return Reserve::Busy;
                    }
                    remove_quietly(&path);
                }
                Err(_) => return Reserve::Failed,
            }
        }
        // Lost the takeover race to another reserver.
        Reserve::Busy
    }

    /// Replace the tab bar line atomically (write + rename), so the tab bar's
    /// `cat` never reads a half-written file.
    pub fn write_statusline(&self, line: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let tmp = self.path(STATUSLINE_TMP);
        std::fs::write(&tmp, format!("{line}\n"))?;
        std::fs::rename(&tmp, self.path(STATUSLINE))
    }

    pub fn read_statusline(&self) -> Option<String> {
        let s = std::fs::read_to_string(self.path(STATUSLINE)).ok()?;
        Some(s.trim_end().to_string())
    }

    pub fn remove_statusline(&self) {
        remove_quietly(&self.path(STATUSLINE));
    }

    /// Pidfiles left by 0.1.x per-pane watchers (`<pane>.pid`).
    pub fn legacy_pidfiles(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "pid")
                    && p.file_name().is_some_and(|n| n != PIDFILE)
            })
            .collect();
        out.sort();
        out
    }
}
```

In `src/consts.rs`, add below `PLUGIN_ID`:

```rust
// Tab bar status line (Nerd Font: md-coffee U+F0176, md-coffee_outline U+F06CA).
// Shown while the switch is on; with the switch off there is no line at all.
pub const STATUS_AWAKE: &str = "\u{F0176} awake";
pub const STATUS_IDLE: &str = "\u{F06CA} idle";
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test && cargo fmt --check`
Expected: all tests pass (old ones included), fmt clean.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs src/lib.rs src/consts.rs
git commit -m "feat: add the fixed-name state dir store

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `pane.list` snapshot RPC

**Files:**
- Modify: `src/herdr/rpc.rs`, `src/herdr/mod.rs`
- Test: `src/herdr/rpc.rs` (unit), `tests/rpc_wire.rs`, `tests/watcher_loop.rs` (compile fix only)

**Interfaces:**
- Consumes: `crate::policy::active(&str) -> bool`
- Produces (used by Task 4):
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct Snapshot { pub any_active: bool, pub pane_ids: Vec<String> }` — `pane_ids` sorted ascending, every pane (shells included)
  - `pub fn parse_snapshot(reply: &Value) -> Result<Snapshot, HerdrError>`
  - `Rpc::snapshot(&mut self) -> Result<Snapshot, HerdrError>` — `HerdrError::Io` means herdr is unreachable; `HerdrError::Protocol` means herdr answered with an error or a malformed reply
  - `herdr::Snapshot` re-exported from `src/herdr/mod.rs`

- [ ] **Step 1: Write the failing unit tests**

Append to the `tests` module in `src/herdr/rpc.rs`:

```rust
    fn list(panes: serde_json::Value) -> serde_json::Value {
        json!({"id":"r1","result":{"type":"pane_list","panes": panes}})
    }

    #[test]
    fn snapshot_no_panes_is_inactive() {
        let s = parse_snapshot(&list(json!([]))).unwrap();
        assert_eq!(s, Snapshot { any_active: false, pane_ids: vec![] });
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib rpc`
Expected: compile errors — `parse_snapshot` / `Snapshot` not found.

- [ ] **Step 3: Write the implementation**

In `src/herdr/rpc.rs`, add `use crate::policy::active;` to the imports, then add after `parse_pane_state`:

```rust
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
        has_agent && p.get("agent_status").and_then(Value::as_str).is_some_and(active)
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
```

Add to the `Rpc` trait (keep the existing methods for now; Task 5 removes them):

```rust
    fn snapshot(&mut self) -> Result<Snapshot, HerdrError>;
```

Add to `impl Rpc for SocketRpc`:

```rust
    fn snapshot(&mut self) -> Result<Snapshot, HerdrError> {
        let reply = self.call("pane.list", json!({}))?;
        parse_snapshot(&reply)
    }
```

In `src/herdr/mod.rs` change the rpc re-export to:

```rust
pub use rpc::{HerdrError, PaneState, Rpc, Snapshot, SocketRpc};
```

In `tests/watcher_loop.rs`, keep the file compiling by adding to `impl Rpc for FakeRpc` (Task 4 rewrites this file):

```rust
    fn snapshot(&mut self) -> Result<herdr_espresso::herdr::Snapshot, HerdrError> {
        unimplemented!("the per-pane loop never lists panes")
    }
```

- [ ] **Step 4: Add the wire test**

Append to `tests/rpc_wire.rs` (it reuses the file's `stub_server` and `request` helpers):

```rust
#[test]
fn snapshot_lists_all_panes() {
    let reply = "{\"id\":\"r1\",\"result\":{\"type\":\"pane_list\",\"panes\":[\
                 {\"pane_id\":\"w1:p1\",\"agent\":\"claude\",\"agent_status\":\"working\"},\
                 {\"pane_id\":\"w1:p2\",\"agent_status\":\"unknown\"}]}}\n"
        .to_string();
    let (sock, rx) = stub_server("list", vec![reply]);
    let mut rpc = SocketRpc::connect(&sock).unwrap();
    let snap = rpc.snapshot().expect("pane_list reply -> Ok");
    assert!(snap.any_active);
    assert_eq!(snap.pane_ids, vec!["w1:p1", "w1:p2"]);

    let req = request(&rx);
    assert_eq!(req["method"], "pane.list");
    // No workspace filter: the watcher needs every pane in the session.
    assert_eq!(req["params"], serde_json::json!({}));
    let _ = std::fs::remove_file(&sock);
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test && cargo fmt --check`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/herdr/rpc.rs src/herdr/mod.rs tests/rpc_wire.rs tests/watcher_loop.rs
git commit -m "feat: snapshot every pane with pane.list

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Subscription covering every pane

**Files:**
- Modify: `src/herdr/events.rs`, `src/herdr/mod.rs`, `src/consts.rs`, `src/watcher.rs` (compile fix only)
- Test: `tests/rpc_wire.rs`, `tests/watcher_loop.rs` (compile fix only)

**Background:** herdr emits an agent's status change **only** as `pane.agent_status_changed`, and that subscription requires a `pane_id`. So the stream must list every pane explicitly, plus unfiltered lifecycle kinds that announce when the set of panes changes. If any listed pane no longer exists, herdr rejects the whole request and closes the connection; the first line back is the ack (`result`) or an `error`. If a subscriber falls behind, herdr sends `events_lost` and closes the connection (→ EOF).

**Interfaces:**
- Consumes: `rpc::{request_line, reply_error, HerdrError}`
- Produces (used by Task 4):
  - `pub const LIFECYCLE_EVENTS: &[&str] = &["pane.created", "pane.closed", "pane.exited", "pane.agent_detected"]`
  - `pub fn subscribe_request(pane_ids: &[String]) -> String`
  - `SocketEvents::subscribe(path: &str, pane_ids: &[String]) -> Result<SocketEvents, HerdrError>` — reads and checks the ack
  - `trait Waiter { fn wait(&mut self, timeout: Option<Duration>) -> Wake; fn subscribe(&mut self, pane_ids: &[String]) -> Result<(), HerdrError>; }` — `subscribe` replaces the stream only on success; `HerdrError::Io` = herdr unreachable, `HerdrError::Protocol` = rejected
  - `PollWaiter::new(path: &str, sig_read: UnixStream) -> PollWaiter` — starts with no stream; after `Wake::Eof` it holds no stream until the next successful `subscribe`
  - `consts::SUBSCRIBE_ACK_TIMEOUT: Duration` (2 s)

- [ ] **Step 1: Write the failing wire tests**

Append to `tests/rpc_wire.rs`, and add `SocketEvents` to its `use herdr_espresso::herdr::{...}` line:

```rust
#[test]
fn subscribe_lists_lifecycle_and_every_pane() {
    let ack = "{\"id\":\"s1\",\"result\":{\"type\":\"subscription_started\"}}\n".to_string();
    let (sock, rx) = stub_server("sub", vec![ack]);
    let ids = vec!["w1:p1".to_string(), "w2:p3".to_string()];
    SocketEvents::subscribe(&sock, &ids).expect("ack -> Ok");

    let req = request(&rx);
    assert_eq!(req["method"], "events.subscribe");
    let subs = req["params"]["subscriptions"].as_array().unwrap();
    let types: Vec<&str> = subs.iter().map(|s| s["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        [
            "pane.created",
            "pane.closed",
            "pane.exited",
            "pane.agent_detected",
            "pane.agent_status_changed",
            "pane.agent_status_changed"
        ]
    );
    // Lifecycle kinds are unfiltered; status changes are per pane (herdr
    // requires a pane_id for them).
    assert!(subs[..4].iter().all(|s| s.get("pane_id").is_none()));
    assert_eq!(subs[4]["pane_id"], "w1:p1");
    assert_eq!(subs[5]["pane_id"], "w2:p3");
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn rejected_subscription_is_an_error() {
    // herdr rejects the whole request when a listed pane is gone.
    let reply = "{\"id\":\"s1\",\"error\":{\"code\":\"pane_not_found\",\"message\":\"w1:p9\"}}\n"
        .to_string();
    let (sock, _rx) = stub_server("subrej", vec![reply]);
    let err = SocketEvents::subscribe(&sock, &["w1:p9".to_string()])
        .err()
        .expect("error ack must not look like a live stream");
    assert!(format!("{err:?}").contains("pane_not_found"));
    let _ = std::fs::remove_file(&sock);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test rpc_wire`
Expected: compile error — `SocketEvents::subscribe` takes `(&str, &str)`.

- [ ] **Step 3: Implement the subscription**

In `src/consts.rs` add:

```rust
// How long `events.subscribe` may take to acknowledge before we give up.
pub const SUBSCRIBE_ACK_TIMEOUT: Duration = Duration::from_secs(2);
```

In `src/herdr/events.rs`:

1. Replace the imports at the top with:

```rust
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::rpc::{reply_error, request_line, HerdrError};
use crate::consts::SUBSCRIBE_ACK_TIMEOUT;
use serde_json::{json, Value};
```

2. Add to the `Waiter` trait:

```rust
    /// Replace the event stream with one covering `pane_ids`. The old stream
    /// is dropped only once the new one is acknowledged. `HerdrError::Io`:
    /// herdr is unreachable; `HerdrError::Protocol`: herdr rejected it (e.g.
    /// a listed pane closed in the meantime).
    fn subscribe(&mut self, pane_ids: &[String]) -> Result<(), HerdrError>;
```

3. Replace the `SocketEvents` doc comment and `subscribe` with:

```rust
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
        stream.set_read_timeout(Some(SUBSCRIBE_ACK_TIMEOUT))?;
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line)? == 0 {
            return Err(HerdrError::Protocol("subscription closed before ack".into()));
        }
        let ack: Value =
            serde_json::from_str(&line).map_err(|e| HerdrError::Protocol(e.to_string()))?;
        reply_error(&ack)?;
        // We never parse event payloads — any pushed bytes just mean
        // "re-snapshot" — so from here on poll()+drain handle the stream.
        stream.set_read_timeout(None)?;
        stream.set_nonblocking(true)?;
        Ok(Self { stream })
    }
```

(keep `raw_fd` and `drain` unchanged).

4. Replace `PollWaiter` and its impls with:

```rust
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
```

5. In `src/herdr/mod.rs`, change the events re-export to:

```rust
pub use events::{subscribe_request, PollWaiter, SocketEvents, Waiter, Wake, LIFECYCLE_EVENTS};
```

- [ ] **Step 4: Keep the old per-pane watcher compiling**

Task 4 replaces `watch()`; until then, in `src/watcher.rs` `watch()` replace the `events` / `waiter` construction:

```rust
    let events = match crate::herdr::SocketEvents::subscribe(&sock, pane_id) {
        Ok(e) => e,
        Err(_) => return 1,
    };
    let mut waiter = crate::herdr::PollWaiter::new(events, sig_read);
```

with:

```rust
    let mut waiter = crate::herdr::PollWaiter::new(&sock, sig_read);
    if waiter.subscribe(&[pane_id.to_string()]).is_err() {
        return 1;
    }
```

(`Waiter` is already imported there via `crate::herdr::{PaneState, Rpc, Waiter, Wake}`, so `subscribe` is in scope.)

In `tests/watcher_loop.rs`, add to `impl Waiter for FakeWaiter`:

```rust
    fn subscribe(&mut self, _ids: &[String]) -> Result<(), HerdrError> {
        Ok(())
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test && cargo fmt --check`
Expected: all pass, including the two new wire tests.

- [ ] **Step 6: Commit**

```bash
git add src/herdr/events.rs src/herdr/mod.rs src/consts.rs src/watcher.rs tests/rpc_wire.rs tests/watcher_loop.rs
git commit -m "feat: subscribe to every pane's status plus pane lifecycle

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Global watcher loop

**Files:**
- Modify: `src/watcher.rs`, `src/consts.rs`, `src/main.rs`
- Test: `tests/watcher_loop.rs` (rewrite)

**Interfaces:**
- Consumes: `Rpc::snapshot`, `Snapshot`, `Waiter::{wait, subscribe}`, `PollWaiter::new(&str, UnixStream)`, `StateDir` (`from_env`, `write_statusline`, `remove_statusline`, `remove_pid_if`), `STATUS_AWAKE`, `STATUS_IDLE`, `policy::{Machine, Action}`, `EspressoCtl::{rotate, kill, is_up}`
- Produces (used by Task 5):
  - `pub trait StatusSink { fn show(&mut self, line: &str); fn clear(&mut self); }`
  - `pub struct FileStatus(pub StateDir)` implementing `StatusSink`
  - `pub fn run_loop<R: Rpc, W: Waiter, C: EspressoCtl, S: StatusSink>(rpc: &mut R, waiter: &mut W, esp: &mut C, sink: &mut S, now: impl FnMut() -> Instant)`
  - `pub fn watch() -> i32` (no arguments)
  - `consts::RESUBSCRIBE_MIN_GAP: Duration` (5 s), `consts::SYNC_ROUNDS: usize` (3)

**Loop contract** (what the tests pin):
- *Sync* = up to `SYNC_ROUNDS` rounds of: snapshot; if `pane_ids` equals the subscribed set → apply `any_active`; else `waiter.subscribe(pane_ids)` and go round again (the snapshot that counts is the one taken *after* the subscription is live). Snapshot `Io` error or subscribe `Io` error → stop. Snapshot `Protocol` error → keep the previous state. Subscribe `Protocol` error → next round. Rounds exhausted → apply the latest snapshot, unless no stream was ever established → stop.
- The loop starts with a sync; then each `Wake::Event` syncs, `Wake::Timeout` only runs timers, `Wake::Stop` stops, `Wake::Eof` forgets the subscription and syncs (two EOFs < `RESUBSCRIBE_MIN_GAP` apart → stop).
- After every step: timers (`on_timer`), then the display line: `STATUS_AWAKE` if `esp.is_up()` else `STATUS_IDLE`, sent to the sink only when it changed.
- On exit, always: `esp.kill()`, `sink.clear()`.

- [ ] **Step 1: Rewrite the loop tests (failing)**

Replace the whole of `tests/watcher_loop.rs` with:

```rust
use herdr_espresso::consts::{STATUS_AWAKE, STATUS_IDLE};
use herdr_espresso::espresso::{EspressoCtl, EspressoError};
use herdr_espresso::herdr::{HerdrError, Rpc, Snapshot, Waiter, Wake};
use herdr_espresso::watcher::{run_loop, StatusSink};
use std::time::{Duration, Instant};

/// One scripted `pane.list` outcome.
#[derive(Clone)]
enum List {
    Panes(bool, &'static [&'static str]),
    Rejected,
    Unreachable,
}

fn io_err() -> HerdrError {
    HerdrError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
}

struct FakeRpc {
    script: Vec<List>,
    i: usize,
}
impl FakeRpc {
    fn new(script: Vec<List>) -> Self {
        Self { script, i: 0 }
    }
}
impl Rpc for FakeRpc {
    fn snapshot(&mut self) -> Result<Snapshot, HerdrError> {
        let step = self.script.get(self.i).cloned().unwrap_or(List::Unreachable);
        self.i += 1;
        match step {
            List::Panes(any_active, ids) => Ok(Snapshot {
                any_active,
                pane_ids: ids.iter().map(|s| s.to_string()).collect(),
            }),
            List::Rejected => Err(HerdrError::Protocol("internal".into())),
            List::Unreachable => Err(io_err()),
        }
    }
    fn notify(&mut self, _t: &str, _b: &str) -> Result<(), HerdrError> {
        Ok(())
    }
}

/// One scripted `subscribe` outcome.
#[derive(Clone, Copy)]
enum Sub {
    Ok,
    Rejected,
    Unreachable,
}

/// Yields scripted wakes (then `Stop` forever) and scripted subscribe
/// outcomes (then `Ok`), recording every subscribe attempt.
struct FakeWaiter {
    wakes: Vec<Wake>,
    waited: usize,
    subs: Vec<Sub>,
    attempts: Vec<Vec<String>>,
}
impl FakeWaiter {
    fn new(wakes: Vec<Wake>, subs: Vec<Sub>) -> Self {
        Self {
            wakes,
            waited: 0,
            subs,
            attempts: Vec::new(),
        }
    }
}
impl Waiter for FakeWaiter {
    fn wait(&mut self, _t: Option<Duration>) -> Wake {
        let w = self.wakes.get(self.waited).copied().unwrap_or(Wake::Stop);
        self.waited += 1;
        w
    }
    fn subscribe(&mut self, ids: &[String]) -> Result<(), HerdrError> {
        let outcome = self.subs.get(self.attempts.len()).copied().unwrap_or(Sub::Ok);
        self.attempts.push(ids.to_vec());
        match outcome {
            Sub::Ok => Ok(()),
            Sub::Rejected => Err(HerdrError::Protocol("pane_not_found".into())),
            Sub::Unreachable => Err(io_err()),
        }
    }
}

#[derive(Default)]
struct FakeEsp {
    up: bool,
    rotates: u32,
    kills: u32,
}
impl EspressoCtl for FakeEsp {
    fn rotate(&mut self) -> Result<(), EspressoError> {
        self.up = true;
        self.rotates += 1;
        Ok(())
    }
    fn kill(&mut self) {
        // Idempotent: only counts a kill that actually stops a running hold, so
        // the final cleanup kill is a no-op when espresso is already down.
        if self.up {
            self.up = false;
            self.kills += 1;
        }
    }
    fn is_up(&self) -> bool {
        self.up
    }
}

#[derive(Default)]
struct FakeSink {
    shown: Vec<String>,
    cleared: bool,
}
impl StatusSink for FakeSink {
    fn show(&mut self, line: &str) {
        self.shown.push(line.to_string());
        self.cleared = false;
    }
    fn clear(&mut self) {
        self.cleared = true;
    }
}

const A: &[&str] = &["w1:p1"];
const AB: &[&str] = &["w1:p1", "w1:p2"];

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A clock that advances 10 s on every read, so the 5 s stop-grace elapses
/// between consecutive steps.
fn advancing() -> impl FnMut() -> Instant {
    let start = Instant::now();
    let mut ticks = 0u64;
    move || {
        let t = start + Duration::from_secs(ticks * 10);
        ticks += 1;
        t
    }
}

fn run(
    rpc: &mut FakeRpc,
    waiter: &mut FakeWaiter,
    clock: impl FnMut() -> Instant,
) -> (FakeEsp, FakeSink) {
    let mut esp = FakeEsp::default();
    let mut sink = FakeSink::default();
    run_loop(rpc, waiter, &mut esp, &mut sink, clock);
    (esp, sink)
}

#[test]
fn seeded_active_holds_and_shows_awake_until_stop() {
    // Subscribe first, then the snapshot taken under the live subscription
    // decides — herdr does not replay events from before a subscription.
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A), List::Panes(true, A)]);
    let mut waiter = FakeWaiter::new(vec![Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.attempts, vec![ids(A)]);
    assert_eq!(rpc.i, 2);
    assert_eq!(esp.rotates, 1);
    assert_eq!(esp.kills, 1); // cleanup on Stop
    assert_eq!(sink.shown, vec![STATUS_AWAKE]);
    assert!(sink.cleared);
}

#[test]
fn seeded_idle_shows_idle() {
    let mut rpc = FakeRpc::new(vec![List::Panes(false, A), List::Panes(false, A)]);
    let mut waiter = FakeWaiter::new(vec![Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(esp.rotates, 0);
    assert_eq!(sink.shown, vec![STATUS_IDLE]);
    assert!(sink.cleared);
}

#[test]
fn all_idle_releases_after_grace_and_shows_idle() {
    let mut rpc = FakeRpc::new(vec![
        List::Panes(true, A),
        List::Panes(true, A),
        List::Panes(false, A),
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Timeout, Wake::Stop], vec![]);
    let (esp, sink) = run(&mut rpc, &mut waiter, advancing());
    assert_eq!(esp.rotates, 1);
    // Released by the stop-grace timer; the cleanup kill is then a no-op, so
    // exactly 1 proves the timer did it.
    assert_eq!(esp.kills, 1);
    assert_eq!(sink.shown, vec![STATUS_AWAKE, STATUS_IDLE]);
}

#[test]
fn reactivating_within_grace_keeps_holding() {
    let mut rpc = FakeRpc::new(vec![
        List::Panes(true, A),
        List::Panes(true, A),
        List::Panes(false, A),
        List::Panes(true, A),
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Event, Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(esp.rotates, 1);
    assert_eq!(esp.kills, 1); // only the cleanup
    assert_eq!(sink.shown, vec![STATUS_AWAKE]);
}

#[test]
fn rejected_list_keeps_the_previous_state() {
    // If the error were read as "nothing active", the stop-grace timer would
    // release espresso and the line would flip to idle.
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A), List::Panes(true, A), List::Rejected]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Timeout, Wake::Stop], vec![]);
    let (esp, sink) = run(&mut rpc, &mut waiter, advancing());
    assert_eq!(esp.rotates, 1);
    assert_eq!(esp.kills, 1); // only the cleanup
    assert_eq!(sink.shown, vec![STATUS_AWAKE]);
}

#[test]
fn unreachable_list_exits_and_cleans_up() {
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A), List::Panes(true, A), List::Unreachable]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Event], vec![]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.waited, 1);
    assert_eq!(esp.kills, 1);
    assert!(sink.cleared);
}

#[test]
fn new_pane_resubscribes_then_resnapshots() {
    // A pane appears (idle), and its agent is already working by the time the
    // new subscription is live: the post-subscribe snapshot must catch it.
    let mut rpc = FakeRpc::new(vec![
        List::Panes(false, A),
        List::Panes(false, A),
        List::Panes(false, AB),
        List::Panes(true, AB),
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Event, Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.attempts, vec![ids(A), ids(AB)]);
    assert_eq!(rpc.i, 4);
    assert_eq!(esp.rotates, 1);
    assert_eq!(sink.shown, vec![STATUS_IDLE, STATUS_AWAKE]);
}

#[test]
fn rejected_subscription_retries_with_a_fresh_list() {
    // w1:p2 closed between pane.list and events.subscribe.
    let mut rpc = FakeRpc::new(vec![
        List::Panes(false, AB),
        List::Panes(true, A),
        List::Panes(true, A),
    ]);
    let mut waiter = FakeWaiter::new(vec![Wake::Stop], vec![Sub::Rejected, Sub::Ok]);
    let t0 = Instant::now();
    let (esp, _sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.attempts, vec![ids(AB), ids(A)]);
    assert_eq!(esp.rotates, 1);
}

#[test]
fn never_subscribed_exits_without_waiting() {
    // With no stream nothing would ever wake the loop, so it must not start.
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A); 3]);
    let mut waiter = FakeWaiter::new(
        vec![Wake::Stop],
        vec![Sub::Rejected, Sub::Rejected, Sub::Rejected],
    );
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.waited, 0);
    assert_eq!(esp.rotates, 0);
    assert!(sink.shown.is_empty());
    assert!(sink.cleared);
}

#[test]
fn eof_resubscribes_and_keeps_watching() {
    // herdr closes a stream that fell behind (`events_lost`); the watcher
    // must resubscribe even though the pane set is unchanged.
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A); 4]);
    let mut waiter = FakeWaiter::new(vec![Wake::Eof, Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (esp, _sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.attempts, vec![ids(A), ids(A)]);
    assert_eq!(waiter.waited, 2);
    assert_eq!(rpc.i, 4);
    assert_eq!(esp.rotates, 1);
}

#[test]
fn eof_with_herdr_gone_exits() {
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A); 3]);
    let mut waiter = FakeWaiter::new(vec![Wake::Eof, Wake::Stop], vec![Sub::Ok, Sub::Unreachable]);
    let t0 = Instant::now();
    let (esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.waited, 1);
    assert_eq!(esp.kills, 1);
    assert!(sink.cleared);
}

#[test]
fn rapid_eofs_exit_instead_of_spinning() {
    let mut rpc = FakeRpc::new(vec![List::Panes(true, A); 6]);
    let mut waiter = FakeWaiter::new(vec![Wake::Eof, Wake::Eof, Wake::Stop], vec![]);
    let t0 = Instant::now();
    let (_esp, sink) = run(&mut rpc, &mut waiter, move || t0);
    assert_eq!(waiter.attempts.len(), 2); // initial + one resubscribe
    assert_eq!(waiter.waited, 2);
    assert!(sink.cleared);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test watcher_loop`
Expected: compile errors — `StatusSink` missing, `run_loop` signature mismatch.

- [ ] **Step 3: Implement the loop**

In `src/consts.rs` add:

```rust
// Two stream EOFs closer together than this end the watcher instead of
// resubscribing, so a server that keeps closing the stream cannot spin us.
pub const RESUBSCRIBE_MIN_GAP: Duration = Duration::from_secs(5);
// Snapshot/subscribe rounds per sync before settling for the latest snapshot.
pub const SYNC_ROUNDS: usize = 3;
```

Replace the whole of `src/watcher.rs` with:

```rust
use std::os::unix::net::UnixStream;
use std::time::Instant;

use crate::consts::{RESUBSCRIBE_MIN_GAP, STATUS_AWAKE, STATUS_IDLE, SYNC_ROUNDS};
use crate::espresso::{EspressoCtl, EspressoError};
use crate::herdr::{HerdrError, Rpc, Waiter, Wake};
use crate::policy::{Action, Machine};
use crate::store::StateDir;

/// Where the watcher publishes the tab bar line.
pub trait StatusSink {
    fn show(&mut self, line: &str);
    fn clear(&mut self);
}

/// Publishes to `statusline` in the state dir, which the user's
/// `ui.tab_bar_right` command entry reads.
pub struct FileStatus(pub StateDir);

impl StatusSink for FileStatus {
    fn show(&mut self, line: &str) {
        if let Err(e) = self.0.write_statusline(line) {
            eprintln!("herdr-espresso: write statusline failed: {e}");
        }
    }
    fn clear(&mut self) {
        self.0.remove_statusline();
    }
}

/// Outcome of bringing the subscription and the status up to date.
enum Synced {
    /// Apply this "any agent active" value.
    Active(bool),
    /// Nothing reliable to apply; keep the current state.
    Keep,
    /// herdr is gone (or we could never subscribe): end the watcher.
    Stop,
}

/// Global event loop. Blocks in the `Waiter` (kernel `poll()`) until herdr
/// pushes an event, the stop signal fires, or a timer deadline elapses — no
/// periodic polling. Every event triggers a fresh `pane.list` snapshot.
pub fn run_loop<R: Rpc, W: Waiter, C: EspressoCtl, S: StatusSink>(
    rpc: &mut R,
    waiter: &mut W,
    esp: &mut C,
    sink: &mut S,
    mut now: impl FnMut() -> Instant,
) {
    let mut machine = Machine::new();
    let mut warned_missing = false;
    let mut shown: Option<&'static str> = None;
    let mut subscribed: Option<Vec<String>> = None;
    let mut last_eof: Option<Instant> = None;

    let mut running = match sync(rpc, waiter, &mut subscribed) {
        Synced::Active(active) => {
            let n = now();
            let acts = machine.on_status(active, n);
            apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
            true
        }
        Synced::Keep => true,
        Synced::Stop => false,
    };
    if running {
        render(esp, sink, &mut shown);
    }

    while running {
        let timeout = machine
            .next_deadline()
            .map(|d| d.saturating_duration_since(now()));
        let outcome = match waiter.wait(timeout) {
            Wake::Stop => Synced::Stop,
            Wake::Timeout => Synced::Keep,
            Wake::Event => sync(rpc, waiter, &mut subscribed),
            Wake::Eof => {
                // The stream is gone: herdr exited, or it closed a stream that
                // fell behind (`events_lost`). Resubscribe unless this keeps
                // happening.
                let n = now();
                if last_eof.is_some_and(|t| n.saturating_duration_since(t) < RESUBSCRIBE_MIN_GAP) {
                    Synced::Stop
                } else {
                    last_eof = Some(n);
                    subscribed = None;
                    sync(rpc, waiter, &mut subscribed)
                }
            }
        };
        match outcome {
            Synced::Stop => running = false,
            Synced::Active(active) => {
                let n = now();
                let acts = machine.on_status(active, n);
                apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
            }
            Synced::Keep => {}
        }
        if running {
            // Fire any timers now due (renew / stop-grace). Cheap and a no-op
            // unless a deadline has actually passed.
            let n = now();
            let acts = machine.on_timer(n);
            apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
            render(esp, sink, &mut shown);
        }
    }

    // Cleanup — every exit path (toggle-off/SIGTERM, server gone). Releasing
    // espresso here never waits on the stop-grace, so toggle-off is instant.
    esp.kill();
    sink.clear();
}

/// Snapshot the panes and make sure the subscription covers exactly them. The
/// snapshot that counts is one taken while the matching subscription is live:
/// herdr does not replay events from before a subscription, so a change made
/// between an earlier snapshot and the subscribe would otherwise be missed.
fn sync<R: Rpc, W: Waiter>(
    rpc: &mut R,
    waiter: &mut W,
    subscribed: &mut Option<Vec<String>>,
) -> Synced {
    let mut latest = Synced::Keep;
    for _ in 0..SYNC_ROUNDS {
        let snap = match rpc.snapshot() {
            Ok(s) => s,
            Err(HerdrError::Io(e)) => {
                eprintln!("herdr-espresso: pane.list failed, herdr unreachable: {e}");
                return Synced::Stop;
            }
            Err(e) => {
                eprintln!("herdr-espresso: pane.list rejected: {e:?}");
                break;
            }
        };
        if subscribed.as_deref() == Some(snap.pane_ids.as_slice()) {
            return Synced::Active(snap.any_active);
        }
        latest = Synced::Active(snap.any_active);
        match waiter.subscribe(&snap.pane_ids) {
            Ok(()) => *subscribed = Some(snap.pane_ids),
            Err(HerdrError::Io(e)) => {
                eprintln!("herdr-espresso: subscribe failed, herdr unreachable: {e}");
                return Synced::Stop;
            }
            // Usually a pane that closed since the snapshot: list again.
            Err(e) => eprintln!("herdr-espresso: subscribe rejected: {e:?}"),
        }
    }
    if subscribed.is_none() {
        // Without a stream nothing would ever wake us.
        return Synced::Stop;
    }
    latest
}

fn render<C: EspressoCtl, S: StatusSink>(esp: &C, sink: &mut S, shown: &mut Option<&'static str>) {
    let want = if esp.is_up() { STATUS_AWAKE } else { STATUS_IDLE };
    if *shown != Some(want) {
        sink.show(want);
        *shown = Some(want);
    }
}

fn apply<R: Rpc, C: EspressoCtl>(
    actions: &[Action],
    esp: &mut C,
    rpc: &mut R,
    machine: &mut Machine,
    now: Instant,
    warned_missing: &mut bool,
) {
    for act in actions {
        match act {
            Action::RotateLease => match esp.rotate() {
                Ok(()) => {}
                Err(EspressoError::NotFound) => {
                    // espresso missing: warn once, and retry sooner than the
                    // normal renew cadence in case it appears on PATH.
                    if !*warned_missing {
                        *warned_missing = true;
                        let _ = rpc.notify(
                            "espresso not found",
                            "Install espresso and keep it on PATH.",
                        );
                    }
                    machine.note_rotate_failed(now);
                }
                Err(EspressoError::Spawn(_)) => machine.note_rotate_failed(now),
            },
            Action::KillEspresso => esp.kill(),
        }
    }
}

pub fn watch() -> i32 {
    let dir = StateDir::from_env();
    let code = run_watcher(&dir);
    // Only our own pidfile: a replacement watcher may already have claimed it.
    dir.remove_pid_if(std::process::id() as i32);
    code
}

fn run_watcher(dir: &StateDir) -> i32 {
    // Self-pipe for instant toggle-off: the SIGTERM handler writes a byte to
    // `sig_write`; the watcher's poll() wakes on `sig_read` and cleans up at
    // once — no periodic flag-polling, no wakeup latency.
    let (sig_read, sig_write) = match UnixStream::pair() {
        Ok(pair) => pair,
        Err(_) => return 1,
    };
    if signal_hook::low_level::pipe::register(signal_hook::consts::SIGTERM, sig_write).is_err() {
        eprintln!("herdr-espresso: failed to register SIGTERM handler");
    }

    let Some(sock) = crate::herdr::socket_path() else {
        eprintln!("herdr-espresso: HERDR_SOCKET_PATH not set");
        return 1;
    };
    let mut rpc = match crate::herdr::SocketRpc::connect(&sock) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("herdr-espresso: cannot reach herdr: {e:?}");
            return 1;
        }
    };
    let mut waiter = crate::herdr::PollWaiter::new(&sock, sig_read);
    let mut esp = crate::espresso::Lease::default();
    let mut sink = FileStatus(dir.clone());

    run_loop(&mut rpc, &mut waiter, &mut esp, &mut sink, Instant::now);
    0
}
```

In `src/main.rs`, change the `Watch` arm to:

```rust
        Command::Watch { .. } => herdr_espresso::watcher::watch(),
```

(`cli.rs` is cleaned up in Task 5.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test && cargo fmt --check`
Expected: all pass. If `cargo build` warns that `PaneState`/`set_marker` are unused, that is expected until Task 5.

- [ ] **Step 5: Commit**

```bash
git add src/watcher.rs src/consts.rs src/main.rs tests/watcher_loop.rs
git commit -m "feat: global watcher following every agent pane

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Global toggle, startup hook, and removal of the per-pane model

**Files:**
- Create: `src/control.rs`, `src/startup.rs`, `bin/startup`, `tests/cli.rs`
- Modify: `src/toggle.rs` (rewrite), `src/cli.rs`, `src/main.rs`, `src/lib.rs`, `src/consts.rs`, `src/herdr/rpc.rs`, `src/herdr/mod.rs`, `tests/rpc_wire.rs`, `herdr-plugin.toml`, `Cargo.toml`, `Cargo.lock`
- Delete: `src/state.rs`, `src/status.rs`, `bin/status`

**Interfaces:**
- Consumes: `StateDir` (all of Task 1), `store::{pid_alive, read_pid_at, Reserve}`, `watcher::watch`, `SocketRpc::{connect, notify}`, `espresso::daemon_installed`, `consts::PLUGIN_ID`
- Produces:
  - `control::is_named(pid: i32, name: &str) -> bool`, `control::is_ours(pid: i32) -> bool`
  - `control::live_watcher(dir: &StateDir) -> Option<i32>`
  - `control::terminate(pid: i32, wait: Duration) -> bool` (true = exited on SIGTERM)
  - `control::stop_watcher(dir: &StateDir, pid: i32)`
  - `#[derive(Debug, PartialEq, Eq)] pub enum Spawn { Started(i32), AlreadyRunning, Failed }`, `control::spawn_watcher(dir: &StateDir) -> Spawn`
  - `control::sweep_legacy(dir: &StateDir)`
  - `toggle::toggle() -> i32`, `startup::startup() -> i32`
  - `consts::STOP_WAIT: Duration` (3 s)

- [ ] **Step 1: Write the failing `control` unit tests**

Create `src/control.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn own_exe_name() -> String {
        std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn identifies_a_process_by_executable_name() {
        let me = std::process::id() as i32;
        assert!(is_named(me, &own_exe_name()));
        assert!(!is_named(me, "definitely-not-this"));
        // The test binary is not herdr-espresso, and a dead pid is nobody.
        assert!(!is_ours(me));
        assert!(!is_ours(999_999_999));
    }

    /// Spawn `sh -c script` and reap it on a thread, so the pid really
    /// disappears once it exits (an unreaped child stays a zombie).
    fn spawn_reaped(script: &str) -> i32 {
        let mut child = Command::new("sh").arg("-c").arg(script).spawn().unwrap();
        let pid = child.id() as i32;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        pid
    }

    #[test]
    fn terminate_stops_a_cooperative_process() {
        let pid = spawn_reaped("exec sleep 60");
        assert!(terminate(pid, Duration::from_secs(2)));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!pid_alive(pid));
    }

    #[test]
    fn terminate_kills_a_process_that_ignores_sigterm() {
        // An ignored disposition survives exec, so `sleep` ignores SIGTERM.
        let pid = spawn_reaped("trap '' TERM; exec sleep 60");
        std::thread::sleep(Duration::from_millis(100)); // let the trap install
        assert!(!terminate(pid, Duration::from_millis(200)));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!pid_alive(pid));
    }

    #[test]
    fn sweep_legacy_removes_old_pidfiles_and_logs_only() {
        let root = std::env::temp_dir().join(format!("he-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = StateDir::new(&root);
        std::fs::write(root.join("w1%3Ap1.pid"), "999999999").unwrap();
        std::fs::write(root.join("w1%3Ap1.log"), "old").unwrap();
        dir.write_pid(999_999_998).unwrap();
        sweep_legacy(&dir);
        assert!(!root.join("w1%3Ap1.pid").exists());
        assert!(!root.join("w1%3Ap1.log").exists());
        assert_eq!(dir.read_pid(), Some(999_999_998));
    }
}
```

Add `pub mod control;` to `src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib control`
Expected: compile errors — `is_named`, `terminate`, `sweep_legacy` not found.

- [ ] **Step 3: Implement `control`**

In `src/consts.rs` add:

```rust
// How long toggle-off / startup wait for a watcher to exit on SIGTERM before
// SIGKILLing it.
pub const STOP_WAIT: Duration = Duration::from_secs(3);
```

Put above the test module in `src/control.rs`:

```rust
//! Watcher process control shared by `toggle` and `startup`.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::consts::STOP_WAIT;
use crate::store::{pid_alive, read_pid_at, Reserve, StateDir};

const EXE_NAME: &str = "herdr-espresso";

/// Path of the executable running as `pid` (macOS `proc_pidpath`).
fn exe_path(pid: i32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(PathBuf::from(OsString::from_vec(buf)))
}

pub fn is_named(pid: i32, name: &str) -> bool {
    pid_alive(pid) && exe_path(pid).is_some_and(|p| p.file_name().is_some_and(|n| n == name))
}

/// A live herdr-espresso process. Liveness alone is not enough: after a reboot
/// a stale pidfile's pid can belong to any process, which we must not signal.
pub fn is_ours(pid: i32) -> bool {
    is_named(pid, EXE_NAME)
}

pub fn live_watcher(dir: &StateDir) -> Option<i32> {
    dir.read_pid().filter(|&pid| is_ours(pid))
}

/// SIGTERM `pid`, wait up to `wait` for it to exit, then SIGKILL it. Returns
/// whether it exited on SIGTERM (i.e. ran its own cleanup).
pub fn terminate(pid: i32, wait: Duration) -> bool {
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    false
}

pub fn stop_watcher(dir: &StateDir, pid: i32) {
    if !terminate(pid, STOP_WAIT) {
        // Killed hard, so its own cleanup never ran. Its espresso lease still
        // expires on its own within LEASE.
        dir.remove_statusline();
    }
    dir.remove_pid_if(pid);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Spawn {
    Started(i32),
    AlreadyRunning,
    Failed,
}

/// Start the detached global watcher, unless one is already running.
pub fn spawn_watcher(dir: &StateDir) -> Spawn {
    match dir.try_reserve_pid(is_ours) {
        Reserve::Reserved => {}
        Reserve::Busy => return Spawn::AlreadyRunning,
        Reserve::Failed => return Spawn::Failed,
    }
    let failed = || {
        dir.remove_pid();
        Spawn::Failed
    };
    let Ok(exe) = std::env::current_exe() else {
        return failed();
    };

    // Default both streams to null: a detached watcher must NEVER inherit the
    // caller's tty (Command's default for an unset stream is inherit()).
    // Upgrade to the log file, truncated per start, when it is available.
    let mut cmd = Command::new(exe);
    cmd.arg("watch")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(dir.log_path())
    {
        if let Ok(f2) = f.try_clone() {
            cmd.stderr(Stdio::from(f2));
        }
        cmd.stdout(Stdio::from(f));
    }
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else {
        return failed();
    };
    // A watcher that cannot reach herdr exits at once; catch that here rather
    // than report "on" with nothing running. try_wait (not a liveness probe):
    // our exited child stays a zombie until reaped, and kill(0) succeeds on it.
    std::thread::sleep(Duration::from_millis(100));
    if !matches!(child.try_wait(), Ok(None)) {
        return failed();
    }
    let pid = child.id() as i32;
    if dir.write_pid(pid).is_err() {
        return failed();
    }
    Spawn::Started(pid)
}

/// Stop 0.1.x per-pane watchers and drop their files. On SIGTERM they clear
/// their own `$espresso` pane token, so no marker is left behind.
pub fn sweep_legacy(dir: &StateDir) {
    for path in dir.legacy_pidfiles() {
        if let Some(pid) = read_pid_at(&path) {
            if is_ours(pid) {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("log"));
    }
}
```

Run: `cargo test --lib control`
Expected: 4 tests pass.

- [ ] **Step 4: Write the failing CLI tests**

Create `tests/cli.rs`:

```rust
//! Binary-level checks for the paths that need no running herdr.

use std::path::{Path, PathBuf};
use std::process::Command;

fn state_dir(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("he-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run(sub: &str, dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_herdr-espresso"))
        .arg(sub)
        .env("HERDR_PLUGIN_STATE_DIR", dir)
        .env_remove("HERDR_SOCKET_PATH")
        .output()
        .unwrap()
}

#[test]
fn startup_while_off_clears_leftovers() {
    // A watcher killed hard leaves its line and pidfile behind.
    let dir = state_dir("startup-off");
    std::fs::write(dir.join("statusline"), "x\n").unwrap();
    std::fs::write(dir.join("watcher.pid"), "999999999").unwrap();
    let out = run("startup", &dir);
    assert!(out.status.success());
    assert!(!dir.join("statusline").exists());
    assert!(!dir.join("watcher.pid").exists());
}

#[test]
fn toggle_on_without_herdr_fails_and_stays_off() {
    // The watcher exits at once without a socket; toggle must notice and not
    // leave the switch on.
    let dir = state_dir("toggle-fail");
    let out = run("toggle", &dir);
    assert!(!out.status.success());
    assert!(!dir.join("enabled").exists());
    assert!(!dir.join("watcher.pid").exists());
}

#[test]
fn startup_while_on_without_herdr_fails_and_keeps_the_switch() {
    // The switch is the user's intent; a failed restore must not erase it.
    let dir = state_dir("startup-on");
    std::fs::write(dir.join("enabled"), "").unwrap();
    std::fs::write(dir.join("statusline"), "x\n").unwrap();
    let out = run("startup", &dir);
    assert!(!out.status.success());
    assert!(dir.join("enabled").exists());
    assert!(!dir.join("statusline").exists());
}
```

Run: `cargo test --test cli`
Expected: FAIL — `startup` is not a subcommand yet (clap exits 2), so `startup_while_off_clears_leftovers` fails.

- [ ] **Step 5: Rewrite `toggle`, add `startup`, wire the CLI**

Replace the whole of `src/toggle.rs` with:

```rust
use crate::consts::PLUGIN_ID;
use crate::control::{self, Spawn};
use crate::herdr::{self, Rpc, SocketRpc};
use crate::store::StateDir;

/// Turn the global keep-awake monitor on or off. Decided by whether a watcher
/// is actually running, not by the `enabled` file: a watcher that died leaves
/// the tab bar empty (looks off), and one press must turn it back on.
pub fn toggle() -> i32 {
    let dir = StateDir::from_env();
    control::sweep_legacy(&dir);

    if let Some(pid) = control::live_watcher(&dir) {
        if let Err(e) = dir.set_enabled(false) {
            eprintln!("herdr-espresso: clear enabled failed: {e}");
        }
        control::stop_watcher(&dir, pid);
        notify("espresso off", "No longer keeping the Mac awake.");
        println!("espresso: off");
        return 0;
    }

    if let Err(e) = dir.set_enabled(true) {
        eprintln!("herdr-espresso: set enabled failed: {e}");
        return 1;
    }
    match control::spawn_watcher(&dir) {
        Spawn::Started(_) => {
            notify("espresso on", "Keeping the Mac awake while any agent works.");
            if !crate::espresso::daemon_installed() {
                notify(
                    &format!("{PLUGIN_ID}: lid-closed not active"),
                    "Run `espresso daemon install` for lid-closed keep-awake.",
                );
            }
            println!("espresso: on");
            0
        }
        Spawn::AlreadyRunning => {
            println!("espresso: already on");
            0
        }
        Spawn::Failed => {
            let _ = dir.set_enabled(false);
            eprintln!(
                "herdr-espresso: watcher failed to start, see {}",
                dir.log_path().display()
            );
            1
        }
    }
}

/// Best-effort toast; toggling works the same when herdr cannot show it.
fn notify(title: &str, body: &str) {
    let Some(sock) = herdr::socket_path() else {
        return;
    };
    match SocketRpc::connect(&sock) {
        Ok(mut rpc) => {
            if let Err(e) = rpc.notify(title, body) {
                eprintln!("herdr-espresso: notify failed: {e:?}");
            }
        }
        Err(e) => eprintln!("herdr-espresso: notify failed: {e:?}"),
    }
}
```

Create `src/startup.rs`:

```rust
use crate::control::{self, Spawn};
use crate::store::StateDir;

/// The manifest's `[[startup]]` hook: herdr runs it once the server is up,
/// including after a live handoff. Restores the switch from `enabled`.
pub fn startup() -> i32 {
    let dir = StateDir::from_env();
    control::sweep_legacy(&dir);

    // Replace rather than reuse a live watcher: after a live handoff the
    // outgoing one can still look alive, then exit on its dropped stream and
    // leave none running.
    if let Some(pid) = control::live_watcher(&dir) {
        control::stop_watcher(&dir, pid);
    }

    if !dir.enabled() {
        // Leftovers from a watcher that was killed hard.
        dir.remove_statusline();
        dir.remove_pid();
        return 0;
    }
    match control::spawn_watcher(&dir) {
        Spawn::Started(_) | Spawn::AlreadyRunning => 0,
        Spawn::Failed => {
            dir.remove_statusline();
            eprintln!(
                "herdr-espresso: watcher failed to start, see {}",
                dir.log_path().display()
            );
            1
        }
    }
}
```

Replace the `Command` enum in `src/cli.rs` with:

```rust
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Turn the global keep-awake monitor on or off.
    Toggle,
    /// Restore the monitor after herdr starts (manifest `[[startup]]` hook).
    Startup,
    #[command(hide = true)]
    Watch,
}
```

Replace the `match` in `src/main.rs` with:

```rust
    let code = match Cli::parse().command {
        Command::Toggle => herdr_espresso::toggle::toggle(),
        Command::Startup => herdr_espresso::startup::startup(),
        Command::Watch => herdr_espresso::watcher::watch(),
    };
```

Set `src/lib.rs` to:

```rust
pub mod consts;
pub mod control;
pub mod espresso;
pub mod herdr;
pub mod policy;
pub mod startup;
pub mod store;
pub mod toggle;
pub mod watcher;
```

Create `bin/startup`:

```bash
#!/usr/bin/env bash
# bin/startup
exec "${HERDR_PLUGIN_ROOT:?}/target/release/herdr-espresso" startup "$@"
```

and `chmod +x bin/startup`.

- [ ] **Step 6: Remove the per-pane leftovers**

- `git rm src/state.rs src/status.rs bin/status`
- `src/consts.rs`: delete `METADATA_SOURCE`, `MARKER_TOKEN`, `MARKER` and the sidebar-marker comment above them.
- `src/herdr/rpc.rs`: delete `PaneState`, `parse_pane_state`, the `pane_state` / `set_marker` / `clear_marker` trait methods and their `SocketRpc` impls, the `MARKER`/`MARKER_TOKEN`/`METADATA_SOURCE` import, and the unit tests `parse_state_agent_present`, `parse_state_no_agent_when_agent_field_absent_or_null`, `parse_state_gone_on_error_reply`. Reword the `reply_error` doc comment's last sentence to: `treats a rejected request as done.` The trait becomes:

```rust
pub trait Rpc {
    fn snapshot(&mut self) -> Result<Snapshot, HerdrError>;
    fn notify(&mut self, title: &str, body: &str) -> Result<(), HerdrError>;
}
```

- `src/herdr/mod.rs`: rpc re-export becomes `pub use rpc::{HerdrError, Rpc, Snapshot, SocketRpc};`; delete `focused_pane_id`.
- `tests/rpc_wire.rs`: change the module doc to `//! Wire-shape tests for the herdr socket requests, against a stub of herdr's socket.`; delete `set_marker_reports_a_sidebar_token`, `clear_marker_nulls_the_token` and `ok_reply`; rewrite `error_reply_surfaces_as_err` to use `notify`:

```rust
#[test]
fn error_reply_surfaces_as_err() {
    // A healthy connection carrying an `error` body must not read as success.
    let reply = "{\"id\":\"r1\",\"error\":{\"code\":\"busy\",\"message\":\"toast rate limited\"}}\n"
        .to_string();
    let (sock, _rx) = stub_server("err", vec![reply]);
    let mut rpc = SocketRpc::connect(&sock).unwrap();
    let err = rpc
        .notify("t", "b")
        .expect_err("an error reply must not be reported as success");
    assert!(format!("{err:?}").contains("busy"));
    let _ = std::fs::remove_file(&sock);
}
```

- [ ] **Step 7: Manifest and version**

Replace `herdr-plugin.toml` with:

```toml
id = "espresso"
name = "Espresso Guard"
version = "0.2.0"
min_herdr_version = "0.8.2"
description = "Keep macOS awake (incl. lid-closed) while any herdr agent is working."
platforms = ["macos"]

[[build]]
command = ["cargo", "build", "--release"]

[[startup]]
command = ["bash", "bin/startup"]

[[actions]]
id = "toggle"
title = "Espresso: toggle global keep-awake"
contexts = ["global", "workspace", "pane"]
command = ["bash", "bin/toggle"]
```

In `Cargo.toml` set `version = "0.2.0"` and `description = "Keep macOS awake (incl. lid-closed) while any herdr agent is working."`.

- [ ] **Step 8: Run everything**

Run: `cargo build --release && cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: builds with no warnings (`Cargo.lock` now records 0.2.0), all tests pass (including `tests/cli.rs`), fmt and clippy clean. Then:

Run: `grep -rn "pane_state\|set_marker\|MARKER\|focused_pane_id\|list_monitored\|state::" src tests`
Expected: no output.

- [ ] **Step 9: Commit**

```bash
git add -A src tests bin herdr-plugin.toml Cargo.toml Cargo.lock
git commit -m "feat: global toggle and startup restore; drop per-pane monitoring

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Documentation

**Files:**
- Modify: `README.md`, `README.zh-CN.md`, `docs/manual-test.md`

No code. Keep each README's existing sections that are not mentioned below (badges, Installation, Uninstalling, Development, License) unchanged, except: in Installation, the `--ref` example becomes `--ref v0.2.0`.

- [ ] **Step 1: `README.md`**

Replace the blockquote tagline with:

```markdown
> Keep your Mac awake while any coding agent in herdr is working — one switch for all of herdr.
```

Replace the intro paragraph and its three bullets (everything between the badges and `## Features`) with:

```markdown
`herdr-espresso` is a [herdr](https://github.com/ogulcancelik/herdr) plugin that
keeps macOS awake while any agent in herdr is actively working, and lets the Mac
sleep again once they have all stopped. It drives the
[`espresso`](https://github.com/Hanyang-Li/espresso) CLI: when you switch it on,
one small detached watcher follows every pane's agent status over the herdr
socket and holds (or releases) an `espresso` session accordingly.

- **Any agent working/blocked → awake.** While at least one agent is active the
  Mac won't sleep (and, with espresso's lid-closed helper installed, stays awake
  even with the lid shut).
- **All idle/done → sleep.** When the last active agent stops, `espresso` is
  released after a short grace period; the switch stays on and re-acquires it
  the next time any agent becomes active.
- **One switch for all of herdr.** Toggle it once; it covers every workspace,
  tab and pane, including ones opened later, and survives herdr restarts.
```

Replace `## Features` bullets with:

```markdown
- **Follows every agent.** No per-pane setup — panes and agents that appear
  later are picked up automatically.
- **Tab bar status.** `󰅶 awake` / `󰛊 idle` on the right of herdr's tab bar while
  the switch is on, nothing while it is off — one line of herdr config away
  (see [Tab bar status](#tab-bar-status)).
- **Remembers the switch.** Turned on, it comes back on by itself after herdr
  restarts.
- **Self-healing.** `espresso` is held via a short lease renewed while agents
  work, so a crashed watcher can never pin the Mac awake — the lease expires on
  its own within ~90s.
- **Event-driven.** The watcher blocks in the kernel and uses ~0% CPU while
  idle; toggle-off is instant.
```

In `## Requirements`, change the herdr line to `- [herdr](https://github.com/ogulcancelik/herdr) **0.8.2** or later.` and the optional line's last sentence to `Without it, only idle/display sleep is prevented; switching on shows a reminder.`

In `## Keybinding`, change `description = "espresso: toggle monitor on focused pane"` to `description = "espresso: toggle global keep-awake"` and replace the paragraph after the code block with:

```markdown
Press it anywhere to switch keep-awake on or off for all of herdr. A toast
confirms each switch. The action is also listed in herdr's command palette as
**Espresso: toggle global keep-awake**.
```

Replace the whole `## Sidebar marker` section with:

````markdown
## Tab bar status

While the switch is on, the plugin writes its state to a small file that a
herdr tab bar entry can display. Add a command entry to `ui.tab_bar_right`:

```toml
[ui]
tab_bar_right = [
  { type = "command", command = "cat ~/.local/state/herdr/plugins/espresso/statusline", interval_seconds = 2, timeout_seconds = 1 },
]
```

| Shown | Meaning |
|---|---|
| `󰅶 awake` | On, and at least one agent is working or blocked — the Mac stays awake. |
| `󰛊 idle` | On, all agents idle — the Mac may sleep. |
| *(nothing)* | Off. |

The icons are Nerd Font glyphs (md-coffee, md-coffee_outline); use a Nerd Font
in your terminal. If you set `XDG_STATE_HOME`, the file lives at
`$XDG_STATE_HOME/herdr/plugins/espresso/statusline` instead. Other entries
(`datetime`, `hostname`, …) can sit alongside it in the same list.

### Upgrading from 0.1

0.1 monitored panes one at a time and marked them in the sidebar with a
`$espresso` token. 0.2 replaces that with the global switch: the old per-pane
watchers are stopped automatically the first time 0.2 runs, and you can remove
`$espresso` from `[ui.sidebar.agents] rows` (and any `rows_by_agent` entries).
````

Replace the bullets of `## How it works` with:

```markdown
- **One watcher.** Switching on starts one detached watcher. It subscribes to
  herdr's pane lifecycle events plus every pane's agent-status events, and
  re-reads the pane list (`pane.list`) whenever something changes; when panes
  come or go it rebuilds the subscription.
- **Any working/blocked → awake.** The watcher holds `espresso` via a short
  90-second lease that it renews (rotates) every 60 seconds while any agent
  stays active, so the hold never has a coverage gap.
- **All idle/done → release after grace.** Once no agent is active, the watcher
  waits ~5 seconds (so a brief flip back to active doesn't thrash it) before
  releasing `espresso`. The switch stays on.
- **Self-healing lease.** Because the lease is short and renewed rather than
  held open-ended, a watcher that dies unexpectedly (crash, `kill -9`) can't
  keep the Mac awake — its `espresso` lease expires within ~90 seconds.
- **Restart-safe.** The switch is remembered in herdr's plugin state dir; when
  the herdr server starts, the plugin's startup hook brings the watcher back.
  Detaching a client doesn't affect it; `herdr server stop` ends it.
```

- [ ] **Step 2: `README.zh-CN.md`**

Apply the same structural changes in Chinese, mirroring the English text above section by section. Use this wording for the parts users see most:

- Tagline: `> 只要 herdr 里有任意编码 agent 在工作，就让 Mac 保持唤醒——整个 herdr 一个开关。`
- Section titles: `## 标签栏状态`（replacing `## 侧边栏标记`）, `### 从 0.1 升级`.
- Status table rows: `` `󰅶 awake` `` → `已开启，且至少一个 agent 在 working 或 blocked——Mac 保持唤醒。`; `` `󰛊 idle` `` → `已开启，所有 agent 都空闲——Mac 可以睡眠。`; `*（不显示）*` → `已关闭。`
- Keybinding description string stays English (`espresso: toggle global keep-awake`), as it is config.
- Requirement: `herdr **0.8.2** 或更高版本`.

Every code block must be identical to the English README's.

- [ ] **Step 3: `docs/manual-test.md`**

Replace the whole file with:

````markdown
# Manual test checklist

These steps need a real herdr (≥ 0.8.2) and the real `espresso` CLI on `PATH` —
they exercise macOS sleep assertions and herdr's socket and plugin hooks, which
the automated tests stub out. Run them on macOS.

`$STATE` below is `~/.local/state/herdr/plugins/espresso` (or
`$XDG_STATE_HOME/herdr/plugins/espresso`).

## Setup

1. Link the plugin into your local herdr from this repo:

   ```bash
   herdr plugin link "$PWD"
   ```

   Confirm the build hook succeeds and `target/release/herdr-espresso` exists.

2. Add the keybinding and the tab bar entry to your herdr config:

   ```toml
   [[keys.command]]
   key = "prefix+/"
   type = "plugin_action"
   command = "espresso.toggle"
   description = "espresso: toggle global keep-awake"

   [ui]
   tab_bar_right = [
     { type = "command", command = "cat ~/.local/state/herdr/plugins/espresso/statusline", interval_seconds = 2, timeout_seconds = 1 },
   ]
   ```

   `herdr server reload-config` must report `"diagnostics": []`.

## Checklist

- [ ] **1. Toggle on.** Press `prefix+/` with any pane focused (a plain shell is
      fine). An `espresso on` toast appears, `$STATE/enabled` and
      `$STATE/watcher.pid` exist, and within ~2 s the tab bar shows `󰛊 idle`
      (or `󰅶 awake` if an agent is already working).

- [ ] **2. Any agent working → awake.** Drive an agent in any pane to `working`.
      Within ~2 s the tab bar shows `󰅶 awake`, and
      `pmset -g assertions | grep -i espresso` shows a hold.

- [ ] **3. Two agents, one stops → still awake.** Start a second agent in
      another workspace and make both work. Let one go idle: the tab bar stays
      `󰅶 awake` and the assertion stays.

- [ ] **4. All idle → release after grace.** Let the last agent go idle. After
      ~5 s the tab bar shows `󰛊 idle` and the espresso assertion is gone. The
      switch is still on (`$STATE/enabled` exists).

- [ ] **5. New pane is followed.** With the switch on and everything idle, open
      a new pane, start an agent there and make it work. The tab bar turns
      `󰅶 awake` — the watcher picked up a pane that did not exist when it
      started.

- [ ] **6. Toggle off.** Press `prefix+/` again. An `espresso off` toast
      appears, the tab bar entry disappears, `$STATE/enabled`,
      `$STATE/watcher.pid` and `$STATE/statusline` are gone, and no espresso
      assertion remains.

- [ ] **7. Restart restores the switch.** Toggle on, then `herdr server stop`
      and start herdr again. Without pressing anything the tab bar comes back
      (`󰛊 idle` / `󰅶 awake`) and `$STATE/watcher.pid` names a new live
      process. Repeat with the switch off: after the restart the entry stays
      hidden and no watcher runs.

- [ ] **8. Watcher crash self-heals.** With an agent working, `kill -9` the pid
      in `$STATE/watcher.pid`. The espresso assertion is *not* dropped at once
      but expires within ~90 s. The tab bar may keep showing the stale line
      until the next toggle or herdr start; pressing `prefix+/` once starts a
      fresh watcher (a dead watcher counts as off).

- [ ] **9. Upgrade from 0.1 cleans up.** With 0.1 installed, monitor a pane so
      its sidebar shows `espresso`. Link 0.2 and press `prefix+/`. The old
      per-pane watcher exits, the sidebar marker disappears, and
      `$STATE/*.pid` contains only `watcher.pid`.

- [ ] **10. Missing-daemon reminder.** Without `espresso daemon install`, toggle
      on: a toast tells you to run `espresso daemon install` for lid-closed
      keep-awake.

- [ ] **11. Idle CPU.** With the switch on and several panes open but idle,
      `top -pid <watcher pid>` shows ~0% CPU.
````

- [ ] **Step 4: Check and commit**

Run: `grep -n "Sidebar marker\|侧边栏标记\|focused pane\|0\.7\.0\|herdr-espresso status" README.md README.zh-CN.md docs/manual-test.md`
Expected: no output.

```bash
git add README.md README.zh-CN.md docs/manual-test.md
git commit -m "docs: document the global switch and tab bar status

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```
