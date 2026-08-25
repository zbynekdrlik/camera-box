//! #1203 — offline integration guard for `scripts/ndi-halving-watchdog.sh`'s DECISION COMPOSITION
//! (`main`/`handle_input`): the 2-pass confirm → page, the gated reattach CURE + its per-input
//! cooldown escalation (cure → page-within-cooldown → cure-past-cooldown, never reattach-spam), the
//! HEALTHY recovery ping, the no-double-page SKIP (receiver OR sender down per #1001), the healthy-
//! SIBLING context line, and the tap-broken WARN. The pure decision matrix is covered by
//! `tests/python/test_ndi_halving_decision_1203.py`; this file covers the CALLER GLUE that drives the
//! cure/alert/recovery side effects (per #414 — an unattended production timer's novel logic, esp.
//! an ACTUATOR arm, must be tested, weighted like a correctness bug).
//!
//! Fully offline + deterministic: the ssh probe is replaced via `NDI_HALVING_PROBE_CMD`, the cure via
//! `NDI_HALVING_CURE_CMD` (records each reattach — no OBS touched), and `airuleset.py notify` via a
//! stub `AIRULESET_NOTIFY` (records each alert body — no Discord). The clock is pinned via
//! `NDI_HALVING_NOW`, and every state file is a per-test tempdir path
//! (`.claude/rules/ci-testing-gotchas.md` #975 tempdir isolation). `--dry-run` proves the DECISION
//! wiring via the "WOULD …" log lines; a LIVE (non-dry) pass proves the cure is genuinely DRIVEN
//! through the seam (dry-run deliberately SKIPS the reattach, so the cure-count is only meaningful
//! live). Same fixture-shim pattern as `tests/harness_asio_starve_alert_watchdog_1023.rs`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn watchdog() -> PathBuf {
    let s = manifest_dir().join("scripts/ndi-halving-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

// One stream-OBS recv-timing #797 line for `source` at `ts` (n per-interval, cap_avg in ms).
fn line(ts: &str, source: &str, n: u32, cap: f64) -> String {
    format!(
        "{ts}: [distroav] recv-timing #797 '{source}': n={n} cap_avg={cap:.2}ms cap_max=99.00ms out_avg=0.20ms out_max=1.10ms\n"
    )
}

// A per-source history at ~5.0 s spacing (needs >=2 lines for the within-pass rate).
fn hist(source: &str, n: u32, cap: f64) -> String {
    let mut s = String::new();
    for i in 0..4u32 {
        let ss = i * 5;
        s += &line(
            &format!("14:0{}:{:02}.017", ss / 60, ss % 60),
            source,
            n,
            cap,
        );
    }
    s
}

// 2ME PGM halved (15 fps: n=75/~5s, cap 65.9) + a healthy 60 fps sibling (n=300/~5s, cap 16.3).
fn halved_with_sibling() -> String {
    hist("NDI 2ME PGM", 75, 65.90) + &hist("NDI cam1", 300, 16.30)
}
// 2ME PGM halved, NO healthy sibling in the watched set.
fn halved_only() -> String {
    hist("NDI 2ME PGM", 75, 65.90)
}
// 2ME PGM healthy (30 fps: n=150/~5s, cap 12.6).
fn healthy_only() -> String {
    hist("NDI 2ME PGM", 150, 12.60)
}

/// A per-test rig: a tempdir with a probe.sh that cats a fixture log the test rewrites per pass, a
/// cure.sh that records each reattach invocation, a notify stub that records each alert body, and the
/// alert + network-reach state files.
struct Rig {
    _dir: tempfile::TempDir,
    probe: PathBuf,
    cure: PathBuf,
    cure_calls: PathBuf,
    notify: PathBuf,
    notify_calls: PathBuf,
    logfix: PathBuf,
    state: PathBuf,
    netreach: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe.sh");
        write_exec(
            &probe,
            "#!/usr/bin/env bash\ncat \"$NDI_HALVING_TEST_LOG\" 2>/dev/null || true\n",
        );
        let cure = dir.path().join("cure.sh");
        let cure_calls = dir.path().join("cure-calls.txt");
        // Records `<ip> <input>` per invocation and succeeds (exit 0) — a stand-in for the real
        // obs_phase2 idle-receiver -> restore, so the harness proves the cure was DRIVEN without OBS.
        write_exec(
            &cure,
            &format!(
                "#!/usr/bin/env bash\nprintf '%s %s\\n' \"$1\" \"$2\" >> {}\n",
                cure_calls.display()
            ),
        );
        // AIRULESET_NOTIFY stub: invoked as `python3 <notify> notify --body "…"`; records every body.
        let notify = dir.path().join("notify_stub.py");
        let notify_calls = dir.path().join("notify-calls.txt");
        fs::write(
            &notify,
            format!(
                "import sys\nopen(r'{}', 'a').write(' '.join(sys.argv[1:]) + '\\n')\n",
                notify_calls.display()
            ),
        )
        .unwrap();
        Rig {
            probe,
            cure,
            cure_calls,
            notify,
            notify_calls,
            logfix: dir.path().join("obslog.txt"),
            state: dir.path().join("ndi-halving.state"),
            netreach: dir.path().join("netreach.state"),
            _dir: dir,
        }
    }

    fn seed_receiver_down(&self) {
        fs::write(&self.netreach, "alerted_stream=1\n").unwrap();
    }
    fn seed_sender_down(&self) {
        fs::write(&self.netreach, "alerted_strih=1\n").unwrap();
    }

    fn cure_call_count(&self) -> usize {
        fs::read_to_string(&self.cure_calls)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }
    fn notify_bodies(&self) -> String {
        fs::read_to_string(&self.notify_calls).unwrap_or_default()
    }

    /// Run ONE pass. `dry` runs `--dry-run` (proves the decision via "WOULD …" logs, never a real
    /// cure/notify); non-dry drives the real cure/notify seams (proves the cure is DRIVEN + records
    /// the alert body). `selfheal` arms the cure arm; `now` pins the clock; `inputs` is the `;`-list
    /// of `<name>|<fps>` specs. Returns stdout+stderr.
    fn pass(&self, log: &str, inputs: &str, selfheal: bool, now: u64, dry: bool) -> String {
        fs::write(&self.logfix, log).unwrap();
        let mut cmd = Command::new("bash");
        cmd.arg(watchdog());
        if dry {
            cmd.arg("--dry-run");
        }
        let out = cmd
            .env(
                "NDI_HALVING_PROBE_CMD",
                format!("bash {}", self.probe.display()),
            )
            .env(
                "NDI_HALVING_CURE_CMD",
                format!("bash {}", self.cure.display()),
            )
            .env("AIRULESET_NOTIFY", &self.notify)
            .env("NDI_HALVING_TEST_LOG", &self.logfix)
            .env("NDI_HALVING_STATE_FILE", &self.state)
            .env("NDI_HALVING_NETREACH_STATE_FILE", &self.netreach)
            .env("NDI_HALVING_INPUTS", inputs)
            .env("NDI_HALVING_SELFHEAL", if selfheal { "1" } else { "0" })
            .env("NDI_HALVING_COOLDOWN_S", "600")
            .env("NDI_HALVING_NOW", now.to_string())
            .current_dir(manifest_dir())
            .output()
            .expect("failed to run watchdog");
        assert!(
            out.status.success(),
            "watchdog pass exited non-zero: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }
}

const PGM: &str = "NDI 2ME PGM|30;NDI cam1|60";
const PGM_ONLY: &str = "NDI 2ME PGM|30";

// ---------------------------------------------------------------------------------------------
// (a) halved + healthy sibling: pass 1 HOLDS (2-pass confirm), pass 2 PAGES (cure OFF) with the
//     per-connection sibling context; the healthy 60 fps sibling reads HEALTHY.
// ---------------------------------------------------------------------------------------------
#[test]
fn halved_with_healthy_sibling_holds_then_pages_report_only() {
    let rig = Rig::new();
    let p1 = rig.pass(&halved_with_sibling(), PGM, false, 1000, true);
    assert!(
        p1.contains("-> HALVED"),
        "pass1 should classify HALVED: {p1}"
    );
    assert!(
        p1.contains("holding") && !p1.contains("WOULD alert"),
        "pass1 must HOLD (2-pass confirm), not page: {p1}"
    );
    assert!(
        p1.contains("'NDI cam1' on stream") && p1.contains("-> HEALTHY"),
        "the 60 fps sibling should read HEALTHY: {p1}"
    );

    let p2 = rig.pass(&halved_with_sibling(), PGM, false, 1005, true);
    assert!(
        p2.contains("WOULD alert: 'NDI 2ME PGM' CONFIRMED halved"),
        "pass2 must page once CONFIRMED across 2 passes (cure OFF): {p2}"
    );
    assert!(
        p2.contains("per-connection"),
        "the page must carry the healthy-sibling (per-connection) context: {p2}"
    );
}

// ---------------------------------------------------------------------------------------------
// (b) cure arm ARMED (LIVE, non-dry so the reattach is genuinely driven through the seam): pass 2
//     CURES instead of paging; a still-halved pass WITHIN the cooldown PAGES (no reattach-spam,
//     records the alert body); a pass PAST the cooldown cures again.
// ---------------------------------------------------------------------------------------------
#[test]
fn armed_cure_reattaches_then_cooldown_gates_reattach_spam() {
    let rig = Rig::new();
    rig.pass(&halved_only(), PGM_ONLY, true, 1000, false); // pass1 holds
    let p2 = rig.pass(&halved_only(), PGM_ONLY, true, 1005, false);
    assert!(
        p2.contains("reattach attempted"),
        "armed pass2 must drive the reattach, not page: {p2}"
    );
    assert_eq!(rig.cure_call_count(), 1, "exactly one reattach so far");
    assert!(
        rig.notify_bodies().is_empty(),
        "a cure pass must not page: {}",
        rig.notify_bodies()
    );

    // Still halved, only 300 s later (< 600 s cooldown) -> PAGE, do NOT re-cure.
    let p3 = rig.pass(&halved_only(), PGM_ONLY, true, 1305, false);
    assert!(
        !p3.contains("reattach attempted"),
        "within cooldown must NOT re-cure: {p3}"
    );
    assert_eq!(
        rig.cure_call_count(),
        1,
        "no second reattach within the cooldown"
    );
    assert!(
        rig.notify_bodies().contains("POLOVIČNEJ kadencii")
            && rig.notify_bodies().contains("NDI 2ME PGM"),
        "within cooldown a persistent halving must PAGE (recorded alert body): {}",
        rig.notify_bodies()
    );

    // 700 s after the cure (>= 600 s cooldown) -> cure again.
    rig.pass(&halved_only(), PGM_ONLY, true, 1705, false);
    assert_eq!(
        rig.cure_call_count(),
        2,
        "a second reattach past the cooldown"
    );
}

// ---------------------------------------------------------------------------------------------
// (c) no-double-page: receiver (stream) OR sender (strih) down per #1001 -> SKIP every input.
// ---------------------------------------------------------------------------------------------
#[test]
fn receiver_down_per_issue_1001_skips_all_inputs() {
    let rig = Rig::new();
    rig.seed_receiver_down();
    let p = rig.pass(&halved_only(), PGM_ONLY, true, 1000, false);
    assert!(
        p.contains("SKIP all inputs this pass"),
        "a #1001-confirmed-down receiver must SKIP (no double page): {p}"
    );
    assert_eq!(rig.cure_call_count(), 0, "SKIP must never cure");
    assert!(rig.notify_bodies().is_empty(), "SKIP must never page");
}

#[test]
fn sender_down_per_issue_1001_skips_all_inputs() {
    let rig = Rig::new();
    rig.seed_sender_down();
    let p = rig.pass(&halved_only(), PGM_ONLY, true, 1000, false);
    assert!(
        p.contains("SKIP all inputs this pass"),
        "a #1001-confirmed-down SENDER (2ME PGM producer) must also SKIP: {p}"
    );
}

// ---------------------------------------------------------------------------------------------
// (d) recovery: after an input we PAGED for reads HEALTHY again, fire ONE recovery ping.
// ---------------------------------------------------------------------------------------------
#[test]
fn recovery_ping_fires_once_after_a_paged_input_clears() {
    let rig = Rig::new();
    rig.pass(&halved_only(), PGM_ONLY, false, 1000, true); // hold
    let p2 = rig.pass(&halved_only(), PGM_ONLY, false, 1005, true); // page
    assert!(
        p2.contains("WOULD alert"),
        "precondition: pass2 pages: {p2}"
    );

    let p3 = rig.pass(&healthy_only(), PGM_ONLY, false, 1010, true);
    assert!(
        p3.contains("WOULD send recovery: 'NDI 2ME PGM'"),
        "a paged input reading HEALTHY again must fire ONE recovery ping: {p3}"
    );
    let p4 = rig.pass(&healthy_only(), PGM_ONLY, false, 1015, true);
    assert!(
        !p4.contains("WOULD send recovery"),
        "recovery must fire only once: {p4}"
    );
}

// ---------------------------------------------------------------------------------------------
// (e) tap-broken: an input whose recv-timing line is ABSENT stays UNKNOWN and, after enough blind
//     passes, fires ONE "tap broken" WARN (never a silent-unknown).
// ---------------------------------------------------------------------------------------------
#[test]
fn a_never_seen_input_fires_a_tap_broken_warn() {
    let rig = Rig::new();
    // Watch an input the log never mentions (only 'NDI cam1' is present), threshold=2.
    let log = hist("NDI cam1", 300, 16.30);
    let run = |now: u64| -> String {
        fs::write(&rig.logfix, &log).unwrap();
        let out = Command::new("bash")
            .arg(watchdog())
            .arg("--dry-run")
            .env(
                "NDI_HALVING_PROBE_CMD",
                format!("bash {}", rig.probe.display()),
            )
            .env("NDI_HALVING_TEST_LOG", &rig.logfix)
            .env("NDI_HALVING_STATE_FILE", &rig.state)
            .env("NDI_HALVING_NETREACH_STATE_FILE", &rig.netreach)
            .env("NDI_HALVING_INPUTS", "NDI 2ME PGM|30")
            .env("NDI_HALVING_TAP_BROKEN_THRESHOLD", "2")
            .env("NDI_HALVING_NOW", now.to_string())
            .current_dir(manifest_dir())
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let p1 = run(1000);
    assert!(
        p1.contains("-> UNKNOWN") && !p1.contains("tap BROKEN"),
        "blind pass1: UNKNOWN, no WARN yet: {p1}"
    );
    let p2 = run(1005);
    assert!(
        p2.contains("tap BROKEN"),
        "a never-seen input must fire ONE tap-broken WARN past the threshold (never a silent unknown): {p2}"
    );
}

// ---------------------------------------------------------------------------------------------
// (f) BORDERLINE holds — never pages, never cures, and does not advance the confirm counter.
// ---------------------------------------------------------------------------------------------
#[test]
fn borderline_holds_and_never_pages() {
    let rig = Rig::new();
    // 22 fps: n=110/~5s -> between 0.6*30 (18) and 0.85*30 (25.5); cap 12.6 (well under the caps).
    let border = hist("NDI 2ME PGM", 110, 12.60);
    let p1 = rig.pass(&border, PGM_ONLY, true, 1000, false);
    assert!(
        p1.contains("-> BORDERLINE"),
        "should classify BORDERLINE: {p1}"
    );
    let p2 = rig.pass(&border, PGM_ONLY, true, 1005, false);
    assert!(
        !p2.contains("reattach attempted"),
        "BORDERLINE must never cure even sustained: {p2}"
    );
    assert_eq!(rig.cure_call_count(), 0);
    assert!(
        rig.notify_bodies().is_empty(),
        "BORDERLINE must never page: {}",
        rig.notify_bodies()
    );
}

// ---------------------------------------------------------------------------------------------
// (g) attempt_reattach's INTERNAL logic — the actuator's most dangerous branch (#414 / #1203 🟡4).
//     Driven through the REAL bash function via a FAKE obs_phase2 (NDI_HALVING_OBS_PHASE2), NOT the
//     whole-function CURE_CMD override — so the PREV parse, the empty-PREV refusal, the restore
//     retry, and the LEFT-IDLED (restore-failed) page all execute under test.
// ---------------------------------------------------------------------------------------------

/// A fake obs_phase2.py: records "idle"/"restore" per call to $FAKE_OBS_CALLS; on idle prints
/// PREV_NDI_NAME=$FAKE_PREV (nothing if empty); on restore exits $FAKE_RESTORE_RC.
const FAKE_OBS_PHASE2: &str = r#"import sys, os
open(os.environ["FAKE_OBS_CALLS"], "a").write(("restore" if "--restore" in sys.argv else "idle") + "\n")
if "--restore" not in sys.argv:
    p = os.environ.get("FAKE_PREV", "")
    if p:
        print("PREV_NDI_NAME=" + p)
    raise SystemExit(0)
raise SystemExit(int(os.environ.get("FAKE_RESTORE_RC", "0")))
"#;

/// Run a confirmed-cure episode (hold @1000, cure @1005) through the REAL attempt_reattach with a
/// fake obs_phase2. Returns (pass2 stdout+stderr, obs_phase2 call-kinds, notify bodies).
fn cure_via_fake_obs_phase2(prev: &str, restore_rc: i32) -> (String, String, String) {
    let rig = Rig::new();
    let fake = rig._dir.path().join("fake_obs_phase2.py");
    fs::write(&fake, FAKE_OBS_PHASE2).unwrap();
    let obs_calls = rig._dir.path().join("obs-phase2-calls.txt");

    let run = |now: u64| -> String {
        fs::write(&rig.logfix, halved_only()).unwrap();
        let out = Command::new("bash")
            .arg(watchdog())
            .env(
                "NDI_HALVING_PROBE_CMD",
                format!("bash {}", rig.probe.display()),
            )
            // NO NDI_HALVING_CURE_CMD -> the real attempt_reattach runs, shelling the fake obs_phase2.
            .env("NDI_HALVING_OBS_PHASE2", &fake)
            .env("AIRULESET_NOTIFY", &rig.notify)
            .env("NDI_HALVING_OBS_WS_PW", "x") // non-empty so the armed-passwordless warn stays quiet
            .env("NDI_HALVING_TEST_LOG", &rig.logfix)
            .env("NDI_HALVING_STATE_FILE", &rig.state)
            .env("NDI_HALVING_NETREACH_STATE_FILE", &rig.netreach)
            .env("NDI_HALVING_INPUTS", PGM_ONLY)
            .env("NDI_HALVING_SELFHEAL", "1")
            .env("NDI_HALVING_COOLDOWN_S", "600")
            .env("NDI_HALVING_NOW", now.to_string())
            .env("FAKE_OBS_CALLS", &obs_calls)
            .env("FAKE_PREV", prev)
            .env("FAKE_RESTORE_RC", restore_rc.to_string())
            .current_dir(manifest_dir())
            .output()
            .expect("run");
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    run(1000); // hold
    let p2 = run(1005); // confirmed -> cure via the real attempt_reattach
    let calls = fs::read_to_string(&obs_calls).unwrap_or_default();
    let bodies = fs::read_to_string(&rig.notify_calls).unwrap_or_default();
    (p2, calls, bodies)
}

#[test]
fn reattach_idles_then_restores_when_prev_is_captured() {
    let (p2, calls, bodies) = cure_via_fake_obs_phase2("CAM2 (usb)", 0);
    assert!(
        p2.contains("reattached 'NDI 2ME PGM'") && p2.contains("reattach attempted"),
        "PREV captured + restore ok must reattach: {p2}"
    );
    assert!(
        calls.contains("idle") && calls.contains("restore"),
        "must idle THEN restore: {calls}"
    );
    assert!(
        bodies.is_empty(),
        "a successful reattach must not page: {bodies}"
    );
}

#[test]
fn reattach_refuses_to_idle_when_prev_is_missing() {
    let (p2, calls, bodies) = cure_via_fake_obs_phase2("", 0);
    assert!(
        p2.contains("did not return PREV_NDI_NAME") && p2.contains("name untouched"),
        "empty PREV must refuse — name untouched, no restore: {p2}"
    );
    assert!(
        calls.contains("idle") && !calls.contains("restore"),
        "no restore may be attempted when PREV was not captured: {calls}"
    );
    assert!(
        bodies.is_empty(),
        "a could-not-start cure is safe, must not page: {bodies}"
    );
}

#[test]
fn reattach_failure_leaves_input_idled_and_pages_immediately() {
    let (p2, calls, bodies) = cure_via_fake_obs_phase2("CAM2 (usb)", 1);
    assert!(
        p2.contains("LEFT IDLED"),
        "a persistent restore failure must log the LEFT-IDLED wedge: {p2}"
    );
    // idle once + two restore retries.
    assert_eq!(
        calls.matches("restore").count(),
        2,
        "the restore must be retried once (two attempts): {calls}"
    );
    assert!(
        bodies.contains("ostal IDLED") && bodies.contains("NDI 2ME PGM"),
        "a LEFT-IDLED input must page IMMEDIATELY naming the manual remedy: {bodies}"
    );
}
