//! #1114 REZÍDUUM (harness side) — after the merged WS-side CLEAR-then-SET `reattach()`, the fresh
//! DistroAV finder must RE-RESOLVE the live post-bounce burn sender by URL, MEASURED at up to ~2 min
//! on the live rig (owner comments 2026-08-19: two cameras/run read "no pixel change" through the
//! whole ~52s [2/8] attempt budget, then recover). The old `preflight_mv_reverify()` fired the
//! re-attach kick INSIDE the loop (attempt 1) and let the ~2min re-resolve race the remaining ~48s of
//! budget → false FAIL "camera leg is dead" → destructive #1093 escalation (force-kill strih OBS,
//! which itself poisoned a recording verdict).
//!
//! The fix: after the kick, give the fresh finder its OWN bounded re-resolve window via a new sourced
//! helper `mv_reverify_resolve_wait` (scripts/lib/mv-reverify-escalate.sh) — a REAL poll of the SAME
//! `frozen-camera-gate.py` gate that exits the instant a pixel changes (a fast re-lock costs ~0
//! extra) and only spends the full measured window on a genuinely slow re-resolve. Gated to the
//! non-cleanup (deploy) context so the cleanup trap stays fast. NOT a blind sleep, NOT a blind
//! workflow-budget bump.
//!
//! Two functional tests drive the helper with a fake `frozen-camera-gate.py` (no rig, no OBS, exactly
//! like harness_optical_chain_cleanup_surface_860.rs drives its lib); the structural tests pin the
//! wiring in recording-e2e.sh (a read-only preflight only the live rig can exercise end-to-end).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn recording_e2e() -> String {
    let path = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn escalate_lib() -> String {
    format!(
        "{}/scripts/lib/mv-reverify-escalate.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Run a bash driver that sources the escalate lib after wiring a fake `frozen-camera-gate.py` into
/// $HERE. Returns (stdout, stderr, exit_code).
fn drive_resolve_wait(succeed_at: u32, resolve_settle_s: u32) -> (String, String, i32) {
    let lib = escalate_lib();
    // The fake gate increments a counter file and exits 0 on the succeed_at'th call (0 = never).
    let script = format!(
        r#"
set -uo pipefail
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/frozen-camera-gate.py" <<'PY'
import os, sys
cf = os.environ["FAKE_COUNTER"]
n = 0
try:
    n = int(open(cf).read().strip())
except Exception:
    pass
n += 1
open(cf, "w").write(str(n))
sat = int(os.environ.get("FAKE_SUCCEED_AT", "0"))
sys.exit(0 if (sat and n >= sat) else 1)
PY
mkdir -p "$TMP/probebin"
: > "$TMP/probebin/frozen-camera-gate"
export HERE="$TMP"
export STRIH="strih.invalid"
export MV_REVERIFY_HEAL_WAIT_CMD=/bin/true
export PROBE_BIN_DIR="$TMP/probebin"
export FAKE_COUNTER="$TMP/counter"
export FAKE_SUCCEED_AT="{succeed_at}"
export PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S="{resolve_settle_s}"
export PREFLIGHT_MV_REVERIFY_RESOLVE_CADENCE_S="1"
source '{lib}'
mv_reverify_resolve_wait "cam1" "1" "5"
echo "RW_RC=$?"
"#
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .stdin(Stdio::null())
        .output()
        .expect("run resolve-wait driver");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// A leg that re-resolves partway through the window: the helper must exit 0 the instant a pixel
/// changes (early return), reporting recovery — never spend the whole window when the leg is back.
#[test]
fn resolve_wait_returns_zero_the_instant_the_fresh_finder_re_resolves() {
    let (stdout, stderr, rc) = drive_resolve_wait(2, 30);
    assert_eq!(rc, 0, "driver exit. stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("RW_RC=0"),
        "#1114: mv_reverify_resolve_wait must return 0 when the leg delivers a pixel change within \
         the re-resolve window. stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("recovered") && stderr.contains("receiver reset"),
        "#1114: recovery must be reported naming the receiver reset. stderr:\n{stderr}"
    );
}

/// A genuinely dead leg: the helper must poll to its bounded deadline, then return 1 (never hang,
/// never exit) so the caller falls through to the normal attempt loop / escalation.
#[test]
fn resolve_wait_returns_one_after_a_bounded_deadline_on_a_dead_leg() {
    let (stdout, stderr, rc) = drive_resolve_wait(0, 2);
    assert_eq!(
        rc, 0,
        "the bash driver itself must exit cleanly. stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("RW_RC=1"),
        "#1114: mv_reverify_resolve_wait must return 1 when the leg never delivers within the \
         window (so the caller can escalate). stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("did not re-resolve within the measured window"),
        "#1114: the deadline path must report the fresh finder did not re-resolve. stderr:\n{stderr}"
    );
}

/// The helper must be sized from a real MEASUREMENT (env-overridable), not a hardcoded blind sleep.
#[test]
fn resolve_wait_budget_is_env_overridable_and_a_real_poll() {
    let lib = fs::read_to_string(escalate_lib()).expect("read escalate lib");
    assert!(
        lib.contains("mv_reverify_resolve_wait()"),
        "#1114: scripts/lib/mv-reverify-escalate.sh must define mv_reverify_resolve_wait"
    );
    assert!(
        lib.contains("PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S")
            && lib.contains("PREFLIGHT_MV_REVERIFY_RESOLVE_CADENCE_S"),
        "#1114: the re-resolve window must be overridable env (RESOLVE_SETTLE_S deadline + \
         RESOLVE_CADENCE_S poll cadence), sized from the measured ~2min, never a hardcoded sleep"
    );
    assert!(
        lib.contains("frozen-camera-gate.py"),
        "#1114: it must be a REAL poll of the same pixel-change gate (exits early on recovery), not \
         a blind sleep"
    );
}

/// recording-e2e.sh must actually CALL the helper, from inside preflight_mv_reverify's first-failure
/// re-attach branch, and ONLY in the non-cleanup (deploy) context (cleanup must stay fast so it can
/// never outlast a GH-Actions cancellation grace window).
#[test]
fn preflight_calls_resolve_wait_after_the_kick_only_in_the_deploy_context() {
    let s = recording_e2e();
    let func_start = s
        .find("preflight_mv_reverify() {")
        .expect("preflight_mv_reverify must be defined");
    let func_end = s[func_start..]
        .find("\n}\n")
        .map(|i| func_start + i)
        .expect("function body must close with a bare }");
    let body = &s[func_start..func_end];
    assert!(
        body.contains("mv_reverify_resolve_wait"),
        "#1114: preflight_mv_reverify must call mv_reverify_resolve_wait after the re-attach kick. \
         Body:\n{body}"
    );
    // the resolve-wait call must be gated to the non-cleanup (deploy) context, matching the FAIL
    // message's own cleanup guard already present in this function.
    let call = body
        .find("mv_reverify_resolve_wait")
        .expect("resolve-wait call present");
    let preceding = &body[..call];
    assert!(
        preceding.rfind(r#"!= "cleanup""#).is_some(),
        "#1114: the resolve-wait call must be gated to the non-cleanup context (a != \"cleanup\" \
         guard must precede it in the function body). Body:\n{body}"
    );
}

/// The 758 invariant must survive: the re-attach still fires EXACTLY once (the resolve poll uses
/// frozen-camera-gate.py, never a second strih_mv_scenes.py call).
#[test]
fn resolve_wait_does_not_add_a_second_reattach() {
    let s = recording_e2e();
    let func_start = s.find("preflight_mv_reverify() {").expect("defined");
    let func_end = s[func_start..]
        .find("\n}\n")
        .map(|i| func_start + i)
        .expect("closes");
    let body = &s[func_start..func_end];
    assert_eq!(
        body.matches("strih_mv_scenes.py").count(),
        1,
        "#1114/#758: the re-attach must still fire exactly ONCE — the resolve-wait poll must use \
         frozen-camera-gate.py, not a second strih_mv_scenes.py reattach"
    );
}

/// #1114 review 🟡-2 / repo rule #1133 (.claude/rules/ci-testing-gotchas.md): mv_reverify_resolve_wait
/// runs under the caller's `set -euo pipefail`, wrapped `if mv_reverify_resolve_wait ...; then`. The
/// drive_resolve_wait helper sources under `set -uo` (no -e), so it is blind to a `set -e` abort. This
/// case sources under the caller's EXACT `set -euo pipefail`, invokes it the SAME `if`-wrapped way, and
/// asserts a sentinel after it is reached on BOTH recovery and deadline — locking the run can never abort.
#[test]
fn resolve_wait_never_aborts_the_caller_under_set_euo_pipefail() {
    let lib = escalate_lib();
    // succeed_at 2 = recovers; succeed_at 0 = deadline (RESOLVE_SETTLE_S=2s).
    for (succeed_at, settle) in [(2u32, 30u32), (0u32, 2u32)] {
        let script = format!(
            r#"
set -euo pipefail
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/frozen-camera-gate.py" <<'PY'
import os, sys
cf = os.environ["FAKE_COUNTER"]
n = 0
try:
    n = int(open(cf).read().strip())
except Exception:
    pass
n += 1
open(cf, "w").write(str(n))
sat = int(os.environ.get("FAKE_SUCCEED_AT", "0"))
sys.exit(0 if (sat and n >= sat) else 1)
PY
mkdir -p "$TMP/probebin"
: > "$TMP/probebin/frozen-camera-gate"
export HERE="$TMP" STRIH="strih.invalid" PROBE_BIN_DIR="$TMP/probebin" MV_REVERIFY_HEAL_WAIT_CMD=/bin/true
export FAKE_COUNTER="$TMP/counter" FAKE_SUCCEED_AT="{succeed_at}"
export PREFLIGHT_MV_REVERIFY_RESOLVE_SETTLE_S="{settle}" PREFLIGHT_MV_REVERIFY_RESOLVE_CADENCE_S="1"
source '{lib}'
if mv_reverify_resolve_wait "cam1" "1" "5"; then :; else :; fi
echo "REACHED_END"
"#
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(&script)
            .stdin(Stdio::null())
            .output()
            .expect("run set-e resolve-wait driver");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("REACHED_END"),
            "#1114/#1133: mv_reverify_resolve_wait (succeed_at={succeed_at}) must never set -e-abort \
             its `if`-wrapped caller under `set -euo pipefail`. stdout:\n{stdout}"
        );
    }
}
