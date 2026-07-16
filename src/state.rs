use std::path::PathBuf;

pub fn sanitize_pane_id(pane_id: &str) -> String {
    let mut out = String::with_capacity(pane_id.len());
    for &b in pane_id.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HERDR_PLUGIN_STATE_DIR") {
        return PathBuf::from(dir);
    }
    std::env::temp_dir().join("herdr-espresso")
}

pub fn pidfile(pane_id: &str) -> PathBuf {
    state_dir().join(format!("{}.pid", sanitize_pane_id(pane_id)))
}

pub fn write_pidfile(pane_id: &str, pid: i32) -> std::io::Result<()> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(pidfile(pane_id), pid.to_string())
}

pub fn read_pidfile(pane_id: &str) -> Option<i32> {
    let s = std::fs::read_to_string(pidfile(pane_id)).ok()?;
    s.trim().parse().ok()
}

pub fn remove_pidfile(pane_id: &str) {
    let _ = std::fs::remove_file(pidfile(pane_id));
}

pub fn remove_pidfile_sanitized(sanitized: &str) {
    let _ = std::fs::remove_file(state_dir().join(format!("{sanitized}.pid")));
}

pub fn pid_alive(pid: i32) -> bool {
    // kill(pid, 0) probes existence without sending a signal.
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn list_monitored() -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(state_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".pid") {
            if let Ok(s) = std::fs::read_to_string(entry.path()) {
                if let Ok(pid) = s.trim().parse::<i32>() {
                    out.push((stem.to_string(), pid));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_is_readable_and_collision_free() {
        assert_eq!(sanitize_pane_id("w1:p1"), "w1%3Ap1");
        assert_eq!(sanitize_pane_id("w1.p1_2-3"), "w1.p1_2-3");
        assert_ne!(sanitize_pane_id("a:b"), sanitize_pane_id("a-b"));
        assert_ne!(sanitize_pane_id("a/b"), sanitize_pane_id("a%2Fb"));
    }

    #[test]
    fn roundtrip_pidfile(/* uses a temp HERDR_PLUGIN_STATE_DIR */) {
        let dir = std::env::temp_dir().join(format!("he-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HERDR_PLUGIN_STATE_DIR", &dir);
        assert_eq!(read_pidfile("w1:pX"), None);
        write_pidfile("w1:pX", 4242).unwrap();
        assert_eq!(read_pidfile("w1:pX"), Some(4242));
        remove_pidfile("w1:pX");
        assert_eq!(read_pidfile("w1:pX"), None);
    }

    #[test]
    fn self_pid_is_alive() {
        assert!(pid_alive(std::process::id() as i32));
        assert!(!pid_alive(999_999_999));
    }
}
