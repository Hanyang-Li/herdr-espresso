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
- `events.subscribe` — `pane.agent_status_changed` requires a `pane_id`, and
  it is the **only** event emitted when an agent's status changes
  (`pane.updated` fires only when the agent *name* changes). `pane.created`,
  `pane.closed`, `pane.exited` and `pane.agent_detected` accept no filter.
  One request may carry many subscriptions; if any referenced pane does not
  exist the whole request is rejected and the connection closed. The first
  line back is the ack. New subscriptions do **not** replay history:
  subscribe first, then snapshot. A subscriber that falls behind gets an
  `events_lost` error line and its connection is closed.
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
min_herdr_version = "0.8.2"   # tab_bar_right (0.8.2); [[startup]] exists since 0.7.5

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

The event stream must cover every pane's status, so the subscription is
rebuilt whenever the set of panes changes:

- subscription = unfiltered `pane.created`, `pane.closed`, `pane.exited`,
  `pane.agent_detected` + one `pane.agent_status_changed` entry per pane id
  (all panes, shells included, so an agent started in a shell is seen).

**Sync** (startup, and every wake that reports events):

1. `pane.list` → snapshot `{any_active, pane_ids}`.
2. If `pane_ids` equals the subscribed set → apply `any_active`, done.
3. Otherwise open a new subscription for `pane_ids` (the old stream is
   replaced only when the new one is acknowledged), then go back to 1 — the
   snapshot that counts is the one taken after the subscription is live.
   A rejected subscription (a pane closed in between) also goes back to 1.
   At most 3 rounds per sync; if no stream was ever established, the watcher
   exits.

Status is fed to the existing `Machine::on_status`; timer wakes go to
`on_timer` (renew / stop-grace), unchanged. After each step the display
state is derived — `awake` while espresso is actually held
(`EspressoCtl::is_up`), else `idle` — and `statusline` is rewritten only when
it changed.

`any_active` = some pane has a non-empty `agent` and `agent_status` is
`working` or `blocked`.

### Exit paths and cleanup

- SIGTERM (toggle-off or startup replacement) → exit.
- Event stream EOF (server gone, or `events_lost` closed the stream) → drop
  the stream and sync again, which resubscribes; if the socket cannot be
  reached the server is gone → exit (keeping `enabled` so startup restores
  it). Two EOFs less than 5 s apart → exit, so a server that keeps closing
  the stream cannot spin the loop.
- Every exit runs the same cleanup: kill espresso, remove `statusline`,
  remove `watcher.pid`.

### Error handling

- Pushed event bytes are never parsed; any of them just triggers a sync.
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

- `herdr/rpc.rs`: replace `pane_state`/`PaneState` with `snapshot()` backed
  by `pane.list` and a pure `parse_snapshot(reply) -> Result<Snapshot, _>`;
  drop `set_marker`/`clear_marker`.
- `herdr/events.rs`: `SocketEvents::subscribe(path, pane_ids)` (lifecycle +
  per-pane status, ack checked); `Waiter::subscribe(pane_ids)` swaps streams.
- `watcher.rs`: drop `pane_id`; add a `StatusSink` trait (write/clear
  statusline); the sync logic above, fully testable with fakes.
- `toggle.rs`: global toggle; new `startup.rs`.
- `state.rs`: fixed file names, atomic write, legacy pidfile sweep; drop
  `sanitize_pane_id`, `list_monitored`.
- `consts.rs`: drop `MARKER*`; add icon/word constants.
- Delete `status.rs`, `bin/status`; add `bin/startup`.
- `policy.rs`, `espresso.rs`: unchanged.
- README (en + zh-CN), `docs/manual-test.md`: rewritten for the global model.

## Testing

- Unit: `parse_snapshot` (no panes, shells only, idle agent, working,
  blocked, error reply, sorted pane ids); state (enabled flag, atomic statusline, pidfile
  reserve/race, legacy sweep); display state derivation.
- `tests/watcher_loop.rs` with fake Rpc/Waiter/StatusSink/Espresso: seeded
  active → awake immediately; all idle → release after grace; reactivate in
  grace cancels; list error keeps state; pane set change resubscribes and
  re-snapshots; rejected subscription retries; EOF resubscribes; EOF with
  herdr unreachable cleans up; Stop cleans up and clears statusline.
- `tests/rpc_wire.rs`: `pane.list` request shape; subscribe request shape and ack handling.
- Manual (herdr 0.9.3): toggle shows/hides the entry; two agents alternate →
  stays awake, all stop → idle; `pmset -g assertions` shows espresso's hold;
  restarting the herdr server restores the switch.

## Out of scope

Multiple herdr servers sharing one plugin state dir; per-pane overrides;
debouncing beyond the existing drain coalescing (revisit only if CPU use is
measurably high).
