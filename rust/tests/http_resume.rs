//! Resumable bulk-transfer tests.
//!
//! `get_resumable` exists because the container is the only large transfer in
//! the protocol and the CDN path drops connections. A dropped body must resume
//! from the byte after the last one received, not restart — and the reassembled
//! buffer must be byte-identical to an uninterrupted transfer.
//!
//! A minimal TCP server stands in for the CDN so the failure modes (mid-body
//! close, server ignoring `Range`) are reproducible offline.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hotaudio::http;

/// Position-dependent body content, so a dropped, reordered or duplicated byte
/// is detectable rather than silently absorbed.
fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// Read request headers, returning the value of `Range: bytes=<n>-` if present.
fn read_request(stream: &TcpStream) -> Option<usize> {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut range_at = None;
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if header.trim().is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if lower.starts_with("range:") {
            range_at = lower
                .split("bytes=")
                .nth(1)
                .and_then(|r| r.split('-').next())
                .and_then(|s| s.trim().parse::<usize>().ok());
        }
    }
    range_at
}

/// A listener bound to an ephemeral port, shared with the serving thread.
fn bind() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("bind loopback")
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into()
}

#[test]
fn resumes_after_a_mid_body_disconnect() {
    let body = payload(400_000);
    let drop_at = 150_000usize;

    let listener = bind();
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));

    let server = {
        let body = body.clone();
        let hits = hits.clone();
        std::thread::spawn(move || {
            // First connection: promise the full length, send a prefix, hang up.
            let (stream, _) = listener.accept().expect("accept 1");
            let range = read_request(&stream);
            assert!(range.is_none(), "first request should not be ranged");
            hits.fetch_add(1, Ordering::SeqCst);
            let mut out = stream.try_clone().expect("clone");
            let _ = write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = out.write_all(&body[..drop_at]);
            let _ = out.flush();
            drop(out);
            drop(stream);

            // Second connection: honour the range and send only the remainder.
            let (stream, _) = listener.accept().expect("accept 2");
            let range = read_request(&stream);
            assert_eq!(range, Some(drop_at.min(body.len())), "resume offset");
            hits.fetch_add(1, Ordering::SeqCst);
            let start = range.unwrap_or(0).min(body.len());
            let tail = &body[start..];
            let mut out = stream.try_clone().expect("clone");
            let _ = write!(
                out,
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\r\n",
                tail.len()
            );
            let _ = out.write_all(tail);
            let _ = out.flush();
        })
    };

    let resp = http::get_resumable(&agent(), &format!("http://{addr}/x"), &[], |_, _| {})
        .expect("resumable fetch should recover");
    server.join().expect("server thread");

    assert_eq!(
        resp.body, body,
        "resumed body must be byte-identical to an uninterrupted transfer"
    );
    assert_eq!(resp.body.len(), 400_000);
    assert_eq!(hits.load(Ordering::SeqCst), 2, "one drop plus one resume");
}

#[test]
fn discards_the_partial_buffer_when_the_server_ignores_range() {
    // A server that answers 200 (not 206) to a ranged request is resending the
    // whole object; appending to the partial buffer would corrupt it.
    let body = payload(20_000);
    let prefix = 5_000usize;

    let listener = bind();
    let addr = listener.local_addr().expect("addr");

    let server = {
        let body = body.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept 1");
            let _ = read_request(&stream);
            let mut out = stream.try_clone().expect("clone");
            let _ = write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = out.write_all(&body[..prefix]);
            let _ = out.flush();
            drop(out);
            drop(stream);

            // Answers 200 and the full body, deliberately ignoring the range.
            let (stream, _) = listener.accept().expect("accept 2");
            let _ = read_request(&stream);
            let mut out = stream.try_clone().expect("clone");
            let _ = write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = out.write_all(&body);
            let _ = out.flush();
        })
    };

    let resp = http::get_resumable(&agent(), &format!("http://{addr}/y"), &[], |_, _| {})
        .expect("fetch should recover");
    server.join().expect("server thread");

    assert_eq!(
        resp.body, body,
        "a 200 reply must replace the buffer, not append to it"
    );
}

#[test]
fn reports_progress_monotonically() {
    let body = payload(300_000);
    let listener = bind();
    let addr = listener.local_addr().expect("addr");

    let server = {
        let body = body.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let _ = read_request(&stream);
            let mut out = stream.try_clone().expect("clone");
            let _ = write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = out.write_all(&body);
            let _ = out.flush();
        })
    };

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    let resp = http::get_resumable(&agent(), &format!("http://{addr}/z"), &[], move |loaded, total| {
        sink.lock().expect("lock").push((loaded, total));
    })
    .expect("fetch");
    server.join().expect("server thread");

    assert_eq!(resp.body, body);
    let samples = seen.lock().expect("lock").clone();
    assert!(samples.len() >= 2, "expected streaming progress, got {:?}", samples);
    // Loaded must never go backwards, and must finish at the body length.
    assert!(
        samples.windows(2).all(|w| w[1].0 >= w[0].0),
        "progress went backwards: {samples:?}"
    );
    let (last_loaded, _) = *samples.last().expect("samples");
    assert_eq!(last_loaded, body.len() as u64);
}
