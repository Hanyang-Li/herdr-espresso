//! Wire-shape tests for the marker RPC.
//!
//! These exist because the marker silently never worked: the plugin sent a
//! `custom_status` field that herdr's `pane.report_metadata` does not have, and
//! the resulting `invalid_metadata_request` error reply was discarded. Field
//! names and error handling are now pinned against a stub of herdr's socket.

use herdr_espresso::herdr::{Rpc, SocketRpc};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::thread;

/// A stub of herdr's API socket: one request per connection, answered with a
/// scripted reply, then closed — same contract as the real server. Returns the
/// socket path and a channel of the request lines it received.
fn stub_server(name: &str, replies: Vec<String>) -> (String, mpsc::Receiver<String>) {
    let path = std::env::temp_dir().join(format!("he-rpc-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind stub socket");
    let (tx, rx) = mpsc::channel();
    let expected = replies.len();
    thread::spawn(move || {
        let mut sent = 0;
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            let mut line = String::new();
            if BufReader::new(&conn).read_line(&mut line).unwrap_or(0) == 0 {
                continue; // SocketRpc::connect probes with a connect-and-drop
            }
            let _ = tx.send(line);
            let _ = conn.write_all(replies[sent].as_bytes());
            sent += 1;
            if sent == expected {
                break;
            }
        }
    });
    (path.to_string_lossy().into_owned(), rx)
}

fn ok_reply() -> String {
    "{\"id\":\"r1\",\"result\":{\"type\":\"ok\"}}\n".to_string()
}

fn request(rx: &mpsc::Receiver<String>) -> serde_json::Value {
    let line = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("stub server received a request");
    serde_json::from_str(&line).expect("request is one JSON line")
}

#[test]
fn set_marker_reports_a_sidebar_token() {
    let (sock, rx) = stub_server("set", vec![ok_reply()]);
    let mut rpc = SocketRpc::connect(&sock).unwrap();
    rpc.set_marker("w1:p1").expect("ok reply -> Ok");

    let req = request(&rx);
    assert_eq!(req["method"], "pane.report_metadata");
    assert_eq!(req["params"]["pane_id"], "w1:p1");
    assert_eq!(req["params"]["source"], "espresso");
    // herdr carries plugin-reported sidebar labels in `tokens` (rendered where
    // the sidebar row template references `$espresso`). `custom_status` is not
    // a field herdr has ever accepted.
    assert_eq!(req["params"]["tokens"]["espresso"], "espresso");
    assert!(req["params"].get("custom_status").is_none());
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn clear_marker_nulls_the_token() {
    let (sock, rx) = stub_server("clear", vec![ok_reply()]);
    let mut rpc = SocketRpc::connect(&sock).unwrap();
    rpc.clear_marker("w1:p1").expect("ok reply -> Ok");

    let req = request(&rx);
    assert_eq!(req["method"], "pane.report_metadata");
    assert_eq!(req["params"]["source"], "espresso");
    // A null token value removes just that token, leaving other sources' tokens
    // on the pane untouched.
    assert!(req["params"]["tokens"]["espresso"].is_null());
    assert!(req["params"]["tokens"].get("espresso").is_some());
    assert!(req["params"].get("clear_custom_status").is_none());
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn error_reply_surfaces_as_err() {
    // The failure mode that hid the bug: a healthy connection carrying an
    // `error` body. Treating that as success made a marker that never appeared
    // indistinguishable from a working one.
    let reply = "{\"id\":\"r1\",\"error\":{\"code\":\"invalid_metadata_request\",\
                 \"message\":\"missing metadata field to set or clear\"}}\n"
        .to_string();
    let (sock, _rx) = stub_server("err", vec![reply]);
    let mut rpc = SocketRpc::connect(&sock).unwrap();
    let err = rpc
        .set_marker("w1:p1")
        .expect_err("an error reply must not be reported as success");
    let shown = format!("{err:?}");
    assert!(
        shown.contains("invalid_metadata_request"),
        "error should carry herdr's code, got: {shown}"
    );
    let _ = std::fs::remove_file(&sock);
}
