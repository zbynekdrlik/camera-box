//! Behavioral guard for `scripts/lib/manifest-autosource.sh` (#1082) — the best-effort layer that
//! makes the `[0/8]` version-integrity gate's byte facet a genuine POINTER to CI truth: it
//! auto-sources each box's CI-authoritative BUNDLE_MANIFEST for its OWN marker SHA and gathers imag's
//! deployed `.so` sha256s over ssh, so the gate compares DEPLOYED bytes (not just the hand-written
//! GENLOCK_BUILD_SHA marker) against the manifest.
//!
//! Everything here is BEST-EFFORT: a fetch/gather failure yields `""` so the caller omits the arg and
//! the gate facet stays DORMANT (opt-in) — never a spurious refuse. `gh run download` and the imag
//! ssh gather are isolated behind env-overridable command seams (#836 executable-fixture), so this
//! whole path is proven offline with NO gh, NO ssh, NO network.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/manifest-autosource.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

/// Source the lib (its `set +e` is applied by the harness after the source, mirroring the gate's own
/// run_sourced) and run `body`, returning stdout. extra_env threads the #836 fixture seams.
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\nset +e\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("LIB", lib());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn tmpdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("mas-test-{}-{}", std::process::id(), rand_suffix()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn write_file(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    p
}

// ── imag_so_bytes_csv: the pure LOCAL parser turning the ssh gather's `<path> <sha>` lines into the
// `path=sha,path=sha` CSV the gate's --imag-bytes wants ─────────────────────────────────────────

#[test]
fn imag_so_bytes_csv_builds_path_sha_csv() {
    let gather =
        "lib/x86_64-linux-gnu/libobs.so.30 aaaa\nlib/x86_64-linux-gnu/obs-plugins/distroav.so bbbb";
    let out = run_sourced("imag_so_bytes_csv \"$G\"", &[("G", gather)]);
    assert_eq!(
        out.trim(),
        "lib/x86_64-linux-gnu/libobs.so.30=aaaa,lib/x86_64-linux-gnu/obs-plugins/distroav.so=bbbb",
        "must join the gathered path/sha lines into the --imag-bytes CSV form: {out:?}"
    );
}

#[test]
fn imag_so_bytes_csv_empty_on_tool_missing() {
    // #833: a missing remote sha256sum surfaces as TOOL_MISSING, never a measured zero — the parser
    // must yield "" (facet dormant), never a partial/false CSV.
    let out = run_sourced(
        "imag_so_bytes_csv \"$G\"",
        &[("G", "TOOL_MISSING:sha256sum")],
    );
    assert_eq!(
        out.trim(),
        "",
        "TOOL_MISSING must yield an empty CSV (dormant): {out:?}"
    );
}

#[test]
fn imag_so_bytes_csv_empty_on_empty_input() {
    let out = run_sourced("imag_so_bytes_csv \"$G\"", &[("G", "")]);
    assert_eq!(
        out.trim(),
        "",
        "empty gather -> empty CSV (dormant): {out:?}"
    );
}

#[test]
fn imag_so_gather_cmd_emits_the_three_genlock_so_paths() {
    // The remote snippet must sha256 exactly the 3 genlock-bearing .so files (the libobs core +
    // distroav + libobs-opengl, per setup-imag.sh), keyed by their manifest-relative paths.
    let out = run_sourced("imag_so_gather_cmd", &[]);
    assert!(
        out.contains("lib/x86_64-linux-gnu/libobs.so.30"),
        "must gather libobs.so.30: {out}"
    );
    assert!(
        out.contains("lib/x86_64-linux-gnu/obs-plugins/distroav.so"),
        "must gather distroav.so: {out}"
    );
    assert!(
        out.contains("lib/x86_64-linux-gnu/libobs-opengl.so.30"),
        "must gather libobs-opengl.so.30: {out}"
    );
    assert!(out.contains("sha256sum"), "must use sha256sum: {out}");
    assert!(
        out.contains("TOOL_MISSING"),
        "must fail loud by name if sha256sum is absent (#833): {out}"
    );
}

// ── manifest_autosource_fetch: the #836 executable-fixture seam replaces gh entirely ────────────

#[test]
fn manifest_autosource_fetch_uses_the_executable_seam() {
    let dir = tmpdir();
    // A fixture that stands in for `gh run download`: it just writes a manifest to DEST (the 5th arg)
    // and echoes DEST — proving the seam is honored with no gh/network.
    let seam = write_file(
        &dir,
        "seam.sh",
        "#!/usr/bin/env bash\nset -e\ndest=\"$5\"\nmkdir -p \"$(dirname \"$dest\")\"\nprintf '{\"files\":[]}' > \"$dest\"\nprintf '%s' \"$dest\"\n",
    );
    std::fs::set_permissions(&seam, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let dest = dir.join("out-manifest.json");
    let out = run_sourced(
        "manifest_autosource_fetch owner/repo linux-genlock.yml obs-genlock-linux-x86_64 \"$SHA\" \"$DEST\"",
        &[
            ("MANIFEST_AUTOSOURCE_CMD", seam.to_str().unwrap()),
            ("SHA", "abc123def456"),
            ("DEST", dest.to_str().unwrap()),
        ],
    );
    assert_eq!(
        out.trim(),
        dest.to_str().unwrap(),
        "fetch must echo the DEST path the seam produced: {out:?}"
    );
    assert!(dest.exists(), "the seam-produced manifest must be at DEST");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn manifest_autosource_fetch_dormant_on_seam_failure() {
    let dir = tmpdir();
    // A seam that fails (no run at that SHA / download error) -> fetch echoes "" (dormant), never
    // a partial path — the caller then omits --manifest and the byte facet stays dormant.
    let seam = write_file(&dir, "fail.sh", "#!/usr/bin/env bash\nexit 3\n");
    std::fs::set_permissions(&seam, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let dest = dir.join("out.json");
    let out = run_sourced(
        "manifest_autosource_fetch owner/repo linux-genlock.yml art \"$SHA\" \"$DEST\"",
        &[
            ("MANIFEST_AUTOSOURCE_CMD", seam.to_str().unwrap()),
            ("SHA", "abc"),
            ("DEST", dest.to_str().unwrap()),
        ],
    );
    assert_eq!(
        out.trim(),
        "",
        "a failed fetch must yield an empty path (dormant): {out:?}"
    );
    assert!(!dest.exists(), "nothing must be written on a failed fetch");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn manifest_autosource_fetch_dormant_on_empty_sha() {
    // No marker SHA -> nothing to key the artifact on -> dormant (never invokes the seam at all).
    let dir = tmpdir();
    let seam = write_file(
        &dir,
        "seam.sh",
        "#!/usr/bin/env bash\necho SEAM-RAN >&2\nprintf '%s' \"$5\"\n",
    );
    std::fs::set_permissions(&seam, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let dest = dir.join("out.json");
    let out = run_sourced(
        "manifest_autosource_fetch owner/repo wf art \"\" \"$DEST\"",
        &[
            ("MANIFEST_AUTOSOURCE_CMD", seam.to_str().unwrap()),
            ("DEST", dest.to_str().unwrap()),
        ],
    );
    assert_eq!(
        out.trim(),
        "",
        "empty SHA must yield an empty path (dormant): {out:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── the small state-reading helpers recording-e2e.sh keys the auto-source on ────────────────────

#[test]
fn genlock_build_sha_state_read_reads_the_marker_sha() {
    let dir = tmpdir();
    let state = write_file(
        &dir,
        "strih.json",
        "{\"obs_version\":\"32.1.2\",\"genlock_build_sha\":\"26de1c3c23980488a110dbf02e5e472f15cb001d\"}",
    );
    let out = run_sourced(
        "genlock_build_sha_state_read \"$F\"",
        &[("F", state.to_str().unwrap())],
    );
    assert_eq!(
        out.trim(),
        "26de1c3c23980488a110dbf02e5e472f15cb001d",
        "must read the genlock_build_sha marker from the box state: {out:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn manifest_autosource_state_has_key_gates_on_a_nonempty_value() {
    let dir = tmpdir();
    let with = write_file(
        &dir,
        "with.json",
        "{\"obs_dll_sha256\":\"deadbeefcafebabe0000000000000000000000000000000000000000deadbeef\"}",
    );
    let without = write_file(&dir, "without.json", "{\"obs_version\":\"32.1.2\"}");
    let empty = write_file(&dir, "empty.json", "{\"obs_dll_sha256\":\"\"}");
    let out_with = run_sourced(
        "manifest_autosource_state_has_key \"$F\" obs_dll_sha256 && echo YES || echo NO",
        &[("F", with.to_str().unwrap())],
    );
    let out_without = run_sourced(
        "manifest_autosource_state_has_key \"$F\" obs_dll_sha256 && echo YES || echo NO",
        &[("F", without.to_str().unwrap())],
    );
    let out_empty = run_sourced(
        "manifest_autosource_state_has_key \"$F\" obs_dll_sha256 && echo YES || echo NO",
        &[("F", empty.to_str().unwrap())],
    );
    assert_eq!(
        out_with.trim(),
        "YES",
        "a box reporting the byte key -> auto-source engages"
    );
    assert_eq!(
        out_without.trim(),
        "NO",
        "a box NOT reporting the key -> stay dormant"
    );
    assert_eq!(
        out_empty.trim(),
        "NO",
        "an EMPTY key value must not count as reported (would flip obs_dll_sha256 UNKNOWN)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
