//! Issue 1367 — pixel proof for every classified missing/unreadable slot the merge could not
//! extract (`scripts/lib/missing-slot-pixels.sh`).
//!
//! Tier-0 (std-only, no rig): the lib's pure builders are called directly under the caller's real
//! `set -euo pipefail`; the load-bearing INDEXING contract is pinned with REAL ffmpeg against a VFR
//! fixture (a recording with a timestamp gap, where the verdict's CFR decode duplicates a frame);
//! and the whole runner is driven end-to-end against a fake `sshpass` that runs the "remote" side
//! locally. `scripts/recording-e2e.sh` is checked by substring anchors only.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/missing-slot-pixels.sh")
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the lib under `set -euo pipefail` and run `snippet` with `envs`. Returns
/// (exit_ok, stdout, stderr).
fn run(snippet: &str, envs: &[(&str, &str)]) -> (bool, String, String) {
    let script = format!(
        "set -euo pipefail\n. \"{}\"\n{}",
        lib_script().display(),
        snippet
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&script);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// A fresh, collision-free scratch dir (removed on drop).
fn tmpdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(args)
        .output()
        .expect("ffmpeg must be installed (the verdict decode needs it too)");
    assert!(
        out.status.success(),
        "ffmpeg {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A 64x48, 30 fps VFR recording where frame 10 is dropped WITH its timestamp: 29 packets that the
/// verdict's CFR decode reads back as 30 frames (a duplicate fills the gap).
fn vfr_fixture(dir: &Path) -> PathBuf {
    let p = dir.join("gap 1367.mkv");
    ffmpeg(&[
        "-v",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x48:rate=30:duration=1",
        "-vf",
        "select=not(eq(n\\,10))",
        "-fps_mode",
        "vfr",
        "-c:v",
        "ffv1",
        p.to_str().expect("utf8"),
    ]);
    p
}

/// The verdict's own decode (src/probe/recording.rs read_frames), frame `k` of it.
fn verdict_frame(rec: &Path, k: usize) -> Vec<u8> {
    let raw = ffmpeg(&[
        "-v",
        "error",
        "-nostdin",
        "-i",
        rec.to_str().expect("utf8"),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "gray",
        "pipe:1",
    ]);
    raw[k * 64 * 48..(k + 1) * 64 * 48].to_vec()
}

fn png_gray(png: &Path) -> Vec<u8> {
    ffmpeg(&[
        "-v",
        "error",
        "-i",
        png.to_str().expect("utf8"),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "gray",
        "pipe:1",
    ])
}

// ---- the pure builders --------------------------------------------------------------------------

const VERDICT: &str = r#"{"overall_pass": true, "full_chain": {"loss": {
  "cam3": {"classified": [
    {"id": 1, "kind": "burn_unreadable", "frame_index": 100, "png": null},
    {"id": 2, "kind": "burn_unreadable", "frame_index": 50, "png": "/x/frame-50.png"},
    {"id": 3, "kind": "real_drop", "frame_index": 7, "png": null},
    {"id": 4, "kind": "burn_unreadable", "frame_index": 100, "png": null}]},
  "strih": {"classified": [
    {"id": 5, "kind": "burn_unreadable", "frame_index": 20, "png": null},
    {"id": 6, "kind": "real_drop", "frame_index": null, "png": null}]},
  "imag": {"classified": [{"id": 7, "kind": "burn_unreadable", "frame_index": 5, "png": null}]},
  "cam2_cam1": {"classified": [{"id": 8, "kind": "burn_unreadable", "frame_index": 9, "png": null}]},
  "stream": {"classified": []}
}}}"#;

#[test]
fn frames_pick_only_unproven_slots_of_box_backed_nodes_sorted_deduped_and_capped() {
    let tmp = tmpdir();
    let d = tmp.path();
    let v = d.join("verdict.json");
    fs::write(&v, VERDICT).expect("write");
    let vs = v.display();
    let (ok, all, err) = run(&format!("missing_slot_pixels_frames '{vs}' 0"), &[]);
    assert!(ok, "{err}");
    assert_eq!(all, "cam3\t7\ncam3\t100\nstrih\t20");
    let (ok, capped, err) = run(&format!("missing_slot_pixels_frames '{vs}' 2"), &[]);
    assert!(ok, "{err}");
    assert_eq!(capped, "cam3\t7\ncam3\t100");
    let (ok, none, err) = run(
        "missing_slot_pixels_frames /nonexistent/verdict.json 0",
        &[],
    );
    assert!(ok, "an unreadable verdict is not an error: {err}");
    assert_eq!(none, "");
}

#[test]
fn cap_defaults_to_twelve_and_rejects_junk() {
    for (val, want) in [
        ("", "12"),
        ("0", "12"),
        ("x", "12"),
        ("-3", "12"),
        ("5", "5"),
    ] {
        let (ok, out, err) = run(
            "missing_slot_pixels_cap",
            &[("MISSING_SLOT_PIXELS_CAP", val)],
        );
        assert!(ok, "{err}");
        assert_eq!(out, want, "MISSING_SLOT_PIXELS_CAP={val:?}");
    }
}

#[test]
fn camera_slots_live_on_the_strih_recording_node_burns_on_the_stream_recording() {
    for (node, want) in [
        ("cam1", "strih"),
        ("cam7", "strih"),
        ("strih", "stream"),
        ("stream", "stream"),
        ("imag", ""),
        ("cam2_cam1", ""),
    ] {
        let (ok, out, err) = run(&format!("missing_slot_pixels_box_for_node {node}"), &[]);
        assert!(ok, "{err}");
        assert_eq!(out, want, "{node}");
    }
}

#[test]
fn every_slot_brings_its_two_neighbours_never_below_zero() {
    let (ok, out, err) = run("missing_slot_pixels_indices 12 0 13 | tr '\\n' ' '", &[]);
    assert!(ok, "{err}");
    assert_eq!(out, "0 1 11 12 13 14");
    let (ok, out, err) = run("missing_slot_pixels_select_expr 0 1 12", &[]);
    assert!(ok, "{err}");
    assert_eq!(out, "select=eq(n\\,0)+eq(n\\,1)+eq(n\\,12)");
}

#[test]
fn stage_one_is_the_verdict_decode_byte_for_byte() {
    // The verdict's frame_index is the ordinal on THIS pipe; drifting from it would silently export
    // the wrong frames.
    let rs = read("src/probe/recording.rs");
    assert!(rs.contains(".args([\"-v\", \"error\", \"-nostdin\", \"-i\"])"));
    assert!(rs.contains(".args([\"-f\", \"rawvideo\", \"-pix_fmt\", \"gray\", \"pipe:1\"])"));
    let (ok, head, err) = run("missing_slot_pixels_decode_head", &[]);
    assert!(ok, "{err}");
    assert_eq!(head, "-v error -nostdin -i");
    let (ok, tail, err) = run("missing_slot_pixels_decode_tail", &[]);
    assert!(ok, "{err}");
    assert_eq!(tail, "-f rawvideo -pix_fmt gray pipe:1");
}

#[test]
fn strih_lx_export_runs_at_the_same_idle_priority_as_the_on_box_decode() {
    let s = read("scripts/recording-verdict-on-strih-lx.sh");
    let (ok, snippet, err) = run("missing_slot_pixels_lowprio_snippet", &[]);
    assert!(ok, "{err}");
    assert!(
        s.contains(&format!("LOWPRIO_SNIPPET='{snippet}'")),
        "the lib's idle-priority prefix must be byte-identical to the strih-lx decode's: {snippet}"
    );
}

// ---- the indexing contract, with real ffmpeg --------------------------------------------------

#[test]
fn exported_pngs_are_the_verdict_frames_even_after_a_timestamp_gap() {
    let tmp = tmpdir();
    let d = tmp.path();
    let rec = vfr_fixture(d);
    // Prove the fixture exercises the contract: the CFR decode has MORE frames than the file has
    // packets, so a naive select on the source would be off by one after frame 10.
    let decoded = ffmpeg(&[
        "-v",
        "error",
        "-nostdin",
        "-i",
        rec.to_str().expect("utf8"),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "gray",
        "pipe:1",
    ])
    .len()
        / (64 * 48);
    assert_eq!(decoded, 30, "the verdict decode fills the gap");
    let out = d.join("out dir");
    let script = format!(
        "bash -c \"$(missing_slot_pixels_linux_script '{}' '{}' $(missing_slot_pixels_indices 12 20))\"",
        rec.display(),
        out.display()
    );
    let (ok, _, err) = run(&script, &[]);
    assert!(ok, "the export script must succeed locally: {err}");
    for k in [11usize, 12, 13, 19, 20, 21] {
        let png = out.join(format!("frame-{k}.png"));
        assert!(png.is_file(), "frame-{k}.png must be exported");
        assert_eq!(
            png_gray(&png),
            verdict_frame(&rec, k),
            "frame-{k}.png must be the verdict's frame {k}"
        );
    }
    assert!(
        !out.join("frame-15.png").exists(),
        "only the slots and their neighbours are exported"
    );
}

// ---- the Windows (stream box) program -----------------------------------------------------------

#[test]
fn windows_program_runs_the_pipeline_through_cmd_at_the_decode_priority() {
    let (ok, ps, err) = run(
        "missing_slot_pixels_windows_ps 'C:\\_REC\\Bob'\"'\"'s 12-00.mkv' 'C:\\cb\\out' 11 12 13",
        &[],
    );
    assert!(ok, "{err}");
    assert!(ps.contains("PriorityClass = 'BelowNormal'"), "{ps}");
    assert!(
        ps.contains("$rec = 'C:\\_REC\\Bob''s 12-00.mkv'"),
        "a quote is doubled: {ps}"
    );
    assert!(
        ps.contains("& cmd.exe /c $cmdFile"),
        "cmd's pipe is binary-safe: {ps}"
    );
    assert!(
        ps.contains("-f rawvideo -pix_fmt gray pipe:1 | ffmpeg "),
        "stage 1 is the verdict decode: {ps}"
    );
    assert!(ps.contains("-framerate 1 -i pipe:0"), "{ps}");
    assert!(ps.contains("-frame_pts 1 -frames:v 3"), "{ps}");
    assert!(
        ps.contains("\"select=eq(n\\,11)+eq(n\\,12)+eq(n\\,13)\""),
        "{ps}"
    );
    assert!(ps.contains("frame-%%d.png"), "% doubled in the .cmd: {ps}");
    let (ok, _, _) = run(
        "missing_slot_pixels_windows_ps 'C:\\_REC\\50%.mkv' 'C:\\cb' 1",
        &[],
    );
    assert!(!ok, "a path a .cmd line cannot carry is refused");
}

// ---- the runner, end-to-end against a fake sshpass --------------------------------------------

/// A fake `sshpass` first on PATH: `sshpass -p PW ssh OPTS… TARGET CMD` runs CMD locally with bash;
/// `sshpass -p PW scp -r OPTS… TARGET:SRC DEST` copies SRC locally. A Windows `powershell …` CMD
/// fails (no PowerShell here), which exercises the best-effort failure path.
fn fake_bin(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).expect("mkdir bin");
    let sshpass = bin.join("sshpass");
    fs::write(
        &sshpass,
        r#"#!/usr/bin/env bash
shift 2
tool="$1"; shift
last="${*: -1}"
case "$tool" in
  ssh) exec bash -c "$last" ;;
  scp) src="${*: -2:1}"; exec cp -r "${src#*:}" "$last" ;;
esac
exit 99
"#,
    )
    .expect("write fake");
    let mut perm = fs::metadata(&sshpass).expect("stat").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&sshpass, perm).expect("chmod");
    bin
}

#[test]
fn runner_exports_pulls_and_records_the_proofs_without_touching_the_verdict() {
    let tmp = tmpdir();
    let d = tmp.path();
    let rec = vfr_fixture(d);
    let outdir = d.join("outdir");
    fs::create_dir_all(&outdir).expect("mkdir");
    let report = outdir.join("verdict-77.json");
    fs::write(
        &report,
        r#"{"overall_pass": true, "full_chain": {"loss": {
  "cam3": {"classified": [{"id": 1, "kind": "burn_unreadable", "frame_index": 12, "png": null}]},
  "strih": {"classified": [{"id": 2, "kind": "burn_unreadable", "frame_index": 4, "png": null}]}
}}}"#,
    )
    .expect("write verdict");
    let bin = fake_bin(d);
    let remote = d.join("remote");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let snippet = format!(
        "missing_slot_pixels_run '{}' '{}' 77 strih-lx.test linux '{}' stream.test 'C:\\stream.mkv' 'C:\\cb\\out'",
        report.display(),
        outdir.display(),
        rec.display()
    );
    let (ok, stdout, err) = run(
        &snippet,
        &[
            ("PATH", path.as_str()),
            ("STRIH_LX_REMOTE_OUT_DIR", remote.to_str().expect("utf8")),
        ],
    );
    assert!(ok, "the runner never fails the caller: {err}");
    for k in [11usize, 12, 13] {
        let png = outdir.join("cam3-missing").join(format!("frame-{k}.png"));
        assert!(
            png.is_file(),
            "cam3 slot 12 -> frame-{k}.png pulled: {stdout}"
        );
        assert_eq!(png_gray(&png), verdict_frame(&rec, k), "frame {k}");
        assert!(
            stdout.contains(&png.display().to_string()),
            "every path is logged: {stdout}"
        );
    }
    assert!(
        !remote.join("missing-slot-pixels-strih-77").exists(),
        "the remote export dir is removed after the pull"
    );
    let v = read_json(&report);
    assert!(
        v.contains("\"overall_pass\": true"),
        "verdict untouched: {v}"
    );
    assert!(v.contains("\"missing_slot_pixels\""), "{v}");
    assert!(v.contains("\"strih\": \"ok\""), "{v}");
    assert!(
        v.contains("\"stream\": \"failed\""),
        "the stream box (no PowerShell here) fails best-effort: {v}"
    );
    assert!(v.contains("\"exported_slots\": 1"), "{v}");
    assert!(v.contains("\"total_slots\": 2"), "{v}");
}

fn read_json(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn runner_is_silent_when_every_slot_already_has_a_proof() {
    let tmp = tmpdir();
    let d = tmp.path();
    let report = d.join("verdict.json");
    let body = r#"{"overall_pass": true, "full_chain": {"loss": {"cam3": {"classified": []}}}}"#;
    fs::write(&report, body).expect("write");
    let (ok, stdout, err) = run(
        &format!(
            "missing_slot_pixels_run '{}' '{}' 1 h linux /r s /r 'C:\\o'",
            report.display(),
            d.display()
        ),
        &[],
    );
    assert!(ok, "{err}");
    assert!(stdout.contains("nothing to export"), "{stdout}");
    assert_eq!(
        read_json(&report),
        body,
        "a clean run leaves the verdict byte-identical"
    );
}

// ---- the recording-e2e.sh wiring ----------------------------------------------------------------

#[test]
fn recording_e2e_runs_the_step_after_the_merge_before_the_report_and_cleanup_plan() {
    let s = read("scripts/recording-e2e.sh");
    assert_eq!(
        s.matches(". \"$HERE/lib/missing-slot-pixels.sh\"").count(),
        1,
        "sourced once"
    );
    assert_eq!(
        s.matches("missing_slot_pixels_run ").count(),
        1,
        "called once"
    );
    let merge = s
        .find("\"$VERDICT_BIN\" \"${MERGE_ARGS[@]}\" || GATE=$?")
        .expect("the executed merge");
    let call = s.find("missing_slot_pixels_run ").expect("the call");
    let report = s
        .find("e2e_discord_report_send \"$REPORT_JSON\"")
        .expect("the Discord report");
    let cleanup = s
        .find("--- [8/8e] cleanup plan (JSON secured")
        .expect("the #652 cleanup plan");
    assert!(merge < call && call < report && report < cleanup, "order");
    let line = s[call..].lines().next().expect("line");
    assert!(
        line.trim_end().ends_with("|| true"),
        "never aborts the run: {line}"
    );
}
