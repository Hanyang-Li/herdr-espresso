# herdr-espresso ☕

> Keep your Mac awake while a herdr pane's coding agent is working — automatically, per pane.

**English** | [简体中文](README.zh-CN.md)

![license](https://img.shields.io/badge/license-MIT-blue.svg)
![platform](https://img.shields.io/badge/platform-macOS-lightgrey.svg)

`herdr-espresso` is a [herdr](https://github.com/ogulcancelik/herdr) plugin that
keeps macOS awake while a monitored pane's coding agent is actively working, and
lets the Mac sleep again once the agent goes idle. It drives the
[`espresso`](https://github.com/Hanyang-Li/espresso) CLI: for each pane you turn
monitoring on, a small detached watcher subscribes to that pane's agent-status
events over the herdr socket and holds (or releases) an `espresso` session
accordingly.

- **Working/blocked → awake.** While the agent is active the Mac won't sleep
  (and, with espresso's lid-closed helper installed, stays awake even with the
  lid shut).
- **Idle/done → sleep.** When the agent stops, `espresso` is released after a
  short grace period; the pane stays monitored and re-acquires it the next time
  the agent becomes active.
- **Per pane, fully automatic.** Toggle it once per pane; multiple panes are
  monitored independently. Closing the pane — or the agent exiting — stops that
  pane's monitor automatically.

## Features

- **Follows the agent.** No manual on/off — monitoring tracks the agent's state.
- **Only agent panes.** Toggling a plain shell is declined with a notification
  (a shell has no working/idle state to track).
- **Sidebar marker.** An `espresso` label appears next to a monitored pane and
  disappears when monitoring stops.
- **Self-healing.** `espresso` is held via a short lease renewed while the agent
  works, so a crashed watcher can never pin the Mac awake — the lease expires on
  its own within ~90s.
- **Event-driven.** The watcher blocks in the kernel and uses ~0% CPU while
  idle; toggle-off is instant.

## Requirements

- **macOS only.**
- [herdr](https://github.com/ogulcancelik/herdr) **0.7.0** or later.
- The [`espresso`](https://github.com/Hanyang-Li/espresso) CLI installed and on
  your `PATH`. Install it with:
  ```sh
  curl -fsSL https://raw.githubusercontent.com/Hanyang-Li/espresso/main/install.sh | sh
  ```
- **Optional:** run `espresso daemon install` once (requires `sudo`) so the Mac
  also stays awake with the **lid closed**. Without it, only idle/display sleep
  is prevented; toggling monitoring on shows a one-time reminder.

## Installation

```sh
herdr plugin install Hanyang-Li/herdr-espresso
```

herdr clones the repo, runs the build hook (`cargo build --release`), and
registers the plugin. Pin a specific version with `--ref`:

```sh
herdr plugin install Hanyang-Li/herdr-espresso --ref v0.1.0
```

## Keybinding

The plugin doesn't bind a key on its own — add one to your herdr config to bind
the `espresso.toggle` action, for example:

```toml
[[keys.command]]
key = "prefix+/"
type = "plugin_action"
command = "espresso.toggle"
description = "espresso: toggle monitor on focused pane"
```

Press it while an **agent** pane is focused to toggle monitoring for that pane.
An `espresso` marker appears next to the pane in the sidebar while it's
monitored. Toggling a pane with no agent is declined with a notification.

## How it works

- **Working/blocked → awake.** The watcher holds `espresso` via a short
  90-second lease that it renews (rotates) every 60 seconds while the agent
  stays active, so the hold never has a coverage gap.
- **Idle/done → release after grace.** After the agent stops being active, the
  watcher waits ~5 seconds (so a brief flip back to active doesn't thrash it)
  before releasing `espresso`. Monitoring continues.
- **Agent exit / pane close → stop.** If the pane closes, or the agent exits
  and the pane returns to a plain shell, the watcher stops on its own: it
  releases `espresso` and removes the marker.
- **Self-healing lease.** Because the lease is short and renewed rather than
  held open-ended, a watcher that dies unexpectedly (crash, `kill -9`) can't
  keep the Mac awake — its `espresso` lease expires within ~90 seconds.
- **Detach-safe.** The watcher runs detached in its own session, so detaching
  from herdr doesn't stop monitoring; `herdr server stop` cleans it up.
- **Per-pane independence.** Each monitored pane has its own watcher and its own
  `espresso` hold; toggling or closing one has no effect on others.

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
