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

// ── #1346: the FULL windows-genlock bundle manifest, fetched as the gate's alternate ─────────────

#[test]
fn win_full_manifest_fetch_targets_the_full_bundle_workflow_and_artifact_1346() {
    let dir = tmpdir();
    // The seam records the (workflow, artifact, sha) it was asked for, writes a manifest to DEST and
    // echoes it -- proving the helper asks for the FULL bundle (windows-genlock.yml /
    // obs-genlock-windows-x64), keyed on the SAME marker sha as the FAST fetch.
    let seam = write_file(
        &dir,
        "seam.sh",
        "#!/usr/bin/env bash\nset -e\nprintf '%s %s %s' \"$2\" \"$3\" \"$4\" > \"$(dirname \"$5\")/asked.txt\"\nprintf '{\"files\":[]}' > \"$5\"\nprintf '%s' \"$5\"\n",
    );
    std::fs::set_permissions(&seam, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let dest = dir.join("win-full-manifest.json");
    let out = run_sourced(
        "manifest_autosource_fetch_win_full owner/repo \"$SHA\" \"$DEST\"",
        &[
            ("MANIFEST_AUTOSOURCE_CMD", seam.to_str().unwrap()),
            ("SHA", "54995646abc"),
            ("DEST", dest.to_str().unwrap()),
        ],
    );
    assert_eq!(
        out.trim(),
        dest.to_str().unwrap(),
        "the helper must echo the fetched manifest path: {out:?}"
    );
    let asked = std::fs::read_to_string(dir.join("asked.txt")).expect("the seam must have run");
    assert_eq!(
        asked, "windows-genlock.yml obs-genlock-windows-x64 54995646abc",
        "must fetch the FULL bundle's manifest for the given marker sha"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn win_full_manifest_fetch_is_dormant_on_failure_1346() {
    let dir = tmpdir();
    let seam = write_file(&dir, "fail.sh", "#!/usr/bin/env bash\nexit 3\n");
    std::fs::set_permissions(&seam, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let dest = dir.join("win-full-manifest.json");
    let failed = run_sourced(
        "manifest_autosource_fetch_win_full owner/repo \"$SHA\" \"$DEST\"",
        &[
            ("MANIFEST_AUTOSOURCE_CMD", seam.to_str().unwrap()),
            ("SHA", "abc"),
            ("DEST", dest.to_str().unwrap()),
        ],
    );
    assert_eq!(
        failed.trim(),
        "",
        "a failed fetch yields no alternate (the FAST manifest is judged alone)"
    );
    let no_sha = run_sourced(
        "manifest_autosource_fetch_win_full owner/repo \"\" \"$DEST\"",
        &[
            ("MANIFEST_AUTOSOURCE_CMD", seam.to_str().unwrap()),
            ("DEST", dest.to_str().unwrap()),
        ],
    );
    assert_eq!(no_sha.trim(), "", "no marker sha -> no alternate");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stand-in `gh` on PATH for the real (non-seam) fetch path: `run list` resolves run 4242 with its
/// updatedAt (the jq output `<id> <updatedAt>`; `$STUB_DIR/runlist` overrides it, e.g. a re-run),
/// `run download` writes a BUNDLE_MANIFEST.json into its `--dir` and counts the download -- or fails
/// once `$STUB_DIR/dl-fail` exists, or hangs (exec sleep) once `$STUB_DIR/dl-hang` exists.
/// `run list` itself fails once `$STUB_DIR/list-fail` exists.
fn write_gh_stub(dir: &std::path::Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = write_file(
        &bin,
        "gh",
        "#!/usr/bin/env bash\n\
         case \"$1 $2\" in\n\
         \"run list\")\n\
           [ -f \"$STUB_DIR/list-fail\" ] && exit 1\n\
           cat \"$STUB_DIR/runlist\" 2>/dev/null || printf '4242 2026-09-25T04:48:30Z' ;;\n\
         \"run download\")\n\
           [ -f \"$STUB_DIR/dl-fail\" ] && exit 1\n\
           [ -f \"$STUB_DIR/dl-hang\" ] && exec sleep 30\n\
           echo x >> \"$STUB_DIR/downloads\"\n\
           d=\"\"\n\
           while [ $# -gt 0 ]; do [ \"$1\" = \"--dir\" ] && d=\"$2\"; shift; done\n\
           mkdir -p \"$d/bin/64bit\"\n\
           printf '{\"files\":[]}' > \"$d/BUNDLE_MANIFEST.json\" ;;\n\
         *) exit 2 ;;\n\
         esac\n",
    );
    std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin
}

/// #1346 review: the FULL bundle artifact is ~270 MB, so the manifest of a CI run is cached per
/// (workflow, artifact, run id) -- a run's artifact never changes -- and a later fetch of the same
/// run reads the cache instead of downloading again.
#[test]
fn manifest_fetch_caches_by_run_id_and_skips_the_second_download_1346() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path().to_path_buf();
    let bin = write_gh_stub(&dir);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let cache = dir.join("cache");
    let dest1 = dir.join("one/win-full-manifest.json");
    let dest2 = dir.join("two/win-full-manifest.json");
    let out = run_sourced(
        "manifest_autosource_fetch_win_full o/r \"$SHA\" \"$DEST1\"; echo; \
         : > \"$STUB_DIR/dl-fail\"; \
         manifest_autosource_fetch_win_full o/r \"$SHA\" \"$DEST2\"",
        &[
            ("PATH", path.as_str()),
            ("MANIFEST_AUTOSOURCE_CMD", ""),
            ("MANIFEST_AUTOSOURCE_CACHE_DIR", cache.to_str().unwrap()),
            ("STUB_DIR", dir.to_str().unwrap()),
            ("SHA", "54995646abc"),
            ("DEST1", dest1.to_str().unwrap()),
            ("DEST2", dest2.to_str().unwrap()),
        ],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [dest1.to_str().unwrap(), dest2.to_str().unwrap()],
        "both fetches must deliver the manifest: {out:?}"
    );
    let downloads = std::fs::read_to_string(dir.join("downloads")).unwrap_or_default();
    assert_eq!(
        downloads.lines().count(),
        1,
        "the second fetch of the same run must come from the cache, not a new download"
    );
    assert!(
        cache
            .join("windows-genlock.yml--obs-genlock-windows-x64--4242-20260925044830.json")
            .is_file(),
        "the manifest must be cached under workflow--artifact--run_id"
    );
    assert!(
        dest2.is_file(),
        "the cached copy must land at the second DEST"
    );
    drop(td);
}

#[test]
fn manifest_fetch_stays_dormant_and_caches_nothing_when_the_download_fails_1346() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path().to_path_buf();
    let bin = write_gh_stub(&dir);
    std::fs::write(dir.join("dl-fail"), "").unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let cache = dir.join("cache");
    let dest = dir.join("win-full-manifest.json");
    let out = run_sourced(
        "manifest_autosource_fetch_win_full o/r \"$SHA\" \"$DEST\"",
        &[
            ("PATH", path.as_str()),
            ("MANIFEST_AUTOSOURCE_CMD", ""),
            ("MANIFEST_AUTOSOURCE_CACHE_DIR", cache.to_str().unwrap()),
            ("STUB_DIR", dir.to_str().unwrap()),
            ("SHA", "54995646abc"),
            ("DEST", dest.to_str().unwrap()),
        ],
    );
    assert_eq!(out.trim(), "", "a failed download yields no manifest");
    assert!(!dest.exists(), "nothing written to DEST");
    assert!(
        !cache
            .join("windows-genlock.yml--obs-genlock-windows-x64--4242-20260925044830.json")
            .exists(),
        "a failed download must never leave a cache entry"
    );
    drop(td);
}

/// #1346 review round 2: a GitHub re-run KEEPS the run id (only the attempt and updatedAt change)
/// and a full re-run republishes non-reproducible bytes, so the cache key carries updatedAt -- a
/// re-run of an already-cached run is downloaded again, never served the old attempt's manifest.
#[test]
fn manifest_fetch_rerun_of_a_cached_run_is_downloaded_again_1346() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path().to_path_buf();
    let bin = write_gh_stub(&dir);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let cache = dir.join("cache");
    let dest1 = dir.join("one/win-full-manifest.json");
    let dest2 = dir.join("two/win-full-manifest.json");
    let out = run_sourced(
        "manifest_autosource_fetch_win_full o/r \"$SHA\" \"$DEST1\"; echo; \
         printf '4242 2026-09-26T10:00:00Z' > \"$STUB_DIR/runlist\"; \
         manifest_autosource_fetch_win_full o/r \"$SHA\" \"$DEST2\"",
        &[
            ("PATH", path.as_str()),
            ("MANIFEST_AUTOSOURCE_CMD", ""),
            ("MANIFEST_AUTOSOURCE_CACHE_DIR", cache.to_str().unwrap()),
            ("STUB_DIR", dir.to_str().unwrap()),
            ("SHA", "54995646abc"),
            ("DEST1", dest1.to_str().unwrap()),
            ("DEST2", dest2.to_str().unwrap()),
        ],
    );
    assert_eq!(
        out.lines().count(),
        2,
        "both fetches must deliver the manifest: {out:?}"
    );
    let downloads = std::fs::read_to_string(dir.join("downloads")).unwrap_or_default();
    assert_eq!(
        downloads.lines().count(),
        2,
        "a re-run (same run id, new updatedAt) must be downloaded again, not served from the cache"
    );
    for stamp in ["20260925044830", "20260926100000"] {
        assert!(
            cache
                .join(format!(
                    "windows-genlock.yml--obs-genlock-windows-x64--4242-{stamp}.json"
                ))
                .is_file(),
            "each attempt is cached under its own updatedAt ({stamp})"
        );
    }
    drop(td);
}

/// #1346 review: the gh calls are bounded -- a stalled download must not hang the [0/8] preflight.
#[test]
fn manifest_fetch_is_bounded_when_the_download_hangs_1346() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path().to_path_buf();
    let bin = write_gh_stub(&dir);
    std::fs::write(dir.join("dl-hang"), "").unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let dest = dir.join("win-full-manifest.json");
    let started = std::time::Instant::now();
    let out = run_sourced(
        "manifest_autosource_fetch_win_full o/r \"$SHA\" \"$DEST\"",
        &[
            ("PATH", path.as_str()),
            ("MANIFEST_AUTOSOURCE_CMD", ""),
            ("MANIFEST_AUTOSOURCE_TIMEOUT_S", "1"),
            (
                "MANIFEST_AUTOSOURCE_CACHE_DIR",
                dir.join("cache").to_str().unwrap(),
            ),
            ("STUB_DIR", dir.to_str().unwrap()),
            ("SHA", "54995646abc"),
            ("DEST", dest.to_str().unwrap()),
        ],
    );
    assert_eq!(out.trim(), "", "a timed-out download yields no manifest");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the fetch must give up at its timeout, not wait out the 30 s hang: {:?}",
        started.elapsed()
    );
    drop(td);
}

// ── #1346 main ruling (ROZHODNUTÉ 5829099220): which Windows manifest(s) the gate gets ─────────────
//
// The FULL manifest is judged ALONE only for a full-only build (no successful fast run at the marker
// sha). A fast run that exists but whose manifest could not be fetched is a FETCH OUTAGE: the byte pin
// is omitted for that run with a loud line -- never a refusal of a correctly fast-deployed box.

/// Run `body` with the PATH-stubbed gh and return (stdout, stderr-file contents).
fn run_with_gh_stub(dir: &std::path::Path, body: &str, extra: &[(&str, &str)]) -> (String, String) {
    let bin = write_gh_stub(dir);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let errf = dir.join("stderr.txt");
    let mut env: Vec<(&str, &str)> = vec![
        ("PATH", path.as_str()),
        ("MANIFEST_AUTOSOURCE_CMD", ""),
        ("STUB_DIR", dir.to_str().unwrap()),
        ("ERRF", errf.to_str().unwrap()),
    ];
    env.extend_from_slice(extra);
    let out = run_sourced(body, &env);
    let err = std::fs::read_to_string(&errf).unwrap_or_default();
    (out, err)
}

#[test]
fn win_manifest_pair_decide_follows_the_main_ruling_1346() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path();
    // (fast, full, fast-run state) -> (primary, alternate, stderr must contain)
    let cases: [(&str, &str, &str, &str, &str, &str); 7] = [
        ("F", "U", "found", "F", "U", ""),
        ("F", "", "found", "F", "", ""),
        ("", "U", "none", "U", "", "full-only build"),
        ("", "U", "found", "", "", "fetch outage"),
        ("", "U", "unknown", "", "", "fetch outage"),
        ("", "", "none", "", "", ""),
        ("", "", "found", "", "", ""),
    ];
    for (fast, full, state, want_p, want_a, want_err) in cases {
        let (out, err) = run_with_gh_stub(
            dir,
            "win_manifest_pair_decide \"$FAST\" \"$FULL\" \"$STATE\" 2>\"$ERRF\"",
            &[("FAST", fast), ("FULL", full), ("STATE", state)],
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            [want_p, want_a],
            "fast={fast:?} full={full:?} state={state}: {out:?}"
        );
        if want_err.is_empty() {
            assert!(
                err.is_empty(),
                "no log line expected for fast={fast:?} full={full:?} state={state}: {err:?}"
            );
        } else {
            assert!(
                err.contains(want_err),
                "fast={fast:?} full={full:?} state={state}: stderr must say {want_err:?}: {err:?}"
            );
        }
    }
}

#[test]
fn manifest_autosource_run_state_reads_found_none_unknown_1346() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path();
    for (runlist, list_fail, want) in [
        ("1", false, "found"),
        ("0", false, "none"),
        ("oops", false, "unknown"),
        ("1", true, "unknown"),
    ] {
        std::fs::write(dir.join("runlist"), runlist).unwrap();
        let fail = dir.join("list-fail");
        if list_fail {
            std::fs::write(&fail, "").unwrap();
        } else {
            let _ = std::fs::remove_file(&fail);
        }
        let (out, _) = run_with_gh_stub(
            dir,
            "manifest_autosource_run_state o/r windows-genlock-fast.yml \"$SHA\"",
            &[("SHA", "54995646abc")],
        );
        assert_eq!(
            out.trim(),
            want,
            "run list {runlist:?} (fail={list_fail}) must read {want}"
        );
    }
    let (empty_sha, _) = run_with_gh_stub(
        dir,
        "manifest_autosource_run_state o/r windows-genlock-fast.yml \"\"",
        &[],
    );
    assert_eq!(
        empty_sha.trim(),
        "unknown",
        "no marker sha -> unknown, never none"
    );
}

/// A full-only build (no successful fast run at the marker sha): the FULL manifest is the pin.
#[test]
fn win_manifest_pair_resolve_pins_a_full_only_build_1346() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path();
    std::fs::write(dir.join("runlist"), "0").unwrap();
    let (out, err) = run_with_gh_stub(
        dir,
        "win_manifest_pair_resolve o/r \"$SHA\" \"\" /x/win-full-manifest.json 2>\"$ERRF\"",
        &[("SHA", "54995646abc")],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        ["/x/win-full-manifest.json", ""],
        "a full-only build is judged against the full manifest alone: {out:?}"
    );
    assert!(err.contains("full-only build"), "must log why: {err:?}");
}

/// A fast run exists but its manifest fetch failed: a fetch outage -> the byte pin is omitted for
/// this run (loud line), never the full manifest alone (which would refuse a fast-deployed box).
#[test]
fn win_manifest_pair_resolve_omits_the_pin_on_a_fast_fetch_outage_1346() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path();
    std::fs::write(dir.join("runlist"), "1").unwrap();
    let (out, err) = run_with_gh_stub(
        dir,
        "win_manifest_pair_resolve o/r \"$SHA\" \"\" /x/win-full-manifest.json 2>\"$ERRF\"",
        &[("SHA", "54995646abc")],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        ["", ""],
        "a fetch outage omits the byte pin: {out:?}"
    );
    assert!(
        err.contains("WARNING") && err.contains("fetch outage"),
        "the omission must be loud: {err:?}"
    );
    // The run lookup itself failing is also an outage, never a full-only verdict.
    std::fs::write(dir.join("list-fail"), "").unwrap();
    let (out2, _) = run_with_gh_stub(
        dir,
        "win_manifest_pair_resolve o/r \"$SHA\" \"\" /x/win-full-manifest.json 2>\"$ERRF\"",
        &[("SHA", "54995646abc")],
    );
    assert_eq!(out2.lines().collect::<Vec<_>>(), ["", ""]);
}

/// With the fast manifest in hand no run lookup happens, and the pair passes through unchanged.
#[test]
fn win_manifest_pair_resolve_passes_a_fetched_fast_manifest_through_1346() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path();
    std::fs::write(dir.join("list-fail"), "").unwrap();
    let (out, err) = run_with_gh_stub(
        dir,
        "win_manifest_pair_resolve o/r \"$SHA\" /x/fast.json /x/full.json 2>\"$ERRF\"",
        &[("SHA", "54995646abc")],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, ["/x/fast.json", "/x/full.json"], "{out:?}");
    assert!(
        err.is_empty(),
        "no log when the fast manifest was fetched: {err:?}"
    );
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
