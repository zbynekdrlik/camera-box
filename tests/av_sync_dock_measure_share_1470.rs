//! Issue 1470 — the av-sync dock decodes the QPSK marker only from the measurement source's OWN
//! share of the program mix, never from the whole track-1 mix.
//!
//! Live 24.10.2026 on stream-snv the dock read -900.9 ms while `recording-verdict --av-sync` read the
//! stream in sync (+24.5 ms): the program played the measurement clip (run 911016) next to `mbc`, and
//! the clip's digital marker (scatter 0-1 ms, more readings) out-clustered the room marker. An 8-bit
//! marker cannot tell two emitters apart, so (ROZHODNUTÉ 6381433364) the dock reads `mbc`'s own share:
//! - a callback connected with `obs_add_raw_audio_callback` runs after the mix of each tick and copies
//!   the measurement source's per-source mix buffer (`obs_source_get_audio_mix`) into the existing
//!   FIFO, only when `camera-box-measure-share.hpp` proves it is the whole tick at offset 0; any other
//!   tick is silence, never the mix;
//! - `st_raw_audio` (the output's whole, re-chunked mix) publishes nothing in camera-box mode;
//! - the source is the one the decode worker already looks up by `CAMERA_BOX_MEASURE_SOURCE_NAME`,
//!   now held for the audio thread; a source out of the program for a second ends the session
//!   (`CbAudioGate::NotInProgram`: STALE + one log line).
//!
//! The take rule and the two-emitter case end to end are the g++ self-test
//! `vendor/av-sync-dock/test/measure-share-selftest.cpp`, which this file compiles and runs. The
//! wiring is pinned here and by the pwsh step "Assert dock measures only the measurement source's
//! share (issue 1470)" in BOTH windows-genlock workflows.

use std::path::PathBuf;
use std::process::Command;

#[path = "support/cpp_source.rs"]
mod cpp_source;
use cpp_source::{squish, strip_cpp_comments, unique_body_of};
#[allow(dead_code)]
#[path = "support/av_sync_dock_output.rs"]
mod av_sync_dock_output;

const DOCK_OUTPUT: &str = av_sync_dock_output::LABEL;
const SELFTEST: &str = "vendor/av-sync-dock/test/measure-share-selftest.cpp";
const SHARE_HEADER: &str = "vendor/av-sync-dock/src/camera-box-measure-share.hpp";
const WORKER_HEADER: &str = "vendor/av-sync-dock/src/camera-box-audio-worker.hpp";
const CMAKE: &str = "vendor/av-sync-dock/CMakeLists.txt";

const MIX_TICK_SIG: &str =
    "void st_cb_mix_tick(void *param, size_t mix_idx, struct audio_data *mix)";
const HOLD_SIG: &str =
    "void cb_measure_source_hold(struct sync_test_output *st, obs_source_t *src)";
const CONNECT_SIG: &str = "void cb_measure_share_connect(struct sync_test_output *st)";
const DISCONNECT_SIG: &str = "void cb_measure_share_disconnect(struct sync_test_output *st)";

fn manifest(rel: &str) -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), rel].iter().collect()
}

fn read(rel: &str) -> String {
    let p = manifest(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn code() -> String {
    squish(&strip_cpp_comments(&av_sync_dock_output::source()))
}

/// `needles` appear in `body` in this order.
fn in_order(body: &str, name: &str, needles: &[&str]) {
    let mut at = 0usize;
    for need in needles {
        let found = body[at..].find(need).unwrap_or_else(|| {
            panic!(
                "{DOCK_OUTPUT}: {name} no longer has `{need}` after its previous step (issue 1470)"
            )
        });
        at += found + need.len();
    }
}

#[test]
fn measure_share_selftest_passes() {
    let src = manifest(SELFTEST);
    assert!(
        src.exists(),
        "the measure-share self-test must exist: {}",
        src.display()
    );
    let dir = tempfile::tempdir().expect("create the scratch dir");
    let bin = dir.path().join("selftest");
    let compile = Command::new("g++")
        .args(["-std=c++11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn g++ (install build-essential) for the measure-share self-test");
    let run = compile.status.success().then(|| {
        Command::new(&bin)
            .output()
            .expect("run the compiled measure-share self-test")
    });
    assert!(
        compile.status.success(),
        "camera-box-measure-share.hpp + its self-test must compile clean \
         (-std=c++11 -Wall -Wextra -Werror):\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = run.expect("ran");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("ALL PASS"),
        "issue 1470: the dock must decode only the measurement source's own share of the mix -- the \
         whole mix locks on the measurement clip, the share on the room marker, a source out of the \
         program reads absent. Output:\n{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// The output's own raw_audio (the whole mix, re-chunked off the mix ticks by libobs) never feeds
/// the camera-box decode.
#[test]
fn the_whole_mix_never_reaches_the_camera_box_decode() {
    let src = code();
    let cb = unique_body_of(
        &src,
        "void st_raw_audio(void *data, struct audio_data *frames)",
    );
    assert!(
        cb.contains("cb_active = st->cb_mode_active; } if (cb_active) { return; }"),
        "{DOCK_OUTPUT}: st_raw_audio must return in camera-box mode without publishing (issue 1470)"
    );
    for banned in [
        "cb_audio_gate_and_publish(",
        "cb_audio_fifo.publish(",
        "last_qr_ns",
    ] {
        assert!(
            !cb.contains(banned),
            "{DOCK_OUTPUT}: st_raw_audio still has `{banned}` -- the whole program mix (the \
             measurement clip's marker included) reaches the decode again (issue 1470)"
        );
    }
    assert_eq!(
        src.matches("cb_audio_gate_and_publish(st, ").count(),
        1,
        "{DOCK_OUTPUT}: the FIFO copy has ONE caller, the mix-tick callback (issue 1470)"
    );
    assert_eq!(
        src.matches("cb_audio_fifo.publish(").count(),
        1,
        "{DOCK_OUTPUT}: the FIFO has ONE producer, cb_audio_gate_and_publish (issue 1470)"
    );
}

/// The mix-tick callback copies the measurement source's own share (or silence), never the mix.
#[test]
fn the_mix_tick_takes_only_the_measurement_source_share() {
    let src = code();
    let tick = unique_body_of(&src, MIX_TICK_SIG);
    in_order(
        tick,
        "st_cb_mix_tick",
        &[
            "const uint64_t tick_cost_start_ns = os_gettime_ns();",
            "cb_active = st->cb_mode_active; last_qr_ns = st->cb_video_last_decode_ts_ns; } if (!cb_active) return;",
            "tick.tick_start_ns = mix->timestamp;",
            "tick.tick_end_ns = mix->timestamp + util_mul_div64(frames, 1000000000ULL, st->audio_sample_rate);",
            "std::lock_guard<std::mutex> lock(st->cb_measure_src_mutex);",
            "obs_source_t *src = st->cb_measure_src;",
            "if (src != st->cb_share_src_seen) { st->cb_share_tracker.reset(); st->cb_share_src_seen = src; }",
            "tick.source_found = src != nullptr;",
            "tick.source_active = src && obs_source_active(src);",
            "tick.source_pending = !src || obs_source_audio_pending(src);",
            "tick.source_ts_after_ns = src ? obs_source_get_audio_timestamp(src) : 0;",
            "decision = st->cb_share_tracker.observe(tick);",
            "if (decision.take) { struct obs_source_audio_mix m; obs_source_get_audio_mix(src, &m);",
            "share[c] = m.output[mix_idx].data[c];",
            "camerabox::cb_measure_share_fill(decision.take, share, nch, frames, out); }",
            "if (decision.absent_long && st->cb_measure_source_present.load(std::memory_order_relaxed)) { st->cb_audio_fifo.end_session((unsigned)camerabox::CbAudioGate::NotInProgram); camerabox::cb_atomic_max_u64(st->cb_audio_publish_max_ns, os_gettime_ns() - tick_cost_start_ns); return; }",
            "share_frames.data[c] = (uint8_t *)out[c];",
            "share_frames.frames = (uint32_t)frames; share_frames.timestamp = mix->timestamp;",
            "cb_audio_gate_and_publish(st, &share_frames, last_qr_ns); camerabox::cb_atomic_max_u64(st->cb_audio_publish_max_ns, os_gettime_ns() - tick_cost_start_ns);",
        ],
    );
    for banned in [
        "mix->data",
        "obs_get_source_by_name(",
        "obs_source_release(",
        "blog(",
        "->push(",
        "st_raw_audio_camera_box(",
        "signal_handler_signal(",
        ".assign(",
        ".resize(",
        "new ",
    ] {
        assert!(
            !tick.contains(banned),
            "{DOCK_OUTPUT}: st_cb_mix_tick runs `{banned}` on libobs's audio thread (issues 1381 / \
             1470) -- the mix samples, a lookup, a release, a log line, a decode or an allocation"
        );
    }
}

/// The measurement source is the one the decode worker already looks up by name, held for the audio
/// thread and released off the lock; the callback lives exactly as long as the decode.
#[test]
fn the_measurement_source_is_held_and_the_callback_follows_the_output() {
    let src = code();
    let refresh = unique_body_of(
        &src,
        "static void cb_refresh_measure_source(struct sync_test_output *st, uint64_t video_ts)",
    );
    in_order(
        refresh,
        "cb_refresh_measure_source",
        &[
            "obs_source_t *src = obs_get_source_by_name(CAMERA_BOX_MEASURE_SOURCE_NAME);",
            "const bool present = src != nullptr;",
            "cb_measure_source_hold(st, src);",
            "st->cb_measure_source_present.exchange(present);",
        ],
    );
    assert!(
        !refresh.contains("obs_source_release(src);"),
        "{DOCK_OUTPUT}: cb_refresh_measure_source releases the source it hands the audio thread"
    );
    assert_eq!(
        src.matches("obs_get_source_by_name(CAMERA_BOX_MEASURE_SOURCE_NAME)")
            .count(),
        1,
        "{DOCK_OUTPUT}: one lookup by CAMERA_BOX_MEASURE_SOURCE_NAME (issue 1470)"
    );
    assert!(
        !src.contains("\"mbc\""),
        "{DOCK_OUTPUT}: the measurement source is named by CAMERA_BOX_MEASURE_SOURCE_NAME only, \
         never a second hard-coded name (issue 1470)"
    );
    let hold = unique_body_of(&src, HOLD_SIG);
    assert!(
        hold.contains(
            "obs_source_t *old; { std::lock_guard<std::mutex> lock(st->cb_measure_src_mutex); old = st->cb_measure_src; st->cb_measure_src = src; } obs_source_release(old);"
        ),
        "{DOCK_OUTPUT}: cb_measure_source_hold must swap under the lock and release the old \
         reference after it (a last release never runs under the lock the audio thread takes)"
    );
    let connect = unique_body_of(&src, CONNECT_SIG);
    in_order(
        connect,
        "cb_measure_share_connect",
        &[
            "st->cb_share_planes[c].assign(AUDIO_OUTPUT_FRAMES, 0.0f);",
            "st->cb_share_tracker = camerabox::CbMeasureShareTracker();",
            "st->cb_share_src_seen = nullptr;",
            "st->cb_share_mix_idx = obs_output_get_mixer(st->context);",
            "obs_add_raw_audio_callback(st->cb_share_mix_idx, nullptr, st_cb_mix_tick, st);",
            "st->cb_share_connected = true;",
        ],
    );
    let disconnect = unique_body_of(&src, DISCONNECT_SIG);
    assert!(
        disconnect.contains(
            "if (!st->cb_share_connected) return; obs_remove_raw_audio_callback(st->cb_share_mix_idx, st_cb_mix_tick, st); st->cb_share_connected = false;"
        ),
        "{DOCK_OUTPUT}: cb_measure_share_disconnect must remove the callback once"
    );

    let start = unique_body_of(&src, "static bool st_start(void *data)");
    in_order(
        start,
        "st_start",
        &[
            "cb_measure_share_disconnect(st);",
            "st->cb_decode_mailbox.stop();",
            "st->cb_audio_fifo.start(",
            "cb_measure_share_connect(st);",
            "if (!obs_output_begin_data_capture(st->context, OBS_OUTPUT_VIDEO | OBS_OUTPUT_AUDIO)) { cb_measure_share_disconnect(st);",
        ],
    );
    let stop = unique_body_of(&src, "static void st_stop(void *data, uint64_t)");
    assert!(
        stop.contains(
            "cb_measure_share_disconnect(st); obs_output_end_data_capture(st->context); st->cb_decode_mailbox.stop(); st->cb_audio_fifo.stop();"
        ),
        "{DOCK_OUTPUT}: st_stop must remove the mix-tick callback before it stops the FIFO it feeds"
    );
    assert!(
        stop.contains(
            "st->cb_audio_fifo.stop(); cb_measure_source_hold(st, nullptr); st->cb_measure_source_checked = false;"
        ),
        "{DOCK_OUTPUT}: a stopped dock must not keep the measurement source alive, and the next start \
         must look it up at once (review round 1)"
    );
    let destroy = unique_body_of(&src, "static void st_destroy(void *data)");
    in_order(
        destroy,
        "st_destroy",
        &[
            "cb_measure_share_disconnect(st);",
            "st->cb_decode_mailbox.stop();",
            "st->cb_audio_fifo.stop();",
            "cb_measure_source_hold(st, nullptr);",
            "delete st;",
        ],
    );
}

/// The STALE reason, the header the output includes, and the new translation unit is compiled.
#[test]
fn the_not_in_program_reason_the_header_and_the_build() {
    let src = code();
    assert!(
        src.contains("#include \"camera-box-measure-share.hpp\""),
        "{DOCK_OUTPUT}: the output must include {SHARE_HEADER}"
    );
    for need in [
        "std::mutex cb_measure_src_mutex;",
        "obs_source_t *cb_measure_src = nullptr;",
        "camerabox::CbMeasureShareTracker cb_share_tracker;",
        "std::vector<float> cb_share_planes[MAX_AV_PLANES];",
    ] {
        assert!(
            src.contains(need),
            "{DOCK_OUTPUT}: struct sync_test_output lost `{need}`"
        );
    }
    let worker = squish(&strip_cpp_comments(&read(WORKER_HEADER)));
    assert!(
        worker.contains("NotInProgram,"),
        "{WORKER_HEADER}: CbAudioGate needs the NotInProgram reason (issue 1470)"
    );
    assert!(
        worker.contains(
            "case CbAudioGate::NotInProgram: return \"measurement source not in the program mix (issue 1470)\";"
        ),
        "{WORKER_HEADER}: the STALE line must name the program mix (issue 1470)"
    );
    assert!(
        read(CMAKE).contains("src/sync-test-output-measure.cpp"),
        "{CMAKE}: the measurement-share translation unit must be compiled"
    );
}
