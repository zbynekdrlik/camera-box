//! Regression guards for scripts/recording-e2e.sh path/host correctness (#7/#105/#156).
//!
//! Root cause found on the first real validation run: the cam1 `--record-grab-ts` sidecar
//! path was `$OUTDIR/...`, but `$OUTDIR` exists on dev1, NOT on cam1 — so camera-box on cam1
//! failed at startup with `create grab-ts sidecar ...: No such file or directory`, exited,
//! and the whole cam1 grab node was silently empty (the ffv1 mkv had only its header). The
//! fix: write the sidecar to a cam1-LOCAL path (`CAM1_GRAB_TS_REMOTE=/tmp/...`) and scp it
//! back to the dev1 `$OUTDIR` path the verdict reads. These tests read the script as text
//! and pin that the cam1-side arg is the local path and the scp source matches it.

use std::fs;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Source recording-fetch-windows.sh and call `urlencode_name <arg>`, returning its
/// stdout. The script's `main` is guarded by a sourced-vs-executed check, so sourcing it
/// runs only the function definitions.
fn urlencode_name(arg: &str) -> String {
    let script = format!(
        "{}/scripts/recording-fetch-windows.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; urlencode_name \"$2\"")
        .arg("bash") // $0
        .arg(&script) // $1 — the script to source
        .arg(arg) // $2 — the name to encode
        .output()
        .expect("run urlencode_name");
    assert!(
        out.status.success(),
        "urlencode_name failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// The `--record-grab-ts` value handed to camera-box ON cam1 must be the cam1-LOCAL
/// remote path, NEVER the dev1 `$OUTDIR` path (which does not exist on cam1 and ENOENTs
/// the grab). We assert the cam1 launch uses `CAM1_GRAB_TS_REMOTE`, and that that var is
/// rooted at /tmp (a path that always exists on the camera), not under `$OUTDIR`.
#[test]
fn cam1_record_grab_ts_uses_a_cam1_local_path_not_dev1_outdir() {
    let s = read("scripts/recording-e2e.sh");

    // CAM1_GRAB_TS_REMOTE is defined and is a /tmp path (exists on cam1), not $OUTDIR.
    let remote_def = s
        .lines()
        .find(|l| l.trim_start().starts_with("CAM1_GRAB_TS_REMOTE="))
        .expect("CAM1_GRAB_TS_REMOTE must be defined (the cam1-local sidecar path)");
    assert!(
        remote_def.contains("/tmp/"),
        "CAM1_GRAB_TS_REMOTE must be a /tmp (cam1-local) path: {remote_def:?}"
    );
    assert!(
        !remote_def.contains("$OUTDIR") && !remote_def.contains("${OUTDIR}"),
        "CAM1_GRAB_TS_REMOTE must NOT live under the dev1 $OUTDIR (cam1 has no such dir): {remote_def:?}"
    );

    // The cam1 camera-box launch passes the REMOTE (local) var to --record-grab-ts.
    assert!(
        s.contains("--record-grab-ts ${CAM1_GRAB_TS_REMOTE}")
            || s.contains("--record-grab-ts $CAM1_GRAB_TS_REMOTE"),
        "the cam1 --record-grab-ts arg must be CAM1_GRAB_TS_REMOTE (the cam1-local path)"
    );
    // It must NOT pass the dev1 $OUTDIR-rooted CAM1_GRAB_TS as the cam1-side write path.
    assert!(
        !s.contains("--record-grab-ts ${CAM1_GRAB_TS}")
            && !s.contains("--record-grab-ts $CAM1_GRAB_TS "),
        "the cam1 write path must not be the dev1 $OUTDIR CAM1_GRAB_TS (would ENOENT on cam1)"
    );
}

/// The scp that pulls the sidecar back to dev1 must read it from the cam1-LOCAL remote
/// path and write it to the dev1 `$OUTDIR` path the verdict consumes.
#[test]
fn grab_ts_sidecar_is_pulled_from_the_cam1_local_path_to_the_dev1_outdir_path() {
    let s = read("scripts/recording-e2e.sh");
    // The scp source on cam1 is the REMOTE local path; the destination is the dev1 path.
    assert!(
        s.contains("root@\"$CAM1_IP\":\"$CAM1_GRAB_TS_REMOTE\" \"$CAM1_GRAB_TS\""),
        "scp must copy cam1:CAM1_GRAB_TS_REMOTE -> dev1:CAM1_GRAB_TS"
    );
    // The dev1-side CAM1_GRAB_TS (what the verdict reads) stays under $OUTDIR.
    let dev1_def = s
        .lines()
        .find(|l| l.trim_start().starts_with("CAM1_GRAB_TS="))
        .expect("CAM1_GRAB_TS (dev1 path) must be defined");
    assert!(
        dev1_def.contains("$OUTDIR") || dev1_def.contains("${OUTDIR}"),
        "CAM1_GRAB_TS (dev1 verdict input) must live under $OUTDIR: {dev1_def:?}"
    );
}

/// The gate must ALWAYS receive a --win-status for strih AND stream (NOT conditional on the
/// status file existing). If the fetch failed and the file is absent, the gate must mark that
/// node UNKNOWN and FAIL — never silently drop it and certify only cam1+cam2 (the review bug).
#[test]
fn gate_always_passes_win_status_for_both_windows_nodes() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("--win-status \"strih=$DANTE_STRIH_STATUS\""),
        "the gate must ALWAYS be given strih=--win-status (so a missing file -> UNKNOWN -> fail)"
    );
    assert!(
        s.contains("--win-status \"stream=$DANTE_STREAM_STATUS\""),
        "the gate must ALWAYS be given stream=--win-status (so a missing file -> UNKNOWN -> fail)"
    );
    // The previous bug gated the --win-status args on `[ -s "$DANTE_..._STATUS" ]` (file exists),
    // which dropped a node whose fetch failed. That conditional must be gone.
    assert!(
        !s.contains("[ -s \"$DANTE_STRIH_STATUS\" ]  && GATE_WIN_ARGS")
            && !s.contains("GATE_WIN_ARGS+=(--win-status \"strih="),
        "the --win-status args must NOT be conditional on the status file existing"
    );
}

/// The DanteSync NTP+PTP gate must be the FIRST hard step (#7): it must appear in the
/// script BEFORE the cam1/cam2 launch and the OBS recording start, so a not-locked cluster
/// fails fast before any measurement.
#[test]
fn dantesync_gate_runs_before_any_recording() {
    let s = read("scripts/recording-e2e.sh");
    let gate = s
        .find("dantesync-gate.sh")
        .expect("recording-e2e.sh must invoke dantesync-gate.sh");
    let start_record = s
        .find("StartRecord on strih")
        .expect("recording-e2e.sh must start OBS recording");
    assert!(
        gate < start_record,
        "the DanteSync NTP+PTP gate must run BEFORE StartRecord (fail fast on an unlocked cluster)"
    );
}

// ---------------------------------------------------------------------------
// #163 — record the PRODUCTION scene program, never a colliding probe input.
//
// Root cause (live-confirmed strih 2026-06-22): the harness routed program to the
// PHASE2-PROBE scene whose `phase2-probe-src` ndi_source was pointed at "CAM1 (usb)" —
// the SAME source-name the ALWAYS-ON prod input `NDI cam5` already holds. DistroAV
// allows ONE receiver per (source-name) on a host, so the probe input received NO NDI
// and the probe scene recorded pure BLACK (luma min=max=0; every frame undecodable),
// blocking the strict cam1→strih + strih→stream measurement.
//
// Fix: record the EXISTING certified prod scene program directly — strih's prod scene
// already shows cam1 via `NDI cam5` (genlock-certified), and stream's full-screen scene
// already shows strih via `NDI 2ME PGM`. No second receiver, no source-name collision.
// recording-e2e.sh must therefore route program via the `prod-scene` action (not the
// colliding probe `setup`).
// ---------------------------------------------------------------------------

/// recording-e2e.sh must route the strih + stream OBS PROGRAM via `obs_phase2.py
/// prod-scene` (record the certified production scene program), NOT via the probe
/// `setup` action that points the colliding `phase2-probe-src` ndi_source at a
/// source-name the always-on prod input already holds (→ black recording, #163).
#[test]
fn recording_e2e_records_prod_scene_not_colliding_probe() {
    let s = read("scripts/recording-e2e.sh");
    // The new prod-scene program routing must be used for BOTH boxes.
    assert!(
        s.contains("obs_phase2.py\" prod-scene") || s.contains("obs_phase2.py prod-scene"),
        "#163: recording-e2e.sh must route the OBS program via the `prod-scene` action \
         (record the certified prod scene program), not the colliding probe setup."
    );
    // The colliding probe `setup` action must NOT be used by the recording harness any
    // more — that is what created the second receiver on the prod source-name → black.
    assert!(
        !s.contains("obs_phase2.py\" setup") && !s.contains("obs_phase2.py setup"),
        "#163: recording-e2e.sh must NOT use `obs_phase2.py setup` (the probe-input path \
         that collides with the always-on prod NDI input and records black). Record the \
         prod scene program via `prod-scene` instead."
    );
}

/// The harness must name the certified PROD scenes it records: strih's prod scene that
/// already shows cam1 via the certified genlock input (`Cam 5`), and stream's
/// full-screen scene that shows strih's feed (`NDI 2ME PGM`). These are the scenes
/// proven (live + the prior 3-node run) to record NON-black; the probe scene records
/// black.
#[test]
fn recording_e2e_names_the_certified_prod_scenes() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("STRIH_PROG_SCENE") && s.contains("Cam 5"),
        "#163: the strih program scene must be the certified prod scene 'Cam 5' (already \
         shows cam1 via the genlock-certified `NDI cam5` input — no probe receiver)."
    );
    assert!(
        s.contains("STREAM_PROG_SCENE"),
        "#163: the stream program scene must be a named full-screen scene showing strih's \
         feed (`NDI 2ME PGM`), recorded directly — not the colliding probe scene."
    );
}

/// Teardown must restore the prior PROGRAM scenes on BOTH boxes (it routed them to the
/// prod recording scene for the run). The prod-scene action records prev program/preview
/// to state; teardown restores it — never strands the recording scene as live program.
#[test]
fn recording_e2e_teardown_restores_program_scenes() {
    let s = read("scripts/recording-e2e.sh");
    // teardown still runs for both boxes in cleanup (restoring prev program scene).
    let cleanup = s
        .find("cleanup()")
        .expect("recording-e2e.sh must define cleanup()");
    let body = &s[cleanup..];
    assert!(
        body.contains("teardown --host \"$STREAM\"") && body.contains("teardown --host \"$STRIH\""),
        "#163: cleanup() must teardown (restore prior program scene) on BOTH strih and \
         stream after recording the prod scene program."
    );
}

// ---------------------------------------------------------------------------
// #163: recording-fetch-windows.sh must URL-ENCODE the OBS recording filename.
//
// OBS's default recording name is `YYYY-MM-DD HH-MM-SS.ext` — it contains a SPACE.
// python http.server serves it only at the percent-encoded path (`.../...%20...`). The
// fetch built the URL from the RAW name, so curl sent a malformed request and the fetch
// FAILED — the strih/stream recordings (recorded fine, prod-scene program, NON-black)
// never reached the verdict, so the strict cam1→strih + strih→stream hops were
// unmeasured. RED on the raw-name URL; GREEN once the name is URL-encoded.
// ---------------------------------------------------------------------------

/// The fetch must URL-encode the recording filename's spaces (a raw space breaks curl).
#[test]
fn fetch_windows_url_encodes_spaces_in_the_recording_name() {
    // The exact OBS default name shape that broke the run.
    assert_eq!(
        urlencode_name("2026-06-22 20-58-26.mkv"),
        "2026-06-22%2020-58-26.mkv",
        "#163: recording-fetch-windows.sh must percent-encode spaces in the OBS recording \
         filename (python http.server serves it only at the %20-encoded path); a raw space \
         makes curl send a malformed request and the fetch fails."
    );
    // A name with no spaces is unchanged (no double-encoding, no corruption).
    assert_eq!(urlencode_name("clip.mp4"), "clip.mp4");
}

/// The URL must be built from the ENCODED name, never the raw `${name}` (the bug). Static
/// guard so a regression that drops the encode step fails even where bash isn't run.
#[test]
fn fetch_windows_builds_url_from_encoded_name_not_raw() {
    let s = read("scripts/recording-fetch-windows.sh");
    assert!(
        s.contains("urlencode_name"),
        "#163: recording-fetch-windows.sh must URL-encode the filename via urlencode_name."
    );
    // The url must use the encoded var, not the raw ${name}.
    assert!(
        s.contains("${WIN_HTTP_PORT}/${enc}"),
        "#163: the fetch URL must use the ENCODED name (${{enc}}), not the raw ${{name}} \
         (a raw space in the path makes curl fail)."
    );
    assert!(
        !s.contains("${WIN_HTTP_PORT}/${name}"),
        "#163 regression: the fetch URL must NOT be built from the raw ${{name}} (unencoded \
         spaces break the request)."
    );
}
