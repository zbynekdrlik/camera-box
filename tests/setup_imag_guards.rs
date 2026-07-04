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
        call_lines, 4,
        "{SETUP}: expected exactly 4 manifest_sha_for_path call sites — the original install-time \
         verify (libobs.so.30 + distroav.so) PLUS the #472 no-op re-verify (same two files, \
         looked up again from the CACHED manifest) — found {call_lines}; update this test if the \
         call count genuinely changed"
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
        .find("nohup obs >/tmp/obs-launch.log")
        .expect("{SETUP} must launch obs via nohup");
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
// #487 — port setup-device.sh's #295 brick-prevention stack onto imag-nb: kernel apt-hold,
// initrd-guarantee postinst hook, and an unattended-upgrade kernel lockdown, PLUS a reusable
// safe_grub_regen() helper (never a raw ad-hoc grub edit). This is the safety net the upcoming
// #482 (lowlatency kernel) and #483 (CPU isolation) grub.d changes are built on. Already
// LIVE-APPLIED groundwork on imag-nb (2026-07-04) — this codifies it into setup-imag.sh so a
// from-scratch re-provision reproduces the same boot-safety posture.
// ============================================================================================

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

// ============================================================================================
// #482 — preempt=full via linux-lowlatency-hwe-24.04, with ZERO kernel downgrade. LIVE-VERIFIED
// finding: there is no lowlatency kernel IMAGE at the 6.17 line (newest are 6.8/6.11 — a
// downgrade); the 6.17 generic kernel is already PREEMPT_DYNAMIC, so the lowlatency-kernel CONFIG
// package alone drops preempt=full onto it. Already LIVE-APPLIED on imag-nb (2026-07-04); this
// codifies it into setup-imag.sh.
// ============================================================================================

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
    assert!(
        body.contains("takes effect on the NEXT boot"),
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
