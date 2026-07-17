# Manual test checklist

These steps require a real herdr installation (≥ 0.7.0) and a real
`espresso` CLI on `PATH` — they exercise the actual macOS sleep-assertion
and herdr socket/plugin machinery, which cannot be verified by the
automated unit tests alone. Run them on macOS.

## Setup

1. From this repo's directory, link the plugin into your local herdr:

   ```bash
   herdr plugin link "$PWD"
   ```

   Confirm the build hook succeeds (`cargo build --release`) and
   `target/release/herdr-espresso` exists.

2. Add the keybinding to your herdr config:

   ```toml
   [[keys.command]]
   key = "prefix+/"
   type = "plugin_action"
   command = "espresso.toggle"
   description = "espresso: toggle monitor on focused pane"
   ```

   Reload herdr's config (or restart the herdr server) so the plugin and
   keybinding are picked up.

## Checklist

- [ ] **1. Toggle on + marker appears.** In a pane running an agent, focus
      it and press `prefix+/`. Confirm the `espresso` marker appears
      next to the pane in the sidebar, and that a watcher is running:
      `herdr-espresso status` (or the `espresso.status` action) lists the
      pane with a live watcher pid.

- [ ] **2. Working → Mac stays awake.** Drive the agent in that pane to the
      `working` status. Check for an active `espresso` assertion:

      ```bash
      pmset -g assertions | grep -i espresso
      ```

      (or look for an assertion process named/owned by `espresso`). Confirm
      one is present.

- [ ] **3. Idle → release after grace.** Let the agent go idle (or finish,
      `done`). Wait ~5 seconds, then re-check `pmset -g assertions`; the
      espresso assertion should be gone. The pane should still show as
      monitored (marker still present, watcher still running).

- [ ] **4. Toggle off cleans up.** Press `prefix+/` again on
      that pane. Confirm: the `espresso` marker disappears, `herdr-espresso status`
      no longer lists the pane, and there is no lingering `espresso` process
      for it (check `pmset -g assertions` and/or `ps aux | grep espresso`).

- [ ] **5. Two panes are independent.** Toggle monitoring on in two separate
      panes and drive both agents to `working`. Confirm both hold
      `espresso` (two assertions, or one shared — either way both panes
      show as monitored). Close one of the panes. Confirm that pane's
      watcher and `espresso` process stop, while the other pane's watcher
      keeps running and its hold is unaffected.

- [ ] **6. Detach keeps monitoring alive.** With a pane monitored and its
      agent `working`, detach from herdr (`prefix+q`) and reattach. Confirm
      the watcher is still running and the pane is still monitored — the
      herdr server and its socket persist across client detach, so the
      watcher (which talks to the socket, not the client) is unaffected.
      (Running `herdr server stop` does stop the watchers, since the
      socket/server they depend on goes away.)

- [ ] **7. Watcher crash self-heals.** With a pane monitored and `working`
      (so an `espresso` lease is held), find the watcher's pid
      (`herdr-espresso status`) and kill it hard: `kill -9 <pid>`. Confirm
      the `espresso` process it was holding is *not* killed immediately,
      but self-expires on its own within about 90 seconds (check
      `pmset -g assertions` before and after). This confirms the lease
      lifetime bounds the damage from an unexpectedly-dead watcher — nothing
      is left running indefinitely.

- [ ] **8. Missing-daemon warning.** Without having run
      `espresso daemon install` (or on a machine where it is known not to
      be installed), toggle monitoring on for a pane. Confirm a notification
      appears telling you to run `espresso daemon install` for lid-closed
      keep-awake. This check runs once at the start of each watcher's
      lifetime (i.e. once per toggle-on), not once globally — expect it
      again if you toggle the pane off and on, or start monitoring a
      different pane, while the daemon remains uninstalled.
