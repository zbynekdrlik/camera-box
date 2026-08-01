//! #921 — real-optical-frame fixture harness for the A/V-sync dock's video-QR decode path.
//!
//! Issue 690 found the LIVE dock's video-QR decode rate collapsing to ~2% at steady state (from
//! ~55.6% shortly after launch) even though `st_raw_video_camera_box_decode`
//! (`vendor/av-sync-dock/src/sync-test-output.cpp`) already runs the #398 top-band, area-downscale,
//! Otsu-binarized-retry decode, not norihiro's crude whole-frame downscale. Per this repo's
//! standing rule (`.claude/rules/pattern-change-needs-decode-fixture.md`, "Zmena vzoru => decode
//! fixture test"), NO decode change lands without an offline test over REAL captured frames — this
//! is that harness.
//!
//! The fixtures under `tests/fixtures/av-sync-dock-921/` are real frames of the stream OBS
//! PROGRAM canvas (scene PRO) pulled via OBS-WS `GetSourceScreenshot` during a live TEST session
//! (cam2's dual-QR filmed by the broadcast camera) — exactly the pixel content
//! `st_raw_video_camera_box_decode` receives, converted to 8-bit grayscale raw (1920x1080,
//! row-major, no header) so this Tier-0 test needs no `image`-crate dependency on default
//! features.
//!
//! This harness compiles + runs the REAL vendored decode against those real frames in two
//! scenarios:
//!   * Scenario A (no cache) mirrors TODAY's shipped code: `quirc_resize()` is called
//!     unconditionally on every single decode call. All 3 real frames must decode — this alone
//!     PROVES the decode geometry/algorithm is not the defect (falsifying that hypothesis with
//!     real pixels, not just static code reading).
//!   * Scenario B (cached) exercises the #921 fix: a persistent `CbQrResizeCache` is reused across
//!     6 decode calls (the 3 fixtures, twice through — simulating a long-running dock session
//!     where the frame geometry never changes). Decode results must be IDENTICAL to scenario A,
//!     and `quirc_resize()` must be called exactly ONCE — `vendor/av-sync-dock/deps/quirc/lib/
//!     quirc.c`'s `quirc_resize()` has no early-out for an unchanged size (it unconditionally
//!     callocs 3 fresh buffers and frees the old ones every call); at 60fps that is 180 alloc+free
//!     calls/second for a size that never changes across the output's lifetime — real allocator
//!     churn the #921 design comment ties to the diagnostic's own temporal shape (decode reliability
//!     WORSENING with dock uptime, not a constant miss rate).
//!
//! `CbQrResizeCache`/`cb_qr_resize_needed` do not exist before the #921 fix — this test's C++
//! source fails to COMPILE against the unmodified `camera-box-video.hpp`, which is the genuine RED
//! for this ticket (this repo's real-frame decode ALREADY succeeds 6/6, so there is no runtime
//! decode failure to pin; the pinned defect is the missing resize cache, and a compile failure in
//! the temp-compiled harness is exactly how `tests/av_sync_dock_lock_926.rs` pins the analogous
//! "the mirror header doesn't have this yet" RED).

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const HARNESS_CPP: &str = r#"
#include "camera-box-video.hpp"
#include "camera-box-qr.hpp"
#include "quirc.h"

#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <vector>

using namespace camerabox;

static int g_failures = 0;
#define CHECK(cond, msg)                                                         \
    do {                                                                         \
        if (!(cond)) {                                                           \
            std::fprintf(stderr, "FAIL (%s:%d): %s\n", __FILE__, __LINE__, msg); \
            g_failures++;                                                        \
        }                                                                        \
    } while (false)

static int g_resize_calls = 0;

struct DecodeResult {
    bool found;
    uint32_t frame_id;
};

static std::vector<uint8_t> read_fixture(const char *path, size_t expect)
{
    FILE *f = std::fopen(path, "rb");
    if (!f) {
        std::fprintf(stderr, "cannot open fixture %s\n", path);
        std::exit(2);
    }
    std::vector<uint8_t> buf(expect);
    size_t got = std::fread(buf.data(), 1, expect, f);
    std::fclose(f);
    if (got != expect) {
        std::fprintf(stderr, "short read %s: %zu/%zu\n", path, got, expect);
        std::exit(2);
    }
    return buf;
}

/* Mirrors sync-test-output.cpp's st_raw_video_camera_box_decode() exactly, minus the OBS
 * video_data/pixel-format plumbing -- each fixture is already a tight row-major 8bpp luma buffer
 * at stride==width. `cache` == nullptr reproduces TODAY's unconditional per-frame resize; a
 * non-null cache exercises the #921 fix. */
static DecodeResult decode_one(const uint8_t *src, uint32_t width, uint32_t height, struct quirc *qr,
                                CbQrResizeCache *cache)
{
    DecodeResult r{false, 0};
    CbTopBandPlan plan = cb_top_band_decode_plan(width, height);
    if (plan.band_h == 0 || plan.dst_w == 0 || plan.dst_h == 0)
        return r;

    bool need_resize = cache ? cb_qr_resize_needed(*cache, plan.dst_w, plan.dst_h) : true;
    if (need_resize) {
        g_resize_calls++;
        if (quirc_resize(qr, (int)plan.dst_w, (int)plan.dst_h) < 0) {
            if (cache)
                *cache = CbQrResizeCache();
            return r;
        }
    }

    for (int pass = 0; pass < 2 && !r.found; pass++) {
        int w = 0, h = 0;
        uint8_t *qbuf = quirc_begin(qr, &w, &h);
        cb_box_downscale_luma(src, width, plan.band_h, qbuf, (uint32_t)w, (uint32_t)h);
        if (pass == 1)
            cb_binarize_otsu(qbuf, (size_t)w * (size_t)h);
        quirc_end(qr);

        int num_codes = quirc_count(qr);
        for (int i = 0; i < num_codes && !r.found; i++) {
            struct quirc_code code;
            struct quirc_data data;
            quirc_extract(qr, i, &code);
            auto err = quirc_decode(&code, &data);
            if (err == QUIRC_ERROR_DATA_ECC) {
                quirc_flip(&code);
                err = quirc_decode(&code, &data);
            }
            if (err)
                continue;
            data.payload[QUIRC_MAX_PAYLOAD - 1] = 0;
            CameraBoxQrData cb;
            if (!decode_camera_box_qr((char *)data.payload, &cb))
                continue;
            r.found = true;
            r.frame_id = cb.frame_id;
        }
    }
    return r;
}

int main(int argc, char **argv)
{
    if (argc != 4) {
        std::fprintf(stderr, "usage: %s fixture0 fixture1 fixture2\n", argv[0]);
        return 2;
    }
    const uint32_t W = 1920, H = 1080;
    std::vector<std::vector<uint8_t>> frames;
    for (int i = 1; i < argc; i++)
        frames.push_back(read_fixture(argv[i], (size_t)W * H));

    /* Scenario A: TODAY's behaviour -- fresh quirc context, resize called unconditionally on
     * every decode call (no cache). Every real captured frame must decode. */
    {
        struct quirc *qr = quirc_new();
        CHECK(qr != nullptr, "quirc_new failed (scenario A)");
        g_resize_calls = 0;
        int ok = 0;
        for (auto &f : frames) {
            DecodeResult r = decode_one(f.data(), W, H, qr, nullptr);
            if (r.found)
                ok++;
        }
        CHECK(ok == (int)frames.size(),
              "scenario A (no cache): every real captured frame must decode -- proves the current "
              "top-band/downscale/Otsu geometry is NOT the defect");
        CHECK(g_resize_calls == (int)frames.size(),
              "scenario A (no cache): today's code resizes on EVERY call, unconditionally");
        quirc_destroy(qr);
    }

    /* Scenario B: the #921 fix -- ONE persistent quirc context + cache, decoding the SAME 3
     * fixtures TWICE THROUGH (6 calls total), simulating a long-running dock session where the
     * frame geometry never changes. Must decode identically AND resize only ONCE. */
    {
        struct quirc *qr = quirc_new();
        CHECK(qr != nullptr, "quirc_new failed (scenario B)");
        CbQrResizeCache cache;
        g_resize_calls = 0;
        int ok = 0, total = 0;
        for (int lap = 0; lap < 2; lap++) {
            for (auto &f : frames) {
                total++;
                DecodeResult r = decode_one(f.data(), W, H, qr, &cache);
                if (r.found)
                    ok++;
            }
        }
        CHECK(ok == total,
              "scenario B (cached): decode results must be IDENTICAL to scenario A -- the cache "
              "must never change what decodes");
        CHECK(g_resize_calls == 1,
              "scenario B (cached): quirc_resize must be called exactly ONCE across repeated "
              "frames of unchanged geometry -- the #921 fix (quirc_resize has no early-out for an "
              "unchanged size; called every frame at 60fps that is real allocator churn)");
        quirc_destroy(qr);
    }

    if (g_failures == 0) {
        std::printf("av_sync_dock_video_decode_921: ALL PASS\n");
        return 0;
    }
    std::printf("av_sync_dock_video_decode_921: %d FAILURE(S)\n", g_failures);
    return 1;
}
"#;

#[test]
fn real_optical_frames_decode_and_resize_is_cached() {
    let src_dir = manifest_dir().join("vendor/av-sync-dock/src");
    let quirc_dir = manifest_dir().join("vendor/av-sync-dock/deps/quirc/lib");
    for hdr in ["camera-box-video.hpp", "camera-box-qr.hpp"] {
        assert!(
            src_dir.join(hdr).exists(),
            "#921: missing dock header {}",
            src_dir.join(hdr).display()
        );
    }

    let fixtures_dir = manifest_dir().join("tests/fixtures/av-sync-dock-921");
    let fixture_names = [
        "stream-program-0.y8",
        "stream-program-2.y8",
        "stream-program-4.y8",
    ];
    for name in fixture_names {
        let p = fixtures_dir.join(name);
        assert!(
            p.exists(),
            "#921: missing real-frame fixture {}",
            p.display()
        );
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "av_sync_dock_video_decode_921_{}_{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&work).expect("create temp workdir");

    let cpp_src = work.join("harness.cpp");
    std::fs::write(&cpp_src, HARNESS_CPP).expect("write C++ harness");

    // The vendored quirc sources are real C (rely on implicit void* -> T* conversions that are
    // valid C but ill-formed C++) -- compile them with a real C compiler, never g++, and link the
    // resulting objects into the C++ harness.
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());

    let mut objs = Vec::new();
    for c_file in ["quirc.c", "decode.c", "identify.c", "version_db.c"] {
        let obj = work.join(format!("{c_file}.o"));
        let compile = Command::new(&cc)
            .args(["-O2", "-c"])
            .arg(quirc_dir.join(c_file))
            .arg("-I")
            .arg(&quirc_dir)
            .arg("-o")
            .arg(&obj)
            .output()
            .unwrap_or_else(|e| panic!("failed to invoke C compiler '{cc}': {e}"));
        assert!(
            compile.status.success(),
            "#921: vendored quirc {c_file} failed to compile with {cc}:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        objs.push(obj);
    }

    let harness_obj = work.join("harness.o");
    let compile = Command::new(&cxx)
        .args(["-std=c++17", "-Wall", "-O2", "-c"])
        .arg(&cpp_src)
        .arg("-I")
        .arg(&src_dir)
        .arg("-I")
        .arg(&quirc_dir)
        .arg("-o")
        .arg(&harness_obj)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke C++ compiler '{cxx}': {e}"));
    assert!(
        compile.status.success(),
        "#921: fixture harness failed to compile against camera-box-video.hpp -- this is the \
         expected RED before CbQrResizeCache/cb_qr_resize_needed exist:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let bin = work.join("harness");
    let link = Command::new(&cxx)
        .arg(&harness_obj)
        .args(&objs)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke linker via '{cxx}': {e}"));
    assert!(
        link.status.success(),
        "#921: fixture harness failed to link:\n{}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&bin)
        .arg(fixtures_dir.join(fixture_names[0]))
        .arg(fixtures_dir.join(fixture_names[1]))
        .arg(fixtures_dir.join(fixture_names[2]))
        .output()
        .expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success() && String::from_utf8_lossy(&run.stdout).contains("ALL PASS"),
        "#921: real-optical-frame video-QR decode harness FAILED (exit {:?}).\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
