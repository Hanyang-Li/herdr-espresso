# herdr-espresso Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A macOS herdr plugin that keeps the Mac awake (via the `espresso`
CLI, including lid-closed) while a monitored pane's coding agent is working or
blocked, toggled per pane by a keybinding, with per-pane independence and
leak-proof cleanup.

**Architecture:** One binary `herdr-espresso` with three subcommands. `toggle`
(the keybinding action) turns monitoring on/off for `$HERDR_PANE_ID` by
spawning/killing a detached per-pane `watch` process. Each `watch` process owns
one pane: it subscribes to that pane's herdr socket events, and drives a short
leased `espresso -t 90` child that it renews by process rotation while the agent
is active. All decision logic lives in a pure, clock-injected state machine;
herdr socket IO and espresso process control sit behind traits so the machine is
unit-testable without a real herdr or espresso.

**Tech Stack:** Rust 1.96 (2021 edition), `serde` + `serde_json`, `libc` (for
`kill`, `setsid`), `signal-hook` (SIGTERM handling). herdr socket API
(line-delimited JSON over the unix socket at `$HERDR_SOCKET_PATH`).

## Global Constraints

- Platform: macOS only. `herdr-plugin.toml` sets `platforms = ["macos"]`.
- `min_herdr_version = "0.7.0"`.
- Plugin id: `espresso`. Toggle action id: `espresso.toggle`.
- Metadata `source` string: `espresso` (ASCII `[A-Za-z0-9:._-]`, ≤80 chars).
- Sidebar marker `custom_status`: `󰅶` (U+F0176; ≤32 chars).
- Timing constants (fixed, no config surface in v1): `LEASE = 90s`,
  `RENEW_INTERVAL = 60s`, `STOP_GRACE = 5s`, renew retry on spawn failure `10s`.
- "Active" agent statuses (keep espresso open): `working`, `blocked`. Everything
  else (`idle`, `done`, `unknown`, …) is inactive.
- Keep-awake is RAII: killing an espresso process releases its assertion and its
  daemon refcount. Never rely on espresso handling signals specially.
- License: MIT. Commit messages end with the Co-Authored-By trailer used in this
  repo's existing commits.

---

## File Structure

```
herdr-espresso/
  Cargo.toml            # bin "herdr-espresso"; deps serde, serde_json, libc, signal-hook
  herdr-plugin.toml     # plugin manifest (id=espresso, macos, build+actions)
  .gitignore            # /target
  LICENSE               # MIT
  README.md             # install, keybinding, behavior, helper prerequisite
  bin/toggle            # exec "$HERDR_PLUGIN_ROOT/target/release/herdr-espresso" toggle "$@"
  bin/status            # exec "$HERDR_PLUGIN_ROOT/target/release/herdr-espresso" status "$@"
  src/
    main.rs             # clap dispatch: toggle | watch | status
    cli.rs              # clap arg definitions
    consts.rs           # timing constants, marker, source, plugin id
    policy.rs           # pure state machine: active(status) + Machine (clock-injected)
    state.rs            # pidfile path sanitize + read/write/liveness + reconcile
    espresso.rs         # EspressoCtl trait + real leased-child impl + daemon status probe
    herdr/
      mod.rs            # re-exports; env helpers (socket path, pane id)
      rpc.rs            # Rpc: connect + one-shot request/response; pane.get, report_metadata, notification.show
      events.rs         # Subscriber: connect + subscribe + blocking read-with-timeout of pushed lines
    toggle.rs           # toggle on/off; spawn detached watcher; reconcile
    watcher.rs          # per-pane event+timer loop wiring Machine + traits
    status.rs           # list monitored panes
  tests/
    (integration tests as noted per task)
  docs/
    manual-test.md      # real-herdr verification checklist
```

**Two-connection model:** `Subscriber` (events.rs) only reads pushed event lines
after subscribing; `Rpc` (rpc.rs) only does request→response. Separate unix
sockets to the same `$HERDR_SOCKET_PATH` avoid interleaving pushed events with
RPC replies on one stream.

**Re-snapshot strategy:** the watcher never parses event payloads. Any pushed
line means "re-read authoritative state": it calls `Rpc::pane_get(pane_id)`. A
successful reply yields `agent_status`; an error/not-found reply means the pane
is gone → clean up and exit.

---

## Task 1: Project scaffold builds and runs

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `LICENSE`, `src/main.rs`, `src/cli.rs`,
  `src/consts.rs`

**Interfaces:**
- Produces: binary `herdr-espresso` with clap subcommands `toggle`, `watch
  <pane_id>`, `status`; `consts` module with the timing/marker constants.

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "herdr-espresso"
version = "0.1.0"
edition = "2021"
description = "Keep macOS awake (incl. lid-closed) while a herdr pane's agent is working."
license = "MIT"

[[bin]]
name = "herdr-espresso"
path = "src/main.rs"

[dependencies]
clap = { version = "4.5", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
libc = "0.2"
signal-hook = "0.3"
```

- [ ] **Step 2: Create `.gitignore`**

```
/target
```

- [ ] **Step 3: Create `LICENSE`** — standard MIT text, copyright holder
  `Ragdoll_SL`, year 2026.

- [ ] **Step 4: Create `src/consts.rs`**

```rust
use std::time::Duration;

pub const PLUGIN_ID: &str = "espresso";
pub const METADATA_SOURCE: &str = "espresso";
pub const MARKER: &str = "\u{f0176}"; // 󰅶

pub const LEASE: Duration = Duration::from_secs(90);
pub const RENEW_INTERVAL: Duration = Duration::from_secs(60);
pub const STOP_GRACE: Duration = Duration::from_secs(5);
pub const RENEW_RETRY: Duration = Duration::from_secs(10);
```

- [ ] **Step 5: Create `src/cli.rs`**

```rust
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "herdr-espresso", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Toggle monitoring for the focused pane ($HERDR_PANE_ID).
    Toggle,
    /// Internal: per-pane watcher (spawned detached by `toggle`).
    #[command(hide = true)]
    Watch { pane_id: String },
    /// List currently monitored panes.
    Status,
}
```

- [ ] **Step 6: Create `src/main.rs`**

```rust
mod cli;
mod consts;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    let code = match Cli::parse().command {
        Command::Toggle => {
            eprintln!("toggle: not yet implemented");
            1
        }
        Command::Watch { pane_id } => {
            eprintln!("watch {pane_id}: not yet implemented");
            1
        }
        Command::Status => {
            eprintln!("status: not yet implemented");
            1
        }
    };
    std::process::exit(code);
}
```

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: compiles; `target/debug/herdr-espresso --help` lists `toggle`,
`status` (not `watch`, which is hidden).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore LICENSE src/
git commit -m "feat: scaffold herdr-espresso binary and CLI

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Policy state machine (pure, clock-injected)

**Files:**
- Create: `src/policy.rs`
- Modify: `src/main.rs` (add `mod policy;`)

**Interfaces:**
- Produces:
  - `fn active(status: &str) -> bool`
  - `enum Action { RotateLease, KillEspresso }`
  - `struct Machine` with:
    - `fn new() -> Machine`
    - `fn on_status(&mut self, active: bool, now: Instant) -> Vec<Action>`
    - `fn on_timer(&mut self, now: Instant) -> Vec<Action>`
    - `fn next_deadline(&self) -> Option<Instant>`
  - Machine internal state: `active: bool`, `espresso_up: bool`,
    `pending_stop: Option<Instant>`, `next_renew: Option<Instant>`.

Semantics the tests below lock in:
- Becoming active (from inactive): cancel `pending_stop`; if espresso not up,
  emit `RotateLease` now and set `next_renew = now + RENEW_INTERVAL`,
  `espresso_up = true`.
- Staying active on a repeat status: no action (renew is driven by the timer).
- Becoming inactive: if espresso up and no `pending_stop`, set
  `pending_stop = now + STOP_GRACE`. No immediate kill.
- Timer fires:
  - if `pending_stop` is due → emit `KillEspresso`, clear `pending_stop` and
    `next_renew`, `espresso_up = false`.
  - else if `next_renew` is due and active → emit `RotateLease`, set
    `next_renew = now + RENEW_INTERVAL`.
- `next_deadline` = earliest of `pending_stop` / `next_renew` that is set.

- [ ] **Step 1: Write failing tests**

```rust
// src/policy.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{RENEW_INTERVAL, STOP_GRACE};
    use std::time::{Duration, Instant};

    #[test]
    fn active_only_for_working_and_blocked() {
        assert!(active("working"));
        assert!(active("blocked"));
        assert!(!active("idle"));
        assert!(!active("done"));
        assert!(!active("unknown"));
        assert!(!active(""));
    }

    #[test]
    fn activating_opens_espresso_immediately_and_schedules_renew() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        assert_eq!(m.on_status(true, t0), vec![Action::RotateLease]);
        assert_eq!(m.next_deadline(), Some(t0 + RENEW_INTERVAL));
    }

    #[test]
    fn repeat_active_status_does_nothing() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0);
        assert_eq!(m.on_status(true, t0), vec![]);
    }

    #[test]
    fn renew_timer_rotates_and_reschedules() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0);
        let t1 = t0 + RENEW_INTERVAL;
        assert_eq!(m.on_timer(t1), vec![Action::RotateLease]);
        assert_eq!(m.next_deadline(), Some(t1 + RENEW_INTERVAL));
    }

    #[test]
    fn inactive_arms_stop_grace_then_kills() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0);
        assert_eq!(m.on_status(false, t0), vec![]); // no immediate kill
        assert_eq!(m.next_deadline(), Some(t0 + STOP_GRACE));
        let t1 = t0 + STOP_GRACE;
        assert_eq!(m.on_timer(t1), vec![Action::KillEspresso]);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn reactivating_before_grace_cancels_kill() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0);
        m.on_status(false, t0);
        // back to active before grace fires: no new open (espresso still up), grace cancelled
        assert_eq!(m.on_status(true, t0), vec![]);
        assert_eq!(m.next_deadline(), Some(t0 + RENEW_INTERVAL));
    }

    #[test]
    fn inactive_does_not_surface_stale_renew_deadline() {
        // Deactivating shortly before a scheduled renew must expose only the
        // stop-grace deadline, not the (suppressed) renew — otherwise the
        // scheduler wakes on a deadline that fires nothing and stalls.
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0); // next_renew = t0 + RENEW_INTERVAL
        let t1 = t0 + Duration::from_secs(58);
        m.on_status(false, t1); // pending_stop = t1 + STOP_GRACE
        assert_eq!(m.next_deadline(), Some(t1 + STOP_GRACE));
    }
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test policy`
Expected: FAIL — `active`, `Machine`, `Action` not defined.

- [ ] **Step 3: Implement `src/policy.rs`**

```rust
use crate::consts::{RENEW_INTERVAL, STOP_GRACE};
use std::time::Instant;

pub fn active(status: &str) -> bool {
    matches!(status, "working" | "blocked")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    RotateLease,
    KillEspresso,
}

#[derive(Debug, Default)]
pub struct Machine {
    active: bool,
    espresso_up: bool,
    pending_stop: Option<Instant>,
    next_renew: Option<Instant>,
}

impl Machine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_status(&mut self, active: bool, now: Instant) -> Vec<Action> {
        self.active = active;
        if active {
            self.pending_stop = None;
            if !self.espresso_up {
                self.espresso_up = true;
                self.next_renew = Some(now + RENEW_INTERVAL);
                return vec![Action::RotateLease];
            }
            return vec![];
        }
        if self.espresso_up && self.pending_stop.is_none() {
            self.pending_stop = Some(now + STOP_GRACE);
        }
        vec![]
    }

    pub fn on_timer(&mut self, now: Instant) -> Vec<Action> {
        if let Some(stop) = self.pending_stop {
            if now >= stop {
                self.pending_stop = None;
                self.next_renew = None;
                self.espresso_up = false;
                return vec![Action::KillEspresso];
            }
        }
        if self.active {
            if let Some(renew) = self.next_renew {
                if now >= renew {
                    self.next_renew = Some(now + RENEW_INTERVAL);
                    return vec![Action::RotateLease];
                }
            }
        }
        vec![]
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        // While inactive, next_renew is retained (so reactivation resumes the
        // original schedule) but must NOT surface as a deadline — the renew
        // action is suppressed by `on_timer`'s `if self.active` guard, so a
        // visible renew deadline would fire nothing and stall in the past.
        let renew = if self.active { self.next_renew } else { None };
        [self.pending_stop, renew].into_iter().flatten().min()
    }
}
```

Also add `mod policy;` to `src/main.rs`.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test policy`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add src/policy.rs src/main.rs
git commit -m "feat: pure policy state machine for espresso lease/debounce

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Pidfile state (sanitize, read/write, liveness)

**Files:**
- Create: `src/state.rs`
- Modify: `src/main.rs` (add `mod state;`)

**Interfaces:**
- Produces:
  - `fn sanitize_pane_id(pane_id: &str) -> String`
  - `fn state_dir() -> PathBuf` (from `$HERDR_PLUGIN_STATE_DIR`, fallback
    `$TMPDIR/herdr-espresso`)
  - `fn pidfile(pane_id: &str) -> PathBuf`
  - `fn write_pidfile(pane_id: &str, pid: i32) -> std::io::Result<()>`
  - `fn read_pidfile(pane_id: &str) -> Option<i32>`
  - `fn remove_pidfile(pane_id: &str)`
  - `fn pid_alive(pid: i32) -> bool` (via `libc::kill(pid, 0)`)
  - `fn list_monitored() -> Vec<(String /*sanitized*/, i32)>`

Sanitization rule: keep ASCII `[A-Za-z0-9._-]`; every other byte becomes
`%XX` (uppercase hex). This is readable for the common `w1:p1` (→ `w1%3Ap1`)
and collision-free.

- [ ] **Step 1: Write failing tests**

```rust
// src/state.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_is_readable_and_collision_free() {
        assert_eq!(sanitize_pane_id("w1:p1"), "w1%3Ap1");
        assert_eq!(sanitize_pane_id("w1.p1_2-3"), "w1.p1_2-3");
        assert_ne!(sanitize_pane_id("a:b"), sanitize_pane_id("a-b"));
        assert_ne!(sanitize_pane_id("a/b"), sanitize_pane_id("a%2Fb"));
    }

    #[test]
    fn roundtrip_pidfile(/* uses a temp HERDR_PLUGIN_STATE_DIR */) {
        let dir = std::env::temp_dir().join(format!("he-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HERDR_PLUGIN_STATE_DIR", &dir);
        assert_eq!(read_pidfile("w1:pX"), None);
        write_pidfile("w1:pX", 4242).unwrap();
        assert_eq!(read_pidfile("w1:pX"), Some(4242));
        remove_pidfile("w1:pX");
        assert_eq!(read_pidfile("w1:pX"), None);
    }

    #[test]
    fn self_pid_is_alive() {
        assert!(pid_alive(std::process::id() as i32));
        assert!(!pid_alive(999_999_999));
    }
}
```

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test state`
Expected: FAIL — functions not defined.

- [ ] **Step 3: Implement `src/state.rs`**

```rust
use std::path::PathBuf;

pub fn sanitize_pane_id(pane_id: &str) -> String {
    let mut out = String::with_capacity(pane_id.len());
    for &b in pane_id.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HERDR_PLUGIN_STATE_DIR") {
        return PathBuf::from(dir);
    }
    std::env::temp_dir().join("herdr-espresso")
}

pub fn pidfile(pane_id: &str) -> PathBuf {
    state_dir().join(format!("{}.pid", sanitize_pane_id(pane_id)))
}

pub fn write_pidfile(pane_id: &str, pid: i32) -> std::io::Result<()> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(pidfile(pane_id), pid.to_string())
}

pub fn read_pidfile(pane_id: &str) -> Option<i32> {
    let s = std::fs::read_to_string(pidfile(pane_id)).ok()?;
    s.trim().parse().ok()
}

pub fn remove_pidfile(pane_id: &str) {
    let _ = std::fs::remove_file(pidfile(pane_id));
}

pub fn pid_alive(pid: i32) -> bool {
    // kill(pid, 0) probes existence without sending a signal.
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn list_monitored() -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(state_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".pid") {
            if let Ok(s) = std::fs::read_to_string(entry.path()) {
                if let Ok(pid) = s.trim().parse::<i32>() {
                    out.push((stem.to_string(), pid));
                }
            }
        }
    }
    out
}
```

Add `mod state;` to `src/main.rs`.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test state -- --test-threads=1`
Expected: PASS (env-var test runs single-threaded to avoid races).

- [ ] **Step 5: Commit**

```bash
git add src/state.rs src/main.rs
git commit -m "feat: pidfile state with collision-free pane-id sanitization

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: herdr socket client (RPC + Subscriber)

**Files:**
- Create: `src/herdr/mod.rs`, `src/herdr/rpc.rs`, `src/herdr/events.rs`
- Modify: `src/main.rs` (add `mod herdr;`)

**Interfaces:**
- Produces (in `herdr`):
  - `fn socket_path() -> Option<String>` (`$HERDR_SOCKET_PATH`)
  - `fn focused_pane_id() -> Option<String>` (`$HERDR_PANE_ID`, else
    `HERDR_PLUGIN_CONTEXT_JSON.focused_pane_id`)
  - trait `Rpc` with: `fn pane_status(&mut self, pane_id: &str) ->
    Result<Option<String>, HerdrError>` (Ok(None) = pane gone),
    `fn set_marker(&mut self, pane_id: &str) -> Result<(), HerdrError>`,
    `fn clear_marker(&mut self, pane_id: &str) -> Result<(), HerdrError>`,
    `fn notify(&mut self, title: &str, body: &str) -> Result<(), HerdrError>`
  - `struct SocketRpc` implementing `Rpc` over a `UnixStream`
  - trait `Events` with: `fn next_line(&mut self, timeout: Option<Duration>) ->
    NextLine` where `enum NextLine { Line, Timeout, Eof }`
  - `struct SocketEvents` implementing `Events`; constructor subscribes to
    `pane.agent_status_changed` + `pane.closed` for one pane.
  - `fn request_line(id, method, params) -> String` and `fn parse_pane_status(
    reply: &serde_json::Value) -> Option<String>` (pure helpers, unit-tested)

Wire formats (verbatim from herdr socket-api):
- Request: `{"id":"r1","method":"pane.get","params":{"pane_id":"w1:p1"}}\n`
- Success: `{"id":"r1","result":{"type":"pane_info","pane":{"pane_id":"w1:p1",
  "agent_status":"working",...}}}`
- Error reply: has an `"error"` object and no usable `result.pane`.
- Subscribe: `{"id":"s1","method":"events.subscribe","params":{"subscriptions":[
  {"type":"pane.agent_status_changed","pane_id":"w1:p1"},
  {"type":"pane.closed","pane_id":"w1:p1"}]}}`
- `report_metadata` set: `{"id":"r2","method":"pane.report_metadata","params":{
  "pane_id":"w1:p1","source":"espresso","custom_status":"󰅶"}}`
- `report_metadata` clear: same but `"clear_custom_status":true` and no
  `custom_status`.
- `notification.show`: `{"id":"r3","method":"notification.show","params":{
  "title":"...","body":"..."}}`

- [ ] **Step 1: Write failing tests for the pure helpers**

```rust
// src/herdr/rpc.rs  (tests at bottom)
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
```

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test herdr`
Expected: FAIL — helpers not defined.

- [ ] **Step 3: Implement `src/herdr/rpc.rs`**

```rust
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
    fn from(e: std::io::Error) -> Self { HerdrError::Io(e) }
}

pub fn request_line(id: &str, method: &str, params: Value) -> String {
    let mut s = serde_json::to_string(&json!({
        "id": id, "method": method, "params": params
    })).expect("serialize request");
    s.push('\n');
    s
}

pub fn parse_pane_status(reply: &Value) -> Option<String> {
    reply.get("result")?
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
        Ok(Self { reader, writer: stream, seq: 0 })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, HerdrError> {
        self.seq += 1;
        let id = format!("r{}", self.seq);
        self.writer.write_all(request_line(&id, method, params).as_bytes())?;
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
        self.call("pane.report_metadata", json!({
            "pane_id": pane_id, "source": METADATA_SOURCE, "custom_status": MARKER
        }))?;
        Ok(())
    }
    fn clear_marker(&mut self, pane_id: &str) -> Result<(), HerdrError> {
        self.call("pane.report_metadata", json!({
            "pane_id": pane_id, "source": METADATA_SOURCE, "clear_custom_status": true
        }))?;
        Ok(())
    }
    fn notify(&mut self, title: &str, body: &str) -> Result<(), HerdrError> {
        self.call("notification.show", json!({"title": title, "body": body}))?;
        Ok(())
    }
}
```

- [ ] **Step 4: Implement `src/herdr/events.rs`**

Partial data must survive a mid-line read timeout, or a status-change event
(including a terminal "went idle") can be silently lost while the lease keeps
renewing — leaving espresso on forever. So bytes are buffered in a struct field
across calls, and framing goes through a pure, testable `take_line` helper.

```rust
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
                Ok(0) => break,                    // server closed before ack
                Ok(n) => me.buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,                   // timeout/other: proceed
            }
        }
        Ok(me)
    }
}

impl Events for SocketEvents {
    fn next_line(&mut self, timeout: Option<Duration>) -> NextLine {
        // Return an already-buffered complete line first (batched events).
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
                    // Partial line: bytes retained in self.buf; keep reading.
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
        assert_eq!(buf, b"{\"partial\":"); // not lost
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
```

- [ ] **Step 5: Implement `src/herdr/mod.rs`**

```rust
pub mod events;
pub mod rpc;

pub use events::{Events, NextLine, SocketEvents};
pub use rpc::{HerdrError, Rpc, SocketRpc};

pub fn socket_path() -> Option<String> {
    std::env::var("HERDR_SOCKET_PATH").ok()
}

pub fn focused_pane_id() -> Option<String> {
    if let Ok(id) = std::env::var("HERDR_PANE_ID") {
        if !id.is_empty() {
            return Some(id);
        }
    }
    let ctx = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?;
    let v: serde_json::Value = serde_json::from_str(&ctx).ok()?;
    v.get("focused_pane_id")?.as_str().map(str::to_string)
}
```

Add `mod herdr;` to `src/main.rs`.

- [ ] **Step 6: Run tests, verify pass; build**

Run: `cargo test herdr && cargo build`
Expected: PASS (3 tests); builds.

- [ ] **Step 7: Commit**

```bash
git add src/herdr/ src/main.rs
git commit -m "feat: herdr socket client (RPC + event subscriber)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: espresso control (leased child + rotation + daemon probe)

**Files:**
- Create: `src/espresso.rs`
- Modify: `src/main.rs` (add `mod espresso;`)

**Interfaces:**
- Produces:
  - trait `EspressoCtl` with: `fn rotate(&mut self) -> Result<(), EspressoError>`
    (spawn new lease, kill old), `fn kill(&mut self)`, `fn is_up(&self) -> bool`
  - `struct Lease` implementing `EspressoCtl` (spawns `espresso -t 90`)
  - `enum EspressoError { NotFound, Spawn(std::io::Error) }`
  - `fn daemon_installed() -> bool` (runs `espresso daemon status`, checks output)

Notes:
- `rotate`: `let child = Command::new("espresso").arg("-t").arg("90").spawn()`;
  if `Err` with `ErrorKind::NotFound` → `EspressoError::NotFound` (and do NOT
  kill the old child). On success, kill the previous child pid (SIGKILL) and
  store the new one.
- Confirm-alive: after spawn, `child.try_wait()`; if it already exited, treat as
  spawn failure and keep the old child.
- `kill`: SIGKILL the tracked pid if any, clear it.
- Lease duration comes from `consts::LEASE` (`90`), formatted as whole seconds.

- [ ] **Step 1: Write failing tests (daemon-probe parsing + kill of a real sleep)**

```rust
// src/espresso.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_stops_a_spawned_child() {
        // Use `sleep` as a stand-in child we fully control.
        let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id() as i32;
        assert!(crate::state::pid_alive(pid));
        kill_pid(pid);
        // give the kernel a moment
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = child.wait();
        assert!(!crate::state::pid_alive(pid));
    }

    #[test]
    fn lease_secs_is_whole_seconds() {
        assert_eq!(lease_secs(), "90");
    }
}
```

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test espresso`
Expected: FAIL — `kill_pid`, `lease_secs` not defined.

- [ ] **Step 3: Implement `src/espresso.rs`**

```rust
use std::process::{Child, Command};

use crate::consts::LEASE;

#[derive(Debug)]
pub enum EspressoError {
    NotFound,
    Spawn(std::io::Error),
}

pub fn lease_secs() -> String {
    LEASE.as_secs().to_string()
}

pub fn kill_pid(pid: i32) {
    unsafe { libc::kill(pid, libc::SIGKILL); }
}

pub trait EspressoCtl {
    fn rotate(&mut self) -> Result<(), EspressoError>;
    fn kill(&mut self);
    fn is_up(&self) -> bool;
}

#[derive(Default)]
pub struct Lease {
    current: Option<Child>,
}

impl EspressoCtl for Lease {
    fn rotate(&mut self) -> Result<(), EspressoError> {
        let mut child = match Command::new("espresso").arg("-t").arg(lease_secs()).spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(EspressoError::NotFound),
            Err(e) => return Err(EspressoError::Spawn(e)),
        };
        // Confirm it did not immediately exit.
        if let Ok(Some(_status)) = child.try_wait() {
            return Err(EspressoError::Spawn(std::io::Error::other("espresso exited immediately")));
        }
        // New child is up: kill the old one, then adopt the new.
        if let Some(old) = self.current.take() {
            kill_pid(old.id() as i32);
            let mut old = old;
            let _ = old.wait();
        }
        self.current = Some(child);
        Ok(())
    }

    fn kill(&mut self) {
        if let Some(child) = self.current.take() {
            kill_pid(child.id() as i32);
            let mut child = child;
            let _ = child.wait();
        }
    }

    fn is_up(&self) -> bool {
        self.current.is_some()
    }
}

pub fn daemon_installed() -> bool {
    // `espresso daemon status` exits 0 and reports installed state; if the
    // binary is missing or the helper is not installed, treat as not installed.
    match Command::new("espresso").args(["daemon", "status"]).output() {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
            out.status.success() && text.contains("installed") && !text.contains("not installed")
        }
        Err(_) => false,
    }
}
```

> Implementer note: verify the exact `espresso daemon status` output wording
> against the installed espresso (`espresso daemon status`) and adjust the
> `contains` check if needed; the behavior (warn-and-continue) does not depend
> on getting this perfectly right.

Add `mod espresso;` to `src/main.rs`.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test espresso`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/espresso.rs src/main.rs
git commit -m "feat: espresso leased-child control with rotation and daemon probe

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Watcher loop (wire Machine + traits) with a mock-driven test

**Files:**
- Create: `src/watcher.rs`
- Modify: `src/main.rs` (add `mod watcher;`, dispatch `Command::Watch`),
  `src/policy.rs` (add `Machine::note_rotate_failed` + a test)

**Interfaces:**
- Consumes: `policy::{Machine, Action, active}`, `herdr::{Rpc, Events,
  NextLine}`, `espresso::{EspressoCtl, EspressoError}`, `state`, `consts`.
- Produces:
  - `Machine::note_rotate_failed(&mut self, now: Instant)` — after a failed
    espresso spawn, shortens the next renew to `now + RENEW_RETRY` so the
    degraded path retries sooner than the normal cadence.
  - `fn run_loop<R: Rpc, E: Events, C: EspressoCtl>(pane_id: &str, rpc: &mut R,
    events: &mut E, esp: &mut C, now: impl FnMut() -> Instant, stop:
    &AtomicBool) -> ()` — the testable core.
  - `fn watch(pane_id: &str) -> i32` — the real entry: builds `SocketRpc`,
    `SocketEvents`, `Lease`, installs SIGTERM→`stop`, sets marker, runs
    `run_loop`, clears marker on exit, removes pidfile.

Loop contract (locked by the test):
- On each iteration: `deadline = machine.next_deadline()`; compute `timeout =
  deadline - now` (or `None`); `events.next_line(timeout)`:
  - `NextLine::Line` → `status = rpc.pane_status(pane_id)`:
    - `Ok(None)` (pane gone) → break (cleanup).
    - `Ok(Some(s))` → `apply(machine.on_status(active(&s), now), esp, rpc,
      &mut machine, now, ...)`.
    - `Err(_)` → treat as pane gone → break.
  - `NextLine::Timeout` → `apply(machine.on_timer(now), ...)`.
  - `NextLine::Eof` → break (cleanup).
  - between iterations, if `stop` is set → break (cleanup).
- `apply(actions)`: for `RotateLease` call `esp.rotate()` — `Ok` do nothing;
  `Err(NotFound)` notify once via `rpc.notify` and `machine.note_rotate_failed(
  now)`; `Err(Spawn)` `machine.note_rotate_failed(now)` (no notify). For
  `KillEspresso` call `esp.kill()`.
- cleanup (after loop): `esp.kill()`; `rpc.clear_marker(pane_id)` (best effort);
  `state::remove_pidfile(pane_id)`.

Add to `src/policy.rs` (in this task) the method and a test:

```rust
    // in impl Machine
    pub fn note_rotate_failed(&mut self, now: Instant) {
        self.next_renew = Some(now + crate::consts::RENEW_RETRY);
    }
```

```rust
    // in policy tests
    #[test]
    fn note_rotate_failed_shortens_next_renew() {
        use crate::consts::RENEW_RETRY;
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0);            // next_renew = t0 + RENEW_INTERVAL (60s)
        m.note_rotate_failed(t0);         // shorten to t0 + RENEW_RETRY (10s)
        assert_eq!(m.next_deadline(), Some(t0 + RENEW_RETRY));
    }
```

- [ ] **Step 1: Write a failing mock-driven test**

```rust
// tests/watcher_loop.rs
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use herdr_espresso::herdr::{Events, NextLine, Rpc, HerdrError};
use herdr_espresso::espresso::{EspressoCtl, EspressoError};
use herdr_espresso::watcher::run_loop;

// Scripted event source: yields a sequence of NextLine values, then Eof.
struct FakeEvents { script: Vec<NextLine>, i: usize }
impl Events for FakeEvents {
    fn next_line(&mut self, _t: Option<Duration>) -> NextLine {
        let n = self.script.get(self.i).map(clone_nl).unwrap_or(NextLine::Eof);
        self.i += 1; n
    }
}
fn clone_nl(n: &NextLine) -> NextLine { match n { NextLine::Line=>NextLine::Line, NextLine::Timeout=>NextLine::Timeout, NextLine::Eof=>NextLine::Eof } }

struct FakeRpc { statuses: Vec<Option<String>>, i: usize, marker_cleared: bool }
impl Rpc for FakeRpc {
    fn pane_status(&mut self, _p:&str)->Result<Option<String>,HerdrError>{
        let s = self.statuses.get(self.i).cloned().unwrap_or(None); self.i+=1; Ok(s)
    }
    fn set_marker(&mut self,_p:&str)->Result<(),HerdrError>{Ok(())}
    fn clear_marker(&mut self,_p:&str)->Result<(),HerdrError>{self.marker_cleared=true; Ok(())}
    fn notify(&mut self,_t:&str,_b:&str)->Result<(),HerdrError>{Ok(())}
}

#[derive(Default)]
struct FakeEsp { up: bool, rotates: u32, kills: u32 }
impl EspressoCtl for FakeEsp {
    fn rotate(&mut self)->Result<(),EspressoError>{ self.up=true; self.rotates+=1; Ok(()) }
    fn kill(&mut self){ self.up=false; self.kills+=1; }
    fn is_up(&self)->bool{ self.up }
}

#[test]
fn working_then_pane_closed_opens_then_cleans_up() {
    // Line#1: status=working -> rotate(open). Line#2: pane gone (None) -> break.
    let mut events = FakeEvents { script: vec![NextLine::Line, NextLine::Line], i: 0 };
    let mut rpc = FakeRpc { statuses: vec![Some("working".into()), None], i:0, marker_cleared:false };
    let mut esp = FakeEsp::default();
    let stop = AtomicBool::new(false);
    let t0 = Instant::now();
    run_loop("w1:p1", &mut rpc, &mut events, &mut esp, move || t0, &stop);
    assert_eq!(esp.rotates, 1);      // opened on working
    assert_eq!(esp.kills, 1);        // killed during cleanup
    assert!(rpc.marker_cleared);     // marker cleared on cleanup
}
```

- [ ] **Step 2: Add lib target so the integration test can import modules**

Create `src/lib.rs` re-exporting the modules, and point the binary at it.

```rust
// src/lib.rs  (toggle/status modules are added in Task 7, not here)
pub mod consts;
pub mod policy;
pub mod state;
pub mod herdr;
pub mod espresso;
pub mod watcher;
```

Update `Cargo.toml`:

```toml
[lib]
name = "herdr_espresso"
path = "src/lib.rs"
```

And change `src/main.rs` to `use herdr_espresso::{...}` instead of `mod`
declarations (keep only `mod cli;` local to the binary).

- [ ] **Step 3: Run test, verify fail**

Run: `cargo test --test watcher_loop`
Expected: FAIL — `run_loop` not defined.

- [ ] **Step 4: Implement `src/watcher.rs`**

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::consts::PLUGIN_ID;
use crate::espresso::{EspressoCtl, EspressoError};
use crate::herdr::{Events, NextLine, Rpc};
use crate::policy::{active, Action, Machine};
use crate::state;

// Upper bound on how long a single `next_line` blocks, so the loop rechecks
// `stop` (toggle-off / SIGTERM) promptly even when there is no timer deadline
// (e.g. a pane that is idle at toggle-on: no lease, no pending stop). Without
// this, a toggle-off while idle would leave the marker and pidfile until the
// next pane event. Waking early is harmless: `on_timer` returns no actions
// unless a deadline is actually due.
const POLL_CAP: Duration = Duration::from_secs(1);

pub fn run_loop<R: Rpc, E: Events, C: EspressoCtl>(
    pane_id: &str,
    rpc: &mut R,
    events: &mut E,
    esp: &mut C,
    mut now: impl FnMut() -> Instant,
    stop: &AtomicBool,
) {
    let mut machine = Machine::new();
    let mut warned_missing = false;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let timeout = match machine.next_deadline() {
            Some(d) => d.saturating_duration_since(now()).min(POLL_CAP),
            None => POLL_CAP,
        };
        match events.next_line(Some(timeout)) {
            NextLine::Line => match rpc.pane_status(pane_id) {
                Ok(Some(s)) => {
                    let n = now();
                    let acts = machine.on_status(active(&s), n);
                    apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
                }
                Ok(None) | Err(_) => break, // pane gone or unreachable
            },
            NextLine::Timeout => {
                let n = now();
                let acts = machine.on_timer(n);
                apply(&acts, esp, rpc, &mut machine, n, &mut warned_missing);
            }
            NextLine::Eof => break,
        }
    }

    // Cleanup (covers SIGTERM, pane close, and server gone).
    esp.kill();
    let _ = rpc.clear_marker(pane_id);
    state::remove_pidfile(pane_id);
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

pub fn watch(pane_id: &str) -> i32 {
    let Some(sock) = crate::herdr::socket_path() else {
        eprintln!("HERDR_SOCKET_PATH not set");
        return 1;
    };
    let mut rpc = match crate::herdr::SocketRpc::connect(&sock) {
        Ok(r) => r,
        Err(_) => return 1,
    };
    let mut events = match crate::herdr::SocketEvents::subscribe(&sock, pane_id) {
        Ok(e) => e,
        Err(_) => return 1,
    };
    let mut esp = crate::espresso::Lease::default();

    // Marker on; one-time helper warning.
    let _ = rpc.set_marker(pane_id);
    if !crate::espresso::daemon_installed() {
        let _ = rpc.notify(
            &format!("{PLUGIN_ID}: lid-closed not active"),
            "Run `espresso daemon install` for lid-closed keep-awake.",
        );
    }

    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone());

    run_loop(pane_id, &mut rpc, &mut events, &mut esp, Instant::now, &stop);
    0
}
```

Wire `Command::Watch { pane_id } => herdr_espresso::watcher::watch(&pane_id)` in
`src/main.rs`.

- [ ] **Step 5: Run tests, verify pass; build**

Run: `cargo test --test watcher_loop && cargo build`
Expected: PASS; builds.

- [ ] **Step 6: Commit**

```bash
git add src/watcher.rs src/lib.rs src/main.rs Cargo.toml tests/watcher_loop.rs
git commit -m "feat: per-pane watcher loop wiring policy, herdr, and espresso

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: toggle + status commands (detached spawn, reconcile)

**Files:**
- Create: `src/toggle.rs`, `src/status.rs`
- Modify: `src/main.rs` (dispatch `Toggle`/`Status`), `src/lib.rs`

**Interfaces:**
- Consumes: `state`, `herdr::focused_pane_id`, `consts`.
- Produces: `fn toggle() -> i32`, `fn status() -> i32`.

toggle contract:
1. `pane_id = herdr::focused_pane_id()`; if none → notify (best effort) + return 1.
2. `existing = state::read_pidfile(&pane_id)`:
   - `Some(pid)` and `state::pid_alive(pid)` → **off**: `libc::kill(pid,
     SIGTERM)`, `state::remove_pidfile(&pane_id)`, return 0. (Watcher clears
     marker + kills espresso on its way out.)
   - `Some(pid)` not alive → stale: `remove_pidfile`, fall through to on.
   - `None` → on.
3. **on**: spawn detached `herdr-espresso watch <pane_id>`:
   - `Command::new(current_exe).arg("watch").arg(&pane_id)`, redirect
     stdin/stdout/stderr to a log file under `state_dir()`, `pre_exec(||
     { libc::setsid(); Ok(()) })`, `.spawn()`.
   - after spawn, sleep ~100ms, confirm `pid_alive(child.id())`; if alive,
     `write_pidfile(&pane_id, child.id())`, return 0; else return 1.

- [ ] **Step 1: Implement `src/toggle.rs`**

```rust
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::{herdr, state};

pub fn toggle() -> i32 {
    let Some(pane_id) = herdr::focused_pane_id() else {
        eprintln!("no focused pane (HERDR_PANE_ID unset)");
        return 1;
    };

    if let Some(pid) = state::read_pidfile(&pane_id) {
        if state::pid_alive(pid) {
            unsafe { libc::kill(pid, libc::SIGTERM); }
            state::remove_pidfile(&pane_id);
            println!("espresso: monitoring off for {pane_id}");
            return 0;
        }
        state::remove_pidfile(&pane_id); // stale
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return 1,
    };
    let _ = std::fs::create_dir_all(state::state_dir());
    let log = state::state_dir().join(format!("{}.log", state::sanitize_pane_id(&pane_id)));

    // Default both streams to null: a detached watcher must NEVER inherit the
    // caller's tty (Command's default for an unset stream is inherit()).
    // Upgrade to the log file only when it is fully available.
    let mut cmd = Command::new(exe);
    cmd.arg("watch").arg(&pane_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        match f.try_clone() {
            Ok(f2) => {
                cmd.stdout(Stdio::from(f));
                cmd.stderr(Stdio::from(f2));
            }
            Err(_) => {
                cmd.stdout(Stdio::from(f)); // stderr stays null
            }
        }
    }
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return 1,
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let pid = child.id() as i32;
    if !state::pid_alive(pid) {
        return 1;
    }
    if state::write_pidfile(&pane_id, pid).is_err() {
        return 1;
    }
    println!("espresso: monitoring on for {pane_id}");
    0
}
```

- [ ] **Step 2: Implement `src/status.rs`**

```rust
use crate::state;

pub fn status() -> i32 {
    let mut any = false;
    for (sanitized, pid) in state::list_monitored() {
        if state::pid_alive(pid) {
            println!("monitoring: {sanitized} (watcher pid {pid})");
            any = true;
        } else {
            state::remove_pidfile_sanitized(&sanitized); // reconcile stale
        }
    }
    if !any {
        println!("no panes monitored");
    }
    0
}
```

Add to `src/state.rs`:

```rust
pub fn remove_pidfile_sanitized(sanitized: &str) {
    let _ = std::fs::remove_file(state_dir().join(format!("{sanitized}.pid")));
}
```

- [ ] **Step 3: Wire dispatch in `src/main.rs`**

```rust
Command::Toggle => herdr_espresso::toggle::toggle(),
Command::Watch { pane_id } => herdr_espresso::watcher::watch(&pane_id),
Command::Status => herdr_espresso::status::status(),
```

Add `pub mod toggle;` and `pub mod status;` to `src/lib.rs`.

- [ ] **Step 4: Build + smoke test dispatch**

Run: `cargo build && HERDR_PANE_ID= ./target/debug/herdr-espresso toggle; echo $?`
Expected: prints "no focused pane" and exit 1 (no socket needed for this path).
Run: `./target/debug/herdr-espresso status`
Expected: "no panes monitored", exit 0.

- [ ] **Step 5: Commit**

```bash
git add src/toggle.rs src/status.rs src/state.rs src/main.rs src/lib.rs
git commit -m "feat: toggle (detached setsid spawn + reconcile) and status commands

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Plugin manifest, wrappers, docs

**Files:**
- Create: `herdr-plugin.toml`, `bin/toggle`, `bin/status`, `README.md`,
  `docs/manual-test.md`

**Interfaces:**
- Produces: an installable/linkable herdr plugin exposing `espresso.toggle` and
  `espresso.status`.

- [ ] **Step 1: Create `herdr-plugin.toml`**

```toml
id = "espresso"
name = "Espresso Guard"
version = "0.1.0"
min_herdr_version = "0.7.0"
description = "Keep macOS awake (incl. lid-closed) while a focused pane's agent is working."
platforms = ["macos"]

[[build]]
command = ["cargo", "build", "--release"]

[[actions]]
id = "toggle"
title = "Espresso: toggle monitor on focused pane"
contexts = ["pane", "workspace", "global"]
command = ["bash", "bin/toggle"]

[[actions]]
id = "status"
title = "Espresso: list monitored panes"
contexts = ["global"]
command = ["bash", "bin/status"]
```

- [ ] **Step 2: Create `bin/toggle` and `bin/status`** (both `chmod +x`)

```bash
#!/usr/bin/env bash
# bin/toggle
exec "${HERDR_PLUGIN_ROOT:?}/target/release/herdr-espresso" toggle "$@"
```

```bash
#!/usr/bin/env bash
# bin/status
exec "${HERDR_PLUGIN_ROOT:?}/target/release/herdr-espresso" status "$@"
```

- [ ] **Step 3: Create `README.md`** covering: what it does; requirements
  (macOS, herdr ≥0.7.0, `espresso` on PATH, optional `espresso daemon install`
  for lid-closed); install (`herdr plugin install <owner>/herdr-espresso` or
  `herdr plugin link "$PWD"`); the keybinding snippet below; behavior (working/
  blocked → awake, idle/done → sleep after grace, per-pane independent, pane
  close cleanup, self-healing lease); the GitHub topic `herdr-plugin` note.

Keybinding snippet for the README (user adds to their herdr config):

```toml
[[keys.command]]
key = "prefix+shift+option+e"
type = "plugin_action"
command = "espresso.toggle"
description = "espresso: toggle monitor on focused pane"
```

- [ ] **Step 4: Create `docs/manual-test.md`** — a checklist requiring a real
  herdr + espresso:
  1. `herdr plugin link "$PWD"`; add the keybinding; reload config.
  2. In a pane running an agent, press `prefix+shift+option+e`; confirm the `󰅶`
     marker appears in the sidebar and a watcher pid exists (`herdr-espresso
     status`).
  3. Drive the agent to `working`; `pmset -g assertions | grep espresso` (or
     `espresso`-created assertion) shows an assertion; let it go idle; after ~5s
     the assertion clears.
  4. Toggle again; marker disappears, no watcher, no espresso process.
  5. With two panes both monitored and working, both hold; close one pane → its
     espresso stops, the other keeps holding.
  6. Detach (`prefix+q`) while working → reattach; monitoring still active.
  7. Kill a watcher with `kill -9 <pid>` → its espresso self-expires within 90s.
  8. Without `espresso daemon install`, toggling on shows the one-time
     lid-closed warning notification.

- [ ] **Step 5: Verify release build + link locally**

Run: `cargo build --release && test -x target/release/herdr-espresso && echo ok`
Expected: `ok`.

- [ ] **Step 6: Commit**

```bash
chmod +x bin/toggle bin/status
git add herdr-plugin.toml bin/ README.md docs/manual-test.md
git commit -m "feat: plugin manifest, action wrappers, README and manual-test docs

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-review notes (addressed)

- **Spec coverage:** keybinding→toggle action (Task 7 + 8); working/blocked→open
  (Task 2 `active`); idle/done→close after grace (Task 2 timer); per-pane
  independence (per-pane watcher, Task 6/7); pane-close connected cleanup (Task 6
  `Ok(None)`→cleanup); `󰅶` marker set/clear (Task 4 + 6); helper warn-and-continue
  (Task 5/6); short-lease renewal + gap-free + self-healing (Task 2 machine +
  Task 5 `Lease::rotate`); detach safety (Task 7 setsid); reconcile stale
  (Task 7 toggle + Task 7 status).
- **Type consistency:** `Rpc`/`Events`/`EspressoCtl` trait method names are used
  identically in Task 6's `run_loop`, the Task 6 mock test, and the real impls in
  Tasks 4–5. `Action::{RotateLease,KillEspresso}` match between Task 2 and Task 6.
- **Placeholders:** none; every code step is complete. The single implementer
  note (Task 5 `daemon status` wording) is a verification-against-reality step,
  not a missing implementation — the warn-and-continue behavior is defined.

---

## Appendix: post-review fixes (applied during execution)

Fixes found by task/final reviews that diverge from the task code blocks above.
The committed source is authoritative; this records the deltas.

- **policy `next_deadline`** suppresses `next_renew` while inactive (stale
  deadline would fire nothing and stall). [Task 2 review]
- **`SocketEvents`** buffers bytes in a struct field via a pure `take_line`
  helper so a mid-line read timeout can't drop a partial event line. [Task 4]
- **watcher `POLL_CAP`** (1s) caps `next_line` blocking so toggle-off/SIGTERM is
  noticed promptly even with no timer deadline. [Task 6]
- **`RENEW_RETRY`** wired via `Machine::note_rotate_failed`. [Task 6 plan]
- **toggle stdio** defaults to `Stdio::null()`; upgrades to the log file only
  when fully available (never inherits the caller tty). [Task 7]
- **watcher seed:** `run_loop` reads `pane.get` once before the loop so an
  already-working pane acquires a lease without waiting for an event; loop is
  `if enter { loop { … } }`. [FINAL — Critical]
- **atomic pidfile:** `state::try_reserve_pidfile` (O_EXCL, writes the owner's
  own pid as sentinel) prevents a concurrent toggle-on from spawning a second,
  orphaned watcher. [FINAL — Important]
- **SIGTERM registered first** in `watch()`, before marker/daemon probe, so a
  toggle-off during startup is caught and cleanup runs. [FINAL — Important]
