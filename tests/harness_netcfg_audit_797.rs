//! #797 — pure-function guard for `scripts/lib/netcfg-audit.sh`, the SHARED decision core for the
//! dev1-side venue-switch config-drift audit (the `netcfg` facet).
//!
//! What this ticket became (see the ticket's own retraction history): the "OBS caps at 50 fps"
//! receive bug was a measurement artifact, and the one real transport defect — foh2 10G-trunk→edge
//! microburst egress tail-drop — was already fixed by raising `shared-buffers` 40%→80%. What remains
//! is that the venue MikroTik switch chain has NO durable, machine-checkable record of its healthy
//! state: the KEPT `shared-buffers=80%` fix has no guard against a silent revert, and per-port drop
//! counters / link rates / roles are watched by nobody until someone hand-ssh-es in mid-incident.
//! This facet captures a checked-in baseline of the 4 CRS310s + the RB4011 and REPORTS drift.
//!
//! This file pins the PURE parse + drift-classification the `netcfg-audit.sh` orchestrator and the
//! `netcfg-drift-alert-watchdog.sh` consume, so it is correct regardless of any live rig.
//!
//! Same convention as `tests/harness_asio_starve_health_1023.rs` / `tests/harness_cadence_health_794.rs`:
//! source the REAL lib (source-only, no side effects) and exercise the pure functions directly.
//! RED before the lib exists (sourcing fails, every test fails); GREEN after. Fixtures are the exact
//! live RouterOS 7.23.3 output shapes captured read-only 2026-08-17 from foh2_video_switch.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/netcfg-audit.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL lib and run `body` against its pure functions. Returns (exit, stdout, stderr).
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

// The exact live `/interface ethernet monitor <port> once` shape (single-column) for a 2.5G edge port.
const MONITOR_FIX: &str = r#"MON=$(cat <<'FIX'
                      name: ether3
                    status: link-ok
                      rate: 2.5Gbps
               full-duplex: yes
FIX
)
"#;

// The exact live `/interface ethernet print stats where name=ether2` drop/error tail (single-column;
// RouterOS prints thousands with an embedded space, e.g. `100 054`).
const STATS_FIX: &str = r#"ST=$(cat <<'FIX'
                     ;;;  dante
                   name: ether2
           rx-fcs-error:      1
         tx-drop-packet:      100 054
  tx-drop-queue1-packet:      100 054
  tx-drop-queue2-packet:      0
FIX
)
"#;

// The exact live `/interface ethernet switch qos settings print` block (the KEPT fix lives here).
const QOS_FIX: &str = r#"QOS=$(cat <<'FIX'
  multicast-buffers: 10%
     mirror-buffers: 10%
     mirror-profile: default
     shared-buffers: 80%
FIX
)
"#;

// ------------------------------------------------------------------------------------------------
// lib shape — the pure functions must be defined
// ------------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "netcfg_parse_field",
        "netcfg_parse_stat",
        "netcfg_normalize_rate",
        "netcfg_normalize_version",
        "netcfg_classify_match",
        "netcfg_classify_rate",
        "netcfg_classify_drop_rate",
        "netcfg_drift_verdict",
        "netcfg_port_is_designated",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ------------------------------------------------------------------------------------------------
// netcfg_parse_field — extract the value after `<label>:` from a single-column RouterOS block
// ------------------------------------------------------------------------------------------------
#[test]
fn parse_field_reads_monitor_rate_and_status() {
    assert_eq!(
        stdout_of(&format!("{MONITOR_FIX}netcfg_parse_field rate \"$MON\"")),
        "2.5Gbps"
    );
    assert_eq!(
        stdout_of(&format!("{MONITOR_FIX}netcfg_parse_field status \"$MON\"")),
        "link-ok"
    );
    assert_eq!(
        stdout_of(&format!(
            "{MONITOR_FIX}netcfg_parse_field full-duplex \"$MON\""
        )),
        "yes"
    );
    // the port name in the monitor block
    assert_eq!(
        stdout_of(&format!("{MONITOR_FIX}netcfg_parse_field name \"$MON\"")),
        "ether3"
    );
}

#[test]
fn parse_field_reads_shared_buffers_and_is_start_anchored() {
    assert_eq!(
        stdout_of(&format!(
            "{QOS_FIX}netcfg_parse_field shared-buffers \"$QOS\""
        )),
        "80%"
    );
    // `shared-buffers:` must NOT be matched by a request for the `buffers` label appearing mid-word,
    // and `mirror-buffers:` must NOT satisfy a `shared-buffers` query (start-anchored label).
    assert_eq!(
        stdout_of(&format!(
            "{QOS_FIX}netcfg_parse_field mirror-buffers \"$QOS\""
        )),
        "10%"
    );
}

#[test]
fn parse_field_empty_when_absent() {
    assert_eq!(
        stdout_of(&format!(
            "{MONITOR_FIX}netcfg_parse_field nonesuch \"$MON\""
        )),
        ""
    );
}

// ------------------------------------------------------------------------------------------------
// netcfg_parse_stat — a named counter integer, thousands-space stripped
// ------------------------------------------------------------------------------------------------
#[test]
fn parse_stat_strips_thousands_space() {
    assert_eq!(
        stdout_of(&format!(
            "{STATS_FIX}netcfg_parse_stat tx-drop-queue1-packet \"$ST\""
        )),
        "100054"
    );
    assert_eq!(
        stdout_of(&format!(
            "{STATS_FIX}netcfg_parse_stat rx-fcs-error \"$ST\""
        )),
        "1"
    );
    // queue2 is a genuine 0 (present); a distinct counter name never bleeds into a similarly-named one
    assert_eq!(
        stdout_of(&format!(
            "{STATS_FIX}netcfg_parse_stat tx-drop-queue2-packet \"$ST\""
        )),
        "0"
    );
}

#[test]
fn parse_stat_empty_when_absent() {
    assert_eq!(
        stdout_of(&format!("{STATS_FIX}netcfg_parse_stat rx-jabber \"$ST\"")),
        ""
    );
}

// ------------------------------------------------------------------------------------------------
// netcfg_normalize_rate — RouterOS rate string -> comparable Mbps integer
// ------------------------------------------------------------------------------------------------
#[test]
fn normalize_rate_maps_all_known_speeds() {
    assert_eq!(stdout_of("netcfg_normalize_rate 10Gbps"), "10000");
    assert_eq!(stdout_of("netcfg_normalize_rate 2.5Gbps"), "2500");
    assert_eq!(stdout_of("netcfg_normalize_rate 1Gbps"), "1000");
    assert_eq!(stdout_of("netcfg_normalize_rate 100Mbps"), "100");
    assert_eq!(stdout_of("netcfg_normalize_rate 10Mbps"), "10");
}

#[test]
fn normalize_rate_empty_on_junk() {
    assert_eq!(stdout_of("netcfg_normalize_rate ''"), "");
    assert_eq!(stdout_of("netcfg_normalize_rate link-down"), "");
}

#[test]
fn normalize_version_strips_channel() {
    assert_eq!(
        stdout_of("netcfg_normalize_version '7.23.3 (stable)'"),
        "7.23.3"
    );
    assert_eq!(stdout_of("netcfg_normalize_version 7.22"), "7.22");
}

// ------------------------------------------------------------------------------------------------
// netcfg_classify_match — exact-match drift for stable config fields
// ------------------------------------------------------------------------------------------------
#[test]
fn classify_match_verdicts() {
    assert_eq!(stdout_of("netcfg_classify_match 80% 80%"), "OK");
    assert_eq!(stdout_of("netcfg_classify_match 40% 80%"), "DRIFT"); // the exact silent-revert case
    assert_eq!(stdout_of("netcfg_classify_match '' 80%"), "ABSENT");
    assert_eq!(stdout_of("netcfg_classify_match 80% ''"), "UNSET");
}

// ------------------------------------------------------------------------------------------------
// netcfg_classify_rate — link-speed drift (a baselined port negotiating slower = DEGRADED)
// ------------------------------------------------------------------------------------------------
#[test]
fn classify_rate_verdicts() {
    assert_eq!(stdout_of("netcfg_classify_rate 10Gbps 10Gbps"), "OK");
    assert_eq!(stdout_of("netcfg_classify_rate 1Gbps 10Gbps"), "DEGRADED"); // duplex/speed regression
    assert_eq!(stdout_of("netcfg_classify_rate 2.5Gbps 1Gbps"), "FASTER"); // better than baseline, info
    assert_eq!(stdout_of("netcfg_classify_rate '' 10Gbps"), "ABSENT"); // link down / not present
    assert_eq!(stdout_of("netcfg_classify_rate 1Gbps ''"), "UNSET");
}

// ------------------------------------------------------------------------------------------------
// netcfg_classify_drop_rate — measured drop DELTA over a window vs a per-second threshold
// ------------------------------------------------------------------------------------------------
#[test]
fn classify_drop_rate_verdicts() {
    // 300 drops over 6 s = 50/s, over a 1/s threshold -> DROPPING (the incident's microburst signature)
    assert_eq!(stdout_of("netcfg_classify_drop_rate 300 6 1"), "DROPPING");
    // 0 drops over 6 s -> OK (the post-fix healthy state)
    assert_eq!(stdout_of("netcfg_classify_drop_rate 0 6 1"), "OK");
    // exactly at threshold is OK (rate must EXCEED it)
    assert_eq!(stdout_of("netcfg_classify_drop_rate 6 6 1"), "OK");
    // a negative delta = counters reset (reboot) since the window's first read -> report-only RESET
    assert_eq!(stdout_of("netcfg_classify_drop_rate -5 6 1"), "RESET");
    // a zero/garbage window never divides-by-zero -> UNKNOWN
    assert_eq!(stdout_of("netcfg_classify_drop_rate 300 0 1"), "UNKNOWN");
    assert_eq!(stdout_of("netcfg_classify_drop_rate x 6 1"), "UNKNOWN");
    // a non-numeric THRESHOLD must fall back to the safe 1/s default (deterministic), NOT be passed
    // raw to awk (where `rate > "bad"` is a fragile string comparison that silently MISSES real
    // drops). 12/6=2/s EXCEEDS the default 1/s -> DROPPING; without the fallback awk string-compares
    // "2" > "bad" -> false -> a missed drop storm. This case discriminates the fix.
    assert_eq!(stdout_of("netcfg_classify_drop_rate 12 6 bad"), "DROPPING");
    // and a genuinely low rate under the default is still OK
    assert_eq!(stdout_of("netcfg_classify_drop_rate 3 6 bad"), "OK");
    // a valid FLOAT threshold still works
    assert_eq!(stdout_of("netcfg_classify_drop_rate 3 6 0.4"), "DROPPING");
}

// ------------------------------------------------------------------------------------------------
// netcfg_drift_verdict — aggregate a set of per-field statuses into CLEAN | DRIFT
// ------------------------------------------------------------------------------------------------
#[test]
fn drift_verdict_aggregates() {
    assert_eq!(
        stdout_of("netcfg_drift_verdict OK OK FASTER UNSET"),
        "CLEAN"
    );
    // report-only statuses do not page
    assert_eq!(
        stdout_of("netcfg_drift_verdict OK ABSENT RESET UNKNOWN"),
        "CLEAN"
    );
    // any hard-drift status pages
    assert_eq!(stdout_of("netcfg_drift_verdict OK DRIFT OK"), "DRIFT");
    assert_eq!(stdout_of("netcfg_drift_verdict OK OK DEGRADED"), "DRIFT");
    assert_eq!(stdout_of("netcfg_drift_verdict OK DROPPING"), "DRIFT");
    // no args -> CLEAN (nothing to judge)
    assert_eq!(stdout_of("netcfg_drift_verdict"), "CLEAN");
}

// ------------------------------------------------------------------------------------------------
// netcfg_port_is_designated — the drop-sampler ALWAYS-probe set (#1110). A `node|port` in the
// space-separated designated list is live-sampled every `--check` regardless of cumulative-counter
// growth, so foh2_video's egress toward strih (the direct-DAC uplink `sfp-sfpplus2`) always yields
// a fresh drop DELTA for the next starvation episode. Exit 0 = designated, exit 1 = plain.
// ------------------------------------------------------------------------------------------------

/// Run `body` (which itself prints `RC=$?`) against the sourced lib and return the trimmed stdout,
/// so a non-zero PREDICATE return is observable (unlike `stdout_of`, which asserts the body rc==0).
fn rc_line(body: &str) -> String {
    run_sourced(body).1.trim().to_string()
}

#[test]
fn port_is_designated_matches_only_exact_node_port_tokens() {
    let list = "foh2_video|sfp-sfpplus2";
    // the strih uplink (foh2_video egress toward strih) IS designated -> exit 0
    assert_eq!(
        rc_line(&format!(
            "netcfg_port_is_designated foh2_video sfp-sfpplus2 '{list}'; echo RC=$?"
        )),
        "RC=0"
    );
    // a DIFFERENT port on the SAME switch is not designated -> exit 1
    assert_eq!(
        rc_line(&format!(
            "netcfg_port_is_designated foh2_video sfp-sfpplus1 '{list}'; echo RC=$?"
        )),
        "RC=1"
    );
    // the SAME port name on a DIFFERENT switch is not designated (match is exact node|port) -> exit 1
    assert_eq!(
        rc_line(&format!(
            "netcfg_port_is_designated foh1_video sfp-sfpplus2 '{list}'; echo RC=$?"
        )),
        "RC=1"
    );
}

#[test]
fn port_is_designated_handles_multi_token_and_empty() {
    // a multi-token list matches ANY of its exact node|port pairs
    let two = "foh2_video|sfp-sfpplus2 foh1_video|sfp-sfpplus1";
    assert_eq!(
        rc_line(&format!(
            "netcfg_port_is_designated foh2_video sfp-sfpplus2 '{two}'; echo RC=$?"
        )),
        "RC=0"
    );
    assert_eq!(
        rc_line(&format!(
            "netcfg_port_is_designated foh1_video sfp-sfpplus1 '{two}'; echo RC=$?"
        )),
        "RC=0"
    );
    // an empty designated list never matches -> exit 1 (the default-off case)
    assert_eq!(
        rc_line("netcfg_port_is_designated foh2_video sfp-sfpplus2 ''; echo RC=$?"),
        "RC=1"
    );
    // an empty node or port never matches -> exit 1
    assert_eq!(
        rc_line("netcfg_port_is_designated '' sfp-sfpplus2 'foh2_video|sfp-sfpplus2'; echo RC=$?"),
        "RC=1"
    );
    assert_eq!(
        rc_line("netcfg_port_is_designated foh2_video '' 'foh2_video|sfp-sfpplus2'; echo RC=$?"),
        "RC=1"
    );
}
