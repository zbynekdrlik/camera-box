//! #450 / #289 — `setup-device.sh` must provision the fleet's realtime CPU-isolation +
//! genlock config in ONE idempotent run, closing the gaps that forced manual steps on cam5.
//!
//! The live fleet known-good target (verified against cam1/cam2/cam4) is `isolcpus=3` on the kernel
//! cmdline (core 3 reserved for the SCHED_FIFO capture/emit path, #289), the drop-in
//! `camera-box.service.d/cpu-affinity.conf` → `CPUAffinity=3`, and the drop-in
//! `camera-box.service.d/genlock.conf` → `Environment=CAMERA_BOX_GENLOCK_FPS=60`.
//!
//! #450 rescope: the genlock.conf FPS is now READ from the per-cam `CAMERA_GENLOCK_FPS` table
//! (`scripts/camera-set.sh`, resolved via `resolve_device_name`, #451) instead of a hardcoded
//! literal `60` — every fleet camera resolves to 60 today, so the deployed value is unchanged,
//! but the source is now a single per-cam table entry rather than a second hardcoded copy here.
//!
//! Today `setup-device.sh` writes NONE of these (each was a manual SSH edit that drifted across
//! the fleet), and it downloads the binary + dantesync with `curl` while the minimal create-usb
//! base image ships WITHOUT curl — so those downloads silently failed on cam5.
//!
//! These guards pin the load-bearing contract: read the REAL provisioning script and assert on the
//! REAL commands. Style mirrors the repo's other script guards (`appliance_boot_hardening.rs`,
//! `cluster_clock_setup.rs`). RED before the hardening (none of the below present); GREEN after.

use std::path::PathBuf;

const SCRIPT: &str = "scripts/setup-device.sh";

fn read_script() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCRIPT);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// True if `needle` appears on a line that is NOT a `#` comment — so a comment that merely mentions
/// the string cannot satisfy the assertion (the real command must be present). Mirrors the
/// `on_noncomment_line` helper in `appliance_boot_hardening.rs`.
fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// Index of the first NON-comment line containing `needle`.
fn first_noncomment_idx(body: &str, needle: &str) -> Option<usize> {
    body.lines()
        .position(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// Index of the first line (comment or not) containing `needle`.
fn first_idx(body: &str, needle: &str) -> Option<usize> {
    body.lines().position(|l| l.contains(needle))
}

/// 1. The realtime CPU-isolation drop-in must be WRITTEN by the provisioner. Today no script creates
///    it — CPUAffinity was a manual SSH edit, so a reinstalled box comes up with the grab NOT pinned
///    to the isolated core and wobbles under box load (#289). `daemon-reload` (already in STEP 7)
///    picks it up.
#[test]
fn setup_device_writes_cpu_affinity_dropin() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "camera-box.service.d/cpu-affinity.conf"),
        "setup-device.sh must WRITE the drop-in \
         /etc/systemd/system/camera-box.service.d/cpu-affinity.conf — today no script creates it and \
         CPUAffinity was a manual SSH edit that drifted across the fleet (#450/#289)"
    );
    assert!(
        on_noncomment_line(&body, "CPUAffinity=3"),
        "the cpu-affinity.conf drop-in must set `CPUAffinity=3` so the SCHED_FIFO grab is pinned to \
         the isolcpus=3 reserved core on a fresh box (#289)"
    );
}

/// 2. The genlock emit-rate drop-in must be WRITTEN by the provisioner. Without it a reinstalled box
///    free-runs/uncapped instead of emitting at the fleet-correct rate. Program-feeding cams emit
///    60fps (stream decimates 60->30 downstream); the live cam1/cam2/cam4 drop-in is 60.
#[test]
fn setup_device_writes_genlock_dropin() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "camera-box.service.d/genlock.conf"),
        "setup-device.sh must WRITE the drop-in .../genlock.conf so a reinstalled box emits at the \
         fleet-correct rate instead of free-running/uncapped (#450/#11)"
    );
    // #450 rescope: the FPS is no longer a hardcoded literal — it is read from the per-cam
    // CAMERA_GENLOCK_FPS table (scripts/camera-set.sh, #451) so a future per-camera divergence
    // needs only a camera-set.sh edit, not a setup-device.sh edit too.
    assert!(
        on_noncomment_line(
            &body,
            "Environment=CAMERA_BOX_GENLOCK_FPS=${CAMERA_GENLOCK_FPS}"
        ),
        "the genlock.conf drop-in must set `CAMERA_BOX_GENLOCK_FPS` from the per-cam \
         `CAMERA_GENLOCK_FPS` table (resolve_device_name / scripts/camera-set.sh, #450/#451) — \
         not a hardcoded literal `60`"
    );
    assert!(
        !on_noncomment_line(&body, "Environment=CAMERA_BOX_GENLOCK_FPS=60"),
        "the genlock.conf drop-in must no longer hardcode `CAMERA_BOX_GENLOCK_FPS=60` — it must \
         come from the per-cam table via CAMERA_GENLOCK_FPS (#450)"
    );
}

/// #528 design pivot (2026-07-08) — the per-box CAMERA_DISPLAY_SOURCE table wiring this test used
/// to require (config_toml_display_section, an appended [display] config.toml section) is GONE.
/// The owner rejected the whole per-box-config approach (camboxes have no keyboard/mouse; the
/// preview monitor moves between cameras during an event) — the HDMI cameraman preview is now
/// UNCONDITIONAL and fleet-wide, baked into the binary's own default (`DEFAULT_DISPLAY_SOURCE` in
/// src/main.rs). setup-device.sh no longer wires anything display-related into config.toml OR
/// ExecStart.
#[test]
fn setup_device_no_longer_wires_any_per_box_display_mechanism() {
    let body = read_script();
    // NON-comment lines only: an explanatory "this used to call X" comment legitimately mentions
    // the retired names for history — only an actual definition/call site is a real regression.
    assert!(
        !on_noncomment_line(&body, "config_toml_display_section")
            && !on_noncomment_line(&body, "execstart_display_flag"),
        "#528: setup-device.sh must no longer define/call config_toml_display_section or \
         execstart_display_flag -- the HDMI cameraman preview is unconditional (baked into the \
         binary's default), not a per-box config this script wires"
    );
    assert!(
        !on_noncomment_line(&body, "CAMERA_DISPLAY_SOURCE")
            && !on_noncomment_line(&body, "CAMERA_DISPLAY_EXECSTART_SOURCE"),
        "#528: setup-device.sh must no longer reference the retired per-cam display-source tables"
    );
    // #450's canonical-ExecStart invariant, now unconditional on EVERY box (no per-cam exception).
    assert!(
        !body.contains(r#"--display "STRIH-SNV"#),
        "#528: setup-device.sh must never hardcode a --display flag into ExecStart -- the preview \
         lives entirely in the binary's own default (#450/#528)"
    );
}

/// 3. The provisioner must add `isolcpus=3` AND the #303 quiet-core companions
///    (`nohz_full=3 rcu_nocbs=3 irqaffinity=0-2`) to the kernel cmdline (GRUB_CMDLINE_LINUX), and
///    every one of the edits must land BEFORE `update-grub` regenerates grub.cfg or the flags never
///    take effect. #303 closes the gap #289 left open: nohz_full stops the periodic scheduler tick
///    on the isolated core, rcu_nocbs offloads RCU callbacks off it, and irqaffinity=0-2 defaults ALL
///    boot IRQs onto the general cores (the only lever that moves managed MSI xhci IRQs, which
///    reject runtime smp_affinity writes). This edit lives inside the #295 initrd-guaranteed grub
///    step so it can never strand a box on an initrd-less kernel the way an ad-hoc grub edit did.
#[test]
fn setup_device_adds_isolcpus_to_grub_cmdline_before_update_grub() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "GRUB_CMDLINE_LINUX"),
        "setup-device.sh must edit GRUB_CMDLINE_LINUX to add the realtime-isolation kernel flags \
         (#450/#289/#303)"
    );
    const FLAGS: [&str; 4] = [
        "isolcpus=3",
        "nohz_full=3",
        "rcu_nocbs=3",
        "irqaffinity=0-2",
    ];
    for flag in FLAGS {
        assert!(
            on_noncomment_line(&body, flag),
            "setup-device.sh must add `{flag}` to the kernel cmdline to quiet the isolated \
             realtime capture/emit core (#289/#303)"
        );
    }
    let cmds: Vec<&str> = body
        .lines()
        .map(str::trim_start)
        .filter(|t| !t.starts_with('#'))
        .collect();
    let grub_idx = cmds
        .iter()
        .position(|t| t.contains("update-grub"))
        .expect("update-grub present");
    for flag in FLAGS {
        let flag_idx = cmds
            .iter()
            .position(|t| t.contains(flag))
            .unwrap_or_else(|| panic!("{flag} present on a command line"));
        assert!(
            flag_idx < grub_idx,
            "the `{flag}` GRUB_CMDLINE_LINUX edit must run BEFORE update-grub, or the regenerated \
             grub.cfg won't carry the flag (#289/#303)"
        );
    }
}

/// Extract the #289/#303 `for flag_tag in ... done` grub-cmdline append loop as literal shell
/// text, redirect its hardcoded `/etc/default/grub` path to `grub_path`, and execute it via bash.
///
/// This closes a review-flagged test-rigor gap in
/// `setup_device_adds_isolcpus_to_grub_cmdline_before_update_grub` above: that test only proves
/// the 4 flag names appear as TEXT before `update-grub` — a check satisfied even by the loop's
/// OWN HEADER line (`for flag_tag in "isolcpus=3:#289" "nohz_full=3:#303" ...`), regardless of
/// what the loop BODY actually does. A future regression that breaks the body (a typo in the sed
/// pattern, a deleted append, a wrong variable) would still keep the flag names on that header
/// line and pass every existing assertion while silently no longer writing the kernel-cmdline
/// flags on real hardware. This test actually RUNS the real mutation logic against a simulated
/// grub file and asserts on the resulting content, so a body regression fails HERE.
fn run_grub_flag_loop(grub_path: &std::path::Path) {
    let body = read_script();
    let lines: Vec<&str> = body.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.contains("for flag_tag in "))
        .expect("the #289/#303 `for flag_tag in ...` loop header must be present");
    let end = lines[start..]
        .iter()
        .position(|l| l.trim() == "done")
        .map(|i| start + i)
        .expect("the flag_tag loop's closing `done` must be present");
    let block = lines[start..=end].join("\n");
    assert!(
        block.contains("/etc/default/grub"),
        "sanity: the extracted #289/#303 loop must reference /etc/default/grub — extraction \
         anchors may be stale"
    );
    let redirected = block.replace("/etc/default/grub", &grub_path.to_string_lossy());
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!("set -euo pipefail\n{redirected}"))
        .output()
        .expect("failed to spawn bash");
    assert!(
        out.status.success(),
        "the #289/#303 grub flag-append loop exited non-zero.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 3b. FUNCTIONAL guard (not textual): actually run the #289/#303 grub-cmdline append loop
///     against 5 realistic `/etc/default/grub` states — empty file, no GRUB_CMDLINE_LINUX line at
///     all, the live-fleet isolcpus=3-only cmdline, mixed pre-existing content, and a pre-existing
///     empty-quoted value — and assert every one of the 4 flags actually lands in the file, in ONE
///     idempotent run each. A second run against the same file must never duplicate a flag. Also
///     asserts a same-prefix decoy (`GRUB_CMDLINE_LINUX_DEFAULT=`) is never touched.
#[test]
fn setup_device_grub_flag_loop_is_idempotent_and_adds_all_flags() {
    const FLAGS: [&str; 4] = [
        "isolcpus=3",
        "nohz_full=3",
        "rcu_nocbs=3",
        "irqaffinity=0-2",
    ];
    let cases: [(&str, &str); 5] = [
        ("empty file", ""),
        ("no GRUB_CMDLINE_LINUX line", "GRUB_DEFAULT=0\nGRUB_TIMEOUT=5\n"),
        (
            "live-fleet isolcpus=3-only",
            "GRUB_DEFAULT=saved\nGRUB_CMDLINE_LINUX_DEFAULT=\"quiet splash\"\nGRUB_CMDLINE_LINUX=\"isolcpus=3\"\n",
        ),
        (
            "mixed pre-existing content",
            "GRUB_DEFAULT=saved\nGRUB_CMDLINE_LINUX=\"quiet splash isolcpus=3\"\n",
        ),
        (
            "pre-existing empty-quoted value",
            "GRUB_DEFAULT=saved\nGRUB_CMDLINE_LINUX=\"\"\n",
        ),
    ];
    for (label, initial) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let grub_path = dir.path().join("grub");
        std::fs::write(&grub_path, initial).expect("write initial simulated grub file");

        run_grub_flag_loop(&grub_path);
        let once = std::fs::read_to_string(&grub_path).expect("read grub file after 1st run");
        for flag in FLAGS {
            assert!(
                once.contains(flag),
                "case '{label}': `{flag}` missing from GRUB_CMDLINE_LINUX after the append loop \
                 ran. Content: {once:?}"
            );
        }
        if initial.contains("GRUB_CMDLINE_LINUX_DEFAULT") {
            assert!(
                once.contains("GRUB_CMDLINE_LINUX_DEFAULT=\"quiet splash\""),
                "case '{label}': the loop must never touch the GRUB_CMDLINE_LINUX_DEFAULT decoy \
                 line. Content: {once:?}"
            );
        }

        run_grub_flag_loop(&grub_path);
        let twice = std::fs::read_to_string(&grub_path).expect("read grub file after 2nd run");
        assert_eq!(
            once, twice,
            "case '{label}': re-running the append loop must be idempotent (no duplicated flags)"
        );
    }
}

/// 4. curl must be ENSURED before its first use. STEP 3 downloads the binary and STEP 17 downloads
///    dantesync via curl, but the minimal create-usb base image ships without curl — so both
///    downloads silently failed on cam5. A pre-flight must install `curl ca-certificates` BEFORE the
///    first `curl` download, and STEP 16's package list must also carry curl for re-run coverage.
#[test]
fn setup_device_ensures_curl_before_first_download() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "curl ca-certificates"),
        "setup-device.sh must apt-get install `curl ca-certificates` (the base image has no curl; \
         the binary/dantesync downloads silently failed on cam5) (#450)"
    );
    let install_idx =
        first_noncomment_idx(&body, "curl ca-certificates").expect("curl install line present");
    // #457: STEP 3's binary source resolution replaced the static $BINARY_URL release download
    // with $BINARY_SRC (an explicit --binary/CAMERA_BOX_BINARY_URL override, curled when it's a
    // URL) -- the curl-before-first-download invariant still holds, just against the new literal.
    let download_idx =
        first_idx(&body, "curl -fsSL \"$BINARY_SRC\"").expect("binary curl download present");
    assert!(
        install_idx < download_idx,
        "curl must be ensured BEFORE the first `curl -fsSL \"$BINARY_SRC\"` download — otherwise the \
         download silently fails on a base image with no curl, as it did on cam5 (#450)"
    );
    // Belt-and-braces: the main package-install step (STEP 16) must ALSO list curl so a re-run
    // keeps it present even when the pre-flight guard short-circuits (curl already installed).
    let step16 = body
        .lines()
        .find(|l| l.contains("apt-get install") && l.contains("avahi-daemon"))
        .expect("STEP 16 package-install line present");
    assert!(
        step16.contains("curl"),
        "STEP 16's package list must include curl so it is (re)installed on every run, not only when \
         the pre-flight guard fires (#450). Line: {step16}"
    );
}
