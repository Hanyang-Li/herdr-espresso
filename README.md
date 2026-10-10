# herdr-espresso ☕

> Keep your Mac awake while any coding agent in herdr is working — one switch for all of herdr.

**English** | [简体中文](README.zh-CN.md)

![license](https://img.shields.io/badge/license-MIT-blue.svg)
![platform](https://img.shields.io/badge/platform-macOS-lightgrey.svg)

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
- **Forced mode.** Press the key again to keep the Mac awake regardless of
  agent status, until you switch it off.

## Features

- **Follows every agent.** No per-pane setup — panes and agents that appear
  later are picked up automatically.
- **Tab bar status.** `󰅶 awake` / `󰛊 idle` / `󰅷 forced` on the right of herdr's
  tab bar while the switch is on, nothing while it is off — one line of herdr config away
  (see [Tab bar status](#tab-bar-status)).
- **Remembers the switch.** Turned on, it comes back on by itself — in the same
  mode — after herdr restarts.
- **Self-healing.** `espresso` is held via a short lease renewed while agents
  work, so a crashed watcher can never pin the Mac awake — the lease expires on
  its own within ~90s.
- **Event-driven.** The watcher blocks in the kernel and uses ~0% CPU while
  idle; toggle-off is instant.

## Requirements

- **macOS only.**
- [herdr](https://github.com/ogulcancelik/herdr) **0.8.2** or later.
- The [`espresso`](https://github.com/Hanyang-Li/espresso) CLI installed and on
  your `PATH`. Install it with:
  ```sh
  curl -fsSL https://raw.githubusercontent.com/Hanyang-Li/espresso/main/install.sh | sh
  ```
- **Optional:** run `espresso daemon install` once (requires `sudo`) so the Mac
  also stays awake with the **lid closed**. Without it, only idle/display sleep
  is prevented; switching on shows a reminder.

## Installation

```sh
herdr plugin install Hanyang-Li/herdr-espresso
```

herdr clones the repo, runs the build hook (`cargo build --release`), and
registers the plugin. Pin a specific version with `--ref`:

```sh
herdr plugin install Hanyang-Li/herdr-espresso --ref v1.1.0
```

## Keybinding

The plugin doesn't bind a key on its own — add one to your herdr config to bind
the `espresso.toggle` action, for example:

```toml
[[keys.command]]
key = "prefix+/"
type = "plugin_action"
command = "espresso.toggle"
description = "espresso: cycle keep-awake (auto / forced / off)"
```

Press it anywhere to cycle keep-awake for all of herdr through three modes:

**off → auto → forced → off**

| Mode | Behavior |
|---|---|
| **auto** | Awake while any agent is working or blocked, may sleep once all are idle. |
| **forced** | Awake no matter what the agents are doing, until switched off. |
| **off** | Nothing is held. |

A toast confirms each switch. The action is also listed in herdr's command
palette as **Espresso: cycle keep-awake (auto / forced / off)**.

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
| `󰅶 awake` | Auto, and at least one agent is working or blocked — the Mac stays awake. |
| `󰛊 idle` | Auto, all agents idle — the Mac may sleep. |
| `󰅷 forced` | Forced — the Mac stays awake regardless of agents. |
| *(nothing)* | Off. |

The icons are Nerd Font glyphs (md-coffee, md-coffee_outline, md-coffee_to_go);
use a Nerd Font in your terminal. herdr strips colors and other styling from
tab bar command output, so the modes are told apart by glyph and text. If you set `XDG_STATE_HOME`, the file lives at
`$XDG_STATE_HOME/herdr/plugins/espresso/statusline` instead. Other entries
(`datetime`, `hostname`, …) can sit alongside it in the same list.

### Upgrading from 0.1

0.1 monitored panes one at a time and marked them in the sidebar with a
`$espresso` token. 1.0 replaces that with the global switch: the old per-pane
watchers are stopped automatically the first time 1.0 runs, and you can remove
`$espresso` from `[ui.sidebar.agents] rows` (and any `rows_by_agent` entries).
The `espresso.status` action was also removed in 1.0: remove any keybinding to it,
or herdr reports it in `herdr server reload-config` diagnostics.

## How it works

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
- **Forced ignores agents.** In forced mode the watcher holds and renews the
  lease the whole time; it stays subscribed only to notice herdr exiting.
  Switching auto → forced replaces the watcher, so the hold is dropped for only
  a few milliseconds.
- **Self-healing lease.** Because the lease is short and renewed rather than
  held open-ended, a watcher that dies unexpectedly (crash, `kill -9`) can't
  keep the Mac awake — its `espresso` lease expires within ~90 seconds.
- **Restart-safe.** The switch and its mode are remembered in herdr's plugin
  state dir; when
  the herdr server starts, the plugin's startup hook brings the watcher back.
  Detaching a client doesn't affect it; `herdr server stop` ends it. Toggle and
  startup take a lock (`watcher.lock`) so they never race each other.

## Uninstalling

```sh
herdr plugin uninstall Hanyang-Li/herdr-espresso
```

## Development

For local development, or to try changes before publishing, **link** a working
copy instead of installing from GitHub:

```sh
git clone https://github.com/Hanyang-Li/herdr-espresso
cd herdr-espresso
herdr plugin link "$PWD"      # runs the build hook and registers it in place
```

Build and test directly:

```sh
cargo build --release
cargo test
```

Releases are built and published automatically by GitHub Actions when a `v*`
tag is pushed (`.github/workflows/release.yml`).

## License

[MIT](LICENSE) © Hanyang Li
