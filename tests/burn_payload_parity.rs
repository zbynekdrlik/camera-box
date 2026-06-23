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

#[test]
fn four_qr_rectangles_do_not_overlap_and_are_in_frame() {
    // #111 REGRESSION GUARD: the four QRs (camera L/R top + strih burn bottom-left + stream
    // burn bottom-right) MUST NOT overlap and MUST be in-frame. The old geometry (camera
    // QR vertically centered + a SINGLE center-bottom burn at ~700px, shared by BOTH nodes)
    // overlapped the camera QR (covered ~220px of each half) AND drew both nodes' burns in
    // the same pixels — so a stream recording carried 0 readable paired frames. This pure
    // geometry check fails on that layout and passes only when all four are disjoint.
    const W: i64 = 1920;
    const H: i64 = 1080;
    const CAM_PX: i64 = 700; // recording-e2e.sh QR_SIZE default
    const BURN_PX: i64 = 300; // resolve_qr_px() default
    const BURN_MARGIN: i64 = 40; // burn_draw_qr margin
    let rects = four_qr_rects(W, H, CAM_PX, BURN_PX, BURN_MARGIN);
    let names = ["cam-left", "cam-right", "strih-burn", "stream-burn"];

    for (i, r) in rects.iter().enumerate() {
        assert!(
            r.x >= 0 && r.y >= 0 && r.x + r.w <= W && r.y + r.h <= H,
            "{} rect {r:?} is out of the {W}×{H} frame",
            names[i]
        );
    }
    for i in 0..rects.len() {
        for j in (i + 1)..rects.len() {
            assert!(
                !rects[i].overlaps(&rects[j]),
                "#111 layout regression: {} {:?} overlaps {} {:?} — a frame cannot carry two \
                 readable QRs in the same pixels (this is the readability/0-paired-frames bug)",
                names[i],
                rects[i],
                names[j],
                rects[j]
            );
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
    const BURN_PX: u32 = 300;
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
    // Node identity: the run_id is env-overridable (OBS_BURN_RUN_ID) with the reserved
    // strih/stream defaults that sit OUTSIDE cam2's range so #108 distinguishes nodes.
    assert!(
        flt.contains("OBS_BURN_RUN_ID"),
        "{BURN_FILTER}: the per-node run_id is no longer env-configurable via \
         OBS_BURN_RUN_ID; #108 cannot distinguish node-stamp from cam2-stamp. #111 reverted."
    );
    assert!(
        flt.contains("911002"),
        "{BURN_FILTER}: the reserved strih node run_id default (911002) is gone."
    );
    assert!(
        flt.contains("911004"),
        "{BURN_FILTER}: the reserved stream node run_id default (911004) is gone."
    );
    // Gated behind an env var, default OFF on prod (mirrors OBS_GENLOCK_WALL_CLOCK).
    assert!(
        flt.contains("OBS_BURN_QR"),
        "{BURN_FILTER}: the burn is no longer gated behind an env var (OBS_BURN_QR); it \
         must default OFF so the production install is unaffected until #108 enables it."
    );
    // The QR pixel size is tunable per-rig (OBS_BURN_QR_PX) so the operator can pick a
    // size that survives the NDI re-compression at the OBS outputs.
    assert!(
        flt.contains("OBS_BURN_QR_PX"),
        "{BURN_FILTER}: the OBS_BURN_QR_PX size override is gone — the rig can no longer \
         tune the NDI-survivable QR size. #111 reverted."
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
