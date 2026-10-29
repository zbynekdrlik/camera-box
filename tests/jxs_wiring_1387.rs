//! Issue 1387 -- the JPEG XS link's wiring (std-only, every build): the capture loop calls the
//! sender only through its off-by-default entry point, before the NDI send (slice 1b); the deployed artifact
//! is built with the codec; every CI job that compiles `--all-features` installs nasm; the
//! vendored library is built out of tree.

use std::path::Path;

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Slice 1b (deliberate change of the slice-1 order): the hand-off sits right after the genlock
/// timecode is fixed and BEFORE the #1242 stagger sleep and the NDI send, so the JPEG XS path
/// never waits for the NDI send call; it passes the very timecode variable the NDI frame carries.
#[test]
fn the_capture_loop_starts_the_link_only_from_the_env_and_offers_before_the_ndi_send() {
    let m = read("src/main.rs");
    assert_eq!(
        m.matches("JxsSender::start_from_env(").count(),
        1,
        "one entry point"
    );
    assert_eq!(m.matches("jxs.offer(").count(), 1, "one hand-off");
    let start = m.find("JxsSender::start_from_env(").unwrap();
    let timecode = m
        .find(
            "let capture_timecode_100ns = camera_box::genlock_stamp::genlock_emit_timecode_100ns(",
        )
        .unwrap();
    let noted = m
        .find(".note_emitted_stamp_100ns(capture_timecode_100ns, out_interval_ns);")
        .unwrap();
    let guard = m.find("if let Some(jxs) = jxs_sender.as_ref() {").unwrap();
    let offer = m.find("jxs.offer(").unwrap();
    let stagger = m.find("camera_box::send_stagger::should_sleep(").unwrap();
    let repeats = m
        .find("let starvation_repeats = decimation_gate.last_poll_starvation_repeats();")
        .unwrap();
    let send = m.find("emit_one(capture_timecode_100ns);").unwrap();
    assert!(
        start < timecode,
        "the sender exists before the capture loop"
    );
    assert!(
        timecode < noted && noted < guard,
        "the timecode is fixed first"
    );
    assert!(
        guard < offer,
        "the hand-off is guarded by the Option the env entry point returns"
    );
    assert!(
        offer < stagger,
        "the hand-off comes before the stagger sleep"
    );
    assert!(
        offer < repeats && offer < send,
        "and before every NDI send of the iteration"
    );
    // it passes the same timecode the NDI frame carries
    let region = &m[offer..offer + 400];
    assert!(region.contains("capture_timecode_100ns"), "{region}");
    assert!(!region.contains("starvation_repeat_timecode"), "{region}");
    // the 30 fps selection reads the NDI emit rate (the frame's own grid), never a counter
    assert!(region.contains("emit_fps: send_fps"), "{region}");
}

#[test]
fn the_deployed_appliance_and_the_probe_tools_are_built_with_the_codec() {
    let ci = read(".github/workflows/ci.yml");
    assert!(ci.contains(
        "      - name: Build release\n        run: cargo build --release --features jxs\n"
    ));
    assert!(ci.contains("cargo build --release --features probe,jxs \\"));
    assert!(ci.contains("run: cargo build --release --features jxs --bin jxs-recv-probe"));
    assert!(ci.contains("            target/release/jxs-recv-probe\n"));
    let rel = read(".github/workflows/release.yml");
    assert!(rel.contains("run: cargo build --release --features jxs"));
    assert!(rel.contains("libv4l-dev libasound2-dev nasm"));
}

/// Byte offset of the next top-level job key (`  name:` on its own line) in `rest`.
fn next_job_offset(rest: &str) -> usize {
    let mut pos = 0;
    for line in rest.split_inclusive('\n') {
        let l = line.trim_end_matches('\n');
        let key = l.strip_prefix("  ").unwrap_or("");
        if !key.is_empty() && !key.starts_with(' ') && !key.starts_with('#') && key.ends_with(':') {
            return pos;
        }
        pos += line.len();
    }
    rest.len()
}

#[test]
fn every_ci_job_that_builds_all_features_installs_nasm() {
    let ci = read(".github/workflows/ci.yml");
    for job in [
        "  lint:\n",
        "  test:\n",
        "  coverage:\n",
        "  mutants:\n",
        "  build:\n",
    ] {
        let start = ci.find(job).unwrap_or_else(|| panic!("job {job:?}"));
        let rest = &ci[start + job.len()..];
        let body = &rest[..next_job_offset(rest)];
        let apt = body
            .lines()
            .find(|l| l.contains("apt-get install -y libv4l-dev"))
            .unwrap_or_else(|| panic!("{job:?} has no apt line"));
        assert!(apt.contains(" nasm"), "{job:?}: {apt}");
    }
}

#[test]
fn the_codec_crate_is_linted_and_tested_on_its_own() {
    let ci = read(".github/workflows/ci.yml");
    assert!(ci.contains("run: cargo clippy -p svt-jpegxs-sys --all-targets -- -D warnings"));
    assert!(ci.contains("run: cargo nextest run -p svt-jpegxs-sys"));
    // the on-demand mutation run builds the codec paths it mutates
    assert!(ci.contains("cargo mutants --in-diff pr.diff --features probe,jxs \\"));
}

#[test]
fn the_vendored_library_is_built_out_of_tree_without_lto_or_native_flags() {
    let c = read("jxs/sys/csrc/CMakeLists.txt");
    assert!(
        c.contains("set(CMAKE_OUTPUT_DIRECTORY \"${CMAKE_BINARY_DIR}/lib\" CACHE PATH \"\" FORCE)")
    );
    assert!(c.contains("set(JPEGXS_LTO OFF CACHE BOOL \"\" FORCE)"));
    assert!(c.contains("set(NATIVE OFF CACHE BOOL \"\" FORCE)"));
    assert!(c.contains("set(BUILD_SHARED_LIBS OFF CACHE BOOL \"\" FORCE)"));
    let b = read("jxs/sys/build.rs");
    assert!(b.contains(".profile(\"Release\")"));
    let pin = read("vendor/svt-jpeg-xs/CAMERA-BOX-VENDOR.md");
    assert!(pin.contains("01bc7cc7123d2e4a3e330632fabec873f3cee62b"));
    // the vendored tree carries only the library, not the upstream apps/tests/build outputs
    for absent in ["Source/App", "tests", "Bin", "Build"] {
        assert!(
            !Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("vendor/svt-jpeg-xs")
                .join(absent)
                .exists(),
            "{absent} must not be vendored"
        );
    }
}

#[test]
fn the_codec_is_an_optional_linux_dependency_behind_the_jxs_feature() {
    let c = read("Cargo.toml");
    assert!(c.contains("svt-jpegxs-sys = { path = \"jxs/sys\", optional = true }"));
    assert!(c.contains("jxs = [\"dep:svt-jpegxs-sys\"]"));
    let linux = c
        .find("[target.'cfg(target_os = \"linux\")'.dependencies]")
        .unwrap();
    let dep = c.find("svt-jpegxs-sys = {").unwrap();
    let dev = c.find("[dev-dependencies]").unwrap();
    assert!(linux < dep && dep < dev, "a Linux-only dependency");
}
