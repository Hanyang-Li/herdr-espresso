# herdr-espresso

**Espresso Guard** — a herdr plugin that keeps macOS awake while a herdr
pane's coding agent is actively working, and lets it sleep again once the
agent goes idle.

Under the hood it wraps the `espresso` CLI: for each pane you toggle
monitoring on, a small detached watcher subscribes to that pane's
agent-status events over the herdr socket and holds (or releases) an
`espresso` process accordingly.

## What it does

- Toggling monitoring on a pane starts a per-pane watcher.
- While monitored: when the pane's agent is `working` or `blocked`, the
  watcher holds an `espresso` lease so the Mac does not sleep (and, with the
  optional daemon installed, does not sleep even with the lid closed). When
  the agent goes `idle`/`done`, the watcher releases `espresso` after a short
  grace period.
- A `󰅶` marker appears next to the pane in the herdr sidebar while it is
  monitored, and is removed when monitoring is toggled off.
- Multiple panes can be monitored independently and simultaneously; each has
  its own watcher and its own `espresso` hold.
- Closing a monitored pane stops its watcher, releases its `espresso`, and
  removes the marker automatically.

## Requirements

- **macOS only.** This plugin is not supported on Linux or Windows.
- herdr **0.7.0** or later.
- The `espresso` CLI installed and available on `PATH`. Without it, the
  plugin cannot hold the Mac awake at all — the watcher notifies you (once
  per pane's monitoring session) that `espresso` was not found.
- **Optional:** run `espresso daemon install` once (requires `sudo`) to
  additionally keep the Mac awake with the **lid closed**. Without this
  one-time setup, the plugin still prevents idle/display sleep, but a lid
  close can still put the machine to sleep. Each time you toggle monitoring
  on while the daemon is not installed, you'll see a notification reminding
  you to run it.

## Install

Install directly from GitHub:

```bash
herdr plugin install Hanyang-Li/herdr-espresso
```

For local development, or to try this repo before it's published, link it
in place instead:

```bash
herdr plugin link "$PWD"
```

Either way, herdr runs the plugin's build hook (`cargo build --release`) to
produce `target/release/herdr-espresso`, which the `bin/toggle` and
`bin/status` wrappers exec.

For the plugin to be discoverable in the herdr marketplace (as opposed to
installed by explicit `owner/repo`), the repository must be public and
tagged with the GitHub topic `herdr-plugin`.

## Keybinding

This plugin does not register a keybinding on its own — add one to your
herdr config to bind `espresso.toggle` to a key, for example:

```toml
[[keys.command]]
key = "prefix+shift+option+e"
type = "plugin_action"
command = "espresso.toggle"
description = "espresso: toggle monitor on focused pane"
```

Press it while a pane is focused to toggle monitoring for that pane. You can
also invoke `espresso.toggle` and `espresso.status` from anywhere herdr lets
you run plugin actions (e.g. a command palette), since `toggle` supports the
`pane`, `workspace`, and `global` contexts and `status` supports `global`.

## Behavior details

- **Working/blocked → awake.** The watcher holds `espresso` via a short
  90-second lease that it renews (rotates) every 60 seconds while the agent
  stays active, so the hold never has a coverage gap.
- **Idle/done → release after grace.** After the agent stops being active,
  the watcher waits ~5 seconds (in case it flips back to active almost
  immediately) before releasing `espresso`. Monitoring itself is not
  affected — the pane stays monitored and will re-acquire `espresso` the
  next time the agent becomes active.
- **Self-healing lease.** Because the lease is short and renewed rather than
  held open-ended, if the watcher process dies unexpectedly (crash, `kill
  -9`, etc.) the outstanding `espresso` lease expires on its own within about
  90 seconds — there is no way for a dead watcher to keep the Mac awake
  indefinitely.
- **Detach-safe.** The watcher is spawned detached (its own session), so
  detaching from herdr (and the herdr server/socket persisting in the
  background) does not stop monitoring. Running `herdr server stop` does
  clean the watchers up.
- **Per-pane independence.** Each monitored pane has its own watcher and its
  own `espresso` process; toggling or closing one pane has no effect on
  others.

## Checking what's monitored

The primary indicator is visual: any monitored pane shows the `󰅶` marker in
the herdr sidebar. No command needed.

To list monitored panes from the CLI, run the `status` action through herdr.
herdr captures a plugin action's stdout into its log rather than returning it
inline, so it's two steps:

```bash
herdr plugin action invoke status --plugin espresso   # run it
herdr plugin log list --plugin espresso               # read the `stdout` field
```

The output is either `no panes monitored` or one `monitoring: <pane> (watcher
pid <n>)` line per monitored pane.

(`herdr plugin action invoke`/`herdr plugin log list` are herdr's own commands;
there is no `herdr-espresso` command on your `PATH` — the binary lives inside
the installed plugin and is only ever run by herdr via the `bin/` wrappers.)

## License

MIT — see `LICENSE`.
