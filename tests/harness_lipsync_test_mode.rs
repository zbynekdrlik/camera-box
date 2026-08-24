//! issue 930 + issue 1187 — `scripts/lipsync-test-mode.sh`'s pure remote-command builders, sourced
//! and called directly. NEVER touches cam2 or the network — mirrors rig-mode.sh's own
//! painter_launch_remote/painter_stop_remote convention and the harness style already established
//! for it (`tests/harness_cam2_painter_provisioning_863.rs`).
//!
//! issue 1187 (root fix of issue 1176 prong 3): the playback transport moved OFF raw `/dev/fb0`
//! (legacy fbdev, which leaves a stale frame in fb0 memory after ffmpeg is killed) ONTO DRM/KMS via
//! `mpv --vo=drm` — mpv page-flips its OWN buffers at vblank, never touches fb0, and cleanly
//! restores the CRTC on exit. The A/V lead moved from a two-demux ffmpeg `-itsoffset` hack to mpv's
//! native `--audio-delay`; the old fbdev-specific pacing guard (whose whole reason to exist was
//! "/dev/fb0 has no clock of its own") is replaced by a lightweight mpv decode + presence PREFLIGHT
//! that touches neither fb0 nor the CRTC; and the stop path now blanks fb0 belt-and-braces (the
//! #660 mechanism) so the kernel fbdev emulation can never reveal a stale frame after mpv releases
//! the DRM master.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lipsync-test-mode.sh")
}

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn run_sourced(call: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(". \"$1\"; {call}"))
        .arg("bash")
        .arg(script())
        .output()
        .expect("spawn bash");
    assert!(
        out.status.success(),
        "sourced call `{call}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn lipsync_test_mode_script_exists_and_is_executable() {
    let meta = fs::metadata(script())
        .unwrap_or_else(|e| panic!("scripts/lipsync-test-mode.sh missing: {e}"));
    assert!(meta.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "scripts/lipsync-test-mode.sh must be executable"
        );
    }
}

/// The painter-stop command MUST key on the SAME pidfile constant rig-mode.sh's own TEST-mode
/// painter uses (`/run/rig-painter.pid` by default) -- a mismatch would silently target the
/// wrong (or no) process. Never a bare `pkill -f frame-probe` (would also match this very ssh
/// command's own cmdline -- the exact discipline rig-mode.sh's painter_stop_remote documents).
#[test]
fn stop_painter_cmds_kills_by_pidfile_never_pkill_by_name() {
    let cmds = run_sourced("lipsync_stop_painter_cmds /run/rig-painter.pid");
    assert!(cmds.contains("/run/rig-painter.pid"));
    assert!(cmds.contains("kill"));
    assert!(
        !cmds.contains("pkill"),
        "930: must kill by pidfile, never a name-based pkill (self-match risk): {cmds}"
    );

    let rig_mode = read("scripts/rig-mode.sh");
    assert!(
        rig_mode.contains("PAINTER_PIDFILE=\"${PAINTER_PIDFILE:-/run/rig-painter.pid}\""),
        "930: lipsync-test-mode.sh's default PAINTER_PIDFILE must match rig-mode.sh's own \
         constant -- it is stopping/restoring the SAME painter process"
    );
    let this_script = read("scripts/lipsync-test-mode.sh");
    assert!(
        this_script.contains("PAINTER_PIDFILE=\"${PAINTER_PIDFILE:-/run/rig-painter.pid}\""),
        "930: this script's default PAINTER_PIDFILE must stay byte-identical to rig-mode.sh's"
    );
}

// --------------------------------------------------------------------------------------------- //
// issue 1187 — the DRM/KMS playback transport (mpv --vo=drm), replacing the raw fbdev ffmpeg path.
// --------------------------------------------------------------------------------------------- //

/// The playback command must feed ONE `mpv` process both the DRM/KMS video sink (`--vo=drm`) AND
/// the SAME ALSA device (`--audio-device=alsa/...`, forced stereo -- the live sanity test found the
/// device refuses mono) from a SINGLE process -- never two processes that could drift, and never a
/// raw `/dev/fb0` write.
#[test]
fn playback_cmds_feeds_one_mpv_process_both_sinks_1187() {
    let cmds = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    // "nohup mpv" only appears on the ACTUAL invocation line (the failure-message prose also
    // mentions the word "mpv", so counting bare "mpv" substrings would be wrong).
    let mpv_invocations = cmds.matches("nohup mpv").count();
    assert_eq!(
        mpv_invocations, 1,
        "1187: exactly ONE mpv invocation (single process feeding both sinks), never two that \
         could drift apart: {cmds}"
    );
    assert!(
        cmds.contains("--vo=drm"),
        "1187: video must go to DRM/KMS (--vo=drm), never raw fbdev: {cmds}"
    );
    assert!(
        cmds.contains("--audio-device=alsa/hw:CARD=PCH,DEV=3"),
        "1187: audio must go to the SAME ALSA device via mpv's alsa/ device syntax: {cmds}"
    );
    assert!(
        cmds.contains("--audio-channels=stereo"),
        "1187: force stereo -- the live sanity test found the ALSA device refuses mono: {cmds}"
    );
    assert!(
        cmds.contains("--loop-file=inf"),
        "1187: must loop -- the ~60s asset must cover an arbitrary-length recording window: {cmds}"
    );
    assert!(cmds.contains("/run/rig-lipsync-playback.pid"));
}

/// The transport change's WHOLE POINT: playback must NEVER write raw `/dev/fb0` again (no `-f
/// fbdev`, no fbdev sink of any kind). This is the structural fix for issue 1176's stale-frame leak.
#[test]
fn playback_cmds_never_writes_raw_fb0_1187() {
    let with_lead = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid 408",
    );
    let zero_lead = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid 0",
    );
    for (label, cmds) in [("lead=408", &with_lead), ("lead=0", &zero_lead)] {
        assert!(
            !cmds.contains("fbdev"),
            "1187 ({label}): playback must never use the fbdev sink again: {cmds}"
        );
        assert!(
            !cmds.contains("-f fbdev") && !cmds.contains("/dev/fb0"),
            "1187 ({label}): playback must never write raw /dev/fb0: {cmds}"
        );
    }
}

/// The playback command must FAIL LOUD (not silently proceed) if mpv dies immediately after launch
/// -- mirrors rig-mode.sh's own painter-liveness verification convention (never claim a launch
/// succeeded without checking the process is actually alive).
#[test]
fn playback_cmds_fails_loud_if_mpv_dies_immediately_1187() {
    let cmds = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    assert!(
        cmds.contains("kill -0") && cmds.contains("FAIL"),
        "1187: must verify the launched pid is actually alive and FAIL loud if not: {cmds}"
    );
}

/// LIPSYNC_AUDIO_LEAD_MS=0 (the knob's off position) must produce a NO-SHIFT audio-delay and still
/// exactly ONE mpv process reading the asset ONCE -- the knob must be a true no-op at zero. Also
/// true when the arg is omitted entirely (back-compat with every pre-1187 call site/test).
#[test]
fn playback_cmds_zero_lead_is_a_no_op_shift_1187() {
    let without_lead_arg = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    let with_zero_lead = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid 0",
    );
    assert_eq!(
        without_lead_arg, with_zero_lead,
        "1187: LIPSYNC_AUDIO_LEAD_MS=0 (or the arg omitted) must be byte-identical: \
         without={without_lead_arg} with_zero={with_zero_lead}"
    );
    assert_eq!(
        without_lead_arg.matches("nohup mpv").count(),
        1,
        "1187: the zero-lead path keeps exactly ONE mpv process: {without_lead_arg}"
    );
    assert!(
        without_lead_arg.contains("--audio-delay=0.000"),
        "1187: zero lead must map to a no-op --audio-delay=0.000: {without_lead_arg}"
    );
}

/// LIPSYNC_AUDIO_LEAD_MS > 0 must compensate via mpv's native `--audio-delay` -- a NEGATIVE value,
/// because mpv semantics are "positive delays audio, negative delays VIDEO", and the calibrated
/// compensation delays the VIDEO relative to audio (the exact equivalent of the old ffmpeg positive
/// `-itsoffset` on the video input). Still exactly ONE mpv process reading the asset ONCE (no
/// second demux -- mpv applies the offset internally).
#[test]
fn playback_cmds_applies_audio_lead_via_mpv_audio_delay_1187() {
    let cmds = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid 408",
    );
    assert_eq!(
        cmds.matches("nohup mpv").count(),
        1,
        "1187 audio-lead: still ONE mpv process (mpv applies the offset internally, no second \
         demux/process): {cmds}"
    );
    assert_eq!(
        cmds.matches("-i '/run/lipsync-test.mp4'").count(),
        0,
        "1187: mpv takes the media as a positional arg, not ffmpeg's `-i` (no second demux): {cmds}"
    );
    assert!(
        cmds.contains("--audio-delay=-0.408"),
        "1187: LIPSYNC_AUDIO_LEAD_MS=408 must become a NEGATIVE --audio-delay=-0.408 (mpv: \
         negative delays video, the equivalent of the old ffmpeg video +itsoffset): {cmds}"
    );
    assert!(
        cmds.contains("audio_lead_ms=408"),
        "1187: the success message must report the applied lead for operator visibility: {cmds}"
    );
}

/// A fractional-ms lead (not a round hundreds value) must still convert cleanly to a negative
/// seconds delay -- proves the ms->s conversion isn't hardcoded/special-cased for 408 alone.
#[test]
fn playback_cmds_converts_an_arbitrary_lead_ms_to_a_negative_delay_1187() {
    let cmds = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid 125",
    );
    assert!(
        cmds.contains("--audio-delay=-0.125"),
        "1187: 125ms must become --audio-delay=-0.125: {cmds}"
    );
}

/// An empty DRM_DEVICE arg lets mpv auto-select the connected KMS card (#854: `/dev/dri/cardN`
/// numbering is not a stable ABI, so auto is the safe default). A non-empty value pins it via
/// `--drm-device`.
#[test]
fn playback_cmds_pins_drm_device_only_when_set_1187() {
    let auto = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    assert!(
        !auto.contains("--drm-device"),
        "1187: empty DRM_DEVICE must let mpv auto-select the KMS card (no --drm-device): {auto}"
    );
    let pinned = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 /dev/dri/card1 hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    assert!(
        pinned.contains("--drm-device=/dev/dri/card1"),
        "1187: a set DRM_DEVICE must pin mpv's KMS card via --drm-device: {pinned}"
    );
}

// --------------------------------------------------------------------------------------------- //
// issue 1191 — the playback speech must be peak-normalized (+N dB, default 9) into the mic-chain
// AGC operating point. The asset speech (peak -9.8 dBFS) is ~25dB under the AGC operating point set
// by the loud QPSK marker (~0 dBFS), so un-boosted speech captures ~-50 dBFS and SyncNet reads
// conf ~1 on EVERY chunk (unmeasurable). A `--af=volume` filter with the LIPSYNC_PLAYBACK_GAIN_DB
// env seam (default 9) fixes it (live-verified: envelope corr 0.976, SyncNet conf 6.4 at +9dB).
// --------------------------------------------------------------------------------------------- //

/// The single mpv playback invocation must carry a peak-normalizing gain filter
/// `--af=volume=${LIPSYNC_PLAYBACK_GAIN_DB:-9}dB` -- the env seam is expanded on the REMOTE (cam2)
/// side so the default (9) is baked self-documenting into the generated command and the supervisor
/// can re-tune the gain via the paired cross-check campaign WITHOUT a code change (same seam
/// philosophy as LIPSYNC_AUDIO_LEAD_MS). Orthogonal to `--audio-delay`: present whether a lead is
/// applied or not, and always on the SAME single mpv process (never a second one).
#[test]
fn playback_cmds_carries_playback_gain_af_seam_1191() {
    let cmds = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    assert!(
        cmds.contains("--af=volume=${LIPSYNC_PLAYBACK_GAIN_DB:-9}dB"),
        "1191: the mpv playback line must peak-normalize speech via --af=volume with the \
         LIPSYNC_PLAYBACK_GAIN_DB env seam (default 9, remote-side expansion) -- else un-boosted \
         asset speech stays ~25dB under the marker-set AGC operating point (SyncNet unmeasurable): \
         {cmds}"
    );
    assert_eq!(
        cmds.matches("nohup mpv").count(),
        1,
        "1191: the gain filter must be an arg on the single mpv process, never a second process: \
         {cmds}"
    );
    // The gain seam is orthogonal to the A/V lead -- it must be present with a lead applied too.
    let with_lead = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid 408",
    );
    assert!(
        with_lead.contains("--af=volume=${LIPSYNC_PLAYBACK_GAIN_DB:-9}dB"),
        "1191: the gain seam must be present regardless of the audio-lead value: {with_lead}"
    );
}

// --------------------------------------------------------------------------------------------- //
// issue 1187 — the mpv decode + presence PREFLIGHT (replaces the fbdev-specific pacing guard).
// --------------------------------------------------------------------------------------------- //

/// The preflight must probe decode via mpv NULL sinks only -- it must touch NEITHER `/dev/fb0` NOR
/// the DRM/KMS CRTC (no `--vo=drm`, no fbdev), so running it before the painter is even restored
/// can never leave a stale frame or fight the display. The old fbdev-cadence apparatus is gone: mpv
/// paces off vblank natively, so there is no fbdev-no-clock bug left to measure.
#[test]
fn preflight_cmd_uses_null_sinks_never_touches_fb0_or_drm_1187() {
    let cmds = run_sourced("lipsync_preflight_cmd /run/lipsync-test.mp4");
    assert!(
        cmds.contains("--vo=null") && cmds.contains("--ao=null"),
        "1187: the preflight must decode to NULL sinks only: {cmds}"
    );
    assert!(
        !cmds.contains("fbdev") && !cmds.contains("/dev/fb0"),
        "1187: the preflight must never write raw /dev/fb0: {cmds}"
    );
    assert!(
        !cmds.contains("--vo=drm"),
        "1187: the preflight must not take the DRM/KMS CRTC (that is playback's job): {cmds}"
    );
    assert!(
        cmds.contains("command -v mpv"),
        "1187: the preflight must check mpv is installed and FAIL loud if not: {cmds}"
    );
}

/// Functional proof: a fake `mpv` on PATH that exits 0 makes the preflight PASS (decode probe
/// succeeded, mpv present).
#[test]
fn preflight_passes_when_mpv_present_and_decodes_1187() {
    let out = run_preflight_with_fake_mpv(Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "1187: a present, decoding mpv must PASS: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("preflight passed"),
        "1187: the pass message must say the preflight passed: {stdout}"
    );
}

/// Functional proof: with NO mpv on PATH the preflight FAILS loud with a message naming mpv and
/// pointing at provisioning (never a silent proceed into a playback launch that would then fail).
#[test]
fn preflight_fails_loud_when_mpv_missing_1187() {
    let out = run_preflight_with_fake_mpv(None);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "1187: a missing mpv must FAIL the preflight loud: {stderr}"
    );
    assert!(
        stderr.contains("FAIL") && stderr.contains("mpv"),
        "1187: the failure message must name mpv as the missing dependency: {stderr}"
    );
}

/// Functional proof: a fake `mpv` that exits nonzero (asset undecodable / mpv broken) FAILS the
/// preflight loud -- never a silent proceed.
#[test]
fn preflight_fails_loud_when_mpv_cannot_decode_1187() {
    let out = run_preflight_with_fake_mpv(Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "1187: an mpv that cannot decode must FAIL the preflight loud: {stderr}"
    );
    assert!(
        stderr.contains("FAIL"),
        "1187: the decode-failure message must be a clean FAIL: {stderr}"
    );
}

/// Runs `lipsync_preflight_cmd` with a controlled mpv binary via the `LIPSYNC_MPV_BIN` env seam
/// (never PATH-shadowing -- a real mpv on the test box/CI must not perturb the verdict).
/// `mpv_exit == Some(code)` points `LIPSYNC_MPV_BIN` at an ABSOLUTE-path fake `mpv` exiting with
/// that code; `None` points it at a bogus name that does not exist (missing-dependency case), so
/// `command -v "$LIPSYNC_MPV_BIN"` fails deterministically regardless of what is installed.
fn run_preflight_with_fake_mpv(mpv_exit: Option<i32>) -> std::process::Output {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let mpv_bin = match mpv_exit {
        Some(code) => {
            let fake = tmp.path().join("fake-mpv");
            fs::write(&fake, format!("#!/usr/bin/env bash\nexit {code}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
            }
            fake.display().to_string()
        }
        None => "mpv-definitely-not-installed-xyzzy".to_string(),
    };
    Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; eval \"$(lipsync_preflight_cmd /tmp/media.mp4)\"")
        .arg("bash")
        .arg(script())
        .env("LIPSYNC_MPV_BIN", mpv_bin)
        .output()
        .expect("spawn bash")
}

/// `cmd_start` must run the mpv preflight on the uploaded remote path via its OWN dedicated
/// `lipsync_preflight_cmd` call, BEFORE the persistent playback launch -- a static-text pin
/// alongside the functional proofs above.
#[test]
fn start_runs_the_preflight_before_the_persistent_playback_1187() {
    let s = read("scripts/lipsync-test-mode.sh");
    let guard_call_at = s
        .find("cam_ssh \"$(lipsync_preflight_cmd")
        .expect("1187: cmd_start must call lipsync_preflight_cmd");
    let playback_call_at = s
        .find("cam_ssh \"$(lipsync_playback_cmds")
        .expect("lipsync_playback_cmds call present");
    assert!(
        guard_call_at < playback_call_at,
        "1187: the mpv preflight must run BEFORE the persistent playback launch"
    );
}

// --------------------------------------------------------------------------------------------- //
// issue 1187 — the stop path blanks fb0 belt-and-braces (the #660 mechanism).
// --------------------------------------------------------------------------------------------- //

/// The stop-playback command must key on the SAME pidfile the start command wrote, AND blank fb0
/// belt-and-braces after the kill: after mpv exits and releases the DRM master, the kernel fbdev
/// emulation can re-take scanout from /dev/fb0 memory -- zeroing it (the #660 `dd` mechanism)
/// guarantees a black screen, never a stale frame, before rig-mode.sh restores the painter.
#[test]
fn stop_playback_cmds_kills_by_pidfile_and_blanks_fb0_1187() {
    let cmds = run_sourced("lipsync_stop_playback_cmds /run/rig-lipsync-playback.pid /dev/fb0");
    assert!(cmds.contains("/run/rig-lipsync-playback.pid"));
    assert!(cmds.contains("kill"));
    assert!(
        cmds.contains("dd if=/dev/zero of=") && cmds.contains("/dev/fb0"),
        "1187: stop must blank fb0 belt-and-braces (the #660 dd mechanism) after killing mpv: \
         {cmds}"
    );
    assert!(
        cmds.contains("bs=1M count=8"),
        "1187: the fb0 blank must reuse the canonical #660 size (8 MiB, one 1080p XRGB frame): \
         {cmds}"
    );
}

/// The fb0-blank must REUSE the canonical #660 builder (`rig_test_ledger_clean_paint_fallback_cmds`
/// from scripts/lib/rig-test-ledger.sh), not a hand-rolled second copy -- ONE source of truth for
/// the blank mechanism. Proven by sourcing the lib's builder directly and confirming the stop cmd
/// contains its exact output for the same fb device.
#[test]
fn stop_playback_fb0_blank_reuses_the_canonical_660_builder_1187() {
    let stop = run_sourced("lipsync_stop_playback_cmds /run/rig-lipsync-playback.pid /dev/fb0");
    let canonical = run_sourced("rig_test_ledger_clean_paint_fallback_cmds /dev/fb0");
    let canonical_trimmed = canonical.trim();
    assert!(
        !canonical_trimmed.is_empty(),
        "1187: the canonical #660 builder must be sourced + callable from lipsync-test-mode.sh"
    );
    assert!(
        stop.contains(canonical_trimmed),
        "1187: stop must embed the canonical #660 blank builder's output verbatim (one source of \
         truth): stop={stop} canonical={canonical_trimmed}"
    );
}

/// The stop-playback command must key on the SAME pidfile the start command wrote (kept from the
/// original #930 contract).
#[test]
fn stop_playback_cmds_kills_by_the_same_pidfile() {
    let cmds = run_sourced("lipsync_stop_playback_cmds /run/rig-lipsync-playback.pid /dev/fb0");
    assert!(cmds.contains("/run/rig-lipsync-playback.pid"));
    assert!(cmds.contains("kill"));
}

/// `stop` must call rig-mode.sh's OWN `test` mode to restore -- never a hand-rolled partial
/// restore (the acceptance criterion: "TEST mode restored and verified after every run").
#[test]
fn stop_subcommand_calls_rig_mode_sh_test_to_restore() {
    let s = read("scripts/lipsync-test-mode.sh");
    assert!(
        s.contains("rig-mode.sh") && s.contains("rig-mode.sh\" test"),
        "930: stop must restore via `rig-mode.sh test` (full re-verified restore), never a \
         hand-rolled partial one: {s}"
    );
}

/// `cmd_start` must set an ERR trap (with `errtrace` enabled so it fires even for a failure
/// inside a called function like `cam_ssh`) restoring TEST mode via `rig-mode.sh test` -- a
/// scp/ssh failure between killing the TEST-mode painter and starting the lipsync playback must
/// never leave cam2 with NEITHER the QR/QPSK painter NOR the lipsync playback running (930
/// finding 8). The trap must be cleared once `cmd_start` completes successfully, so a later
/// unrelated failure elsewhere in the script doesn't also trigger it.
#[test]
fn start_sets_an_err_trap_that_restores_test_mode_930() {
    let s = read("scripts/lipsync-test-mode.sh");
    assert!(
        s.contains("set -o errtrace"),
        "930: errtrace needed so the ERR trap fires for a failure inside a called function \
         (cam_ssh), not just a bare command: {s}"
    );
    assert!(
        s.contains(r#"trap 'bash "$HERE/rig-mode.sh" test' ERR"#),
        "930: cmd_start must set an ERR trap that restores TEST mode via rig-mode.sh: {s}"
    );
    assert!(
        s.contains("trap - ERR"),
        "930: the ERR trap must be cleared once cmd_start completes successfully: {s}"
    );
    // The trap must be set AFTER the painter is already killed (the window it protects) and
    // cleared BEFORE the function's final success message -- never wrapping the whole function.
    let set_at = s.find("set -o errtrace").expect("errtrace present");
    let kill_at = s
        .find("cam_ssh \"$(lipsync_stop_painter_cmds")
        .expect("painter kill present");
    let clear_at = s.find("trap - ERR").expect("trap clear present");
    assert!(
        kill_at < set_at && set_at < clear_at,
        "930: ERR trap must be scoped between the painter kill and the success clear"
    );
}

#[test]
fn main_with_unknown_subcommand_fails_loud() {
    let out = Command::new("bash")
        .arg(script())
        .arg("bogus")
        .output()
        .expect("spawn bash");
    assert!(!out.status.success());
}

#[test]
fn start_with_a_missing_media_file_fails_loud_before_touching_the_network() {
    let out = Command::new("bash")
        .arg(script())
        .arg("start")
        .arg("/nonexistent/path/does-not-exist.mp4")
        .output()
        .expect("spawn bash");
    assert!(
        !out.status.success(),
        "930: a missing asset must fail BEFORE any ssh/scp attempt (no network in this test env, \
         so a hang/network-error here would mean the file-existence check was skipped)"
    );
}

// --------------------------------------------------------------------------------------------- //
// issue 930 (carried into 1187/1191) — LIPSYNC_AUDIO_LEAD_MS is a static-ms A/V-lead knob. issue 930
// derived the ffmpeg/ALSA output pipeline depth D at ~408ms and used it as the DEFAULT; issue 1187
// moved the transport (ffmpeg -itsoffset -> mpv --audio-delay). issue 1191 changes the DEFAULT to 0:
// under mpv the measured offset at lead=0 is +40ms ≈ ±1 frame of zero, so 408 was a stale
// ffmpeg-era constant. The knob (env var, validation) is otherwise unchanged and 408 stays available
// via the env seam so the supervisor can re-tune without a code change.
// --------------------------------------------------------------------------------------------- //

/// `LIPSYNC_AUDIO_LEAD_MS` must default to 0 -- issue 1191: the mpv-era (issue 1187) measured offset
/// at lead=0 is +40ms ≈ ±1 frame of zero, so 0 is the correct default. 408 was the stale ffmpeg-era
/// ALSA-pipeline-depth constant (issue 930); it stays available via the env seam for re-calibration.
#[test]
fn lipsync_audio_lead_ms_env_defaults_to_0_1191() {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; echo \"$LIPSYNC_AUDIO_LEAD_MS\"")
        .arg("bash")
        .arg(script())
        .output()
        .expect("spawn bash");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "0",
        "1191: default audio-lead compensation must be 0ms (the mpv-era measured offset at lead=0 \
         is +40ms ≈ ±1 frame of zero; 408 was the stale ffmpeg-era constant): {stdout}"
    );
}

/// `cmd_start` must pass `$LIPSYNC_AUDIO_LEAD_MS` through to `lipsync_playback_cmds` as its 5th
/// arg, with `$LIPSYNC_DRM_DEVICE` as the 2nd (video-sink) arg -- a static-text pin alongside the
/// functional proofs above.
#[test]
fn start_passes_drm_device_and_audio_lead_ms_to_playback_cmds_1187() {
    let s = read("scripts/lipsync-test-mode.sh");
    assert!(
        s.contains(
            "cam_ssh \"$(lipsync_playback_cmds \"$remote_media\" \"$LIPSYNC_DRM_DEVICE\" \"$LIPSYNC_AUDIO_DEVICE\" \"$LIPSYNC_PLAYBACK_PIDFILE\" \"$LIPSYNC_AUDIO_LEAD_MS\")\""
        ),
        "1187: cmd_start must pass LIPSYNC_DRM_DEVICE (2nd) + LIPSYNC_AUDIO_LEAD_MS (5th) through \
         to lipsync_playback_cmds: {s}"
    );
}

/// A non-integer `LIPSYNC_AUDIO_LEAD_MS` must fail loud, before any ssh/scp attempt -- same
/// fail-fast discipline as the missing-media-file check above.
#[test]
fn start_fails_loud_on_a_non_integer_audio_lead_ms_930() {
    let out = Command::new("bash")
        .arg(script())
        .arg("start")
        .env("LIPSYNC_AUDIO_LEAD_MS", "abc")
        .output()
        .expect("spawn bash");
    assert!(
        !out.status.success(),
        "930: a non-integer LIPSYNC_AUDIO_LEAD_MS must fail loud before touching the network"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LIPSYNC_AUDIO_LEAD_MS"),
        "930: failure message must name the bad env var: {stderr}"
    );
}

/// A negative `LIPSYNC_AUDIO_LEAD_MS` must also fail loud -- the knob's defined semantics are
/// "0 = off, positive = advance audio by that many ms"; a negative value has no defined meaning.
#[test]
fn start_fails_loud_on_a_negative_audio_lead_ms_930() {
    let out = Command::new("bash")
        .arg(script())
        .arg("start")
        .env("LIPSYNC_AUDIO_LEAD_MS", "-5")
        .output()
        .expect("spawn bash");
    assert!(
        !out.status.success(),
        "930: a negative LIPSYNC_AUDIO_LEAD_MS must fail loud -- undefined for this knob"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LIPSYNC_AUDIO_LEAD_MS"),
        "930: failure message must name the bad env var: {stderr}"
    );
}

// --------------------------------------------------------------------------------------------- //
// issue 1190 — the steady-state painter runs under cam2-painter.service (Restart=always, issue 1008
// model), so a pidfile-ONLY kill lets systemd respawn it ~100ms later; the respawn re-takes the DRM
// master and `mpv --vo=drm` (started ~10s later, issue 1187) then cannot acquire the CRTC and dies
// instantly. The painter-stop builder must STOP THE UNIT first (systemd will not respawn a stopped
// unit) and FAIL LOUD if it stays active; and mpv (run --no-terminal, which swallows its stderr)
// must write a native --log-file so the death is diagnosable from the box.
// --------------------------------------------------------------------------------------------- //

/// The painter-stop builder must STOP the `cam2-painter` unit BEFORE the pidfile kill -- otherwise
/// systemd (`Restart=always`) respawns the painter between the kill and the mpv launch and re-takes
/// the DRM master. The pidfile kill stays AFTER it as a belt for the transient, unit-less
/// verification-only nohup painter (cam2-painter-lifecycle rule).
#[test]
fn stop_painter_cmds_stops_the_cam2_painter_unit_before_the_pidfile_kill_1190() {
    let cmds = run_sourced("lipsync_stop_painter_cmds /run/rig-painter.pid");
    let unit_stop_at = cmds
        .find("systemctl stop cam2-painter")
        .expect("1190: must stop the cam2-painter unit so systemd cannot respawn the painter");
    let pidfile_term_at = cmds
        .find("kill \"$PID\"")
        .expect("the pidfile TERM kill must still be present (belt for the transient painter)");
    assert!(
        unit_stop_at < pidfile_term_at,
        "1190: the unit stop must come BEFORE the pidfile kill (else systemd respawns the painter \
         between the two and re-takes the DRM master): {cmds}"
    );
}

/// After stopping the unit + pidfile-killing any transient painter, the builder must FAIL LOUD if
/// `cam2-painter` is somehow still active -- a live unit would respawn the painter and re-take the
/// DRM master, making the upcoming mpv playback impossible. Mirrors the existing "survived TERM+KILL"
/// fail-loud in the same builder (refuse playback, never proceed under a live painter).
#[test]
fn stop_painter_cmds_fails_loud_if_the_cam2_painter_unit_is_still_active_1190() {
    let cmds = run_sourced("lipsync_stop_painter_cmds /run/rig-painter.pid");
    let is_active_at = cmds
        .find("systemctl is-active")
        .expect("1190: must verify cam2-painter is no longer active after the stop");
    assert!(
        cmds[is_active_at..].contains("cam2-painter"),
        "1190: the is-active guard must check the cam2-painter unit: {cmds}"
    );
    let after = &cmds[is_active_at..];
    assert!(
        after.contains("FAIL") && after.contains("exit 1"),
        "1190: a still-active unit after the stop must FAIL loud and refuse playback (exit 1): \
         {cmds}"
    );
}

/// The playback builder must add mpv's NATIVE `--log-file` -- mpv runs with `--no-terminal` (which
/// swallows its stderr), so without a log-file `/run/rig-lipsync-playback.log` is empty and a fatal
/// error (e.g. DRM master unavailable) is undiagnosable from the box. `--no-terminal` STAYS; the
/// log-file is the visibility fix, not dropping it.
#[test]
fn playback_cmds_writes_an_mpv_native_log_file_1190() {
    let cmds = run_sourced(
        "lipsync_playback_cmds /run/lipsync-test.mp4 '' hw:CARD=PCH,DEV=3 /run/rig-lipsync-playback.pid",
    );
    assert!(
        cmds.contains("--log-file=/run/rig-lipsync-playback.mpv.log"),
        "1190: mpv --no-terminal swallows its stderr, so an mpv-native --log-file is needed to make \
         a fatal error (e.g. DRM master unavailable) diagnosable from the box: {cmds}"
    );
    assert!(
        cmds.contains("--no-terminal"),
        "1190: --no-terminal must stay -- the --log-file is the visibility fix, not dropping \
         --no-terminal: {cmds}"
    );
}
