//! #1069 — dynamic cambox-input enumeration for the dev1-side FROZEN-INPUT alert watchdog, pointed
//! at STRIH (the camera branch), reusing the #1052 decision core.
//!
//! Root cause (issue 1069, surfaced by the issue-935 forensics + issue-1096 live incidents): strih's
//! DistroAV receiver can WEDGE a cambox input — `genlock-fifo audit 'NDI camN': received=` frozen
//! while the line KEEPS printing — and strih keeps compositing the frozen frame into program. No
//! dev1 watchdog reads strih's per-cambox counters: the #1052 watchdog watches the STREAM box + only
//! `NDI 2ME PGM`, using a STATIC source list the ticket forbids for the changing cambox set.
//!
//! This file pins TWO new pieces (RED before they exist, GREEN after):
//!   A. the PURE `frozen_input_cambox_sources` enumeration filter in scripts/lib/frozen-input-health.sh
//!      (dynamic: derive the watched cambox set from live strih OBS-log reality, exclude the program /
//!      preview feeds — never a static cam-number list);
//!   B. the ENUMERATION MODE (`FROZEN_INPUT_ENUMERATE=1`) of scripts/frozen-input-alert-watchdog.sh —
//!      enumerate the cambox sources each pass, then feed the UNCHANGED per-source classify/confirm/
//!      throttle path; and the fail-loud enum-blind WARN (a failed enumeration is never a silent green).
//!
//! Same convention as tests/harness_frozen_input_health_1052.rs: source/run the REAL shell with
//! ALL I/O stubbed on PATH / via the *_CMD env overrides, so the decision logic is exercised with no
//! live rig. Tier-0: compile with `--no-run` then run the compiled binary directly.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/frozen-input-health.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn watchdog() -> PathBuf {
    let s = manifest_dir().join("scripts/frozen-input-alert-watchdog.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL lib and run `body`; returns (exit, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}", body = body);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stdout_of(body: &str) -> String {
    let (rc, out, err) = run_sourced(body);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out.trim().to_string()
}

// A realistic strih OBS-log tail (the exact shape read live 2026-08-17): four cambox camera inputs
// plus the program + preview feeds, each printed many times.
const STRIH_LOG: &str = "\
15:22:17.263: genlock-fifo audit 'NDI cam3': received=43240 consumed=21623 locked=1\n\
15:22:17.263: genlock-fifo audit 'NDI cam2': received=43208 consumed=21623 locked=1\n\
15:22:17.263: genlock-fifo audit 'NDI cam4': received=43245 consumed=21622 locked=1\n\
15:22:17.263: genlock-fifo audit 'NDI 2ME PGM (mv)': received=21623 consumed=21621 locked=1\n\
15:22:22.063: genlock-fifo audit 'NDI 2ME PVW': received=21772 consumed=21772 locked=1\n\
15:22:22.263: genlock-fifo audit 'NDI cam1': received=43005 consumed=21773 locked=1\n\
15:22:22.263: genlock-fifo audit 'NDI cam3': received=43540 consumed=21773 locked=1\n\
15:22:22.263: genlock-fifo audit 'NDI cam2': received=43508 consumed=21773 locked=1\n\
15:22:22.263: genlock-fifo audit 'NDI cam4': received=43545 consumed=21772 locked=1\n\
15:22:22.263: genlock-fifo audit 'NDI cam1': received=43300 consumed=21923 locked=1\n";

// ---------------------------------------------------------------------------------------------
// A. frozen_input_cambox_sources — the pure enumeration filter
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_enumeration_filter() {
    let out = stdout_of("type frozen_input_cambox_sources >/dev/null 2>&1 && echo DEFINED");
    assert_eq!(
        out, "DEFINED",
        "frozen_input_cambox_sources is not defined by the lib"
    );
}

/// Feed the realistic log on stdin; expect the 4 cambox camera inputs, deduped, in first-seen order,
/// and NOT the program/preview feeds.
fn enumerate(raw: &str) -> Vec<String> {
    // The raw text is passed via the RAW env var (below) so newlines survive; printf it on stdin.
    let body = "printf '%s' \"$RAW\" | frozen_input_cambox_sources";
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .env("RAW", raw)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run enumerate harness");
    assert!(
        out.status.success(),
        "enumerate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn enumeration_returns_the_cambox_camera_inputs_only() {
    let got = enumerate(STRIH_LOG);
    assert_eq!(
        got,
        vec!["NDI cam3", "NDI cam2", "NDI cam4", "NDI cam1"],
        "must enumerate exactly the 4 cambox inputs, deduped, in first-seen order: {got:?}"
    );
}

#[test]
fn enumeration_excludes_program_and_preview_feeds() {
    let got = enumerate(STRIH_LOG);
    for feed in ["NDI 2ME PGM (mv)", "NDI 2ME PVW"] {
        assert!(
            !got.iter().any(|s| s == feed),
            "the {feed} program/preview feed must NOT be a watched cambox input: {got:?}"
        );
    }
}

#[test]
fn enumeration_of_empty_log_is_empty() {
    assert!(enumerate("").is_empty(), "empty log => no watched sources");
    // a log with only the program feed => nothing to watch
    assert!(
        enumerate("00:00:00.000: genlock-fifo audit 'NDI 2ME PGM (mv)': received=5 locked=1\n")
            .is_empty(),
        "a log with only non-cambox feeds enumerates to empty"
    );
}

// ---------------------------------------------------------------------------------------------
// B. the watchdog's ENUMERATION MODE, end-to-end with stubbed I/O
// ---------------------------------------------------------------------------------------------

/// Write an executable stub file and return its path.
fn write_stub(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, body).unwrap();
    let mut perms = fs::metadata(&p).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&p, perms).unwrap();
    p
}

/// Run one watchdog pass in enumeration mode against strih, with all I/O stubbed. Returns the
/// accumulated notify-stub log content after the pass.
struct EnumRig {
    dir: PathBuf,
    enumerate_cmd: PathBuf,
    probe_cmd: PathBuf,
    notify: PathBuf,
    state_dir: PathBuf,
    notify_log: PathBuf,
}

impl EnumRig {
    fn new(tag: &str, enumerate_body: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("cbox-fi-enum-1069-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let state_dir = base.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let notify_log = base.join("notify.log");
        fs::write(&notify_log, "").unwrap();

        // Enumeration stub: prints the raw strih OBS-log tail (arg1 = ip, ignored).
        let enumerate_cmd = write_stub(&base, "enumerate.sh", enumerate_body);

        // Per-source received= stub: prints a `genlock-fifo audit '<src>': received=N` line. cam2 is
        // FROZEN (constant), every other cam ADVANCES (grows each call, tracked in a per-source file).
        let probe_body = r#"#!/usr/bin/env bash
src="$2"
key=$(printf '%s' "$src" | tr -c 'A-Za-z0-9' '_')
cf="$STUB_STATE/cnt_$key"
n=$(cat "$cf" 2>/dev/null || echo 0); n=$((n+1)); printf '%s' "$n" > "$cf"
case "$src" in
  *cam2*) rc=5000 ;;
  *) rc=$((5000 + n)) ;;
esac
printf "12:00:00.000: genlock-fifo audit '%s': received=%s consumed=1 locked=1\n" "$src" "$rc"
"#;
        let probe_cmd = write_stub(&base, "probe.sh", probe_body);

        // Notify stub: a python "airuleset.py" that records its argv to STUB_NOTIFY_LOG.
        let notify_body = r#"#!/usr/bin/env python3
import sys, os
with open(os.environ["STUB_NOTIFY_LOG"], "a") as f:
    f.write("\n---NOTIFY---\n" + "\n".join(sys.argv[1:]) + "\n")
"#;
        let notify = write_stub(&base, "notify.py", notify_body);

        EnumRig {
            dir: base,
            enumerate_cmd,
            probe_cmd,
            notify,
            state_dir,
            notify_log,
        }
    }

    fn run_pass(&self, confirm_threshold: &str, enum_blind_threshold: &str) {
        let out = Command::new("bash")
            .arg(watchdog())
            .env("FROZEN_INPUT_ENUMERATE", "1")
            .env("FROZEN_INPUT_RECEIVER", "strih|10.77.9.202")
            .env("FROZEN_INPUT_SENDER", "strih")
            .env("FROZEN_INPUT_ALERT_TAG", "#1069")
            .env("FROZEN_INPUT_ENUMERATE_CMD", &self.enumerate_cmd)
            .env("FROZEN_INPUT_PROBE_CMD", &self.probe_cmd)
            .env("AIRULESET_NOTIFY", &self.notify)
            .env("STUB_NOTIFY_LOG", &self.notify_log)
            .env("STUB_STATE", &self.state_dir)
            .env("FROZEN_INPUT_ALERT_STATE_DIR", &self.state_dir)
            .env(
                "FROZEN_INPUT_ALERT_STATE_FILE",
                self.state_dir.join("strih.state"),
            )
            .env(
                "FROZEN_INPUT_NETREACH_STATE_FILE",
                self.state_dir.join("does-not-exist.state"),
            )
            .env("FROZEN_INPUT_ALERT_CONFIRM_THRESHOLD", confirm_threshold)
            .env("FROZEN_INPUT_ENUM_BLIND_THRESHOLD", enum_blind_threshold)
            .current_dir(manifest_dir())
            .output()
            .expect("failed to run watchdog pass");
        // The watchdog must never crash on a pass (set -uo pipefail, best-effort).
        assert!(
            out.status.success(),
            "watchdog pass exited non-zero: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn notify_log(&self) -> String {
        fs::read_to_string(&self.notify_log).unwrap_or_default()
    }
}

impl Drop for EnumRig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn enumeration_mode_pages_a_frozen_cambox_input_and_not_the_advancing_ones() {
    let rig = EnumRig::new(
        "frozen",
        &format!("#!/usr/bin/env bash\ncat <<'LOG'\n{STRIH_LOG}LOG\n"),
    );
    // Pass 1 seeds every cambox source (no prior sample -> UNKNOWN, no page).
    rig.run_pass("1", "24");
    assert_eq!(
        rig.notify_log().trim(),
        "",
        "pass 1 seeds only — nothing paged yet"
    );
    // Pass 2: cam2 counter held (FROZEN), the others advanced. Confirm threshold 1 -> page cam2.
    rig.run_pass("1", "24");
    let log = rig.notify_log();
    assert!(
        log.contains("#1069") && log.contains("NDI cam2") && log.to_lowercase().contains("frozen"),
        "must page a #1069 frozen alert naming NDI cam2: {log}"
    );
    // The advancing cameras + the program/preview feeds must NEVER appear in a frozen alert.
    for other in ["NDI cam1", "NDI cam3", "NDI cam4", "2ME PGM", "2ME PVW"] {
        assert!(
            !log.contains(&format!("{other}: received")),
            "an advancing / non-cambox source must not be paged as frozen: {other} in {log}"
        );
    }
}

#[test]
fn enumeration_blind_fires_a_fail_loud_warn_never_a_silent_green() {
    // The enumeration read returns NOTHING (a broken tap / read failure). The watchdog must NOT sit
    // silently green — after the enum-blind threshold it fires one fail-loud WARN.
    let rig = EnumRig::new("blind", "#!/usr/bin/env bash\ntrue\n"); // prints nothing
    rig.run_pass("2", "1"); // threshold 1 -> WARN on the first blind pass
    let log = rig.notify_log();
    assert!(
        log.contains("#1069") && log.to_lowercase().contains("enumerat"),
        "an empty enumeration must fire a fail-loud enumeration-blind WARN: {log}"
    );
}
