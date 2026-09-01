//! #1114 ROOT FIX (deploy-context proactive receiver reset) — the merged WS-side CLEAR-then-SET
//! `reattach()` + the harness `mv_reverify_resolve_wait` (bounded re-resolve poll) already RECOVER a
//! bounced leg, but only REACTIVELY: `preflight_mv_reverify()`'s attempt-1 pixel check is spent FIRST
//! against a strih receiver still holding the dead pre-bounce production URL, so a bounced leg ALWAYS
//! fails attempt-1 (logging the alarming "no pixel change right after its deploy" / "camera leg is
//! dead" line) and only THEN kicks. The owner directive (issuecomment-5335833149) is to "sequence the
//! burn deploy so the receiver is kicked BEFORE the pixel-change poll starts counting".
//!
//! The fix: a new sourced helper `mv_reverify_proactive_reset` (scripts/lib/mv-reverify-escalate.sh)
//! that, at each deploy site, fires the CLEAR-then-SET reattach + the bounded re-resolve wait BEFORE
//! the guarded reverify runs — so the guarded reverify passes attempt-1 cleanly. WARN-only (always
//! returns 0; the guarded reverify below stays the real gate + the reactive fallback for a genuinely
//! dead leg). DEPLOY-context only (never in the cleanup trap), ALL_CAMBOX-gated, opt-out via
//! PREFLIGHT_MV_REVERIFY_PROACTIVE=0. Reuses the merged reattach + resolve-wait (its own #1197
//! finder-heal-wait + #795 mangle guard) — NO vendored C change, NO workflow-budget bump.
//!
//! Functional tests drive the helper with fake `strih_mv_scenes.py` + `frozen-camera-gate.py` on
//! $HERE (no rig, no OBS — the #833/#716 pattern the sibling resolve-wait test uses); structural
//! tests pin the wiring in recording-e2e.sh (a read-only preflight only the live rig runs end-to-end).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn recording_e2e() -> String {
    let path = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn escalate_lib_path() -> String {
    format!(
        "{}/scripts/lib/mv-reverify-escalate.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn escalate_lib_text() -> String {
    fs::read_to_string(escalate_lib_path()).expect("read mv-reverify-escalate.sh")
}

/// Fixed bash driver (no format! interpolation — every varying knob comes in via `.env()`), so the
/// bash `${...}`/`$(...)` need no brace-escaping. It wires fake `strih_mv_scenes.py` (logs each
/// --reattach) + `frozen-camera-gate.py` (succeeds on the FAKE_SUCCEED_AT'th call, 0=never) into a
/// fresh $HERE, sources the lib, calls the proactive reset once, and prints one machine-parseable line.
const DRIVER: &str = r#"
set -uo pipefail
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/strih_mv_scenes.py" <<PY
import sys
open("$TMP/reattach.log","a").write("x\n")
PY
cat > "$TMP/frozen-camera-gate.py" <<PY
import os,sys
cf="$TMP/gate.count"
n=0
try: n=int(open(cf).read().strip())
except Exception: pass
n+=1
open(cf,"w").write(str(n))
sat=int(os.environ.get("FAKE_SUCCEED_AT","0"))
sys.exit(0 if (sat and n>=sat) else 1)
PY
mkdir -p "$TMP/probebin"; : > "$TMP/probebin/frozen-camera-gate"
: > "$TMP/reattach.log"; : > "$TMP/gate.count"
export HERE="$TMP" STRIH="strih.invalid" PROBE_BIN_DIR="$TMP/probebin"
export MV_REVERIFY_HEAL_WAIT_CMD=/bin/true
export PREFLIGHT_MV_REVERIFY_RESOLVE_CADENCE_S="1"
export PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S="${PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S:-3}"
# shellcheck disable=SC1090
source "$LIB"
mv_reverify_proactive_reset "cam1" "1" "5" >/dev/null 2>&1
rc=$?
reat=$(wc -l < "$TMP/reattach.log" | tr -d ' ')
gate=$(cat "$TMP/gate.count" 2>/dev/null || echo 0)
[ -z "$gate" ] && gate=0
echo "rc=$rc reattach=$reat gate=$gate"
"#;

/// (rc, reattach_count, gate_calls) from one driver run with the given extra env.
fn drive(extra_env: &[(&str, &str)]) -> (i32, u32, u32) {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(DRIVER);
    cmd.env("LIB", escalate_lib_path());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run proactive-reset driver");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("rc="))
        .unwrap_or_else(|| {
            panic!(
                "driver produced no rc= line. stdout:\n{stdout}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let mut rc = -1;
    let mut reat = 0u32;
    let mut gate = 0u32;
    for tok in line.split_whitespace() {
        if let Some(v) = tok.strip_prefix("rc=") {
            rc = v.parse().unwrap_or(-1);
        } else if let Some(v) = tok.strip_prefix("reattach=") {
            reat = v.parse().unwrap_or(0);
        } else if let Some(v) = tok.strip_prefix("gate=") {
            gate = v.parse().unwrap_or(0);
        }
    }
    (rc, reat, gate)
}

// ---- functional: the helper's behavior over fakes -------------------------------------------------

/// #1114 review 🟡-1: the SILENT pre-probe (the FIRST gate call) must leave an ALREADY-delivering leg
/// UNTOUCHED — no reattach kick, no teardown — so a later ALL_CAMBOX-loop camera that re-resolved on
/// its own during the preceding cameras' serial reverifies is never regressed into a fresh re-resolve.
#[test]
fn proactive_reset_skips_the_kick_when_the_leg_is_already_delivering() {
    // FAKE_SUCCEED_AT=1 → the pre-probe (gate call 1) succeeds → return without kicking.
    let (rc, reat, gate) = drive(&[("ALL_CAMBOX", "1"), ("FAKE_SUCCEED_AT", "1")]);
    assert_eq!(rc, 0, "#1114: a delivering leg must return 0");
    assert_eq!(
        reat, 0,
        "#1114/🟡-1: an already-delivering receiver must NOT be torn down (no reattach)"
    );
    assert_eq!(
        gate, 1,
        "#1114: exactly the one silent pre-probe runs on a delivering leg"
    );
}

/// A genuinely STALE bounced leg: the pre-probe (gate call 1) fails, so the helper kicks the
/// CLEAR-then-SET reattach ONCE, then the resolve poll (gate call 2) delivers → rc 0. This is the
/// common bounced case the guarded reverify then confirms cleanly (no counted attempt-1 failure).
#[test]
fn proactive_reset_kicks_once_when_stale_then_recovers() {
    // FAKE_SUCCEED_AT=2 → pre-probe fails (1), kick, resolve poll succeeds (2).
    let (rc, reat, gate) = drive(&[("ALL_CAMBOX", "1"), ("FAKE_SUCCEED_AT", "2")]);
    assert_eq!(rc, 0, "#1114: proactive reset must return 0 on recovery");
    assert_eq!(
        reat, 1,
        "#1114: a stale leg is reset with exactly one CLEAR-then-SET reattach"
    );
    assert!(
        gate >= 2,
        "#1114: the pre-probe (fail) + at least one resolve poll (success) must run, got {gate}"
    );
}

/// WARN-only: a genuinely dead leg (the poll NEVER succeeds) STILL returns 0 — the proactive reset
/// never aborts the run; the guarded reverify + its reactive fallback stay the real gate/escalation.
#[test]
fn proactive_reset_is_warn_only_even_on_a_dead_leg() {
    let (rc, reat, gate) = drive(&[
        ("ALL_CAMBOX", "1"),
        ("FAKE_SUCCEED_AT", "0"),
        ("PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S", "2"),
    ]);
    assert_eq!(
        rc, 0,
        "#1114: proactive reset must be WARN-only (return 0 even when the leg never re-resolves)"
    );
    assert_eq!(
        reat, 1,
        "#1114: it still kicks once before riding out the (bounded) re-resolve window"
    );
    assert!(
        gate >= 1,
        "#1114: it must actually poll the resolve gate within the window, got {gate}"
    );
}

/// The cleanup trap must stay fast: in cleanup context the proactive reset is a no-op (no kick, rc 0)
/// so it can never outlast a GH-Actions cancellation grace window.
#[test]
fn proactive_reset_skips_in_cleanup_context() {
    let (rc, reat, _g) = drive(&[
        ("ALL_CAMBOX", "1"),
        ("FAKE_SUCCEED_AT", "1"),
        ("PREFLIGHT_MV_REVERIFY_CONTEXT", "cleanup"),
    ]);
    assert_eq!(rc, 0, "#1114: cleanup context must return 0");
    assert_eq!(
        reat, 0,
        "#1114: cleanup context must NOT fire a reattach kick"
    );
}

/// Opt-out seam: PREFLIGHT_MV_REVERIFY_PROACTIVE=0 disables the proactive reset entirely (no kick).
#[test]
fn proactive_reset_honours_the_opt_out_env() {
    let (rc, reat, _g) = drive(&[
        ("ALL_CAMBOX", "1"),
        ("FAKE_SUCCEED_AT", "1"),
        ("PREFLIGHT_MV_REVERIFY_PROACTIVE", "0"),
    ]);
    assert_eq!(rc, 0, "#1114: opt-out must return 0");
    assert_eq!(
        reat, 0,
        "#1114: PREFLIGHT_MV_REVERIFY_PROACTIVE=0 must skip the reattach kick"
    );
}

/// Single-camera runs (ALL_CAMBOX unset/0) never sweep the secondary legs, so the proactive reset is
/// a no-op there — mirroring preflight_mv_reverify's own ALL_CAMBOX gate.
#[test]
fn proactive_reset_is_a_noop_outside_all_cambox() {
    let (rc, reat, _g) = drive(&[("ALL_CAMBOX", "0"), ("FAKE_SUCCEED_AT", "1")]);
    assert_eq!(rc, 0, "#1114: non-ALL_CAMBOX must return 0");
    assert_eq!(reat, 0, "#1114: non-ALL_CAMBOX must skip the reattach kick");
}

/// #1133 (.claude/rules/ci-testing-gotchas.md): the helper is called BARE at the deploy sites, which
/// run under `set -euo pipefail`. A dead-leg proactive reset must NEVER set-e-abort the caller.
#[test]
fn proactive_reset_never_aborts_the_caller_under_set_euo_pipefail() {
    let script = format!(
        r#"
set -euo pipefail
export LIB="{lib}"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/strih_mv_scenes.py" <<'PY'
import sys
PY
cat > "$TMP/frozen-camera-gate.py" <<'PY'
import sys
sys.exit(1)
PY
mkdir -p "$TMP/probebin"; : > "$TMP/probebin/frozen-camera-gate"
export HERE="$TMP" STRIH="strih.invalid" PROBE_BIN_DIR="$TMP/probebin"
export MV_REVERIFY_HEAL_WAIT_CMD=/bin/true
export ALL_CAMBOX=1 PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S=1 PREFLIGHT_MV_REVERIFY_RESOLVE_CADENCE_S=1
# shellcheck disable=SC1090
source "$LIB"
mv_reverify_proactive_reset "cam1" "1" "5" >/dev/null 2>&1
echo "CONTINUED"
"#,
        lib = escalate_lib_path()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run set-e driver");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("CONTINUED"),
        "#1114/#1133: the caller must continue past a dead-leg proactive reset under set -euo pipefail. stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---- structural: the wiring in recording-e2e.sh + the lib ----------------------------------------

/// The lib must define the helper, gated to deploy context with an opt-out seam.
#[test]
fn escalate_lib_defines_the_proactive_reset_helper() {
    let lib = escalate_lib_text();
    assert!(
        lib.contains("mv_reverify_proactive_reset()"),
        "#1114: scripts/lib/mv-reverify-escalate.sh must define mv_reverify_proactive_reset"
    );
    // deploy-context gate + opt-out seam must be present in the helper.
    assert!(
        lib.contains(r#"!= "cleanup""#) && lib.contains("PREFLIGHT_MV_REVERIFY_PROACTIVE"),
        "#1114: the helper must be deploy-context gated (!= \"cleanup\") with a PREFLIGHT_MV_REVERIFY_PROACTIVE opt-out"
    );
}

/// The cam1 [2/8] deploy site must call the proactive reset BEFORE the guarded reverify (so the kick
/// lands before the pixel poll starts counting), and AFTER the once-only painter-up wait.
#[test]
fn cam1_deploy_site_proactive_reset_before_the_guarded_reverify() {
    let s = recording_e2e();
    let proactive = s
        .find(r#"mv_reverify_proactive_reset "$CAMERA_NAME" "${CAMERA_NAME#cam}""#)
        .expect("#1114: cam1 deploy site must call mv_reverify_proactive_reset");
    let guarded = s
        .find(r#"mv_reverify_or_escalate "$CAMERA_NAME" "${CAMERA_NAME#cam}""#)
        .expect("#1114: cam1 guarded reverify call must still exist");
    assert!(
        proactive < guarded,
        "#1114: the proactive reset must run BEFORE the guarded reverify at the cam1 site"
    );
    let painter = s
        .find("mv_reverify_painter_up_wait")
        .expect("#1093: painter-up wait must still exist");
    assert!(
        painter < proactive,
        "#1093/#1114: the painter-up wait must still precede the proactive reset (and thus the probe)"
    );
}

/// The ALL_CAMBOX [2b/8] reverify loop must call the proactive reset BEFORE its guarded reverify.
#[test]
fn all_cambox_loop_proactive_reset_before_the_guarded_reverify() {
    let s = recording_e2e();
    let proactive = s
        .find(r#"mv_reverify_proactive_reset "$_cn" "${_cn#cam}""#)
        .expect("#1114: the ALL_CAMBOX loop must call mv_reverify_proactive_reset");
    let guarded = s
        .find(r#"mv_reverify_or_escalate "$_cn" "${_cn#cam}""#)
        .expect("#1114: the ALL_CAMBOX guarded reverify call must still exist");
    assert!(
        proactive < guarded,
        "#1114: the proactive reset must run BEFORE the guarded reverify in the ALL_CAMBOX loop"
    );
}

/// #758 invariant guard: the proactive reset must NOT touch preflight_mv_reverify's body — the
/// reactive reattach inside it still fires EXACTLY once (strih_mv_scenes.py count in the body == 1).
#[test]
fn preflight_mv_reverify_body_reattach_count_unchanged() {
    let s = recording_e2e();
    let start = s
        .find("preflight_mv_reverify() {")
        .expect("preflight_mv_reverify defined");
    let end = s[start..]
        .find("\n}\n")
        .map(|i| start + i)
        .expect("body closes");
    let body = &s[start..end];
    assert_eq!(
        body.matches("strih_mv_scenes.py").count(),
        1,
        "#1114/#758: preflight_mv_reverify's own reattach must still fire exactly once (the proactive \
         reset lives in the lib, not this body)"
    );
}

/// #1114 review 🔵-2: the proactive reset must honour the same PREFLIGHT_MV_REVERIFY_CALL_TIMEOUT seam
/// the guarded reverify uses — its call_timeout default must chain through that env, not a bare 30.
#[test]
fn proactive_reset_call_timeout_honours_the_shared_env_seam() {
    let lib = escalate_lib_text();
    assert!(
        lib.contains(r#"call_timeout="${3:-${PREFLIGHT_MV_REVERIFY_CALL_TIMEOUT:-30}}""#),
        "#1114: mv_reverify_proactive_reset's call_timeout must default via PREFLIGHT_MV_REVERIFY_CALL_TIMEOUT (the seam preflight_mv_reverify honours), not a hardcoded 30"
    );
}

/// #1114 review 🔵-3: a non-integer PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S must NOT fatally abort the
/// caller. `mv_reverify_resolve_wait`'s `$((SECONDS + resolve_s))` would throw an "invalid arithmetic
/// operator" (a fatal expansion error `|| true` cannot catch) on a float; the coercion `${resolve_s%.*}`
/// (the #1197 finder-heal precedent) closes it. This is the WARN-only impossibility the proactive-reset
/// doc now claims, so it is exercised through resolve_wait directly.
#[test]
fn resolve_wait_float_settle_s_never_aborts_the_caller() {
    let script = format!(
        r#"
set -euo pipefail
export LIB="{lib}"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/frozen-camera-gate.py" <<'PY'
import sys
sys.exit(0)
PY
mkdir -p "$TMP/probebin"; : > "$TMP/probebin/frozen-camera-gate"
export HERE="$TMP" STRIH="strih.invalid" PROBE_BIN_DIR="$TMP/probebin"
export MV_REVERIFY_HEAL_WAIT_CMD=/bin/true
export PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S=90.5 PREFLIGHT_MV_REVERIFY_RESOLVE_CADENCE_S=1
# shellcheck disable=SC1090
source "$LIB"
mv_reverify_resolve_wait "cam1" "1" "5" >/dev/null 2>&1 || true
echo "CONTINUED"
"#,
        lib = escalate_lib_path()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run float-settle driver");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("CONTINUED"),
        "#1114/🔵-3: a float RESOLVE_SETTLE_S must be coerced, not fatally abort the caller. stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
