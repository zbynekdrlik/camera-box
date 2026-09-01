//! #1258 layer 2 -- the `[4c/8]` frozen-camera gate's `received=` tap is STILL blind after the
//! merged layer-1 EncodedCommand fix (8642a2b5d). Live proof (supervisor manual E2E run 1651316094,
//! dev1 checkout at 1.7.0-dev.607, issue-1258 comment 5497028679): `mv_reverify_probe_raw` now reads
//! strih's OBS log fine (104 896 bytes of real text came back), but PowerShell 5.1 `gc` WITHOUT
//! `-Encoding` reads the UTF-8 log as ANSI and re-encodes on output -- an audit line's non-ASCII
//! glyph (the approx-sign in `(approx F frames @ ...)`) comes back as an INVALID-UTF-8 byte. In a
//! UTF-8 locale, GNU grep then flags stdin BINARY (empty stdout, "binary file matches" on stderr),
//! and even when grep still finds the line, sed's trailing `.*` refuses to consume the invalid byte
//! and leaves line-tail garbage after the captured digits -- either way the extracted `received=`
//! value is NOT a clean bare integer, so every caller's `case '' | *[!0-9]*` treats it as "none" /
//! UNKNOWN (the exact `prev=none curr=none -> UNKNOWN` signature of run 1651316094).
//!
//! The fix makes every extraction stage byte-safe end to end: `LC_ALL=C grep -a` finds the line
//! regardless of invalid bytes anywhere in the stream, `LC_ALL=C sed` lets `.*` consume any byte so
//! the captured group is clean. Applied to the THREE direct consumers of the same
//! `genlock-fifo audit '<src>': received=N` tap family (the [4c/8] gate's own read path):
//!   * `mv_reverify_extract_received` (scripts/lib/mv-reverify-escalate.sh) -- the shared extractor
//!     `frozen-cam-received.sh` (the [4c/8] gate itself) and `mv_reverify_probe_received` both use;
//!   * `probe_received` (scripts/frozen-input-alert-watchdog.sh) -- the stream frozen-input tap;
//!   * `extract_sample` (scripts/cadence-alert-watchdog.sh) -- the non-60 cadence tap (#1259-added,
//!     same ps_encoded_command fetch mechanism, same `received=` family).
//!
//! Plus one DEFENSIVE hardening (not independently reproducible as RED via a Tier-0 pipe fixture --
//! GNU grep's `-o`/piped-stdin binary-detection heuristic did not trigger on any fixture size tried
//! here, though the SAME grep binary DOES flag an identical byte via a direct file argument, proving
//! the underlying hazard is real): `frozen_input_cambox_sources` (scripts/lib/frozen-input-health.sh)
//! -- the enumeration reader that shares the SAME raw `mv_reverify_probe_raw` stream via
//! `probe_enumerate`. Its test below is GREEN-only (asserts the fixed code stays correct on an
//! adversarial fixture), not a reproduced RED/GREEN pair.
//!
//! Tier-0: no rig, no real ssh. Fixtures are raw bytes (a genuine invalid-UTF-8 continuation byte,
//! 0xA0, mirroring the live capture) fed to the REAL sourced functions via a fixture FILE (never an
//! embedded Rust string literal, which cannot hold invalid UTF-8) read back with `cat` inside the
//! bash harness -- the same shape `tests/harness_ps_encoded_fleet_1259.rs`'s `stdout_of` uses for a
//! pure command-builder, extended to pipe a byte fixture as the sourced function's stdin/arg.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "received_tap_byte_safety_1258_{}_{}_{}",
        tag,
        std::process::id(),
        nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

/// The RED fixture: a raw multi-line OBS-log tail mirroring the live ANSI-reencode corruption --
/// TWO lines carry an invalid UTF-8 byte (0xA0, a lone continuation byte with no leading byte), one
/// of them the "NDI cam1" target line itself with the invalid byte immediately after `received=`.
/// `NDI cam3`'s target line is untouched by the invalid byte (proves the fix doesn't merely "work
/// when the target line itself is clean" -- the WHOLE-STREAM binary-flag hazard from an EARLIER
/// line must not blind extraction of a LATER, clean line either).
fn raw_tail_fixture() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(
        b"21:14:55.000: genlock-fifo audit 'NDI 2ME PGM': latency_ms=90 (\xa0F frames @ 30.000fps)\n",
    );
    v.extend_from_slice(
        b"21:15:00.123: genlock-fifo audit 'NDI cam1': received=2656858\xa0%^0 frames @ 30.000fps) other=stuff\n",
    );
    v.extend_from_slice(
        b"21:15:05.140: genlock-fifo audit 'NDI cam3': received=1900222 (\xa030 frames @ 30.000fps)\n",
    );
    v
}

fn write_fixture(dir: &Path) -> PathBuf {
    let f = dir.join("fixture.bin");
    fs::write(&f, raw_tail_fixture()).unwrap();
    f
}

// ---- 1. mv_reverify_extract_received (scripts/lib/mv-reverify-escalate.sh) ---------------------

fn mv_reverify_extract(source: &str) -> String {
    let dir = tmp_dir("mvr");
    let fixture = write_fixture(&dir);
    let lib = manifest_dir().join("scripts/lib/mv-reverify-escalate.sh");
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\ncat \"{fixture}\" | mv_reverify_extract_received '{source}'",
        lib = lib.display(),
        fixture = fixture.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(manifest_dir())
        .output()
        .expect("run bash");
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn mv_reverify_extract_received_survives_invalid_utf8_bytes_1258() {
    let cam1 = mv_reverify_extract("NDI cam1");
    assert_eq!(
        cam1, "2656858",
        "#1258 layer 2: mv_reverify_extract_received must return the clean bare received= integer \
         even when the raw OBS-log tail carries invalid-UTF-8 bytes (PS ANSI-reencode corruption) -- \
         got {cam1:?} (a non-numeric/garbage/empty value reads as 'none' downstream -> UNKNOWN, the \
         run-1651316094 signature)"
    );
    let cam3 = mv_reverify_extract("NDI cam3");
    assert_eq!(
        cam3, "1900222",
        "#1258 layer 2: an EARLIER invalid byte in the stream (on a DIFFERENT source's line) must not \
         blind extraction of a LATER, clean target line -- got {cam3:?}"
    );
}

// ---- 2. probe_received (scripts/frozen-input-alert-watchdog.sh) --------------------------------

fn frozen_input_probe_received(source: &str) -> String {
    let dir = tmp_dir("fiaw");
    let fixture = write_fixture(&dir);
    let stub = dir.join("stub_probe.sh");
    fs::write(
        &stub,
        format!("#!/usr/bin/env bash\ncat \"{}\"\n", fixture.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let script_path = manifest_dir().join("scripts/frozen-input-alert-watchdog.sh");
    let script = format!(
        "set -uo pipefail\n. \"{sp}\" 2>/dev/null\nprobe_received 10.0.0.1 '{source}'",
        sp = script_path.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("FROZEN_INPUT_PROBE_CMD", &stub)
        .current_dir(manifest_dir())
        .output()
        .expect("run bash");
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn frozen_input_probe_received_survives_invalid_utf8_bytes_1258() {
    let cam1 = frozen_input_probe_received("NDI cam1");
    assert_eq!(
        cam1, "2656858",
        "#1258 layer 2: frozen-input-alert-watchdog.sh's probe_received must return the clean \
         received= integer despite invalid-UTF-8 bytes in the raw PS-fetched OBS-log tail -- got \
         {cam1:?}"
    );
}

// ---- 3. extract_sample (scripts/cadence-alert-watchdog.sh) -------------------------------------

fn cadence_extract_sample(source: &str) -> String {
    let dir = tmp_dir("cad");
    let fixture = write_fixture(&dir);
    let script_path = manifest_dir().join("scripts/cadence-alert-watchdog.sh");
    let script = format!(
        "set -uo pipefail\n. \"{sp}\" 2>/dev/null\nraw=\"$(cat \"{fixture}\")\"\nextract_sample \"$raw\" '{source}'",
        sp = script_path.display(),
        fixture = fixture.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(manifest_dir())
        .output()
        .expect("run bash");
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn cadence_extract_sample_survives_invalid_utf8_bytes_1258() {
    let sample = cadence_extract_sample("NDI cam1");
    assert!(
        sample.starts_with("2656858 "),
        "#1258 layer 2: cadence-alert-watchdog.sh's extract_sample must return the clean \
         `<received> <timestamp>` pair despite invalid-UTF-8 bytes in the raw PS-fetched tail -- got \
         {sample:?}"
    );
}

// ---- 4. frozen_input_cambox_sources (scripts/lib/frozen-input-health.sh) -- GREEN-only ---------
// Defensive hardening: this grep -oE/sed -E pair shares the SAME raw `mv_reverify_probe_raw` stream
// (via frozen-input-alert-watchdog.sh's probe_enumerate) as the proven-RED functions above, but no
// Tier-0 pipe fixture reproduced a failure on the UNFIXED code (GNU grep's `-o`+piped-stdin binary
// heuristic did not trigger here, though a direct FILE argument with the identical byte DOES trigger
// it -- `grep -oE ... fixture.bin` -> exit 1, empty stdout). Applied identically for defense in
// depth (per the #1258 comment 5497028679 directive: "the parser must stay byte-safe regardless").
// This test asserts the FIXED code enumerates correctly on the adversarial fixture -- not a
// regression-reproduction pair.
fn frozen_input_cambox_sources_on_fixture() -> String {
    let dir = tmp_dir("fih");
    let fixture = write_fixture(&dir);
    let lib = manifest_dir().join("scripts/lib/frozen-input-health.sh");
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\ncat \"{fixture}\" | frozen_input_cambox_sources",
        lib = lib.display(),
        fixture = fixture.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(manifest_dir())
        .output()
        .expect("run bash");
    let _ = fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn frozen_input_cambox_sources_enumerates_despite_invalid_utf8_bytes_1258() {
    let out = frozen_input_cambox_sources_on_fixture();
    let sources: Vec<&str> = out.lines().collect();
    assert_eq!(
        sources,
        vec!["NDI cam1", "NDI cam3"],
        "#1258 layer 2 (defensive): frozen_input_cambox_sources must enumerate both cambox sources \
         from the adversarial fixture (excluding the program feed 'NDI 2ME PGM') regardless of the \
         invalid-UTF-8 bytes elsewhere in the raw tail -- got {out:?}"
    );
}
