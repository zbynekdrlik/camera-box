//! #399/#312/#753/#827 — the strih NDI-input→camera mapping must be ENFORCED (set + verified
//! distinct) on every rig activation, not hand-set. Locks: (a) set-ndi-mapping.py's FULL_MAP
//! keeps every camera's pin as a FACT (cam1-7, #827: retiring a camera from the ACTIVE fleet
//! never deletes its pin), (b) active_map()/parse_map_args() filter that down to the currently
//! ACTIVE cameras (default cam1-4), (c) rig-mode.sh calls the enforcer (with --active) in BOTH
//! test and event modes, (d) the pure mapping logic (pins distinct, duplicate detection,
//! active-set reactivation) is correct. The live OBS WS set itself is exercised on the rig, not
//! here (these are pure-file / pure-python locks — no OBS, no ssh).

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
fn set_ndi_mapping_py_full_map_keeps_all_seven_pins_as_facts() {
    let s = read("scripts/set-ndi-mapping.py");
    // #753 PIVOT (2026-07-14, binding user directive): the fixed Claude-owned mapping is now 1:1
    // (the pre-pivot offset table is HISTORY, see set-ndi-mapping.py's own module docstring).
    // #827 (2026-07-27, binding owner directive): retiring cam5/cam6/cam7 from the ACTIVE fleet
    // must NEVER delete their pins -- FULL_MAP keeps all seven as facts; active_map() filters.
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
            "#399/#312/#753/#827: set-ndi-mapping.py FULL_MAP must pin {inp:?} -> {snd:?} \
             (a fact, regardless of CAMERA_ACTIVE_SET)"
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
    assert!(
        s.contains("--active \"$CAMERA_ACTIVE_SET\""),
        "#827: rig-mode.sh must pass camera-set.sh's CAMERA_ACTIVE_SET straight through to \
         set-ndi-mapping.py's own --active flag -- never a second hardcoded active-camera list"
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

/// Run a python script against set-ndi-mapping.py, asserting it exits 0 and prints "OK".
fn run_py_check(body: &str) -> (String, String) {
    let script = format!(
        r#"import importlib.util as u, sys
spec = u.spec_from_file_location("m", "{p}/scripts/set-ndi-mapping.py")
m = u.module_from_spec(spec); spec.loader.exec_module(m)
{body}
print("OK")
"#,
        p = manifest_dir().display()
    );
    let out = Command::new("python3")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run python");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn full_map_is_seven_distinct_cameras_and_duplicates_are_detected() {
    // Drive the script's pure helpers directly: FULL_MAP must be 7 distinct senders (every
    // camera the fleet has ever wired, #827: facts never deleted), and duplicates() must flag a
    // repeated sender (the recurring "two inputs both on CAM4" bug — #312's own repointing of
    // "NDI cam4" away from CAM4 (usb) fixed exactly this case live).
    let (stdout, stderr) = run_py_check(
        r#"senders = [s for _, s in m.FULL_MAP]
assert len(m.FULL_MAP) == 7, "must be 7 inputs"
assert len(set(senders)) == 7, f"pins must be 7 DISTINCT cameras, got {senders}"
assert not m.duplicates(dict(m.FULL_MAP)), "the pins must have no duplicate"
assert m.duplicates({"NDI cam1": "CAM4 (usb)", "NDI cam2": "CAM4 (usb)"}), "must flag a dup"
"#,
    );
    assert!(
        stdout.contains("OK"),
        "#399 pure-mapping checks failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
}

#[test]
fn active_map_defaults_to_exactly_the_active_camera_1198() {
    // #827/issue 947/issue 939/#1110/issue 1170/issue 1198/issue 1216: with no override,
    // active_map() (and therefore parse_map_args(None), the no-`--map` path main() takes) must
    // resolve to exactly cam1/cam2/cam3/cam5/cam6/cam7 -- the DEFAULT_ACTIVE_SET fallback (cam4
    // retired 2026-08-02, issue 947, the ONE camera still out; cam1 + cam2 RESTORED 2026-08-27,
    // issue 1198; cam5/cam6/cam7 RESTORED 2026-08-28, issue 1216, bigger splitter fitted).
    let (stdout, stderr) = run_py_check(
        r#"import os
os.environ.pop("CAMERA_ACTIVE_SET", None)
want = m.active_map()
senders = sorted(s.split(" ", 1)[0] for _, s in want)
assert senders == ["CAM1", "CAM2", "CAM3", "CAM5", "CAM6", "CAM7"], senders
assert m.parse_map_args(None) == want, "no --map -> active_map()"
"#,
    );
    assert!(
        stdout.contains("OK"),
        "issue 1198 active_map default checks failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
}

#[test]
fn an_explicit_map_override_always_wins_regardless_of_active_set() {
    // An operator's explicit --map is never filtered by activity -- even naming a "retired"
    // camera must pass through untouched (an intentional one-off override).
    let (stdout, stderr) = run_py_check(
        r#"override = ["NDI cam5=CAM5 (usb)"]
assert m.parse_map_args(override, "cam1 cam2") == [("NDI cam5", "CAM5 (usb)")], "explicit --map must win outright"
"#,
    );
    assert!(
        stdout.contains("OK"),
        "#827 explicit --map override checks failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
}

/// #827 REVERSIBILITY PROOF: widening the active set to include a retired camera (cam5) must
/// make active_map() cover it -- with ZERO code changes beyond the active-set argument. This is
/// the actual proof the reversal works, not a comment claiming it does.
#[test]
fn active_map_reactivates_a_retired_camera_when_the_active_set_widens() {
    // cam4 (the one camera issue 1216 leaves out, #947) is the proof camera here now that
    // cam5/cam6/cam7 are default-active again.
    let (stdout, stderr) = run_py_check(
        r#"want = m.active_map("cam1 cam2 cam3 cam4 cam5 cam6 cam7")
assert ("NDI cam4", "CAM4 (usb)") in want, want
senders = sorted(s.split(" ", 1)[0] for _, s in want)
assert senders == ["CAM1", "CAM2", "CAM3", "CAM4", "CAM5", "CAM6", "CAM7"], senders
# And shrinking it back out un-reactivates it, just as easily.
want_shrunk = m.active_map("cam1 cam2 cam3 cam5 cam6 cam7")
assert ("NDI cam4", "CAM4 (usb)") not in want_shrunk, want_shrunk
"#,
    );
    assert!(
        stdout.contains("OK"),
        "#827/#1216 reactivation-proof checks failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
}

#[test]
fn default_active_set_env_var_matches_camera_set_sh_exactly() {
    // #827: set-ndi-mapping.py is invoked as a standalone subprocess (not sourced) -- its
    // DEFAULT_ACTIVE_SET fallback must read the SAME env var name camera-set.sh exports
    // (CAMERA_ACTIVE_SET) and the SAME literal default, so the two can never silently disagree.
    let py = read("scripts/set-ndi-mapping.py");
    let sh = read("scripts/camera-set.sh");
    assert!(
        py.contains(r#"os.environ.get("CAMERA_ACTIVE_SET", "cam1 cam2 cam3 cam5 cam6 cam7")"#),
        "issue 1216: set-ndi-mapping.py must read $CAMERA_ACTIVE_SET with the identical literal \
         fallback camera-set.sh itself defaults to (cam5/cam6/cam7 restored 2026-08-28)"
    );
    assert!(
        sh.contains(r#"CAMERA_ACTIVE_SET="${CAMERA_ACTIVE_SET:-cam1 cam2 cam3 cam5 cam6 cam7}""#),
        "issue 1216: camera-set.sh must default CAMERA_ACTIVE_SET to the identical literal"
    );
}
