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
- To hold indefinitely we spawn `espresso -t <large>` (e.g. 100 days) and kill
  it to close. If the child exits while the pane is still working, the watcher
  respawns it.

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
   - Otherwise → **toggle on**: spawn a detached `herdr-espresso watch
     <pane_id>`, write the pidfile.
3. Emit a herdr notification reflecting the new state.

### watch flow (core)
1. Connect to `$HERDR_SOCKET_PATH`.
2. Set the marker: `pane.report_metadata { pane_id, source:"espresso",
   custom_status:"󰅶" }` (no ttl).
3. `events.subscribe` for `pane.agent_status_changed` and `pane.closed` on this
   pane; then `pane.get` once to seed the initial status.
4. Event loop (blocking, read line by line):
   - status ∈ {`working`,`blocked`} → ensure the espresso child is running.
   - status ∈ {idle/done/other} → after the stop-grace elapses, kill the
     espresso child (keep watching). A return to working/blocked before the
     grace expires cancels the pending close.
   - `pane.closed` for this pane → kill espresso, clear the marker, remove the
     pidfile, exit.
   - socket EOF (herdr gone) → kill espresso, exit (marker is moot).
5. `SIGTERM` handler (toggle-off) → kill espresso, clear the marker via
   `pane.report_metadata { pane_id, source:"espresso", clear_custom_status:true
   }`, remove the pidfile, exit 0.
6. espresso child = `espresso -t <large>`. Respawn if it dies while the pane is
   still working/blocked.
7. On startup, run `espresso daemon status`; if the helper is not installed,
   `notification.show` a one-time warning, then continue.

### Debounce
- working↔blocked transitions never toggle espresso (both keep it open).
- A single stop-grace timer (default ~5s, no env/config surface in v1) delays
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
- `src/espresso.rs` — spawn/kill the espresso child, `espresso daemon status`
  probe.
- `src/policy.rs` — pure function `desired_espresso(status) -> bool` (and the
  debounce decision), unit-testable without a socket.

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

## Testing

- Unit (TDD where practical):
  - `policy::desired_espresso` for each status.
  - debounce decision (pending-close set/cancel across a simulated clock).
  - `state` pane-id sanitization is readable and collision-free.
  - JSON-RPC request/response and event-line encode/decode.
- Event-loop test against an in-memory / mock stream feeding scripted event
  lines, asserting espresso open/close calls and metadata calls.
- Manual verification checklist (`docs/manual-test.md`) requiring a real herdr:
  toggle on/off, working→open, idle→close after grace, pane close cleanup,
  two panes independent, marker appears/disappears, helper-missing warning.

## v1 non-goals

- No persistence across herdr restart (herdr restart → watcher socket EOF →
  self-cleanup; user re-toggles).
- No per-pane configurable policies.
- No autostart / event-hook bootstrap; monitoring is entirely toggle-driven.
- No tunable env/config surface for grace periods (fixed defaults in v1).
