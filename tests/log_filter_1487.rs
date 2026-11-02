//! Issue 1487: the camera-box binary's log filter passes every crate that logs for it. A target
//! is a module path, so a line from `rig_core::affinity` never matches a bare `camera_box`
//! directive: the pin and realtime failure WARNs would leave the journal with no error.

#![cfg(target_os = "linux")]

use std::io::Write;
use std::sync::{Arc, Mutex};

use tracing_subscriber::EnvFilter;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What the binary's real filter lets through while `run` logs.
fn logged_through_the_filter(debug: bool, run: impl FnOnce()) -> String {
    let out = Captured::default();
    let writer = out.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(camera_box::log_filter::directives(debug)))
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, run);
    let text = out.0.lock().unwrap().clone();
    String::from_utf8(text).unwrap()
}

#[test]
fn an_affinity_line_reaches_the_journal_at_both_levels() {
    for debug in [false, true] {
        // Logs one INFO or WARN line whatever the box's cores allow.
        let text = logged_through_the_filter(debug, || {
            camera_box::affinity::pin_off_capture_core("log-filter-test");
        });
        assert!(
            text.contains("log-filter-test thread"),
            "debug={debug}: the affinity line was filtered out: {text:?}"
        );
    }
}

#[test]
fn a_jpeg_xs_target_passes_too() {
    for debug in [false, true] {
        let text = logged_through_the_filter(debug, || {
            tracing::warn!(target: "camera_box_jxs::jxs_send", "jxs line");
        });
        assert!(text.contains("jxs line"), "debug={debug}: {text:?}");
    }
}

#[test]
fn the_binary_uses_these_directives() {
    let main =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs")).unwrap();
    assert!(main.contains("EnvFilter::new(camera_box::log_filter::directives(args.debug))"));
    assert!(!main.contains("EnvFilter::new(\"camera_box"));
}
