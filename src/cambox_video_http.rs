//! Issue 1540 slice 2 -- the read-only HTTP endpoint of the cambox video counters
//! (`GET :8896/video.json`, [`crate::cambox_video_stats`]).
//!
//! A plain std `TcpListener` on its own thread: the endpoint is one read-only GET every 5-10 s
//! from dcontroller, so no HTTP framework or async runtime enters the appliance process. One
//! connection is served at a time, so each one has ONE deadline of [`CONNECTION_DEADLINE`] for its
//! whole request and the drain after the answer (a client trickling a byte at a time cannot hold
//! the server past it), and at most [`MAX_REQUEST_BYTES`] of request line + headers (a body is
//! never read). GET (or HEAD) of [`crate::cambox_video_stats::PATH`] = 200 `application/json`;
//! another path = 404; another method = 405; an oversize, timed-out or unparsable request = 400.
//! The thread runs at SCHED_OTHER and only reads atomics, so the capture path never waits on it.
//!
//! It listens on every interface without authentication: the rig LAN is the only network a
//! cambox is on, and the document is read-only counters plus the version and the boot id.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// The longest request line + headers a request may send.
pub const MAX_REQUEST_BYTES: usize = 4096;

/// How long one connection may take: its request, the answer and the drain after it.
pub const CONNECTION_DEADLINE: Duration = Duration::from_secs(2);

/// The answer to one request: status code, reason, body (empty body = none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub reason: &'static str,
    pub body: String,
}

/// The answer to a request head (`GET /video.json HTTP/1.1 ...`); `render` builds the JSON body
/// only for a matching GET/HEAD. `None` head = an oversize or unreadable request.
pub fn respond(head: Option<&str>, render: impl FnOnce() -> String) -> (Response, bool) {
    let bad = || Response {
        status: 400,
        reason: "Bad Request",
        body: String::new(),
    };
    let Some(head) = head else {
        return (bad(), false);
    };
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return (bad(), false);
    };
    let path = target.split('?').next().unwrap_or(target);
    let head_only = method == "HEAD";
    if method != "GET" && !head_only {
        return (
            Response {
                status: 405,
                reason: "Method Not Allowed",
                body: String::new(),
            },
            false,
        );
    }
    if path != crate::cambox_video_stats::PATH {
        return (
            Response {
                status: 404,
                reason: "Not Found",
                body: String::new(),
            },
            false,
        );
    }
    (
        Response {
            status: 200,
            reason: "OK",
            body: render(),
        },
        head_only,
    )
}

/// One read that ends by `deadline`: `None` once the deadline has passed, the read failed or
/// timed out, or the peer closed.
fn read_by(stream: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> Option<usize> {
    let left = deadline.checked_duration_since(Instant::now())?;
    if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
        return None;
    }
    match stream.read(buf) {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

/// Read the request head (up to the blank line, CRLF or bare LF) by `deadline`; `None` when it
/// grows past [`MAX_REQUEST_BYTES`], misses the deadline, or the client closes before the blank
/// line.
fn read_head(stream: &mut TcpStream, deadline: Instant) -> Option<String> {
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    loop {
        if buf.len() > MAX_REQUEST_BYTES {
            return None;
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.windows(2).any(|w| w == b"\n\n") {
            return String::from_utf8(buf).ok();
        }
        let n = read_by(stream, &mut chunk, deadline)?;
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn serve_one(mut stream: TcpStream, render: &dyn Fn() -> String) -> std::io::Result<()> {
    let deadline = Instant::now() + CONNECTION_DEADLINE;
    stream.set_write_timeout(Some(CONNECTION_DEADLINE))?;
    let head = read_head(&mut stream, deadline);
    let (r, head_only) = respond(head.as_deref(), render);
    let content_type = if r.status == 200 {
        "application/json"
    } else {
        "text/plain"
    };
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        r.status,
        r.reason,
        r.body.len()
    );
    if !head_only {
        out.push_str(&r.body);
    }
    stream.write_all(out.as_bytes())?;
    stream.flush()?;
    // Close politely: a client still sending (an oversize request) would otherwise get a TCP
    // reset instead of the answer. The drain ends with the connection's deadline.
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut sink = [0u8; 1024];
    while read_by(&mut stream, &mut sink, deadline).is_some() {}
    Ok(())
}

/// Serve `listener` forever, one connection at a time, `render` building each 200 body. A failed
/// accept or a broken client is logged at debug level and the loop goes on.
pub fn serve(listener: TcpListener, render: impl Fn() -> String) {
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Err(e) = serve_one(stream, &render) {
                    tracing::debug!("video stats http: a client failed: {e} (issue 1540)");
                }
            }
            Err(e) => tracing::debug!("video stats http: accept failed: {e} (issue 1540)"),
        }
    }
}

/// Start the endpoint for this node: the port from [`crate::cambox_video_stats::PORT_ENV`]
/// (`0` = off, a bad value = one WARN and the default), each 200 body rendered from a fresh
/// snapshot of `counters` with `read_at` = now.
pub fn start_from_env(
    meta: crate::cambox_video_stats::NodeMeta,
    counters: std::sync::Arc<crate::cambox_video_stats::CamboxVideoCounters>,
) -> bool {
    use crate::cambox_video_stats as stats;
    let env = std::env::var(stats::PORT_ENV).ok();
    let port = match stats::port_from_env(env.as_deref()) {
        Ok(Some(p)) => p,
        Ok(None) => {
            tracing::info!("video stats http: off ({}=0) (issue 1540)", stats::PORT_ENV);
            return false;
        }
        Err(msg) => {
            tracing::warn!("video stats http: {msg}");
            stats::DEFAULT_PORT
        }
    };
    spawn(port, move || {
        stats::render_json(&meta, &counters.snapshot(), stats::now_unix_ms()).to_string()
    })
}

/// Bind `0.0.0.0:port` and serve it on a `video-stats-http` thread. A bind or spawn failure is
/// one WARN: the appliance keeps running without the endpoint.
pub fn spawn(port: u16, render: impl Fn() -> String + Send + 'static) -> bool {
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                "video stats http: cannot listen on :{port}: {e} -- no {} endpoint (issue 1540)",
                crate::cambox_video_stats::PATH
            );
            return false;
        }
    };
    match std::thread::Builder::new()
        .name("video-stats-http".into())
        .spawn(move || serve(listener, render))
    {
        Ok(_) => {
            tracing::info!(
                "video stats http: serving GET :{port}{} (issue 1540)",
                crate::cambox_video_stats::PATH
            );
            true
        }
        Err(e) => {
            tracing::warn!("video stats http: thread spawn failed: {e} (issue 1540)");
            false
        }
    }
}
