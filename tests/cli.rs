//! Binary-level checks for the paths that need no running herdr.

use std::path::{Path, PathBuf};
use std::process::Command;

fn state_dir(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("he-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run(sub: &str, dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_herdr-espresso"))
        .arg(sub)
        .env("HERDR_PLUGIN_STATE_DIR", dir)
        .env_remove("HERDR_SOCKET_PATH")
        .output()
        .unwrap()
}

#[test]
fn startup_while_off_clears_leftovers() {
    // A watcher killed hard leaves its line and pidfile behind.
    let dir = state_dir("startup-off");
    std::fs::write(dir.join("statusline"), "x\n").unwrap();
    std::fs::write(dir.join("watcher.pid"), "999999999").unwrap();
    let out = run("startup", &dir);
    assert!(out.status.success());
    assert!(!dir.join("statusline").exists());
    assert!(!dir.join("watcher.pid").exists());
}

#[test]
fn toggle_on_without_herdr_fails_and_stays_off() {
    // The watcher exits at once without a socket; toggle must notice and not
    // leave the switch on.
    let dir = state_dir("toggle-fail");
    let out = run("toggle", &dir);
    assert!(!out.status.success());
    assert!(!dir.join("enabled").exists());
    assert!(!dir.join("watcher.pid").exists());
}

#[test]
fn startup_while_on_without_herdr_fails_and_keeps_the_switch() {
    // The switch is the user's intent; a failed restore must not erase it.
    let dir = state_dir("startup-on");
    std::fs::write(dir.join("enabled"), "").unwrap();
    std::fs::write(dir.join("statusline"), "x\n").unwrap();
    let out = run("startup", &dir);
    assert!(!out.status.success());
    assert!(dir.join("enabled").exists());
    assert!(!dir.join("statusline").exists());
    assert!(!dir.join("watcher.pid").exists());
}

#[test]
fn startup_in_forced_mode_without_herdr_keeps_the_mode() {
    let dir = state_dir("startup-forced");
    std::fs::write(dir.join("enabled"), "forced").unwrap();
    let out = run("startup", &dir);
    assert!(!out.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.join("enabled")).unwrap(),
        "forced"
    );
    assert!(!dir.join("watcher.pid").exists());
}
