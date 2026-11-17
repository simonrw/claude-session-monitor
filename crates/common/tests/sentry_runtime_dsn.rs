//! The Sentry DSN can be supplied in the environment when a binary starts.
#![cfg(feature = "sentry")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

/// Accepts one HTTP request, answers 200 and returns the request line.
fn receive_one_request(listener: TcpListener) -> String {
    let (mut stream, _) = listener.accept().expect("accept");
    let mut request_line = String::new();
    BufReader::new(&stream)
        .read_line(&mut request_line)
        .expect("read request line");

    stream
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        .expect("write response");

    request_line
}

#[test]
fn reports_to_the_dsn_set_at_runtime() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(receive_one_request(listener)));

    // SAFETY: this is the only test in this binary, so nothing else touches the environment.
    unsafe { std::env::set_var("SENTRY_DSN", format!("http://key@127.0.0.1:{port}/42")) };

    let _guard = common::sentry::init("test");
    common::sentry::capture_error(&std::io::Error::other("boom"));

    let request_line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("nothing was reported to the runtime DSN");

    assert!(
        request_line.starts_with("POST /api/42/envelope/"),
        "unexpected request: {request_line}"
    );
}
