//! Issue 1381 — the av-sync dock's camera-box audio decode runs on its own worker, and only while it
//! measures something.
//!
//! Live 27.9.2026 on the resolume cg OBS the dock's `raw_audio` callback, which runs on libobs's
//! AUDIO thread, re-decoded the marker window of the whole program mix on every push. With music on
//! program the mixer fell 13-22 s behind real time, and the FOH VBAN feed turned into silence and
//! dropped audio. `cb_mode_active` had been latched by ONE burn QR from a CG_CHAIN E2E run hours
//! earlier and was never cleared. The fix, pinned here and by the pwsh step "Assert dock audio
//! decode runs off the audio thread (issue 1381)" in BOTH windows-genlock workflows:
//! - the audio callback only runs the gate and copies the block into a bounded FIFO
//!   (`camera-box-audio-worker.hpp`); the decode, the pairing and the diag tick run on the worker;
//! - the gate opens only while a camera-box QR was decoded within
//!   `CAMERA_BOX_TEST_SIGNAL_FRESH_NS`, and only on a box that has the measurement source `mbc`,
//!   whose presence is read on the video decode worker, never on the audio thread;
//! - a full FIFO drops the block, counts it, and the worker resets the marker decoders before the
//!   next block, so a gap never stitches into a false marker;
//! - the worker is started before data capture and joined on stop / destroy / in the destructor;
//! - the diag line appends `decode_ms_max= decode_ms_sum= audio_dropped= decode_resets=
//!   audio_publish_max_us=`.
//!
//! The FIFO policy itself is proven by the g++ self-test `vendor/av-sync-dock/test/
//! audio-worker-selftest.cpp`, which this file compiles and runs.

use std::path::PathBuf;
use std::process::Command;

#[path = "support/cpp_source.rs"]
mod cpp_source;
use cpp_source::{body_of, squish, strip_cpp_comments};

const DOCK_OUTPUT: &str = "vendor/av-sync-dock/src/sync-test-output.cpp";
const SELFTEST: &str = "vendor/av-sync-dock/test/audio-worker-selftest.cpp";
const AUDIO_HEADER: &str = "vendor/av-sync-dock/src/camera-box-audio.hpp";
const DOCK_UI: &str = "vendor/av-sync-dock/src/sync-test-dock.cpp";

fn manifest(rel: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), rel].iter().collect()
}

fn code() -> String {
    let p = manifest(DOCK_OUTPUT);
    let src =
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
    squish(&strip_cpp_comments(&src))
}

#[test]
fn audio_worker_selftest_passes() {
    let src = manifest(SELFTEST);
    assert!(
        src.exists(),
        "the audio worker self-test must exist: {}",
        src.display()
    );
    let dir =
        std::env::temp_dir().join(format!("audio-worker-selftest-1381-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the scratch dir");
    let bin = dir.join("selftest");
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
        .arg(&bin)
        .output()
        .expect("spawn g++ (install build-essential) for the audio worker self-test");
    let run = compile.status.success().then(|| {
        Command::new(&bin)
            .output()
            .expect("run the compiled audio worker self-test")
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        compile.status.success(),
        "camera-box-audio-worker.hpp + its self-test must compile clean \
         (-std=c++11 -Wall -Wextra -Werror -pthread):\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = run.expect("ran");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("ALL PASS"),
        "issue 1381: the audio FIFO must keep the audio thread free of the decode, keep blocks in \
         order, drop + count + reset on overflow (never a false marker across a gap) and end a \
         session only after its last block. Output:\n{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// The callback libobs runs on its audio thread does the gate and a copy, nothing else, in
/// camera-box mode.
#[test]
fn the_audio_callback_only_gates_and_copies() {
    let src = code();
    assert!(src.contains("#include \"camera-box-audio-worker.hpp\""));
    assert!(src.contains("camerabox::CbAudioBlockFifo cb_audio_fifo;"));
    let cb = body_of(
        &src,
        "static void st_raw_audio(void *data, struct audio_data *frames)",
    );
    assert!(
        cb.contains(
            "cb_active = st->cb_mode_active; last_qr_ns = st->cb_video_last_decode_ts_ns; } \
             if (cb_active) { cb_audio_gate_and_publish(st, frames, last_qr_ns); return; }"
        ),
        "{DOCK_OUTPUT}: st_raw_audio must hand camera-box mode to the gate + FIFO copy and return"
    );
    let publish = body_of(
        &src,
        "static void cb_audio_gate_and_publish(struct sync_test_output *st, const struct audio_data *frames, uint64_t last_qr_ns)",
    );
    for need in [
        "const camerabox::CbAudioGate gate = camerabox::cb_audio_decode_gate( true, st->cb_measure_source_present.load(std::memory_order_relaxed), now, last_qr_ns, CAMERA_BOX_TEST_SIGNAL_FRESH_NS);",
        "if (gate == camerabox::CbAudioGate::Open) {",
        "st->cb_audio_fifo.publish(planes, nch, frames->frames, frames->timestamp);",
        "st->cb_audio_fifo.end_session((unsigned)gate);",
        "camerabox::cb_atomic_max_u64(st->cb_audio_publish_max_ns, os_gettime_ns() - publish_start_ns);",
    ] {
        assert!(
            publish.contains(need),
            "{DOCK_OUTPUT}: cb_audio_gate_and_publish no longer has `{need}` (issue 1381)"
        );
    }
    for (name, body) in [("st_raw_audio", cb), ("cb_audio_gate_and_publish", publish)] {
        for banned in [
            "st_raw_audio_camera_box(",
            "->push(",
            "cb_audio_diag_tick(",
            "cb_ensure_audio_picker(",
            "signal_handler_signal(",
            "obs_get_source_by_name(",
            "blog(",
        ] {
            assert!(
                !body.contains(banned),
                "{DOCK_OUTPUT}: {name} runs `{banned}` on libobs's audio thread again (issue 1381) \
                 -- the mixer falls behind real time"
            );
        }
    }
}

/// The worker runs the unchanged camera-box decode on the copied block and resets the decoders at
/// every gap; ending a session unlocks the dock and shows STALE.
#[test]
fn the_audio_worker_decodes_the_copy_and_resets_on_a_gap() {
    let src = code();
    let run = body_of(
        &src,
        "static void st_audio_block_run(struct sync_test_output *st, const camerabox::CbAudioBlock &block)",
    );
    for need in [
        "frames.data[c] = (uint8_t *)block.planes[c].data();",
        "frames.frames = (uint32_t)block.frames; frames.timestamp = block.timestamp;",
        "st_raw_audio_camera_box(st, &frames);",
    ] {
        assert!(
            run.contains(need),
            "{DOCK_OUTPUT}: st_audio_block_run no longer has `{need}` (issue 1381)"
        );
    }
    let gap = body_of(
        &src,
        "static void st_audio_block_gap(struct sync_test_output *st, const camerabox::CbAudioBlock &block)",
    );
    for need in [
        "if (st->cb_audio_dec) st->cb_audio_dec->reset_window();",
        "if (block.gap & camerabox::CB_AUDIO_GAP_SESSION) cb_audio_session_begin(st);",
    ] {
        assert!(
            gap.contains(need),
            "{DOCK_OUTPUT}: st_audio_block_gap no longer has `{need}` (issue 1381) -- a gap could \
             stitch two stretches of audio into a false marker"
        );
    }
    let forget = body_of(
        &src,
        "static void cb_audio_forget_lock(struct sync_test_output *st)",
    );
    for need in [
        "st->cb_lock_audit = camerabox::CbLockAuditTracker();",
        "signal_lock_state_changed(st->context, false);",
    ] {
        assert!(
            forget.contains(need),
            "{DOCK_OUTPUT}: cb_audio_forget_lock no longer has `{need}` (issue 1381)"
        );
    }
    let recovery = body_of(
        &src,
        "static void cb_apply_pairing_recovery(struct sync_test_output *st, const camerabox::CbDockPairingRecovery &rec)",
    );
    assert!(
        recovery.contains("cb_audio_forget_lock(st);"),
        "{DOCK_OUTPUT}: the dead-pairing recovery forgets the lock through the one helper (issue 1381)"
    );
    let end = body_of(
        &src,
        "static void st_audio_session_end(struct sync_test_output *st, unsigned reason)",
    );
    for need in [
        "if (st->cb_audio_dec) st->cb_audio_dec->reset_window();",
        "cb_audio_forget_lock(st);",
        "signal_stale_changed(st->context, true);",
        "st->cb_audio_paused = true;",
    ] {
        assert!(
            end.contains(need),
            "{DOCK_OUTPUT}: st_audio_session_end no longer has `{need}` (issue 1381)"
        );
    }
    let begin = body_of(
        &src,
        "static void cb_audio_session_begin(struct sync_test_output *st)",
    );
    // A session also begins after an output restart, which discards a session end the worker had
    // not reached: the lock is forgotten here too (review round 1).
    for need in [
        "st->cb_input_staleness = camerabox::CbDockInputStaleness();",
        "st->cb_pairing_watchdog = camerabox::CbDockPairingWatchdog();",
        "cb_audio_forget_lock(st);",
        "signal_stale_changed(st->context, false);",
    ] {
        assert!(
            begin.contains(need),
            "{DOCK_OUTPUT}: cb_audio_session_begin no longer has `{need}` (issue 1381)"
        );
    }
}

#[test]
fn audio_worker_lifecycle_follows_the_output() {
    let src = code();
    let start = body_of(&src, "static bool st_start(void *data)");
    let stop_first = start
        .find("st->cb_audio_fifo.stop();")
        .expect("st_start joins a previous audio worker");
    let resize = start.find("quirc_resize(").expect("quirc_resize");
    assert!(
        stop_first < resize,
        "st_start must join the audio worker before it rewrites state"
    );
    let begin_capture = start
        .find("obs_output_begin_data_capture(")
        .expect("begin capture");
    let started = start
        .find("st->cb_audio_fifo.start(")
        .expect("st_start starts the audio worker");
    assert!(
        started < begin_capture,
        "the audio worker must run before the first raw_audio can arrive"
    );
    assert!(start.contains("st_audio_worker_thread_setup;"));
    let stop = body_of(&src, "static void st_stop(void *data, uint64_t)");
    assert!(
        stop.contains(
            "obs_output_end_data_capture(st->context); st->cb_decode_mailbox.stop(); st->cb_audio_fifo.stop();"
        ),
        "st_stop must join the audio worker after ending data capture"
    );
    let destroy = body_of(&src, "static void st_destroy(void *data)");
    let d_stop = destroy
        .find("st->cb_audio_fifo.stop();")
        .expect("destroy joins");
    let d_del = destroy.find("delete st;").expect("delete");
    assert!(d_stop < d_del);
    let dtor = body_of(&src, "~sync_test_output()");
    let t_stop = dtor.find("cb_audio_fifo.stop();").expect("dtor joins");
    let t_del = dtor.find("delete cb_audio_dec;").expect("dtor frees");
    assert!(
        t_stop < t_del,
        "the destructor must join the audio worker before freeing the picker it decodes with"
    );
}

/// The `mbc` presence check takes the sources mutex, so it runs on the video decode worker, rate
/// limited, and the audio thread only reads an atomic. The name is declared once, in
/// `camera-box-audio.hpp`, which both the output and the dock UI (its ASRC section) include.
#[test]
fn measurement_source_presence_is_read_off_the_audio_thread() {
    let read = |rel: &str| {
        let p = manifest(rel);
        let s = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
        squish(&strip_cpp_comments(&s))
    };
    assert!(
        read(AUDIO_HEADER).contains("#define CAMERA_BOX_MEASURE_SOURCE_NAME \"mbc\""),
        "{AUDIO_HEADER}: the measurement source name is declared here (issue 1381)"
    );
    assert!(
        read(DOCK_UI)
            .contains("#define CAMERA_BOX_ASRC_SOURCE_NAME CAMERA_BOX_MEASURE_SOURCE_NAME"),
        "{DOCK_UI}: the ASRC section names the same measurement source (issue 1381)"
    );
    let src = code();
    assert!(
        !src.contains("#define CAMERA_BOX_MEASURE_SOURCE_NAME"),
        "{DOCK_OUTPUT}: the measurement source name has one declaration, in {AUDIO_HEADER}"
    );
    assert!(src.contains("std::atomic<bool> cb_measure_source_present{false};"));
    let record = body_of(
        &src,
        "static void cb_video_qr_record(struct sync_test_output *st, uint32_t frame_id, uint64_t video_ts)",
    );
    assert!(
        record.contains("cb_refresh_measure_source(st, video_ts);"),
        "{DOCK_OUTPUT}: the QR record (decode worker) must refresh the measurement-source check"
    );
    let refresh = body_of(
        &src,
        "static void cb_refresh_measure_source(struct sync_test_output *st, uint64_t video_ts)",
    );
    for need in [
        "video_ts - st->cb_measure_source_check_ts < CAMERA_BOX_MEASURE_SOURCE_RECHECK_NS",
        "obs_source_t *src = obs_get_source_by_name(CAMERA_BOX_MEASURE_SOURCE_NAME);",
        "st->cb_measure_source_present.exchange(present);",
    ] {
        assert!(
            refresh.contains(need),
            "{DOCK_OUTPUT}: cb_refresh_measure_source no longer has `{need}` (issue 1381)"
        );
    }
}

#[test]
fn the_diag_line_reports_the_audio_worker() {
    let src = code();
    assert!(
        src.contains(
            "channel_switches=%llu\" \" decode_ms_max=%.3f decode_ms_sum=%.1f audio_dropped=%llu \
             decode_resets=%llu audio_publish_max_us=%llu\""
        ),
        "{DOCK_OUTPUT}: the diag line must append `decode_ms_max= decode_ms_sum= audio_dropped= \
         decode_resets= audio_publish_max_us=` after the existing tokens (issue 1381)"
    );
    assert!(src.contains(
        "(unsigned long long)st->cb_switch_log.total, (double)st->cb_audio_fifo.take_process_max_ns() / 1e6, \
         (double)st->cb_audio_fifo.take_process_sum_ns() / 1e6, (unsigned long long)st->cb_audio_fifo.dropped(), \
         (unsigned long long)st->cb_audio_fifo.resets(), (unsigned long long)(st->cb_audio_publish_max_ns.exchange(0) / 1000));"
    ));
}
