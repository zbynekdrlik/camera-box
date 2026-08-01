//! issue 930 — `scripts/lipsync-asset.sh`'s pure functions (source URL/sha256 pins, the
//! deterministic ffmpeg trim command, the av_sync_measure.py baseline command, and sha256
//! verification), sourced and called directly. NEVER touches the network — mirrors the
//! recording-verdict-on-*.sh convention (`tests/harness_recording_verdict_on_imag.rs`).

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    manifest_dir().join("scripts/lipsync-asset.sh")
}

fn read(p: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Run `. script; <call>` and return trimmed stdout, panicking with stderr on a non-zero exit.
fn run_sourced(call: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(". \"$1\"; {call}"))
        .arg("bash") // $0
        .arg(script()) // $1
        .output()
        .expect("spawn bash");
    assert!(
        out.status.success(),
        "sourced call `{call}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn lipsync_asset_script_exists_and_is_executable() {
    let meta =
        fs::metadata(script()).unwrap_or_else(|e| panic!("scripts/lipsync-asset.sh missing: {e}"));
    assert!(meta.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "scripts/lipsync-asset.sh must be executable"
        );
    }
}

#[test]
fn source_url_and_sha256_match_the_pinned_provenance_doc() {
    let provenance = read("assets/lipsync/PROVENANCE.md");
    let url = run_sourced("lipsync_asset_source_url");
    let sha = run_sourced("lipsync_asset_source_sha256");

    assert!(
        provenance.contains(&url),
        "930: PROVENANCE.md must document the exact URL the script fetches: {url}"
    );
    assert!(
        provenance.contains(&sha),
        "930: PROVENANCE.md must document the exact sha256 the script verifies: {sha}"
    );
    // The real pinned values from this ticket's own live fetch — pin them here too so a
    // silent drift in EITHER the script or PROVENANCE.md (edited independently) is caught.
    assert_eq!(
        url,
        "https://upload.wikimedia.org/wikipedia/commons/4/45/Kamala_Harris%27_speech_during_Celebrating_America.ogv"
    );
    assert_eq!(
        sha,
        "7ece8fe0ae7aba1374ca9951c0a8f0ca5a9816430d95a38880f93ef87c533b78"
    );
}

#[test]
fn trim_cmd_is_the_documented_30s_60s_recipe() {
    let cmd = run_sourced("lipsync_asset_trim_cmd /tmp/source.ogv /tmp/test.mp4");
    for needle in [
        "ffmpeg",
        "-ss 30",
        "-t 60",
        "scale=1280:720",
        "-r 60",
        "libx264",
        "yuv420p",
        "aac",
        "-ar 44100",
        "-ac 2",
        "/tmp/source.ogv",
        "/tmp/test.mp4",
    ] {
        assert!(
            cmd.contains(needle),
            "930: trim command missing `{needle}`: {cmd}"
        );
    }
}

/// A path containing a space must survive `eval` as ONE argv entry, not split in two -- proves
/// `lipsync_asset_trim_cmd`'s `printf %q` quoting is real, not just "looks quoted".
#[test]
fn trim_cmd_safely_quotes_paths_with_spaces() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    // A fake `ffmpeg` that just dumps its argv, one per line, so we can inspect exactly how the
    // shell split the quoted command -- never invokes the real ffmpeg.
    let fake_bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin_dir).unwrap();
    fs::write(
        fake_bin_dir.join("ffmpeg"),
        "#!/usr/bin/env bash\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            fake_bin_dir.join("ffmpeg"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    let src = "/tmp/a b/source.ogv";
    let out_path = "/tmp/a b/test.mp4";
    let path_env = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap()
    );
    let output = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; eval \"$(lipsync_asset_trim_cmd \"$2\" \"$3\")\"")
        .arg("bash")
        .arg(script())
        .arg(src)
        .arg(out_path)
        .env("PATH", path_env)
        .output()
        .expect("spawn bash");
    assert!(
        output.status.success(),
        "930: eval'd trim command must succeed against the fake ffmpeg: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let argv = String::from_utf8_lossy(&output.stdout);
    assert!(
        argv.lines().any(|l| l == src),
        "930: the source path with a space must arrive as ONE argv entry, not split: {argv}"
    );
    assert!(
        argv.lines().any(|l| l == out_path),
        "930: the output path with a space must arrive as ONE argv entry, not split: {argv}"
    );
}

#[test]
fn baseline_cmd_without_calibration_log() {
    let cmd = run_sourced("lipsync_asset_baseline_cmd python3 /repo/scripts/av_sync_measure.py /repo/assets/lipsync/test.mp4");
    assert!(cmd.contains("python3"));
    assert!(cmd.contains("av_sync_measure.py"));
    assert!(cmd.contains("--media"));
    assert!(cmd.contains("test.mp4"));
    assert!(
        !cmd.contains("--calibration-log"),
        "930: no calibration-log arg given -> must not appear: {cmd}"
    );
}

#[test]
fn baseline_cmd_with_calibration_log() {
    let cmd = run_sourced(
        "lipsync_asset_baseline_cmd python3 /repo/scripts/av_sync_measure.py /repo/assets/lipsync/test.mp4 /tmp/cal.jsonl",
    );
    assert!(cmd.contains("--calibration-log"));
    assert!(cmd.contains("/tmp/cal.jsonl"));
}

#[test]
fn verify_sha256_matches_and_rejects_mismatch() {
    let mut tmp = tempfile::NamedTempFile::new().expect("tmpfile");
    tmp.write_all(b"issue 930 lipsync fixture content")
        .expect("write");
    let path = tmp.path().to_str().unwrap().to_string();

    // Compute the real sha256 the same way sha256sum would, via the shell (no extra rust dep).
    let real_sha = Command::new("bash")
        .arg("-c")
        .arg(format!("sha256sum '{path}' | awk '{{print $1}}'"))
        .output()
        .expect("sha256sum");
    let real_sha = String::from_utf8_lossy(&real_sha.stdout).trim().to_string();

    let ok = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". \"$1\"; lipsync_asset_verify_sha256 '{path}' '{real_sha}'"
        ))
        .arg("bash")
        .arg(script())
        .status()
        .expect("spawn bash");
    assert!(ok.success(), "930: correct sha256 must verify true");

    let bad = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". \"$1\"; lipsync_asset_verify_sha256 '{path}' 'deadbeef'"
        ))
        .arg("bash")
        .arg(script())
        .status()
        .expect("spawn bash");
    assert!(
        !bad.success(),
        "930: a wrong sha256 must NEVER verify true (fail-closed on a corrupted/stale download)"
    );
}

#[test]
fn main_with_unknown_subcommand_fails_loud_never_silently_succeeds() {
    let out = Command::new("bash")
        .arg(script())
        .arg("not-a-real-subcommand")
        .output()
        .expect("spawn bash");
    assert!(
        !out.status.success(),
        "930: an unknown subcommand must exit non-zero, not silently do nothing"
    );
}

/// #930 finding 11 — `lipsync-asset.sh fetch` writes a `sample-frame.jpg` by-eye sanity check
/// into `assets/lipsync/`, but the ignore rules only covered `*.ogv`/`*.mp4`/`*.jsonl` there.
/// Never committed -- mirrors the OTHER lipsync asset binaries in the same directory.
#[test]
fn gitignore_covers_the_sample_frame_jpg_930() {
    let gi = read(".gitignore");
    assert!(
        gi.lines().any(|l| l.trim() == "assets/lipsync/*.jpg"),
        "930: .gitignore must ignore assets/lipsync/*.jpg (the sample-frame.jpg by-eye sanity \
         check lipsync-asset.sh writes) -- same as the other lipsync asset binaries: {gi}"
    );
}
