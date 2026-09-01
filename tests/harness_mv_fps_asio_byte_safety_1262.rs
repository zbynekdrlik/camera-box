//! #1262 -- extends the #1258 byte-safety fix (a PS-5.1-ANSI-reencoded invalid UTF-8 byte in the raw
//! OBS-log tail can blind a plain `grep`/`sed` extraction of a DIFFERENT audit-line family) to the
//! `multiview-audit:` (mv-fps) and `asrc: source ... starved_blocks=` (asio-starve) taps named in
//! this ticket.
//!
//! **This ticket's own verification NARROWS the mechanism the ticket body proposed, with evidence
//! (see the STEP-0/design comments on #1262):** an invalid byte living on a SEPARATE, `\n`-terminated
//! line elsewhere in the same raw tail does NOT blind grep 3.11 (verified empirically -- single-line,
//! 30-line, and a realistic 2000-line/241 KB fixture all matched cleanly with a PLAIN `grep -F`,
//! because `multiview-audit:`/`asrc:` lines are never co-emitted with the `≈` glyph the way the #1258
//! `genlock-fifo audit … received=…` line is). The REAL, reproducible mechanism for these two taps is
//! a missing `\n` at a PS→ssh transport-chunk boundary gluing a corrupted `genlock-fifo audit (…)`
//! line directly onto the following clean target line with no separator -- now the invalid byte IS
//! co-resident on the "line" grep/sed must decode, and the SAME `LC_ALL=C grep -a[F]` / `LC_ALL=C sed`
//! fix #1258 established applies identically.
//!
//! Fixed:
//!   * `mv_fps_extract_audit_lines` (NEW, `scripts/lib/mv-fps-health.sh`) -- the shared byte-safe
//!     `multiview-audit:` extractor both `scripts/mv-fps-alert-watchdog.sh::handle_box` and
//!     `scripts/lib/mv-fps-preflight.sh::mv_fps_preflight_probe` now call (was a plain inline
//!     `grep -F` duplicated at both call sites).
//!   * `asio_starve_parse_blocks` (`scripts/lib/asio-starve-health.sh`) -- already had `grep -aF`
//!     (defeats the binary-abort), but its trailing `sed` had no `LC_ALL=C` and returned GARBAGE
//!     (not a clean empty string) on the adversarial fixture -- worse than "none", since a
//!     differently-shaped garble would not by chance still contain the right digits.
//!
//! Verified UNCHANGED (no fix needed, cited with evidence, not asserted here -- see the STEP-0
//! comment): `scripts/ndi-halving-watchdog.sh` has NO bash grep/sed extraction on the raw tail at
//! all (it flows straight into `ndi_halving_decision.py analyze`'s stdin), and that script already
//! decodes tolerantly (`sys.stdin.buffer.read().decode("utf-8", errors="replace")`, commit
//! `801ad5b29`, its own regression test already in
//! `tests/python/test_ndi_halving_decision_1203.py::test_analyze_survives_non_utf8_bytes_and_crlf_1203`).
//!
//! Tier-0: no rig, no real ssh. Fixtures are raw bytes (fed via a fixture FILE, never an embedded
//! Rust string literal, which cannot hold invalid UTF-8) piped through `cat` into the REAL sourced
//! shell functions -- the exact shape `tests/harness_received_tap_byte_safety_1258.rs` uses.

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
        "mv_fps_asio_byte_safety_1262_{}_{}_{}",
        tag,
        std::process::id(),
        nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

/// A corrupted `genlock-fifo audit` line (the #1258 ANSI-reencode invalid byte, 0xA0, a lone
/// continuation byte with no leading byte -- mirrors the live corruption) with NO trailing `\n`,
/// glued directly onto the following clean target line -- the reproduced #1262 mechanism (a
/// transport-chunk boundary dropping the separator).
fn glued_prefix() -> &'static [u8] {
    b"03:10:01.123: [genlock-fifo] audit relock=0 underrun=0 late_hold=0 (\xa030 frames @ 60fps)"
}

fn write_bytes(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let f = dir.join(name);
    fs::write(&f, data).unwrap();
    f
}

fn run_bash(script: &str) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(manifest_dir())
        .output()
        .expect("run bash");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

// ---- 1. mv_fps_extract_audit_lines (scripts/lib/mv-fps-health.sh) ------------------------------

fn mv_fps_extract(fixture: &Path) -> (String, String) {
    let lib = manifest_dir().join("scripts/lib/mv-fps-health.sh");
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\ncat \"{fixture}\" | mv_fps_extract_audit_lines",
        lib = lib.display(),
        fixture = fixture.display(),
    );
    let (_rc, out, err) = run_bash(&script);
    (out, err)
}

#[test]
fn mv_fps_extract_audit_lines_survives_a_glued_corrupted_line_1262() {
    let dir = tmp_dir("mvfps_glued");
    let mut data = glued_prefix().to_vec();
    data.extend_from_slice(
        b"03:11:05.456: multiview-audit: monitor=3 divisor=1 rendered_fps=27.90 target=30.00 \
          floor=28.00 cx=1920 cy=1080\r\n",
    );
    let fixture = write_bytes(&dir, "fixture.bin", &data);
    let (out, err) = mv_fps_extract(&fixture);
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.contains("multiview-audit: monitor=3"),
        "#1262: mv_fps_extract_audit_lines must return the multiview-audit line even when a \
         corrupted genlock-fifo line is glued directly onto it (missing \\n at a transport-chunk \
         boundary) -- got out={out:?} stderr={err:?}"
    );
    assert!(
        !err.contains("binary"),
        "#1262: extraction must never hit grep's binary-file detection -- stderr={err:?}"
    );
}

#[test]
fn mv_fps_extract_audit_lines_is_not_blinded_by_corruption_on_separate_lines_1262() {
    // Documents the #1262 STEP-0 finding: an invalid byte on a SEPARATE, \n-terminated line does
    // NOT need the glued-line mechanism to reproduce a failure -- it never blinds a plain `grep -F`
    // in this grep version either, before OR after the fix. Locks the finding in as a regression
    // guard (a future grep behavior change here would be worth knowing about).
    let dir = tmp_dir("mvfps_separate");
    let mut data = Vec::new();
    for i in 0..30 {
        data.extend_from_slice(
            format!(
                "03:1{}:01.123: [genlock-fifo] audit relock=0 underrun=0 late_hold=0 (",
                i % 10
            )
            .as_bytes(),
        );
        data.push(0xA0); // the invalid byte, on its OWN separate \n-terminated line
        data.extend_from_slice(format!("{} frames @ 60fps)\r\n", 30 + i).as_bytes());
    }
    data.extend_from_slice(
        b"03:11:05.456: multiview-audit: monitor=3 divisor=1 rendered_fps=27.90 target=30.00 \
          floor=28.00 cx=1920 cy=1080\r\n",
    );
    let fixture = write_bytes(&dir, "fixture.bin", &data);
    let (out, err) = mv_fps_extract(&fixture);
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.contains("multiview-audit: monitor=3"),
        "#1262: 30 corrupted (but separate-line) genlock-fifo lines must never blind extraction of \
         a later clean multiview-audit line -- got out={out:?} stderr={err:?}"
    );
}

// ---- 2. mv_fps_preflight_probe (scripts/lib/mv-fps-preflight.sh) -- uses the SAME shared helper --

#[test]
fn mv_fps_preflight_probe_survives_a_glued_corrupted_line_1262() {
    let dir = tmp_dir("mvfps_pre");
    let mut data = glued_prefix().to_vec();
    data.extend_from_slice(
        b"03:11:05.456: multiview-audit: monitor=3 divisor=1 rendered_fps=27.90 target=30.00 \
          floor=28.00 cx=1920 cy=1080\r\n",
    );
    let fixture = write_bytes(&dir, "fixture.bin", &data);
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

    let lib = manifest_dir().join("scripts/lib/mv-fps-preflight.sh");
    let script = format!(
        "set -uo pipefail\nMV_FPS_PREFLIGHT_PROBE_CMD=\"{stub}\"\nexport MV_FPS_PREFLIGHT_PROBE_CMD\n\
         . \"{lib}\" 2>/dev/null\nmv_fps_preflight_probe 10.0.0.1 linux u p 2000",
        stub = stub.display(),
        lib = lib.display(),
    );
    let (_rc, out, err) = run_bash(&script);
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.contains("multiview-audit: monitor=3"),
        "#1262: mv_fps_preflight_probe must return the multiview-audit line even when a corrupted \
         genlock-fifo line is glued directly onto it -- got out={out:?} stderr={err:?}"
    );
}

// ---- 3. asio_starve_parse_blocks (scripts/lib/asio-starve-health.sh) ---------------------------

fn asio_parse_blocks(fixture: &Path, source: &str) -> String {
    let lib = manifest_dir().join("scripts/lib/asio-starve-health.sh");
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\ncat \"{fixture}\" | asio_starve_parse_blocks '{source}'",
        lib = lib.display(),
        fixture = fixture.display(),
    );
    let (_rc, out, _err) = run_bash(&script);
    out
}

#[test]
fn asio_starve_parse_blocks_returns_a_clean_integer_not_garbage_on_a_glued_corrupted_line_1262() {
    let dir = tmp_dir("asio_glued");
    let mut data = glued_prefix().to_vec();
    data.extend_from_slice(
        b"03:10:05.456: asrc: source 'ASIO Input Capture' estimated=0.00ppm applied=0.00ppm \
          outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks=2946 \
          (#803/#806/#960)\r\n",
    );
    let fixture = write_bytes(&dir, "fixture.bin", &data);
    let blocks = asio_parse_blocks(&fixture, "ASIO Input Capture");
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        blocks, "2946",
        "#1262: asio_starve_parse_blocks must return the CLEAN bare starved_blocks integer, never \
         garbage (a non-numeric string that happens to still satisfy some downstream comparisons by \
         accident of one specific byte layout is not a fix) -- got {blocks:?}"
    );
    // A non-numeric value would read as unhealthy/unknown, never crash the caller -- but it must
    // still be a CLEAN empty string on a genuine miss, never mid-line garbage. Positive assertion
    // above is the load-bearing one; this documents the classifier's own fail-safe for context.
    assert!(
        blocks.chars().all(|c| c.is_ascii_digit()),
        "#1262: a parsed starved_blocks value must be purely numeric or empty -- got {blocks:?}"
    );
}

#[test]
fn asio_starve_parse_blocks_is_not_blinded_by_corruption_on_separate_lines_1262() {
    let dir = tmp_dir("asio_separate");
    let mut data = Vec::new();
    for i in 0..30 {
        let mut line = format!(
            "03:1{i}:01.123: [genlock-fifo] audit relock=0 underrun=0 late_hold=0 (",
            i = i % 10
        )
        .into_bytes();
        line.push(0xA0);
        line.extend_from_slice(format!("{} frames @ 60fps)\r\n", 30 + i).as_bytes());
        data.extend_from_slice(&line);
    }
    data.extend_from_slice(
        b"03:10:05.456: asrc: source 'ASIO Input Capture' estimated=0.00ppm applied=0.00ppm \
          outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks=2946 \
          (#803/#806/#960)\r\n",
    );
    let fixture = write_bytes(&dir, "fixture.bin", &data);
    let blocks = asio_parse_blocks(&fixture, "ASIO Input Capture");
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        blocks, "2946",
        "#1262: 30 corrupted (but separate-line) genlock-fifo lines must never blind or garble \
         extraction of a later clean asrc line -- got {blocks:?}"
    );
}
