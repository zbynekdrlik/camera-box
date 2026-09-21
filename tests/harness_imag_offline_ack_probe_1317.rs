//! issue 1317 — the imag offline-ack STALE-ACK decision must use a SERVICE-level reachability
//! probe, never a bare ICMP ping. imag-nb is OUT of the rig (~1 year, owner 20.9.2026 — its USB
//! ethernet dongle now carries strih-lx at .202). On the venue LAN 10.77.9.182 still answers ICMP
//! (a router / proxy-ARP / foreign responder — `ip neigh` INCOMPLETE, ssh:22 refused, dantesync
//! silent), so the old `ping -c1 -W2 "$IMAG_IP"` staleness probe reads the acked-absent box as
//! REACHABLE and rejects the legitimate ack as STALE, all-UNKNOWN drift-guard, and the issue-789
//! gate HARD-BLOCKS every `rig-mode.sh test` and E2E `[0/8]`.
//!
//! The fix: one shared predicate `imag_service_reachable HOST` in `scripts/lib/imag-offline-ack.sh`
//! — reachable iff a TCP connect to ssh :22 succeeds OR dantesync `:8898/status` answers; NEVER a
//! bare ping. Both `scripts/rig-mode.sh` sites and the `scripts/recording-e2e.sh` imag stale-ack
//! mirror call it. Tier-0 seams `IMAG_REACH_SSH_PROBE_CMD` / `IMAG_REACH_HTTP_PROBE_CMD` (a fixture
//! command whose EXIT CODE stands in for the probe) make this hermetically testable with no rig.
//!
//! These tests source the REAL lib / read the REAL scripts — no OBS/ssh/live rig — mirroring
//! `tests/harness_imag_offline_ack_1013.rs` + `tests/harness_rig_mode_imag_offline_leg_1171.rs`.

use std::fs;
use std::process::Command;

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Source `scripts/lib/imag-offline-ack.sh` (cwd = crate root) and call `imag_service_reachable`
/// with the given env; return true iff the predicate exits 0. The two probe seams stand in for the
/// real ssh/http probes so this is hermetic (no network).
fn service_reachable(host: &str, env: &[(&str, &str)]) -> bool {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(". scripts/lib/imag-offline-ack.sh; imag_service_reachable \"$1\"")
        .arg("bash") // $0
        .arg(host) // $1
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run bash").status.success()
}

// ---------------------------------------------------------------------------
// 1. imag_service_reachable — the pure service-probe predicate (seam-driven).
// ---------------------------------------------------------------------------

#[test]
fn service_reachable_is_defined_and_never_uses_ping() {
    let lib = read("scripts/lib/imag-offline-ack.sh");
    assert!(
        lib.contains("imag_service_reachable()"),
        "1317: imag-offline-ack.sh must define imag_service_reachable"
    );
    // The whole point: the predicate must NOT be a bare ICMP ping (a venue router/proxy-ARP fools it).
    let def = lib
        .find("imag_service_reachable()")
        .expect("imag_service_reachable must be defined");
    let body_end = lib[def..]
        .find("\n}\n")
        .map(|i| def + i)
        .unwrap_or(lib.len());
    let body = &lib[def..body_end];
    assert!(
        !body.contains("ping "),
        "1317: imag_service_reachable must NOT use a bare ICMP ping. Got:\n{body}"
    );
    // It must probe the two SERVICES (ssh :22 + dantesync :8898) and expose both Tier-0 seams.
    assert!(
        body.contains("/dev/tcp/") && body.contains(":22"),
        "1317: imag_service_reachable must probe ssh :22 via a TCP connect. Got:\n{body}"
    );
    assert!(
        body.contains(":8898"),
        "1317: imag_service_reachable must probe dantesync :8898. Got:\n{body}"
    );
    assert!(
        body.contains("IMAG_REACH_SSH_PROBE_CMD") && body.contains("IMAG_REACH_HTTP_PROBE_CMD"),
        "1317: imag_service_reachable must expose both Tier-0 probe seams. Got:\n{body}"
    );
}

#[test]
fn service_reachable_is_unreachable_when_both_probes_fail() {
    // The exact issue-1317 case: a box that answers ICMP but whose ssh AND dantesync are both down.
    assert!(
        !service_reachable(
            "10.77.9.182",
            &[
                ("IMAG_REACH_SSH_PROBE_CMD", "false"),
                ("IMAG_REACH_HTTP_PROBE_CMD", "false"),
            ],
        ),
        "1317: both services down -> NOT reachable (so a legit ack is NOT rejected as stale)"
    );
}

#[test]
fn service_reachable_when_ssh_answers() {
    assert!(
        service_reachable(
            "10.77.9.182",
            &[
                ("IMAG_REACH_SSH_PROBE_CMD", "true"),
                ("IMAG_REACH_HTTP_PROBE_CMD", "false"),
            ],
        ),
        "1317: ssh :22 answering -> reachable (a genuinely-back box makes its ack STALE)"
    );
}

#[test]
fn service_reachable_when_only_http_answers() {
    assert!(
        service_reachable(
            "10.77.9.182",
            &[
                ("IMAG_REACH_SSH_PROBE_CMD", "false"),
                ("IMAG_REACH_HTTP_PROBE_CMD", "true"),
            ],
        ),
        "1317: dantesync :8898 answering alone -> reachable"
    );
}

#[test]
fn service_reachable_is_false_for_an_empty_host() {
    assert!(
        !service_reachable("", &[]),
        "1317: an empty host must never read as reachable"
    );
}

// ---------------------------------------------------------------------------
// 2. rig-mode.sh — BOTH stale-ack sites use the service probe, not ping.
// ---------------------------------------------------------------------------

#[test]
fn rig_mode_sources_the_imag_offline_ack_lib() {
    let s = read("scripts/rig-mode.sh");
    assert!(
        s.contains("lib/imag-offline-ack.sh"),
        "1317: rig-mode.sh must source scripts/lib/imag-offline-ack.sh for imag_service_reachable"
    );
}

#[test]
fn rig_mode_stale_ack_sites_use_the_service_probe_not_ping() {
    let s = read("scripts/rig-mode.sh");
    // Negative: the ICMP ping stale-ack probe is gone from BOTH former sites.
    assert!(
        !s.contains("ping -c1 -W2 \"$IMAG_IP\""),
        "1317: rig-mode.sh must no longer probe imag staleness with a bare ping"
    );
    // Positive: both former sites (resolve_imag_offline_leg + require_imag_genlock_current) now call
    // the shared service predicate. There are exactly two such sites.
    let n = s.matches("imag_service_reachable \"$IMAG_IP\"").count();
    assert_eq!(
        n, 2,
        "1317: both rig-mode.sh stale-ack sites must call imag_service_reachable \"$IMAG_IP\" \
         (found {n})"
    );
}

// ---------------------------------------------------------------------------
// 3. recording-e2e.sh — the imag [0/8] stale-ack mirror uses the service probe.
// ---------------------------------------------------------------------------

#[test]
fn recording_e2e_imag_stale_ack_block_uses_the_service_probe_not_ping() {
    let s = read("scripts/recording-e2e.sh");
    // Slice the imag-acked branch of the [0/8] reachability loop (from its guard to the `continue`).
    let start = s
        .find("cambox_offline_ack_is_acked \"imag\"; then")
        .expect("1317: the imag-acked reachability branch must exist");
    let end = start
        + s[start..]
            .find("\n    continue")
            .expect("1317: the imag-acked branch must end with `continue`");
    let block = &s[start..end];
    assert!(
        block.contains("imag_service_reachable \"$_ip\""),
        "1317: the imag stale-ack decision must use imag_service_reachable, not ping. Got:\n{block}"
    );
    assert!(
        !block.contains("ping"),
        "1317: no bare ping may remain in the imag stale-ack decision block. Got:\n{block}"
    );
    // The stale-ack contract itself is unchanged: a genuinely-back imag still fails loud.
    assert!(
        block.contains("cambox_offline_ack_stale_message"),
        "1317: an acked-but-REACHABLE imag must still fail as a stale ack. Got:\n{block}"
    );
}

// ---------------------------------------------------------------------------
// 4. rig-fleet.txt — imag is acked offline with the owner's 20.9.2026 reason.
// ---------------------------------------------------------------------------

#[test]
fn rig_fleet_acks_imag_offline_with_a_reason() {
    // The checked-in default-ack file the harness falls back to. Read it through the SAME parser the
    // gates use (cambox_offline_ack_effective -> cambox_offline_ack_reason), so this proves the ack
    // is actually parseable, not just present as text.
    let out = Command::new("bash")
        .arg("-c")
        .arg(
            ". scripts/lib/cambox-offline-ack.sh; \
             eff=\"$(cambox_offline_ack_effective \"\" rig-fleet.txt)\"; \
             CAMBOX_OFFLINE_ACK=\"$eff\" cambox_offline_ack_reason imag",
        )
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run bash");
    let reason = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        !reason.is_empty() && reason != "unspecified",
        "1317: rig-fleet.txt must ack `imag` offline with a real reason (owner 20.9.2026). Got: {reason:?}"
    );
    assert!(
        reason.contains("2026"),
        "1317: the imag ack reason must carry the owner-decision date. Got: {reason:?}"
    );
}
