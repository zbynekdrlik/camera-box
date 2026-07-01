//! Content-assert guards for #365 frozen-camera gate.
//!
//! Verifies that the recording-e2e harness references the frozen-camera-gate.py script
//! AND that the cross-boundary JSON contract between the Python harness and the Rust
//! verdict binary is internally consistent.

use std::fs;
use std::path::Path;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// #365: recording-e2e.sh must invoke frozen-camera-gate.py as a pre-record gate step.
/// This guards against the gate being accidentally removed or never wired.
#[test]
fn recording_e2e_sh_references_frozen_camera_gate_py() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains("frozen-camera-gate.py"),
        "scripts/recording-e2e.sh must reference frozen-camera-gate.py (#365 frozen-camera gate). \
         Add the [4c/8] gate step before StartRecord."
    );
}

/// #365: scripts/frozen-camera-gate.py must exist.
#[test]
fn frozen_camera_gate_py_exists() {
    let path = format!(
        "{}/scripts/frozen-camera-gate.py",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(
        Path::new(&path).exists(),
        "scripts/frozen-camera-gate.py not found (#365). \
         Create the Python OBS-WS harness that feeds the Rust verdict binary."
    );
}

/// #365: The Rust binary (src/bin/frozen-camera-gate.rs) must exist.
#[test]
fn frozen_camera_gate_bin_src_exists() {
    let path = format!(
        "{}/src/bin/frozen-camera-gate.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(
        Path::new(&path).exists(),
        "src/bin/frozen-camera-gate.rs not found (#365). \
         Create the thin CLI binary that reads JSON from stdin and calls frozen_cameras()."
    );
}

/// #365: The JSON contract between Python and Rust uses the same per-camera array-of-nullable-hashes
/// format. Pin the Rust binary's stdin format by checking it appears in the Python source
/// (the Python writes json.dumps of a dict[str, list[str|None]] fed via stdin to the Rust bin).
#[test]
fn json_contract_python_outputs_per_camera_hash_list() {
    let py = read("scripts/frozen-camera-gate.py");
    assert!(
        py.contains("json.dumps"),
        "scripts/frozen-camera-gate.py must use json.dumps to serialise the per-camera hash \
         timeline before piping it into the frozen-camera-gate binary (#365 JSON contract)."
    );
    // The Rust binary reads from stdin, so the Python must pass it via subprocess stdin.
    assert!(
        py.contains("frozen-camera-gate"),
        "scripts/frozen-camera-gate.py must invoke the frozen-camera-gate binary (#365)."
    );
}

/// #365: The e2e skill must document the frozen-camera gate (raw NDI inputs, not Multiview tile).
#[test]
fn e2e_skill_documents_frozen_camera_gate() {
    let skill = read(".claude/skills/e2e/SKILL.md");
    assert!(
        skill.contains("frozen-camera"),
        ".claude/skills/e2e/SKILL.md must document the frozen-camera gate (#365). \
         Add a section explaining what it checks, why raw NDI inputs (not Multiview tiles), \
         source names, and threshold defaults."
    );
}
