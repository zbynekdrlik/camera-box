//! #999 — the dock's operator-visible "Latency" QLabel + polarity text (`SyncTestDock::
//! on_sync_found` in `sync-test-dock.cpp`) is a code path issue #953's sign fix NEVER touched
//! (`git show <953-commit> -- vendor/av-sync-dock/src/sync-test-dock.cpp` is empty): it still
//! computes `ts = audio_ts - video_ts` in norihiro's ORIGINAL native convention, while #953 fixed
//! the sign convention only at the separate OBS-log `blog()` call sites inside
//! `st_raw_audio_camera_box` (`sync-test-output.cpp`), via `cb_dock_lock_display_offset_ms()`.
//!
//! Live evidence this explains (2026-08-06): operator screenshot showed `Latency -57.1ms "Audio
//! early"` (dock-native, unconverted — close to issue 952's pre-#953 `dock ~= -gate - 55`
//! relation) while the SAME window's (already-#953-converted) OBS log LOCKED/UPDATED lines showed
//! `+20..+47ms`, and the offline calibrated truth (`recording-verdict --av-sync`) read `~0ms`.
//!
//! This fixes it: `cb_dock_latency_display_ms(dock_native_ts_ns, gate_convention)` applies the
//! SAME negation `cb_dock_lock_display_offset_ms()` already applies to every log line, and swaps
//! the polarity label accordingly, when `gate_convention` is true. `gate_convention` is threaded
//! through a new `sync_index::gate_convention` field (mirrors the existing
//! `audio_marker_found_s::sparse_index` flag's exact purpose), set `true` only at camera-box's own
//! two `signal_sync_found` call sites in `st_raw_audio_camera_box` — norihiro's own legacy
//! list-based method (mutually exclusive with camera-box mode) keeps `gate_convention=false` and
//! is byte-for-byte unchanged.
//!
//! Same twin-harness pattern as `tests/av_sync_dock_suggested_target_953.rs` (#953) /
//! `tests/av_sync_dock_outcome_955.rs` (#955) — compile + run a tiny real C++ program against the
//! real production header, off-rig.
//!
//! RED (before the fix): `cb_dock_latency_display_ms`/`CbLatencyPolarity`/`CbLatencyDisplay` do
//! not exist in `camera-box-audio.hpp` -> fails to COMPILE.
//! GREEN: it compiles and every scenario matches `src/av_sync_dock.rs::dock_latency_display_ms`'s
//! own unit tests (including the live-evidence -57.1ms case).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const HARNESS_CPP: &str = r#"
#include "camera-box-audio.hpp"
#include <cmath>
#include <cstdio>

using namespace camerabox;

static int g_failures = 0;

#define CHECK(cond, msg)                                                          \
    do {                                                                          \
        if (!(cond)) {                                                            \
            std::fprintf(stderr, "FAIL (%s:%d): %s\n", __FILE__, __LINE__, msg);  \
            g_failures++;                                                         \
        }                                                                         \
    } while (false)

int main()
{
    // (1) gate_convention=false reproduces norihiro's ORIGINAL on_sync_found byte-for-byte: no
    // negation, Positive/"Audio lagged" when ts>0.
    {
        CbLatencyDisplay d = cb_dock_latency_display_ms(5000000, false); // +5ms native
        CHECK(d.display_ms == 5.0, "native +5ms must stay +5.0");
        CHECK(d.polarity == CbLatencyPolarity::Positive, "native ts>0 must be Positive");

        d = cb_dock_latency_display_ms(-5000000, false); // -5ms native
        CHECK(d.display_ms == -5.0, "native -5ms must stay -5.0");
        CHECK(d.polarity == CbLatencyPolarity::Negative, "native ts<0 must be Negative");

        d = cb_dock_latency_display_ms(0, false);
        CHECK(d.display_ms == 0.0, "native 0 must stay 0.0");
        CHECK(d.polarity == CbLatencyPolarity::None, "native ts==0 must be None (label untouched)");
    }

    // (2) gate_convention=true negates and swaps the polarity label -- the SAME +5ms native
    // reading as above must now read -5.0 / Positive-label-swapped-to... (opposite branch).
    {
        CbLatencyDisplay d = cb_dock_latency_display_ms(5000000, true); // +5ms native -> gate -5.0
        CHECK(d.display_ms == -5.0, "gate-converted +5ms native must be -5.0");
        CHECK(d.polarity == CbLatencyPolarity::Positive,
              "gate-negative display must map to the Positive locale key (\"Audio lagged\")");

        d = cb_dock_latency_display_ms(-5000000, true); // -5ms native -> gate +5.0
        CHECK(d.display_ms == 5.0, "gate-converted -5ms native must be +5.0");
        CHECK(d.polarity == CbLatencyPolarity::Negative,
              "gate-positive display must map to the Negative locale key (\"Audio early\")");

        d = cb_dock_latency_display_ms(0, true);
        CHECK(d.display_ms == 0.0, "gate-converted 0 must stay 0.0");
        CHECK(!std::signbit(d.display_ms),
              "gate-converted 0 must be +0.0, never IEEE -0.0 (QString would render \"-0.0 ms\")");
        CHECK(d.polarity == CbLatencyPolarity::None, "gate ts==0 must be None (label untouched)");
    }

    // (3) issue 999's own live evidence: dock-native ts ~= -57.1ms must convert to +57.1ms in gate
    // convention (matching the OBS log's own +20..+47ms window's SIGN, not stay negative).
    {
        CbLatencyDisplay d = cb_dock_latency_display_ms(-57100000, true);
        CHECK(d.display_ms == 57.1, "live-evidence -57.1ms native must convert to +57.1ms gate");
        CHECK(d.polarity == CbLatencyPolarity::Negative,
              "gate-positive (video lags audio) must show \"Audio early\" (Negative key)");
    }

    // (4) gate-convention display must be the EXACT negation of the native display -- never a
    // second, independently-computed sign flip that could silently drift from
    // cb_dock_lock_display_offset_ms (the SAME function every OBS log line already uses).
    {
        int64_t samples[] = {-123456000, -1, 0, 1, 999999, 55000000, -57100000};
        for (size_t i = 0; i < sizeof(samples) / sizeof(samples[0]); i++) {
            CbLatencyDisplay native = cb_dock_latency_display_ms(samples[i], false);
            CbLatencyDisplay gate = cb_dock_latency_display_ms(samples[i], true);
            CHECK(gate.display_ms == cb_dock_lock_display_offset_ms(native.display_ms),
                  "gate display must equal cb_dock_lock_display_offset_ms(native display)");
        }
    }

    // (5) sync_index carries the new gate_convention field, defaulting false (norihiro's legacy
    // method, unchanged unless explicitly threaded true by camera-box's own call sites).
    {
        struct sync_index si;
        CHECK(si.gate_convention == false, "sync_index::gate_convention must default to false");
    }

    std::printf("av_sync_dock_latency_display_999: %d FAILURE(S)\n", g_failures);
    return g_failures == 0 ? 0 : 1;
}
"#;

#[test]
fn cb_dock_latency_display_ms_matches_the_rust_gate_convention_negation() {
    let src_dir = manifest_dir().join("vendor/av-sync-dock/src");
    let header = src_dir.join("camera-box-audio.hpp");
    assert!(
        header.exists(),
        "#999: missing pure audio header {} — the Latency display conversion logic must live \
         there so it stays unit-testable off-rig.",
        header.display()
    );
    // sync_index lives in sync-test-output.hpp; the harness includes camera-box-audio.hpp only,
    // so also pull in the struct definition for check (5) above via a minimal local re-declare
    // is NOT an option (must be the REAL struct, not a stand-in) -- include the real header too.
    let output_header = src_dir.join("sync-test-output.hpp");
    assert!(
        output_header.exists(),
        "#999: missing {} — sync_index::gate_convention must be added there.",
        output_header.display()
    );

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "av_sync_dock_latency_display_999_{}_{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&work).expect("create temp workdir");
    let src = work.join("harness.cpp");
    let bin = work.join("harness");
    // sync-test-output.hpp includes <obs-module.h>, which is not available off-rig -- but
    // sync_index itself has zero OBS dependency, so pull JUST that struct's shape by including
    // camera-box-audio.hpp (for the functions under test) and re-declaring sync_index's exact
    // field list inline in the harness would risk drifting from the real struct. Instead, the
    // harness includes camera-box-audio.hpp only for the FUNCTION checks (1-4); check (5)
    // constructs a `struct sync_index` textually matching the real one is intentionally SKIPPED
    // here to avoid the <obs-module.h> dependency -- the real field addition is instead verified
    // by a plain source-text assertion below (grep for the field on the real header), which is
    // exactly as strong a proof for a one-line default-initialized bool field.
    let full_source = HARNESS_CPP.replace(
        "\n    // (5) sync_index carries the new gate_convention field, defaulting false (norihiro's legacy\n    // method, unchanged unless explicitly threaded true by camera-box's own call sites).\n    {\n        struct sync_index si;\n        CHECK(si.gate_convention == false, \"sync_index::gate_convention must default to false\");\n    }\n",
        "\n",
    );
    std::fs::write(&src, full_source).expect("write C++ harness");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let compile = Command::new(&cxx)
        .arg("-std=c++17")
        .arg("-Wall")
        .arg("-I")
        .arg(&src_dir)
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke C++ compiler '{cxx}': {e}"));
    assert!(
        compile.status.success(),
        "#999: camera-box-audio.hpp (+ cb_dock_latency_display_ms/CbLatencyDisplay/\
         CbLatencyPolarity) failed to compile with {cxx}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#999: cb_dock_latency_display_ms regression harness FAILED (exit {:?}).\nstdout: {}\n\
         stderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );

    // sync_index::gate_convention (check 5, kept as a source-text assertion -- see the comment
    // above the harness rewrite for why the field isn't exercised through a compiled check here).
    let output_hpp_text =
        std::fs::read_to_string(&output_header).expect("read sync-test-output.hpp");
    assert!(
        output_hpp_text.contains("bool gate_convention = false;"),
        "#999: struct sync_index in {} must carry the literal `bool gate_convention = false;` \
         field declaration (a bare substring match on the word would be satisfied by the doc \
         comment alone — review finding on this PR) so on_sync_found can tell camera-box's own \
         direct-ring events (gate convention) apart from norihiro's legacy list-based ones \
         (native convention, unchanged)",
        output_header.display()
    );
}
