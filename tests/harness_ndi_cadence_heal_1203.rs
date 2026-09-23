//! #1203 — offline functional guard for `scripts/lib/ndi-cadence-heal.sh`'s orchestration
//! (`ndi_cadence_verify_and_heal`): read every strih NDI input's cadence via the #797 tap (REUSING
//! `ndi_halving_decision.py analyze`, never a 2nd parser), and for each HALVED input drive the
//! two-arm escalation from the pure `cure_plan` — idle-restore → (still halved) sender-restart →
//! (still halved) escalate — bounded, report-only, ALWAYS exit 0, logging one
//! `[cleanup] ndi-cadence: <input> HALVED -> <arm> -> <result>` line per arm and writing the
//! `ndi-cadence-<RUN>.json` telemetry. The pure decision matrix + JSON shape are covered by
//! `tests/python/test_ndi_halving_decision_1203.py`; this file covers the SHELL GLUE that drives the
//! heal seams (per #414 — an unattended production actuator's novel logic is tested like a
//! correctness bug).
//!
//! Fully offline + deterministic: the strih OBS-log read is replaced via `NDI_CADENCE_FETCH_CMD`,
//! the idle-restore arm via `NDI_CADENCE_IDLE_CMD`, and the sender-restart arm via
//! `NDI_CADENCE_SENDER_CMD` (each records its call and, per an env flag, "heals" the input by
//! touching a marker the fetch fake reads back — so a re-read reflects the cure). The settle is
//! pinned to 0 (`NDI_CADENCE_SETTLE_S`), the run dir is a per-test tempdir, and nothing touches OBS
//! or the rig. Same fixture-shim shape as `tests/harness_ndi_halving_watchdog_1203.rs`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/ndi-cadence-heal.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

/// A per-test rig: a fetch fake that emits a #797 log per input (HEALTHY once its heal-marker
/// exists, else HALVED), an idle fake + a sender fake (each records its call and, when its
/// `*_HEALS` env flag is 1, touches the input's heal-marker), a run dir, and a runner that sources
/// the lib and calls the orchestrator.
struct Rig {
    dir: tempfile::TempDir,
    fetch: PathBuf,
    idle: PathBuf,
    idle_calls: PathBuf,
    sender: PathBuf,
    sender_calls: PathBuf,
    markers: PathBuf,
    runner: PathBuf,
    run_dir: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let markers = dir.path().join("markers");
        fs::create_dir_all(&markers).unwrap();
        let run_dir = dir.path().join("run");
        fs::create_dir_all(&run_dir).unwrap();

        // A stable per-input marker filename (spaces -> _).
        let key = "san() { printf '%s' \"$1\" | tr -c 'A-Za-z0-9' '_'; }\n";

        // fetch.sh <host>: for EVERY watched input, emit two recv-timing #797 lines ~5s apart.
        // HEALTHY (n=300/5s=60fps, cap 16.3) once markers/<key> exists; else HALVED (n=150=30fps,
        // cap 33.1). Reads the input list from NDI_CADENCE_INPUTS just like the lib.
        let fetch = dir.path().join("fetch.sh");
        write_exec(
            &fetch,
            &format!(
                "#!/usr/bin/env bash\n{key}\
IFS=';' read -ra specs <<< \"$NDI_CADENCE_INPUTS\"\n\
for spec in \"${{specs[@]}}\"; do\n\
  name=\"${{spec%%|*}}\"\n\
  [ -n \"$name\" ] || continue\n\
  if [ -f \"{markers}/$(san \"$name\")\" ]; then n=300; cap=16.30; else n=150; cap=33.10; fi\n\
  printf \"14:00:00.017: [distroav] recv-timing #797 '%s': n=%s cap_avg=%s ms cap_max=99.00ms out_avg=0.20ms out_max=1.10ms\\n\" \"$name\" \"$n\" \"$cap\" | sed 's/ ms/ms/'\n\
  printf \"14:00:05.017: [distroav] recv-timing #797 '%s': n=%s cap_avg=%sms cap_max=99.00ms out_avg=0.20ms out_max=1.10ms\\n\" \"$name\" \"$n\" \"$cap\"\n\
done\n",
                markers = markers.display()
            ),
        );

        let idle = dir.path().join("idle.sh");
        let idle_calls = dir.path().join("idle-calls.txt");
        // idle.sh <host> <input>: record, and heal (touch marker) iff IDLE_HEALS=1.
        write_exec(
            &idle,
            &format!(
                "#!/usr/bin/env bash\n{key}\
printf '%s\\n' \"$2\" >> {calls}\n\
[ \"${{IDLE_HEALS:-0}}\" = 1 ] && : > \"{markers}/$(san \"$2\")\"\n\
exit 0\n",
                calls = idle_calls.display(),
                markers = markers.display()
            ),
        );

        let sender = dir.path().join("sender.sh");
        let sender_calls = dir.path().join("sender-calls.txt");
        // sender.sh <input> [host]: record, and heal iff SENDER_HEALS=1.
        write_exec(
            &sender,
            &format!(
                "#!/usr/bin/env bash\n{key}\
printf '%s\\n' \"$1\" >> {calls}\n\
[ \"${{SENDER_HEALS:-0}}\" = 1 ] && : > \"{markers}/$(san \"$1\")\"\n\
exit 0\n",
                calls = sender_calls.display(),
                markers = markers.display()
            ),
        );

        // runner.sh <host>: source the lib, call the orchestrator. `bash <written-file>` is allowed
        // in a worktree (ci-testing-gotchas), so this doubles as the local Tier-0 harness.
        let runner = dir.path().join("runner.sh");
        write_exec(
            &runner,
            &format!(
                "#!/usr/bin/env bash\nset -euo pipefail\n. \"{lib}\"\nndi_cadence_verify_and_heal \"$1\"\n",
                lib = lib().display()
            ),
        );

        Rig {
            fetch,
            idle,
            idle_calls,
            sender,
            sender_calls,
            markers,
            runner,
            run_dir,
            dir,
        }
    }

    fn pre_heal(&self, input: &str) {
        let san: String = input
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        fs::write(self.markers.join(san), "").unwrap();
    }

    fn idle_count(&self) -> usize {
        count_lines(&self.idle_calls)
    }
    fn sender_count(&self) -> usize {
        count_lines(&self.sender_calls)
    }

    /// Run the orchestrator once. `inputs` = `;`-list of `<name>|<fps>`; the `*_heals` flags pick
    /// which arm heals. Returns (exit_code, stdout+stderr).
    fn run(&self, inputs: &str, idle_heals: bool, sender_heals: bool) -> (i32, String) {
        let out = Command::new("bash")
            .arg(&self.runner)
            .arg("10.77.9.202")
            .env(
                "NDI_CADENCE_FETCH_CMD",
                format!("bash {}", self.fetch.display()),
            )
            .env(
                "NDI_CADENCE_IDLE_CMD",
                format!("bash {}", self.idle.display()),
            )
            .env(
                "NDI_CADENCE_SENDER_CMD",
                format!("bash {}", self.sender.display()),
            )
            .env("NDI_CADENCE_INPUTS", inputs)
            .env("NDI_CADENCE_SETTLE_S", "0")
            .env("NDI_CADENCE_RUN_ID", "run-test")
            .env("NDI_CADENCE_RUN_DIR", &self.run_dir)
            .env("IDLE_HEALS", if idle_heals { "1" } else { "0" })
            .env("SENDER_HEALS", if sender_heals { "1" } else { "0" })
            .current_dir(manifest_dir())
            .output()
            .expect("failed to run ndi-cadence-heal orchestrator");
        let code = out.status.code().unwrap_or(-1);
        (
            code,
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn telemetry(&self) -> String {
        fs::read_to_string(self.run_dir.join("ndi-cadence-run-test.json")).unwrap_or_default()
    }
}

fn count_lines(p: &Path) -> usize {
    fs::read_to_string(p)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

const CAMS: &str = "NDI cam1|60";

// ---------------------------------------------------------------------------------------------
// (a) all inputs already at 60 fps -> no arm fires, exit 0, no HALVED line.
// ---------------------------------------------------------------------------------------------
#[test]
fn all_healthy_reads_trigger_no_action() {
    let rig = Rig::new();
    rig.pre_heal("NDI cam1"); // marker present -> fetch emits HEALTHY
    let (code, out) = rig.run(CAMS, false, false);
    assert_eq!(code, 0, "verify_and_heal must ALWAYS exit 0: {out}");
    assert_eq!(rig.idle_count(), 0, "no heal on a healthy read: {out}");
    assert_eq!(rig.sender_count(), 0, "no heal on a healthy read: {out}");
    assert!(
        !out.contains("HALVED ->"),
        "no HALVED escalation logged: {out}"
    );
    // telemetry still written, with zero incidents.
    let t = rig.telemetry();
    assert!(
        t.contains("\"halved\": 0"),
        "telemetry must record 0 halved: {t}"
    );
}

// ---------------------------------------------------------------------------------------------
// (b) one halved input -> idle-restore heals it: idle called once, sender never, result healed.
// ---------------------------------------------------------------------------------------------
#[test]
fn one_halved_input_idle_restore_heals() {
    let rig = Rig::new();
    let (code, out) = rig.run(CAMS, /*idle_heals*/ true, /*sender_heals*/ false);
    assert_eq!(code, 0, "{out}");
    assert_eq!(rig.idle_count(), 1, "the idle-restore arm runs once: {out}");
    assert_eq!(
        rig.sender_count(),
        0,
        "the sender arm must NOT run once idle heals it: {out}"
    );
    assert!(
        out.contains("[cleanup] ndi-cadence: NDI cam1 HALVED -> idle-restore -> healed"),
        "must log the idle-restore heal: {out}"
    );
    let t = rig.telemetry();
    assert!(
        t.contains("\"arm\": \"idle-restore\"") && t.contains("\"result\": \"healed\""),
        "{t}"
    );
    assert!(t.contains("\"healed\": 1"), "{t}");
}

// ---------------------------------------------------------------------------------------------
// (c) still halved after idle-restore -> the sender-restart arm heals it (both arms recorded).
// ---------------------------------------------------------------------------------------------
#[test]
fn still_halved_escalates_to_sender_restart() {
    let rig = Rig::new();
    let (code, out) = rig.run(CAMS, /*idle_heals*/ false, /*sender_heals*/ true);
    assert_eq!(code, 0, "{out}");
    assert_eq!(rig.idle_count(), 1, "the idle arm is tried first: {out}");
    assert_eq!(rig.sender_count(), 1, "then the sender arm heals it: {out}");
    assert!(
        out.contains("HALVED -> idle-restore -> still-halved"),
        "the idle arm did not take -> logged still-halved: {out}"
    );
    assert!(
        out.contains("[cleanup] ndi-cadence: NDI cam1 HALVED -> sender-restart -> healed"),
        "the sender arm heals it: {out}"
    );
    let t = rig.telemetry();
    assert!(t.contains("\"arm\": \"sender-restart\""), "{t}");
    assert!(t.contains("\"healed\": 1"), "{t}");
}

// ---------------------------------------------------------------------------------------------
// (d) never heals -> both arms tried, then escalate; ALWAYS exit 0 (report-only, never fails a
//     caller under set -euo pipefail — the #1133 class).
// ---------------------------------------------------------------------------------------------
#[test]
fn never_heals_escalates_and_still_exits_zero() {
    let rig = Rig::new();
    let (code, out) = rig.run(CAMS, false, false);
    assert_eq!(code, 0, "escalation must STILL exit 0 (report-only): {out}");
    assert_eq!(rig.idle_count(), 1, "idle tried: {out}");
    assert_eq!(rig.sender_count(), 1, "sender tried: {out}");
    assert!(
        out.contains("[cleanup] ndi-cadence: NDI cam1 HALVED -> escalate"),
        "both arms exhausted -> escalate logged: {out}"
    );
    let t = rig.telemetry();
    assert!(
        t.contains("\"escalated\": 1"),
        "telemetry records the escalation: {t}"
    );
    assert!(t.contains("\"arm\": \"escalate\""), "{t}");
}

// ---------------------------------------------------------------------------------------------
// (e) bounded per-input: two halved inputs handled in the same pass (parallel), each its own arm.
// ---------------------------------------------------------------------------------------------
#[test]
fn multiple_inputs_each_get_their_own_escalation() {
    let rig = Rig::new();
    // cam1 pre-healed (reads HEALTHY), cam2 halved and idle-restore heals it.
    rig.pre_heal("NDI cam1");
    let (code, out) = rig.run("NDI cam1|60;NDI cam2|60", true, false);
    assert_eq!(code, 0, "{out}");
    assert_eq!(rig.idle_count(), 1, "only the halved cam2 is cured: {out}");
    assert!(
        out.contains("NDI cam2 HALVED -> idle-restore -> healed"),
        "{out}"
    );
    assert!(
        !out.contains("NDI cam1 HALVED"),
        "the healthy cam1 is never cured: {out}"
    );
    let t = rig.telemetry();
    assert!(
        t.contains("\"halved\": 1") && t.contains("\"healed\": 1"),
        "{t}"
    );
}

#[allow(dead_code)]
fn _rig_dir_kept_alive(r: &Rig) -> &Path {
    r.dir.path()
}

// ---------------------------------------------------------------------------------------------
// (f) #1203 item (b): the E2E cleanup() wires the orchestrator EXACTLY ONCE, AFTER the cambox
//     parallel-restore group and BEFORE the OBS program-scene teardown, pinning the run-scoped
//     telemetry (RUN_ID + OUTDIR) and targeting the strih host. This is the additive #675-pattern
//     call that hands the rig back with verified 60fps receivers — a static-anchor guard (the
//     orchestrator's own behavior is covered by (a)-(e) above; here we pin only the wiring).
// ---------------------------------------------------------------------------------------------
fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn recording_e2e_cleanup_wires_cadence_verify_once_after_parallel_restore() {
    let s = recording_e2e_text();

    // Single call site: the orchestrator is CALLED exactly once (the source line below names the
    // FILE `ndi-cadence-heal.sh`, never the function, so this count is the call alone).
    let calls: Vec<usize> = s
        .match_indices("ndi_cadence_verify_and_heal")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "recording-e2e.sh must call ndi_cadence_verify_and_heal EXACTLY once (found {}): {:?}",
        calls.len(),
        calls
    );
    let call = calls[0];

    // The lib is sourced exactly once. Anchor on the source STATEMENT, not the bare basename — the
    // sibling `. "$HERE/lib/..."` sources in this file each carry a `# shellcheck source=...`
    // directive too, so the basename appears twice per source; the `. "$HERE/..."` statement is the
    // unambiguous single occurrence.
    assert_eq!(
        s.matches(". \"$HERE/lib/ndi-cadence-heal.sh\"").count(),
        1,
        "the ndi-cadence-heal.sh lib must be sourced exactly once in recording-e2e.sh"
    );

    // AFTER the cambox parallel-restore/retry group — anchored on its terminal surface-failure step
    // (a single-occurrence literal in recording-e2e.sh).
    let group_end = s
        .find("cambox_parallel_surface_painter_failure")
        .expect("the cambox parallel-restore group's surface-painter-failure step must exist");
    assert!(
        group_end < call,
        "the cadence verify must be placed AFTER the cambox parallel-restore group \
         (surface-painter-failure @ {group_end}, call @ {call})"
    );

    // BEFORE the OBS program-scene teardown region begins.
    let teardown = s
        .find("restore OBS program scenes")
        .expect("the OBS program-scene teardown banner must exist");
    assert!(
        call < teardown,
        "the cadence verify must run BEFORE the OBS program-scene teardown \
         (call @ {call}, teardown @ {teardown})"
    );

    // The call pins the run-scoped telemetry so ndi-cadence-<RUN_ID>.json lands in THIS run's dir,
    // and it targets the strih host.
    let pre = &s[call.saturating_sub(140)..call];
    assert!(
        pre.contains("NDI_CADENCE_RUN_ID=\"$RUN_ID\"")
            && pre.contains("NDI_CADENCE_RUN_DIR=\"$OUTDIR\""),
        "the call must pin NDI_CADENCE_RUN_ID/RUN_DIR so the telemetry lands in this run's dir: \
         {pre:?}"
    );
    let post = &s[call..(call + 60).min(s.len())];
    assert!(
        post.contains("\"$STRIH\""),
        "the cadence verify must target the strih host ($STRIH): {post:?}"
    );
}
