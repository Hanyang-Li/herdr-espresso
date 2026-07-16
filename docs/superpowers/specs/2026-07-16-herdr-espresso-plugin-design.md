# herdr-espresso plugin — design

Date: 2026-07-16
Status: approved-design (pending spec review)

## Purpose

A herdr plugin that keeps macOS awake (including lid-closed, when espresso's
root helper is installed) while a monitored pane's coding agent is actively
working. Monitoring is toggled per pane by a keybinding; multiple panes can be
monitored independently at the same time.

espresso is a separate, already-installed CLI (`espresso`, macOS-only). This
plugin drives it; it does not vendor or link espresso's source.

## Requirements (from the user)

1. Keybinding `prefix + shift + option + e` toggles monitoring on the currently
   focused pane's agent. While monitoring is on: when the agent is working,
   espresso is opened automatically; when it is not working, espresso is closed
   automatically — but monitoring continues.
2. Multiple panes can be monitored at the same time, each running the same logic
   independently. When a pane closes, if it has an espresso open, that espresso
   is closed too.
3. When monitoring is enabled on a pane's agent, add the `custom-status`
   metadata marker `󰅶` on the sidebar. Disabling monitoring removes it.

## Decisions

- **"working" semantics:** espresso is kept open when the agent status is
  `working` **or** `blocked`. Only `idle`/`done` (and other non-active states)
  close espresso. Rationale: the user wants the Mac to stay awake even while the
  agent waits for input.
- **"multiple panes" meaning:** each monitored pane runs the same
  working→open / not-working→close logic, fully independent of the others.
  No per-pane configurable policies in v1.
- **espresso lid-closed helper (needs one-time `sudo espresso daemon install`):**
  if the helper is not installed, monitoring still turns on (idle-sleep
  protection only; lid-closed will still sleep) and the plugin warns once via a
  herdr notification suggesting `espresso daemon install`.
- **Plugin id:** `espresso`. Action invoked as `espresso.toggle`.
- **Debounce:** include a stop-grace before closing espresso (default ~5s) to
  avoid flapping when status briefly drops to idle/done. Opening is immediate.
- **espresso lifetime = short lease + renewal, not one long process.** The
  watcher holds a short-lived `espresso -t <LEASE>` and renews it by process
  rotation before it expires. This bounds any leak to the lease window if the
  watcher dies unexpectedly (self-healing) and keeps the model detach-safe.
  espresso has no "extend timer" verb — each process's `-t` is fixed at launch —
  so renewal is necessarily spawning a fresh process and killing the old one,
  not extending in place. Constants: `LEASE = 90s`, `RENEW_INTERVAL = 60s`,
  `STOP_GRACE = 5s` (fixed in v1, no config surface).

## Key facts established about the platforms

### herdr
- Actions receive injected env: `HERDR_SOCKET_PATH`, `HERDR_BIN_PATH`,
  `HERDR_ENV=1`, `HERDR_PLUGIN_ID`, `HERDR_PLUGIN_ROOT`,
  `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_STATE_DIR`,
  `HERDR_PLUGIN_CONTEXT_JSON`, and (when available) `HERDR_WORKSPACE_ID`,
  `HERDR_TAB_ID`, `HERDR_PANE_ID`, plus `HERDR_PLUGIN_ACTION_ID`.
- Socket API is line-delimited JSON-RPC over the unix socket at
  `HERDR_SOCKET_PATH`. Relevant methods:
  - `events.subscribe` — subscription objects like
    `{ "type": "pane.agent_status_changed", "pane_id": "w1:p1" }`. First response
    acknowledges; later lines are pushed events. Pane event types include
    `pane.created`, `pane.closed`, `pane.focused`, `pane.agent_status_changed`.
  - `pane.get` / `pane.list` / `agent.get` — read current agent status.
  - `pane.report_metadata` — `{ pane_id, source, custom_status, ttl_ms?,
    clear_custom_status? }`. `custom_status` is capped at 32 chars (`󰅶` fits).
    Omit `ttl_ms` so the marker persists until cleared or the pane closes.
    herdr removes a source's metadata automatically when the pane closes.
    `source` must be ASCII `[A-Za-z0-9:._-]`, ≤80 chars — use `espresso`.
  - `notification.show` — `{ title, body?, sound? }` for the helper warning.
- Keybindings live in the **user's herdr config**, not the plugin manifest.
  Modifier tokens: `ctrl`/`control`, `shift`, `alt`/`option`/`meta` (→ Alt),
  `cmd`/`command`/`super`. The requested combo is `prefix+shift+option+e`.

### espresso
- Not an on/off daemon toggle. Keep-awake = an espresso process being alive
  (RAII). Modes: `espresso -t <secs|clock>` (countdown / until a clock time),
  `espresso -- <command>` (hold while command runs), and bare `espresso` just
  prints help and exits 1.
- On exit — normal Drop **or** killed by signal — the IOKit assertion is
  released (kernel releases process-scoped assertions on death; Drop also
  releases deterministically) and the lid-closed daemon decrements its refcount
  when espresso's socket fd closes. So killing the espresso child cleanly
  releases everything; espresso needs no special signal handling from us.
- Lid-closed keep-awake requires the root launchd helper (`espresso daemon
  install`, one-time sudo). Without it, only idle-sleep is prevented.
- Keep-awake is continuous as long as **at least one** espresso process is alive
  at any instant, because each process holds its own assertion and its own
  daemon refcount. This is what makes lease renewal by process rotation gap-free
  (see "Lease and renewal").

## Architecture — per-pane watcher subprocess

Chosen over a single resident session watcher because it maps 1:1 to
"independent panes", makes pane-close and crash cleanup inherent RAII, and needs
no central daemon, autostart, or action↔watcher IPC. The extra socket
connections (one per monitored pane, typically a handful) are negligible.

Single binary `herdr-espresso` with clap subcommands:

- `toggle` — invoked by the keybinding action. Reads `$HERDR_PANE_ID`, toggles
  monitoring for that pane.
- `watch <pane_id>` — the detached, per-pane watcher (internal/hidden command).
- `status` — list currently monitored panes (also exposed as an action).

### toggle flow
1. `pane_id = $HERDR_PANE_ID`; fall back to
   `HERDR_PLUGIN_CONTEXT_JSON.focused_pane_id`. If neither, `notification.show`
   an error and exit non-zero.
2. Look up `$HERDR_PLUGIN_STATE_DIR/<sanitized pane_id>.json` (watcher pid).
   - Exists and alive → **toggle off**: `SIGTERM` the watcher, remove the
     pidfile. The watcher's shutdown kills espresso and clears the marker.
   - Exists but **not alive (stale)** → reconcile: treat as off, remove the
     stale pidfile, then proceed as toggle-on.
   - Otherwise → **toggle on**: spawn the watcher fully detached (`setsid`, new
     session, stdio redirected to a log file under `HERDR_PLUGIN_STATE_DIR`), so
     client detach / terminal close never `SIGHUP`s it. Write the pidfile only
     after confirming the process is still alive a moment later.
3. Emit a herdr notification reflecting the new state.

### watch flow (core)
1. Connect to `$HERDR_SOCKET_PATH`.
2. Set the marker: `pane.report_metadata { pane_id, source:"espresso",
   custom_status:"󰅶" }` (no ttl).
3. `events.subscribe` for `pane.agent_status_changed` and `pane.closed` on this
   pane; then `pane.get` once to seed the initial status.
4. Event loop — a single-threaded loop that unifies events and timers. Each
   iteration computes the nearest pending deadline (next renew tick, or a
   pending stop-grace expiry) and reads the socket with that timeout:
   - Read returns an event:
     - `pane.agent_status_changed`, status ∈ {`working`,`blocked`} → become
       "active": cancel any pending stop-grace; if no espresso is running, run a
       renew tick immediately to open one and start the renew cadence.
     - status ∈ {idle/done/other} → become "inactive": arm a stop-grace timer
       (`STOP_GRACE`). Returning to active before it fires cancels it.
     - `pane.closed` for this pane → kill espresso, clear the marker, remove the
       pidfile, exit.
   - Read times out (a timer is due):
     - renew tick due (while active) → rotate the lease (below).
     - stop-grace due → kill espresso, stop the renew cadence (keep watching).
   - Socket EOF (server gone, e.g. `herdr server stop`) → kill espresso, exit
     (marker is moot).
5. `SIGTERM` handler (toggle-off) → kill espresso, clear the marker via
   `pane.report_metadata { pane_id, source:"espresso", clear_custom_status:true
   }`, remove the pidfile, exit 0.
6. On startup, run `espresso daemon status`; if the helper is not installed,
   `notification.show` a one-time warning, then continue.

### Lease and renewal
The watcher tracks exactly one "current" espresso PID and renews by process
rotation (espresso has no extend-timer verb):

- **Renew tick** (fires every `RENEW_INTERVAL` while active):
  1. Spawn a fresh `espresso -t <LEASE>` (detached) → `PID_new`.
  2. Confirm `PID_new` did not immediately exit.
  3. Success → `SIGKILL PID_old` (if any); `current = PID_new`; next tick at
     `now + RENEW_INTERVAL`.
  4. Spawn failed (espresso missing / transient) → keep `PID_old` (it protects
     until its own expiry); `notification.show` once if espresso is missing;
     retry in ~10s.
- **Open** (inactive→active) → run a renew tick immediately, then keep the
  cadence.
- **Close** (stop-grace fired / `pane.closed` / `SIGTERM`) → `SIGKILL current`,
  cancel the cadence.
- **Gap-free:** the new process is spawned before the old is killed, and the old
  still has `LEASE - RENEW_INTERVAL` (≥30s) remaining, so at least one espresso
  is always alive and the daemon refcount never reaches 0.
- **Self-healing:** if the watcher dies for any reason, `current` self-expires
  within ≤`LEASE` (90s) and the Mac is freed automatically.
- **Tidy:** spawn-then-kill keeps the steady state at ~1 process and refcount 1;
  close kills the single tracked PID with nothing left to leak.

### Debounce
- working↔blocked transitions never toggle espresso (both keep it open).
- A single stop-grace timer (`STOP_GRACE`, no env/config surface in v1) delays
  closing when status drops to a non-active state; reactivation cancels it.
- Opening is immediate.

### Multiple panes / connected close
Each monitored pane owns its own watcher process, espresso child, and pidfile —
fully independent (requirement 2). The espresso daemon's refcount equals the
number of monitored panes currently working/blocked. Pane close is handled by
each watcher's `pane.closed` subscription (requirement 2's connected-close).

## Components

- `src/main.rs`, `src/cli.rs` — clap CLI dispatch (`toggle` | `watch` |
  `status`).
- `src/herdr/mod.rs` — herdr socket client: connect, one-shot RPC, subscribe,
  read pushed events, `report_metadata`, `notification.show`. Line-delimited
  JSON over `std::os::unix::net::UnixStream`.
- `src/toggle.rs` — toggle on/off logic.
- `src/watcher.rs` — per-pane event loop and lifecycle.
- `src/state.rs` — pidfile read/write and pane-id → filename sanitization
  (`w1:p1` → `w1_p1`, non-`[A-Za-z0-9._-]` bytes percent-escaped to avoid
  collisions).
- `src/espresso.rs` — spawn a leased `espresso -t <LEASE>` child, track the
  current PID, rotate (spawn-then-kill) on renew, kill on close, and probe
  `espresso daemon status`.
- `src/policy.rs` — pure, socket-free, unit-testable decisions:
  `active(status) -> bool` (working/blocked → true) and the timer/debounce state
  machine (when to open, arm/cancel stop-grace, schedule the next renew).

## Project layout

```
herdr-espresso/
  Cargo.toml            # deps: serde, serde_json, libc, signal-hook
  herdr-plugin.toml
  README.md
  LICENSE
  bin/toggle            # thin wrapper: exec "$HERDR_PLUGIN_ROOT/target/release/herdr-espresso" toggle
  bin/status
  src/ ...              # as above
  tests/
  docs/superpowers/specs/2026-07-16-herdr-espresso-plugin-design.md
```

### herdr-plugin.toml (shape)

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

Action commands exec the release binary via `$HERDR_PLUGIN_ROOT` because the
action's working directory is not guaranteed to be the plugin root.

### Keybinding (documented for the user; not shipped in the manifest)

```toml
[[keys.command]]
key = "prefix+shift+option+e"
type = "plugin_action"
command = "espresso.toggle"
description = "espresso: toggle monitor on focused pane"
```

## Error handling

- Missing `HERDR_PANE_ID` and no focused pane in context → notify + non-zero
  exit; no watcher spawned.
- Socket connect failure in `watch` → exit non-zero. After spawning the
  watcher, toggle-on writes the pidfile only after confirming the process is
  still alive a short moment later (no separate handshake protocol).
- Stale pidfile (pid not alive) → treated as "off"; toggle-on proceeds and
  overwrites it.
- espresso binary missing → `notification.show` once; monitoring stays on and
  no-ops on open until `espresso` is on PATH again, so it recovers without a
  re-toggle.
- herdr socket EOF mid-run → watcher cleans up and exits.

## Lifecycle & detach safety

herdr is client/server: `prefix+q` detaches only the client; the server, panes,
agents, and the socket at `HERDR_SOCKET_PATH` keep running. `herdr server stop`
tears the server down.

- **Client detach / reattach** → the watcher's socket stays connected (no EOF);
  it keeps managing espresso normally. This is intended: a background agent that
  keeps working after you detach should keep the Mac awake. Because the watcher
  is `setsid`-detached, the client's terminal closing never signals it.
- **`herdr server stop` / server crash / reboot** → socket EOF → the (surviving,
  detached) watcher kills espresso and exits.
- **Watcher killed hard (SIGKILL / OOM)** → its current espresso lease
  self-expires within ≤`LEASE` (90s); no indefinite leak.
- **Stale state** → `toggle`/`status` reconcile dead-pid pidfiles.

## Testing

- Unit (TDD where practical):
  - `policy::active(status)` for each status (working/blocked → true).
  - the timer/debounce state machine over a simulated clock: open on activate,
    arm/cancel stop-grace, schedule renew ticks, close when grace fires.
  - `state` pane-id sanitization is readable and collision-free.
  - JSON-RPC request/response and event-line encode/decode.
- Event-loop test against an in-memory / mock stream feeding scripted event
  lines plus a controllable clock, asserting the espresso open/rotate/close
  calls and the metadata set/clear calls.
- Manual verification checklist (`docs/manual-test.md`) requiring a real herdr:
  toggle on/off, working→open, idle→close after grace, pane close cleanup,
  two panes independent, marker appears/disappears, helper-missing warning.

## v1 non-goals

- No persistence across herdr restart (herdr restart → watcher socket EOF →
  self-cleanup; user re-toggles).
- No per-pane configurable policies.
- No autostart / event-hook bootstrap; monitoring is entirely toggle-driven.
- No tunable env/config surface for the timing constants (`LEASE`,
  `RENEW_INTERVAL`, `STOP_GRACE` are fixed in v1).
