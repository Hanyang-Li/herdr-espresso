//! Wire-shape tests for the herdr socket requests, against a stub of herdr's socket.

use herdr_espresso::consts::{RPC_TIMEOUT, SUBSCRIBE_ACK_TIMEOUT};
use herdr_espresso::herdr::{Rpc, SocketEvents, SocketRpc};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

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

fn request(rx: &mpsc::Receiver<String>) -> serde_json::Value {
    let line = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("stub server received a request");
    serde_json::from_str(&line).expect("request is one JSON line")
}

#[test]
fn error_reply_surfaces_as_err() {
    // A healthy connection carrying an `error` body must not read as success.
    let reply =
        "{\"id\":\"r1\",\"error\":{\"code\":\"busy\",\"message\":\"toast rate limited\"}}\n"
            .to_string();
    let (sock, _rx) = stub_server("err", vec![reply]);
    let mut rpc = SocketRpc::connect(&sock).unwrap();
    let err = rpc
        .notify("t", "b")
        .expect_err("an error reply must not be reported as success");
    assert!(format!("{err:?}").contains("busy"));
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn snapshot_lists_all_panes() {
    let reply = "{\"id\":\"r1\",\"result\":{\"type\":\"pane_list\",\"panes\":[\
                 {\"pane_id\":\"w1:p1\",\"agent\":\"claude\",\"agent_status\":\"working\"},\
                 {\"pane_id\":\"w1:p2\",\"agent_status\":\"unknown\"}]}}\n"
        .to_string();
    let (sock, rx) = stub_server("list", vec![reply]);
    let mut rpc = SocketRpc::connect(&sock).unwrap();
    let snap = rpc.snapshot().expect("pane_list reply -> Ok");
    assert!(snap.any_active);
    assert_eq!(snap.pane_ids, vec!["w1:p1", "w1:p2"]);

    let req = request(&rx);
    assert_eq!(req["method"], "pane.list");
    // No workspace filter: the watcher needs every pane in the session.
    assert_eq!(req["params"], serde_json::json!({}));
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn subscribe_lists_lifecycle_and_every_pane() {
    let ack = "{\"id\":\"s1\",\"result\":{\"type\":\"subscription_started\"}}\n".to_string();
    let (sock, rx) = stub_server("sub", vec![ack]);
    let ids = vec!["w1:p1".to_string(), "w2:p3".to_string()];
    SocketEvents::subscribe(&sock, &ids).expect("ack -> Ok");

    let req = request(&rx);
    assert_eq!(req["method"], "events.subscribe");
    let subs = req["params"]["subscriptions"].as_array().unwrap();
    let types: Vec<&str> = subs.iter().map(|s| s["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        [
            "pane.created",
            "pane.closed",
            "pane.exited",
            "pane.agent_detected",
            "pane.agent_status_changed",
            "pane.agent_status_changed"
        ]
    );
    // Lifecycle kinds are unfiltered; status changes are per pane (herdr
    // requires a pane_id for them).
    assert!(subs[..4].iter().all(|s| s.get("pane_id").is_none()));
    assert_eq!(subs[4]["pane_id"], "w1:p1");
    assert_eq!(subs[5]["pane_id"], "w2:p3");
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn rejected_subscription_is_an_error() {
    // herdr rejects the whole request when a listed pane is gone.
    let reply = "{\"id\":\"s1\",\"error\":{\"code\":\"pane_not_found\",\"message\":\"w1:p9\"}}\n"
        .to_string();
    let (sock, _rx) = stub_server("subrej", vec![reply]);
    let err = SocketEvents::subscribe(&sock, &["w1:p9".to_string()])
        .err()
        .expect("error ack must not look like a live stream");
    assert!(format!("{err:?}").contains("pane_not_found"));
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn subscription_timeout_if_herdr_accepts_but_never_acks() {
    // A listener that accepts the connection and reads the request but never
    // sends a reply. The subscribe call should timeout waiting for the ack.
    let path = std::env::temp_dir().join(format!("he-rpc-{}-timeout.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind stub socket");

    let path_clone = path.to_string_lossy().into_owned();
    thread::spawn(move || {
        if let Ok(conn) = listener.accept() {
            let (stream, _addr) = conn;
            // Read the request line but never send a reply; hold the connection open.
            let mut line = String::new();
            let _ = BufReader::new(&stream).read_line(&mut line);
            // Keep the connection open for 10 seconds (longer than SUBSCRIBE_ACK_TIMEOUT).
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    });

    let start = Instant::now();
    let err = SocketEvents::subscribe(&path.to_string_lossy(), &["w1:p1".to_string()])
        .err()
        .expect("timeout must surface as an error");
    let elapsed = start.elapsed();

    // Should timeout within SUBSCRIBE_ACK_TIMEOUT + a small margin.
    assert!(
        elapsed >= SUBSCRIBE_ACK_TIMEOUT,
        "timeout took {} s, expected at least {:?}",
        elapsed.as_secs_f64(),
        SUBSCRIBE_ACK_TIMEOUT
    );
    assert!(
        elapsed < SUBSCRIBE_ACK_TIMEOUT + std::time::Duration::from_secs(2),
        "timeout took {} s, expected at most {:?} + 2s",
        elapsed.as_secs_f64(),
        SUBSCRIBE_ACK_TIMEOUT
    );
    assert!(format!("{err:?}").contains("Io") || format!("{err:?}").contains("timeout"));
    let _ = std::fs::remove_file(&path_clone);
}

#[test]
fn call_times_out_if_herdr_accepts_but_never_replies() {
    // A pane.list that herdr accepts but never answers must not block the
    // watcher loop (and with it lease renewal) forever. The stub sleeps only
    // ~10 s, so even a regression cannot hang the suite.
    let path = std::env::temp_dir().join(format!("he-rpc-{}-rpctimeout.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind stub socket");
    thread::spawn(move || {
        // The first connection is SocketRpc::connect's probe; hold every
        // connection open after reading its request line, never replying.
        for conn in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut line = String::new();
                let _ = BufReader::new(&conn).read_line(&mut line);
                thread::sleep(std::time::Duration::from_secs(10));
            });
        }
    });

    let mut rpc = SocketRpc::connect(&path.to_string_lossy()).unwrap();
    let start = Instant::now();
    let err = rpc.snapshot().expect_err("a silent herdr must time out");
    let elapsed = start.elapsed();

    assert!(
        matches!(err, herdr_espresso::herdr::HerdrError::Io(_)),
        "expected Io timeout, got {err:?}"
    );
    assert!(
        elapsed < RPC_TIMEOUT + std::time::Duration::from_secs(2),
        "call took {} s, expected at most {:?} + 2s",
        elapsed.as_secs_f64(),
        RPC_TIMEOUT
    );
    let _ = std::fs::remove_file(&path);
}
