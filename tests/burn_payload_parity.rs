//! #111 — DistroAV QR render-time burn (Path B): byte-identity + vendored-source guards.
//!
//! The burn filter stamps each rendered frame with a QR carrying a payload that is
//! BYTE-IDENTICAL to the camera-box probe payload (src/probe/payload.rs), so the
//! existing `rqrr` recorded-file decoder (src/probe/recording.rs / #106) reads the
//! node's stamp UNCHANGED, and #108's per-hop subtraction is valid. This test proves
//! that identity AT THE BYTE LEVEL by COMPILING AND RUNNING the actual vendored C++
//! encoder (vendor/distroav/src/burn-payload.hpp) and comparing its output to the Rust
//! `Payload::encode()` for known triples — not a hand-copied fixture, the real code.
//!
//! Two facets, mirroring the proven tests/genlock_preload.rs convention:
//!   1. C++↔Rust byte-identity of the payload string AND the gen_ts_ns boundary math,
//!      by compiling+running the vendored headers with g++ at test time.
//!   2. Vendored-source + CMake + windows-genlock-workflow guards so a future
//!      `git subtree pull` (#44) or a build-wiring regression can't silently drop the
//!      burn filter / encoder.

#![cfg(feature = "probe")]

use camera_box::probe::luma::bgra_to_luma;
use camera_box::probe::painter::next_wall_boundary_ns;
use camera_box::probe::payload::Payload;
use camera_box::probe::qr::decode_qr_luma_all;
use std::path::PathBuf;
use std::process::Command;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn vendor_file(rel: &str) -> String {
    let p = manifest().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const BURN_PAYLOAD_HPP: &str = "vendor/distroav/src/burn-payload.hpp";
const BURN_CLOCK_HPP: &str = "vendor/distroav/src/burn-clock.hpp";
const BURN_QR_HPP: &str = "vendor/distroav/src/burn-qr.hpp";
const QRCODEGEN_CPP: &str = "vendor/distroav/src/qrcodegen/qrcodegen.cpp";
const BURN_FILTER: &str = "vendor/distroav/src/ndi-burn-filter.cpp";
const DISTROAV_CMAKE: &str = "vendor/distroav/CMakeLists.txt";
const PLUGIN_MAIN: &str = "vendor/distroav/src/plugin-main.cpp";
const WINDOWS_GENLOCK_WF: &str = ".github/workflows/windows-genlock.yml";

// ---- 1. C++↔Rust byte-identity (compile + run the real vendored encoder) -----

/// Known (run_id, frame_id, gen_ts_ns) triples spanning the field ranges the burn
/// uses: cam2-style small ids, the reserved node run_ids (strih 911002 / stream
/// 911004), a large epoch-ns timestamp, and edge zeros/maxes.
fn parity_triples() -> Vec<(u32, u32, i64)> {
    vec![
        (42, 9001, 1_234_567_890),
        (0, 0, 0),
        (1, 2, 3),
        (911002, 7, 1_718_600_000_000_000_000), // strih node stamp
        (911004, 1, 1_718_600_000_033_333_333), // stream node stamp
        (u32::MAX, u32::MAX, i64::MAX),         // upper edges
        (123456, 4294967295, 9_000_000_000_000_000), // mixed
    ]
}

/// Compile a tiny C++ main that includes the vendored burn-payload header and prints
/// `encode(run_id, frame_id, gen_ts_ns)` for argv triples, then run it. Returns the
/// C++-produced wire strings, one per triple. PANICS (fails the test) if the header is
/// missing or does not compile — that IS the RED state before the filter is built.
fn cpp_encode(triples: &[(u32, u32, i64)]) -> Vec<String> {
    // Unique dir PER CALL: tests run concurrently in one process, so a PID-only path
    // races two writers on the same binary ("Text file busy"). A monotonic counter
    // isolates each invocation.
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("burn_parity_{}_{}", std::process::id(), seq));
    std::fs::create_dir_all(&dir).unwrap();
    let main_cpp = dir.join("parity_main.cpp");
    let bin = dir.join("parity_main");

    // The harness includes the headers by absolute path (no CMake needed). It exercises
    // the SAME inline functions the burn filter calls, so a match proves the burned
    // bytes equal Payload::encode. BURN_CLOCK_NO_WALL keeps burn-clock.hpp's pure math
    // compilable without linking <chrono>'s wall read (not exercised here — see the
    // boundary-math parity test, which calls next_wall_boundary_ns directly).
    let payload_hpp = manifest().join(BURN_PAYLOAD_HPP);
    let clock_hpp = manifest().join(BURN_CLOCK_HPP);
    let src = format!(
        r#"
#define BURN_CLOCK_NO_WALL 1
#include "{payload}"
#include "{clock}"
#include <cstdio>
#include <cstdlib>
#include <string>
int main(int argc, char **argv) {{
    // argv: groups of 3 -> run_id frame_id gen_ts_ns
    for (int i = 1; i + 2 < argc + 1 && i + 2 <= argc; i += 3) {{
        uint32_t run_id = (uint32_t) strtoul(argv[i], nullptr, 10);
        uint32_t frame_id = (uint32_t) strtoul(argv[i+1], nullptr, 10);
        int64_t gen_ts = (int64_t) strtoll(argv[i+2], nullptr, 10);
        std::string s = burn_payload::encode(run_id, frame_id, gen_ts);
        // also exercise the boundary math so a compile error there is caught here
        (void) burn_clock::next_wall_boundary_ns(gen_ts, 33333333);
        printf("%s\n", s.c_str());
    }}
    return 0;
}}
"#,
        payload = payload_hpp.display(),
        clock = clock_hpp.display(),
    );
    std::fs::write(&main_cpp, src).unwrap();

    let compile = Command::new("g++")
        .args(["-std=c++17", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(&main_cpp)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("g++ must be installed on the test runner (ubuntu-latest ships it)");
    assert!(
        compile.status.success(),
        "vendored C++ burn encoder did not compile (#111 RED until the headers exist \
         and are correct):\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );

    let mut args: Vec<String> = Vec::new();
    for (r, f, g) in triples {
        args.push(r.to_string());
        args.push(f.to_string());
        args.push(g.to_string());
    }
    let run = Command::new(&bin)
        .args(&args)
        .output()
        .expect("running the compiled C++ encoder failed");
    assert!(run.status.success(), "C++ encoder exited nonzero");
    String::from_utf8(run.stdout)
        .unwrap()
        .lines()
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn cpp_encoder_is_byte_identical_to_rust_payload_encode() {
    let triples = parity_triples();
    let cpp = cpp_encode(&triples);
    assert_eq!(
        cpp.len(),
        triples.len(),
        "C++ encoder produced {} lines for {} triples",
        cpp.len(),
        triples.len()
    );
    for ((run_id, frame_id, gen_ts_ns), cpp_str) in triples.iter().zip(cpp.iter()) {
        let rust = Payload {
            run_id: *run_id,
            frame_id: *frame_id,
            gen_ts_ns: *gen_ts_ns,
        }
        .encode();
        assert_eq!(
            cpp_str, &rust,
            "BYTE MISMATCH for (run_id={run_id}, frame_id={frame_id}, gen_ts_ns={gen_ts_ns}): \
             C++ burn produced {cpp_str:?}, Rust Payload::encode produced {rust:?}. The burned \
             QR would NOT decode to the same payload the #106/#108 decoder expects."
        );
    }
}

#[test]
fn cpp_burned_payload_round_trips_through_the_rust_decoder() {
    // The decoder (rqrr -> Payload::decode) is the consumer. Prove every C++-produced
    // string decodes back to the EXACT input fields — the end-to-end identity #108 needs.
    let triples = parity_triples();
    let cpp = cpp_encode(&triples);
    for ((run_id, frame_id, gen_ts_ns), cpp_str) in triples.iter().zip(cpp.iter()) {
        let decoded = Payload::decode(cpp_str).unwrap_or_else(|| {
            panic!("Rust decoder REJECTED the C++-burned payload {cpp_str:?} (bad CRC or format)")
        });
        assert_eq!(decoded.run_id, *run_id);
        assert_eq!(decoded.frame_id, *frame_id);
        assert_eq!(decoded.gen_ts_ns, *gen_ts_ns);
    }
}

#[test]
fn cpp_boundary_math_matches_rust_next_wall_boundary_ns() {
    // The burned gen_ts_ns is the RAW render-instant wall clock (NOT boundary-snapped) so
    // it shares the camera-box painter's RAW basis — that is what makes #108's cam→strih
    // subtraction bias-free (#108 finding #2; snapping the burn against the raw cam2 stamp
    // would inject a ~½-frame bias). burn_clock::next_wall_boundary_ns is the retained,
    // documented port of the painter's PACING grid (the cadence it sleeps to, not the
    // stamp). This parity test guards that port's C++↔Rust bit-identity so the documented
    // relationship can't silently drift. Compile a tiny harness and compare.
    let dir = std::env::temp_dir().join(format!("burn_boundary_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main_cpp = dir.join("boundary_main.cpp");
    let bin = dir.join("boundary_main");
    let clock_hpp = manifest().join(BURN_CLOCK_HPP);
    let src = format!(
        r#"
#define BURN_CLOCK_NO_WALL 1
#include "{clock}"
#include <cstdio>
#include <cstdlib>
int main(int argc, char **argv) {{
    for (int i = 1; i + 1 < argc; i += 2) {{
        int64_t now = (int64_t) strtoll(argv[i], nullptr, 10);
        int64_t period = (int64_t) strtoll(argv[i+1], nullptr, 10);
        printf("%lld\n", (long long) burn_clock::next_wall_boundary_ns(now, period));
    }}
    return 0;
}}
"#,
        clock = clock_hpp.display(),
    );
    std::fs::write(&main_cpp, src).unwrap();
    let compile = Command::new("g++")
        .args(["-std=c++17", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(&main_cpp)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("g++ must be installed");
    assert!(
        compile.status.success(),
        "burn-clock.hpp did not compile (#111 RED):\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // 30 fps period and a couple of edge cases. Use u64 inputs that fit i64 (the C++ is
    // i64 to allow the strictly-next semantics; the painter's u64 values are < i64::MAX).
    let cases: Vec<(u64, u64)> = vec![
        (0, 33_333_333),
        (33_333_333, 33_333_333), // exact boundary -> strictly next advances one period
        (33_333_332, 33_333_333),
        (1_718_600_000_000_000_000, 33_333_333),
        (1_000_000_000, 16_666_667), // 60 fps
        (42, 0),                     // period 0 -> guard returns now
    ];
    let mut args: Vec<String> = Vec::new();
    for (now, period) in &cases {
        args.push(now.to_string());
        args.push(period.to_string());
    }
    let run = Command::new(&bin)
        .args(&args)
        .output()
        .expect("run boundary main");
    let cpp_out: Vec<i64> = String::from_utf8(run.stdout)
        .unwrap()
        .lines()
        .map(|l| l.parse::<i64>().unwrap())
        .collect();
    assert_eq!(cpp_out.len(), cases.len());
    for ((now, period), cpp) in cases.iter().zip(cpp_out.iter()) {
        let rust = next_wall_boundary_ns(*now, *period) as i64;
        assert_eq!(
            *cpp, rust,
            "boundary math mismatch for now={now} period={period}: C++={cpp} Rust={rust}"
        );
    }
}

#[test]
fn cpp_rendered_burn_qr_decodes_back_through_rqrr() {
    // THE end-to-end proof: the C++ render path (qrcodegen modules -> BGRA, EC-High,
    // white quiet zone — burn-qr.hpp) must produce a QR that the PRODUCTION recorded-file
    // decoder (rqrr -> decode_qr_luma_all, src/probe/qr.rs / #106) reads back to the EXACT
    // burned payloads. A dual-QR frame (left=even tick, right=odd tick — the anti-blur
    // Vernier layout the rig uses) is rendered by a compiled C++ harness to a raw BGRA
    // file; Rust loads it, converts to luma, decodes BOTH QRs, and asserts they equal the
    // C++-burned Payload::encode strings. This is what proves the burn is usable, not just
    // byte-identical on the wire.
    let dir = std::env::temp_dir().join(format!("burn_render_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main_cpp = dir.join("render_main.cpp");
    let bin = dir.join("render_main");
    let out_bgra = dir.join("frame.bgra");

    let payload_hpp = manifest().join(BURN_PAYLOAD_HPP);
    let qr_hpp = manifest().join(BURN_QR_HPP);
    let qrcg_cpp = manifest().join(QRCODEGEN_CPP);
    let qrcg_inc = manifest().join("vendor/distroav/src"); // so qrcodegen/qrcodegen.hpp resolves

    // 1920x1080 BGRA, two QRs at ~700px in the L/R halves — the production dual-QR layout.
    const W: u32 = 1920;
    const H: u32 = 1080;
    const QR_PX: u32 = 700;
    let left = (911002u32, 6518u32, 1_718_600_000_000_000_000i64); // even tick
    let right = (911002u32, 6519u32, 1_718_600_000_033_333_333i64); // odd tick

    let src = format!(
        r#"
#include "{payload}"
#include "{qr}"
#include <cstdint>
#include <cstdio>
#include <vector>
int main(int argc, char **argv) {{
    if (argc < 2) return 2;
    const uint32_t W = {w}, H = {h}, QR_PX = {qr_px};
    std::vector<uint8_t> buf((size_t)W * H * 4, 255); // white BGRA canvas
    const uint32_t stride = W * 4;
    const uint32_t half = W / 2;
    std::string l = burn_payload::encode({lr}, {lf}, {lg});
    std::string r = burn_payload::encode({rr}, {rf}, {rg});
    burn_qr::render(buf.data(), stride, W, H, l, 0, half, H/2, QR_PX);
    burn_qr::render(buf.data(), stride, W, H, r, half, W - half, H/2, QR_PX);
    FILE *f = fopen(argv[1], "wb");
    if (!f) return 3;
    fwrite(buf.data(), 1, buf.size(), f);
    fclose(f);
    // also print the two payloads so the test compares against the SAME bytes the C++ used
    printf("%s\n%s\n", l.c_str(), r.c_str());
    return 0;
}}
"#,
        payload = payload_hpp.display(),
        qr = qr_hpp.display(),
        w = W,
        h = H,
        qr_px = QR_PX,
        lr = left.0,
        lf = left.1,
        lg = left.2,
        rr = right.0,
        rf = right.1,
        rg = right.2,
    );
    std::fs::write(&main_cpp, src).unwrap();

    let compile = Command::new("g++")
        .args(["-std=c++17", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg("-I")
        .arg(&qrcg_inc)
        .arg(&main_cpp)
        .arg(&qrcg_cpp)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("g++ must be installed");
    assert!(
        compile.status.success(),
        "burn-qr.hpp / qrcodegen did not compile (#111 RED):\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&bin)
        .arg(&out_bgra)
        .output()
        .expect("running the C++ renderer failed");
    assert!(
        run.status.success(),
        "C++ renderer exited nonzero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let burned: Vec<String> = String::from_utf8(run.stdout)
        .unwrap()
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(burned.len(), 2, "expected 2 burned payload strings");

    let data = std::fs::read(&out_bgra).unwrap();
    assert_eq!(data.len(), (W * H * 4) as usize, "BGRA frame size mismatch");
    let luma = bgra_to_luma(&data, W, H, W * 4);
    let decoded = decode_qr_luma_all(luma);
    let decoded_strs: Vec<String> = decoded.iter().map(|p| p.encode()).collect();

    for b in &burned {
        assert!(
            decoded_strs.contains(b),
            "rqrr did NOT decode the C++-burned QR {b:?}. Decoded: {decoded_strs:?}. The burn \
             produces a QR the production recorded-file decoder cannot read."
        );
    }
    // Both distinct ticks decoded (dual-QR Vernier intact).
    let l = Payload {
        run_id: left.0,
        frame_id: left.1,
        gen_ts_ns: left.2,
    }
    .encode();
    let r = Payload {
        run_id: right.0,
        frame_id: right.1,
        gen_ts_ns: right.2,
    }
    .encode();
    assert!(decoded_strs.contains(&l), "left tick {l:?} not decoded");
    assert!(decoded_strs.contains(&r), "right tick {r:?} not decoded");
}

/// Axis-aligned rectangle [x, x+w) × [y, y+h) for the overlap test.
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

impl Rect {
    fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// The four QR bounding rectangles in the composited 1920×1080 stream frame, computed from
/// the REAL geometry functions (the painter top-anchor + the C++ burn corner placement,
/// mirrored), the #111 4-corner layout. The camera dual-QR halves render at ~`cam_px`
/// (forced square by min/max_dimensions); the burns at `burn_px` in the two bottom corners.
fn four_qr_rects(w: i64, h: i64, cam_px: i64, burn_px: i64, burn_margin: i64) -> [Rect; 4] {
    let half = w / 2;
    // Camera dual-QR: each half centered horizontally in its half, TOP-anchored.
    let cam_oy = camera_box::probe::qr::qr_origin_y(
        h as u32,
        cam_px as u32,
        camera_box::probe::qr::VAnchor::Top,
    ) as i64;
    let cam_left_ox = (half - cam_px) / 2;
    let cam_right_ox = half + (half - cam_px) / 2;
    let cam_left = Rect {
        x: cam_left_ox,
        y: cam_oy,
        w: cam_px,
        h: cam_px,
    };
    let cam_right = Rect {
        x: cam_right_ox,
        y: cam_oy,
        w: cam_px,
        h: cam_px,
    };
    // Burns: bottom-left + bottom-right corners (mirror burn_geom::corner_placement: band is
    // exactly burn_px wide, flush to the corner with burn_margin clearance; bottom edge at
    // h - burn_margin).
    let burn_y = h - burn_margin - burn_px;
    let strih = Rect {
        x: burn_margin,
        y: burn_y,
        w: burn_px,
        h: burn_px,
    }; // bottom-left
    let stream = Rect {
        x: w - burn_margin - burn_px,
        y: burn_y,
        w: burn_px,
        h: burn_px,
    }; // bottom-right
    [cam_left, cam_right, strih, stream]
}

/// Burn QR px for a canvas of height `h`, mirroring the C++ `burn_qr_px_for_canvas`
/// auto path (#186 / #172): `BURN_QR_HEIGHT_FRACTION` (0.28) × height, floored at 64.
/// Keep this constant in sync with `vendor/distroav/src/burn-geom.hpp`.
fn burn_px_for_canvas(h: i64) -> i64 {
    const BURN_QR_HEIGHT_FRACTION: f64 = 0.28;
    let px = (BURN_QR_HEIGHT_FRACTION * h as f64) as i64;
    px.max(64)
}

/// Burn edge margin for a canvas of height `h`, mirroring `burn_margin_for_canvas`
/// (#172, canvas-relative ≈ 40px-on-1080, floored at 8). Keep in sync with burn-geom.hpp.
fn burn_margin_for_canvas(h: i64) -> i64 {
    let m = ((40.0 / 1080.0) * h as f64) as i64;
    m.max(8)
}

/// The camera dual-QR px for a canvas of height `h`: production paints 700px on the 1080
/// cam2 monitor and it RIDES through the upscale, so on a taller canvas the same content
/// arrives scaled by h/1080 (the 4K stream is a 2× upscale of the 1080 program).
fn cam_px_for_canvas(h: i64) -> i64 {
    (700 * h) / 1080
}

/// #463 — imag's bottom-CENTER-LEFT corner burn rect, mirroring
/// `burn_geom::corner_placement`'s new `Corner::BottomCenterLeft` case EXACTLY: one
/// `burn_margin` clear of the bottom-left (strih) burn's trailing edge
/// (`burn_margin + burn_px + burn_margin`), same row as the other two corner burns
/// (`burn_y = h - burn_margin - burn_px`). Clamped in-frame the same way the C++ does for a
/// degenerate tiny canvas.
fn imag_burn_rect(w: i64, h: i64, burn_px: i64, burn_margin: i64) -> Rect {
    let burn_y = h - burn_margin - burn_px;
    let wanted_x = burn_margin + burn_px + burn_margin;
    let x = if wanted_x + burn_px <= w {
        wanted_x
    } else if w > burn_px {
        w - burn_px
    } else {
        0
    };
    Rect {
        x,
        y: burn_y,
        w: burn_px,
        h: burn_px,
    }
}

#[test]
fn four_qr_rectangles_do_not_overlap_and_are_in_frame() {
    // #111 REGRESSION GUARD + #172 canvas-independence + #186 canvas-relative burns: the four
    // QRs (camera L/R top + strih burn bottom-left + stream burn bottom-right) MUST NOT overlap
    // and MUST be in-frame — on EVERY canvas, not just the production 1080 (the #172 gap: the
    // old test hardcoded 1920×1080 + a fixed 300px burn, so a 4K canvas with the same fixed
    // 300px silently went soft, and a smaller canvas could re-overlap, with no test catching it).
    // Now the burn px is canvas-relative (0.29×h, #186) and the camera QR rides the upscale, so
    // the clearance is asserted as a function of canvas height across several resolutions.
    let names = ["cam-left", "cam-right", "strih-burn", "stream-burn"];
    // 720p, 1080p (production), 1440p, 4K — the burn must stay non-overlapping + in-frame on all.
    for (w, h) in [(1280i64, 720i64), (1920, 1080), (2560, 1440), (3840, 2160)] {
        let cam_px = cam_px_for_canvas(h);
        let burn_px = burn_px_for_canvas(h);
        let burn_margin = burn_margin_for_canvas(h); // #172: canvas-relative
        let rects = four_qr_rects(w, h, cam_px, burn_px, burn_margin);
        for (i, r) in rects.iter().enumerate() {
            assert!(
                r.x >= 0 && r.y >= 0 && r.x + r.w <= w && r.y + r.h <= h,
                "{} rect {r:?} is out of the {w}×{h} frame (burn_px={burn_px})",
                names[i]
            );
        }
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                assert!(
                    !rects[i].overlaps(&rects[j]),
                    "#172/#186 layout regression at {w}×{h} (cam_px={cam_px} burn_px={burn_px}): \
                     {} {:?} overlaps {} {:?} — a frame cannot carry two readable QRs in the same \
                     pixels (the readability/0-paired-frames bug)",
                    names[i],
                    rects[i],
                    names[j],
                    rects[j]
                );
            }
        }
    }
}

#[test]
fn imag_burn_does_not_overlap_the_four_existing_qrs_or_cam1_center_burn_463() {
    // #463 REGRESSION GUARD: imag's new bottom-CENTER-LEFT corner burn joins the existing four
    // QRs (camera L/R + strih BL + stream BR) AND the separate Rust-side cam1 center-bottom
    // capture burn (`qr::cam1_burn_origin`) — none of the FIVE rects may overlap, on every canvas
    // (mirrors `four_qr_rectangles_do_not_overlap_and_are_in_frame`'s multi-resolution style).
    let names = [
        "cam-left",
        "cam-right",
        "strih-burn",
        "stream-burn",
        "imag-burn",
        "cam1-burn",
    ];
    for (w, h) in [(1280i64, 720i64), (1920, 1080), (2560, 1440), (3840, 2160)] {
        let cam_px = cam_px_for_canvas(h);
        let burn_px = burn_px_for_canvas(h);
        let burn_margin = burn_margin_for_canvas(h);
        let four = four_qr_rects(w, h, cam_px, burn_px, burn_margin);
        let imag = imag_burn_rect(w, h, burn_px, burn_margin);

        // The cam1 capture burn: 320px square, horizontally centered, bottom-anchored with a
        // 24px margin (`qr::CAM1_BURN_QR_PX` / `CAM1_BURN_BOTTOM_MARGIN_PX`, production-fixed —
        // NOT canvas-relative, unlike the corner burns — so only exercised at the production
        // 1920×1080 canvas where its geometry is defined).
        let mut rects: Vec<Rect> = four.to_vec();
        rects.push(imag);
        if w == 1920 && h == 1080 {
            let cam1 = Rect {
                x: (1920 - 320) / 2,
                y: 1080 - 320 - 24,
                w: 320,
                h: 320,
            };
            rects.push(cam1);
        }

        for (i, r) in rects.iter().enumerate() {
            assert!(
                r.x >= 0 && r.y >= 0 && r.x + r.w <= w && r.y + r.h <= h,
                "{} rect {r:?} is out of the {w}×{h} frame (#463)",
                names[i]
            );
        }
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                assert!(
                    !rects[i].overlaps(&rects[j]),
                    "#463 layout regression at {w}×{h}: {} {:?} overlaps {} {:?}",
                    names[i],
                    rects[i],
                    names[j],
                    rects[j]
                );
            }
        }
    }
}

#[test]
fn four_corner_layout_all_four_qrs_decode_in_one_frame() {
    // THE end-to-end no-overlap proof: render the FULL composited stream frame —
    //   - camera dual-QR (top band) via the production Rust painter renderer
    //     (render_qr_dual_bgra, VAnchor::Top), and
    //   - strih burn (bottom-LEFT) + stream burn (bottom-RIGHT) via the production C++ burn
    //     renderer (burn_qr::render + burn_geom::corner_placement),
    // then assert the PRODUCTION recorded-file decoder (decode_qr_luma_all) reads back ALL
    // FOUR distinct payloads. On the old center-bottom-700 geometry the burns overwrote the
    // camera QRs and each other, so fewer than four decoded — this test is the regression
    // lock for the 4-corner placement.
    let dir = std::env::temp_dir().join(format!("burn_4corner_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main_cpp = dir.join("fourc_main.cpp");
    let bin = dir.join("fourc_main");
    let out_bgra = dir.join("frame.bgra");

    const W: u32 = 1920;
    const H: u32 = 1080;
    const CAM_PX: u32 = 700;
    // #186: the burn is now canvas-relative (0.28×height); on the 1080 production canvas
    // that is 302px (bigger modules than the old fixed 300 — survives the 4K stream upscale,
    // still clear of the dual-QR). This test renders + decodes the 1080 composite at that size.
    // (margin = burn_margin_for_canvas(1080) = 40, the production value.)
    const BURN_PX: u32 = 302;
    const MARGIN: u32 = 40;

    // 1) Camera dual-QR via the REAL Rust painter renderer (top-anchored).
    let cam_left = Payload {
        run_id: 720163,
        frame_id: 6518,
        gen_ts_ns: 1_718_600_000_000_000_000,
    };
    let cam_right = Payload {
        run_id: 720163,
        frame_id: 6519,
        gen_ts_ns: 1_718_600_000_016_666_666,
    };
    let mut frame = camera_box::probe::qr::render_qr_dual_bgra(&cam_left, &cam_right, W, H, CAM_PX);

    // 2) Overlay the two node burns via the REAL C++ burn renderer + corner geometry. The
    //    C++ harness reads the existing BGRA frame (the camera dual-QR), burns the strih QR
    //    bottom-left and the stream QR bottom-right, and writes it back — exactly what the
    //    DistroAV filter does to the composited program video on each box.
    std::fs::write(&out_bgra, &frame).unwrap();

    let payload_hpp = manifest().join(BURN_PAYLOAD_HPP);
    let qr_hpp = manifest().join(BURN_QR_HPP);
    let geom_hpp = manifest().join("vendor/distroav/src/burn-geom.hpp");
    let qrcg_cpp = manifest().join(QRCODEGEN_CPP);
    let qrcg_inc = manifest().join("vendor/distroav/src");

    let strih = (911002u32, 100u32, 1_718_600_000_033_333_333i64); // bottom-left
    let stream = (911004u32, 200u32, 1_718_600_000_050_000_000i64); // bottom-right

    let src = format!(
        r#"
#include "{payload}"
#include "{qr}"
#include "{geom}"
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <vector>
int main(int argc, char **argv) {{
    if (argc < 2) return 2;
    const uint32_t W = {w}, H = {h}, BURN_PX = {burn_px}, MARGIN = {margin};
    const size_t n = (size_t)W * H * 4;
    std::vector<uint8_t> buf(n);
    FILE *in = fopen(argv[1], "rb");
    if (!in) return 3;
    if (fread(buf.data(), 1, n, in) != n) {{ fclose(in); return 4; }}
    fclose(in);
    const uint32_t stride = W * 4;
    // strih = bottom-left, stream = bottom-right (exactly burn_draw_qr's path).
    std::string s = burn_payload::encode({sr}, {sf}, {sg});
    std::string t = burn_payload::encode({tr}, {tf}, {tg});
    auto bl = burn_geom::corner_placement(W, H, burn_geom::Corner::BottomLeft, BURN_PX, MARGIN);
    auto br = burn_geom::corner_placement(W, H, burn_geom::Corner::BottomRight, BURN_PX, MARGIN);
    burn_qr::render(buf.data(), stride, W, H, s, bl.band_x, bl.band_w, bl.band_cy, bl.square_px);
    burn_qr::render(buf.data(), stride, W, H, t, br.band_x, br.band_w, br.band_cy, br.square_px);
    FILE *out = fopen(argv[1], "wb");
    if (!out) return 5;
    fwrite(buf.data(), 1, n, out);
    fclose(out);
    printf("%s\n%s\n", s.c_str(), t.c_str());
    return 0;
}}
"#,
        payload = payload_hpp.display(),
        qr = qr_hpp.display(),
        geom = geom_hpp.display(),
        w = W,
        h = H,
        burn_px = BURN_PX,
        margin = MARGIN,
        sr = strih.0,
        sf = strih.1,
        sg = strih.2,
        tr = stream.0,
        tf = stream.1,
        tg = stream.2,
    );
    std::fs::write(&main_cpp, src).unwrap();

    let compile = Command::new("g++")
        .args(["-std=c++17", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg("-I")
        .arg(&qrcg_inc)
        .arg(&main_cpp)
        .arg(&qrcg_cpp)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("g++ must be installed");
    assert!(
        compile.status.success(),
        "4-corner render did not compile:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&bin)
        .arg(&out_bgra)
        .output()
        .expect("run 4corner main");
    assert!(
        run.status.success(),
        "4corner renderer exited nonzero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let burned: Vec<String> = String::from_utf8(run.stdout)
        .unwrap()
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(
        burned.len(),
        2,
        "expected 2 burned (strih+stream) payload strings"
    );

    // Decode the FULL composited frame and assert all FOUR distinct QRs read back.
    frame = std::fs::read(&out_bgra).unwrap();
    let luma = bgra_to_luma(&frame, W, H, W * 4);
    let decoded: Vec<String> = decode_qr_luma_all(luma)
        .iter()
        .map(|p| p.encode())
        .collect();

    let expected = [
        cam_left.encode(),
        cam_right.encode(),
        burned[0].clone(),
        burned[1].clone(),
    ];
    for e in &expected {
        assert!(
            decoded.contains(e),
            "#111: QR {e:?} did NOT decode from the 4-corner composite. Decoded {} of 4: \
             {decoded:?}. Some QRs overlap — the 4-corner layout is not readable.",
            decoded.len()
        );
    }
    assert!(
        decoded.len() >= 4,
        "#111: expected >=4 readable QRs in the composite, got {}: {decoded:?}",
        decoded.len()
    );
}

#[test]
fn five_corner_layout_including_imag_burn_decodes_in_one_frame_463() {
    // #463 end-to-end no-overlap proof, extending `four_corner_layout_all_four_qrs_decode_in_one_
    // frame`: render the camera dual-QR + burn strih (bottom-left) + stream (bottom-right) +
    // imag (bottom-CENTER-LEFT, the NEW corner) via the REAL C++ renderer + corner geometry, then
    // assert the PRODUCTION recorded-file decoder reads back ALL FIVE distinct payloads. If the
    // new BottomCenterLeft placement collided with strih's corner (or the camera QR), fewer than
    // five would decode — this is the regression lock for #463's 3-corner layout.
    let dir = std::env::temp_dir().join(format!("burn_5corner_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main_cpp = dir.join("fivec_main.cpp");
    let bin = dir.join("fivec_main");
    let out_bgra = dir.join("frame.bgra");

    const W: u32 = 1920;
    const H: u32 = 1080;
    const CAM_PX: u32 = 700;
    const BURN_PX: u32 = 302;
    const MARGIN: u32 = 40;

    // 1) Camera dual-QR via the REAL Rust painter renderer (top-anchored).
    let cam_left = Payload {
        run_id: 720163,
        frame_id: 7518,
        gen_ts_ns: 1_718_600_100_000_000_000,
    };
    let cam_right = Payload {
        run_id: 720163,
        frame_id: 7519,
        gen_ts_ns: 1_718_600_100_016_666_666,
    };
    let mut frame = camera_box::probe::qr::render_qr_dual_bgra(&cam_left, &cam_right, W, H, CAM_PX);
    std::fs::write(&out_bgra, &frame).unwrap();

    let payload_hpp = manifest().join(BURN_PAYLOAD_HPP);
    let qr_hpp = manifest().join(BURN_QR_HPP);
    let geom_hpp = manifest().join("vendor/distroav/src/burn-geom.hpp");
    let qrcg_cpp = manifest().join(QRCODEGEN_CPP);
    let qrcg_inc = manifest().join("vendor/distroav/src");

    let strih = (911002u32, 300u32, 1_718_600_100_033_333_333i64); // bottom-left
    let stream = (911004u32, 400u32, 1_718_600_100_050_000_000i64); // bottom-right
    let imag = (911003u32, 500u32, 1_718_600_100_066_666_666i64); // bottom-center-left (#463)

    // 2) Overlay the THREE node burns via the REAL C++ burn renderer + corner geometry —
    //    strih/stream/imag, exactly what the DistroAV filter does to program video on each box.
    let src = format!(
        r#"
#include "{payload}"
#include "{qr}"
#include "{geom}"
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <vector>
int main(int argc, char **argv) {{
    if (argc < 2) return 2;
    const uint32_t W = {w}, H = {h}, BURN_PX = {burn_px}, MARGIN = {margin};
    const size_t n = (size_t)W * H * 4;
    std::vector<uint8_t> buf(n);
    FILE *in = fopen(argv[1], "rb");
    if (!in) return 3;
    if (fread(buf.data(), 1, n, in) != n) {{ fclose(in); return 4; }}
    fclose(in);
    const uint32_t stride = W * 4;
    std::string s = burn_payload::encode({sr}, {sf}, {sg});
    std::string t = burn_payload::encode({tr}, {tf}, {tg});
    std::string m = burn_payload::encode({mr}, {mf}, {mg});
    auto bl = burn_geom::corner_placement(W, H, burn_geom::Corner::BottomLeft, BURN_PX, MARGIN);
    auto br = burn_geom::corner_placement(W, H, burn_geom::Corner::BottomRight, BURN_PX, MARGIN);
    auto bcl = burn_geom::corner_placement(W, H, burn_geom::Corner::BottomCenterLeft, BURN_PX, MARGIN);
    burn_qr::render(buf.data(), stride, W, H, s, bl.band_x, bl.band_w, bl.band_cy, bl.square_px);
    burn_qr::render(buf.data(), stride, W, H, t, br.band_x, br.band_w, br.band_cy, br.square_px);
    burn_qr::render(buf.data(), stride, W, H, m, bcl.band_x, bcl.band_w, bcl.band_cy, bcl.square_px);
    FILE *out = fopen(argv[1], "wb");
    if (!out) return 5;
    fwrite(buf.data(), 1, n, out);
    fclose(out);
    printf("%s\n%s\n%s\n", s.c_str(), t.c_str(), m.c_str());
    return 0;
}}
"#,
        payload = payload_hpp.display(),
        qr = qr_hpp.display(),
        geom = geom_hpp.display(),
        w = W,
        h = H,
        burn_px = BURN_PX,
        margin = MARGIN,
        sr = strih.0,
        sf = strih.1,
        sg = strih.2,
        tr = stream.0,
        tf = stream.1,
        tg = stream.2,
        mr = imag.0,
        mf = imag.1,
        mg = imag.2,
    );
    std::fs::write(&main_cpp, src).unwrap();

    let compile = Command::new("g++")
        .args(["-std=c++17", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg("-I")
        .arg(&qrcg_inc)
        .arg(&main_cpp)
        .arg(&qrcg_cpp)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("g++ must be installed");
    assert!(
        compile.status.success(),
        "5-corner (#463) render did not compile:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&bin)
        .arg(&out_bgra)
        .output()
        .expect("run 5corner main");
    assert!(
        run.status.success(),
        "5corner renderer exited nonzero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let burned: Vec<String> = String::from_utf8(run.stdout)
        .unwrap()
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(
        burned.len(),
        3,
        "expected 3 burned (strih+stream+imag) payload strings"
    );

    // Decode the FULL composited frame and assert all FIVE distinct QRs read back.
    frame = std::fs::read(&out_bgra).unwrap();
    let luma = bgra_to_luma(&frame, W, H, W * 4);
    let decoded: Vec<String> = decode_qr_luma_all(luma)
        .iter()
        .map(|p| p.encode())
        .collect();

    let expected = [
        cam_left.encode(),
        cam_right.encode(),
        burned[0].clone(),
        burned[1].clone(),
        burned[2].clone(),
    ];
    for e in &expected {
        assert!(
            decoded.contains(e),
            "#463: QR {e:?} did NOT decode from the 5-corner composite (cam L/R + strih + \
             stream + imag). Decoded {} of 5: {decoded:?}. Some QRs overlap — the new \
             BottomCenterLeft placement is not readable.",
            decoded.len()
        );
    }
    assert!(
        decoded.len() >= 5,
        "#463: expected >=5 readable QRs in the composite, got {}: {decoded:?}",
        decoded.len()
    );
}

#[test]
fn burn_geom_corner_from_string_and_tiny_frame_clamp() {
    // Compile + run a tiny C++ harness over burn-geom.hpp asserting:
    //   1. corner_from_string parses EVERY documented OBS_BURN_CORNER form correctly —
    //      especially the long forms "bottom-right"/"bottom-left" (the old per-2nd-char
    //      parse mapped "bottom-right" to BottomLeft → stream would burn into strih's
    //      corner and re-collide; the #111 review caught this).
    //   2. corner_placement does NOT underflow uint32 on a degenerate tiny frame
    //      (frame < margin + side): band_x/band_cy must clamp to in-frame, not wrap to a
    //      huge off-frame coordinate.
    let dir = std::env::temp_dir().join(format!("burn_geom_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main_cpp = dir.join("geom_main.cpp");
    let bin = dir.join("geom_main");
    let geom_hpp = manifest().join("vendor/distroav/src/burn-geom.hpp");

    let src = format!(
        r#"
#include "{geom}"
#include <cstdint>
#include <cstdio>
#include <initializer_list>
using burn_geom::Corner;
using burn_geom::corner_from_string;
using burn_geom::corner_placement;
static int fails = 0;
static void want(const char* s, Corner dflt, Corner exp, const char* label) {{
    Corner got = corner_from_string(s, dflt);
    if (got != exp) {{ printf("FAIL %s: corner_from_string(%s) wrong\n", label, s?s:"<null>"); ++fails; }}
}}
int main() {{
    // Long forms (the regression) — independent of the default.
    want("bottom-right", Corner::BottomLeft, Corner::BottomRight, "bottom-right");
    want("bottom-left", Corner::BottomRight, Corner::BottomLeft, "bottom-left");
    want("bottom_right", Corner::BottomLeft, Corner::BottomRight, "bottom_right");
    want("Bottom-Right", Corner::BottomLeft, Corner::BottomRight, "Bottom-Right(case)");
    // Short + word forms.
    want("br", Corner::BottomLeft, Corner::BottomRight, "br");
    want("bl", Corner::BottomRight, Corner::BottomLeft, "bl");
    want("right", Corner::BottomLeft, Corner::BottomRight, "right");
    want("left", Corner::BottomRight, Corner::BottomLeft, "left");
    want("r", Corner::BottomLeft, Corner::BottomRight, "r");
    want("l", Corner::BottomRight, Corner::BottomLeft, "l");
    // Null/empty/garbage → default.
    want(nullptr, Corner::BottomRight, Corner::BottomRight, "null->dflt");
    want("", Corner::BottomLeft, Corner::BottomLeft, "empty->dflt");
    want("xyz", Corner::BottomRight, Corner::BottomRight, "garbage->dflt");

    // Tiny-frame clamp: frame smaller than margin+side must not underflow.
    auto p = corner_placement(40, 30, Corner::BottomRight, 300, 40);
    if (p.band_x > 40) {{ printf("FAIL tiny: band_x=%u underflowed\n", p.band_x); ++fails; }}
    if (p.band_cy > 30) {{ printf("FAIL tiny: band_cy=%u underflowed\n", p.band_cy); ++fails; }}
    // Production size sanity: BR burn lands flush against the right/bottom margins.
    auto q = corner_placement(1920, 1080, Corner::BottomRight, 300, 40);
    if (q.band_x != 1920 - 40 - q.square_px) {{ printf("FAIL prod BR band_x=%u\n", q.band_x); ++fails; }}
    auto z = corner_placement(1920, 1080, Corner::BottomLeft, 300, 40);
    if (z.band_x != 40) {{ printf("FAIL prod BL band_x=%u\n", z.band_x); ++fails; }}

    // #463: BottomCenterLeft (imag's slot) — one margin clear of BottomLeft's trailing edge
    // (margin + side + margin), same vertical row as the other corners.
    auto bcl = corner_placement(1920, 1080, Corner::BottomCenterLeft, 300, 40);
    if (bcl.band_x != 40 + 300 + 40) {{
        printf("FAIL prod BCL band_x=%u (want %u)\n", bcl.band_x, 40u + 300u + 40u);
        ++fails;
    }}
    if (bcl.band_cy != z.band_cy) {{
        printf("FAIL BCL band_cy=%u differs from BL band_cy=%u (same row expected)\n",
               bcl.band_cy, z.band_cy);
        ++fails;
    }}
    // BCL must not overlap BL (its right edge must be >= BL's right edge) nor extend past a
    // reasonable canvas-center bound (well short of 960, the 1920-wide canvas midpoint).
    if (bcl.band_x < z.band_x + z.square_px) {{
        printf("FAIL BCL band_x=%u overlaps BL right edge=%u\n", bcl.band_x, z.band_x + z.square_px);
        ++fails;
    }}
    if (bcl.band_x + bcl.square_px >= 800) {{
        printf("FAIL BCL right edge=%u reaches the cam1 center-burn zone (starts at x=800)\n",
               bcl.band_x + bcl.square_px);
        ++fails;
    }}
    // Tiny-frame clamp for the NEW corner too — must not underflow.
    auto tiny_bcl = corner_placement(40, 30, Corner::BottomCenterLeft, 300, 40);
    if (tiny_bcl.band_x > 40) {{
        printf("FAIL tiny BCL: band_x=%u underflowed\n", tiny_bcl.band_x);
        ++fails;
    }}
    if (tiny_bcl.band_cy > 30) {{
        printf("FAIL tiny BCL: band_cy=%u underflowed\n", tiny_bcl.band_cy);
        ++fails;
    }}

    // #186 / #172: canvas-relative burn px + margin (burn_qr_px_for_canvas / burn_margin_for_canvas).
    using burn_geom::burn_qr_px_for_canvas;
    using burn_geom::burn_margin_for_canvas;
    // An absolute OBS_BURN_QR_PX override (non-zero `configured`) is returned verbatim.
    if (burn_qr_px_for_canvas(444, 1080) != 444) {{ printf("FAIL override\n"); ++fails; }}
    // Auto (configured=0): 0.28*h. 1080 -> 302 (≈ old 300, clears dual-QR);
    // 4K (2160) -> 604 (the soft 4K stream burn is now ~2x, crisp through the upscale).
    uint32_t px1080 = burn_qr_px_for_canvas(0, 1080);
    uint32_t px2160 = burn_qr_px_for_canvas(0, 2160);
    if (px1080 != 302) {{ printf("FAIL auto1080=%u (want 302)\n", px1080); ++fails; }}
    if (px2160 != 604) {{ printf("FAIL auto2160=%u (want 604)\n", px2160); ++fails; }}
    if (px2160 <= 300) {{ printf("FAIL 4K burn not enlarged: %u\n", px2160); ++fails; }}
    // Tiny canvas floors at 64 (still a readable burn).
    if (burn_qr_px_for_canvas(0, 100) != 64) {{ printf("FAIL tiny floor\n"); ++fails; }}
    // CLEARANCE (#172): the auto burn (bottom-anchored, canvas-relative margin) must clear a
    // top-anchored camera dual-QR (700px on 1080, scaled by h/1080) on EVERY canvas — its top
    // row must sit at or below the dual-QR bottom (24*scale + 700*scale). 720 is the case the
    // old fixed-40 margin failed.
    for (uint32_t h : {{(uint32_t)720, (uint32_t)1080, (uint32_t)1440, (uint32_t)2160}}) {{
        uint32_t cam_bottom = (24u * h) / 1080u + (700u * h) / 1080u;
        uint32_t bpx = burn_qr_px_for_canvas(0, h);
        uint32_t bmargin = burn_margin_for_canvas(h);
        auto pl = corner_placement(1920u * h / 1080u, h, Corner::BottomLeft, bpx, bmargin);
        uint32_t burn_top = pl.band_cy - pl.square_px / 2;
        if (burn_top < cam_bottom) {{
            printf("FAIL clearance h=%u: burn_top=%u < cam_bottom=%u\n", h, burn_top, cam_bottom);
            ++fails;
        }}
    }}

    printf("DONE fails=%d\n", fails);
    return fails == 0 ? 0 : 1;
}}
"#,
        geom = geom_hpp.display(),
    );
    std::fs::write(&main_cpp, src).unwrap();

    let compile = Command::new("g++")
        .args(["-std=c++17", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(&main_cpp)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("g++ must be installed");
    assert!(
        compile.status.success(),
        "burn-geom.hpp corner/clamp harness did not compile:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );
    let run = Command::new(&bin).output().expect("run geom harness");
    assert!(
        run.status.success(),
        "#111 corner-parse / tiny-frame-clamp regression:\n{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

// ---- 2. Vendored-source / build-wiring guards --------------------------------

#[test]
fn burn_payload_header_is_vendored() {
    let hpp = squish(&vendor_file(BURN_PAYLOAD_HPP));
    assert!(
        hpp.contains("namespace burn_payload"),
        "{BURN_PAYLOAD_HPP}: the C++ burn payload encoder is missing — #111 reverted."
    );
    // The wire format + CRC must match payload.rs (the decoder's contract).
    assert!(
        hpp.contains("crc32_iso_hdlc") && hpp.contains("0xEDB88320"),
        "{BURN_PAYLOAD_HPP}: CRC-32/ISO-HDLC (reflected poly 0xEDB88320) missing — the \
         burned CRC would not match the Rust decoder."
    );
    assert!(
        hpp.contains(r#""P" + b"#) || hpp.contains(r#""P""#),
        "{BURN_PAYLOAD_HPP}: the 'P' wire prefix is gone — Payload::decode strips 'P'."
    );
}

#[test]
fn burn_filter_source_is_vendored_and_wired() {
    let flt = squish(&vendor_file(BURN_FILTER));
    // The filter is an OBS effect filter that burns the QR into the rendered output
    // each frame, using the shared encoder + clock.
    assert!(
        flt.contains("burn_payload::encode"),
        "{BURN_FILTER}: the burn filter no longer calls burn_payload::encode — it would \
         burn the wrong/no payload. #111 reverted."
    );
    assert!(
        flt.contains("burn_clock::gen_ts_ns"),
        "{BURN_FILTER}: the burn filter no longer stamps the RAW render-instant wall-clock \
         gen_ts_ns (burn_clock::gen_ts_ns); the node stamp would not share cam2's RAW \
         painter timebase and #108's cam→strih subtraction would be biased (finding #2)."
    );
    // #257: node identity (run_id) is derived from the HOST ROLE (no OBS_BURN_RUN_ID env), with the
    // reserved strih/stream defaults that sit OUTSIDE cam2's range so #108 distinguishes nodes.
    assert!(
        flt.contains("burn_host_is_stream"),
        "{BURN_FILTER}: #257 — the per-node run_id is no longer derived from the host role \
         (burn_host_is_stream); re-apply."
    );
    assert!(
        flt.contains("911002"),
        "{BURN_FILTER}: the reserved strih node run_id default (911002) is gone."
    );
    assert!(
        flt.contains("911004"),
        "{BURN_FILTER}: the reserved stream node run_id default (911004) is gone."
    );
    // #463: imag-nb (Topology v2, EPIC #466) gets its OWN reserved run_id + host-role predicate +
    // bottom-CENTER-LEFT corner — the third node in the burn filter's host-role dispatch.
    assert!(
        flt.contains("911003"),
        "{BURN_FILTER}: the reserved imag node run_id default (911003, #463) is gone."
    );
    assert!(
        flt.contains("burn_host_is_imag"),
        "{BURN_FILTER}: #463 — the imag host-role predicate (burn_host_is_imag) is gone; \
         imag-nb would fall through to strih's run_id/corner and collide with strih's burn."
    );
    assert!(
        flt.contains("Corner::BottomCenterLeft"),
        "{BURN_FILTER}: #463 — imag's bottom-center-left corner assignment is gone; imag would \
         burn into strih's or stream's corner and collide with it."
    );
    // #257: the burn is gated by the per-source genlock_burn bool (read LIVE from the parent each
    // render), NOT an OBS_BURN_QR env. The env reads MUST be gone; the runtime gate must be present.
    assert!(
        !flt.contains("getenv(\"OBS_BURN"),
        "{BURN_FILTER}: #257 — an OBS_BURN_* env read is BACK; the burn is a per-source bool (no env)."
    );
    assert!(
        flt.contains("obs_source_get_genlock_burn"),
        "{BURN_FILTER}: #257 — the burn no longer reads the parent source's genlock_burn flag \
         (obs_source_get_genlock_burn); the runtime gate is inert. Re-apply."
    );
    // (#111 4-corner layout) The burn MUST be placed in this node's bottom CORNER via the
    // shared corner geometry, NOT the old full-width center-bottom strip — otherwise the
    // strih + stream burns overlap each other (strih→stream 0 paired frames) and the camera
    // dual-QR. Guard the corner-placement call + per-node corner resolution.
    assert!(
        flt.contains("burn_geom::corner_placement"),
        "{BURN_FILTER}: the burn no longer uses burn_geom::corner_placement — it would draw \
         a center-bottom strip that overlaps the other node's burn and the camera QR (the \
         #111 readability/0-paired-frames bug). #111 4-corner layout reverted."
    );
    assert!(
        flt.contains("OBS_BURN_CORNER") || flt.contains("Corner::BottomRight"),
        "{BURN_FILTER}: the per-node bottom corner (strih=left, stream=right) is gone — both \
         nodes would burn into the same corner and overlap. #111 reverted."
    );
    // It renders the target then draws the QR (the texrender/stage pattern, or a
    // texture overlay) — assert it actually renders the source through, not replaces it.
    assert!(
        flt.contains("obs_source_process_filter") || flt.contains("gs_texrender_begin"),
        "{BURN_FILTER}: the filter does not render its target through — the burned output \
         would lose the underlying video."
    );
    // (review C1) The output sprite draw MUST be sRGB-correct (mirror libobs
    // render_filter_tex) or the composited program video is gamma-shifted downstream into
    // the recording under OBS's default linear-sRGB pipeline. Guard the sRGB-aware path so
    // a refactor/subtree-pull can't silently drop it again.
    assert!(
        flt.contains("gs_get_linear_srgb") && flt.contains("gs_effect_set_texture_srgb"),
        "{BURN_FILTER}: the output draw is not sRGB-correct (gs_get_linear_srgb + \
         gs_effect_set_texture_srgb missing) — the burned program video would be \
         color-shifted in the recording (review C1)."
    );
}

#[test]
fn burn_geom_header_is_vendored() {
    // The #111 4-corner placement geometry must stay vendored + freestanding so the burn
    // filter and the parity test share ONE tested corner-placement implementation. A
    // subtree pull (#44) that drops it would revert the no-overlap layout.
    let geom = squish(&vendor_file("vendor/distroav/src/burn-geom.hpp"));
    assert!(
        geom.contains("namespace burn_geom") && geom.contains("corner_placement"),
        "vendor/distroav/src/burn-geom.hpp: the #111 corner-placement geometry is missing — \
         the burn would fall back to a center strip that overlaps the other QRs."
    );
    assert!(
        geom.contains("BottomCenterLeft"),
        "vendor/distroav/src/burn-geom.hpp: #463 — imag's bottom-center-left corner is gone \
         from the Corner enum / corner_placement's cases."
    );
    assert!(
        geom.contains("BottomLeft") && geom.contains("BottomRight"),
        "vendor/distroav/src/burn-geom.hpp: the per-node bottom corners are gone."
    );
}

#[test]
fn burn_filter_is_registered_in_plugin_main() {
    let pm = squish(&vendor_file(PLUGIN_MAIN));
    assert!(
        pm.contains("create_ndi_burn_filter_info"),
        "{PLUGIN_MAIN}: the burn filter is not registered (create_ndi_burn_filter_info / \
         obs_register_source); OBS would not expose it. #111 reverted."
    );
}

#[test]
fn burn_filter_is_in_the_distroav_cmake_sources() {
    let cm = squish(&vendor_file(DISTROAV_CMAKE));
    assert!(
        cm.contains("src/ndi-burn-filter.cpp"),
        "{DISTROAV_CMAKE}: ndi-burn-filter.cpp is not in target_sources — it would not \
         compile into distroav.dll. #111 build-wiring reverted."
    );
}

#[test]
fn windows_genlock_workflow_gates_on_the_burn_filter() {
    // Lock-step with the other vendored-patch guards: the production Windows build
    // re-asserts the #111 burn-filter tokens in pwsh BEFORE the 150-min build, since
    // this Linux Rust guard can't compile on the windows runner. A subtree bump (#44)
    // that drops the burn filter then fails HERE, fast, not after a 150-min build.
    let wf = squish(&vendor_file(WINDOWS_GENLOCK_WF));
    assert!(
        wf.contains("ndi-burn-filter.cpp") || wf.contains("create_ndi_burn_filter_info"),
        "{WINDOWS_GENLOCK_WF}: #111 — the production build no longer asserts the QR burn \
         filter is present; re-add the pwsh #111 gate."
    );
    assert!(
        wf.contains("burn_payload::encode"),
        "{WINDOWS_GENLOCK_WF}: #111 — the production build no longer asserts the burn \
         filter calls the shared byte-identical encoder; re-add the pwsh #111 gate."
    );
}
