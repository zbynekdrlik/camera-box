//! #828 — the `camera-box.service` unit must cap ANY runaway restart storm.
//!
//! With `Restart=always` + `RestartSec=3` and NO `StartLimit*`, a box that exits repeatedly (the
//! original no-device `bail!`, or any other crash loop) restarts every ~3 s forever (cam4:
//! `NRestarts=27719`). The binary now settles the no-device case in-process (`no_device`), but the
//! unit must ALSO carry a generous `StartLimit*` as a general backstop against any other runaway
//! — while keeping the fast `RestartSec=3` recovery for a genuine mid-stream transient.
//!
//! Static file assertion (Tier-0): the `StartLimit*` directives belong in `[Unit]`, not `[Service]`.

use std::fs;
use std::path::PathBuf;

fn service_text() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("systemd/camera-box.service");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Return the body of the named INI section (e.g. "Unit"), up to the next `[Section]` header.
///
/// Headers are matched LINE-ANCHORED (start of file or after a newline) — a bare
/// `text.find("[Service]")` would land on the token inside the [Unit] section's own
/// explanatory comment ("placing them in [Service] silently no-ops"), slicing a comment
/// fragment instead of the real section (the anchor-uniqueness trap; caught live on CI).
fn section<'a>(text: &'a str, name: &str) -> &'a str {
    let header = format!("[{name}]");
    let start = if text.starts_with(&header) {
        0
    } else {
        let nl_header = format!("\n{header}");
        text.find(&nl_header)
            .map(|i| i + 1)
            .unwrap_or_else(|| panic!("no line-anchored [{name}] section"))
    };
    let after = &text[start + header.len()..];
    match after.find("\n[") {
        Some(end) => &after[..end],
        None => after,
    }
}

#[test]
fn unit_section_declares_a_start_limit_burst_and_interval() {
    let text = service_text();
    let unit = section(&text, "Unit");
    assert!(
        unit.contains("StartLimitIntervalSec="),
        "[Unit] must set StartLimitIntervalSec= to bound a restart storm"
    );
    assert!(
        unit.contains("StartLimitBurst="),
        "[Unit] must set StartLimitBurst= to bound a restart storm"
    );
}

#[test]
fn start_limit_lives_in_unit_not_service_section() {
    // StartLimitIntervalSec/StartLimitBurst are UNIT-level in modern systemd; putting them in
    // [Service] silently no-ops (a classic footgun).
    let text = service_text();
    let service = section(&text, "Service");
    assert!(
        !service.contains("StartLimitIntervalSec="),
        "StartLimitIntervalSec must NOT be in [Service] (unit-level directive)"
    );
    assert!(
        !service.contains("StartLimitBurst="),
        "StartLimitBurst must NOT be in [Service] (unit-level directive)"
    );
}

#[test]
fn keeps_fast_restartsec_for_genuine_transients() {
    // The generous StartLimit must NOT come paired with a slowed global RestartSec — a live
    // camera's genuine transient crash still recovers in ~3 s, not tens of seconds.
    let text = service_text();
    let service = section(&text, "Service");
    assert!(
        service.contains("RestartSec=3"),
        "RestartSec=3 must stay for fast live-camera recovery"
    );
    assert!(
        service.contains("Restart=always"),
        "Restart=always must stay"
    );
}
