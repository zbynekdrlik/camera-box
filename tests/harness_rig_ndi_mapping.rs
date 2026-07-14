//! #399/#312/#753 — the strih NDI-input→camera mapping must be ENFORCED (set + verified
//! 7-distinct) on every rig activation, not hand-set. Locks: (a) set-ndi-mapping.py exists with
//! the fixed 7-distinct pins, (b) rig-mode.sh calls the enforcer in BOTH test and event modes,
//! (c) the pure mapping logic (pins distinct, duplicate detection) is correct. The live OBS WS
//! set itself is exercised on the rig, not here (these are pure-file / pure-python locks — no
//! OBS, no ssh).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn read(p: &str) -> String {
    fs::read_to_string(manifest_dir().join(p)).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

#[test]
fn set_ndi_mapping_py_exists_with_the_seven_distinct_pins() {
    let s = read("scripts/set-ndi-mapping.py");
    // #753 PIVOT (2026-07-14, binding user directive): the fixed Claude-owned mapping is now 1:1
    // (the pre-pivot offset table is HISTORY, see set-ndi-mapping.py's own module docstring).
    for (inp, snd) in [
        ("NDI cam1", "CAM1 (usb)"),
        ("NDI cam2", "CAM2 (usb)"),
        ("NDI cam3", "CAM3 (usb)"),
        ("NDI cam4", "CAM4 (usb)"),
        ("NDI cam5", "CAM5 (usb)"),
        ("NDI cam6", "CAM6 (usb)"),
        ("NDI cam7", "CAM7 (usb)"),
    ] {
        assert!(
            s.contains(inp) && s.contains(snd),
            "#399/#312/#753: set-ndi-mapping.py DEFAULT_MAP must pin {inp:?} -> {snd:?}"
        );
    }
}

#[test]
fn rig_mode_enforces_the_mapping_in_both_test_and_event() {
    let s = read("scripts/rig-mode.sh");
    assert!(
        s.contains("set-ndi-mapping.py"),
        "#399: rig-mode.sh must invoke set-ndi-mapping.py"
    );
    assert!(
        s.contains("enforce_strih_ndi_mapping"),
        "#399: rig-mode.sh must define + call the enforce_strih_ndi_mapping helper"
    );
    // It must be CALLED from both do_test and do_event (the mapping is invariant across modes).
    let do_test = s
        .split("do_test()")
        .nth(1)
        .unwrap_or("")
        .split("do_event()")
        .next()
        .unwrap_or("");
    let do_event = s
        .split("do_event()")
        .nth(1)
        .unwrap_or("")
        .split("\nmain()")
        .next()
        .unwrap_or("");
    assert!(
        do_test.contains("enforce_strih_ndi_mapping"),
        "#399: do_test must enforce the NDI mapping"
    );
    assert!(
        do_event.contains("enforce_strih_ndi_mapping"),
        "#399: do_event must enforce the NDI mapping"
    );
}

#[test]
fn pins_are_seven_distinct_cameras_and_duplicates_are_detected() {
    // Drive the script's pure helpers directly: DEFAULT_MAP must be 7 distinct senders (#753:
    // widened 6->7 with cam7), and duplicates() must flag a repeated sender (the recurring "two
    // inputs both on CAM4" bug — #312's own repointing of "NDI cam4" away from CAM4 (usb) fixed
    // exactly this case live).
    let script = format!(
        r#"import importlib.util as u, sys
spec = u.spec_from_file_location("m", "{p}/scripts/set-ndi-mapping.py")
m = u.module_from_spec(spec); spec.loader.exec_module(m)
senders = [s for _, s in m.DEFAULT_MAP]
assert len(m.DEFAULT_MAP) == 7, "must be 7 inputs"
assert len(set(senders)) == 7, f"pins must be 7 DISTINCT cameras, got {{senders}}"
assert not m.duplicates(dict(m.DEFAULT_MAP)), "the pins must have no duplicate"
assert m.duplicates({{"NDI cam1": "CAM4 (usb)", "NDI cam2": "CAM4 (usb)"}}), "must flag a dup"
assert m.parse_map_args(None) == list(m.DEFAULT_MAP), "no --map -> the pins"
print("OK")
"#,
        p = manifest_dir().display()
    );
    let out = Command::new("python3")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run python");
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("OK"),
        "#399 pure-mapping checks failed:\nstdout:{}\nstderr:{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
