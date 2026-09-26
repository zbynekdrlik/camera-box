//! Issue 1367 — the A/V-sync dock's per-frame video decode must never run on libobs's
//! video-output thread.
//!
//! Live 26.9.2026 on the resolume cg OBS (build afd7cc184): `st_raw_video` in
//! `vendor/av-sync-dock/src/sync-test-output.cpp` ran the camera-box top-band gather + up to two
//! quirc passes (and norihiro's whole-frame quirc + marker search outside camera-box mode)
//! synchronously in the `raw_video` callback, which libobs calls on its ONE video-output thread.
//! With QR content on screen OBS skipped 28 % of output frames for every raw output on the box
//! (the cg-obs NDI output fell to 16-18.7 fps); stopping `sync-test-output` restored 30.0 fps.
//!
//! The fix (design 5846477682, Approach 1): `st_raw_video` only copies what the decoders read into
//! a two-buffer latest-pending mailbox (`camera-box-decode-mailbox.hpp`) and a worker thread runs
//! the unchanged decoders on the copy, with the frame's own timestamp.
//!
//! Two gates here. `mailbox_policy_selftest_passes` compiles and runs the dependency-free g++
//! self-test (`vendor/av-sync-dock/test/decode-mailbox-selftest.cpp`): a 50 ms fake decode never
//! blocks the producer for more than 2 ms, the latest frame wins, dropped frames are counted, and
//! stop/destroy join an in-flight decode.
//!
//! The source-anchor checks prove `sync-test-output.cpp` is WIRED to it: the callback only
//! publishes, the worker starts on output start and is stopped on stop/destroy, and the diag line
//! reports `decode_dropped`. The dock compiles ONLY on the Windows runner, so these are ALSO
//! mirrored by the pwsh step "Assert dock decode runs off the video-output thread" in BOTH
//! windows-genlock workflows (`.claude/rules/av-sync-dock-anchor-refactor-safety.md`).

use std::path::PathBuf;
use std::process::Command;

const DOCK_OUTPUT: &str = "vendor/av-sync-dock/src/sync-test-output.cpp";

fn manifest(rel: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), rel].iter().collect()
}

fn vendor_file(rel: &str) -> String {
    let p = manifest(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop `/* ... */` and `// ...` comments so a prose mention of a function name never satisfies
/// or breaks an anchor (the source has no comment markers inside string literals it matters for).
fn strip_cpp_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
        } else if c == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            let end = s[i + 2..]
                .find("*/")
                .map(|e| i + 2 + e + 2)
                .unwrap_or(b.len());
            out.push(' ');
            i = end;
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            let end = s[i..].find('\n').map(|e| i + e).unwrap_or(b.len());
            i = end;
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

/// The comment-stripped, whitespace-squished source.
fn code() -> String {
    squish(&strip_cpp_comments(&vendor_file(DOCK_OUTPUT)))
}

/// The balanced-brace body of the first function whose signature starts with `sig`.
fn body_of<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("{DOCK_OUTPUT}: `{sig}` not found"));
    let open = start + src[start..].find('{').expect("function body opening brace");
    let mut depth = 0usize;
    for (off, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..=open + off];
                }
            }
            _ => {}
        }
    }
    panic!("{DOCK_OUTPUT}: unbalanced body for `{sig}`");
}

#[test]
fn mailbox_policy_selftest_passes() {
    let src = manifest("vendor/av-sync-dock/test/decode-mailbox-selftest.cpp");
    assert!(
        src.exists(),
        "the mailbox self-test must exist: {}",
        src.display()
    );
    let out_bin = std::env::temp_dir().join(format!(
        "decode-mailbox-selftest-{}-{}",
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    ));
    let compile = Command::new("g++")
        .args([
            "-std=c++11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-pthread",
        ])
        .arg(&src)
        .arg("-o")
        .arg(&out_bin)
        .output()
        .expect(
            "spawn g++ (install build-essential) — the mailbox self-test needs a C++11 compiler",
        );
    assert!(
        compile.status.success(),
        "camera-box-decode-mailbox.hpp + its self-test must compile clean \
         (-std=c++11 -Wall -Wextra -Werror -pthread):\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out_bin)
        .output()
        .expect("run the compiled decode-mailbox self-test");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let _ = std::fs::remove_file(&out_bin);
    assert!(
        run.status.success() && stdout.contains("ALL PASS"),
        "issue 1367: the decode mailbox must keep the video thread free of the decode (a 50 ms \
         decode never blocks the producer > 2 ms), let the latest frame win and count drops. \
         Output:\n{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn raw_video_callback_only_publishes_the_copy() {
    let src = code();
    assert!(
        src.contains("#include \"camera-box-decode-mailbox.hpp\""),
        "issue 1367: sync-test-output.cpp must include camera-box-decode-mailbox.hpp"
    );
    assert!(
        src.contains("camerabox::CbDecodeMailbox<st_video_decode_job> cb_decode_mailbox;"),
        "issue 1367: the output must own the decode mailbox"
    );
    assert!(
        src.contains("#include \"camera-box-frame-copy.hpp\""),
        "issue 1367: the frame copies must come from the self-tested camera-box-frame-copy.hpp"
    );
    let body = body_of(
        &src,
        "static void st_raw_video(void *data, struct video_data *frame)",
    );
    assert!(
        body.contains("st->cb_decode_mailbox.publish("),
        "issue 1367: st_raw_video must hand the frame copy to the decode worker"
    );
    assert!(
        body.contains("job.timestamp = frame->timestamp;"),
        "issue 1367: the job must carry the frame's own timestamp"
    );
    assert!(
        body.contains(
            "camerabox::cb_atomic_max_u64(st->cb_publish_max_ns, os_gettime_ns() - publish_start_ns);"
        ),
        "issue 1367: st_raw_video must record its own cost for the diag line's publish_max_us"
    );
    for banned in [
        "st_raw_video_camera_box_decode(",
        "st_raw_video_qrcode_decode(",
        "st_raw_video_find_marker(",
        "st_video_decode_job_run(",
        "video_marker_found(",
        "cb_video_qr_record(",
        "quirc_",
        "signal_handler_signal(",
    ] {
        assert!(
            !body.contains(banned),
            "issue 1367: st_raw_video (libobs's video-output thread) must not run `{banned}` — \
             a decode there makes OBS skip output frames for every raw output on the box"
        );
    }
}

#[test]
fn worker_runs_the_unchanged_decoders_on_the_copy() {
    let src = code();
    let body = body_of(
        &src,
        "static void st_video_decode_job_run(struct sync_test_output *st, st_video_decode_job &job)",
    );
    for call in [
        "st_raw_video_camera_box_decode(st, job.band.data(), job.timestamp)",
        "st_raw_video_qrcode_decode(st, job.grid.data(), job.timestamp)",
        "st_raw_video_find_marker(st, job)",
    ] {
        assert!(
            body.contains(call),
            "issue 1367: the decode worker must run `{call}` on the copied frame with its own \
             timestamp"
        );
    }
}

#[test]
fn worker_lifecycle_follows_the_output() {
    let src = code();
    let start = body_of(&src, "static bool st_start(void *data)");
    let restop_at = start
        .find("st->cb_decode_mailbox.stop();")
        .expect("issue 1367: st_start must first stop a worker left from a previous start");
    let resize_at = start
        .find("quirc_resize(")
        .expect("st_start resizes the quirc context");
    assert!(
        restop_at < resize_at,
        "issue 1367: no worker may be running while st_start rewrites the state it decodes with"
    );
    assert!(
        start.contains("st_decode_worker_thread_setup)"),
        "issue 1367: the worker thread must be named and lowered below normal priority"
    );
    let start_at = start
        .find("st->cb_decode_mailbox.start(")
        .expect("issue 1367: st_start must start the decode worker");
    let capture_at = start
        .find("obs_output_begin_data_capture(")
        .expect("st_start begins data capture");
    assert!(
        start_at < capture_at,
        "issue 1367: the decode worker must be running before the video callbacks start"
    );

    let stop = body_of(&src, "static void st_stop(void *data, uint64_t)");
    assert!(
        stop.contains("obs_output_end_data_capture(st->context); st->cb_decode_mailbox.stop();"),
        "issue 1367: st_stop must stop + join the decode worker after ending data capture"
    );

    let destroy = body_of(&src, "static void st_destroy(void *data)");
    let stop_at = destroy
        .find("st->cb_decode_mailbox.stop();")
        .expect("issue 1367: st_destroy must stop + join the decode worker");
    let delete_at = destroy
        .find("delete st;")
        .expect("st_destroy deletes the output");
    assert!(
        stop_at < delete_at,
        "issue 1367: the worker must be joined before the output (its quirc contexts) is freed"
    );

    let dtor = body_of(&src, "~sync_test_output()");
    let dstop = dtor
        .find("cb_decode_mailbox.stop();")
        .expect("issue 1367: ~sync_test_output must join the worker first");
    let dquirc = dtor
        .find("quirc_destroy(")
        .expect("the destructor frees quirc");
    assert!(
        dstop < dquirc,
        "issue 1367: the worker must be joined before quirc_destroy frees what it decodes with"
    );
}

#[test]
fn diag_line_reports_dropped_decodes() {
    let src = code();
    assert!(
        src.contains(
            "ring_hit=%llu ring_miss=%llu locked=%s state=%s decode_dropped=%llu publish_max_us=%llu"
        ),
        "issue 1367: the dock diag line must append decode_dropped=%llu publish_max_us=%llu \
         (existing tokens unchanged)"
    );
    assert!(
        src.contains("st->cb_publish_max_ns.exchange(0) / 1000"),
        "issue 1367: publish_max_us must be the per-window max of st_raw_video's own cost"
    );
    assert!(
        src.contains("st->cb_decode_mailbox.dropped()"),
        "issue 1367: decode_dropped must be the mailbox's own dropped-frame counter"
    );
}
