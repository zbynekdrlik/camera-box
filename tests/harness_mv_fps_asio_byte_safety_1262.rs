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
//! an ADVERSARIALLY-CONSTRUCTED (not observed live) missing `\n` gluing a corrupted `genlock-fifo
//! audit (…)` line directly onto the following clean target line with no separator -- now the
//! invalid byte IS co-resident on the "line" grep/sed must decode, and the SAME `LC_ALL=C grep -a[F]`
//! / `LC_ALL=C sed` fix #1258 established applies identically. The one genuinely realistic same-line
//! trigger for asio-starve is a non-ASCII WATCHED SOURCE NAME (today's names are plain ASCII, so this
//! is latent, not active) -- see the design comment on the ticket.
//!
//! Fixed:
//!   * `mv_fps_extract_audit_lines` (NEW, `scripts/lib/mv-fps-health.sh`) -- the shared byte-safe
//!     `multiview-audit:` extractor both `scripts/mv-fps-alert-watchdog.sh::handle_box` and
//!     `scripts/lib/mv-fps-preflight.sh::mv_fps_preflight_probe` now call (was a plain inline
//!     `grep -F` duplicated at both call sites). Review-caught (2026-09-01): `LC_ALL=C grep -a`
//!     alone fixes only the SHELL side -- the real `mv-fps-gate` binary reads stdin via Rust's
//!     `read_to_string`, which REJECTS any invalid byte outright, so the extracted line still
//!     needed the invalid byte STRIPPED (`LC_ALL=C tr -d '\200-\377'`), not merely un-blinded.
//!     `multiview-audit:` lines are pure ASCII by construction, so stripping is lossless, and
//!     `mv_audit::parse_audit_line` finds the marker via `line.find(MARKER)`, so a garbled prefix
//!     before the marker is harmless.
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

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A corrupted `genlock-fifo audit` line matching the REAL emitter shape
/// (`vendor/obs-studio/libobs/obs-source.c:5140`: `genlock-fifo audit '<src>': received=... (≈N
/// frames @ X.XXXfps) ...`), carrying the #1258 ANSI-reencode invalid byte (0xA0, a lone
/// continuation byte with no leading byte -- mirrors the live corruption) where the `≈` glyph
/// would sit, with NO trailing `\n` -- glued directly onto the following clean target line. This
/// is the reproduced #1262 mechanism: an adversarially-constructed missing-newline glue, not an
/// observed live transport failure (see the module doc comment).
fn glued_prefix() -> &'static [u8] {
    b"21:14:55.000: genlock-fifo audit 'NDI 2ME PGM': received=2656858 (\xa0F frames @ 30.000fps)"
}

/// One corrupted, but properly `\n`-terminated, `genlock-fifo audit` line (same real emitter
/// shape as [`glued_prefix`]) for the "many separate corrupted lines never blind a later clean
/// target line" regression lock.
fn separate_corrupted_line(i: u32) -> Vec<u8> {
    let mut line = format!(
        "21:{:02}:{:02}.000: genlock-fifo audit 'NDI cam{}': received={} (",
        i % 60,
        (i * 7) % 60,
        (i % 7) + 1,
        1_000_000 + i
    )
    .into_bytes();
    line.push(0xA0); // the invalid byte, ON ITS OWN \n-terminated line -- never blinds a later line
    line.extend_from_slice(format!("{} frames @ 60.000fps)\n", 30 + i % 10).as_bytes());
    line
}

fn write_bytes(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let f = dir.join(name);
    std::fs::write(&f, data).unwrap();
    f
}

/// Runs `script` under bash, pinning the AMBIENT locale to `C.UTF-8` explicitly -- never rely on
/// whatever `LANG`/`LC_ALL` a CI runner happens to export. Grep's binary-content / invalid-UTF-8
/// detection is ITSELF locale-dependent: under a plain `C`/POSIX locale (or no locale set at all)
/// grep does NO multibyte validation whatsoever, so the bug these tests prove does NOT reproduce
/// there (verified locally: under `LC_ALL=C`, the OLD unfixed asio pipeline returns a CLEAN "2946"
/// on the same adversarial fixture, not garbage -- a test run under that ambient locale would
/// silently stop discriminating RED from GREEN). The fixed functions' own explicit per-command
/// `LC_ALL=C` overrides win for their own pipeline stages regardless of this outer pin, so GREEN
/// is unaffected either way -- this only makes the test's OWN process-wide locale deterministic.
/// Returns (exit_code, RAW stdout bytes, stderr as string).
fn run_bash_bytes(script: &str) -> (i32, Vec<u8>, String) {
    let pinned = format!("export LC_ALL=C.UTF-8\n{script}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&pinned)
        .current_dir(manifest_dir())
        .output()
        .expect("run bash");
    (
        out.status.code().unwrap_or(-1),
        out.stdout,
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn run_bash(script: &str) -> (i32, String, String) {
    let (rc, stdout, stderr) = run_bash_bytes(script);
    (
        rc,
        String::from_utf8_lossy(&stdout).trim().to_string(),
        stderr,
    )
}

// ---- 1. mv_fps_extract_audit_lines (scripts/lib/mv-fps-health.sh) ------------------------------

fn mv_fps_extract_bytes(fixture: &Path) -> (Vec<u8>, String) {
    let lib = manifest_dir().join("scripts/lib/mv-fps-health.sh");
    let script = format!(
        "set -uo pipefail\n. \"{lib}\" 2>/dev/null\ncat \"{fixture}\" | mv_fps_extract_audit_lines",
        lib = lib.display(),
        fixture = fixture.display(),
    );
    let (_rc, stdout, err) = run_bash_bytes(&script);
    (stdout, err)
}

#[test]
fn mv_fps_extract_audit_lines_survives_a_glued_corrupted_line_1262() {
    let dir = TempDir::new().expect("tempdir");
    let mut data = glued_prefix().to_vec();
    data.extend_from_slice(
        b"21:15:00.456: multiview-audit: monitor=3 divisor=1 rendered_fps=27.90 target=30.00 \
          floor=28.00 cx=1920 cy=1080\r\n",
    );
    let fixture = write_bytes(dir.path(), "fixture.bin", &data);
    let (stdout, err) = mv_fps_extract_bytes(&fixture);
    assert!(
        String::from_utf8_lossy(&stdout).contains("multiview-audit: monitor=3"),
        "#1262: mv_fps_extract_audit_lines must return the multiview-audit line even when a \
         corrupted genlock-fifo line is glued directly onto it (an adversarially-constructed \
         missing \\n) -- got out={:?} stderr={err:?}",
        String::from_utf8_lossy(&stdout)
    );
    // Review-caught (2026-09-01): the extracted bytes must be VALID UTF-8, not merely non-empty --
    // the real `mv-fps-gate` binary reads its input via Rust's `read_to_string`, which REJECTS any
    // invalid byte outright, so a glued line that still carries the raw 0xA0 would poison the real
    // gate's read (exit 2, a misleading "gate binary broken?" log line) even though extraction
    // itself "succeeded". This is the assertion that actually catches that class of defect --
    // `from_utf8_lossy` (used above for the readable failure message) would silently mask it.
    assert!(
        std::str::from_utf8(&stdout).is_ok(),
        "#1262: mv_fps_extract_audit_lines's output must be valid UTF-8 (the real mv-fps-gate \
         binary's read_to_string rejects anything else) -- got raw bytes {stdout:?}"
    );
}

#[test]
fn mv_fps_extract_audit_lines_is_not_blinded_by_corruption_on_separate_lines_1262() {
    // Documents the #1262 STEP-0 finding: an invalid byte on a SEPARATE, \n-terminated line never
    // needed the glued-line mechanism to reproduce a failure in the first place -- a plain
    // `grep -F` already matched cleanly in this shape, before OR after the fix (the fixed
    // function's `LC_ALL=C grep -a` is immune to grep's binary-file detection BY CONSTRUCTION, so
    // this test can no longer observe a UTF-8-locale-mode grep regression the way it could against
    // the original unfixed code -- it stays as a straightforward multi-line regression lock, not an
    // ongoing behavioral discriminator).
    let dir = TempDir::new().expect("tempdir");
    let mut data = Vec::new();
    for i in 0..30 {
        data.extend_from_slice(&separate_corrupted_line(i));
    }
    data.extend_from_slice(
        b"21:15:00.456: multiview-audit: monitor=3 divisor=1 rendered_fps=27.90 target=30.00 \
          floor=28.00 cx=1920 cy=1080\r\n",
    );
    let fixture = write_bytes(dir.path(), "fixture.bin", &data);
    let (out, err) = mv_fps_extract(&fixture);
    assert!(
        out.contains("multiview-audit: monitor=3"),
        "#1262: 30 corrupted (but separate-line) genlock-fifo lines must never blind extraction of \
         a later clean multiview-audit line -- got out={out:?} stderr={err:?}"
    );
}

fn mv_fps_extract(fixture: &Path) -> (String, String) {
    let (stdout, err) = mv_fps_extract_bytes(fixture);
    (String::from_utf8_lossy(&stdout).trim().to_string(), err)
}

// ---- 2. mv_fps_preflight_probe (scripts/lib/mv-fps-preflight.sh) -- uses the SAME shared helper --

#[test]
fn mv_fps_preflight_probe_survives_a_glued_corrupted_line_1262() {
    let dir = TempDir::new().expect("tempdir");
    let mut data = glued_prefix().to_vec();
    data.extend_from_slice(
        b"21:15:00.456: multiview-audit: monitor=3 divisor=1 rendered_fps=27.90 target=30.00 \
          floor=28.00 cx=1920 cy=1080\r\n",
    );
    let fixture = write_bytes(dir.path(), "fixture.bin", &data);
    let stub = dir.path().join("stub_probe.sh");
    std::fs::write(
        &stub,
        format!("#!/usr/bin/env bash\ncat \"{}\"\n", fixture.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();

    let lib = manifest_dir().join("scripts/lib/mv-fps-preflight.sh");
    let script = format!(
        "set -uo pipefail\nMV_FPS_PREFLIGHT_PROBE_CMD=\"{stub}\"\nexport MV_FPS_PREFLIGHT_PROBE_CMD\n\
         . \"{lib}\" 2>/dev/null\nmv_fps_preflight_probe 10.0.0.1 linux u p 2000",
        stub = stub.display(),
        lib = lib.display(),
    );
    let (_rc, out, err) = run_bash(&script);
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
    let dir = TempDir::new().expect("tempdir");
    let mut data = glued_prefix().to_vec();
    data.extend_from_slice(
        b"21:15:05.456: asrc: source 'ASIO Input Capture' estimated=0.00ppm applied=0.00ppm \
          outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks=2946 \
          (#803/#806/#960)\r\n",
    );
    let fixture = write_bytes(dir.path(), "fixture.bin", &data);
    let blocks = asio_parse_blocks(&fixture, "ASIO Input Capture");
    assert_eq!(
        blocks, "2946",
        "#1262: asio_starve_parse_blocks must return the CLEAN bare starved_blocks integer, never \
         garbage (a non-numeric string that happens to still satisfy some downstream comparisons by \
         accident of one specific byte layout is not a fix) -- got {blocks:?}"
    );
}

#[test]
fn asio_starve_parse_blocks_is_not_blinded_by_corruption_on_separate_lines_1262() {
    // Same STEP-0-documentation intent as mv_fps_extract_audit_lines_is_not_blinded_..._1262 above
    // -- see that test's comment.
    let dir = TempDir::new().expect("tempdir");
    let mut data = Vec::new();
    for i in 0..30 {
        data.extend_from_slice(&separate_corrupted_line(i));
    }
    data.extend_from_slice(
        b"21:15:05.456: asrc: source 'ASIO Input Capture' estimated=0.00ppm applied=0.00ppm \
          outer_bias=0.00ppm cumulative_correction=0.000ms/60s starved_blocks=2946 \
          (#803/#806/#960)\r\n",
    );
    let fixture = write_bytes(dir.path(), "fixture.bin", &data);
    let blocks = asio_parse_blocks(&fixture, "ASIO Input Capture");
    assert_eq!(
        blocks, "2946",
        "#1262: 30 corrupted (but separate-line) genlock-fifo lines must never blind or garble \
         extraction of a later clean asrc line -- got {blocks:?}"
    );
}
