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

/// #522: SaveProjectors must be FALSE — the openbox-autostart boot hook (step 15) is now the
/// SOLE, authoritative opener of BOTH projectors (PROGRAM+MULTIVIEW) on every boot, self-healing
/// monitor assignment even if the box's monitor topology changes. Leaving OBS's own
/// SaveProjectors restore ON would double-open the projectors (OBS restoring its last-saved
/// projector state on top of the boot hook re-opening them fresh). Supersedes the pre-#522
/// SaveProjectors=true behaviour, which relied on OBS's own restore and never re-applied the
/// #507 monitor-selection fix on a topology change.
#[test]
fn setup_imag_disables_save_projectors_522() {
    let body = read(SETUP);
    assert!(
        body.contains("SaveProjectors=false"),
        "{SETUP} must seed SaveProjectors=false (#522) — the boot-hook openbox autostart script \
         is now the sole authoritative projector opener; OBS's own save/restore would double-\
         open them"
    );
    assert!(
        !body.contains("SaveProjectors=true"),
        "{SETUP} must NOT seed SaveProjectors=true anywhere — that was the pre-#522 behaviour \
         this ticket replaces"
    );
}

/// #526: setup-imag.sh must DELETE any leftover ~/.config/autostart/obs.desktop from a pre-#530
/// provision. Modern Ubuntu's systemd --user launches every ~/.config/autostart/*.desktop as an
/// app-<id>@autostart.service once graphical-session.target is up, so a leftover obs.desktop
/// starts a SECOND obs ~30s after boot (an "OBS is already running" modal stuck over the
/// projector — live-hit 2026-07-05) on top of the openbox autostart's launch. Remove it.
#[test]
fn setup_imag_removes_leftover_xdg_autostart_526() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"rm -f "$USER_HOME/.config/autostart/obs.desktop""#),
        "{SETUP} must remove the leftover XDG autostart obs.desktop (#526) — systemd --user \
         double-launches OBS from it, producing a stuck 'OBS is already running' modal"
    );
}

/// #522: the openbox autostart must ZERO saved_projectors in the scene-collection JSON BEFORE
/// launching OBS. OBS restores a collection's saved_projectors on load INDEPENDENT of
/// SaveProjectors=false (that flag only stops OBS SAVING new ones on exit); a stale entry — from
/// before the fix — is still restored, stacking a DUPLICATE program projector on the HDMI output
/// on top of the one the autostart opens. Stripping it every boot keeps the open idempotent (1+1).
#[test]
fn setup_imag_autostart_strips_saved_projectors_522() {
    let body = read(SETUP);
    assert!(
        body.contains("saved_projectors") && body.contains("json.dump"),
        "{SETUP} openbox autostart must zero saved_projectors in the scene-collection JSON \
         (#522) — OBS restores them independent of SaveProjectors=false, duplicating the HDMI \
         program projector"
    );
    let strip = body
        .find("saved_projectors")
        .expect("saved_projectors strip must be present");
    // must run BEFORE OBS launches so OBS loads the stripped collection (restore happens at load).
    let launch = body
        .find("taskset -c 2-11 obs &")
        .expect("the autostart OBS launch must be present");
    assert!(
        strip < launch,
        "the saved_projectors strip must run BEFORE the autostart launches OBS (#522)"
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

/// #522/#488: the projector subcommand must select monitors by connector TYPE (HDMI vs
/// non-HDMI), never by the "eDP" substring — on imag-nb's dGPU (#500, RTX 5050 Laptop / PRIME
/// nvidia-primary) the panel enumerates as "DP-0(0)", not "eDP-1" as on the older Intel-iGPU
/// naming, so an "eDP not in name" filter is AMBIGUOUS (neither "DP-0(0)" nor "HDMI-0(1)"
/// contains "eDP") and can wrongly open PROGRAM on the panel instead of the projector — the root
/// cause of #522/#488.
#[test]
fn imag_scenes_projector_selects_by_hdmi_not_edp_522() {
    let body = read(SCENES);
    assert!(
        !body.contains(r#""eDP" not in m.get("monitorName", "")"#),
        "{SCENES}: projector() must NOT filter by 'eDP not in monitorName' any more — that \
         filter is ambiguous on the dGPU box's DP-0/HDMI-0 naming and is the root cause of \
         #522/#488"
    );
    assert!(
        body.contains(r#""HDMI" in m.get("monitorName", "")"#),
        "{SCENES}: projector() must select the PROGRAM projector's monitor via 'HDMI in \
         monitorName' — the one connector-type string that stays stable across both the old \
         Intel-iGPU (eDP-1) and the new dGPU (DP-0) panel namings"
    );
    assert!(
        body.contains(r#""HDMI" not in m.get("monitorName", "")"#),
        "{SCENES}: projector() must select the MULTIVIEW projector's monitor via 'HDMI not in \
         monitorName' (the panel) — the complementary selection to the PROGRAM projector's HDMI \
         monitor"
    );
    assert!(
        body.contains("OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM")
            && body.contains("OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW"),
        "{SCENES}: --projector must open BOTH the PROGRAM projector (HDMI) and the built-in \
         MULTIVIEW projector (panel) — the boot hook self-heals both every boot (#522/#507)"
    );
}

// ============================================================================================
// #501 -- MV monitor-only twin scenes feed the built-in multiview from LOW-bandwidth NDI
// receivers (genlock_monitor=true -> vendor/distroav's genlock lockdown MONITOR-SOURCE
// exception, ~9x cheaper than the full-bw "Cam N" receivers) so the #276/#278/#293
// render-budget decouple keeps the program at 60fps with the multiview open. Root cause
// (runtime-proven with an eglSwapBuffers-counting shim, #501 comment 2026-07-04): a single
// multiview render costs ~80ms CPU because its 6 cells render ALL 6 cameras' FULL-1080p NDI
// sources, whose async texture uploads happen ONLY during the multiview's own render.
// ============================================================================================

/// The seeder must create a SEPARATE low-bandwidth "MV Cam N" twin scene per camera, bound to
/// the SAME fleet NDI name but flagged genlock_monitor=true so the vendor/distroav lockdown
/// forces low-bandwidth receive instead of the certified highest-bandwidth default.
#[test]
fn imag_scenes_seeds_six_low_bandwidth_mv_monitor_scenes() {
    let body = read(SCENES);
    assert!(
        body.contains("f\"MV Cam {n}\""),
        "{SCENES} must seed 6 \"MV Cam N\" monitor-only twin scenes (camera-box #501)"
    );
    assert!(
        body.contains("\"genlock_monitor\": True"),
        "{SCENES} must set genlock_monitor=true on the MV twin's NDI input settings -- this is \
         the flag vendor/distroav's genlock lockdown reads to force LOW-bandwidth NDI receive \
         instead of the certified HIGHEST (camera-box #501)"
    );
    let mv_block_start = body
        .find("f\"MV Cam {n}\"")
        .expect("MV scene block must exist");
    assert!(
        body[mv_block_start..].contains("ignore_err=True"),
        "{SCENES}: MV twin seeding must be idempotent (ignore_err=True) -- the same self-healing \
         convention already used by the real Cam scene loop"
    );
}

/// The multiview must show ONLY the low-bw MV twins -- the real full-bw "Cam N" scenes (which the
/// Stream Deck cuts) and any other scene must be hidden from it, via the obs-websocket
/// `SetSourcePrivateSettings` per-scene `show_in_multiview` key (OBS frontend
/// components/Multiview.cpp / widgets/OBSBasic_Scenes.cpp: default true, per-scene private
/// setting).
#[test]
fn imag_scenes_configures_multiview_membership_via_private_settings() {
    let body = read(SCENES);
    assert!(
        body.contains("SetSourcePrivateSettings") && body.contains("\"show_in_multiview\""),
        "{SCENES} must set the per-scene `show_in_multiview` private setting via the \
         obs-websocket `SetSourcePrivateSettings` request (camera-box #501)"
    );
    assert!(
        body.contains("startswith(\"MV Cam \")"),
        "{SCENES} must gate show_in_multiview on the MV-scene-name prefix -- true for the MV \
         twins, false for the real Cam scenes and everything else (camera-box #501)"
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
        body.contains("BUNDLE_MANIFEST.json") && body.contains("manifest_sha_for_path"),
        "{SETUP} must look up the #120 BUNDLE_MANIFEST.json's per-file sha256 before installing \
         any downloaded genlock file"
    );
    assert!(
        body.contains("sha256sum") && body.contains("sha256 mismatch"),
        "{SETUP} manifest verify must sha256-compare every checked file and fail loud on mismatch"
    );
    let lookup_fn = body
        .find("manifest_sha_for_path() {")
        .expect("manifest_sha_for_path must be a real function definition");
    let verify_fn = body
        .find("verify_file_sha() {")
        .expect("verify_file_sha must be a real function definition");
    assert!(
        lookup_fn < verify_fn,
        "{SETUP}: manifest_sha_for_path must be defined before verify_file_sha (both before use)"
    );
}

/// #120 sha256 verification must cover BOTH swapped files, not just libobs.so.30. distroav.so
/// ships in a SEPARATE artifact (distroav-linux-fast-so) with no manifest of its own (only a
/// commit-id text file, not a content hash) — the ONLY way to content-verify it is to cross-check
/// it against the bundle's OWN manifest entry for the same relpath (live-verified byte-identical
/// across both build jobs). Without this, the one file actually loaded into OBS as the
/// NDI-carrying plugin had ZERO integrity check — found independently by 3 review passes.
#[test]
fn setup_imag_verifies_distroav_integrity_via_bundle_manifest_crosscheck() {
    let body = read(SETUP);
    assert!(
        body.contains("lib/x86_64-linux-gnu/obs-plugins/distroav.so")
            && body.contains("cross-checked against bundle manifest"),
        "{SETUP} must sha256-verify the downloaded distroav.so by cross-checking it against the \
         bundle manifest's entry for lib/x86_64-linux-gnu/obs-plugins/distroav.so — a build-SHA \
         text-equality check alone (DISTROAV_BUILD_SHA.txt) does not prove the BYTES are intact"
    );
    let want_libobs = body
        .find("WANT_LIBOBS_SHA=\"$(manifest_sha_for_path")
        .expect("libobs.so.30's expected sha must be looked up via manifest_sha_for_path");
    let want_distroav = body
        .find("WANT_DISTROAV_SHA=\"$(manifest_sha_for_path")
        .expect("distroav.so's expected sha must ALSO be looked up via manifest_sha_for_path");
    let verify_libobs = body
        .find("verify_file_sha \"$BUNDLE_LIBOBS\" \"$WANT_LIBOBS_SHA\"")
        .expect("libobs.so.30 must actually be verify_file_sha'd against its looked-up sha");
    let verify_distroav = body
        .find("verify_file_sha \"$FAST_DISTROAV\" \"$WANT_DISTROAV_SHA\"")
        .expect("distroav.so must actually be verify_file_sha'd against its looked-up sha");
    assert!(
        want_libobs < verify_libobs && want_distroav < verify_distroav,
        "{SETUP}: each expected sha must be resolved BEFORE the corresponding verify_file_sha call"
    );
}

/// A `fail()`-capable function call MUST be captured via a BARE `VAR="$(func ...)"` assignment,
/// never embedded as one of several arguments to another command. Empirically confirmed during
/// review of this exact PR: `cmd "$(func_that_calls_fail)" other_arg` only kills the command-
/// substitution SUBSHELL under `set -e` — `cmd` still runs with that argument silently EMPTY,
/// silently defeating the fail-loud contract `manifest_sha_for_path` exists to provide. This is a
/// DIFFERENT (subtler) footgun than the already-documented `| while` pipe-subshell one: a bare
/// `VAR="$(func)"` assignment DOES correctly propagate the abort (verified too), so the fix is
/// "resolve into a variable first, then pass the variable" — never inline the substitution.
#[test]
fn setup_imag_manifest_lookup_never_inlined_in_multi_arg_call() {
    let body = read(SETUP);
    // Every CALL site of manifest_sha_for_path (excluding its own `fn() {` definition line) must
    // be a bare `IDENT="$(manifest_sha_for_path ...)"` assignment — i.e. the trimmed line starts
    // with an identifier, `=`, then the substitution. A call embedded mid-line as one of several
    // arguments to a DIFFERENT command (e.g. `verify_file_sha "$X" "$(manifest_sha_for_path ...)"`)
    // would fail this shape check.
    let mut call_lines = 0;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if !trimmed.contains("manifest_sha_for_path")
            || trimmed.starts_with("manifest_sha_for_path()")
        {
            continue;
        }
        // Skip comment lines / doc prose mentioning the function name.
        if trimmed.starts_with('#') {
            continue;
        }
        call_lines += 1;
        let is_bare_assignment = trimmed
            .split_once("=\"$(manifest_sha_for_path")
            .is_some_and(|(ident, _)| {
                !ident.is_empty() && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
        assert!(
            is_bare_assignment,
            "{SETUP}: line `{trimmed}` calls manifest_sha_for_path but is NOT a bare \
             `IDENT=\"$(manifest_sha_for_path ...)\"` assignment on its own line — a fail() inside \
             it would be silently swallowed by the command-substitution subshell if this call is \
             embedded as an argument to another command instead"
        );
    }
    assert_eq!(
        call_lines, 6,
        "{SETUP}: expected exactly 6 manifest_sha_for_path call sites — the install-time verify \
         (libobs.so.30 + distroav.so + #499 bin/obs) PLUS the #472 no-op re-verify (same three \
         files, looked up again from the CACHED manifest) — found {call_lines}; update this test \
         if the call count genuinely changed"
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

/// The idempotency short-circuit must happen BEFORE the ~90MB/~1600-file bundle is ever
/// downloaded — resolving the run's headSha (one cheap `gh run view` API call, no download) is
/// enough to know whether a no-op re-run needs to touch the network at all. Downloading first and
/// only skipping the INSTALL step would pay full bandwidth + full manifest-hash cost on every
/// re-run even when nothing changed (found in review).
#[test]
fn setup_imag_checks_idempotency_before_downloading_bundle() {
    let body = read(SETUP);
    let headsha_call = body
        .find("gh run view \"$RUN_ID\" --repo \"$GENLOCK_REPO\" --json headSha")
        .expect("NEW_SHA must be resolved via a cheap `gh run view --json headSha` call");
    let noop_check = body
        .find("if [ \"$DEPLOYED_SHA\" = \"$NEW_SHA\" ]")
        .expect("the idempotency no-op check must exist");
    let bundle_download = body
        .find("gh run download \"$RUN_ID\" --repo \"$GENLOCK_REPO\" -n obs-genlock-linux-x86_64")
        .expect("the bundle download must still exist for the non-no-op path");
    assert!(
        headsha_call < noop_check && noop_check < bundle_download,
        "{SETUP}: order must be resolve-headSha -> idempotency-check -> (only then) download — \
         a no-op re-run must never pay the bundle download cost"
    );
}

/// #472 defense-in-depth (follow-up from PR #471 review, deliberately deferred): the no-op
/// idempotency skip (`setup_imag_checks_idempotency_before_downloading_bundle`) trusts the
/// on-disk SHA marker + file *existence* alone — it must ALSO re-verify the CURRENTLY INSTALLED
/// libobs.so.30/distroav.so bytes against the manifest cached locally on the last successful
/// swap, and fall through to a fresh re-install (never just warn) on any mismatch. Without this,
/// a silently-reverted install (e.g. an unattended apt upgrade slipping past the apt-mark hold)
/// would report "already deployed" forever — step 10's runtime log-verify would eventually catch
/// it, but only after a confusing failure.
#[test]
fn setup_imag_reverifies_installed_bytes_on_idempotent_noop() {
    let body = read(SETUP);
    let cached_manifest = "$GENLOCK_MARKER_DIR/BUNDLE_MANIFEST.json";
    assert!(
        body.contains(cached_manifest),
        "{SETUP}: the no-op path must re-verify against the manifest CACHED at \
         $GENLOCK_MARKER_DIR/BUNDLE_MANIFEST.json (copied there on the last successful swap) — \
         re-verifying only ever against a freshly-downloaded bundle manifest would defeat the \
         whole point of skipping the download on a no-op re-run"
    );
    assert!(
        body.contains("manifest_sha_for_path \"$CACHED_MANIFEST\""),
        "{SETUP}: the no-op re-verify must reuse the existing manifest_sha_for_path pure lookup \
         (not reinvent a second jq lookup) — same discipline as the #120 install-time verify"
    );
    assert!(
        body.contains("sha256sum \"$LIBOBS_REAL\"")
            && body.contains("sha256sum \"$DISTROAV_REAL\""),
        "{SETUP}: the no-op re-verify must sha256 the CURRENTLY INSTALLED files (not the \
         about-to-be-downloaded bundle) — that is the whole point of the #472 defense-in-depth \
         check"
    );
    assert!(
        body.contains("forcing re-install"),
        "{SETUP}: a bytes mismatch on the no-op path must fall through to a fresh re-install — \
         never just warn and keep trusting the stale on-disk marker"
    );
    let noop_check = body
        .find("if [ \"$DEPLOYED_SHA\" = \"$NEW_SHA\" ]")
        .expect("the idempotency SHA-marker check must exist");
    let reverify = body
        .find(cached_manifest)
        .expect("cached manifest path must be referenced");
    let bundle_download = body
        .find("gh run download \"$RUN_ID\" --repo \"$GENLOCK_REPO\" -n obs-genlock-linux-x86_64")
        .expect("the bundle download must still exist for the non-no-op path");
    assert!(
        noop_check < reverify && reverify < bundle_download,
        "{SETUP}: order must be SHA-marker-check -> cached-manifest re-verify -> (only if still \
         valid) skip, else (only then) download — the re-verify is pure local sha256, it must \
         run BEFORE any network cost is paid"
    );
}

/// `gh run list ... -q '.[0].databaseId'` on an EMPTY result list yields the literal text "null"
/// (jq's normal behaviour indexing a nonexistent array element) — NOT an empty string — so a bare
/// `[ -n "$RUN_ID" ]` guard would wrongly treat "null" as a valid id and proceed to
/// `gh run download null ...`. Empirically verified during review. `// empty` is the fix.
#[test]
fn setup_imag_run_id_resolution_guards_against_jq_null_string() {
    let body = read(SETUP);
    assert!(
        body.contains("-q '.[0].databaseId // empty'"),
        "{SETUP} must resolve RUN_ID with `// empty` (not a bare `.[0].databaseId`) — otherwise \
         an empty successful-run list silently becomes the literal string \"null\", which passes \
         `[ -n \"$RUN_ID\" ]` and proceeds to download a nonexistent run"
    );
}

/// `gh run list` must filter to `--branch dev` — linux-genlock.yml carries a bare
/// `workflow_dispatch:` with no branch restriction (in addition to push-to-dev), so an unfiltered
/// "latest successful run" could pick up an experimental manual dispatch on some other branch.
/// `--branch main` would be wrong for the opposite reason (the workflow never runs ON main).
#[test]
fn setup_imag_run_resolution_filters_to_dev_branch() {
    let body = read(SETUP);
    assert!(
        body.contains("--branch dev") && body.contains("--status success"),
        "{SETUP} must resolve the genlock run via `--branch dev --status success` — never \
         unfiltered (a stray workflow_dispatch on another branch could become \"latest\") and \
         never `--branch main` (the workflow never runs on main)"
    );
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
/// otherwise there is no way back to a known-good stock OBS if the genlock build is bad. Exactly
/// TWO bounded backup dirs (stock, made once; previous, overwritten each swap) — never one
/// unboundedly-accumulating timestamped dir per re-run (the #185 unbounded-target/-growth lesson).
#[test]
fn setup_imag_backs_up_stock_files_before_swap() {
    let body = read(SETUP);
    for needle in [
        "/opt/obs-backup",
        "stock-pre-genlock",
        "\"$GENLOCK_BACKUP_ROOT/previous\"",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} must back up the pre-swap PPA-stock libobs.so.30/distroav.so under `{needle}` \
             — a permanent stock backup (made once) + a previous-build backup (overwritten each \
             swap), so rollback is always possible without unbounded disk growth"
        );
    }
    assert!(
        !body.contains("date +%Y-%m-%d-%H%M%S"),
        "{SETUP}: backups must NOT use a per-run timestamped directory name — that accumulates \
         unboundedly across every re-run (every dev push touching vendor/** re-triggers \
         linux-genlock.yml); use the bounded stock/previous pair instead"
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
///
/// PRECISION NOTE (found in review, TWICE — once for distroav/NDI, then again for this render-tick
/// assertion in a later deep-review pass): checking `body.contains("render tick ENABLED")` alone
/// is a WEAK test — that exact substring ALSO appears in the unconditional success echo
/// (`echo "  genlock render tick ENABLED (#460 build proof)"`), which prints regardless of what
/// the actual `grep -iE 'genlock:.*(render tick ENABLED|...)'` check found. A loose substring
/// check would keep passing even if the real grep/fail() structure were gutted and only that
/// success line survived. This test asserts on the literal `grep -iE` invocation text, matching
/// the same discriminator already correctly used for the distroav/NDI asserts below.
#[test]
fn setup_imag_step10_verifies_genlock_log_markers() {
    let body = read(SETUP);
    assert!(
        body.contains("grep -iE 'genlock:.*(render tick ENABLED") && body.contains("$OBS_CFG/logs"),
        "{SETUP} step 10 must grep the OBS log for the literal `genlock:.*(render tick ENABLED` \
         regex — a plain substring check on the unescaped success-echo text would incidentally \
         match the unconditional 'genlock render tick ENABLED (#460 build proof)' print, not the \
         actual functional check"
    );
    assert!(
        body.contains("grep -i '\\[distroav\\] plugin loaded'"),
        "{SETUP} step 10 must grep the OBS log for the REGEX-escaped `\\[distroav\\] plugin \
         loaded` pattern — a plain substring check on the unescaped text would incidentally match \
         only the WARNING fallback prose, not the actual functional check"
    );
    assert!(
        body.contains("grep -i 'NDI library initialized'"),
        "{SETUP} step 10 must grep the OBS log for the literal `NDI library initialized` pattern \
         (the real DistroAV log line, live-verified on imag-nb)"
    );
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

/// An unattended `apt upgrade` must not be able to silently revert libobs.so.30 back to PPA-stock
/// bytes behind the operator's back — dpkg still tracks it under the obs-studio package. `distroav`
/// must NOT be held: this rework removed the stock DistroAV .deb install entirely, so `distroav`
/// is no longer a dpkg package at all (found in review — the old test asserted a hold that would
/// be a silent no-op for a package that no longer exists). A hold failure must be LOGGED, never
/// silently swallowed (`comprehensive-logging.md`) — it is a real drift-protection guarantee.
#[test]
fn setup_imag_holds_obs_studio_against_apt_upgrade_drift() {
    let body = read(SETUP);
    assert!(
        body.contains("apt-mark hold obs-studio"),
        "{SETUP} must `apt-mark hold obs-studio` after the hot-swap so an unattended apt upgrade \
         cannot silently revert the genlock libobs.so.30 deploy"
    );
    assert!(
        !body.contains("apt-mark hold obs-studio distroav"),
        "{SETUP}: must NOT try to hold `distroav` — this rework removed the stock .deb install, \
         so distroav is not a dpkg package any more and holding it would be a misleading no-op"
    );
    assert!(
        body.contains("WARNING: apt-mark hold"),
        "{SETUP}: a failed apt-mark hold must be LOGGED (not silently `|| true`'d away) — it is a \
         real drift-protection guarantee, not a cosmetic nicety"
    );
}

/// A swap-time OBS kill must SIGKILL and WAIT for actual death, never a bare SIGTERM +
/// fixed-sleep. Found in review: `pkill -x obs || true; sleep 2` (SIGTERM) can leave OBS still
/// exiting after 2s; step 9's `if ! pgrep -x obs` relaunch guard would then SKIP relaunching,
/// silently leaving the OLD build's process resident even though the NEW build's bytes + marker
/// are already on disk. The Windows convention (`launch-obs-genlock.sh --force`) uses
/// `Stop-Process -Force` = SIGKILL for exactly this reason.
#[test]
fn setup_imag_swap_kill_uses_sigkill_and_waits_for_death() {
    let body = read(SETUP);
    assert!(
        body.contains("pkill -9 -x obs"),
        "{SETUP} must SIGKILL (`pkill -9`) obs on a swap-time relaunch, not a bare SIGTERM \
         `pkill -x obs` — a slow-to-exit process would otherwise dodge step 9's relaunch guard"
    );
    let kill = body
        .find("pkill -9 -x obs")
        .expect("pkill -9 -x obs must be present");
    let wait_loop = body
        .find("pgrep -x obs >/dev/null 2>&1 || break")
        .expect("the kill must be followed by a wait-for-death loop, not a fixed sleep alone");
    let fail_if_undead = body
        .find("would not die after SIGKILL")
        .expect("must fail loud if obs is still alive after the wait budget");
    assert!(
        kill < wait_loop && wait_loop < fail_if_undead,
        "{SETUP}: order must be SIGKILL -> wait-for-death loop -> fail-loud-if-still-alive"
    );
}

/// `ls -t "$DIR"/*.txt | head -1` under `set -euo pipefail`: when the glob matches NOTHING, `ls`
/// exits non-zero even though `head` succeeds with empty output, and pipefail propagates that
/// failure to the bare assignment — `set -e` would abort the script THERE, before the intended
/// `[ -n "$LATEST_LOG" ] || fail "no OBS log found..."` check ever runs, with no diagnostic at all
/// (stderr redirected to /dev/null). Empirically verified during review. `|| true` on the whole
/// pipeline is the fix — matches step 8's pre-existing `APP_DESKTOP=$(ls ... 2>/dev/null || true)`.
#[test]
fn setup_imag_latest_log_lookup_survives_empty_glob_under_pipefail() {
    let body = read(SETUP);
    assert!(
        body.contains("ls -t \"$OBS_LOG_DIR\"/*.txt 2>/dev/null | head -1 || true"),
        "{SETUP}: the LATEST_LOG lookup must end in `| head -1 || true` — without it, an empty \
         log directory (glob matches nothing) makes `ls` fail non-zero, and under pipefail that \
         silently aborts the whole script via set -e BEFORE the intended fail() check can run"
    );
}

/// #476: a genlock RE-deploy (step 6) force-restarts OBS to load the swapped bytes. A leftover
/// crash sentinel from that force-restart then makes the relaunched OBS pop the "Crash or
/// unclean shutdown detected" recovery modal and hang headless — WebSocket :4455 never comes
/// up (hit live 2026-07-04, recovered by hand). Step 9 must clear
/// ~/.config/obs-studio/.sentinel/* BEFORE launching obs, mirroring the Windows
/// launch-obs-genlock.sh sentinel-clear convention.
#[test]
fn setup_imag_clears_obs_crash_sentinel_before_launch() {
    let body = read(SETUP);
    assert!(
        body.contains("rm -rf \"${OBS_CFG}/.sentinel\""),
        "{SETUP} must remove ${{OBS_CFG}}/.sentinel/* before launching OBS (mirrors the Windows \
         launch-obs-genlock.sh `Remove-Item .sentinel\\*` convention, #476)"
    );
    let sentinel_clear = body
        .find("rm -rf \"${OBS_CFG}/.sentinel\"")
        .expect("sentinel-clear line must exist");
    let obs_launch = body
        .find("nohup taskset -c 2-11 obs >/tmp/obs-launch.log")
        .expect("{SETUP} must launch obs via nohup (pinned to the #483 P-core block)");
    assert!(
        sentinel_clear < obs_launch,
        "{SETUP}: the crash-sentinel clear must happen BEFORE the obs launch line — a stale \
         sentinel from a force-restart otherwise pops the recovery modal and hangs OBS headless"
    );
}

/// The `gh` CLI bootstrap must capture curl's output into a variable BEFORE grepping it — never a
/// live `curl | grep | head -1` pipe, which shares the same early-pipe-closure class the SONAME
/// check documents (a downstream stage closing early can SIGPIPE an upstream writer under
/// pipefail). Low real-world risk for a small JSON payload, but the codebase's own stated
/// discipline (documented next to the SONAME check two steps later) should be applied consistently
/// rather than duplicated as a live pipe in new code (found in review).
#[test]
fn setup_imag_gh_deb_url_discovery_captures_curl_output_first() {
    let body = read(SETUP);
    assert!(
        body.contains("GH_RELEASE_JSON=\"$(curl -fsSL https://api.github.com/repos/cli/cli/releases/latest)\""),
        "{SETUP}: curl's output must be captured into a variable (GH_RELEASE_JSON) BEFORE any \
         grep/head processing — not a live `curl | grep | head -1` pipe"
    );
    let capture = body
        .find("GH_RELEASE_JSON=\"$(curl")
        .expect("GH_RELEASE_JSON capture must exist");
    let grep_from_var = body
        .find("printf '%s' \"$GH_RELEASE_JSON\"")
        .expect("the gh .deb URL must be grepped FROM the captured variable, not a live pipe");
    assert!(
        capture < grep_from_var,
        "{SETUP}: GH_RELEASE_JSON must be captured before it is grepped"
    );
}

// ============================================================================================
// #479 — provision DanteSync on imag-nb so genlock's system-clock read (CLOCK_REALTIME) stays
// disciplined to the same cluster master as the cameras. A re-provision must reproduce the
// 2026-07-04 by-hand fix (dantesync 1.8.17, NIC-pinned, timesyncd masked, PTP/NTP lock verified).
// ============================================================================================

/// The dantesync unit must pin BOTH the resolved NIC (imag is a notebook with other interfaces)
/// and the cluster master strih.lan — a bare `dantesync` would fall back to the public NTP pool
/// and silently desync imag's clock from the rest of the rig.
#[test]
fn setup_imag_installs_dantesync_pinned_to_nic_and_master() {
    let body = read(SETUP);
    assert!(
        body.contains("ExecStart=/usr/local/bin/dantesync -i ${NIC} --ntp-server strih.lan"),
        "{SETUP} must write a dantesync ExecStart pinned to both the resolved NIC and the \
         cluster master strih.lan (live-verified fix, 2026-07-04) — not a bare `dantesync` \
         (public-pool default) and not an unpinned NIC (imag has other interfaces)"
    );
}

/// DanteSync OWNS the clock (ops-skill hard rule) — systemd-timesyncd must be masked so nothing
/// else can ever discipline imag's clock alongside it, and the mask must happen BEFORE dantesync
/// is enabled so the two clock sources can never race even for a single boot cycle.
#[test]
fn setup_imag_masks_timesyncd_before_enabling_dantesync() {
    let body = read(SETUP);
    assert!(
        body.contains("systemctl mask systemd-timesyncd"),
        "{SETUP} must mask systemd-timesyncd — dantesync OWNS the clock, per the ops skill hard \
         rule (never timesyncd/chrony/ptp4l alongside it)"
    );
    let mask = body
        .find("systemctl mask systemd-timesyncd")
        .expect("timesyncd mask must exist");
    let start = body
        .find("systemctl restart dantesync")
        .expect("dantesync (re)start must exist");
    assert!(
        mask < start,
        "{SETUP}: systemd-timesyncd must be masked BEFORE dantesync is (re)started — the two clock \
         sources must never race, not even for one boot cycle"
    );
}

/// The install must fail loud (never silently proceed unlocked) if PTP/NTP lock is not achieved
/// within budget — a re-provision that "succeeds" without clock discipline is the exact
/// FIFO-skew-drift regression #479 exists to prevent.
#[test]
fn setup_imag_verifies_dantesync_ptp_lock() {
    let body = read(SETUP);
    assert!(
        body.contains(r"grep -qE '\[PTP\][[:space:]]+(LOCK|NANO)|\[NTP\] offset'"),
        "{SETUP} must poll `journalctl -u dantesync` for a PTP LOCK/NANO or NTP offset line \
         before declaring the step done"
    );
    assert!(
        body.contains("did not report PTP/NTP lock within 150s of restart"),
        "{SETUP} must fail loud when dantesync never reports PTP/NTP lock — a silent pass here \
         would let a re-provision ship with an undisciplined clock"
    );
    // #491: the lock check must be RESTART-ANCHORED (--since the restart instant), never a bare
    // `-n 50` line window — on a re-provision the pre-restart LOCK scrolls out of a 50-line window
    // and a fresh post-restart LOCK (30-70s later) is missed, false-failing a healthy re-lock.
    assert!(
        body.contains(r#"journalctl -u dantesync --no-pager --since "@$DANTESYNC_RESTART_EPOCH""#),
        "{SETUP}: the dantesync lock check must anchor journalctl to the restart instant \
         (--since @$DANTESYNC_RESTART_EPOCH), not a bare `-n 50` window (#491 false-fail fix)"
    );
    assert!(
        !body.contains("journalctl -u dantesync --no-pager -n 50"),
        "{SETUP}: the dantesync lock check must NOT use the bare `-n 50` window that #491 fixed"
    );
    // #491: the re-lock budget must be generous (~150s) — a restart's PTP re-acquisition is
    // legitimately slower than a cold start; the old 60s (seq 1 30) budget false-failed.
    assert!(
        body.contains("for i in $(seq 1 75)"),
        "{SETUP}: the dantesync lock-verify budget must be ~150s (seq 1 75 x 2s) — the old 60s \
         budget false-failed a valid re-lock (#491)"
    );
}

/// The dantesync binary install must have a fallback path (GitHub release OR a cam-box copy) so
/// a re-provision cannot fail solely because GitHub is unreachable from imag's network.
#[test]
fn setup_imag_dantesync_has_gh_release_and_cambox_fallback() {
    let body = read(SETUP);
    for needle in [
        "api.github.com/repos/${DANTESYNC_REPO}/releases/latest",
        "dantesync-linux-amd64",
        "CAM_PW",
        "dantesync copy from cam1 fallback failed",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} dantesync install must reference `{needle}` — GitHub release fetch with a \
             cam-box scp fallback, mirroring the step-6/NDI-runtime fallback pattern already in \
             this script"
        );
    }
}

// ============================================================================================
// #485 — imag-nb desktop de-jitter: mask GNOME/Ubuntu background jitter sources on the
// single-app OBS kiosk + OBS-native ProcessPriority=High. All reversible, security updates stay
// fully enabled (only their schedule is pinned).
// ============================================================================================

/// systemd-oomd, the file indexer, the groupware factories, and apport/whoopsie must all be
/// masked — none of them provide value on a kiosk box that no human ever browses/mails on, and
/// oomd specifically is known to kill whole GNOME sessions (incl. OBS) on transient PSI spikes.
#[test]
fn setup_imag_masks_oomd_tracker_evolution_apport_whoopsie() {
    let body = read(SETUP);
    for needle in [
        "systemctl mask systemd-oomd.service systemd-oomd.socket",
        "tracker-miner-fs-3.service",
        "tracker3 reset -s",
        "evolution-source-registry.service",
        "systemctl mask apport.service whoopsie.service",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} desktop de-jitter step must reference `{needle}` — none of these background \
             services provide value on a single-app kiosk, and systemd-oomd is known to kill \
             whole GNOME sessions (incl. OBS) on transient PSI memory-pressure spikes"
        );
    }
}

/// snapd auto-refresh must be held forever (unused firefox/snap-store snaps) — a mid-service
/// "restart to update" banner popping over the fullscreen program output is the failure this
/// avoids. Hold-only: snapd itself must never be removed or disabled.
#[test]
fn setup_imag_holds_snap_refresh_forever() {
    let body = read(SETUP);
    assert!(
        body.contains("snap refresh --hold=forever"),
        "{SETUP} must `snap refresh --hold=forever` — an unattended snap refresh can pop a \
         \"restart to update\" banner over the fullscreen program output"
    );
}

/// apt-daily-upgrade.timer must be pinned to a fixed off-hours OnCalendar via a drop-in — but
/// security updates themselves must stay fully enabled (never masked/disabled here). Only the
/// SCHEDULE is pinned so an update can never land mid-service.
#[test]
fn setup_imag_pins_apt_daily_upgrade_offhours_without_disabling_security_updates() {
    let body = read(SETUP);
    for needle in [
        "/etc/systemd/system/apt-daily-upgrade.timer.d/imag-offhours.conf",
        "OnCalendar=*-*-* 04:00",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} must pin apt-daily-upgrade.timer's schedule via `{needle}`"
        );
    }
    assert!(
        !body.contains("mask apt-daily-upgrade")
            && !body.contains("disable --now apt-daily-upgrade")
            && !body.contains("mask unattended-upgrades"),
        "{SETUP}: apt-daily-upgrade/unattended-upgrades must NEVER be masked or disabled here — \
         only the SCHEDULE is pinned, security updates stay fully enabled (#485 explicit \
         instruction: do NOT disable security updates)"
    );
}

/// GNOME compositor animations must be turned off — one less compositor cost on the fullscreen
/// program output, applied the same way the existing sleep/screensaver gsettings calls are.
#[test]
fn setup_imag_turns_off_gnome_animations() {
    let body = read(SETUP);
    assert!(
        body.contains("gs org.gnome.desktop.interface enable-animations false"),
        "{SETUP} must turn off GNOME animations via the existing `gs` gsettings helper"
    );
}

/// OBS's own ProcessPriority render-starvation knob must be forced to High. global.ini already
/// exists with ProcessPriority=Normal by the time this step runs (OBS writes it on first launch,
/// and a re-provision runs against an already-launched box) — so the step must flip an EXISTING
/// value in place, not just append a key that would leave the real Normal value in effect.
#[test]
fn setup_imag_seeds_obs_process_priority_high() {
    let body = read(SETUP);
    assert!(
        body.contains(
            "sed -i 's/^ProcessPriority=.*/ProcessPriority=High/' \"$OBS_CFG/global.ini\""
        ),
        "{SETUP} must sed-replace an EXISTING ProcessPriority= line in global.ini to High — \
         appending a new key alone would leave a pre-existing `ProcessPriority=Normal` in effect \
         (Qt's ini backend keeps the first-seen value on a straight duplicate KEY, unlike a \
         duplicate SECTION header)"
    );
    assert!(
        body.contains("printf '\\n[General]\\nProcessPriority=High\\n' >> \"$OBS_CFG/global.ini\""),
        "{SETUP} must also cover the fresh-box case (no ProcessPriority key yet) by appending a \
         [General] section, mirroring seed_ini's own duplicate-section convention for LastVersion"
    );
}

/// The ProcessPriority edit must run AFTER global.ini is seeded (step 8), never before — editing
/// a file before `touch`/seed_ini creates it would silently no-op the sed branch every time.
#[test]
fn setup_imag_process_priority_edit_runs_after_global_ini_seed() {
    let body = read(SETUP);
    let seed = body
        .find("seed_ini \"$OBS_CFG/global.ini\"")
        .expect("global.ini must be seeded via seed_ini");
    let priority_edit = body
        .find("ProcessPriority=High")
        .expect("the ProcessPriority=High edit must exist");
    assert!(
        seed < priority_edit,
        "{SETUP}: the ProcessPriority=High edit must run AFTER global.ini is seeded — editing \
         before the file is created/seeded would leave the sed branch permanently a no-op"
    );
}

// ============================================================================================
// #486 — network performance tuning (sysctl + EEE/flow-control), scoped to the ONE NDI NIC
// resolved in step 1. Mirrors setup-device.sh STEP 14, but NEVER a for-every-interface loop —
// imag also carries Wi-Fi/other adapters that must stay untouched.
// ============================================================================================

/// The sysctl drop-in must carry the same core low-latency knobs as the fleet's STEP 14 —
/// larger buffers, BBR congestion control, tcp_nodelay/low_latency, IPv6 off.
#[test]
fn setup_imag_writes_scoped_network_performance_sysctl() {
    let body = read(SETUP);
    for needle in [
        "/etc/sysctl.d/99-network-performance.conf",
        "net.core.rmem_max = 134217728",
        "net.core.wmem_max = 134217728",
        "net.ipv4.tcp_congestion_control = bbr",
        "net.ipv4.tcp_nodelay = 1",
        "net.ipv6.conf.all.disable_ipv6 = 1",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} must write `{needle}` into the network-performance sysctl drop-in \
             (mirrors setup-device.sh STEP 14)"
        );
    }
}

/// #486's whole point is to scope the EEE/flow-control tuning to the ONE resolved NDI NIC — a
/// for-every-interface loop (the setup-device.sh STEP 14 shape) would also hit imag's Wi-Fi/other
/// adapters, which must stay untouched (imag is a notebook, unlike the single-NIC cam appliances).
#[test]
fn setup_imag_scopes_eee_flowcontrol_to_ndi_nic_not_every_interface() {
    let body = read(SETUP);
    for needle in [
        "ethtool --set-eee \"$NIC\" eee off",
        "ethtool -A \"$NIC\" rx off tx off",
        "if [ \"\\$IFACE\" = \"${NIC}\" ]; then",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} must scope the EEE/flow-control tuning to `{needle}` — the ONE NDI NIC \
             resolved in step 1, not every interface"
        );
    }
    assert!(
        !body.contains("for iface in /sys/class/net/*/device"),
        "{SETUP} must NOT loop over every interface for EEE/flow-control tuning (that is the \
         setup-device.sh STEP 14 shape) — imag also carries Wi-Fi/other adapters that a \
         for-every-interface loop would wrongly touch"
    );
}

/// A networkd-dispatcher hook alone would miss a NIC that never re-fires the routable event
/// (e.g. it was already up before the script ran) — the fix must ALSO apply immediately once at
/// provision time AND persist across reboots via rc.local (belt-and-suspenders, some USB-ethernet
/// chipsets reset EEE state on power cycle).
#[test]
fn setup_imag_reapplies_eee_off_in_rc_local_for_boot_persistence() {
    let body = read(SETUP);
    assert!(
        body.contains("ethtool --set-eee ${NIC} eee off")
            && body.contains("ethtool -A ${NIC} rx off tx off"),
        "{SETUP}: rc.local (re-run on every boot) must also carry the EEE/flow-control-off calls \
         for the resolved NIC — a networkd-dispatcher hook alone is not guaranteed to re-fire on \
         every boot"
    );
}

/// #486 must be inserted as a NEW step strictly between step 1 (static IP / NIC discovery) and
/// the original step 2 (governor) — per the issue's explicit instruction.
#[test]
fn setup_imag_network_tuning_step_lands_between_step1_and_governor_step() {
    let body = read(SETUP);
    let step1 = body
        .find("Static IP ${STATIC_IP}")
        .expect("step 1 (static IP) must exist");
    let network_step = body
        .find("Network performance tuning (#486)")
        .expect("the #486 network-performance step must exist");
    let governor_step = body
        .find("Max performance: governor")
        .expect("the governor step must exist");
    assert!(
        step1 < network_step && network_step < governor_step,
        "{SETUP}: the #486 network-performance step must land strictly between step 1 (static \
         IP / NIC discovery) and the governor step — per the issue's explicit placement"
    );
}

// ============================================================================================
// #482/#483/#487 — codify the imag-nb kernel/isolation/boot-safety hardening that was already
// applied + LIVE-VERIFIED on imag-nb (2026-07-04): preempt=full via linux-lowlatency-hwe-24.04
// (zero kernel downgrade), a P-core-block CPU isolation for OBS (isolcpus/nohz_full/irqaffinity)
// + an OBS taskset pin on both launch paths, and the #295 boot-safety net (kernel apt-hold +
// initrd-guarantee hook + unattended-upgrade kernel lockdown) that underpins both grub changes.
// This is PURE codification — the box itself is already hardened; these tests pin that a
// from-scratch re-provision reproduces the exact same live state.
// ============================================================================================

/// The step order must be: #487 (boot safety net) BEFORE #482 (lowlatency kernel) BEFORE #483
/// (CPU isolation) — the boot-safety net's kernel-hold + initrd-guarantee underpins both grub
/// changes that follow it, so it must land first.
#[test]
fn setup_imag_boot_safety_net_precedes_lowlatency_and_isolation_steps() {
    let body = read(SETUP);
    let safety_step = body
        .find("Boot safety net (#487)")
        .expect("the #487 boot-safety-net step must exist");
    let lowlatency_step = body
        .find("Low-latency kernel (#482)")
        .expect("the #482 lowlatency-kernel step must exist");
    let isolation_step = body
        .find("CPU isolation (#483)")
        .expect("the #483 CPU-isolation step must exist");
    assert!(
        safety_step < lowlatency_step && lowlatency_step < isolation_step,
        "{SETUP}: order must be #487 (boot safety net) -> #482 (lowlatency kernel) -> #483 (CPU \
         isolation) — the boot-safety net's kernel-hold + initrd-guarantee underpins the grub \
         changes both later steps make"
    );
}

/// #487: the generic kernel packages must be pinned (apt-mark hold) so an upgrade can never
/// silently swap the boot kernel — the same class of failure that bricked CAM3/CAM4 (#295).
#[test]
fn setup_imag_holds_generic_kernel_packages_487() {
    let body = read(SETUP);
    const HOLD_CMD: &str =
        "apt-mark hold linux-image-generic-hwe-24.04 linux-headers-generic-hwe-24.04";
    assert!(
        body.contains(HOLD_CMD),
        "{SETUP} must run `{HOLD_CMD}` (imag runs the HWE kernel line, not the plain -generic \
         names the cam fleet's setup-device.sh uses) so a surprise kernel can never be installed \
         (#487, extends #295)"
    );
}

/// #487: found in review -- a bare `apt-mark hold ... || echo WARNING` correctly does not abort
/// the script on failure, but the step's closing summary echo must NOT unconditionally claim
/// "kernel pinned" regardless of whether the hold actually succeeded. The outcome must be tracked
/// in a variable and the summary wording must branch on it, so a genuine hold failure is never
/// masked by an adjacent line asserting success.
#[test]
fn setup_imag_kernel_hold_summary_reflects_real_outcome_487() {
    let body = read(SETUP);
    assert!(
        body.contains("KERNEL_HOLD_OK=1"),
        "{SETUP} must track the apt-mark hold outcome in a KERNEL_HOLD_OK variable (#487)"
    );
    assert!(
        body.contains("KERNEL_HOLD_OK=0")
            && body.contains("WARNING: apt-mark hold of the generic kernel packages failed"),
        "{SETUP} must set KERNEL_HOLD_OK=0 in the SAME failure branch that prints the hold-failed \
         WARNING (#487)"
    );
    assert!(
        body.contains("if [ \"$KERNEL_HOLD_OK\" -eq 1 ]; then")
            && body.contains("kernel apt-mark hold FAILED, see WARNING above"),
        "{SETUP}: the step's closing summary echo must branch on KERNEL_HOLD_OK -- claiming \
         \"kernel pinned\" unconditionally would misreport a real hold failure as success (#487)"
    );
}

/// #487: unattended-upgrades must NOT be masked/disabled wholesale on imag — #485 already made
/// the deliberate choice to keep security updates flowing (only their schedule pinned). #487
/// instead blacklists the kernel packages specifically and pins Automatic-Reboot=false.
#[test]
fn setup_imag_kernel_lockdown_487_does_not_disable_unattended_upgrades_wholesale() {
    let body = read(SETUP);
    assert!(
        body.contains("/etc/apt/apt.conf.d/51imag-kernel-lockdown"),
        "{SETUP} must write /etc/apt/apt.conf.d/51imag-kernel-lockdown (#487)"
    );
    for needle in [
        "Unattended-Upgrade::Package-Blacklist",
        "\"linux-image\";",
        "\"linux-headers\";",
        "\"linux-generic\";",
        "\"linux-lowlatency\";",
        "\"lowlatency-kernel\";",
        "Unattended-Upgrade::Automatic-Reboot \"false\";",
    ] {
        assert!(
            body.contains(needle),
            "{SETUP} #487 kernel-lockdown drop-in must contain `{needle}`"
        );
    }
    assert!(
        !body.contains("mask unattended-upgrades")
            && !body.contains("disable --now unattended-upgrades"),
        "{SETUP}: #487 must NOT mask/disable unattended-upgrades wholesale on imag — #485 already \
         made the deliberate choice to keep security updates flowing on this box (only the \
         SCHEDULE is pinned); #487 blacklists just the kernel packages instead"
    );
}

/// #487/#295: the initrd-guarantee postinst hook must be installed so any FUTURE kernel install
/// (even one that slips past the apt-mark hold) always gets an initrd before grub can default to
/// it — the exact mechanism that would have prevented CAM3/CAM4's brick.
#[test]
fn setup_imag_installs_initrd_guarantee_postinst_hook_487() {
    let body = read(SETUP);
    assert!(
        body.contains("/etc/kernel/postinst.d/zz-camera-box-initrd-guarantee"),
        "{SETUP} must install the /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee hook (#487, \
         ported verbatim from setup-device.sh's #295 fix)"
    );
    assert!(
        body.contains("chmod +x /etc/kernel/postinst.d/zz-camera-box-initrd-guarantee"),
        "{SETUP} must make the initrd-guarantee hook executable"
    );
}

/// #487: the safe-grub mechanism must be a REUSABLE helper function (never a raw ad-hoc grub
/// edit) that (a) guarantees every installed kernel has an initrd before update-grub runs, and
/// (b) refuses to trust the regenerated grub.cfg if the default entry lacks a kernel image or an
/// initrd. This is the SAME contract as setup-device.sh's inline STEP 10, factored into a
/// function so it is reused by both #482 and #483 below instead of duplicated.
#[test]
fn setup_imag_safe_grub_regen_helper_defined_with_full_295_contract() {
    let body = read(SETUP);
    let func_start = body
        .find("safe_grub_regen() {")
        .expect("{SETUP} must define a safe_grub_regen() helper function (#487)");
    let func_end = body[func_start..]
        .find("\n}\n")
        .map(|off| func_start + off)
        .expect("safe_grub_regen function body must close with a bare `}` line");
    let func_body = &body[func_start..func_end];
    assert!(
        func_body.contains("/boot/vmlinuz-*") && func_body.contains("update-initramfs -c -k"),
        "safe_grub_regen must guarantee every installed kernel has an initrd before update-grub \
         (#295/#487)"
    );
    assert!(
        func_body.contains("update-grub"),
        "safe_grub_regen must actually call update-grub"
    );
    assert!(
        func_body.contains("menuentry ") && func_body.contains("grub.cfg"),
        "safe_grub_regen must read the generated grub.cfg and extract its default menuentry"
    );
    let validates_initrd = func_body
        .lines()
        .any(|l| l.contains("grep") && l.contains("initrd"));
    assert!(
        validates_initrd,
        "safe_grub_regen must grep the extracted default entry for an initrd line and fail loud \
         if absent — without it grub could still default-boot an initrd-less kernel (#295)"
    );
    assert!(
        func_body.contains("fail \"#295:"),
        "safe_grub_regen must fail loud (via the script's own fail()) when the default entry is \
         unsafe, not just warn"
    );
}

/// #482: imag must install `linux-lowlatency-hwe-24.04` (the CONFIG package that drops
/// preempt=full onto the EXISTING 6.17 generic kernel) — never a real lowlatency kernel IMAGE,
/// which at the 6.17 line would be a DOWNGRADE (live-verified finding, #482 comment).
#[test]
fn setup_imag_installs_lowlatency_config_not_a_kernel_downgrade_482() {
    let body = read(SETUP);
    assert!(
        body.contains("apt-get install -y linux-lowlatency-hwe-24.04"),
        "{SETUP} must install linux-lowlatency-hwe-24.04 (#482) — the meta/config package that \
         pulls in `lowlatency-kernel` without swapping the kernel image"
    );
    assert!(
        body.contains("dpkg -s lowlatency-kernel"),
        "{SETUP} must check for the `lowlatency-kernel` config package (idempotent guard against \
         re-running apt-get on every re-provision)"
    );
    // No `apt-get install` line may target a BARE lowlatency kernel IMAGE package (e.g.
    // `linux-image-lowlatency` or a bare `linux-lowlatency` metapackage without the `-hwe-24.04`
    // config-package suffix) — that would be the live-verified DOWNGRADE (newest lowlatency
    // images are 6.8/6.11 vs the 6.17 generic kernel already running). Scoped to install COMMAND
    // lines only, so this does not false-trip on the unrelated `"linux-lowlatency";`
    // Unattended-Upgrade::Package-Blacklist entry the #487 step also writes.
    let bad_install = body.lines().any(|l| {
        let t = l.trim_start();
        t.contains("apt-get install")
            && (t.contains("linux-image-lowlatency")
                || t.contains(" linux-lowlatency ")
                || t.contains(" linux-lowlatency\""))
            && !t.contains("linux-lowlatency-hwe-24.04")
    });
    assert!(
        !bad_install,
        "{SETUP} must NEVER `apt-get install` a bare lowlatency KERNEL IMAGE package — at the \
         6.17 line the newest lowlatency images are 6.8/6.11, a DOWNGRADE that loses 13th-gen \
         CPU/iGPU/USB-NIC support (live-verified finding, #482)"
    );
}

/// #482: the script must VERIFY (post-install, without rebooting) that the lowlatency-kernel
/// config package actually dropped a grub.d file carrying preempt=full — never just trust that
/// the apt install succeeded silently.
#[test]
fn setup_imag_verifies_preempt_full_present_after_lowlatency_install_482() {
    let body = read(SETUP);
    assert!(
        body.contains("[ -f /etc/default/grub.d/99-lowlatency.cfg ]"),
        "{SETUP} must verify /etc/default/grub.d/99-lowlatency.cfg exists after installing the \
         lowlatency-kernel config package (#482)"
    );
    assert!(
        body.contains("grep -q 'preempt=full' /etc/default/grub.d/99-lowlatency.cfg"),
        "{SETUP} must grep 99-lowlatency.cfg for `preempt=full` and fail loud if absent — refuse \
         to trust the config package silently (#482)"
    );
    // Found in review: a bare `"takes effect on the NEXT boot"` substring also matches the
    // UNRELATED #483 CPU-isolation step's own reboot-caveat echo (scripts/setup-imag.sh's
    // "CPU isolation takes effect on the NEXT boot..." line) — deleting the #482 line entirely
    // would still leave this assertion passing. Anchor on the `preempt=full` prefix, which is
    // unique to the #482 note, to actually pin the feature this test claims to verify.
    assert!(
        body.contains("preempt=full takes effect on the NEXT boot"),
        "{SETUP} must note that preempt=full only takes effect on the NEXT boot — this \
         provisioning script does not reboot the box itself (#482)"
    );
}

/// #482/#487: the newly-installed lowlatency-kernel config packages must ALSO be held (apt-mark
/// hold), same discipline as the pre-existing generic kernel packages — an unattended upgrade
/// must never be able to silently swap this config out from under the deployed preempt=full.
#[test]
fn setup_imag_holds_lowlatency_config_packages_after_install_482() {
    let body = read(SETUP);
    assert!(
        body.contains("apt-mark hold lowlatency-kernel linux-lowlatency-hwe-24.04"),
        "{SETUP} must `apt-mark hold lowlatency-kernel linux-lowlatency-hwe-24.04` right after \
         installing them (#482/#487) — otherwise an unattended upgrade could silently revert the \
         preempt=full config"
    );
}

/// #483: the CPU-isolation grub.d drop-in must carry the EXACT live-verified cmdline — the P-core
/// block cpu2-11 isolated for OBS, nohz_full scoped ONLY to the future genlock render-tick pair
/// (10,11 — not the whole block, per #303's load-balancing-signal caveat), and IRQs parked off
/// the isolated block.
#[test]
fn setup_imag_writes_isolation_grub_dropin_with_exact_cmdline_483() {
    let body = read(SETUP);
    assert!(
        body.contains("/etc/default/grub.d/98-imag-isolation.cfg"),
        "{SETUP} must write /etc/default/grub.d/98-imag-isolation.cfg (#483)"
    );
    assert!(
        body.contains(
            r#"GRUB_CMDLINE_LINUX_DEFAULT="$GRUB_CMDLINE_LINUX_DEFAULT isolcpus=2,3,4,5,6,7,8,9,10,11 nohz_full=10,11 irqaffinity=0,1,12,13,14,15""#
        ),
        "{SETUP} 98-imag-isolation.cfg must carry the EXACT live-verified cmdline: \
         isolcpus=2-11 (whole P-core block for OBS), nohz_full=10,11 ONLY (the future genlock \
         render-tick core pair, not the whole block — #303), irqaffinity=0,1,12,13,14,15 (#483)"
    );
    assert!(
        !body.contains("rcu_nocbs=10,11") && !body.contains("rcu_nocbs=2,3,4,5,6,7,8,9,10,11"),
        "{SETUP}: 98-imag-isolation.cfg must NOT duplicate rcu_nocbs — #482's 99-lowlatency.cfg \
         already sets rcu_nocbs=all (which covers 10,11), reconciled per the #482 LIVE-DONE \
         comment"
    );
}

/// #483: the isolation drop-in write must be followed by a CALL to safe_grub_regen (not merely
/// its definition) so grub.cfg is actually regenerated with the initrd-guarantee + validation —
/// never a raw `update-grub` bypassing the #295 safety net.
#[test]
fn setup_imag_isolation_step_calls_safe_grub_regen_not_raw_update_grub_483() {
    let body = read(SETUP);
    let isolation_write = body
        .find("/etc/default/grub.d/98-imag-isolation.cfg")
        .expect("the isolation grub.d write must exist");
    // The LAST occurrence of "safe_grub_regen" in the file must be a bare CALL (not the `() {`
    // definition) — i.e. step 8 actually invokes the helper after writing its own drop-in.
    let last_mention = body
        .rfind("safe_grub_regen")
        .expect("safe_grub_regen must be mentioned (defined + called)");
    let trailing = &body[last_mention..];
    assert!(
        trailing.starts_with("safe_grub_regen\n"),
        "{SETUP}: the LAST occurrence of `safe_grub_regen` must be a bare call on its own line \
         (step 8 invoking the helper), not the `() {{` definition — a defined-but-never-called \
         helper would leave 98-imag-isolation.cfg (and 99-lowlatency.cfg) never picked up by \
         grub-mkconfig"
    );
    assert!(
        isolation_write < last_mention,
        "{SETUP}: safe_grub_regen must be CALLED after the 98-imag-isolation.cfg drop-in is \
         written (#483)"
    );
    // Never a raw ad-hoc `update-grub` call OUTSIDE the safe_grub_regen helper itself — the whole
    // point of #487/#295 is that grub is NEVER regenerated without the initrd-guarantee first.
    // Found in review: an exact `t == "update-grub"` match has a latent gap -- it would miss a
    // future rogue call written as `update-grub 2>&1`, `update-grub --foo`, or with a trailing
    // redirect/comment (the first-word-of-the-line IS the invoked command regardless of what
    // follows it). Match on the first whitespace-separated token instead of the whole line.
    let raw_update_grub_calls = body
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with('#') && t.split_whitespace().next() == Some("update-grub")
        })
        .count();
    assert_eq!(
        raw_update_grub_calls, 1,
        "{SETUP}: `update-grub` must be invoked EXACTLY once, inside safe_grub_regen — any other \
         bare `update-grub` call would be a raw ad-hoc grub edit bypassing the #295 initrd \
         guarantee"
    );
}

/// #483/#522: OBS must be pinned to the isolated P-core block cpu2-11 on BOTH launch paths — the
/// boot-time openbox autostart script AND the script's own provisioning-time launcher. Without
/// this, isolcpus (once active after the next boot) would STARVE OBS onto cpu0,1,12-15 — the
/// tiny remainder reserved for GNOME/housekeeping/E-cores (live-verified finding, #483 comment).
/// #522 replaced the old GNOME `.config/autostart/obs.desktop` sed-patch mechanism (dead code on
/// this lightdm+openbox box, which never read XDG autostart) with a real openbox autostart
/// script — the taskset pin now lives THERE instead of a sed-patched .desktop `Exec=` line.
#[test]
fn setup_imag_pins_obs_to_pcore_block_on_both_launch_paths_483() {
    let body = read(SETUP);
    assert!(
        body.contains("taskset -c 2-11 obs &"),
        "{SETUP}: the openbox autostart script (#522) must launch OBS via `taskset -c 2-11 obs` \
         — without it, isolcpus would starve the boot-launched OBS onto cpu0,1,12-15"
    );
    assert!(
        body.contains("nohup taskset -c 2-11 obs >/tmp/obs-launch.log"),
        "{SETUP} must launch OBS via `taskset -c 2-11 obs` in the provisioning-time launcher \
         (#483), matching the boot-time openbox autostart script"
    );
}

/// #483: `nice -n -5` must NOT be added to either OBS launch path — live-confirmed the desktop
/// user lacks CAP_SYS_NICE, so a nice call would either fail loud (breaking the launch) or be a
/// silently-ignored no-op; the #483 comment explicitly records this was dropped on the real box.
#[test]
fn setup_imag_does_not_add_nice_to_obs_launchers_483() {
    let body = read(SETUP);
    assert!(
        !body.contains("nice -n -5 obs") && !body.contains("nice -n -5 taskset"),
        "{SETUP} must NOT add `nice -n -5` to either OBS launch path — the desktop user lacks \
         CAP_SYS_NICE (live-confirmed, #483 comment); only the taskset P-core-block pin applies"
    );
}

/// #536 (revert of #525): provisioning must NOT touch OBS's "Lock UI" setting (user.ini
/// [BasicWindow] DocksLocked) at all — neither at provision (seed_ini) nor at boot (the openbox
/// autostart). Locking the UI is a real, wanted OBS feature the operator controls; #525's actual
/// cause was simply that the UI was locked, and the fix is telling the operator to unlock it once
/// themselves, not silently overriding their choice on every boot.
#[test]
fn setup_imag_does_not_touch_docks_locked_536() {
    let body = read(SETUP);
    assert!(
        !body.contains("DocksLocked"),
        "{SETUP} must NOT reference DocksLocked anywhere — provisioning leaves OBS's \"Lock UI\" \
         setting entirely to the operator (#536 revert of the #525 hard-force)"
    );
}

/// #483/#522: the Desktop icon (double-click launch) must stay UNPINNED (`Exec=obs`, no
/// taskset) — only the boot-time openbox autostart script is pinned to the P-core block. #522
/// removed the old `.config/autostart/obs.desktop` GNOME entry entirely; #526 additionally has
/// setup-imag.sh `rm -f` any leftover of it (systemd --user double-launches OBS from it). So this
/// test forbids WRITING that dead XDG entry — removing a leftover with `rm -f` is allowed.
#[test]
fn setup_imag_leaves_desktop_icon_unpinned_483() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"cp -f "$APP_DESKTOP" "$USER_HOME/Desktop/obs.desktop""#),
        "{SETUP}: the Desktop double-click icon must still be a byte-identical copy of the \
         vendor .desktop (Exec=obs, unpinned) — only the boot-time openbox autostart script \
         (#522) is pinned to the P-core block"
    );
    assert!(
        !body.contains("sed -i 's/^Exec=obs$/Exec=taskset -c 2-11 obs/'"),
        "{SETUP}: must NOT sed-patch ANY .desktop file's Exec= line any more (#522) — the \
         taskset pin now lives in the openbox autostart script body, not a patched .desktop entry"
    );
    assert!(
        !body.contains(r#"cp -f "$APP_DESKTOP" "$USER_HOME/.config/autostart/obs.desktop""#),
        "{SETUP}: must no longer WRITE .config/autostart/obs.desktop — modern Ubuntu's systemd \
         --user launches it as a second OBS; .config/openbox/autostart is the SOLE boot-durable \
         authority (#522). (Removing a leftover of it with `rm -f` IS allowed — #526.)"
    );
}

/// TOTAL_STEPS must match the actual number of `step()` calls in the script — a drift here means
/// either a step was added without bumping the counter (progress display under-counts) or the
/// counter was bumped without adding a step (display over-counts). This is a general invariant,
/// re-verified here because #500 added a new NVIDIA driver step (16 -> 17).
#[test]
fn setup_imag_total_steps_matches_actual_step_calls() {
    let body = read(SETUP);
    let declared: usize = body
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("TOTAL_STEPS="))
        .and_then(|s| s.trim().parse().ok())
        .expect("TOTAL_STEPS=<N> must be declared");
    // Count actual `step N "..."` INVOCATIONS (not the `step() { ... }` function definition
    // itself, and not any comment/prose mentioning the word).
    let actual = body
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("step ")
                && !t.starts_with("step() {")
                && t.chars().nth(5).is_some_and(|c| c.is_ascii_digit())
        })
        .count();
    assert_eq!(
        declared, 18,
        "TOTAL_STEPS must be 18 after #504 added the kiosk-environment step (openbox+lightdm+GNOME \
         purge) to the prior 17"
    );
    assert_eq!(
        actual, declared,
        "{SETUP}: TOTAL_STEPS ({declared}) must match the actual number of `step N \"...\"` \
         invocations ({actual}) — a mismatch means the progress display under/over-counts"
    );
}

// ============================================================================================
// #499 — the genlock hot-swap must ALSO swap the OBS FRONTEND binary /usr/bin/obs. Codifies a
// fix already applied + LIVE-VERIFIED on imag-nb (2026-07-04): the multiview render-budget
// decouple (#276/#278/#293) and the "newlevel.media" window title live in the frontend EXE
// (vendor/obs-studio/frontend/), NOT libobs.so.30 — a genlock deploy that skips /usr/bin/obs
// leaves a half-stock box (multiview choked the program render to 16fps/59ms on the stock
// frontend vs 60fps/1.7ms after the swap).
// ============================================================================================

/// The hot-swap must overwrite the REAL frontend executable path, not just the two libraries.
#[test]
fn setup_imag_hotswaps_frontend_binary_499() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"OBS_FRONTEND_REAL="/usr/bin/obs""#),
        "{SETUP} must define OBS_FRONTEND_REAL=/usr/bin/obs — the genlock hot-swap must ALSO \
         swap the frontend executable, not just libobs.so.30/distroav.so (#499)"
    );
    assert!(
        body.contains(r#"BUNDLE_OBS="$GENLOCK_TMP/bundle/bin/obs""#),
        "{SETUP} must resolve the bundle's bin/obs path — the genlock bundle (obs-genlock-linux-\
         x86_64) already carries the built frontend executable at bin/obs"
    );
}

/// #499's frontend sha must be looked up via manifest_sha_for_path and actually verify_file_sha'd
/// — the same integrity discipline already applied to libobs.so.30/distroav.so. A frontend binary
/// installed without a sha check would have zero integrity guarantee.
#[test]
fn setup_imag_verifies_frontend_bin_obs_via_bundle_manifest() {
    let body = read(SETUP);
    let want_obs = body
        .find("WANT_OBS_SHA=\"$(manifest_sha_for_path")
        .expect("frontend bin/obs expected sha must be looked up via manifest_sha_for_path");
    let verify_obs = body
        .find("verify_file_sha \"$BUNDLE_OBS\" \"$WANT_OBS_SHA\"")
        .expect("bundle bin/obs must actually be verify_file_sha'd against its looked-up sha");
    assert!(
        want_obs < verify_obs,
        "{SETUP}: WANT_OBS_SHA must be resolved BEFORE the verify_file_sha call for bin/obs"
    );
    assert!(
        body.contains("'bin/obs'"),
        "{SETUP}: the manifest lookups for the frontend binary must use the literal manifest \
         relpath 'bin/obs' (matches the #120 BUNDLE_MANIFEST.json entry, live-confirmed on \
         imag-nb: sha b53294a9...)"
    );
}

/// The stock PPA frontend must be backed up ONCE, before the first-ever swap, at the SAME
/// live-verified path already hand-created on imag-nb (/opt/obs-backup/obs.stock) — never a
/// per-run accumulating name (same #185 discipline as the libobs/distroav stock backup).
#[test]
fn setup_imag_backs_up_stock_frontend_once_499() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"OBS_FRONTEND_STOCK_BACKUP="$GENLOCK_BACKUP_ROOT/obs.stock""#),
        "{SETUP} must back up the stock frontend to $GENLOCK_BACKUP_ROOT/obs.stock — the exact \
         path already hand-verified live on imag-nb (sha 9898bf32... == the stock PPA /usr/bin/obs)"
    );
    let guard = body
        .find(r#"if [ ! -f "$OBS_FRONTEND_STOCK_BACKUP" ]; then"#)
        .expect(
            "the stock frontend backup must be guarded to happen ONLY ONCE (never on a re-swap)",
        );
    let install = body
        .find(r#"install -m 0755 -o root -g root "$BUNDLE_OBS" "$OBS_FRONTEND_REAL""#)
        .expect("the frontend install call must exist");
    assert!(
        guard < install,
        "{SETUP}: the stock frontend backup must happen BEFORE the frontend is overwritten"
    );
}

/// The frontend install must preserve EXECUTE permissions (0755) — unlike the two libraries
/// (0644) — since /usr/bin/obs is invoked directly as a program, not dlopen'd.
#[test]
fn setup_imag_installs_frontend_with_exec_perms_499() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"install -m 0755 -o root -g root "$BUNDLE_OBS" "$OBS_FRONTEND_REAL""#),
        "{SETUP} must `install -m 0755` the frontend binary to /usr/bin/obs — it is an \
         executable, not a shared library (which get 0644 like libobs.so.30/distroav.so)"
    );
}

/// A post-swap build-proof for the frontend, mirroring the SONAME check already done for
/// libobs.so.30: the installed /usr/bin/obs must actually reference the multiview render-budget
/// decouple symbol (obs_display_set_render_divisor, #276/#278/#293) — live-verified via
/// `nm -D -u` on imag-nb after the hand-swap. A stock/wrong binary must never be silently
/// accepted as "swapped".
#[test]
fn setup_imag_verifies_frontend_render_divisor_symbol_postswap_499() {
    let body = read(SETUP);
    assert!(
        body.contains("nm -D -u \"$OBS_FRONTEND_REAL\"")
            && body.contains("obs_display_set_render_divisor"),
        "{SETUP} must `nm -D -u` the swapped /usr/bin/obs and grep for \
         obs_display_set_render_divisor — the stock PPA frontend never references this symbol \
         (live-confirmed); its absence means a stock/wrong binary was installed"
    );
    assert!(
        body.contains("refuse a stock/wrong frontend binary"),
        "{SETUP} must fail loud (not warn) when the render-divisor symbol check fails"
    );
    // Found in review: `nm -D -u` on this binary emits ~2900 lines (~170KB, live-measured) and
    // the target symbol sits at line ~286 -- a `grep -q` would exit right after that early match,
    // SIGPIPE-ing `nm` mid-write, and under `set -euo pipefail` that would wrongly fail() a
    // CORRECT build (same footgun class the SONAME check above already documents/avoids).
    let symbol_check_line = body
        .lines()
        .find(|l| l.contains("nm -D -u \"$OBS_FRONTEND_REAL\""))
        .expect("the nm -D -u pipeline line must exist");
    assert!(
        !symbol_check_line.contains("grep -q"),
        "{SETUP}: the nm -D -u | grep check for obs_display_set_render_divisor must NOT use \
         `grep -q` — an early match closes the pipe before `nm` finishes writing its ~170KB of \
         output, SIGPIPEs `nm`, and under pipefail wrongly fails a CORRECT build. Use a plain \
         `grep 'pattern' >/dev/null` instead (matches the SONAME check's own convention)"
    );
}

/// The idempotency no-op check (#472 defense-in-depth) must ALSO cover the frontend binary — a
/// re-run that only checks libobs.so.30/distroav.so bytes could wrongly report "already deployed"
/// while the frontend silently reverted to stock (e.g. an unattended apt reinstall of obs-studio,
/// which owns /usr/bin/obs via dpkg).
#[test]
fn setup_imag_frontend_included_in_idempotency_reverify_499() {
    let body = read(SETUP);
    assert!(
        body.contains(
            r#"[ -f "$LIBOBS_REAL" ] && [ -f "$DISTROAV_REAL" ] && [ -f "$OBS_FRONTEND_REAL" ]"#
        ),
        "{SETUP}: the NOOP_VALID existence check must require the frontend binary too, not just \
         libobs.so.30/distroav.so"
    );
    assert!(
        body.contains("WANT_OBS_SHA_CACHED") && body.contains("GOT_OBS_SHA_CACHED"),
        "{SETUP}: the cached-manifest re-verify (#472) must ALSO compare the frontend binary's \
         installed bytes against the cached manifest, not just libobs.so.30/distroav.so"
    );
}

/// All three swapped files (libobs.so.30, distroav.so, and the frontend) must be installed
/// together in the SAME deploy block, before the SAME GENLOCK_BUILD_SHA.txt marker is written —
/// they version together under one build SHA, never independently.
#[test]
fn setup_imag_frontend_versions_together_with_libobs_and_distroav_499() {
    let body = read(SETUP);
    let install_libobs = body
        .find(r#"install -m 0644 -o root -g root "$BUNDLE_LIBOBS" "$LIBOBS_REAL""#)
        .expect("libobs install must exist");
    let install_distroav = body
        .find(r#"install -m 0644 -o root -g root "$FAST_DISTROAV" "$DISTROAV_REAL""#)
        .expect("distroav install must exist");
    let install_frontend = body
        .find(r#"install -m 0755 -o root -g root "$BUNDLE_OBS" "$OBS_FRONTEND_REAL""#)
        .expect("frontend install must exist");
    let marker_write = body
        .find(r#"echo "$NEW_SHA" > "$GENLOCK_MARKER_DIR/GENLOCK_BUILD_SHA.txt""#)
        .expect("the build-SHA marker write must exist");
    assert!(
        install_libobs < install_distroav
            && install_distroav < install_frontend
            && install_frontend < marker_write,
        "{SETUP}: libobs, distroav, and the frontend must ALL be installed before the single \
         GENLOCK_BUILD_SHA.txt marker is written — all three files version together under one \
         build SHA"
    );
}

// ============================================================================================
// #500 — setup-imag.sh must install nvidia-driver-595-open + PRIME nvidia-primary. Codifies a
// fix already applied + LIVE-VERIFIED on imag-nb (2026-07-04): the HDMI program-projector output
// is wired through the NVIDIA dGPU (RTX 5050 Laptop / Blackwell), which the plain proprietary
// nvidia-driver-595 package fails to initialize (RmInitAdapter failed 0x22:0x56:1017) — only the
// -open kernel-modules flavor brings it up.
// ============================================================================================

/// The -open flavor must be installed — plain nvidia-driver-595 does NOT init this Blackwell dGPU
/// (live-reproduced), even though `ubuntu-drivers devices` recommends the plain package.
#[test]
fn setup_imag_installs_nvidia_driver_595_open_500() {
    let body = read(SETUP);
    assert!(
        body.contains("nvidia-driver-595-open"),
        "{SETUP} must install nvidia-driver-595-open — the OPEN kernel-modules flavor is required \
         to initialize the RTX 5050 Laptop (Blackwell) dGPU; plain nvidia-driver-595 fails with \
         RmInitAdapter failed 0x22:0x56:1017 (live-reproduced on imag-nb)"
    );
    let install_check = body.find("dpkg -s nvidia-driver-595-open").expect(
        "{SETUP} must check dpkg -s nvidia-driver-595-open before (re-)installing — idempotency",
    );
    let apt_install = body
        .find("apt-get install -y nvidia-driver-595-open")
        .expect("{SETUP} must apt-get install -y nvidia-driver-595-open");
    assert!(
        install_check < apt_install,
        "{SETUP}: the dpkg -s idempotency check must come BEFORE the apt-get install call"
    );
}

/// PRIME must be set to nvidia (not on-demand) — on-demand mode left the HDMI dGPU output dead
/// (live-verified); nvidia-primary brings up BOTH the HDMI output and the laptop's own eDP panel.
#[test]
fn setup_imag_prime_select_nvidia_500() {
    let body = read(SETUP);
    assert!(
        body.contains("prime-select nvidia"),
        "{SETUP} must `prime-select nvidia` — on-demand PRIME mode left the HDMI dGPU output dead \
         (live-verified on imag-nb); nvidia-primary is required for both HDMI and eDP to run on \
         the RTX 5050"
    );
    assert!(
        body.contains("prime-select missing after nvidia-driver-595-open install"),
        "{SETUP} must fail loud if prime-select is missing after the driver install"
    );
}

/// A DKMS driver install regenerates initramfs for the running kernel — the #295/#487 safe-grub
/// discipline must be re-applied (never trust an initrd/grub change blindly), reusing the SAME
/// safe_grub_regen helper the #482/#483 grub.d drops already call, not a fresh ad-hoc grub edit.
#[test]
fn setup_imag_nvidia_step_reuses_safe_grub_regen_500() {
    let body = read(SETUP);
    // There must be exactly two CALL sites of safe_grub_regen (never a bare function definition
    // counted as a call): the #482/#483 CPU-isolation step, and this NVIDIA step.
    let call_sites = body
        .lines()
        .filter(|l| l.trim() == "safe_grub_regen")
        .count();
    assert_eq!(
        call_sites, 2,
        "{SETUP}: safe_grub_regen must be called exactly twice — once after the #482/#483 grub.d \
         drops, and once after the #500 nvidia driver install"
    );
    let prime = body
        .find("prime-select nvidia")
        .expect("prime-select nvidia must exist");
    let nvidia_step_regen = body[prime..]
        .find("safe_grub_regen")
        .map(|off| prime + off)
        .expect("safe_grub_regen must be called AFTER prime-select nvidia in the #500 step");
    let marker = body
        .find("nvidia-smi already enumerates")
        .expect("the post-driver nvidia-smi check must exist");
    assert!(
        nvidia_step_regen < marker,
        "{SETUP}: safe_grub_regen must run BEFORE the nvidia-smi liveness echo in the #500 step \
         (never trust initrd/grub state without re-verifying it first)"
    );
}

/// The #500 step must land AFTER CPU isolation (#483, so safe_grub_regen is already defined and
/// has already run once) and BEFORE the NDI runtime step — grouping all boot-level system config
/// (kernel, CPU isolation, GPU driver) ahead of the app-level installs (NDI, OBS).
#[test]
fn setup_imag_nvidia_step_lands_between_cpu_isolation_and_ndi_runtime_500() {
    let body = read(SETUP);
    let cpu_isolation = body
        .find("CPU isolation (#483)")
        .expect("the #483 CPU isolation step must exist");
    let nvidia_step = body
        .find("NVIDIA dGPU driver (#500)")
        .expect("the #500 NVIDIA driver step must exist");
    let ndi_step = body
        .find("NDI runtime 6.3.2 from cam1")
        .expect("the NDI runtime step must exist");
    assert!(
        cpu_isolation < nvidia_step && nvidia_step < ndi_step,
        "{SETUP}: the #500 NVIDIA driver step must land strictly between the #483 CPU isolation \
         step and the NDI runtime step"
    );
}

/// A grub/initrd-touching change must never claim to take effect immediately — the same "NOTE:
/// takes effect on the NEXT boot" convention already used by the #482 lowlatency kernel and the
/// #483 CPU isolation steps (this script never reboots the box).
#[test]
fn setup_imag_nvidia_step_notes_next_boot_500() {
    let body = read(SETUP);
    assert!(
        body.contains("the PRIME GPU mode + the new DKMS module take full effect on the NEXT boot")
            && body.contains("this script does not reboot the box"),
        "{SETUP}: the #500 nvidia step must note that the PRIME mode + DKMS module take effect on \
         the NEXT boot, matching the convention already used by the #482/#483 grub-touching steps"
    );
}

/// Driver-upgrade freedom is explicitly wanted by the user — this pin must not be silently
/// treated as an immovable LTS choice; the comment must document that a newer -open flavor should
/// be preferred if one becomes available.
#[test]
fn setup_imag_nvidia_step_documents_driver_upgrade_freedom_500() {
    let body = read(SETUP);
    assert!(
        body.contains("prefer the newest available `-open` flavor over 595 if one has")
            || body.contains("prefer the newest available -open flavor"),
        "{SETUP}: the #500 nvidia step must document that a newer -open driver flavor should be \
         preferred over the 595 pin if one becomes available (user's explicit driver-upgrade-\
         freedom directive)"
    );
}

// ============================================================================================
// #522/#488 — imag-nb reboot-durable openbox autostart. Root cause: nothing in the repo wrote
// the box's real boot-time state (~/.config/openbox/autostart, hand-edited on the box, NEVER
// reproduced by setup-imag.sh) — a reboot silently regressed to the WRONG primary output (HDMI
// instead of the panel) and dropped the #507 projector self-heal. setup-imag.sh is now the SOLE
// writer of this file.
// ============================================================================================

/// setup-imag.sh must write an EXECUTABLE ~/.config/openbox/autostart — this is the file
/// lightdm+openbox actually reads on login/boot (unlike the dead XDG .config/autostart/ dir).
#[test]
fn setup_imag_writes_openbox_autostart_522() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"$USER_HOME/.config/openbox/autostart"#),
        "{SETUP} must generate ~/.config/openbox/autostart — the file openbox (lightdm session) \
         actually reads on boot; unlike .config/autostart/*.desktop (GNOME/XDG only, dead on \
         this box), this is the real reboot-durable authority (#522)"
    );
    assert!(
        body.contains(r#"chmod +x "$USER_HOME/.config/openbox/autostart""#),
        "{SETUP}: the generated openbox autostart script must be made executable (chmod +x) or \
         openbox silently never runs it"
    );
}

/// The autostart script must set the non-HDMI PANEL primary at 1920x1080@60 — and must NEVER
/// apply --primary to the HDMI projector output (the exact #522/#488 regression: the
/// hand-edited autostart wrongly primaried HDMI instead of the panel).
#[test]
fn setup_imag_autostart_primaries_panel_not_hdmi_522() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"$1 !~ /^HDMI/"#) && body.contains(r#"$1 ~  /^HDMI/"#),
        "{SETUP}: the autostart script must select PANEL as the connected non-HDMI output and \
         PROJ as the connected HDMI output (xrandr awk filters) — matches imag_scenes.py's \
         HDMI-vs-panel rule (#522)"
    );
    assert!(
        body.contains(r#"xrandr --output "$PANEL" --primary --mode 1920x1080 --rate 60"#),
        "{SETUP}: the autostart script must set the PANEL primary at 1920x1080@60 — the \
         #522/#488 regression was the hand-edited autostart wrongly setting HDMI primary instead"
    );
    // The bug this ticket fixes: --primary must be scoped to $PANEL only, never to $PROJ/HDMI.
    assert!(
        !body.contains(r#"xrandr --output "$PROJ" --primary"#),
        "{SETUP}: the autostart script must NEVER set --primary on $PROJ (the HDMI projector) — \
         that was the exact #522/#488 hand-edited-box regression this ticket fixes"
    );
}

/// The autostart script must launch OBS taskset-pinned to the isolated P-core block, exactly as
/// the provisioning-time launcher does (#483) — a reboot must not silently starve OBS.
#[test]
fn setup_imag_autostart_launches_obs_pinned_522() {
    let body = read(SETUP);
    assert!(
        body.contains("taskset -c 2-11 obs &"),
        "{SETUP}: the openbox autostart script must launch OBS via `taskset -c 2-11 obs` (#483) \
         — without it, isolcpus would starve the boot-launched OBS onto cpu0,1,12-15"
    );
}

/// The autostart script must wait for the OBS WebSocket (:4455) to come up BEFORE seeding — a
/// race here would run imag_scenes.py against a not-yet-listening OBS and silently no-op.
#[test]
fn setup_imag_autostart_waits_for_websocket_before_seeding_522() {
    let body = read(SETUP);
    let wait_loop = body
        .find("/dev/tcp/127.0.0.1/4455")
        .expect("{SETUP}: the autostart script must wait on 127.0.0.1:4455 before seeding");
    let obs_launch = body
        .find("taskset -c 2-11 obs &")
        .expect("obs launch must exist in the autostart script");
    assert!(
        obs_launch < wait_loop,
        "{SETUP}: OBS must be launched BEFORE the autostart script waits for its WebSocket to \
         come up"
    );
}

/// The autostart script must self-heal BOTH the scene/#507-multiview-membership seed AND the
/// projector layout on EVERY boot — a reboot must never require a human to re-run
/// imag_scenes.py by hand.
#[test]
fn setup_imag_autostart_seeds_and_opens_projectors_522() {
    let body = read(SETUP);
    assert!(
        body.contains(r#"--host 127.0.0.1"#),
        "{SETUP}: the autostart script must invoke imag_scenes.py against the LOCAL OBS \
         (127.0.0.1) — it runs ON the box, not remotely from dev1"
    );
    let seed_call = body
        .find(r#""$PYBIN" "$SCN" --host 127.0.0.1 >"#)
        .expect("{SETUP}: the autostart script must run the seed (no --projector) invocation");
    let projector_call = body
        .find(r#""$PYBIN" "$SCN" --host 127.0.0.1 --projector"#)
        .expect("{SETUP}: the autostart script must ALSO run the --projector invocation");
    assert!(
        seed_call < projector_call,
        "{SETUP}: the seed invocation (scenes/#507 multiview membership) must run BEFORE \
         --projector, so the projectors open against a fully-seeded scene collection"
    );
}

/// setup-imag.sh must actually INSTALL imag_scenes.py + a websocket-client dependency onto the
/// box at a fixed path, and the __PYBIN__/__SCN__ placeholders left in the quoted heredoc must
/// actually get substituted — a literal "__PYBIN__" left in the generated file would make the
/// boot-time self-heal a permanent no-op. The boot hook cannot depend on a hand-made venv or a
/// checked-out copy of the repo (setup-imag.sh runs standalone on the box, per its own step-12
/// comment: "no sibling scripts/... checked out there").
#[test]
fn setup_imag_installs_imag_scenes_py_and_websocket_dep_522() {
    let body = read(SETUP);
    assert!(
        body.contains("python3-websocket"),
        "{SETUP} must install the python3-websocket system package — the boot-hook \
         imag_scenes.py imports `websocket` and cannot depend on a hand-made venv (#522)"
    );
    assert!(
        body.contains(r#"SCN="/usr/local/bin/imag_scenes.py""#),
        "{SETUP} must resolve SCN to a fixed on-box install path for imag_scenes.py (#522)"
    );
    assert!(
        body.contains("scripts/imag_scenes.py")
            && (body.contains("gh api") || body.contains("curl")),
        "{SETUP} must actually fetch/install scripts/imag_scenes.py onto the box (via gh api or \
         curl) — not just reference the path"
    );
    let sed_sub = body
        .find(r#"sed -i "s#__PYBIN__#${PYBIN}#; s#__SCN__#${SCN}#""#)
        .expect(
            "{SETUP} must sed-substitute the __PYBIN__/__SCN__ placeholders with the actual \
             resolved paths right after writing the heredoc — a literal __PYBIN__ left in the \
             generated file would make the boot-time self-heal a permanent no-op",
        );
    let heredoc_write = body
        .find(r#"cat > "$USER_HOME/.config/openbox/autostart" <<'AUTOSTART_EOF'"#)
        .expect("the openbox autostart heredoc write must exist");
    assert!(
        heredoc_write < sed_sub,
        "{SETUP}: the placeholder substitution must happen AFTER the heredoc is written"
    );
}

// =============================================================================
// #504 — imag-nb kiosk codification: GNOME desktop → openbox+lightdm appliance.
//
// imag-nb was hand-converted live to an openbox kiosk (owner comment 2026-07-04); these guards
// pin that whole conversion into setup-imag.sh so a from-scratch provision lands in the kiosk, not
// GNOME — and so a later edit cannot silently drop the DM switch (which, missing, black-walled the
// box) or turn the bounded GNOME purge into a blind autoremove cascade (which could reach the
// KEEP services the appliance depends on). The owner's KEEP / DISABLE / PURGE lists are the
// authoritative contract (issue #504 body + comments); these tests assert the script honours them
// verbatim. Style matches the guards above: read the REAL script, assert the REAL contract.
// =============================================================================

/// The kiosk needs the light WM + display manager actually installed (a from-scratch box ships
/// only GNOME) — the owner's "convert to openbox+lightdm" directive is a no-op if they are never
/// apt-installed. #504.
#[test]
fn setup_imag_504_installs_openbox_and_lightdm() {
    let body = read(SETUP);
    assert!(
        body.lines().any(|l| {
            l.contains("apt-get install") && l.contains("openbox") && l.contains("lightdm")
        }),
        "{SETUP} (#504) must `apt-get install` BOTH openbox and lightdm — a from-scratch box has \
         neither, and the kiosk cannot exist without them"
    );
}

/// lightdm must autologin the desktop user straight into an OPENBOX session — the headless box has
/// no operator to type a password, and the session must be openbox (not GNOME) for the kiosk. #504.
#[test]
fn setup_imag_504_writes_lightdm_autologin_openbox_session() {
    let body = read(SETUP);
    assert!(
        body.contains("/etc/lightdm/lightdm.conf.d/50-imag-autologin.conf"),
        "{SETUP} (#504) must write the lightdm autologin drop-in \
         /etc/lightdm/lightdm.conf.d/50-imag-autologin.conf"
    );
    assert!(
        body.contains("[Seat:*]"),
        "{SETUP} (#504) the lightdm autologin conf must carry the [Seat:*] header"
    );
    assert!(
        body.contains("autologin-user=${DESKTOP_USER}") && body.contains(r#"DESKTOP_USER="newlevel""#),
        "{SETUP} (#504) must set autologin-user to ${{DESKTOP_USER}} (=newlevel) — the headless box \
         has no one to log in by hand"
    );
    assert!(
        body.contains("autologin-session=openbox"),
        "{SETUP} (#504) must set autologin-session=openbox — the kiosk WM, NOT GNOME"
    );
}

/// The display-manager MUST be switched to lightdm by an EXPLICIT symlink, never `systemctl enable
/// lightdm` — the owner hit `systemctl enable`'s "instance name specified" failure which left
/// /etc/systemd/system/display-manager.service absent → the box booted with NO DM (black wall +
/// extra reboot, 2026-07-04). #504.
#[test]
fn setup_imag_504_switches_display_manager_symlink_to_lightdm() {
    let body = read(SETUP);
    assert!(
        body.contains(
            "ln -sf /lib/systemd/system/lightdm.service /etc/systemd/system/display-manager.service"
        ),
        "{SETUP} (#504) must set the display-manager.service symlink to lightdm EXPLICITLY \
         (`ln -sf …lightdm.service …display-manager.service`) — `systemctl enable lightdm` failed \
         to create it and black-walled the box"
    );
    // A `systemctl enable lightdm` would be the exact anti-pattern the owner flagged — it must not
    // be how the DM gets switched.
    assert!(
        !body.contains("systemctl enable lightdm")
            && !body.contains("systemctl enable --now lightdm"),
        "{SETUP} (#504) must NOT rely on `systemctl enable lightdm` to switch the DM — it fails \
         'instance name specified' and does not create the display-manager symlink"
    );
}

/// Every desktop-bloat service on the owner's DISABLE list must be disabled — they waste resources
/// on a production appliance. #504.
///
/// gdm3 is asserted SEPARATELY from the `--now` loop (review finding, 2026-07-05): on a genuine
/// from-scratch GNOME box, gdm3 still owns the CURRENT `:0` session that the later OBS-launch step
/// depends on — stopping it immediately (`--now`) would kill that session mid-provision. gdm3 must
/// still be disabled (so it never starts again after the reboot to lightdm), just not stopped NOW.
#[test]
fn setup_imag_504_disables_bloat_services() {
    let body = read(SETUP);
    let list_start = body
        .find("for svc in ")
        .expect("{SETUP} (#504) must have a `for svc in …` service-disable loop")
        + "for svc in ".len();
    let list_end = list_start
        + body[list_start..]
            .find("; do")
            .expect("{SETUP} (#504) the disable loop must be `for svc in …; do`");
    let disable_list = &body[list_start..list_end];
    for svc in [
        "cups",
        "cups-browsed",
        "bluetooth",
        "ModemManager",
        "colord",
        "switcheroo-control",
        "gnome-remote-desktop",
    ] {
        assert!(
            disable_list.split_whitespace().any(|w| w == svc),
            "{SETUP} (#504) the service-disable loop must include `{svc}` (owner DISABLE list)"
        );
    }
    assert!(
        !disable_list.split_whitespace().any(|w| w == "gdm3"),
        "{SETUP} (#504) gdm3 must NOT be in the `--now` (immediate-stop) loop — on a from-scratch \
         box it may still own the live `:0` session the OBS-launch step needs; it gets a SEPARATE \
         disable-without-`--now` call instead"
    );
    assert!(
        body.contains(r#"systemctl disable --now "$svc""#),
        "{SETUP} (#504) must `systemctl disable --now` each listed service (stop + disable)"
    );
    assert!(
        body.contains("systemctl disable gdm3 >/dev/null 2>&1 || true"),
        "{SETUP} (#504) must disable gdm3 (for the NEXT boot) WITHOUT `--now` — stopping it \
         immediately could kill a live `:0` GNOME session mid-provision on a from-scratch box"
    );
}

/// The GNOME purge must be an EXPLICIT owner-listed package set purged with `apt-get purge`, and
/// must NEVER be a bare `apt-get autoremove` — a blind autoremove would sweep every now-orphaned
/// forward-dependency (unbounded cascade, the exact hazard the owner called out). #504.
#[test]
fn setup_imag_504_purges_gnome_explicit_never_autoremove() {
    let body = read(SETUP);
    let p_start = body
        .find(r#"GNOME_PURGE_PKGS=""#)
        .expect("{SETUP} (#504) must define an explicit GNOME_PURGE_PKGS package list");
    let p_rest = &body[p_start + r#"GNOME_PURGE_PKGS=""#.len()..];
    let p_end = p_rest
        .find('"')
        .expect("{SETUP} (#504) GNOME_PURGE_PKGS must be a quoted string");
    let purge_list = &p_rest[..p_end];
    for pkg in [
        "gnome-shell",
        "gdm3",
        "nautilus",
        "firefox",
        "gnome-remote-desktop",
        "gnome-shell-extension-ubuntu-dock",
        "gnome-shell-extension-ubuntu-tiling-assistant",
        "gnome-shell-extension-appindicator",
    ] {
        assert!(
            purge_list.split_whitespace().any(|w| w == pkg),
            "{SETUP} (#504) GNOME_PURGE_PKGS must list `{pkg}` explicitly (owner PURGE list)"
        );
    }
    assert!(
        body.contains("apt-get purge"),
        "{SETUP} (#504) must actually `apt-get purge` the GNOME package list"
    );
    // NO bare autoremove COMMAND anywhere (a comment explaining WHY not is allowed).
    let autoremove_command = body
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .any(|l| l.contains("autoremove"));
    assert!(
        !autoremove_command,
        "{SETUP} (#504) must NEVER run `apt-get autoremove` — an unbounded forward-dep sweep is the \
         exact hazard the owner banned; purge the EXPLICIT list only"
    );
}

/// The KEEP set (avahi/NDI, sshd, dantesync, remoteos-mcp/MCP agent, NetworkManager, lightdm,
/// openbox) is what the appliance runs on — it must NEVER appear in the disable loop or the purge
/// list. A regression that adds one there would knock the box off the network / kill NDI / lose the
/// MCP agent, exactly the failure the reboot-recoverable directive guards against. #504.
#[test]
fn setup_imag_504_never_disables_or_purges_the_keep_set() {
    let body = read(SETUP);
    let d_start = body
        .find("for svc in ")
        .expect("{SETUP} (#504) must have a `for svc in …` service-disable loop")
        + "for svc in ".len();
    let d_end = d_start
        + body[d_start..]
            .find("; do")
            .expect("{SETUP} (#504) the disable loop must be `for svc in …; do`");
    let disable_list = &body[d_start..d_end];
    let p_start = body
        .find(r#"GNOME_PURGE_PKGS=""#)
        .expect("{SETUP} (#504) must define an explicit GNOME_PURGE_PKGS package list")
        + r#"GNOME_PURGE_PKGS=""#.len();
    let p_end = p_start
        + body[p_start..]
            .find('"')
            .expect("{SETUP} (#504) GNOME_PURGE_PKGS must be a quoted string");
    let purge_list = &body[p_start..p_end];
    for keep in [
        "avahi",
        "avahi-daemon",
        "sshd",
        "ssh",
        "openssh-server",
        "dantesync",
        "remoteos-mcp",
        "NetworkManager",
        "network-manager",
        "lightdm",
        "openbox",
    ] {
        assert!(
            !disable_list.split_whitespace().any(|w| w == keep),
            "{SETUP} (#504) KEEP service `{keep}` must NOT be in the disable loop"
        );
        assert!(
            !purge_list.split_whitespace().any(|w| w == keep),
            "{SETUP} (#504) KEEP package `{keep}` must NOT be in the GNOME purge list"
        );
    }
}

/// Owner's HARD ORDER (2026-07-04 incident): install openbox+lightdm AND switch the DM symlink to
/// lightdm BEFORE the GNOME purge — purging gdm3 with no lightdm/DM-symlink yet left the box with
/// NO display manager (black wall). #504.
#[test]
fn setup_imag_504_installs_and_switches_dm_before_purge() {
    let body = read(SETUP);
    let install_idx = body
        .find("apt-get install -y openbox lightdm")
        .expect("openbox+lightdm install present");
    let symlink_idx = body
        .find("ln -sf /lib/systemd/system/lightdm.service /etc/systemd/system/display-manager.service")
        .expect("DM symlink present");
    let purge_idx = body.find("apt-get purge").expect("GNOME purge present");
    assert!(
        install_idx < purge_idx,
        "{SETUP} (#504) openbox+lightdm must be INSTALLED before the GNOME purge (owner's black-wall \
         incident) — a box must never be left with no working WM/DM"
    );
    assert!(
        symlink_idx < purge_idx,
        "{SETUP} (#504) the display-manager symlink must be switched to lightdm BEFORE gdm3 is \
         purged — otherwise the box boots with NO display manager (black wall)"
    );
    assert!(
        install_idx < symlink_idx,
        "{SETUP} (#504) openbox+lightdm must be INSTALLED before the DM symlink is switched — the \
         script's own runtime guard refuses to switch the symlink until lightdm.service exists on \
         disk, so the install must come first"
    );
}

/// Defense-in-depth (review finding, 2026-07-05): the DM symlink must be RE-VERIFIED after the
/// GNOME purge, not just set once before it — gdm3's dpkg postrm runs AFTER the symlink switch, and
/// a postrm that silently re-pointed display-manager.service back would be exactly the black-wall
/// failure mode this step exists to prevent. #504.
#[test]
fn setup_imag_504_reasserts_dm_symlink_after_purge() {
    let body = read(SETUP);
    let symlink_idx = body
        .find("ln -sf /lib/systemd/system/lightdm.service /etc/systemd/system/display-manager.service")
        .expect("DM symlink switch present");
    let purge_idx = body.find("apt-get purge").expect("GNOME purge present");
    let reassert_idx = body
        .find(r#"[ "$(readlink -f /etc/systemd/system/display-manager.service)" = "/lib/systemd/system/lightdm.service" ]"#)
        .expect(
            "{SETUP} (#504) must re-verify the display-manager symlink AFTER the GNOME purge — a \
             package postrm (gdm3) running after the initial switch could silently re-point it back",
        );
    assert!(
        purge_idx < reassert_idx,
        "{SETUP} (#504) the DM-symlink re-assert must run AFTER the GNOME purge, not before — \
         otherwise it can't catch a postrm that re-points the symlink"
    );
    assert!(
        symlink_idx < reassert_idx,
        "{SETUP} (#504) the re-assert must come after the initial symlink switch"
    );
    assert!(
        body.contains(
            r##"fail "#504: display-manager.service no longer points at lightdm after the GNOME purge"##
        ),
        "{SETUP} (#504) the re-assert must FAIL LOUD (not warn) if the symlink drifted — leaving \
         the box with an uncertain DM is the exact black-wall risk"
    );
}

/// Root-cause extension of the gdm3-ordering fix (review finding, 2026-07-05): purging the gdm3
/// PACKAGE runs its own maintainer scripts, which stop the service on removal REGARDLESS of the
/// `disable` (no `--now`) call in step 15(d) — so on a genuine from-scratch box, gdm3's package
/// purge can still tear down the CURRENT `:0` session the OBS-launch step (17) depends on. The
/// launch step must detect a dead `:0` and degrade gracefully (defer to the next-boot autostart)
/// instead of hard-failing the whole provisioning run over an EXPECTED intermediate state. #504.
#[test]
fn setup_imag_504_obs_launch_degrades_gracefully_when_x_session_is_dead() {
    let body = read(SETUP);
    assert!(
        body.contains("[ -S /tmp/.X11-unix/X0 ]"),
        "{SETUP} (#504) the OBS-launch step must check whether :0's X11 socket is actually alive \
         before attempting to launch OBS into it — step 15's GNOME purge can have torn it down"
    );
    let x_check_idx = body
        .find("[ -S /tmp/.X11-unix/X0 ]")
        .expect("X11 socket check present");
    let old_hardfail_idx = body.find(r#"pgrep -x obs >/dev/null || fail "OBS did not start"#);
    assert!(
        old_hardfail_idx.is_some_and(|i| x_check_idx < i),
        "{SETUP} (#504) the 'OBS did not start' hard-fail must be reachable ONLY behind the X11 \
         socket check — a dead :0 (expected on a from-scratch box) must not hard-fail the whole run"
    );
    assert!(
        body.contains("OBS_LAUNCHED_THIS_RUN=1") && body.contains("OBS_LAUNCHED_THIS_RUN=0"),
        "{SETUP} (#504) must track whether OBS actually launched THIS run (both the success and \
         the deferred-to-next-boot paths must set the flag) so step 18's verify can key off it"
    );
    assert!(
        body.contains("auto-launch via the lightdm+openbox"),
        "{SETUP} (#504) the deferred path must explain WHY (no live :0) and WHAT happens next \
         (autostart on next boot) — a silent skip would look like an unexplained no-op"
    );
}

/// Step 18's WebSocket/genlock/NDI verify must be GATED on OBS having actually launched this run —
/// otherwise the same from-scratch dead-:0 state makes step 18 hard-fail on ":4455 not listening"
/// for a fully-expected reason (step 17 correctly deferred the launch to next boot). #504.
#[test]
fn setup_imag_504_verify_step_gated_on_obs_launched_this_run() {
    let body = read(SETUP);
    let gate_idx = body
        .find(r#"if [ "$OBS_LAUNCHED_THIS_RUN" -eq 1 ]; then"#)
        .expect("{SETUP} (#504) step 18's verify body must be gated on OBS_LAUNCHED_THIS_RUN");
    let ws_wait_idx = body
        .find("OBS WebSocket :4455 not listening")
        .expect("the WS :4455 hard-fail must still exist for the launched-this-run path");
    let genlock_marker_idx = body
        .find("NOT the genlock build (check the #460 hot-swap step)")
        .expect("the genlock log-verify hard-fail must still exist for the launched-this-run path");
    let skip_msg_idx = body
        .find("skipping the WebSocket/genlock/NDI")
        .expect("{SETUP} (#504) must explain that the verify was skipped, and why");
    assert!(
        gate_idx < ws_wait_idx && gate_idx < genlock_marker_idx && gate_idx < skip_msg_idx,
        "{SETUP} (#504) the OBS_LAUNCHED_THIS_RUN gate must wrap BOTH the WS-wait and the genlock \
         log-verify hard-fails, with the skip message in the else branch"
    );
}

// ============================================================================================
// #484 -- the genlock render-tick thread pin (vendor/obs-studio/libobs/obs-video.c) calls
// sched_setscheduler(SCHED_FIFO) on the ONE timing-critical graphics thread. OBS runs as the
// unprivileged desktop user, so without an rtprio ulimit grant that syscall fails EPERM and the
// pin silently degrades to SCHED_OTHER (the pin's warn-and-continue fallback). The provisioner
// must write the limits.d drop-in that grants it, near the #483 CPU-isolation reservation.
// ============================================================================================

/// The provisioner must WRITE an idempotent /etc/security/limits.d/ drop-in granting the desktop
/// user `rtprio`, so OBS's #484 genlock render-tick pin can actually enter SCHED_FIFO.
#[test]
fn setup_imag_grants_rtprio_for_genlock_rt_pin_484() {
    let body = read(SETUP);
    assert!(
        body.contains("/etc/security/limits.d/95-imag-genlock-rtprio.conf"),
        "{SETUP} must WRITE the limits.d drop-in \
         /etc/security/limits.d/95-imag-genlock-rtprio.conf — without an rtprio ulimit grant, OBS \
         (running as the unprivileged desktop user) cannot set SCHED_FIFO and the #484 genlock \
         render-tick pin degrades to SCHED_OTHER"
    );
    // The actual grant line must give the desktop user `rtprio` headroom (20 > the ~10 the thread
    // requests). It is a limits.conf content line (not a shell/file comment), so exclude `#` lines.
    let has_grant = body.lines().any(|l| {
        let t = l.trim_start();
        !t.starts_with('#')
            && t.contains("rtprio")
            && t.contains("${DESKTOP_USER}")
            && t.contains("20")
    });
    assert!(
        has_grant,
        "{SETUP} the rtprio drop-in must grant `${{DESKTOP_USER}} - rtprio 20` (headroom above the \
         ~10 the #484 render-tick thread requests) — the ulimit that lets a non-root user request \
         SCHED_FIFO"
    );
    // It must be reserved together with the #483 CPU-isolation those cores exist for: the drop-in
    // is written after the #483 grub-isolation step (they are one concern — reserve + grant).
    let iso_idx = body
        .find("isolcpus=2,3,4,5,6,7,8,9,10,11 nohz_full=10,11")
        .expect("the #483 CPU-isolation cmdline must still be present");
    let rtprio_idx = body
        .find("/etc/security/limits.d/95-imag-genlock-rtprio.conf")
        .expect("the #484 rtprio drop-in must be present");
    assert!(
        iso_idx < rtprio_idx,
        "{SETUP}: the #484 rtprio grant must be written alongside/after the #483 CPU-isolation \
         reservation (the reserved cores + the rtprio grant are one appliance-hardening concern)"
    );
}
