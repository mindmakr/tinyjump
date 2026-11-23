//! Loopback HTTP automation server on `127.0.0.1:<port>`:
//!   GET  /_tj/inspect
//!   POST /_tj/input
//!   GET  /_tj/screenshot (PNG)

use serde_json::{json, Value as Json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const MAX_REQUEST_BYTES: usize = 1 << 20;

pub enum AutoReq {
    Inspect(Sender<Result<Json, String>>),
    Input(Json, Sender<Result<Json, String>>),
    Screenshot(Sender<Result<Vec<u8>, String>>),
}

enum ReadErr {
    TooLarge,
    BadOrEof,
}

pub fn start(port: u16, dispatch: Arc<dyn Fn(AutoReq) + Send + Sync>) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("automation bind 127.0.0.1:{port} failed: {e}"))?;
    eprintln!("tinyjump-linux: automation on 127.0.0.1:{port}");
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let dispatch = dispatch.clone();
            thread::spawn(move || handle(stream, &dispatch));
        }
    });
    Ok(())
}

fn handle(mut stream: TcpStream, dispatch: &Arc<dyn Fn(AutoReq) + Send + Sync>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let (method, path, body) = match read_http_request(&mut stream) {
        Ok(req) => req,
        Err(ReadErr::TooLarge) => {
            send_response(&mut stream, 413, "application/json", json!({ "error": "request too large" }).to_string().as_bytes());
            return;
        }
        Err(ReadErr::BadOrEof) => return,
    };
    match (method.as_str(), path.as_str()) {
        ("GET", "/_tj/inspect") => {
            let (tx, rx) = channel();
            dispatch(AutoReq::Inspect(tx));
            reply_json(&mut stream, rx.recv_timeout(Duration::from_secs(10)));
        }
        ("POST", "/_tj/input") => match serde_json::from_slice::<Json>(&body) {
            Ok(v) if v.is_object() => {
                let (tx, rx) = channel();
                dispatch(AutoReq::Input(v, tx));
                reply_json(&mut stream, rx.recv_timeout(Duration::from_secs(10)));
            }
            _ => send_response(&mut stream, 400, "application/json", json!({ "error": "body must be one protocol input" }).to_string().as_bytes()),
        },
        ("GET", "/_tj/screenshot") => {
            let (tx, rx) = channel();
            dispatch(AutoReq::Screenshot(tx));
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(png)) => send_response(&mut stream, 200, "image/png", &png),
                Ok(Err(e)) => send_response(&mut stream, 500, "application/json", json!({ "error": e }).to_string().as_bytes()),
                Err(_) => send_response(&mut stream, 504, "application/json", json!({ "error": "screenshot timed out" }).to_string().as_bytes()),
            }
        }
        _ => send_response(&mut stream, 404, "application/json", json!({ "error": "not found" }).to_string().as_bytes()),
    }
}

fn reply_json(stream: &mut TcpStream, res: Result<Result<Json, String>, std::sync::mpsc::RecvTimeoutError>) {
    match res {
        Ok(Ok(v)) => send_response(stream, 200, "application/json", json!({ "ok": v }).to_string().as_bytes()),
        Ok(Err(e)) => send_response(stream, 400, "application/json", json!({ "error": e }).to_string().as_bytes()),
        Err(_) => send_response(stream, 504, "application/json", json!({ "error": "request timed out" }).to_string().as_bytes()),
    }
}

fn send_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        504 => "Gateway Timeout",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn read_http_request(stream: &mut TcpStream) -> Result<(String, String, Vec<u8>), ReadErr> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    let mut content_len = 0usize;
    loop {
        let n = stream.read(&mut chunk).map_err(|_| ReadErr::BadOrEof)?;
        if n == 0 {
            return Err(ReadErr::BadOrEof);
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_REQUEST_BYTES {
            return Err(ReadErr::TooLarge);
        }
        if header_end.is_none() {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
                let head = std::str::from_utf8(&buf[..pos]).map_err(|_| ReadErr::BadOrEof)?;
                for line in head.lines().skip(1) {
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_len = v.trim().parse().map_err(|_| ReadErr::BadOrEof)?;
                            if content_len > MAX_REQUEST_BYTES {
                                return Err(ReadErr::TooLarge);
                            }
                        }
                    }
                }
            }
        }
        if let Some(end) = header_end {
            if buf.len() >= end + content_len {
                let head = std::str::from_utf8(&buf[..end - 4]).map_err(|_| ReadErr::BadOrEof)?;
                let mut parts = head.lines().next().ok_or(ReadErr::BadOrEof)?.split_whitespace();
                let method = parts.next().ok_or(ReadErr::BadOrEof)?.to_string();
                let raw_path = parts.next().ok_or(ReadErr::BadOrEof)?;
                let path = raw_path.split('?').next().unwrap_or(raw_path).to_string();
                return Ok((method, path, buf[end..end + content_len].to_vec()));
            }
        }
    }
}
