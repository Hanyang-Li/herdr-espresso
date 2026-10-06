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

- [ ] **7. Double press.** With the switch off, press `prefix+/` twice in quick
      succession. The result is "on, then off": the entry appears briefly (or
      not at all) and ends hidden, `$STATE/watcher.pid` is gone, and
      `ps aux | grep "herdr-espresso watch"` shows no watcher left running.

- [ ] **8. Restart restores the switch.** Toggle on, then `herdr server stop`
      and start herdr again. Without pressing anything the tab bar comes back
      (`󰛊 idle` / `󰅶 awake`) and `$STATE/watcher.pid` names a new live
      process. Repeat with the switch off: after the restart the entry stays
      hidden and no watcher runs.

- [ ] **9. Watcher crash self-heals.** With an agent working, `kill -9` the pid
      in `$STATE/watcher.pid`. The espresso assertion is *not* dropped at once
      but expires within ~90 s. The tab bar may keep showing the stale line
      until the next toggle or herdr start; pressing `prefix+/` once starts a
      fresh watcher (a dead watcher counts as off).

- [ ] **10. Upgrade from 0.1 cleans up.** With 0.1 installed, monitor a pane so
      its sidebar shows `espresso`. Link 1.0 and press `prefix+/`. The old
      per-pane watcher exits, the sidebar marker disappears, and
      `$STATE/*.pid` contains only `watcher.pid`.

- [ ] **11. Missing-daemon reminder.** Without `espresso daemon install`, toggle
      on: a toast tells you to run `espresso daemon install` for lid-closed
      keep-awake.

- [ ] **12. Idle CPU.** With the switch on and several panes open but idle,
      `top -pid <watcher pid>` shows ~0% CPU.
