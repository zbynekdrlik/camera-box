//! issue 1370 — EXECUTABLE parity gate: the Rust burn-slot table vs the shipped C++ geometry.
//!
//! [`camera_box::burn_regions`] is the ONE Rust copy of where the DistroAV burn filter draws a
//! corner burn. The recording decode crops those slots to recover a burn the tiles miss, and the
//! colour gate dodges them. This test COMPILES the shipped `vendor/distroav/src/burn-geom.hpp` —
//! never a retyped copy — and runs the filter's own call shape for every corner on production,
//! 720p, 4K and narrow canvases: `burn_qr_px_for_canvas(0, h)` + `burn_margin_for_canvas(h)` +
//! `corner_placement`. It asserts the Rust slot is exactly the square `burn_qr::render` centres in
//! that band: x = `band_x`, side = `square_px`, top = `band_cy - square_px / 2`.
//!
//! `c++` is required (present on every runner and dev box). Per test-strictness this FAILS
//! LOUDLY rather than skipping when the toolchain is missing.

use camera_box::burn_regions::{slot_rect, BurnSlot};
use std::path::PathBuf;
use std::process::Command;

const CORNERS: [(BurnSlot, &str); 4] = [
    (BurnSlot::BottomLeft, "BottomLeft"),
    (BurnSlot::BottomRight, "BottomRight"),
    (BurnSlot::BottomCenterLeft, "BottomCenterLeft"),
    (BurnSlot::BottomCenterRight, "BottomCenterRight"),
];

/// Production, 720p (odd burn side 201), 4K, a short canvas, and 1080-high narrow canvases that
/// reach every BottomCenterLeft / BottomCenterRight fallback tier.
const CANVASES: [(u32, u32); 10] = [
    (1920, 1080),
    (1280, 720),
    (3840, 2160),
    (1920, 200),
    (1200, 1080),
    (900, 1080),
    (650, 1080),
    (600, 1080),
    (500, 1080),
    (320, 180),
];

fn harness_source() -> String {
    let hpp = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/distroav/src/burn-geom.hpp");
    format!(
        r#"
#include "{hpp}"
#include <cstdio>
#include <cstdlib>
int main(int argc, char **argv)
{{
    const burn_geom::Corner corners[4] = {{
        burn_geom::Corner::BottomLeft, burn_geom::Corner::BottomRight,
        burn_geom::Corner::BottomCenterLeft, burn_geom::Corner::BottomCenterRight}};
    for (int i = 1; i + 1 < argc; i += 2) {{
        const uint32_t w = (uint32_t)strtoul(argv[i], nullptr, 10);
        const uint32_t h = (uint32_t)strtoul(argv[i + 1], nullptr, 10);
        const uint32_t margin = burn_geom::burn_margin_for_canvas(h);
        const uint32_t qr_px = burn_geom::burn_qr_px_for_canvas(0, h);
        for (int c = 0; c < 4; c++) {{
            const burn_geom::Placement p = burn_geom::corner_placement(w, h, corners[c], qr_px, margin);
            printf("%u %u %u %u\n", p.band_x, p.band_w, p.band_cy, p.square_px);
        }}
    }}
    return 0;
}}
"#,
        hpp = hpp.display()
    )
}

/// Compile + run the harness over [`CANVASES`]; one `(band_x, band_w, band_cy, square_px)` per
/// canvas × corner, in [`CORNERS`] order.
fn cpp_placements() -> Vec<[u32; 4]> {
    let dir = std::env::temp_dir().join(format!("burn_regions_parity_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("burn_geom_main.cpp");
    let bin = dir.join("burn_geom_main");
    std::fs::write(&src, harness_source()).unwrap();
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let out = Command::new(&cxx)
        .args(["-std=c++17", "-O1", "-Wall", "-Wextra", "-Werror"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("issue 1370: could not run the C++ compiler `{cxx}` ({e})"));
    assert!(
        out.status.success(),
        "issue 1370: burn-geom.hpp harness failed to compile:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut args = Vec::new();
    for (w, h) in CANVASES {
        args.push(w.to_string());
        args.push(h.to_string());
    }
    let run = Command::new(&bin)
        .args(&args)
        .output()
        .expect("run harness");
    assert!(run.status.success(), "harness exited {:?}", run.status);
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            let v: Vec<u32> = l.split(' ').map(|t| t.parse().unwrap()).collect();
            [v[0], v[1], v[2], v[3]]
        })
        .collect()
}

#[test]
fn every_corner_slot_is_the_square_the_burn_filter_draws_1370() {
    let cpp = cpp_placements();
    assert_eq!(cpp.len(), CANVASES.len() * CORNERS.len());
    let mut rows = cpp.iter();
    for (w, h) in CANVASES {
        for (slot, name) in CORNERS {
            let [band_x, band_w, band_cy, square] = *rows.next().unwrap();
            assert_eq!(
                band_w, square,
                "{w}x{h} {name}: the band is exactly one QR wide"
            );
            let r = slot_rect(slot, w, h).expect("non-empty canvas");
            assert_eq!(
                (r.x, r.y, r.w, r.h),
                (band_x, band_cy - square / 2, square, square),
                "{w}x{h} {name}: burn_regions slot must be the square burn-geom.hpp places \
                 (band_x={band_x} band_cy={band_cy} square={square})"
            );
        }
    }
}
