# herdr-espresso global monitor — design

Date: 2026-10-06
Status: approved-design
Supersedes: the per-pane model in `2026-07-16-herdr-espresso-plugin-design.md`

## Purpose

Replace per-pane monitoring with one herdr-wide switch. While the switch is
on, the Mac is kept awake whenever **any** agent pane in herdr is active, and
allowed to sleep once **all** agents have stopped. The plugin's state is shown
in one public place — herdr's tab bar — instead of per-pane sidebar markers.

## Requirements (from the user)

1. One global toggle (no per-pane mode). On: follow every agent; off: never
   hold espresso. The switch survives herdr restarts.
2. espresso is held while any agent is `working` or `blocked`; released (after
   the existing 5 s stop-grace) when none is.
3. A global status indicator in the tab bar, Nerd Font icon + word, no emoji,
   no agent count:
   - on, some agent active: `󰅶 awake` (md-coffee, U+F0176)
   - on, all idle: `󰛊 idle` (md-coffee_outline, U+F06CA)
   - off: nothing (the tab bar entry hides itself)

## herdr facts this design relies on (verified against v0.9.3)

- Manifest `[[startup]]`: herdr runs each enabled plugin's startup command once
  after a session is restored and the API socket is ready, and again after a
  live handoff to a new server. Not on client attach, config reload, or
  plugin link/enable. It is a one-shot command, not a supervised daemon.
  Startup and action commands get `HERDR_PLUGIN_STATE_DIR`, `HERDR_SOCKET_PATH`.
- `events.subscribe` — `pane.agent_status_changed` requires a `pane_id`, but
  `pane.updated`, `pane.created`, `pane.closed`, `pane.exited` and
  `pane.agent_detected` accept no filter and cover every pane. New
  subscriptions do **not** replay history: subscribe first, then snapshot. A
  subscriber that falls behind gets an `events_lost` error line.
- `pane.list` with `{}` returns `{"result":{"panes":[PaneInfo...]}}`; each
  `PaneInfo` has `agent` (absent for shells) and `agent_status`
  (`idle|working|blocked|done|unknown`).
- `ui.tab_bar_right` command entries (`{ type = "command", command, interval_seconds, timeout_seconds }`)
  run via `/bin/sh -lc` on the server, show the last line of successful
  output, and clear the value on failure or empty output. They do **not**
  receive plugin env vars.
- The plugin state dir is `$XDG_STATE_HOME/herdr/plugins/espresso` or, without
  `XDG_STATE_HOME`, `~/.local/state/herdr/plugins/espresso`.

## Design

### Manifest

```toml
id = "espresso"
version = "0.2.0"
min_herdr_version = "0.9.0"

[[startup]]
command = ["bash", "bin/startup"]

[[actions]]
id = "toggle"
title = "Espresso: toggle global keep-awake"
contexts = ["global", "workspace", "pane"]
command = ["bash", "bin/toggle"]
```

The `status` action and `bin/status` are removed.

### Files in the state dir

| File | Meaning |
|---|---|
| `enabled` | Persistent switch intent; read by startup. |
| `watcher.pid` | The single global watcher's pid. |
| `statusline` | The exact text the tab bar shows; exists only while a watcher runs. |
| `watcher.log` | Watcher stdout/stderr, truncated on each watcher start. |

### Subcommands

- `toggle` — decides by **watcher liveness**, not by `enabled`:
  - watcher alive → off: remove `enabled`, SIGTERM the watcher, toast
    `espresso off`.
  - watcher not alive → on: write `enabled`, atomically reserve `watcher.pid`,
    spawn the detached watcher (setsid, stdio to `watcher.log`), toast
    `espresso on`; warn once if the lid-closed helper is not installed.
  - A crashed watcher shows an empty tab bar entry, i.e. "off"; one press
    turns it on again.
- `startup` — stop any live watcher (SIGTERM, wait for exit, bounded), then:
  if `enabled` exists, spawn a fresh watcher; otherwise remove leftover
  `statusline` and `watcher.pid`. Replacing rather than skipping avoids the
  race where an outgoing watcher (live handoff) still looks alive, startup
  skips, then it exits and leaves no watcher.
- `watch` (hidden) — the watcher.

### Watcher loop

1. Subscribe (unfiltered pane lifecycle events above), then snapshot with
   `pane.list`, compute `any_active`, feed `Machine::on_status`.
2. Block in `PollWaiter`. Every wake that reports events → one `pane.list`
   (the drain already coalesces a burst into one wake) → `on_status`.
   Timer wakes → `on_timer` (renew / stop-grace), unchanged.
3. After each step, derive the display state — `awake` when the machine holds
   espresso, else `idle` — and rewrite `statusline` only when it changed.

`any_active` = some pane has a non-empty `agent` and `agent_status` is
`working` or `blocked`.

### Exit paths and cleanup

- SIGTERM (toggle-off or startup replacement) → exit.
- Event stream EOF → try once to reconnect and resubscribe; on success
  re-snapshot and continue; if the socket cannot be reached, the server is
  gone → exit (keeping `enabled` so startup restores it).
- Every exit runs the same cleanup: kill espresso, remove `statusline`,
  remove `watcher.pid`.

### Error handling

- `events_lost` and any other pushed bytes just trigger a re-snapshot.
- `pane.list` transport error (socket unreachable) → treat as server gone →
  exit. Error reply → log, keep the previous state.
- espresso missing → toast once, retry every 10 s (existing). Spawn failure →
  retry every 10 s (existing). A crashed watcher's lease expires within 90 s.
- `statusline` is written atomically (temp file + rename); write failures are
  logged only.

### Migration from 0.1.x

`startup` and `toggle` SIGTERM any legacy per-pane watchers (any `*.pid` in
the state dir other than `watcher.pid`) and remove their pidfiles; the old
watchers clear their own `$espresso` pane tokens on exit. README tells users
they can drop `$espresso` from `[ui.sidebar.agents] rows`.

### Tab bar config (user side)

```toml
[ui]
tab_bar_right = [
  { type = "command", command = "cat ~/.local/state/herdr/plugins/espresso/statusline", interval_seconds = 2, timeout_seconds = 1 },
]
```

`cat` on a fixed path is used because tab bar commands get no plugin env and
the plugin binary lives under a hashed install dir. Missing file → command
fails → the entry hides.

## Code changes

- `herdr/rpc.rs`: replace `pane_state`/`PaneState` with `any_active()` backed
  by `pane.list` and a pure `parse_any_active(reply) -> Result<bool, _>`;
  drop `set_marker`/`clear_marker`.
- `herdr/events.rs`: unfiltered subscription; `SocketEvents::subscribe(path)`.
- `watcher.rs`: drop `pane_id`; add a `StatusSink` trait (write/clear
  statusline) and a resubscribe hook on the `Waiter` so the loop is fully
  testable with fakes.
- `toggle.rs`: global toggle; new `startup.rs`.
- `state.rs`: fixed file names, atomic write, legacy pidfile sweep; drop
  `sanitize_pane_id`, `list_monitored`.
- `consts.rs`: drop `MARKER*`; add icon/word constants.
- Delete `status.rs`, `bin/status`; add `bin/startup`.
- `policy.rs`, `espresso.rs`: unchanged.
- README (en + zh-CN), `docs/manual-test.md`: rewritten for the global model.

## Testing

- Unit: `parse_any_active` (no panes, shells only, idle agent, working,
  blocked, error reply); state (enabled flag, atomic statusline, pidfile
  reserve/race, legacy sweep); display state derivation.
- `tests/watcher_loop.rs` with fake Rpc/Waiter/StatusSink/Espresso: seeded
  active → awake immediately; all idle → release after grace; reactivate in
  grace cancels; list error keeps state; EOF + reconnect continues; EOF +
  reconnect failure cleans up; Stop cleans up and clears statusline.
- `tests/rpc_wire.rs`: `pane.list` request shape; unfiltered subscribe shape.
- Manual (herdr 0.9.3): toggle shows/hides the entry; two agents alternate →
  stays awake, all stop → idle; `pmset -g assertions` shows espresso's hold;
  restarting the herdr server restores the switch.

## Out of scope

Multiple herdr servers sharing one plugin state dir; per-pane overrides;
debouncing beyond the existing drain coalescing (revisit only if CPU use is
measurably high).
