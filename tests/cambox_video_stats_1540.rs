//! Issue 1540 slice 2: the cambox serves its cumulative video counters on `:8896/video.json`
//! (schema 1 of the dcontroller draft). Default features: the counters, the schema mapping and
//! the std HTTP endpoint through a real socket.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use camera_box::cambox_video_http;
use camera_box::cambox_video_stats::{
    port_from_env, render_json, rfc3339_utc, run_id, CamboxVideoCounters, CaptureWindow,
    CounterSnapshot, JxsWindow, NodeMeta, DEFAULT_PORT, PATH, SCHEMA,
};
use serde_json::Value;

fn meta(jxs: Option<&str>) -> NodeMeta {
    NodeMeta {
        node: "cam2".into(),
        ndi_name: "CAM2 (usb)".into(),
        jxs_target: jxs.map(str::to_string),
        // 2026-11-20T02:49:10Z
        run_start_unix_ms: 1_795_142_950_000,
        run_id: Some("b0a1:12345".into()),
        version: "1.8.17".into(),
    }
}

fn window(captured: u64, sent: u64) -> CaptureWindow {
    CaptureWindow {
        captured,
        sent,
        capture_dropped_total: 0,
        corrupted_total: 0,
        starvation_repeats: 0,
        blind_shed: 0,
    }
}

#[test]
fn capture_windows_accumulate_and_the_loss_term_is_never_negative() {
    let c = CamboxVideoCounters::default();
    c.note_capture_window(&CaptureWindow {
        captured: 300,
        sent: 300,
        capture_dropped_total: 2,
        corrupted_total: 1,
        starvation_repeats: 1,
        blind_shed: 0,
    });
    // an over-rate window (captured more than sent) is no loss
    c.note_capture_window(&CaptureWindow {
        captured: 310,
        sent: 300,
        capture_dropped_total: 2,
        corrupted_total: 1,
        starvation_repeats: 0,
        blind_shed: 2,
    });
    // a window that sent 4 frames the device never delivered
    c.note_capture_window(&CaptureWindow {
        captured: 296,
        sent: 300,
        capture_dropped_total: 5,
        corrupted_total: 3,
        starvation_repeats: 4,
        blind_shed: 1,
    });
    let s = c.snapshot();
    assert_eq!(s.capture_frames, 906);
    assert_eq!(s.emit_frames, 900);
    assert_eq!(s.capture_lost, 4);
    // the device counters are cumulative: the newest value, never summed
    assert_eq!(s.capture_dropped, 5);
    assert_eq!(s.capture_corrupted, 3);
    assert_eq!(s.ndi_duplicated, 5);
    assert_eq!(s.ndi_skipped, 3);
}

#[test]
fn jxs_windows_sum_and_a_skipped_sequence_is_counted_unread() {
    let c = CamboxVideoCounters::default();
    let w = JxsWindow {
        frames: 150,
        slices: 2400,
        bytes: 9_000_000,
        dropped_busy: 1,
        send_errors: 0,
        encode_errors: 0,
        slot_fills: 2,
        fec_sent: 300,
        fec_repaired: 1,
        bpp: 3,
        pace_mbps: 400,
        encode_ms_sum: 150.0 * 6.5,
    };
    c.note_jxs_window(1, &w);
    c.note_jxs_window(2, &w);
    // windows 3 and 4 were published and replaced before the capture loop read them
    c.note_jxs_window(
        5,
        &JxsWindow {
            bpp: 4,
            encode_ms_sum: 150.0 * 7.25,
            ..w
        },
    );
    let s = c.snapshot();
    assert_eq!(s.jxs_windows, 3);
    assert_eq!(s.jxs_windows_unread, 2);
    assert_eq!(s.jxs_frames, 450);
    assert_eq!(s.jxs_slices, 7200);
    assert_eq!(s.jxs_bytes, 27_000_000);
    assert_eq!(s.jxs_dropped_busy, 3);
    assert_eq!(s.jxs_slot_fills, 6);
    assert_eq!(s.jxs_fec_sent, 900);
    assert_eq!(s.jxs_fec_repaired, 3);
    // the newest window's values
    assert_eq!(s.jxs_bpp, Some(4));
    assert_eq!(s.jxs_pace_mbps, Some(400));
    assert_eq!(s.jxs_latency_us, Some(7250));
}

#[test]
fn a_frameless_jxs_window_keeps_the_last_latency() {
    let c = CamboxVideoCounters::default();
    assert_eq!(c.snapshot().jxs_latency_us, None);
    c.note_jxs_window(
        1,
        &JxsWindow {
            frames: 10,
            encode_ms_sum: 50.0,
            ..JxsWindow::default()
        },
    );
    c.note_jxs_window(2, &JxsWindow::default());
    assert_eq!(c.snapshot().jxs_latency_us, Some(5000));
}

#[test]
fn the_link_up_time_is_the_first_one() {
    let c = CamboxVideoCounters::default();
    assert_eq!(c.snapshot().jxs_connected_at_ms, None);
    c.note_jxs_connected(1_000);
    c.note_jxs_connected(2_000);
    assert_eq!(c.snapshot().jxs_connected_at_ms, Some(1_000));
}

#[test]
fn rfc3339_is_utc_seconds() {
    assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339_utc(1_795_142_950), "2026-11-20T02:49:10Z");
    // a leap day and the last second of a year
    assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(rfc3339_utc(1_798_761_599), "2026-12-31T23:59:59Z");
}

#[test]
fn run_id_is_boot_id_and_start_ticks() {
    // field 22 (starttime) = 12345; the command name holds a space and a parenthesis
    let stat = "4242 (camera (box) x) S 1 4242 4242 0 -1 4194560 100 0 0 0 5 6 0 0 -11 0 9 0 12345 1000 200 18446744073709551615";
    assert_eq!(run_id("3f1c-aa\n", stat).as_deref(), Some("3f1c-aa:12345"));
    assert_eq!(run_id("", stat), None);
    assert_eq!(run_id("x", "4242 (cb) S 1 2"), None);
    assert_eq!(run_id("x", "no parenthesis"), None);
}

#[test]
fn port_env_default_off_and_bad() {
    assert_eq!(DEFAULT_PORT, 8896);
    assert_eq!(port_from_env(None), Ok(Some(8896)));
    assert_eq!(port_from_env(Some(" ")), Ok(Some(8896)));
    assert_eq!(port_from_env(Some("0")), Ok(None));
    assert_eq!(port_from_env(Some("9000")), Ok(Some(9000)));
    assert!(port_from_env(Some("x")).is_err());
}

#[test]
fn the_document_carries_the_draft_fields() {
    let c = CamboxVideoCounters::default();
    c.note_capture_window(&CaptureWindow {
        captured: 296,
        sent: 300,
        capture_dropped_total: 5,
        corrupted_total: 3,
        starvation_repeats: 4,
        blind_shed: 1,
    });
    c.note_jxs_connected(1_795_142_952_000);
    c.note_jxs_window(
        1,
        &JxsWindow {
            frames: 150,
            dropped_busy: 2,
            slices: 2400,
            encode_ms_sum: 150.0 * 6.5,
            bpp: 3,
            ..JxsWindow::default()
        },
    );
    let v = render_json(
        &meta(Some("10.77.9.202:5004")),
        &c.snapshot(),
        1_795_143_005_000,
    );
    assert_eq!(v["schema"], SCHEMA);
    assert_eq!(v["node"], "cam2");
    assert_eq!(v["kind"], "cambox");
    assert_eq!(v["version"], "1.8.17");
    assert_eq!(v["read_at"], "2026-11-20T02:50:05Z");
    assert_eq!(v["run_start"], "2026-11-20T02:49:10Z");
    assert_eq!(v["run_id"], "b0a1:12345");
    assert_eq!(v["counters_since"], "2026-11-20T02:49:10Z");
    assert_eq!(v["capture_frames"], 296);
    assert_eq!(v["capture_lost"], 4);
    assert_eq!(v["capture_dropped"], 5);
    assert_eq!(v["capture_corrupted"], 3);
    assert_eq!(v["emit_frames"], 300);
    assert_eq!(v["send_dropped_busy"], 2);

    let conns = v["connections"].as_array().expect("connections");
    assert_eq!(conns.len(), 2);
    let ndi = &conns[0];
    assert_eq!(ndi["id"], "ndi:cam2->CAM2 (usb)");
    assert_eq!(ndi["kind"], "ndi");
    assert_eq!(ndi["direction"], "out");
    assert_eq!(ndi["source"], "cam2");
    assert_eq!(ndi["sink"], Value::Null);
    assert_eq!(ndi["stream_name"], "CAM2 (usb)");
    assert_eq!(ndi["latency_ms"], Value::Null);
    assert_eq!(ndi["frames_sent"], 300);
    assert_eq!(ndi["frames_duplicated"], 4);
    assert_eq!(ndi["frames_skipped"], 1);
    assert_eq!(ndi["counters_since"], "2026-11-20T02:49:10Z");

    let jxs = &conns[1];
    assert_eq!(jxs["id"], "jxs:cam2->10.77.9.202:5004");
    assert_eq!(jxs["kind"], "jxs");
    assert_eq!(jxs["direction"], "out");
    assert_eq!(jxs["sink"], "10.77.9.202:5004");
    assert_eq!(jxs["connected"], true);
    assert_eq!(jxs["latency_ms"], 6.5);
    assert_eq!(jxs["frames_sent"], 150);
    assert_eq!(jxs["frames_dropped_busy"], 2);
    assert_eq!(jxs["slices_sent"], 2400);
    assert_eq!(jxs["bpp"], 3);
    assert_eq!(jxs["windows"], 1);
    assert_eq!(jxs["windows_unread"], 0);
    assert_eq!(jxs["counters_since"], "2026-11-20T02:49:12Z");
}

#[test]
fn without_a_jxs_target_only_the_ndi_output_is_listed_and_a_link_not_up_reads_unconnected() {
    let s = CounterSnapshot::default();
    let v = render_json(&meta(None), &s, 1_795_143_005_000);
    assert_eq!(v["connections"].as_array().map(Vec::len), Some(1));
    let v = render_json(&meta(Some("strih-snv:5004")), &s, 1_795_143_005_000);
    let jxs = &v["connections"][1];
    assert_eq!(jxs["connected"], false);
    assert_eq!(jxs["counters_since"], Value::Null);
    assert_eq!(jxs["latency_ms"], Value::Null);
    assert_eq!(jxs["bpp"], Value::Null);
}

fn request(port: u16, raw: &[u8]) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.write_all(raw).expect("write");
    let mut out = String::new();
    s.read_to_string(&mut out).expect("read");
    out
}

fn serve_on_a_free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let counters = std::sync::Arc::new(CamboxVideoCounters::default());
    counters.note_capture_window(&window(300, 300));
    let m = meta(None);
    std::thread::spawn(move || {
        cambox_video_http::serve(listener, move || {
            render_json(&m, &counters.snapshot(), 1_795_143_005_000).to_string()
        })
    });
    port
}

#[test]
fn the_endpoint_answers_get_with_the_document_and_refuses_the_rest() {
    let port = serve_on_a_free_port();

    let ok = request(
        port,
        format!("GET {PATH} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes(),
    );
    let (head, body) = ok.split_once("\r\n\r\n").expect("head + body");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(head.contains("Content-Type: application/json"), "{head}");
    assert!(
        head.contains(&format!("Content-Length: {}", body.len())),
        "{head}"
    );
    let v: Value = serde_json::from_str(body).expect("json");
    assert_eq!(v["kind"], "cambox");
    assert_eq!(v["capture_frames"], 300);

    // a query string is ignored
    let q = request(port, format!("GET {PATH}?x=1 HTTP/1.1\r\n\r\n").as_bytes());
    assert!(q.starts_with("HTTP/1.1 200 OK\r\n"), "{q}");

    let head_only = request(port, format!("HEAD {PATH} HTTP/1.1\r\n\r\n").as_bytes());
    assert!(head_only.starts_with("HTTP/1.1 200 OK\r\n"), "{head_only}");
    assert!(
        head_only.ends_with("\r\n\r\n"),
        "HEAD carries no body: {head_only}"
    );

    let missing = request(port, b"GET /bundle-state.json HTTP/1.1\r\n\r\n");
    assert!(missing.starts_with("HTTP/1.1 404 "), "{missing}");

    let post = request(port, format!("POST {PATH} HTTP/1.1\r\n\r\n").as_bytes());
    assert!(post.starts_with("HTTP/1.1 405 "), "{post}");

    let huge = format!("GET {PATH} HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(8192));
    let refused = request(port, huge.as_bytes());
    assert!(refused.starts_with("HTTP/1.1 400 "), "{refused}");

    // still serving after the refusals
    let again = request(port, format!("GET {PATH} HTTP/1.1\r\n\r\n").as_bytes());
    assert!(again.starts_with("HTTP/1.1 200 OK\r\n"), "{again}");
}
