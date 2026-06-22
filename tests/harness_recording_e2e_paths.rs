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

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
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
