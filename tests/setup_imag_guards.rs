//! #458 — imag-nb one-shot provisioner guards.
//!
//! imag-nb (10.77.9.182) is the NEW 60fps low-latency IMAG OBS box (spec
//! docs/superpowers/specs/2026-07-03-imag-nb-topology-design.md). The user's standing directive
//! from the cam6 provisioning saga: a new box is provisioned by ONE unambiguous script, never a
//! pile of ad-hoc manual steps. These tests pin the load-bearing contract of that script pair
//! (`scripts/setup-imag.sh` on-box + `scripts/imag_scenes.py` WS seeding from dev1) so a later
//! edit cannot silently drop a step that made the box production-ready.
//!
//! Style follows the repo's other script guards (`appliance_boot_hardening.rs`,
//! `cluster_clock_setup.rs`): read the REAL scripts and assert on the REAL contract.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const SETUP: &str = "scripts/setup-imag.sh";
const SCENES: &str = "scripts/imag_scenes.py";

/// The script must fail loud (script-failure policy) and never half-provision silently.
#[test]
fn setup_imag_fails_loud() {
    let body = read(SETUP);
    assert!(
        body.contains("set -euo pipefail"),
        "{SETUP} must run under `set -euo pipefail` — a half-provisioned imag box that LOOKS done \
         is the exact cam6-saga failure this one-shot script exists to kill"
    );
}

/// curl must be ENSURED before any download step — the cam5/#450 lesson: base images ship
/// without curl and every fetch silently fails mid-provision (hit AGAIN live on imag-nb).
#[test]
fn setup_imag_ensures_curl_up_front() {
    let body = read(SETUP);
    let curl_ensure = body
        .find("apt-get install -y curl ca-certificates")
        .expect("setup-imag.sh must install curl+ca-certificates up-front (cam5/#450 lesson)");
    let first_curl_use = body
        .find("curl -fsSL")
        .expect("setup-imag.sh downloads via curl -fsSL");
    assert!(
        curl_ensure < first_curl_use,
        "{SETUP}: the curl-ensure preflight must come BEFORE the first curl use"
    );
}

/// USB autosuspend must be forced OFF: the box's ONLY rig link is a USB-ethernet dongle
/// (enx…), and a suspended USB NIC silently stalls all 6 NDI streams.
#[test]
fn setup_imag_disables_usb_autosuspend() {
    let body = read(SETUP);
    assert!(
        body.contains("/sys/bus/usb/devices/*/power/control"),
        "{SETUP} must write `on` to /sys/bus/usb/devices/*/power/control — the rig NIC is USB; \
         autosuspend on it = all camera feeds stall"
    );
}

/// The IMAG box must never sleep or blank mid-service: lid ignore + sleep targets masked.
#[test]
fn setup_imag_masks_sleep_and_lid() {
    let body = read(SETUP);
    assert!(
        body.contains("HandleLidSwitch=ignore"),
        "{SETUP} must set HandleLidSwitch=ignore (it is a NOTEBOOK — closing the lid must not \
         kill the LED-wall program)"
    );
    assert!(
        body.contains("systemctl mask sleep.target suspend.target hibernate.target"),
        "{SETUP} must mask the sleep/suspend/hibernate targets (fleet parity)"
    );
}

/// DistroAV's Linux NDI loader scans ONLY /usr/lib, /usr/lib64 and /usr/local/lib
/// (non-recursive; NOT the multiarch dir, NOT the ld.so cache — vendor/distroav
/// src/plugin-main.cpp load_ndilib). Live-proven on imag-nb: without a libndi.so.<N>
/// symlink in a scanned dir the plugin loads UI-only with ERR-404.
#[test]
fn setup_imag_symlinks_ndi_into_distroav_scan_path() {
    let body = read(SETUP);
    assert!(
        body.contains("/usr/local/lib/libndi.so.6"),
        "{SETUP} must symlink libndi.so.6 into /usr/local/lib — the only fleet-convention dir \
         DistroAV's own Linux loader actually scans (ERR-404 otherwise, hit live on imag-nb)"
    );
}

/// The OBS WebSocket must come up on the fleet-convention port with no auth (stream-box
/// convention) — every rig WS tool (imag_scenes.py, render-budget gate, burn tooling) keys on it.
#[test]
fn setup_imag_seeds_websocket_4455() {
    let body = read(SETUP);
    for needle in [
        "ServerEnabled=true",
        "ServerPort=4455",
        "AuthRequired=false",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} must pre-seed the OBS WebSocket config with `{needle}` — without it no rig \
             tooling (scene seeding, render-budget gate, E2E) can reach the box"
        );
    }
}

/// Projector persistence: SaveProjectors=true is what makes the fullscreen PROGRAM projector
/// survive an OBS restart — without it every reboot loses the LED-wall output.
#[test]
fn setup_imag_persists_projectors() {
    let body = read(SETUP);
    assert!(
        body.contains("SaveProjectors=true"),
        "{SETUP} must seed SaveProjectors=true so the program projector survives OBS restarts"
    );
}

/// The scene seeder must create all six cameras, bind them 1:1 to the fleet NDI names, and
/// run the canvas at 1080p60 — the whole point of the box.
#[test]
fn imag_scenes_seeds_six_cams_at_1080p60() {
    let body = read(SCENES);
    assert!(
        body.contains("range(1, 7)"),
        "{SCENES} must iterate cameras 1..=6 (all six NDI cams)"
    );
    for needle in ["CANVAS_W, CANVAS_H, FPS = 1920, 1080, 60", "\"ndi_source\""] {
        assert!(
            body.contains(needle),
            "{SCENES} must pin `{needle}` — imag-nb is the 1080p60 NDI IMAG cutter"
        );
    }
    assert!(
        body.contains("(usb)"),
        "{SCENES} must bind inputs to the fleet `CAM<n> (usb)` NDI source names"
    );
}

/// Low-latency mode on every NDI source is the box's reason to exist.
#[test]
fn imag_scenes_pins_low_latency() {
    let body = read(SCENES);
    assert!(
        body.contains("\"latency\": 1"),
        "{SCENES} must set the DistroAV low-latency mode (latency=1) on every NDI input"
    );
}

/// The projector subcommand must refuse to open on the built-in panel — the program projector
/// belongs on the EXTERNAL (HDMI) monitor, and silently projecting onto eDP would look 'done'
/// while the LED wall stays black.
#[test]
fn imag_scenes_projector_targets_external_monitor_only() {
    let body = read(SCENES);
    assert!(
        body.contains("eDP") && body.contains("OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM"),
        "{SCENES} --projector must exclude the eDP built-in panel and open the PROGRAM mix \
         projector on the external monitor"
    );
}

// ============================================================================================
// #458 remaining scope: step 6 reworked from a stock-DistroAV bootstrap into a genlock (#460)
// hot-swap over the PPA base. These guards pin that contract so a later edit cannot silently
// regress back to the stock plugin or drop the fail-loud / idempotent / manifest-verify pieces.
// ============================================================================================

/// The stock DistroAV .deb bootstrap this step used to run MUST be gone — imag-nb runs the
/// CUSTOMIZED genlock DistroAV, not the upstream release (user directive, #458 comment
/// 2026-07-03: "the stock-DistroAV bootstrap path is dead").
#[test]
fn setup_imag_no_longer_installs_stock_distroav_deb() {
    let body = read(SETUP);
    assert!(
        !body.contains("api.github.com/repos/DistroAV/DistroAV/releases/latest"),
        "{SETUP} must NOT fetch the stock DistroAV release .deb any more — the genlock (#460) \
         build's distroav.so is the plugin now"
    );
}

/// Step 6 must pull the #460 genlock Linux artifacts (the full bundle for libobs.so.30 + the
/// dedicated fast distroav.so) from the linux-genlock.yml workflow via `gh run download` — never
/// re-invent a raw curl/API artifact fetch (private-repo Actions artifacts need `gh`'s auth).
#[test]
fn setup_imag_pulls_460_genlock_artifacts_via_gh_cli() {
    let body = read(SETUP);
    for needle in [
        "linux-genlock.yml",
        "obs-genlock-linux-x86_64",
        "distroav-linux-fast-so",
        "gh run download",
        "gh run list",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} step 6 must reference `{needle}` — it deploys the #460 genlock Linux \
             artifacts, not the stock DistroAV bootstrap"
        );
    }
}

/// A private-repo GitHub Actions artifact download needs authenticated `gh` — the step must
/// fail loud (not silently proceed unauthenticated) when GH_TOKEN is missing.
#[test]
fn setup_imag_requires_gh_token_fail_loud() {
    let body = read(SETUP);
    assert!(
        body.contains("GH_TOKEN") && body.contains("GH_TOKEN env required"),
        "{SETUP} must fail loud when GH_TOKEN is unset — it cannot download the private #460 \
         CI artifact unauthenticated"
    );
}

/// The hot-swap must overwrite the REAL dpkg-owned files at their live-verified imag-nb paths —
/// `libobs.so.30` (SONAME == filename, confirmed live) and the DistroAV plugin `.so`. Swapping
/// the wrong path silently leaves the box on stock bytes.
#[test]
fn setup_imag_hotswaps_real_libobs_and_distroav_paths() {
    let body = read(SETUP);
    for needle in [
        "/usr/lib/x86_64-linux-gnu/libobs.so.30",
        "/usr/lib/x86_64-linux-gnu/obs-plugins/distroav.so",
        "lib/x86_64-linux-gnu/libobs.so.30",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} must reference `{needle}` — the genlock hot-swap targets the REAL, \
             live-verified dpkg file paths on imag-nb"
        );
    }
}

/// A post-swap SONAME sanity check must exist — installing the wrong file (or a corrupted one)
/// at the right path must not pass silently.
#[test]
fn setup_imag_verifies_soname_after_swap() {
    let body = read(SETUP);
    assert!(
        body.contains("readelf") && body.contains("SONAME"),
        "{SETUP} must sanity-check the deployed libobs.so.30's SONAME after the hot-swap"
    );
}

/// #120's per-file sha256 bundle manifest must be verified before anything is installed — a
/// corrupted or tampered download must never be trusted onto the box. setup-imag.sh runs
/// standalone on the box (no sibling scripts/genlock-manifest.sh checked out), so this must be
/// an INLINE check, not a shell-out to the repo tool.
#[test]
fn setup_imag_verifies_bundle_manifest_before_install() {
    let body = read(SETUP);
    assert!(
        body.contains("BUNDLE_MANIFEST.json") && body.contains("verify_bundle_manifest"),
        "{SETUP} must verify the #120 BUNDLE_MANIFEST.json (per-file sha256) before installing \
         any downloaded genlock file"
    );
    assert!(
        body.contains("sha256sum") && body.contains("manifest sha mismatch"),
        "{SETUP} manifest verify must sha256-compare every listed file and fail loud on mismatch"
    );
    let verify_fn = body
        .find("verify_bundle_manifest() {")
        .expect("verify_bundle_manifest must be a real function definition");
    let here_string_loop = body
        .find("done <<< \"$entries\"")
        .expect("the manifest verify loop must use a `<<<` here-string, not a `| while` pipe");
    assert!(
        verify_fn < here_string_loop,
        "{SETUP}: the here-string loop must be INSIDE verify_bundle_manifest (defined before used)"
    );
}

/// The manifest-verify loop must NEVER be a `| while` pipe — under `set -o pipefail` a piped
/// subshell's `exit` from fail() does not abort the parent script (the same footgun already
/// documented for the step-4 ldconfig check). A regression here would make a corrupted-artifact
/// failure silently non-fatal.
#[test]
fn setup_imag_manifest_verify_loop_is_not_a_pipe_subshell() {
    let body = read(SETUP);
    assert!(
        !body.contains("jq -r '.files[] | \"\\(.path)\\t\\(.sha256)\"' \"$manifest\" | while"),
        "{SETUP}: the manifest entries must be captured into a variable and consumed via a \
         `<<<` here-string — piping jq directly into `while read` would run the loop (and any \
         fail() inside it) in a subshell that can't abort the parent script"
    );
}

/// The swap must be idempotent: re-running onto the SAME build SHA is a no-op, never a
/// redundant re-download/re-swap.
#[test]
fn setup_imag_genlock_swap_is_idempotent() {
    let body = read(SETUP);
    for needle in ["GENLOCK_BUILD_SHA.txt", "already deployed", "DEPLOYED_SHA"] {
        assert!(
            body.contains(needle),
            "{SETUP} must track the deployed build SHA in `{needle}` and skip re-swapping when \
             it already matches (idempotent re-run)"
        );
    }
}

/// A deployed-build marker must be dropped where drift-guard (or a human) can read what's live —
/// mirrors the Windows `ProgramData\obs-studio\DEPLOYED_GENLOCK.txt` convention.
#[test]
fn setup_imag_drops_deployed_genlock_marker() {
    let body = read(SETUP);
    assert!(
        body.contains("/opt/obs-genlock") && body.contains("GENLOCK_BUILD_SHA.txt"),
        "{SETUP} must drop a deployed-build marker under /opt/obs-genlock so drift-guard (or a \
         human) can read what genlock build is live on the box"
    );
}

/// The ORIGINAL PPA-stock bytes must be preserved before the first swap ever overwrites them —
/// otherwise there is no way back to a known-good stock OBS if the genlock build is bad.
#[test]
fn setup_imag_backs_up_stock_files_before_swap() {
    let body = read(SETUP);
    assert!(
        body.contains("/opt/obs-backup") && body.contains(".bak"),
        "{SETUP} must back up the pre-swap PPA-stock libobs.so.30/distroav.so — both a dated \
         backup dir (/opt/obs-backup) and a permanent .bak, so rollback to stock is always possible"
    );
}

/// The NDI runtime symlink from step 4 (the ERR-404 fix) must survive the step 5/6 rework
/// untouched — it is a DIFFERENT plugin-scan-path concern from the genlock hot-swap.
#[test]
fn setup_imag_still_keeps_ndi_symlink_after_genlock_rework() {
    let body = read(SETUP);
    let ndi_symlink = body
        .find("/usr/local/lib/libndi.so.6")
        .expect("the step-4 NDI symlink must still be present after the step 5/6 rework");
    let genlock_step = body
        .find("Genlock hot-swap (#460)")
        .expect("step 6 must be reworked into the genlock hot-swap");
    assert!(
        ndi_symlink < genlock_step,
        "{SETUP}: the NDI symlink (step 4) must still come BEFORE the genlock hot-swap (step 6) \
         — step ordering must not have been disturbed by the rework"
    );
}

/// Step 10's verify must be extended with the Linux equivalent of launch-obs-genlock.sh's
/// Windows log-verify: the OBS log is the authoritative proof a stock/wrong build cannot fake.
#[test]
fn setup_imag_step10_verifies_genlock_log_markers() {
    let body = read(SETUP);
    for needle in [
        "render tick ENABLED",
        "[distroav] plugin loaded",
        "NDI library initialized",
        "$OBS_CFG/logs",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} step 10 must log-verify `{needle}` — the OBS log is the authoritative \
             genlock-build proof (mirrors launch-obs-genlock.sh's Windows check)"
        );
    }
}

/// The genlock log verify must fail loud when the build proof is absent — a stock/wrong OBS
/// must never be silently accepted as "provisioned".
#[test]
fn setup_imag_step10_fails_loud_on_missing_genlock_marker() {
    let body = read(SETUP);
    assert!(
        body.contains("NOT the genlock build"),
        "{SETUP} step 10 must fail loud (not warn) when no genlock capability marker is found \
         in the OBS log"
    );
}

/// An unattended `apt upgrade` must not be able to silently revert the hot-swapped files back to
/// PPA/stock bytes behind the operator's back — dpkg still tracks these two files as belonging
/// to the obs-studio/distroav packages.
#[test]
fn setup_imag_holds_packages_against_apt_upgrade_drift() {
    let body = read(SETUP);
    assert!(
        body.contains("apt-mark hold"),
        "{SETUP} must `apt-mark hold` obs-studio/distroav after the hot-swap so an unattended \
         apt upgrade cannot silently revert the genlock deploy"
    );
}
