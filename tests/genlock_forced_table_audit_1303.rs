//! #1303 part 4 — the report-only per-box-class certified-table AUDIO-parity audit.
//!
//! Two halves, both Tier-0 (no rig, no compiled binary at runtime beyond the crate itself). The
//! Rust API classification tests over `camera_box::genlock_forced_table_audit` assert concrete
//! verdicts for each expectation class and both mismatch directions, so each can FAIL. The
//! sourced-bash replica `scripts/lib/genlock-forced-table-audit.sh` gets print-shape tests
//! (run_sourced style, like tests/harness_cg_chain_verify_1300.rs), plus a PARITY gate pinning the
//! bash verdict to the Rust `audio_verdict` over a fixed vector set so the two can never drift.

use camera_box::genlock_forced_table_audit::{
    any_mismatch, audio_verdict, audit_box, classify, expected_audio, AudioExpectation,
    AudioVerdict, BoxClass, NdiInput,
};
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/genlock-forced-table-audit.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

/// Source the REAL lib and run `body`. Returns (exit, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib_path())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn input(name: &str, ndi_audio: bool, yuv_range: &str) -> NdiInput {
    NdiInput {
        name: name.to_string(),
        ndi_audio,
        yuv_range: yuv_range.to_string(),
        yuv_colorspace: String::new(),
    }
}

// ---- Rust API classification (each can FAIL) --------------------------------------------------

#[test]
fn rust_program_source_audio_off_is_the_live_defect() {
    let a = classify(
        BoxClass::Resolume,
        &input("sp-slow_video", false, "partial"),
    );
    assert_eq!(a.expected, AudioExpectation::ExpectedAudio);
    assert_eq!(a.verdict, AudioVerdict::MismatchProgramSilent);
    assert!(
        a.yuv_partial_on_program,
        "forced-partial program source -> yuv advisory"
    );
}

#[test]
fn rust_camera_audio_on_is_a_mismatch_but_silent_is_ok() {
    assert_eq!(
        classify(BoxClass::Strih, &input("CAM2 (usb)", true, "")).verdict,
        AudioVerdict::MismatchCameraAudible
    );
    assert_eq!(
        classify(BoxClass::Strih, &input("CAM2 (usb)", false, "")).verdict,
        AudioVerdict::Ok
    );
}

#[test]
fn rust_box_defaults_split_camera_chain_vs_cg() {
    assert_eq!(
        expected_audio(BoxClass::Imag, "odd"),
        AudioExpectation::ExpectedSilent
    );
    assert_eq!(
        expected_audio(BoxClass::Resolume, "odd"),
        AudioExpectation::ExpectedAudio
    );
}

#[test]
fn rust_any_mismatch_summary() {
    let clean = audit_box(BoxClass::Resolume, &[input("sp-fast_video", true, "full")]);
    assert!(!any_mismatch(&clean));
    let dirty = audit_box(BoxClass::Resolume, &[input("sp-fast_video", false, "full")]);
    assert!(any_mismatch(&dirty));
}

// ---- Certified per-box table (#1303 owner ruling 2026-09-15) -----------------------------------
//
// „žiadny — zvuk na strih/stream ide cez Dante, NDI audio ostáva vypnuté" — program audio over NDI
// exists on the cg OBS (resolume) ONLY; strih/stream/imag carry the mastered mix over Dante/ASIO, so
// EVERY NDI input there is silent and NDI audio ENABLED on any of them is the double-audio defect.
// These vectors are today's five live deploy-preflight rows (must grade OK), the inverse (audible on
// a silent box = a mismatch), and the resolume program/camera pins.

#[test]
fn certified_five_strih_stream_program_rows_are_ok_1303() {
    // The five FALSE MISMATCH-PROGRAM-SILENT rows from the 15.9 12:08 preflight: with NDI audio OFF
    // on a strih/stream program input, the certified table grades OK (Dante carries the audio).
    assert_eq!(
        classify(BoxClass::Strih, &input("cg", false, "")).verdict,
        AudioVerdict::Ok
    );
    assert_eq!(
        classify(BoxClass::Strih, &input("NDI 2ME PGM (mv)", false, "")).verdict,
        AudioVerdict::Ok
    );
    assert_eq!(
        classify(BoxClass::Stream, &input("NDI 2ME PGM", false, "")).verdict,
        AudioVerdict::Ok
    );
    assert_eq!(
        classify(BoxClass::Stream, &input("NDI obs hudba", false, "")).verdict,
        AudioVerdict::Ok
    );
    assert_eq!(
        classify(BoxClass::Stream, &input("NDIA cg stream", false, "")).verdict,
        AudioVerdict::Ok
    );
}

#[test]
fn certified_audible_on_a_silent_box_is_a_mismatch_1303() {
    // The inverse: NDI audio ENABLED on a strih/stream input is the double-audio hazard the owner
    // named -> a mismatch (not OK). (The specific MISMATCH-AUDIBLE token is pinned in the GREEN
    // variant test + the parity gate.)
    assert!(
        classify(BoxClass::Stream, &input("NDI obs hudba", true, ""))
            .verdict
            .is_mismatch()
    );
    assert!(classify(BoxClass::Strih, &input("cg", true, ""))
        .verdict
        .is_mismatch());
}

#[test]
fn certified_resolume_is_the_only_program_audio_box_1303() {
    // resolume keeps the program->audio / camera->silent table.
    assert_eq!(
        classify(BoxClass::Resolume, &input("sp-fast_video", true, "")).verdict,
        AudioVerdict::Ok
    );
    assert_eq!(
        classify(BoxClass::Resolume, &input("sp-fast_video", false, "")).verdict,
        AudioVerdict::MismatchProgramSilent
    );
    assert_eq!(
        classify(BoxClass::Resolume, &input("NDI cam1", true, "")).verdict,
        AudioVerdict::MismatchCameraAudible
    );
}

// ---- Bash replica: print shape ----------------------------------------------------------------

#[test]
fn bash_audit_prints_verdict_lines_and_summary() {
    let body = "printf 'sp-fast_video\\ttrue\\tfull\\tBT.709\\n\
sp-slow_video\\tfalse\\tpartial\\tBT.709\\n\
cg\\ttrue\\t\\t\\n\
CAM3 (usb)\\ttrue\\t\\t\\n' | genlock_forced_table_audit resolume";
    let (rc, out, err) = run_sourced(body);
    assert_eq!(
        rc, 0,
        "report-only: never non-zero.\nstdout={out}\nstderr={err}"
    );
    assert!(out.contains("box=resolume"), "header names the box:\n{out}");
    assert!(
        out.contains("sp-fast_video: expected=audio ndi_audio=true -> OK"),
        "clean program row:\n{out}"
    );
    assert!(
        out.contains("sp-slow_video: expected=audio ndi_audio=false -> MISMATCH-PROGRAM-SILENT"),
        "the live-defect row:\n{out}"
    );
    assert!(
        out.contains("NOTE yuv_range=partial on a program source"),
        "yuv advisory on the forced-partial program row:\n{out}"
    );
    assert!(
        out.contains("CAM3 (usb): expected=silent ndi_audio=true -> MISMATCH-CAMERA-AUDIBLE"),
        "camera-audible row:\n{out}"
    );
    assert!(
        out.contains("# summary: 4 input(s), 2 MISMATCH"),
        "summary count:\n{out}"
    );
}

#[test]
fn bash_audit_is_report_only_even_when_all_clean() {
    let body = "printf 'CAM1 (usb)\\tfalse\\t\\t\\n' | genlock_forced_table_audit imag";
    let (rc, out, _e) = run_sourced(body);
    assert_eq!(rc, 0);
    assert!(out.contains("# summary: 1 input(s), 0 MISMATCH"), "{out}");
}

// ---- Parity gate: bash verdict == Rust verdict over a fixed vector set -------------------------

/// The Rust verdict rendered as the bash token, so the two are directly comparable.
fn rust_token(bc: BoxClass, name: &str, ndi_audio: bool) -> &'static str {
    match audio_verdict(expected_audio(bc, name), ndi_audio) {
        AudioVerdict::Ok => "OK",
        AudioVerdict::MismatchProgramSilent => "MISMATCH-PROGRAM-SILENT",
        AudioVerdict::MismatchCameraAudible => "MISMATCH-CAMERA-AUDIBLE",
    }
}

#[test]
fn bash_replica_matches_rust_over_a_fixed_vector_set() {
    let boxes = [
        (BoxClass::Strih, "strih"),
        (BoxClass::Stream, "stream"),
        (BoxClass::Imag, "imag"),
        (BoxClass::Resolume, "resolume"),
    ];
    let names = [
        "sp-fast_video",
        "cg",
        "NDI 2ME PGM",
        "mbc",
        "NDI obs hudba",
        "NDIAr ppt",
        "VBAN cg-resolume",
        "CAM1 (usb)",
        "CAM3",
        "camera2",
        "some_odd_input",
    ];
    for (bc, bname) in boxes {
        for name in names {
            for ndi in [true, false] {
                let want = rust_token(bc, name, ndi);
                let body = format!(
                    "genlock_forced_table_verdict {bname} '{name}' {ndi}",
                    ndi = if ndi { "true" } else { "false" }
                );
                let (rc, out, err) = run_sourced(&body);
                assert_eq!(rc, 0, "verdict rc for {bname}/{name}/{ndi}: {err}");
                assert_eq!(
                    out.trim(),
                    want,
                    "PARITY drift: box={bname} name={name} ndi_audio={ndi} \
                     -> bash={:?} rust={want}",
                    out.trim()
                );
            }
        }
    }
}
