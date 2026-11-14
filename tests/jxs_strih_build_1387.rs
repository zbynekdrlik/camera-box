//! Issue 1387 slice 2 -- the CI wiring of the JPEG XS source (std-only, every build):
//! the strih Linux variant builds the jxs-recv-ffi static library (Rust + nasm), hands it and
//! rustc's native-library list to DistroAV's CMake, and refuses a distroav.so without the source
//! or with an executable stack; no other DistroAV build turns the source on; a new vstack pin
//! re-runs the strih build.

#[path = "support/vstack.rs"]
mod vstack;

use std::path::Path;

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The text of one job of a workflow: from `  <name>:` to the next top-level job.
fn job<'a>(wf: &'a str, name: &str) -> &'a str {
    let head = format!("\n  {name}:\n");
    let start = wf.find(&head).unwrap_or_else(|| panic!("job {name}")) + 1;
    let rest = &wf[start + head.len() - 1..];
    let end = rest
        .match_indices("\n  ")
        .find(|(i, _)| {
            let after = &rest[i + 3..];
            !after.starts_with(' ') && !after.starts_with('#') && after.contains(':')
        })
        .map_or(rest.len(), |(i, _)| i);
    &wf[start..start + head.len() - 1 + end]
}

/// One step of a job, from its `- name:` line to the next step.
fn step<'a>(job: &'a str, name: &str) -> &'a str {
    let head = format!("- name: {name}");
    let start = job.find(&head).unwrap_or_else(|| panic!("step `{name}`"));
    let end = job[start + head.len()..]
        .find("\n      - ")
        .map_or(job.len(), |i| start + head.len() + i);
    &job[start..end]
}

const WF: &str = ".github/workflows/linux-genlock.yml";

/// CI's first strih build (run 40909341652) printed the native-libs note in colour
/// (`ESC[1mESC[92mnote ESC[0mESC[1m: native-static-libs: ...`), so the `^note:` sed matched
/// nothing and the step refused a perfectly good library. The step must turn colour off.
#[test]
fn the_static_library_step_prints_the_native_libs_note_without_colour() {
    let wf = read(WF);
    let strih = job(&wf, "linux-genlock-build-strih");
    let ffi = step(strih, "Build the jxs-recv-ffi static library (issue 1387)");
    assert!(
        ffi.contains("CARGO_TERM_COLOR: never"),
        "the FFI build step must set CARGO_TERM_COLOR: never so the native-libs note is plain text: {ffi}"
    );
}

#[test]
fn the_strih_build_makes_the_static_library_and_hands_it_to_distroav() {
    let wf = read(WF);
    let strih = job(&wf, "linux-genlock-build-strih");
    let apt = step(strih, "Install build dependencies (apt)");
    assert!(apt.contains(" nasm"), "nasm for SVT-JPEG-XS: {apt}");
    assert!(
        apt.contains(" libclang-dev"),
        "bindgen for the v4l2 sys crate: {apt}"
    );
    assert!(strih.contains("uses: dtolnay/rust-toolchain@stable"));
    let ffi = step(strih, "Build the jxs-recv-ffi static library (issue 1387)");
    for needle in [
        "cargo rustc --locked --release -p jxs-recv-ffi --crate-type staticlib -- --print native-static-libs",
        "sed -n 's/^note: native-static-libs: //p'",
        "echo \"JXS_FFI_NATIVE_LIBS=$libs\" >> \"$GITHUB_ENV\"",
        "target/release/libjxs_recv_ffi.a",
    ] {
        assert!(ffi.contains(needle), "the FFI build step lacks `{needle}`");
    }
    let cfg = step(strih, "Configure DistroAV against the genlocked OBS SDK");
    for needle in [
        "-DCAMERA_BOX_JXS_SOURCE=ON",
        "-DCAMERA_BOX_JXS_FFI_LIB=\"$VSTACK_SRC/target/release/libjxs_recv_ffi.a\"",
        "-DCAMERA_BOX_JXS_FFI_INCLUDE=\"$VSTACK_SRC/jxs/recv-ffi/include\"",
        "-DCAMERA_BOX_JXS_FFI_NATIVE_LIBS=\"$JXS_FFI_NATIVE_LIBS\"",
    ] {
        assert!(
            cfg.contains(needle),
            "the DistroAV configure lacks `{needle}`"
        );
    }
    // the library is built before DistroAV is configured
    assert!(
        strih.find("- name: Build the jxs-recv-ffi").unwrap()
            < strih.find("- name: Configure DistroAV").unwrap()
    );
    let stage = step(
        strih,
        "Stage artifact (OBS rundir + distroav.so + data + build SHA + strih flags marker)",
    );
    assert!(stage.contains("grep -aq 'camera_box_jxs_source' \"$distroav_so\""));
    assert!(stage.contains("readelf -lW \"$distroav_so\""));
    assert!(stage.contains("GNU_STACK"));
}

#[test]
fn no_other_distroav_build_turns_the_source_on() {
    let wf = read(WF);
    assert_eq!(wf.matches("-DCAMERA_BOX_JXS_SOURCE=ON").count(), 1);
    for other in ["linux-distroav-compile-check", "linux-genlock-build"] {
        assert!(!job(&wf, other).contains("CAMERA_BOX_JXS"), "{other}");
    }
}

#[test]
fn a_receiver_change_reruns_the_strih_build() {
    let wf = read(WF);
    let paths = &wf[wf.find("    paths:").unwrap()..wf.find("\npermissions:").unwrap()];
    for p in ["'scripts/vstack-release.pin'", "'Cargo.toml'"] {
        assert!(paths.contains(p), "trigger path {p}");
    }
}

/// Issue 1487: the crates the OBS build links never depend on the appliance, or OBS needs
/// camera-box while camera-box deploys that OBS.
#[test]
fn the_crates_the_obs_build_links_never_depend_on_the_appliance() {
    for manifest in [
        "jxs/recv-ffi/Cargo.toml",
        "jxs/send-ffi/Cargo.toml",
        "jxs/link/Cargo.toml",
        "jxs/sys/Cargo.toml",
        "rig/core/Cargo.toml",
    ] {
        let text = vstack::read(manifest);
        let dep = text.lines().map(str::trim_start).find(|l| {
            l.starts_with("camera-box =")
                || l.starts_with("camera-box.")
                || l.ends_with(".camera-box]")
                || l.contains("package = \"camera-box\"")
        });
        assert_eq!(dep, None, "{manifest} depends on the camera-box crate");
    }
}
