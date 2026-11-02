//! Issue 1387 -- the JPEG XS link's wiring (std-only, every build): the capture loop calls the
//! sender only through its off-by-default entry point, before the NDI send (slice 1b); the deployed artifact
//! is built with the codec; every CI job that compiles `--all-features` installs nasm; the
//! vendored library is built out of tree.

use std::path::Path;

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Issue 1387, 1.11.2026: the cambox entry retries a target that cannot connect yet, with the
/// production socket and the 1 s interval (a unit test injects its own connector, so only this
/// pins what camera-box itself runs).
#[test]
fn the_cambox_entry_retries_with_the_real_socket_every_second() {
    let s = read("src/jxs_send.rs");
    let start = s.find("pub fn start_from_env(").expect("the entry");
    let body = &s[start..];
    let body = &body[..body[1..]
        .find("\n    pub fn ")
        .map_or(body.len(), |i| i + 1)];
    assert!(body.contains("Self::start_retrying("), "{body}");
    assert!(body.contains("JxsSocket::connect,"), "{body}");
    assert!(body.contains("CONNECT_RETRY,"), "{body}");
    assert!(s.contains(
        "pub const CONNECT_RETRY: std::time::Duration = std::time::Duration::from_secs(1);"
    ));
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
    // the handoff diagnostics' capture instant is the RAW V4L2 one, never the main loop's
    // `capture_realtime_100ns` variable (the phase tracker's slot middle while locked)
    let lit = &m[offer..offer + m[offer..].find("});").unwrap()];
    assert!(
        lit.contains(
            "capture_realtime_100ns: camera_box::genlock_stamp::capture_realtime_100ns(\n                            info.capture_monotonic_100ns,"
        ),
        "{lit}"
    );
}

/// Slice 1a: in burn mode (the E2E's probe build with `CAMERA_BOX_BURN_RUN_ID`) the link moves to
/// the `cam1-burn` thread and is offered the BURNED frame there: the current frame only, after the
/// QR, before the NDI send. Production (no burn) keeps the link on the capture thread and offers
/// the raw frame there, exactly as before (the test above pins that offer and its order).
#[test]
fn in_burn_mode_the_link_moves_to_the_burn_thread_and_production_keeps_the_raw_offer() {
    let m = read("src/main.rs");
    let one = |needle: &str| {
        assert_eq!(m.matches(needle).count(), 1, "exactly one {needle:?}");
        m.find(needle).unwrap()
    };
    // the link moves only behind the probe feature (a production build cannot take it), only in
    // the burn setup, before the burn thread starts
    let rebind = one("#[cfg(feature = \"probe\")]\n        let mut jxs_sender = jxs_sender;");
    let burn_setup = m.find("if burn_run_id.is_some() {").unwrap();
    let take = one("let burn_jxs_link = jxs_sender.take();");
    let spawn = one(".name(\"cam1-burn\".into())");
    assert!(
        rebind < burn_setup && burn_setup < take && take < spawn,
        "taken in the burn setup, before the burn thread starts"
    );
    // the burn thread: one helper draws the QR and then offers the burned buffer, before the NDI
    // send of that same buffer
    assert_eq!(
        m.matches("burn_qr_yuyv(").count(),
        0,
        "the helper is the one burn site (QR first, then the JPEG XS offer)"
    );
    let ring = one("run_burn_ring(rx,");
    // the needle is split so this std-only text test never reads as a probe user to
    // local_build_policy_bounds_target (it imports nothing from the probe module)
    let helper = one(concat!(
        "camera_box::",
        "probe::burn_jxs::burn_then_offer_jxs("
    ));
    let ndi = one("burn_sender.send_frame_data_with_timecode(");
    assert!(
        spawn < ring && ring < helper && helper < ndi,
        "the burned frame goes to JPEG XS before the NDI send"
    );
    let call = &m[helper..ndi];
    assert!(call.contains("&mut job.buf,"), "{call}");
    assert!(
        call.contains("burn_jxs_link.as_ref().zip(job.jxs.as_ref()),"),
        "{call}"
    );
    // only the CURRENT frame's job carries the JPEG XS stamp: it is set after every starvation
    // repeat went out and right before the current frame, and each job takes it (a repeat finds
    // nothing), so the repeats stay NDI-only like the production offer
    let job = one("let job = BurnJob {");
    let taken = one("jxs: burn_jxs_current.take(),");
    let submit = one("match ring.submit(job) {");
    assert!(
        job < taken && taken < submit,
        "the job literal takes the stamp"
    );
    let repeats = one("for j in (1..=starvation_repeats).rev() {");
    let set = one("burn_jxs_current.set(");
    let current = one("emit_one(capture_timecode_100ns);");
    assert!(
        repeats < set && set < current,
        "set after the repeats, right before the current frame"
    );
    let stamp = &m[set..current];
    // the same stamp the production offer passes: the NDI frame's timecode, the emit rate and
    // the RAW capture instant (never the phase tracker's slot middle)
    assert!(
        stamp.contains("timecode_100ns: capture_timecode_100ns,"),
        "{stamp}"
    );
    assert!(stamp.contains("emit_fps: send_fps,"), "{stamp}");
    assert!(
        stamp.contains(
            "capture_realtime_100ns: camera_box::genlock_stamp::capture_realtime_100ns(\n"
        ),
        "{stamp}"
    );
    assert!(stamp.contains("info.capture_monotonic_100ns,"), "{stamp}");
    // production: the capture loop's raw offer is behind no feature gate, so it runs whenever
    // the link was not moved (every production build, a probe build without the burn)
    let guard = one("if let Some(jxs) = jxs_sender.as_ref() {");
    let prev = m[..guard].trim_end().lines().last().unwrap().trim_start();
    assert!(!prev.starts_with("#[cfg("), "{prev}");
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

/// Live 31.10.2026 (cam5 `ps -L`, verify-device `(ah)` on cam2/3/5): upstream
/// `svt_jxs_create_thread` raised every worker it created to SCHED_FIFO 99 whenever the process
/// ran as root, so the cambox encoder ran ABOVE the capture thread (FIFO 90). The one local patch:
/// a worker keeps its creator's scheduling (SCHED_OTHER, or the opt-in `CAMERA_BOX_JXS_FIFO_PRIO`
/// the feeder takes before it opens a session).
#[test]
fn the_vendored_codec_never_raises_its_workers_to_a_realtime_policy() {
    let t = read("vendor/svt-jpeg-xs/Source/Lib/Common/Codec/Threads/SvtThreads.c");
    assert!(
        !t.contains("pthread_setschedparam("),
        "no realtime policy set by the library itself"
    );
    assert!(!t.contains("sched_priority = 99"), "no FIFO 99 worker");
    let pin = read("vendor/svt-jpeg-xs/CAMERA-BOX-VENDOR.md");
    assert!(
        pin.contains("SvtThreads.c") && !pin.contains("**No local patches.**"),
        "the patch is recorded in CAMERA-BOX-VENDOR.md"
    );
}
