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

/// 3. The provisioner must add `isolcpus=3` to the kernel cmdline (GRUB_CMDLINE_LINUX), and the edit
///    must land BEFORE `update-grub` regenerates grub.cfg or the flag never takes effect. ONLY
///    isolcpus — the live fleet cmdline carries no nohz_full/rcu_nocbs (deferred to #303). This edit
///    lives inside the #295 initrd-guaranteed grub step so it can never strand a box on an
///    initrd-less kernel the way an ad-hoc grub edit did.
#[test]
fn setup_device_adds_isolcpus_to_grub_cmdline_before_update_grub() {
    let body = read_script();
    assert!(
        on_noncomment_line(&body, "GRUB_CMDLINE_LINUX"),
        "setup-device.sh must edit GRUB_CMDLINE_LINUX to add the realtime-isolation kernel flag \
         (#450/#289)"
    );
    assert!(
        on_noncomment_line(&body, "isolcpus=3"),
        "setup-device.sh must add `isolcpus=3` to the kernel cmdline so core 3 is reserved for the \
         realtime capture/emit path — the live fleet cmdline carries exactly isolcpus=3 (#289). \
         (ONLY isolcpus — nohz_full/rcu_nocbs stay deferred to #303.)"
    );
    let cmds: Vec<&str> = body
        .lines()
        .map(str::trim_start)
        .filter(|t| !t.starts_with('#'))
        .collect();
    let isolcpus_idx = cmds
        .iter()
        .position(|t| t.contains("isolcpus=3"))
        .expect("isolcpus=3 present on a command line");
    let grub_idx = cmds
        .iter()
        .position(|t| t.contains("update-grub"))
        .expect("update-grub present");
    assert!(
        isolcpus_idx < grub_idx,
        "the isolcpus=3 GRUB_CMDLINE_LINUX edit must run BEFORE update-grub, or the regenerated \
         grub.cfg won't carry the flag (#289)"
    );
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
