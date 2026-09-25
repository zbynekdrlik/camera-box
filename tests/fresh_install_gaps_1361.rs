//! issue 1361 (slice: the fresh-install gaps, design comment 5830231872) -- a fresh
//! `setup-strih.sh` run must reproduce the production strih-lx box, not a box missing the hand fixes.
//!
//! - G3: the Downstream Keyer OBS plugin from the pinned upstream .deb (sha256 on the .deb AND on the
//!   extracted plugin), `scripts/lib/obs-downstream-keyer.sh`, setup-strih step 4c, verify-strih
//!   item 35.
//! - G4: the zero-loss restart mode's strih text goes through the `strih_platform` helpers.
//!
//! G1 (python3-websocket in the shared baseline + its grader row) is pinned in
//! `tests/obs_box_baseline_1357.rs`; G2 (the shared remoteos-mcp installer) in
//! `tests/remoteos_mcp_1361.rs`.
//!
//! Tier-0: sourced bash + PATH stubs (curl / apt-get / dpkg-deb), no root, no network, no rig.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("issue 1361: read {}: {e}", p.display()))
}

const DSK_LIB: &str = "scripts/lib/obs-downstream-keyer.sh";
const PLATFORM_LIB: &str = "scripts/lib/strih-platform.sh";

/// Source `lib` under the callers' strict mode and run `body` with `env`. (exit, stdout, stderr)
fn run(lib: &str, env: &[(&str, &str)], body: &str) -> (i32, String, String) {
    let harness = format!("set -euo pipefail\n. \"$LIB\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("LIB", root().join(lib));
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write_exec(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// curl: copies the fake .deb to the `-o` target and logs its argv.
const CURL_STUB: &str = r#"#!/bin/bash
out=""; url=""
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  case "${args[$i]}" in
    -o) out="${args[$((i + 1))]}" ;;
    http*) url="${args[$i]}" ;;
  esac
done
echo "CURL_ARGV: $*" >> "$STUB_LOG"
case "$url" in
  *.deb) cp "$FAKE_DEB" "$out" ;;
esac
exit 0
"#;

const LOG_STUB: &str = r#"#!/bin/bash
echo "$(basename "$0"): $*" >> "$STUB_LOG"
exit 0
"#;

fn on_code_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

// ------------------------------------------------------------------------------------------------
// G3 -- the Downstream Keyer plugin
// ------------------------------------------------------------------------------------------------

#[test]
fn dsk_pins_are_the_live_strih_lx_plugin() {
    let (c, out, err) = run(
        DSK_LIB,
        &[],
        "obs_dsk_version; echo; obs_dsk_deb_url; echo; obs_dsk_deb_sha256; echo; obs_dsk_so_sha256; echo; \
         obs_dsk_so_path /usr/lib/x86_64-linux-gnu; echo; obs_dsk_data_dir /usr/share",
    );
    assert_eq!(c, 0, "stderr={err}");
    let want = "0.4.4
https://github.com/exeldro/obs-downstream-keyer/releases/download/0.4.4/downstream-keyer-0.4.4-x86_64-linux-gnu.deb
ad585eec720c3cf690dd548b6cfc72ea5007b447954559c0861959ee60b0cf67
9304d665e7fc96ea54faf7ee31ab5ce0462069de668bcb4be996054876508be5
/usr/lib/x86_64-linux-gnu/obs-plugins/downstream-keyer.so
/usr/share/obs/obs-plugins/downstream-keyer";
    assert_eq!(out, want);
}

const DPKG_DEB_STUB: &str = r#"#!/bin/bash
echo "dpkg-deb: $*" >> "$STUB_LOG"
[ "${1:-}" = -x ] || exit 2
d="$3"
mkdir -p "$d/usr/lib/x86_64-linux-gnu/obs-plugins" "$d/usr/share/obs/obs-plugins/downstream-keyer/locale"
cp "$FAKE_SO" "$d/usr/lib/x86_64-linux-gnu/obs-plugins/downstream-keyer.so"
echo 'DownstreamKeyer="Downstream Keyer"' > "$d/usr/share/obs/obs-plugins/downstream-keyer/locale/en-US.ini"
echo 'DownstreamKeyer="Keyer"' > "$d/usr/share/obs/obs-plugins/downstream-keyer/locale/fr-FR.ini"
"#;

fn sha256(path: &Path) -> String {
    let out = Command::new("sha256sum").arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

struct DskRig {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

impl DskRig {
    fn new() -> DskRig {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        fs::create_dir_all(base.join("bin")).unwrap();
        fs::create_dir_all(base.join("tmp")).unwrap();
        write_exec(&base.join("bin/curl"), CURL_STUB);
        write_exec(&base.join("bin/dpkg-deb"), DPKG_DEB_STUB);
        write_exec(&base.join("bin/apt-get"), LOG_STUB);
        fs::write(base.join("fake.deb"), "a fake .deb for issue 1361\n").unwrap();
        fs::write(base.join("fake.so"), "a fake plugin for issue 1361\n").unwrap();
        fs::write(base.join("stub.log"), "").unwrap();
        DskRig { _dir: dir, base }
    }

    fn install(&self, deb_sha: &str, so_sha: &str) -> (i32, String, String) {
        let b = |p: &str| self.base.join(p).display().to_string();
        let path = format!("{}:{}", b("bin"), std::env::var("PATH").unwrap());
        let body = format!(
            "( eval \"$(obs_dsk_install_cmds 0.4.4 https://example.invalid/dsk.deb {deb_sha} {so_sha} \"$ROOTD/lib\" \"$ROOTD/share\")\" )"
        );
        run(
            DSK_LIB,
            &[
                ("PATH", path.as_str()),
                ("STUB_LOG", b("stub.log").as_str()),
                ("FAKE_DEB", b("fake.deb").as_str()),
                ("FAKE_SO", b("fake.so").as_str()),
                ("ROOTD", self.base.display().to_string().as_str()),
                ("TMPDIR", b("tmp").as_str()),
            ],
            &body,
        )
    }
}

#[test]
fn dsk_install_verifies_both_hashes_then_is_idempotent() {
    let rig = DskRig::new();
    let deb_sha = sha256(&rig.base.join("fake.deb"));
    let so_sha = sha256(&rig.base.join("fake.so"));
    let (c, out, err) = rig.install(&deb_sha, &so_sha);
    assert_eq!(c, 0, "stdout={out}\nstderr={err}");
    let so = rig.base.join("lib/obs-plugins/downstream-keyer.so");
    assert_eq!(sha256(&so), so_sha);
    assert!(rig
        .base
        .join("share/obs/obs-plugins/downstream-keyer/locale/en-US.ini")
        .is_file());
    assert!(out.contains("installed"), "{out}");
    let tmp_left = fs::read_dir(rig.base.join("tmp")).unwrap().count();
    assert_eq!(tmp_left, 0, "the temp dir is removed");

    fs::write(rig.base.join("stub.log"), "").unwrap();
    let (c, out, err) = rig.install(&deb_sha, &so_sha);
    assert_eq!(c, 0, "stderr={err}");
    assert!(out.contains("already installed"), "{out}");
    let log = fs::read_to_string(rig.base.join("stub.log")).unwrap();
    assert!(
        !log.contains("CURL_ARGV"),
        "an installed plugin with the pinned hash downloads nothing: {log}"
    );
}

#[test]
fn dsk_install_refuses_a_wrong_deb_or_plugin_hash() {
    let rig = DskRig::new();
    let deb_sha = sha256(&rig.base.join("fake.deb"));
    let so_sha = sha256(&rig.base.join("fake.so"));
    let wrong = "0".repeat(64);
    let (c, _o, err) = rig.install(&wrong, &so_sha);
    assert_ne!(c, 0);
    assert!(err.contains(".deb sha256 mismatch"), "{err}");
    assert!(!rig
        .base
        .join("lib/obs-plugins/downstream-keyer.so")
        .exists());
    let (c, _o, err) = rig.install(&deb_sha, &wrong);
    assert_ne!(c, 0);
    assert!(err.contains("plugin sha256 mismatch"), "{err}");
    assert!(!rig
        .base
        .join("lib/obs-plugins/downstream-keyer.so")
        .exists());
}

#[test]
fn dsk_verdict_cases() {
    let v = |got: &str, want: &str, loc: &str| {
        run(
            DSK_LIB,
            &[("G", got), ("W", want), ("L", loc)],
            "obs_dsk_verdict \"$G\" \"$W\" \"$L\"",
        )
    };
    assert_eq!(v("aa", "aa", "1"), (0, "ok\n".into(), String::new()));
    assert_eq!(v("", "aa", "1").1, "FAIL: missing\n");
    assert_eq!(v("ab", "aa", "1").1, "FAIL: sha-mismatch\n");
    assert_eq!(v("aa", "", "1").1, "FAIL: sha-mismatch\n");
    assert_eq!(v("aa", "aa", "0").1, "FAIL: no-locale\n");
    assert_eq!(v("aa", "aa", "0").0, 1);
}

#[test]
fn setup_strih_installs_the_dsk_plugin_in_step_4c() {
    let s = read("scripts/setup-strih.sh");
    assert!(
        on_code_line(&s, "lib/obs-downstream-keyer.sh\""),
        "setup-strih must source the DSK lib"
    );
    let step = s.find("step \"4c\" ").expect("a lettered step 4c");
    let prev = s.find("step 4b ").expect("step 4b");
    let next = s.find("step 5 ").expect("step 5");
    assert!(prev < step && step < next, "step 4c sits between 4b and 5");
    let body = &s[step..next];
    assert!(
        body.contains("( eval \"$(obs_dsk_install_cmds \"$(obs_dsk_version)\" \"$(obs_dsk_deb_url)\" \"$(obs_dsk_deb_sha256)\" \"$(obs_dsk_so_sha256)\" /usr/lib/x86_64-linux-gnu /usr/share)\" ) \\"),
        "step 4c runs the emitted block in a subshell with the /usr prefix: {body}"
    );
    assert!(
        body.contains("|| fail "),
        "a failed install fails the run: {body}"
    );
}

#[test]
fn verify_strih_grades_the_dsk_plugin_before_the_baseline_item() {
    let v = read("scripts/verify-strih.sh");
    assert!(
        on_code_line(&v, "lib/obs-downstream-keyer.sh\""),
        "verify-strih must source the DSK lib"
    );
    let item = v.find("# 35) Downstream Keyer").expect("item 35");
    let baseline = v
        .find("# 32) the shared OBS-box appliance baseline")
        .expect("item 32");
    assert!(
        item < baseline,
        "nothing may follow item 33, so item 35 sits before item 32"
    );
    let body = &v[item..baseline];
    assert!(
        body.contains("obs_dsk_verdict ")
            && body.contains("obs_dsk_so_sha256")
            && body.contains("bad \""),
        "{body}"
    );
}

// ------------------------------------------------------------------------------------------------
// G4 -- the Windows-strih remnants in the zero-loss restart mode + the calibrate scripts
// ------------------------------------------------------------------------------------------------

#[test]
fn strih_obs_restart_hint_follows_the_platform() {
    let (c, out, err) = run(PLATFORM_LIB, &[], "strih_obs_restart_hint 10.77.9.202");
    assert_eq!(c, 0, "stderr={err}");
    assert!(
        out.contains("systemctl --user restart strih-obs.service")
            && !out.contains("launch-obs-genlock"),
        "{out}"
    );
    let (c, out, err) = run(PLATFORM_LIB, &[], "strih_obs_restart_hint 192.0.2.10");
    assert_eq!(c, 0, "stderr={err}");
    assert!(
        out.contains("launch-obs-genlock.sh") && !out.contains("systemctl"),
        "{out}"
    );
}

#[test]
fn zero_loss_restart_text_goes_through_the_platform_helpers() {
    let s = read("scripts/recording-e2e.sh");
    let start = s
        .find("zero_loss_record_and_emit_plan() {")
        .expect("the zero-loss planner");
    let end = start + s[start..].find("[4f/8]").expect("the next step");
    let region = &s[start..end];
    assert!(
        region.contains("($(strih_access_label \"$STRIH\")), in place ---"),
        "the [label 8a] banner"
    );
    assert!(
        region.contains("$(strih_obs_restart_hint \"$STRIH\")"),
        "the OBS restart instruction"
    );
    assert!(
        !region.contains("the win-strih/win-stream-snv"),
        "the holder line names the platform"
    );
    let win: Vec<&str> = region.lines().filter(|l| l.contains("win-strih")).collect();
    assert_eq!(
        win.len(),
        1,
        "only the byte-pinned pull-back line keeps its Windows text: {win:?}"
    );
    assert!(
        win[0].contains("(win-strih FileDownload $strih_partial_win -> $strih_partial)"),
        "{win:?}"
    );
}

#[test]
fn calibrate_scripts_carry_no_windows_strih_mcp_name() {
    for script in [
        "scripts/phase_sync_calibrate.py",
        "scripts/av_sync_calibrate.py",
    ] {
        assert!(
            !read(script).contains("win-strih"),
            "{script} still names the retired win-strih MCP"
        );
    }
}
